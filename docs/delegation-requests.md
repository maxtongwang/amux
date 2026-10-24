# Delegation approval requests

Max 2026-10-23: approval identity resolution (refs maxtongwang/ai-config#160).

## Create and apply

An asking worker explicitly creates a request. The server derives the action
through the existing mutation handler and permission table, stores the exact
change, and adds a generated question to the owner session's **Messages**
(`OWNER_AGENT`, currently mrpopo), bound to its stable incarnation.
The question includes the request ID and all structured operands. Its delivery
label is **approval request**; this does not inject a prompt into the terminal.
No card is created. Raw questions and legacy citation pairs carry no authority.

```sh
amux grant request --stdin <<'JSON'
{"method":"PATCH","path":"/api/sessions/worker-a/config","body":{"worktree_verify":"scripts/verify.sh"}}
JSON
amux grant requests --json
```

The owner session must already exist with a stable incarnation. The asker
cannot choose where a reply is read: `reply_session` is no longer an input
and is rejected as an unknown field. Asker-selected-session descriptions and
free-text citation workflows from earlier revisions are superseded. Request creation returns HTTP 202,
`request_id`, `grant_code` (the same ID), `reply_session`, and `sentence`.
It does not perform the proposed mutation. After Max approves, repeat the exact
method/path/body, adding `"request_id":"ID"` to its JSON body. Request metadata
(`request_id`, `why`, and retired `cite`) is excluded from the stored change.
An applied request returns its code in `X-Amux-Grant-Code` as before.

| API | Input or result |
| --- | --- |
| `POST /api/delegation/requests` | `{method, path, body}`; unknown fields and legacy approval metadata refuse |
| `GET /api/delegation/requests` | `{requests, grants, resolutions, mode, measured, n_considered}`; requests and grants are the same records |
| `GET /api/delegation/requests/{id}` | `{request, measured, n_considered}` |
| `GET /api/delegation/grants` | Same list, preserving the existing dashboard and CLI contract |
| `POST /api/delegation/grants/{id}/approve` | Existing direct Remote owner proof; apply the selected stored request |
| `POST /api/delegation/grants/{id}/deny` | Existing owner gate; deny the selected request |
| `POST /api/delegation/grants/{id}/undo` | Existing owner/mrpopo gate; restore that applied request's original delta, subject to identity/current-state checks |

## Resolve typed replies

A reply must have existing server-recorded owner provenance, an explicit
allowlisted affirmative (`max_ref::AFFIRMATIVE_REPLIES`), and the same **current
owner-session incarnation** as the question. Input in a worker pane, another
session's Remote Control, or another Chat tab funds nothing and records
`request_reply_not_owner_session`. Peer echoes and automation carry no owner
provenance; skipped hook copies emit `pane_prompt_skipped`. The server orders messages by their durable row IDs.
Each processed owner reply permanently closes a question window, using
`MAX(reply_row)` from the stored resolution ledger, never a later classification
of its text. Eligible requests are pending records with **no resolution_code** shown **after the previous owner reply and before this reply,
in this same owner session**, across every asking actor. A non-request peer
notice does not close the window. Any owner input (including a new question or
wait) closes it; two explicit unanswered requests make it ambiguous.

- Exactly one: bind this reply to that request. Its actor and proposed change
  cannot be substituted later.
- More than one: `request_ambiguous`; fund nothing and leave all pending with a
  note to ask separately. The server cannot choose the latest of two questions
  in one unanswered window.
- None: `request_no_candidate`; fund nothing. The standalone reply refusal is
  durable and appears in the list, even when there is no request to annotate.
- Nonaffirmative: `request_reply_not_affirmative`; leave requests pending. An
  older pending request from this closed window cannot catch a later yes.

Replies must be no earlier than the shown question's recorded millisecond clock,
within two hours of it, not in the future, and at most one day old when resolved.
Strict row order breaks equal-clock ties: `previous_reply_row < shown_row < reply_row`.
A shown row at or after the reply is ineligible. Recorder timestamps retain
milliseconds so same-second questions and answers keep their actual order.
The old own-lane citation exception is superseded for explicit requests:
mrpopo may ask, but only Max's own recorded owner-session input can answer.
Passkey mode still requires direct Remote owner proof for every application;
a pane reply alone does not apply it. `amux grant CODE` is not an affirmative
or an approval selector.

## Owner input routes

All recorded replies converge on `cmd_hist_record_with_id` →
`delegation_grants::on_row` → `delegation_requests::resolve_observed`.
The resolution transaction reserves each reply once, including no-candidate
outcomes. Reading a request can reconcile a missed resolution, so hook tests
check the committed resolution before any API read can repair it.

| Entry point | Recording and resolution |
| --- | --- |
| Native pane input, provider Remote Control, raw terminal/keys input | Applied `UserPromptSubmit` on `/api/sessions/{name}/report` → `pane_prompts::record`; human pane provenance resolves only in the bound owner session. The hook must include `state="active"`; HTTP 200 with `applied=false` is an ignored observation. Disabled pane history and isolated status-only hooks intentionally do not record input |
| Dashboard, phone/Remote, owner CLI `/send`; worker-ID send alias | Recorded direct/queued sends retain verified owner process provenance and resolve; chat-type workers use this path too |
| Chat tab `/api/sessions/{worker}@chat/send` | Records a `chat` row in the parent worker's Messages with direct delivery and verified owner provenance; resolves only when that parent is the bound owner session |
| Owner `/steer`, including next-turn UI replies | With history recording enabled, records queued owner provenance and resolves |
| Project executor `/send` | Records queued owner provenance before the project steering projection; resolves. Peer input retains session-message provenance |
| Approve button `/api/delegation/grants/{id}/approve` | Applies the selected request through its separate direct Remote proof gate; does not synthesize a typed reply |

Process provenance is taken only from server-attributed owner requests.
Workers and invited members receive no owner proof. The Bash owner CLI posts
text/msg_id to the worker-ID alias without a history flag: verified owner input
with no relay source defaults to recorded history. Explicit `record_history:false`
opts out; missing proof never enables this default. The default emits
`owner_send_history_defaulted`. `outside`, `remote`, and
`remote-forwarded` owner replies can correlate; only `remote` proves direct
Remote application. No-history sends, board intake, failed/stuck sends,
automation and peer messages cannot supply typed approval evidence.

## Records and refusals

Migration **0129** extends `_amux_delegation_grants`; its primary-key `code` is
the unique request ID. It adds actor/reply incarnation bindings, reply session,
shown message row, structured change JSON, resolved reply row, refusal code/note.
`target_incarnation` and `created_at` already exist. The JSON includes the typed
action (tag before/after sets), typed target, exact body, and effective toggle
values. Method and path are stored separately and must also match exactly.
`created_at`, `expires_at`, `cite_spent_at` and resolution `created_at` use Unix
seconds. `cmd_history.ts` uses Unix milliseconds; strict row IDs establish ordering, with millisecond clock bounds refusing reversed
or stale timestamps. Equal millisecond clocks use strict row order.

`cmd_history.delegation_session_incarnation` records the receiving incarnation at
message creation. `delegation_message_type` retains the original message type
when project execution projects an owner row to `steering`; peer steering rows
retain their original session-message provenance and cannot become approvals. `_amux_delegation_resolutions` records every owner reply's
outcome: `reply_row` primary key, unique nullable `request_id`, session, code,
note and creation time. Unique indexes reserve shown rows and resolved replies.
The existing 0127/0128 evidence-row reservation remains the final grant-spending
constraint, including failed and undone grants and legacy duplicate evidence.
0124–0128 are unchanged. Old grants without explicit bindings cannot acquire
new approval or replay authority through this mechanism. Undo keeps its existing
original-target check and does not depend on the asking worker still being live.

| Refusal | Meaning |
| --- | --- |
| `request_id_required` | The mutation has no explicit request ID |
| `request_not_found` | The cited ID does not exist |
| `request_not_shown` | No surviving generated question in the bound session |
| `request_unresolved` | No reply has resolved this request |
| `request_ambiguous` / `request_no_candidate` | Distinct correlation refusals described above |
| `request_change_mismatch` | Different action, target, setting, value, direction, effective toggle, body, method or path |
| `request_actor_identity_changed` | Different asking actor/name/incarnation |
| `request_target_identity_changed` | Different target incarnation |
| `request_reply_identity_changed` | Different receiving incarnation |
| `request_reply_not_owner_session` | Typed input or a legacy request bound outside the current owner session |
| `request_reply_outside_window` | Reply failed the freshness bounds |
| `request_passkey_required` | The current approval mode requires the Remote proof |
| `cite_pair_spent` | The reply already reserved a grant; no second application |

`amux grant list`, `amux grant requests`, and Settings → Delegated changes show
pending resolution notes and no-candidate outcomes. JSON preserves stable IDs
and codes. WARN `delegation_request_refused` / `delegation_grant_refused` and
`delegation_request_resolution` verdicts make correlation failures searchable.

## Recovery and verification

The request and its shown question commit in the same Store transaction. A reply's
resolution and request binding also commit together. Missed hooks are reconciled
from the durable ledger on request read/list/application. Existing applying-grant
crash behavior stays fail closed: side effects span SQLite/files, so an interrupted
mutation is never automatically replayed and its approval remains reserved.

Run `python3 scripts/test-delegation-request-schema.py` for SQLite-only schema,
uniqueness and real SIGKILL boundary probes. Run the `delegation_grants`,
`delegation_mrpopo`, `delegation_matrix`, `delegation_sweep`, `delegation_log`,
`timestamp_units_declared` tests and the delegation/max_ref/migration lib tests
through the repository's isolated safe-cargo runner. CLI/dashboard behavior:
`node --test e2e/delegation-grant-failure.test.mjs e2e/board-delivery-chip.test.mjs`.

## B2/B3 review coverage

`.pr18-b1-notes.md` lists the complete class sweep, route tests, exact one-match
`scripts/mutate.sh` substitutions and expected red tests. The route suite lives
in `api/delegation_reply_tests.rs` and drives the real router with the existing
process-identity test resolver. Paused pane delivery and a held chat turn keep
providers and real tmux out of the fixtures. It reads committed resolution rows
before any GET/list/application recovery can hide a severed recorder wire.
No schema change is needed; migrations 0124–0130 are untouched.
