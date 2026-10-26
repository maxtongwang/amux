#!/usr/bin/env python3
"""A worker's `amux rm` asks the server to delete and never touches tmux or files itself.

Exercises the shipped cmd_rm against a private HTTP server, never live tmux
(ai-config #378/#387). Exit 0 when every case passes.
"""
import http.server
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import threading
import unittest

CLI = Path(os.environ.get('AMUX_TEST_CLI', Path(__file__).resolve().parents[1] / 'amux'))
FUNCTIONS = ('_transport_breadcrumb', '_curl', '_delete_via_api', 'cmd_rm')


class RmTests(unittest.TestCase):
    def run_rm(self, status=200, body='{"ok":true,"message":"deleted"}', worker='dev-lead'):
        requests = []

        class Handler(http.server.BaseHTTPRequestHandler):
            def do_POST(self):
                requests.append((self.path, self.headers.get('X-Amux-Worker'),
                                 self.headers.get('X-Amux-Worker-Token')))
                self.send_response(status)
                self.end_headers()
                self.wfile.write(body.encode())

            def log_message(self, *args):
                pass

        server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        self.addCleanup(server.server_close)
        self.addCleanup(server.shutdown)
        source = CLI.read_text()
        functions = []
        for name in FUNCTIONS:
            match = re.search(r'^' + name + r'\(\) \{\n.*?^}', source, re.M | re.S)
            self.assertIsNotNone(match, name)
            functions.append(match.group())
        with tempfile.TemporaryDirectory() as tmp:
            env_file = Path(tmp, 'target.env')
            env_file.write_text('CC_DIR=/tmp\n')
            script = '\n'.join(functions) + '''
resolve_session() { echo "$1"; }
is_running() { return 0; }
cmd_stop() { echo 'FORBIDDEN local stop' >&2; return 99; }
tmux() { echo 'FORBIDDEN direct tmux' >&2; return 99; }
rm() { echo 'FORBIDDEN local rm' >&2; return 99; }
git() { echo 'FORBIDDEN local git' >&2; return 99; }
die() { echo "$*" >&2; exit 1; }
cmd_rm target
'''
            env = dict(os.environ, AMUX_API=f'http://127.0.0.1:{server.server_port}',
                       AMUX_WORKER=worker, AMUX_SESSION='stale-session', AMUX_WORKER_TOKEN='test-token',
                       AMUX_HOME=tmp, CC_HOME=tmp, CC_SESSIONS=tmp, RED='', DIM='', RESET='')
            result = subprocess.run(['bash', '-c', script], env=env,
                                    capture_output=True, text=True, timeout=10)
            self.assertTrue(env_file.exists(), 'the CLI must not remove the env file itself')
        self.assertNotIn('FORBIDDEN', result.stderr)
        self.assertEqual(requests, [('/api/sessions/target/delete', worker, 'test-token')])
        return result

    def test_worker_rm_is_deleted_by_the_server(self):
        result = self.run_rm()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('removed target', result.stdout)

    def test_refusals_and_invalid_responses_fail_closed(self):
        cases = (
            (403, {'ok': False, 'code': 'agent_delete_not_authorized', 'error': 'not your team'}),
            (409, {'ok': False, 'code': 'agent_delete_running', 'error': 'still running'}),
            (409, {'ok': False, 'code': 'agent_delete_no_handoff', 'error': 'no handoff'}),
            (500, {'ok': True}), (200, {}), (200, '<html>error</html>'),
        )
        for status, body in cases:
            text = body if isinstance(body, str) else json.dumps(body)
            with self.subTest(status=status, body=text):
                result = self.run_rm(status, text)
                self.assertNotEqual(result.returncode, 0)
                self.assertNotIn('removed', result.stdout)
                self.assertIn('delete', result.stderr)
                if isinstance(body, dict) and body.get('code'):
                    self.assertIn(body['code'], result.stderr)

    def test_unreachable_server_never_falls_back(self):
        source = CLI.read_text()
        body = re.search(r'^_delete_via_api\(\) \{\n.*?^}', source, re.M | re.S).group()
        self.assertIn('no local delete attempted', body)
        rm = re.search(r'^cmd_rm\(\) \{\n.*?^}', source, re.M | re.S).group()
        worker_branch = rm.split('_delete_via_api "$name"', 1)
        self.assertEqual(len(worker_branch), 2, 'cmd_rm must call _delete_via_api')
        self.assertIn('return', worker_branch[1].split('fi', 1)[0], 'the worker branch must return before local cleanup')


if __name__ == '__main__':
    unittest.main()
