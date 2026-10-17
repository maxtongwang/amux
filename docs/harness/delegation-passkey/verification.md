# Passkey verification evidence

All test execution below used `/Users/mwang/.local/bin/amux-isolated` (shown as `amux-isolated` for portability). No tmux guards were mutated. No live endpoint was used. Every execution session completed or was interrupted; no test binary remains running.

## Completed checks

| Command | Actual result |
| --- | --- |
| `amux-isolated node --test e2e/delegation-passkey.test.mjs` | `# tests 3`, `# pass 3`, `# fail 0` (initial and final restored-source runs) |
| `node --check crates/amux-dashboard/static/app.js` | exit 0, no syntax errors |
| `rustfmt --edition 2021 crates/amux-server/src/api/delegation_passkeys.rs` | exit 0; parser accepted source, not a typecheck |
| `git diff --check` | exit 0, no whitespace errors |

## Browser mutation evidence

Each mutation printed `mutate apply: LANDED`, the selected test failed, and the trap printed `mutate revert: LANDED`. Final restored-source suite: 3 passed, 0 failed. These are browser adapter checks; they do not prove server cryptographic enforcement.

```sh
amux-isolated scripts/mutate.sh run crates/amux-dashboard/static/app.js "keys?.mode === 'passkey'" "keys?.mode === 'mutated-passkey'" -- node --test --test-name-pattern='pending approval' e2e/delegation-passkey.test.mjs
# not ok 1 - pending approval sends device assertion bound to grant; cite skips device
# tests 1; pass 0; fail 1; exit 1; mutation reverted

amux-isolated scripts/mutate.sh run crates/amux-dashboard/static/app.js 'if (create && cred.response.getTransports)' 'if (false && cred.response.getTransports)' -- node --test --test-name-pattern='registration converts' e2e/delegation-passkey.test.mjs
# not ok 1 - registration converts binary fields and refreshes visible credentials
# tests 1; pass 0; fail 1; exit 1; mutation reverted

amux-isolated scripts/mutate.sh run crates/amux-dashboard/static/app.js '!window.isSecureContext || !navigator.credentials' 'false || !navigator.credentials' -- node --test --test-name-pattern='revoke binds' e2e/delegation-passkey.test.mjs
# not ok 1 - revoke binds authentication to credential and secure-context failure surfaces
# tests 1; pass 0; fail 1; exit 1; mutation reverted
```

## Rust build blocked

```sh
CARGO_TARGET_DIR=$HOME/.amux/rust-build-target scripts/safe-cargo.sh clippy -p amux-server --all-targets -- -D warnings
# safe-cargo.sh: 2 concurrent local cargo build(s) already running through this wrapper -- waiting for a slot
# interrupted during slot acquisition, exit 130; cargo never started
```

The wrapper's throttle path (`$HOME/.amux/cargo-throttle`) is outside this session's writable sandbox roots. Its mkdir errors are suppressed; the message alone cannot distinguish occupied slots from permission refusal. The shared Git metadata was likewise explicitly denied a write when `git fetch` attempted FETCH_HEAD. No concurrency, target budget, HOME, wrapper, or safety setting was overridden. This is NOT a `target_size` result and NOT a successful clippy run. No additional cargo commands were attempted after interruption.

Consequences: Rust typecheck, targeted software-authenticator execution, Rust guard mutations and Cargo-generated Cargo.lock update remain open. The manifest pins 0.5.5, but Cargo.lock is deliberately unchanged because cargo never ran; it was not edited by hand. The four Rust tests are written, not claimed passing. On a host authorized to use the wrapper, run:

```sh
CARGO_TARGET_DIR=$HOME/.amux/rust-build-target scripts/safe-cargo.sh clippy -p amux-server --all-targets -- -D warnings
amux-isolated env CARGO_TARGET_DIR=$HOME/.amux/rust-build-target scripts/safe-cargo.sh test -p amux-server --lib api::delegation_passkeys::tests -- --test-threads=1
```

Rust mutation coverage, current-counter race/restart probes, real SIGKILL recovery, device acceptance, and 375px/desktop screenshots in both themes remain unverified. Playwright is not installed in this checkout (`Cannot find module 'playwright'`). GitNexus CLI/index were unavailable; symbol/call-site scope was inspected with repository text searches.

## Stacked integration boundary

Initial base is c9cb02155046a9ea5e101483df7eab749bb195a0. Remote PR #18 head observed: 57185271b353d9cb0f7c22138c99e0926f9c6a14; its raw grant source still did not contain the final mode/extension implementation when inspected. Main tip observed: 6f3baed55ae3ce89122d0862882b950e6bf260bb. Main has migration 0123, #18 reserves 0124/0126, #10 reserves 0125; this branch adds 0127. Recheck numbering and rebase onto #18's final cite-default owner-only setting before readiness.

Publication uses a private Git metadata directory under /private/tmp, pointing its worktree to this authorized checkout and preserving the original base. This avoids modifying the read-only common Git directory; code edits stay in the requested worktree. The original checkout HEAD remains c9cb0215, so its native status still shows the committed draft as working changes. Use the published draft SHA as the handoff identity.
