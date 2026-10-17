#!/usr/bin/env bash
# AMUX-2620 — regression coverage for the AMUX-1888 shell-evaluation hazard.
#
# tests/test_bash_quoting.py + test_inline_arg_quoting.py covered this via a
# python parity harness and were deleted in the rust scrub. The hazard is
# unchanged: `amux send X "text with `backticks`"` is expanded by the SENDER'S
# shell before amux ever sees it. Measured blast radii on the real incident: a
# garbled message, a LEAKED CREDENTIAL, and a stray `git rebase --quit` on a
# shared checkout.
#
# WHAT THIS CAN AND CANNOT TEST, stated because the distinction is the whole
# point. amux cannot un-expand an inline argument — by the time the process
# starts, the shell is done. What amux CAN guarantee, and what this pins, is
# that --stdin/--file carry bytes through to the wire UNTOUCHED, and that
# nothing downstream re-evaluates them.
#
# Captures the real POST with a throwaway listener, and runs against a TEMP
# CC_HOME holding one fake worker: a test that delivers messages to the live
# fleet on every run is its own incident, and the CLI refuses an unknown worker
# before it ever POSTs.
set -euo pipefail
# Private tmux server, never the host fleet's (lib/test-isolation.sh).
# shellcheck source=lib/test-isolation.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib/test-isolation.sh"; amux_test_isolate_tmux "$(mktemp -d)" || exit 1
cd "$(dirname "$0")/.."
AMUX_BIN="${AMUX_BIN:-./amux}"   # override to run against a broken fixture
PASS=0; FAIL=0
# Isolated fleet: CC_HOME is overridable (amux:35), so nothing here can reach a
# real worker or appear in the roster.
FAKE_HOME=$(mktemp -d)
mkdir -p "$FAKE_HOME/sessions"
cat > "$FAKE_HOME/sessions/quoting-target.env" <<'ENVEOF'
CC_DIR="/tmp"
CC_PROVIDER="claude"
ENVEOF
# `amux send` refuses a worker that is not RUNNING before it POSTs, and running
# means `tmux has-session`. A stub on PATH satisfies that without a real pane,
# so the test exercises the shipped send path without touching tmux or a lane.
STUB_BIN=$(mktemp -d)
cat > "$STUB_BIN/tmux" <<'TMUXEOF'
#!/bin/bash
case "$1" in
  has-session) exit 0 ;;
  list-sessions) echo "quoting-target"; exit 0 ;;
  *) exit 0 ;;
esac
TMUXEOF
chmod +x "$STUB_BIN/tmux"
trap 'rm -rf "$FAKE_HOME" "$STUB_BIN"' EXIT
ok(){ echo "  ok   $1"; PASS=$((PASS+1)); }
bad(){ echo "  FAIL $1"; echo "       $2"; FAIL=$((FAIL+1)); }

# The payload. Every metacharacter that fired in AMUX-1888.
PAYLOAD='hello `date` $(whoami) ${HOME} "quoted" '"'"'single'"'"' \back\slash'

start_listener() {
  CAP=$(mktemp); PORTF=$(mktemp)
  python3 - "$CAP" "$PORTF" <<'PY' &
import sys, http.server, socketserver, threading
cap, portf = sys.argv[1], sys.argv[2]
class H(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        n = int(self.headers.get('Content-Length') or 0)
        open(cap, 'wb').write(self.rfile.read(n))
        self.send_response(200); self.send_header('Content-Type','application/json')
        self.end_headers(); self.wfile.write(b'{"ok":true,"message":"captured"}')
    def log_message(self, *a): pass
with socketserver.TCPServer(("127.0.0.1", 0), H) as s:
    open(portf, "w").write(str(s.server_address[1]))
    s.handle_request()
PY
  LPID=$!
  # If the CLI never POSTs (e.g. it refused the worker), the listener would
  # block forever and wedge the run — which is exactly what the first version
  # of this script did.
  ( sleep 25; kill $LPID 2>/dev/null ) 2>/dev/null & WPID=$!
  disown $WPID 2>/dev/null || true
  for _ in $(seq 1 50); do [ -s "$PORTF" ] && break; sleep 0.1; done
  PORT=$(cat "$PORTF")
}

# Returns the `text` field the CLI actually put on the wire.
sent_text() {
  python3 -c "
import json,sys
try: print(json.load(open('$CAP'))['text'], end='')
except Exception as e: print('<<UNPARSEABLE: %s>>' % e, end='')
"
}

# ── 1. --stdin ──────────────────────────────────────────────────────────────
start_listener
printf '%s' "$PAYLOAD" | timeout 20 env PATH="$STUB_BIN:$PATH" CC_HOME="$FAKE_HOME" AMUX_API="http://127.0.0.1:$PORT" \
  AMUX_SESSION=quoting-test bash "$AMUX_BIN" send quoting-target --no-board --stdin >/dev/null 2>&1
GOT=$(sent_text); kill $WPID 2>/dev/null || true; wait $LPID 2>/dev/null
[ "$GOT" = "$PAYLOAD" ] && ok "--stdin carries bytes literally" \
  || bad "--stdin mangled the payload" "want: $PAYLOAD
       got:  $GOT"

# ── 2. --file ───────────────────────────────────────────────────────────────
start_listener
MSGF=$(mktemp); printf '%s' "$PAYLOAD" > "$MSGF"
timeout 20 env PATH="$STUB_BIN:$PATH" CC_HOME="$FAKE_HOME" AMUX_API="http://127.0.0.1:$PORT" \
  AMUX_SESSION=quoting-test bash "$AMUX_BIN" send quoting-target --no-board --file "$MSGF" >/dev/null 2>&1
GOT=$(sent_text); kill $WPID 2>/dev/null || true; wait $LPID 2>/dev/null
[ "$GOT" = "$PAYLOAD" ] && ok "--file carries bytes literally" \
  || bad "--file mangled the payload" "want: $PAYLOAD
       got:  $GOT"

# ── 3. inline argument, ALREADY-literal text ────────────────────────────────
# The shell cannot be un-done, so this asserts the half amux owns: text that
# reaches amux with literal metacharacters must not be re-evaluated on its way
# to the wire. Single-quoted here, so the shell hands the bytes over intact.
start_listener
timeout 20 env PATH="$STUB_BIN:$PATH" CC_HOME="$FAKE_HOME" AMUX_API="http://127.0.0.1:$PORT" \
  AMUX_SESSION=quoting-test bash "$AMUX_BIN" send quoting-target --no-board 'inline `date` $(whoami) ${HOME} "q" \back' >/dev/null 2>&1
GOT=$(sent_text); kill $WPID 2>/dev/null || true; wait $LPID 2>/dev/null
# The double-quote and backslash are LOAD-BEARING. Without them a
# string-interpolated body still yields valid JSON, so this case passed against
# the very implementation it exists to reject — verified: with the original
# payload the broken fixture failed --stdin and --file but this one went green.
[ "$GOT" = 'inline `date` $(whoami) ${HOME} "q" \back' ] && ok "inline arg is not re-evaluated by amux" \
  || bad "inline arg was re-evaluated or mangled" "got: $GOT"

echo
echo "  $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
