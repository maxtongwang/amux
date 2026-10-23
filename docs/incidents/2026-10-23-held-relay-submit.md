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

- Consume complete recent history deliveries in chronological order, at most once per row, with exact coverage of the entire composer. Support expanded/wrapped text and each delivery's sized Claude/Codex paste chip. Owner prefixes, suffixes, punctuation and text between relays refuse submission. No substring ownership matching is introduced; history's word-boundary rules are unchanged.
- Preserve the eight-character floor, including the existing single-chip recognition path. A size-less `[Pasted text #N]` is not proof of its hidden content and remains protected.
- Use one watcher per lane and the existing send lock; cancellation releases its lane claim. Every idle Enter follows a fresh ownership/generation/picker check. Verification gets no hidden retry; a dropped Enter returns through the same checks on the next iteration. Slash deliveries are ineligible.
- Re-arm at the next idle after each 30-minute window, within a six-hour watch budget. Durable recovery reads at most 32 eligible history rows from six hours and runs on every eligible idle sweep, including when a stuck stamp already exists. Combined delivery recognition authorises Enter, not Ctrl-U.
- Log observed state transitions with `recovered_from_history` and `collapsed_display`, plus explicit composer mismatch, rearm, unconfirmed Enter and budget-exhausted verdicts. Preserve owner text and existing receipt truth.

## Verification evidence

The committed `relay_idle_submit_tests` cover ordered relays, once-per-row consumption, owner words and punctuation at all positions, short/slash text, wrapped/chipped displays, generation/pickers, age/row/lane scoping, cancellation cleanup and the former deadline. The explicit private-tmux route replay calls the real `send_post` and sweep: a new send parks behind relays older than 30 minutes, an existing stuck stamp recovers, the first Enter drops, the next submits, and owner/slash/picker cases receive no keys. It uses `tmux_command()` and the test-build private socket, temp home/database/files, no model.

Cargo did not acquire the mandated target directory's build lock. No Cargo compilation or unit/route execution completed. Attempts used `CARGO_TARGET_DIR=$HOME/.amux/rust-build-target/verification`, `AMUX_HOME=/private/tmp/amux-stuck-relay-cargo`, `AMUX_CARGO_MAX_SECONDS=30` (60 for the final test attempt), and `scripts/safe-cargo.sh`:

| Command after wrapper | Result |
| --- | --- |
| `check --workspace --offline` | Exit 124; blocked on build-directory lock, before compilation |
| `test -p amux-server --lib relay_idle_submit_tests --offline -- --include-ignored --nocapture` | Exit 124; blocked on build-directory lock, no tests run |
| `clippy --workspace --all-targets --offline -- -D warnings` | Exit 124; blocked on build-directory lock, no lint result |

The sandbox also denies the budget wrapper's `ps` probe; the wrapper explicitly reported those measurements unavailable and retained its timeout. Early unbounded attempts were interrupted (130), not reported as failures of Rust code. The first receipt write was refused by the sandbox; subsequent attempts routed receipts to the temp home.

A separate `rustc --test` check extracts the actual pure functions and five committed assertions, linking the existing cached regex dependency. Its local driver is `/private/tmp/relay194-proof/extract.py`; it does not replace whole-crate checks:

| Command | Result |
| --- | --- |
| `python3 /private/tmp/relay194-proof/extract.py --baseline` | 2 core regression assertions fail against the pre-fix matcher (0 passed, 2 failed) |
| `python3 /private/tmp/relay194-proof/extract.py` | 5 passed, 0 failed |
| `scripts/mutate.sh run crates/amux-server/src/api/session_verbs.rs 'rows.iter().take(32).rev()' 'rows.iter().take(1).rev()' -- python3 /private/tmp/relay194-proof/extract.py` | Mutation landed; ordered and chip-sequence assertions failed; inverse mutation restored source |
| `scripts/mutate.sh run crates/amux-server/src/api/session_verbs.rs 'if want.chars().count() < 8 {' 'if want.chars().count() < 1 {' -- python3 /private/tmp/relay194-proof/extract.py` | Mutation landed; short-text assertion failed; inverse mutation restored source |
| `scripts/mutate.sh run crates/amux-server/src/api/session_verbs.rs 'if !idle || now < *deadline {' 'if now < *deadline {' -- python3 /private/tmp/relay194-proof/extract.py` | Mutation landed; busy-to-idle rearm assertion failed; inverse mutation restored source |
| `rustfmt --edition 2021 --emit stdout crates/amux-server/src/api/session_verbs.rs > /private/tmp/relay194-rustfmt.rs` | Exit 0, Rust source parsed; emitted formatting was not applied to unrelated code |
| `git diff --check` | Exit 0 |

Class sweep: every call to `verify_submitted`, `retry_held_idle_draft` and `spawn_direct_draft_idle_submit`; sweep restart; exact single-delivery and chip matching; both provider chip formats; unchanged owner/history word-boundary semantics. No push, deployment, live tmux or port access. GitHub remote-tip verification was unavailable (`Could not resolve host: github.com`); GitNexus tooling/index is unavailable. The frustrations audit found 409 entries and 0 structural problems; its optional board probe was refused by the sandbox before connecting. The orchestrator must run the queued Cargo checks and private-tmux replay before publishing.
