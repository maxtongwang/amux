#!/usr/bin/env python3
"""Exercise the installed Bash guard with a read-only fake tmux, never live tmux."""
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / 'scripts/hooks/owner-input-guard.py'


class OwnerInputGuard(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='owner-input-guard-')
        self.addCleanup(self.temp.cleanup)
        self.home = Path(self.temp.name)
        fake = self.home / 'tmux'
        fake.write_text('#!/usr/bin/env python3\nimport sys\na=sys.argv[1:]\n'
                        'assert "display-message" in a, a\n'
                        'print("amux-owner-configured" if "%7" in a or "@8" in a else "amux-other")\n')
        fake.chmod(0o755)
        self.env = dict(os.environ, PATH=str(self.home) + os.pathsep + os.environ['PATH'],
                        AMUX_STATUS_HOME=str(self.home), AMUX_STATUS_OWNER='owner-configured')

    def run_guard(self, command, tool='Bash'):
        return subprocess.run([sys.executable, '-B', str(SCRIPT)],
                              input=json.dumps({'tool_name': tool, 'tool_input': {'command': command}}),
                              env=self.env, capture_output=True, text=True, timeout=10)

    def assert_blocked(self, command):
        result = self.run_guard(command)
        self.assertEqual(result.returncode, 2, (command, result.stderr))
        audit = json.loads((self.home / 'logs/owner-input-guard.log').read_text().splitlines()[-1])
        self.assertEqual(audit['verdict'], 'owner_tmux_input_refused')
        self.assertTrue(audit['measured'])
        self.assertEqual(audit['n_considered'], 1)
        self.assertNotIn('command', audit)

    def test_each_injection_verb(self):
        for verb in ('send-keys', 'paste-buffer', 'load-buffer', 'set-buffer'):
            with self.subTest(verb=verb):
                self.assert_blocked(f'tmux {verb} -t amux-owner-configured yes')

    def test_target_socket_and_quoting_forms(self):
        for command in (
            'tmux send-keys -t owner-configured yes',
            'tmux send-keys -t=owner-configured yes',
            'tmux send-keys -t "=amux-owner-configured:0.1" yes',
            "tmux send-keys -t'amux-owner-configured:1' yes",
            'tmux -L private send-keys -t amux-owner-configured Enter',
            "tmux -S '/tmp/private sock' paste-buffer -t '=amux-owner-configured'",
            '/opt/homebrew/bin/tmux -Lprivate set-buffer -b amux-owner-configured yes',
            'tmux -S/tmp/private load-buffer -b owner-configured /tmp/relay',
            'bash -c "tmux send-keys -t amux-owner-configured yes"',
            'printf hello; tmux send-keys -t amux-owner-configured yes',
            r'tmux list-panes \; send-keys -t amux-owner-configured yes',
        ):
            with self.subTest(command=command): self.assert_blocked(command)

    def test_pane_and_window_ids_resolve_to_owner(self):
        self.assert_blocked('tmux -L private send-keys -t %7 yes')
        self.assert_blocked('tmux -S /tmp/private paste-buffer -t @8')

    def test_global_buffer_writes_are_refused(self):
        self.assert_blocked('tmux load-buffer /tmp/relay')
        self.assert_blocked('tmux set-buffer yes')

    def test_non_owner_target_allowed(self):
        for command in ('tmux send-keys -t amux-other yes',
                        'tmux -L private paste-buffer -t %9',
                        'tmux load-buffer -b other /tmp/relay',
                        'tmux set-buffer -b other yes',
                        'tmux list-panes', 'echo owner-configured'):
            with self.subTest(command=command):
                self.assertEqual(self.run_guard(command).returncode, 0)
        self.assertFalse((self.home / 'logs/owner-input-guard.log').exists())

    def test_no_owner_config_fails_closed_for_injection(self):
        self.env.pop('AMUX_STATUS_OWNER')
        self.assert_blocked('tmux send-keys -t other yes')

    def test_non_bash_is_untouched(self):
        self.assertEqual(self.run_guard('tmux send-keys -t owner-configured yes', 'Read').returncode, 0)

    def test_both_installers_register_bash_guard_idempotently(self):
        for filename in ('install-claude-status-hooks.py', 'install-native-status-hooks.py'):
            spec = importlib.util.spec_from_file_location('installer', SCRIPT.with_name(filename))
            module = importlib.util.module_from_spec(spec); spec.loader.exec_module(module)
            for provider in ('claude', 'codex'):
                def merge(data):
                    if filename.startswith('install-claude'):
                        return module.merge(data, "$HOME/.amux/hook-report.sh", provider=provider)
                    return module.merge(data, provider, SCRIPT.with_name('native-status.py'))
                first = merge({}); second = merge(json.loads(json.dumps(first)))
                self.assertEqual(first, second)
                guards = [(g['matcher'], h) for g in first['hooks']['PreToolUse'] for h in g['hooks']
                          if 'owner-input-guard.py' in h.get('command', '')]
                self.assertEqual(len(guards), 1, (filename, provider))
                self.assertEqual(guards[0][0], 'Bash')
                if provider == 'codex': self.assertEqual(guards[0][1]['statusMessage'], 'amux owner input guard')


if __name__ == '__main__': unittest.main()
