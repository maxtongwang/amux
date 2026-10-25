#!/usr/bin/env python3
"""Hermetic real-hook transport driver for Python and Rust report-route tests.

All paths and HTTP effects use a caller-owned scratch root. No tmux or live API.
"""
import importlib.util
import io
import json
import os
from pathlib import Path
import sys
import time
from unittest.mock import MagicMock, patch


def run(root, actions, worker='mrpopo', sequence=1):
    spec = importlib.util.spec_from_file_location('observer', Path(__file__).parent/'hooks/native-status.py')
    hook = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(hook)
    folder = root/'status-events'/worker/'reply-run'
    folder.mkdir(parents=True, exist_ok=True)
    (folder/'counter.json').write_text(json.dumps({'sequence': sequence-1}))
    (root/'auth_token').write_text('fixture-only')
    requests = []
    fail_live = False

    def response(req, **kwargs):
        if fail_live and req.get_header('X-amux-approval-nonce'):
            raise OSError('fixture live send failed')
        requests.append({'body': json.loads(req.data), 'nonce': req.get_header('X-amux-approval-nonce')})
        value = MagicMock()
        value.__enter__.return_value = io.StringIO('{"ok":true}')
        return value

    env = dict(AMUX_STATUS_OWNER=worker, AMUX_STATUS_URL='https://fixture.invalid')
    with patch.dict(os.environ, env), patch.object(hook.ssl, 'create_default_context'), patch.object(hook.urllib.request, 'urlopen', side_effect=response):
        for action in actions:
            if action['op'] == 'replay':
                hook.deliver(root, worker, 'reply-run')
                continue
            payload = hook.observe(action['data'], root, worker, 'reply-run', 'claude', time.time())
            assert payload, action
            path = folder/('%020d.json' % payload['sequence'])
            if action['op'] in ('queue', 'tamper'):
                # A peer can write all fields, including a made-up live flag/nonce.
                forged = dict(payload, **action.get('rewrite', {}))
                path.write_text(json.dumps(forged))
            if action['op'] == 'queue':
                continue
            fail_live = action['op'] == 'failed-live'
            hook.deliver(root, worker, 'reply-run', payload)
            if fail_live:
                queued = json.loads(path.read_text())
                assert not any(key in queued for key in hook.APPROVAL_FIELDS), queued
            fail_live = False
    return requests


if __name__ == '__main__':
    source = json.load(sys.stdin)
    print(json.dumps(run(Path(sys.argv[1]), source['actions'], source.get('worker', 'mrpopo'), source.get('sequence', 1))))
