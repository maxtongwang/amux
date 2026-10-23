# Usage attribution and equivalents

The Cost tab and `GET /api/usage/report.md` show **list-price equivalent on Max
subscriptions, not billed**. All breakdowns use the same configured rates and
counters as the headline. Unpriced responses contribute tokens but no equivalent
dollars; the priced subtotal has `cost_status: "partial"`. Empty models count as
unpriced even if an explicit default rate exists. Known zero rates remain
separate from unknown rates. `billed_usd: null` and `billing_measured: false` do
not claim that a subscription invoice was zero.

## Recorded work provenance

Validated native SessionStart and UserPromptSubmit observations snapshot work
against the durable `_amux_workers.id` (or the existing legacy
`CC_INCARNATION`, exposed as `incarnation:<id>`), conversation and native turn ID.
Legacy identities retain observed aliases and incarnation-bound rename history
in a usage dimension; this does not create or modify a worker lifecycle. The
response writer persists that identity, response/request IDs, attempt/generation,
one primary typed link, reason/source and effective time. Renames retain the
stable ID; current and historical aliases are displayed together.

Card attribution uses bounded `task_attempts`, project continuation history and
historical `task.claimed` intervals. Intervals are half-open and capped at 24 hours;
claims stop when the attempt era starts, including across aliases. A validated
fresh native turn can renew a long active card from its current lease; historical
backfill alone cannot extend the 24-hour cap. A lease without an attempt is
bounded by its recorded expiry. Overlapping different cards/generations
and contradictory snapshots remain explicitly conflicting. A later card cannot
relabel an already recorded response. Child runs inherit the parent's work at the
child transcript's recorded start, rather than the parent's later card. Missing
or expired child-start evidence stays unknown. Isolated native observations
record no work links; their children inherit that restriction.

When no card exists, the existing worker environment editor/API can set
`AMUX_WORK_REFERENCES` to a JSON array with exactly one structured external link:

```json
[{"kind":"issue","id":"owner/repo#258","source":"owner","effective_at":1792771200}]
```

`kind` is `pr`, `issue` or `deal`; IDs must be nonempty, have no surrounding whitespace, be at most 512 characters,
and contain no control characters. The timestamp is Unix seconds, no more than
24 hours old and never in the future. Capture stamps the validated source as
`worker_environment`. Remove the setting to clear it at the next native turn.
Multiple, malformed or stale links produce an explicit unknown/conflicting
reason. External references are never extracted from prompt prose. Current
worker configuration is consulted only at capture, never for historical work
backfill. Historical attribution backfill fills ledger provenance from retained
intervals; missing evidence is visible as a coverage gap. Existing response IDs
and historical counters require a separate reviewed repair, not this migration.

## Shared read contract

`GET /api/observability`, `GET /api/usage/report.md` and
`GET /api/observability/ledger` accept these selectors:

| Query | Meaning |
|---|---|
| `days` | Rolling days, 1–365; default 7 |
| `since`, `until` | Exact Unix-second range `[since, until)`; defaults returned in JSON |
| `worker_id` | Durable worker ID; spans aliases/resumes |
| `session` | Recorded alias, also resolves its unique durable ID; empty means fleet |
| `unknown_worker=true` | Only conversations with no recorded owner and no stable worker ID |
| `group` | Existing group membership; applies without a worker/session selector |
| `conversation` | Exact persisted provider conversation key |
| `work_kind`, `work_id` | Both required; kind is `card`, `pr`, `issue` or `deal` |
| `unattributed=true` | Responses with no primary work link |

Days and timestamps are UTC, including day buckets. Invalid selectors return 400
with `error`, `measured: false` and `n_considered: 0`. Store/query failures return
503 (500 for the markdown report) with the same measurement fields. A successful
empty read is `measured: true, n_considered: 0`, distinct from a failed read.

The JSON rollup includes `by_work_reference`, `by_attribution_reason`,
`by_attribution_source`, `by_session`, `by_conversation`, `by_day`, and `by_model`,
plus response attribution coverage and ledger freshness. Card titles come from
live `issues`, not retired `task_windows`. `by_task` remains a compatibility alias
for the work-reference breakdown. Legacy rows awaiting provenance backfill say
`historical_attribution_pending`; requests never substitute today's card.

`cost_basis` includes the exact label, USD-per-million rates, a SHA-256 version of
the effective configured table and declared Claude fallback, and an explicitly unverified/null as-of date.
Delegated figures are included in the total. All groups use that same table,
rather than legacy stored money values. Configured defaults remain in
`runtime_jobs/token_ledger.rs`; the existing prices configuration overrides them.

The ledger endpoint adds `limit` (1–500, default 100) and `after_id` (nonnegative,
default 0). It returns ascending stable ledger IDs, `rows`, and
`next_after_id` (null at the end). Hold the returned `since` and `until` fixed
while paging. Rows include four token counters in
`[input, cache_read, cache_write, output]` order, `context_tokens`, attribution,
response/request/parent IDs and compaction evidence. Context is input-side tokens
per response. An explicit retained transcript compaction boundary is evidence;
its absence has `measured: false`, not a claim that compaction never happened.

The Cost tab exposes the same scopes as expandable work/worker/conversation rows,
then UTC days and individual responses. No worker identity and no work reference
are separate facts; unattributed work does not measure idleness.

## Accounting and verification

Claude fragments merge each counter's maximum before pricing. Cross-pass replay
uses the same component-wise maxima and reprices them in the serialized writer;
`MAX(old_cost, new_cost)` cannot price complementary fragments correctly. Cursor
and response writes commit together. Incomplete trailing JSON waits for the next
pass, and an unattached compaction boundary stays before the cursor.

Codex uses cumulative counter differences, suppresses repeated cumulative events,
and replays the prefix for model/counter carryover before emitting only new rows.
A native ordinal remains the response key when present; missing ordinals use a
stable byte boundary. Last-only events are retained with a logged cumulative
measurement gap. Historical legacy keys are not rewritten. Logs expose
`ledger_counter_fragments_merged`, `ledger_counter_replay_merged`,
`codex_cumulative_reset`, `codex_cumulative_unavailable` and
`usage_attribution_coverage_gap` verdicts.

Run the Cost checks in [VERIFY.md](../VERIFY.md). Rust tests cover fragments,
replay, aliases, transitions, gaps, conflicts, children, external references,
unknown/isolated work, boundary/filter/pagination behavior and matching pricing.
The Node tests execute the real Cost renderer and drilldown request code.
