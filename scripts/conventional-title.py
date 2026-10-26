#!/usr/bin/env python3
"""Conventional title checker — the one validator for commit subjects, PR titles and issue titles (refs #411).

Format (STANDARDS.md, Conventional Titles): `type(scope): summary`, scope optional (letters, digits, `.`, `_`, `/`, `-`;
note names keep their case), `!` before the colon marks a breaking change. Usage:
  conventional-title.py [--kind commit|title] [--json] TITLE...      check strings
  conventional-title.py --range BASE..HEAD [--json]                  check every commit subject in a git range
Exit 0 when every input is valid, 1 when any is not, 2 on a usage or git error.
"""
import argparse
import json
import re
import subprocess
import sys

TYPES = ('feat', 'fix', 'docs', 'style', 'refactor', 'perf', 'test', 'build', 'ci', 'chore', 'revert')
TITLE = re.compile(r'(?P<type>[a-z]+)(?:\((?P<scope>[A-Za-z0-9][A-Za-z0-9._/-]*)\))?(?P<breaking>!)?: (?P<summary>\S.*)')
# Subjects Git or GitHub generates. Revert is accepted everywhere (GitHub's Revert button titles a PR this way);
# merge and autosquash subjects are accepted for commits only.
REVERT = re.compile(r'Revert ".+"')
COMMIT_ONLY = (re.compile(r'Merge (pull request #\d+|PR #\d+|branch |remote-tracking branch |tag )'),
               re.compile(r'(fixup|squash|amend)! \S'))
EXAMPLE = 'feat(scope): add the thing'


def check(text, kind='title'):
    """(ok, reason) for one string. Pure; the single definition every caller shares."""
    if REVERT.fullmatch(text) or (kind == 'commit' and any(p.match(text) for p in COMMIT_ONLY)):
        return True, 'generated'
    if text != text.strip() or '\n' in text:
        return False, 'must be one line with no surrounding whitespace'
    match = TITLE.fullmatch(text)
    if not match:
        return False, f'expected `type(scope): summary`, for example `{EXAMPLE}`'
    if match['type'] not in TYPES:
        return False, f'unknown type `{match["type"]}`; use one of: ' + ', '.join(TYPES)
    return True, 'ok'


def range_subjects(spec):
    result = subprocess.run(['git', 'log', '--format=%H%x09%s', spec], capture_output=True, text=True)
    if result.returncode:
        raise RuntimeError(result.stderr.strip() or f'git log {spec} failed')
    return [tuple(line.split('\t', 1)) for line in result.stdout.splitlines() if line]


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.split('\n')[0])
    parser.add_argument('titles', nargs='*')
    parser.add_argument('--kind', choices=('commit', 'title'), default='title',
                        help='commit also accepts git-generated Merge/Revert subjects')
    parser.add_argument('--range', help='git revision range whose commit subjects are checked (kind commit)')
    parser.add_argument('--json', action='store_true', help='print a JSON result with per-input verdicts')
    args = parser.parse_args(argv)
    if bool(args.range) == bool(args.titles):
        parser.error('give titles or --range, not both and not neither')
    try:
        items = [(sha, subject, 'commit') for sha, subject in range_subjects(args.range)] if args.range \
            else [(None, t, args.kind) for t in args.titles]
    except RuntimeError as error:
        print(f'conventional-title: {error}', file=sys.stderr)
        return 2
    results = []
    for sha, text, kind in items:
        ok, reason = check(text, kind)
        results.append({'sha': sha, 'title': text, 'kind': kind, 'ok': ok, 'reason': reason})
    if args.json:
        print(json.dumps({'ok': all(r['ok'] for r in results), 'results': results}, indent=2))
    else:
        for r in results:
            if not r['ok']:
                print(f'{(r["sha"] or "")[:9]} {r["title"]!r}: {r["reason"]}'.strip(), file=sys.stderr)
    return 0 if all(r['ok'] for r in results) else 1


if __name__ == '__main__':
    sys.exit(main())
