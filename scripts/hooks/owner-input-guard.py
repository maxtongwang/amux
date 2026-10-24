#!/usr/bin/env python3
"""Refuse hook-mediated tmux injection into the configured native owner session.

Same-uid processes outside PreToolUse remain outside this boundary. No commands
are executed except read-only tmux target resolution for pane/window ids.
"""
import json
import os
from pathlib import Path
import re
import shlex
import subprocess
import sys
import time

INJECTION_VERBS = frozenset(('send-keys', 'paste-buffer', 'load-buffer', 'set-buffer'))
BUFFER_VERBS = frozenset(('load-buffer', 'set-buffer'))
TARGET_OPTIONS = frozenset(('-t', '-b'))
SOCKET_OPTIONS = frozenset(('-L', '-S'))
AUDIT_VERDICT = 'owner_tmux_input_refused'


def owner_target(target, owner, socket_args):
    target = target.lstrip('=')
    if target.split(':', 1)[0] in (owner, 'amux-' + owner):
        return True
    if target.startswith(('%', '@')) or not target:
        # An omitted send target means the current pane, not "no target".
        args = ['tmux', *socket_args, 'display-message', '-p']
        if target:
            args.extend(('-t', target))
        args.append('#{session_name}')
        if any(c in target for c in ('\x00', '\r', '\n')):
            return True
        try:
            result = subprocess.run(args, capture_output=True, text=True, timeout=2, check=False)
            if result.returncode != 0:
                return True  # unresolved identity is not permission to type
            return result.stdout.strip() in (owner, 'amux-' + owner)
        except (OSError, subprocess.TimeoutExpired):
            return True
    return False


def refused(command, owner):
    # shlex preserves quoted targets and exposes shell/tmux command separators.
    # Nested shell strings are inspected recursively as commands, never run.
    # Scan all following segments: escaped tmux separators also introduce verbs.
    try:
        lexer = shlex.shlex(command, posix=True, punctuation_chars=';&|()\n')
        lexer.whitespace_split = True
        lexer.commenters = ''
        tokens = list(lexer)
    except ValueError:
        return 'unparseable tmux injection' if re.search(r'\btmux\b.*\b(?:send-keys|paste-buffer|load-buffer|set-buffer)\b', command) else None
    for i, token in enumerate(tokens):
        if Path(token).name != 'tmux':
            if 'tmux ' in token and token != command:
                nested = refused(token, owner)
                if nested:
                    return nested
            continue
        socket_args = []
        verb = None
        targets = []
        j = i + 1
        while j < len(tokens):
            item = tokens[j]
            if item in SOCKET_OPTIONS and j + 1 < len(tokens):
                socket_args.extend((item, tokens[j + 1])); j += 2; continue
            if item.startswith(('-L', '-S')) and len(item) > 2:
                socket_args.extend((item[:2], item[2:])); j += 1; continue
            if item in INJECTION_VERBS:
                verb = item
            elif item in TARGET_OPTIONS and j + 1 < len(tokens):
                targets.append(tokens[j + 1]); j += 2; continue
            elif item.startswith(('-t', '-b')) and len(item) > 2:
                targets.append(item[2:])
            j += 1
        if verb and (not owner or (not targets and verb in BUFFER_VERBS)
                     or any(owner_target(t, owner, socket_args) for t in (targets or ['']))):
            return verb
    return None


def main():
    try:
        payload = json.load(sys.stdin)
    except (ValueError, OSError):
        return 0
    if payload.get('tool_name') != 'Bash':
        return 0
    command = (payload.get('tool_input') or {}).get('command', '')
    owner = os.environ.get('AMUX_STATUS_OWNER', '')
    reason = refused(command, owner)
    if not reason:
        return 0
    home = Path(os.environ.get('AMUX_STATUS_HOME') or os.environ.get('AMUX_HOME') or Path.home() / '.amux')
    try:
        logs = home / 'logs'
        logs.mkdir(parents=True, exist_ok=True)
        with (logs / 'owner-input-guard.log').open('a') as stream:
            # Never log the command or prompt text.
            stream.write(json.dumps({'ts': time.time(), 'verdict': AUDIT_VERDICT,
                                     'owner': owner, 'operation': reason, 'measured': True,
                                     'n_considered': 1}) + '\n')
    except OSError as error:
        sys.stderr.write(f'owner-input-guard audit write failed: {error}\n')
    sys.stderr.write('Blocked owner tmux input; use amux native delivery. '
                     'The owner approval session accepts no worker paste or keys.\n')
    return 2


if __name__ == '__main__':
    sys.exit(main())
