#!/usr/bin/env bash
# test-land-window.sh [amux]: with `git config amux.landMinIntervalS N` a
# holder whose branch was pushed less than N seconds ago holds the lock until N
# has passed (so waiters join its batch), and logs it; a --priority holder does
# not wait (Ethan, 2026-10-04: pushes every 3 to 5 min superseded every 17-min
# Fast Checks run).
set -uo pipefail
AM="${1:-$(cd "$(dirname "$0")/.." && pwd)/amux}"
AM="$(cd "$(dirname "$AM")" && pwd)/$(basename "$AM")"
H="$(mktemp -d "${TMPDIR:-/tmp}/lwn.XXXXXX")"; mkdir -p "$H/.amux/logs/land"
# Hermetic: never the live server or the real ~/.amux (lib/test-isolation.sh).
# shellcheck source=lib/test-isolation.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib/test-isolation.sh"; amux_test_isolate "$H" || exit 1
cd "$H" || exit 1
git init -q --bare --initial-branch=main origin.git
git clone -q origin.git lane 2>/dev/null; cd lane || exit 1
git config user.email t@t; git config user.name t
echo a > f; git add f; git commit -qm base; git push -q origin HEAD:main 2>/dev/null
git config amux.landMinIntervalS 20
land() { HOME="$H" AMUX_LAND_NOTIFY=0 AMUX_WORKER=lwn bash "$AM" land --tries 1 --no-batch "$@" >/dev/null 2>&1; }
fail=0
echo b > f; git commit -qm one -- f; land || { echo "FAIL first land"; fail=1; }
git fetch -q origin; git reset -q --hard origin/main
echo c > f; git commit -qm two -- f
t0=$(date +%s); land; rc=$?; dt=$(( $(date +%s) - t0 ))
git fetch -q origin
[ "$rc" = 0 ] && [ "$(git show origin/main:f)" = c ] && echo "ok   the second land lands" || { echo "FAIL second land rc=$rc"; fail=1; }
[ "$dt" -ge 2 ] && echo "ok   it held the lock for the window (${dt}s)" || { echo "FAIL no hold (${dt}s)"; fail=1; }
grep -q 'landing window: main was pushed .*amux.landMinIntervalS=20' "$H/.amux/logs/land.log" && echo "ok   land.log names the hold and the setting" || { echo "FAIL no window line"; fail=1; }
git reset -q --hard origin/main; echo d > f; git commit -qm three -- f
n_before=$(grep -c 'landing window' "$H/.amux/logs/land.log")
git config amux.landPriorityPaths "f"
AMUX_LAND_PRIORITY_PATHS="f" land --priority --reason "gate fix" ; rc=$?
n_after=$(grep -c 'landing window' "$H/.amux/logs/land.log")
[ "$rc" = 0 ] && [ "$n_before" = "$n_after" ] && echo "ok   a priority land does not wait" || { echo "FAIL priority waited or failed (rc=$rc, $n_before -> $n_after)"; fail=1; }
git fetch -q origin; git reset -q --hard origin/main; echo e > f; git commit -qm four -- f
git config amux.landWindowExemptGatePath false
n_before=$(grep -c 'landing window: main' "$H/.amux/logs/land.log")
AMUX_LAND_PRIORITY_PATHS="f" land --priority --reason "gate fix" ; rc=$?
n_after=$(grep -c 'landing window: main' "$H/.amux/logs/land.log")
[ "$rc" = 0 ] && [ "$n_after" -gt "$n_before" ] && echo "ok   with the gate-path exemption off, a gate-path priority push waits" || { echo "FAIL gate-path priority did not wait (rc=$rc, $n_before -> $n_after)"; fail=1; }
git fetch -q origin; git reset -q --hard origin/main; echo g > f; git commit -qm five -- f
n_before=$(grep -c 'landing window: main' "$H/.amux/logs/land.log")
land --priority --reason "grant: test 1" ; rc=$?
n_after=$(grep -c 'landing window: main' "$H/.amux/logs/land.log")
[ "$rc" = 0 ] && [ "$n_after" = "$n_before" ] && echo "ok   a granted priority push still skips it" || { echo "FAIL granted push waited (rc=$rc, $n_before -> $n_after)"; fail=1; }
cd / && rm -rf -- "${H:?}"
exit $fail
