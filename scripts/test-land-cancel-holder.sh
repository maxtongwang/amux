#!/usr/bin/env bash
# test-land-cancel-holder.sh [amux]: --cancel-for reaches the HOLDER, not only
# queued waiters (mixpeek-override, 2026-10-02 20:17Z: main was red on four
# tree-guard tests and gates' 9-push hold was doomed to burn its 30-minute
# guard leg). The holder sits in a slow push hook; the cancel must stop it and
# its hook, release the lock, refuse the wrong owner, and nothing is pushed.
set -uo pipefail
AM="${1:-$(cd "$(dirname "$0")/.." && pwd)/amux}"
AM="$(cd "$(dirname "$AM")" && pwd)/$(basename "$AM")"
# Hermetic: never the live server or the real ~/.amux (lib/test-isolation.sh).
# shellcheck source=lib/test-isolation.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib/test-isolation.sh"; amux_test_isolate "$(mktemp -d)" || exit 1
T="$(mktemp -d "${TMPDIR:-/tmp}/lch.XXXXXX")"; cd "$T" || exit 1
git init -q --bare --initial-branch=main origin.git
git clone -q origin.git lane 2>/dev/null; cd lane || exit 1
git config user.email t@t; git config user.name t
echo a > f; git add f; git commit -qm base; git push -q origin HEAD:main 2>/dev/null
mkdir -p .git/hooks; printf '#!/bin/sh\nsleep 120\n' > .git/hooks/pre-push; chmod +x .git/hooks/pre-push
echo b > f; git commit -qm "lane change" -- f
AMUX_LAND_NOTIFY=0 AMUX_WORKER=lch-holder bash "$AM" land --no-batch > "$T/holder.out" 2>&1 &
key=""; for _ in $(seq 1 40); do
  for d in "$HOME"/.amux/locks/land-*; do [ "$(cat "$d/who" 2>/dev/null)" = lch-holder ] && key="$d"; done
  [ -n "$key" ] && pgrep -f 'sleep 120' >/dev/null && break; sleep 0.5
done
fail=0
[ -n "$key" ] || { echo "FAIL setup: the holder never took the lock"; rm -rf -- "${T:?}"; exit 1; }
hp="$(cat "$key/pid")"
AMUX_WORKER=orch bash "$AM" land --cancel-for someone-else "$hp" --reason "wrong owner" >/dev/null 2>&1 \
  && { echo "FAIL a wrong owner cancelled the hold"; fail=1; } || echo "ok   wrong owner refused"
kill -0 "$hp" 2>/dev/null && echo "ok   holder untouched by the refused cancel" || { echo "FAIL holder died on a refused cancel"; fail=1; }
AMUX_WORKER=orch bash "$AM" land --cancel-for lch-holder "$hp" --reason "main is red on the tree guards" >/dev/null 2>&1 \
  && echo "ok   cancel-for accepted the holder pid" || { echo "FAIL cancel-for refused the holder"; fail=1; }
sleep 2
kill -0 "$hp" 2>/dev/null && { echo "FAIL holder still running"; fail=1; } || echo "ok   holder stopped"
[ -d "$key" ] && { echo "FAIL lock not released"; fail=1; } || echo "ok   lock released"
git fetch -q origin main; [ "$(git show origin/main:f)" = a ] && echo "ok   nothing pushed" || { echo "FAIL something was pushed"; fail=1; }
grep -q "orch cancelled lch-holder's HOLD .*reason: main is red" "$HOME/.amux/logs/land.log" && echo "ok   land.log names both lanes and the reason" || { echo "FAIL log line"; fail=1; }
grep -q "this hold was cancelled by orch at .*: main is red on the tree guards" "$T/holder.out" \
  && echo "ok   the holder's own output names the canceller and the reason" \
  || { echo "FAIL holder output: $(tail -2 "$T/holder.out")"; fail=1; }
pkill -f 'sleep 120' 2>/dev/null; wait 2>/dev/null
rm -rf -- "${T:?}"
exit $fail
