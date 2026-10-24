# Delegation approval requests

Max 2026-10-23; dev-lead ruling (b), 2026-10-24; refs maxtongwang/ai-config#160.

## Create, display, apply

An asking worker creates an explicit request. The server derives the action
through the actual mutation handler and permission table and stores its exact
change, actor/target incarnations and current owner incarnation (`OWNER_AGENT`).
The generated question includes the request ID and every structured operand.
Creation records a question in owner Messages; **it does not establish display**.
Raw questions, terminal injections, notifications and old citation pairs carry
no approval authority. Asker-selected `reply_session` is an unknown field.

```sh
amux grant request --stdin <<'JSON'
{"method":"PATCH","path":"/api/sessions/worker-a/config","body":{"worktree_verify":"scripts/verify.sh"}}
JSON
amux grant requests --json
```

The owner opens the owner worker's Messages tab (dashboard or phone web UI).
Before inserting questions into the DOM, `_approvalRender` calls the owner-only
render preparation endpoint. Its `approval_render_pending` row closes the old
window and supplies the render watermark. A yes while the new display receipt
is in flight therefore cannot fund the old question. Failed preparation leaves
the new questions off screen. After DOM insertion, `_approvalObserve` records
only an expanded question fully
intersecting the visible viewport. Hidden, collapsed, partial and detached rows
produce no receipt. The renderer sends the exact rendered text, question row ID
and render watermark. The server verifies owner process provenance, current
incarnation, unchanged question text, pending/unrefused status and expiry.
History changes after the render watermark refuse a delayed receipt; simultaneous
render receipts alone may serialize without invalidating each other.

The receipt transaction inserts a separate `cmd_history` row with
`delivery='approval_display'`, server millisecond `ts`/`delivered_at`, and
`repeat_of=ask_row`; then it sets the request's `shown_row` to that receipt.
The question remains `ask_row`. Repeated acknowledgments are idempotent and
cannot move the display or reopen its window. Pre-fix rows that merely used
the question as `shown_row` are ineligible; create a new request.

The owner pane has a second receipt path. `scripts/hooks/native-status.py`
forwards the owner's `Stop.last_assistant_message` verbatim, falling back to the
latest complete assistant text in the hook's transcript (Claude or Codex).
It never uses reasoning, tool output, partial records or text before a newer
user message. Only the configured owner worker carries assistant text.
`POST /api/sessions/{owner}/report` → `native_status::apply` →
`delegation_requests::native_assistant_turn` records the assistant message in
`cmd_history`, after native run/sequence/provider validation. Only a self-report
may carry pane evidence; outside reports strip it, and server-owned Chat status
never enters the pane recorder. The server-written
launch record must carry the same stable incarnation as the current owner and
request; old launch records without that identity cannot supply a pane receipt.

The entire stored `ask_text` must occur verbatim in that single assistant message,
and the request ID must occur as an exact token, delimited by whitespace or ASCII
punctuation. There is no normalization, fuzzy matching, substring ID match or
cross-message concatenation. Exactly one pending, unrefused, unexpired,
request without a prior pane receipt may match. A first pane receipt may
supersede a Messages receipt; both evidence rows remain in history. A match marks that **assistant row**
`delivery='approval_pane_display'`, `repeat_of=ask_row`, `delivered_at=ts`, and
binds `shown_row` to its order position. The receipt, native acceptance and
assistant row commit in one transaction. A replay adopts the committed receipt;
a later assistant turn, even repeating the question, cannot reopen it.
Assistant text is stripped from fleet status and lifecycle event projections.

The scoped Messages tab retains its existing `approval_display` path. CLI output,
Chat, other panes/sessions, global history views and push/phone notifications
cannot supply a receipt. A claimed unsupported surface returns
`request_display_surface_unsupported`; a browser without IntersectionObserver
shows that code and records nothing. Reading request/history APIs never arms a
request. No terminal prompt or board card is injected.

After approval, repeat the exact method/path/body with `request_id` in its JSON.
Metadata (`request_id`, `why`, retired `cite`) is excluded from the stored change.
Application rechecks identities, display receipt, action/target/operands and
method/path/body. It spends the reply once in the existing grant transaction.

| API | Input or result |
| --- | --- |
| `POST /api/delegation/requests` | `{method,path,body}` → HTTP 202, `request_id`, `grant_code`, `reply_session`, `sentence`, exact `question`, `how` |
| `POST /api/delegation/questions/{ask_row}/display` | Owner renderer only: `{surface:"owner-messages",text,after_row}` → `{ok,ask_row,surface}` or a structured refusal |
| `POST /api/sessions/{owner}/report` | Native observer only: accepted `Stop` with verbatim `assistant_message`, run/sequence/time/provider identity → `{ok,applied}`; no public display claim for pane |
| `POST /api/delegation/questions/render` | Owner renderer: `{surface:"owner-messages"}` → `{ok,after_row}`; closes previous window before DOM insertion |
| `GET /api/delegation/requests` | `{requests,grants,resolutions,mode,measured,n_considered}` |
| `GET /api/delegation/requests/{id}` | `{request,measured,n_considered}` |
| `GET /api/delegation/grants` | Same list, preserving dashboard/CLI contracts |
| `POST /api/delegation/grants/{id}/approve` | Separate direct Remote proof; apply the selected displayed request |
| `POST /api/delegation/grants/{id}/deny` | Existing owner gate |
| `POST /api/delegation/grants/{id}/undo` | Existing owner/mrpopo gate; restore original delta subject to identity/current-state checks |

## The next-input window

Only the **single latest display** can receive a reply. That reply must be the
very next owner input, with no intervening owner-session output. The resolver
reads the maximum preceding relevant **cmd_history row ID**, not a previously
processed resolution. Relevant rows are display receipts, pane/owner-send input
with recorded owner provenance, assistant output, and session output originating
from the owner session. A peer delivery is neither owner input nor owner output.
Missing or out-of-order resolver hooks cannot erase a stored owner boundary.

An empty owner `/send` is an Enter control, not a rejected text message. It
closes the window without funding a request even if the control succeeds.
Steer and Chat still reject empty text and retain the same nonfunding boundary.
Default CLI `source_session` relays keep peer provenance through history opt-out,
refusal and duplicate transport IDs; they do not consume an owner reply window.
Native input retains its original `delegation_message_type='user'` when project
execution projects its display type to `steering`. A future display row cannot
skip current input reconciliation or satisfy the strict reply row ordering.

Owner input with missing/uncertain provenance also closes the window, without
funding. Discarded, empty, injected, truncated, disabled-history and duplicate
submissions leave an `approval_input_uncertain` row when a window is open.
Such input records `request_reply_not_owner_input`; a later yes cannot reuse
the display. Verified peer history retains `type=session`, including when its
sender explicitly requests history. Legacy user/chat rows lacking owner proof
are treated conservatively as uncertain input rather than silently ignored.

Verified owner process input never becomes a peer copy by matching message
text. Native owner input commits with native run/provider/sequence acceptance,
using the current launch incarnation and an explicit `prompt_truncated:false`
from the hook. The hook preserves the native `session_id` for both terminal
and provider Remote Control submissions; both use the same submit route and
require a preceding pane or Messages receipt. Old hooks without truncation
evidence close the window and fund nothing. The bundled observer is adopted
by the existing native-status recovery path. Accepted input and discarded
markers publish Message events, so the Messages view can show the boundary.
Spool replay records input in the acceptance transaction before ACK deletion;
failed input writes roll back acceptance, and failed reconciliation retains
the spool. Replayed/rejected submit observations can close a newly opened
window but cannot fund it or rearm an old receipt.

The pending request must point to that latest event row, have no resolution row
or refusal code, and precede the reply strictly. Two displayed questions select
only the latest; an intervening owner question, wait, no or output means neither
can receive a later yes. A refused latest request never exposes an older one.
The former “since previous reply” scan and multi-request ambiguity decision have
been removed. The resolutions table records outcomes and replay protection only.

The reply needs the same current owner incarnation and a full allowlisted
phrase (`max_ref::AFFIRMATIVE_REPLIES`). Questions, negations and qualifications
refuse. Recorded display/reply clocks must be ordered, within two hours, not in
the future and no more than one day old. Equal clocks use strict row order.
A missing eligible display records `request_no_candidate`; a nonaffirmative
records `request_reply_not_affirmative`. Both outcomes are durable.

Server-owned chat output, including the owner companion, records an
`approval_output` boundary before publishing provider output or the finished
reply. Applied native lifecycle events other than `UserPromptSubmit` also close
windows conservatively in their transaction. These markers store no provider
text and emit `delegation_request_output_boundary`. A native Stop may then record
a new exact assistant-turn receipt; lifecycle state alone never acknowledges
display. Every subsequent assistant turn or owner row closes the window by
cmd_history order.

## Owner input routes

Recorded replies converge on `cmd_hist_record_with_id` →
`delegation_grants::on_row` → `delegation_requests::resolve_observed`.
Each requires a prior owner Messages or native assistant-turn receipt; none can
manufacture one from an input row.

| Entry point | Evidence |
| --- | --- |
| Native pane/provider Remote Control/raw terminal input | Applied `UserPromptSubmit` → `pane_prompts::record`; only bound owner-session provenance can answer |
| Dashboard, phone/Remote, owner CLI `/send`; worker-ID alias | Verified owner direct/queued history, including chat-type workers |
| Chat tab `{worker}@chat/send` | `chat` row in the parent worker's history with verified owner provenance |
| Recorded `/steer` | Queued owner provenance |
| Project executor `/send` | Original owner provenance preserved across steering projection |
| Approve button | Separate direct Remote proof; does not synthesize a typed reply |

Workers and invited members receive no owner proof. Owner CLI sends without a
history flag default to recording only with verified owner process provenance
and no relay source (`owner_send_history_defaulted`). Explicit
`record_history:false`, disabled pane history, isolated status-only hooks,
automation and failed/board-routed sends cannot supply approval replies.
Passkey mode still requires direct Remote proof at application; pane approval
alone cannot apply it. `amux grant CODE` is not a reply selector.

## Storage and recovery

No migration is added: existing `ask_row`, `shown_row`, `repeat_of`, `ts` and
`delivered_at` represent question/display evidence. A pane display uses the
assistant row itself, rather than inserting a separate display row. Migration numbering
remains dense through 0129; 0130 is unused here. Incarnation and original message
type columns remain from 0129. Existing unique indexes protect shown/resolution
rows, and 0127/0128 reserve spent replies, including failed and undone grants.
The display receipt and binding commit together, as do resolution and request
binding. Duplicate receipts adopt committed evidence without inserting a new
row. Missed resolution hooks reconcile in row order on read/list/application;
window closure does not depend on that reconciliation running first.

| Refusal | Meaning |
| --- | --- |
| `request_display_surface_unsupported` | No approval render hook for the claimed surface |
| `request_display_not_owner` | No verified owner process provenance |
| `request_display_ineligible` | Changed/missing/refused/expired/legacy question, wrong incarnation, or stale render watermark |
| `request_display_prepare_failed` | Pre-render barrier transaction failed; new questions stay off screen |
| `request_display_failed` | Receipt transaction failed; no authority assumed |
| `request_not_shown` | No surviving bound render receipt |
| `request_no_candidate` / `request_unresolved` | No eligible immediately preceding display / request has no resolved reply |
| `request_reply_not_affirmative` | Input is outside the complete affirmative allowlist |
| `request_reply_not_owner_input` | Discarded or uncertain input consumed the displayed window without funding |
| `request_input_boundary_failed` | A discarded send's boundary could not be stored; the send fails closed |
| `request_reply_outside_window` | Display/reply freshness or clock ordering failed |
| `request_reply_not_owner_session` | Input or legacy request bound outside the owner session |
| `request_reply_identity_changed` | Receiving incarnation changed |
| `request_actor_identity_changed` / `request_target_identity_changed` | Actor/target binding changed |
| `request_change_mismatch` | Proposed mutation differs from the stored change |
| `request_passkey_required` | Direct Remote proof still required |
| `cite_pair_spent` | Reply already reserved by a grant |

Settings → Delegated changes and `amux grant requests --json` retain request and
resolution IDs/codes. `delegation_request_created` deliberately makes no display
claim; `delegation_request_displayed` records receipt ID/time/surface. WARN
`delegation_request_refused`/`delegation_grant_refused` makes failures searchable.

## Verification

Run `python3 -B scripts/test-native-status.py` for exact Stop payloads and
transcript extraction. Run `python3 -B scripts/test-delegation-reply-window.py` and
`python3 -B scripts/test-delegation-request-schema.py` for actual SQLite queries,
constraints and SIGKILL rollback/receipt adoption. Rust route tests live in
`api/delegation_reply_tests.rs`; run them and all delegation, max_ref and migration
suites through the isolated safe-cargo runner. CLI/renderer tests:
`node --test e2e/delegation-display.test.mjs e2e/delegation-grant-failure.test.mjs e2e/board-delivery-chip.test.mjs`.
The untracked `.pr18-b1-notes.md` contains the class sweep, exact mutations and
parent-owned Rust verification still required.
