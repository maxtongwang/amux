#!/usr/bin/env bash
# scripts/lib/test-isolation.sh must isolate, and must REFUSE when it cannot.
#
# Every CLI test leans on amux_test_isolate to keep it off the live fleet
# (a test that pinned only HOME created workers on the real server,
# 2026-10-16). A guard that never refuses is no guard, so each refusal is
# driven here with its own input, beside the passing case.
#
# Usage: scripts/test-test-isolation.sh   Exit 0 = all pass, 1 = a failure.
set -uo pipefail
cd "$(dirname "$0")/.."
PASS=0; FAIL=0
check() { if [ "$2" = "$3" ]; then PASS=$((PASS+1)); else FAIL=$((FAIL+1)); echo "  FAIL  $1 (want $2, got $3)"; fi; }
T=$(mktemp -d); trap 'rm -rf "$T"' EXIT
real=$(python3 -c 'import os, pwd; print(pwd.getpwuid(os.getuid()).pw_dir)')

# Each case runs in its own bash, so nothing it exports reaches this script.
# The passing case inherits a lane's whole environment, CC_HOME included.
mkdir -p "$T/ok"
rc=$(CC_HOME="$real/.amux" AMUX_SESSION=lane AMUX_WORKER=lane AMUX_WORKER_TOKEN=x \
     AMUX_API=https://localhost:8824 AMUX_URL=https://localhost:8824 TMUX=/tmp/s,1,0 TMUX_PANE=%1 AMUX_BIN=./amux bash -c '
  source scripts/lib/test-isolation.sh
  amux_test_isolate "$1" || exit 10
  [ "$HOME" = "$1" ] && [ "$CC_HOME" = "$1/.amux" ] && [ "$AMUX_HOME" = "$1/.amux" ] || exit 11
  [ -z "${AMUX_SESSION:-}${AMUX_WORKER:-}${AMUX_WORKER_TOKEN:-}${TMUX:-}${TMUX_PANE:-}" ] || exit 12
  [ "$AMUX_API" = http://127.0.0.1:9 ] && [ "$AMUX_URL" = http://127.0.0.1:9 ] || exit 14
  [ "${AMUX_BIN:-}" = ./amux ] || exit 13
' _ "$T/ok" >/dev/null 2>&1; echo $?)
check "pins HOME/CC_HOME/AMUX_HOME, clears identity, points the API at a dead port, keeps AMUX_BIN" 0 "$rc"

rc=$(bash -c 'source scripts/lib/test-isolation.sh; amux_test_isolate "$1" || exit 9; export AMUX_API=https://localhost:8824; amux_test_assert_isolated' _ "$T/ok" 2>"$T/err"; echo $?)
check "the assert refuses the live server port" 1 "$rc"
rc=$(bash -c 'source scripts/lib/test-isolation.sh; amux_test_isolate "$1" || exit 9; unset AMUX_URL; amux_test_assert_isolated' _ "$T/ok" 2>"$T/err"; echo $?)
check "the assert refuses an unset URL (the CLI would fall back to 8824)" 1 "$rc"

rc=$(bash -c 'source scripts/lib/test-isolation.sh; amux_test_isolate "$1"' _ "$real" 2>"$T/err"; echo $?)
check "refuses the real home as the test home" 1 "$rc"
grep -q "REFUSING: HOME resolves to the real home" "$T/err"; check "names the real-home cause" 0 $?

mkdir -p "$T/ep/.amux"; echo '{"canonical_url":"https://localhost:8824"}' > "$T/ep/.amux/endpoint.json"
rc=$(bash -c 'source scripts/lib/test-isolation.sh; amux_test_isolate "$1"' _ "$T/ep" 2>"$T/err"; echo $?)
check "refuses when an endpoint.json is reachable from the test home" 1 "$rc"
grep -q "endpoint is reachable" "$T/err"; check "names the endpoint cause" 0 $?

mkdir -p "$T/late"
rc=$(bash -c 'source scripts/lib/test-isolation.sh; amux_test_isolate "$1" || exit 9; export AMUX_SESSION=lane; amux_test_assert_isolated' _ "$T/late" 2>"$T/err"; echo $?)
check "the assert refuses an identity set after isolating" 1 "$rc"

rc=$(bash -c 'source scripts/lib/test-isolation.sh; amux_test_isolate "$1" || exit 9; export CC_HOME="$2/.amux"; amux_test_assert_isolated' _ "$T/late" "$real" 2>"$T/err"; echo $?)
check "the assert refuses CC_HOME pointed back at the real ~/.amux" 1 "$rc"

rc=$(bash -c 'source scripts/lib/test-isolation.sh; amux_test_isolate "$1"' _ "$T/missing" 2>/dev/null; echo $?)
check "refuses a directory that does not exist" 1 "$rc"

rc=$(env -u AMUX_TEST_LIVE bash -c 'source scripts/lib/test-isolation.sh; amux_test_require_live; exit 0' 2>"$T/err"; echo $?)
check "a live-server test skips (exit 2) without AMUX_TEST_LIVE=1" 2 "$rc"
rc=$(AMUX_TEST_LIVE=1 bash -c 'source scripts/lib/test-isolation.sh; amux_test_require_live; exit 0' 2>/dev/null; echo $?)
check "and runs when the caller opts in" 0 "$rc"

echo "test isolation: $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
