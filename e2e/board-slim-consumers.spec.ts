// board-slim-consumers.spec coverage — use shared browser defaults and route diagnostics.
import { test, expect } from './fixtures';

// AMUX-2840. Every board-item consumer that reads desc/log, driven against a
// REAL slim payload — not a synthetic object, because the point is whether the
// server's slim shape and the client's readers agree, and a hand-built fixture
// asserts my belief about the shape rather than the shape.
//
// Each of these fails SILENTLY when it regresses: the surface renders, it just
// renders nothing. So every assertion here is about CONTENT, never presence.

test('client readers survive the real slim payload', async ({ page, request }) => {
  await page.goto('/');
  await page.waitForLoadState('networkidle');
  const token = await page.evaluate(() => (window as any)._AMUX_AUTH_TOKEN);
  const auth = { Authorization: `Bearer ${token}`, 'Content-Type': 'application/json' };

  // Seed one card carrying every field these consumers read.
  const mk = await request.post('/api/board', {
    headers: auth,
    data: {
      title: 'slim consumer fixture',
      desc: 'PREVIEW LINE HERE\nNew task: folded one\nNEEDS-YOU: answer this',
      status: 'todo', type: 'chore',
    },
  });
  expect(mk.ok()).toBeTruthy();
  const id = (await mk.json()).id;

  const slim = await (await request.get('/api/board?archived=0&slim=1', { headers: auth })).json();
  const item = slim.find((i: any) => i.id === id);
  expect(item, 'the seeded card must be in the slim payload').toBeTruthy();

  // The shape slim actually serves — asserted, so the rest is meaningful.
  expect(item.desc, 'slim must not ship desc').toBeUndefined();
  expect(item.log, 'slim must not ship log').toBeUndefined();

  const r = await page.evaluate((it: any) => {
    const w = window as any;
    return {
      preview: (it.desc !== undefined ? it.desc : (it.desc_head || '')).split('\n')[0].slice(0, 80),
      folded: it.folded_n !== undefined ? it.folded_n > 0 : false,
      ask: w._focusAsk(it),
      descLen: it.desc_len,
    };
  }, item);

  expect(r.preview, 'card preview must not go blank under slim').toBe('PREVIEW LINE HERE');
  expect(r.folded, 'is:folded must still select this card').toBe(true);
  expect(r.ask, 'the NEEDS-YOU ask must survive').toBe('answer this');
  expect(r.descLen, 'desc_len must be served — the save guard depends on it').toBeGreaterThan(0);

  await request.delete(`/api/board/${id}`, { headers: auth });
});
