// chat-fixture — a Chat conversation served through the app's own /chat endpoints for e2e specs.
// The peek poller and chat watchdog reload /chat on their own schedule; a stub answers them the same way (refs #47).
import { expect } from './fixtures';
import type { Page } from '@playwright/test';

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
  let total = 0, spec = 0, release = () => {};
  const held = new Promise<void>(r => { release = r; });
  await page.route(/\/api\/sessions\/[^/?]+\/chat(?:\?.*)?$/, r => {
    // Only the snapshot read: anything else on this path is not the conversation.
    if (r.request().method() !== 'GET') return r.fallback();
    total++;
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
    /** For the spec's finally: release the stream and say how often the app re-fetched under the test. */
    finish() {
      release();
      console.log(`[CHAT-RELOADS] spec=${spec} app=${total - spec}`);
    },
  };
}
