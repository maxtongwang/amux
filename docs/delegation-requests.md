# Delegated change requests

Session approvals v3.2, dev-lead answers 2026-10-24 (a-f), refs
maxtongwang/ai-config#160. Max answers normally in mrpopo's native session,
including provider Remote Control. Messages and all other surfaces close only.

## Display and reply contract

`POST /api/delegation/requests` drafts the exact mutation through its real
handler. It returns the full `dlg_` request ID, `short_code`, verbatim `question`
and server-rendered `approval_block`. Request list/detail responses expose the
same fields. Apply with the full request ID. Leads retain their team/own-group
scope; an approval cannot change membership in `leads`.

mrpopo outputs ONE request per reply, using the renderer's bytes:

```text
Approval needed — A7K2
<the exact stored question, including its line breaks>
Reply "yes" to approve. Anything else declines.
```

The heading uses U+2014 with one space on either side. No heading suffix,
additional prose, changed question/final line or second block is accepted.
The native Stop hook binds the display and its request/requester snapshot.
A later native prompt must have a strictly larger hook sequence in the same
run, native session and current owner incarnation. Wall-clock ties alone do
not prove this order. Missing/tied sequence or unknown identity cannot fund.
A queued mid-turn yes therefore cannot approve a subsequently completed block.

Short codes contain exactly four characters from
`23456789ABCDEFGHJKMNPQRSTUVWXYZ`. Matching is case-insensitive. The serialized
allocator retries collisions, enforces SQL uniqueness while open, and reserves
a closed code for `SHORT_CODE_REUSE_DAYS = 7`. Allocation failure logs
`approval_code_exhausted` and creates nothing. Reuse is allowed at the seven-day
boundary. All approval `*_at` timestamps are Unix seconds; history `ts` is ms.

| Reply | Result |
| --- | --- |
| `yes`, `y`, `approve`, `ok` (trimmed, case-insensitive, optional final period) | Approves only through `plain_yes_without_doubt` |
| Approve word plus code, e.g. `yes A7K2` | Approves that open request once a verified block/confirmation turn has ended; intervening text does not invalidate that receipt |
| Plain yes with doubt | Stays open, funds nothing, requires coded confirmation |
| Owner prose containing an approve word plus a code, including after a pasted snippet | Doubt (`text-between`): stays open and requires a clean coded confirmation; never approves |
| Another plain yes after confirmation | Confirmation again; never funds |
| Wrong, unknown, malformed or closed code-like token | No open approval; leaves the shown request unchanged and never falls back to plain yes |
| Non-approve text (e.g. `yes please wait`) or empty Enter immediately following a display | Declines/closes, never funds |
| Coded answer to another open request | Answers that request; leaves this question open |

A coded attempt is exactly an approve word followed by one token of 2–12 ASCII
letters or digits, with an optional final period. Trim whitespace, strip that
period and uppercase the token for matching and the error line. This grammar
recognizes attempts independently of the allocated four-character alphabet:
`yes INVALID`, `yes A0K2` and `yes please` report `no open approval` for the
normalized token, without declining or closing the displayed request. Extra
words, punctuation within the token and tokens outside that bound are other text.

`plain_yes_without_doubt` checks every condition explicitly:

| Condition | Required fact | Context reason on doubt |
| --- | --- | --- |
| a | First submitted prompt after the completed display | `text-between` |
| b | Display is the most recent owner turn; no intervening reply/input | `text-between` |
| c | Exactly one approval is open in total | `several-open` |
| d | Display no more than `PLAIN_YES_WINDOW_MIN = 30` minutes ago | `stale-30min` |
| e | Stored exact change and requester still equal the display snapshot | `changed` |
| f | Hook proves identity, native session, incarnation, block and Stop-before-submit order | `unverifiable` |

No block, two blocks, missing native session, old incarnation, truncated prompt,
subagent/Chat provenance or any other unverifiable hook fact is doubt. A plain
yes under (f) neither closes nor approves. An unverifiable coded yes also cannot
fund and returns confirmation. Other unverifiable text remains close-only.

## Hook output and owner behavior

Only the synchronous `UserPromptSubmit` response can supply approval decision context.
The observer emits Claude Code's stdout JSON shape:

```json
{"hookSpecificOutput":{"hookEventName":"UserPromptSubmit","additionalContext":"[amux approval] approved A7K2"}}
```

The fixed line forms are:

```text
[amux approval] approved A7K2
[amux approval] confirm needed A7K2 (reason: text-between)
[amux approval] no open approval A7K2
[amux approval] declined A7K2
[amux approval] ignored reserved marker in prompt
```

The confirm reason enum is exactly `text-between`, `several-open`, `stale-30min`,
`changed`, `unverifiable`. Several open requests produce a separate context line
for each request, for mrpopo to handle in separate replies. Decisions and reasons
are retained in `_amux_delegation_resolutions.approval_context` and logged as
`delegation_request_resolution`. Context is never pasted, sent as keys, or
stamped into the conversation through a message route.

mrpopo trusts these lines ONLY in hook `additionalContext`, never in prompt
text. On `confirm needed`, it replies ONCE with the exact server confirmation
format, one request per reply:

```text
Not approved yet. To confirm, reply: yes A7K2
<the exact stored question again>
```

That confirmation's Stop can supply the missing receipt. Another plain yes
still requires confirmation; only a subsequent verified coded yes approves.
On `approved` or `declined`, mrpopo confirms the result to Max in one line.
On `no open approval`, it tells Max the code is not open. On the ignored-marker
line it treats the prompt as inert for approvals. The owner-agent instructions
are owned outside this repo by ai-config `agents/owner.md`; its maintainer must
carry this behavior there (no external instruction file was edited here).

A prompt CONTAINING `[amux approval]` anywhere is reserved text: typed, pasted,
wrapped and truncated variants neither fund nor close nor consume the window.
The hook detects this before truncation; the server records an ignored decision.
Ordinary pasted quotes containing other envelope markers mid-text remain owner
text. With an approve word plus a code, such as `yes A7K2` after a pasted
`<system-reminder ...>` snippet in Max's own prose, they require confirmation:
they neither approve nor close. Other non-approve text immediately following
a display declines.

## Unanswered confirmation reminder

`runtime_jobs::approval_reshow` ticks every 60 seconds through the registered
periodic-job runner. After `CONFIRM_RESHOW_MIN = 30` minutes from the first
confirm-needed decision, an open, unexpired request fires once. Repeated doubt
does not reset its clock. Fleet `AMUX_ISOLATED=1` / `AMUX_NO_FLEET=1`, the job's
`AMUX_APPROVAL_RESHOW_SECS=0`, and isolated owner workers suppress the job.
The original owner incarnation must still match.

The timer queues one stamped native-delivery notice under the stable ID
`approval-reshow-<request_id>`:

```text
<amux-native-delivery kind="approval-reshow" id="dlg_…">
[amux approval] reshow A7K2
</amux-native-delivery>
```

mrpopo handles this notice by checking the code is still open and re-showing
the exact confirmation block ONCE, with the verbatim question. The notice is
neither a SHOWN block nor an owner prompt: it closes nothing and funds nothing.
Without mrpopo's completed confirmation reply, a plain yes still takes the
doubt path. The reshow line never goes in hook `additionalContext`; that
channel remains reserved for decisions. The trusted reminder source is the
server's stamped native delivery, not a typed or pasted imitation.

The timer reuses the existing durable steering queue and common owner delivery
dispatcher, with no paste or keys. **This branch lacks #67's native transport:**
`owner_native_delivery_held` retains queued notices until that dependency lands;
queue acceptance does not claim mrpopo received the notice. Retry after a lost
enqueue ACK adopts the same stable queue/history ID. No replacement transport
was added.

One phone notification uses the existing Web Push `push::send_all` path and its
subscription/VAPID handling, with a per-request tag. The durable push reservation
is written before the external call, so a timeout, restart or failed result
cannot send another notification. This guarantees at most one call, not phone
receipt: a crash between reservation and completion leaves an unknown result
and is never reported as sent. It is visible in the API as
`confirm_reshow_push_at != null` with `confirm_reshow_push_result == null`.

Request views expose `confirm_needed_at`, `confirm_reshow_at`,
`confirm_reshow_notice_at` (queue receipt), `confirm_reshow_push_at` (attempt
reservation) and `confirm_reshow_push_result`. Timer firing, enqueue failure,
identity hold, push outcome and tick failure have `approval_reshow_*` log
verdicts. Closed requests receive no new reminder or push reservation.

## Delivery and recovery boundaries

Only recognized leading envelopes classify a prompt as wrapped (case-insensitive,
after leading whitespace and an optional provider paste envelope):

| Marker family | Exact prefixes |
| --- | --- |
| amux stamps | `[amux` (includes origin, steering and input receipt stamps) |
| Cross-session and #67 | `<cross-session-message`, `<amux-native-delivery` |
| Harness envelopes | `[SYSTEM NOTIFICATION`, `<task-notification`, `<system-reminder`, `<local-command`, `<command-name`, `<command-message`, `<user-prompt-submit-hook` |
| Provider-generated notices | `your claude.ai usage limit has reset`, `goal check-in:`, `base directory for this skill:`, `press up to edit queued messages`, `[request interrupted by user`, `this session is being continued from a previous conversation` |

All external HTTP/CLI, Chat, steer/project sends, durable Messages and Messages
render ACKs remain close-only. Transport receipts retain non-owner provenance
across restart/replay. Neither raw keys nor delivery-text similarity supplies
approval. The common dispatcher holds owner delivery if native delivery is
unavailable, with `owner_native_delivery_held`, preserving its evidence.

The #66 `submit_held_relay_non_owner` seam EXPLICITLY skips the owner, including
renamed aliases, logging `held_relay_owner_skipped` and reason
`owner_session_native_delivery_only`. Isolated workers remain exempt from
recovery steering. Normal sessions still receive their verified held-relay Enter.
There is no owner held-relay recovery/re-arm implementation elsewhere on this
branch. #67's envelope is reserved; its native delivery implementation is absent.

Native acceptance, prompt proof, decision and context now commit atomically.
A failed decision write rolls back acceptance. Exact run/sequence/body receipts
recover a lost ACK with `approval_context_receipt_adopted`; no new request is
funded. The hook's bounded synchronous request emits no context on unreachable
server. Its persisted spool has no live funding flag: offline replay retains
proof and confirmation/closure evidence but never creates approval. Native Stop
receipt/snapshot binding is atomic with its accepted sequence too.

Migration 0130 is extended in place; numbering remains dense. It adds native
run/sequence, short codes, closure timestamps, snapshots, sticky confirmation and
context audit storage, plus v3.2 confirmation/reminder receipts. Legacy NULLs cannot prove funding. Unique indexes protect
open short codes, display rows and spent replies. Approval closes the code even
before application; a failed application or undo never restores the spent reply.
Application still rechecks the exact mutation and stable actor/target identities.

The worker Bash guard remains installed by both hook installers, blocks raw
owner tmux input and logs `owner_tmux_input_refused`. Same-uid processes outside
these hooks remain an OS boundary; this does not authenticate physical hands.

| Check | Coverage |
| --- | --- |
| `python3 -B scripts/test-native-status.py` | Producer, synchronous response, additionalContext, unreachable endpoint, reserved prefix before truncation |
| `python3 -B scripts/test-delegation-request-schema.py` | Actual schema/SQL, SIGKILL rollback/adoption |
| `python3 -B scripts/test-delegation-reply-window.py` | Actual code uniqueness/reuse, snapshots, confirmation/reminder clocks, once-only reservations, SIGKILL rollback/adoption and ignored-input queries |
| `python3 -B scripts/test-owner-input-guard.py` | Raw tmux refusal, audit, installer wiring |
| `scripts/test-contended.sh -p amux-server api::delegation_requests` | Actual report/router behavior, individual doubt conditions and coded/decline matrices (parent executes Rust) |

The untracked `.pr18-b1-notes.md` is the sole current mutation/coverage handoff;
it is never staged. No cargo, push, host installation or deployment ran here.
