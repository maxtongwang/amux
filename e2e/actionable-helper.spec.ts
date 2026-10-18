// Browser contract for helpers.ts waitForActionable/clickWhenActionable on synthetic pages (page.setContent, no amux state).
// Hosted e2e failure investigation, "Proposed bounded fix" item 2; the pure half is pinned by e2e/actionable.test.mjs.
import { test, expect } from './fixtures';
import type { Page } from './fixtures';
import { ActionableTimeout, clickWhenActionable, waitForActionable } from './helpers';

// Short budgets: every failing case here is EXPECTED to exhaust its budget.
const SHORT = 1_500;

async function mount(page: Page, body: string, script = '') {
  await page.setContent(`<!doctype html><html><head><style>
    body{margin:0;font:14px sans-serif} button{width:120px;height:40px}
  </style></head><body>${body}<script>
    window.__clicks = 0; window.__clickedWhileMoving = null;
    document.addEventListener('click', e => {
      if (e.target && e.target.id === 'target') { window.__clicks++; window.__clickedWhileMoving = !!window.__moving; }
    }, true);
    ${script}
  </script></body></html>`);
}

const clicks = (page: Page) => page.evaluate(() => (window as any).__clicks as number);

async function rejection(p: Promise<unknown>): Promise<ActionableTimeout> {
  const e = await p.then(() => null, (err) => err);
  expect(e, 'the helper resolved; it was expected to reject').toBeInstanceOf(ActionableTimeout);
  return e as ActionableTimeout;
}

test('a covered target raises ActionableTimeout naming the covering element, and is never clicked', async ({ page }) => {
  await mount(page, `
    <button id="target">go</button>
    <div id="wt-overlay" class="open" style="position:fixed;inset:0">
      <div class="wt-body" style="position:absolute;inset:0;background:rgba(0,0,0,.2)"></div>
    </div>`);
  const err = await rejection(clickWhenActionable(page.locator('#target'), { budgetMs: SHORT, label: 'covered fixture' }));
  expect(err.phase).toBe('hit-test');
  expect(err.cover).toBe('div#wt-overlay.open > div.wt-body');
  expect(err.details.cover_measured).toBe(true);
  expect(err.details.budget_ms).toBe(SHORT);
  // The trial click holds the budget until it times out; diagnostics are not counted in elapsed.
  expect(err.details.elapsed_ms).toBeGreaterThanOrEqual(SHORT - 50);
  expect(err.details.selector).toContain('#target');
  expect(err.details.rect).not.toBeNull();
  expect(err.details.viewport).not.toBeNull();
  expect(err.details.project).toBe(test.info().project.name);
  expect(err.details.worker).toBe(test.info().workerIndex);
  expect(err.details.screenshot).toMatch(/^not-actionable-covered-fixture\.png$/);
  expect(test.info().attachments.map(a => a.name)).toContain('not-actionable-covered-fixture.png');
  expect(err.details.call_log?.join('\n') ?? '', 'Playwright\'s own call log is carried').toMatch(/intercepts pointer events/);
  expect(err.message).toContain('div#wt-overlay.open > div.wt-body');
  expect(await clicks(page), 'a covered target must never receive the click').toBe(0);
});

test('a moving target is waited on until its box is at rest, then clicked once', async ({ page }) => {
  await mount(page, `<button id="target" style="position:absolute;left:0;top:100px">go</button>`, `
    window.__moving = true; let f = 0;
    const el = document.getElementById('target');
    const step = () => { f++; el.style.left = (f * 4) + 'px'; if (f < 40) requestAnimationFrame(step); else window.__moving = false; };
    requestAnimationFrame(step);`);
  await clickWhenActionable(page.locator('#target'), { label: 'moving fixture' });
  expect(await clicks(page)).toBe(1);
  expect(await page.evaluate(() => (window as any).__clickedWhileMoving), 'clicked while still animating').toBe(false);
});

test('a target that is disabled at first succeeds once it becomes enabled', async ({ page }) => {
  await mount(page, `<button id="target" disabled>go</button>`, `
    let f = 0; const el = document.getElementById('target');
    const step = () => { if (++f < 30) requestAnimationFrame(step); else el.disabled = false; };
    requestAnimationFrame(step);`);
  const ready = await waitForActionable(page.locator('#target'));
  expect(ready.deadline.remaining()).toBeGreaterThan(0);
  expect(await clicks(page), 'the preflight itself never clicks').toBe(0);
  await clickWhenActionable(page.locator('#target'));
  expect(await clicks(page)).toBe(1);
});

test('a target that never enables fails at the enabled phase with no cover', async ({ page }) => {
  await mount(page, `<button id="target" disabled>go</button>`);
  const err = await rejection(waitForActionable(page.locator('#target'), { budgetMs: SHORT }));
  expect(err.phase).toBe('enabled');
  expect(err.cover).toBeNull();
});

test('no force path exists: bypass options are refused before anything runs', async ({ page }) => {
  await mount(page, `<button id="target">go</button>`);
  for (const bad of [{ force: true }, { trial: true }, { timeout: 1 }]) {
    await expect(clickWhenActionable(page.locator('#target'), bad as any)).rejects.toThrow(TypeError);
  }
  expect(await clicks(page)).toBe(0);
});

test('scroll:false turns an off-screen target into a viewport failure; the default scrolls to it', async ({ page }) => {
  await mount(page, `<div style="height:4000px"></div><button id="target">go</button>`);
  const err = await rejection(waitForActionable(page.locator('#target'), { budgetMs: SHORT, scroll: false }));
  expect(err.phase).toBe('viewport');
  expect(err.details.reason).toMatch(/scrolling is disabled/);
  expect(await page.evaluate(() => scrollY), 'scroll:false must not scroll').toBe(0);
  await clickWhenActionable(page.locator('#target'));
  expect(await clicks(page)).toBe(1);
});

test('an ambiguous locator fails at resolve instead of picking one', async ({ page }) => {
  await mount(page, `<button class="dup">a</button><button class="dup">b</button>`);
  const err = await rejection(waitForActionable(page.locator('.dup'), { budgetMs: SHORT }));
  expect(err.phase).toBe('resolve');
  expect(err.details.reason).toMatch(/^2 elements match/);
});
