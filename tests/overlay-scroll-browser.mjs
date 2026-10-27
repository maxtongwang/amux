// Can every window be scrolled to its last line? (AMUX-4684)
//
// Ethan, 2026-09-15, screenshotting a markdown preview whose final table row
// was cut off: "make sure these windows can be scrolled down to the bottom
// test every window".
//
// This drives the SHIPPED check, `_modalLayoutCheck()`, rather than a copy of
// its logic, so a regression in the client is a red test here and a beacon in
// every reader's browser at the same time. Read-only fixtures; no live worker.
//
// The last case deliberately breaks a window and requires the check to SAY SO.
// Without it this file would pass just as happily against a check that returns
// an empty list for everything.
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import path from 'node:path';
import {chromium} from 'playwright';
import {testBase} from './lib/live-base.mjs';

const root = path.resolve(import.meta.dirname, '..');
const base = testBase('AMUX_TEST_URL', 'https://localhost:8824');
const source = await fs.readFile(path.join(root, 'crates/amux-dashboard/static/app.js'), 'utf8');
const css = await fs.readFile(path.join(root, 'crates/amux-dashboard/static/app.css'), 'utf8');

const REACH = [':content-unreachable', ':last-line-below-scroller', ':last-line-covered'];
const now = Math.floor(Date.now() / 1000);

// Long enough that every window has to scroll, and wide enough that the
// markdown table behaves like the file in the report.
const rows = Array.from({length: 60}, (_, i) =>
  `| 2026-07-${String((i % 28) + 1).padStart(2, '0')} 21:00 (evening) | Line ${i} of a quote long enough to wrap onto a second line inside its own cell. | Source ${i}, a book with a long title | Tradition ${i} |`);
const markdown = ['# Daily log', '', '| When | Quote | Source | Tradition |', '| --- | --- | --- | --- |', ...rows, ''].join('\n');

const board = Array.from({length: 24}, (_, i) => ({
  id: 'FIX-' + i, session: 'scroll-fixture', status: i % 3 ? 'todo' : 'doing',
  title: 'Fixture card ' + i + ' with a title long enough to wrap on a phone screen',
  type: 'chore', owner_type: 'agent', tags: [], desc: 'Fixture body.\n\n'.repeat(40),
  depends_on: [], archived: false, created: now - 3600, updated: now - 60, pos: i,
}));
const sessions = [{name: 'scroll-fixture', provider: 'claude', model: 'claude-haiku-4-5',
  active_model: 'claude-haiku-4-5', status: 'active', running: true, lifecycle: 'active',
  archived: false, tags: [], dir: '/tmp/scroll-fixture', flags: '', desc: 'Scroll fixture',
  preview: '', task_name: '', task_source: '', task_board_id: '',
  runtime_board: {measured: true, runtime_status: 'active', status: 'linked', verdict: 'linked',
    card_id: 'FIX-0', observed_card_id: 'FIX-0'}}];

const WINDOWS = [
  ['file-overlay', async p => { await p.evaluate(() => openFilePreview('/fixture/daily-log.md')); await p.waitForTimeout(1200); },
   async p => p.evaluate(() => closeFilePreview())],
  ['board-detail-overlay', async p => { await p.evaluate(() => openBoardDetail('FIX-0')); await p.waitForTimeout(1200); },
   async p => p.evaluate(() => closeBoardDetail())],
  ['about-overlay', async p => { await p.evaluate(() => openAbout()); await p.waitForTimeout(500); },
   async p => p.evaluate(() => document.getElementById('about-overlay').classList.remove('active'))],
  ['create-overlay', async p => { await p.evaluate(() => openCreate()); await p.waitForTimeout(500); },
   async p => p.evaluate(() => document.getElementById('create-overlay').classList.remove('active'))],
  ['filters-modal', async p => { await p.evaluate(() => openFiltersModal()); await p.waitForTimeout(500); },
   async p => p.evaluate(() => closeFiltersModal())],
  ['queue-overlay', async p => { await p.evaluate(() => showQueueModal()); await p.waitForTimeout(500); },
   async p => p.evaluate(() => document.getElementById('queue-overlay').classList.remove('active'))],
];

const browser = await chromium.launch({headless: true});
try {
  // 420px tall is the phone with its keyboard up, which is the state that
  // leaves a window too short for its own content.
  for (const [width, height] of [[1280, 900], [375, 812], [375, 420]]) {
    const context = await browser.newContext({ignoreHTTPSErrors: true, serviceWorkers: 'block',
      viewport: {width, height}});
    await context.addInitScript(() => {
      localStorage.setItem('amux_walkthrough_done', '1');
      window.EventSource = class {
        static OPEN = 1; static CLOSED = 2; readyState = 1;
        close() {this.readyState = 2;} addEventListener() {} removeEventListener() {}
      };
    });
    const page = await context.newPage();
    await page.route('**/app.js*', r => r.fulfill({contentType: 'text/javascript', body: source}));
    await page.route('**/app.css*', r => r.fulfill({contentType: 'text/css', body: css}));
    await page.route('**/api/**', r => {
      const u = new URL(r.request().url());
      if (r.request().method() !== 'GET') return r.fulfill({status: 409, json: {error: 'read-only UI fixture'}});
      if (u.pathname === '/api/sessions') return r.fulfill({json: sessions});
      if (u.pathname === '/api/board') return r.fulfill({json: board});
      if (u.pathname === '/api/board/FIX-0') return r.fulfill({json: board[0]});
      if (u.pathname === '/api/file') return r.fulfill({json: {path: '/fixture/daily-log.md', name: 'daily-log.md', content: markdown, size: markdown.length, is_text: true}});
      if (u.pathname === '/api/sync') return r.fulfill({json: {issues: board, sessions}});
      return r.continue();
    });
    await page.goto(base + '/#view=board');
    await page.waitForFunction(() => typeof _modalLayoutCheck === 'function' && typeof openFilePreview === 'function');

    for (const [id, open, close] of WINDOWS) {
      await open(page);
      const shownNow = await page.evaluate(i => {
        const el = document.getElementById(i);
        const s = el && getComputedStyle(el);
        return !!(el && s.display !== 'none' && s.opacity !== '0' && el.getBoundingClientRect().height > 0);
      }, id);
      assert.ok(shownNow, `${id} did not open at ${width}x${height}; a window nobody opened proves nothing`);
      // Scroll every container to its end, the way a reader would, then ask
      // the shipped check whether the last line is actually reachable.
      await page.evaluate(i => {
        const el = document.getElementById(i);
        [el, ...el.querySelectorAll('*')].reverse()
          .forEach(e => { if (e.scrollHeight > e.clientHeight + 4) e.scrollTop = e.scrollHeight; });
      }, id);
      await page.waitForTimeout(150);
      const clipped = await page.evaluate(() => _modalLayoutCheck().clipped);
      const mine = clipped.filter(c => c.startsWith(id) && REACH.some(k => c.endsWith(k)));
      assert.deepEqual(mine, [], `${id} cannot show its last line at ${width}x${height}: ${clipped.join(', ')}`);
      await close(page);
      await page.waitForTimeout(200);
    }

    // The check has to be able to fail, or every line above is decoration.
    await page.evaluate(() => openFilePreview('/fixture/daily-log.md'));
    await page.waitForTimeout(1000);
    const broken = await page.evaluate(() => {
      const body = document.getElementById('file-body');
      const before = _modalLayoutCheck().clipped.filter(c => c.startsWith('file-overlay:'));
      body.style.overflowY = 'hidden';                       // content nobody can scroll to
      const trapped = _modalLayoutCheck().clipped.filter(c => c.endsWith(':content-unreachable'));
      body.style.overflowY = '';
      body.scrollTop = body.scrollHeight;
      const foot = document.createElement('div');            // a footer parked on the last line
      foot.style.cssText = 'position:fixed;left:0;right:0;bottom:0;height:160px;background:#123;z-index:99999';
      document.body.appendChild(foot);
      const covered = _modalLayoutCheck().clipped.filter(c => c.endsWith(':last-line-covered'));
      foot.remove();
      return {before, trapped, covered};
    });
    assert.deepEqual(broken.before, [], `the healthy window must be quiet at ${width}x${height}`);
    assert.ok(broken.trapped.length, 'content with no way to scroll it must be reported');
    assert.ok(broken.covered.length, 'a last line under a fixed footer must be reported');
    await page.evaluate(() => closeFilePreview());

    console.log(`PASS ${width}x${height}: ${WINDOWS.length} windows reach their last line, and the check reports both failures when injected`);
    await context.close();
  }
} finally {await browser.close();}
