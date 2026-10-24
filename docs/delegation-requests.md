# Delegated change requests

Max 2026-10-24 12:48: session-based approval (refs maxtongwang/ai-config#160).
The owner answers in the native owner session, using either the terminal or
provider Remote Control. No new typing convention or Messages visit is required.

## Request and reply contract

`POST /api/delegation/requests` drafts a supported mutation through its actual
handler without applying it. The request stores the exact change, method, path,
body, actor/target incarnations and owner incarnation. Leads remain limited to
team workers and their own groups; requests cannot grant membership in `leads`.
The request ID is the existing grant code. Creation alone proves no display.

The creation response and request view include `approval_block`, rendered by
`delegation_requests::render_request` / `approval_block`. mrpopo must output that
block verbatim as its entire assistant reply:

```text
Approval needed — <request id>
<question copied verbatim, including its line breaks>
Reply "yes" to approve. Anything else declines.
```

The heading contains an em dash, U+2014, with one space on each side. Its ID line
has no suffix. The stored question and final line must match byte for byte.
Additional prose, wrong/missing IDs, paraphrases, altered punctuation and two
blocks in one reply provide no display receipt. A question containing another
approval heading also fails closed.

Only the owner's native `Stop` hook supplies this display receipt. The very next
`UserPromptSubmit` in that same native session and stable owner incarnation may
fund the request. Trimmed, case-insensitive whole replies `yes`, `y`, `approve`
and `ok` approve. Everything else, including empty Enter, `okay`, `yes please`
and the former `yep`/`go ahead` phrases, closes without approval.

Another prompt or assistant turn invalidates the receipt. Showing the exact
block again creates a new window for the still-pending request. A replayed hook
cannot reopen it. One native reply funds one request and remains spent after
failed application or undo. Application compares the exact stored change and
all identities; an approval cannot transfer to a different change.

## Input and delivery boundaries

Funding uses only accepted native hook payloads. The report route verifies run,
provider, event sequence, native session ID and launch incarnation. Each owner
prompt records its text, native session ID and server-computed `wrapped` boolean.
Missing/truncated/subagent/Chat payloads close without funding. Delivery-text
similarity, pre-echo matching and HTTP process provenance do not decide funding.

| Input path | Verdict |
| --- | --- |
| Owner terminal or provider Remote Control `UserPromptSubmit`, immediately after exact native block | Funds only an affirmative, unwrapped prompt in the same native session/incarnation |
| Wrapped native prompt | Closes only |
| Dashboard `/send`, owner CLI, outside/Remote/remote-forwarded `/send` | Closes only |
| Chat tab, Chat worker, recorded steer, project send | Closes only |
| Durable `/api/messages` (including env-apply fan-out) | Closes at enqueue; native delivery is wrapped again at dispatch |
| Owner Messages render/display ACK | Display telemetry only; rendering closes the previous window |
| Worker/peer send, self-send, Chat relay, steer, project relay, schedule, goal, server notices | Stamped; their native submission closes only |
| Peer raw keys, control bytes, empty Enter into owner | Refused before writing |
| Owner raw Enter | Closes only |
| Remote Approve button in passkey mode | Additional application proof after native approval; cannot fund |

`[amux…]`, `<cross-session-message`, injected/provider stamps and the reserved
`<amux-native-delivery` envelope are wrapped. Long provider paste envelopes are
unwrapped only far enough to inspect their leading stamp. Durable
`[amux-input-receipt:<nonce>]` transport records remain for audit/replay; text
matching remains only in delivery/history deduplication.

The common send dispatcher holds owner input with
`verdict=owner_native_delivery_held` if its adapter cannot deliver natively. It
never falls through to terminal/herdr paste. #67 native delivery is absent on
this branch; the fake execution adapter in route tests models handled native
delivery. Held text remains in the delivery/transport evidence for retry by the
owning caller. No worker may substitute raw keys.

The server-only #66 seam, `submit_held_relay_non_owner`, remains available for an
already verified, stamped held relay. It commits a non-owner closing boundary
before Enter, uses an incarnation-scoped writer, and refuses if boundary storage
fails. It exposes no peer key permission or HTTP action. The later native prompt
is wrapped and cannot fund. Parent default pending dev-lead confirmation: #66 is
allowed on these terms; Messages remains close-only; a queued native prompt
counts if it is next and nothing intervenes.

## Worker Bash guard

`owner-input-guard.py` is installed beside the read/mail guards. Both native and
managed hook installers register a `PreToolUse` Bash matcher for Claude and
Codex. Codex still requires its native hook trust review. The guard resolves the
owner from the server-exported `AMUX_STATUS_OWNER`, which follows rename config.
Missing owner config refuses injection rather than guessing an owner name.

The guard blocks `tmux send-keys`, `paste-buffer`, `load-buffer` and `set-buffer`
with owner session targets, `=name`, `amux-name`, `-t` attached/separate targets,
quoted targets and pane/window IDs. ID resolution uses read-only tmux queries
with the original `-L`/`-S` socket options. Unknown IDs fail closed. Untargeted
global buffer writes are refused. Explicit non-owner targets remain allowed.
Each refusal appends `verdict=owner_tmux_input_refused` to
`~/.amux/logs/owner-input-guard.log`, without command or prompt text.

**Same-uid residual:** a process outside this hook—a background script, a
provider without the hook, or direct tmux socket use—can still type unwrapped
text into the owner session. This design does not authenticate physical hands.
Only OS separation closes that boundary.

## Storage and verification

Migration 0129 retains request records, exact changes and incarnation bindings.
0130 adds `cmd_history.approval_native_session` and `approval_wrapped`; legacy
NULL values never prove native approval. A separate fresh branch also claims
0130, so the parent must renumber one migration when integrating them.

Display rows, binding and accepted native sequence commit in one transaction.
Prompt rows commit with native acceptance; ordered resolution is recoverable
from the ledger and spool. Unique display/reply/evidence indexes prevent reuse.
Ordering reads preceding ledger rows, not merely processed resolutions.

`delegation_native_prompt` logs session proof and wrapped verdict;
`delegation_request_displayed` logs the native receipt;
`delegation_pane_no_exact_request`, `delegation_request_resolution` and WARN
`delegation_request_refused` explain refusal. Existing expiry, identity and
spent-reply checks remain in force.

| Check | Expected result |
| --- | --- |
| `python3 -B scripts/test-owner-input-guard.py` | Guard forms, audit and both installers pass |
| `python3 -B scripts/test-native-status.py` | Native producer/transcript and installer tests pass |
| `python3 -B scripts/test-delegation-request-schema.py` | Actual SQL and SIGKILL rollback/adoption pass |
| `python3 -B scripts/test-delegation-reply-window.py` | Actual ledger-order/storage queries pass |
| `scripts/test-contended.sh -p amux-server api::delegation_requests::tests` | Session route tests pass (parent runs Rust checks) |

The untracked `.pr18-b1-notes.md` holds the current class sweep, exact mutation
commands, parent defaults and verification limits. SQLite probes do not replace
the Rust routes or workspace checks.
