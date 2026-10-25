#!/usr/bin/env python3
"""Test the shipped Bash dispatch with private homes and a fake HTTP transport."""
import json
import os
from pathlib import Path
import subprocess
import sqlite3
import tempfile
import unittest

REPO = Path(__file__).resolve().parents[1]
GRANT = "vault-owner-global-write-v1"


class VaultGrantsCli(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="amux-vault-grants-cli-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        bin_dir = self.root / "bin"
        bin_dir.mkdir()
        self.calls = self.root / "calls.jsonl"
        self.env = {k: v for k, v in os.environ.items()
                    if not k.startswith(("AMUX_", "CC_", "TMUX"))}
        self.env.update(HOME=str(self.root), CC_HOME=str(self.root / "amux"),
                        AMUX_HOME=str(self.root / "amux"), CURL_HOME=str(self.root),
                        AMUX_API="http://vault-fixture.invalid", PATH=f"{bin_dir}:{os.environ['PATH']}",
                        VAULT_CLI_CALLS=str(self.calls))
        curl = bin_dir / "curl"
        curl.write_text('''#!/usr/bin/env python3
import json, os, sys
with open(os.environ["VAULT_CLI_CALLS"], "a") as f:
    f.write(json.dumps(sys.argv[1:]) + "\\n")
url = sys.argv[-1]
if url == "http://vault-fixture.invalid/api/vault/grants":
    print(json.dumps({"measured": True, "grants": [{"id": "vault-owner-global-write-v1", "grantee": "mrpopo", "grantee_id": "wrk_fixture", "identity_kind": "worker_id", "scope": "global", "created_at": 1792896858.0, "revoked_at": None, "unexpected": "cli-canary-must-not-print"}]}))
elif url.endswith("/vault-owner-global-write-v1/revoke") or url.endswith("/vault-owner-global-write-v1/regrant"):
    print(json.dumps({"ok": True}))
else:
    sys.exit(22)
''')
        curl.chmod(0o755)
        tmux = bin_dir / "tmux"
        tmux.write_text("#!/bin/sh\nexit 1\n")
        tmux.chmod(0o755)

    def cli(self, *args):
        return subprocess.run(["bash", str(REPO / "amux"), "vault", "grants", *args],
                              env=self.env, capture_output=True, text=True, timeout=20)

    def test_vault_grants_listing(self):
        out = self.cli()
        self.assertEqual(out.returncode, 0, out.stderr)
        for expected in (GRANT, "mrpopo", "wrk_fixture", "global", "1792896858"):
            self.assertIn(expected, out.stdout)
        self.assertNotIn("cli-canary-must-not-print", out.stdout)
        structured = self.cli("--json")
        self.assertEqual(structured.returncode, 0, structured.stderr)
        data = json.loads(structured.stdout)
        self.assertEqual(data["timestamp_unit"], "unix_seconds")
        self.assertEqual(len(data["grants"][0]), 6)
        self.assertNotIn("cli-canary-must-not-print", structured.stdout)
        calls = [json.loads(s) for s in self.calls.read_text().splitlines()]
        self.assertTrue(all(c[-1] == "http://vault-fixture.invalid/api/vault/grants" for c in calls))

    def test_vault_grants_preserves_worker_origin_and_errors(self):
        sessions = self.root / "amux" / "sessions"
        sessions.mkdir(parents=True, exist_ok=True)
        (sessions / "peer.env").write_text("CC_NAME=peer\n")
        self.env.update(AMUX_WORKER="peer", AMUX_SESSION="peer")
        for action in ("revoke", "regrant"):
            out = self.cli(action, GRANT)
            self.assertEqual(out.returncode, 0, out.stderr)
            args = json.loads(self.calls.read_text().splitlines()[-1])
            self.assertIn("X-Amux-Worker: peer", args)
            self.assertIn("POST", args)
        self.assertNotEqual(self.cli("revoke", "../bad").returncode, 0)
        self.assertNotEqual(self.cli("revoke", "missing").returncode, 0)
        self.assertNotEqual(self.cli("--invalid").returncode, 0)


class VaultGrantSeed(unittest.TestCase):
    def test_seed_is_idempotent_and_keeps_revocation(self):
        sql = (REPO / "crates/amux-server/migrations/0126_vault_write_grant.sql").read_text()
        with sqlite3.connect(":memory:") as conn:
            conn.executescript(sql)
            conn.execute("UPDATE vault_write_grants SET grantee_id='pinned-id', incarnation='pinned-inc', revoked_at=123")
            before = conn.execute("SELECT * FROM vault_write_grants").fetchall()
            conn.executescript(sql)
            conn.executescript(sql)
            self.assertEqual(conn.execute("SELECT * FROM vault_write_grants").fetchall(), before)
            row = conn.execute("SELECT id, grantee, scope, authority, created_by, created_at FROM vault_write_grants").fetchone()
            self.assertEqual(row[:3], (GRANT, "configured_owner", "global"))
            self.assertIn('"Grant vault writes"', row[3])
            self.assertIn("<19:49 context>", row[3])
            self.assertIn("<19:52 context>", row[3])
            self.assertIn("PR <number>", row[4])
            self.assertEqual(row[5], 1792896858.0)


if __name__ == "__main__":
    unittest.main()
