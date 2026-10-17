#!/usr/bin/env bash
# `amux board needsyou` moves a card under the needs:you tag rule.
#
# THE BUG. The verb wrote the needs:you tag first and returned on its refusal.
# Since f4167abf the server refuses that tag on any card not already in needsyou
# (needsyou_tag_requires_status), so the verb could not move any card, and the
# refusal told the caller to run the same verb (MF-1173).
#
# WHAT IS PINNED. A throwaway listener enforces the server rule: a PATCH carrying
# a needs:you tag is refused with 409 unless the card is in needsyou. Cells:
#   - a blocked card moves: exit 0, writes in the order ask, status, tag;
#   - a refused status move: exit non-zero, no tag write, and the message says the
#     ask was recorded and the move was not.
# Nothing here touches a real board.
set -euo pipefail
# Private tmux server, never the host fleet's (lib/test-isolation.sh).
# shellcheck source=lib/test-isolation.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib/test-isolation.sh"; amux_test_isolate_tmux "$(mktemp -d)" || exit 1
cd "$(dirname "$0")/.."
AMUX_BIN="${AMUX_BIN:-./amux}"
PASS=0; FAIL=0
ok(){ echo "  ok   $1"; PASS=$((PASS+1)); }
bad(){ echo "  FAIL $1"; echo "       $2"; FAIL=$((FAIL+1)); }

TMPD=$(mktemp -d)
trap 'kill $LPID 2>/dev/null; rm -rf "$TMPD"' EXIT

# State file: the card's status, and whether status moves are refused.
python3 - "$TMPD" <<'PY' &
import sys, json, os, http.server, socketserver
d = sys.argv[1]
state_f, log_f, port_f = os.path.join(d, "state.json"), os.path.join(d, "patches.log"), os.path.join(d, "port")
def state():
    return json.load(open(state_f))
class H(http.server.BaseHTTPRequestHandler):
    def _send(self, code, obj):
        b = json.dumps(obj).encode()
        self.send_response(code); self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(b))); self.end_headers(); self.wfile.write(b)
    def do_GET(self):
        st = state()
        self._send(200, {"id": "TEST-1", "status": st["status"], "tags": st.get("tags", [])})
    def do_PATCH(self):
        n = int(self.headers.get("Content-Length") or 0)
        body = json.loads(self.rfile.read(n) or b"{}")
        st = state()
        kind = "tag" if "tags" in body else ("status" if "status" in body else ("ask" if "ask_type" in body else "other"))
        with open(log_f, "a") as f: f.write(kind + "\n")
        if kind == "tag" and "needs:you" in body["tags"] and st["status"] != "needsyou":
            return self._send(409, {"blocked": True, "code": "needsyou_tag_requires_status",
                                    "error": "a needs:you tag requires the needsyou status", "item": "TEST-1", "ok": False})
        if kind == "status":
            if st.get("refuse_status"):
                return self._send(409, {"attempted_status": body["status"], "blocked": True, "error": "gate not acknowledged",
                                        "gate": ["Something true"], "item": "TEST-1", "ok": False})
            st["status"] = body["status"]; json.dump(st, open(state_f, "w"))
            return self._send(200, {"ok": True, "id": "TEST-1", "status": st["status"]})
        if kind == "tag":
            st["tags"] = body["tags"]; json.dump(st, open(state_f, "w"))
        self._send(200, {"ok": True, "id": "TEST-1", "status": st["status"]})
    def log_message(self, *a): pass
class S(socketserver.TCPServer):
    allow_reuse_address = True
json.dump({"status": "blocked"}, open(state_f, "w"))
with S(("127.0.0.1", 0), H) as srv:
    open(port_f, "w").write(str(srv.server_address[1]))
    srv.serve_forever()
PY
LPID=$!
disown $LPID 2>/dev/null || true
for _ in $(seq 1 50); do [ -s "$TMPD/port" ] && break; sleep 0.1; done
[ -s "$TMPD/port" ] || { echo "FAIL: listener never started"; exit 1; }
PORT=$(cat "$TMPD/port")

run_needsyou() {  # run_needsyou <refuse_status true|false> -> sets RC, OUT, ORDER
  python3 -c "import json,sys; json.dump({'status':'blocked','refuse_status':sys.argv[1]=='true'}, open(sys.argv[2],'w'))" "$1" "$TMPD/state.json"
  : > "$TMPD/patches.log"
  if OUT=$(timeout 30 env AMUX_API="http://127.0.0.1:$PORT" AMUX_SESSION=needsyou-order-test CC_HOME="$TMPD" \
      bash "$AMUX_BIN" board needsyou TEST-1 --actor Ethan --ask "${2:-decision}" \
      --question "Should this ship?" --unblocks "The lane ships it." 2>&1); then RC=0; else RC=$?; fi
  ORDER=$(tr '\n' ' ' < "$TMPD/patches.log" | sed 's/ $//')
}

echo "amux board needsyou write order"

run_needsyou false
[ "$RC" -eq 0 ] && ok "a blocked card moves to needsyou (exit 0)" || bad "a blocked card moves to needsyou (exit 0)" "exit $RC; order: $ORDER; output: $OUT"
[ "$ORDER" = "ask status tag" ] && ok "writes go ask, status, tag" || bad "writes go ask, status, tag" "order: $ORDER"
case "$(cat "$TMPD/state.json")" in *'"needs:you"'*) ok "the card ends tagged needs:you" ;;
  *) bad "the card ends tagged needs:you" "state: $(cat "$TMPD/state.json")" ;; esac

run_needsyou true
[ "$RC" -ne 0 ] && ok "a refused status move exits non-zero" || bad "a refused status move exits non-zero" "exit 0; order: $ORDER"
[ "$ORDER" = "ask status" ] && ok "a refused move sends no tag write" || bad "a refused move sends no tag write" "order: $ORDER"
case "$OUT" in *"typed ask was recorded"*"STATUS MOVE was refused"*) ok "a refused move says the ask was recorded and the move was not" ;;
  *) bad "a refused move says the ask was recorded and the move was not" "output: $OUT" ;; esac

for kind in budget customer_outbound; do
  run_needsyou false "$kind"
  [ "$RC" -eq 0 ] && [ "$ORDER" = "ask status tag" ] && ok "$kind reaches the server through the normal CLI" || bad "$kind reaches the server through the normal CLI" "$RC: $OUT; $ORDER"
done
run_needsyou false blocked
[ "$RC" -ne 0 ] && [ -z "$ORDER" ] && ok "unknown category is refused before any write" || bad "unknown category is refused before any write" "$RC: $OUT; $ORDER"
echo "passed $PASS, failed $FAIL"
[ "$FAIL" -eq 0 ]
