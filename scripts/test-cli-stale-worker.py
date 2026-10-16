#!/usr/bin/env python3
"""A renamed worker's CLI calls carry its NEW name in X-Amux-Worker, not the one it was launched with.

Regression for 2026-10-16: after dot-agent -> tmux-window-naming, AMUX_SESSION was corrected from
the pane (AMUX-4033) but AMUX_WORKER was not, and every header reads ${AMUX_WORKER:-${AMUX_SESSION}},
so `amux send` was refused at the group boundary as `dot-agent`.
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


def healed_events(spool_lines, bodies):
    """(var, from, to) of every stale_env_identity_healed row, still spooled or already shipped."""
    rows = []
    for line in spool_lines:
        try:
            rows.append(json.loads(line))
        except ValueError:
            pass
    for body in bodies:
        try:
            doc = json.loads(body)
        except ValueError:
            continue
        rows.extend(doc.get('rows', []) if isinstance(doc, dict) else [])
    return {(r.get('var'), r.get('from'), r.get('to')) for r in rows if r.get('event') == 'stale_env_identity_healed'}


class StaleWorkerTests(unittest.TestCase):
    def send(self, *, session, worker, pane_session, env_files):
        seen, bodies = [], []

        class Handler(http.server.BaseHTTPRequestHandler):
            def record(self):
                n = int(self.headers.get('Content-Length') or 0)
                bodies.append(self.rfile.read(n).decode(errors='replace') if n else '')
                seen.append((self.command, self.path, self.headers.get('X-Amux-Worker'), self.headers.get('X-Amux-Session')))
                data = json.dumps({'ok': True, 'delivered': True}).encode()
                self.send_response(200); self.send_header('Content-Length', str(len(data)))
                self.end_headers(); self.wfile.write(data)
            do_POST = do_GET = do_PATCH = record
            def log_message(self, *_): pass

        server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        try:
            with tempfile.TemporaryDirectory() as home, tempfile.TemporaryDirectory() as bindir:
                os.makedirs(Path(home, 'sessions'))
                for name in env_files:
                    Path(home, 'sessions', name + '.env').write_text(f'CC_DIR="{home}"\n')
                tmux = Path(bindir, 'tmux')
                # display-message -p -t <pane> '#S' answers the pane's session; anything else is a no-op.
                tmux.write_text('#!/bin/sh\ncase "$1" in display-message) echo "%s";; esac\nexit 0\n' % pane_session)
                tmux.chmod(0o755)
                url = 'http://127.0.0.1:' + str(server.server_port)
                env = dict(os.environ, PATH=bindir + os.pathsep + os.environ['PATH'], AMUX_API=url, AMUX_URL=url,
                           CC_HOME=home, AMUX_SESSION=session, AMUX_WORKER=worker, TMUX_PANE='%9', TMUX='/tmp/x,1,0')
                env.pop('AMUX_WORKER_TOKEN', None)
                subprocess.run(['bash', CLI, 'send', 'peer', 'hello'], text=True, capture_output=True, env=env, timeout=60)
                spool = Path(home, 'cli-transport.jsonl')
                logged = healed_events((spool.read_text() if spool.exists() else '').splitlines(), bodies)
        finally:
            server.shutdown(); server.server_close()
        workers = {w for (_, _, w, _) in seen if w}
        return workers, seen, logged

    def test_renamed_worker_sends_as_its_new_name(self):
        workers, seen, logged = self.send(session='old-name', worker='old-name', pane_session='amux-new-name',
                                          env_files=['new-name', 'peer'])
        self.assertTrue(seen, 'the CLI made no request')
        self.assertEqual(workers, {'new-name'}, seen)
        # The correction announces itself, for each variable it rewrote.
        self.assertEqual(logged, {('AMUX_SESSION', 'old-name', 'new-name'), ('AMUX_WORKER', 'old-name', 'new-name')})

    def test_a_live_claimed_name_is_left_alone(self):
        # A deliberate claim of an existing session is not rewritten from the pane.
        workers, seen, logged = self.send(session='other', worker='other', pane_session='amux-new-name',
                                  env_files=['other', 'new-name', 'peer'])
        self.assertTrue(seen, 'the CLI made no request')
        self.assertEqual(workers, {'other'}, seen)
        self.assertEqual(logged, set())

    def test_outside_an_amux_pane_nothing_changes(self):
        workers, seen, logged = self.send(session='old-name', worker='old-name', pane_session='my-own-tmux',
                                  env_files=['new-name', 'peer'])
        self.assertTrue(seen, 'the CLI made no request')
        self.assertEqual(workers, {'old-name'}, seen)
        self.assertEqual(logged, set())


if __name__ == '__main__':
    unittest.main()
