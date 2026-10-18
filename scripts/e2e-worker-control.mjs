#!/usr/bin/env node
// e2e worker control report — per-case pass/fail and wall time for each Playwright worker setting,
// read from downloaded e2e-evidence-* artifacts; a run without evidence is UNMEASURED, never a pass.
//
// usage: e2e-worker-control.mjs <control-dir> [--all] [--json]
//   <control-dir>/<run_id>/dispatch.json   {"workers": N, "sha": "<sha>"}  (written by e2e-worker-control.sh)
//   <control-dir>/<run_id>/run.json        `gh run view --json ...` output (optional)
//   <control-dir>/<run_id>/artifacts/e2e-evidence-<shard>-attempt-<n>/shard-<shard>/{control,manifest,final}.json + events.ndjson
// Exit 0 = report printed; 1 = integrity error (mixed SHAs, disagreeing worker settings, no runs); 2 = usage.
import fs from 'node:fs';
import path from 'node:path';
import { pathToFileURL } from 'node:url';

export const EXEC_BUDGET_MS = 20 * 60 * 1000; // rust.yml --global-timeout=1200000
export const JOB_BUDGET_MS = 30 * 60 * 1000; // rust.yml e2e-shards timeout-minutes: 30

// The same outcome rule as scripts/check-e2e-evidence.mjs: an expected
// failure (test.fail) that failed as declared is a pass of the case.
export function classify(event) {
  if (event.status === 'skipped' && event.outcome === 'skipped') return 'skip';
  if (event.outcome === 'expected' && event.status === event.expected &&
      ['passed', 'failed', 'timedOut'].includes(event.status)) return 'pass';
  return 'fail';
}

export function caseLabel(test) {
  return [test.project, ...(test.title || []).filter(Boolean).filter(t => t !== test.project)].join(' › ');
}

const median = xs => {
  const s = xs.filter(x => Number.isFinite(x)).sort((a, b) => a - b);
  return s.length ? s[Math.floor((s.length - 1) / 2)] : null;
};
const max = xs => { const s = xs.filter(x => Number.isFinite(x)); return s.length ? Math.max(...s) : null; };
const ms = (a, b) => (a && b ? Date.parse(b) - Date.parse(a) : null);

// Pure: one run's evidence -> its verdict. `shards` holds only the newest
// attempt of each shard that produced an artifact.
export function summarizeRun({ runId, dispatch = null, runMeta = null, shards = [] }) {
  const errors = [];
  const controls = shards.map(s => s.control).filter(Boolean);
  const workerValues = [...new Set(controls.map(c => c.workers))];
  if (workerValues.length > 1) errors.push(`run ${runId}: shards disagree on workers ${JSON.stringify(workerValues)}`);
  let workers = workerValues.length === 1 ? workerValues[0] : null;
  if (dispatch?.workers != null) {
    if (workers != null && workers !== Number(dispatch.workers))
      errors.push(`run ${runId}: evidence says workers=${workers} but it was dispatched with workers=${dispatch.workers}`);
    workers ??= Number(dispatch.workers);
  }
  const shas = [...new Set([...controls.map(c => c.sha), runMeta?.headSha, dispatch?.sha].filter(Boolean))];
  if (shas.length > 1) errors.push(`run ${runId}: SHA disagreement ${JSON.stringify(shas)}`);

  const jobs = new Map();
  for (const j of runMeta?.jobs || []) {
    const m = /^e2e-shards \((\d+)\)$/.exec(j.name || '');
    if (m) jobs.set(Number(m[1]), j);
  }
  const totals = [...new Set(shards.map(s => s.manifest?.shard?.total).filter(Number.isFinite))];
  if (totals.length > 1) errors.push(`run ${runId}: shards disagree on the shard total ${JSON.stringify(totals)}`);
  const shardTotal = totals[0] ?? (jobs.size || null);

  const cases = new Map(); // id -> { test, result: pass|fail|skip|unfinished }
  // A shard row exists for every shard that left evidence OR ran as a job, so
  // a job killed before uploading still counts against the job budget.
  const byShard = new Map(shards.map(s => [s.manifest?.shard?.current ?? s.shard, s]));
  const shardRows = [...new Set([...byShard.keys(), ...jobs.keys()])].sort((a, b) => a - b).map(n => {
    const s = byShard.get(n), job = jobs.get(n);
    return { shard: n, evidence: Boolean(s?.manifest), final: s?.final?.status ?? null, exec_ms: s?.final?.duration_ms ?? null,
      job_ms: job ? ms(job.startedAt, job.completedAt) : null, job_conclusion: job?.conclusion ?? null };
  });
  for (const s of shards) {
    const current = s.manifest?.shard?.current ?? s.shard;
    if (!s.manifest) continue;
    const finished = new Map();
    for (const line of (s.events || '').split('\n').filter(Boolean)) {
      let e;
      try { e = JSON.parse(line); } catch { errors.push(`run ${runId} shard ${current}: unparseable event line`); continue; }
      if (e.event === 'finished') finished.set(e.id, e);
    }
    for (const t of s.manifest.tests || []) {
      const e = finished.get(t.id);
      cases.set(t.id, { test: t, result: e ? classify(e) : 'unfinished' });
    }
  }
  const measuredShards = shards.filter(s => s.manifest).length;
  const incomplete = shards.some(s => s.manifest && !s.final) ||
    (shardTotal != null && measuredShards < shardTotal) || shardTotal == null;
  const state = measuredShards === 0 ? 'unmeasured' : incomplete ? 'partial' : 'complete';
  const why_unmeasured = state !== 'unmeasured' ? null
    : runMeta && runMeta.status !== 'completed' ? `run status ${runMeta.status}`
      : 'no e2e-evidence artifact (expired, never uploaded, or download failed)';
  return { run_id: String(runId), workers, sha: shas[0] ?? null, state, why_unmeasured,
    shard_total: shardTotal, measured_shards: measuredShards, shards: shardRows,
    run_ms: runMeta ? ms(runMeta.createdAt, runMeta.updatedAt) : null, cases, errors };
}

// Pure: runs -> per-setting and per-case counts.
export function aggregate(runs) {
  const errors = runs.flatMap(r => r.errors);
  if (!runs.length) errors.push('no runs to report');
  const shas = [...new Set(runs.map(r => r.sha).filter(Boolean))];
  if (shas.length > 1) errors.push(`runs span more than one SHA ${JSON.stringify(shas)}; a control compares settings at ONE SHA`);
  const settings = new Map();
  const unknown = runs.filter(r => r.workers == null);
  for (const r of unknown) errors.push(`run ${r.run_id}: worker setting unknown (no evidence and no dispatch record)`);
  for (const r of runs.filter(r => r.workers != null)) {
    if (!settings.has(r.workers)) settings.set(r.workers, { workers: r.workers, runs: [] });
    settings.get(r.workers).runs.push(r);
  }
  const order = [...settings.keys()].sort((a, b) => a - b);
  const cases = new Map(); // id -> { label, per: { workers: {pass, fail, skip, unfinished, unmeasured} } }
  for (const w of order) for (const r of settings.get(w).runs) for (const [id, c] of r.cases) {
    if (!cases.has(id)) cases.set(id, { id, label: caseLabel(c.test), per: {} });
    const per = (cases.get(id).per[w] ||= { pass: 0, fail: 0, skip: 0, unfinished: 0, unmeasured: 0 });
    per[c.result]++;
  }
  for (const c of cases.values()) for (const w of order) {
    const per = (c.per[w] ||= { pass: 0, fail: 0, skip: 0, unfinished: 0, unmeasured: 0 });
    per.unmeasured = settings.get(w).runs.length - (per.pass + per.fail + per.skip + per.unfinished);
  }
  const perSetting = order.map(w => {
    const rs = settings.get(w).runs, shards = rs.flatMap(r => r.shards);
    const count = st => rs.filter(r => r.state === st).length;
    return { workers: w, runs: rs.length, complete: count('complete'), partial: count('partial'), unmeasured: count('unmeasured'),
      runs_with_failure: rs.filter(r => [...r.cases.values()].some(c => c.result === 'fail' || c.result === 'unfinished')).length,
      exec_ms: { median: median(shards.map(s => s.exec_ms)), max: max(shards.map(s => s.exec_ms)) },
      job_ms: { median: median(shards.map(s => s.job_ms)), max: max(shards.map(s => s.job_ms)) },
      run_ms: { median: median(rs.map(r => r.run_ms)), max: max(rs.map(r => r.run_ms)) },
      over_exec_budget: shards.filter(s => s.exec_ms > EXEC_BUDGET_MS || s.final === 'timedout').length,
      over_job_budget: shards.filter(s => s.job_ms > JOB_BUDGET_MS || ['cancelled', 'timed_out'].includes(s.job_conclusion)).length,
      shards_with_evidence: shards.filter(s => s.evidence).length };
  });
  const measured = runs.some(r => r.state !== 'unmeasured');
  return { verdict: 'e2e_worker_control', measured, n_considered: runs.length, sha: shas.length === 1 ? shas[0] : null,
    settings: perSetting, cases: [...cases.values()].sort((a, b) => a.label.localeCompare(b.label)),
    runs: runs.map(({ cases: _c, ...r }) => ({ ...r, n_cases: _c.size })), errors };
}

const secs = v => (v == null ? '-' : String(Math.round(v / 1000)));
const pad = (rows) => {
  const widths = rows[0].map((_, i) => Math.max(...rows.map(r => String(r[i]).length)));
  return rows.map(r => r.map((c, i) => String(c).padEnd(widths[i])).join('  ').trimEnd()).join('\n');
};

export function formatReport(agg, { all = false } = {}) {
  const out = [];
  out.push(`e2e worker control at ${agg.sha ?? '(unknown or mixed SHA)'}: ${agg.n_considered} run(s), measured=${agg.measured}`);
  out.push('');
  out.push(pad([['workers', 'runs', 'complete', 'partial', 'unmeasured', 'runs w/ fail|unfinished',
    'shard exec s (med/max)', 'shard job s (med/max)', 'run wall s (med/max)', '>20m exec', '>30m job'],
  ...agg.settings.map(s => [s.workers, s.runs, s.complete, s.partial, s.unmeasured, s.runs_with_failure,
    `${secs(s.exec_ms.median)}/${secs(s.exec_ms.max)}`, `${secs(s.job_ms.median)}/${secs(s.job_ms.max)}`,
    `${secs(s.run_ms.median)}/${secs(s.run_ms.max)}`, s.over_exec_budget, s.over_job_budget])]));
  const ws = agg.settings.map(s => s.workers);
  const shown = all ? agg.cases : agg.cases.filter(c => ws.some(w => c.per[w].fail || c.per[w].unfinished));
  out.push('');
  out.push(all ? `All ${agg.cases.length} case(s); cells are pass/fail/unfinished/unmeasured (skips not shown):`
    : `${shown.length} of ${agg.cases.length} observed case(s) failed or did not finish in some run; cells are pass/fail/unfinished/unmeasured:`);
  if (shown.length) out.push(pad([['case', ...ws.map(w => `workers=${w}`)],
    ...shown.map(c => [c.label, ...ws.map(w => { const p = c.per[w]; return `${p.pass}/${p.fail}/${p.unfinished}/${p.unmeasured}`; })])]));
  const unmeasured = agg.runs.filter(r => r.state !== 'complete');
  if (unmeasured.length) {
    out.push('');
    out.push('Runs not fully measured (their missing shards count as unmeasured, never as passes):');
    for (const r of unmeasured) out.push(`  ${r.run_id} workers=${r.workers ?? '?'} ${r.state}: ` +
      (r.why_unmeasured ?? `${r.measured_shards}/${r.shard_total ?? '?'} shard(s) with evidence`));
  }
  if (agg.errors.length) { out.push(''); out.push('INTEGRITY ERRORS:'); for (const e of agg.errors) out.push(`  ${e}`); }
  return out.join('\n');
}

const readJson = (file) => { try { return JSON.parse(fs.readFileSync(file, 'utf8')); } catch { return null; } };
const readText = (file) => { try { return fs.readFileSync(file, 'utf8'); } catch { return null; } };

// Filesystem -> summarizeRun input. Keeps the newest attempt of each shard,
// the same rule as the e2e verdict job in rust.yml.
export function loadRun(runDir) {
  const runId = path.basename(runDir);
  const artifacts = path.join(runDir, 'artifacts');
  const newest = new Map();
  for (const name of fs.existsSync(artifacts) ? fs.readdirSync(artifacts) : []) {
    const m = /^e2e-evidence-(\d+)(?:-attempt-(\d+))?$/.exec(name);
    if (!m) continue;
    const shard = Number(m[1]), attempt = Number(m[2] || 0);
    if (!newest.has(shard) || newest.get(shard).attempt < attempt) newest.set(shard, { shard, attempt, dir: path.join(artifacts, name) });
  }
  const shards = [...newest.values()].sort((a, b) => a.shard - b.shard).map(({ shard, attempt, dir }) => {
    const inner = fs.existsSync(path.join(dir, `shard-${shard}`)) ? path.join(dir, `shard-${shard}`) : dir;
    return { shard, attempt, control: readJson(path.join(inner, 'control.json')), manifest: readJson(path.join(inner, 'manifest.json')),
      final: readJson(path.join(inner, 'final.json')), events: readText(path.join(inner, 'events.ndjson')) };
  });
  return summarizeRun({ runId, dispatch: readJson(path.join(runDir, 'dispatch.json')), runMeta: readJson(path.join(runDir, 'run.json')), shards });
}

export function report(root) {
  const runs = fs.readdirSync(root, { withFileTypes: true }).filter(e => e.isDirectory() && /^\d+$/.test(e.name))
    .map(e => loadRun(path.join(root, e.name)));
  return aggregate(runs);
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  const args = process.argv.slice(2), root = args.find(a => !a.startsWith('--'));
  if (!root || args.includes('--help') || args.includes('-h')) {
    console.error('usage: e2e-worker-control.mjs <control-dir> [--all] [--json]');
    process.exit(root ? 0 : 2);
  }
  const agg = report(root);
  if (args.includes('--json')) console.log(JSON.stringify(agg, null, 2));
  else console.log(formatReport(agg, { all: args.includes('--all') }));
  console.error(JSON.stringify({ verdict: agg.verdict, measured: agg.measured, n_considered: agg.n_considered, sha: agg.sha,
    settings: agg.settings.map(s => ({ workers: s.workers, runs: s.runs, complete: s.complete, unmeasured: s.unmeasured })), errors: agg.errors.length }));
  process.exitCode = agg.errors.length ? 1 : 0;
}
