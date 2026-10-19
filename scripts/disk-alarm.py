#!/usr/bin/env python3
"""DMIW-7: say so before the disk fills, instead of after every build fails.

Run by rust-auto-build.sh on each build run. Below AMUX_DISK_ALARM_LEADS_GB (50) free,
every lead (workers tagged `leads`, or AMUX_DISK_ALARM_TO) gets one message; below
AMUX_DISK_ALARM_OWNER_GB (20) the owner also gets `amux alert` (push + text).
A level is announced when it is first reached or gets worse, and repeated every
AMUX_DISK_ALARM_REPEAT_S (6 h) while it lasts; recovery clears it. Each recipient
is tracked on its own, so a failed send is retried on the next run without
repeating it to those who already got it. State lives
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

# Isolated-run endpoint guard: scripts/lib/amux_endpoint.py, installed beside
# hook copies. Under AMUX_ISOLATED_RUN=1 a live URL becomes a dead port.
import os, sys  # noqa: E401
_AE_HERE = os.path.dirname(os.path.realpath(__file__))
sys.path[:0] = [p for p in (_AE_HERE, os.path.join(_AE_HERE, "lib"),
                            os.path.join(_AE_HERE, os.pardir, "lib")) if p not in sys.path]
try:
    from amux_endpoint import guard as _endpoint_guard
except ImportError:  # an install without the shared file: refuse only when isolated
    def _endpoint_guard(url, client):
        return "http://127.0.0.1:9" if os.environ.get("AMUX_ISOLATED_RUN") == "1" else url

GB = 1024 ** 3
RANK = {'ok': 0, 'leads': 1, 'owner': 2}


def level_for(free, leads_gb, owner_gb):
    if free < owner_gb * GB:
        return 'owner'
    if free < leads_gb * GB:
        return 'leads'
    return 'ok'


def decide(level, state, now, repeat_s):
    """(announce, new_state). A new or worse level, or one last announced repeat_s
    ago, starts a fresh announcement round: nobody has been told yet."""
    if level == 'ok':
        return False, {}
    worse = RANK[level] > RANK.get(state.get('level', 'ok'), 0)
    stale = now - float(state.get('at', 0)) >= repeat_s
    if worse or stale:
        return True, {'level': level, 'at': now, 'told': []}
    return bool(set(state.get('pending', []))), state


def leads():
    named = os.environ.get('AMUX_DISK_ALARM_TO')
    if named:
        return [n.strip() for n in named.split(',') if n.strip()]
    url = _endpoint_guard(os.environ.get('AMUX_URL', 'https://localhost:8824'),
                          'disk-alarm').rstrip('/') + '/api/sessions'
    ctx = ssl.create_default_context()
    ctx.check_hostname, ctx.verify_mode = False, ssl.CERT_NONE
    with urllib.request.urlopen(url, context=ctx, timeout=5) as r:
        sessions = json.load(r)
    return sorted(s['name'] for s in sessions
                  if not s.get('archived') and 'leads' in (s.get('tags') or []))


def tool_env():
    """launchd's builder PATH lacks Homebrew and ~/.local/bin, and `amux send`
    needs tmux before anything else (measured in review: "tmux is required")."""
    extra = ['/opt/homebrew/bin', '/usr/local/bin', str(Path.home() / '.local' / 'bin')]
    path = os.environ.get('PATH', '/usr/bin:/bin').split(':')
    return {**os.environ, 'PATH': ':'.join(path + [p for p in extra if p not in path])}


def notify(level, free, recipients):
    """Send to each recipient ('owner' means `amux alert`); (sent, failed)."""
    env = tool_env()
    amux = (os.environ.get('AMUX_BIN') or shutil.which('amux', path=env['PATH'])
            or str(Path.home() / '.local' / 'bin' / 'amux'))
    text = ('Disk alarm: {:.0f} GB free on this machine (alarm below {} GB). Rust build caches '
            'reclaim automatically; anything else large needs an owner.').format(
                free / GB, os.environ.get('AMUX_DISK_ALARM_LEADS_GB', '50'))
    sent, failed = [], []
    for name in recipients:
        if name == 'owner':
            argv = [amux, 'alert', 'Disk nearly full: {:.0f} GB free. Builds and workers will '
                    'start failing; clear space or tell a lead.'.format(free / GB),
                    'disk below {} GB'.format(os.environ.get('AMUX_DISK_ALARM_OWNER_GB', '20'))]
            done = subprocess.run(argv, env=env, capture_output=True, text=True, timeout=30)
        else:
            done = subprocess.run([amux, 'send', name, '--stdin'], input=text, env=env,
                                  text=True, capture_output=True, timeout=20)
        (sent if done.returncode == 0 else failed).append(name)
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
                everyone = leads()
            except Exception as error:  # noqa: BLE001 - the owner alert must not depend on it
                everyone, verdict['leads_error'] = [], '{}: {}'.format(type(error).__name__, error)
            if level == 'owner':
                everyone = everyone + ['owner']
            told = set(new_state.get('told', []))
            todo = [n for n in everyone if n not in told]
            verdict['sent'], verdict['failed'] = notify(level, free, todo)
            # An unreadable lead list is a failure too: keep the round open so the
            # next run retries rather than waiting out the 6-hour repeat.
            pending = verdict['failed'] + (['leads-unlisted'] if 'leads_error' in verdict else [])
            new_state = {**new_state, 'told': sorted(told | set(verdict['sent'])), 'pending': pending}
        if new_state != state:
            state_path.parent.mkdir(parents=True, exist_ok=True)
            state_path.write_text(json.dumps(new_state))
    except Exception as error:  # noqa: BLE001 - never break the builder tick
        verdict['error'] = '{}: {}'.format(type(error).__name__, error)
    print(json.dumps(verdict))
    return 0


if __name__ == '__main__':
    sys.exit(main())
