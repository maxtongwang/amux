// outbox-connectivity.spec coverage — use shared browser defaults and route diagnostics.
import { test, expect } from './fixtures';

// AMUX-2585. While offline the outbox hands every mutation a synthetic 202 that
// never touched the network. It is `ok`, so apiCall used to reset
// consecutiveFailures on it — the client's own queued writes perpetually
// cleared the evidence that the server was unreachable, and the counter could
// never climb back to the 2 that latches offline. The more the user did while
// offline, the more thoroughly the offline detector was disarmed.

test('a locally-queued 202 is not treated as proof of connectivity', async ({ page }) => {
  await page.goto('/');
  await page.waitForLoadState('networkidle');

  const r = await page.evaluate(() => {
    const w = window as any;
    return {
      // the synthetic response the outbox returns while offline
      synthetic: w._isLocallyQueued(w._outboxAccepted()),
      // CONTROLS — a real server response must not be mistaken for a queued one,
      // or the guard would suppress the reset on every successful call and the
      // client would latch offline while perfectly connected (the opposite bug,
      // and a worse one).
      realOk: w._isLocallyQueued(new Response('{}', { status: 200 })),
      real202: w._isLocallyQueued(new Response('{}', { status: 202 })),
      junk: w._isLocallyQueued(null),
    };
  });

  expect(r.synthetic, 'the outbox 202 must be recognised as locally queued').toBe(true);
  expect(r.realOk, 'a real 200 must NOT be treated as locally queued').toBe(false);
  expect(r.real202, 'a real 202 without the marker must NOT be treated as locally queued').toBe(false);
  expect(r.junk, 'must not throw or claim on a null response').toBe(false);
});

test('the synthetic response still reads as accepted to UI code', async ({ page }) => {
  await page.goto('/');
  await page.waitForLoadState('networkidle');
  // The outbox contract is that callers treat a queued write as ACCEPTED rather
  // than crashed. The fix must not change that — it only stops the response
  // being read as network evidence.
  const r = await page.evaluate(async () => {
    const resp = (window as any)._outboxAccepted();
    return { ok: resp.ok, status: resp.status, body: await resp.json() };
  });
  expect(r.ok).toBe(true);
  expect(r.status).toBe(202);
  expect(r.body).toMatchObject({ ok: true, queued: true, offline: true });
});
