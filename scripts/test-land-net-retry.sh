#!/usr/bin/env bash
# test-land-net-retry.sh [amux]: a push that fails before reaching the remote
# ("Could not resolve host") is retried with backoff and lands; a hook refusal
# is not retried (2026-10-03: router DNS timed out 23:30-23:50Z and gs12-obs's
# land stopped as if refused).
set -uo pipefail
AM="${1:-$(cd "$(dirname "$0")/.." && pwd)/amux}"
AM="$(cd "$(dirname "$AM")" && pwd)/$(basename "$AM")"
H="$(mktemp -d "${TMPDIR:-/tmp}/lnr.XXXXXX")"; mkdir -p "$H/.amux/logs/land"
# Hermetic: never the live server or the real ~/.amux (lib/test-isolation.sh).
# shellcheck source=lib/test-isolation.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib/test-isolation.sh"; amux_test_isolate "$H" || exit 1
cd "$H" || exit 1
git init -q --bare --initial-branch=main origin.git
git clone -q origin.git lane 2>/dev/null; cd lane || exit 1
git config user.email t@t; git config user.name t
echo a > f; git add f; git commit -qm base; git push -q origin HEAD:main 2>/dev/null
mkdir -p .git/hooks
cat > .git/hooks/pre-push <<HK
#!/bin/sh
n=\$(cat "$H/n" 2>/dev/null || echo 0); n=\$((n+1)); echo \$n > "$H/n"
[ -f "$H/refuse" ] && { echo "leg: ✗ FAILED"; exit 1; }
[ \$n -le 2 ] && { echo "fatal: unable to access 'https://github.com/x/y.git/': Could not resolve host: github.com"; exit 1; }
exit 0
HK
chmod +x .git/hooks/pre-push
fail=0
echo b > f; git commit -qm one -- f
HOME="$H" AMUX_LAND_NET_BACKOFF_S=1 AMUX_LAND_NOTIFY=0 AMUX_WORKER=lnr bash "$AM" land --tries 1 --no-batch >/dev/null 2>&1; rc=$?
git fetch -q origin
[ "$rc" = 0 ] && [ "$(git show origin/main:f)" = b ] && echo "ok   two DNS failures are retried and the push lands" || { echo "FAIL rc=$rc"; fail=1; }
[ "$(grep -c 'WARN push did not reach origin (network: Could not resolve host' "$H/.amux/logs/land.log")" = 2 ] && echo "ok   each retry is logged with its cause" || { echo "FAIL log: $(grep -a WARN "$H/.amux/logs/land.log")"; fail=1; }
git reset -q --hard origin/main; echo c > f; git commit -qm two -- f; touch "$H/refuse"; echo 0 > "$H/n"
HOME="$H" AMUX_LAND_NET_BACKOFF_S=1 AMUX_LAND_NOTIFY=0 AMUX_WORKER=lnr bash "$AM" land --tries 1 --no-batch >/dev/null 2>&1; rc=$?
[ "$rc" != 0 ] && [ "$(cat "$H/n")" = 1 ] && echo "ok   a hook refusal is not retried" || { echo "FAIL refusal rc=$rc runs=$(cat "$H/n")"; fail=1; }
cd / && rm -rf -- "${H:?}"
exit $fail
