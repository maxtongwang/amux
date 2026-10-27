// Read-only browser check for the lease, blocked-by and attempts rendering
// (RR-0052 / AMUX-4529). Board and sessions are fixtures; every write is
// refused, and no live worker is touched.
//
// The fixtures are the states that are hard to catch by reading: a lease whose
// holder stopped beating, a dependency this client never loaded, and a card
// whose attempts outnumber the one holding it now. Each assertion below is
// paired with the mutation that should break it, because a chip that renders
// is not the same as a chip that renders the truth.
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import path from 'node:path';
import {chromium} from 'playwright';
import {testBase} from './lib/live-base.mjs';

const root = path.resolve(import.meta.dirname, '..');
const base = testBase('AMUX_TEST_URL', 'https://localhost:8824');
const source = await fs.readFile(path.join(root, 'crates/amux-dashboard/static/app.js'), 'utf8');
const css = await fs.readFile(path.join(root, 'crates/amux-dashboard/static/app.css'), 'utf8');

const now = Math.floor(Date.now() / 1000);
function card(id, status, extra = {}) {
  return {id, session: 'lease-fixture', status, title: 'Fixture ' + id, type: 'chore',
    owner_type: 'agent', tags: [], desc: '', depends_on: [], archived: false,
    created: now - 7200, updated: now - 60, pos: 0, ...extra};
}
// Held right now: beating, and with over half an hour left on the lease.
const held = card('LEASE-FRESH', 'doing', {lease: {holder: 'lane-alpha', attempt: 1, generation: 1,
  acquired_at: now - 600, heartbeat_at: now - 30, expires_at: now + 1770}});
// Held on paper only: the holder stopped beating over an hour ago and the
// lease ran out, so another worker may take the card. This is the state that
// looked identical to the one above before the chip existed.
const stale = card('LEASE-STALE', 'doing', {lease: {holder: 'lane-beta', attempt: 3, generation: 2,
  acquired_at: now - 9000, heartbeat_at: now - 5400, expires_at: now - 3600}});
const unheld = card('LEASE-NONE', 'doing');
const blocked = card('DEP-OPEN', 'todo', {depends_on: ['LEASE-FRESH']});
const satisfied = card('DEP-DONE', 'todo', {depends_on: ['DEP-FINISHED']});
const finishedDep = card('DEP-FINISHED', 'verified');
// The dependency is not in the loaded board, so its status is unknowable here.
const unknownDep = card('DEP-UNKNOWN', 'todo', {depends_on: ['OFF-BOARD-1', 'OFF-BOARD-2']});
// A finished card's unresolved dependency no longer blocks anything.
const terminal = card('DEP-TERMINAL', 'done', {depends_on: ['LEASE-FRESH']});
const board = [held, stale, unheld, blocked, satisfied, finishedDep, unknownDep, terminal];

// Four attempts on one card, newest last in the payload so the renderer's own
// ordering is what puts #4 on top.
const attempts = [
  {attempt: 1, generation: 1, worker: 'lane-alpha', started_at: now - 20000, ended_at: now - 19000,
   outcome: 'parked', to_status: 'backlog', ended_by: 'board_drive', reason: 'Auto-parked from Doing: unresolved dependency'},
  {attempt: 2, generation: 1, worker: 'lane-beta', started_at: now - 18000, ended_at: now - 17000,
   outcome: 'expired', to_status: 'todo', ended_by: 'lease_reaper', reason: null},
  {attempt: 3, generation: 2, worker: 'lane-beta', started_at: now - 9000, ended_at: now - 3600,
   outcome: null, to_status: null, ended_by: null, reason: null},
  {attempt: 4, generation: 2, worker: 'lane-alpha', started_at: now - 600, ended_at: null,
   outcome: null, to_status: null, ended_by: null, reason: null},
];
const detail = {...held, attempts, lease: {...held.lease, attempt: 4}};

const sessions = [{name: 'lease-fixture', provider: 'claude', model: 'claude-haiku-4-5',
  active_model: 'claude-haiku-4-5', status: 'active', running: true, lifecycle: 'active',
  archived: false, tags: [], dir: '/tmp/board-lease-fixture', flags: '', desc: 'Lease fixture',
  preview: '', task_name: '', task_source: '', task_board_id: '',
  runtime_board: {measured: true, runtime_status: 'active', status: 'linked', verdict: 'linked',
    card_id: 'LEASE-FRESH', observed_card_id: 'LEASE-FRESH'}}];

const browser = await chromium.launch({headless: true});
try {
  for (const width of [1280, 375]) {
    const context = await browser.newContext({ignoreHTTPSErrors: true, serviceWorkers: 'block',
      viewport: {width, height: 900}});
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
      if (u.pathname === '/api/board/LEASE-FRESH') return r.fulfill({json: detail});
      if (u.pathname === '/api/sync') return r.fulfill({json: {issues: board, sessions}});
      return r.continue();
    });
    await page.goto(base + '/#view=board');
    await page.waitForFunction(() => typeof renderBoard === 'function' && boardItems.length === 8);
    await page.locator('#tab-board').click();
    await page.evaluate(() => {boardViewMode = 'status'; boardOwnerFilter = 'all'; renderBoard();});
    await page.locator('.board-card[data-id="LEASE-FRESH"]').waitFor();

    const chip = id => page.locator(`.board-card[data-id="${id}"] .board-card-lease`);
    const blockChip = id => page.locator(`.board-card[data-id="${id}"] .board-card-blocked`);

    // A live hold names its holder and how long since it beat. The heartbeat
    // age is the assertion that matters: the holder's name alone was already
    // available from the card's session.
    assert.equal(await chip('LEASE-FRESH').count(), 1, 'a held card must show its lease');
    const freshText = await chip('LEASE-FRESH').innerText();
    assert.match(freshText, /lane-alpha/);
    assert.match(freshText, /just now|\dm ago/, 'the chip must carry a heartbeat age');
    assert.doesNotMatch(freshText, /attempt/, 'a first attempt is the default and stays unsaid');
    assert.equal(await page.locator('.board-card[data-id="LEASE-FRESH"] .board-card-lease-stale').count(), 0);
    // timeAgo() has no future branch, so a lease with half an hour left used to
    // read "expires just now" in the tooltip. This is that regression.
    const freshTitle = await chip('LEASE-FRESH').getAttribute('title');
    assert.match(freshTitle, /lease expires in \d+m/, `expiry must read as remaining time, got: ${freshTitle}`);
    assert.doesNotMatch(freshTitle, /expires just now/);

    // An expired hold must not look like a healthy one.
    assert.equal(await page.locator('.board-card[data-id="LEASE-STALE"] .board-card-lease-stale').count(), 1,
      'an expired lease must be marked stale');
    const staleText = await chip('LEASE-STALE').innerText();
    assert.match(staleText, /expired/);
    assert.match(staleText, /attempt 3/, 'a repeat attempt is the point of the chip');
    assert.match(await chip('LEASE-STALE').getAttribute('title'), /another worker may claim it/);

    assert.equal(await chip('LEASE-NONE').count(), 0, 'an unheld card must show no lease chip');

    // Blocked-by only for dependencies this client can SEE are unfinished.
    assert.equal(await blockChip('DEP-OPEN').count(), 1);
    assert.match(await blockChip('DEP-OPEN').innerText(), /LEASE-FRESH/);
    assert.equal(await blockChip('DEP-DONE').count(), 0, 'a finished dependency does not block');
    assert.equal(await blockChip('DEP-TERMINAL').count(), 0, 'a finished card is not blocked by anything');
    // The honest answer for a dependency that was never loaded is "unknown",
    // never "blocked": this client has no row for it to read.
    const unknownText = await blockChip('DEP-UNKNOWN').innerText();
    assert.match(unknownText, /2 unknown/);
    const unknownTitle = await blockChip('DEP-UNKNOWN').getAttribute('title');
    assert.match(unknownTitle, /not in the loaded board/);
    assert.match(unknownTitle, /OFF-BOARD-1/, 'name the dependencies it could not resolve');

    // Nothing may spill out of its card at phone width.
    const overflow = await page.evaluate(() => {
      const bad = [];
      document.querySelectorAll('.board-card').forEach(c => {
        const box = c.getBoundingClientRect();
        c.querySelectorAll('.board-card-lease,.board-card-blocked').forEach(el => {
          const b = el.getBoundingClientRect();
          if (b.right > box.right + 1 || b.bottom > box.bottom + 1) bad.push(c.dataset.id + ':' + el.className);
        });
      });
      return bad;
    });
    assert.deepEqual(overflow, [], 'lease and blocked-by chips must stay inside their card');

    // Detail: every attempt, newest first, with the live one named as such.
    await page.evaluate(() => openBoardDetail('LEASE-FRESH'));
    await page.waitForFunction(() => /Attempts \(4\)/.test(document.getElementById('bd-meta')?.innerText || ''));
    const meta = await page.locator('#bd-meta').innerText();
    const order = ['#4', '#3', '#2', '#1'].map(n => meta.indexOf(n));
    assert.deepEqual(order, [...order].sort((a, b) => a - b), 'attempts must read newest first');
    assert.match(meta, /holding now/, 'the attempt matching the live lease is still open');
    assert.match(meta, /parked/);
    assert.match(meta, /left it in backlog/);
    assert.match(meta, /ended by lease_reaper/);
    assert.match(meta, /Auto-parked from Doing/);
    // An attempt that ended with no recorded outcome must say that rather than
    // borrowing the live one's wording.
    assert.match(meta, /ended without a recorded outcome/);

    console.log(`PASS ${width}px: lease holder + heartbeat, stale lease, blocked-by, unknown dependency, contained chips, attempts newest-first`);
    await context.close();
  }
} finally {await browser.close();}
