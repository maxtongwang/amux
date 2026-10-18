# E2E Mismatch Operands

Diagnostics for the unresolved state mismatches in the hosted e2e failure
investigation (F01, F03/F12, F11). Each one failed on a wrong title, status
or count, and a status or count has no operand of its own. Nothing here
changes an assertion, a timing or product code. These are hypotheses to test,
not findings.

## Recorder

| Part | Role |
| --- | --- |
| `e2e/operands.ts` | `recordOperands(page, opts)`, plus the `operands` fixture, which flushes in teardown after the status is known and before the page closes |
| `e2e/operands-core.mjs` | Pure half: option validation, path filter, redaction, body summary, size caps, report and console line |
| `e2e/operands-core.test.mjs` | `node --test` contract for the pure half, run in spa-gates |

On every run each recorder prints one line, e.g.
`e2e_operands {"verdict":"e2e_operands","label":"peek-filter","measured":true,"n_considered":4,"n_requests_seen":31,"attached":false,...}`.
Only a test whose status differs from its expected status attaches
`e2e-operands-<label>.json`. That file holds the matched requests (method,
path and query, status or failure, start, end and duration on the recorder's
clock, and the request and response body size and sha256, plus redacted JSON
for `jsonPaths`), the step marks and the snapshots. Each snapshot holds the
globals, the DOM items and the probe, taken at `capture()` and at teardown.

Never recorded: request headers, cookies and storage state. Credential-named
keys and query parameters, and bearer- or basic-shaped values, are replaced
with `[redacted]`. Strings keep 240 characters plus their length and hash.
Requests keep the newest 300 and report how many were dropped. A capture has a
5s budget, so a wedged page yields `why_unmeasured` instead of a teardown
timeout.

## Hypotheses per case

| Case | Spec | Hypothesis | Confirming operand | Refuting operand |
| --- | --- | --- | --- | --- |
| F03, F12 | `lifecycle/sync-progress.spec.ts`: pill reads `Sync error`, expected `/Syncing 0\/3/` (ios-safari only) | `updateConnectionStatus` puts `readState` (`_sessionLoadError` while `navigator.onLine !== false`, `_boardReadError`, `_syncReadError`) and `_localWriteError` ahead of `_syncPillText`. A read that failed in the offline window counted as a server error, not as offline, because WebKit kept `navigator.onLine` true under `context.setOffline` (the spec dispatches `offline` by hand). The read that would clear it (board read or delta sync, which runs after queue replay) sits behind the PATCH the test holds, so `Sync error` outranks `Syncing 0/3` for as long as that PATCH is held | `finally` snapshot: `_boardReadError` or `_syncReadError` (or `_sessionLoadError`) non-empty, `_syncPillText` = `Syncing 0/3`, `navigator.onLine` value. Network: a failed `/api/board` or `/api/sync` GET between marks `offline` and `online`, and no later successful one | All read errors empty with `_localWriteError` set (the write-error path); or `_syncPillText` empty (the checklist never built) |
| F01 | `mdai-pages.spec.ts` Edit section: saved file keeps title `Where I left off`; model, timeout and refresh did change | A settings re-render from `_mdaiCur.doc` landed after the title fill and before the first Save, for example a late open-time graph, history or active load. No save had stored `_mdaiPendingDoc` yet, so the title input reverted. Model, timeout and refresh were set after that render, so they survived. The refused first save then put the reverted title into `_mdaiPendingDoc`, and the second save wrote it | First `PUT /api/files/mdai/doc` request JSON has `doc.title` = `Where I left off`, and an `/api/files/mdai/*` response finished between mark `settings title filled` and that PUT | First PUT carries `Resume here` and the second does not (the pending doc was cleared between saves, e.g. `_mdaiSetTab` or an open reset); or both PUTs carry `Resume here` and the response `doc.title` is old (a server write defect) |
| F11 | `terminal-message-navigation.spec.ts` filter test: `#peek-msg-count` `0`, expected `1`, while the summary reads `Human · Board references` | (a) An SSE `messages` invalidation from another spec on the shared server ran `_peekMessagesLoad()`. That replaced the injected `_peekMsgRows` with the server's rows for `nav-probe` (none), so `Review AMUX-4242` reclassified to `unknown`. (b) A `fetchBoard` reassigned `boardItems` from the server without `AMUX-4242`, so `_peekHasBoardRef` needed a prefix the board lacks | (a) A `/api/history` GET in the log; `_peekMsgRows` without the specimen; probe prompt `kind: unknown` with `board_ref: true`. (b) Probe `board_has_card: false` with `board_prefixes` lacking `AMUX`; prompt `kind: human`, `board_ref: false`; a `/api/board` GET finishing after the test started | Probe prompt `kind: human`, `board_ref: true`, `matches_filters: true`, yet the count is `0`: the count was not recomputed after the filter change, a product ordering defect |

The `client-debug` beacons are kept as JSON in F01 and F11. In F11,
`peek-message-filter` reports `matched_messages` and `n_considered` at each
filter change. In F01, `mdai-render-deferred` shows a graph render that
waited for an edit in progress.

## Not yet run

The browser half has not run: not its listeners, page readers, fixture flush
or attachment. Local Playwright and server runs are not allowed here (bind
rule). The first hosted run of these three specs is its first execution, and
each test's `e2e_operands` line with `measured: true` and a non-zero
`n_requests_seen` is its positive control. No TypeScript type check is
configured in this repo; the `.ts` files were syntax-checked after type
stripping.
