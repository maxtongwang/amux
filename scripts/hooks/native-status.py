#!/usr/bin/env python3
"""Native lifecycle observer and private UserPromptSubmit approval context bridge.

Persist status before delivery; approval facts exist only in this invocation. The
launch identity is inherited, never looked up afresh by an old hook process.
"""
import fcntl
import json
import os
from pathlib import Path
import re
import secrets
import ssl
import subprocess
import sys
import time
import urllib.request

INVOCATION_NONCE_BYTES = 16  # 128 bits; encoded as 32 hexadecimal characters.
MAX_PROMPT_CHARS = 20_000
HOOK_DELIVERY_TIMEOUT_S = 2.0
MAX_DELIVERY_EVENTS = 512
APPROVAL_FIELDS = ('prompt', 'prompt_truncated', 'approval_reserved_marker',
                   'assistant_message', 'approval_nonce')


def assistant_message(data, provider):
    # Transcripts are mutable shared files, never evidence of a displayed block.
    direct = data.get('last_assistant_message')
    return direct if isinstance(direct, str) else None


def status_only(payload):
    payload = dict(payload)
    for key in APPROVAL_FIELDS:
        payload.pop(key, None)
    payload['approval_live'] = False
    return payload


def synchronous_event(payload, worker):
    return payload['event'] == 'UserPromptSubmit' or (
        payload['event'] == 'Stop' and worker == os.environ.get('AMUX_STATUS_OWNER'))


def write_json(path, value):
    temp = path.with_suffix('.tmp')
    with temp.open('w') as out:
        json.dump(value, out)
        out.flush()
        os.fsync(out.fileno())
    temp.replace(path)


def queue_status(folder, payload):
    write_json(folder / ('%020d.json' % payload['sequence']), status_only(payload))


def log_drop(root, worker, error):
    with (root / 'status-observer-errors.jsonl').open('a') as out:
        out.write(json.dumps({'at': time.time(), 'worker': worker,
                              'verdict': 'approval_live_dropped', 'measured': True,
                              'n_considered': 1, 'error_type': type(error).__name__}) + '\n')


def state_for(data):
    event = data.get('hook_event_name')
    if event == 'Notification':
        return {'permission_prompt': 'blocked', 'elicitation_dialog': 'waiting',
                'agent_needs_input': 'waiting'}.get(data.get('notification_type'))
    if event == 'SessionStart':
        return 'active' if data.get('source') == 'compact' else 'idle'
    if event in ('PreToolUse', 'PermissionRequest') and data.get('tool_name', '').split('.')[-1] in ('AskUserQuestion', 'request_user_input'):
        return 'waiting'
    return {'UserPromptSubmit': 'active', 'PreToolUse': 'active',
            'PermissionRequest': 'blocked', 'PostToolUse': 'active',
            'PostToolUseFailure': 'active', 'PreCompact': 'active',
            'PostCompact': 'active', 'Stop': 'idle', 'Interrupt': 'idle',
            'SessionEnd': 'idle', 'StopFailure': 'error'}.get(event)


def deliver(root, worker, run, live=None):
    """Replay status first; send this invocation's facts directly, once only."""
    folder = root / 'status-events' / worker / run
    deadline = time.monotonic() + HOOK_DELIVERY_TIMEOUT_S
    try:
        with (folder / 'deliver.lock').open('a') as lock:
            while True:
                try:
                    fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
                    break
                except BlockingIOError:
                    if live is None:
                        return None
                    if time.monotonic() >= deadline:
                        raise TimeoutError('status delivery lock')
                    time.sleep(0.01)
            token = (root / 'auth_token').read_text().strip()
            context = ssl.create_default_context(cafile=str(root / 'tls' / 'cert.pem'))

            def post(payload, nonce=None):
                url = os.environ['AMUX_STATUS_URL'] + '/api/sessions/' + worker + '/report'
                req = urllib.request.Request(url, data=json.dumps(payload).encode(), headers={
                    'Authorization': 'Bearer ' + token, 'Content-Type': 'application/json',
                    'X-Amux-Session': worker,
                    **({'X-Amux-Approval-Nonce': nonce} if nonce else {}),
                    **({'X-Amux-Worker-Token': os.environ['AMUX_WORKER_TOKEN']} if os.environ.get('AMUX_WORKER_TOKEN') else {})})
                with urllib.request.urlopen(req, context=context, timeout=max(0.01, deadline-time.monotonic())) as response:
                    ack = json.load(response)
                if not ack.get('ok'):
                    raise RuntimeError('status report refused')
                return ack

            for path in sorted(folder.glob('0*.json'))[:MAX_DELIVERY_EVENTS]:
                if time.monotonic() >= deadline:
                    raise TimeoutError('status delivery deadline')
                payload = status_only(json.loads(path.read_text()))
                if live is not None and payload.get('sequence', 0) >= live['sequence']:
                    break
                post(payload)
                path.unlink(missing_ok=True)
            if live is not None:
                # Never re-read the live body or nonce from a queue file.
                ack = post(live, live['approval_nonce'])
                (folder / ('%020d.json' % live['sequence'])).unlink(missing_ok=True)
                return ack.get('approval_context')
    except Exception as error:
        if live is None:
            raise
        # Only status remains durable. Neither a failed Stop nor prompt can be revived.
        log_drop(root, worker, error)
        queue_status(folder, live)
    return None


def approval_output(context):
    # Only server responses enter this channel. Prompt text is never a signal.
    if isinstance(context, str) and context:
        return {'hookSpecificOutput': {'hookEventName': 'UserPromptSubmit',
                                       'additionalContext': context}}
    return None


def observe(data, root, worker, run, provider, occurred_at):
    state = state_for(data)
    if state is None:
        return False
    # A subagent's lifecycle is not the lane's: its Stop must not mark a
    # working lane idle. But a subagent's permission prompt or question
    # blocks the whole lane on the human, so those edges ARE the lane's
    # (tubescience-parity, 2026-09-30: WORKING over a subagent's "Do you want
    # to proceed?"). The same subagent's next active edge clears its own block.
    agent = data.get('agent_id') or data.get('subagent_id') or ''
    if agent and state not in ('blocked', 'waiting', 'active'):
        return False
    folder = root / 'status-events' / worker / run
    folder.mkdir(parents=True, exist_ok=True, mode=0o700)
    with (folder / 'write.lock').open('a') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        counter = folder / 'counter.json'
        previous = json.loads(counter.read_text()) if counter.exists() else {}
        if agent and state == 'active' and not (
                previous.get('state') in ('blocked', 'waiting') and previous.get('agent') == agent):
            return False
        # Claude also calls an AskUserQuestion notification 'permission_prompt'.
        # Preserve the explicit tool's waiting state until a real next edge.
        if data.get('hook_event_name') == 'Notification' and previous.get('state') == 'waiting':
            state = 'waiting'
        seq = previous.get('sequence', 0) + 1
        payload = {'native_status': True, 'provider': provider, 'run_id': run,
                   'sequence': seq, 'event_ts': occurred_at, 'state': state,
                   'event': data['hook_event_name'], 'source': provider + '-hook',
                   'session_id': data.get('session_id', ''),
                   'turn_id': data.get('turn_id', ''), 'model': data.get('model', '')}
        # The submitted text, so amux can record a prompt a person typed into
        # the pane (it never passed through amux's send path). Bounded; the
        # server strips it before storing the status report (F8(c)).
        if data.get('hook_event_name') == 'UserPromptSubmit' and isinstance(data.get('prompt'), str):
            payload['prompt'] = data['prompt'][:MAX_PROMPT_CHARS]
            payload['prompt_truncated'] = len(data['prompt']) > MAX_PROMPT_CHARS
            # Detect the reserved prefix before truncation, including pasted/peer text.
            payload['approval_reserved_marker'] = '[amux approval]' in data['prompt']
        # Only the owner's own completed turn can acknowledge pane display.
        # Carry verbatim text only in memory, never in the status spool.
        if worker == os.environ.get('AMUX_STATUS_OWNER', '') and not agent and data.get('hook_event_name') == 'Stop':
            message = assistant_message(data, provider)
            if isinstance(message, str) and message:
                payload['assistant_message'] = message
        # startup / resume / clear / compact: the server drops queued
        # auto-approvals on an owner /clear.
        if data.get('hook_event_name') == 'SessionStart' and isinstance(data.get('source'), str):
            payload['start_source'] = data['source']
        payload['approval_nonce'] = secrets.token_hex(INVOCATION_NONCE_BYTES)
        payload['approval_live'] = True
        if agent:
            payload['subagent'] = True
        write_json(counter, {'sequence': seq, 'state': state, 'agent': agent})
        # Replay must not consume this status sequence before its live facts.
        # A failed send persists status only, after the live attempt ends.
        if not synchronous_event(payload, worker):
            queue_status(folder, payload)
    return payload


def main():
    occurred_at = time.time()
    worker = os.environ.get('AMUX_STATUS_WORKER', '')
    run = os.environ.get('AMUX_STATUS_RUN_ID', '')
    root = os.environ.get('AMUX_STATUS_HOME', '')
    if not root or not re.fullmatch(r'[A-Za-z0-9_-]+', worker) or not re.fullmatch(r'[a-f0-9-]+', run):
        return
    root = Path(root)
    if len(sys.argv) > 1 and sys.argv[1] == '--deliver':
        deliver(root, worker, run)
        return
    provider = sys.argv[1] if len(sys.argv) > 1 else ''
    if provider not in ('codex', 'claude'):
        return
    data = json.load(sys.stdin)
    payload = observe(data, root, worker, run, provider, occurred_at)
    if payload and synchronous_event(payload, worker):
        context = deliver(root, worker, run, payload)
        output = approval_output(context) if data['hook_event_name'] == 'UserPromptSubmit' else None
        if output is not None:
            print(json.dumps(output))
    elif payload:
        subprocess.Popen([sys.executable, str(Path(__file__).resolve()), '--deliver'],
                         stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                         stderr=subprocess.DEVNULL, start_new_session=True, close_fds=True)


if __name__ == '__main__':
    # Bounded network errors emit no context and never fund on replay. Pending files
    # remain available to the server even if immediate network delivery fails.
    try:
        main()
    except Exception as error:
        try:
            root = Path(os.environ['AMUX_STATUS_HOME'])
            with (root / 'status-observer-errors.jsonl').open('a') as out:
                out.write(json.dumps({'at': time.time(), 'worker': os.environ.get('AMUX_STATUS_WORKER', ''),
                                      'error_type': type(error).__name__}) + '\n')
        except Exception:
            pass
