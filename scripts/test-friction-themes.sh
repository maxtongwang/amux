#!/usr/bin/env bash
# Cells for scripts/friction_themes.py.
#
# WHY THIS EXISTS. Every failure mode this scanner has is a QUIET one: it prints
# a clean report and the report is wrong. There is no crash to notice.
#
#   - The Mixpeek ledger uses a checkbox format, the amux one uses fields. If the
#     Mixpeek parser stops matching (their format drifts, a heading changes), it
#     returns [] and every cross-repo cluster silently becomes amux-only. The
#     report still prints, still says "measured", and Mixpeek reads as clean.
#     That is the exact shape of the failure this fleet has 41 ledger entries
#     about, so cell C is the most important one in the file.
#   - `continue` is how Ethan drives the fleet every evening. Counting it as a
#     restatement of the idle-stall rule fired that class on normal operation
#     (11 hits in one window, 7 of them one broadcast). Cell D pins the filter.
#   - He pastes documents under his instructions. Matching rule classes against
#     the pasted body attributes a peer's words to him: it scored "permissions"
#     on a message whose instruction was "clear the backlog". Cell E pins that.
#   - cmd_history.ts is ms and issues.created is seconds. Mixing them makes
#     every card 56,000 days old and every lane look like it closed nothing:
#     both look like findings. Cell F pins the warning that catches a unit flip.
#
# Cells run the SHIPPED script against a synthetic store via AMUX_DB /
# MIXPEEK_REPO, rather than restating its logic: simulating what you believe
# the code does cannot catch it doing something else (ethos rule 7).
set -euo pipefail
cd "$(dirname "$0")/.."
SCAN="${FRICTION_THEMES:-$(pwd)/scripts/friction_themes.py}"
TMP=$(mktemp -d); trap 'rm -rf "$TMP"' EXIT
# Hermetic: never the live server or the real ~/.amux (lib/test-isolation.sh).
# shellcheck source=lib/test-isolation.sh
mkdir -p "$TMP/home"
source scripts/lib/test-isolation.sh; amux_test_isolate "$TMP/home" || exit 1
DB="$TMP/t.db"
PASS=0; FAIL=0
ok()  { PASS=$((PASS+1)); echo "  ok   $1"; }
bad() { FAIL=$((FAIL+1)); echo "  FAIL $1"; }

NOW_MS=$(python3 -c 'import time;print(int(time.time()*1000))')
NOW_S=$((NOW_MS / 1000))

# ---------------------------------------------------------------------------
# A synthetic store: two lanes, one per repo, and a board with a growing pile.
# ---------------------------------------------------------------------------
python3 - "$DB" "$NOW_MS" <<'PY'
import sqlite3, sys
db, now = sys.argv[1], int(sys.argv[2])
con = sqlite3.connect(db)
con.execute("""CREATE TABLE cmd_history (id INTEGER PRIMARY KEY AUTOINCREMENT,
    text TEXT NOT NULL, type TEXT NOT NULL DEFAULT 'direct',
    session TEXT NOT NULL DEFAULT '', ts INTEGER NOT NULL,
    origin TEXT NOT NULL DEFAULT '', card_id TEXT, delivery TEXT,
    queued_at INTEGER, delivered_at INTEGER, submit_verdict TEXT)""")
con.execute("""CREATE TABLE issues (id TEXT PRIMARY KEY, title TEXT NOT NULL,
    "desc" TEXT NOT NULL DEFAULT '', status TEXT NOT NULL DEFAULT 'todo',
    session TEXT, creator TEXT NOT NULL DEFAULT '', due TEXT,
    created INTEGER NOT NULL, updated INTEGER NOT NULL, deleted INTEGER,
    archived INTEGER NOT NULL DEFAULT 0, closed_at INTEGER)""")

H = 3600_000
def msg(text, session, hours_ago, type='user', origin=''):
    con.execute("INSERT INTO cmd_history (text,type,session,ts,origin) VALUES (?,?,?,?,?)",
                (text, type, session, now - int(hours_ago * H), origin))

# Cell D: the evening broadcast. Bare drives, both repos, inside the window.
for lane in ("amux", "tubescience", "mixpeek-general"):
    msg("continue", lane, 2)
msg("[08:12 PM] continue", "backend", 2)

# Cell E: the instruction is about the backlog; "permissions" appears ONLY in
# 4 KB of pasted context below it. Must not score the permissions class.
# A realistic paste: a blank line, then a transcript in NORMAL-length lines.
# An earlier version of this cell used one 6 KB line, which a plain character
# cap would have passed by accident: the cell has to look like MSG-35336.
PASTE = "\n\n" + "\n".join(
    "Them: the service account needs permission and a credential and api key access."
    for _ in range(60))
msg("[01:12 PM] can you clear the backlog of anything not relevant" + PASTE, "tubescience", 3)
msg("[01:20 PM] same, clear the backlog on your side" + PASTE, "amux", 3)

# A genuine restatement of an already-written rule, in BOTH repos, n>=2.
msg("[09:00 AM] you need to verify this actually works in prod before calling it done",
    "amux", 4)
msg("[09:05 AM] verify it in prod, e2e, dont mark it verified on faith",
    "backend", 4)
# Baseline days so the trailing rate is real and low.
for d in range(2, 13):
    msg("routine work on the ingestion pipeline, no rule restated here at all", "backend", d * 24)

# Board: a needsyou pile in mixpeek that GREW over 7 days, and an amux pile
# that SHRANK. Only the growing one may be active.
for i in range(40):
    created = NOW_S = now // 1000 - 30 * 86400
    con.execute("INSERT INTO issues (id,title,status,session,created,updated) "
                "VALUES (?,?,?,?,?,?)",
                (f"BACKE-{i}", "old mixpeek card", "needsyou", "backend",
                 created, created))
for i in range(15):  # created INSIDE the 7d window -> the pile grew
    created = now // 1000 - 3 * 86400
    con.execute("INSERT INTO issues (id,title,status,session,created,updated) "
                "VALUES (?,?,?,?,?,?)",
                (f"MI-{i}", "new mixpeek card", "needsyou", "mvs-infra",
                 created, created))
for i in range(40):  # amux pile: all old, and 20 of them CLOSED in the window
    created = now // 1000 - 30 * 86400
    closed = (now // 1000 - 2 * 86400) if i < 20 else None
    con.execute("INSERT INTO issues (id,title,status,session,created,updated,closed_at) "
                "VALUES (?,?,?,?,?,?,?)",
                (f"AMUX-{i}", "amux card", "needsyou" if closed is None else "done",
                 "amux", created, created, closed))
con.commit()
PY

mkdir -p "$TMP/amux" "$TMP/mixpeek"

# DATES ARE GENERATED, NEVER LITERAL (AF-386). The fixture below feeds
# `signal_ledger_clusters`, which is a FRESHNESS signal: it fires on entries
# newer than now minus FRICTION_DAYS (default 1). Written on 2026-08-30 with
# 2026-08-30 typed in, every cell passed. On 2026-09-01 the cut moved past the
# fixture, the cluster went inactive, and CI went red across every author's push
# with neither the test nor its subject having changed since.
#
# The cost was not the red. It was that the red said "Mixpeek ledger parser is
# not reading the real format" while the parser was correct, so it accused
# innocent code for two days. A literal date in a fixture for a time-windowed
# signal is a test with a fuse on it.
#
# @TODAY@ is substituted after the heredocs rather than interpolated inside
# them: the bodies carry parentheses and pipes, and a quoted heredoc keeps them
# literal.
TODAY="$(date +%Y-%m-%d)"
cat > "$TMP/amux/frustrations.md" <<'EOF'
# amux frustrations

  ## an indented template that must never count itself
  AREA: cli
  STATUS: open

## A real amux entry about instruments
AREA: instruments
STATUS: open
DATE: @TODAY@
SESSION: amux
CARD: AF-1
EOF

# Two OPEN mixpeek entries in the instruments class, dated today. Real format,
# copied in shape from ~/Dev/mixpeek/FRUSTRATIONS.md.
cat > "$TMP/mixpeek/FRUSTRATIONS.md" <<'EOF'
# Mixpeek Product Frustrations

- [ ] **@TODAY@ | API/metrics (a latency percentile cannot see failed requests, so it reads fastest when the endpoint is broken)** *(backend)*: body text here.
- [ ] **@TODAY@ | Engine/instrumentation (the probe reports zero when it never ran)** *(tubescience)*: body text here.
- [x] **@TODAY@ | API/closed (this one is done and must not be counted)** *(backend)*: body.
EOF
for _f in "$TMP/amux/frustrations.md" "$TMP/mixpeek/FRUSTRATIONS.md"; do
  sed "s/@TODAY@/$TODAY/g" "$_f" > "$_f.tmp" && mv "$_f.tmp" "$_f"
done
cp CLAUDE.md "$TMP/amux/CLAUDE.md" 2>/dev/null || echo "verified deploy tests" > "$TMP/amux/CLAUDE.md"
mkdir -p "$TMP/amux/.claude/rules"
cp .claude/rules/ethos.md "$TMP/amux/.claude/rules/" 2>/dev/null || true
cp .claude/rules/frustrations.md "$TMP/amux/.claude/rules/" 2>/dev/null || true
echo "verification e2e prod tests permission credential" > "$TMP/mixpeek/CLAUDE.md"

run() { AMUX_DB="$DB" AMUX_REPO="$TMP/amux" MIXPEEK_REPO="$1" \
        FRICTION_DAYS="${2:-1}" python3 "$SCAN" --json 2>/dev/null; }

echo "== friction_themes cells =="

# ---------------------------------------------------------------------------
# Cell A: the scan runs and reports its sources honestly.
# ---------------------------------------------------------------------------
OUT=$(run "$TMP/mixpeek")
if [ -n "$OUT" ] && echo "$OUT" | python3 -c "
import json,sys; d=json.load(sys.stdin)
sys.exit(0 if d['n_signals_computed'] > 0 else 1)"; then
  ok "A: scan produces signals against a synthetic store"
else
  bad "A: scan produced nothing"
fi

# ---------------------------------------------------------------------------
# Cell B: an UNREADABLE mixpeek checkout must never read as a clean Mixpeek.
# This is the rule-4 cell: measured=false, not a zero.
# ---------------------------------------------------------------------------
OUT=$(run "$TMP/does-not-exist")
if echo "$OUT" | python3 -c "
import json,sys
d = json.load(sys.stdin)
src = d['sources']['mixpeek FRUSTRATIONS.md']
assert src['readable'] is False, 'unreadable ledger not reported as unreadable'
# and no cluster may claim a mixpeek count it could not have read
for s in d['active']:
    if s['key'].startswith('ledger-cluster'):
        assert s['detail'].get('standing_mixpeek', 0) == 0 or s['why_unmeasured'], \
            f\"{s['key']} reported mixpeek entries from an unreadable ledger\"
assert 'mixpeek FRUSTRATIONS.md' in d['unreadable_sources']
"; then
  ok "B: an unreadable Mixpeek ledger is reported, not counted as clean"
else
  bad "B: unreadable Mixpeek ledger degraded into a zero"
fi

# HARNESS SELF-CHECK, before cell C is allowed to render a verdict (AF-386).
# Cell C reads a FRESHNESS signal, so a fixture whose dates have aged past the
# window produces exactly the same output as a parser that returns nothing. For
# two days it printed the parser accusation while the parser was correct. If the
# dates ever go stale again, this says SETUP and names the cut, so the next
# reader starts at the fixture instead of at innocent code.
FRESH_CUT="$(python3 -c "
import os, time
print(time.strftime('%Y-%m-%d',
      time.localtime(time.time() - float(os.environ.get('FRICTION_DAYS', '1')) * 86400)))")"
n_stale=$(grep -hoE '[0-9]{4}-[0-9]{2}-[0-9]{2}' \
            "$TMP/amux/frustrations.md" "$TMP/mixpeek/FRUSTRATIONS.md" 2>/dev/null \
          | awk -v c="$FRESH_CUT" '$0 < c' | wc -l | tr -d ' ')
if [ "${n_stale:-0}" -gt 0 ]; then
  bad "SETUP: ${n_stale} fixture date(s) older than the freshness cut ${FRESH_CUT} -- the fixture aged out; cell C is about the fixture, not the parser"
fi

# ---------------------------------------------------------------------------
# Cell C: the Mixpeek checkbox parser actually parses. A parser that silently
# returns [] passes every other cell in this file and makes Mixpeek invisible.
# ---------------------------------------------------------------------------
OUT=$(run "$TMP/mixpeek")
if echo "$OUT" | python3 -c "
import json,sys
d = json.load(sys.stdin)
inst = [s for s in d['active'] if s['key'] == 'ledger-cluster:instruments']
assert inst, 'instruments cluster absent: mixpeek entries were not parsed'
s = inst[0]
assert s['detail']['standing_mixpeek'] >= 2, \
    f\"expected >=2 open mixpeek entries, got {s['detail']['standing_mixpeek']}\"
assert s['detail']['standing_amux'] >= 1, 'amux entry not parsed'
assert s['detail']['spans_repos'] is True, 'cross-repo cluster not detected'
# the [x] entry is CLOSED and must not be counted
assert s['detail']['standing_mixpeek'] == 2, \
    f\"a closed [x] entry was counted: {s['detail']['standing_mixpeek']}\"
"; then
  ok "C: Mixpeek checkbox ledger parses, spans_repos fires, [x] excluded"
else
  bad "C: Mixpeek ledger parser is not reading the real format"
fi

# ---------------------------------------------------------------------------
# Cell H: "attribution" in the MARKETING sense must not score the session class
# (AF-392). Cell E covers a word appearing only in PASTED CONTEXT; this is the
# other miss, where Ethan writes the word himself in a different domain. Three of
# three hits on 2026-09-01 were GTM measurement attribution, and n=3 against a
# 0.15/day baseline read as a 20x spike that was entirely noise.
#
# Pins BOTH directions against the SHIPPED pattern, because a fix that simply
# stopped matching would satisfy the first half alone.
# ---------------------------------------------------------------------------
if python3 - "$(pwd)/scripts/friction_themes.py" <<'ATTRPY'
import importlib.util, re, sys
spec = importlib.util.spec_from_file_location("ft", sys.argv[1])
m = importlib.util.module_from_spec(spec); spec.loader.exec_module(m)
pat = dict((k, p) for k, p, _ in m.RULE_CLASSES)["attribution"]
rx = re.compile(pat, re.I)

marketing = [
    "go thru our gtm playbooks and tickers and make sure we have measurement attribution",
    "we should also have the ability to attribute some kind of meta-properties to each email",
]
session = [
    "the staged-guard named the wrong session as the editor, fix the attribution",
    "who wrote this file? the blame says one thing and the record says another",
    "every write should carry X-Amux-Session so the audit trail names who made it",
    "this misattributed my commit to another lane",
]
for t in marketing:
    assert not rx.search(t), "marketing sense scored the session class: " + t
for t in session:
    assert rx.search(t), "a real attribution restatement stopped matching: " + t
ATTRPY
then
  ok "H: marketing 'attribution' does not score the session class, and real ones still do"
else
  bad "H: the attribution class no longer discriminates the marketing sense from the session one"
fi

# ---------------------------------------------------------------------------
# Cell I: an entry describing a success report contradicted by an empty result
# reaches the instruments cluster whatever subsystem produced it (AF-394).
#
# AREA_CANON is first-match-wins over a title that leads with its subsystem, so
# "Engine/batches (a batch reports COMPLETED ... writes ZERO documents)" is
# `engine` and the "Instruments that lie" theme read QUIET while three fresh
# instances sat in the same scan. Reordering would have fixed nothing: 13 of the
# 16 such entries contain no word from the instruments arm at all.
#
# Pins the SURGICAL property, not just the membership. The extra label must ADD
# and never MOVE, because every subsystem cluster's trailing baseline depends on
# its count staying comparable.
# ---------------------------------------------------------------------------
if python3 - "$(pwd)/scripts/friction_themes.py" <<'CANONPY'
import importlib.util, sys
spec = importlib.util.spec_from_file_location("ft", sys.argv[1])
m = importlib.util.module_from_spec(spec); spec.loader.exec_module(m)

green = [
    "Engine/batches (a batch reports COMPLETED, 100%, `failed_objects []` and writes ZERO documents)",
    "API/buckets-syncs (a sync transfers ZERO bytes, reports COMPLETED with every file synced)",
    "Engine/Write-path (SILENT WRITE LOSS: batch reported documents_written=1,813 + COMPLETED)",
]
for t in green:
    assert "instruments" in m.extra_areas(t), "green-but-empty missed: " + t
    # AND it must keep its subsystem label, or this MOVED an entry instead of adding.
    assert m.canon_area(t) != "unclassified", "subsystem label lost: " + t

# The subsystem answer is untouched: this is an ADDITIONAL membership.
assert m.canon_area(green[0]) == "engine", m.canon_area(green[0])
assert m.canon_area(green[1]) == "api-contract", m.canon_area(green[1])

# THE 2026-09-04 WIDENING, one specimen per new arm, in the fleet's real wording.
# The three originals above all say "reports COMPLETED", so they pin the WORDING
# of three specimens rather than the class; measured that day, 63 further open
# entries were the same shape in different words and the membership missed every
# one. Without these cells the new arms can be deleted and everything stays green.
for t in [
    # a 2xx carrying nothing
    "API/routing (documents/list through the SHARED host returns a silent 200-empty for a dedicated-tenant namespace)",
    "API/retrievers (three filter-operator spellings, one 400s, one works, and one returns HTTP 200 with zero rows)",
    # a success status contradicted in the same breath
    "API/retrievers (`query_expand` exceeds a 6000ms hard ceiling, is cancelled, and reports HTTP 200 / status: completed)",
    # work discarded while the call answers normally
    "API/Ingestion — `objects/batch` silently drops any blob whose URL its server-side fetcher cannot reach",
    "MVS (the vector store's `count()` SILENTLY IGNORES its `filters` argument and returns the NAMESPACE total)",
    # a control that answers and does not act
    "API/apps — setting an App `is_active: false` does NOT take it offline; the public URL keeps serving",
    "API/Retrievers — `post_filters` is typed, documented, autocompleted, and never applied by any stage",
    # the class stated outright
    "API/collections (a document can be listed and still be unsearchable, and nothing says which)",
]:
    assert "instruments" in m.extra_areas(t), "widened arm missed its own specimen: " + t
    assert m.canon_area(t) != "unclassified", "subsystem label lost by the widening: " + t

# NEGATIVE: an ordinary defect must not acquire the label, or instruments
# absorbs the whole ledger and the theme stops discriminating.
#
# The last four are the ones the WIDENING could plausibly over-match: a bare
# success word, a bare "green", a plain 200, and an ordinary "does not" that is
# not a control failing to act. All four were measured against the shipped
# pattern before it was widened and must stay out.
for t in [
    "Studio/retrievers (the input-type picker offered a type the API rejects at create time)",
    "API/manifest (POST /v1/manifest/diff ignores X-Namespace-Id and runs org-wide)",
    "CI/Security (the weekly full-tree secret scan shares a cancel-in-progress group)",
    "CI (the suite is green on main and the nightly deep run is red, and nobody owns the difference)",
    "API/auth (a valid key returns 200 and the docs example returns 401, so the example is wrong)",
    "Studio/pricing (the estimate does not match the invoice by a few cents on annual plans)",
    "Docs/api-reference (the endpoint is documented and the SDK method name does not match it)",
]:
    assert m.extra_areas(t) == [], "ordinary defect wrongly labelled instruments: " + t
CANONPY
then
  ok "I: green-but-empty reaches instruments, keeps its subsystem, and ordinary defects do not"
else
  bad "I: the cross-cutting label either missed its class, moved an entry, or over-matched"
fi

# ---------------------------------------------------------------------------
# Cell J: amux's own appended text is not the human repeating himself.
#
# `cross-lane-repeat` read n=14 on 2026-09-04 and 4 of the 14 were the @-mention
# footer amux appends. MSG-40511 and MSG-42067 share 194 identical characters and
# nothing else: one asks to add public datasets to a table, the other asks for MVS
# throughput metrics. The signal called them the same instruction to two lanes.
#
# The cell asserts the two directions that matter: the footer is removed, and the
# human's own words in front of it survive intact. Stripping the whole message
# would pass a naive "footer is gone" check and delete the signal.
# ---------------------------------------------------------------------------
if python3 - "$(pwd)/scripts/friction_themes.py" <<'FOOTERPY'
import importlib.util, sys
spec = importlib.util.spec_from_file_location("ft", sys.argv[1])
m = importlib.util.module_from_spec(spec); spec.loader.exec_module(m)

FOOT = ('\n\n[amux: @-mentions above are amux workers, NOT files. Reach them via HTTP '
        'API, never tmux directly]\n  @mvs-research \u2192 POST $AMUX_URL/api/sessions/'
        'mvs-research/send  {"text":"<msg>"}')
a = "[02:39 PM] add the public datasets that `@mvs-research` is working on to the table md" + FOOT
b = "[10:11 AM] I want MVS to have system level logging/metrics for write/import throughput" + FOOT

ia, ib = m.instruction_of(a), m.instruction_of(b)
assert "[amux:" not in ia and "[amux:" not in ib, "the appended footer survived the strip"
assert "public datasets" in ia, "the human's own words were stripped too: " + repr(ia)
assert "system level logging" in ib, "the human's own words were stripped too: " + repr(ib)

# THE PROPERTY. Two unrelated instructions must share no shingle once the footer
# is gone. Before the strip they shared the footer's phrases and scored as a
# cross-lane repeat.
assert not (set(m.shingle(ia)) & set(m.shingle(ib))), \
    "two unrelated instructions still share a phrase: " + repr(set(m.shingle(ia)) & set(m.shingle(ib)))

# NEGATIVE CONTROL: a message with no footer is untouched, and two genuinely
# identical instructions must STILL match, or the strip has deleted the signal.
plain = "review your backlog and todo see whats still relevant, discard what is not"
assert m.instruction_of(plain) == plain, "a footerless message was altered"
assert set(m.shingle(m.instruction_of(plain))) & set(m.shingle(m.instruction_of(plain + FOOT))), \
    "the same instruction with and without a footer must still match"
FOOTERPY
then
  ok "J: amux's appended footer is stripped, the human's words survive, real repeats still match"
else
  bad "J: the footer strip either left amux's text in, ate the human's, or killed the signal"
fi

# ---------------------------------------------------------------------------
# Cell D: `continue` is fleet operation, not a rule restatement.
# ---------------------------------------------------------------------------
if echo "$OUT" | python3 -c "
import json,sys
d = json.load(sys.stdin)
for s in d['active']:
    if s['key'] == 'rule-restatement:idle-stall':
        texts = [e['text'] for e in s['evidence']]
        bare = [t for t in texts if t.strip().lower().endswith('continue')]
        assert not bare, f'bare drive counted as a restatement: {bare}'
"; then
  ok "D: bare 'continue' drives do not score the idle-stall rule"
else
  bad "D: bare drive commands are being counted as rule restatements"
fi

# ---------------------------------------------------------------------------
# Cell E: a word appearing only in PASTED CONTEXT must not score its class.
# ---------------------------------------------------------------------------
if echo "$OUT" | python3 -c "
import json,sys
d = json.load(sys.stdin)
perms = [s for s in d['active'] if s['key'] == 'rule-restatement:permissions']
assert not perms, (
    'permissions scored on pasted context: the only messages containing those '
    'words have \"clear the backlog\" as their instruction')
"; then
  ok "E: pasted context below an instruction does not score a rule class"
else
  bad "E: rule classes are matching text Ethan pasted, not text he wrote"
fi

# ---------------------------------------------------------------------------
# Cell F: a growing pile is active; a SHRINKING pile of the same size is not.
# A standing count would fire on both, which is the accumulate-vs-discriminate
# failure the whole file is built to avoid.
# ---------------------------------------------------------------------------
if echo "$OUT" | python3 -c "
import json,sys
d = json.load(sys.stdin)
keys = {s['key'] for s in d['active']}
assert 'board-resting:mixpeek:needsyou' in keys, \
    'the GROWING mixpeek pile did not fire'
assert 'board-resting:amux:needsyou' not in keys, \
    'the SHRINKING amux pile fired: the signal is a standing count, not a delta'
"; then
  ok "F: growing pile fires, shrinking pile of the same size does not"
else
  bad "F: board-resting is not discriminating growth from size"
fi

# ---------------------------------------------------------------------------
# Cell G: a timestamp unit flip is announced, not silently absorbed.
# ---------------------------------------------------------------------------
cp "$DB" "$TMP/flip.db"
python3 - "$TMP/flip.db" <<'PY'
import sqlite3, sys
con = sqlite3.connect(sys.argv[1])
# One row written in the wrong unit, as a future migration might.
con.execute("UPDATE issues SET created = created * 1000 WHERE id = 'BACKE-0'")
con.commit()
PY
if AMUX_DB="$TMP/flip.db" AMUX_REPO="$TMP/amux" MIXPEEK_REPO="$TMP/mixpeek" \
   python3 "$SCAN" --json 2>/dev/null | python3 -c "
import json,sys
d = json.load(sys.stdin)
w = d.get('timestamp_unit_warning')
assert w and 'issues.created' in w, f'unit flip not announced: {w!r}'
"; then
  ok "G: a timestamp unit flip surfaces as a warning"
else
  bad "G: a timestamp unit flip is absorbed silently"
fi

# H. AF-450: `e2e` as a task LABEL is not a restatement of the verification rule.
#
# The bare \be2e\b arm counted Ethan's own harness prompt names — measured
# 2026-09-03, 9 of the signal's 18 window hits fired on that arm alone and every
# one was a label. THE CONTROL IS THE SECOND HALF: a real demand mentioning e2e
# must still match, or the fix has deleted the signal rather than sharpened it.
if python3 - <<'PY'
import re, sys
sys.path.insert(0, "scripts")
import importlib.util
spec = importlib.util.spec_from_file_location("ft", "scripts/friction_themes.py")
m = importlib.util.module_from_spec(spec); spec.loader.exec_module(m)
pat = dict((k, p) for k, p, _ in m.RULE_CLASSES)["verification"]
rx = re.compile(pat, re.I)
LABELS = [
    "ISOLATED-E2E-20260903: Reply only with isolated-ok.",
    "September 3 Gemini E2E workflow. Work only in /tmp/amux-reverify-20260903/gemini",
    "E2E-Q-20260903-CLAUDE: What is the difference between todo and backlog?",
    "September 3 Claude E2E workflow. Work only in /tmp/amux-reverify-20260903/claude",
]
DEMANDS = [
    "did you run e2e?", "no e2e tests?", "run the e2e suite before you call it done",
    "did the e2e pass", "rerun e2e",
]
bad = [s for s in LABELS if rx.search(s)]
assert not bad, f"harness prompt NAMES still count as restatements: {bad}"
missed = [s for s in DEMANDS if not rx.search(s)]
assert not missed, f"real e2e demands no longer match — the arm was deleted, not fixed: {missed}"
# And the arms that never had this problem must be untouched.
assert rx.search("did you verify it in prod?"), "the verify/in-prod arms regressed"
PY
then
  ok "H: e2e as a label is not a restatement, and a real e2e demand still is"
else
  bad "H: the e2e arm counts task labels, or the fix deleted real demands"
fi

# ---------------------------------------------------------------------------
# I: concentration — one incident must not read as a recurring class (AF-511)
#
# `n` alone cannot separate a class from one long incident. Measured 2026-09-05:
# idle-stall (7.5x baseline), deploy-live (13x) and verification were the day's
# three loudest signals and all three were ~90% one lane over two lane-days —
# one ATE-44 incident. docs/friction-themes.md increments OCCURRENCES from these,
# and its own header calls an inflated OCCURRENCES the one way it can corrupt
# itself.
# ---------------------------------------------------------------------------
if python3 - <<'PY'
import importlib.util, time
spec = importlib.util.spec_from_file_location("ft", "scripts/friction_themes.py")
m = importlib.util.module_from_spec(spec); spec.loader.exec_module(m)
DAY = 86400_000
# A FIXED midday, not time.time(): five minutes back from a run at 00:01 UTC
# crossed midnight and read as two lane-days (CI red 2026-10-01 00:01Z).
# Case J below is the one that tests the midnight edge on purpose.
now = 1788611400_000  # 2026-09-05T12:30:00Z: no zone has local midnight within 5 minutes of it

inc = m.concentration([("amux-testing-e2e", now - i * 60_000) for i in range(5)])
assert inc["distinct_lanes"] == 1, inc
assert inc["distinct_lane_days"] == 1, inc
assert inc["top_lane_share"] == 1.0, inc
assert inc["incident_shaped"] is True, inc

# THE CONTROL. Same n=5, five lanes, five days: a real class. Without this a
# helper returning incident_shaped=True unconditionally passes the block above.
cls = m.concentration([("lane-%d" % i, now - i * DAY) for i in range(5)])
assert cls["distinct_lanes"] == 5, cls
assert cls["distinct_lane_days"] == 5, cls
assert cls["top_lane_share"] == 0.2, cls
assert cls["incident_shaped"] is False, cls

# n is identical and the verdicts are opposite, which is the whole point.
assert inc["sampled_over"] == cls["sampled_over"] == 5

# n=2 from one lane on one day is NOT an incident: nothing distinguishes it from
# ordinary conversation, and calling it one would suppress small real signals.
assert m.concentration([("a", now), ("a", now - 60_000)])["incident_shaped"] is False

# An empty set says NOTHING rather than a clean zero (ethos rule 4).
assert m.concentration([]) is None
assert m.concentration([("a", None)]) is None
PY
then
  ok "I: one clustered lane is incident-shaped; the same n over five lanes is not"
else
  bad "I: concentration cannot separate an incident from a class"
fi

# ---------------------------------------------------------------------------
# J: the incident verdict must not depend on where MIDNIGHT falls (AF-585)
#
# Block I above pins the incident shape, and it passed for three days while the
# production path could not fire it once. Its timestamps are clustered at `now`,
# so they land on one calendar date ~99.7% of the day; real evidence comes from
# a ROLLING 24h window, which always covers two dates. On 2026-09-08 all 11
# active signals reported distinct_days=2 and incident_shaped=False, including
# deploy-live at n=39 from ONE lane at 100% — the exact specimen block I cites.
#
# Fixed timestamps, deliberately: `now` is what hid this.
# ---------------------------------------------------------------------------
if python3 - <<'PY'
import importlib.util, time
spec = importlib.util.spec_from_file_location("ft", "scripts/friction_themes.py")
m = importlib.util.module_from_spec(spec); spec.loader.exec_module(m)

def at(s):
    return time.mktime(time.strptime(s, "%Y-%m-%d %H:%M")) * 1000

# ONE incident, five messages, four minutes wide. Placed twice: mid-afternoon,
# and straddling midnight. The data is identical; only the wall clock differs.
noon = [("amux-testing-e2e", at("2026-09-08 12:00") - i * 60_000) for i in range(5)]
midn = [("amux-testing-e2e", at("2026-09-08 00:02") - i * 60_000) for i in range(5)]
a, b = m.concentration(noon), m.concentration(midn)

assert a["distinct_days"] == 1 and b["distinct_days"] == 2, (a, b)
assert a["incident_shaped"] is True, a
assert b["incident_shaped"] is True, b   # <-- False before AF-585
assert a["sampled_over"] == b["sampled_over"] == 5

# The control still has to hold at the same window: spread lanes over spread
# days is not an incident however the dates fall.
DAY = 86400_000
cls = m.concentration([("lane-%d" % i, at("2026-09-08 12:00") - i * DAY)
                       for i in range(5)])
assert cls["incident_shaped"] is False, cls

# THE CITED SPECIMEN, at its real measured shape: deploy-live on 2026-09-08 was
# n=39 from one lane spanning 21.1h of a 24h window. A "clustered in time"
# threshold suppresses it from the opposite side to the calendar bug, so pin it
# directly rather than trusting that a burst test covers it.
long_run = m.concentration([("amux-testing-e2e",
                             at("2026-09-08 08:51") - i * (21.1 * 3600_000 / 38))
                            for i in range(39)])
assert long_run["distinct_lanes"] == 1, long_run
assert long_run["evidence_span_hours"] >= 21.0, long_run
assert long_run["incident_shaped"] is True, long_run
PY
then
  ok "J: the same incident reads incident-shaped at noon and across midnight"
else
  bad "J: incident_shaped is decided by the calendar date, not by the data"
fi

# ---------------------------------------------------------------------------
# K: one message to many lanes is not many lanes hitting one friction (AF-585)
#
# The mirror of J and the more dangerous half, because every other number makes
# a broadcast look MORE like a theme: maximum distinct_lanes, minimum top-lane
# share. Measured 2026-09-08 in a single 24h window: 41 lanes got one identical
# message in the minute at 18:09, 28 in another, and "continue" reached 24 and
# 19. rule-restatement:idle-stall then reported n=83 over 55 lanes with a 31%
# top-lane share, which is the most theme-shaped concentration a signal can
# have, and OCCURRENCES increments off exactly that.
# ---------------------------------------------------------------------------
if python3 - <<'PY'
import importlib.util, time
spec = importlib.util.spec_from_file_location("ft", "scripts/friction_themes.py")
m = importlib.util.module_from_spec(spec); spec.loader.exec_module(m)
t = time.mktime(time.strptime("2026-09-08 12:00", "%Y-%m-%d %H:%M")) * 1000
DAY = 86400_000

# The real shape: one send, 41 lanes, one minute.
bc = m.concentration([("lane-%d" % i, t) for i in range(41)])
assert bc["distinct_lanes"] == 41, bc          # looks maximally broad
assert bc["top_lane_share"] < 0.03, bc         # and maximally unconcentrated
assert bc["widest_minute_lanes"] == 41, bc
assert bc["fanout_share"] == 1.0, bc
assert bc["broadcast_shaped"] is True, bc
assert bc["incident_shaped"] is False, bc

# THE CONTROL, and it is the one that matters: a genuine class must NOT be
# called a broadcast. Forty-one lanes, but each on its own day and its own
# minute. Without this, returning broadcast_shaped=True unconditionally passes.
real = m.concentration([("lane-%d" % i, t - i * DAY - i * 60_000)
                        for i in range(41)])
assert real["distinct_lanes"] == 41, real
assert real["widest_minute_lanes"] == 1, real
assert real["fanout_share"] == 0.0, real
assert real["broadcast_shaped"] is False, real

# Same n, same lane count, opposite verdicts — the whole point of the block.
assert bc["sampled_over"] == real["sampled_over"] == 41

# A handful of lanes coinciding in one minute is coincidence, not a broadcast.
few = m.concentration([("lane-%d" % i, t) for i in range(3)])
assert few["broadcast_shaped"] is False, few
PY
then
  ok "K: 41 lanes in one minute is a broadcast; 41 lanes over 41 days is a class"
else
  bad "K: concentration reports a fan-out as maximal breadth"
fi

# ---------------------------------------------------------------------------
# J: concentration is computed over the FULL set, never the displayed sample
#
# The cell that caught a real error. The 2026-09-05 sweep reported "6/6 shown:
# amux-testing-e2e" and concluded one lane; over the full 11 rows it is 2 lanes
# at 91%. The sample was truncated at MAX_PER_SIGNAL=6 and the conclusion was
# drawn from the truncation.
# ---------------------------------------------------------------------------
if FRICTION_MAX_EVIDENCE=2 python3 - <<'PY'
import importlib.util, time
spec = importlib.util.spec_from_file_location("ft", "scripts/friction_themes.py")
m = importlib.util.module_from_spec(spec); spec.loader.exec_module(m)
assert m.MAX_PER_SIGNAL == 2, "the cap under test did not take effect"
now = int(time.time() * 1000)
pairs = [("lane-a", now), ("lane-a", now - 1000)] + [("lane-b", now - i) for i in range(2, 8)]
c = m.concentration(pairs)
assert c["sampled_over"] == 8, "computed over the sample, not the population: %s" % (c,)
assert c["top_lane"] == "lane-b", c
assert c["distinct_lanes"] == 2, c
PY
then
  ok "J: concentration counts every row, not the MAX_PER_SIGNAL sample"
else
  bad "J: concentration is measuring the truncation instead of the signal"
fi

# ---------------------------------------------------------------------------
# K: the SHIPPED script hands concentration every row, not the evidence sample
#
# Cell J proves the helper counts what it is given. It does NOT prove the CALL
# SITE gives it everything, and that is a different rule: a mutation changing
# `in_win` to `in_win[:MAX_PER_SIGNAL]` at the call site left J green. Measured
# while writing this file, which is the same wiring-vs-wording gap that let three
# other suites pass over deleted call sites today.
#
# So this drives the shipped scanner, per this file's own opening rule.
# ---------------------------------------------------------------------------
KDB="$TMP/conc.db"
python3 - "$KDB" "$NOW_MS" <<'PY'
import sqlite3, sys
db, now = sys.argv[1], int(sys.argv[2])
con = sqlite3.connect(db)
con.execute("""CREATE TABLE cmd_history (id INTEGER PRIMARY KEY AUTOINCREMENT,
    text TEXT NOT NULL, type TEXT NOT NULL DEFAULT 'direct',
    session TEXT NOT NULL DEFAULT '', ts INTEGER NOT NULL,
    origin TEXT NOT NULL DEFAULT '', card_id TEXT, delivery TEXT,
    queued_at INTEGER, delivered_at INTEGER, submit_verdict TEXT)""")
con.execute("""CREATE TABLE issues (id TEXT PRIMARY KEY, title TEXT NOT NULL,
    "desc" TEXT NOT NULL DEFAULT '', status TEXT NOT NULL DEFAULT 'todo',
    session TEXT, creator TEXT NOT NULL DEFAULT '', due TEXT,
    created INTEGER NOT NULL, updated INTEGER NOT NULL, deleted INTEGER,
    archived INTEGER NOT NULL DEFAULT 0, closed_at INTEGER)""")
M = 60_000
# 8 restatements of one rule, 6 from one lane and 2 from another, all inside the
# window. With the evidence cap at 2 below, a call site that passed the sample
# would see 2 rows and one lane.
for i in range(6):
    con.execute("INSERT INTO cmd_history (text,type,session,ts,origin) VALUES (?,?,?,?,?)",
                ("did you verify it in prod?", "user", "lane-loud", now - (i + 1) * M, ""))
for i in range(2):
    con.execute("INSERT INTO cmd_history (text,type,session,ts,origin) VALUES (?,?,?,?,?)",
                ("did you verify it in prod?", "user", "lane-quiet", now - (i + 20) * M, ""))
con.commit()
PY
if AMUX_DB="$KDB" AMUX_REPO="$TMP/amux" MIXPEEK_REPO="$TMP/mixpeek" \
   FRICTION_DAYS=1 FRICTION_MAX_EVIDENCE=2 python3 "$SCAN" --json 2>/dev/null \
 | python3 -c '
import json, sys
d = json.load(sys.stdin)
sig = [s for s in d["active"] if s["key"] == "rule-restatement:verification"]
assert sig, "the seeded rule class did not fire: %s" % [s["key"] for s in d["active"]]
s = sig[0]
c = s["concentration"]
assert c, "the shipped script emitted no concentration at all"
assert len(s["evidence"]) == 2, "the evidence cap did not apply: %d" % len(s["evidence"])
assert c["sampled_over"] == 8, \
    "call site passed the SAMPLE, not the population: sampled_over=%s" % c["sampled_over"]
assert c["distinct_lanes"] == 2, c
assert c["top_lane"] == "lane-loud" and c["top_lane_share"] == 0.75, c
'
then
  ok "K: the shipped scanner computes concentration over all 8 rows while showing 2"
else
  bad "K: the call site is passing the truncated evidence sample"
fi

echo
if python3 scripts/test-friction-attachments.py "$SCAN"; then
  ok "L: actual cross-lane signal excludes upload metadata and preserves real instructions"
else
  bad "L: attachment metadata still contaminates cross-lane instruction evidence"
fi

echo "  $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
