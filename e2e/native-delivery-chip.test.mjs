// Native receipt labels — acceptance, queue admission and consumption stay distinct.
import { readFileSync } from 'node:fs';
import assert from 'node:assert/strict';
import { test } from 'node:test';

const source = readFileSync(new URL('../crates/amux-dashboard/static/app.js', import.meta.url), 'utf8');
function extract(name) {
  const start = source.indexOf('function ' + name + '(');
  assert.ok(start >= 0, name);
  let depth = 0;
  for (let i = source.indexOf('{', start); i < source.length; i++) {
    if (source[i] === '{') depth++;
    if (source[i] === '}' && --depth === 0) return source.slice(start, i + 1);
  }
  assert.fail('function must be complete');
}
const chip = new Function('_fmtDur', '_msgQueued', extract('_msgDeliveryChip') + '\nreturn _msgDeliveryChip;')(String, () => false);
const normalize = new Function(extract('_msgNorm') + '\nreturn _msgNorm;')();
const labels = {
  reserved:'native reserved', written_unconfirmed:'native unconfirmed', held:'held by Claude',
  admitted:'admitted to Claude queue', consumed:'received by Claude', denied:'denied by Claude',
  refused:'refused by Claude', expired:'expired in Claude', dropped:'dropped by Claude', unknown:'native delivery unknown',
};

for (const [state, label] of Object.entries(labels)) {
  test('Messages retains and displays native ' + state, () => {
    const receipt = {transport:'claude_uds_v1', state, attempt_id:'fixture', reason:'fixture'};
    const row = normalize({id:42, session:'fixture', delivery:'queued', delivered_at:null, delivery_receipt:receipt});
    assert.deepEqual(row.delivery_receipt, receipt);
    const html = chip(row);
    assert.equal(html.replace(/<[^>]+>/g, ''), label);
    if (!['admitted', 'consumed'].includes(state)) assert.ok(html.includes('No automatic resend or paste'));
    if (state === 'admitted') assert.ok(html.includes('does not prove model consumption'));
  });
}
test('native states override a stale legacy delivered timestamp', () => {
  const html = chip({delivery:'direct', delivered_at:1234, delivery_receipt:{transport:'claude_uds_v1', state:'written_unconfirmed'}});
  assert.ok(html.includes('native unconfirmed'));
  assert.ok(!html.includes('>direct<'));
});
