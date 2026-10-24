#!/usr/bin/env python3
"""Exercise B1's actual storage queries without cargo (no HTTP/Rust execution)."""
import sqlite3
import unittest
from pathlib import Path
import importlib.util

ROOT = Path(__file__).resolve().parents[1]
REQUESTS = ROOT / "crates/amux-server/src/api/delegation_requests.rs"
spec = importlib.util.spec_from_file_location(
    "request_schema", ROOT / "scripts/test-delegation-request-schema.py")
schema = importlib.util.module_from_spec(spec)
spec.loader.exec_module(schema)


class ReplyWindowStorage(unittest.TestCase):
    def setUp(self):
        self.conn = sqlite3.connect(":memory:")
        self.addCleanup(self.conn.close)
        schema.migrate(self.conn)
        self.candidates = schema.source_sql(
            REQUESTS, "SELECT code,reply_incarnation,(SELECT ts FROM cmd_history WHERE id=shown_row) FROM _amux_delegation_grants")

    def previous(self, session, reply):
        query = schema.source_sql(REQUESTS, "SELECT COALESCE(MAX(reply_row),0)")
        return self.conn.execute(query, [session, reply]).fetchone()[0]

    def message(self, text, ty="user", origin="pane", delivery="pane", session="desk"):
        return self.conn.execute(
            "INSERT INTO cmd_history(session,text,type,origin,delivery,ts) VALUES(?,?,?,?,?,1000)",
            [session, text, ty, origin, delivery]).lastrowid

    def request(self, code, shown, refusal=None):
        self.conn.execute(
            "INSERT INTO _amux_delegation_grants(code,actor,class,action,target,method,path,"
            "body,sentence,created_at,expires_at,status,reply_session,reply_incarnation,"
            "shown_row,resolution_code) VALUES(?,'lead','lead','set_tags','worker','PATCH',"
            "'/api/sessions/worker/config','{}','question',1,86401,'pending','desk','inc-desk',?,?)",
            [code, shown, refusal])

    def resolution(self, row, code, session="desk"):
        query = schema.source_sql(REQUESTS, "INSERT INTO _amux_delegation_resolutions(")
        self.conn.execute(query, [row, None, session, code, "stored outcome", 1])

    def test_stored_boundary_survives_peer_echo_for_every_outcome(self):
        # No grant refusal protects this test: only the stored boundary can
        # exclude a still-unclassified request in an already closed window.
        for outcome in ["request_resolved", "request_reply_not_affirmative",
                        "request_ambiguous", "request_no_candidate"]:
            with self.subTest(outcome=outcome):
                self.conn.execute("DELETE FROM _amux_delegation_grants")
                self.conn.execute("DELETE FROM _amux_delegation_resolutions")
                self.conn.execute("DELETE FROM cmd_history")
                shown = self.message("question", "session", "lead", "approval_request")
                self.request("old", shown)
                no = self.message("no")
                self.resolution(no, outcome)
                for ty in ["session", "user", "chat", "steering"]:
                    self.message("no", ty, "lead", "direct")
                yes = self.message("yes")
                self.assertEqual(self.previous("desk", yes), no)
                self.assertEqual(self.conn.execute(
                    self.candidates, ["desk", self.previous("desk", yes), yes]).fetchall(), [])
                # Retyping, projecting or pruning the old row cannot erase a
                # committed resolution. Session and strict row bounds still hold.
                self.conn.execute("DELETE FROM cmd_history WHERE id=?", [no])
                self.assertEqual(self.previous("desk", yes), no)
                self.assertEqual(self.previous("elsewhere", yes), 0)
                self.assertEqual(self.previous("desk", no), 0)
                self.resolution(yes, "request_no_candidate")
                self.assertEqual(self.previous("desk", yes), no)
                self.assertEqual(self.previous("desk", yes+1), yes)

    def test_refused_request_is_never_a_candidate_without_a_boundary(self):
        for code in ["request_reply_not_affirmative", "request_ambiguous",
                     "request_reply_identity_changed", "request_reply_outside_window", ""]:
            with self.subTest(code=code):
                self.conn.execute("DELETE FROM _amux_delegation_grants")
                shown = self.message("question", "session", "lead", "approval_request")
                self.request("refused", shown, code)
                yes = self.message("yes")
                self.assertEqual(self.conn.execute(
                    self.candidates, ["desk", 0, yes]).fetchall(), [])
                # Positive control: the same query must still see an eligible
                # pending row when only the refusal is removed.
                self.conn.execute("UPDATE _amux_delegation_grants SET resolution_code=NULL")
                self.assertEqual(self.conn.execute(
                    self.candidates, ["desk", 0, yes]).fetchall(), [("refused", "inc-desk", 1000)])


if __name__ == "__main__":
    unittest.main(verbosity=2)
