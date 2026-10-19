// chat-markdown-stream.spec — a STREAMED reply renders as sanitized markdown at every cut (mid-link, mid-fence)
// and when finished. Ported from e2e/chaos/chat-markdown.mjs, which no workflow ran (refs #47 review).
import { test, expect } from './fixtures';
import { stubChat, type ChatSnapshot } from './chat-fixture';

declare const _chat: any;

const REPLY = [
  '## Signup flow check\n\n',
  'Here is what I found for **someone@example.com**:\n\n',
  '- Namespace created\n- Research email: *not sent*\n- See the [playbook docs](https://amux.io/docs) for the flow\n\n',
  '| step | status |\n|---|---|\n| namespace | ok |\n| email | missing |\n\n',
  '```python\ndef send_research_email(user):\n    return queue.enqueue(user.id)\n```\n\n',
  'Screenshot of the dashboard:\n\n![amux icon](/icon-192.png)\n\n',
  'Tried <img src=x onerror="window.__xss=1"> and [bad](javascript:window.__xss=2).',
].join('');
const ASKED = { role: 'user', text: 'did the signup email go out?', ts: 1760000000 };

test('a streamed Chat reply renders as sanitized markdown at every cut and when finished', async ({ page }) => {
  test.setTimeout(90_000);
  const errors: string[] = [];
  page.on('pageerror', e => errors.push(String(e)));
  await page.route(/\/api\/sessions(?:\?.*)?$/, r => r.fulfill({ json: [{ name: 'chat-md-stream', provider: 'claude', chat_companion: true, running: false, dir: '/tmp' }] }));
  let snapshot: ChatSnapshot = { messages: [ASKED] };
  const fixture = await stubChat(page, () => snapshot);
  try {
    await page.goto('/?peekEmbed=chat-md-stream&peekTab=chat');
    await expect(page.locator('#peek-overlay')).toBeVisible();
    await expect.poll(() => page.evaluate(() => typeof (window as any).marked !== 'undefined' && typeof (window as any).remend === 'function')).toBe(true);
    await expect.poll(() => page.evaluate(() => _chat.name)).toBe('chat-md-stream@chat');
    expect(await page.evaluate(() => [...document.scripts].some(s => s.src.endsWith('/vendor/md.js'))
      && ![...document.scripts].some(s => /jsdelivr\.net\/npm\/(marked|dompurify)/.test(s.src))), 'marked, DOMPurify and remend load from amux, not a CDN').toBe(true);

    // Each cut is the stub's live turn, loaded the way the app loads it.
    const feed = async (upto: number) => { snapshot = { messages: [ASKED], streaming: { turn_id: 't1', text: REPLY.slice(0, upto) } }; await fixture.load(); };
    const bubble = () => page.evaluate(() => {
      const b = [...document.querySelectorAll('.chat-msg.is-streaming .chat-bubble, .chat-msg.chat-assistant .chat-bubble')].pop() as HTMLElement;
      const body = document.getElementById('peek-body')!;
      return { text: b.innerText, html: b.innerHTML, scrollW: body.scrollWidth, clientW: body.clientWidth };
    });

    await feed(REPLY.indexOf('https://amux.io/docs') + 12);   // inside "(https://amux.io/do"
    let b = await bubble();
    expect(b.text, 'mid-link: no raw ]( markup').not.toContain('](');
    expect(b.text, 'mid-link: no raw [label').not.toContain('[playbook');
    expect(b.html, 'mid-link: no clickable half link').not.toMatch(/href="https:\/\/amux\.io\/do"/);

    const midFence = REPLY.indexOf('return queue');
    await feed(midFence);
    b = await bubble();
    expect(b.text, 'mid-fence: no raw ```').not.toContain('```');
    expect(b.html, 'mid-fence: code already renders as code').toMatch(/<pre><code/);
    expect(b.scrollW, 'mid-fence: no horizontal overflow').toBeLessThanOrEqual(b.clientW + 1);

    for (let i = midFence; i < REPLY.length; i += 37) await feed(i);
    await feed(REPLY.length);
    b = await bubble();
    expect(b.html).toMatch(/<h2/);
    expect(b.html).toMatch(/<li>/);
    expect(b.html).toMatch(/<table/);
    expect(b.html).toMatch(/<pre><code/);
    expect(b.html, 'a link opens in a new tab with rel=noopener').toMatch(/<a href="https:\/\/amux\.io\/docs" target="_blank" rel="noopener"/);
    await expect.poll(() => page.evaluate(() => {
      const i = [...document.querySelectorAll('.chat-bubble img')].find(x => (x as HTMLImageElement).src.endsWith('/icon-192.png')) as HTMLImageElement | undefined;
      if (!i) return 'no image';
      const r = i.getBoundingClientRect(), br = i.closest('.chat-bubble')!.getBoundingClientRect();
      return i.complete && i.naturalWidth > 0 && r.width <= br.width + 1 ? 'fits' : `loaded=${i.complete && i.naturalWidth > 0} w=${r.width} bubble=${br.width}`;
    }), { message: 'the image renders inline and fits the bubble' }).toBe('fits');
    expect(b.html, 'onerror stripped').not.toMatch(/onerror/i);
    expect(b.html, 'javascript: link neutered').not.toMatch(/javascript:/i);
    expect(await page.evaluate(() => (window as any).__xss), 'nothing executed').toBeUndefined();

    // The finished message path renders the same markdown.
    snapshot = { messages: [ASKED, { role: 'assistant', text: REPLY, ts: 1760000001 }] };
    await fixture.load();
    b = await bubble();
    expect(b.html).toMatch(/<table/);
    expect(b.html).toMatch(/amux\.io\/docs/);
    expect(errors, 'no uncaught page errors').toEqual([]);
  } finally {
    fixture.finish();
  }
});
