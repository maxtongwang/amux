// Scoped memory and rules: what the owner sees when setting them, and what the
// server says it reached. No model is launched (start:false); the real-model
// half of this (Claude and Codex workers following the canaries) is the
// results matrix in the PR that added this spec.
//
// Pins four owner-facing defects found by that run:
// - group configuration had no entry point at all (the Groups tab row created
//   no `grp-scope-body-<group>` element, though the save path targeted it);
// - with Memory open, tapping Environment still showed Memory, so its Edit
//   button wrote env text into the group's memory;
// - saving worker memory sent the worker a "update your memory file now" turn
//   that rewrote, and could undo, the owner's text;
// - the memory editor called a per-worker file "Shared, not per-worker" and
//   said nothing for an isolated worker that never receives it.
import { test, expect, Page } from './fixtures';
import { boot, deleteOwnedWorkers, createMockWorker } from './lifecycle/evidence';
import { cleanup } from './teardown';

async function appToken(page: Page): Promise<string> {
  await boot(page);
  const token = await page.evaluate(() => (window as any)._AMUX_AUTH_TOKEN as string);
  expect(token, 'served bootstrap must provide the API token').toBeTruthy();
  return token;
}

test('SCOPED-CONTEXT: group panel, single detail, honest memory save and reach', async ({ page, request }, info) => {
  test.setTimeout(120_000);
  const token = await appToken(page);
  const auth = { Authorization: `Bearer ${token}`, 'Content-Type': 'application/json' };
  const sfx = `${info.project.name}-${Date.now()}`.replace(/[^A-Za-z0-9-]/g, '');
  const group = `scg-${sfx}`.toLowerCase();
  const a = `lc-sc-a-${sfx}`, b = `lc-sc-b-${sfx}`, iso = `lc-sc-i-${sfx}`;
  try {
    for (const [name, tags, isolated] of [[a, [group], false], [b, ['scg-other'], false], [iso, [group], true]] as const) {
      await createMockWorker(request, auth, name, { tags, isolated });
    }

    // The server names who a group memory write reaches and who it cannot.
    const put = await request.put('/api/scope', { headers: auth,
      data: { level: 'group', name: group, capability: 'memory', value: { text: `GMEM-${sfx}` } } });
    expect(put.status()).toBe(200);
    const reach = await put.json();
    expect(reach.affected_workers).toEqual(expect.arrayContaining([a, iso]));
    expect(reach.affected_workers).not.toContain(b);
    expect(reach.isolated_not_reached).toEqual([iso]);
    expect(String(reach.takes_effect)).toContain('re-read');

    // Groups tab: the expanded row renders the group's own configuration.
    await page.evaluate(() => (window as any).fetchSessions());
    await page.evaluate(() => (window as any).switchView('groups'));
    const row = page.locator('.grp-tab-row', { hasText: group }).first();
    await row.locator('.grp-ov-r1').click();
    const panel = page.locator(`[id="grp-scope-body-${group}"]`);
    await expect(panel.locator('.scope-tile', { hasText: 'Memory' })).toBeVisible({ timeout: 15_000 });

    // One detail at a time: Memory, then Environment, shows Environment.
    await panel.locator('.scope-tile', { hasText: 'Memory' }).click();
    await expect(panel.locator('.scope-detail')).toContainText('Memory');
    await panel.locator('.scope-tile', { hasText: 'Environment' }).click();
    await expect(panel.locator('.scope-detail button', { hasText: 'at this level' })).toHaveText(/Edit Environment/);
    await expect(panel.locator('.scope-tile.sel')).toHaveCount(1);

    // Worker memory: Save saves. No follow-up turn is sent to the worker.
    const sends: string[] = [];
    page.on('request', (r) => { if (/\/api\/sessions\/[^/]+\/send$/.test(r.url())) sends.push(r.url()); });
    await page.evaluate((n) => (window as any).openPeek(n), a);
    await page.click('#peek-tab-memory');
    await expect(page.locator('#peek-memory-input')).toBeEnabled();
    await page.fill('#peek-memory-input', `WMEM-${sfx}`);
    const saved = page.waitForResponse((r) => r.url().endsWith(`/api/sessions/${a}/memory`) && r.request().method() === 'POST');
    await page.click('#peek-memory-save');
    const body = await (await saved).json();
    expect(body.affected_workers).toEqual([a]);
    await page.waitForTimeout(1500);
    expect(sends, 'saving memory must not ask the worker to rewrite it').toEqual([]);
    const stored = await (await request.get(`/api/sessions/${a}/memory`, { headers: auth })).json();
    expect(stored.content).toBe(`WMEM-${sfx}`);

    // An isolated worker's memory editor says the text never reaches it.
    await page.evaluate((n) => (window as any).openPeek(n), iso);
    await page.click('#peek-tab-memory');
    await expect(page.locator('#peek-memory-shared')).toContainText('Isolated worker');
  } finally {
    await cleanup('delete scoped-context workers', () => deleteOwnedWorkers(page, request, auth, [a, b, iso]), info);
    await cleanup('clear group memory', () => request.put('/api/scope', { headers: auth,
      data: { level: 'group', name: group, capability: 'memory', value: { text: '' } } }), info);
  }
});
