// Browser Trail tab: attributed, redacted history and measured-empty, each test on its own browser session.
// main 309f74a3 ios-safari: the real routed inspect verb refused (no route receipt) and its error filled the list.
import { test, expect, Page } from './fixtures';
import { clickWhenActionable } from './helpers';

// Opens the inspector on a session no other test uses, with every route this
// panel reaches stubbed. Opening the inspector POSTs the real routed `inspect`
// verb, which the route driver refuses with 409 unless the session holds a
// route receipt, so the answer used to depend on what another test left in
// ~/.amux/browser-routing. Here it answers an empty inspect for this session
// only, and nothing reaches the shared routing state.
async function openTrail(page: Page, history: object) {
  const session = `trail-${test.info().project.name}-${test.info().testId}`;
  const inspectSessions: string[] = [];
  const historySessions: string[] = [];
  await page.route('**/api/browser/profiles', route => route.fulfill({
    contentType: 'application/json',
    body: JSON.stringify({ profiles: [], chrome_profiles: [], backends: ['native'] }),
  }));
  await page.route('**/api/browser/routing/request', route => {
    const payload = route.request().postDataJSON();
    if (payload?.verb !== 'inspect') return route.continue();
    inspectSessions.push(payload.session);
    return route.fulfill({
      contentType: 'application/json',
      body: JSON.stringify({ console: [], network: [], errors: [], counts: {} }),
    });
  });
  await page.route('**/api/browser/history?**', route => {
    historySessions.push(new URL(route.request().url()).searchParams.get('session') || '');
    return route.fulfill({ contentType: 'application/json', body: JSON.stringify({ session, ...history }) });
  });

  await page.goto('/');
  await page.waitForFunction(() => typeof (window as any)._bwInspTab === 'function');
  // `_bwSession` is a top-level `let`: a global lexical binding, not a window property.
  await page.evaluate(s => (0, eval)(`_bwSession = ${JSON.stringify(s)}`), session);
  await page.evaluate(() => (window as any).switchView('browser'));
  await clickWhenActionable(page.locator('#bw-inspect-btn'), { label: 'Inspect' });
  await expect.poll(() => inspectSessions, { message: 'the inspect verb was answered by the stub' }).toEqual([session]);
  const trail = page.locator('button[data-itab="trail"]');
  await clickWhenActionable(trail, { label: 'Trail tab' });
  // The tab switch is the receipt that the tap landed; only then is the list the Trail's.
  await expect(trail).toHaveClass(/active/);
  await expect.poll(() => historySessions, { message: 'the Trail read this test\'s session' }).toContain(session);
  return page.locator('#bw-inspect-list');
}

test('Browser Trail renders attributed actions without sensitive inputs', async ({ page }) => {
  const panel = await openTrail(page, {
    measured: true,
    n_considered: 2,
    returned: 2,
    truncated: false,
    sensitive_values_recorded: false,
    events: [
      {
        at: '2026-09-06T07:12:03Z',
        event: 'browser.action',
        data: {
          action: 'type',
          typed_chars: 18,
          url: 'https://example.com/login',
          http_status: 200,
          sensitive_values_recorded: false,
        },
      },
      {
        at: '2026-09-06T07:11:54Z',
        event: 'browser.navigated',
        data: { url: 'https://example.com/login', ready_state: 'complete' },
      },
    ],
  });

  await expect(panel).toContainText('type');
  await expect(panel).toContainText('18 chars (contents withheld)');
  await expect(panel).toContainText('https://example.com/login');
  await expect(page.locator('#bw-ic-trail')).toHaveText('(2)');
  await expect(panel).not.toContainText('hunter2');
  console.log('[browser-trail] measured=true returned=2/2 sensitive_values_recorded=false');
});

test('Browser Trail distinguishes measured-empty history', async ({ page }) => {
  const panel = await openTrail(page, {
    measured: true, n_considered: 0, returned: 0,
    truncated: false, sensitive_values_recorded: false, events: [],
  });

  await expect(panel).toContainText('No recorded browser actions for this worker yet.');
  console.log('[browser-trail-empty] measured=true n_considered=0');
});
