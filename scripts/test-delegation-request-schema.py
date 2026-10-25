#!/usr/bin/env python3
"""Verify native-session constraints and SIGKILL atomicity using the shipped SQL (no cargo).

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
    for number in range(128, 134):
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
        row=conn.execute('SELECT last_insert_rowid()').fetchone()[0]
        for query,args,index in json.loads(sys.argv[5]):
            args[index]=row
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
    resolution = source_sql(requests.with_name("delegation_approval.rs"), "INSERT INTO _amux_delegation_resolutions(")
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
            display = source_sql(requests, "INSERT INTO cmd_history(session,text,type,origin,delivery,ts,delegation_session_incarnation,approval_native_session,approval_run,approval_sequence)")
            conn.commit()
        display_args = ["desk", "Approval needed — REQ1\n" + question[0] + '\nReply "yes" to approve. Anything else declines.', 1500, "inc-desk", "native-desk", "run", 1]
        receipt = source_sql(requests, "UPDATE cmd_history SET delivery='approval_pane_display',repeat_of=")
        binding = source_sql(requests, "UPDATE _amux_delegation_grants SET resolution_note=")
        followup = [[receipt, [None, shown], 0], [binding, ["REQ1", None, "snapshot"], 1]]
        crash_probe(database, display, display_args, False, followup)
        with sqlite3.connect(database) as conn:
            assert conn.execute("SELECT COUNT(*) FROM cmd_history WHERE delivery IN ('native_assistant','approval_pane_display')").fetchone()[0] == 0
            assert conn.execute("SELECT shown_row FROM _amux_delegation_grants WHERE code='REQ1'").fetchone() == (None,)
        crash_probe(database, display, display_args, True, followup)
        with sqlite3.connect(database) as conn:
            displayed = conn.execute("SELECT shown_row FROM _amux_delegation_grants WHERE code='REQ1'").fetchone()[0]
            assert conn.execute("SELECT repeat_of,ts,delivered_at,approval_native_session FROM cmd_history WHERE id=?", [displayed]).fetchone() == (shown,1500,1500,"native-desk")
            duplicate = row.copy()
            duplicate[0] = "REQ2"
            conn.execute(request, duplicate)
            expect_constraint(conn, "UPDATE _amux_delegation_grants SET shown_row=? WHERE code='REQ2'", [displayed])  # One render receipt, one request.
            conn.execute("DELETE FROM _amux_delegation_grants WHERE code='REQ2'")
            prompt = source_sql(requests, "INSERT INTO cmd_history(session,text,type,origin,delivery,ts,delegation_session_incarnation,approval_native_session,approval_wrapped,approval_run,approval_sequence)")
            reply = conn.execute(prompt, ["desk","yes","approval_native_prompt",2000,"inc-desk","native-desk",False,"pane","run",2]).lastrowid
            assert conn.execute("SELECT text,approval_native_session,approval_wrapped FROM cmd_history WHERE id=?",[reply]).fetchone() == ("yes","native-desk",0)
            conn.commit()
        result = [reply, "REQ1", "desk", "request_resolved", "exactly one", 2, "[amux approval] approved A7K2"]
        crash_probe(database, resolution, result, False)
        with sqlite3.connect(database) as conn:
            assert conn.execute("SELECT COUNT(*) FROM _amux_delegation_resolutions").fetchone()[0] == 0
        crash_probe(database, resolution, result, True)
        with sqlite3.connect(database) as conn:
            assert conn.execute("SELECT request_id FROM _amux_delegation_resolutions WHERE reply_row=?", [reply]).fetchone() == ("REQ1",)
            expect_constraint(conn, resolution, result)
            expect_constraint(conn, resolution, [reply, "REQ2", "desk", "request_resolved", "duplicate reply", 3, None])
            expect_constraint(conn, resolution, [reply+1, "REQ1", "desk", "request_resolved", "duplicate", 3, None])
            conn.execute("UPDATE _amux_delegation_grants SET resolution_row=? WHERE code='REQ1'", [reply])
            conn.execute(claim, ["actor", 2, reply, "REQ1"])
            assert conn.execute("SELECT status,evidence_row,cite_spent_at FROM _amux_delegation_grants WHERE code='REQ1'").fetchone() == ("applying", reply, 2)
            duplicate[17] = None
            conn.execute(request, duplicate)
            expect_constraint(conn, claim, ["actor", 3, reply, "REQ2"])
    print("PASS: 0128-0133 schema, request/reply uniqueness, actual query boundaries and SIGKILL question/native-display/resolution rollback and receipt adoption (SQLite only)")


if __name__ == "__main__":
    main()
