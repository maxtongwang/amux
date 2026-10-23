# Held relay idle submission

Issue: refs #194. Builder: codex, gpt-6.1-sol. Scope: the existing direct-draft watcher and stuck-composer sweep in `api/session_verbs.rs`.

## Read-only log diagnosis

Read every `~/.amux/logs/*.log`, including `server-rs.log`, with binary-safe matching (`rg -a`) and an ANSI-stripping Python counter. Count timestamped records on 2026-10-22 and 2026-10-23 **UTC**, matching the exact `session` field. Worker terminal logs contain source/command echoes; these are not server outcomes. The snapshot ends during October 23 UTC, not after a complete October 23 local day. No live database, tmux, server port or mutable fleet state was inspected or changed.

| Date UTC | Lane | Transcript confirmed | Composer left | Changed by human | Pre-Enter ownership refusal |
| --- | --- | ---: | ---: | ---: | ---: |
| Oct 22 | dev-lead | 19 | 1 | 1 | 1 |
| Oct 22 | business-lead | 28 | 4 | 0 | 0 |
| Oct 22 | mrpopo | 28 | 1 | 0 | 0 |
| Oct 23 | dev-lead | 75 | 1 | 0 | 0 |
| Oct 23 | business-lead | 6 | 0 | 0 | 0 |
| Oct 23 | mrpopo | 29 | 0 | 0 | 0 |
| Total | Three lanes | 185 | 7 | 1 | 1 |

The one watcher mismatch is dev-lead at `2026-10-22T20:10:42.366792Z`; its pre-Enter refusal is at `20:10:39.446014Z` (271 composer characters). These are two signals in one episode, not two proven root causes. The old watcher logged the expected delivery preview, not what its matcher read, so “changed by human” does not establish that a human edited it. No private message bodies are retained here.

| Requested cause/outcome | Observed count | What the evidence establishes |
| --- | ---: | --- |
| Composer mismatch / human_changed | 1 | Single-delivery equality failed; concatenation versus owner edit versus chip is unrecorded |
| gave_up | 0 | No recorded 30-minute expiry on these lanes in this window |
| generating | 0 explicit retry reasons | Busy waits in the watcher were silent; this is not proof there were no busy waits |
| enter_accepted / submitted_at_idle | 0 | No explicit success with either label |
| idle_enter_not_confirmed | 0 | No recorded dropped idle-Enter retry on these lanes |
| Concatenated relays | Not identifiable | Old logs cannot distinguish it from other mismatches |
| Wrapped/collapsed display mismatch | Not identifiable | Old logs do not record the composer representation |
| Stuck composer left for owner | 10 | mrpopo: 9 on Oct 22, 1 on Oct 23; the sweep could not attribute these drafts to a held delivery |
| Idle backoff observation | 1 | mrpopo, Oct 22 `20:33:17.540545Z`, `step=PressEnter`, `presses=0` |

Code inspection found three independent recovery gaps: the watcher accepted only one expanded delivery even though recovery already recognized sized chips; it permanently exited at 30 minutes, including during generation; and the sweep only retried on the initial stuck-state transition. Their production incidence cannot be recovered from the old log fields.

## Implementation and limits

PR #66 review of `c5f8955` reproduced an owner paste being claimed by its line count, and the same problem with an owner copying the relay's bare body. The unsubmitted history row was not proof of composer ownership. The review also found incomplete batch reconciliation and an unlimited Enter loop. This revision addresses those findings:

- Require the expected server-generated `[amux-origin: ...]` prefix in position for every recovered delivery, expanded or chipped. Bare chips and bare bodies remain owner text even when their bytes or dimensions match history. This applies to the sweep, watcher fallback, backoff, verifier retry, steering/project rescue, Ctrl-U eligibility and delivered-duplicate clearing. Fresh explicit sends still deliver the owner's requested text.
- Consume entire stamped deliveries in chronological order, once per row, covering the complete composer. Extra owner text, incorrect stamps, reversed order, extra duplicates, slash commands and unknown-size chips refuse recovery. One watcher per lane and the send lock protect capture, ownership checks, Enter and verification.
- Keep main's 30-minute candidate window (`HELD_RELAY_WINDOW_S`), including direct-send fallback age. Each watcher also stops after 30 minutes. An eligible idle sweep may start a replacement watcher; this does not renew history eligibility. The former “re-arm” merely renewed a log deadline and has been removed.
- Limit recovery to three Enters per held sequence (`HELD_RELAY_MAX_ENTER_ATTEMPTS`), shared by retry/rescue/watch paths. Persist hashed sequence counts in lane metadata before pressing, including across watcher respawns and server restarts. Persistence failure refuses Enter. Reaching the cap records `held_relay_enter_cap_reached` at WARN once; subsequent exhausted sweeps do not reset the counter or repeat that warning. Unconfirmed attempts are INFO.
- Reconcile every history row covered by a combined submission, including `stuck`, `deferred`, `unverified` and queued rows. Match ordered expanded bytes from provider submission evidence, stripping transport paste wrappers. One occurrence still settles one of two identical sends. A confirmed watcher also invokes reconciliation. Keep `reconciled_at` as inferred delivery evidence, rather than an original send receipt.

## Verification evidence

The first review ran Cargo in an isolated environment on the reviewed SHA: relay tests passed and `tmux_target_audit` passed 3/3. The `c5f8955` commit separately records clean clippy. Its broader selected library run reported 163 passed, 1 failed, 3 ignored; the failure was `stop_route_interrupts_busy_tools_without_waiting_for_composer_commands`, also known on main. Those results describe the reviewed SHA, not this revision.

This revision uses the committed `scripts/test-held-relay-review.py`. It extracts the actual production functions and in-tree assertions, compiles them with `rustc --test` and cached dependencies, and runs real SQLite queries against a scratch database. Only the Store/event/home adapters are replaced. It does not validate whole-crate wiring, real tmux, or the running server. The cap wiring assertion inspects each recovery call site before its Enter; it is a source assertion, not an async keystroke replay.

The command prefix for verification here is:

```bash
AMUX_HOME=<scratch>/home AMUX_ISOLATED_NO_SANDBOX=1 \
CARGO_TARGET_DIR=$HOME/.amux/rust-build-target \
~/.local/bin/amux-isolated <command>
```

The existing session sandbox stays active. The wrapper's nested `sandbox-exec` was unavailable (`sandbox_apply: Operation not permitted`, exit 71); no permission override was used. Cargo attempts additionally put a scratch refusal stub first on PATH so they cannot invoke real tmux.

| Command after prefix | Result on this revision |
| --- | --- |
| `python3 scripts/test-held-relay-review.py --source-ref c5f8955 --case <six regression cases>` | 0 passed, 6 failed (exit 101): chip, plain copy, combined pure/SQLite receipts, watch bound and cap wiring |
| `python3 scripts/test-held-relay-review.py` | 13 passed, 0 failed; actual extracted functions and SQL, scratch adapters |
| `python3 scripts/test-held-relay-review.py --mutations` | 11 mutations killed by their named assertions with exit 101; driver exit 0; each inverse restored the production source hash |
| `scripts/safe-cargo.sh check --workspace --offline` | Unavailable: target guard cannot open its lease beside the mandated target (exit 75), before compilation |
| `scripts/safe-cargo.sh test -p amux-server --lib --offline` | Unavailable for the same lease denial (exit 75), no full library tests executed |
| `scripts/safe-cargo.sh test -p amux-server --test tmux_target_audit --offline` | Unavailable for the same lease denial (exit 75), no integration tests executed |
| `scripts/safe-cargo.sh clippy --workspace --all-targets --offline -- -D warnings` | Unavailable for the same lease denial (exit 75), no lint verdict |
| `rustfmt --edition 2021 --emit stdout crates/amux-server/src/api/session_verbs.rs > <scratch>/parsed.rs` | Exit 0; Rust source parses |
| `git diff --check` | Exit 0 |

The six `--case` names, in order, are `owner_chip_matching_a_relay_line_count_never_authorises_keys`, `owner_plain_copy_never_authorises_keys_and_stamps_must_be_in_position`, `combined_receipt_consumes_only_the_submitted_sequence`, `combined_enter_reconciles_each_held_verdict_and_excludes_other_rows`, `history_and_watch_window_remain_thirty_minutes`, and `every_recovery_enter_reserves_the_shared_sequence_budget`.

Mutation coverage: remove the chip's expected stamp prefix; allow the bare body for exact matching; reject valid stamped relays; allow a fourth Enter; remove the watcher's cap reservation; reconcile only one row; omit deferred/unverified candidates; refuse deferred/unverified updates; widen the candidate window to six hours; allow Enter after an attempt persistence failure; and suppress the cap warning. The cap assertion counts actual emitted verdicts, including repeated exhausted sweeps. Each targets its own named case, including a real SQLite assertion for both query mutations.

Class sweep: all `verify_submitted`, `retry_held_idle_draft`, `spawn_direct_draft_idle_submit`, steering/project draft rescue, held-delivery matcher and duplicate-clear callers; both Claude and Codex chip formats; bare/quoted/wrong-position stamps; mixed held verdicts; lane, age and duplicate-row scope. No live tmux, server port, fleet-state write, push or deployment. No private-tmux replay was run because Cargo could not build this revision. Remote-tip verification failed DNS resolution for github.com; GitNexus/index is unavailable.
