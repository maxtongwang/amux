#!/usr/bin/env bash
# `amux register`, `amux exec` and `amux send --keys` go through the server.
#
# THE DEFECT: register and exec wrote ~/.amux/sessions/<name>.env themselves,
# and --keys ran `tmux send-keys` into the target's pane. Every server rule
# was bypassed: only leads create workers (session_verbs::worker_create_refusal),
# and a worker may act only inside its groups. A worker in any group could
# mint a peer, or press y/n/Enter in another group's worker.
#
# This cell runs the real CLI against a loopback recorder that plays the
# server: it refuses creates from the non-lead worker `dev-a`, answers 409 for
# an existing name, and accepts everything else. Asserted: the requests the
# CLI sends, the refusal it prints, its exit status, and that NO session file
# appears and tmux is never asked to send keys, whatever the server says.
#
# Usage: scripts/test-cli-register-through-server.sh   (AMUX_BIN=<cli> to test
# another copy, e.g. the base revision to prove the cell fails there).
# Exit 0 = all pass, 1 = a failure.
set -euo pipefail

cd "$(dirname "$0")/.."
AMUX_BIN="${AMUX_BIN:-./amux}"
PASS=0; FAIL=0

TMP=$(mktemp -d); [ -n "${KEEP:-}" ] && echo "TMP=$TMP"
trap '[ -z "${KEEP:-}" ] && rm -rf "$TMP"; [ -n "${REC_PID:-}" ] && kill "$REC_PID" 2>/dev/null || true' EXIT
mkdir -p "$TMP/.amux/sessions" "$TMP/bin" "$TMP/work"

# HERMETIC, OR IT TALKS TO THE LIVE SERVER. The CLI reads CC_HOME before HOME
# and heals a loopback AMUX_API to $CC_HOME/endpoint.json's canonical_url, so a
# run inside a lane (CC_HOME=~/.amux exported) sent every request below to the
# real server and created real workers (found writing this cell, 2026-10-16).
# Pin CC_HOME into the temp home, drop every lane identity and tmux handle,
# and refuse to run if the CLI could still resolve a real endpoint.
export HOME="$TMP" CC_HOME="$TMP/.amux"
unset AMUX_SESSION AMUX_WORKER AMUX_WORKER_TOKEN TMUX TMUX_PANE AMUX_HOME 2>/dev/null || true
[ ! -e "$CC_HOME/endpoint.json" ] || { echo "FAIL: refusing to run with an endpoint.json in $CC_HOME"; exit 1; }

# A tmux that records instead of acting, so a raw pane write is observable.
cat > "$TMP/bin/tmux" <<SH
#!/usr/bin/env bash
echo "\$*" >> "$TMP/tmux.log"
case "\$1" in has-session|list-sessions|list-panes) exit 1 ;; esac
exit 0
SH
chmod +x "$TMP/bin/tmux"

cat > "$TMP/recorder.py" <<'PY'
import json, sys
from http.server import BaseHTTPRequestHandler, HTTPServer

LOG = sys.argv[1]
EXISTING = {"old-one"}

class H(BaseHTTPRequestHandler):
    def _body(self):
        n = int(self.headers.get("Content-Length") or 0)
        raw = self.rfile.read(n)
        try:
            return json.loads(raw or b"null")
        except Exception:
            return raw.decode("utf-8", "replace")
    def _answer(self, code, obj):
        b = json.dumps(obj).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(b)))
        self.end_headers()
        self.wfile.write(b)
    def _handle(self):
        body = self._body()
        who = self.headers.get("X-Amux-Session", "")
        with open(LOG, "a") as f:
            f.write(json.dumps({"method": self.command, "path": self.path, "who": who, "body": body}) + "\n")
        if self.command == "POST" and self.path == "/api/sessions":
            if who == "dev-a":
                return self._answer(403, {"ok": False, "code": "worker_create_requires_lead",
                    "error": "Only leads can create workers; 'dev-a' is not in the 'leads' group."})
            if isinstance(body, dict) and body.get("name") in EXISTING:
                return self._answer(409, {"error": "session '%s' already exists" % body["name"]})
            return self._answer(201, {"ok": True, "name": body.get("name")})
        if self.command == "PUT" and self.path == "/api/scope":
            if who == "dev-a":
                return self._answer(403, {"error": "'dev-a' may not configure another worker ('old-one')."})
            return self._answer(200, {"ok": True})
        if self.command == "POST" and self.path.endswith("/keys"):
            if who == "dev-a":
                return self._answer(403, {"ok": False, "code": "worker_group_boundary", "error": "worker group boundary refused"})
            return self._answer(200, {"ok": True, "accepted": True})
        return self._answer(404, {"error": "not found"})
    do_POST = do_PUT = do_GET = do_PATCH = _handle
    def log_message(self, *a):
        pass

srv = HTTPServer(("127.0.0.1", 0), H)
print(srv.server_address[1], flush=True)
srv.serve_forever()
PY

python3 "$TMP/recorder.py" "$TMP/requests.jsonl" > "$TMP/port" 2>"$TMP/recorder.err" &
REC_PID=$!
PORT=""
for _ in $(seq 1 100); do
  PORT=$(head -1 "$TMP/port" 2>/dev/null || true)
  [ -n "$PORT" ] && break
  sleep 0.05
done
[ -n "$PORT" ] || { echo "FAIL: the recorder never reported a port"; head -5 "$TMP/recorder.err" || true; exit 1; }

export AMUX_API="http://127.0.0.1:$PORT" AMUX_URL="http://127.0.0.1:$PORT"
export PATH="$TMP/bin:$PATH"
unset AMUX_SESSION AMUX_WORKER AMUX_WORKER_TOKEN TMUX 2>/dev/null || true

check() { if [ "$2" = "1" ]; then PASS=$((PASS+1)); else FAIL=$((FAIL+1)); echo "  FAIL  $1"; fi; }
# Did the recorder see a request matching a python predicate over r?
saw() {
  python3 -c '
import json, sys
rows = [json.loads(l) for l in open(sys.argv[1])] if __import__("os").path.exists(sys.argv[1]) else []
print("1" if any(eval(sys.argv[2], {"r": r}) for r in rows) else "0")
' "$TMP/requests.jsonl" "$1"
}
no_file() { [ -e "$TMP/.amux/sessions/$1.env" ] && echo 0 || echo 1; }
run() { # who, args... -> sets RC, OUT
  local who="$1"; shift
  RC=0
  if [ -n "$who" ]; then
    OUT=$(HOME="$TMP" AMUX_SESSION="$who" AMUX_WORKER="$who" "$AMUX_BIN" "$@" 2>&1) || RC=$?
  else
    OUT=$(HOME="$TMP" "$AMUX_BIN" "$@" 2>&1) || RC=$?
  fi
}

echo "CLI register/exec/keys go through the server"
echo

# 1. The owner registers a new worker: one create request, no file written by the CLI.
run "" register fresh-one --dir "$TMP/work" --model opus
check "owner register exits 0 ($OUT)" "$([ "$RC" = 0 ] && echo 1 || echo 0)"
check "owner register POSTs /api/sessions with name, dir, flags and start:false" \
  "$(saw 'r["method"]=="POST" and r["path"]=="/api/sessions" and r["who"]=="" and r["body"]["name"]=="fresh-one" and r["body"]["dir"]=="'"$TMP/work"'" and r["body"]["flags"]=="--model opus" and r["body"]["start"] is False')"
check "owner register writes no session file itself" "$(no_file fresh-one)"

# 2. A non-lead worker: the server's refusal is printed, the exit is nonzero, nothing is written.
run dev-a register minted --dir "$TMP/work"
check "worker register exits nonzero" "$([ "$RC" != 0 ] && echo 1 || echo 0)"
check "worker register prints the server's refusal ($OUT)" "$(printf '%s' "$OUT" | grep -q "Only leads can create workers" && echo 1 || echo 0)"
check "worker register sent its identity" "$(saw 'r["path"]=="/api/sessions" and r["who"]=="dev-a" and r["body"]["name"]=="minted"')"
check "refused worker register writes no session file" "$(no_file minted)"

# 3. Re-registering an existing worker is a key merge through /api/scope.
run "" register old-one --dir "$TMP/work" --worktree
check "re-register exits 0 ($OUT)" "$([ "$RC" = 0 ] && echo 1 || echo 0)"
check "re-register PUTs a worker env merge of CC_DIR/CC_FLAGS/CC_WORKTREE" \
  "$(saw 'r["method"]=="PUT" and r["path"]=="/api/scope" and r["body"]["level"]=="worker" and r["body"]["name"]=="old-one" and r["body"]["capability"]=="env" and r["body"]["value"]=={"CC_DIR":"'"$TMP/work"'","CC_FLAGS":"","CC_WORKTREE":"1"}')"
check "re-register writes no session file itself" "$(no_file old-one)"

# 4. exec: the worker's create is refused, so nothing is written and nothing starts.
run dev-a exec minted-exec --dir "$TMP/work"
check "worker exec exits nonzero" "$([ "$RC" != 0 ] && echo 1 || echo 0)"
check "worker exec asked the server" "$(saw 'r["path"]=="/api/sessions" and r["who"]=="dev-a" and r["body"]["name"]=="minted-exec"')"
check "refused worker exec writes no session file" "$(no_file minted-exec)"
check "refused worker exec never reached tmux new-session" \
  "$(grep -q 'new-session.*minted-exec' "$TMP/tmux.log" 2>/dev/null && echo 0 || echo 1)"
# The owner's exec registers through the server too (the start that follows
# is not this cell's subject; the recorder created no file for it to start).
run "" exec owner-exec --dir "$TMP/work"
check "owner exec POSTs /api/sessions" "$(saw 'r["path"]=="/api/sessions" and r["who"]=="" and r["body"]["name"]=="owner-exec" and r["body"]["start"] is False')"
check "owner exec writes no session file itself" "$(no_file owner-exec)"

# 5. --keys: one server call per key, in order; tmux is never asked to send keys.
printf 'CC_DIR="%s"\n' "$TMP/work" > "$TMP/.amux/sessions/lane.env"
run "" send lane --keys Down Enter
check "owner keys exit 0 ($OUT)" "$([ "$RC" = 0 ] && echo 1 || echo 0)"
check "keys POST /api/sessions/lane/keys for Down" "$(saw 'r["path"]=="/api/sessions/lane/keys" and r["body"]=={"keys":"Down"}')"
check "keys POST /api/sessions/lane/keys for Enter" "$(saw 'r["path"]=="/api/sessions/lane/keys" and r["body"]=={"keys":"Enter"}')"
run dev-a send lane --keys y
check "worker keys refused by the server exit nonzero" "$([ "$RC" != 0 ] && echo 1 || echo 0)"
check "worker keys carried the worker identity" "$(saw 'r["path"]=="/api/sessions/lane/keys" and r["who"]=="dev-a"')"
check "no key was ever sent with tmux send-keys" \
  "$(grep -q 'send-keys' "$TMP/tmux.log" 2>/dev/null && echo 0 || echo 1)"

echo
echo "passed: $PASS  failed: $FAIL"
[ "$FAIL" -eq 0 ]
