#!/usr/bin/env bash
# AMUX-4652: `amux board needs <ID> "<need>"` reports a refused blocker link.
#
# THE BUG. board needs creates a blocker task, then PATCHes <ID>'s depends_on to
# link it. The PATCH answer was captured into linkresp and never read (shellcheck
# SC2034), so "reported blocker: <ID> now depends on new task ..." printed even
# when the board refused the link or ignored depends_on, and <ID> never parked.
#
# WHAT IS PINNED, both directions. A refused link and an ignored depends_on must
# exit non-zero without the success line. An accepted link must print it and exit
# 0, so a check that refuses everything cannot pass.
#
# Runs against a throwaway listener on a random port: POST (create) answers with a
# new id, GET answers with the card, PATCH (link) answers with the body in a file.
set -euo pipefail
# Private tmux server, never the host fleet's (lib/test-isolation.sh).
# shellcheck source=lib/test-isolation.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib/test-isolation.sh"; amux_test_isolate_tmux "$(mktemp -d)" || exit 1
cd "$(dirname "$0")/.."
AMUX_BIN="${AMUX_BIN:-./amux}"
PASS=0; FAIL=0
ok(){ echo "  ok   $1"; PASS=$((PASS+1)); }
bad(){ echo "  FAIL $1"; echo "       $2"; FAIL=$((FAIL+1)); }

LINK=$(mktemp); PORTF=$(mktemp); OUTF=$(mktemp)
trap 'kill $LPID 2>/dev/null; rm -f "$LINK" "$PORTF" "$OUTF"' EXIT

python3 - "$LINK" "$PORTF" <<'PY' &
import sys, json, http.server, socketserver
link, portf = sys.argv[1], sys.argv[2]
class H(http.server.BaseHTTPRequestHandler):
    def _send(self, code, body):
        b = body if isinstance(body, bytes) else json.dumps(body).encode()
        self.send_response(code); self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(b))); self.end_headers(); self.wfile.write(b)
    def _drain(self):
        n = int(self.headers.get('Content-Length') or 0)
        if n: self.rfile.read(n)
    def do_POST(self):
        self._drain(); self._send(201, {"ok": True, "id": "TEST-2", "status": "todo"})
    def do_GET(self):
        self._send(200, {"id": "TEST-1", "status": "doing", "depends_on": []})
    def do_PATCH(self):
        self._drain()
        raw = open(link, 'rb').read().split(b'\n', 1)
        self._send(int(raw[0] or b'200'), raw[1] if len(raw) > 1 else b'{}')
    def log_message(self, *a): pass
class S(socketserver.TCPServer):
    allow_reuse_address = True
with S(("127.0.0.1", 0), H) as s:
    open(portf, "w").write(str(s.server_address[1]))
    s.serve_forever()
PY
LPID=$!
disown $LPID 2>/dev/null || true
for _ in $(seq 1 50); do [ -s "$PORTF" ] && break; sleep 0.1; done
PORT=$(cat "$PORTF")
[ -n "${PORT:-}" ] || { echo "listener never bound"; exit 1; }

run_needs() {  # run_needs <http status> <json body for the link PATCH>  -> sets RC and OUT
  printf '%s\n%s' "$1" "$2" > "$LINK"
  if timeout 20 env AMUX_API="http://127.0.0.1:$PORT" AMUX_URL="http://127.0.0.1:$PORT" AMUX_SESSION=needs-link-test CC_HOME="$(dirname "$OUTF")" \
      bash "$AMUX_BIN" board needs TEST-1 "a blocker for the test" >"$OUTF" 2>&1; then RC=0; else RC=$?; fi
  OUT=$(cat "$OUTF")
}

echo "amux board needs link write (AMUX-4652)"

run_needs 409 '{"blocked":true,"error":"rev mismatch","ok":false}'
[ "$RC" -ne 0 ] && ok "a refused link exits non-zero" || bad "a refused link exits non-zero" "exit 0; output: $OUT"
case "$OUT" in *"reported blocker"*) bad "a refused link never claims the link" "output: $OUT" ;;
  *) ok "a refused link never claims the link" ;; esac

run_needs 200 '{"ok":true,"id":"TEST-1","status":"doing","ignored_fields":["depends_on"]}'
[ "$RC" -ne 0 ] && ok "an ignored depends_on exits non-zero" || bad "an ignored depends_on exits non-zero" "exit 0; output: $OUT"
case "$OUT" in *"reported blocker"*) bad "an ignored depends_on never claims the link" "output: $OUT" ;;
  *) ok "an ignored depends_on never claims the link" ;; esac

run_needs 200 '{"ok":true,"id":"TEST-1","status":"doing","depends_on":["TEST-2"]}'
[ "$RC" -eq 0 ] && ok "an accepted link exits 0" || bad "an accepted link exits 0" "exit $RC; output: $OUT"
case "$OUT" in *"reported blocker: TEST-1 now depends on new task TEST-2"*) ok "an accepted link reports it" ;;
  *) bad "an accepted link reports it" "output: $OUT" ;; esac

echo "passed $PASS, failed $FAIL"
[ "$FAIL" -eq 0 ]
