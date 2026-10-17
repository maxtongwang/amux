#!/usr/bin/env python3
"""The worker-pane tmux guard (scripts/worker-bin/tmux) against a FAKE real tmux.

Every case runs the guard with AMUX_REAL_TMUX pointing at a recorder script in a
temp dir, so a pass-through is proven by the argv the recorder wrote and a refusal
by the recorder writing nothing. No real tmux is ever started or signalled here.
Exit 0 when every case passes, nonzero otherwise.
"""
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

GUARD = Path(__file__).resolve().parent / 'worker-bin' / 'tmux'


class WorkerTmuxGuardTests(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp(prefix='tmux-guard-test-')).resolve()
        self.addCleanup(shutil.rmtree, self.tmp, True)
        self.record = self.tmp / 'argv.rec'
        self.fake = self.tmp / 'fake-tmux'
        # One argv element per NUL-terminated record; exits 0 and touches nothing else.
        self.fake.write_text('#!/bin/sh\nfor a in "$@"; do printf "%s\\0" "$a"; done > "$FAKE_RECORD"\n')
        self.fake.chmod(0o755)
        self.amux_home = self.tmp / 'amux-home'
        self.live_sock = self.tmp / 'live' / 'sock'
        self.live_sock.parent.mkdir()
        self.live_sock.write_text('')  # stands in for the live server's socket file

    def run_guard(self, *args, tmux_env=True, extra_env=None):
        env = {
            'PATH': '/usr/bin:/bin',
            'HOME': str(self.tmp),
            'AMUX_HOME': str(self.amux_home),
            'AMUX_REAL_TMUX': str(self.fake),
            'FAKE_RECORD': str(self.record),
            'TMUX_TMPDIR': str(self.tmp / 'tmuxtmp'),
        }
        if tmux_env:
            env['TMUX'] = f'{self.live_sock},12345,0'
            env['TMUX_PANE'] = '%1'
        env.update(extra_env or {})
        if self.record.exists():
            self.record.unlink()
        return subprocess.run(['/bin/bash', str(GUARD), *args], env=env, cwd=self.tmp,
                              capture_output=True, text=True, timeout=20)

    def recorded(self):
        if not self.record.exists():
            return None
        return self.record.read_bytes().decode().split('\0')[:-1]

    def log_lines(self):
        log = self.amux_home / 'logs' / 'tmux-guard.log'
        return log.read_text().splitlines() if log.exists() else []

    def assert_refused(self, *args, **kw):
        before = len(self.log_lines())
        r = self.run_guard(*args, **kw)
        self.assertEqual(r.returncode, 1, f'{args}: rc={r.returncode} stderr={r.stderr}')
        self.assertIsNone(self.recorded(), f'{args}: the fake real tmux was invoked on a refusal')
        self.assertIn('env -u TMUX -u TMUX_PANE', r.stderr, f'{args}: no safe form in message')
        self.assertIn('-S', r.stderr)
        lines = self.log_lines()
        self.assertEqual(len(lines), before + 1, f'{args}: expected exactly one new log line')
        self.assertIn('verdict=tmux_guard_refused', lines[-1])
        self.assertIn(f'pwd={self.tmp}', lines[-1])
        return r, lines[-1]

    def assert_passed(self, *args, **kw):
        r = self.run_guard(*args, **kw)
        self.assertEqual(r.returncode, 0, f'{args}: rc={r.returncode} stderr={r.stderr}')
        self.assertEqual(self.recorded(), list(args), f'{args}: argv did not pass through unchanged')
        return r

    # --- refusals on the live server ---------------------------------------------
    def test_bare_kill_server_refused_without_tmux_env(self):
        _, line = self.assert_refused('kill-server', tmux_env=False)
        self.assertIn('argv=kill-server', line)

    def test_kill_server_refused_inside_a_pane_with_no_socket(self):
        self.assert_refused('kill-server')

    def test_kill_session_refused(self):
        self.assert_refused('kill-session', '-t', 'amux-x')

    def test_kill_pane_and_alias_refused(self):
        self.assert_refused('kill-pane', '-t', '%3')
        self.assert_refused('killp')

    def test_dash_K_refused(self):
        self.assert_refused('-K')
        self.assert_refused('-uK', 'list-sessions')

    def test_socket_equal_to_tmux_env_refused(self):
        self.assert_refused('-S', str(self.live_sock), 'kill-server')
        self.assert_refused(f'-S{self.live_sock}', 'kill-server')
        # Same file spelled differently is still the live server.
        self.assert_refused('-S', str(self.live_sock.parent / '.' / 'sock'), 'kill-server')

    def test_dash_L_default_refused(self):
        self.assert_refused('-L', 'default', 'kill-server')
        self.assert_refused('-Ldefault', 'kill-session', '-t', 'x')

    def test_explicit_default_socket_path_refused(self):
        self.assert_refused('-S', f'/tmp/tmux-{os.getuid()}/default', 'kill-server', tmux_env=False)

    def test_dash_L_naming_the_live_socket_refused(self):
        named = self.tmp / 'tmuxtmp' / f'tmux-{os.getuid()}' / 'amux'
        named.parent.mkdir(parents=True)
        named.write_text('')
        self.assert_refused('-L', 'amux', 'kill-server', extra_env={'TMUX': f'{named},1,0'})

    def test_abbreviations_refused(self):
        for word in ('kill-ser', 'kill-serv', 'kill-ses', 'kill-sess', 'kill-p', 'kill-pa'):
            with self.subTest(word=word):
                self.assert_refused(word)

    def test_global_options_before_command_are_parsed(self):
        self.assert_refused('-2', '-u', '-f', '/dev/null', 'kill-server')
        self.assert_refused('-2uv', 'kill-server')
        self.assert_refused('-f', '/dev/null', '--', 'kill-server')

    def test_chained_kill_refused(self):
        _, line = self.assert_refused('list-sessions', ';', 'kill-server')
        self.assertIn('kill-server', line)
        self.assert_refused('list-sessions;', 'kill-server')
        self.assert_refused('new-window', '-d', ';', 'kill-ses', '-t', 'x')

    def test_nested_kill_refused(self):
        self.assert_refused('run-shell', 'tmux kill-server')
        self.assert_refused('if-shell', 'true', 'kill-session -t x')
        self.assert_refused('run', 'tmux kill-server')
        self.assert_refused('new-window', '-d', 'tmux kill-server')
        self.assert_refused('split-window', 'tmux kill-ses -t x')
        self.assert_refused('bind-key', 'X', 'kill-server')
        self.assert_refused('confirm-before', 'kill-server')
        # Boolean flags must not hide the shell command that follows them.
        self.assert_refused('new-window', '-k', '/opt/homebrew/bin/tmux kill-server')
        self.assert_refused('respawn-pane', '-k', '/opt/homebrew/bin/tmux kill-server')
        self.assert_refused('new-window', '-dk', 'tmux kill-server')
        self.assert_refused('split-window', '-h', 'tmux kill-server')
        self.assert_refused('bind-key', '-n', 'X', 'kill-server')
        self.assert_refused('run-shell', '-b', 'tmux kill-server')
        self.assert_refused('new-session', '-d', '-s', 'probe', 'tmux kill-server')

    # --- pass-throughs ---------------------------------------------------------------
    def test_kill_on_other_socket_passes_with_exact_argv(self):
        self.assert_passed('-S', '/tmp/other/sock', 'kill-server')
        self.assert_passed('-S', '/tmp/other/sock', 'kill-session', '-t', 'x')
        self.assert_passed('-L', 'private-test', 'kill-server')
        self.assertEqual(self.log_lines(), [])

    def test_ordinary_commands_pass_unchanged(self):
        self.assert_passed('list-sessions')
        self.assert_passed('new-window', '-d', '-n', 'x y', 'sleep 3')
        self.assert_passed('-u', 'display-message', '-p', '#{session_name}')
        self.assert_passed('list-sessions', ';', 'list-windows')
        self.assert_passed('kill-window', '-t', 'x')
        self.assert_passed()
        self.assertEqual(self.log_lines(), [])

    def test_data_arguments_mentioning_a_kill_pass_through(self):
        # Messages, buffers, typed text and names are data tmux never runs.
        self.assert_passed('display-message', '-p', 'Never run kill-server')
        self.assert_passed('set-buffer', 'Never run kill-server')
        self.assert_passed('send-keys', '-l', 'Explain kill-server')
        self.assert_passed('new-window', '-d', '-n', 'kill-server', 'sleep 3')
        self.assertEqual(self.log_lines(), [])

    def test_escaped_semicolon_is_not_a_separator(self):
        self.assert_passed('display-message', 'a\\;', 'kill-window')

    def test_missing_real_tmux_fails_closed(self):
        r = self.run_guard('list-sessions', extra_env={'AMUX_REAL_TMUX': str(self.tmp / 'nope')})
        self.assertEqual(r.returncode, 127)
        self.assertIsNone(self.recorded())
        r = self.run_guard('list-sessions', extra_env={'AMUX_REAL_TMUX': 'relative/tmux'})
        self.assertEqual(r.returncode, 127)

    def test_real_tmux_pointing_at_guard_refuses_to_recurse(self):
        r = self.run_guard('list-sessions', extra_env={'AMUX_REAL_TMUX': str(GUARD)})
        self.assertEqual(r.returncode, 127)
        self.assertIn('recurse', r.stderr)


if __name__ == '__main__':
    unittest.main()
