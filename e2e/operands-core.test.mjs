// Unit contract for e2e/operands-core.mjs, the browser-free half of recordOperands (e2e/operands.ts).
// Runs in spa-gates (`node --test e2e/*.test.mjs`); the browser half only runs inside the hosted e2e shards.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import {
  MAX_ENTRIES,
  MAX_PARSE_BYTES,
  OPERANDS_VERDICT,
  REDACTED,
  buildOperandsReport,
  capEntries,
  isSensitiveKey,
  matchesPath,
  normalizeOperandOptions,
  operandsLine,
  redact,
  redactUrl,
  summarizeBody,
} from './operands-core.mjs';

const SECRET = 'sk-live-0123456789abcdef';

test('credential-named keys are redacted at any depth; usage counters are not', () => {
  for (const k of ['Authorization', 'authorisation', 'Cookie', 'set-cookie', 'x-api-key', 'api_key', '_AMUX_AUTH_TOKEN',
    'token', 'refresh_token', 'password', 'passwd', 'client_secret', 'session_id', 'X-Amz-Signature', 'credentials']) {
    assert.equal(isSensitiveKey(k), true, k);
  }
  for (const k of ['input_tokens', 'output_tokens', 'title', 'key', 'model', 'session', 'status']) {
    assert.equal(isSensitiveKey(k), false, k);
  }
  const out = redact({ doc: { title: 'Resume here', fetch: { headers: { Authorization: SECRET, Accept: 'application/json' } } },
    usage: { input_tokens: 1234 }, list: [{ token: SECRET }] });
  assert.equal(out.doc.title, 'Resume here');
  assert.equal(out.doc.fetch.headers.Authorization, REDACTED);
  assert.equal(out.doc.fetch.headers.Accept, 'application/json');
  assert.equal(out.usage.input_tokens, 1234);
  assert.equal(out.list[0].token, REDACTED);
  assert.ok(!JSON.stringify(out).includes(SECRET));
});

test('a bearer- or basic-shaped value is redacted even under an innocent key', () => {
  assert.equal(redact({ note: 'Bearer ' + SECRET }).note, REDACTED);
  assert.equal(redact(['basic dXNlcjpwYXNz'])[0], REDACTED);
  assert.equal(redact({ note: 'bearer of news' }).note, REDACTED, 'the shape is matched, not the meaning: over-redaction is the safe side');
  assert.equal(redact({ note: 'the bearer' }).note, 'the bearer');
});

test('long strings keep a head, the dropped length and a hash; caps say what they cut', () => {
  const long = 'x'.repeat(1000);
  const s = redact(long);
  assert.ok(s.startsWith('x'.repeat(240) + '…[+760 chars sha256:'), s.slice(0, 260));
  assert.notEqual(redact('a'.repeat(1000)), redact('b'.repeat(1000)), 'two different long values stay distinguishable');
  const arr = redact(Array.from({ length: 70 }, (_, i) => i));
  assert.equal(arr.length, 61);
  assert.equal(arr.at(-1), '[+10 more items]');
  let deep = { v: 1 };
  for (let i = 0; i < 10; i++) deep = { deep };
  assert.ok(JSON.stringify(redact(deep)).includes('[object depth-capped]'));
  const wide = Object.fromEntries(Array.from({ length: 65 }, (_, i) => ['k' + i, i]));
  assert.equal(redact(wide)['[truncated]'], '+5 more keys');
  assert.equal(redact(undefined), null);
  assert.equal(redact(10n), '10');
});

test('redactUrl drops the origin and replaces credential query values only', () => {
  assert.equal(redactUrl('http://127.0.0.1:18823/api/prefs?key=mdai_root'), '/api/prefs?key=mdai_root');
  const u = redactUrl(`http://h/api/x?token=${SECRET}&session=nav-probe&api_key=${SECRET}`);
  assert.equal(u, `/api/x?token=${REDACTED}&session=nav-probe&api_key=${REDACTED}`);
  assert.ok(!u.includes(SECRET));
  assert.equal(redactUrl('not a url'), '[unparseable url]');
});

test('matchesPath: prefixes read the pathname, RegExps read path+query, and a global RegExp is reusable', () => {
  assert.equal(matchesPath('http://h/api/board/AMUX-1', ['/api/board']), true);
  assert.equal(matchesPath('http://h/api/sessions?x=1', ['/api/board']), false);
  assert.equal(matchesPath('http://h/app.js?v=/api/board', ['/api/board']), false, 'a prefix never matches the query');
  const g = /session=nav-probe/g;
  assert.equal(matchesPath('http://h/api/history?session=nav-probe', [g]), true);
  assert.equal(matchesPath('http://h/api/history?session=nav-probe', [g]), true, 'lastIndex from the previous test must not skip the second');
  assert.equal(matchesPath('::', ['/']), false);
});

test('options are validated at the boundary: an empty or malformed filter is refused, not silently empty', () => {
  assert.throws(() => normalizeOperandOptions({}), /paths must be a non-empty array/);
  assert.throws(() => normalizeOperandOptions({ paths: [] }), /paths must be a non-empty array/);
  assert.throws(() => normalizeOperandOptions({ paths: ['api/board'] }), /must be a RegExp or a pathname prefix/);
  assert.throws(() => normalizeOperandOptions({ paths: ['/a'], headers: true }), /option "headers" is not supported/);
  assert.throws(() => normalizeOperandOptions({ paths: ['/a'], jsonPaths: 'x' }), /jsonPaths must be a non-empty array/);
  for (const bad of ['a;b', 'fetch()', 'x[0]', '', '.a', 'a.']) {
    assert.throws(() => normalizeOperandOptions({ paths: ['/a'], globals: [bad] }), /is not a global name/, JSON.stringify(bad));
  }
  assert.throws(() => normalizeOperandOptions({ paths: ['/a'], dom: [' '] }), /is not a CSS selector/);
  assert.throws(() => normalizeOperandOptions({ paths: ['/a'], probe: 'x' }), /probe must be a function/);
  assert.throws(() => normalizeOperandOptions({ paths: ['/a'], label: 3 }), /label must be a string/);
  const o = normalizeOperandOptions({ paths: ['/a', /b/], globals: ['_mdaiCur.doc', 'navigator.onLine', '$x'], dom: ['#x'] });
  assert.deepEqual(o.globals, ['_mdaiCur.doc', 'navigator.onLine', '$x']);
  assert.deepEqual(o.jsonPaths, []);
  assert.equal(o.probe, null);
  assert.equal(o.label, null);
});

test('summarizeBody: size and hash always; redacted JSON only when asked and parseable', () => {
  assert.deepEqual(summarizeBody(null), { bytes: 0, sha256: null });
  const body = JSON.stringify({ path: '/t/W.mdai', doc: { title: 'Where I left off', fetch: { headers: { Authorization: 'Bearer ' + SECRET } } } });
  const plain = summarizeBody(body);
  assert.equal(plain.bytes, Buffer.byteLength(body));
  assert.match(plain.sha256, /^[0-9a-f]{16}$/);
  assert.equal('json' in plain, false);
  const j = summarizeBody(Buffer.from(body), { json: true });
  assert.equal(j.sha256, plain.sha256, 'Buffer and string of the same bytes hash the same');
  assert.equal(j.json.doc.title, 'Where I left off');
  assert.ok(!JSON.stringify(j).includes(SECRET));
  assert.equal(summarizeBody('<html>', { json: true }).json_skipped, 'not json');
  assert.equal(summarizeBody('x'.repeat(MAX_PARSE_BYTES + 1), { json: true }).json_skipped, `over ${MAX_PARSE_BYTES} bytes`);
  assert.notEqual(summarizeBody('{"a":1}').sha256, summarizeBody('{"a":2}').sha256);
});

test('capEntries keeps the newest and counts what it dropped', () => {
  const all = Array.from({ length: MAX_ENTRIES + 5 }, (_, i) => ({ i }));
  const { entries, dropped } = capEntries(all);
  assert.equal(dropped, 5);
  assert.equal(entries.length, MAX_ENTRIES);
  assert.equal(entries[0].i, 5);
  assert.deepEqual(capEntries([{ i: 1 }]), { entries: [{ i: 1 }], dropped: 0 });
});

const base = { label: 'peek-filter', test: 'spec > t', status: 'failed', expectedStatus: 'passed', listening: true,
  requestsSeen: 9, entries: [{ url: '/api/history' }], marks: [{ name: 'online', t_ms: 5 }],
  snapshots: [{ at: 'teardown', t_ms: 9 }], snapshotErrors: [] };

test('the report says whether it measured, and why not, beside its counts', () => {
  const ok = buildOperandsReport(base);
  assert.equal(ok.verdict, OPERANDS_VERDICT);
  assert.equal(ok.measured, true);
  assert.equal('why_unmeasured' in ok, false);
  assert.equal(ok.n_considered, 1);
  assert.equal(ok.n_requests_seen, 9);
  assert.deepEqual(ok.marks, base.marks);
  const noSnap = buildOperandsReport({ ...base, snapshots: [], snapshotErrors: ['teardown: page closed'] });
  assert.equal(noSnap.measured, false);
  assert.match(noSnap.why_unmeasured, /no snapshot was read: teardown: page closed/);
  const deaf = buildOperandsReport({ ...base, listening: false });
  assert.equal(deaf.measured, false);
  assert.match(deaf.why_unmeasured, /network listeners were not installed/);
  const quiet = buildOperandsReport({ ...base, entries: [], requestsSeen: 0 });
  assert.equal(quiet.measured, true, 'zero matched requests with listeners on is a measured zero');
  assert.equal(quiet.n_considered, 0);
});

test('the console line is one sweepable line with counts and no payload', () => {
  const report = buildOperandsReport({ ...base, entries: [{ url: '/api/x', request_body: { json: { title: 'PAYLOAD-MARK' } } }] });
  const line = operandsLine(report, true);
  assert.equal(line.includes('\n'), false);
  assert.ok(line.startsWith(OPERANDS_VERDICT + ' {'));
  const parsed = JSON.parse(line.slice(OPERANDS_VERDICT.length + 1));
  assert.deepEqual(
    { verdict: parsed.verdict, measured: parsed.measured, n_considered: parsed.n_considered, n_requests_seen: parsed.n_requests_seen, attached: parsed.attached, snapshots: parsed.snapshots },
    { verdict: OPERANDS_VERDICT, measured: true, n_considered: 1, n_requests_seen: 9, attached: true, snapshots: ['teardown'] });
  assert.equal(line.includes('PAYLOAD-MARK'), false);
  const unmeasured = JSON.parse(operandsLine(buildOperandsReport({ ...base, snapshots: [] }), false).slice(OPERANDS_VERDICT.length + 1));
  assert.equal(unmeasured.measured, false);
  assert.match(unmeasured.why_unmeasured, /no snapshot/);
});

test('the browser half never reads request headers or cookies, and runs every snapshot through redact', () => {
  const src = fs.readFileSync(new URL('./operands.ts', import.meta.url), 'utf8');
  for (const accessor of ['.headers(', '.allHeaders(', '.headersArray(', '.headerValue(', '.cookies(', 'storageState(']) {
    assert.equal(src.includes(accessor), false, `operands.ts calls ${accessor}`);
  }
  for (const field of ['globals', 'dom', 'probe']) {
    assert.match(src, new RegExp(`snap\\.${field} = redact\\(`), `snapshot ${field} is not redacted`);
  }
  // Attachments only on a test that did not pass as expected.
  assert.match(src, /const failed = testInfo\.status !== testInfo\.expectedStatus;\s*if \(failed\) \{\s*await testInfo\.attach\(/);
});
