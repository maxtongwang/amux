#!/usr/bin/env bash
# scripts/lib/test-isolation.sh must isolate, and must REFUSE when it cannot.
#
# Every CLI test leans on amux_test_isolate to keep it off the live fleet
# (a test that pinned only HOME created workers on the real server,
# 2026-10-16). A guard that never refuses is no guard, so each refusal is
# driven here with its own input, beside the passing case.
#
# Usage: scripts/test-test-isolation.sh   Exit 0 = all pass, 1 = a failure.
set -euo pipefail
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
rc=0; grep -q "REFUSING: HOME resolves to the real home" "$T/err" || rc=$?; check "names the real-home cause" 0 "$rc"

mkdir -p "$T/ep/.amux"; echo '{"canonical_url":"https://localhost:8824"}' > "$T/ep/.amux/endpoint.json"
rc=$(bash -c 'source scripts/lib/test-isolation.sh; amux_test_isolate "$1"' _ "$T/ep" 2>"$T/err"; echo $?)
check "refuses when an endpoint.json is reachable from the test home" 1 "$rc"
rc=0; grep -q "endpoint is reachable" "$T/err" || rc=$?; check "names the endpoint cause" 0 "$rc"

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

# ── tmux: never the real user's default socket ──────────────────────────────
rc=$(bash -c 'source scripts/lib/test-isolation.sh; amux_test_isolate "$1" || exit 9
  [ -z "${TMUX:-}${TMUX_PANE:-}" ] || exit 10
  [ "$TMUX_TMPDIR" = "$1/tmux" ] && [ -d "$TMUX_TMPDIR" ] || exit 11' _ "$T/ok" 2>/dev/null; echo $?)
check "TMUX/TMUX_PANE unset and TMUX_TMPDIR private to the test" 0 "$rc"
rc=$(bash -c 'source scripts/lib/test-isolation.sh; amux_test_isolate "$1" || exit 9; export TMUX_TMPDIR=/tmp; amux_test_assert_isolated' _ "$T/ok" 2>"$T/err"; echo $?)
check "refuses TMUX_TMPDIR pointed at the system tmp" 1 "$rc"
rc=0; grep -q "default tmux socket" "$T/err" || rc=$?; check "names the default-socket cause" 0 "$rc"
rc=$(bash -c 'source scripts/lib/test-isolation.sh; amux_test_isolate "$1" || exit 9; unset TMUX_TMPDIR; amux_test_assert_isolated' _ "$T/ok" 2>"$T/err"; echo $?)
check "refuses an unset TMUX_TMPDIR (tmux would use the real default socket)" 1 "$rc"
rc=$(bash -c 'source scripts/lib/test-isolation.sh; amux_test_isolate "$1" || exit 9; export TMUX=/tmp/tmux-0/default,1,0; amux_test_assert_isolated' _ "$T/ok" 2>/dev/null; echo $?)
check "refuses an inherited TMUX set after isolating" 1 "$rc"

# A rename and a new-session inside a guarded test land on the PRIVATE server,
# and its server is gone once the test shell exits. Only the private socket
# is ever named (-S <private path>); the default socket is never called.
if command -v tmux >/dev/null 2>&1; then
  mkdir -p "$T/tm"
  sock="$T/tm/tmux/tmux-$(id -u)/default"
  rc=$(bash -c 'source scripts/lib/test-isolation.sh; amux_test_isolate "$1" || exit 9
    [ "$AMUX_TMUX_SOCKET" = "$TMUX_TMPDIR/tmux-$(id -u)/default" ] || exit 14
    amux_tmux -f /dev/null new-session -d -s amux-guard-probe "sleep 60" || exit 10
    amux_tmux rename-session -t =amux-guard-probe amux-guard-renamed || exit 11
    [ -S "$AMUX_TMUX_SOCKET" ] || exit 12
    [ "$(amux_tmux ls -F "#{session_name}")" = amux-guard-renamed ] || exit 13' _ "$T/tm" 2>"$T/err"; echo $?)
  check "new-session and rename land on the explicit private socket, which holds only the probe (12: no socket, 13: other sessions, 14: socket not exported) $(head -1 "$T/err")" 0 "$rc"
  gone=1
  for _ in 1 2 3 4 5 6; do
    if env -u TMUX -u TMUX_PANE tmux -S "$sock" has-session 2>/dev/null; then sleep 1; else gone=0; break; fi
  done
  check "the private tmux server is killed after the test shell exits" 0 "$gone"
  rc=$(env -u AMUX_TMUX_SOCKET bash -c 'source scripts/lib/test-isolation.sh; amux_tmux ls' 2>/dev/null; echo $?)
  check "amux_tmux refuses to run without a private socket" 1 "$rc"

  # THE OUTAGE, against a decoy. A DECOY server on its own socket stands in for
  # the live fleet; TMUX points at it exactly as it does inside a pane. Every
  # guarded destructive path (amux_tmux send-keys / kill-session / kill-server,
  # the teardown, the CLI's own tmux wrapper, the Python helper) must leave the
  # decoy and its session untouched. Before the guard named -S explicitly,
  # `TMUX_TMPDIR=... tmux kill-server` here killed the server TMUX named.
  decoy="$T/decoy.sock"
  env -u TMUX -u TMUX_PANE tmux -f /dev/null -S "$decoy" new-session -d -s decoy-live "sleep 300"
  dpid=$(env -u TMUX -u TMUX_PANE tmux -S "$decoy" display-message -p '#{pid}')
  mkdir -p "$T/dg"
  rc=$(TMUX="$decoy,$dpid,0" TMUX_PANE=%0 bash -c 'source scripts/lib/test-isolation.sh; amux_test_isolate "$1" || exit 9
    export TMUX="$2" TMUX_PANE=%0        # inherited again, as a pane would
    amux_tmux send-keys -t =decoy-live "echo pwned" Enter
    amux_tmux kill-session -t =decoy-live
    amux_tmux kill-server
    amux_test_teardown
    # The CLI under test wraps its own tmux calls with the private socket.
    dg="$1"; set --
    source ./amux >/dev/null 2>&1 || true
    set +eu    # the CLI turns on -euo pipefail; a kill-server with no server is fine
    [ "$(type -t tmux)" = function ] || exit 20
    # tmux-lint: allow(the amux CLI tmux() wrapper under test adds -S with the private socket)
    tmux kill-server 2>/dev/null
    python3 -c "import sys; sys.path.insert(0, \"scripts/lib\"); import os, test_isolation as t; t.kill_private_tmux(t.tmux_env(sys.argv[1], dict(os.environ)))" "$dg"
    exit 0' _ "$T/dg" "$decoy,$dpid,0" 2>"$T/err"; echo $?)
  check "the guarded destructive paths ran ($(head -1 "$T/err"))" 0 "$rc"
  rc=0; env -u TMUX -u TMUX_PANE tmux -S "$decoy" has-session -t =decoy-live 2>/dev/null || rc=$?
  check "the DECOY server and its session survive every guarded kill-server/kill-session/send-keys path" 0 "$rc"
  rc=0; env -u TMUX -u TMUX_PANE tmux -S "$decoy" capture-pane -p -t =decoy-live 2>/dev/null | grep -q pwned || rc=$?
  check "send-keys never reached the decoy pane" 1 "$rc"
  env -u TMUX -u TMUX_PANE tmux -S "$decoy" kill-server 2>/dev/null || true
else
  echo "  note  tmux not installed: the private-server cells were NOT run"
fi

echo "test isolation: $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
