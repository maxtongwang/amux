#!/usr/bin/env bash
# test-land-cancel-for.sh [amux]: --cancel-for refuses a wrong owner and a missing
# reason, cancels the named lane's waiter (whole process group, ticket removed),
# and logs both lanes (mixpeek-override, 2026-10-02: no sanctioned way to pull
# a stale tip out of a 15-push batch). Runs under a throwaway HOME.
AM="${1:-$(cd "$(dirname "$0")/.." && pwd)/amux}"; AM="$(cd "$(dirname "$AM")" && pwd)/$(basename "$AM")"; H="$(mktemp -d "${TMPDIR:-/tmp}/cf.XXXXXX")"
# Hermetic: never the live server or the real ~/.amux (lib/test-isolation.sh).
# shellcheck source=lib/test-isolation.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib/test-isolation.sh"; amux_test_isolate "$H" || exit 1
mkdir -p "$H/.amux/locks/land-test.q" "$H/.amux/logs/land" "$H/.amux/locks/detached"
perl -e 'setpgrp(0,0); exec "sleep", "300"' & W=$!
sleep 1
printf 'lane-b\nabc\n/x/.git\n1\n2026-10-02.25\n' > "$H/.amux/locks/land-test.q/001790000000-$W"
r1="$(HOME="$H" AMUX_WORKER=orch bash "$AM" land --cancel-for lane-x "$W" --reason "wrong owner test" 2>&1)"; c1=$?
alive1=no; kill -0 "$W" 2>/dev/null && alive1=yes
r0="$(HOME="$H" AMUX_WORKER=orch bash "$AM" land --cancel-for lane-b "$W" 2>&1)"; c0=$?
r2="$(HOME="$H" AMUX_WORKER=orch bash "$AM" land --cancel-for lane-b "$W" --reason "stale tip in a 15-push batch" 2>&1)"; c2=$?
sleep 1; alive2=no; kill -0 "$W" 2>/dev/null && alive2=yes
ticket=gone; [ -f "$H/.amux/locks/land-test.q/001790000000-$W" ] && ticket=present
fail=0
[ "$c1" = 1 ] && [ "$alive1" = yes ] && echo "ok   wrong owner refused, waiter untouched" || { echo "FAIL wrong owner: rc=$c1 alive=$alive1 out=$r1"; fail=1; }
[ "$c0" = 2 ] && echo "ok   missing --reason refused" || { echo "FAIL no-reason: rc=$c0 out=$r0"; fail=1; }
[ "$c2" = 0 ] && [ "$alive2" = no ] && [ "$ticket" = gone ] && echo "ok   right owner cancelled: process group stopped, ticket removed" || { echo "FAIL cancel: rc=$c2 alive=$alive2 ticket=$ticket out=$r2"; fail=1; }
grep -q "orch cancelled lane-b's queued land .*reason: stale tip" "$H/.amux/logs/land.log" && echo "ok   land.log names both lanes and the reason" || { echo "FAIL log"; fail=1; }
kill "$W" 2>/dev/null; rm -rf -- "${H:?}"
exit $fail
