#!/usr/bin/env python3
"""Run actual storage queries; AMUX_DELEGATION_SOURCE_ROOT selects a baseline.

SQLite evidence only. The Rust route suite proves recorder/resolver wiring.
"""
import importlib.util
import os
from pathlib import Path
import sqlite3
import unittest

ROOT = Path(__file__).resolve().parents[1]
SOURCE = Path(os.environ.get("AMUX_DELEGATION_SOURCE_ROOT", ROOT))
REQUESTS = SOURCE / "crates/amux-server/src/api/delegation_requests.rs"
GRANTS = SOURCE / "crates/amux-server/src/api/delegation_grants.rs"
spec = importlib.util.spec_from_file_location("request_schema", ROOT / "scripts/test-delegation-request-schema.py")
schema = importlib.util.module_from_spec(spec)
spec.loader.exec_module(schema)


class ReplyWindowStorage(unittest.TestCase):
    def setUp(self):
        self.conn = sqlite3.connect(":memory:")
        self.addCleanup(self.conn.close)
        schema.migrate(self.conn)
        self.candidates = schema.source_sql(REQUESTS, "SELECT code,reply_incarnation,(SELECT ts FROM cmd_history WHERE id=shown_row) FROM _amux_delegation_grants")
        self.current = 'SELECT COALESCE(MAX(id),0)' in REQUESTS.read_text()
        self.boundary = schema.source_sql(REQUESTS, "SELECT COALESCE(MAX(id),0)" if self.current else "SELECT COALESCE(MAX(reply_row),0)")

    def message(self, text, ty="user", origin="pane", delivery="pane", session="desk", process=None):
        return self.conn.execute("INSERT INTO cmd_history(session,text,type,origin,delivery,ts,process_origin) VALUES(?,?,?,?,?,2000,?)", [session,text,ty,origin,delivery,process]).lastrowid

    def request(self, code="REQ1", displayed=True, refusal=None):
        question = ["Approval request " + code, "desk", 1000, "actor", "inc-desk"]
        ask = self.conn.execute(schema.source_sql(GRANTS, "INSERT INTO cmd_history(text,type,session,ts,origin,delivery,delegation_session_incarnation)"), question).lastrowid
        args = [code,"actor","lead","set_config","worker","PATCH","/api/sessions/worker/config","{}","question",None,1,86401,"cite","inc-worker","inc-actor","desk","inc-desk",ask,"{}",question[0]]
        self.conn.execute(schema.source_sql(GRANTS, "INSERT INTO _amux_delegation_grants(code,actor,class,action,target,method,path,body,sentence,why,created_at,expires_at,status,mode,target_incarnation,actor_incarnation"), args)
        shown = ask
        if displayed and self.current:
            query = schema.source_sql(REQUESTS, "INSERT INTO cmd_history(text,type,session,ts,origin,delivery,delegation_session_incarnation,repeat_of,delivered_at)")
            shown = self.conn.execute(query, [question[0],"desk",1500,"actor","inc-desk",ask]).lastrowid
            self.conn.execute("UPDATE _amux_delegation_grants SET shown_row=? WHERE code=?", [shown,code])
        self.conn.execute("UPDATE _amux_delegation_grants SET resolution_code=? WHERE code=?", [refusal,code])
        return shown

    def eligible(self, reply):
        latest = self.conn.execute(self.boundary, ["desk", reply]).fetchone()[0]
        return self.conn.execute(self.candidates, ["desk",latest,reply]).fetchall()

    def test_probe_unrelated_yes_without_display_evidence_funds_hidden_request(self):
        self.request(displayed=False)
        self.assertEqual(self.eligible(self.message("yes")), [])

    def test_probe_unprocessed_intervening_owner_row_does_not_leave_window_open(self):
        self.request()
        self.message("what is the build status?")  # No resolution / on_row.
        self.assertEqual(self.eligible(self.message("yes")), [])

    def test_immediate_yes_funds_latest_display_only(self):
        self.request("OLD")
        self.request("NEW")
        self.assertEqual([r[0] for r in self.eligible(self.message("yes"))], ["NEW"])

    def test_owner_output_closes_window(self):
        for ty in ["assistant", "session"]:
            with self.subTest(ty=ty):
                self.request("OUTPUT" + ty)
                self.message("another question", ty, "desk", "direct")
                self.assertEqual(self.eligible(self.message("yes")), [])

    def test_unprocessed_owner_send_closes_window_for_every_route_shape(self):
        for ty in ["user", "chat"]:
            for delivery in ["direct", "queued"]:
                for process in ["outside", "remote", "remote-forwarded"]:
                    with self.subTest(ty=ty, delivery=delivery, process=process):
                        self.request(ty+delivery+process)
                        self.message("wait", ty, "", delivery, process=process)
                        self.assertEqual(self.eligible(self.message("yes")), [])

    def test_peer_notice_does_not_close_window(self):
        self.request()
        self.message("notice", "session", "lead", "direct", process="worker")
        self.assertEqual([r[0] for r in self.eligible(self.message("yes"))], ["REQ1"])

    def test_uncertain_or_discarded_owner_input_closes_window_without_resolution(self):
        for delivery in ["approval_input_uncertain", "direct", "queued", "unknown"]:
            with self.subTest(delivery=delivery):
                self.request("UNCERTAIN" + delivery)
                self.message("discarded", delivery=delivery, origin="", process=None)
                self.assertEqual(self.eligible(self.message("yes")), [])

    def test_refused_request_is_never_a_candidate_without_a_boundary(self):
        for index, code in enumerate(["request_reply_not_affirmative", "request_ambiguous", "request_reply_identity_changed", "request_reply_outside_window", ""]):
            with self.subTest(code=code):
                shown = self.request("REF" + str(index), refusal=code)
                reply = self.message("yes")
                # Pass the display directly: isolate refusal from next-input guard.
                self.assertEqual(self.conn.execute(self.candidates,["desk",shown,reply]).fetchall(), [])
                self.conn.execute("UPDATE _amux_delegation_grants SET resolution_code=NULL WHERE shown_row=?", [shown])
                self.assertEqual(len(self.conn.execute(self.candidates,["desk",shown,reply]).fetchall()), 1)


if __name__ == "__main__":
    unittest.main(verbosity=2)
