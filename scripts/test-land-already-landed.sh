#!/usr/bin/env bash
# test-land-already-landed.sh [amux]: a waiter whose commit reaches the remote
# through another holder's batch leaves the queue on its next poll instead of
# waiting for its own turn (gs12-compute's 8a794fe3d76 waited 244 min on
# 2026-10-03 after it had landed).
set -uo pipefail
AM="${1:-$(cd "$(dirname "$0")/.." && pwd)/amux}"
AM="$(cd "$(dirname "$AM")" && pwd)/$(basename "$AM")"
H="$(mktemp -d "${TMPDIR:-/tmp}/lal.XXXXXX")"; mkdir -p "$H/.amux/logs/land"
# Hermetic: never the live server or the real ~/.amux (lib/test-isolation.sh).
# shellcheck source=lib/test-isolation.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib/test-isolation.sh"; amux_test_isolate "$H" || exit 1
cd "$H" || exit 1
git init -q --bare --initial-branch=main origin.git
git clone -q origin.git lane 2>/dev/null; cd lane || exit 1
git config user.email t@t; git config user.name t
echo a > f; git add f; git commit -qm base; git push -q origin HEAD:main 2>/dev/null
echo b > f; git commit -qm change -- f
key="$(printf '%s %s' "$(git remote get-url origin)" main | shasum | cut -c1-16)"
lock="$H/.amux/locks/land-$key"; mkdir -p "$lock"
sleep 300 & holder=$!
echo "$holder" > "$lock/pid"; echo other > "$lock/who"; date +%s > "$lock/since"
HOME="$H" AMUX_LAND_NOTIFY=0 AMUX_WORKER=lal bash "$AM" land > "$H/out" 2>&1 & W=$!
for _ in $(seq 1 20); do [ -n "$(ls "$lock.q" 2>/dev/null)" ] && break; sleep 1; done
fail=0
[ -n "$(ls "$lock.q" 2>/dev/null)" ] && echo "ok   the waiter queued" || { echo "FAIL setup: no ticket"; fail=1; }
# another holder's batch lands this very commit
git push -q origin HEAD:main 2>/dev/null; git fetch -q origin
for _ in $(seq 1 30); do kill -0 "$W" 2>/dev/null || break; sleep 1; done
if kill -0 "$W" 2>/dev/null; then echo "FAIL the waiter is still queued after its commit landed"; fail=1
else wait "$W"; rc=$?; [ "$rc" = 0 ] && echo "ok   the waiter left the queue with rc 0" || { echo "FAIL waiter rc=$rc"; fail=1; }; fi
[ -z "$(ls "$lock.q" 2>/dev/null)" ] && echo "ok   its ticket is gone" || { echo "FAIL ticket left: $(ls "$lock.q")"; fail=1; }
grep -q "already on origin/main" "$H/.amux/logs/land.log" && echo "ok   land.log says why" || { echo "FAIL no log line"; fail=1; }
kill "$holder" "$W" 2>/dev/null; wait 2>/dev/null
cd / && rm -rf -- "${H:?}"
exit $fail
