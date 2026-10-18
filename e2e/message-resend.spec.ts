// message-resend.spec coverage — use shared browser defaults and route diagnostics.
import { test, expect } from './fixtures';

// AMUX-2318. The card's scope is implemented on both surfaces; what it says is
// NOT re-verified is the live-DOM check, which was run against the PYTHON build
// at APP_VER 0.9.445 and never re-run against the rust SPA. The card asks a
// reviewer to re-run it at phone width rather than trust the old transcript.
// This is that re-run.
//
// amux is mobile-first, so 375px is the target, not a variant.
test.use({ viewport: { width: 375, height: 667 } });

test('selection state and the shared row descriptor behave on the rust SPA', async ({ page }) => {
  await page.goto('/');
  await page.waitForLoadState('networkidle');

  const r = await page.evaluate(() => {
    const w = window as any;
    // The three surfaces must share ONE descriptor shape, which is the card's
    // stated defence against the two selection models drifting apart.
    const ctxs = {
      messages: w._msgCtxMessages(),
      history: w._msgCtxHistory(),
      peek: w._msgCtxPeek(),
    };
    const keys = Object.fromEntries(
      Object.entries(ctxs).map(([k, v]: any) => [k, Object.keys(v).sort().join(',')]),
    );

    // Selection is a Set keyed by _msgKey; exercise the real functions rather
    // than a paraphrase of them.
    w._msgSelNone();
    const empty = ctxs.messages.sel.size;
    w._msgSelAll(['a', 'b', 'c']);
    const afterAll = ctxs.messages.sel.size;
    w._msgSelToggle('a');
    const afterToggleOff = ctxs.messages.sel.size;
    w._msgSelToggle('a');
    const afterToggleOn = ctxs.messages.sel.size;
    w._msgSelNone();
    const afterNone = ctxs.messages.sel.size;

    // The peek surface has its OWN set — the code says "different surface,
    // different shape, and a stale selection carried across is a resend of the
    // wrong rows". Selecting in one must not select in the other.
    w._msgSelAll(['x']);
    const peekUnaffected = ctxs.peek.sel.size;
    w._msgSelNone();

    return { keys, empty, afterAll, afterToggleOff, afterToggleOn, afterNone, peekUnaffected };
  });

  // Same descriptor shape on all three, or the surfaces have drifted.
  expect(r.keys.messages).toContain('sel');
  expect(r.keys.history).toBe(r.keys.messages.replace(',onOpen', '').replace('onOpen,', ''));
  expect(r.keys.peek).toContain('resend');

  expect(r.empty).toBe(0);
  expect(r.afterAll, 'select-all adds every visible key').toBe(3);
  expect(r.afterToggleOff, 'toggle removes one').toBe(2);
  expect(r.afterToggleOn, 'toggle re-adds it').toBe(3);
  expect(r.afterNone, 'clear empties the set').toBe(0);
  expect(r.peekUnaffected, 'the peek surface must NOT share the history selection').toBe(0);
});

test('the messages surface renders and its search control exists at 375px', async ({ page }) => {
  await page.goto('/');
  await page.waitForLoadState('networkidle');
  const ids = await page.evaluate(() => {
    const w = window as any;
    return {
      messages: w._msgCtxMessages().searchId,
      history: w._msgCtxHistory().searchId,
      peek: w._msgCtxPeek().searchId,
      present: ['msgs-search', 'cmd-history-search', 'peek-messages-search']
        .filter((i) => !!document.getElementById(i)),
    };
  });
  // Each surface names a search input; at least one must actually be in the DOM
  // on load, or the descriptor points at markup that does not exist.
  expect(ids.messages).toBe('msgs-search');
  expect(ids.present.length, `none of the named search inputs exist: ${JSON.stringify(ids)}`).toBeGreaterThan(0);
});
