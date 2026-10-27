#!/usr/bin/env python3
"""Generated commit subjects and card titles obey `type(scope): summary` (ai-config #411).

Reads the fixed templates the `amux` script gives `git commit-tree -m` and the scripts give `file_card`, fills each
variable with a sample word and runs the vendored checker, so a new template that breaks the rule fails here.
"""
import re
import subprocess
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
CHECKER = ROOT / 'scripts/conventional-title.py'
VARIABLE = re.compile(r'\$\{[^}]*\}|\$\([^)]*\)|\$[A-Za-z_][A-Za-z0-9_]*')
COMMIT_TEMPLATE = re.compile(r'git commit-tree [^\n]*? -m "((?:[^"\\]|\\.)*)"?', re.S)
CARD_TEMPLATE = re.compile(r'file_card "[^"]*" "[^"]*" "((?:[^"\\]|\\.)*)"')
SAMPLE = 'sample'


def templates(path, pattern):
    return [m.group(1) for m in pattern.finditer((ROOT / path).read_text())]


def check(text, kind):
    return subprocess.run([sys.executable, str(CHECKER), '--kind', kind, '--', text], capture_output=True, text=True)


class GeneratedSubjects(unittest.TestCase):
    def test_land_commit_subjects(self):
        found = [t for t in templates('amux', COMMIT_TEMPLATE) if not t.startswith('$')]  # "$msg" replays an original
        self.assertGreaterEqual(len(found), 3)  # pick base, drop regenerable paths, regenerate derived files
        for raw in found:
            with self.subTest(template=raw):
                subject = VARIABLE.sub(SAMPLE, raw).splitlines()[0]
                result = check(subject, 'commit')
                self.assertEqual(0, result.returncode, f'{subject!r}: {result.stderr}')

    def test_mac_cleanup_card_titles(self):
        found = templates('scripts/mac-cleanup-tick.sh', CARD_TEMPLATE) + templates('scripts/mac-cleanup-fallback.sh', CARD_TEMPLATE)
        self.assertGreaterEqual(len(found), 2)
        for raw in found:
            with self.subTest(template=raw):
                title = VARIABLE.sub(SAMPLE, raw)
                result = check(title, 'title')
                self.assertEqual(0, result.returncode, f'{title!r}: {result.stderr}')


if __name__ == '__main__':
    unittest.main()
