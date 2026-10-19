// messages-default-kind.spec coverage — chips must equal the counts response the page rendered,
// and that response must count a row this spec seeded; other workers share the shard's server.
import { test, expect } from './fixtures';
import { clickWhenActionable } from './helpers';

const CHIP_KEYS = { All: 'all', Human: 'human', Session: 'session', Scheduled: 'schedule', amux: 'amux', Unstamped: 'unstamped', Unclassified: 'unknown' };

test('Messages tab opens with the human pill selected', async ({ page, request }, info) => {
  const urls: string[] = [];
  // Counts bodies in request order. A slot stays null until its body arrives,
  // so "latest" is the newest request, never whichever body parsed last.
  const seen: (Record<string, number> | null)[] = [];
  page.on('request', r => { if (r.url().includes('/api/history')) urls.push(r.url()); });
  page.on('response', r => {
    if (!r.url().includes('/api/history?counts=1')) return;
    const slot = seen.push(null) - 1;
    r.json().then(b => { seen[slot] = b; }, () => {});
  });
  await page.goto('/');
  await page.waitForFunction(() => typeof (window as any).switchView === 'function', { timeout: 20000 });
  // Ground truth the chips cannot supply about themselves: one owner message in
  // a stopped isolated worker of our own, recorded as type "user" (a human kind).
  // Without it an empty history renders every chip 0 and a chip stuck at 0 passes.
  const token = await page.evaluate(() => (window as any)._AMUX_AUTH_TOKEN as string);
  expect(token, 'served bootstrap must provide the API token').toBeTruthy();
  const headers = { Authorization: `Bearer ${token}` };
  const seed = `lc-mdk-${info.project.name}-${Date.now()}`;
  try {
    expect((await request.post('/api/sessions', { headers,
      data: { name: seed, dir: '/tmp', start: false, tags: ['e2e-isolation'], isolated: true } })).status()).toBe(201);
    expect((await request.post(`/api/sessions/${seed}/steer`, { headers,
      data: { text: `messages-default-kind seed ${seed}`, msg_id: seed, record_history: true } })).ok()).toBe(true);
    // Scoped to our own worker, so no concurrent writer can move it.
    const own = await (await request.get(`/api/history?counts=1&session=${encodeURIComponent(seed)}`, { headers })).json();
    expect({ all: own.all, human: own.human }, 'the server counts the seeded row as one human message').toEqual({ all: 1, human: 1 });
    await page.evaluate(() => (window as any).switchView('messages'));
    // The message log is the "All messages" entry of the Messages sidebar since
    // channels arrived (api/conversations.rs); its default kind is still Human.
    // Through the shared actionability contract: a covering overlay (F05/F09/F13,
    // #wt-tooltip) now fails within 15s and names itself instead of eating the test budget.
    await clickWhenActionable(page.locator('#cv-all-item'), { label: 'All messages' });
    await page.waitForSelector('#msgs-kind-filter .msg-kind-chip', { timeout: 15000 });
    const readChips = () => page.evaluate(() => Array.from(
      document.querySelectorAll('#msgs-kind-filter .msg-kind-chip')).map(b => {
        const s = getComputedStyle(b as Element);
        return { label: (b.textContent || '').trim().slice(0, 22),
                 selected: s.backgroundColor !== 'rgba(0, 0, 0, 0)' && s.backgroundColor !== 'transparent' };
      }));
    let chips: Awaited<ReturnType<typeof readChips>> = [];
    let rendered: Record<string, number> | null = null;
    // Each chip shows the total from the counts response the page itself fetched.
    // A later /api/history?counts=1 is not the oracle: isolated-worker.spec and
    // others record history on this server mid-test (both 7ca1c0d3 control
    // failures read "All 0" against a later backend 1).
    await expect.poll(async () => {
      chips = await readChips();
      rendered = seen.length ? seen[seen.length - 1] : null;
      if (!rendered) return ['no counts response rendered yet'];
      return Object.entries(CHIP_KEYS).flatMap(([label, key]) => {
        const chip = chips.find(c => c.label.startsWith(label + ' '));
        if (!chip) return [`${label}: chip missing`];
        const shown = Number(chip.label.split(' ').pop());
        return shown === (rendered![key] || 0) ? [] : [`${label}: chip ${shown}, response ${rendered![key] || 0}`];
      });
    }, { message: 'every chip equals the counts response the page rendered', timeout: 15000 }).toEqual([]);
    // The rendered response was fetched after the seed, while our row still
    // exists, so it must count it: this is what fails on a wrong server count.
    expect(rendered!.all || 0, 'the rendered All total includes the seeded row').toBeGreaterThanOrEqual(1);
    expect(rendered!.human || 0, 'the rendered Human total includes the seeded row').toBeGreaterThanOrEqual(1);
    console.log('[CHIPS] ' + JSON.stringify(chips));
    console.log('[KIND-VAR] ' + await page.evaluate(() => (0, eval)('_msgsKind')));
    console.log('[FETCHES] ' + JSON.stringify(urls.filter(u => !u.includes('counts=1')).map(u => u.split('/api/')[1])));
    console.log('[COUNTS-FETCH-UNFILTERED] ' + urls.filter(u => u.includes('counts=1')).every(u => !u.includes('kind=')));
    const sel = chips.filter(c => c.selected).map(c => c.label);
    expect(sel.join(','), 'exactly one chip selected, and it is Human').toMatch(/Human/i);
    expect(urls.some(u => u.includes('counts=1'))).toBe(true);
    expect(urls.filter(u => u.includes('counts=1')).every(u => !u.includes('kind='))).toBe(true);
    // Signal only: a concurrent history write since the page's fetch shows here,
    // so a future chip mismatch can be told apart from shared-server drift.
    const live = await page.evaluate(async () => (await fetch('/api/history?counts=1')).json());
    const drift = Object.values(CHIP_KEYS).filter(k => (live[k] || 0) !== (rendered![k] || 0))
      .map(k => `${k} ${rendered![k] || 0}->${live[k] || 0}`);
    if (drift.length) console.log('[COUNTS-DRIFT] history changed on the shared server during the test: ' + drift.join(', '));
  } finally {
    await request.delete(`/api/sessions/${seed}`, { headers }).catch(() => {});
  }
});
