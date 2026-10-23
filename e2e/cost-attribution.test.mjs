// Cost rendering and evidence selectors — refs maxtongwang/ai-config#258.
import { readFileSync } from 'node:fs';
import vm from 'node:vm';
import assert from 'node:assert/strict';
import { test } from 'node:test';
const source = readFileSync(process.env.COST_APP_SOURCE || new URL('../crates/amux-dashboard/static/app.js', import.meta.url), 'utf8');
const start = source.indexOf('function _fmtUsd(');
const end = source.indexOf('// ── Messages > Trends', start);
assert(start > 0 && end > start);
const esc = v => String(v).replaceAll('&', '&amp;').replaceAll('<', '&lt;').replaceAll('>', '&gt;').replaceAll('"', '&quot;');
const ctx = vm.createContext({ URLSearchParams, esc, escJs: esc, API: '', console });
vm.runInContext(source.slice(start, end), ctx);
const fixture = {
  since: 100, until: 200, total_cost: 3, total_tokens: 1000, total_turns: 4,
  delegated_turns: 1, delegated_cost: 1, unpriced_turns: 1,
  attribution_coverage: { measured: true, attributed_responses: 2, responses: 4, percent: 50, no_worker_identity: 1, no_work_reference: 2 },
  cost_basis: { version: 'sha256:fixture', as_of: null },
  by_work_reference: [{ title: 'Live card title', work_kind: 'card', work_id: 'A', cost: 2, work_reference: { kind: 'card', id: 'A', source: 'task_attempts' } }, { title: 'Unattributed work', cost: 1 }],
  by_session: [{ session: 'old-alias', current_alias: 'renamed', historical_aliases: ['old-alias'], worker_id: 'wrk_stable', cost: 2 }, { session: '', cost: 1 }],
  by_conversation: [{ conversation: 'conv', cost: 3 }],
  by_model: [{ model: '', cost: 0, unpriced_turns: 1 }],
  by_day: [{ day: '2026-10-23', cost: 3 }],
};
for (const perSession of [false, true]) test(`basis and unknowns on ${perSession ? 'worker' : 'fleet'} surface`, () => {
  const html = ctx._costRender(fixture, { perSession });
  assert.match(html, /list-price equivalent on Max subscriptions, not billed/);
  const api = readFileSync(new URL('../crates/amux-server/src/api/observability.rs', import.meta.url), 'utf8');
  assert.match(api, /COST_BASIS: &str = \"list-price equivalent on Max subscriptions, not billed\"/);
  assert.match(html, /Unattributed work/);
  assert.match(html, /No worker identity: 1/);
  assert.match(html, /No work reference: 2/);
  assert.match(html, /2 \/ 4 responses linked \(50.0%\)/);
  assert.match(html, /1 unpriced responses/);
  assert.match(html, /Billed dollars: unknown \(not measured\)/);
  assert.doesNotMatch(html, /Ambient|ad-hoc|In&nbsp;Progress|est\. list price/);
  assert.match(html, /UTC: \$3.00 · list-price equivalent/);
  assert.match(html, /of which delegated · equivalent, not billed/);
});
test('stable identity, alias history and work links drive exact evidence scopes', () => {
  const html = ctx._costRender(fixture);
  assert.match(html, /wrk_stable/);
  assert.match(html, /current: renamed · historical: old-alias/);
  assert.match(html, /worker_id=wrk_stable/);
  assert.match(html, /unknown_worker=true/);
  assert.match(html, /work_kind=card&amp;work_id=A/);
  assert.match(html, /unattributed=true/);
  assert.match(html, /openBoardDetail\('A'\)/);
  assert.match(html, /since=100&amp;until=200&amp;conversation=conv/);
});
test('response drilldown preserves context, provenance and explicit compaction unknown', () => {
  const rows = [{ ts: 1792771200, response_id: '<msg>', model: '', priced: false, context_tokens: 967000, historical_alias: 'old', attribution: { reason: 'no_work_reference', source: 'native_turn_snapshot', worker_id: 'wrk_stable', generation: 2, attempt: 1 }, compaction: { measured: false } }];
  const html = ctx._costResponseRows(rows);
  assert.match(html, /UTC/);
  assert.match(html, /&lt;msg&gt;/);
  assert.match(html, /context 967\.0k/);
  assert.match(html, /Unpriced/);
  assert.match(html, /Compaction: unknown/);
  assert.match(html, /no_work_reference \/ native_turn_snapshot/);
  rows[0].compaction = { measured: true, evidence: { source: 'transcript_boundary' } };
  assert.match(ctx._costResponseRows(rows), /Explicit compaction boundary/);
});
test('pagination holds exact boundaries and sends an explicit continuation cursor', async () => {
  let url;
  ctx.fetch = async u => { url = u; return { ok: true, json: async () => ({ measured: true, n_considered: 0, rows: [], next_after_id: 15 }) }; };
  const body = { innerHTML: '', querySelector: () => null, insertAdjacentHTML(_where,html) { this.innerHTML += html; } };
  const node = { dataset: { query: 'since=100&until=200&unknown_worker=true', after: '10' }, querySelector: () => body };
  await ctx._costEvidence(node,true);
  const q = new URL(url,'http://fixture').searchParams;
  assert.equal(q.get('after_id'),'10');assert.equal(q.get('until'),'200');
  assert.equal(q.get('unknown_worker'),'true');assert.equal(node.dataset.after,'15');
  assert.match(body.innerHTML,/More responses/);
});

test('an unmeasured or malformed evidence response is an error, not an empty read', async () => {
  for (const payload of [{measured:false,rows:[],n_considered:0},{measured:true,n_considered:1},{measured:true,rows:[],n_considered:1}]) {
    ctx.fetch=async()=>({ok:true,json:async()=>payload});
    const messages=[];
    ctx.document={createElement:()=>({textContent:''})};
    const node={dataset:{query:'since=100&until=200'},querySelector:()=>({querySelector:()=>null,appendChild:n=>messages.push(n.textContent)})};
    await ctx._costEvidence(node);
    assert.equal(messages.length,1);assert.match(messages[0],/Could not load evidence/);
    assert.equal(node.dataset.loaded,undefined);assert.equal(node.dataset.loading,undefined);
  }
});
