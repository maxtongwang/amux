#!/usr/bin/env python3
"""Offline secret-exec transport/exec regressions; refs maxtongwang/ai-config#310."""
import http.server
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import threading
import unittest

CLI = Path(__file__).resolve().parents[1] / "amux"
SENTINEL = "obviously-fake-secret-exec-sentinel"
TOKEN = "a" * 64


class SecretExecTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        home = Path(self.temp.name)
        (home / "job-tokens").mkdir()
        token = home / "job-tokens/com.test.allowed.token"
        token.write_text(TOKEN)
        token.chmod(0o600)
        self.requests = []
        self.response = (200, {"values": {"MAX_TEST_KEY": SENTINEL}})
        test = self

        class Handler(http.server.BaseHTTPRequestHandler):
            def do_POST(self):
                body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                test.requests.append((self.path, body, self.headers["X-Amux-Worker-Token"]))
                code, data = test.response
                raw = json.dumps(data).encode()
                self.send_response(code)
                self.send_header("Content-Length", str(len(raw)))
                self.end_headers()
                self.wfile.write(raw)

            def log_message(self, *_):
                pass

        self.server = http.server.HTTPServer(("127.0.0.1", 0), Handler)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        self.addCleanup(self.server.server_close)
        self.addCleanup(self.server.shutdown)
        self.env = dict(os.environ, HOME=str(home), CC_HOME=str(home), AMUX_HOME=str(home),
                        AMUX_ISOLATED_RUN="1", AMUX_API=f"http://127.0.0.1:{self.server.server_port}")
        self.env.pop("AMUX_SESSION", None)
        self.env.pop("TMUX_PANE", None)

    def run_cli(self, *command, job="com.test.allowed", names="MAX_TEST_KEY"):
        out = subprocess.run(["bash", str(CLI), "secret-exec", "--job", job, "--names", names,
                              "--", *command], env=self.env, input="stdin survived", text=True,
                             capture_output=True, timeout=15)
        self.assertNotIn(SENTINEL, out.stdout + out.stderr)
        return out

    def test_environment_argv_stdin_and_exit(self):
        code = ('import os,sys; assert os.environ["MAX_TEST_KEY"] == ' + repr(SENTINEL) + '; '
                'assert "MAX_OTHER_KEY" not in os.environ; '
                'assert sys.argv[1:] == ["with spaces", "a\\\"b", "$(literal)"]; '
                'assert sys.stdin.read() == "stdin survived"; print("child ok")')
        out = self.run_cli(sys.executable, "-c", code, "with spaces", 'a"b', "$(literal)")
        self.assertEqual(out.returncode, 0, out.stderr)
        self.assertEqual(out.stdout, "child ok\n")
        self.assertEqual(self.requests, [("/api/vault/job-secrets",
                                         {"job": "com.test.allowed", "names": ["MAX_TEST_KEY"]}, TOKEN)])
        out = self.run_cli(sys.executable, "-c", "raise SystemExit(23)")
        self.assertEqual(out.returncode, 23)

    def test_refusals_are_clear_and_child_never_runs(self):
        for reason in ("job_identity_refused", "unknown_job", "name_not_allowed", "missing_or_wrong_group"):
            with self.subTest(reason=reason):
                self.response = (403, {"error": reason, "names": ["MAX_TEST_KEY"]})
                out = self.run_cli(sys.executable, "-c", 'print("CHILD RAN")')
                self.assertNotEqual(out.returncode, 0)
                self.assertIn(reason, out.stderr)
                self.assertIn("MAX_TEST_KEY", out.stderr)
                self.assertEqual(out.stdout, "")

    def test_server_down(self):
        self.server.shutdown()
        self.server.server_close()
        out = self.run_cli("unused-command")
        self.assertEqual(out.returncode, 1)
        self.assertIn("server_unavailable", out.stderr)
        self.assertIn("MAX_TEST_KEY", out.stderr)

    def test_unregistered_local_job(self):
        out = self.run_cli("unused-command", job="com.test.unknown")
        self.assertEqual(out.returncode, 1)
        self.assertIn("unregistered", out.stderr)
        self.assertEqual(self.requests, [])

    def test_response_values_never_echoed_on_failure(self):
        for response in ((500, {"error": SENTINEL}),
                         (200, {"values": {"MAX_TEST_KEY": SENTINEL, "MAX_OTHER_KEY": SENTINEL}}),
                         (200, {"values": {}}), (200, {"values": {"MAX_TEST_KEY": [SENTINEL]}})):
            self.response = response
            out = self.run_cli("unused-command")
            self.assertEqual(out.returncode, 1)
            self.assertEqual(out.stdout, "")

    def test_token_permissions_and_nonlocal_url_refused(self):
        path = Path(self.env["AMUX_HOME"]) / "job-tokens/com.test.allowed.token"
        path.chmod(0o644)
        self.assertIn("job_credential_permissions", self.run_cli("unused").stderr)
        path.chmod(0o600)
        # The isolated endpoint guard blocks a nonlocal endpoint before transport.
        self.env["AMUX_API"] = "http://192.0.2.1:12345"
        self.assertNotEqual(self.run_cli("unused").returncode, 0)
        self.assertEqual(self.requests, [])


if __name__ == "__main__":
    unittest.main()
