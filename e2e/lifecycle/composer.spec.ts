// Composer delivery coverage — keep mocked outcomes provider-free (refs ai-config #339).
import { test, expect, allowUnusedRoute } from '../fixtures';
import { boot, auth, checkpoint, deleteOwnedWorkers, createMockWorker } from './evidence';

for (const outcome of ['refused', 'accepted', 'queued', 'unconfirmed'] as const) {
  test(`LC-COMPOSER: ${outcome} delivery clears only after local persistence, before server response`, async ({ page, request }, info) => {
    test.setTimeout(60_000);
    await boot(page);
    const headers = await auth(page);
    const name = `lc-compose-${info.project.name}-${Date.now()}`;
    await createMockWorker(request, headers, name);
    let release!: () => void;
    const pending = new Promise<void>(resolve => { release = resolve; });
    const payloads: any[] = [];
    let receiptReads = 0;
    await page.route(`**/api/sessions/${name}/send**`, async route => {
      if (route.request().method() === 'GET') {
        receiptReads++;
        await route.fulfill({ status: 202, json: { accepted: false,
          msg_id: new URL(route.request().url()).searchParams.get('msg_id') } });
        return;
      }
      payloads.push(route.request().postDataJSON());
      await pending;
      await route.fulfill({ status: outcome === 'refused' ? 422 : outcome === 'queued' ? 503 : 200,
        json: ['accepted', 'unconfirmed'].includes(outcome) ? { ok: true, submitted: outcome === 'accepted' } : { error: 'lifecycle controlled refusal' } });
    });
    const entries = () => page.evaluate(name => JSON.parse(localStorage.getItem('amux_offline_queue') || '[]').filter((q: any) => q.url.endsWith(`/${name}/send`)), name);
    try {
      // The SPA is usable at DOMContentLoaded; WebKit can keep its load event
      // pending on background requests. Explicitly returning to Workers also
      // prevents saved-peek restoration from racing the menu click.
      await page.goto('/#view=sessions', {waitUntil:'domcontentloaded'});
      const card = page.locator(`.card[data-session="${name}"]`).locator('visible=true').first();
      await card.locator('.card-menu-btn').click();
      await page.locator('.card-menu.open [data-worker-action="peek-terminal"]').click();
      await page.locator('#peek-composer-more-btn').click();
      const chooser = page.waitForEvent('filechooser');
      await page.locator('#peek-more-menu').getByRole('button', { name: 'Attach file', exact: false }).click();
      await (await chooser).setFiles({ name: 'retained-draft.txt', mimeType: 'text/plain', buffer: Buffer.from('Keep this upload with its queued message.') });
      await expect(page.locator('#peek-attach-bar .peek-attach-chip')).toHaveCount(1);
      await expect(page.locator('#peek-attach-bar .uploading')).toHaveCount(0);
      await expect(page.locator('#peek-attach-bar .failed')).toHaveCount(0);
      const input = page.locator('#peek-cmd-input');
      const send = page.locator('#peek-overlay .send-split-main');
      const message = `Preserve this ${outcome} message ${name}`;
      await input.fill(message);
      await page.evaluate(() => {
        const w = window as any, show = w.showToast; w.__composerToasts = [];
        w.showToast = (text: string) => { w.__composerToasts.push(text); return show(text); };
      });
      await send.click();
      await expect(input, 'durable local acceptance clears before the server answers').toHaveValue('');
      await expect(send).toBeEnabled();
      await expect(send).toHaveText('Send');
      await expect(page.locator('#conn-status').first()).not.toHaveText(/pending/);
      await expect(page.locator('#sync-banner')).not.toHaveClass(/active/);
      await expect(page.locator('#peek-pending-pill')).not.toBeVisible();
      await expect(page.locator('#peek-attach-bar .peek-attach-chip')).toHaveCount(0);
      const saved = await entries();
      expect(saved).toHaveLength(1);
      const intent = JSON.parse(saved[0].options.body);
      expect(intent.text).toContain(message);
      expect(intent.text).toContain('@');
      expect(intent.msg_id).toBeTruthy();
      await expect.poll(() => payloads.length).toBe(1);
      expect(payloads[0]).toEqual(intent);
      await input.fill('A newer draft must survive the old delivery receipt');
      await checkpoint(page, info, 'locally-queued-server-still-pending');
      release();
      if (outcome === 'accepted') await expect.poll(async () => (await entries()).length).toBe(0);
      else {
        // AT LEAST ONE, NOT EXACTLY ONE. `attempts` is a monotonic counter the
        // product is DESIGNED to increment: a pending entry is retried on the
        // sync backoff, so this value moves during the very window the poll
        // watches. `expect.poll` can only wait a value TOWARD its expectation,
        // so once attempts passes 1 the assertion can never be satisfied and
        // burns its full 5s timeout.
        //
        // MEASURED on this host by sampling the entry every 500ms after
        // release(): queued goes 1 -> 2 at t=4000ms against a 5000ms poll, a
        // margin of one second. That is the whole flake: a fast runner reads at
        // ~0ms and passes, a loaded CI runner reads after 4s and fails with
        // "Received: 2". Reproduced deterministically here by waiting 4.5s
        // before the assertion, which turns the intermittent red into a certain
        // one, and `refused` never fails because it settles to `blocked` and
        // blocked entries are excluded from the pending set that gets retried.
        //
        // The subject of this test is that the entry persists locally and
        // clears only after persistence; the no-double-send property is already
        // pinned by `payloads.length` above. One attempt or two, the invariant
        // is that the send was recorded and the entry survived.
        await expect.poll(async () => (await entries())[0]?.attempts).toBeGreaterThanOrEqual(1);
        expect((await entries())[0].state).toBe(outcome === 'refused' ? 'blocked' : 'pending');
        if (outcome === 'unconfirmed') expect((await entries())[0].delivery_uncertain).toBe(true);
      }
      await expect(input).toHaveValue('A newer draft must survive the old delivery receipt');
      if (outcome === 'accepted') expect(await page.evaluate(() => (window as any).__composerToasts)).not.toEqual(
        expect.arrayContaining([expect.stringMatching(/queued operation|syncing|sending/i)]));
      await page.reload({waitUntil:'domcontentloaded'});
      const persisted = await entries();
      if (outcome !== 'accepted') {
        expect(persisted).toHaveLength(1);
        expect(JSON.parse(persisted[0].options.body)).toEqual(intent);
        if (outcome === 'unconfirmed') {
          // The reload starts an automatic sync, and runSyncBanner() JOINS a
          // sync already in flight. Reading `before` while that one was still
          // running made the manual call add no new lookup (CI red on
          // ios-safari, 2026-10-01, "Received: 2"). Let it settle first.
          await page.evaluate(async () => {
            for (let i = 0; i < 50; i++) {
              // eslint-disable-next-line no-undef
              const flight = (globalThis as any).eval('typeof _syncFlight !== "undefined" ? _syncFlight : null');
              if (!flight) return;
              await flight.catch(() => {});
            }
          });
          const before = receiptReads;
          await page.evaluate(() => (window as any).runSyncBanner(true));
          expect(receiptReads, 'uncertain delivery retries only the receipt lookup').toBeGreaterThan(before);
          expect(payloads, 'confirmation must never resend the original command').toHaveLength(1);
          expect(JSON.parse((await entries())[0].options.body)).toEqual(intent);
        }
      }
    } finally {
      release();
      if (!page.isClosed()) {
        await page.evaluate(name => eval('_mutateQueue')((queue: any[]) => { for (let i = queue.length - 1; i >= 0; i--) if (queue[i].url.endsWith(`/${name}/send`)) queue.splice(i, 1); }), name);
        await deleteOwnedWorkers(page, request, headers, [name]);
      }
    }
  });
}

test('LC-COMPOSER: focused and replacement inputs clear exactly the accepted draft', async ({ page }, info) => {
  await boot(page);
  const result = await page.evaluate(() => {
    const name = 'lc-focus-regression';
    const input = document.createElement('textarea');
    input.id = 'input-' + name; document.body.appendChild(input);
    input.value = 'accepted message'; input.focus();
    eval('_draftSave')(name, input.value);
    eval('_composerAcceptLocal')(name, input.value);
    const focusedCleared = input.value === '';
    const replacement = input.cloneNode() as HTMLTextAreaElement;
    replacement.value = 'replacement accepted'; input.replaceWith(replacement); replacement.focus();
    eval('_draftSave')(name, replacement.value);
    eval('_composerAcceptLocal')(name, 'replacement accepted');
    const replacementCleared = replacement.value === '';
    eval('_draftSave')(name, 'older accepted');
    // Create the newer edit AFTER saving the submitted revision: saving now
    // updates every view immediately instead of maintaining a separate copy.
    replacement.value = 'newer edit';
    eval('_composerAcceptLocal')(name, 'older accepted');
    const newerPreserved = replacement.value === 'newer edit' && eval('_draftGet')(name) === 'newer edit';
    replacement.remove(); eval('_draftClear')(name);
    return { focusedCleared, replacementCleared, newerPreserved };
  });
  expect(result).toEqual({ focusedCleared:true, replacementCleared:true, newerPreserved:true });
});

test('LC-COMPOSER: failed local persistence retains draft and sends nothing', async ({ page, request }) => {
  test.setTimeout(60_000);
  await boot(page);
  const headers = await auth(page);
  const name = `lc-storage-${Date.now()}`;
  await createMockWorker(request, headers, name);
  let sends = 0;
  const storageFailures: any[] = [];
  await page.route('**/api/client-debug', route => {
    const body = route.request().postDataJSON();
    if (body.kind === 'outbox-storage') storageFailures.push(body);
    return route.continue();
  });
  // Zero requests is the required outcome when durable local storage fails.
  allowUnusedRoute(page, `**/api/sessions/${name}/send`);
  await page.route(`**/api/sessions/${name}/send`, route => { sends++; return route.fulfill({json:{ok:true, submitted:true}}); });
  try {
    await page.goto('/#view=sessions', {waitUntil:'domcontentloaded'});
    await page.locator(`.card[data-session="${name}"]`).locator('visible=true').first().locator('.card-menu-btn').click();
    await page.locator('.card-menu.open [data-worker-action="peek-terminal"]').click();
    await page.locator('#peek-cmd-input').fill('Storage failure must keep this draft');
    await page.evaluate(() => {
      const original = Storage.prototype.setItem;
      (window as any).__restoreStorage = () => { Storage.prototype.setItem = original; };
      Storage.prototype.setItem = function(key, value) {
        if (key === 'amux_offline_queue') throw new DOMException('Lifecycle controlled quota', 'QuotaExceededError');
        return original.call(this, key, value);
      };
    });
    await page.locator('#peek-overlay .send-split-main').click();
    await expect(page.locator('#peek-cmd-input')).toHaveValue('Storage failure must keep this draft');
    await expect(page.locator('#peek-overlay .send-split-main')).toBeEnabled();
    expect(sends).toBe(0);
    await expect.poll(() => storageFailures.length).toBe(1);
    expect(storageFailures[0]).toMatchObject({verdict:'write_failed',reason:'quota_exceeded',web_locks:true});
    expect(JSON.stringify(storageFailures)).not.toContain('Storage failure must keep this draft');
    await expect(page.locator('#toast')).toContainText('Device storage full');
    await page.evaluate(() => (window as any).showConnHistory());
    await expect(page.locator('#conn-modal-write-notice')).toContainText('Device storage full');
    await expect(page.locator('#conn-modal-write-notice')).toContainText('has not left this device');
  } finally {
    if (!page.isClosed()) {
      await page.evaluate(() => (window as any).__restoreStorage?.());
      const connectionModal = page.locator('#conn-hist-modal');
      if (await connectionModal.isVisible()) await connectionModal.click({position:{x:4,y:4}});
      await deleteOwnedWorkers(page, request, headers, [name]);
    }
  }
});
