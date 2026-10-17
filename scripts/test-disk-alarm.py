#!/usr/bin/env python3
"""DMIW-7: disk-alarm.py tells the leads below 50 GB free and the owner below 20 GB.

Runs the shipped script with a fake `amux`, fake free space and a fake clock; no
message reaches a real worker or the owner's phone.
"""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

SCRIPT = Path(__file__).with_name('disk-alarm.py').resolve()
GB = 1024 ** 3
NOW = 1_800_000_000


class DiskAlarmTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.home = Path(self.temp.name)
        self.calls = self.home / 'amux-calls'
        self.amux = self.home / 'amux'
        self.amux.write_text('#!/bin/sh\nprintf "%s\\n" "$*" >> "{}"\n'
                             '[ "$2" = "${{FAKE_AMUX_FAIL:-}}" ] && exit 1\nexit ${{FAKE_AMUX_EXIT:-0}}\n'.format(self.calls))
        self.amux.chmod(0o755)

    def run_alarm(self, free_gb, now=NOW, **extra):
        env = {**os.environ, 'AMUX_BIN': str(self.amux), 'AMUX_DISK_ALARM_TO': 'business-lead,dev-lead',
               'AMUX_DISK_ALARM_FREE_BYTES': str(int(free_gb * GB)), 'AMUX_DISK_ALARM_NOW': str(now), **extra}
        done = subprocess.run([sys.executable, str(SCRIPT), str(self.home)], env=env,
                              capture_output=True, text=True, timeout=60)
        self.assertEqual(done.returncode, 0, done.stderr)
        return json.loads(done.stdout)

    def calls_made(self):
        return self.calls.read_text().splitlines() if self.calls.exists() else []

    def test_plenty_of_space_is_quiet(self):
        verdict = self.run_alarm(400)
        self.assertEqual((verdict['level'], verdict['fired'], verdict['measured']), ('ok', False, True))
        self.assertEqual(self.calls_made(), [])

    def test_below_fifty_tells_each_lead_once(self):
        verdict = self.run_alarm(45)
        self.assertEqual((verdict['level'], verdict['fired']), ('leads', True))
        self.assertEqual(self.calls_made(), ['send business-lead --stdin', 'send dev-lead --stdin'])
        self.run_alarm(44, now=NOW + 600)  # ten minutes later, same level: deduped
        self.assertEqual(len(self.calls_made()), 2)
        self.run_alarm(44, now=NOW + 6 * 3600)  # six hours later: repeated
        self.assertEqual(len(self.calls_made()), 4)

    def test_below_twenty_also_alerts_the_owner_even_right_after_a_leads_alarm(self):
        self.run_alarm(45)
        verdict = self.run_alarm(15, now=NOW + 60)  # worse level fires at once
        self.assertEqual((verdict['level'], verdict['fired']), ('owner', True))
        calls = self.calls_made()
        self.assertEqual(sum(c.startswith('send ') for c in calls), 4)
        self.assertTrue(calls[-1].startswith('alert Disk nearly full: 15 GB free'))

    def test_recovery_clears_so_the_next_drop_announces_again(self):
        self.run_alarm(45)
        self.run_alarm(300, now=NOW + 60)
        self.assertFalse((self.home / '.amux' / 'disk-alarm.json').read_text().strip('{} \n'))
        self.run_alarm(45, now=NOW + 120)
        self.assertEqual(len(self.calls_made()), 4)

    def test_a_failed_send_is_retried_on_the_next_tick(self):
        verdict = self.run_alarm(45, FAKE_AMUX_EXIT='1')
        self.assertEqual(verdict['failed'], ['business-lead', 'dev-lead'])
        self.run_alarm(45, now=NOW + 60)
        self.assertEqual(len(self.calls_made()), 4, 'not marked announced after a failure')

    def test_thresholds_are_settable(self):
        verdict = self.run_alarm(90, AMUX_DISK_ALARM_LEADS_GB='100', AMUX_DISK_ALARM_OWNER_GB='95')
        self.assertEqual(verdict['level'], 'owner')


    def test_a_partial_failure_retries_only_the_failed_recipient(self):
        self.run_alarm(45, FAKE_AMUX_FAIL='dev-lead')
        self.run_alarm(45, now=NOW + 60)
        sends = [c for c in self.calls_made() if c.startswith('send ')]
        self.assertEqual(sends.count('send business-lead --stdin'), 1, 'told once, not again')
        self.assertEqual(sends.count('send dev-lead --stdin'), 2, 'failed once, retried once')
        self.run_alarm(45, now=NOW + 120)
        self.assertEqual(len(self.calls_made()), 3, 'nothing pending: quiet until the repeat')

    def test_the_owner_is_not_paged_again_when_a_lead_send_fails(self):
        for minute in range(3):  # three builder runs, a lead failing each time
            self.run_alarm(15, now=NOW + 600 * minute, FAKE_AMUX_FAIL='dev-lead')
        alerts = [c for c in self.calls_made() if c.startswith('alert ')]
        self.assertEqual(len(alerts), 1)

    def test_an_unlisted_lead_set_is_retried(self):
        env = {'AMUX_DISK_ALARM_TO': '', 'AMUX_URL': 'https://127.0.0.1:1'}
        verdict = self.run_alarm(45, **env)
        self.assertIn('leads_error', verdict)
        state = json.loads((self.home / '.amux' / 'disk-alarm.json').read_text())
        self.assertEqual(state['pending'], ['leads-unlisted'])

    def test_the_builder_path_reaches_homebrew_tools(self):
        sys.path.insert(0, str(SCRIPT.parent))
        import importlib.util
        spec = importlib.util.spec_from_file_location('disk_alarm', SCRIPT)
        alarm = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(alarm)
        old = os.environ.get('PATH')
        os.environ['PATH'] = '/usr/bin:/bin'  # launchd's builder PATH, roughly
        try:
            path = alarm.tool_env()['PATH'].split(':')
        finally:
            os.environ['PATH'] = old
        self.assertIn('/opt/homebrew/bin', path)
        self.assertEqual(path[:2], ['/usr/bin', '/bin'], 'existing order kept')

if __name__ == '__main__':
    unittest.main()
