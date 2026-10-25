#!/usr/bin/env python3
"""Verify native-ledger upgrades and queue guards using only in-memory SQLite."""
from pathlib import Path
import sqlite3
import unittest

ROOT = Path(__file__).resolve().parents[1]
MIGRATIONS = ROOT / 'crates/amux-server/migrations'


def fixture():
    conn = sqlite3.connect(':memory:')
    conn.row_factory = sqlite3.Row
    conn.execute('CREATE TABLE steering_queue(id TEXT PRIMARY KEY, session TEXT, text TEXT, queued_at REAL)')
    conn.executescript((MIGRATIONS / '0124_native_delivery.sql').read_text())
    conn.execute("INSERT INTO steering_queue(id,session,text,queued_at) VALUES('old','receiver','body',1)")
    conn.execute("INSERT INTO steering_queue(id,session,text,queued_at) VALUES('later','receiver','later',2)")
    fields = {
        'attempt_id': 'attempt', 'session': 'receiver', 'source_key': 'queue:old',
        'message_id': 'message', 'queue_id': 'old', 'card_id': 'card', 'sender': 'peer',
        'msg_id': 'native-message', 'command_uuid': 'command', 'endpoint': '/fixture/socket',
        'endpoint_pid': 123, 'endpoint_uid': 501, 'endpoint_start': 'kernel-token',
        'endpoint_session': 'conversation', 'executable_sha256': 'hash', 'registry_sha256': 'registry',
        'relay_pid': 456, 'relay_start': 'Thu Oct 22 01:02:03 2026', 'body_sha256': 'body-hash',
        'state': 'unknown', 'reason': 'server_restart', 'created_at': 100, 'updated_at': 200,
        'write_attempted_at': 150, 'next_observation_at': 210,
    }
    conn.execute(f"INSERT INTO native_delivery_attempts({','.join(fields)}) VALUES({','.join('?' for _ in fields)})",
                 tuple(fields.values()))
    conn.executescript((MIGRATIONS / '0125_native_uncertainty.sql').read_text())
    return conn, fields


class NativeSchema(unittest.TestCase):
    def test_upgrade_preserves_attempt_ids_evidence_and_cursor(self):
        conn, fields = fixture()
        row = conn.execute('SELECT * FROM native_delivery_attempts').fetchone()
        for field, value in fields.items():
            self.assertEqual(row[field], value, field)
        self.assertEqual(row['unknown_since'], 200)
        self.assertEqual(row['observation_count'], 0)
        self.assertEqual(conn.execute('PRAGMA integrity_check').fetchone()[0], 'ok')

    def test_unknown_is_ordered_and_cannot_be_cleared_or_rewritten(self):
        conn, _ = fixture()
        self.assertEqual(conn.execute('SELECT id FROM native_delivery_pending_queue').fetchall()[0][0], 'old')
        with self.assertRaises(sqlite3.IntegrityError):
            conn.execute("DELETE FROM steering_queue WHERE id='old'")
        with self.assertRaises(sqlite3.IntegrityError):
            conn.execute("UPDATE steering_queue SET text='changed' WHERE id='old'")

    def test_uncertain_releases_ordering_but_never_permits_duplicate_source(self):
        conn, fields = fixture()
        conn.execute("UPDATE native_delivery_attempts SET state='uncertain',reason='unknown_hold_deadline'")
        self.assertEqual(conn.execute('SELECT count(*) FROM native_delivery_pending_queue').fetchone()[0], 0)
        conn.execute("DELETE FROM steering_queue WHERE id='old'")
        self.assertEqual(conn.execute('SELECT id FROM steering_queue').fetchone()[0], 'later')
        duplicate = dict(fields, attempt_id='another-attempt', msg_id='another-message', command_uuid='another-command')
        with self.assertRaisesRegex(sqlite3.IntegrityError, 'native_delivery_attempts.session, native_delivery_attempts.source_key'):
            conn.execute(f"INSERT INTO native_delivery_attempts({','.join(duplicate)}) VALUES({','.join('?' for _ in duplicate)})",
                         tuple(duplicate.values()))
        self.assertEqual(conn.execute('SELECT count(*) FROM native_delivery_attempts').fetchone()[0], 1)

    def test_invalid_state_still_fails_at_the_database_boundary(self):
        conn, _ = fixture()
        with self.assertRaises(sqlite3.IntegrityError):
            conn.execute("UPDATE native_delivery_attempts SET state='delivered-probably'")


if __name__ == '__main__':
    unittest.main()
