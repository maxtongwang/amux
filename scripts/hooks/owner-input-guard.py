#!/usr/bin/env python3
"""Refuse hook-mediated tmux injection into the configured native owner session.

Same-uid processes outside PreToolUse remain outside this boundary. No commands
are executed except read-only tmux target resolution. Indirection is out of scope.
"""
import getopt
import json
import os
from pathlib import Path
import re
import shlex
import subprocess
import sys
import time

# tmux 3.6a cmd.c cmd_find + cmd-{send-keys,paste-buffer,load-buffer,set-buffer}.c.
# Aliases match exactly before canonical-name prefix lookup. The shortest unique
# canonical prefixes account for competing names (send-prefix, lock-*, set-*).
COMMANDS = {
    'send-keys': ('send', 'send-k'),
    'paste-buffer': ('pasteb', 'pa'),
    'load-buffer': ('loadb', 'loa'),
    'set-buffer': ('setb', 'set-b'),
}


def injection_verb(word):
    for name, (alias, shortest) in COMMANDS.items():
        if word == alias or (len(word) >= len(shortest) and name.startswith(word)):
            return name
    return None


BUFFER_VERBS = frozenset(('load-buffer', 'set-buffer'))
TARGET_OPTIONS = frozenset(('-t', '-b'))
SOCKET_OPTIONS = frozenset(('-L', '-S'))
GLOBAL_OPTIONS = '2c:CDdf:hlL:NqS:T:uUvV'  # tmux 3.6a tmux.c
# Client/mouse key dispatch cannot be attested by a pane/session lookup.
UNRESOLVABLE_OPTIONS = {'send-keys': frozenset(('-c', '-K', '-M'))}
COMMAND_OPTIONS = {  # tmux 3.6a command entry .args strings
    'send-keys': 'c:FHKlMN:Rt:X', 'paste-buffer': 'db:prs:t:',
    'load-buffer': 'b:t:w', 'set-buffer': 'ab:t:n:w',
}
AUDIT_VERDICT = 'owner_tmux_input_refused'


def owner_target(target, owner, socket_args):
    # Ask tmux even for literal names: it owns exact/prefix/fnmatch/id semantics.
    args = ['tmux', *socket_args, 'display-message', '-p']
    if target:
        args.extend(('-t', target))
    args.append('#{session_name}')
    if any(c in target for c in ('\x00', '\r', '\n')):
        return 'target_unresolved'
    try:
        result = subprocess.run(args, capture_output=True, text=True, timeout=2, check=False)
        if result.returncode != 0:
            return 'target_unresolved'  # unresolved or ambiguous identity is not permission to type
        sessions = result.stdout.strip().splitlines()
        if len(sessions) != 1 or not sessions[0]:
            return 'target_unresolved'  # empty/multiple results are unresolved too
        return result.stdout.strip() in (owner, 'amux-' + owner)
    except (OSError, subprocess.TimeoutExpired):
        return 'target_unresolved'


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
        return 'unparseable tmux injection' if re.search(r'\btmux\b', command) else None
    for i, token in enumerate(tokens):
        if Path(token).name != 'tmux':
            if 'tmux ' in token and token != command:
                nested = refused(token, owner)
                if nested:
                    return nested
            continue
        try:
            options, rest = getopt.getopt(tokens[i + 1:], GLOBAL_OPTIONS)
        except getopt.GetoptError:
            return 'tmux:unparseable_options'
        socket_args = [part for opt, value in options if opt in SOCKET_OPTIONS for part in (opt, value)]
        segments = [[]]
        j = len(tokens) - len(rest)
        while j < len(tokens):
            item = tokens[j]
            if item in (';', '&&', '||', '|', ')', '\n'):
                segments.append([]); j += 1; continue
            if Path(item).name == 'tmux':
                break  # the next invocation owns its socket options
            segments[-1].append(item)
            j += 1
        for segment in segments:
            operation = next(((index, injection_verb(item)) for index, item in enumerate(segment)
                              if injection_verb(item)), None)
            if operation is None:
                continue
            index, verb = operation
            try:
                options, _ = getopt.getopt(segment[index + 1:], COMMAND_OPTIONS[verb])
            except getopt.GetoptError:
                return verb + ':target_unresolved'
            if any(opt in UNRESOLVABLE_OPTIONS.get(verb, ()) for opt, _ in options):
                return verb + ':target_unresolved'
            targets = [value for opt, value in options if opt in TARGET_OPTIONS]
            if not owner or (not targets and verb in BUFFER_VERBS):
                return verb
            for target in targets or ['']:
                result = owner_target(target, owner, socket_args)
                if result:
                    return verb + (':target_unresolved' if isinstance(result, str) else '')
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
