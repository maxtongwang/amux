# Local build and Amux resource budgets

`scripts/safe-cargo.sh` is the entry point for local checks, tests, clippy and
release builds, including Make targets, installation, and historical build replay. The replay helper uses
the current wrapper even when compiling a revision older than the wrapper.
`make run` uses the signed atomic builder; `make dev` budgets compilation and
then runs the explicitly requested development server outside the build timeout.
It now supervises its own Cargo process group, preserving the
existing target lease and systemd isolation. A budget failure returns a nonzero
exit code and emits a JSON `cargo_budget_refused`, `cargo_budget_stopped`, or
`cargo_budget_unmeasured` record into the invoking build/test log. It does not
signal a peer build or a worker.

| Resource | Default | Override |
| --- | --- | --- |
| Simultaneous wrapper invocations | 2 | `AMUX_CARGO_MAX_CONCURRENT` |
| Compiler jobs per invocation | 2 | `CARGO_BUILD_JOBS` or explicit Cargo `-j` |
| Test threads per test executable | 2 | `RUST_TEST_THREADS` or explicit libtest argument |
| Incremental compilation | on (fork, `scripts/fork-build-speed.sh`) | `CARGO_INCREMENTAL` |
| Dev/test debug info | `line-tables-only` (fork) | `CARGO_PROFILE_DEV_DEBUG`, `CARGO_PROFILE_TEST_DEBUG` |
| Compiler cache | `sccache` when on `PATH` and incremental is off (fork) | `RUSTC_WRAPPER`, `AMUX_CARGO_NO_SCCACHE=1` |
| Aggregate RSS of owned process group | 12 GiB | `AMUX_CARGO_MAX_RSS_MB` |
| Elapsed Cargo execution | 1 hour | `AMUX_CARGO_MAX_SECONDS` |
| Combined selected target directories | 40 GiB | `AMUX_CARGO_MAX_TARGET_GB` |
| Free space reserve on their volumes | 4 GiB | `AMUX_CARGO_MIN_FREE_GB` |
| Idle debug-cache cleanup threshold | above 32 GiB | `AMUX_BUILD_DEBUG_CLEAR_ABOVE_GB` |
| Retry delay for unchanged failed build inputs | 15 minutes | `AMUX_BUILD_FAILURE_RETRY_SECS` |

Memory/time are sampled every 2 seconds, disk every 30 seconds. These are
monitored ceilings, not kernel reservations: a burst can overshoot between
probes. Three consecutive failed probes stop the run instead of reporting a
false measurement. The target limit covers the shared cache, so growth by a
concurrent build can stop this invocation too. Exit 75 means admission/config
refusal; 124 means an execution budget/probe limit; signals retain their conventional
128+signal code. Successful and ordinary failed Cargo exit codes survive.

No budget deletes a file. The existing idle builder cleanup remains the place
for reclaiming oversized debug targets, under Cargo's lifetime and native locks.
The hourly retention sweep now uses that same guard for *every* discovered old
target name, including names outside the two conventional target paths. Cleanup
defers while a build/test is active or the process probe cannot run. Native lock
files remain in place. The canonical target remains excluded from age retention.

The fork defaults (incremental on, `line-tables-only` debug info) change the
fingerprint once, so the first check/test after the change recompiles. Release
optimization/signing settings are unchanged. Developers needing full debug
information can set `CARGO_PROFILE_DEV_DEBUG=2` for that invocation, within the
same budgets. Incremental state roughly doubles a target directory (measured
18G to 34G for one full build and test of amux-server). That is above the
32 GiB idle debug-cache cleanup threshold, so idle cleanup can clear the
incremental state and the next build is cold again, and it leaves less headroom
under the 40 GiB `AMUX_CARGO_MAX_TARGET_GB` budget, which stops a run (exit
124) rather than deleting anything. Setting `CARGO_INCREMENTAL=0` also
turns on sccache (see the table), so that switch alone does not give a
cold-cache comparison: add `AMUX_CARGO_NO_SCCACHE=1` for one. An unchanged failed
release no longer recompiles every minute; a changed build-input commit retries
immediately. The failure marker is cleared only after a successful installation.

## Other Amux storage and process ownership

The live `/api/debug/storage` and `/api/system-jobs` endpoints were measured on
2026-09-12. Storage retention and browser expiry were running, not disabled.

| Area | Existing policy / practical limit |
| --- | --- |
| Server tracing log | 64 MiB copy-truncate threshold, previous generation retained |
| Session logs | 20 MiB each; rotated/stale logs retained 3 days |
| Storage sweep | Hourly, so log thresholds can overshoot between sweeps |
| Append-only operational tables | Named 14–180 day retention; measured per-table outcomes |
| Old noncanonical Cargo targets | 3-day eligibility, now subject to active-target guard |
| Browser processes | 5-minute activity expiry, empty-profile expiry, 4-hour default hard TTL |
| Browser profiles/login state | Kept on disk; process expiry does not erase login state |
| Evidence/audit packages | Existing 7/30-day retention, respectively |
| Board-referenced uploads | Protected by the existing reference keep-set |

The development-host snapshot had about 24 GiB in Amux's state directory:
1.7 GiB shared target, 7.1 GiB separate test target, 4.8 GiB logs, 3.2 GiB browser
profiles, and 3.0 GiB consistent snapshots. Cargo's downloaded registry was about
1.1 GiB. These are observations, not new quotas. No Cargo process was running at
the start of the audit. The live server's RSS was about 744 MiB. Existing swap
usage alone does not attribute current memory pressure to Cargo.

**Diagnostic run folders:** hourly guarded retention is documented in
[automatic housekeeping](automatic-housekeeping.md). One such folder accounted
for 3.6 GiB; recent or referenced output remains protected.

**Known remaining scope:** persistent browser profiles, consistent snapshots, task-linked output,
and arbitrary files written by workers also have no universal byte quota.
They may contain user work and are not silently deleted by this change. Use
the existing disk/reclaim inventory to review them. A caller bypassing
`safe-cargo.sh`, or a child deliberately starting a separate session, is outside
the process-group supervisor. Provider memory and arbitrary worker commands are
not claimed to be bounded by a Cargo wrapper.

## Acceptance evidence

`LC-CARGO-RESOURCE-BOUNDS` is part of the consolidated lifecycle and runs in its
full mode and CI. `python3 scripts/test-cargo-budget.py` exercises real disposable
memory-heavy child processes, timeouts, signal forwarding, target growth,
low-space refusal, unmeasured probes, wrapper defaults, peer survival, and
unchanged-source retry backoff. `scripts/test-cargo-target-guard.py` exercises
target leases and safe idle reclaim, including writer preference for a starved
reclaim: back-to-back builds, stale and orphaned pending markers, and its bounds.
Rust coverage additionally protects an arbitrarily named target holding a live lease.

Validation results and final deployed identity are recorded in the accompanying
resource validation report; these tests do not certify unrelated lifecycle UX.

## Reclaim writer preference

Builds hold the target lease shared, and flock gives a waiting exclusive reclaim
no priority, so overlapping builds could defer it indefinitely while the budget
refused every new build. After consecutive busy-lease deferrals, the reclaim
writes `.<target>.reclaim.pending` beside the lease (pid, time, streak) and waits
for running builds. New `cargo-target-guard.py run` builds wait behind a fresh
marker; running builds are never interrupted. The verdict adds `priority`,
`waited_s` and `pending_streak`; builds log `cargo_reclaim_pending*` lines.
A timed-out window records its end and logs `cargo_reclaim_priority_timeout`.
Further attempts defer normally during the cooldown, with
`priority_backoff_remaining_s` in the verdict. Only reclaim lock contention
counts toward `priority_streak`; probe failures and refused build/clean runs do
not. The existing diagnostic `deferral_streak` still counts other reclaim refusals.

The reclaimer locks the marker inode before atomically publishing it and keeps
that lock throughout its wait. Readers probe the lock, so pid reuse cannot keep
a dead marker live. A stable `.reclaim.pending.lock` serializes publication and
cleanup; peers cannot replace or delete a locked marker. These sidecar files
live beside the target, outside reclaimed profiles.

| Variable | Default | Meaning |
| --- | --- | --- |
| `AMUX_CARGO_RECLAIM_PRIORITY_AFTER` | 3 | Busy deferrals before the reclaim takes priority; 0 disables |
| `AMUX_CARGO_RECLAIM_PRIORITY_WAIT_S` | 1800 | Reclaimer's wait for running builds before deferring |
| `AMUX_CARGO_RECLAIM_PRIORITY_BACKOFF_S` | 1800 | Cooldown after an unsuccessful priority window |
| `AMUX_CARGO_RECLAIM_BUILD_WAIT_S` | 3600 | New build's wait behind a marker before leasing anyway |
| `AMUX_CARGO_RECLAIM_PENDING_MAX_AGE_S` | 3600 | Older markers, or ones without a held lock, are ignored |
| `AMUX_CARGO_RECLAIM_POLL_S` | 2 | Positive poll interval for both waits |

Invalid settings exit 2; the reclaimer wait must not exceed the marker maximum age.

A build nested inside a leased build of the same target (listed in
`AMUX_CARGO_GUARD_LEASED`) neither waits nor makes a reclaim wait for its ancestor.
