#!/usr/bin/env python3
"""`amux restore-running` against a throwaway listener: it POSTs exactly {only?, dry_run} to
/api/fleet/restore, prints the order and results, and exits nonzero on a failure or refusal.
Nothing here reaches a real amux server."""
import http.server
import json
import os
from pathlib import Path
import subprocess
import tempfile
import threading
import unittest

CLI = os.environ.get('AMUX_CLI_PATH', str(Path(__file__).resolve().parents[1] / 'amux'))


class RestoreRunningTests(unittest.TestCase):
    def run_cli(self, *args, status=200, reply=None):
        seen = []

        class Handler(http.server.BaseHTTPRequestHandler):
            def do_POST(self):
                n = int(self.headers.get('Content-Length') or 0)
                seen.append((self.path, json.loads(self.rfile.read(n) or b'{}')))
                data = json.dumps(reply if reply is not None else {}).encode()
                self.send_response(status); self.send_header('Content-Type', 'application/json')
                self.send_header('Content-Length', str(len(data))); self.end_headers(); self.wfile.write(data)
            do_GET = do_POST

            def log_message(self, *_):
                pass

        server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        try:
            with tempfile.TemporaryDirectory() as home:
                url = 'http://127.0.0.1:' + str(server.server_port)
                env = dict(os.environ, AMUX_API=url, AMUX_URL=url, CC_HOME=home)
                for k in ('AMUX_WORKER_TOKEN', 'AMUX_SESSION', 'AMUX_WORKER', 'TMUX', 'TMUX_PANE'):
                    env.pop(k, None)
                r = subprocess.run(['bash', CLI, 'restore-running', *args], text=True, capture_output=True, env=env, timeout=60)
        finally:
            server.shutdown(); server.server_close()
        return r, [s for s in seen if s[0] == '/api/fleet/restore']

    def test_restore_prints_the_order_and_results(self):
        reply = {'ok': True, 'measured': True, 'order': ['mrpopo', 'dev-lead', 'w1'], 'restored': ['mrpopo', 'dev-lead', 'w1'],
                 'failed': [], 'skipped': [{'name': 'old', 'why': 'archived'}]}
        r, seen = self.run_cli(reply=reply)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual([b for _, b in seen], [{'dry_run': False}])
        self.assertIn('order: mrpopo, dev-lead, w1', r.stdout)
        self.assertIn('skipped: old (archived)', r.stdout)

    def test_only_and_dry_run_are_sent(self):
        r, seen = self.run_cli('--only', 'dev-drill-141', '--dry-run',
                               reply={'ok': True, 'dry_run': True, 'measured': True, 'order': ['dev-drill-141'], 'skipped': []})
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual([b for _, b in seen], [{'dry_run': True, 'only': 'dev-drill-141'}])
        self.assertIn('would restore, in order: dev-drill-141', r.stdout)

    def test_a_failed_start_exits_nonzero(self):
        reply = {'ok': False, 'measured': True, 'order': ['a'], 'restored': [], 'failed': [{'name': 'a', 'error': 'tmux refused'}], 'skipped': []}
        r, _ = self.run_cli(reply=reply)
        self.assertNotEqual(r.returncode, 0)
        self.assertIn('failed: a: tmux refused', r.stdout)

    def test_a_refusal_exits_nonzero(self):
        r, _ = self.run_cli(status=403, reply={'ok': False, 'error': 'fleet restore is owner-only; w is a worker'})
        self.assertNotEqual(r.returncode, 0)
        self.assertIn('refused (403)', r.stderr)

    def test_no_recorded_set_is_not_success(self):
        r, _ = self.run_cli(reply={'ok': False, 'measured': False, 'error': 'no running-set.json recorded yet'})
        self.assertNotEqual(r.returncode, 0)
        self.assertIn('nothing to restore', r.stdout)


if __name__ == '__main__':
    unittest.main()
