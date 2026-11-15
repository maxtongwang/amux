#!/usr/bin/env python3
"""Exercise shipped hooks under a 30-second foreign queue lock, refs ai-config#1749."""
import fcntl
import http.server
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import threading
import time
import unittest

HOOK = Path(os.environ.get('HOOK_REPORT_SCRIPT', 'scripts/hooks/hook-report.sh')).resolve()


class Contention(unittest.TestCase):
    def test_locked_queues_return_and_replay(self):
        received = []

        class Handler(http.server.BaseHTTPRequestHandler):
            def do_POST(self):
                received.append(json.loads(self.rfile.read(int(self.headers['Content-Length']))))
                self.send_response(200)
                self.end_headers()
                self.wfile.write(b'{"ok":true}')

            def log_message(self, *args):
                pass

        with tempfile.TemporaryDirectory() as tmp, http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler) as server:
            threading.Thread(target=server.serve_forever, daemon=True).start()
            root = Path(tmp)
            queue = root / '.amux/hook-report-queue'
            queue.mkdir(parents=True)
            paths = [queue / 'probe.json.lock', queue / 'probe.state.json.lock']
            locker = subprocess.Popen([sys.executable, '-c',
                'import fcntl,sys,time; locks=[open(p,"a+") for p in sys.argv[1:]]; '
                '[fcntl.flock(f,fcntl.LOCK_EX) for f in locks]; print("locked",flush=True); time.sleep(30)',
                *map(str, paths)], stdout=subprocess.PIPE, text=True)
            try:
                self.assertEqual(locker.stdout.readline().strip(), 'locked')
                env = dict(os.environ, HOME=tmp, AMUX_SESSION='probe', TMUX_PANE='',
                           AMUX_URL=f'http://127.0.0.1:{server.server_port}')
                for mode in ('active', 'subagent-start', 'subagent-stop'):
                    start = time.monotonic()
                    try:
                        done = subprocess.run(['bash', str(HOOK), mode, 'contention-test'], input='{"session_id":"test","agent_id":"agent"}',
                                              text=True, env=env, capture_output=True, timeout=1)
                    except subprocess.TimeoutExpired:
                        self.fail(f'{mode} hook blocked longer than 1 second under queue lock')
                    elapsed = time.monotonic() - start
                    self.assertEqual(done.returncode, 0, done.stderr)
                    self.assertLess(elapsed, 1)
                    print(f'{mode} enqueue_seconds={elapsed:.3f}', flush=True)
                self.assertEqual(received, [])
                locker.wait(timeout=31)
                deadline = time.monotonic() + 10
                while len(received) < 3 and time.monotonic() < deadline:
                    time.sleep(.05)
                self.assertEqual(len(received), 3, received)
                self.assertEqual([r['subagent'] for r in received if 'subagent' in r], ['start', 'stop'])
                self.assertEqual(sum(r.get('state') == 'active' for r in received), 1)
                log = (root / '.amux/logs/hook-report-failures.log').read_text()
                self.assertIn('verdict=queue_lock_contended', log)
            finally:
                if locker.poll() is None:
                    locker.terminate()
                locker.wait()
                locker.stdout.close()
                server.shutdown()

    def test_native_hook_never_waits_for_write_or_delivery_lock(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            folder = root / 'status-events/worker/abc'
            folder.mkdir(parents=True)
            locks = [open(folder / name, 'a') for name in ('write.lock', 'deliver.lock')]
            for lock in locks:
                fcntl.flock(lock, fcntl.LOCK_EX)
            env = dict(os.environ, AMUX_STATUS_HOME=tmp, AMUX_STATUS_WORKER='worker',
                       AMUX_STATUS_RUN_ID='abc', AMUX_STATUS_URL='http://127.0.0.1:9')
            try:
                start = time.monotonic()
                result = subprocess.run([sys.executable, os.environ.get('NATIVE_STATUS_SCRIPT', 'scripts/hooks/native-status.py'), 'codex'],
                    input='{"hook_event_name":"UserPromptSubmit","prompt":"hello","tool_input":{"secret":"omit"}}',
                    text=True, env=env, capture_output=True, timeout=1)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertLess(time.monotonic() - start, 1)
                pending = list((folder / 'pending').glob('*.json'))
                self.assertEqual(len(pending), 1)
                self.assertNotIn('secret', pending[0].read_text())
            finally:
                for lock in locks:
                    lock.close()
            deadline = time.monotonic() + 5
            while not list(folder.glob('0*.json')) and time.monotonic() < deadline:
                time.sleep(.02)
            values = [json.loads(p.read_text()) for p in folder.glob('0*.json')]
            self.assertEqual(len(values), 1)
            self.assertEqual(values[0]['state'], 'active')
            self.assertEqual(values[0]['prompt'], 'hello')

    def test_native_pending_recovers_after_sigkill_without_duplicate(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            folder = root / 'status-events/worker/abc'
            pending = folder / 'pending'
            pending.mkdir(parents=True)
            event = pending / '00000000000000000001-event.json'
            event.write_text(json.dumps({'data': {'hook_event_name': 'UserPromptSubmit'},
                                         'provider': 'codex', 'occurred_at': 1}))
            env = dict(os.environ, AMUX_STATUS_HOME=tmp, AMUX_STATUS_WORKER='worker',
                       AMUX_STATUS_RUN_ID='abc', AMUX_STATUS_URL='http://127.0.0.1:9')
            command = [sys.executable, 'scripts/hooks/native-status.py', '--deliver']
            with open(folder / 'write.lock', 'a') as guard:
                fcntl.flock(guard, fcntl.LOCK_EX)
                child = subprocess.Popen(command, env=env)
                try:
                    deadline = time.monotonic() + 3
                    owned = False
                    while time.monotonic() < deadline:
                        with open(folder / 'fold.lock', 'a') as fold:
                            try:
                                fcntl.flock(fold, fcntl.LOCK_EX | fcntl.LOCK_NB)
                            except BlockingIOError:
                                owned = True
                                break
                        time.sleep(.02)
                    self.assertTrue(owned, 'drainer did not reach observation boundary')
                    child.kill()
                    child.wait(timeout=2)
                    self.assertEqual(child.returncode, -9)
                    self.assertTrue(event.exists())
                    self.assertEqual(list(folder.glob('0*.json')), [])
                finally:
                    if child.poll() is None:
                        child.kill()
                        child.wait()
            for _ in range(2):
                subprocess.run(command, env=env, check=True, timeout=3)
            self.assertFalse(event.exists())
            values = [json.loads(p.read_text()) for p in folder.glob('0*.json')]
            self.assertEqual(len(values), 1)
            self.assertEqual(values[0]['sequence'], 1)


if __name__ == '__main__':
    unittest.main()
