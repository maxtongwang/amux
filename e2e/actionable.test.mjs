// Unit contract for e2e/actionable.mjs, the browser-free half of waitForActionable — refs report "Proposed bounded fix" item 2.
// Runs in spa-gates (`node --test e2e/*.test.mjs`); the browser half is e2e/actionable-helper.spec.ts.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import {
  ACTIONABLE_DEFAULT_BUDGET_MS,
  ActionableTimeout,
  DeadlineExceeded,
  buildNotActionable,
  centreInViewport,
  createDeadline,
  extractCallLog,
  formatElementPath,
  normalizeActionableOptions,
  raceDeadline,
  rectsStable,
  stableRun,
} from './actionable.mjs';

const R = (x, y, width = 40, height = 20) => ({ x, y, width, height });
const fakeClock = (start = 1_000) => {
  let t = start;
  return { now: () => t, advance: ms => { t += ms; } };
};

test('rect stability: identical frames are stable, any real move is not', () => {
  assert.equal(rectsStable(R(10, 10), R(10, 10)), true);
  assert.equal(rectsStable(R(10, 10), R(10.2, 10)), true, 'sub-pixel jitter under the tolerance is rest');
  assert.equal(rectsStable(R(10, 10), R(11, 10)), false, 'a 1px horizontal move is motion');
  assert.equal(rectsStable(R(10, 10), R(10, 10, 41)), false, 'a size change is motion');
  assert.equal(stableRun([R(0, 0), R(0, 0), R(0, 0)]), true);
  assert.equal(stableRun([R(0, 0), R(0, 0), R(5, 0)]), false, 'the LAST frame moving still fails the run');
  assert.equal(stableRun([R(0, 0)]), false, 'one sample cannot show rest');
});

test('rect stability: malformed samples fail instead of passing silently', () => {
  assert.equal(rectsStable(R(0, 0), { x: NaN, y: 0, width: 1, height: 1 }), false);
  assert.equal(rectsStable(R(0, 0), null), false);
  assert.equal(stableRun(undefined), false);
  assert.equal(stableRun([R(0, 0), {}]), false);
});

test('viewport intersection is judged at the click point, the centre', () => {
  const vp = { width: 393, height: 852 };
  assert.equal(centreInViewport(R(10, 10), vp), true);
  // AF-45's measured iPhone 15 geometry: the button sat at left=983 of a 393px viewport.
  assert.equal(centreInViewport(R(983, 10), vp), false);
  assert.equal(centreInViewport(R(380, 10, 40), vp), false, 'centre at 400 is past the right edge');
  assert.equal(centreInViewport(R(10, -30, 40, 20), vp), false, 'above the fold');
  assert.equal(centreInViewport(R(10, 10, 0, 0), vp), false, 'a zero-size box has no click point');
  assert.equal(centreInViewport(R(10, 10), null), false);
});

test('covering element path names the interceptor the report had to dig out of call logs', () => {
  assert.equal(
    formatElementPath({ chain: [
      { tag: 'div', id: 'wt-overlay', classes: ['open'] },
      { tag: 'div', id: 'wt-backdrop', classes: [] },
    ], truncated: false }),
    'div#wt-overlay.open > div#wt-backdrop',
  );
  assert.equal(
    formatElementPath({ chain: [{ tag: 'div', id: '', classes: ['wt-body', 'x'] }], truncated: true }),
    '… > div.wt-body.x',
  );
  assert.equal(formatElementPath({ chain: [], truncated: false }), null);
  assert.equal(formatElementPath(null), null);
});

test('deadline: one budget, elapsed + remaining account for every millisecond', () => {
  const c = fakeClock();
  const d = createDeadline(15_000, c.now);
  assert.equal(d.budgetMs, 15_000);
  assert.equal(d.elapsed(), 0);
  c.advance(4_000);
  assert.equal(d.elapsed(), 4_000);
  assert.equal(d.remaining(), 11_000);
  assert.equal(d.timeout(3_000), 3_000, 'a step cap below the remaining budget wins');
  assert.equal(d.timeout(), 11_000);
  assert.equal(d.expired(), false);
  c.advance(11_000);
  assert.equal(d.expired(), true);
  assert.equal(d.remaining(), 0);
});

test('deadline: a step timeout is never 0, negative or infinite (Playwright reads 0 as no timeout)', () => {
  const c = fakeClock();
  const d = createDeadline(100, c.now);
  c.advance(500);
  for (const cap of [undefined, 0, -5, 3_000, Infinity]) {
    const t = d.timeout(cap);
    assert.ok(Number.isFinite(t) && t >= 1, `timeout(${cap}) = ${t}`);
  }
});

test('raceDeadline: settles with the work, or rejects with DeadlineExceeded at the budget', async () => {
  const d = createDeadline(5_000);
  assert.equal(await raceDeadline(Promise.resolve(7), d, 'fast'), 7);
  const short = createDeadline(20);
  const never = new Promise(() => {});
  await assert.rejects(raceDeadline(never, short, 'frame'), e => e instanceof DeadlineExceeded && /frame/.test(e.message));
  const c = fakeClock();
  const gone = createDeadline(10, c.now);
  c.advance(10);
  let called = false;
  await assert.rejects(raceDeadline((called = true, never), gone, 'late'), DeadlineExceeded);
  assert.equal(called, true, 'the argument is evaluated by the caller; the race still refuses an expired budget');
  // A loser that rejects after the race must not surface as an unhandled rejection.
  const late = new Promise((_, rej) => setTimeout(() => rej(new Error('late')), 30));
  await assert.rejects(raceDeadline(late, createDeadline(5), 'loser'), DeadlineExceeded);
  await new Promise(r => setTimeout(r, 40));
});

test('options: defaults, and every way to bypass actionability is refused at the boundary', () => {
  assert.deepEqual(normalizeActionableOptions(undefined),
    { budgetMs: ACTIONABLE_DEFAULT_BUDGET_MS, scroll: true, label: null, stableFrames: 3 });
  assert.equal(ACTIONABLE_DEFAULT_BUDGET_MS, 15_000);
  assert.deepEqual(normalizeActionableOptions({ budgetMs: 2_000, scroll: false, label: 'x' }),
    { budgetMs: 2_000, scroll: false, label: 'x', stableFrames: 3 });
  for (const bad of [{ force: true }, { trial: false }, { timeout: 5 }, { noWaitAfter: true }, { position: { x: 1, y: 1 } }]) {
    assert.throws(() => normalizeActionableOptions(bad), TypeError, JSON.stringify(bad));
  }
  assert.throws(() => normalizeActionableOptions({ force: true }), /force/);
  for (const budgetMs of [0, -1, NaN, Infinity, '15000']) {
    assert.throws(() => normalizeActionableOptions({ budgetMs }), TypeError, String(budgetMs));
  }
  assert.throws(() => normalizeActionableOptions({ scroll: 'no' }), TypeError);
  assert.throws(() => normalizeActionableOptions({ stableFrames: 1 }), TypeError, 'one frame cannot show rest');
});

test('call log: Playwright\'s action log is extracted from the last error, ANSI stripped', () => {
  const msg = 'locator.click: Timeout 3000ms exceeded.\nCall log:\n\u001b[2m  - waiting for locator(\'#cv-all-item\')\u001b[22m\n  - <div class="wt-body"> from <div id="wt-tooltip"> subtree intercepts pointer events\n';
  assert.deepEqual(extractCallLog(msg), [
    "- waiting for locator('#cv-all-item')",
    '- <div class="wt-body"> from <div id="wt-tooltip"> subtree intercepts pointer events',
  ]);
  assert.equal(extractCallLog('no log here'), null);
  assert.equal(extractCallLog(undefined), null);
});

const FIELDS = ['project', 'shard', 'worker', 'elapsed_ms', 'budget_ms', 'selector', 'label', 'phase',
  'reason', 'rect', 'viewport', 'cover', 'cover_measured', 'screenshot', 'call_log'];

test('failure: ActionableTimeout carries every diagnostic field and the verdict line is measured', () => {
  const { error, line } = buildNotActionable({
    project: 'ios-safari', shard: '4/16', worker: 1, elapsedMs: 15_002, budgetMs: 15_000,
    selector: "locator('#cv-all-item')", label: 'All messages', phase: 'hit-test', reason: 'trial click timed out',
    rect: R(10, 200), viewport: { width: 393, height: 852 },
    cover: { chain: [{ tag: 'div', id: 'wt-tooltip', classes: [] }, { tag: 'div', id: '', classes: ['wt-body'] }], truncated: false },
    coverWhy: null, screenshot: 'not-actionable-All-messages.png', callLog: ['- a', '- b'],
  });
  assert.ok(error instanceof ActionableTimeout);
  assert.ok(error instanceof Error);
  assert.equal(error.name, 'ActionableTimeout');
  for (const k of FIELDS) assert.ok(k in error.details, `error.details.${k}`);
  assert.equal(error.details.cover, 'div#wt-tooltip > div.wt-body');
  assert.equal(error.phase, 'hit-test');
  assert.equal(error.cover, 'div#wt-tooltip > div.wt-body');
  assert.match(error.message, /div#wt-tooltip > div\.wt-body/);
  assert.match(error.message, /ios-safari/);
  assert.match(error.message, /15002\/15000ms/);
  assert.match(error.message, /HARNESS/);
  const parsed = JSON.parse(line);
  assert.equal(parsed.verdict, 'e2e_not_actionable');
  assert.equal(parsed.measured, true);
  assert.equal(parsed.n_considered, 1);
  for (const k of FIELDS) assert.ok(k in parsed, `verdict line ${k}`);
  assert.equal(line.includes('\n'), false, 'one line');
});

test('failure: an unmeasured cover says so instead of reading as "not covered"', () => {
  const { error, line } = buildNotActionable({
    project: null, shard: null, worker: null, elapsedMs: 3, budgetMs: 15_000, selector: 'x', label: null,
    phase: 'resolve', reason: '0 elements match', rect: null, viewport: null, cover: null,
    coverWhy: 'no element to probe', screenshot: null, callLog: null,
  });
  assert.equal(error.details.cover, null);
  assert.equal(error.details.cover_measured, false);
  assert.equal(error.details.cover_why, 'no element to probe');
  assert.match(error.message, /cover unmeasured \(no element to probe\)/);
  assert.equal(JSON.parse(line).cover_measured, false);
  const clear = buildNotActionable({
    project: null, shard: null, worker: null, elapsedMs: 3, budgetMs: 15_000, selector: 'x', label: null,
    phase: 'stable', reason: 'moving', rect: R(0, 0), viewport: { width: 9, height: 9 }, cover: null,
    coverWhy: null, screenshot: null, callLog: null,
  });
  assert.equal(clear.error.details.cover_measured, true);
  assert.match(clear.error.message, /not covered/);
});

// What the predicate reads: helpers.ts with // and /* */ comments removed, so prose about
// these words cannot satisfy or trip it. It fails if a forced click, a sleep, or a
// non-delegating clickSnapped is written in code.
test('helpers.ts: no forced click, no sleep, and clickSnapped delegates to the shared contract', () => {
  const src = fs.readFileSync(new URL('./helpers.ts', import.meta.url), 'utf8');
  const code = src.replace(/\/\*[\s\S]*?\*\//g, '').replace(/(^|[^:'"`])\/\/.*$/gm, '$1');
  assert.ok(code.length > 200, 'comment stripping left the code');
  assert.doesNotMatch(code, /\bforce\s*:/);
  assert.doesNotMatch(code, /waitForTimeout\s*\(/);
  assert.doesNotMatch(code, /\bsetTimeout\s*\(/);
  const body = /export async function clickSnapped\([^)]*\)[^{]*\{([\s\S]*?)\n\}/.exec(code);
  assert.ok(body, 'clickSnapped is defined');
  assert.match(body[1], /clickWhenActionable\(/);
  assert.doesNotMatch(body[1], /\.click\(/, 'clickSnapped performs no click of its own');
  // The real click is issued once: exactly one non-trial .click( call in the file.
  const clicks = code.match(/\.click\(\{[^}]*\}\)/g) || [];
  assert.equal(clicks.filter(c => !/trial:\s*true/.test(c)).length, 1, clicks.join(' | '));
  assert.equal(clicks.filter(c => /trial:\s*true/.test(c)).length, 1, 'one trial (hit-test) click');
});
