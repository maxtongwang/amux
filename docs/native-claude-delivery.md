# Native Claude delivery

Native delivery ships dark. `AMUX_NATIVE_CLAUDE_DELIVERY=1` enables the resolver
for server-authorized peer prose to local Claude workers. Leave it unset until
the dev lead completes a disposable-Claude smoke test in a private namespace.
Owner input, self sends, slash commands, picker answers, keys, attachments,
other providers and worker execution types retain their existing paths.

## Compatibility and identity

| Setting | Profile |
| --- | --- |
| Reviewed executable | Claude Code 2.1.303, macOS |
| Executable SHA-256 | `d2da3b003dc08b88b7230469014fe3e1dc6fe94bc8f648e89b34175d30269370` |
| Wire / registry protocol | `msgV:1` / `peerProtocol:1` |
| Required registry features | `notify_idle`, `artifact_yield` |
| Registry root | Target `CLAUDE_CONFIG_DIR`, server `CLAUDE_CONFIG_DIR`, otherwise `$HOME/.claude` |
| Scope | Registered process descended from the worker's actual pane |

Discovery reads the registry's actual messaging socket, session and process
start, checks the running executable, and revalidates before write. A global
`claude --version`, a successful connect or a PID-shaped filename is insufficient.
Unknown executable profiles and missing compatible inboxes select the existing
paste adapter before writing. Unsafe paths, identity mismatches and protocol
anomalies stop delivery. Quarantine is tied to the process instance and executable
hash; registry heartbeats cannot clear it. A new instance never replays an old attempt.

The relay has an Amux-owned callback socket in the same vetted private directory
as Claude's inbox. It publishes no Claude session record. Callback credentials
must identify the intended Claude PID, UID and process instance. Sender names
are claims on Claude's native wire; the Amux stamp still comes exclusively from
the server-verified request identity. The stamped body is inside one canonical
peer envelope, with nested wrapper tags neutralized. Unknown sender permission
class is omitted; receiver mode is never copied to manufacture parity.

## Receipt state and recovery

| Evidence | Meaning |
| --- | --- |
| `reserved` | Durable attempt intent; no success is implied |
| `written_unconfirmed` | Write was attempted; normal EOF supplies no positive ACK |
| `held` | Claude has parked the message; Amux cannot bypass approval |
| `admitted` | Verified held-release callback proves queue admission |
| `consumed` | Exact receiver artifact IDs, body hash and process provenance match |
| `denied`, `refused`, `expired`, `dropped` | Native disposition; never paste around it |
| `unknown` | Observation deadline, ambiguous I/O or restart; never automatic resend |

The ledger is `native_delivery_attempts`. A unique source key prevents duplicate
attempts across concurrent drains and restart. Write intent commits before the
first write call, including auth. A native-owned steering row remains claimed
and ordered until evidence permits completion; a SQL guard prevents legacy
cleanup or cancellation from erasing uncertain writes. Clearing Amux cannot
cancel a message already parked in Claude.

The existing timer reconciles bounded batches, including after disabling the
flag. A lost callback, missing artifact or expired observation deadline proves
neither consumption nor cancellation. Normal artifact adoption requires exact
command UUID, receiver session, relay PID and body hash; a native message ID,
when exposed by the artifact, must also match. This includes
a matching member of a valid batch. The receiver instance must still
match; relay instance evidence must also be available. Older/incomplete artifacts
remain unconfirmed. No text/time inference advances native receipts.

Send responses, send receipts and history rows expose `delivery_receipt` with
transport, attempt ID, native IDs, endpoint identity, state and evidence times.
Messages renders that state ahead of the legacy delivery chip. Receipt writes
publish the existing Message refresh event only after the transaction commits,
including coalesced callback updates. Acceptance into
Amux, queue admission and model consumption are separate facts. Logs expose
countable selection, pre-write unavailability, quarantine, callback rejection,
state transition and recovery verdicts without tokens or message bodies.

## Fixture verification

All Rust fixtures use private temporary directories, injected process identity,
fixture registry records, a fake UDS server, fixture databases and recording
terminal adapters. Crash tests SIGKILL a subprocess against a fixture WAL database.
They do not invoke Claude, live tmux, the default Claude socket namespace, the
live server or the user's Amux state.

```sh
CARGO_TARGET_DIR="$HOME/.amux/rust-build-target/verification" scripts/safe-cargo.sh check --workspace
scripts/test-native-delivery.sh
CARGO_TARGET_DIR="$HOME/.amux/rust-build-target/verification" scripts/safe-cargo.sh clippy --workspace --all-targets -- -D warnings
node --test e2e/native-delivery-chip.test.mjs e2e/board-delivery-chip.test.mjs
```

The fake server establishes client and recovery behavior, not compatibility with
a real Claude executable. Repeat disposable-instance validation before enabling
this profile and on every Claude upgrade. Add reviewed profiles through the
transport registry; do not add provider branches to send or drain callers.
