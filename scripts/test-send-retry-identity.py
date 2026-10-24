#!/usr/bin/env python3
"""Exercise shipped CLI retries with a fake transport, never a worker pane."""
import json
import os
from pathlib import Path
import subprocess
import tempfile

root = Path(__file__).resolve().parents[1]
source = (root / 'amux').read_text()
# Carry the shipped receipt helper as well as send/settle/poll: a stub would
# hide regressions in the receipt branch reached by successful retries.
start = source.index('_report_delivery_receipt() {')
end = source.index('\ncmd_stop()', start)
function = source[start:end]
with tempfile.TemporaryDirectory(prefix='amux-send-retry-') as temp:
    folder = Path(temp)
    harness = folder / 'check.sh'
    harness.write_text('''set -u
RED= YELLOW= RESET= GREEN=
CC_HOME="$TEST_DIR"
_warn_second_person_resend() { :; }
_flush_unstamped_ledger() { :; }
tmux_name() { printf 'amux-%s' "$1"; }
sleep() { :; }
session_backend() { echo tmux; }
tmux_has_session_exact() { return 0; }
tmux() { echo CALLED >> "$TEST_DIR/tmux"; return 0; }
_record_unstamped_send() { echo RECORDED >> "$TEST_DIR/tmux"; }
_curl() {
  local previous= argument body=
  for argument in "$@"; do
    [[ "$previous" == -d ]] && body="$argument"
    previous="$argument"
  done
  if [[ -z "$body" ]]; then  # GET .../send?msg_id= : the durable receipt
    local a id= text=
    for a in "$@"; do
      case "$a" in msg_id=*) id="${a#msg_id=}" ;; text=*) text="${a#text=}" ;; esac
    done
    echo "$id" >> "$TEST_DIR/receipts"
    printf '%s' "$text" > "$TEST_DIR/receipt-text"
    case "$MODE" in
      stranded) echo '{"ok":true,"accepted":false,"stranded":true,"delivered":"unknown"}' ;;
      slow-accepted) echo '{"ok":true,"accepted":true,"id":"msg-x"}' ;;
      released) echo '{"ok":true,"accepted":false,"released":true,"delivered":false}' ;;
      *) return 28 ;;
    esac
    return
  fi
  printf '%s\\n' "$body" >> "$TEST_DIR/requests"
  local count; count=$(wc -l < "$TEST_DIR/requests")
  if [[ "$MODE" == refused ]]; then echo '{"ok":false,"error":"explicit refusal"}'; return; fi
  # The incident (2026-10-17): one "pending" answer, every other reply too slow.
  if [[ "$MODE" == slow-accepted || "$MODE" == released || "$MODE" == stranded ]] && [[ "$count" -eq 2 ]]; then
    echo '{"ok":false,"submission":"pending","retryable":true}'; return
  fi
  if [[ "$MODE" == recovered && "$count" -eq 3 ]]; then echo '{"ok":true,"deduped":true,"message":"duplicate retry ignored"}'; return; fi
  return 28
}
''' + function + '\n_send_via_api target "a multiline\nmessage"\n')
    for mode, expected, attempts in [('outage', 1, 3), ('recovered', 0, 3), ('refused', 1, 1),
                                     ('slow-accepted', 0, None), ('released', 1, None), ('stranded', 1, None)]:
        for name in ['requests', 'receipts', 'receipt-text', 'tmux', 'logs/send-failures.jsonl']:
            (folder / name).unlink(missing_ok=True)
        result = subprocess.run(['bash', str(harness)], env={**os.environ, 'MODE':mode, 'TEST_DIR':temp}, capture_output=True, text=True)
        assert result.returncode == expected, (mode, result.stdout, result.stderr)
        requests = [json.loads(line) for line in (folder / 'requests').read_text().splitlines()]
        assert attempts is None or len(requests) == attempts, requests
        ids = {r.get('msg_id') for r in requests}
        assert len(ids) == 1 and next(iter(ids)), requests
        assert all(r['text'] == 'a multiline\nmessage' for r in requests)
        assert not (folder / 'tmux').exists(), 'Unacknowledged sends must not touch a terminal'
        receipts = (folder / 'receipts').read_text().split() if (folder / 'receipts').exists() else []
        if mode in ('slow-accepted', 'released', 'stranded', 'outage'):
            assert receipts and set(receipts) == ids, (mode, receipts, ids)  # the receipt asked about THIS message
            # ...and carried its text, so the server can say "stranded" rather than "released".
            assert (folder / 'receipt-text').read_text() == 'a multiline\nmessage', (folder / 'receipt-text').read_text()
        if mode == 'stranded':
            # A message still in the input box is never "safe to resend".
            assert 'Do NOT send it again' in result.stderr and 'safe' not in result.stderr, result.stderr
        if mode == 'slow-accepted':
            # A send the server accepted is never reported UNCONFIRMED (re-ops-lead's false negative).
            assert 'durable receipt' in result.stdout and 'UNCONFIRMED' not in result.stderr, (result.stdout, result.stderr)
            assert len(requests) > 3, 'timeouts after a pending answer keep polling the same ID'
            assert not (folder / 'logs/send-failures.jsonl').exists()
        if mode == 'released':
            # A released ID is not proof of non-delivery: never "safe to resend".
            assert 'Check the worker' in result.stderr and 'safe' not in result.stderr, result.stderr
        if mode == 'outage':
            row = json.loads((folder / 'logs/send-failures.jsonl').read_text())
            assert row['verdict'] == 'send_delivery_unknown' and row['raw_fallback'] is False
        print(f'PASS {mode}: {attempts} request(s), stable identity, no terminal injection')
