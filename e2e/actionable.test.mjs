// Unit contract for e2e/actionable.mjs, the browser-free waitForActionable/clickWhenActionable — refs report "Proposed bounded fix" item 2.
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
  clickWhenActionableWith,
  createDeadline,
  extractCallLog,
  formatElementPath,
  normalizeActionableOptions,
  raceDeadline,
  rectsStable,
  stableRun,
  waitForActionableWith,
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

// What the predicate reads: helpers.ts and actionable.mjs with // and /* */ comments removed,
// so prose about these words cannot satisfy or trip it. It fails if a forced click, a sleep,
// a click in helpers.ts, or a non-delegating clickSnapped/clickWhenActionable is written in code.
const codeOf = name => {
  const src = fs.readFileSync(new URL(name, import.meta.url), 'utf8');
  const code = src.replace(/\/\*[\s\S]*?\*\//g, '').replace(/(^|[^:'"`])\/\/.*$/gm, '$1');
  assert.ok(code.length > 200, `comment stripping left the code of ${name}`);
  return code;
};
test('helpers.ts: no forced click, no sleep, and every click helper delegates to the shared contract', () => {
  const code = codeOf('./helpers.ts');
  assert.doesNotMatch(code, /\bforce\s*:/);
  assert.doesNotMatch(code, /waitForTimeout\s*\(/);
  assert.doesNotMatch(code, /\bsetTimeout\s*\(/);
  assert.doesNotMatch(code, /\.click\(/, 'helpers.ts issues no click of its own; actionable.mjs owns the one real click');
  const body = /export async function clickSnapped\([^)]*\)[^{]*\{([\s\S]*?)\n\}/.exec(code);
  assert.ok(body, 'clickSnapped is defined');
  assert.match(body[1], /clickWhenActionable\(/);
  const cwa = /export async function clickWhenActionable\([^)]*\)[^{]*\{([\s\S]*?)\n\}/.exec(code);
  assert.ok(cwa, 'clickWhenActionable is defined');
  assert.match(cwa[1], /clickWhenActionableWith\(/);
});
test('actionable.mjs: no forced click or sleep; one real click and one trial click in code', () => {
  const code = codeOf('./actionable.mjs');
  assert.doesNotMatch(code, /\bforce\s*:/);
  assert.doesNotMatch(code, /waitForTimeout\s*\(/);
  const clicks = code.match(/\.click\(\{[^}]*\}\)/g) || [];
  assert.equal(clicks.filter(c => !/trial:\s*true/.test(c)).length, 1, clicks.join(' | '));
  assert.equal(clicks.filter(c => /trial:\s*true/.test(c)).length, 1, 'one trial (hit-test) click');
});

// ── The loop itself, driven by fake locators (PR-2 review follow-ups) ────────
// A fake is Playwright-shaped: each method resolves like the real one, and any
// step handed a `timeout` that hangs honours it the way Playwright does, by
// waiting that long and throwing a TimeoutError. So a step given more than the
// remaining budget visibly overruns the budget.
const REST = R(20, 20);
const sleep = ms => new Promise(r => setTimeout(r, ms));
const timeoutError = ms => Object.assign(new Error(`Timeout ${ms}ms exceeded.`), { name: 'TimeoutError' });

function fakeLocator(over = {}) {
  const calls = { trial: 0, real: 0, steps: [] };
  const page = { isClosed: () => false, evaluate: async () => null };
  const record = (step, opts) => calls.steps.push({ step, at: Date.now(), timeout: opts?.timeout });
  const loc = {
    calls,
    page: () => page,
    toString: () => "locator('#fake')",
    count: async () => 1,
    isVisible: async () => true,
    async isEnabled(opts) { record('enabled', opts); return over.isEnabled ? over.isEnabled(opts) : true; },
    async scrollIntoViewIfNeeded(opts) { record('scroll', opts); if (over.scroll) await over.scroll(opts); },
    async evaluate(_fn, _arg, opts) { record('stable', opts); return over.evaluate ? over.evaluate(opts) : { rects: [REST, REST, REST], viewport: { width: 400, height: 800 } }; },
    async click(opts) {
      record(opts?.trial ? 'trial' : 'click', opts);
      if (opts?.trial) { calls.trial++; return; }
      calls.real++;
      if (over.click) await over.click(opts);
    },
  };
  return loc;
}
const quiet = { failureContext: async () => ({ coverWhy: 'fake locator' }), log: () => {} };

test('clickWhenActionable: a failing real click is performed exactly once, never retried', async () => {
  const loc = fakeLocator({ click: async () => { throw new Error('Element is not attached to the DOM'); } });
  await assert.rejects(clickWhenActionableWith(loc, { budgetMs: 2_000 }, quiet),
    e => e instanceof ActionableTimeout && e.phase === 'click');
  assert.equal(loc.calls.real, 1, `real clicks issued: ${loc.calls.real}`);
  assert.equal(loc.calls.trial, 1, 'the preflight passed once and was not re-run after the click failed');
  // And the success path clicks once too.
  const ok = fakeLocator();
  await clickWhenActionableWith(ok, { budgetMs: 2_000 }, quiet);
  assert.equal(ok.calls.real, 1);
});

// Budget small enough that an uncapped 2s/3s step cap is a visible overrun.
const BUDGET = 150;
const MARGIN = 100;
for (const [phase, over] of [
  ['enabled', { isEnabled: async opts => { await sleep(opts.timeout); throw timeoutError(opts.timeout); } }],
  ['scroll', { scroll: async opts => { await sleep(opts.timeout); throw timeoutError(opts.timeout); } }],
]) {
  test(`deadline: a hung ${phase} step gets only the remaining budget, and elapsed_ms stays within it`, async () => {
    const loc = fakeLocator(over);
    const t0 = Date.now();
    let err;
    await assert.rejects(waitForActionableWith(loc, { budgetMs: BUDGET }, quiet), e => { err = e; return e instanceof ActionableTimeout; });
    assert.equal(err.phase, phase);
    assert.ok(err.details.elapsed_ms <= BUDGET + MARGIN, `elapsed_ms ${err.details.elapsed_ms} > ${BUDGET}+${MARGIN}`);
    assert.equal(err.details.budget_ms, BUDGET);
    const given = loc.calls.steps.filter(s => s.timeout !== undefined);
    assert.ok(given.some(s => s.step === phase), `the ${phase} step was reached and handed a timeout`);
    for (const s of given) {
      const remaining = BUDGET - (s.at - t0);
      assert.ok(s.timeout <= remaining + 5, `${s.step} was given ${s.timeout}ms with ~${remaining}ms of budget left`);
      assert.ok(s.timeout >= 1, `${s.step} timeout ${s.timeout} (0 means wait forever)`);
    }
  });
}

// A finished verdict survives the budget ending inside the NEXT attempt. CI
// (e2e control 7ca1c0d3, 3 of 40 runs): an off-screen target had already been
// judged "viewport", then the retry's isEnabled/evaluate got the last 1-2 ms,
// Playwright threw TimeoutError, and that replaced the verdict with
// "enabled step threw: Timeout 2ms exceeded".
const OFF = R(20, 4000);
for (const step of ['enabled', 'stable']) {
  test(`deadline: a ${step} step timing out at the deadline keeps the finished viewport verdict`, async () => {
    let attempts = 0;
    const hang = async opts => { await sleep(opts.timeout); throw timeoutError(opts.timeout); };
    const loc = fakeLocator({
      isEnabled: async opts => (step === 'enabled' && attempts > 0 ? hang(opts) : true),
      evaluate: async opts => {
        if (step === 'stable' && attempts > 0) return hang(opts);
        attempts++;
        return { rects: [OFF, OFF, OFF], viewport: { width: 400, height: 800 } };
      },
    });
    let err;
    await assert.rejects(waitForActionableWith(loc, { budgetMs: BUDGET, scroll: false }, quiet), e => { err = e; return e instanceof ActionableTimeout; });
    assert.equal(attempts, 1, 'one attempt finished before the budget ran out in the next');
    assert.equal(err.phase, 'viewport', `reported ${err.phase}: ${err.details.reason}`);
    assert.match(err.details.reason, /scrolling is disabled/);
    assert.equal(err.details.interrupted_phase, step, 'the verdict line names where the budget cut in');
    assert.equal(err.details.call_log, null, 'the deadline timeout is not reported as the step\'s own failure');
  });
}

