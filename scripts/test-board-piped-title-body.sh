#!/usr/bin/env bash
# AMUX-4705 — a piped brief is a TITLE AND A BODY, not one enormous title.
#
# THE DEFECT: `--stdin` and `--file` set the title, which is what their usage lines
# say and is exactly the trap. The thing a caller pipes into a card verb is the whole
# brief, and piping is the SANCTIONED way to pass text that must not be shell-evaluated
# (AMUX-1888), so following the documented advice produced a card whose title was a
# paragraph and whose description was empty.
#
# Measured 2026-09-15, two lanes inside one hour. mixpeek-finances hit it on all six
# requests they re-issued onto the new routing (TG-3756 pre-repair: title
# "Full scope, gates, evidence...", desc len 0) and repaired each by PATCH. amux hit it
# on AT-24, compounded by passing --stdin and --desc-stdin together, which both call
# `cat` so the second read nothing.
#
# Asserts the BYTES POSTED, against a loopback recorder, because the split happens in
# the CLI and no server test can see it.
#
# Exit 0 = all pass, 1 = a failure.
set -euo pipefail

cd "$(dirname "$0")/.."
AMUX_BIN="${AMUX_BIN:-./amux}"
PASS=0; FAIL=0

TMP=$(mktemp -d); trap 'rm -rf "$TMP"; [ -n "${REC_PID:-}" ] && kill "$REC_PID" 2>/dev/null || true' EXIT
# Hermetic: never the live server or the real ~/.amux (lib/test-isolation.sh).
# shellcheck source=lib/test-isolation.sh
source scripts/lib/test-isolation.sh; amux_test_isolate "$TMP" || exit 1

cat > "$TMP/recorder.py" <<'PY'
import json, sys
from http.server import BaseHTTPRequestHandler, HTTPServer
OUT = sys.argv[1]

class H(BaseHTTPRequestHandler):
    def do_POST(self):
        n = int(self.headers.get("Content-Length") or 0)
        with open(OUT, "wb") as f:
            f.write(self.rfile.read(n))
        body = json.dumps({"id": "LB-1", "status": "todo", "session": "piptest"}).encode()
        self.send_response(201)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)
    def log_message(self, *a):
        pass

srv = HTTPServer(("127.0.0.1", 0), H)
print(srv.server_address[1], flush=True)
srv.serve_forever()
PY

python3 "$TMP/recorder.py" "$TMP/body.json" > "$TMP/port" 2>"$TMP/recorder.err" &
REC_PID=$!
PORT=""
for _ in $(seq 1 100); do
  PORT=$(head -1 "$TMP/port" 2>/dev/null || true)
  [ -n "$PORT" ] && break
  sleep 0.05
done
if [ -z "$PORT" ]; then
  echo "FAIL: the recorder never reported a port"
  head -5 "$TMP/recorder.err" || true
  exit 1
fi

export AMUX_SESSION="piptest"
export AMUX_WORKER="piptest"
export AMUX_API="http://127.0.0.1:$PORT"
export AMUX_URL="http://127.0.0.1:$PORT"

check() { # label result
  if [ "$2" = "1" ]; then PASS=$((PASS+1)); else FAIL=$((FAIL+1)); echo "  FAIL  $1"; fi
}
says() {
  python3 -c '
import json, sys
b = json.load(open(sys.argv[1]))
print("1" if eval(sys.argv[2], {"b": b}) else "0")
' "$TMP/body.json" "$1"
}

echo "a piped brief splits into a title and a body (AMUX-4705)"
echo

# (a) THE SPECIMEN SHAPE: a heredoc carrying a headline and a scope, which is what
#     mixpeek-finances piped six times.
rm -f "$TMP/body.json"
HOME="$TMP" "$AMUX_BIN" board request lane-b --stdin >"$TMP/o1" 2>&1 <<'EOF'
WS7: ts-batch-drivers n2-standard-8 -> n2-standard-4
Full scope, gates, evidence and acceptance below.

The rehearsal has to run from scratch on a clean machine.
EOF
check "title is the first line only"      "$(says 'b["title"] == "WS7: ts-batch-drivers n2-standard-8 -> n2-standard-4"')"
check "title carries no newline"          "$(says '"\n" not in b["title"]')"
check "the rest became the description"   "$(says 'b.get("desc","").startswith("Full scope, gates, evidence")')"
check "the last line survives too"        "$(says '"clean machine" in b.get("desc","")')"
check "request_to still routes"           "$(says 'b.get("request_to") == "lane-b"')"
# IT SAYS WHAT IT DID. A silent reinterpretation teaches the interface wrong twice.
grep -q "first line is the title" "$TMP/o1" && PASS=$((PASS+1)) || { FAIL=$((FAIL+1)); echo "  FAIL  the split is not announced on stderr"; }

# (b) THE DISCRIMINATION. A single-line title is untouched, and its desc stays
#     empty. Without this cell the split could swallow every title's tail and
#     nothing above would notice.
rm -f "$TMP/body.json"
HOME="$TMP" "$AMUX_BIN" board request lane-b "one line only" >"$TMP/o2" 2>&1 || true
check "a single-line title is unchanged"  "$(says 'b["title"] == "one line only"')"
check "and gets no invented description"  "$(says 'not b.get("desc")')"
grep -q "first line is the title" "$TMP/o2" && { FAIL=$((FAIL+1)); echo "  FAIL  announced a split that did not happen"; } || PASS=$((PASS+1))

# (c) AN EXPLICIT --desc IS NOT DESTROYED. The piped body comes first, then theirs.
rm -f "$TMP/body.json"
HOME="$TMP" "$AMUX_BIN" board request lane-b --desc "explicit tail" --stdin >"$TMP/o3" 2>&1 <<'EOF'
headline here
piped body here
EOF
check "piped body is kept"      "$(says '"piped body here" in b.get("desc","")')"
check "explicit desc is kept"   "$(says '"explicit tail" in b.get("desc","")')"
check "piped body comes first"  "$(says 'b["desc"].index("piped body here") < b["desc"].index("explicit tail")')"

# (d) `board add` shares the parser and the trap, so it shares the fix.
rm -f "$TMP/body.json"
HOME="$TMP" "$AMUX_BIN" board add --stdin >"$TMP/o4" 2>&1 <<'EOF'
add headline
add body line
EOF
check "board add splits too"           "$(says 'b["title"] == "add headline"')"
check "board add keeps the body"       "$(says '"add body line" in b.get("desc","")')"

echo
echo "  population: $((PASS + FAIL)) cells, $FAIL failing"
if [ "$FAIL" -gt 0 ]; then
  echo "FAIL ($FAIL of $((PASS + FAIL)) cells)"
  exit 1
fi
echo "PASS ($PASS cells)"
