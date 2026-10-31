#!/usr/bin/env python3
"""Synthetic secrets exercise the loader and both tools' output boundaries."""
import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import secrets
import sqlite3
import subprocess
import sys
import tempfile
import time
import unittest

SCRIPTS = Path(os.environ.get('REDACT_TEST_SCRIPTS', Path(__file__).parent))
sys.path.insert(0, str(SCRIPTS))
SECRETS = ['ghp_' + 'A' * 36, 'sk-ant-' + 'B' * 40,
           'api_key=' + secrets.token_hex(16), 'https://user:pass@host']
TEXT = 'Why is this still broken again? ' + ' '.join(SECRETS)


def load(name):
    sys.modules.pop('redact_loader', None)
    spec = importlib.util.spec_from_file_location(name, SCRIPTS / (name + '.py'))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class Redaction(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.env = dict(os.environ)
        self.addCleanup(lambda: (os.environ.clear(), os.environ.update(self.env)))
        os.environ.update(AMUX_REDACT_MODULE=str(self.root / 'redactor.py'), AMUX_HOME=str(self.root))
        origin = self.root / 'redactor.py'
        # A fixture contract, not a copy of the upstream pattern table.
        origin.write_text('VALUES = ' + repr(SECRETS) + '\n'
                          'REDACT_MAX_CHARS = 1000\n'
                          'def redact(text):\n'
                          '    for value in VALUES: text = text.replace(value, "[REDACTED]")\n'
                          '    return text[:REDACT_MAX_CHARS]\n'
                          'if __name__ == "__main__": raise RuntimeError("CLI ran")\n')

    def assertSafe(self, output, missing):
        for value in SECRETS:
            self.assertNotIn(value, output)
        self.assertIn('[excerpt withheld: redaction table unavailable]' if missing
                      else '[REDACTED]', output)

    def test_loader(self):
        m = load('redact_loader')
        self.assertSafe(m.redact(TEXT), False)
        self.assertEqual(len(m.redact('x' * 1100)), 1000)
        os.environ['AMUX_REDACT_MODULE'] = str(self.root / 'missing')
        self.assertEqual(load('redact_loader').redact(TEXT), m.WITHHELD)
        origin = self.root / 'redactor.py'
        os.environ['AMUX_REDACT_MODULE'] = str(self.root / 'redactor.py')
        origin.write_text('def redact(text):\n    print(text)\n    raise ValueError(text)\n')
        out = io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(out):
            self.assertEqual(load('redact_loader').redact(TEXT), m.WITHHELD)
        self.assertEqual(out.getvalue(), '')
        origin.write_text('print(' + repr(TEXT) + ')\nraise RuntimeError(\"import failed\")\n')
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(out):
            self.assertEqual(load('redact_loader').redact(TEXT), m.WITHHELD)
        self.assertEqual(out.getvalue(), '')

    def test_invalid_paths_never_import_cwd(self):
        planted = self.root / 'foo/bar.py'
        planted.parent.mkdir(parents=True)
        planted.write_text('from pathlib import Path\n'
                           'Path("imported").touch()\n'
                           'def redact(text): return "PWNED"\n')
        previous = os.getcwd()
        self.addCleanup(os.chdir, previous)
        os.chdir(self.root)
        for value in ('', '.', 'foo/bar.py', str(self.root), str(self.root / 'missing.py')):
            with self.subTest(path=value):
                os.environ['AMUX_REDACT_MODULE'] = value
                module = load('redact_loader')
                self.assertEqual(module.redact(TEXT), module.WITHHELD)
                self.assertFalse(module.redact_event({})['redaction_table_available'])
                self.assertFalse((self.root / 'imported').exists())
        os.environ.pop('AMUX_REDACT_MODULE')
        self.assertEqual(load('redact_loader').redact(TEXT), module.WITHHELD)

    def test_probe_and_no_bytecode(self):
        previous = sys.dont_write_bytecode
        self.addCleanup(setattr, sys, 'dont_write_bytecode', previous)
        sys.dont_write_bytecode = False
        module = load('redact_loader')
        self.assertFalse(sys.dont_write_bytecode)
        self.assertTrue(module.redact_event({})['redaction_table_available'])
        self.assertFalse((self.root / '__pycache__').exists())
        origin = self.root / 'redactor.py'
        for body in ('raise ValueError("broken")', 'return None'):
            origin.write_text('def redact(text):\n    ' + body + '\n')
            self.assertFalse(load('redact_loader').redact_event({})['redaction_table_available'])

    def test_frustration_scan(self):
        db = self.root / 'fixture.db'
        con = sqlite3.connect(db)
        con.execute('CREATE TABLE cmd_history(id,ts,session,text,delivery,type,origin,submit_verdict)')
        for i in (1, 2):
            con.execute('INSERT INTO cmd_history VALUES(?,?,?,?,?,?,?,?)',
                        (i, int(time.time()*1000)-i*30000, 'fixture', TEXT,
                         'direct', 'user', '', 'stuck'))
        con.commit()
        con.close()
        os.environ['AMUX_DB'] = str(db)
        for missing in (False, True):
            if missing: os.environ['AMUX_REDACT_MODULE'] = str(self.root / 'missing')
            run = subprocess.run([sys.executable, str(SCRIPTS / 'frustration_scan.py')],
                                 capture_output=True, text=True)
            self.assertEqual(run.returncode, 0, run.stderr)
            payload = json.loads(run.stdout)
            self.assertGreater(payload['candidates'], 0)
            if missing:
                for finding in payload['findings']:
                    for message in finding['messages']:
                        self.assertEqual(message['text'], '[excerpt withheld: redaction table unavailable]')
            self.assertSafe(run.stdout + run.stderr, missing)
            audit = (self.root / 'logs/friction-sweep.log').read_text()
            for value in SECRETS: self.assertNotIn(value, audit)

    def test_friction_themes(self):
        for missing in (False, True):
            if missing: os.environ['AMUX_REDACT_MODULE'] = str(self.root / 'missing')
            m = load('friction_themes')
            signal = m.Signal('fixture-theme', 'amux', 'Fixture theme', n_considered=2)
            signal.active = True
            signal.evidence = [{'text': TEXT}]
            m.db_connect = lambda: (None, None)
            m.load_prompt_docs = lambda: {}
            m.ts_unit_warning = lambda con: None
            m.signal_rule_restatement = lambda *args: [signal]
            for name in ('signal_cross_lane_repeat', 'signal_ledger_clusters',
                         'signal_nudge_without_movement', 'signal_board_resting'):
                setattr(m, name, lambda *args: [])
            for args in (['tool', '--json', '--all'], ['tool']):
                sys.argv = args
                out = io.StringIO()
                with contextlib.redirect_stdout(out), contextlib.redirect_stderr(out):
                    self.assertEqual(m.main(), 0)
                self.assertSafe(out.getvalue(), missing)
                self.assertIn('fixture-theme', out.getvalue())
                if missing and '--json' in args:
                    self.assertEqual(json.loads(out.getvalue())['active'][0]['evidence'][0]['text'],
                                     '[excerpt withheld: redaction table unavailable]')
            m.log_friction_event({'event': 'fixture', 'text': TEXT})
            audit = self.root / 'logs/friction-sweep.log'
            self.assertSafe(audit.read_text(), missing)
            audit.unlink()
            (self.root / 'logs').rmdir()
            (self.root / 'logs').write_text('force stderr fallback')
            out = io.StringIO()
            with contextlib.redirect_stderr(out):
                m.log_friction_event({'event': 'fixture', 'text': TEXT})
            self.assertSafe(out.getvalue(), missing)
            (self.root / 'logs').unlink()


if __name__ == '__main__':
    unittest.main()
