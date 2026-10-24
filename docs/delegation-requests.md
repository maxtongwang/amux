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
The server exports `AMUX_STATUS_OWNER` in its existing native-observer launch
environment, resolving `delegation::OWNER_AGENT` through the current rename
alias. The hook has no separate owner-name default; missing config carries no
assistant receipt.
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
Owner sends retain their original `delegation_message_type='user'` when project
execution projects their display type to `steering`. A future display row cannot
skip current input reconciliation or satisfy the strict reply row ordering.

Owner input with missing/uncertain provenance also closes the window, without
funding. Discarded, empty, injected, truncated, disabled-history and duplicate
submissions leave an `approval_input_uncertain` row when a window is open.
Such input records `request_reply_not_owner_input`; a later yes cannot reuse
the display. Verified peer history retains `type=session`, including when its
sender explicitly requests history. Legacy user/chat rows lacking owner proof
are treated conservatively as uncertain input rather than silently ignored.
Known peers/members use session provenance on success, failed sends and direct
Chat-tab sends. A recorded owner-worker self-send remains non-owner input:
its original delegation_message_type prevents misclassifying it as output.
Refused or empty owner input reconciles its closing boundary before the route
returns; a later GET or native hook is not needed to publish the closure.

Verified owner process input never becomes a peer copy by matching text.
A native hook proves which provider session submitted, not who supplied its
composer bytes. **Native terminal and provider Remote Control input alone
cannot approve a request.** Unknown native input commits an
`approval_input_uncertain` boundary with native acceptance and funds nothing.
Use a verified owner Messages, CLI, phone, Chat-tab or recorded steer send for
a typed approval. A preceding display receipt remains mandatory.

Every programmatic text delivery into the managed owner pane records an
incarnation-bound `approval_transport` receipt before transport. The pane
receives `[amux-input-receipt:<nonce>]` followed by the delivered text. The
receipt retains that prepared text verbatim, including the worker origin stamp,
even if delivery fails; it records a transport attempt, not direct delivery.
The native observer's exact receipt identifies a non-owner submission: it closes
the window and never funds a question, including on direct/spool replay, after
a restart, and with an incomplete body. Delivery alone leaves the window open.
Duplicate or rejected known receipts retain their non-owner transport origin.
Their recognition logs `native_transport_replayed` even when no open question
needs another boundary.
Worker sends carry the origin stamp and Automation transport regardless of
`record_history` or `deliver_now`, including self-sends and active Chat relays
into the owner pane. Empty/whitespace/stripped peer sends are refused before
transport, so Enter cannot submit the owner's unsent draft.
Body text, recent-message similarity and
absence of a worker header never establish human authorship. An unknown,
malformed or wrong-incarnation receipt closes the window without funding it.
A receipt write failure refuses transport; the failure is logged and retained.
Non-owner bare slash commands into the managed owner pane are refused because
prefixing a receipt would change command semantics. A directly verified owner
send retains bare-command behavior; queued commands without fresh owner proof
must use the owner's native terminal.

Schedules and the goal keeper still reach isolated workers verbatim using
Owner **transport**. Their passive hooks are status only; neither the transport
label nor their standing owner configuration is approval provenance. They
cannot close or fund an approval. Managed deliveries use the non-owner receipt.
Accepted unknown-input markers publish Message events. Reconciliation failure
retains the spool; replay cannot fund twice or rearm a display.

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
| Native pane/provider Remote Control/raw terminal input | Unknown author and valid programmatic receipt submissions close without funding |
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

Raw key injection into the owner session requires verified owner process
origin, even for its own worker process, same-group leads and Chat relays.
`owner_session_keys_refused` is returned before general delegation and logged
at WARN. The legacy session route and worker-ID route share this boundary.
The refusal and log name the supported alternative: "use native delivery (#67)
or let the server re-arm held relays (#66)".
Bash `amux send --keys` uses the attributed API, with no tmux fallback or control
retry after an uncertain ACK. Low-level key/literal writers require a matching
incarnation scope from verified owner input or a recorded text transport.
Owner Enter records a nonfunding boundary; refused peer Enter leaves the
question open. The Workspace shell create/input routes also require owner
origin so a peer cannot use a server-side shell to drive the owner pane.
Server-descended API clients are classified as automation, including workerless
shell schedules. Scheduled shells also export `AMUX_INPUT_SOURCE=automation`;
the CLI forwards this demotion on every request, including after server restart
or shell reparenting. Neither origin funds a question; its eventual managed
pane submission closes the window. Verified
owner-created Workspace PTYs preserve the owner's CLI origin; any worker
ancestry still takes precedence. A client hint can only remove owner proof.

The #66 held-relay recovery implementation is absent on this branch. Its rebase
integration point is `session_verbs::submit_held_relay_non_owner(state, name)`.
After #66 verifies that a relay is held in the managed owner pane, call this
server-only seam instead of the raw key writer. It durably closes the approval
window before sending exactly Enter with an incarnation-bound write scope.
A failed boundary write prevents Enter; the later native hook remains
non-owner input. This seam grants no peer key permission and does not expose
an HTTP action. It cannot bypass #66's held-relay eligibility/recovery checks.
Ineligible panes return `held_relay_owner_managed_only` (403); missing current
incarnations return `held_relay_identity_missing` (409). Both log WARN and name
native delivery (#67) or server recovery (#66); neither permits peer keys.

Sweep regressions are in `api/delegation_reply_tests.rs`. No-Cargo checks:
`python3 -B scripts/test-owner-input-origin.py` executes the shipped CLI functions
with transport fixtures and compiles the actual pure provenance predicate with
rustc. `python3 -B scripts/test-delegation-reply-window.py` executes actual SQL,
including receipt identity and SIGKILL durability. They do not replace Rust
router tests or workspace checks.

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
