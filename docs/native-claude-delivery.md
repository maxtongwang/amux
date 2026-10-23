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

## Structured agent interface

Use the existing authenticated worker client; identity is verified by the server,
never supplied by a body field or inferred from an origin stamp.

| Request | Input and output |
| --- | --- |
| `POST /api/workers/{worker}/send` | JSON `text` and stable client `msg_id`; returns `ok`, `id`, `submitted`, `submission`, optional `queue_id` and `delivery_receipt` |
| `GET /api/sessions/{worker}/send?msg_id={client_id}` | Read durable acceptance and the current `delivery_receipt` without sending again |
| `GET /api/history/{id}` | Existing Messages row by numeric ID or `MSG-{id}`, with its current receipt |
| `GET /api/history?session={worker}` | History rows with receipts batched by exact client/queue IDs |

Example peer request body: `{"text":"Review the attached task note","msg_id":"stable-client-command-id"}`.
Keep the same client ID when reading a late receipt. The server assigns a stable
attempt ID and separate native `msg_id`/command `uuid`; these are distinct from
the client's `msg_id`. Receipt timestamps are Unix seconds; existing history
`delivered_at` remains milliseconds.

For an unconfirmed write, `submitted` is `null`, `submission` is
`written_unconfirmed`, and `delivery_receipt.state` is `written_unconfirmed`.
The receipt also contains `transport`, `attempt_id`, native `msg_id`, `uuid`,
endpoint PID/start/session, executable hash, reason and evidence timestamps.
`ok`/`accepted` report acceptance into Amux. Only `admitted`/`consumed` provide
positive receiver evidence. Native denial, refusal, expiry and drop are durable
receipt states; a pre-write identity quarantine returns HTTP 409 with stable
`code: "native_delivery_quarantined"`, a readable reason and remediation.
Neither authorizes another transport or a fresh send.

The CLI reads the same structured evidence on first replies, retry replies and
late receipts. It prints `ADMITTED` or `RECEIVED` only with positive evidence;
other native states print a refusal or unconfirmed result, return nonzero and
say not to resend. Responses without a structured receipt retain existing CLI
behavior. Machine clients should use the API state rather than parsing prose.

## Fixture verification

All Rust fixtures use private temporary directories, injected process identity,
fixture registry records, a fake UDS server, fixture databases and recording
terminal adapters. Crash tests SIGKILL a subprocess against a fixture WAL database.
They do not invoke Claude, live tmux, the default Claude socket namespace, the
live server or the user's Amux state.

The synchronous SIGKILL peer polls a non-blocking listener, then explicitly
clears inherited non-blocking mode on accepted streams before buffered reads.
Both its read and callback write operations have fixture deadlines; read errors
cannot count as EOF. A socket-pair regression forces non-blocking mode before
configuration so this contract is tested without an accept/scheduling race.

```sh
CARGO_TARGET_DIR="$HOME/.amux/rust-build-target/verification" scripts/safe-cargo.sh check --workspace
scripts/test-native-delivery.sh
CARGO_TARGET_DIR="$HOME/.amux/rust-build-target/verification" scripts/safe-cargo.sh clippy --workspace --all-targets -- -D warnings
node --test e2e/native-delivery-chip.test.mjs e2e/board-delivery-chip.test.mjs
python3 scripts/test-native-delivery-cli.py
```

The fake server establishes client and recovery behavior, not compatibility with
a real Claude executable. Repeat disposable-instance validation before enabling
this profile and on every Claude upgrade. Add reviewed profiles through the
transport registry; do not add provider branches to send or drain callers.
