#!/usr/bin/env python3
"""Verify 0129 constraints and SIGKILL atomicity using the shipped SQL (no cargo).

This probes SQLite transactions, not the HTTP server or handler side effects.
"""
import argparse
import json
from pathlib import Path
import re
import sqlite3
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[1]
MIGRATIONS = ROOT / "crates/amux-server/migrations"


def source_sql(path, prefix):
    """Require one actual production query; absent/ambiguous input is a failure."""
    text = path.read_text()
    start = '"' + prefix
    assert text.count(start) == 1, (prefix, text.count(start))
    literal = re.match(r'"(?:[^"\\]|\\.)*"', text[text.index(start):], re.DOTALL)
    assert literal is not None, prefix
    return json.loads(re.sub(r'\\\n\s*', '', literal.group()))


def migrate(conn):
    conn.execute("CREATE TABLE cmd_history(id INTEGER PRIMARY KEY,session TEXT,text TEXT,type TEXT,origin TEXT,delivery TEXT,process_origin TEXT,ts INTEGER,repeat_of INTEGER,delivered_at INTEGER)")
    for number in range(125, 130):
        files = list(MIGRATIONS.glob(f"{number:04}_*.sql"))
        assert len(files) == 1, files
        sql = files[0].read_text()
        for table, column in re.findall(r'^-- ADDCOL: (\S+) (.+)$', sql, re.MULTILINE):
            conn.execute(f"ALTER TABLE {table} ADD COLUMN {column}")
        conn.executescript(sql)
    conn.commit()


def expect_constraint(conn, query, args):
    try:
        conn.execute(query, args)
    except sqlite3.IntegrityError:
        return
    raise AssertionError("expected a uniqueness refusal")


def crash_probe(database, query, args, commit, followup=None):
    # Synchronize on a receipt from a separate real process, then SIGKILL it.
    code = """
import json, sqlite3, sys
conn=sqlite3.connect(sys.argv[1])
conn.execute('BEGIN IMMEDIATE')
conn.execute(sys.argv[2],json.loads(sys.argv[3]))
if sys.argv[4]=='commit':
    if len(sys.argv)>5:
        query,args=json.loads(sys.argv[5])
        args[1]=conn.execute('SELECT last_insert_rowid()').fetchone()[0]
        conn.execute(query,args)
    conn.commit()
print('boundary-ready',flush=True)
sys.stdin.readline()
"""
    argv = [sys.executable, "-c", code, str(database), query, json.dumps(args), "commit" if commit else "rollback"]
    if followup is not None:
        argv.append(json.dumps(followup))
    child = subprocess.Popen(argv,
                             stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
    try:
        assert child.stdout.readline().strip() == "boundary-ready"
        child.kill()
        assert child.wait(timeout=10) < 0
    finally:
        if child.poll() is None:
            child.kill()
            child.wait(timeout=10)
        child.stdin.close()
        child.stdout.close()


def main():
    argparse.ArgumentParser(description=__doc__).parse_args()
    grants = ROOT / "crates/amux-server/src/api/delegation_grants.rs"
    requests = ROOT / "crates/amux-server/src/api/delegation_requests.rs"
    message = source_sql(grants, "INSERT INTO cmd_history(text,type,session,ts,origin,delivery,delegation_session_incarnation)")
    request = source_sql(grants, "INSERT INTO _amux_delegation_grants(code,actor,class,action,target,method,path,body,sentence,why,created_at,expires_at,status,mode,target_incarnation,actor_incarnation")
    resolution = source_sql(requests, "INSERT INTO _amux_delegation_resolutions(")
    candidates = source_sql(requests, "SELECT code,reply_incarnation,(SELECT ts FROM cmd_history WHERE id=shown_row) FROM _amux_delegation_grants")
    claim = source_sql(grants, "UPDATE _amux_delegation_grants SET status='applying', decided_by=")
    with tempfile.TemporaryDirectory(prefix="amux-request-schema-") as tmp:
        database = Path(tmp) / "requests.db"
        with sqlite3.connect(database) as conn:
            migrate(conn)
        # Death after the question insert, before the request, leaves neither.
        question = ["Approval request REQ1", "desk", 1000, "actor", "inc-desk"]
        crash_probe(database, message, question, False)
        with sqlite3.connect(database) as conn:
            assert conn.execute("SELECT COUNT(*) FROM cmd_history").fetchone()[0] == 0
            assert conn.execute("SELECT COUNT(*) FROM _amux_delegation_grants").fetchone()[0] == 0
            shown = conn.execute(message, question).lastrowid
            row = ["REQ1", "actor", "mrpopo", "set_config:send_allow", "worker", "PATCH", "/api/sessions/worker/config", '{"send_allow":"one"}', "exact change", None, 1, 86401, "cite", "inc-worker", "inc-actor", "desk", "inc-desk", shown, '{"body":{"send_allow":"one"}}', question[0]]
            conn.execute(request, row)
            assert conn.execute(candidates, ["desk", 0, shown+1]).fetchall() == []
            display = source_sql(requests, "INSERT INTO cmd_history(text,type,session,ts,origin,delivery,delegation_session_incarnation,repeat_of,delivered_at)")
            conn.commit()
        display_args = [question[0], "desk", 1500, "actor", "inc-desk", shown]
        binding = source_sql(requests, "UPDATE _amux_delegation_grants SET shown_row=?2,resolution_note=")
        crash_probe(database, display, display_args, False, [binding, ["REQ1", None]])
        with sqlite3.connect(database) as conn:
            assert conn.execute("SELECT COUNT(*) FROM cmd_history WHERE delivery='approval_display'").fetchone()[0] == 0
            assert conn.execute("SELECT shown_row FROM _amux_delegation_grants WHERE code='REQ1'").fetchone() == (None,)
        crash_probe(database, display, display_args, True, [binding, ["REQ1", None]])
        with sqlite3.connect(database) as conn:
            displayed = conn.execute("SELECT shown_row FROM _amux_delegation_grants WHERE code='REQ1'").fetchone()[0]
            assert conn.execute("SELECT repeat_of,ts,delivered_at FROM cmd_history WHERE id=?", [displayed]).fetchone() == (shown,1500,1500)
            assert len(conn.execute(candidates, ["desk", displayed, displayed+1]).fetchall()) == 1
            assert conn.execute(candidates, ["elsewhere", 0, shown+1]).fetchall() == []
            assert conn.execute(candidates, ["desk", shown, shown+1]).fetchall() == []
            duplicate = row.copy()
            duplicate[0] = "REQ2"
            conn.execute(request, duplicate)
            expect_constraint(conn, "UPDATE _amux_delegation_grants SET shown_row=? WHERE code='REQ2'", [displayed])  # One render receipt, one request.
            conn.execute("DELETE FROM _amux_delegation_grants WHERE code='REQ2'")
            reply = conn.execute("INSERT INTO cmd_history(session,text,type,origin,delivery,ts,delegation_session_incarnation) VALUES('desk','yes','user','pane','pane',2000,'inc-desk')").lastrowid
            conn.commit()
        result = [reply, "REQ1", "desk", "request_resolved", "exactly one", 2]
        crash_probe(database, resolution, result, False)
        with sqlite3.connect(database) as conn:
            assert conn.execute("SELECT COUNT(*) FROM _amux_delegation_resolutions").fetchone()[0] == 0
        crash_probe(database, resolution, result, True)
        with sqlite3.connect(database) as conn:
            assert conn.execute("SELECT request_id FROM _amux_delegation_resolutions WHERE reply_row=?", [reply]).fetchone() == ("REQ1",)
            expect_constraint(conn, resolution, result)
            expect_constraint(conn, resolution, [reply, "REQ2", "desk", "request_resolved", "duplicate reply", 3])
            expect_constraint(conn, resolution, [reply+1, "REQ1", "desk", "request_resolved", "duplicate", 3])
            conn.execute("UPDATE _amux_delegation_grants SET resolution_row=? WHERE code='REQ1'", [reply])
            conn.execute(claim, ["actor", 2, reply, "REQ1"])
            assert conn.execute("SELECT status,evidence_row,cite_spent_at FROM _amux_delegation_grants WHERE code='REQ1'").fetchone() == ("applying", reply, 2)
            duplicate[17] = None
            conn.execute(request, duplicate)
            expect_constraint(conn, claim, ["actor", 3, reply, "REQ2"])
    print("PASS: 0125-0129 schema, request/reply uniqueness, actual query boundaries and SIGKILL question/display/resolution rollback and receipt adoption (SQLite only)")


if __name__ == "__main__":
    main()
