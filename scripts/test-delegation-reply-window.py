#!/usr/bin/env python3
"""Exercise v3.1's actual SQLite queries and constraints; Rust owns decisions."""
import importlib.util
from pathlib import Path
import sqlite3
import unittest

ROOT=Path(__file__).resolve().parents[1]
spec=importlib.util.spec_from_file_location('schema',ROOT/'scripts/test-delegation-request-schema.py')
schema=importlib.util.module_from_spec(spec);spec.loader.exec_module(schema)
API=ROOT/'crates/amux-server/src/api'

class ApprovalStorage(unittest.TestCase):
    def setUp(self):
        self.conn=sqlite3.connect(':memory:');self.addCleanup(self.conn.close)
        schema.migrate(self.conn)
        self.request=schema.source_sql(API/'delegation_grants.rs','INSERT INTO _amux_delegation_grants(code,actor,class,action,target,method,path,body,sentence,why,created_at,expires_at,status,mode,target_incarnation,actor_incarnation')

    def add(self,id='dlg_one',code='A7K2'):
        self.conn.execute(self.request,[id,'lead','lead','config','worker','PATCH','/config','{}','question',None,1,2_000_000,'cite','target-inc','lead-inc','mrpopo','owner-inc',None,'{}','Exact question?'])
        self.conn.execute('UPDATE _amux_delegation_grants SET short_code=? WHERE code=?',[code,id])

    def test_short_code_unique_only_while_open_case_insensitive(self):
        self.add()
        with self.assertRaises(sqlite3.IntegrityError): self.add('dlg_two','a7k2')
        self.conn.execute("UPDATE _amux_delegation_grants SET approval_closed_at=1 WHERE code='dlg_one'")
        self.conn.execute("UPDATE _amux_delegation_grants SET short_code='a7k2' WHERE code='dlg_two'")
        self.assertEqual(self.conn.execute('SELECT COUNT(*) FROM _amux_delegation_grants').fetchone(),(2,))

    def test_code_reuse_query_seven_day_boundary_and_open_collision(self):
        self.add()
        query=schema.source_sql(API/'delegation_approval.rs','SELECT EXISTS(SELECT 1 FROM _amux_delegation_grants WHERE short_code=')
        self.assertEqual(self.conn.execute(query,['a7k2',9999999]).fetchone(),(1,))
        self.conn.execute('UPDATE _amux_delegation_grants SET approval_closed_at=100')
        for seconds,expected in [(100+7*86400-1,1),(100+7*86400,0),(100+7*86400+1,0)]:
            self.assertEqual(self.conn.execute(query,['A7K2',seconds-7*86400]).fetchone(),(expected,))

    def test_status_closure_retains_code(self):
        self.add();self.conn.execute("UPDATE _amux_delegation_grants SET status='expired'")
        closed=self.conn.execute('SELECT approval_closed_at,short_code FROM _amux_delegation_grants').fetchone()
        self.assertIsInstance(closed[0],float);self.assertEqual(closed[1],'A7K2')

    def test_snapshot_contains_requester_and_exact_operands(self):
        self.add()
        query=schema.source_sql(API/'delegation_approval.rs','SELECT actor,actor_incarnation,change_spec,method,path,body,ask_text,target_incarnation,reply_incarnation FROM')
        original=self.conn.execute(query,['dlg_one']).fetchone()
        for field in ['actor','actor_incarnation','change_spec','method','path','body','ask_text','target_incarnation','reply_incarnation']:
            with self.subTest(field=field):
                self.conn.execute('SAVEPOINT edit')
                self.conn.execute(f"UPDATE _amux_delegation_grants SET {field}='different'")
                self.assertNotEqual(self.conn.execute(query,['dlg_one']).fetchone(),original)
                self.conn.execute('ROLLBACK TO edit')

    def test_confirmation_stays_open_in_actual_candidate_query(self):
        self.add();query=schema.source_sql(API/'delegation_approval.rs','SELECT g.code,g.short_code,g.shown_row,h.ts/1000.0')
        self.conn.execute("UPDATE _amux_delegation_grants SET confirm_required=1,resolution_code='request_confirm_needed'")
        self.assertEqual(len(self.conn.execute(query,[100,501]).fetchall()),1)
        self.conn.execute('UPDATE _amux_delegation_grants SET approval_closed_at=100')
        self.assertEqual(self.conn.execute(query,[100,501]).fetchall(),[])

    def test_reserved_prompt_is_not_an_intervening_boundary(self):
        query=schema.source_sql(API/'delegation_approval.rs','SELECT COALESCE(MAX(id),0)')
        self.conn.execute("INSERT INTO cmd_history(session,text,type,delivery) VALUES('mrpopo','shown','assistant','approval_pane_display')")
        shown=self.conn.execute('SELECT last_insert_rowid()').fetchone()[0]
        self.conn.execute("INSERT INTO cmd_history(session,text,type,delivery) VALUES('mrpopo','long truncated prefix','user','approval_reserved_prompt')")
        self.conn.execute("INSERT INTO cmd_history(session,text,type,delivery) VALUES('mrpopo','paste [amux approval] approved A7K2','user','approval_native_prompt')")
        self.assertEqual(self.conn.execute(query,['mrpopo',999]).fetchone(),(shown,))

if __name__=='__main__': unittest.main(verbosity=2)
