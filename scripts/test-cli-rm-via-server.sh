#!/usr/bin/env bash
# `amux rm` on a tmux-backed worker retires it through the server (DELETE
# /api/sessions/<name>), because a worker pane's tmux guard refuses every
# kill-session (maxtongwang/ai-config#495). Drives the REAL `amux rm` against a
# fake server and a fake tmux that logs each call. Asserted: server success ->
# no tmux kill; server refusal (403) -> nonzero exit, error shown, no tmux kill,
# env file kept; non-JSON body -> same; server unreachable -> the old local path.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CLI="$ROOT/amux"
TMP="$(mktemp -d)"
SRV_PID=""
cleanup() {
  if [[ -n "$SRV_PID" ]]; then kill "$SRV_PID" 2>/dev/null || true; fi
  rm -rf -- "${TMP:?}"
}
trap cleanup EXIT
fails=0
check() { if [[ "$2" == "$3" ]]; then echo "  ok   $1"; else echo "  FAIL $1: expected '$2', got '$3'"; fails=$((fails+1)); fi; }

mkdir -p "$TMP/bin"
cat > "$TMP/bin/tmux" <<'EOF'
#!/bin/bash
echo "$*" >> "$TMUX_LOG"
exit 0
EOF
chmod +x "$TMP/bin/tmux"
export TMUX_LOG="$TMP/tmux.log"

# Fake server: answers DELETE /api/sessions/<name> per MODE_FILE (ok|forbid|html);
# records each request line in REQ_LOG.
cat > "$TMP/srv.py" <<'EOF'
import http.server, sys, os
class H(http.server.BaseHTTPRequestHandler):
    def do_DELETE(self):
        open(os.environ["REQ_LOG"], "a").write(f"DELETE {self.path} sess={self.headers.get('X-Amux-Session')}\n")
        mode = open(os.environ["MODE_FILE"]).read().strip()
        code, body = {"ok": (200, '{"ok":true,"message":"deleted"}'),
                      "forbid": (403, '{"error":"agents cannot delete sessions"}'),
                      "html": (502, '<html>bad gateway</html>'),
                      "e500": (500, '{"error":"boom"}'),
                      "empty": (200, ''),
                      "slow": (200, '{"ok":true}')}[mode]
        if mode == "slow":
            import time; time.sleep(4)
        self.send_response(code); self.send_header("Content-Length", str(len(body))); self.end_headers()
        self.wfile.write(body.encode())
    def log_message(self, *a): pass
s = http.server.HTTPServer(("127.0.0.1", 0), H)
open(sys.argv[1], "w").write(str(s.server_port))
s.serve_forever()
EOF
export REQ_LOG="$TMP/req.log" MODE_FILE="$TMP/mode"
python3 "$TMP/srv.py" "$TMP/port" &
SRV_PID=$!
for _ in $(seq 50); do [[ -s "$TMP/port" ]] && break; sleep 0.1; done
PORT="$(cat "$TMP/port")"

export CC_HOME="$TMP/home"
mkdir -p "$CC_HOME/sessions"
register() { printf 'CC_DIR="%s"\n' "$TMP" > "$CC_HOME/sessions/$1.env"; }
run_rm() { # <api> <name>; sets OUT and RC
  : > "$TMUX_LOG"
  RC=0
  OUT=$(PATH="$TMP/bin:$PATH" AMUX_API="$1" AMUX_SESSION=boss AMUX_RM_TIMEOUT=1 bash "$CLI" rm "$2" 2>&1) || RC=$?
}
kills() { grep -c 'kill-session' "$TMUX_LOG" || true; }

echo "1. server removes the worker"
register w-ok; echo ok > "$MODE_FILE"; : > "$REQ_LOG"
run_rm "http://127.0.0.1:$PORT" w-ok
check "exit 0" "0" "$RC"
check "printed removed" "yes" "$([[ "$OUT" == *removed* ]] && echo yes || echo no)"
check "no tmux kill-session" "0" "$(kills)"
check "one DELETE with the caller header" "DELETE /api/sessions/w-ok sess=boss" "$(cat "$REQ_LOG")"

echo "2. server refuses (403): no fallback to a raw kill"
register w-no; echo forbid > "$MODE_FILE"
run_rm "http://127.0.0.1:$PORT" w-no
check "nonzero exit" "1" "$RC"
check "server error shown" "yes" "$([[ "$OUT" == *"agents cannot delete sessions"* ]] && echo yes || echo no)"
check "no tmux kill-session" "0" "$(kills)"
check "env file kept" "yes" "$([[ -f "$CC_HOME/sessions/w-no.env" ]] && echo yes || echo no)"

echo "3. non-JSON body is not success"
register w-html; echo html > "$MODE_FILE"
run_rm "http://127.0.0.1:$PORT" w-html
check "nonzero exit" "1" "$RC"
check "no tmux kill-session" "0" "$(kills)"
check "env file kept" "yes" "$([[ -f "$CC_HOME/sessions/w-html.env" ]] && echo yes || echo no)"

refused_case() { # <label> <mode> <name> <expected text>
  register "$3"; echo "$2" > "$MODE_FILE"
  run_rm "http://127.0.0.1:$PORT" "$3"
  check "$1: nonzero exit" "1" "$RC"
  check "$1: error shown" "yes" "$([[ "$OUT" == *"$4"* ]] && echo yes || echo no)"
  check "$1: no tmux kill-session" "0" "$(kills)"
  check "$1: env file kept" "yes" "$([[ -f "$CC_HOME/sessions/$3.env" ]] && echo yes || echo no)"
}
echo "3b. 5xx, empty body and timeout are refusals, not unreachable"
refused_case "HTTP 500 JSON" e500 w-500 "boom"
refused_case "empty body" empty w-empty "empty answer"
refused_case "timeout" slow w-slow "no usable answer"

echo "4. server unreachable (connection refused): the old local path"
register w-down
run_rm "http://127.0.0.1:9" w-down
check "exit 0" "0" "$RC"
check "local tmux kill-session ran" "1" "$(kills)"
check "env file removed locally" "no" "$([[ -f "$CC_HOME/sessions/w-down.env" ]] && echo yes || echo no)"

echo
if [[ "$fails" -eq 0 ]]; then echo "PASS: cli-rm-via-server - all checks passed"; exit 0; fi
echo "cli-rm-via-server: $fails check(s) FAILED"; exit 1
