# Delegation approval requests

Max 2026-10-23: approval identity resolution (refs maxtongwang/ai-config#160).

## Create and apply

An asking worker explicitly creates a request. The server derives the action
through the existing mutation handler and permission table, stores the exact
change, and adds a generated question to the reply session's **Messages**.
The question includes the request ID and all structured operands. Its delivery
label is **approval request**; this does not inject a prompt into the terminal.
No card is created. Raw questions and legacy citation pairs carry no authority.

```sh
amux grant request --stdin <<'JSON'
{"reply_session":"max-desk","method":"PATCH","path":"/api/sessions/worker-a/config","body":{"worktree_verify":"scripts/verify.sh"}}
JSON
amux grant requests --json
```

The reply session must already exist with a stable incarnation. Use the actual
session where Max reads Messages and replies. Request creation returns HTTP 202,
`request_id`, `grant_code` (the same ID), `reply_session`, and `sentence`.
It does not perform the proposed mutation. After Max approves, repeat the exact
method/path/body, adding `"request_id":"ID"` to its JSON body. Request metadata
(`request_id`, `why`, and retired `cite`) is excluded from the stored change.
An applied request returns its code in `X-Amux-Grant-Code` as before.

| API | Input or result |
| --- | --- |
| `POST /api/delegation/requests` | `{reply_session, method, path, body}`; unknown fields and legacy approval metadata refuse |
| `GET /api/delegation/requests` | `{requests, grants, resolutions, mode, measured, n_considered}`; requests and grants are the same records |
| `GET /api/delegation/requests/{id}` | `{request, measured, n_considered}` |
| `GET /api/delegation/grants` | Same list, preserving the existing dashboard and CLI contract |
| `POST /api/delegation/grants/{id}/approve` | Existing direct Remote owner proof; apply the selected stored request |
| `POST /api/delegation/grants/{id}/deny` | Existing owner gate; deny the selected request |
| `POST /api/delegation/grants/{id}/undo` | Existing owner/mrpopo gate; restore that applied request's original delta, subject to identity/current-state checks |

## Resolve typed replies

A reply must have existing server-recorded owner provenance, an explicit
allowlisted affirmative (`max_ref::AFFIRMATIVE_REPLIES`), and the same session
incarnation as the question. The server orders messages by their durable row IDs.
Each owner reply closes a question window. Eligible requests are pending,
unanswered records shown **after the previous owner reply and before this reply,
in this same session**, across every asking actor.

- Exactly one: bind this reply to that request. Its actor and proposed change
  cannot be substituted later.
- More than one: `request_ambiguous`; fund nothing and leave all pending with a
  note to ask separately. The server cannot choose the latest of two questions
  in one unanswered window.
- None: `request_no_candidate`; fund nothing. The standalone reply refusal is
  durable and appears in the list, even when there is no request to annotate.
- Nonaffirmative: `request_reply_not_affirmative`; leave requests pending. An
  older pending request from this closed window cannot catch a later yes.

Existing freshness limits remain: reply within two hours of creation and at most
one day old when resolved. Existing own-lane proof remains: when the asking actor
is also the reply session (or its Chat companion), cite-mode application requires
a direct Remote owner send. Passkey mode requires direct Remote owner proof for
every application; a pane reply alone does not apply it. Text such as
`amux grant CODE` is no longer an affirmative or an approval selector.

## Records and refusals

Migration **0129** extends `_amux_delegation_grants`; its primary-key `code` is
the unique request ID. It adds actor/reply incarnation bindings, reply session,
shown message row, structured change JSON, resolved reply row, refusal code/note.
`target_incarnation` and `created_at` already exist. The JSON includes the typed
action (tag before/after sets), typed target, exact body, and effective toggle
values. Method and path are stored separately and must also match exactly.
`created_at`, `expires_at`, `cite_spent_at` and resolution `created_at` use Unix
seconds. `cmd_history.ts` uses Unix milliseconds; row IDs establish ordering
without depending on subsecond timestamp precision.

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
