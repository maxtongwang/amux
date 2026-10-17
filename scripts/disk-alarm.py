#!/usr/bin/env python3
"""DMIW-7: say so before the disk fills, instead of after every build fails.

Run by rust-auto-build.sh on each tick. Below AMUX_DISK_ALARM_LEADS_GB (50) free,
every lead (workers tagged `leads`, or AMUX_DISK_ALARM_TO) gets one message; below
AMUX_DISK_ALARM_OWNER_GB (20) the owner also gets `amux alert` (push + text).
A level is announced when it is first reached or gets worse, and repeated every
AMUX_DISK_ALARM_REPEAT_S (6 h) while it lasts; recovery clears it. State lives
in <home>/.amux/disk-alarm.json.

Usage: disk-alarm.py <home>   (prints one JSON verdict line, exits 0)
Test seams: AMUX_DISK_ALARM_FREE_BYTES, AMUX_DISK_ALARM_NOW, AMUX_BIN.
"""
import json
import os
from pathlib import Path
import shutil
import ssl
import subprocess
import sys
import time
import urllib.request

GB = 1024 ** 3
RANK = {'ok': 0, 'leads': 1, 'owner': 2}


def level_for(free, leads_gb, owner_gb):
    if free < owner_gb * GB:
        return 'owner'
    if free < leads_gb * GB:
        return 'leads'
    return 'ok'


def decide(level, state, now, repeat_s):
    """(fire, new_state). Fire on a new or worse level, or when the last
    announcement of this level is older than repeat_s."""
    if level == 'ok':
        return False, {}
    worse = RANK[level] > RANK.get(state.get('level', 'ok'), 0)
    stale = now - float(state.get('at', 0)) >= repeat_s
    if worse or stale:
        return True, {'level': level, 'at': now}
    return False, state


def leads():
    named = os.environ.get('AMUX_DISK_ALARM_TO')
    if named:
        return [n.strip() for n in named.split(',') if n.strip()]
    url = os.environ.get('AMUX_URL', 'https://localhost:8824').rstrip('/') + '/api/sessions'
    ctx = ssl.create_default_context()
    ctx.check_hostname, ctx.verify_mode = False, ssl.CERT_NONE
    with urllib.request.urlopen(url, context=ctx, timeout=5) as r:
        sessions = json.load(r)
    return sorted(s['name'] for s in sessions
                  if not s.get('archived') and 'leads' in (s.get('tags') or []))


def notify(level, free, recipients):
    # The builder runs under launchd with the default PATH, which lacks ~/.local/bin.
    amux = (os.environ.get('AMUX_BIN') or shutil.which('amux')
            or str(Path.home() / '.local' / 'bin' / 'amux'))
    text = ('Disk alarm: {:.0f} GB free on this machine (alarm below {} GB). Rust build caches '
            'reclaim automatically; anything else large needs an owner.').format(
                free / GB, os.environ.get('AMUX_DISK_ALARM_LEADS_GB', '50'))
    sent, failed = [], []
    for name in recipients:
        ok = subprocess.run([amux, 'send', name, '--stdin'], input=text, text=True,
                            capture_output=True, timeout=20).returncode == 0
        (sent if ok else failed).append(name)
    if level == 'owner':
        ok = subprocess.run([amux, 'alert', 'Disk nearly full: {:.0f} GB free. Builds and workers will '
                             'start failing; clear space or tell a lead.'.format(free / GB),
                             'disk below {} GB'.format(os.environ.get('AMUX_DISK_ALARM_OWNER_GB', '20'))],
                            capture_output=True, text=True, timeout=30).returncode == 0
        (sent if ok else failed).append('owner')
    return sent, failed


def main():
    if len(sys.argv) != 2 or sys.argv[1] in ('-h', '--help'):
        print(__doc__.strip())
        return 0 if sys.argv[1:] in (['-h'], ['--help']) else 2
    home = Path(sys.argv[1])
    state_path = home / '.amux' / 'disk-alarm.json'
    verdict = {'event': 'disk_alarm', 'measured': False}
    try:
        free = int(os.environ.get('AMUX_DISK_ALARM_FREE_BYTES') or shutil.disk_usage(home).free)
        now = float(os.environ.get('AMUX_DISK_ALARM_NOW') or time.time())
        level = level_for(free, float(os.environ.get('AMUX_DISK_ALARM_LEADS_GB', 50)),
                          float(os.environ.get('AMUX_DISK_ALARM_OWNER_GB', 20)))
        try:
            state = json.loads(state_path.read_text())
        except (OSError, ValueError):
            state = {}
        fire, new_state = decide(level, state, now,
                                 float(os.environ.get('AMUX_DISK_ALARM_REPEAT_S', 6 * 3600)))
        verdict.update(measured=True, level=level, free_gb=round(free / GB, 1), fired=fire)
        if fire:
            try:
                recipients = leads()
            except Exception as error:  # noqa: BLE001 - the owner alert must not depend on it
                recipients, verdict['leads_error'] = [], '{}: {}'.format(type(error).__name__, error)
            verdict['sent'], verdict['failed'] = notify(level, free, recipients)
            if verdict['failed'] or not verdict['sent']:
                new_state = state  # retry on the next tick rather than mark it announced
        if new_state != state:
            state_path.parent.mkdir(parents=True, exist_ok=True)
            state_path.write_text(json.dumps(new_state))
    except Exception as error:  # noqa: BLE001 - never break the builder tick
        verdict['error'] = '{}: {}'.format(type(error).__name__, error)
    print(json.dumps(verdict))
    return 0


if __name__ == '__main__':
    sys.exit(main())
