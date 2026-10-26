#!/usr/bin/env python3
"""Check the system diagram inventory and freshness over a committed Git range."""
import argparse
from fnmatch import fnmatchcase
import json
import os
from pathlib import Path, PurePosixPath
import re
import subprocess
from urllib.parse import unquote, urlsplit

BASE_ENV = 'DIAGRAMS_BASE'
DIAGRAM_DIR = 'docs/diagrams/'
INDEX = DIAGRAM_DIR + 'README.md'
PATHS = DIAGRAM_DIR + 'ARCHITECTURE_PATHS'
REPO_README = 'README.md'
REQUIRED_KINDS = ('Component/topology', 'Main runtime sequence', 'Data flow')
LINK = re.compile(r'(?<!!)\[([^\]\n]+)\]\(<?([^\s<>]+)>?\)')


def git(repo, *args):
    return subprocess.run(['git', '-C', str(repo), *args], check=True,
                          stdout=subprocess.PIPE, stderr=subprocess.PIPE).stdout


def files_at(repo, ref):
    entries = git(repo, 'ls-tree', '-r', '-z', ref).decode('utf-8', 'surrogateescape')
    return {path: mode for entry in entries.split('\0') if entry
            for meta, path in [entry.split('\t', 1)] for mode in [meta.split()[0]]}


def read_file(repo, ref, path, files):
    if files.get(path) not in ('100644', '100755'):
        return ''
    return git(repo, 'show', f'{ref}:{path}').decode('utf-8')


def links(text):
    # Examples and comments are not an inventory or a navigable README link.
    text = re.sub(r'<!--.*?-->|```.*?```|~~~.*?~~~', '', text, flags=re.S)
    return LINK.findall(text)


def local_target(target):
    url = urlsplit(target)
    path = PurePosixPath(unquote(url.path))
    if url.scheme or url.netloc or path.is_absolute() or '..' in path.parts:
        return None
    return str(path)


def check_range(repo, base, head):
    head = git(repo, 'rev-parse', '--verify', f'{head}^{{commit}}').decode().strip()
    merge_base = git(repo, 'merge-base', base, head).decode().strip()
    files = files_at(repo, head)
    base_files = files_at(repo, merge_base)
    result = {'status': 'not-opted-in', 'merge_base': merge_base, 'head': head,
              'architecture_paths': [], 'diagrams_updated': False, 'errors': []}
    if not any(p.startswith(DIAGRAM_DIR) for p in files.keys() | base_files.keys()):
        return result
    errors = result['errors']
    index = read_file(repo, head, INDEX, files)
    if not index:
        errors.append(f'Missing {INDEX}')
    inventory = dict(links(index))
    targets = []
    for kind in REQUIRED_KINDS:
        target = local_target(inventory.get(kind, ''))
        path = DIAGRAM_DIR + (target or '')
        targets.append(target)
        if not target or not target.endswith('.html') or files.get(path) not in ('100644', '100755'):
            errors.append(f'Missing HTML diagram linked as [{kind}] in {INDEX}')
    if len(set(targets)) != len(REQUIRED_KINDS):
        errors.append('Required diagram kinds must link distinct HTML files')
    readme = read_file(repo, head, REPO_README, files)
    if not any(local_target(target) == INDEX for _, target in links(readme)):
        errors.append(f'{REPO_README} must link {INDEX}')
    if files.get(PATHS) not in ('100644', '100755'):
        errors.append(f'Missing {PATHS}')
    # Use both inventories so removing a glob cannot hide an architecture change.
    patterns = {line.split('#', 1)[0].strip()
                for ref, tree in ((head, files), (merge_base, base_files))
                for line in read_file(repo, ref, PATHS, tree).splitlines()}
    patterns.discard('')
    head_patterns = [line.split('#', 1)[0].strip() for line in read_file(repo, head, PATHS, files).splitlines()]
    if files.get(PATHS) in ('100644', '100755') and not any(head_patterns):
        errors.append(f'{PATHS} lists no globs; an opted-in repo must name its architecture paths')
    changed = git(repo, 'diff', '--no-renames', '--name-only', '-z', merge_base, head, '--')
    changed = changed.decode('utf-8', 'surrogateescape').split('\0')[:-1]
    architecture = sorted(p for p in changed if any(fnmatchcase(p, rule) for rule in patterns))
    updated = any(p.startswith(DIAGRAM_DIR) for p in changed)
    if architecture and not updated:
        errors.append('Architecture changed without a docs/diagrams/ update')
    result.update(status='fail' if errors else 'pass', architecture_paths=architecture,
                  diagrams_updated=updated)
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--repo', type=Path, default=Path(__file__).resolve().parents[1])
    parser.add_argument('--base', default=os.environ.get(BASE_ENV),
                        help=f'Base ref/SHA; defaults to {BASE_ENV}, then origin/HEAD')
    parser.add_argument('--head', default='HEAD', help='Candidate commit (default: HEAD)')
    args = parser.parse_args()
    try:
        base = args.base or git(args.repo, 'symbolic-ref', 'refs/remotes/origin/HEAD').decode().strip()
        result = check_range(args.repo, base, args.head)
    except (subprocess.CalledProcessError, UnicodeError, ValueError, OSError):
        print(json.dumps({'status': 'error', 'error': 'Cannot read committed Git range; fetch '
                          f'base history and set {BASE_ENV}; diagram inventories must be UTF-8.'}))
        return 2
    print(json.dumps(result))
    return 1 if result['status'] == 'fail' else 0


if __name__ == '__main__':
    raise SystemExit(main())
