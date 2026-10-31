# Transcript excerpt redaction

The frustration scanners import `redact` from ai-config's `amux/bin/amux-origin`
through `scripts/redact_loader.py`. Set `AI_CONFIG_DIR` to the ai-config checkout;
the default is `~/GitHub/ai-config`. The upstream module owns the patterns,
secret-key value policy, and maximum length. No pattern table is copied here.
Its guarded CLI does not execute on import through `SourceFileLoader`.

A missing module, absent callable, import error, or redactor error replaces the
excerpt with `[excerpt withheld: redaction table unavailable]`. Dependency
stdout and stderr are suppressed. Counts and theme labels remain readable.
Audit events include `redaction_table_available` for discovery of missing tables.
Redaction precedes excerpt truncation and JSON encoding.

## Script output sweep

The sweep inspected transcript/database/history readers and their output calls
under `scripts/`. This checkout has no `bin/` directory.

| Tool | Transcript output and disposition |
| --- | --- |
| `frustration_scan.py` | Message excerpts use the loader; audit JSONL and stderr use its event sanitizer. |
| `friction_themes.py` | Excerpts and ledger titles use the loader before truncation; evidence uses it in JSON and Markdown; audit JSONL and stderr use its event sanitizer. |
| `token-baseline.py` | Reads message text to classify traffic; emits aggregate counts, rates and token measurements, without excerpts. |
| `hooks/hook-report.sh` | Reads transcript usage fields; reports usage and lifecycle metadata, without message bodies. |
| `hooks/native-status.py` | Does not print transcripts; transports original submitted prompts to the server and durable delivery spool. Redacting transport would change owner input. |
| `hooks/ask-guard.py` | Sends tool input for authorization; outputs a gate reason, without transcript excerpts. |
| `hooks/install-claude-status-hooks.py`, `hooks/install-native-status-hooks.py` | Install hook configuration; no transcript output. |
| `hooks/large-read-guard.py` | Reports file-read delegation decisions and paths; no transcript body. |
| `claude-scratch-report.sh`, `claude-scratch-deliver.sh`, `scratch-reaper.py`, `mac-cleanup-tick.sh` | Inspect transcript existence, age or disk usage; output storage/liveness metadata and generated instructions, without transcript excerpts. |
| `transfer-completed-project.py` | Prints transfer receipts/counts, without excerpts. Preserves original database rows in recovery snapshots and target storage; this is lossless data transfer, not an excerpt report. |
| `orch-pace.py`, `pipeline-bottlenecks.py`, `soak.sh` | Emit aggregate operational measurements and generated notices, without transcript excerpts. |
| `frustrations_audit.py`, `frustrations_retire.py`, `frustrations-archive.py` | Operate on ledger entries and board records, not transcripts; archive preserves ledger source text. |
| `claude-hooks/observed-edits-post.py`, `git-hooks/*` | Report edit paths, Git state and gate decisions, without transcript excerpts. |
| `app-store-screenshots.mjs`, `button-audit.mjs`, `perf-ci.sh`, `fixtures/*`, `test*`, `tests/*` | Generate or inspect synthetic fixtures/test results; no production transcript excerpts. |

## Verification command

Run `python3 scripts/test-redact-excerpts.py` with the project test interpreter.
Three tests cover the loader and both tools using fake credentials, a fixture
redactor, missing configuration, and redactor failure. The theme test checks
JSON, Markdown, audit JSONL and its stderr fallback. The scanner test exercises
the actual SQLite query and serialized findings. Existing scanner suites remain
`scripts/test-frustration-scan.sh` and `scripts/test-friction-themes.sh`.
