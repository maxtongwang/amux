#!/usr/bin/env python3
"""Fail on a destructive tmux call in a test or tool script that does not name
its socket explicitly.

WHY (2026-10-16 outage): a test ran `TMUX_TMPDIR=... tmux kill-server` inside a
pane. tmux honours an inherited $TMUX over TMUX_TMPDIR, so the "private"
kill-server killed the LIVE tmux server and every worker on it. The rule: every
`tmux kill-server`, `kill-session` or `send-keys` in scripts/, tests/, e2e/,
.claude/ and crates/*/tests/ names its socket on the same invocation (`-S
<path>` or `-L <name>`), and clears TMUX/TMUX_PANE. TMUX_TMPDIR alone is not a
socket name. The shared helpers do this for you: `amux_tmux` and
`amux_test_isolate` (scripts/lib/test-isolation.sh), `tmux_argv`/`tmux_env`
(scripts/lib/test_isolation.py), `tmux_guard::std_tmux` (Rust).

What is matched: a line holding one of those verbs as a word, with `tmux` on the
same line or the verb in quotes (an argv list continued from the line above).
Comment lines are skipped. A line passes when it carries `-S` or `-L` as a
token, or calls one of the shared helpers above (they add -S by construction). Production code under crates/*/src is not scanned: it goes through
tmux_guard, which adds the -S itself in tests.

ALLOWLIST: put `tmux-lint: allow(<reason>)` on the line or the line before,
or `tmux-lint: allow-file(<reason>)` once in a file whose every match is data
(a source audit's fixtures). The reason is required (an empty `allow()` is
itself an error), so every exception says why it is safe, e.g. a production
boot script that operates the live server by design, or a wrapper that already
adds -S.

Usage: scripts/lint-tmux-socket.py [FILE...]   (no args: the tracked files above)
Exit 0 clean, 1 offenders found, 2 usage error.
"""
import re
import subprocess
import sys
from pathlib import Path

VERB = re.compile(r'(?<![\w-])(kill-server|kill-session|send-keys)(?![\w-])')
QUOTED_VERB = re.compile(r'''["'](kill-server|kill-session|send-keys)["']''')
SOCKET = re.compile(r'''(?<![\w-])["']?-[SL]["']?(?![\w-])''')
ALLOW = re.compile(r'tmux-lint:\s*allow\(([^)]*)\)')
# A whole file whose matches are all DATA (a source audit's fixtures, a list of
# banned words), declared once with its reason.
ALLOW_FILE = re.compile(r'tmux-lint:\s*allow-file\(([^)]*)\)')
# The shared helpers name the private socket by construction.
SAFE_WRAPPER = re.compile(r'(?<![\w-])(amux_tmux|tmux_argv\(|tmux_guard::std_tmux|tmux_guard::tokio_tmux)')
SCAN_DIRS = ('scripts/', 'tests/', 'e2e/', '.claude/')
SCAN_EXT = {'.sh', '.bash', '.py', '.js', '.mjs', '.cjs', '.ts', '.rs'}
SKIP_PARTS = ('node_modules/',)


def is_comment(line):
    s = line.lstrip()
    return s.startswith(('#', '//', '*', '/*', '<!--'))


def scan_text(path, text):
    """Offenders in one file's text, as 'path:line: message' strings."""
    out = []
    lines = text.splitlines()
    whole = ALLOW_FILE.search(text)
    if whole:
        return [] if whole.group(1).strip() else [f'{path}:1: tmux-lint allow-file() needs a reason']
    for i, line in enumerate(lines):
        if is_comment(line) or not VERB.search(line):
            continue
        if 'tmux' not in line and not QUOTED_VERB.search(line):
            continue
        allow = ALLOW.search(line) or (ALLOW.search(lines[i - 1]) if i else None)
        if allow:
            if not allow.group(1).strip():
                out.append(f'{path}:{i + 1}: tmux-lint allow() needs a reason')
            continue
        if SOCKET.search(line) or SAFE_WRAPPER.search(line):
            continue
        verb = VERB.search(line).group(1)
        out.append(f'{path}:{i + 1}: tmux {verb} without an explicit -S/-L socket '
                   f'(TMUX_TMPDIR alone loses to an inherited $TMUX): {line.strip()[:120]}')
    return out


def default_files():
    root = Path(subprocess.run(['git', 'rev-parse', '--show-toplevel'], capture_output=True,
                               text=True, check=True).stdout.strip())
    listed = subprocess.run(['git', 'ls-files'], cwd=root, capture_output=True, text=True,
                            check=True).stdout.splitlines()
    files = []
    for rel in listed:
        if any(p in rel for p in SKIP_PARTS):
            continue
        in_scope = rel.startswith(SCAN_DIRS) or re.match(r'crates/[^/]+/tests/', rel)
        if in_scope and Path(rel).suffix in SCAN_EXT:
            files.append(root / rel)
    return files


def main(argv):
    if argv and argv[0] in ('-h', '--help'):
        print(__doc__)
        return 0
    files = [Path(a) for a in argv] if argv else default_files()
    offenders = []
    for f in files:
        try:
            offenders += scan_text(f, f.read_text(errors='replace'))
        except OSError as e:
            print(f'{f}: unreadable: {e}', file=sys.stderr)
            return 2
    for o in offenders:
        print(o)
    print(f'tmux socket lint: {len(files)} file(s) scanned, {len(offenders)} offender(s)', file=sys.stderr)
    return 1 if offenders else 0


if __name__ == '__main__':
    sys.exit(main(sys.argv[1:]))
