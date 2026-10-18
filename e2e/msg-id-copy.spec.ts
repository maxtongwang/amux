// msg-id-copy.spec coverage — use shared browser defaults and route diagnostics.
import { test, expect } from './fixtures';
import { grantClipboard } from './helpers';
test('copying a message id puts the MSG- prefix on the clipboard', async ({ page, context, browserName }) => {
  // WebKit has no such permission and asking is a hard error — see grantClipboard.
  await grantClipboard(context, browserName);
  await page.goto('/');
  await page.waitForFunction(() => typeof (window as any)._copyMsgId === 'function', { timeout: 20000 });
  // Drive the real function, capturing what it writes — the clipboard is the
  // artifact under test, not the toast.
  const r = await page.evaluate(async () => {
    let wrote: string | null = null;
    const orig = navigator.clipboard.writeText.bind(navigator.clipboard);
    (navigator.clipboard as any).writeText = (t: string) => { wrote = t; return orig(t); };
    let toast: string | null = null;
    const st = (window as any).showToast;
    (window as any).showToast = (m: string) => { toast = m; };
    (window as any)._copyMsgId('28003');          // as the badge passes it
    const a = { wrote, toast };
    (window as any)._copyMsgId('MSG-28003');      // already-prefixed: must not double
    const b = { wrote, toast };
    (window as any).showToast = st;
    return { a, b };
  });
  console.log('[BARE ]' + JSON.stringify(r.a));
  console.log('[PREFIXED]' + JSON.stringify(r.b));
  expect(r.a.wrote, 'bare id must gain the prefix').toBe('MSG-28003');
  expect(r.b.wrote, 'already-prefixed id must not double up').toBe('MSG-28003');
  expect(r.a.toast, 'toast must match what was copied').toBe('MSG-28003 copied');
});
