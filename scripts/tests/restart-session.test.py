#!/usr/bin/env python3
"""Exercise the shipped Bash command with a fake API; no live lanes or network."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
PYTHON = str(Path.home() / '.amux/venvs/re-ops-ci/bin/python3')


class RestartCli(unittest.TestCase):
    def run_cli(self, *args, mode='ok'):
        with tempfile.TemporaryDirectory(prefix='restart-cli-') as scratch:
            root = Path(scratch)
            bin_dir = root / 'bin'
            bin_dir.mkdir()
            if mode == 'timeout':
                interpreter = bin_dir / 'python3'
                interpreter.write_text('#!' + PYTHON + '\nimport sys,time\nsys.argv=sys.argv[1:]\nticks=iter([0,301])\ntime.monotonic=lambda: next(ticks)\nexec(compile(sys.stdin.read(), "<restart>", "exec"))\n')
                interpreter.chmod(0o755)
            else:
                (bin_dir / 'python3').symlink_to(PYTHON)
            curl = bin_dir / 'curl'
            curl.write_text('''#!'''+PYTHON+'''
import json, os, sys
from pathlib import Path
args=sys.argv[1:]
payload=json.loads(args[args.index('-d')+1])
p=Path(os.environ['RESTART_REQUESTS'])
with p.open('a') as f: f.write(json.dumps(payload)+'\\n')
count=len(p.read_text().splitlines())
mode=os.environ['RESTART_MODE']
r=dict(schema_version=1,name='fixture',provider='claude',conversation_id_before='same-id',conversation_id_after='same-id',restarted=not payload['dry_run'],tasks=[],error=None)
if mode=='tasks':
    r['tasks']=[dict(id='task-a',prompt='fixture reminder')]
    if not payload['drop_tasks']: r.update(error='scheduled task records found: use --drop-tasks',restarted=False)
if mode in ('busy','timeout') or (mode=='wait' and count==1): r.update(error='lane is busy',code='restart_busy',restarted=False)
if mode=='changed' and not payload['dry_run']: r['conversation_id_after']='different-id'
if mode=='unreachable': sys.exit(7)
if mode=='nulltasks': r['tasks']=None
if mode=='nullids': r.update(conversation_id_before=None,conversation_id_after=None)
if mode=='old': r=dict(ok=True)
if mode=='malformed': print('not json'); sys.exit(0)
print(json.dumps(r))
''')
            curl.chmod(0o755)
            env = {k: v for k, v in os.environ.items() if not k.startswith(('AMUX_', 'CC_'))}
            env.update(HOME=str(root), CC_HOME=str(root / '.amux'),
                       PATH=str(bin_dir) + ':' + os.environ['PATH'],
                       AMUX_API='http://127.0.0.1:19876', AMUX_ISOLATED_RUN='1',
                       RESTART_REQUESTS=str(root / 'requests'), RESTART_MODE=mode)
            p = subprocess.run(['bash', os.environ.get('AMUX_RESTART_CLI', str(ROOT / 'amux')),
                                'restart', *args], env=env, capture_output=True, text=True, timeout=15)
            requests = root / 'requests'
            sent = [json.loads(line) for line in requests.read_text().splitlines()] if requests.exists() else []
            return p, sent

    def test_same_id_restart(self):
        p, sent = self.run_cli('fixture', '-y', '--json')
        self.assertEqual(p.returncode, 0, p.stderr + p.stdout)
        r = json.loads(p.stdout)
        self.assertTrue(r['restarted'])
        self.assertEqual(r['conversation_id_before'], r['conversation_id_after'])
        self.assertEqual([r['dry_run'] for r in sent], [True, False])
        self.assertTrue(all('new_conversation' not in r for r in sent))

    def test_human_output(self):
        p, _ = self.run_cli('fixture', '-y')
        self.assertEqual(p.returncode, 0, p.stderr)
        self.assertIn('Conversation before: same-id', p.stdout)
        self.assertIn('Conversation after: same-id', p.stdout)

    def test_usage(self):
        for args in [(), ('fixture', '--unknown'), ('a', 'b')]:
            p, sent = self.run_cli(*args, '--json')
            self.assertEqual(p.returncode, 2, p.stdout)
            self.assertIsNotNone(json.loads(p.stdout)['error'])
            self.assertEqual(sent, [])

    def test_busy_refuses_without_stop(self):
        p, sent = self.run_cli('fixture', '-y', '--json', mode='busy')
        self.assertEqual(p.returncode, 1)
        self.assertEqual(json.loads(p.stdout)['code'], 'restart_busy')
        self.assertEqual(len(sent), 1)
        self.assertTrue(sent[0]['dry_run'])

    def test_when_idle_rechecks(self):
        p, sent = self.run_cli('fixture', '-y', '--when-idle', '--json', mode='wait')
        self.assertEqual(p.returncode, 0, p.stdout + p.stderr)
        self.assertIn('Waiting for idle: lane is busy', p.stderr)
        self.assertEqual([r['dry_run'] for r in sent], [True, True, False])

    def test_idle_wait_timeout_changes_nothing(self):
        p, sent = self.run_cli('fixture', '-y', '--when-idle', '--json', mode='timeout')
        self.assertEqual(p.returncode, 1, p.stdout + p.stderr)
        self.assertIn('timed out', json.loads(p.stdout)['error'])
        self.assertEqual(len(sent), 1)
        self.assertTrue(sent[0]['dry_run'])

    def test_task_refusal_lists_records(self):
        p, sent = self.run_cli('fixture', '-y', '--json', mode='tasks')
        self.assertEqual(p.returncode, 1)
        self.assertEqual(json.loads(p.stdout)['tasks'][0]['id'], 'task-a')
        self.assertEqual(len(sent), 1)

    def test_explicit_task_loss(self):
        p, sent = self.run_cli('fixture', '-y', '--drop-tasks', '--json', mode='tasks')
        self.assertEqual(p.returncode, 0, p.stdout + p.stderr)
        self.assertTrue(all(r['drop_tasks'] for r in sent))
        self.assertEqual(len(json.loads(p.stdout)['tasks']), 1)
        self.assertIn('Scheduled task record:', p.stderr)

    def test_changed_id_is_failure(self):
        p, _ = self.run_cli('fixture', '-y', '--json', mode='changed')
        self.assertEqual(p.returncode, 1)
        self.assertFalse(json.loads(p.stdout)['restarted'])
        self.assertIn('preserve', json.loads(p.stdout)['error'])

    def test_transport_and_invalid_responses(self):
        for mode in ['unreachable', 'old', 'malformed', 'nulltasks', 'nullids']:
            p, sent = self.run_cli('fixture', '-y', '--json', mode=mode)
            self.assertEqual(p.returncode, 1, p.stdout + p.stderr)
            self.assertIsNotNone(json.loads(p.stdout)['error'])
            self.assertEqual(len(sent), 1)


if __name__ == '__main__':
    unittest.main()
