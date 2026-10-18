// Phase 0 golden scenarios (RR-0025), updated for the REAL extracted
// dashboard (Phase 8): server boots, the actual SPA loads and parses, health
// is truthful, auth rejects bad tokens, mobile renders without overflow.
import { test, expect } from './fixtures';

test('health returns 200 with build hash and revision', async ({ request }) => {
  const res = await request.get('/health');
  expect(res.status()).toBe(200);
  const body = await res.json();
  expect(body.status).toBe('ok');
  expect(body.server).toBe('amux-rust');
  expect(typeof body.build).toBe('string');
  expect(body.build.length).toBeGreaterThan(0);
  expect(typeof body.rev).toBe('number');
});

test('real dashboard loads: shell, views, no parse errors', async ({ page }) => {
  const pageErrors: string[] = [];
  // pageerror = uncaught exceptions (a parse error in the 1.4MB app.js lands
  // here). Console errors from fetches against not-yet-implemented endpoints
  // are EXPECTED during the strangler-fig phase and asserted separately.
  page.on('pageerror', (err) => pageErrors.push(String(err)));
  await page.goto('/');
  await expect(page).toHaveTitle('amux');
  // Structural markers of the real SPA — the board and session views exist
  // in the DOM (visibility depends on the active tab).
  await expect(page.locator('#board-view')).toBeAttached();
  await expect(page.locator('#session-view')).toBeAttached();
  await expect(page.locator('#conn-status')).toBeAttached();
  expect(pageErrors).toEqual([]);
});

test('static assets serve with correct types', async ({ request }) => {
  for (const [path, type] of [
    ['/app.js', 'text/javascript'],
    ['/app.css', 'text/css'],
    ['/sw.js', 'text/javascript'],
    ['/manifest.json', 'application/'],
  ] as const) {
    const res = await request.get(path);
    expect(res.status(), path).toBe(200);
    expect(res.headers()['content-type'], path).toContain(type);
  }
});

test('viewport renders without horizontal overflow', async ({ page }) => {
  // Runs in both projects; the mobile project (375px) is the load-bearing
  // case — amux is mobile-first.
  await page.goto('/');
  await page.waitForLoadState('networkidle').catch(() => {});
  const overflow = await page.evaluate(
    () => document.documentElement.scrollWidth > document.documentElement.clientWidth + 1,
  );
  expect(overflow).toBe(false);
});

test('protected API rejects a bad bearer token', async ({ request }) => {
  const bad = await request.get('/api/sync?since_rev=0', {
    headers: { Authorization: 'Bearer wrong-token' },
  });
  expect(bad.status()).toBe(401);
});

test('protected API rejects a missing token', async ({ request }) => {
  const res = await request.get('/api/sync?since_rev=0');
  expect(res.status()).toBe(401);
});
