# PR 95 review fixes

Review: `FINDINGS` for `af4aaa71be2f09c51df12ee04a66a4016c628af8`,
2026-11-06, blockers B1 and B2. The review's notes are unchanged.

## Changes and coverage

| Blocker | Root cause and fix | Regression evidence |
| --- | --- | --- |
| B1 | Interpreter matching missed framework Python's `Python` basename. Normalize interpreter case while retaining the first-script-argument constraint. | The framework matcher assertion failed on the original function and passes on the fixed function; ordinary Node/Bun/Python entries and non-provider arguments are checked too. |
| B2 | Setters warned and continued, and launch admission preceded the child shell's imports. Check each secret setter, and settle the existing private startup receipt only after imports. Import refusal returns through `shell_start_failed` before resume recovery. | The original transport acknowledged a refused import as success; the same assertion now passes. New tests verify both resume fields and `start_error` after setter/import refusal, no credential in errors/logs, receipt expiry, and receipt removal before provider invocation. |

The setter validator consumes only the key name and process status, never
stderr or a secret value. It emits `vault_env_import_failed` with the operation
and measurement status. The shell emits a names-only error receipt, which the
existing startup failure recorder logs and returns to its caller.

Class sweep used repository searches for secret setters, provider-command
builders, startup submissions, and the provider matcher, followed by reading
their branches and callers.

| Path | Result |
| --- | --- |
| Surviving-session vault/token setters | Refusal or missing command result stops startup and records the key. |
| Fresh-session vault/token setters | Same validator and failure recorder. |
| Fresh-session deferred provider secrets | Previously ignored setter status; now uses the same checked boundary. |
| Surviving-session provider-auth setters | Already fail startup on refusal and redact values; retained. |
| Initial provider, Claude fresh fallback, Codex update retry | All three launch submissions use the common provider program and import receipt. |
| Generic startup transport tests | Launch fixtures now use the actual provider program so they exercise the import acknowledgement contract. Existing assertions remain. |
| Scheduler and chat | Separate process-env adapters; unchanged, as scoped by the review. |

## Verification and limits

All test invocations requested `TMPDIR=/private/tmp nice -n 10`. The sandbox
reported `nice: setpriority: Operation not permitted` and continued; reduced
priority is therefore unverified. No network or live worker state was used.

The normal `scripts/test-contended.sh --offline -p amux-server --lib worker_token_`
path exited 1 before tests because the shared target fingerprint directory was
not writable. A writable APFS copy of cached artifacts was used for subsequent
commands, with `CARGO_BUILD_JOBS=1` and the repo's `cargo-target-guard.py` and
`cargo-budget.py`. Their process probes were sandbox-denied and logged their
unenforced measurements. No budget thresholds were changed.

| Check | Actual result |
| --- | --- |
| `rustc --test` on the original matcher plus its new framework regression | Exit 101; 0 passed, 1 failed, naming framework `Python`. |
| Small harness using the original `shell_command_with`, provider builder, import emitter and native-arch helpers | Exit 101; 5 passed, 1 failed: a refused import was acknowledged as an admitted launch. B1 was already fixed in this run. |
| Same small harness after the receipt fix | Exit 0; 6 passed, 0 failed. |
| Expanded harness with committed regression tests and their actual metadata/fixture helpers | Exit 0; 16 passed, 0 failed, 0 ignored. |
| The two committed regressions with only the matcher and transport functions restored from reviewed `HEAD` | Exit 101; 0 passed, 2 failed. |
| Same two committed regressions with fixed functions | Exit 0; 2 passed, 0 failed. |
| `cargo check --offline --workspace` under the repo's budget supervisor | Cargo reported `Finished` without compiler errors; the supervisor exited 124 for `disk_reserve`. This guarded check is not green. |
| `rustfmt --check --edition 2021` on both worker-launch environment files | Exit 0. |
| `git diff --check` | Exit 0. |

The expanded harness compiles the complete `worker_launch_env.rs` module and
copies the required production functions and existing test helpers verbatim.
It includes all four new tests from `worker_launch_env_tests.rs`, its existing
actual-process warning test, and the existing isolation, workspace, durable
startup-failure, deferred-secret and import-order tests. It adds one focused
transport refusal assertion. No production dependency is replaced by a stub.
The only fake executable is the tmux fixture that deliberately refuses a
control command or supplies fixture environment output.

This is selected function-level coverage, not the server's complete test
binary. The ordinary Cargo regression build first encountered a borrow error
in a new test fixture, which was corrected. The subsequent build was stopped
with exit 130 when disk space fell below the build reserve. A later build with
`--config 'profile.test.package.amux-server.debug=0'` was also stopped with
exit 130 for disk pressure before tests ran. Neither supplies a test verdict.
The complete server suite and private-tmux lifecycle cases remain unverified
in this fix session; they remain enabled and unchanged.

Local durable artifacts are under `test-results/amux95/` in the task checkout:
`leaf-harness.py`, the generated source snapshot, compilation/test logs,
before-fix failure logs, Cargo attempt logs, and cost readings. That directory
is ignored by Git. Temporary binaries are disposable. Central test-results
history was unavailable because its lock was sandbox-protected. The janitor's
lock was likewise protected; scratch removal refused the local target, so the
generated target was retained rather than bypassing cleanup protections.
The untracked `.target.reclaim.pending.lock` is generated guard state and must
not be staged with the six intended source/doc files.

Updated surfaces: the three Rust files, the launch contract, this report, and
the link from the original launch evidence. External tracking issues, the PR
body, CI, and live server/worker state were not updated; the offline wrapper
owns commit, push and the PR's Review fixes section.

## Final check result

The 16 selected function-level cases pass. Both committed blocker regressions
also fail with their original production functions and pass with the fixes.
Workspace compilation emitted `Finished` in 2m 39s, but the supervisor observed
3,751,936,000 free bytes, below its unchanged reserve, and returned 124. The
guarded check therefore remains unsuccessful. Clippy and full Cargo tests are
unverified; no further heavy run was started below the reserve.

Re-review should run the repository's normal Cargo tests and required Clippy
gate on the wrapper's committed candidate with adequate disk space. These
results do not authorize a merge. The wrapper owns publication and independent
review.

## Time and usage

Wall time to the cost snapshot: 34 minutes, from the native session start at
`2026-11-06T09:05:22.400Z` through `2026-11-06T09:39:21.335Z`. Claude weekly
and Codex 7-day percentages before→after were not measured. The task/session
filtered `cost-equiv.py` reading is **$2.15 API-equivalent value**, with no
unpriced models or malformed records. Later handoff tokens are excluded.
