# Stop through server — ai-config #301

Implemented on `fix/stop-through-server`; base `origin/main` and HEAD at task
start were `a174dde4222a8e37665d08de803a82dddd4a9ef0` (remote tip confirmed).
Commit-only handoff; caller owns publishing and independent review.

## Changes

- Bash stop uses existing POST `/api/sessions/<name>/stop` for both backends,
  carrying canonical worker identity and the existing token transport.
- HTTP errors, malformed bodies, and negative replies fail without a local kill
  fallback. Output distinguishes accepted shutdown from completed shutdown.
- Server applies the existing peer gate before scheduling stop. Owner/self and
  existing peer policy semantics remain in that shared gate. Refusal logs
  `peer_input_refused`; completion/failure retain `stop_process_tree_confirmed`
  and `session_stop_failed`.
- Stop preserves workspaces, consistent with the server lifecycle contract;
  deleting one immediately after asynchronous acceptance would race shutdown.
- Guard unchanged; additional direct-kill cases prove there is no CLI exception.
- Swept stop/kill alias, stop-all, cmd_rm caller, both backend paths, existing
  session stop route and store worker stop route. CLI targets env-based workers
  resolved by resolve_session; store-only worker semantics were not changed.

## Evidence

Every runtime check used `AMUX_ISOLATED_NO_SANDBOX=1 ~/.local/bin/amux-isolated`
with `env AMUX_URL=http://127.0.0.1:1`. CLI HTTP fixtures bind random loopback
ports; guard tests use a fake tmux. No live worker was started or stopped.
Logs are beside this handoff and the final `build-result.json`.

| Command inside isolation | Exit | Log / result |
| --- | --- | --- |
| `python3 scripts/test-cli-stop.py` with `AMUX_TEST_CLI` extracted from origin/main | 1 | before-cli.log; 12 failing subcases |
| `python3 scripts/test-cli-stop.py` | 0 | after-cli.log; 2 tests, 12 subcases |
| `python3 scripts/test-worker-tmux-guard.py` | 0 | guard.log; 25 tests |
| `bash scripts/test-cli-worktree-dirty-warn.sh` | 0 | worktree-warn.log |
| `bash -n amux` | 0 | Bash syntax valid |
| `scripts/safe-cargo.sh test -p amux-server --test stop_authorization` before server fix | 75 | before-route.log, before-route-retry.log; not compiled |
| `scripts/safe-cargo.sh test -p amux-server --test stop_authorization --test stop_dispatch_responsiveness` | 75 | after-route.log; not compiled |
| `scripts/safe-cargo.sh test -p amux-server --lib stop_route_interrupts_busy_tools_without_waiting_for_composer_commands` | 75 | process-stop.log; not compiled |
| `scripts/safe-cargo.sh check --workspace` | 75 | check.log; not compiled |
| `git diff --check` | 0 | No whitespace errors |

## Outstanding verification

The sandbox allows the shared build target directory but not its sibling
`~/.amux/.rust-build-target.reclaim.lock` or pending lock. The sanctioned
safe-cargo wrapper exits 75 before compilation; its protection was not bypassed.
The first attempt also could not write the default test receipt; subsequent
attempts put AMUX_HOME inside this worktree's ignored evidence directory.
The new Rust test has not been compiled or shown failing/passing. Rerun it on
the base with the test copied in, then on this candidate, with the lock paths
writable. Run the existing private-tmux process-stop test as well; full
worker-pane-to-server shutdown and preservation of another live private worker
remain unverified. Clippy is still required before publication.

GitNexus/index were unavailable. Shared failure-history lookup was refused
writing its `.lock`; no past-failure evidence was available. No dependency
installations or native configuration were changed.
