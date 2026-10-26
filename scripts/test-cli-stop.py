#!/usr/bin/env python3
"""Exercise shipped stop functions against a private HTTP server, never live tmux."""
import http.server
import os
from pathlib import Path
import re
import subprocess
import tempfile
import threading
import unittest

CLI = Path(os.environ.get('AMUX_TEST_CLI', Path(__file__).resolve().parents[1] / 'amux'))


class StopTests(unittest.TestCase):
    def run_stop(self, status=202, body='{"ok":true}', backend='tmux'):
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
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        self.addCleanup(server.server_close)
        self.addCleanup(server.shutdown)
        source = CLI.read_text()
        functions = []
        for name in ('_transport_breadcrumb', '_curl', '_stop_via_api', 'cmd_stop'):
            match = re.search(r'^' + name + r'\(\) \{\n.*?^}', source, re.M | re.S)
            self.assertIsNotNone(match, name)
            functions.append(match.group())
        with tempfile.TemporaryDirectory() as tmp:
            script = '\n'.join(functions) + '''
resolve_session() { echo "$1"; }
session_backend() { echo "$BACKEND"; }
is_running() { return 0; }
tmux_name() { echo "amux-$1"; }
need_tmux() { :; }
need_herdr() { :; }
tmux() { echo 'FORBIDDEN direct tmux' >&2; return 99; }
herdr() { echo 'FORBIDDEN direct herdr' >&2; return 99; }
die() { echo "$*" >&2; exit 1; }
cmd_stop target
'''
            env = dict(os.environ, AMUX_API=f'http://127.0.0.1:{server.server_port}',
                       AMUX_WORKER='lead', AMUX_SESSION='stale-session', AMUX_WORKER_TOKEN='test-token',
                       AMUX_HOME=tmp, CC_HOME=tmp, CC_SESSIONS=tmp, BACKEND=backend,
                       RED='', DIM='', RESET='')
            result = subprocess.run(['bash', '-e', '-c', script], env=env,
                                    capture_output=True, text=True, timeout=10)
        self.assertNotIn('FORBIDDEN direct', result.stderr)
        self.assertEqual(requests, [('/api/sessions/target/stop', 'lead', 'test-token')])
        return result

    def test_worker_stop_uses_server_for_both_backends(self):
        for backend in ('tmux', 'herdr'):
            with self.subTest(backend=backend):
                result = self.run_stop(backend=backend)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertIn('stopping', result.stdout)

    def test_refusals_and_invalid_responses_never_fall_back(self):
        for status, body in ((403, '{"error":"outside worker groups"}'),
                             (404, '{"error":"worker not found"}'),
                             (500, '{"ok":true}'), (200, '{}'), (200, '<html>error</html>')):
            for backend in ('tmux', 'herdr'):
                with self.subTest(status=status, body=body, backend=backend):
                    result = self.run_stop(status, body, backend)
                    self.assertNotEqual(result.returncode, 0)
                    self.assertNotIn('stopping', result.stdout)
                    self.assertIn('stop', result.stderr)


if __name__ == '__main__':
    unittest.main()
