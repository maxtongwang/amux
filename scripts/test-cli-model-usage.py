#!/usr/bin/env python3
"""`amux model` and `amux usage` against a throwaway listener (ai-config #149).

amux model sends exactly {model, effort} to PATCH /api/sessions/<w>/config, reports the swap mode,
and prints a server refusal as a refusal with a nonzero exit; it never retries or reroutes. With
no model it reads the configured and live model from /api/sessions. amux usage prints each
provider's windows and says when one was not measured. Nothing here reaches a real amux server.
"""
import http.server
import json
import os
from pathlib import Path
import subprocess
import tempfile
import threading
import unittest

CLI = os.environ.get('AMUX_CLI_PATH', str(Path(__file__).resolve().parents[1] / 'amux'))

SESSIONS = [{'name': 'w1', 'provider': 'claude', 'model': '', 'active_model': 'claude-opus-5-5[1m]',
             'model_source': 'self-report'}]
USAGE = {'measured': True, 'providers': [
    {'id': 'claude', 'label': 'Claude', 'available': True, 'measured': True, 'cache_age_s': 4, 'windows': [
        {'kind': 'session', 'label': '5-hour session', 'used_percent': 19.0, 'resets_at': '2026-10-17T10:39:59+00:00'},
        {'kind': 'weekly_all', 'label': 'Weekly · all models', 'used_percent': 35.0, 'resets_at': '2026-10-22T19:59:59+00:00'}]},
    {'id': 'codex', 'label': 'Codex', 'available': True, 'measured': True, 'windows': [
        {'kind': 'primary', 'label': '7-day', 'used_percent': 20.0, 'resets_at': 1792677895}]},
    {'id': 'gemini', 'label': 'Gemini', 'available': False, 'reason': 'Gemini CLI is not installed on this host.'},
    {'id': 'ollama', 'label': 'Ollama', 'available': True, 'local': True, 'measured': True, 'windows': []},
    {'id': 'grok', 'label': 'Grok', 'available': True, 'measured': False, 'summary': 'no usage API', 'windows': []}]}


class CliModelUsageTests(unittest.TestCase):
    def run_cli(self, *args, patch_status=200, patch_reply=None, usage_status=200, usage_reply=None):
        seen = []

        class Handler(http.server.BaseHTTPRequestHandler):
            def reply(self, code, doc):
                data = json.dumps(doc).encode()
                self.send_response(code); self.send_header('Content-Type', 'application/json')
                self.send_header('Content-Length', str(len(data))); self.end_headers(); self.wfile.write(data)

            def do_GET(self):
                seen.append(('GET', self.path, None, self.headers.get('X-Amux-Worker')))
                if self.path.startswith('/api/sessions'):
                    self.reply(200, SESSIONS)
                elif self.path.startswith('/api/usage'):
                    self.reply(usage_status, usage_reply if usage_reply is not None else USAGE)
                else:
                    self.reply(200, {'ok': True})

            def do_PATCH(self):
                n = int(self.headers.get('Content-Length') or 0)
                body = json.loads(self.rfile.read(n) or b'{}')
                seen.append(('PATCH', self.path, body, self.headers.get('X-Amux-Worker')))
                self.reply(patch_status, patch_reply if patch_reply is not None else
                           {'ok': True, 'applied': True, 'mode': 'hot', 'model': body.get('model'),
                            'message': f"model set to {body.get('model')}"})

            do_POST = do_GET

            def log_message(self, *_):
                pass

        server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        try:
            with tempfile.TemporaryDirectory() as home:
                url = 'http://127.0.0.1:' + str(server.server_port)
                env = dict(os.environ, AMUX_API=url, AMUX_URL=url, CC_HOME=home, AMUX_SESSION='w1', AMUX_WORKER='w1')
                env.pop('AMUX_WORKER_TOKEN', None)
                env.pop('TMUX', None)
                env.pop('TMUX_PANE', None)
                r = subprocess.run(['bash', CLI, *args], text=True, capture_output=True, env=env, timeout=60)
        finally:
            server.shutdown(); server.server_close()
        return r, [s for s in seen if s[1].startswith(('/api/sessions', '/api/usage'))]

    def patches(self, seen):
        return [s for s in seen if s[0] == 'PATCH']

    def test_switch_sends_exactly_model_and_effort_and_reports_the_mode(self):
        r, seen = self.run_cli('model', 'w1', 'claude-sonnet-5-5', '--effort', 'high')
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual([(p[1], p[2]) for p in self.patches(seen)],
                         [('/api/sessions/w1/config', {'model': 'claude-sonnet-5-5', 'effort': 'high'})])
        self.assertIn('mode hot', r.stdout)
        self.assertEqual(self.patches(seen)[0][3], 'w1', 'the request carries the caller as its worker identity')

    def test_effort_only_sends_no_model(self):
        r, seen = self.run_cli('model', 'w1', '--effort=low')
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual([p[2] for p in self.patches(seen)], [{'effort': 'low'}])

    def test_show_reads_configured_and_live_without_changing_anything(self):
        r, seen = self.run_cli('model', 'w1')
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(self.patches(seen), [])
        self.assertIn('unpinned', r.stdout)
        self.assertIn('claude-opus-5-5[1m]', r.stdout)
        self.assertIn('self-report', r.stdout)

    def test_unknown_worker_on_show_fails(self):
        r, _ = self.run_cli('model', 'ghost')
        self.assertNotEqual(r.returncode, 0)
        self.assertIn('no worker', r.stderr)

    def test_a_refusal_is_reported_and_exits_nonzero_without_a_retry(self):
        r, seen = self.run_cli('model', 'peer', 'claude-sonnet-5-5', patch_status=403,
                               patch_reply={'ok': False, 'error': 'config changes to another worker are owner-only'})
        self.assertNotEqual(r.returncode, 0)
        self.assertIn('refused (403)', r.stderr)
        self.assertIn('owner-only', r.stderr)
        self.assertEqual(len(self.patches(seen)), 1, 'a refusal is never retried or rerouted')

    def test_an_agent_rejection_is_a_failure(self):
        r, _ = self.run_cli('model', 'w1', 'nope', patch_status=422,
                            patch_reply={'ok': False, 'applied': False, 'mode': 'hot', 'error': "the agent refused model 'nope'"})
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("refused model 'nope'", r.stderr)

    def test_bad_usage_never_patches(self):
        for args in (('model', 'w1', '--bogus'), ('model', 'w1', 'a', 'b'), ('model', 'w1', '--effort')):
            r, seen = self.run_cli(*args)
            self.assertNotEqual(r.returncode, 0, args)
            self.assertEqual(self.patches(seen), [], args)

    def test_help_names_the_owner_routes(self):
        r, _ = self.run_cli('model', '--help')
        self.assertEqual(r.returncode, 0)
        for phrase in ('hub pane', 'dashboard', '/model', 'never as the owner'):
            self.assertIn(phrase, r.stdout)

    def test_usage_lists_every_provider_and_says_when_unmeasured(self):
        r, seen = self.run_cli('usage')
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn('5-hour session: 19% used', r.stdout)
        self.assertIn('Weekly · all models: 35% used', r.stdout)
        self.assertIn('7-day: 20% used', r.stdout)
        self.assertIn('Gemini: unavailable', r.stdout)
        self.assertIn('Ollama: local, unmetered', r.stdout)
        self.assertIn('Grok: unmeasured', r.stdout)

    def test_a_usage_refusal_fails_in_both_modes(self):
        refusal = {'error': 'unauthorized', 'reason': 'missing_credential'}
        for args in (('usage',), ('usage', '--json')):
            r, _ = self.run_cli(*args, usage_status=401, usage_reply=refusal)
            self.assertNotEqual(r.returncode, 0, args)
            self.assertIn('HTTP 401', r.stderr, args)
            self.assertIn('unauthorized', r.stderr, args)

    def test_usage_json_is_the_server_document(self):
        r, _ = self.run_cli('usage', '--json')
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(json.loads(r.stdout), USAGE)


if __name__ == '__main__':
    unittest.main()
