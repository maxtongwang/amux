#!/usr/bin/env bash
# test-land-fast-gate.sh [amux]: with `git config amux.landFastGate true` the
# push runs the hook in land-precheck mode (LAND_PRECHECK=1, judge only), so a
# slow leg below the hook's stop point does not run; without it, the full hook
# runs (Ethan, 2026-10-03, for the goal-spec-12 build).
set -uo pipefail
AM="${1:-$(cd "$(dirname "$0")/.." && pwd)/amux}"
AM="$(cd "$(dirname "$AM")" && pwd)/$(basename "$AM")"
H="$(mktemp -d "${TMPDIR:-/tmp}/lfg.XXXXXX")"; mkdir -p "$H/.amux/logs/land"
# Hermetic: never the live server or the real ~/.amux (lib/test-isolation.sh).
# shellcheck source=lib/test-isolation.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib/test-isolation.sh"; amux_test_isolate "$H" || exit 1
cd "$H" || exit 1
git init -q --bare --initial-branch=main origin.git
git clone -q origin.git lane 2>/dev/null; cd lane || exit 1
git config user.email t@t; git config user.name t
echo a > f; git add f; git commit -qm base; git push -q origin HEAD:main 2>/dev/null
mkdir -p .git/hooks
cat > .git/hooks/pre-push <<'EOF'
#!/bin/sh
echo "cheap-leg: OK"
if [ "${LAND_PRECHECK:-0}" = 1 ]; then echo "land-precheck: stops here"; exit 0; fi
echo "slow-leg: ✗ FAILED (would take 30 min)"; exit 1
EOF
chmod +x .git/hooks/pre-push
fail=0
echo b > f; git commit -qm one -- f
HOME="$H" AMUX_LAND_NOTIFY=0 AMUX_WORKER=lfg bash "$AM" land --tries 1 --no-batch >/dev/null 2>&1; rc=$?
[ "$rc" != 0 ] && echo "ok   without the switch the full hook runs and its slow leg refuses" || { echo "FAIL pushed without the switch"; fail=1; }
git config amux.landFastGate true
HOME="$H" AMUX_LAND_NOTIFY=0 AMUX_WORKER=lfg bash "$AM" land --tries 1 --no-batch >/dev/null 2>&1; rc=$?
git fetch -q origin
[ "$rc" = 0 ] && [ "$(git show origin/main:f)" = b ] && echo "ok   with amux.landFastGate the push skips the slow leg and lands" || { echo "FAIL fast gate rc=$rc"; fail=1; }
grep -q "fast gate: pushing with the hook's seconds-cheap legs only" "$H/.amux/logs/land.log" && echo "ok   land.log says the fast gate was used" || { echo "FAIL no fast-gate log line"; fail=1; }
cd / && rm -rf -- "${H:?}"
exit $fail
