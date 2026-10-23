// chat-fixture — a Chat conversation served through the app's own /chat endpoints for e2e specs.
// The peek poller and chat watchdog reload /chat on their own schedule; a stub answers them the same way (refs #47).
import { expect } from './fixtures';
import type { Page, Request } from '@playwright/test';

declare const _chat: any;

/** The load-failure text _chatLoad shows when /chat cannot be read: in .chat-empty
 * with nothing saved, or in .chat-offline-note over the conversation it repainted
 * from IndexedDB, which is what a failed RELOAD after a good load looks like. */
const LOAD_FAILURE = /Could not (load the chat|reach the server)/;

export type ChatSnapshot = { messages: any[]; streaming?: { turn_id: string; text: string } | null };

/**
 * Stubs /api/sessions/<name>/chat with `snapshot()` and holds /chat/stream open
 * and silent (the chat counts as connected). `load()` is the spec's own load
 * through the app's _chatLoad; every other /chat request is the app reloading.
 */
export async function stubChat(page: Page, snapshot: () => ChatSnapshot) {
  let total = 0, spec = 0, observed = 0, release = () => {};
  // A later route may answer a load before this fixture's handler sees it.
  const countLoad = (request: Request) => {
    observed++;
    if (request.method() === 'GET' && /^\/api\/sessions\/[^/]+\/chat$/.test(new URL(request.url()).pathname)) total++;
  };
  page.on('request', countLoad);
  const held = new Promise<void>(r => { release = r; });
  await page.route(/\/api\/sessions\/[^/?]+\/chat(?:\?.*)?$/, r => {
    // Only the snapshot read: anything else on this path is not the conversation.
    if (r.request().method() !== 'GET') return r.fallback();
    return r.fulfill({ json: { cursor: 'e2e:1', busy: false, queued: 0, streaming: null, ...snapshot() } });
  });
  await page.route(/\/api\/sessions\/[^/?]+\/chat\/stream/, async r => { await held; await r.abort().catch(() => {}); });
  const noLoadFailure = async () => {
    await expect(page.locator('#peek-body .chat-empty, #peek-body .chat-offline-note', { hasText: LOAD_FAILURE }),
      'the chat never fell back to a load-failure banner').toHaveCount(0);
    expect(await page.evaluate(() => _chat.offlineNote ?? ''), 'no offline note: the last load was served').toBe('');
  };
  return {
    noLoadFailure,
    /** Load the current snapshot the way the app does, then check it did not fail. */
    async load() {
      spec++;
      await page.evaluate(() => (window as any)._chatLoad(_chat.name));
      await noLoadFailure();
    },
    /** Release the stream and report every chat GET, whichever route answered it. */
    finish() {
      release();
      page.off('request', countLoad);
      const counts = { spec, app: total - spec };
      console.log(`[CHAT-RELOADS] spec=${counts.spec} app=${counts.app} observed=${observed} measured=${observed > 0}`);
      return counts;
    },
  };
}
