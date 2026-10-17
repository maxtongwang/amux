// Delegation WebAuthn browser adapter — exercise the shipped handlers with a deterministic credential device (refs maxtongwang/ai-config#160).
import { readFileSync } from 'node:fs';
import vm from 'node:vm';
import assert from 'node:assert/strict';
import test from 'node:test';

const source = readFileSync(new URL('../crates/amux-dashboard/static/app.js', import.meta.url), 'utf8');
function fixture(mode = 'passkey') {
  const calls = [], notices = [], ceremonies = [];
  const boxes = new Map(['delegation-passkeys-list', 'delegation-passkey-mode', 'delegation-grants-list', 'delegation-log-list'].map(id => [id, { innerHTML: '', textContent: '' }]));
  const device = async ({ publicKey }) => {
    assert.equal(publicKey.challenge.constructor.name, 'Uint8Array');
    assert.equal(publicKey.challenge[0], 251);
    for (const c of publicKey.allowCredentials || publicKey.excludeCredentials || []) assert.equal(c.id[0], 251);
    ceremonies.push(publicKey);
    return { id: '-w', rawId: Uint8Array.of(251).buffer, type: 'public-key', getClientExtensionResults: () => ({}),
      response: { clientDataJSON: Uint8Array.of(251).buffer, authenticatorData: Uint8Array.of(1).buffer,
        signature: Uint8Array.of(2).buffer, attestationObject: Uint8Array.of(3).buffer,
        getTransports: () => ['internal'] } };
  };
  const options = () => ({ publicKey: { challenge: '-w', userVerification: 'required', allowCredentials: [{ id: '-w' }] } });
  const context = vm.createContext({ Uint8Array, atob, btoa, window: { isSecureContext: true }, navigator: { credentials: { get: device, create: device } },
    API: '', _authHeaders: () => ({ Authorization: 'Bearer fixture' }), esc: s => String(s), showToast: s => notices.push(s), showConfirm: async () => true,
    document: { getElementById: id => boxes.get(id) }, timeAgo: () => 'now',
    fetch: async (path, init) => {
      const body = init.body && JSON.parse(init.body);
      calls.push({ path, body });
      let data = {};
      if (path === '/api/delegation/passkeys') data = { mode, credentials: [{ id: '-w', rp_id: 'amux.example' }] };
      if (path.endsWith('/authenticate/start')) data = { ceremony: 'challenge-id', options: options() };
      if (path.endsWith('/register/start')) {
        data = { ceremony: 'enroll-id', options: { publicKey: { challenge: '-w', user: { id: '-w' }, excludeCredentials: [{ id: '-w' }] } }, authorization: null };
      }
      if (path.endsWith('/register/finish')) assert.equal(body.credential.response.transports[0], 'internal');
      return { ok: true, json: async () => data };
    } });
  vm.runInContext(source.slice(source.indexOf('async function _delegationPasskeyRequest('), source.indexOf('// AMUX-5270: standing approvals')), context);
  return { context, calls, notices, ceremonies, boxes };
}

test('pending approval sends device assertion bound to grant; cite skips device', async () => {
  for (const mode of ['passkey', 'cite']) {
    const f = fixture(mode);
    await f.context._delegationGrantDecide('ABCD', 'approve');
    const start = f.calls.find(c => c.path.endsWith('/authenticate/start'));
    const finish = f.calls.find(c => c.path.endsWith('/ABCD/approve'));
    assert.ok(finish, 'approval must be sent');
    if (mode === 'passkey') {
      assert.equal(start.body.purpose, 'grant:ABCD');
      assert.equal(finish.body.ceremony, 'challenge-id');
      assert.equal(finish.body.credential.rawId, '-w');
      assert.equal(finish.body.credential.response.signature, 'Ag');
      assert.equal(f.ceremonies.length, 1);
    } else {
      assert.equal(start, undefined);
      assert.equal(f.ceremonies.length, 0);
    }
  }
});
test('registration converts binary fields and refreshes visible credentials', async () => {
  const f = fixture();
  await f.context._delegationPasskeyRegister();
  const finish = f.calls.find(c => c.path.endsWith('/register/finish'));
  assert.equal(finish.body.ceremony, 'enroll-id');
  assert.equal(finish.body.credential.response.attestationObject, 'Aw');
  assert.ok(f.boxes.get('delegation-passkeys-list').innerHTML.includes('Revoke'));
  assert.ok(f.notices.includes('Passkey registered'));
});
test('revoke binds authentication to credential and secure-context failure surfaces', async () => {
  const f = fixture();
  await f.context._delegationPasskeyRevoke('-w');
  assert.equal(f.calls.find(c => c.path.endsWith('/authenticate/start')).body.purpose, 'revoke:-w');
  assert.equal(f.calls.find(c => c.path.endsWith('/-w/revoke')).body.ceremony, 'challenge-id');
  f.context.window.isSecureContext = false;
  await f.context._delegationPasskeyRegister();
  assert.ok(f.notices.some(s => s.includes('HTTPS')));
});
