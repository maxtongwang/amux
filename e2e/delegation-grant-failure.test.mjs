// Grant failure surfaces — preserve the B1 owner re-ask instruction in the real dashboard and CLI.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import vm from 'node:vm';
import { spawnSync } from 'node:child_process';

const source = fs.readFileSync('crates/amux-dashboard/static/app.js', 'utf8');
const start = source.indexOf('async function _delegationGrantsLoad()');
const end = source.indexOf('async function _delegationGrantDecide(', start);
assert.ok(start >= 0 && end > start, 'production grant renderer must exist');
const cli = fs.readFileSync('amux', 'utf8');
const cliStart = cli.indexOf('cmd_grant() {');
const cliEnd = cli.indexOf('cmd_approvals() {', cliStart);
assert.ok(cliStart >= 0 && cliEnd > cliStart, 'production grant CLI must exist');
assert.match(cli, /^\s+grant\)\s+shift; cmd_grant "\$@" ;;/m, 'amux must dispatch the grant command');
const reason = 'failed: re-ask the owner; any cited pair remains spent';
const failed = { code: 'ABCD', status: 'failed', sentence: 'A delegated change', failure_reason: reason, cite_spent_at: 1 };
const payload = { measured: true, n_considered: 1, mode: 'cite', grants: [failed] };
const esc = value => String(value).replaceAll('&', '&amp;').replaceAll('<', '&lt;').replaceAll('>', '&gt;').replaceAll('"', '&quot;').replaceAll("'", '&#39;');

async function render(grants) {
  const box = { innerHTML: '' };
  const ctx = vm.createContext({ API: '', esc, _authHeaders: () => ({}), document: { getElementById: () => box },
    fetch: async () => ({ ok: true, json: async () => ({ ...payload, grants }) }) });
  vm.runInContext(source.slice(start, end), ctx);
  await ctx._delegationGrantsLoad();
  return box.innerHTML;
}

function grantCli(args, response = payload) {
  return spawnSync('bash', ['-c', 'set -euo pipefail\n' + cli.slice(cliStart, cliEnd) + `
die() { printf '%s\\n' "$*" >&2; exit 1; }
_curl() { printf '%s\\n' "$*" >&2; printf '%s' "$GRANT_TEST_RESPONSE"; }
cmd_grant "$@"
`, 'grant-test', ...args], { encoding: 'utf8', env: { PATH: process.env.PATH, AMUX_API: 'http://example.invalid',
    AMUX_SESSION: 'test-lead', GRANT_TEST_RESPONSE: JSON.stringify(response) } });
}

test('failed grant is visible with recovery text and no retry or undo control', async () => {
  const html = await render([{ ...failed, failure_reason: reason + ' <fresh ask>' }]);
  assert.match(html, /ABCD · failed/);
  assert.ok(html.includes(esc(reason + ' <fresh ask>')));
  assert.ok(!html.includes('<fresh ask>'), 'API text must be escaped');
  assert.ok(!html.includes('<button'), 'failed grants cannot reuse their spent pair');
});

test('failed grants remain visible alongside pending and applied grants', async () => {
  const html = await render([{ code: 'PEND', status: 'pending' }, { code: 'DONE', status: 'applied' }, failed]);
  for (const text of ['PEND · pending', 'DONE · applied', 'ABCD · failed', 'Approve', 'Deny', 'Undo', reason]) {
    assert.ok(html.includes(text), `missing ${text}`);
  }
});

test('grant list prints failed status and owner recovery with the worker identity', () => {
  const out = grantCli(['list']);
  assert.equal(out.status, 0, out.stderr);
  assert.match(out.stdout, /ABCD  failed/);
  assert.ok(out.stdout.includes(reason));
  assert.ok(out.stderr.includes('X-Amux-Worker: test-lead'));
  assert.ok(out.stderr.includes('/api/delegation/grants'));
});

test('grant list JSON preserves structured status, reason and measurement', () => {
  const out = grantCli(['list', '--json']);
  assert.equal(out.status, 0, out.stderr);
  assert.deepEqual(JSON.parse(out.stdout), payload);
});

test('grant list refuses unreadable, refused and unmeasured responses', () => {
  for (const response of [null, { ok: false, error: 'reader refused' }, { grants: [], measured: false }]) {
    const out = grantCli(['list'], response);
    assert.notEqual(out.status, 0, out.stdout);
    assert.match(out.stderr, /amux grant list:/);
  }
  assert.notEqual(grantCli(['list', '--unknown']).status, 0);
});
