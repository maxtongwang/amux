// tests/lib/live-base.mjs refuses the live server unless asked — browser tests must not default to the owner's dashboard.
// Each case runs in a child node so its process.exit and env stay out of this runner.
import test from 'node:test';
import assert from 'node:assert/strict';
import {spawnSync} from 'node:child_process';
import path from 'node:path';

const lib = path.join(import.meta.dirname, 'lib', 'live-base.mjs');
function run(env) {
  const code = `const {testBase} = await import(${JSON.stringify(lib)}); console.log(testBase('AMUX_TEST_URL', 'https://localhost:8824'));`;
  const clean = Object.fromEntries(Object.entries(process.env).filter(([k]) => k !== 'AMUX_TEST_URL' && k !== 'AMUX_TEST_LIVE'));
  return spawnSync(process.execPath, ['--input-type=module', '-e', code], {env: {...clean, ...env}, encoding: 'utf8'});
}

test('no target and no opt-in: skip with exit 2, never the live URL', () => {
  const r = run({});
  assert.equal(r.status, 2, r.stderr);
  assert.equal(r.stdout, '');
  assert.match(r.stderr, /AMUX_TEST_LIVE=1/);
});

test('an explicit target wins', () => {
  const r = run({AMUX_TEST_URL: 'http://127.0.0.1:5'});
  assert.equal(r.status, 0, r.stderr);
  assert.equal(r.stdout.trim(), 'http://127.0.0.1:5');
});

test('the live default only on AMUX_TEST_LIVE=1', () => {
  const r = run({AMUX_TEST_LIVE: '1'});
  assert.equal(r.status, 0, r.stderr);
  assert.equal(r.stdout.trim(), 'https://localhost:8824');
});
