#!/usr/bin/env python3
"""Exercise shipped send/receipt functions with fixture-only HTTP and shell adapters."""
import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import unittest

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--cli', type=Path, default=Path(__file__).resolve().parents[1] / 'amux')
args = parser.parse_args()
source = args.cli.read_text()


def function(name):
    found = re.search(r'^' + re.escape(name) + r'\(\) \{\n.*?^\}\n', source, re.M | re.S)
    if not found:
        raise AssertionError(f'missing shipped function: {name}')
    return found[0]


functions = '\n'.join(function(name) for name in (
    '_send_via_api', '_await_send_receipt', '_send_settle_by_receipt'))
if re.search(r'^_report_delivery_receipt\(\)', source, re.M):
    functions += '\n' + function('_report_delivery_receipt')
probes = 0


class ReceiptMatrix(unittest.TestCase):
    def probe(self, state, path, expected, legacy=False, malformed=False):
        global probes
        probes += 1
        with tempfile.TemporaryDirectory(prefix='native-cli-') as temp:
            root = Path(temp)
            payload = {'ok': True, 'accepted': True, 'message': 'fixture'}
            if not legacy:
                payload['delivery_receipt'] = None if malformed else {'state': state, 'transport': 'fixture_transport'}
            responses = {'first': [], 'retry': [''], 'late': ['', '', '']}[path] + [json.dumps(payload)]
            (root / 'responses').write_text('\n'.join(responses) + '\n')
            (root / 'calls').write_text('0\n')
            script = root / 'probe.sh'
            script.write_text(functions + r'''
_warn_second_person_resend() { :; }
tmux_name() { echo fixture; }
_flush_unstamped_ledger() { echo flushed >> "$FIXTURE/flushed"; }
sleep() { :; }
_curl() {
  local n
  n=$(cat "$FIXTURE/calls")
  n=$((n+1))
  echo "$n" > "$FIXTURE/calls"
  sed -n "${n}p" "$FIXTURE/responses"
}
_send_via_api fixture-receiver prose
''')
            env = dict(os.environ, FIXTURE=temp, AMUX_API='fixture://injected', CC_HOME=temp,
                       AMUX_SEND_PENDING_WAIT_S='0', RED='', RESET='')
            result = subprocess.run(['bash', str(script)], env=env, capture_output=True, text=True, timeout=10)
            self.assertEqual(result.returncode, expected, result.stdout + result.stderr)
            self.assertEqual(int((root / 'calls').read_text()), len(responses), 'no re-post after a receipt')
            combined = result.stdout + result.stderr
            if legacy:
                self.assertIn('sent to fixture-receiver', combined)
            else:
                self.assertNotIn('sent to fixture-receiver', combined)
                self.assertIn('delivery_receipt:', combined)
                if expected or state == 'written_unconfirmed':
                    self.assertIn('Do NOT resend', combined)
                else:
                    self.assertIn('RECEIVED' if state == 'consumed' else 'ADMITTED', combined)
            self.assertEqual((root / 'flushed').exists(), expected == 0)

    def test_unconfirmed_and_hold(self):
        for state in ('reserved', 'written_unconfirmed', 'held', 'unknown', 'uncertain'):
            for path in ('first', 'retry', 'late'):
                with self.subTest(state=state, path=path):
                    self.probe(state, path, 0 if state == 'written_unconfirmed' else 1)

    def test_native_dispositions(self):
        for state in ('denied', 'refused', 'expired', 'dropped'):
            for path in ('first', 'retry', 'late'):
                with self.subTest(state=state, path=path):
                    self.probe(state, path, 1)

    def test_positive_evidence(self):
        for state in ('admitted', 'consumed'):
            for path in ('first', 'retry', 'late'):
                with self.subTest(state=state, path=path):
                    self.probe(state, path, 0)

    def test_legacy_acceptance_unchanged(self):
        for path in ('first', 'retry', 'late'):
            with self.subTest(path=path):
                self.probe(None, path, 0, legacy=True)

    def test_unknown_or_malformed_receipt_is_unconfirmed(self):
        for malformed in (False, True):
            for path in ('first', 'retry', 'late'):
                with self.subTest(malformed=malformed, path=path):
                    self.probe('future-state', path, 1, malformed=malformed)


class AbandonAction(unittest.TestCase):
    def test_shipped_abandon_cli_uses_scoped_owner_gated_api_and_reports_failure(self):
        global probes
        for status in ('ok', 'denied', 'unavailable'):
            probes += 1
            with tempfile.TemporaryDirectory(prefix='native-abandon-cli-') as temp:
                (Path(temp) / 'auth_token').write_text('fixture-owner-bearer\n')
                script = Path(temp) / 'probe.sh'
                script.write_text(function('cmd_delivery') + r'''
resolve_session() { echo fixture-worker; }
_curl() {
  printf '%s\n' "$@" > "$FIXTURE/request"
  case "$FIXTURE_STATUS" in
    ok) echo '{"ok":true,"delivery_receipt":{"state":"uncertain","status_note":"may have been delivered"}}' ;;
    denied) echo '{"ok":false,"code":"native_abandon_owner_only"}' ;;
    unavailable) return 22 ;;
  esac
}
cmd_delivery abandon fixture-worker fixture-attempt
''')
                result = subprocess.run(['bash', str(script)], env=dict(os.environ, FIXTURE=temp,
                    FIXTURE_STATUS=status, AMUX_API='fixture://injected', CC_HOME=temp), capture_output=True, text=True, timeout=10)
                self.assertEqual(result.returncode, 0 if status == 'ok' else 1, result.stdout + result.stderr)
                request = (Path(temp) / 'request').read_text()
                self.assertIn('fixture://injected/api/workers/fixture-worker/steer', request)
                self.assertIn('"action": "abandon"', request)
                self.assertIn('"attempt_id": "fixture-attempt"', request)
                self.assertIn('X-Amux-Worker:', request)
                self.assertIn('Authorization: Bearer fixture-owner-bearer', request)
                if status == 'ok':
                    self.assertIn('may have been delivered', result.stdout)



result = unittest.TextTestRunner(verbosity=1).run(unittest.TestSuite([unittest.defaultTestLoader.loadTestsFromTestCase(case) for case in (ReceiptMatrix, AbandonAction)]))
print(f'CLI receipt matrix: {probes} fixture probes, {len(result.failures)} failures, {len(result.errors)} errors')
raise SystemExit(not result.wasSuccessful())
