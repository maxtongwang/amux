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
        fake.write_text("#!/usr/bin/env python3\nimport json, os, sys\na = sys.argv[1:]\nassert 'display-message' in a, a\nassert a[-1] == '#{session_name}', a\nwith open(os.path.join(os.path.dirname(__file__), 'resolutions.jsonl'), 'a') as f:\n    f.write(json.dumps(a)+'\\n')\nt = a[a.index('-t')+1] if '-t' in a else '%7'\nif t in ('missing', 'ambiguous'):\n    sys.exit(1)\nif t == 'empty':\n    sys.exit(0)\nif t == 'multiple':\n    print('amux-other\\namux-owner-configured'); sys.exit(0)\ns = t.lstrip('=').split(':')[0]\nprint('amux-owner-configured' if s in ('owner-configured', 'amux-owner-configured', 'amux-owner-conf', 'amux-owner*', '%7', '@8') else 'amux-other')\n")
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

    def test_aliases_and_every_unambiguous_prefix(self):
        spellings = ['send', 'pasteb', 'loadb', 'setb']
        for name, shortest in [('send-keys', 'send-k'), ('paste-buffer', 'pa'),
                               ('load-buffer', 'loa'), ('set-buffer', 'set-b')]:
            spellings += [name[:n] for n in range(len(shortest), len(name)+1)]
        for verb in spellings:
            with self.subTest(verb=verb):
                self.assert_blocked(f'tmux {verb} -t amux-owner-conf yes')
                self.assertEqual(self.run_guard(f'tmux {verb} -t amux-other yes').returncode, 0)

    def test_every_target_is_resolved_and_failures_refused(self):
        for verb in ('send-keys', 'paste-buffer', 'load-buffer', 'set-buffer'):
            for target in ('amux-owner-configured', 'amux-owner-conf', 'amux-owner*',
                           'missing', 'ambiguous', 'empty', 'multiple'):
                with self.subTest(verb=verb, target=target):
                    self.assert_blocked(f'tmux {verb} -t "{target}" yes')
        resolutions = [json.loads(line) for line in (self.home/'resolutions.jsonl').read_text().splitlines()]
        self.assertEqual(len(resolutions), 28)

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

    def test_socket_flags_belong_to_each_invocation(self):
        commands = [
            ('tmux -Lprivate send-key -t amux-other yes; tmux -S "/tmp/socket path" pasteb -t amux-other',
             [['-L','private'], ['-S','/tmp/socket path']]),
        ]
        for command, sockets in commands:
            self.assertEqual(self.run_guard(command).returncode, 0)
            resolutions=[json.loads(line) for line in (self.home/'resolutions.jsonl').read_text().splitlines()]
            self.assertEqual(len(resolutions), 2)
            for args, socket in zip(resolutions, sockets):
                self.assertEqual(args[:2], socket)
        for socket in ('-Lprivate', '-S"/tmp/socket path"'):
            self.assert_blocked(f'tmux {socket} send -t "amux-owner*" yes')

    def test_separators_do_not_mix_owner_and_nonowner_targets(self):
        for separator in (';', '&&', '||', r'\;'):
            for verb in ('send', 'pasteb', 'loadb', 'setb'):
                self.assert_blocked(f'tmux {verb} -t "amux-owner*" yes {separator} tmux {verb} -t amux-other yes')
                self.assertEqual(self.run_guard(f'tmux {verb} -t amux-other yes {separator} tmux {verb} -t amux-other yes').returncode, 0)

    def test_bundled_target_and_global_options(self):
        for verb, flags in [('send', '-lt'), ('pasteb', '-dt'), ('loadb', '-wt'), ('setb', '-at')]:
            for target in ('amux-owner*', 'amux-other'):
                command=f'tmux -uLprivate {verb} {flags} "{target}" yes'
                if target == 'amux-owner*': self.assert_blocked(command)
                else: self.assertEqual(self.run_guard(command).returncode, 0)
        resolutions=[json.loads(line) for line in (self.home/'resolutions.jsonl').read_text().splitlines()]
        self.assertEqual(len(resolutions), 8)
        self.assertTrue(all(args[:2] == ['-L','private'] for args in resolutions))

    def test_client_and_mouse_key_dispatch_fail_closed(self):
        for flags in ('-K', '-M', '-c owner-client'):
            self.assert_blocked(f'tmux send {flags} -t amux-other yes')

    def test_unresolved_target_reason_is_logged(self):
        for target in ('missing', 'ambiguous', 'empty', 'multiple'):
            self.assert_blocked(f'tmux send -t {target} yes')
            audit=json.loads((self.home/'logs/owner-input-guard.log').read_text().splitlines()[-1])
            self.assertEqual(audit['operation'], 'send-keys:target_unresolved')

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
