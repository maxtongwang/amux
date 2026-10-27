#!/usr/bin/env bash
# test-land-to-head.sh [amux]: `amux land --to-head <pid>` moves one queued
# ticket ahead of a granted priority ticket (mixpeek-override, 2026-10-03: the
# pushed-tree extract fix sat behind aged and priority tickets). Two waiters
# queue behind a fake holder: a priority one, then an ordinary one; --to-head
# on the ordinary one must put it first. Also: aging reads git config.
set -uo pipefail
AM="${1:-$(cd "$(dirname "$0")/.." && pwd)/amux}"
AM="$(cd "$(dirname "$AM")" && pwd)/$(basename "$AM")"
H="$(mktemp -d "${TMPDIR:-/tmp}/lth.XXXXXX")"; mkdir -p "$H/.amux/logs/land"
# Hermetic: never the live server or the real ~/.amux (lib/test-isolation.sh).
# shellcheck source=lib/test-isolation.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib/test-isolation.sh"; amux_test_isolate "$H" || exit 1
cd "$H" || exit 1
git init -q --bare --initial-branch=main origin.git
mk() { git clone -q origin.git "$1" 2>/dev/null; git -C "$1" config user.email t@t; git -C "$1" config user.name t; }
mk seed; (cd seed && echo a > f && git add f && git commit -qm base && git push -q origin HEAD:main 2>/dev/null)
mk lp; (cd lp && mkdir -p scripts/ci && echo p > scripts/ci/p.sh && git add scripts/ci/p.sh && git commit -qm prio -- scripts/ci/p.sh)
mk lo; (cd lo && echo o > g && git add g && git commit -qm ord -- g)
key="$(printf '%s %s' "$(git -C lo remote get-url origin)" main | shasum | cut -c1-16)"
lock="$H/.amux/locks/land-$key"; mkdir -p "$lock"
sleep 300 & holder=$!
echo "$holder" > "$lock/pid"; echo other > "$lock/who"; date +%s > "$lock/since"
(cd lp && HOME="$H" AMUX_LAND_NOTIFY=0 AMUX_WORKER=lp bash "$AM" land --priority --reason "granted" >/dev/null 2>&1) & P1=$!
sleep 3
(cd lo && git config amux.landPriorityAgingS 99999 && HOME="$H" AMUX_LAND_NOTIFY=0 AMUX_WORKER=lo bash "$AM" land >/dev/null 2>&1) & P2=$!
first() { ls "$lock.q" 2>/dev/null | sort | head -1; }
for _ in $(seq 1 30); do [ "$(ls "$lock.q" 2>/dev/null | wc -l | tr -d ' ')" -ge 2 ] && break; sleep 1; done
wpid="$(for f in "$lock.q"/*; do [ "$(sed -n 1p "$f")" = lo ] && echo "${f##*-}"; done)"
fail=0
[ -n "$wpid" ] || { echo "FAIL setup: the ordinary waiter never queued"; fail=1; }
case "$(first)" in 000*) [ "$(sed -n 1p "$lock.q/$(first)")" = lp ] && echo "ok   the priority ticket is first before --to-head" ;; *) echo "FAIL setup: $(first) first"; fail=1 ;; esac
HOME="$H" AMUX_WORKER=orch bash "$AM" land --to-head "$wpid" --reason "the gate fix" >/dev/null 2>&1 || { echo "FAIL --to-head refused"; fail=1; }
for _ in $(seq 1 15); do [ "$(first)" = "000000000000-$wpid" ] && break; sleep 1; done
[ "$(first)" = "000000000000-$wpid" ] && echo "ok   --to-head put the ordinary ticket first" || { echo "FAIL head is $(first)"; fail=1; }
grep -q "moved to the head of the queue by orch: the gate fix" "$H/.amux/logs/land.log" && echo "ok   the move is logged with who and why" || { echo "FAIL no log line"; fail=1; }
HOME="$H" AMUX_WORKER=orch bash "$AM" land --to-head "$wpid" >/dev/null 2>&1 && { echo "FAIL --to-head without --reason accepted"; fail=1; } || echo "ok   --reason is required"
kill "$holder" 2>/dev/null; pkill -f "$H" 2>/dev/null; kill "$P1" "$P2" 2>/dev/null; wait 2>/dev/null
[ "$fail" = 0 ] && { cd / && rm -rf -- "${H:?}"; } || echo "kept $H"
exit $fail
