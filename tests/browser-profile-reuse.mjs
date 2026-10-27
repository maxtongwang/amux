#!/usr/bin/env node
// End-to-end proof that a browser PROFILE created in amux (create -> navigate ->
// persist page state -> flush) is reusable by BOTH the CDP path and headless
// Playwright. Run: `node tests/browser-profile-reuse.mjs` against a running amux.
//
// WHAT IT LOCKS IN (asserted; the test exits non-zero if any regress):
//   1. Create + start writes a real amux-owned user-data-dir (returned by /start).
//   2. A localStorage + cookie marker set via the amux CDP driver is actually stored.
//   3. /stop flushes cleanly (clean_exit:true) -- the SIGTERM that persists Chrome's
//      Cookies/Local Storage; state only exists after a clean exit.
//   4. CDP REUSE (amux relaunches the same profile): BOTH localStorage and cookie
//      survive -- full-state reuse via CDP is bulletproof.
//   5. Playwright REUSE (launchPersistentContext on the same dir), real Chrome
//      (channel:chrome) AND bundled Chromium: localStorage survives.
//
// WHAT IT DOCUMENTS BUT DOES NOT YET ASSERT (a known, carded boundary):
//   Cookies do NOT survive amux -> Playwright reuse on macOS. Not an engine
//   mismatch (channel:chrome is real Chrome and still loses them): amux encrypts
//   cookies with the real macOS Keychain ("Chrome Safe Storage"), while Playwright
//   launches Chrome with --use-mock-keychain --password-store=basic and cannot
//   decrypt them. localStorage/IndexedDB (not keychain-encrypted) cross fine.
//   The fix is to launch amux's Chrome with --use-mock-keychain too (browser.rs
//   ~:482), which makes cookies portable at the cost of a one-time re-login for
//   existing real-keychain profiles. Tracked separately; when it lands, promote
//   the cookie-via-Playwright checks below from OBSERVED to asserted.
//
// GOTCHAS this test encodes for any caller doing profile reuse:
//   - /api/browser/action eval MUST pass `session` or it resolves a blank
//     (about:blank, opaque-origin) tab and page state is inaccessible.
//   - Wait for the page to actually land on the target origin before reading.
//   - Single-writer: never hold the user-data-dir in two Chromes at once. Always
//     create -> use -> /stop (clean_exit) BEFORE reusing it elsewhere.
process.env.NODE_TLS_REJECT_UNAUTHORIZED = '0'; // amux serves a self-signed cert on localhost
import { chromium } from 'playwright';
import { execSync } from 'node:child_process';
import { testBase } from './lib/live-base.mjs';

// `amux url` resolves the canonical base and self-heals past a stale $AMUX_URL
// (a lane started pre-cutover carries the retired :8822 in its env -- AMUX-3046).
// Do NOT read AMUX_URL directly: it is exactly the value that goes stale.
// The live base only on AMUX_TEST_LIVE=1 (tests/lib/live-base.mjs).
const BASE = testBase('AMUX_TEST_URL', () => execSync('./amux url', { encoding: 'utf8' })).trim();
const SESS = 'browserprofiletest';
const NAME = 'e2eprof' + Math.random().toString(36).slice(2, 8);
const K = 'amux_profile_reuse_marker';
const CK = 'amux_profile_reuse_cookie';
const TOKEN = 'TOK-' + Math.random().toString(36).slice(2, 12);
const ORIGIN = 'https://example.com'; // stable, minimal third-party origin (stands in for a login site)
const ORIGIN_HOST = 'example.com';

let failures = 0;
const ok = (cond, label) => {
  console.log(`  ${cond ? 'PASS' : 'FAIL'}  ${label}`);
  if (!cond) failures++;
};
const note = (label) => console.log(`  NOTE  ${label}`);
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const api = async (method, path, body) => {
  const r = await fetch(BASE + path, {
    method,
    headers: { 'Content-Type': 'application/json' },
    body: body ? JSON.stringify(body) : undefined,
  });
  const t = await r.text();
  let j;
  try { j = JSON.parse(t); } catch { j = t; }
  return { status: r.status, body: j };
};
const evalPage = async (script) =>
  (await api('POST', '/api/browser/action', { action: 'eval', script, session: SESS })).body;
const settle = async () => {
  for (let i = 0; i < 30; i++) {
    const b = await evalPage('location.href');
    if (b && typeof b.result === 'string' && b.result.indexOf(ORIGIN_HOST) >= 0) return true;
    await sleep(500);
  }
  return false;
};

async function main() {
  console.log(`[browser-profile-reuse] base=${BASE} profile=${NAME} origin=${ORIGIN}`);

  // Respect the single shared browser slot: refuse to evict a peer's session.
  const pre = await api('GET', '/api/browser/status');
  if (pre.body && pre.body.running) {
    console.log('SKIP: the shared amux browser slot is in use (running:true). Re-run when free; refusing to evict a peer (AMUX-3063).');
    return 0;
  }

  let userDataDir;
  try {
    await api('POST', '/api/browser/profile/create', { name: NAME });
    const start = await api('POST', '/api/browser/start', { profile: NAME, url: ORIGIN, session: SESS });
    userDataDir = start.body && start.body.user_data_dir;
    ok(!!userDataDir && userDataDir.includes('playwright-auth'), `create+start returns an amux-owned user_data_dir (${userDataDir})`);
    ok(await settle(), 'amux page navigates to the target origin');

    const setRes = await evalPage(
      `(()=>{try{localStorage.setItem(${JSON.stringify(K)},${JSON.stringify(TOKEN)});document.cookie=${JSON.stringify(CK + '=' + TOKEN + ';path=/;max-age=3600')};return {ls:localStorage.getItem(${JSON.stringify(K)}),ck:document.cookie.indexOf(${JSON.stringify(CK)})>=0};}catch(e){return {err:String(e)}}})()`,
    );
    ok(setRes.result && setRes.result.ls === TOKEN && setRes.result.ck === true, `marker (localStorage+cookie) set via amux CDP: ${JSON.stringify(setRes.result)}`);

    const stop1 = await api('POST', '/api/browser/stop');
    ok(stop1.body && stop1.body.clean_exit === true, 'stop flushes the profile cleanly (clean_exit:true)');

    // --- CDP reuse: amux relaunches the same profile ---
    await api('POST', '/api/browser/start', { profile: NAME, url: ORIGIN, session: SESS });
    await settle();
    const cdp = await evalPage(`(()=>({ls:localStorage.getItem(${JSON.stringify(K)}),ck:document.cookie.indexOf(${JSON.stringify(CK)})>=0}))()`);
    ok(cdp.result && cdp.result.ls === TOKEN, 'CDP reuse: localStorage survives');
    ok(cdp.result && cdp.result.ck === true, 'CDP reuse: cookie survives (same real Chrome + keychain)');
    await api('POST', '/api/browser/stop');

    // --- Playwright reuse: same user-data-dir, real Chrome then bundled Chromium ---
    for (const cfg of [{ label: 'channel:chrome', channel: 'chrome' }, { label: 'bundled-chromium' }]) {
      let ctx;
      try {
        ctx = await chromium.launchPersistentContext(userDataDir, {
          headless: true,
          ignoreHTTPSErrors: true,
          ...(cfg.channel ? { channel: cfg.channel } : {}),
        });
        const page = ctx.pages()[0] || (await ctx.newPage());
        await page.goto(ORIGIN, { waitUntil: 'domcontentloaded', timeout: 25000 });
        const ls = await page.evaluate((k) => localStorage.getItem(k), K);
        const cookies = await ctx.cookies();
        const ck = cookies.some((c) => c.name === CK && c.value === TOKEN);
        ok(ls === TOKEN, `Playwright reuse (${cfg.label}): localStorage survives`);
        if (ck) note(`Playwright reuse (${cfg.label}): cookie survived (keychain fix may have landed -- promote this to an assert)`);
        else note(`Playwright reuse (${cfg.label}): cookie MISSING (expected on macOS: real-keychain vs Playwright --use-mock-keychain)`);
      } finally {
        if (ctx) await ctx.close();
      }
    }
  } finally {
    await api('DELETE', '/api/browser/profile/' + NAME);
  }

  console.log(failures === 0 ? '\nRESULT: PASS (all reuse guarantees hold)' : `\nRESULT: FAIL (${failures} guarantee(s) regressed)`);
  return failures === 0 ? 0 : 1;
}

main().then((code) => process.exit(code)).catch((e) => {
  console.error('[browser-profile-reuse] ERROR', e && (e.stack || e.message || String(e)));
  process.exit(1);
});
