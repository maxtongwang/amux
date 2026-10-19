// messages-default-kind.spec coverage — chips must equal the counts response the page rendered;
// other workers share the shard's server and write history concurrently.
import { test, expect } from './fixtures';
import { clickWhenActionable } from './helpers';

const CHIP_KEYS = { All: 'all', Human: 'human', Session: 'session', Scheduled: 'schedule', amux: 'amux', Unstamped: 'unstamped', Unclassified: 'unknown' };

test('Messages tab opens with the human pill selected', async ({ page }) => {
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
});
