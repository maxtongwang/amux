// scripts/e2e-worker-control.mjs — the worker-concurrency control report must count every case per
// setting, keep a run with no evidence UNMEASURED (never a pass), and refuse mixed SHAs or settings.
import test from 'node:test';
import assert from 'node:assert/strict';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { aggregate, classify, formatReport, report, summarizeRun } from '../scripts/e2e-worker-control.mjs';

const FIXTURES = path.join(path.dirname(fileURLToPath(import.meta.url)), 'fixtures', 'e2e-worker-control');
const SHA = 'a'.repeat(40);
const byLabel = (agg, label) => agg.cases.find(c => c.label === label);

test('fixture control: per-setting run states, newest attempt wins, missing shards stay unmeasured', () => {
  const agg = report(FIXTURES);
  assert.deepEqual(agg.errors, []);
  assert.equal(agg.sha, SHA);
  assert.equal(agg.n_considered, 4);
  assert.equal(agg.measured, true);
  const [w1, w2] = agg.settings;
  assert.deepEqual([w1.workers, w1.runs, w1.complete, w1.partial, w1.unmeasured], [1, 2, 1, 0, 1]);
  assert.deepEqual([w2.workers, w2.runs, w2.complete, w2.partial, w2.unmeasured], [2, 2, 1, 1, 0]);
  // draw-realtime: passed in the one measured workers=1 run, failed in the one
  // workers=2 run whose shard 2 has evidence; the other run of each is unmeasured.
  const draw = byLabel(agg, 'desktop › draw-realtime.spec.ts › p95 under budget');
  assert.deepEqual(draw.per[1], { pass: 1, fail: 0, skip: 0, unfinished: 0, unmeasured: 1 });
  assert.deepEqual(draw.per[2], { pass: 0, fail: 1, skip: 0, unfinished: 0, unmeasured: 1 });
  // Run 102 shard 1: attempt 1 failed t1, attempt 2 passed t1 and never finished
  // t2. Only attempt 2 counts, so t1 is a pass and t2 is unfinished, not passed.
  assert.deepEqual(byLabel(agg, 'desktop › golden.spec.ts › loads').per[2], { pass: 2, fail: 0, skip: 0, unfinished: 0, unmeasured: 0 });
  assert.deepEqual(byLabel(agg, 'mobile › golden.spec.ts › loads').per[2], { pass: 1, fail: 0, skip: 0, unfinished: 1, unmeasured: 0 });
  assert.deepEqual(byLabel(agg, 'desktop › todo.spec.ts › skipped one').per[1], { pass: 0, fail: 0, skip: 1, unfinished: 0, unmeasured: 1 });
});

test('fixture control: wall times and budgets include a job that uploaded no evidence', () => {
  const agg = report(FIXTURES);
  const [w1, w2] = agg.settings;
  assert.deepEqual(w1.exec_ms, { median: 1000000, max: 1100000 });
  assert.deepEqual(w2.exec_ms, { median: 600000, max: 780000 });
  assert.equal(w2.job_ms.max, 31 * 60000, 'run 102 shard 2 ran 31 min and was cancelled before uploading');
  assert.equal(w2.over_job_budget, 1);
  assert.equal(w1.over_job_budget, 0);
  assert.equal(w2.over_exec_budget, 0);
  assert.equal(w2.shards_with_evidence, 3);
  const run202 = agg.runs.find(r => r.run_id === '202');
  assert.equal(run202.state, 'unmeasured');
  assert.match(run202.why_unmeasured, /no e2e-evidence artifact/);
});

test('the printed report names unmeasured runs and shows only cases that failed or did not finish', () => {
  const text = formatReport(report(FIXTURES));
  assert.match(text, /2 of 4 observed case\(s\) failed or did not finish/);
  assert.match(text, /draw-realtime\.spec\.ts › p95 under budget\s+1\/0\/0\/1\s+0\/1\/0\/1/);
  assert.match(text, /202 workers=1 unmeasured: no e2e-evidence artifact/);
  assert.match(text, /102 workers=2 partial: 1\/2 shard\(s\) with evidence/);
  assert.doesNotMatch(text, /todo\.spec\.ts/);
  assert.doesNotMatch(text, /INTEGRITY ERRORS/);
  assert.match(formatReport(report(FIXTURES), { all: true }), /todo\.spec\.ts › skipped one/);
});

const shard = (over = {}) => ({
  shard: 1, control: { workers: 2, sha: SHA },
  manifest: { shard: { current: 1, total: 1 }, tests: [{ id: 'x', title: ['', 'desktop', 'a.spec.ts', 'x'], project: 'desktop' }] },
  events: JSON.stringify({ event: 'finished', id: 'x', retry: 0, status: 'passed', expected: 'passed', outcome: 'expected' }) + '\n',
  final: { status: 'passed', duration_ms: 5 }, ...over,
});

test('a complete single-shard run is complete and its case passes', () => {
  const r = summarizeRun({ runId: 1, dispatch: { workers: 2, sha: SHA }, shards: [shard()] });
  assert.equal(r.state, 'complete');
  assert.deepEqual(r.errors, []);
  assert.equal(r.cases.get('x').result, 'pass');
});

test('integrity: evidence that contradicts the dispatch record is an error, not a silent relabel', () => {
  const r = summarizeRun({ runId: 1, dispatch: { workers: 1, sha: SHA }, shards: [shard()] });
  assert.equal(r.workers, 2, 'the evidence is the operand');
  assert.match(r.errors.join('\n'), /evidence says workers=2 but it was dispatched with workers=1/);
});

test('integrity: shards disagreeing on workers, and a run head SHA that differs, are errors', () => {
  const two = [shard(), shard({ shard: 2, control: { workers: 1, sha: SHA } })];
  assert.match(summarizeRun({ runId: 1, shards: two }).errors.join('\n'), /shards disagree on workers/);
  const r = summarizeRun({ runId: 1, runMeta: { headSha: 'b'.repeat(40), status: 'completed' }, shards: [shard()] });
  assert.match(r.errors.join('\n'), /SHA disagreement/);
});

test('integrity: runs at two SHAs cannot form one control, and an empty control is an error', () => {
  const a = summarizeRun({ runId: 1, shards: [shard()] });
  const b = summarizeRun({ runId: 2, shards: [shard({ control: { workers: 1, sha: 'c'.repeat(40) } })] });
  const agg = aggregate([a, b]);
  assert.equal(agg.sha, null);
  assert.match(agg.errors.join('\n'), /more than one SHA/);
  assert.deepEqual(aggregate([]).errors, ['no runs to report']);
  assert.equal(aggregate([]).measured, false);
});

test('a run with no evidence and no dispatch record has an unknown setting and is reported as such', () => {
  const r = summarizeRun({ runId: 9, runMeta: { status: 'in_progress' }, shards: [] });
  assert.equal(r.state, 'unmeasured');
  assert.equal(r.why_unmeasured, 'run status in_progress');
  assert.match(aggregate([r]).errors.join('\n'), /run 9: worker setting unknown/);
});

test('a run missing one shard artifact is partial even when every shard it has finished cleanly', () => {
  const one = shard({ manifest: { ...shard().manifest, shard: { current: 1, total: 2 } } });
  const r = summarizeRun({ runId: 1, shards: [one] });
  assert.equal(r.state, 'partial');
  assert.deepEqual([r.measured_shards, r.shard_total], [1, 2]);
});

test('a shard with a manifest but no final.json (runner killed) leaves the run partial, its open cases unfinished', () => {
  const r = summarizeRun({ runId: 1, shards: [shard({ final: null, events: '' })] });
  assert.equal(r.state, 'partial');
  assert.equal(r.cases.get('x').result, 'unfinished');
});

test('classify follows check-e2e-evidence: declared failures pass, skips skip, everything else fails', () => {
  assert.equal(classify({ status: 'passed', expected: 'passed', outcome: 'expected' }), 'pass');
  assert.equal(classify({ status: 'failed', expected: 'failed', outcome: 'expected' }), 'pass');
  assert.equal(classify({ status: 'skipped', expected: 'skipped', outcome: 'skipped' }), 'skip');
  assert.equal(classify({ status: 'failed', expected: 'passed', outcome: 'unexpected' }), 'fail');
  assert.equal(classify({ status: 'timedOut', expected: 'passed', outcome: 'unexpected' }), 'fail');
  assert.equal(classify({ status: 'passed', expected: 'passed', outcome: 'flaky' }), 'fail');
  assert.equal(classify({ status: 'interrupted', expected: 'passed', outcome: 'unexpected' }), 'fail');
});
