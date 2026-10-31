# Restart a worker

`amux restart <name> [-y] [--when-idle] [--drop-tasks] [--json]` reloads launch-time
settings while resuming the recorded conversation. It keeps the worker's name,
env, groups, cards and memory. `amux fresh` still discards the conversation.

A running worker must be positively idle. A busy or unmeasured running worker
is refused. Stopped workers can restart from their recorded conversation. `--when-idle` prints the reason to stderr and retries preflight every two
seconds for up to five minutes. A final busy check may still refuse if work starts
between preflight and restart. `-y` skips terminal confirmation. No terminal without `-y` is a refusal.

Claude, Codex and Gemini use `cc_conversation_id`, `codex_session_id` and
`gemini_session_id`, respectively, through the existing launch adapters. Missing
IDs and unsupported providers or worker types are refused before stopping.
Explicit restart disables the ordinary Claude launcher's fresh-conversation
fallback; failed resume is an error.
Human output prints the conversation ID before and after. A changed ID or failed
launch is an error; inspect lane status before retrying an ambiguous failure.

## Scheduled task safety

Task restoration is **unverified on this host**. Restart inventories Claude
`CronCreate` and `ScheduleWakeup` calls in the current conversation transcript,
plus the project's `.claude/scheduled_tasks.json` if present. These are
conservative records, not a claim that every listed task is still active:
completed and cancelled tasks can also appear, and project records can belong
to another lane. Unreadable or malformed sources refuse the restart.

When records exist, restart prints them and refuses unless `--drop-tasks` is
explicitly supplied. That flag accepts possible task loss; it does not delete
the task store or promise which tasks survive. Keep the printed records to
recreate missing tasks. `-y` alone never accepts task loss.

[Claude's scheduling docs](https://code.claude.com/docs/en/scheduled-tasks) say
resume restores unexpired cron tasks, with exceptions for expired/past-due tasks,
self-paced loops, background Bash and monitor tasks. Installed `claude --help`
confirms `--resume [value]`. Neither proves this installed version restores tasks.

The throwaway experiment used Claude Code **2.1.310**, a temporary working dir,
`HOME`, `CLAUDE_CONFIG_DIR` and `XDG_CONFIG_HOME`, with credential-bearing env
variables removed. It touched no live lane or private config:

```text
claude --bare --setting-sources '' --settings '{}' --strict-mcp-config \
  --mcp-config '{"mcpServers":{}}' \
  --session-id 10710710-7107-4107-8107-107107107107 -p \
  'Create one session-only and one durable CronCreate task for next week, then list both with CronList. Do not run any other tools.'
exit: 1
Not logged in · Please run /login
```

No tasks were created, so a second `--resume` invocation could not establish
persistence. A credentialed throwaway-lane experiment remains required before
relaxing this conservative task gate.

## Structured agent surface

The CLI uses `PATCH /api/sessions/<name>/config`:

```json
{"restart":true,"dry_run":true,"drop_tasks":false}
```

`dry_run` inventories tasks and checks the idle boundary without stopping. For
execution, send `dry_run:false` (or omit it), with
`expected_conversation_id:"<preflight ID>"` to refuse stale preflight identities.
The server rechecks safety while holding the same lane send lock as direct
messages, and retains it throughout stop/start. `new_conversation` is absent.

The result includes `schema_version:1`, `name`, `provider`,
`conversation_id_before`, `conversation_id_after`, `restarted`, `tasks` and
`error` (null on success). Busy refusals also include `code:"restart_busy"`.
Preflight has `restarted:false`; execution succeeds only with `restarted:true`
and equal IDs. HTTP 409 refuses before stopping; HTTP 500 signals a failed
restart or changed identity. `--json` emits one result object on stdout; wait
messages and task records printed before execution go to stderr. CLI exit codes: 0 restarted, 1 refused/failed, 2 usage.

Logs expose `conversation_restart_refused` and `conversation_restart_result`
verdicts, with reason, provider and identity preservation, without task prompts.
