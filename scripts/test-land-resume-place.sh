#!/usr/bin/env bash
# test-land-resume-place.sh [amux]: a land that stops because its commits
# conflict with origin at its turn keeps its queue place for a rebased
# re-enqueue (mixpeek-override, 2026-10-03: long stacks waited 80-95 min,
# conflicted at their turn, went to the back and repeated).
set -uo pipefail
AM="${1:-$(cd "$(dirname "$0")/.." && pwd)/amux}"
AM="$(cd "$(dirname "$AM")" && pwd)/$(basename "$AM")"
H="$(mktemp -d "${TMPDIR:-/tmp}/lrp.XXXXXX")"; mkdir -p "$H/.amux/logs/land"
# Hermetic: never the live server or the real ~/.amux (lib/test-isolation.sh).
# shellcheck source=lib/test-isolation.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib/test-isolation.sh"; amux_test_isolate "$H" || exit 1
cd "$H" || exit 1
git init -q --bare --initial-branch=main origin.git
mk() { git clone -q origin.git "$1" 2>/dev/null; git -C "$1" config user.email t@t; git -C "$1" config user.name t; }
mk seed; (cd seed && echo a > f && git add f && git commit -qm base && git push -q origin HEAD:main 2>/dev/null)
mk lane; (cd lane && echo lane > f && git commit -qm "lane edit" -- f)
key="$(printf '%s %s' "$(git -C lane remote get-url origin)" main | shasum | cut -c1-16)"
lock="$H/.amux/locks/land-$key"; mkdir -p "$lock"
sleep 300 & holder=$!
echo "$holder" > "$lock/pid"; echo other > "$lock/who"; date +%s > "$lock/since"
(cd lane && HOME="$H" AMUX_LAND_NOTIFY=0 AMUX_WORKER=lrp AMUX_LAND_WAIT_POLL_S=2 bash "$AM" land > "$H/first.out" 2>&1) & L1=$!
for _ in $(seq 1 20); do [ -n "$(ls "$lock.q" 2>/dev/null)" ] && break; sleep 1; done
first="$(ls "$lock.q" | head -1)"; arr="${first%%-*}"
# origin moves under the waiting lane, conflicting with its edit
(cd seed && echo other > f && git commit -qm "conflicting" -- f && git push -q origin HEAD:main 2>/dev/null)
sleep 3
kill "$holder" 2>/dev/null; rm -rf -- "${lock:?}"
wait "$L1"; rc1=$?
fail=0
[ "$rc1" = 2 ] && echo "ok   the land stopped on the conflict at its turn (rc 2)" || { echo "FAIL first land rc=$rc1: $(tail -2 "$H/first.out")"; fail=1; }
[ -f "$lock.q/.resume-lrp" ] && echo "ok   a resume marker records its place" || { echo "FAIL no resume marker"; fail=1; }
# the lane rebases (resolving the conflict) and lands again behind a new holder
(cd lane && git fetch -q origin && git rebase -q origin/main 2>/dev/null; echo merged > f; git add f; GIT_EDITOR=true git rebase --continue >/dev/null 2>&1 || git commit -qm "resolved" -- f) >/dev/null 2>&1
mkdir -p "$lock"; sleep 300 & holder=$!
echo "$holder" > "$lock/pid"; echo other > "$lock/who"; date +%s > "$lock/since"
sleep 2
(cd lane && HOME="$H" AMUX_LAND_NOTIFY=0 AMUX_WORKER=lrp bash "$AM" land > "$H/second.out" 2>&1) & L2=$!
for _ in $(seq 1 20); do [ -n "$(ls "$lock.q" 2>/dev/null)" ] && break; sleep 1; done
second="$(ls "$lock.q" | head -1)"
[ "${second%%-*}" = "$arr" ] && echo "ok   the rebased re-enqueue took back its arrival time ($arr)" || { echo "FAIL new ticket $second, original arrival $arr"; fail=1; }
grep -q "resumed its queue place" "$H/.amux/logs/land.log" && echo "ok   land.log records the resume" || { echo "FAIL no resume log line"; fail=1; }
kill "$holder" "$L2" 2>/dev/null; pkill -f "$H" 2>/dev/null; wait 2>/dev/null
cd / && rm -rf -- "${H:?}"
exit $fail
