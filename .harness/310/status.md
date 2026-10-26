# Secret-exec builder handoff

## Scope and state

Implemented on `feat/secret-exec-310`, based on `origin/main` a174dde4222a8e37665d08de803a82dddd4a9ef0 (confirmed with `git ls-remote --heads origin main`). Issue: maxtongwang/ai-config#310. Commit-only handoff; caller owns publishing and independent review. No plist, real vault, native configuration, dependency installation or lockfile changed. Pre-existing `.codex-build.log` is untouched and excluded.

High risk: new secret-delivery API and job authorization. Implementation complete; Rust compilation and execution remain unverified because of the sandbox/shared build lock (#255).

## Implemented behavior

- Bash `amux secret-exec` is self-contained in the installed CLI, using its existing Python 3 dependency. Reads a private job token, sends names and a credential header to a literal loopback URL, validates the exact response-name set, then `execvpe`s separate argv with the fetched environment. Preserves stdin and child exit status. No shell evaluation, proxy, redirect, credential argv, or arbitrary server error echo.
- `POST /api/vault/job-secrets` requires real loopback peer metadata, a registered label, its token and allowed names. It shares worker identity's digest/constant-time comparison and vault's existing encryption and global/group precedence. It never inherits worker scopes. Delivery is all-or-nothing; missing or wrong-group names, unknown labels, bad credentials, unsafe registries and vault failures refuse with machine-readable errors. Server success/refusal signals and vault audit contain no values.
- Read-only owner registry `secret-jobs.json` (0600, user-owned regular file, no final symlink) lists label/groups/allowed_names/token_sha256. There is no registration or grant-approval API; existing grants cannot widen it.
- Docs include owner registration, crash-between-token-and-registry recovery, revocation/rotation, API contract and plist ProgramArguments. No real registration performed.
- Route added to request-log route table. Vault changes are one additive resolver function; worker identity changes extract its existing comparison, limiting overlap with PR #72.

## Authentication decision for PR

Use a per-job random 32-byte token, kept as 64 lowercase hex characters in a 0600 `job-tokens/<label>.token`. Server registry stores only its SHA-256. Uses `X-Amux-Worker-Token` and the same constant-time verifier as worker identity, with no rule-10 unenforced/unminted bypass. Token possession proves the registered identity, not launchd ancestry. Unrestricted same-UID processes can read other job tokens, as with existing worker identity/grants; this limitation is explicit in docs. Registry and token directories must be outside agent-writable sandboxes. Owner bearer alone and another job's token do not authorize this endpoint.

## Verification evidence

All executable tests used `~/.local/bin/amux-isolated`. Nested `sandbox-exec` initially exited 71 (`sandbox_apply: Operation not permitted`). Subsequent runs use the documented `AMUX_ISOLATED_NO_SANDBOX=1` under the enclosing Codex sandbox; wrapper environment isolation and endpoint guard remain active. Commands below include that prefix.

| Command | Exit | Result / local log |
| --- | --- | --- |
| `AMUX_ISOLATED_NO_SANDBOX=1 ~/.local/bin/amux-isolated python3 scripts/test-secret-exec.py` | 0 | 6 tests, OK; `cli-tests.log` |
| Same CLI suite against a temporary extraction of baseline `a174dde:amux` | 1 | 6 tests, 9 assertion/subtest failures; `cli-before.log` (missing verb) |
| `AMUX_ISOLATED_NO_SANDBOX=1 ~/.local/bin/amux-isolated bash -n amux` | 0 | Bash syntax valid |
| `AMUX_ISOLATED_NO_SANDBOX=1 ~/.local/bin/amux-isolated scripts/test-cli-offset-safe.sh` | 0 | PASS; wrapper/trailing exit+brace intact |
| `AMUX_ISOLATED_NO_SANDBOX=1 ~/.local/bin/amux-isolated scripts/test-cli-sourceable.sh` | 0 | PASS; `cli-sourceable.log` |
| `AMUX_ISOLATED_NO_SANDBOX=1 ~/.local/bin/amux-isolated scripts/safe-cargo.sh check --workspace` | 75 | No build: shared lock denied; `cargo-check.log` |
| `AMUX_ISOLATED_NO_SANDBOX=1 ~/.local/bin/amux-isolated scripts/safe-cargo.sh test -p amux-server --test secret_jobs_api --test vault_secrets_api` | 75 | No tests ran; `server-tests.log` |
| `AMUX_ISOLATED_NO_SANDBOX=1 ~/.local/bin/amux-isolated scripts/safe-cargo.sh test -p amux-server api::request_log::tests::absolute_route_literals_in_api_files_are_tabled_even_if_never_mounted` | 75 | No tests ran; `route-test.log` |
| `AMUX_ISOLATED_NO_SANDBOX=1 ~/.local/bin/amux-isolated scripts/test-contended.sh -p amux-server` | 75 | NO TEST RAN, source fingerprint retained; `server-suite.log` |
| `AMUX_ISOLATED_NO_SANDBOX=1 ~/.local/bin/amux-isolated scripts/safe-cargo.sh clippy --workspace --all-targets -- -D warnings` | 75 | No build; `clippy.log` |
| `rustfmt --edition 2021 crates/amux-server/src/api/secret_jobs.rs crates/amux-server/tests/secret_jobs_api.rs` | 0 | New Rust files formatted and parsed; not typechecked |
| `git diff --check` | 0 | No whitespace errors |

CLI tests verify only the requested value is injected, argv with spaces/quotes is unchanged, stdin survives, child exit status propagates, server-down and distinct refusals fail closed, bad token file permissions are refused, and error/unexpected response values never reach stdout/stderr.

`secret_jobs_api.rs` is written but not run: actual router checks cross-job token refusal, unknown jobs, allowlist refusal, wrong groups, missing values, no worker-scope crossover, absent/remote peers, registry permissions/corruption/revocation. It starts an ephemeral loopback server and executes the actual CLI; sentinel assertions cover child output, server tracing, positive request-log evidence and vault audit. All vault data is fake in a temporary home with file keystore.

## Blockers and next steps

The lead must run the Rust commands above outside this sandbox before claiming verification or publishing. The denied lock is `/Users/mwang/.amux/.rust-build-target.reclaim.lock`; the pending lock and test-receipt directory are also unwritable. No lock bypass or alternate build-cache workaround attempted. `test-results.py failures search --repo amux --text vault` also failed with permission denied on its `.lock`; durable evidence is retained here instead. GitNexus executable/index and an amux project-memory routing entry were not available.

Independent reviewer should inspect the same-UID trust limit and verify the new endpoint through the full middleware stack, plus run the full server suite and route-table assertion. Caller should carry the authentication decision above into the PR. Existing-suite green is not claimed.
