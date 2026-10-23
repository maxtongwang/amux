// The sync banner records what it claims (AMUX-4682).
//
// Ethan screenshotted "Syncing 0/2" over two messages the server had already
// delivered. Finding out which two operations it meant was impossible: the
// client beacons a composer accept and a display join, and wrote nothing when
// it told him N operations were unsynced. This pins that it now does, and — the
// half that is easy to get wrong — that it does so WITHOUT the message text.
//
// `describeOp` embeds a 30-character preview of a send's body, so the obvious
// implementation of "beacon the op labels" would ship worker messages to
// /api/client-debug. The second assertion below is the one that would catch
// that coming back.
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import path from 'node:path';
import {chromium} from 'playwright';
import {testBase} from './lib/live-base.mjs';

const root = path.resolve(import.meta.dirname, '..');
const base = testBase('AMUX_TEST_URL', 'https://localhost:8824');
const source = await fs.readFile(path.join(root, 'crates/amux-dashboard/static/app.js'), 'utf8');
const css = await fs.readFile(path.join(root, 'crates/amux-dashboard/static/app.css'), 'utf8');

// A body a careless implementation would leak verbatim.
const SECRET_TEXT = 'do it all test eval etc locally SENTINEL-4682';

const browser = await chromium.launch({headless: true});
try {
  const context = await browser.newContext({ignoreHTTPSErrors: true, serviceWorkers: 'block',
    viewport: {width: 1280, height: 900}});
  await context.addInitScript(() => {
    localStorage.setItem('amux_walkthrough_done', '1');
    window.EventSource = class {
      static OPEN = 1; static CLOSED = 2; readyState = 1;
      close() {this.readyState = 2;} addEventListener() {} removeEventListener() {}
    };
  });
  const page = await context.newPage();
  const beacons = [];
  await page.route('**/app.js*', r => r.fulfill({contentType: 'text/javascript', body: source}));
  await page.route('**/app.css*', r => r.fulfill({contentType: 'text/css', body: css}));
  // ONE handler for /api/**, with client-debug captured inside it. A separate
  // `**/api/client-debug*` route registered first is SHADOWED: playwright
  // matches the most recently registered route, so the catch-all below would
  // answer the beacon with its 409 and this array would stay empty — which
  // reads exactly like "the beacon was never emitted".
  await page.route('**/api/**', r => {
    const u = new URL(r.request().url());
    if (u.pathname === '/api/client-debug') {
      try { beacons.push(JSON.parse(r.request().postData() || '{}')); } catch (e) {}
      return r.fulfill({json: {ok: true}});
    }
    // The queued ops SUCCEED, so the queue drains, failCount stays 0 and the
    // banner reaches its clear path. A 409 here leaves it up for review by
    // design, and the `cleared` beacon would never fire — the test would be
    // measuring the failure branch while claiming to measure the lifecycle.
    if (/\/api\/sessions\/[^/]+\/(send|start)$/.test(u.pathname)) {
      return r.fulfill({json: {ok: true, id: 'srv-1'}});
    }
    if (r.request().method() !== 'GET') return r.fulfill({status: 409, json: {error: 'read-only fixture'}});
    if (u.pathname === '/api/sessions') return r.fulfill({json: []});
    if (u.pathname === '/api/board') return r.fulfill({json: []});
    if (u.pathname === '/api/sync') return r.fulfill({json: {issues: [], sessions: []}});
    return r.continue();
  });
  await page.goto(base + '/#view=board');
  await page.waitForFunction(() => typeof _syncBannerBeacon === 'function');

  // DRIVE THE REAL PATH. Calling _syncBannerBeacon directly would prove the
  // function works and nothing about whether the banner calls it — deleting the
  // call site left that version of this test green, which is the whole reason
  // it goes through _runSyncBanner instead.
  await page.evaluate(secret => {
    offlineQueue = [
      {id: 1, url: '/api/sessions/mixpeek-agent-memory-research/send', state: 'pending',
       options: {method: 'POST', body: JSON.stringify({text: secret, msg_id: 'm1'})},
       queued_at: Date.now() - 42000, delivery_uncertain: true},
      {id: 2, url: '/api/sessions/lane-b/start', state: 'pending',
       options: {method: 'POST'}, queued_at: Date.now() - 9000},
    ];
  }, SECRET_TEXT);
  await page.evaluate(() => _runSyncBanner(false));
  await page.waitForTimeout(4000);

  const shown = beacons.find(b => b.verdict === 'sync_banner_shown');
  const cleared = beacons.find(b => b.verdict === 'sync_banner_cleared');
  assert.ok(shown, `the banner must record that it was shown; got ${beacons.map(b => b.verdict).join(',')}`);
  assert.ok(cleared, 'the banner must record when it cleared');

  // IT SAYS WHAT IT WAS COUNTING. This is the question that could not be
  // answered from the screenshot.
  assert.equal(shown.items, 2, 'the count the reader saw');
  assert.equal(shown.done, 0, '"0 of 2" means none done');
  assert.equal(shown.n_considered, 2, 'a measurement publishes its population');
  assert.equal(shown.ops.length, 2, 'both operations are identified');
  assert.equal(shown.ops[0].action, 'send');
  assert.equal(shown.ops[0].target, 'mixpeek-agent-memory-research', 'which lane the send was for');
  assert.ok(shown.ops[0].queued_s >= 40, `how long it had been queued, got ${shown.ops[0].queued_s}`);
  assert.equal(shown.ops[0].uncertain, true, 'an uncertain send is distinguishable from an unsent one');
  assert.equal(shown.ops[1].action, 'start', 'a non-send operation is identified by its action too');

  // AND NOT WHAT THEY SAID. The label held the body; the beacon must not.
  const wire = JSON.stringify(beacons);
  assert.ok(!wire.includes(SECRET_TEXT),
    'the beacon must not carry message text: describeOp previews the body, and this is /api/client-debug');
  assert.ok(!wire.includes('SENTINEL-4682'), 'no fragment of the body either');

  // The duration is the other half of "how bad was it".
  assert.ok(typeof cleared.up_ms === 'number', 'the cleared beacon reports how long the banner was up');

  console.log(`PASS: banner records ${shown.items} ops (${shown.ops.map(o => o.action + '/' + o.target).join(', ')}), carries no message text`);
  await context.close();
} finally {await browser.close();}
