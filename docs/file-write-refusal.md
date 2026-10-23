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
may be hidden (`AMUX_FILES_ROOT` overrides the default `$HOME`), but hidden
components underneath it are refused except for the Draw rule below. The general
filesystem APIs accept absolute paths and check all components. No project-root
or dashboard-only exemption exists; repo `.gitignore` edits remain refused.

## Draw Storage Exception

Only amux's own Draw root, `$AMUX_HOME/draw`, waives the hidden-component rule.
The server uses `config::amux_home()` (`AMUX_HOME`, legacy `CC_HOME`, else
`$HOME/.amux`) and resolves the root with `resolve_nonstrict`. Both the requested
filesystem path and its fully resolved target must be inside that canonical
root, using component containment rather than string prefixes. Existing path
prefixes are resolved even when the destination has not been created yet.

The exemption does not waive sensitive home/system path, dangerous-write, or
existing executable-file checks. A symlink inside Draw pointing outside the root
is refused. The data directory itself, siblings such as `.amux/notdraw`, prefix
tricks such as `.amux/drawx` or `.amuxdraw`, and all other hidden paths remain
refused.

The dashboard's `_drawDataDir()` uses the injected `_AMUX_DATA_DIR` (the server's
amux home), falling back to `_AMUX_HOME + '/.amux'` (the bootstrap's user home).
Its `draw/library.excalidrawlib`, `draw/recents.json`, and default
`draw/drawings` saves use `PUT /api/file` and qualify for this exception.

## Regression Verification

The `write_refusal_` unit-test filter covers each newly protected endpoint,
including an ordinary sibling that still succeeds. The existing
`api::files::tests` cover the already-protected rooted upload, including
resolved hidden directories and write-time executable/symlink refusals.
Run server tests through the isolated private-HOME command from the task
handoff, always including `-- --skip process_identity`.

The `write_refusal_draw_` viewer tests cover Draw saves, symlink escapes, repo
`.gitignore`, and sibling/prefix refusals. Containment mutation proofs also use
an ordinary filename outside Draw so the independent `authorized_keys` refusal
cannot hide a broken containment check.
