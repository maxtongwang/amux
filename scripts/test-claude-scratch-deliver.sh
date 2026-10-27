#!/usr/bin/env bash
# AMUX-4732: the scratch report reaches each lane as a CARD, and only when the
# bytes are actually attributable to that lane.
#
# THE CELL THAT MATTERS MOST IS ambiguous_files_nothing. The report emits
# `AMBIGUOUS:19-candidates` for a workspace 19 lanes share. Delivering that is 18
# wrong cards, each costing a real recipient a read and a judgement, and it is
# the cheapest mistake for this script to make because the row LOOKS complete:
# it has a size, a class, and an owner field that is populated.
#
# HERMETIC. Runs against a fixture TSV and a stub `amux` on PATH, so no real
# board is touched and no real lane is asked for anything. The stub is also what
# lets the refusal cell exist at all: `cross_board_create_forbidden` is a real
# 403 a worker hits, and there is no way to provoke it on purpose against the
# live board without filing something somewhere.
#
# -e per the AF-561 ratchet: without it a harness prints PASS after calling a
# helper that does not exist.
set -euo pipefail
# Private tmux server, never the host fleet's (lib/test-isolation.sh).
# shellcheck source=lib/test-isolation.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib/test-isolation.sh"; amux_test_isolate_tmux "$(mktemp -d)" || exit 1
cd "$(dirname "$0")/.."
SUT="./scripts/claude-scratch-deliver.sh"
PASS=0; FAIL=0
ok(){ echo "  ok   $1"; PASS=$((PASS+1)); }
bad(){ echo "  FAIL $1"; echo "       $2"; FAIL=$((FAIL+1)); }

T=$(mktemp -d "${TMPDIR:-/tmp}/csd-test.XXXXXX")
trap 'rm -rf "$T"' EXIT

# The report's real column order: size, transcript age, class, reach, owner, conversation.
cat > "$T/rows.tsv" <<'TSV'
29.12	0.0d	LIVE	reachable	mixpeek-ops-server	-Users-ethan-Dev-mixpeek-server-ops/caceffea
20.52	0.0d	LIVE	reachable	mixpeek-homepage-claude	-Users-ethan-Dev-mixpeek-homepage/03e652c4
15.83	0.0d	LIVE	reachable-via-candidates	AMBIGUOUS:19-candidates running=backend	-Users-ethan-Dev-mixpeek/db837290
9.64	0.0d	LIVE	reachable-via-candidates	AMBIGUOUS:10-candidates running=amux	-Users-ethan-Dev-amux/c25121f5
4.42	2.5d	LIVE	no-owner	unattributed	-Users-ethan-Dev-mixpeek-server/b72fa321
0.40	0.0d	LIVE	reachable	mixpeek-finances	-Users-ethan-Dev-tiny/eabcd09e
7.10	0.0d	LIVE	reachable,escalated-09-14	mvs-pitr	-Users-ethan-Dev-mvs/aaaa1111
TSV
# The report prints a PROSE FOOTER even under --tsv (its summary block is not
# inside the `if TSV != 1` guard), so these lines really do arrive on the same
# stream. Appended verbatim from a live run.
cat >> "$T/rows.tsv" <<'FOOTER'

276 conversation(s), 118.9 GB total

BY REACH (can the owner be told, right now)
  reachable       80.2 GB over 9  ask the lane; it can act today
FOOTER

# ---- the stub CLI -------------------------------------------------------
# Records every call, and can be told to refuse, which is the only way to reach
# the refusal branch without asking a real lane for anything.
mkdir -p "$T/bin"
cat > "$T/bin/amux" <<'STUB'
#!/usr/bin/env bash
echo "$*" >> "$AMUX_STUB_LOG"
case "$1 $2" in
  "board request")
    if [ "${AMUX_STUB_REFUSE:-0}" = 1 ]; then
      cat >/dev/null
      echo '{"ok":false,"error":"cross_board_create_forbidden"}' >&2
      exit 4
    fi
    cat >/dev/null; echo "SCRATCH-1 -> todo (requested)"; exit 0 ;;
  "board progress") cat >/dev/null; echo "updated"; exit 0 ;;
  "url") echo "http://127.0.0.1:1"; exit 0 ;;
esac
exit 0
STUB
chmod +x "$T/bin/amux"

run() {  # run <extra args...>  -> sets OUT and RC
  : > "$T/calls.log"
  set +e
  OUT=$(PATH="$T/bin:$PATH" AMUX_STUB_LOG="$T/calls.log" AMUX_URL="http://127.0.0.1:1" \
        AMUX_SESSION=deliver-test "$SUT" --from-tsv "$T/rows.tsv" "$@" 2>&1)
  RC=$?
  set -e
  CALLS=$(cat "$T/calls.log" 2>/dev/null || true)
}

echo "claude-scratch-deliver (AMUX-4732)"

# ---- 1. dry run is the default and files nothing -------------------------
run
[ "$RC" -eq 0 ] && ok "a plain run exits 0" || bad "a plain run exits 0" "exit $RC"
case "$OUT" in *"PLAN ONLY"*) ok "a plain run says it is a plan" ;;
  *) bad "a plain run says it is a plan" "$OUT" ;; esac
case "$CALLS" in *"board request"*) bad "a plain run files nothing" "called: $CALLS" ;;
  *) ok "a plain run files nothing" ;; esac

# ---- 2. THE CARD'S CENTRAL CONSTRAINT ------------------------------------
run --apply
case "$CALLS" in *AMBIGUOUS*) bad "an ambiguous row is never delivered" "called: $CALLS" ;;
  *) ok "an ambiguous row is never delivered" ;; esac
case "$CALLS" in *unattributed*) bad "an unattributed row is never delivered" "called: $CALLS" ;;
  *) ok "an unattributed row is never delivered" ;; esac
# ...and it is REPORTED rather than dropped: 15.83 GB nobody can address is
# still 15.83 GB, and silence about it reads as nothing left to do.
case "$OUT" in *"NOT DELIVERED"*AMBIGUOUS*) ok "an ambiguous row is reported, not dropped" ;;
  *) bad "an ambiguous row is reported, not dropped" "$OUT" ;; esac

# ---- 3. the attributable lanes each get exactly one card -----------------
for lane in mixpeek-ops-server mixpeek-homepage-claude; do
  n=$(printf '%s\n' "$CALLS" | grep -c "board request $lane " || true)
  [ "$n" -eq 1 ] && ok "$lane gets exactly one card" || bad "$lane gets exactly one card" "n=$n; $CALLS"
done

# ---- 4. the threshold ----------------------------------------------------
# 0.40 GB is below the 1.0 default. A finding under the threshold is noise; 10
# of 301 conversations are >= 1 GB and delivering all 301 is a different kind of
# wrong card.
case "$CALLS" in *"board request mixpeek-finances"*) bad "a sub-threshold row is not delivered" "$CALLS" ;;
  *) ok "a sub-threshold row is not delivered" ;; esac

# ---- 5. a refusal is reported AS a refusal -------------------------------
# curl exits 0 on a 403, so a delivery that trusted the transport would count
# this as delivered. That is the failure the 2026-09-14 attempt had: it could
# not say it had reached nobody.
: > "$T/calls.log"
set +e
OUT=$(PATH="$T/bin:$PATH" AMUX_STUB_LOG="$T/calls.log" AMUX_STUB_REFUSE=1 AMUX_URL="http://127.0.0.1:1" \
      AMUX_SESSION=deliver-test "$SUT" --from-tsv "$T/rows.tsv" --apply 2>&1)
RC=$?
set -e
case "$OUT" in *REFUSED*cross_board_create_forbidden*) ok "a refused create is reported as refused" ;;
  *) bad "a refused create is reported as refused" "$OUT" ;; esac
# THREE deliverable lanes in the fixture now (ops-server, homepage-claude, and
# the comma-flagged mvs-pitr added for cell 5c), so all three refusals must be
# counted. The number is spelled out rather than loosened to "non-zero": a
# summary that undercounts refusals is the same lie as one that overcounts
# deliveries.
case "$OUT" in *"delivered=0"*"refused=3"*) ok "the summary counts refusals, not deliveries" ;;
  *) bad "the summary counts refusals, not deliveries" "$(printf '%s' "$OUT" | tail -2)" ;; esac
[ "$RC" -ne 0 ] && ok "a run that reached nobody exits non-zero" || bad "a run that reached nobody exits non-zero" "exit $RC"

# ---- 5b. A SECOND RUN UPDATES, IT DOES NOT DUPLICATE ---------------------
# Constraint 2 on the card. Without it a tick files one new card per lane per
# run, and the board already carries 12,745 rows. The lookup is a real HTTP GET,
# so this needs a listener rather than a stubbed binary: a dead port makes
# `existing` empty and the create path passes every time, which is exactly the
# vacuous green this cell exists to prevent.
BPORT=$(mktemp "$T/port.XXXXXX")
python3 - "$BPORT" <<'PY' &
import sys, http.server, socketserver, json
portf = sys.argv[1]
BOARD = [
    {"id": "MOS-77", "status": "todo", "title": "[scratch] mixpeek-ops-server is holding 25.00 GB in /private/tmp/claude-501"},
    {"id": "MOS-78", "status": "done", "title": "[scratch] an old closed one that must NOT be reused"},
]
class H(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        body = json.dumps(BOARD if "mixpeek-ops-server" in self.path else []).encode()
        self.send_response(200); self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body))); self.end_headers(); self.wfile.write(body)
    def log_message(self, *a): pass
class S(socketserver.TCPServer):
    allow_reuse_address = True
with S(("127.0.0.1", 0), H) as srv:
    open(portf, "w").write(str(srv.server_address[1]))
    srv.serve_forever()
PY
BPID=$!
disown $BPID 2>/dev/null || true
for _ in $(seq 1 50); do [ -s "$BPORT" ] && break; sleep 0.1; done
PORT=$(cat "$BPORT")
[ -n "${PORT:-}" ] || { echo "board listener never bound"; exit 1; }

: > "$T/calls.log"
set +e
OUT=$(PATH="$T/bin:$PATH" AMUX_STUB_LOG="$T/calls.log" AMUX_URL="http://127.0.0.1:$PORT" \
      AMUX_SESSION=deliver-test "$SUT" --from-tsv "$T/rows.tsv" --apply 2>&1)
RC=$?
set -e
CALLS=$(cat "$T/calls.log")
kill $BPID 2>/dev/null || true
wait $BPID 2>/dev/null || true

case "$CALLS" in *"board progress MOS-77"*) ok "an existing card is UPDATED, not duplicated" ;;
  *) bad "an existing card is UPDATED, not duplicated" "called: $CALLS" ;; esac
case "$CALLS" in *"board request mixpeek-ops-server"*) bad "the lane with a card gets no second card" "called: $CALLS" ;;
  *) ok "the lane with a card gets no second card" ;; esac
# A TERMINAL card must not be reused as the update target, or a lane that closed
# last week's card never hears about this week's growth.
case "$CALLS" in *"board progress MOS-78"*) bad "a closed card is never reused" "called: $CALLS" ;;
  *) ok "a closed card is never reused" ;; esac
# The lane with no existing card still gets one, so the update path did not
# swallow the create path.
case "$CALLS" in *"board request mixpeek-homepage-claude"*) ok "a lane with no card still gets one" ;;
  *) bad "a lane with no card still gets one" "called: $CALLS" ;; esac
case "$OUT" in *"updated=1"*) ok "the summary counts the update" ;;
  *) bad "the summary counts the update" "$(printf '%s' "$OUT" | tail -2)" ;; esac

# ---- 5c. THE TWO DEFECTS THE FIRST LIVE RUN FOUND ------------------------
# Both were invisible to a hermetic fixture until the fixture carried the live
# shapes, which is the argument for running the plan against real data before
# trusting any of this.
run --apply

# (a) REACH IS A FLAG SET. The live value on the biggest finding was
# `reachable,escalated-09-14`, and an exact match on "reachable" dropped it. The
# lane held 29.12 GB and is one of the two that never got the 2026-09-14
# message, so the flag recording the escalation is what suppressed the next one.
case "$CALLS" in *"board request mvs-pitr"*) ok "a comma-flagged reachable lane is still delivered" ;;
  *) bad "a comma-flagged reachable lane is still delivered" "called: $CALLS" ;; esac
# ...and the near-miss stays excluded: `reachable-via-candidates` contains
# "reachable" as a SUBSTRING, so a loosened test that used a substring match
# would sweep every ambiguous row back in.
case "$CALLS" in *AMBIGUOUS*) bad "via-candidates is still excluded after the loosening" "called: $CALLS" ;;
  *) ok "via-candidates is still excluded after the loosening" ;; esac

# (b) THE PROSE FOOTER IS NOT A FINDING. The first live run counted 287 rows
# against 276 real conversations and printed two footer lines as findings, with
# a doubled unit ("118.9 GB totalGB") that is the tell.
case "$OUT" in *"conversation(s)"*) bad "a footer line is never treated as a row" "$OUT" ;;
  *) ok "a footer line is never treated as a row" ;; esac
case "$OUT" in *"GBGB"*|*"totalGB"*) bad "no doubled unit from a misparsed footer" "$OUT" ;;
  *) ok "no doubled unit from a misparsed footer" ;; esac
# The denominator counts rows, not lines: 4 rows are >= 1.0 GB in the fixture
# (29.12, 20.52, 15.83, 9.64, 4.42, 7.10 = 6), and the footer must not inflate it.
case "$OUT" in *"6 row(s) at >= 1.0GB"*) ok "the row count excludes the footer" ;;
  *) bad "the row count excludes the footer" "$(printf '%s' "$OUT" | head -1)" ;; esac

# ---- 6. never a delete path, same promise as the report it reads ---------
# A property of the SOURCE, not of any run: a test that only checked output
# would stay green the day somebody adds an --apply-delete.
if grep -qE '\brm -rf|rmtree|--apply-delete' "$SUT"; then
  bad "the deliverer has no delete path" "found a delete-shaped token in $SUT"
else
  ok "the deliverer has no delete path"
fi

echo "passed $PASS, failed $FAIL"
[ "$FAIL" -eq 0 ]
