#!/usr/bin/env bash
# AMUX-4653 — `amux board request <worker> <title>` must post `request_to`.
#
# THE DEFECT: it posted {"title": ..., "status": "backlog", "reviewer": "<worker>"}
# with no session, so the card landed on the SENDER's own board. Dispatch selects by
# session and the reviewer nudge fires only on review/done, so the named worker was
# never offered it. 88 such cards were sitting in senders' backlogs on 2026-09-15,
# ten of them mixpeek-finances' (MF-1160..1167, 1169, 1174), draining back into their
# own pickup as churn.
#
# The server half is covered by crates/amux-server/tests/board_request.rs. This cell
# covers the half that file cannot see: the BYTES THE CLI SENDS. A server that honours
# request_to and a CLI that still sends reviewer both pass their own tests while the
# feature is dead, which is exactly the shape that produced the original bug.
#
# It runs the real dispatch path against a throwaway HTTP recorder on loopback, so
# what is asserted is the actual request body, not a re-derivation of it.
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

# A recorder, not a mock board: it writes the POST body to a file and answers with
# the smallest response _board_outcome accepts. Plain HTTP on purpose, because the
# CLI's `curl -sk` speaks either and a self-signed cert would add a moving part that
# has nothing to do with what is being asserted.
cat > "$TMP/recorder.py" <<'PY'
import json, sys
from http.server import BaseHTTPRequestHandler, HTTPServer

OUT = sys.argv[1]

class H(BaseHTTPRequestHandler):
    def do_POST(self):
        n = int(self.headers.get("Content-Length") or 0)
        with open(OUT, "wb") as f:
            f.write(self.rfile.read(n))
        body = json.dumps({"id": "LB-1", "status": "todo", "session": "lane-b",
                           "requested_by": "requesttest"}).encode()
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
# Wait for the port line rather than sleeping a guessed interval: a fixed sleep is
# either slower than it needs to be or flaky on a loaded box, and this one can say
# WHY it gave up.
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

export AMUX_SESSION="requesttest"
export AMUX_WORKER="requesttest"
export AMUX_API="http://127.0.0.1:$PORT"
export AMUX_URL="http://127.0.0.1:$PORT"

check() { # label condition-result
  if [ "$2" = "1" ]; then PASS=$((PASS+1)); else FAIL=$((FAIL+1)); echo "  FAIL  $1"; fi
}
jq_says() { # jq-ish python expression over the captured body
  python3 -c '
import json, sys
b = json.load(open(sys.argv[1]))
print("1" if eval(sys.argv[2], {"b": b}) else "0")
' "$TMP/body.json" "$1"
}

echo "board request routes to the target (AMUX-4653)"
echo

HOME="$TMP" "$AMUX_BIN" board request lane-b "pick up the docker bundle" \
  --type investigation --desc "WS5 of the standalone epic" --due 2026-09-30 \
  >"$TMP/out" 2>&1 || true

if [ ! -s "$TMP/body.json" ]; then
  echo "  FAIL  the CLI sent no request body at all"
  head -5 "$TMP/out" || true
  exit 1
fi

check "request_to names the target lane"        "$(jq_says 'b.get("request_to") == "lane-b"')"
check "reviewer is NOT used to carry the target" "$(jq_says 'b.get("reviewer") is None')"
# The old body pinned status=backlog, which is the column dispatch does not hand out.
check "status is left to the server default"     "$(jq_says 'b.get("status") is None')"
check "no session is claimed by the caller"      "$(jq_says 'b.get("session") is None')"

# The structured fields are the whole reason this is a create rather than a message:
# a captured message is titled from its prompt text and has nowhere to put these.
check "type survives"  "$(jq_says 'b.get("type") == "investigation"')"
check "desc survives"  "$(jq_says 'b.get("desc") == "WS5 of the standalone epic"')"
check "due survives"   "$(jq_says 'b.get("due") == "2026-09-30"')"
check "title survives" "$(jq_says 'b.get("title") == "pick up the docker bundle"')"

# --depends-on is the one field a caller most often adds to a handover, and it
# travelled on the old body too. A regression here would be silent.
rm -f "$TMP/body.json"
HOME="$TMP" "$AMUX_BIN" board request lane-b "with a dependency" \
  --depends-on AMUX-1 --depends-on AMUX-2 >"$TMP/out2" 2>&1 || true
check "depends_on survives" "$(jq_says 'b.get("depends_on") == ["AMUX-1", "AMUX-2"]')"

# THE HELP MUST NOT STILL PROMISE THE OLD BEHAVIOUR. A lane that reads "creates a
# card on your own board" will keep hand-rolling its own delegation, which is what
# the 88 stranded cards and the ASK-message workaround both were.
help_text=$(HOME="$TMP" "$AMUX_BIN" board request --help 2>&1 || true)
case "$help_text" in
  *"NAMED WORKER'S board"*) PASS=$((PASS+1)) ;;
  *) FAIL=$((FAIL+1)); echo "  FAIL  the help does not say the card lands on their board" ;;
esac
case "$help_text" in
  *"own board and links the named"*) FAIL=$((FAIL+1)); echo "  FAIL  the help still describes the old own-board behaviour" ;;
  *) PASS=$((PASS+1)) ;;
esac

# ---------------------------------------------------------------------------
# AMUX-4662: a review handoff the reviewer cannot receive must SAY SO.
#
# The server has answered reviewer_notified false, with a full reason, since
# AMUX-3771, and the CLI threw it away. Measured 2026-09-15 on a throwaway card
# against paused amux-testing: the API named the refusal, `amux board review`
# printed one line, "AMUX-4707 -> review". Four real cards went to that same
# paused lane over five hours with nobody told.
#
# The recorder answers with the unreachable shape so this cell tests the CLI's
# handling of it, which is the half no server test can see.
# ---------------------------------------------------------------------------
cat > "$TMP/recorder2.py" <<'PY2'
import json, sys
from http.server import BaseHTTPRequestHandler, HTTPServer

class H(BaseHTTPRequestHandler):
    def do_PATCH(self):
        n = int(self.headers.get("Content-Length") or 0)
        self.rfile.read(n)
        body = json.dumps({
            "id": "LB-9", "status": "review", "reviewer": "lane-paused",
            "reviewer_notified": False,
            "reviewer_notify_reason": "the note is on the card, but reviewer 'lane-paused' could NOT be told: interaction refused: 'lane-paused' is paused.",
        }).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)
    def log_message(self, *a):
        pass

srv = HTTPServer(("127.0.0.1", 0), H)
print(srv.server_address[1], flush=True)
srv.serve_forever()
PY2
python3 "$TMP/recorder2.py" > "$TMP/port2" 2>"$TMP/rec2.err" &
REC2_PID=$!
# Drop it from the job table, or bash prints "Terminated" into the verdict output
# when the kill below reaps it. Noise in a harness that prints a verdict is how a
# real failure line gets scrolled past.
disown %% 2>/dev/null || true
PORT2=""
for _ in $(seq 1 100); do
  PORT2=$(head -1 "$TMP/port2" 2>/dev/null || true)
  [ -n "$PORT2" ] && break
  sleep 0.05
done
if [ -z "$PORT2" ]; then
  echo "  FAIL  the review recorder never reported a port"
  FAIL=$((FAIL+1))
else
  AMUX_API="http://127.0.0.1:$PORT2" AMUX_URL="http://127.0.0.1:$PORT2" HOME="$TMP" \
    "$AMUX_BIN" board review LB-9 --reviewer lane-paused \
      --checked "Implemented and self-tested" "Diff / PR is up" "Ready for another set of eyes" \
      >"$TMP/rv.out" 2>"$TMP/rv.err" || true
  kill "$REC2_PID" 2>/dev/null || true
  if grep -q "REVIEWER WAS NOT TOLD" "$TMP/rv.err"; then PASS=$((PASS+1)); else
    FAIL=$((FAIL+1)); echo "  FAIL  the CLI swallowed reviewer_notified=false"
    echo "    stdout: $(head -2 "$TMP/rv.out" 2>/dev/null)"
    echo "    stderr: $(head -2 "$TMP/rv.err" 2>/dev/null)"
  fi
  if grep -q "lane-paused" "$TMP/rv.err"; then PASS=$((PASS+1)); else
    FAIL=$((FAIL+1)); echo "  FAIL  the warning does not name the reviewer"
  fi
  # THE MOVE SUCCEEDED, so the success line must still be on stdout and the
  # warning must be on stderr. A caller parsing stdout must not see a failure.
  if grep -q "LB-9" "$TMP/rv.out"; then PASS=$((PASS+1)); else
    FAIL=$((FAIL+1)); echo "  FAIL  the transition line left stdout"
  fi
fi

echo
echo "  population: $((PASS + FAIL)) cells, $FAIL failing"
if [ "$FAIL" -gt 0 ]; then
  echo "FAIL ($FAIL of $((PASS + FAIL)) cells)"
  exit 1
fi
echo "PASS ($PASS cells)"
