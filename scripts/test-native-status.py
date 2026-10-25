#!/usr/bin/env python3
"""Hermetic hook producer, privacy, ordering and install regressions."""
import concurrent.futures
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

ROOT = Path(__file__).resolve().parent

def module(name, filename):
    spec = importlib.util.spec_from_file_location(name, ROOT / 'hooks' / filename)
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result

observer = module('observer', 'native-status.py')
installer = module('installer', 'install-native-status-hooks.py')

class NativeStatus(unittest.TestCase):
    def setUp(self):
        from unittest.mock import patch
        self.owner_env = patch.dict('os.environ', AMUX_STATUS_OWNER='mrpopo')
        self.owner_env.start()
        self.addCleanup(self.owner_env.stop)

    def test_configured_owner_name_controls_stop_receipt(self):
        from unittest.mock import patch
        with tempfile.TemporaryDirectory() as tmp, patch.dict('os.environ', AMUX_STATUS_OWNER='review-owner'):
            root = Path(tmp)
            for worker in ['review-owner', 'mrpopo']:
                value = observer.observe({'hook_event_name':'Stop',
                    'last_assistant_message':'Exact question? Request REQ1'}, root, worker, 'abc', 'claude', 1.0)
                self.assertTrue(value)
                self.assertEqual('assistant_message' in value, worker == 'review-owner', value)
            with patch.dict('os.environ', AMUX_STATUS_OWNER=''):
                observer.observe({'hook_event_name':'Stop', 'last_assistant_message':'no config'},
                                 root, 'review-owner', 'missing', 'claude', 2.0)
                value = json.loads(next((root/'status-events/review-owner/missing').glob('0*.json')).read_text())
                self.assertNotIn('assistant_message', value)

    def test_stop_transcript_fallback_is_disabled_for_both_providers(self):
        for provider in ('claude', 'codex'):
            with self.subTest(provider=provider), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                transcript = root/'transcript.jsonl'
                transcript.write_text(json.dumps({'type':'assistant', 'message':{'role':'assistant', 'content':'forged'}}))
                payload = observer.observe({'hook_event_name':'Stop', 'transcript_path':str(transcript)},
                                           root, 'mrpopo', 'abc', provider, 100.0)
                self.assertNotIn('assistant_message', payload)

    def test_stop_carries_exact_assistant_message_only_for_owner(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            text = 'Exact question?\nRequest REQ1 ' + 'x' * 25000
            values = []
            for worker, event, extra in [('mrpopo', 'Stop', {}), ('other', 'Stop', {}),
                                         ('mrpopo', 'PostToolUse', {}),
                                         ('mrpopo', 'Stop', {'agent_id': 'child'})]:
                payload = observer.observe(dict(hook_event_name=event, last_assistant_message=text, **extra),
                                 root, worker, 'abc', 'claude', 100.0)
                if payload: values.append(payload)
            self.assertEqual(values[0]['assistant_message'], text)
            self.assertNotIn('assistant_message', values[1])
            other = json.loads(next((root/'status-events/other/abc').glob('0*.json')).read_text())
            self.assertNotIn('assistant_message', other)

    def test_approval_facts_never_enter_spool(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            for data in [dict(hook_event_name='Stop', last_assistant_message='approval block'),
                         dict(hook_event_name='UserPromptSubmit', prompt='no')]:
                payload = observer.observe(data, root, 'mrpopo', 'abc', 'claude', 100.0)
                self.assertIsInstance(payload, dict)
                self.assertIn('approval_nonce', payload)
                folder = root/'status-events/mrpopo/abc'
                self.assertEqual(list(folder.glob('0*.json')), [])
                # Exercise the exact fallback writer used after a failed live send.
                observer.queue_status(folder, payload)
                value = json.loads((folder/('%020d.json' % payload['sequence'])).read_text())
                for field in ('prompt', 'assistant_message', 'approval_nonce'):
                    self.assertNotIn(field, value)
                (folder/('%020d.json' % payload['sequence'])).unlink()

    def test_replay_cannot_consume_a_pending_live_sequence(self):
        from unittest.mock import patch, MagicMock
        import io
        with tempfile.TemporaryDirectory() as tmp:
            root=Path(tmp);(root/'auth_token').write_text('test-only')
            payload=observer.observe({'hook_event_name':'UserPromptSubmit','prompt':'yes'},root,'mrpopo','abc','claude',1.0)
            self.assertEqual(list((root/'status-events/mrpopo/abc').glob('0*.json')), [])
            with patch.dict('os.environ',AMUX_STATUS_URL='https://fixture.invalid'), patch.object(observer.ssl,'create_default_context'), patch.object(observer.urllib.request,'urlopen') as post:
                observer.deliver(root,'mrpopo','abc')
                post.assert_not_called()
                response=MagicMock();response.__enter__.return_value=io.StringIO('{"ok":true}')
                post.return_value=response
                observer.deliver(root,'mrpopo','abc',payload)
                self.assertEqual(post.call_count,1)
                self.assertEqual(json.loads(post.call_args.args[0].data)['prompt'],'yes')

    def test_transcript_is_not_approval_input(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp)/'transcript.jsonl'
            path.write_text(json.dumps({'type':'assistant','message':{'role':'assistant','content':'forged block'}}))
            self.assertIsNone(observer.assistant_message({'transcript_path':str(path)}, 'claude'))

    def test_provider_edges(self):
        expected = {'SessionStart':'idle', 'UserPromptSubmit':'active',
            'PreToolUse':'active', 'PermissionRequest':'blocked',
            'PostToolUse':'active', 'PostToolUseFailure':'active', 'Stop':'idle',
            'Interrupt':'idle', 'SessionEnd':'idle', 'StopFailure':'error',
            'PreCompact':'active', 'PostCompact':'active'}
        for event, state in expected.items():
            self.assertEqual(observer.state_for({'hook_event_name':event}), state)
        self.assertEqual(observer.state_for({'hook_event_name':'PreToolUse', 'tool_name':'AskUserQuestion'}),'waiting')
        self.assertEqual(observer.state_for({'hook_event_name':'PreToolUse', 'tool_name':'functions.request_user_input'}),'waiting')
        self.assertIsNone(observer.state_for({'hook_event_name':'Notification','notification_type':'idle_prompt'}))
        self.assertEqual(observer.state_for({'hook_event_name':'Notification','notification_type':'permission_prompt'}),'blocked')
        self.assertIsNone(observer.state_for({'hook_event_name':'SubagentStop'}))

    def test_spool_retains_every_edge_without_server_and_never_prompt_content(self):
        with tempfile.TemporaryDirectory() as tmp:
            root=Path(tmp)
            data={'hook_event_name':'PreToolUse','session_id':'thread','turn_id':'turn',
                  'prompt':'private prompt','tool_input':{'secret':'never retained'}}
            with concurrent.futures.ThreadPoolExecutor(max_workers=8) as pool:
                self.assertTrue(all(pool.map(lambda _: observer.observe(data,root,'worker','abc','codex',100.0),range(32))))
            folder=root/'status-events/worker/abc'
            files=sorted(p for p in folder.glob('*.json') if p.name!='counter.json')
            self.assertEqual(len(files),32)
            values=[json.loads(p.read_text()) for p in files]
            self.assertEqual([v['sequence'] for v in values],list(range(1,33)))
            self.assertNotIn('private prompt',json.dumps(values))
            self.assertNotIn('secret',json.dumps(values))
            self.assertTrue(all(v['event_ts']==100 for v in values))
            self.assertFalse(observer.observe(dict(data,agent_id='child'),root,'worker','abc','codex',101))

    def test_only_user_prompt_submit_carries_the_prompt_bounded(self):
        with tempfile.TemporaryDirectory() as tmp:
            root=Path(tmp)
            value=observer.observe({'hook_event_name':'UserPromptSubmit','prompt':'x'*25000},root,'worker','abc','claude',1.0)
            edge=observer.observe({'hook_event_name':'PostToolUse','prompt':'private prompt'},root,'worker','abc','claude',2.0)
            self.assertEqual(len(value['prompt']),20000)
            self.assertTrue(value['prompt_truncated'])
            self.assertNotIn('prompt',edge)
            paths=list((root/'status-events/worker/abc').glob('0*.json'))
            self.assertEqual(len(paths), 1)
            self.assertNotIn('prompt', json.loads(paths[0].read_text()))

    def test_owner_submit_preserves_session_proof_and_reports_truncation(self):
        for session in ['terminal-session', 'remote-control-session']:
            with self.subTest(session=session), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                values = [observer.observe({'hook_event_name':'UserPromptSubmit',
                          'session_id':session, 'prompt':prompt}, root, 'mrpopo', 'abc', 'claude', 1.0)
                          for prompt in ['no', 'yes' + ' ' * 20000 + 'do not proceed']]
                self.assertEqual(values[0]['prompt'],'no')
                self.assertFalse(values[0]['prompt_truncated'])
                self.assertTrue(values[1]['prompt_truncated'])
                self.assertTrue(all(v['session_id']==session and v['run_id']=='abc' for v in values))
                self.assertNotEqual(values[0]['approval_nonce'], values[1]['approval_nonce'])

    def test_question_notification_preserves_waiting_but_real_permission_blocks(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            for i, data in enumerate([
                {'hook_event_name':'PreToolUse','tool_name':'AskUserQuestion'},
                {'hook_event_name':'PermissionRequest','tool_name':'AskUserQuestion'},
                {'hook_event_name':'Notification','notification_type':'permission_prompt'},
                {'hook_event_name':'PostToolUse'},
                {'hook_event_name':'PermissionRequest','tool_name':'Bash'},
            ], 1):
                observer.observe(data, root, 'worker', 'abc', 'claude', float(i))
            values=[json.loads(p.read_text())['state'] for p in sorted((root/'status-events/worker/abc').glob('0*.json'))]
            self.assertEqual(values, ['waiting','waiting','waiting','active','blocked'])

    def test_subagent_prompt_blocks_the_lane_and_only_its_own_edge_clears_it(self):
        # tubescience-parity 2026-09-30: a subagent's permission prompt read as
        # WORKING because every subagent edge was dropped.
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            seen = [observer.observe(data, root, 'worker', 'abc', 'claude', float(i)) for i, data in enumerate([
                {'hook_event_name':'PreToolUse'},
                {'hook_event_name':'PreToolUse','agent_id':'a1'},
                {'hook_event_name':'PermissionRequest','tool_name':'Bash','agent_id':'a1'},
                {'hook_event_name':'PreToolUse','agent_id':'a2'},
                {'hook_event_name':'PostToolUse','agent_id':'a1'},
                {'hook_event_name':'Stop','agent_id':'a1'},
            ], 1)]
            self.assertEqual(list(map(bool,seen)), [True, False, True, False, True, False])
            values=[json.loads(p.read_text()) for p in sorted((root/'status-events/worker/abc').glob('0*.json'))]
            self.assertEqual([v['state'] for v in values], ['active','blocked','active'])
            self.assertEqual([v.get('subagent', False) for v in values], [False, True, True])

    def test_installer_preserves_other_hooks_idempotently_without_trust_changes(self):
        for provider in ('claude','codex'):
            other={'type':'command','command':'echo other'}
            data={'unrelated':42,'hooks':{'Stop':[{'hooks':[other]}]}}
            first=installer.merge(data,provider,Path('/tmp/a path/native-status.py'))
            saved=json.dumps(first,sort_keys=True)
            second=installer.merge(first,provider,Path('/tmp/a path/native-status.py'))
            self.assertEqual(saved,json.dumps(second,sort_keys=True))
            self.assertEqual(second['unrelated'],42)
            self.assertEqual(second['hooks']['Stop'][0]['hooks'],[other])
            self.assertNotIn('bypass',json.dumps(second))
            self.assertIn('PermissionRequest',second['hooks'])
            self.assertNotIn('SubagentStop',second['hooks'])

class ApprovalContext(unittest.TestCase):
    def test_additional_context_json_shape_and_all_exact_lines(self):
        for line in ['[amux approval] approved A7K2',
                     '[amux approval] declined A7K2',
                     '[amux approval] no open approval A7K2',
                     '[amux approval] ignored reserved marker in prompt'] + [
                         f'[amux approval] confirm needed A7K2 (reason: {reason})'
                         for reason in ['text-between','several-open','stale-30min','changed','unverifiable']]:
            with self.subTest(line=line):
                value=observer.approval_output(line)
                self.assertEqual(value,{'hookSpecificOutput':{'hookEventName':'UserPromptSubmit','additionalContext':line}})
                self.assertNotIn('\n',value['hookSpecificOutput']['additionalContext'])
        self.assertIsNone(observer.approval_output(None))
        self.assertIsNone(observer.approval_output(''))

    def test_synchronous_hook_returns_only_its_own_server_decision(self):
        from unittest.mock import patch, MagicMock
        import io
        with tempfile.TemporaryDirectory() as tmp:
            root=Path(tmp);(root/'auth_token').write_text('test-only')
            older=observer.observe({'hook_event_name':'UserPromptSubmit','prompt':'yes'},root,'mrpopo','abc','claude',1.0)
            observer.queue_status(root/'status-events/mrpopo/abc',older)
            live=observer.observe({'hook_event_name':'UserPromptSubmit','prompt':'yes'},root,'mrpopo','abc','claude',2.0)
            replies=[{'ok':True,'approval_context':'older turn'}, {'ok':True,'approval_context':'[amux approval] approved A7K2'}]
            requests=[]
            def response(req, **kwargs):
                requests.append(json.loads(req.data))
                result=MagicMock();result.__enter__.return_value=io.StringIO(json.dumps(replies.pop(0)))
                return result
            with patch.dict('os.environ',AMUX_STATUS_URL='https://test.invalid'), patch.object(observer.ssl,'create_default_context'), patch.object(observer.urllib.request,'urlopen',side_effect=response):
                self.assertEqual(observer.deliver(root,'mrpopo','abc',live),'[amux approval] approved A7K2')
            self.assertEqual([r['approval_live'] for r in requests],[False,True])
            self.assertEqual(list((root/'status-events/mrpopo/abc').glob('0*.json')),[])

    def test_unreachable_server_emits_no_line_and_retains_nonfunding_spool(self):
        import os, subprocess, sys, ssl
        with tempfile.TemporaryDirectory() as tmp:
            root=Path(tmp);(root/'auth_token').write_text('test-only');(root/'tls').mkdir()
            # Use a public CA bundle; the endpoint is a deliberately closed loopback port.
            import shutil
            shutil.copyfile(ssl.get_default_verify_paths().cafile,root/'tls/cert.pem')
            env=dict(os.environ,AMUX_STATUS_HOME=tmp,AMUX_STATUS_WORKER='mrpopo',AMUX_STATUS_OWNER='mrpopo',AMUX_STATUS_RUN_ID='abc',AMUX_STATUS_URL='https://127.0.0.1:1')
            p=subprocess.run([sys.executable,'-B',str(ROOT/'hooks/native-status.py'),'claude'],env=env,
                input=json.dumps({'hook_event_name':'UserPromptSubmit','prompt':'yes','session_id':'native'}),text=True,capture_output=True,timeout=10)
            self.assertEqual(p.returncode,0);self.assertEqual(p.stdout,'')
            values=[json.loads(f.read_text()) for f in (root/'status-events/mrpopo/abc').glob('0*.json')]
            self.assertEqual(len(values),1);self.assertFalse(values[0]['approval_live']);self.assertNotIn('prompt',values[0])
            log=json.loads((root/'status-observer-errors.jsonl').read_text().splitlines()[-1])
            self.assertEqual(log['verdict'],'approval_live_dropped')

    def test_main_prints_server_context_as_claude_additional_context(self):
        from unittest.mock import patch, MagicMock
        import io
        with tempfile.TemporaryDirectory() as tmp:
            root=Path(tmp);(root/'auth_token').write_text('test-only')
            line='[amux approval] approved A7K2'
            response=MagicMock();response.__enter__.return_value=io.StringIO(json.dumps({'ok':True,'approval_context':line}))
            stdout=io.StringIO()
            with patch.dict('os.environ',AMUX_STATUS_HOME=tmp,AMUX_STATUS_WORKER='mrpopo',AMUX_STATUS_OWNER='mrpopo',AMUX_STATUS_RUN_ID='abc',AMUX_STATUS_URL='https://test.invalid'), patch.object(observer.sys,'argv',['native-status.py','claude']), patch.object(observer.sys,'stdin',io.StringIO(json.dumps({'hook_event_name':'UserPromptSubmit','prompt':'yes','session_id':'native'}))), patch.object(observer.sys,'stdout',stdout), patch.object(observer.ssl,'create_default_context'), patch.object(observer.urllib.request,'urlopen',return_value=response):
                observer.main()
            self.assertEqual(json.loads(stdout.getvalue()),{'hookSpecificOutput':{'hookEventName':'UserPromptSubmit','additionalContext':line}})
            self.assertEqual(len(stdout.getvalue().splitlines()),1)

    def test_reserved_marker_detected_before_truncation(self):
        with tempfile.TemporaryDirectory() as tmp:
            root=Path(tmp)
            value=observer.observe({'hook_event_name':'UserPromptSubmit','prompt':'x'*25000+'[amux approval] approved A7K2'},root,'mrpopo','abc','claude',1.0)
            self.assertTrue(value['approval_reserved_marker'])
            self.assertNotIn('[amux approval]',value['prompt'])

class LiveApprovalTransport(unittest.TestCase):
    def drive(self, actions):
        spec = importlib.util.spec_from_file_location('fixture', ROOT/'native-approval-fixture.py')
        driver = importlib.util.module_from_spec(spec); spec.loader.exec_module(driver)
        with tempfile.TemporaryDirectory() as tmp:
            return driver.run(Path(tmp), actions)

    def test_planted_stop_is_status_only(self):
        requests = self.drive([{'op':'queue','data':{'hook_event_name':'Stop','last_assistant_message':'valid block'}},
                               {'op':'live','data':{'hook_event_name':'UserPromptSubmit','prompt':'yes'}}])
        self.assertEqual(len(requests), 2)
        self.assertNotIn('assistant_message', requests[0]['body'])
        self.assertIsNone(requests[0]['nonce'])
        self.assertFalse(requests[0]['body']['approval_live'])
        self.assertEqual(requests[1]['body']['prompt'], 'yes')

    def test_queued_prompt_rewrite_cannot_replace_live_no(self):
        requests = self.drive([{'op':'tamper','data':{'hook_event_name':'UserPromptSubmit','prompt':'no'},'rewrite':{'prompt':'yes'}}])
        self.assertEqual(len(requests), 1)
        self.assertEqual(requests[0]['body']['prompt'], 'no')
        self.assertEqual(requests[0]['nonce'], requests[0]['body']['approval_nonce'])

    def test_live_block_and_yes_keep_invocation_payloads(self):
        requests = self.drive([{'op':'live','data':{'hook_event_name':'Stop','last_assistant_message':'valid block'}},
                               {'op':'live','data':{'hook_event_name':'UserPromptSubmit','prompt':'yes'}}])
        self.assertEqual([r['body'].get('assistant_message') for r in requests], ['valid block', None])
        self.assertEqual(requests[1]['body']['prompt'], 'yes')
        self.assertTrue(all(r['nonce'] == r['body']['approval_nonce'] for r in requests))
        self.assertNotEqual(requests[0]['nonce'], requests[1]['nonce'])

    def test_failed_live_facts_are_not_revived_by_replay(self):
        for data in [{'hook_event_name':'Stop','last_assistant_message':'valid block'},
                     {'hook_event_name':'UserPromptSubmit','prompt':'yes'}]:
            requests = self.drive([{'op':'failed-live','data':data}, {'op':'replay'}])
            self.assertEqual(len(requests), 1)
            self.assertFalse(requests[0]['body']['approval_live'])
            for field in ('prompt', 'assistant_message', 'approval_nonce'):
                self.assertNotIn(field, requests[0]['body'])
            self.assertIsNone(requests[0]['nonce'])

if __name__=='__main__': unittest.main()
