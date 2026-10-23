# File Write Refusals

The filesystem mutation APIs share `api::fs::write_refusal`: hidden path
components, sensitive home/system paths, code-execution targets, and existing
executable files are refused. Refusals return HTTP 403 with
`{ok:false, code, error}` and one WARN containing the verdict, `measured=true`,
and `n_considered=1`. Paths in refusal logs use Debug formatting to escape
control characters.

| API | Checked paths |
| --- | --- |
| `POST /api/files/upload` | Requested and resolved paths relative to the configured files root; the opened target is also checked by `atomic_write_checked` |
| `POST /api/fs/upload` | Requested directory and filename, resolved destination, and any collision-suffixed filename |
| `PUT /api/file` | Requested and resolved file |
| `POST /api/fs/rename` | Requested and resolved source and destination |
| `DELETE /api/fs/delete` | Requested and resolved target |
| `POST /api/fs/mkdir` | Requested and resolved directory |

The rooted Files API retains its namespace boundary: the configured root itself
may be hidden (`AMUX_FILES_ROOT` overrides the default `$HOME`), but hidden components underneath it
are refused. The general filesystem APIs accept absolute paths and check all
components. No project-root or dashboard-only exemption exists.

## Dashboard Compatibility Decision

The viewer saves text, spreadsheets, and Draw data through `PUT /api/file`.
Its text allowlist and extensionless branch previously allowed names such as
`.gitignore`. The absolute-path API has no project-root restriction. The shared
rule now refuses those edits, including ordinary files inside hidden folders.

Draw defaults to `_drawDataDir()` (`_AMUX_DATA_DIR`, else
`_AMUX_HOME + '/.amux'`). Its `draw/library.excalidrawlib`, `draw/recents.json`,
and default `draw/drawings` paths therefore encounter the hidden-directory
rule under the normal `.amux` root. A lead must decide how to scope legitimate
hidden-path editing and Draw storage; this change introduces no exception.

## Regression Verification

The `write_refusal_` unit-test filter covers each newly protected endpoint,
including an ordinary sibling that still succeeds. The existing
`api::files::tests` cover the already-protected rooted upload, including
resolved hidden directories and write-time executable/symlink refusals.
Run server tests through the isolated private-HOME command from the task
handoff, always including `-- --skip process_identity`.
