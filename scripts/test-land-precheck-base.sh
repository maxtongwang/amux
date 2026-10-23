#!/usr/bin/env bash
# test-land-precheck-base.sh [amux]: the enqueue precheck is handed the base as
# a SHA resolved once (never the ref name it might re-read mid-run), and a range
# longer than AMUX_LAND_PRECHECK_MAX_RANGE is reported unmeasured instead of
# refused (2026-10-04 22:05Z: a briefly wrong origin/main swept 4,165 published
# commits into gs12-spend's refusal).
set -uo pipefail
AM="${1:-$(cd "$(dirname "$0")/.." && pwd)/amux}"
AM="$(cd "$(dirname "$AM")" && pwd)/$(basename "$AM")"
H="$(mktemp -d "${TMPDIR:-/tmp}/lpb.XXXXXX")"; mkdir -p "$H/.amux/logs/land"
# Hermetic: never the live server or the real ~/.amux (lib/test-isolation.sh).
# shellcheck source=lib/test-isolation.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib/test-isolation.sh"; amux_test_isolate "$H" || exit 1
cd "$H" || exit 1
git init -q --bare --initial-branch=main origin.git
git clone -q origin.git lane 2>/dev/null; cd lane || exit 1
git config user.email t@t; git config user.name t
echo a > f; git add f; git commit -qm base; git push -q origin HEAD:main 2>/dev/null
cat > "$H/chk.sh" <<EOF
#!/bin/sh
echo "\$1" > "$H/base-arg"
echo "leg: REFUSED (always)"; exit 1
EOF
chmod +x "$H/chk.sh"
fail=0
echo b > f; git commit -qm one -- f
HOME="$H" AMUX_LAND_PRECHECK="$H/chk.sh" AMUX_LAND_NOTIFY=0 AMUX_WORKER=lpb bash "$AM" land --tries 1 --no-batch >/dev/null 2>&1; rc=$?
arg="$(cat "$H/base-arg" 2>/dev/null)"
[[ "$arg" =~ ^[0-9a-f]{40}$ ]] && [ "$arg" = "$(git rev-parse origin/main)" ] && echo "ok   the check is handed the base as a resolved SHA" || { echo "FAIL base arg '$arg'"; fail=1; }
[ "$rc" != 0 ] && echo "ok   a short range is still judged (refused here)" || { echo "FAIL short range not refused"; fail=1; }
echo c > f; git commit -qm two -- f
HOME="$H" AMUX_LAND_PRECHECK="$H/chk.sh" AMUX_LAND_PRECHECK_MAX_RANGE=1 AMUX_LAND_NOTIFY=0 AMUX_WORKER=lpb bash "$AM" land --tries 1 --no-batch >/dev/null 2>&1; rc=$?
grep -q 'WARN enqueue precheck unmeasured: the range .* is 2 commits (over 1)' "$H/.amux/logs/land.log" && echo "ok   an over-long range is logged unmeasured" || { echo "FAIL no unmeasured line"; fail=1; }
grep -q 'lpb enqueue precheck refused' <(tail -3 "$H/.amux/logs/land.log") && { echo "FAIL the over-long range was refused"; fail=1; } || echo "ok   and it is not refused on"
cd / && rm -rf -- "${H:?}"
exit $fail
