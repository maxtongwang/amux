// Approval display receipts — exercise the production visibility observer, never a history read.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import vm from 'node:vm';

const source = readFileSync('crates/amux-dashboard/static/app.js', 'utf8');
const start = source.indexOf('const _approvalRenderTickets =');
const end = source.indexOf('function _cmdHistItemHTML(', start);
assert.ok(start >= 0 && end > start);

function fixture({visibility = 'visible', collapsed = false, connected = true, failed = false} = {}) {
  const calls = [], notices = [], observers = [];
  const document = {visibilityState:visibility, addEventListener() {}};
  const row = {dataset:{approvalAsk:'17', approvalAfter:'23'}, isConnected:connected,
    getClientRects:() => [1], querySelector:() => ({textContent:'Exact change', classList:{contains:() => collapsed}})};
  const list = {isConnected:true, querySelectorAll:() => [row], addEventListener() {}};
  class IntersectionObserver {
    constructor(callback, options) { this.callback = callback; this.options = options; observers.push(this); }
    observe() {} unobserve() {} disconnect() {}
  }
  const context = vm.createContext({document, IntersectionObserver, API:'',
    _authHeaders:() => ({Authorization:'owner-test'}), showToast:t => notices.push(t),
    fetch:async (url, options) => { calls.push({url,...options}); return {ok:!failed,json:async () => ({ok:!failed,code:'request_display_ineligible'})}; }});
  vm.runInContext(source.slice(start, end), context);
  context._approvalObserve(list);
  const notify = (ratio = 1) => observers.at(-1).callback([{target:row,isIntersecting:ratio>0,intersectionRatio:ratio}]);
  const flush = () => vm.runInContext('_approvalDisplayQueue',context);
  return {calls,notices,notify,flush,observers,document,row,context,list};
}

test('visible expanded owner question records exact text and ledger watermark once', async () => {
  const f=fixture(); f.notify(); await f.flush(); f.notify(); await f.flush();
  assert.equal(f.calls.length,1);
  assert.equal(f.calls[0].url,'/api/delegation/questions/17/display');
  assert.equal(f.calls[0].method,'POST');
  assert.deepEqual(JSON.parse(f.calls[0].body),{surface:'owner-messages',text:'Exact change',after_row:23});
  assert.equal(f.observers[0].options.threshold,1);
});

test('hidden collapsed partial and unmounted questions never record display', async () => {
  for (const options of [{visibility:'hidden'},{collapsed:true},{connected:false},{}]) {
    const f=fixture(options); f.notify(Object.keys(options).length ? 1 : 0.1); await f.flush();
    assert.equal(f.calls.length,0,JSON.stringify(options));
  }
});

test('display refusal surfaces its code and never becomes silent success', async () => {
  const f=fixture({failed:true}); f.notify(); await f.flush();
  assert.equal(f.calls.length,1);
  assert.deepEqual(f.notices,['Approval display refused: request_display_ineligible']);
});

test('receipt is wired only after owner Messages DOM insertion', () => {
  const render=source.slice(source.indexOf('function _peekMessagesRender()'),source.indexOf('function _peekMsgPageCount()'));
  assert.ok(render.includes('_approvalRender(list, (_body || _placeholder)'));
  const guarded=source.slice(source.indexOf('async function _approvalRender('),source.indexOf('const _approvalDisplaySeen ='));
  assert.ok(guarded.indexOf('list.innerHTML = html;',guarded.indexOf('await r.json()')) > guarded.indexOf('await r.json()'));
  assert.ok(guarded.indexOf('_approvalObserve(list)') > guarded.indexOf('await r.json()'));
  assert.match(source,/data-approval-ask="\$\{Number\(e.id\)\}"/);
});


test('preparation failure never puts a new approval question on screen', async () => {
  const f=fixture({failed:true}); f.list.innerHTML='previous view';
  await f.context._approvalRender(f.list,'<div data-approval-ask="17">Exact change</div>');
  assert.equal(f.calls[0].url,'/api/delegation/questions/render');
  assert.equal(f.list.innerHTML,'previous view');
  assert.deepEqual(f.notices,['Approval display refused: request_display_ineligible']);
});


test('surfaces without a render hook refuse to show an approval question', () => {
  const f=fixture();
  const question={delivery:'approval_request',text:'Reply yes for SECRET-OPERAND'};
  assert.equal(f.context._approvalQuestionText(question,true),question.text);
  const refused=f.context._approvalQuestionText(question,false);
  assert.match(refused,/request_display_surface_unsupported/);
  assert.ok(!refused.includes('SECRET-OPERAND'));
  assert.equal(f.context._approvalQuestionText({text:'ordinary message'},false),'ordinary message');
});
