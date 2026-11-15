#!/usr/bin/env python3
"""Test prepare-launch's actual CLI handler with an isolated HTTP transport fake."""
import pathlib
import subprocess
import tempfile
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[1]


class PrepareLaunchCLI(unittest.TestCase):
    def invoke(self, arguments, transport_status=0):
        source = (ROOT / "amux").read_text()
        handler = source.split("cmd_prepare_launch() {", 1)[1].split("\n_start_via_api() {", 1)[0]
        with tempfile.TemporaryDirectory() as directory:
            pathlib.Path(directory, "auth_token").write_text("fixture-owner-credential\n")
            script = r'''
set -euo pipefail
CC_HOME="$1"; shift
AMUX_API=http://127.0.0.1:9
AMUX_WORKER=fixture-lead
AMUX_SESSION=caller
transport_status="$1"; shift
_die_status=2
die() { echo "$*" >&2; exit "$_die_status"; }
_curl() {
  local headers
  headers="$(cat)"
  [[ "$headers" == 'Authorization: Bearer fixture-owner-credential' ]] || return 90
  [[ "$*" != *fixture-owner-credential* ]] || return 91
  [[ "$*" == *'X-Amux-Session: fixture-lead'* ]] || return 92
  [[ "$*" == *'/api/sessions/probe/prepare-launch'* ]] || return 93
  [[ "$transport_status" == 0 ]] || return "$transport_status"
  printf '%s\n' '{"name":"probe","env":{"AMUX_WORKER_TOKEN":"fixture-child-credential"},"run_identity":"run","token_env_key":"AMUX_WORKER_TOKEN","expires_at":1}'
}
'''
            script += "cmd_prepare_launch() {" + handler
            script += '\ncmd_prepare_launch "$@"\n'
            return subprocess.run(
                ["bash", "-c", script, "test", directory, str(transport_status), *arguments],
                capture_output=True, text=True, check=False,
            )

    def test_success_preserves_credential_json_on_stdout_only(self):
        result = self.invoke(["probe", "--json"])
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('"token_env_key":"AMUX_WORKER_TOKEN"', result.stdout)
        self.assertNotIn("credential", result.stderr)

    def test_http_refusal_has_no_credential_output(self):
        result = self.invoke(["probe", "--json"], 22)
        self.assertEqual(result.returncode, 1)
        self.assertEqual(result.stdout, "")
        self.assertIn("refused", result.stderr)

    def test_transport_failure_is_uncertain(self):
        result = self.invoke(["probe", "--json"], 28)
        self.assertEqual(result.returncode, 5)
        self.assertEqual(result.stdout, "")
        self.assertIn("uncertain", result.stderr)

    def test_missing_json_and_invalid_name_refuse_before_transport(self):
        for arguments in (["probe"], ["../probe", "--json"], ["probe", "--json", "extra"]):
            with self.subTest(arguments=arguments):
                result = self.invoke(arguments)
                self.assertEqual(result.returncode, 2)
                self.assertEqual(result.stdout, "")


if __name__ == "__main__":
    unittest.main()
