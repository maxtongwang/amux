# Message connection provenance

## Sender evidence rules

Message sender resolution is owned by `api/message_identity.rs` and the adapter
`session_verbs::request_sender_of`. HTTP middleware `process_identity::attribute`
strips inbound process-proof headers, resolves the exact connection through #8's
OS socket-holder/ancestry resolver, and records the original worker claims before
normalizing authorization headers. Neither a worker token nor environment-derived
`X-Amux-Session` / `X-Amux-Worker` proves message authorship.

| Connection evidence | Delivered attribution |
| --- | --- |
| Descendant of worker A's pane, claiming A or no worker | Verified worker A |
| Descendant of worker A's pane, claiming B | Process in worker A, claims B; unverified |
| Outside worker panes, claiming B | Outside process, claims B; unverified |
| Verified GUI browser connection (below) with the validated dashboard UI token, no worker/body claim, no invited member | Owner UI input |
| Remote tailnet peer (100.64.0.0/10, fd7a:115c:a1e0::/48, or the proven `tailscale serve` forward) with the UI token, no worker/body claim, no invited member | Owner UI input |
| Outside process (curl, script, launchd job), the owner's own non-amux tmux pane, or a non-tailnet network peer, even with the UI token | Unverified; logs `owner_ui_token_without_owner_connection` |
| Verified invited member | That member, never owner |
| Missing, disabled, unsupported or unresolved process evidence | Unverified; client-name headers confer no owner status |

A controlling tty alone does not prove descent from a worker pane. If that tty
belongs to a worker but ancestry cannot prove it, the resolver fails closed.
Cached inventory can confirm a worker, never an owner pane. Every currently
visible holder must resolve to the same worker; holders spanning workers, or a
pane and an outside process, are ambiguous and name no verified worker. A memo
may confirm a worker only after a complete current socket scan matches its
remembered holder set.
A generic outside CLI process is unverified, even with the owner bearer and
the UI token. Existing same-uid limitations documented in `process_identity.rs`
remain: this is process attribution, not an OS isolation boundary.

## Owner connection proof

The UI token is derived from the bearer, which every same-user process can
read, so it proves nothing on its own (round-2 B1). Owner input requires the
token AND an owner connection, decided by the resolver and checked in
`message_identity::owner_connection`:

| Owner connection | Proof |
| --- | --- |
| `Origin::Browser` | Every process from each socket holder up to launchd (pid 1) is one allow-listed browser vendor whose code signature verifies: Google Chrome (team `EQHXZ8M8AV`), Microsoft Edge (team `UBF8T346G9`) or Safari's exact `com.apple.Safari` identifier (Apple platform signing). Browser names and executable paths do not establish authority |
| `Origin::Remote` | A tailnet peer no local process holds, or the proven `tailscale serve` forward (`process_identity::forwarded`) |

The signature check (`browser_identity.rs`) reads the identifier, team and
authority chain from `codesign -dv --verbose=2`, then requires
`codesign --verify --strict --test-requirement` against an Apple-anchored
requirement naming the vendor's team. `/usr/bin/codesign` runs as a bounded
subprocess (no Security-framework binding is in the dependency tree; codesign
never shows UI); completed answers are cached per executable path and inode.
On Linux no signature can be verified, so only a tailnet peer is an owner
connection there. The verifier is a trait; tests use a fake and never run
codesign.

Shared `com.apple.WebKit.Networking` connections stay unverified: that XPC
service also serves arbitrary WKWebView apps and its launchd parent does not
identify the responsible app. Safari owner identification therefore works only
when the socket's process chain reaches Safari's own signed executable; Safari
traffic held only by the shared service has no owner authority. Refusals log
`browser_connection_unverified` and, when a UI token is presented,
`owner_ui_token_without_owner_connection`.

Every accepted browser process must also have readable argv and pass the
interactive-browser check. Amux's live registry and persisted launch records
exclude managed browser PIDs after restarts; `--remote-debugging-port`,
`--remote-debugging-pipe` (split or equals forms), and an amux-owned
`--user-data-dir` independently exclude automation. The profile predicate is
the launcher's `is_amux_owned`, not a second profile-root definition. Argument
lookup failures deny owner authority; neither argv nor environment is logged.

Chat-tab queues retain the raw input beside a serialized `Sender`; the model
receives the stamp at execution, including a retry after conversation reset.
Transcripts, command history and `chat_inflight_text` retain the owner's words.
Relay stamps preserve up to 300 owner characters; identity labels retain their
160-character cap. Older queues without `sender` retain their delivery bytes
without receiving a second stamp. Boot receipt IDs include the target, recorded
sender and serialized stamp as well as the text. Body stamp prefixes are quoted
case-insensitively in current and historical formatters.

The ancestry rule excludes a browser under a worker pane (it is that worker),
and a browser started by a launchd job's script, shell or terminal (that
process is not a signed browser). A curl or script holding the token is an
outside process. A LAN peer is `Origin::Lan`.

Residual same-user limit: a same-user script can still ask LaunchServices to
open the GUI browser (`open -a`), or a launchd job can exec the browser binary
directly, and drive the dashboard there; the browser is then indistinguishable
from one the owner opened. Tracked on ai-config #136. The passkey switch is
the hard guarantee: it requires a human presence check that no same-user
process can supply.

Only positively proved owner input and server automation may use #10's existing
instruction-free slash-command allow-list. A worker self-send remains worker
input and is stamped. Unknown client labels no longer say "the owner".
Empty-send Enter on a picker or draft and bare dialog answers require owner
connection proof, regardless of delivery flags or the default send origin.
Extracted suggestions re-enter `delivery_text` and its command allow-list; if
later parked, their extracted body is retained beside the sender.

## Text entry sweep

| Entry | Connection evidence handling |
| --- | --- |
| `send_post` | Resolves once, preserves Sender beside direct/queued text; self-sends have no prose exemption |
| `steer_mutate` | Same resolver; serializes Sender in the steering stamp column |
| `/start` prompt | Same resolver and command gate before boot-prompt queueing |
| Legacy worker creation prompt | Same resolver before starting or queueing the initial prompt |
| Board conversations | Same sender check at text-post boundary; unverified posts refused with `conversation_sender_unverified`; background relay retains server-set connection headers |
| Schedules | Mutation/run-now attribution uses the same resolver; execution converges through `deliver_automated` and `Sender::Amux`, never a verified worker inferred from schedule env |
| Chat tab/relay | Companion send receives the same Sender, stamps text and starts an owner turn only for proved Owner; an owner-relay quote is accepted only from a proved worker identity |
| Stop (`stop_session_process`, tmux and herdr) | Types nothing into the pane (round-2 B4): SIGTERM to the provider CLI (the pane shell's children; herdr's exec'd pane process), SIGKILL of its tree after the 15s grace, logged `worker_stopped_by_sigterm` or `worker_stopped_by_sigkill`; a SIGKILLed pane's tty modes are reset through the device, not a typed `stty` |

Unverified claims emit `message_sender_unverified`; absent proof emits
`message_sender_unattributed`. Existing `worker_identity_overridden` continues
to name mismatched process/header identity. Sender stamps survive durable
steering storage and are rendered at delivery, beside any server-added notes.

## CLI receipt reporting

The send response reports `stamp_label` for known direct stamps, allow-listed
bare commands, and queued sender labels. Chat sends report their rendered label.
The Bash CLI prints that field, or explicitly says the server did not report a
stamp. A queued label says queued; a durable acceptance receipt or deduped retry
without a label does not claim that a stamp was applied. Owner dialog responses
whose bare/stamped branch is not exposed by the receipt remain unreported.

## Main incident trace

On main `51847291`, `session_verbs.rs:28431` takes the sender from the header;
`:28457` calls `origin_stamped`; `:14192` returns `None` when `origin == name`.
The outside launchd process claimed `dev-lead` and sent to `dev-lead`, so this
self-send exemption delivered its prose bare. It was not a special CLI flag.
In #10, that old helper remains only in historical composer/receipt matching;
new pane deliveries converge through `delivery_text`, which has no prose
self-send exemption and uses the connection-derived Sender.
The CLI unconditionally printed "origin-stamped" on HTTP success.

The locally available `origin/main` during this task was `dd9de428`; the same
branches are at `session_verbs.rs:28468` and `:14197`. A standalone unit test
compiled from those exact main functions reproduced the failure: 0 passed,
1 failed (`outside_self_claim_must_not_deliver_bare`). This task's head remains
`81b23c59`; no commits or git-metadata writes were attempted.

## Unit verification evidence

`amux-isolated python3 scripts/test-message-provenance-unit.py` compiles the
shipped dependency-free sender decision directly and extracts the exact shipped
process classifier with fake process tables. It starts no server or tmux.
Results: sender policy 4 passed; ancestry classifier 3 passed.

| Sender unit test | Rule whose removal makes it fail |
| --- | --- |
| `outside_process_claim_never_becomes_owner_or_bare` | `message_identity.rs:26`, `if mismatch`; also `:47`, the bare-command gate |
| `pane_a_claiming_b_names_both_without_verifying_b` | `message_identity.rs:26`, `if mismatch` |
| `own_pane_verifies_a_even_without_environment_headers` | `message_identity.rs:32`, the pane-proof branch |
| `owner_requires_a_browser_or_tailnet_connection_and_the_ui_token` | `message_identity.rs`, `e.owner_ui` in the owner condition |
| `a_token_bearing_local_or_lan_process_is_not_the_owner` | `message_identity::owner_connection`, limited to `browser` and `remote` |

All four rule mutations were run with `scripts/mutate.sh run`, compiling the
real decision with `rustc --test`: each returned test-failure exit 101, and the
restored code passed. Reintroducing the CLI's first-attempt "origin-stamped"
claim made its tests fail 4 of 30 outcome cells; restoration passed all 30.
Temporary mutation evidence: `/tmp/amux-provenance-unit.CD4Bv6/*.log`.

`amux-isolated bash scripts/test-send-retry-annotation.sh` -> PASS (30 outcome cells).
`bash -n amux` and `git diff --check` -> exit 0.
Rustfmt parsing of changed Rust files -> exit 0; repository-wide existing
formatting differences mean `rustfmt --check` is not a clean formatting verdict.

`scripts/safe-cargo.sh check -p amux-server --tests --locked --offline` waited
at the shared cargo throttle and was interrupted (exit 130); the crate and its
in-crate `connection_evidence_controls_the_delivered_stamp` test have not been
compiled/run. No `cargo test`, server, e2e, GUI, live tmux, installation or
lockfile/credential changes were performed. Full crate typechecking remains a
handoff requirement, not a claimed pass.

GitNexus executable/index were unavailable in this checkout; the entry-point
sweep used source and call-site searches with `rg`.

## Round-two builder handoff

Local draft `454a2886` was retained; the additional edits remain uncommitted in
the requested worktree; they were committed as `22aac264`. Round 2's B1 and B4
are below.

| Blocker | Retained from draft | Completed fresh |
| --- | --- | --- |
| B2 | Extracted suggestions call `delivery_text` | Retain extracted body if later parked; record actual delivery bytes; writer regression uses production Sender stamp methods; add native slash allow-list cases |
| B3 | Empty sends require `owner_proven`; unproven dialog answers park | Move empty refusal before adapter/pane actions; separate dialog regression; log actual sender and proof |
| B5 | Classify complete holder consensus; memo requires a complete current scan and equal holder sets | Verify both worker orders, pane/outside mixtures, same-worker positive control, added/missing holders, and memo call-site wiring |
| B6 | CI invokes provenance unit script; schedule audit and initial chat expectations use stamps | Fix remaining chat peek expectation; compare schedule/chat test literals with production stamp rendering |

Negative controls used `amux-isolated bash scripts/mutate.sh run` on one exact
source fragment, running the provenance script (or harness-wiring gate for CI).
Every applied mutation failed and was restored byte-for-byte. An initial B3
mutation was refused because its target occurred twice; a unique contextual
fragment was then used and produced the assertion failures below.

| Removed rule | Named red line |
| --- | --- |
| Suggestion formatter call | `extracted_suggestion_writer_receives_formatter_bytes ... FAILED` |
| Extracted body retention | `FAIL: test_extracted_body_survives_dialog_or_draft_parking` |
| Empty-send owner proof | `peer_empty_send_cannot_submit_picker_or_draft ... FAILED`; `unverified_outside_empty_send_cannot_submit_picker_or_draft ... FAILED` |
| Dialog owner proof | `peer_and_unverified_dialog_answers_park_even_if_text_matches_an_option ... FAILED` |
| Holder consensus | `shared_socket_holders_are_order_independent ... FAILED` |
| Complete memo holder equality | `memo_holder_equality_rejects_added_or_missing_holders ... FAILED` |
| Memo equality call-site gate | `FAIL: test_memo_requires_complete_current_scan` |
| CI provenance invocation | `NEW UNWIRED HARNESS(ES)` naming `test-message-provenance-unit.py`; `test-harness-wired: 3 passed, 1 failed` |
| Schedule stamp expectation | `schedule_and_chat_expectations_match_production_stamps ... FAILED` |
| Chat peek stamp expectation | `schedule_and_chat_expectations_match_production_stamps ... FAILED` |

Logs: `/private/tmp/pr10-fixes-p_iuz5a4/B*.log`.

| Command | Actual result |
| --- | --- |
| `amux-isolated python3 scripts/test-message-provenance-unit.py` | Exit 0; 4 Python wiring checks; Rust batches 9, 4, 6 passed (the four identity cases also run within the delivery batch) |
| `amux-isolated bash scripts/test-harness-wired.sh` | Exit 0; 4 passed, 0 failed |
| `amux-isolated rustfmt --edition 2021 --emit stdout --config skip_children=true crates/amux-server/src/api/session_verbs.rs crates/amux-server/src/api/process_identity.rs crates/amux-server/src/api/schedules.rs crates/amux-server/tests/worker_types_e2e.rs > /dev/null` | Exit 0; syntax parsing only, no formatting writes |
| `git diff --check` | Exit 0 |
| `amux-isolated scripts/safe-cargo.sh check -p amux-server --tests --locked --offline` (20-second process-group deadline) | Waited at shared two-build throttle; terminated after 20s before compilation; no typecheck verdict |
| `gh run view 39103121996 -R maxtongwang/amux --log-failed` | Exit 1; cannot connect to `api.github.com`; hosted CI not reverified |

The standalone writer case executes the production suggestion assignment with a
recording formatter using production Sender stamp methods. The native
`extracted_suggestion_commands_follow_the_delivery_allowlist` test exercises the
real formatter/regex policy, but was not executed because no Cargo tests were
allowed. Native schedule/chat endpoint tests were likewise not executed. No
server, e2e, GUI, live tmux, commit, lockfile, env or credential changes occurred.

## Round 2: B1 and B4

| Blocker | Change |
| --- | --- |
| B1 | Owner input needs the UI token and an owner connection (see Owner connection proof). `owner_connection_proven` and the sender decision share `message_identity::owner_connection`; the owner-pane path and LAN peers no longer prove the owner |
| B4 | `stop_session_process` types nothing: SIGTERM, a 15s grace, then SIGKILL of the provider tree, behind `StopIo`, which has no pane writer. The writer-sweep exemption for stop is gone, with the now-unused `type_line` and `herdr_prompt` |

Each rule was broken alone with `scripts/mutate.sh run`, through `amux-isolated`;
every mutation landed, went red and was restored (the diff hash was identical
before and after).

| Mutation | Red tests |
| --- | --- |
| `owner_connection` also accepts `outside` | `a_token_bearing_local_or_lan_process_is_not_the_owner`, `b1_a_token_bearing_curl_is_not_the_owner`, `b1_a_browser_started_by_a_launchd_job_is_not_the_owner`, `b1_a_bad_or_missing_signature_is_not_the_owner`, `b1_a_wrong_team_id_is_not_the_owner`, `connection_evidence_controls_the_delivered_stamp`, `the_owner_is_proven_by_the_connection_alone` |
| Ancestor walk stops after the first verified process | `b1_a_browser_started_by_a_launchd_job_is_not_the_owner`, `b1_a_bad_or_missing_signature_is_not_the_owner`, `b1_a_wrong_team_id_is_not_the_owner` |
| Team check disabled | `b1_a_wrong_team_id_is_not_the_owner`, `a_browser_needs_identifier_team_and_a_verifying_signature` |
| Strict verification skipped | `b1_a_bad_or_missing_signature_is_not_the_owner`, `a_browser_needs_identifier_team_and_a_verifying_signature` |
| Every network peer treated as tailnet | `b1_a_tailnet_peer_is_the_owner_and_a_lan_peer_is_not`, `the_decision_grants_outside_only_on_a_fresh_inventory` |
| `owner_connection_proven` ignores the connection | `the_owner_is_proven_by_the_connection_alone` |
| Stop types `/exit` again | `a_stop_types_nothing_into_the_pane`, `only_the_send_layer_types_message_text_into_a_pane` |
| Grace period skipped | `a_provider_that_exits_on_sigterm_is_never_sigkilled`, `sigterm_escalates_to_sigkill_only_after_the_grace_period` |
| SIGTERM not sent | `a_provider_that_exits_on_sigterm_is_never_sigkilled`, `sigterm_escalates_to_sigkill_only_after_the_grace_period`, `an_unconfirmed_exit_after_sigkill_is_a_failed_stop` |

Positive cases that are not mutation targets: `b1_a_signed_browser_outside_every_pane_is_the_owner`,
and `b1_the_same_browser_under_a_worker_pane_is_not_the_owner`, which is the pane classifier's rule
(that worker's input). The chaos fake `claude` now logs and ignores SIGTERM, and
`conversation-recycle-recovery.mjs` waits for that SIGTERM and asserts no `/exit`. Neither was run,
because e2e is outside this builder's allowed commands.
