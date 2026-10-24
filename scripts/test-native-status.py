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
                self.assertTrue(observer.observe({'hook_event_name':'Stop',
                    'last_assistant_message':'Exact question? Request REQ1'}, root, worker, 'abc', 'claude', 1.0))
                value = json.loads(next((root/'status-events'/worker/'abc').glob('0*.json')).read_text())
                self.assertEqual('assistant_message' in value, worker == 'review-owner', value)
            with patch.dict('os.environ', AMUX_STATUS_OWNER=''):
                observer.observe({'hook_event_name':'Stop', 'last_assistant_message':'no config'},
                                 root, 'review-owner', 'missing', 'claude', 2.0)
                value = json.loads(next((root/'status-events/review-owner/missing').glob('0*.json')).read_text())
                self.assertNotIn('assistant_message', value)

    def test_stop_transcript_fallback_uses_only_complete_latest_assistant_text(self):
        for provider, records in [
            ('claude', [{'type':'assistant', 'message':{'role':'assistant', 'content':[
                {'type':'thinking', 'thinking':'private reasoning'},
                {'type':'text', 'text':'Exact question? Request REQ1'}]}}]),
            ('codex', [{'type':'response_item', 'payload':{'type':'message', 'role':'assistant',
                'content':[{'type':'output_text', 'text':'Exact question? Request REQ1'}]}}]),
        ]:
            with self.subTest(provider=provider), tempfile.TemporaryDirectory() as tmp:
                root=Path(tmp); transcript=root/'transcript.jsonl'
                transcript.write_text('\n'.join(map(json.dumps,records))+'\n')
                data={'hook_event_name':'Stop', 'transcript_path':str(transcript)}
                self.assertTrue(observer.observe(data,root,'mrpopo','abc',provider,100.0))
                value=json.loads(next((root/'status-events/mrpopo/abc').glob('0*.json')).read_text())
                self.assertEqual(value['assistant_message'],'Exact question? Request REQ1')
                self.assertNotIn('private reasoning',json.dumps(value))
                if provider == 'claude':
                    with transcript.open('a') as out:
                        out.write(json.dumps({'type':'assistant','message':{'role':'assistant',
                                  'content':[{'type':'tool_use','name':'example'}]}})+'\n')
                    self.assertIsNone(observer.assistant_message(data,provider))
                with transcript.open('a') as out:
                    out.write(json.dumps({'type':'user','message':{'role':'user','content':'new input'}})
                              if provider=='claude' else json.dumps({'type':'event_msg','payload':{'type':'user_message','message':'new input'}}))
                self.assertIsNone(observer.assistant_message(data,provider))
                transcript.write_text('{"partial":')
                self.assertIsNone(observer.assistant_message(data,provider))

    def test_stop_carries_exact_assistant_message_only_for_owner(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            text = 'Exact question?\nRequest REQ1 ' + 'x' * 25000
            for worker, event, extra in [('mrpopo', 'Stop', {}), ('other', 'Stop', {}),
                                         ('mrpopo', 'PostToolUse', {}),
                                         ('mrpopo', 'Stop', {'agent_id': 'child'})]:
                observer.observe(dict(hook_event_name=event, last_assistant_message=text, **extra),
                                 root, worker, 'abc', 'claude', 100.0)
            values = [json.loads(p.read_text()) for p in sorted((root/'status-events/mrpopo/abc').glob('0*.json'))]
            self.assertEqual(values[0]['assistant_message'], text)
            self.assertNotIn('assistant_message', values[1])
            other = json.loads(next((root/'status-events/other/abc').glob('0*.json')).read_text())
            self.assertNotIn('assistant_message', other)

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
        # F8(c): the server records a prompt typed into the pane, so the submit
        # edge carries its text. No other edge does.
        with tempfile.TemporaryDirectory() as tmp:
            root=Path(tmp)
            observer.observe({'hook_event_name':'UserPromptSubmit','prompt':'x'*25000},root,'worker','abc','claude',1.0)
            observer.observe({'hook_event_name':'PostToolUse','prompt':'private prompt'},root,'worker','abc','claude',2.0)
            values=[json.loads(p.read_text()) for p in sorted((root/'status-events/worker/abc').glob('0*.json'))]
            self.assertEqual(len(values[0]['prompt']),20000)
            self.assertTrue(values[0]['prompt_truncated'])
            self.assertNotIn('prompt',values[1])

    def test_owner_submit_preserves_session_proof_and_reports_truncation(self):
        for session in ['terminal-session', 'remote-control-session']:
            with self.subTest(session=session), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                for prompt in ['no', 'yes' + ' ' * 20000 + 'do not proceed']:
                    self.assertTrue(observer.observe({'hook_event_name':'UserPromptSubmit',
                        'session_id':session, 'prompt':prompt}, root, 'mrpopo', 'abc', 'claude', 1.0))
                values=[json.loads(p.read_text()) for p in sorted((root/'status-events/mrpopo/abc').glob('0*.json'))]
                self.assertEqual(len(values),2)
                self.assertEqual(values[0]['prompt'],'no')
                self.assertFalse(values[0]['prompt_truncated'])
                self.assertTrue(values[1]['prompt_truncated'])
                self.assertTrue(all(v['session_id']==session and v['run_id']=='abc' for v in values))

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
            observer.observe({'hook_event_name':'UserPromptSubmit','prompt':'yes'},root,'mrpopo','abc','claude',1.0)
            observer.observe({'hook_event_name':'UserPromptSubmit','prompt':'yes'},root,'mrpopo','abc','claude',2.0)
            replies=[{'ok':True,'approval_context':'older turn'}, {'ok':True,'approval_context':'[amux approval] approved A7K2'}]
            requests=[]
            def response(req, **kwargs):
                requests.append(json.loads(req.data))
                result=MagicMock();result.__enter__.return_value=io.StringIO(json.dumps(replies.pop(0)))
                return result
            with patch.dict('os.environ',AMUX_STATUS_URL='https://test.invalid'), patch.object(observer.ssl,'create_default_context'), patch.object(observer.urllib.request,'urlopen',side_effect=response):
                self.assertEqual(observer.deliver(root,'mrpopo','abc',2),'[amux approval] approved A7K2')
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
            self.assertEqual(len(values),1);self.assertNotIn('approval_live',values[0])

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
            observer.observe({'hook_event_name':'UserPromptSubmit','prompt':'x'*25000+'[amux approval] approved A7K2'},root,'mrpopo','abc','claude',1.0)
            value=json.loads(next((root/'status-events/mrpopo/abc').glob('0*.json')).read_text())
            self.assertTrue(value['approval_reserved_marker'])
            self.assertNotIn('[amux approval]',value['prompt'])

if __name__=='__main__': unittest.main()
