#!/usr/bin/env bash
# AMUX-2626 / AMUX-2670 — the unstamped-send ledger must actually reconcile.
#
# When the server is unreachable, `amux send` falls back to raw tmux, which
# drops provenance, the audit row and delivery verification. AMUX-2670 covers
# that: record the send to a LOCAL ledger and flush it to POST /api/history as
# `raw-tmux-fallback` on the next send that reaches the server, so an unaudited
# send and a send that never happened stop looking identical.
#
# WHY THIS TEST EXISTS. That mechanism had never fired: zero `raw-tmux-fallback`
# rows in cmd_history, all time, and no pending file. Zero is the one-output/
# two-states shape — it reads the same for "the fallback genuinely never
# happens" (good) and "the ledger is broken and every fallback send is lost"
# (the exact failure it was built to prevent). A safety net nobody has ever seen
# catch anything is a safety net nobody has tested.
#
# It cannot be exercised by sending: a real fallback TYPES INTO A PEER'S PANE.
# So this drives the two shipped functions directly, against an isolated
# CC_HOME, and asserts the row lands server-side with the right type and origin.
#
# Exit 0 = pass, 1 = failure, 2 = skipped (server unreachable — the test needs a
# live /api/history and says so rather than passing vacuously).
set -euo pipefail
# Acts on the LIVE server by design: opt in with AMUX_TEST_LIVE=1 (lib/test-isolation.sh).
# shellcheck source=lib/test-isolation.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib/test-isolation.sh"; amux_test_require_live
cd "$(dirname "$0")/.."

PASS=0; FAIL=0
ok()  { PASS=$((PASS+1)); echo "  ok   — $1"; }
bad() { FAIL=$((FAIL+1)); echo "  FAIL — $1"; }

API="${AMUX_API:-https://localhost:8824}"
if ! curl -sk -m 5 -o /dev/null "$API/health" 2>/dev/null; then
  echo "SKIP: $API/health unreachable — this test needs a live server to reconcile into."
  echo "      Skipping is deliberate: passing without a server would assert nothing."
  exit 2
fi

# EXTRACT THE SHIPPED FUNCTIONS rather than reimplementing them. A copy here
# would pass forever while the real ones rot — the failure this file exists to
# prevent, one level up.
_fn_range() {  # $1 = function name -> "start,end" of its definition
  awk -v want="$1" '
    $0 ~ "^"want"\\(\\) *\\{" { start=NR; depth=0 }
    start && /\{/ { depth += gsub(/\{/,"{") }
    start && /\}/ { depth -= gsub(/\}/,"}"); if (depth<=0) { print start","NR; exit } }
  ' amux
}
# TRANSITIVELY. Extracting a function without its callees is the same rot this
# file was built to catch, one layer down, and it had already happened: AMUX-40
# (978645c0) swapped the bare `curl` inside _flush_unstamped_ledger for the
# hang-guarded `_curl` helper, which this test did not extract. `_curl` was then
# an undefined command, its rc-127 hit the flush's own "server went away
# mid-flush: KEEP the row" branch, and three assertions went red reading:
#
#   FAIL — the row never reached /api/history — every fallback send would be lost
#
# A green server, a working POST (verified by hand), and a test insisting the
# audit trail was broken. The diagnosis pointed at the mechanism; the defect was
# in the harness reading it. So walk the call graph instead of naming three
# functions and hoping the list stays complete.
SRC=""; HAVE=" "; NEED="_unstamped_ledger _record_unstamped_send _flush_unstamped_ledger"
while [ -n "$NEED" ]; do
  NEXT=""
  for f in $NEED; do
    case "$HAVE" in *" $f "*) continue ;; esac
    r=$(_fn_range "$f")
    if [ -z "$r" ]; then
      bad "could not locate $f in ./amux — the CLI moved and this test is now blind"
      echo; echo "$PASS passed, $FAIL failed"; exit 1
    fi
    body=$(sed -n "${r}p" amux)
    SRC="$SRC$body"$'\n'
    HAVE="$HAVE$f "
    for dep in $(printf '%s\n' "$body" | grep -oE '\b_[a-z0-9_]+' | sort -u); do
      case "$HAVE$NEXT " in *" $dep "*) continue ;; esac
      grep -q "^$dep() {" amux && NEXT="$NEXT $dep"
    done
  done
  NEED="$NEXT"
done

TMPHOME=$(mktemp -d)
trap 'rm -rf "$TMPHOME"' EXIT
export CC_HOME="$TMPHOME" AMUX_SESSION="ledger-selftest" AMUX_API="$API"
GREEN=""; RESET=""
eval "$SRC"

for f in _unstamped_ledger _record_unstamped_send _flush_unstamped_ledger; do
  if ! type -t "$f" >/dev/null; then
    bad "$f did not survive extraction — the assertions below would be vacuous"
    echo; echo "$PASS passed, $FAIL failed"; exit 1
  fi
done
ok "ledger functions + callees extracted from the shipped CLI:$HAVE"
# The closure is only worth having if a missing callee is LOUD. Without this,
# the next helper swap reproduces AMUX-40 exactly: an undefined command, a
# silent rc, and a red assertion pointing at the wrong subsystem.
MISSING=""
for dep in $(printf '%s\n' "$SRC" | grep -oE '\b_[a-z0-9_]+' | sort -u); do
  grep -q "^$dep() {" amux || continue          # not a CLI function, not our problem
  type -t "$dep" >/dev/null 2>&1 || MISSING="$MISSING $dep"
done
if [ -z "$MISSING" ]; then
  ok "every CLI helper the extracted code calls is defined here (no silent rc-127)"
else
  bad "extracted code calls undefined CLI helper(s):$MISSING — assertions below would blame the ledger for a harness gap"
fi

MARK="__ledgerselftest$(date +%s)$$__"
LEDGER=$(_unstamped_ledger)

# 1. A fallback send is RECORDED locally, because the server cannot be told.
_record_unstamped_send "ledger-selftest-target" "$MARK body"
if [ "$(wc -l < "$LEDGER" 2>/dev/null || echo 0)" -ge 1 ]; then
  ok "a fallback send is recorded to the local ledger"
else
  bad "nothing was recorded — a fallback send would leave no trace at all"
fi

# 2. The flush RECONCILES it into the audit trail and clears the local file.
_flush_unstamped_ledger >/dev/null 2>&1
if [ ! -s "$LEDGER" ]; then
  ok "the ledger is cleared after a successful flush"
else
  bad "rows remain after flush — they would be re-sent forever"
fi

# 3. THE ONE THAT MATTERS: it landed server-side, typed so an unstamped
#    injection is distinguishable from an audited send.
FOUND=$(curl -sk -m 10 "$API/api/history?limit=50" 2>/dev/null \
  | MARK="$MARK" python3 -c "
import json,os,sys
mark=os.environ['MARK']
try: d=json.load(sys.stdin)
except Exception: print('0|'); sys.exit(0)
rows=d if isinstance(d,list) else (d.get('rows') or d.get('items') or [])
hit=[r for r in rows if mark in json.dumps(r)]
print(f\"{len(hit)}|{hit[0].get('type','') if hit else ''}\")" 2>/dev/null)
N="${FOUND%%|*}"; TYPE="${FOUND##*|}"
if [ "${N:-0}" -ge 1 ] && [ "$TYPE" = "raw-tmux-fallback" ]; then
  ok "reconciled into the audit trail as type=raw-tmux-fallback"
elif [ "${N:-0}" -ge 1 ]; then
  bad "row landed but typed '$TYPE' — an unstamped send must be distinguishable"
else
  bad "the row never reached /api/history — every fallback send would be lost"
fi

# 4. THE UNIT. cmd_history.ts is MILLISECONDS (declared in
#    invariants/checks.rs TIMESTAMP_COLUMNS). The ledger wrote SECONDS, so
#    every reconciled row landed dated 1970-01-21 and sorted below every real
#    row — /api/history?limit=50 never returned one. The send was recorded and
#    was unfindable where anyone looks, which is the failure this ledger exists
#    to prevent, reached by the ledger itself.
#
#    Asserted against the NEWEST row the endpoint returns rather than a literal,
#    so it stays true as the clock moves and cannot pass by coincidence.
NEWEST=$(curl -sk -m 10 "$API/api/history?limit=1" 2>/dev/null \
  | python3 -c "
import json,sys
try: d=json.load(sys.stdin)
except Exception: print(0); sys.exit(0)
rows=d if isinstance(d,list) else []
print(rows[0].get('ts') or 0 if rows else 0)" 2>/dev/null)
if [ "${NEWEST:-0}" -gt 100000000000 ]; then
  ok "the endpoint's newest row is in milliseconds (control: the column's unit is what we think)"
else
  bad "control failed — the newest history row is not millis, so the check below proves nothing"
fi
MYTS=$(curl -sk -m 10 "$API/api/history?limit=50" 2>/dev/null \
  | MARK="$MARK" python3 -c "
import json,os,sys
mark=os.environ['MARK']
try: d=json.load(sys.stdin)
except Exception: print(0); sys.exit(0)
rows=d if isinstance(d,list) else []
hit=[r for r in rows if mark in json.dumps(r)]
print(hit[0].get('ts') or 0 if hit else 0)" 2>/dev/null)
if [ "${MYTS:-0}" -gt 100000000000 ]; then
  ok "the reconciled row carries a MILLISECOND ts, so it sorts with real rows"
else
  bad "the reconciled row's ts is ${MYTS:-0} — seconds into a millis column dates it 1970 and \
hides it from every time-ordered view"
fi

# 5. WHAT THE FALLBACK TELLS ITS USER (AF-454). Sections 1-4 prove the ledger
#    reconciles. This one proves the tool SAYS SO, which is a separate claim and
#    was false for as long as the ledger has existed: the closing message ended
#    "no origin stamp, no audit" and was printed one line AFTER
#    _record_unstamped_send wrote the audit row.
#
#    That cost a real measurement pass. gtm-engine hit a server flap on
#    2026-09-03, read "no audit" literally, and filed a provenance gap that
#    AMUX-2670 had already closed; their own send was in the trail the whole
#    time as MSG-40621 (type raw-tmux-fallback, origin "unstamped-fallback from
#    gtm-engine"). A mechanism the user is told does not exist is a mechanism
#    that does not reach them (ethos rule 1).
#
#    Asserted against the SOURCE, deliberately. This block cannot be executed
#    without typing into a peer's live pane, which is the same reason sections
#    1-4 drive the functions directly instead of sending. A static assertion
#    that can fail beats a dynamic one that cannot run.
#    And it reads ONLY the `echo` lines. The first draft of this check matched
#    the whole block and went red against the FIXED code, because the comment
#    explaining the old wording quotes "no audit" verbatim. A check that reads
#    the prose around a line instead of the line is pinned to the wrong layer,
#    and would have passed just as happily on a revert that kept the comment.
# THE RAW TMUX FALLBACK IS GONE, and this section now pins its ABSENCE (AMUX-4771).
#
# What was here asserted the wording of the fallback's closing block, extracted
# with `sed -n '/^  _record_unstamped_send /,/^  return 0$/p' amux`. That pattern
# expects the CALL SITE, indented two spaces inside the send path. There is no
# call site: `_record_unstamped_send` appears exactly ONCE in ./amux, its own
# definition at column 0, and it has appeared once at HEAD~200, HEAD~80 and
# HEAD~20 too. The block has not existed for the whole window anyone can check.
#
# It is not a regression, it is a deliberate removal. The terminal path now
# writes a `send_delivery_unknown` row with `raw_fallback: False` and says so to
# the sender: "No raw terminal paste was attempted." `_flush_unstamped_ledger`
# stays live because the comment above it says why -- "Historical fallback
# ledgers still flush" -- and it is still called on every acknowledged send.
#
# AND THE OLD CHECK COULD NOT SAY ANY OF THAT. Under `set -euo pipefail` the
# zero-match grep aborted the whole script AT THE ASSIGNMENT, before its own
# `if [ -z "$BLOCK" ]` could report "this check proves nothing" and before the
# summary line printed. The last thing a reader saw was an `ok`, so a run that
# died read as a clean pass. That is AF-561 in mirror image: no match is a
# legitimate answer and needs `|| true`.
BLOCK=$(sed -n '/^  _record_unstamped_send /,/^  return 0$/p' amux | grep '^ *echo ' || true)
if [ -n "$BLOCK" ]; then
  # The raw fallback came back. That is a real change and the old assertions
  # about its wording become live again, so fail loudly rather than pass a
  # branch nobody has reviewed since it was removed.
  bad "a raw-tmux fallback block reappeared in ./amux; typing into a peer's live pane bypasses dedupe (see this file's history for the wording checks it used to make)"
else
  ok "no raw-tmux fallback block exists to mis-word (it was removed; the terminal path records send_delivery_unknown instead)"
fi
# The removal is only safe if the sender is TOLD. An unacknowledged send that
# silently does nothing is the loss this whole file is about.
case "$(grep -c 'No raw terminal paste was attempted' amux || true)" in
  0) bad "the unacknowledged-send path no longer tells the sender that nothing was typed" ;;
  *) ok "the sender is told explicitly that no raw terminal paste was attempted" ;;
esac
case "$(grep -c 'send_delivery_unknown' amux || true)" in
  0) bad "nothing records send_delivery_unknown, so an unacknowledged send leaves no local trace" ;;
  *) ok "an unacknowledged send still leaves a local send_delivery_unknown trace" ;;
esac

# 6. WHAT THE RECEIVER SEES (AF-455) -- now pinned as an ABSENCE (AMUX-4771).
#
#    This section asserted the shape of the raw INJECTION: that the body carried
#    a marker rather than arriving bare, that the marker said "NOT
#    server-verified" rather than claiming an identity, and that the audit row
#    kept the undecorated text. Every one of those is a good property OF A
#    FEATURE THAT NO LONGER EXISTS.
#
#    ./amux says so itself, right above the ledger: "direct terminal pasting was
#    removed after the TubeScience stacked-draft incident (AMUX-4359)". There is
#    no `-l "$marked"` line, no `local marked=`, and no `_record_unstamped_send`
#    call site anywhere in the CLI.
#
#    So the concern this section carried is now satisfied by construction: a
#    peer message cannot be shape-identical to an owner prompt if no peer
#    message is ever typed into a pane. What is worth pinning is that it STAYS
#    removed -- reintroducing it would revive AF-455 and AMUX-1818 together.
#
#    THE OLD VERSION COULD NOT REPORT THIS. `KEYS=$(grep ... | head -1)` matched
#    nothing and, under `set -euo pipefail`, aborted the script at the
#    assignment -- before its own "this check proves nothing" arm and before the
#    summary. Same bug as section 5, the same line apart.
# tmux-lint: allow(a grep pattern over the CLI source, not a tmux call)
INJECT=$(grep -n 'tmux send-keys .* -l "' amux | head -1 || true)
if [ -n "$INJECT" ]; then
  bad "a raw send-keys injection path reappeared in ./amux ($INJECT) -- direct terminal pasting was removed after AMUX-4359 and revives AF-455/AMUX-1818"
else
  ok "no raw send-keys injection path exists (removed after AMUX-4359), so a peer message cannot arrive shaped like an owner prompt"
fi
# The removal has to be explained where the next reader of the send path looks,
# or it reads as an accident and gets 'restored'.
case "$(grep -c 'direct terminal pasting was removed' amux || true)" in
  0) bad "nothing in ./amux records WHY the raw paste path is absent; the next reader will treat it as a gap and re-add it" ;;
  *) ok "./amux records why the raw paste path is absent (AMUX-4359), so it is not mistaken for a gap" ;;
esac

echo
echo "$PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ] || exit 1
