// Files cwd initialization — a late saved preference must respect explicit navigation and folder-target actions (refs ai-config #340).
import { test, expect } from './fixtures';
import { auth } from './lifecycle/evidence';
import { cleanup } from './teardown';
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';

declare const _filesCwd: string;
const CWD_RESPONSE_DELAY_MS = 1500;

for (const explicit of [false, true]) {
  test(explicit ? 'Files keeps an explicit hash path when saved cwd arrives late'
    : 'Files opened immediately waits for saved cwd', async ({ page, request }, info) => {
    const dir = await mkdtemp(path.join(tmpdir(), 'amux-files-cwd-'));
    const other = path.join(dir, 'explicit');
    await mkdir(other);
    await writeFile(path.join(dir, 'saved.txt'), 'saved directory');
    await writeFile(path.join(other, 'explicit.txt'), 'explicit directory');
    await page.goto('/#view=sessions');
    const headers = await auth(page);
    const previous = await request.get('/api/prefs?key=files_cwd', { headers });
    expect(previous.ok()).toBeTruthy();
    const prior = await previous.json();
    let releaseCwd!: () => void;
    const navigated = new Promise<void>(resolve => { releaseCwd = resolve; });
    let held = false;
    try {
      expect((await request.post('/api/prefs', { headers,
        data: { key: 'files_cwd', value: dir } })).ok()).toBeTruthy();
      await page.route('**/api/prefs?key=files_cwd*', async route => {
        const response = await route.fetch();
        held = true;
        // Hold until navigation too, so a slow runner cannot miss the race.
        await Promise.all([new Promise(resolve => setTimeout(resolve, CWD_RESPONSE_DELAY_MS)), navigated]);
        await route.fulfill({ response });
      });
      await page.reload({ waitUntil: 'domcontentloaded' });
      await expect.poll(() => held).toBe(true);
      await page.locator('#tab-files').click();
      if (explicit) {
        await page.evaluate(p => { location.hash = '#path=' + encodeURIComponent(p); }, other);
        await expect(page.locator('#files-breadcrumb .fe-crumb').last()).toHaveText('explicit');
      }
      const lateResponse = page.waitForResponse('**/api/prefs?key=files_cwd*');
      releaseCwd();
      const response = await lateResponse;
      expect(response.ok()).toBeTruthy();
      expect((await response.json()).value).toBe(dir);
      await expect.poll(() => page.evaluate(() => _filesCwd)).toBe(dir);
      const expected = explicit ? other : dir;
      await expect(page.locator('#files-breadcrumb .fe-crumb').last()).toHaveText(path.basename(expected));
      await expect(page.locator('#files-body .fe-row').filter({ hasText: explicit ? 'explicit.txt' : 'saved.txt' })).toBeVisible();
    } finally {
      releaseCwd();
      await cleanup('finish delayed cwd response', () => page.unrouteAll({ behavior: 'wait' }), info);
      await cleanup('restore files_cwd', () => request.post('/api/prefs', { headers,
        data: { key: 'files_cwd', value: prior.value || '' } }), info);
      await cleanup('remove cwd fixture', () => rm(dir, { recursive: true, force: true }), info);
    }
  });
}

test('Upload started while saved cwd is loading targets the saved cwd', async ({ page, request }, info) => {
  const dir = await mkdtemp(path.join(tmpdir(), 'amux-files-cwd-upload-'));
  const sourceDir = await mkdtemp(path.join(tmpdir(), 'amux-files-cwd-src-'));
  const name = `cwd-race-${Date.now()}.txt`;
  const source = path.join(sourceDir, name);
  await writeFile(source, 'uploaded during cwd wait');
  await page.goto('/#view=sessions');
  const headers = await auth(page);
  const previous = await request.get('/api/prefs?key=files_cwd', { headers });
  expect(previous.ok()).toBeTruthy();
  const prior = await previous.json();
  let releaseCwd!: () => void;
  const released = new Promise<void>(resolve => { releaseCwd = resolve; });
  let held = false;
  const targets: any[] = [];
  page.on('console', async msg => {
    if (msg.text().startsWith('files_target_dir')) targets.push(await msg.args()[1]?.jsonValue());
  });
  try {
    expect((await request.post('/api/prefs', { headers,
      data: { key: 'files_cwd', value: dir } })).ok()).toBeTruthy();
    await page.route('**/api/prefs?key=files_cwd*', async route => {
      const response = await route.fetch();
      held = true;
      await released;
      await route.fulfill({ response });
    });
    await page.reload({ waitUntil: 'domcontentloaded' });
    await expect.poll(() => held).toBe(true);
    await page.locator('#tab-files').click();
    const choose = page.waitForEvent('filechooser');
    const button = page.getByTitle('Upload files into this folder', { exact: true });
    if (await button.isVisible()) await button.click();
    else {
      await page.locator('#files-overflow-btn').click();
      await page.locator('#files-overflow-menu').getByRole('button', { name: /Upload files/ }).click();
    }
    await (await choose).setFiles(source);
    // The upload is now waiting on the held cwd; releasing it must send the file there.
    releaseCwd();
    await expect.poll(async () => readFile(path.join(dir, name), 'utf8').catch(() => null),
      { message: `upload must land in the saved cwd ${dir}`, timeout: 30_000 }).toBe('uploaded during cwd wait');
    await expect.poll(() => targets.find(t => t?.action === 'upload'),
      { message: 'files_target_dir diagnostic must report the upload wait' })
      .toMatchObject({ action: 'upload', waited: true, root: false, measured: true });
  } finally {
    releaseCwd();
    await cleanup('finish delayed cwd response', () => page.unrouteAll({ behavior: 'wait' }), info);
    await cleanup('restore files_cwd', () => request.post('/api/prefs', { headers,
      data: { key: 'files_cwd', value: prior.value || '' } }), info);
    await cleanup('remove cwd fixtures', async () => {
      await rm(dir, { recursive: true, force: true });
      await rm(sourceDir, { recursive: true, force: true });
    }, info);
  }
});
