#!/usr/bin/env python3
"""Install passive observers without touching unrelated hooks or hook trust."""
import argparse
import json
from pathlib import Path
import shlex

import sys as _sys
# The shared labeller lives beside this file; importlib loaders (tests) and runs from any cwd must find it.
_sys.path.insert(0, str(__import__('pathlib').Path(__file__).resolve().parent))
from hook_labels import reconcile  # noqa: E402

COMMON = ('SessionStart', 'SessionEnd', 'UserPromptSubmit', 'PreToolUse',
          'PermissionRequest', 'PostToolUse', 'PreCompact', 'PostCompact', 'Stop')


def merge(data, provider, script, changes=None):
    hooks = data.setdefault('hooks', {})
    events = COMMON + (('Interrupt',) if provider == 'codex' else ('Notification', 'PostToolUseFailure', 'StopFailure'))
    command = 'python3 ' + shlex.quote(str(script)) + ' ' + provider
    desired = {}
    for event in events:
        handler = {'type': 'command', 'command': command, 'timeout': 3}
        if provider == 'codex':
            handler['statusMessage'] = 'amux native status'
        group = {'hooks': [handler]}
        if event in ('PreToolUse', 'PermissionRequest', 'PostToolUse', 'PostToolUseFailure'):
            group['matcher'] = '.*'
        desired[event] = [group]
    if provider == 'codex':
        count = reconcile(hooks, desired, lambda value: isinstance(value, str) and 'native-status.py' in value)
        if changes is not None:
            changes.append(count)
        return data
    # This observer coexists with the managed subagent/token producer. That
    # producer yields main-state authority on native-instrumented launches.
    for event, groups in list(hooks.items()):
        kept = []
        for group in groups:
            items = [h for h in group.get('hooks', []) if 'native-status.py' not in h.get('command', '')]
            if items:
                kept.append(dict(group, hooks=items))
        hooks[event] = kept
    for event, groups in desired.items():
        hooks.setdefault(event, []).extend(groups)
    return data


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--provider', required=True, choices=('codex', 'claude'))
    parser.add_argument('--settings', type=Path, required=True)
    parser.add_argument('--script', type=Path, required=True)
    parser.add_argument('--dry-run', action='store_true', help='count changed managed hooks without writing settings')
    args = parser.parse_args()
    if args.dry_run and args.provider != 'codex':
        parser.error('--dry-run currently requires --provider codex')
    data = json.loads(args.settings.read_text()) if args.settings.exists() else {}
    changes = []
    result = merge(data, args.provider, args.script, changes)
    if args.dry_run:
        print(f'{sum(changes)} hooks would change in {args.settings}')
        raise SystemExit(0)
    args.settings.parent.mkdir(parents=True, exist_ok=True)
    tmp = args.settings.with_suffix('.status-hook.tmp')
    tmp.write_text(json.dumps(result, indent=2) + '\n')
    tmp.chmod(args.settings.stat().st_mode & 0o777 if args.settings.exists() else 0o600)
    tmp.replace(args.settings)
    if args.provider == 'codex':
        print(f'Installed passive codex observations; {sum(changes)} hooks changed (review in Codex: /hooks).')
    else:
        print('Installed passive claude observations.')
