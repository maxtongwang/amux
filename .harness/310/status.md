# Secret-exec builder handoff

## Scope and state

Implemented on `feat/secret-exec-310`, based on `origin/main` a174dde4222a8e37665d08de803a82dddd4a9ef0 (confirmed with `git ls-remote --heads origin main`). Issue: maxtongwang/ai-config#310. Commit-only handoff; caller owns publishing and independent review. No plist, real vault, native configuration, dependency installation or lockfile changed. Pre-existing `.codex-build.log` is untouched and excluded.

High risk: new secret-delivery API and job authorization. Round 3 implementation complete. Round 2 Rust check, clippy, targeted server tests and request-log tests passed in lead/reviewer runs; this round's Rust changes await the lead's checks, as required by the brief.

## Implemented behavior

- Bash `amux secret-exec` is self-contained in the installed CLI, using its existing Python 3 dependency. Reads a private job token, sends names and a credential header to an HTTP/HTTPS loopback URL with verified local TLS, validates the exact response-name set, then `execvpe`s separate argv with the fetched environment. Preserves stdin and child exit status. No shell evaluation, proxy, redirect, credential argv, or arbitrary server error echo.
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

## Round 2 middleware fix

Moved only `secret_jobs::routes()` from the owner/worker-protected router into its own authenticated subrouter, merged after process attribution. This uses the existing subrouter/layer composition mechanism, without an attribution allowlist or caller-controlled bypass. The endpoint still enforces loopback peer metadata, registry token, names and groups itself; it never invokes an owner/worker handler. Its existing `secret_job_delivered` and `secret_job_refused` tracing and request logging remain active. The logger middleware is applied directly so its fallback wrapper cannot shadow or conflict with the main router fallback.

Added an assertion that `/api/vault/secrets` with the same job token and loopback metadata returns 403 `worker_origin_unattributed`. Job test requests now omit the owner bearer; empty-token requests retain it to test that an owner credential alone cannot deliver job secrets. Both in-process and real HTTP cases continue using `api::router`, the builder called by production `lib.rs:780`; no test-only middleware ordering or attribution disable was introduced.

Class sweep: inspected all vault route mounts, owner bearer, worker identity/policy, outer process attribution and member identity, the existing independently authenticated callback/ChatGPT subrouters, and request logger composition. Only the job route leaves those owner/worker layers. The outer member-header sanitizer remains in place; the job handler does not use its identity.

Pre-fix evidence: lead/tests.log records the allowed-job assertion failing (403 instead of 200, exit 101); the brief supplies the `worker_origin_unattributed` body. The local pre-fix run (`round2-before.log`) and all post-fix Rust attempts stopped before compilation, exit 75, on the same sandbox-denied shared reclaim lock and receipt directory. No red-to-green execution is claimed. GitNexus is unavailable (no local index); failure-history search also cannot acquire its lock.

| Round 2 command (prefix: `AMUX_ISOLATED_NO_SANDBOX=1 ~/.local/bin/amux-isolated`) | Exit | Evidence |
| --- | --- | --- |
| `scripts/safe-cargo.sh check --workspace` | 75 | `round2-check.log` |
| `scripts/safe-cargo.sh test -p amux-server --test secret_jobs_api --test vault_secrets_api` | 75 | `round2-tests.log` |
| `scripts/safe-cargo.sh clippy --workspace --all-targets -- -D warnings` | 75 | `round2-clippy.log` |
| `scripts/safe-cargo.sh test -p amux-server api::request_log::tests` | 75 | `round2-request-log.log` |
| `scripts/test-contended.sh -p amux-server` | 75 | `round2-suite.log` |
| `rustfmt --edition 2021 --check crates/amux-server/tests/secret_jobs_api.rs` (no prefix) | 0 | Format/parse only |
| `git diff --check` (no prefix) | 0 | Whitespace clean |

Caller next step: rerun the Rust checks outside this sandbox and verify the allowed-job assertion flips to 200, sibling route stays 403, and real CLI/log-canary cases pass. Commit only; no push performed. Existing lead evidence and `.codex-build.log` preserved.


## Round 3 transport and review fixes

Starting head: `e6f63f1e3d092f826143eff9ab2b632fe31c3a2a`. Read both exact-head review reports in `/Users/mwang/GitHub/amux/.git/claude-reviews/`. The older sandbox-denied runs above are historical, not the latest round-2 result: `lead2/summary.txt` reports check/clippy/tests/route/cli all exit 0. Opus independently reports secret_jobs_api and vault_secrets_api (1 each), request-log tests (31), Python tests (6), and clippy passing at that head; Sonnet confirms both integration tests and Python passing. Neither report claims a full server-suite pass.

- The verb reuses `cmd_url`, explicitly exports the resolved value to its Python process, and preserves `AMUX_API` precedence. HTTPS verifies hostname and certificate against the active `tls/connection.pem`, falling back to `tls/cert.pem` only when no active bundle exists. Existing generic CLI clients use insecure TLS options; those were inspected but deliberately not copied. Only localhost/127.0.0.1/::1 and HTTP/HTTPS are accepted; no redirects or proxies. Timeout and response cap are named, and oversize responses fail closed.
- Missing/malformed tokens are checked before registry access. Unknown labels, wrong tokens and registry failures share 403 `job_identity_refused`; detailed registry/unknown-label reasons stay in tracing and value-free refusal audit records. Token hashes must be lowercase 64-hex or load fails with `job_registry_token_hash_invalid`.
- Real-router integration test now uses TLS, a scratch-home certificate and endpoint.json, with no AMUX_API. Distinct values per scope/name expose precedence and worker-scope crossover. Added refusal audit and malformed-hash assertions. These Rust assertions are written/formatted, not executed in this sandbox.
- Help/docs explain that the token is the control even behind tailscale serve. Correct PR usage: `amux secret-exec --job <label> --names A,B -- <cmd> [args...]`.
- Sweep: every AMUX_API read in the verb, dispatcher resolution/export boundary, cmd_url discovery, active certificate precedence, and all CLI additions in the PR (secret-exec is the only new verb). Existing plain HTTP tests remain because ephemeral loopback HTTP is explicitly supported. The non-loopback assertion invokes the sourced verb after the isolation guard, requiring the verb's own refusal code.

| Round 3 check | Exit | Result |
| --- | --- | --- |
| `AMUX_ISOLATED_NO_SANDBOX=1 ~/.local/bin/amux-isolated python3 scripts/test-secret-exec.py` before implementation | 1 | 8 tests; discovery failed with KeyError AMUX_API, trusted HTTPS failed with local_server_url_required; `round3-before.log` |
| Same command after implementation | 0 | 9 tests OK, including both flipped assertions, unrelated-certificate refusal and explicit endpoint precedence; `round3-python.log` |
| `AMUX_ISOLATED_NO_SANDBOX=1 ~/.local/bin/amux-isolated bash -n amux` | 0 | Syntax valid |
| `AMUX_ISOLATED_NO_SANDBOX=1 ~/.local/bin/amux-isolated scripts/test-cli-offset-safe.sh` | 0 | PASS |
| `AMUX_ISOLATED_NO_SANDBOX=1 ~/.local/bin/amux-isolated scripts/test-cli-sourceable.sh` | 0 | PASS, including its negative control |
| `rustfmt --edition 2021 --check crates/amux-server/src/api/secret_jobs.rs crates/amux-server/tests/secret_jobs_api.rs` | 0 | Parse/format only |
| `git diff --check` | 0 | Clean |

Python discovery test disables only the CLI endpoint guard in its scratch child, because that guard deliberately ignores endpoint.json. HOME, CC_HOME, AMUX_HOME, endpoint and trust material all point to the temporary fixture; the outer test remains under amux-isolated. Rust TLS test uses the same scratch-only discovery pattern. No live endpoint or native configuration was touched.

Rust commands are intentionally not attempted this round per brief. Caller must run workspace check/clippy, secret_jobs_api + vault_secrets_api and request-log tests before publishing/reviewing this new head. Prior-head passes do not verify these changes. Failure-history lookup again failed (exit 1, denied .lock); GitNexus index is unavailable. Raw new logs remain local and are not committed; existing evidence and lead directories are preserved. Commit only, no push.
