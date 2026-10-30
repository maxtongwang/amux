#!/usr/bin/env python3
"""Offline secret-exec transport/exec regressions; refs maxtongwang/ai-config#310."""
import http.server
import json
import os
from pathlib import Path
import ssl
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
            def handle(self):
                try:
                    super().handle()
                except ConnectionResetError:
                    # Pin refusal closes TLS before sending an HTTP request.
                    pass

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
        # Exercise the verb with the same Python interpreter as this test run.
        bin_dir = home / "bin"
        bin_dir.mkdir()
        (bin_dir / "python3").symlink_to(sys.executable)
        self.env["PATH"] = str(bin_dir) + os.pathsep + self.env["PATH"]
        self.env.pop("AMUX_SESSION", None)
        self.env.pop("TMUX_PANE", None)

    def run_cli(self, *command, job="com.test.allowed", names="MAX_TEST_KEY"):
        out = subprocess.run(["bash", str(CLI), "secret-exec", "--job", job, "--names", names,
                              "--", *command], env=self.env, input="stdin survived", text=True,
                             capture_output=True, timeout=15)
        self.assertNotIn(SENTINEL, out.stdout + out.stderr)
        return out

    def enable_tls(self, san="DNS:localhost,IP:127.0.0.1"):
        tls = Path(self.env["AMUX_HOME"]) / "tls"
        tls.mkdir()
        config = tls / "openssl.cnf"
        config.write_text("[req]\ndistinguished_name=dn\nx509_extensions=ext\nprompt=no\n"
                          f"[dn]\nCN=amux.example.test\n[ext]\nsubjectAltName={san}\n")
        subprocess.run(["openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes",
                        "-days", "1", "-config", str(config), "-keyout", str(tls / "key.pem"),
                        "-out", str(tls / "cert.pem")], check=True, capture_output=True)
        context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        context.load_cert_chain(tls / "cert.pem", tls / "key.pem")
        self.server.socket = context.wrap_socket(self.server.socket, server_side=True)
        return tls

    def test_discovered_https_without_amux_api(self):
        self.enable_tls(san="DNS:amux.example.test")
        # A scratch endpoint exercises production discovery; isolation deliberately
        # ignores endpoint.json, so disable that guard only in this scratch child.
        self.env["AMUX_ISOLATED_RUN"] = "0"
        self.env.pop("AMUX_API", None)
        self.env.pop("AMUX_URL", None)
        endpoint = Path(self.env["CC_HOME"]) / "endpoint.json"
        endpoint.write_text(json.dumps({"canonical_url": f"https://localhost:{self.server.server_port}"}))
        out = self.run_cli(sys.executable, "-c", "print('TLS child ok')")
        self.assertEqual(out.returncode, 0, out.stderr)
        self.assertEqual(out.stdout, "TLS child ok\n")
        self.assertEqual(len(self.requests), 1)

    def test_explicit_api_wins_over_discovery(self):
        self.env["AMUX_URL"] = "http://127.0.0.1:1"
        out = self.run_cli(sys.executable, "-c", "print('override ok')")
        self.assertEqual(out.returncode, 0, out.stderr)
        self.assertEqual(out.stdout, "override ok\n")

    def test_https_verifies_certificate_and_active_bundle(self):
        tls = self.enable_tls()
        self.env["AMUX_API"] = f"https://localhost:{self.server.server_port}"
        leaf = (tls / "cert.pem").read_bytes()
        (tls / "connection.pem").write_bytes(leaf + (tls / "key.pem").read_bytes())
        (tls / "cert.pem").unlink()
        out = self.run_cli(sys.executable, "-c", "print('trusted')")
        self.assertEqual(out.returncode, 0, out.stderr)
        self.requests.clear()
        # A readable but unrelated trust anchor must refuse before any HTTP request.
        other_key = tls / "other-key.pem"
        subprocess.run(["openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes",
                        "-days", "1", "-config", str(tls / "openssl.cnf"),
                        "-keyout", str(other_key), "-out", str(tls / "connection.pem")],
                       check=True, capture_output=True)
        other = (tls / "connection.pem").read_bytes()
        # A bundle pins its FIRST certificate, not any certificate in its chain.
        (tls / "connection.pem").write_bytes(other + leaf)
        out = self.run_cli("unused-command")
        self.assertEqual(out.returncode, 1)
        self.assertIn("server_certificate_mismatch", out.stderr)
        self.assertEqual(self.requests, [])
        (tls / "connection.pem").write_bytes(leaf + other + (tls / "key.pem").read_bytes())
        out = self.run_cli(sys.executable, "-c", "print('chain trusted')")
        self.assertEqual(out.returncode, 0, out.stderr)
        self.assertEqual(out.stdout, "chain trusted\n")
        self.assertEqual(len(self.requests), 1)

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
        # Invoke the verb after sourcing so the isolation guard cannot mask its check.
        out = subprocess.run(["bash", "-c", 'source "$1"; AMUX_API=http://192.0.2.1:12345; cmd_secret_exec --job com.test.allowed --names MAX_TEST_KEY -- unused',
                              "test", str(CLI)], env=self.env, capture_output=True, text=True, timeout=15)
        self.assertEqual(out.returncode, 1)
        self.assertIn("local_server_url_required", out.stderr)
        self.assertEqual(self.requests, [])


if __name__ == "__main__":
    unittest.main()
