#!/usr/bin/env bash
# Worker-token recovery for the status hook: a lane whose env lacks AMUX_WORKER_TOKEN
# must still send X-Amux-Worker-Token, read from its own tmux session environment.
# Drives the shipped hook against a stub server and a fake tmux on PATH.
set -Eeuo pipefail
trap 'echo "FAIL hook-report worker token line=$LINENO command=$BASH_COMMAND" >&2' ERR
cd "$(dirname "$0")/.."
TMP="$(mktemp -d)"
SERVER_PID=""
trap '[ -z "$SERVER_PID" ] || kill "$SERVER_PID" 2>/dev/null || true; rm -rf "$TMP"' EXIT
SECRET="tok-0123456789abcdef"

cat > "$TMP/server.py" <<'PY'
import http.server,json,socketserver,sys
capture,port_file=sys.argv[1:3]
class H(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        self.rfile.read(int(self.headers.get("content-length","0")))
        row={"token":self.headers.get("X-Amux-Worker-Token")}
        with open(capture,"a") as s: s.write(json.dumps(row)+"\n")
        self.send_response(200); self.end_headers(); self.wfile.write(b"{}")
    def log_message(self,*a): pass
class S(socketserver.TCPServer): allow_reuse_address=True
with S(("127.0.0.1",0),H) as srv:
    open(port_file,"w").write(str(srv.server_address[1]))
    srv.serve_forever()
PY
/usr/bin/python3 "$TMP/server.py" "$TMP/requests.jsonl" "$TMP/port" &
SERVER_PID=$!
for _ in $(seq 1 100); do [ -s "$TMP/port" ] && break; sleep .02; done
URL="http://127.0.0.1:$(cat "$TMP/port")"

mkdir -p "$TMP/bin"
cat > "$TMP/bin/tmux" <<'SH'
#!/bin/sh
case "$1" in
  display-message) echo amux-lane1 ;;
  show-environment)
    case "$FAKE_TMUX_MODE" in
      token) echo "AMUX_WORKER_TOKEN=$FAKE_TMUX_TOKEN" ;;
      absent) echo "-AMUX_WORKER_TOKEN" ;;
      error) echo "no such session" >&2; exit 1 ;;
    esac ;;
esac
SH
chmod +x "$TMP/bin/tmux"

# run_hook <mode> <env-token|""> <path: drain|curl>; echoes the token the server saw
run_hook() {
  local mode="$1" envtok="$2" path="$3" home="$TMP/home-$1-$3-${2:+env}"
  mkdir -p "$home/.amux/sessions" "$home/.amux/logs"
  : > "$home/.amux/sessions/lane1.env"
  # A regular file where the queue dir should be makes enqueue fail, so the hook
  # falls through to its immediate curl send.
  [ "$path" != curl ] || : > "$home/.amux/hook-report-queue"
  rm -f "$TMP/requests.jsonl"
  env -u AMUX_WORKER_TOKEN HOME="$home" PATH="$TMP/bin:$PATH" AMUX_URL="$URL" \
    AMUX_SESSION=lane1 TMUX_PANE=%1 FAKE_TMUX_MODE="$mode" FAKE_TMUX_TOKEN="$SECRET" \
    ${envtok:+AMUX_WORKER_TOKEN="$envtok"} \
    bash scripts/hooks/hook-report.sh active prompt-hook <<<'{}' 2>/dev/null
  for _ in $(seq 1 200); do [ -s "$TMP/requests.jsonl" ] && break; sleep .05; done
  LAST_HOME="$home"
  [ -s "$TMP/requests.jsonl" ] || { echo "NOREQUEST"; return; }
  /usr/bin/python3 -c 'import json,sys; print(json.loads(open(sys.argv[1]).readline())["token"])' "$TMP/requests.jsonl"
}

for path in drain curl; do
  got=$(run_hook token "" "$path"); [ "$got" = "$SECRET" ] || { echo "FAIL $path token present: got '$got'" >&2; exit 1; }
  echo "ok   $path: token recovered from tmux session env is sent"
  got=$(run_hook token "env-supplied" "$path"); [ "$got" = env-supplied ] || { echo "FAIL $path env wins: got '$got'" >&2; exit 1; }
  echo "ok   $path: a token already in the env is not replaced"
  got=$(run_hook absent "" "$path"); [ "$got" = None ] || { echo "FAIL $path absent: got '$got'" >&2; exit 1; }
  echo "ok   $path: tmux without the variable sends no header"
  got=$(run_hook error "" "$path"); [ "$got" = None ] || { echo "FAIL $path tmux error: got '$got'" >&2; exit 1; }
  echo "ok   $path: a tmux error sends no header and still reports"
done

# The value is never written to the hook's logs.
if grep -rqs "$SECRET" "$TMP"/home-*/.amux/logs; then echo "FAIL token value reached a log" >&2; exit 1; fi
echo "ok   token value is never logged"
echo "PASS hook-report worker token"
