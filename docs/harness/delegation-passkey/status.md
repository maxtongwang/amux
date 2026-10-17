# Delegation passkey status

Stacked on `feat/owner-delegated-changes` (PR #18), initial base c9cb0215. Tracks maxtongwang/ai-config#160. Scope: draft PR only; no install, live server, or merge.

## Observable acceptance criteria

1. Only the owner bearer can enroll; first enrollment is explicit bootstrap and loudly audited/fed to leads. Later enrollments require Remote origin or an existing UV passkey. Revoking all credentials never reopens bootstrap.
2. In passkey mode, only a valid UV assertion for the exact pending grant can reach its existing once-only apply path. Typed approvals cannot bypass it. Cite mode does not call `approval_proof_ok`.
3. Origin, RP hash, signature, current credential counter, revocation, expiry, operation binding and single use are enforced server-side. Ceremony state survives restart; rejected attempts consume the ceremony.
4. Settings uses shared theme classes and 44px controls, with register/list/revoke and browser create/get ceremonies.
5. Pinned maintained crate, Cargo-generated lockfile, isolated tests and mutation evidence; resource refusals are recorded, never overridden.

## Dependency boundary

PR #18 is still evolving its cite-default setting and `approval_proof_ok` extension. This branch retains the initial base's cite behavior and supplies the passkey implementation under that exact function name. Rebase onto #18's final mode setting before readiness. No hand-written lockfile edits.

## Safety boundary

All test execution uses `amux-isolated`. Cargo uses `scripts/safe-cargo.sh` and the shared target with its normal budget. No tmux guard mutations, live endpoints, installations, or worker messages.

## Implementation state

Server, migration 0127, dashboard ceremonies and four software-authenticator tests written. Three browser adapter tests pass; all three fail under their selected mutation and pass after restoration. Rust verification and Cargo-generated lockfile update are blocked before cargo starts by wrapper slot acquisition in the restricted sandbox. See [verification.md](verification.md) for exact commands, results and remaining probes. Published as draft [amux PR #25](https://github.com/maxtongwang/amux/pull/25) against `feat/owner-delegated-changes`; no install or merge. Publication metadata: `/private/tmp/delegation-passkey-git.SGidlt/repo.git`, worktree at the requested `delegation-passkey` path. Initial implementation commit: e5dd4a09f41977f12dfeecc658b712068aa645bd. Native checkout HEAD remains c9cb0215 because common Git metadata is read-only; the published branch is the handoff identity.
