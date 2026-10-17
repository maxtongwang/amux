#!/usr/bin/env bash
# AMUX-4570: `amux board doing` names the right remedy for each refusal.
#
# THE BUG. The refusal handler in _board_outcome treated any error containing
# "doing" as a WIP refusal. The continuation gate refuses with the codes
# doing_requires_next_action and doing_next_action_not_a_sentence, so a card
# with no next_action printed "Finish or demote the held card ... --override-doing"
# and exited 4. No card was held; the remedy was `amux board next <ID> "..."`.
# Reading only the tail, the obvious move was --override-doing, which cannot
# clear this gate.
#
# AMUX-4731, THE SECOND REPAIR OF THE SAME ARM. AMUX-4570 special-cased two
# error codes by name and left the predicate underneath unchanged: it still
# matched on the PRESENCE of `holding`. Two different refusals send that key and
# mean different things, a doing-WIP refusal sends a LIST of held cards and
# todo_wip_limit_reached sends an INTEGER queue depth, so `amux board todo`
# printed the doing remedy and exited 4, naming --override-doing, which cannot
# clear a todo limit. The server had sent four correct remedies in how_to_fix in
# the same body. The cells below pin the branch on `code` and the printing of
# the server remedy, in both directions.
#
# WHAT IS PINNED, in both directions. A continuation refusal must print the
# server remedy and exit 1, and must never print the override hint. The real WIP
# refusal (a holding list, error "already holding doing") must still print the
# override hint and exit 4, and the ack gate must still exit 3. Narrowing the WIP
# test in a way that silences the real WIP hint would pass the first half alone.
#
# Runs against a throwaway listener on a random port that answers the one PATCH
# a plain status move sends with a canned body. Nothing here touches a real board.
set -euo pipefail
# Private tmux server, never the host fleet's (lib/test-isolation.sh).
# shellcheck source=lib/test-isolation.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib/test-isolation.sh"; amux_test_isolate_tmux "$(mktemp -d)" || exit 1
cd "$(dirname "$0")/.."
AMUX_BIN="${AMUX_BIN:-./amux}"
PASS=0; FAIL=0
ok(){ echo "  ok   $1"; PASS=$((PASS+1)); }
bad(){ echo "  FAIL $1"; echo "       $2"; FAIL=$((FAIL+1)); }

REPLY=$(mktemp); PORTF=$(mktemp); ERRF=$(mktemp)
trap 'kill $LPID 2>/dev/null; rm -f "$REPLY" "$PORTF" "$ERRF"' EXIT

# Answers every PATCH with the status and body currently in $REPLY
# (first line: HTTP status, rest: JSON body).
python3 - "$REPLY" "$PORTF" <<'PY' &
import sys, http.server, socketserver
reply, portf = sys.argv[1], sys.argv[2]
class H(http.server.BaseHTTPRequestHandler):
    def _serve(self):
        n = int(self.headers.get('Content-Length') or 0)
        if n: self.rfile.read(n)
        raw = open(reply, 'rb').read().split(b'\n', 1)
        code = int(raw[0] or b'200'); body = raw[1] if len(raw) > 1 else b'{}'
        self.send_response(code); self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(body))); self.end_headers(); self.wfile.write(body)
    do_PATCH = do_POST = do_GET = _serve
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

run_board() {  # run_board <status> <http status> <json body>  -> sets RC and ERR
  printf '%s\n%s' "$2" "$3" > "$REPLY"
  if timeout 20 env AMUX_API="http://127.0.0.1:$PORT" AMUX_SESSION=continuation-hint-test CC_HOME="$(dirname "$ERRF")" \
      bash "$AMUX_BIN" board "$1" TEST-1 >/dev/null 2>"$ERRF"; then RC=0; else RC=$?; fi
  ERR=$(cat "$ERRF")
}
run_doing() { run_board doing "$1" "$2"; }

echo "amux board doing refusal remedies (AMUX-4570)"

# The server body for a missing next_action, keys as api/board.rs sends them.
MISSING='{"error":"doing_requires_next_action","why":"This card does not say what to DO next.","how":"amux board next <ID> \"<what the next actor should do>\"","escape":"amux board doing <ID> --force  (audited)","scope":"This gate is on doing only."}'
run_doing 400 "$MISSING"
[ "$RC" -eq 1 ] && ok "missing next_action exits 1" || bad "missing next_action exits 1" "exit $RC"
case "$ERR" in *"amux board next TEST-1"*) ok "missing next_action names amux board next TEST-1" ;;
  *) bad "missing next_action names amux board next TEST-1" "stderr: $ERR" ;; esac
case "$ERR" in *"--override-doing"*) bad "missing next_action never suggests --override-doing" "stderr: $ERR" ;;
  *) ok "missing next_action never suggests --override-doing" ;; esac
case "$ERR" in *"amux board doing TEST-1 --force"*) ok "missing next_action shows the audited force escape" ;;
  *) bad "missing next_action shows the audited force escape" "stderr: $ERR" ;; esac

NOT_SENTENCE='{"error":"doing_next_action_not_a_sentence","why":"next_action has to be a sentence.","how":"amux board next <ID> \"<what the next actor should do>\""}'
run_doing 400 "$NOT_SENTENCE"
[ "$RC" -eq 1 ] && ok "not-a-sentence exits 1" || bad "not-a-sentence exits 1" "exit $RC"
case "$ERR" in *"--override-doing"*) bad "not-a-sentence never suggests --override-doing" "stderr: $ERR" ;;
  *) ok "not-a-sentence never suggests --override-doing" ;; esac

# The live WIP refusal body (seen 2026-09-15 on AMUX-4637).
WIP='{"blocked":true,"cli":"amux board doing TEST-1 --override-doing","error":"already holding doing","holding":["AMUX-9"],"ok":false,"session":"continuation-hint-test"}'
run_doing 409 "$WIP"
[ "$RC" -eq 4 ] && ok "real WIP refusal still exits 4" || bad "real WIP refusal still exits 4" "exit $RC"
# Match the hint line itself. The CLI echoes the raw body to stderr first, and the WIP
# body carries its own cli field with --override-doing, so a bare substring match
# passes even when the hint is never printed (caught by the WIP-narrowing mutation).
case "$ERR" in *"Finish or demote the held card"*"amux board doing TEST-1 --override-doing"*) ok "real WIP refusal still prints the override-doing hint" ;;
  *) bad "real WIP refusal still prints the override-doing hint" "stderr: $ERR" ;; esac

GATE='{"attempted_status":"doing","blocked":true,"error":"gate not acknowledged","gate":["Scope is clear","Has an owner"],"item":"TEST-1","kind":"gate_blocked","ok":false}'
run_doing 409 "$GATE"
[ "$RC" -eq 3 ] && ok "ack gate still exits 3" || bad "ack gate still exits 3" "exit $RC"
case "$ERR" in *"--checked"*) ok "ack gate still names --checked" ;;
  *) bad "ack gate still names --checked" "stderr: $ERR" ;; esac

# ---- AMUX-4731: todo_wip_limit_reached must not get the doing remedy ----
# The live body, keys as api/board.rs 10816 sends them. `holding` is an INTEGER
# here, which is the whole trap: the old predicate accepted it as a held-card
# list.
TODO_WIP='{"error":"todo queue is at its limit for this lane","code":"todo_wip_limit_reached","ok":false,"blocked":true,"item":"TEST-1","attempted_status":"todo","session":"continuation-hint-test","holding":20,"limit":20,"why":"continuation-hint-test already holds 20 todo card(s) and the limit is 20.","close_these_first":[{"id":"AMUX-11","title":"an old one","days_since_touched":9,"already_undispatchable":true}],"how_to_fix":{"finish_one":"amux board done <ID> --evidence-stdin","not_next":"amux board backlog <ID> --trigger \"<what re-arms it>\"","not_a_unit_of_work":"amux board discard <ID>","raise_it":"set AMUX_TODO_WIP_LIMIT=<n>","force":"true (explicit bypass; logged)"}}'
run_board todo 409 "$TODO_WIP"
case "$ERR" in *"--override-doing"*) bad "todo WIP never suggests --override-doing" "stderr: $ERR" ;;
  *) ok "todo WIP never suggests --override-doing" ;; esac
case "$ERR" in *"What clears it:"*"amux board backlog"*) ok "todo WIP prints the server remedy naming backlog" ;;
  *) bad "todo WIP prints the server remedy naming backlog" "stderr: $ERR" ;; esac
case "$ERR" in *"amux board done TEST-1 --evidence-stdin"*) ok "todo WIP substitutes the real id into the remedy" ;;
  *) bad "todo WIP substitutes the real id into the remedy" "stderr: $ERR" ;; esac
case "$ERR" in *"AMUX-11"*"already undispatchable"*) ok "todo WIP names the stalest cards the server computed" ;;
  *) bad "todo WIP names the stalest cards the server computed" "stderr: $ERR" ;; esac
# ITS OWN EXIT CODE: 4 is the doing-WIP code and the two need opposite remedies,
# so a script cannot be left unable to tell them apart.
[ "$RC" -eq 7 ] && ok "todo WIP exits 7, not the doing-WIP 4" || bad "todo WIP exits 7, not the doing-WIP 4" "exit $RC"

# how_to_fix is a bare STRING at api/board.rs 5100. A reader that assumes a dict
# prints nothing for it, which is the same silent-drop this card is about.
TODO_WIP_STR='{"ok":false,"code":"todo_wip_limit_reached","error":"todo queue is at its limit for this lane","session":"continuation-hint-test","holding":20,"limit":20,"how_to_fix":"create in backlog or finish existing todo work"}'
run_board todo 409 "$TODO_WIP_STR"
case "$ERR" in *"What clears it:"*"create in backlog"*) ok "a string how_to_fix is printed too" ;;
  *) bad "a string how_to_fix is printed too" "stderr: $ERR" ;; esac
[ "$RC" -eq 7 ] && ok "string-form todo WIP also exits 7" || bad "string-form todo WIP also exits 7" "exit $RC"

run_doing 200 '{"ok":true,"id":"TEST-1","status":"doing"}'
[ "$RC" -eq 0 ] && ok "a successful move exits 0" || bad "a successful move exits 0" "exit $RC; stderr: $ERR"

echo "passed $PASS, failed $FAIL"
[ "$FAIL" -eq 0 ]
