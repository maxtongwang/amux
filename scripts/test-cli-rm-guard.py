#!/usr/bin/env python3
"""Exercise the shipped rm client under the real guard with an isolated HTTP fixture."""
import http.server
import json
import os
from pathlib import Path
import subprocess
import tempfile
import threading
import unittest

ROOT = Path(__file__).resolve().parents[1]


class RemoveGuardTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.home = Path(self.tmp.name)
        self.sessions = self.home / 'sessions'
        self.sessions.mkdir()
        for name in ('finished', 'finished-peer', 'lead'):
            (self.sessions / (name + '.env')).write_text('CC_BACKEND="tmux"\n')
        self.record = self.home / 'tmux-calls'
        fake = self.home / 'real-tmux'
        # The finished agent left a bare shell: has-session still succeeds.
        fake.write_text('#!/bin/sh\nprintf "%s\\n" "$*" >> "$TMUX_RECORD"\nexit 0\n')
        fake.chmod(0o755)
        self.requests = []
        self.status, self.body = 200, {'ok': True, 'message': 'deleted'}
        fixture = self

        class Handler(http.server.BaseHTTPRequestHandler):
            def do_DELETE(self):
                fixture.requests.append((self.path, self.headers.get('X-Amux-Session')))
                if self.path == '/api/sessions/finished' and self.server_allowed():
                    (fixture.sessions / 'finished.env').unlink()
                payload = fixture.body if isinstance(fixture.body, bytes) else json.dumps(fixture.body).encode()
                self.send_response(fixture.status)
                self.send_header('Content-Length', str(len(payload)))
                self.end_headers()
                self.wfile.write(payload)

            def server_allowed(self):
                return fixture.status == 200 and fixture.body == {'ok': True, 'message': 'deleted'}

            def log_message(self, *_):
                pass

        server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        self.addCleanup(server.server_close)
        self.addCleanup(server.shutdown)
        url = f'http://127.0.0.1:{server.server_port}'
        self.env = dict(os.environ, HOME=str(self.home), CC_HOME=str(self.home),
                        AMUX_HOME=str(self.home), AMUX_API=url, AMUX_URL=url,
                        AMUX_SESSION='lead', AMUX_WORKER='lead',
                        AMUX_REAL_TMUX=str(fake), TMUX_RECORD=str(self.record),
                        PATH=str(ROOT / 'scripts/worker-bin') + os.pathsep + os.environ['PATH'])
        for key in ('TMUX', 'TMUX_PANE', 'AMUX_WORKER_TOKEN', 'AMUX_ISOLATED'):
            self.env.pop(key, None)

    def run_cli(self, verb='rm', name='finished'):
        return subprocess.run(['bash', str(ROOT / 'amux'), verb, name], env=self.env,
                              capture_output=True, text=True, timeout=20)

    def test_finished_shell_removed_through_server_only(self):
        result = self.run_cli()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.requests, [('/api/sessions/finished', 'lead')])
        self.assertFalse((self.sessions / 'finished.env').exists())
        self.assertTrue((self.sessions / 'finished-peer.env').exists())
        self.assertIn('removed', result.stdout)
        self.assertFalse(self.record.exists(), 'rm must not invoke local tmux')

    def test_remove_aliases(self):
        for verb in ('remove', 'del'):
            with self.subTest(verb=verb):
                (self.sessions / 'finished.env').write_text('CC_BACKEND="tmux"\n')
                result = self.run_cli(verb)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertFalse((self.sessions / 'finished.env').exists())

    def test_failures_preserve_worker_and_log(self):
        for status, body in ((403, {'error': 'deletion requires authorization'}),
                             (500, {'ok': True}), (200, {}), (200, []),
                             (200, b'not json'), (200, {'ok': False}), (200, {'ok': 'true'}),
                             (200, {'ok': True, 'error': 'refused'})):
            with self.subTest(status=status, body=body):
                self.status, self.body = status, body
                result = self.run_cli()
                self.assertNotEqual(result.returncode, 0)
                self.assertTrue((self.sessions / 'finished.env').exists())
                self.assertNotIn('removed', result.stdout)
                self.assertIn('verdict=worker_remove_failed',
                              (self.home / 'logs/server-rs.log').read_text())
                self.assertFalse(self.record.exists())

    def test_unreachable_server_preserves_worker(self):
        self.env['AMUX_API'] = self.env['AMUX_URL'] = 'http://127.0.0.1:0'
        result = self.run_cli()
        self.assertNotEqual(result.returncode, 0)
        self.assertTrue((self.sessions / 'finished.env').exists())
        self.assertFalse(self.record.exists())

    def test_unknown_worker_does_not_request_delete(self):
        result = self.run_cli(name='missing')
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(self.requests, [])

    def test_raw_kills_remain_refused(self):
        for args in (['kill-session', '-t', '=amux-finished-peer'], ['kill-server']):
            with self.subTest(args=args):
                result = subprocess.run(['tmux', *args], env=self.env,
                                        capture_output=True, text=True, timeout=20)
                self.assertEqual(result.returncode, 1, result.stderr)
                self.assertFalse(self.record.exists())
                self.assertIn('verdict=tmux_guard_refused',
                              (self.home / 'logs/tmux-guard.log').read_text())


if __name__ == '__main__':
    unittest.main()
