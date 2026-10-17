// View freshness — terminal and fleet timestamps must measure applied data, not transport liveness.
import { test, expect, allowUnusedRoute, type Page } from './fixtures';

const NAME = 'zz-view-freshness';
const worker = { name: NAME, running: true, status: 'idle', lifecycle: 'active',
  provider: 'claude', dir: '/tmp', tags: [] };

async function setup(page: Page) {
  const state = { hangPeek: false, peek304: false, hangSessions: false, sessionReads: 0, pendingSessions: [] as (() => Promise<void>)[], beacons: [] as any[] };
  await page.route('**/api/events**', r => r.abort());
  allowUnusedRoute(page, '**/api/events**'); // Embed/offline startup can skip SSE.
  await page.route(/\/api\/sessions(?:\?.*)?$/, async r => {
    state.sessionReads++;
    if (state.hangSessions) {
      state.pendingSessions.push(() => r.fulfill({ json: [worker] }));
      return; // Accepted request with no response until recovery.
    }
    await r.fulfill({ json: [worker] });
  });
  await page.route(`**/api/sessions/${NAME}/**`, r => r.fulfill({ json: {} }));
  allowUnusedRoute(page, `**/api/sessions/${NAME}/**`); // Optional companion/plan reads.
  await page.route(`**/api/sessions/${NAME}/peek**`, async r => {
    if (state.hangPeek) return;
    if (state.peek304) { await r.fulfill({ status: 304 }); return; }
    await r.fulfill({ headers: { ETag: '"freshness-frame"' }, json: { name: NAME, output: 'Terminal freshness specimen',
      live: 'Terminal freshness specimen', history: '' } });
  });
  await page.route('**/api/client-debug', async r => {
    state.beacons.push(r.request().postDataJSON());
    await r.fulfill({ json: { ok: true } });
  });
  await page.addInitScript(() => localStorage.setItem('amux_walkthrough_done', '1'));
  await page.goto('/');
  await page.waitForFunction(name => typeof (window as any).openPeek === 'function'
    && (globalThis as any).eval('sessions').some((s: any) => s.name === name), NAME);
  await page.evaluate(name => (window as any).openPeek(name), NAME);
  await expect(page.locator('#peek-body')).toContainText('Terminal freshness specimen');
  await expect(page.locator('#peek-status')).toContainText('Updated');
  return state;
}

for (const recovery of ['unchanged', '304']) {
  test(`hung terminal reads show measured staleness and recover on ${recovery}`, async ({ page }) => {
    test.setTimeout(60000);
    const state = await setup(page);
    state.hangPeek = true;
    const status = page.locator('#peek-status');
    await expect(status).toContainText('Stale since', { timeout: 25000 });
    await expect(status).toHaveText(/Stale since .+ \(\d+s\)/);
    await expect.poll(() => state.beacons.filter(b => b.verdict === 'peek-stale').length).toBe(1);
    expect(state.beacons.find(b => b.verdict === 'peek-stale')).toMatchObject({ measured: true, age_ms: expect.any(Number) });
    state.peek304 = recovery === '304';
    state.hangPeek = false;
    await expect(status).toContainText('Updated', { timeout: 15000 });
    await expect(status).not.toContainText('Stale since');
  });
}

test('selected terminal text explains the pause and clears after selection is released', async ({ page }) => {
  test.setTimeout(60000);
  const state = await setup(page);
  await page.locator('#peek-body').evaluate(el => {
    const range = document.createRange();
    range.selectNodeContents(el);
    const selection = window.getSelection()!;
    selection.removeAllRanges();
    selection.addRange(range);
  });
  const status = page.locator('#peek-status');
  await expect(status).toContainText('Paused while text is selected · since', { timeout: 25000 });
  await expect.poll(() => state.beacons.filter(b => b.verdict === 'selection-paused').length).toBe(1);
  expect(state.beacons.find(b => b.verdict === 'selection-paused')).toMatchObject({ measured: true, age_ms: expect.any(Number) });
  await page.evaluate(() => window.getSelection()!.removeAllRanges());
  await expect(status).toContainText('Updated', { timeout: 15000 });
});

test('transport contact cannot hide stale fleet data and a fresh snapshot clears the banner', async ({ page }) => {
  test.setTimeout(70000);
  const state = await setup(page);
  state.hangSessions = true;
  // Model continuing SSE pings without sessions payloads. Keep the transport
  // clock live while the real watchdog measures the independently aging data.
  await page.evaluate(() => {
    setInterval(() => (globalThis as any).eval('_lastDataTime = Date.now()'), 1000);
  });
  const fleet = page.locator('.cards.workers-stale');
  await expect(fleet).toContainText('Stale since', { timeout: 45000 });
  const reads = state.sessionReads;
  await page.evaluate(() => (globalThis as any).eval('_checkViewFreshness(Date.now())'));
  expect(state.sessionReads).toBe(reads);
  // Complete the actual fetch on this page; recovery must use production wiring.
  state.hangSessions = false;
  await Promise.all(state.pendingSessions.splice(0).map(release => release()));
  await expect(page.locator('.cards')).not.toHaveClass(/workers-stale/);
  await expect(page.locator('.cards')).not.toContainText('Stale since');
});
