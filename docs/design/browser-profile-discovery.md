# Browser profile discovery diagnostics

Source audit and implementation handoff, 2026-10-17. No real browser data was
inspected. The reported worker PermissionError is supplied evidence; whether the
server has the same macOS grants remains unmeasured.

Source entry points (current working-tree lines):
`integrations/browser.rs:45` selects the root; `:483` reads the saved registry;
`:511` inventories saved profiles. `api/browser.rs:2600` serves profile discovery;
`api/browser_routing.rs:164` serves routing config. `amux:5946` formats diagnostic
status. New Chrome reader: `integrations/browser_profile_discovery.rs:73`.
All Rust paths above are under `crates/amux-server/src/`.

## Discovery sources and failures

| Surface | Source and former silent failures | Empty meaning |
|---|---|---|
| `GET /api/browser/routing/config` | `api/browser_routing.rs::config` formerly used read `.ok()`, parse `.ok()`, `Value::Null`, then cache `unwrap_or_default`; hardcoded `measured:true` | Missing root/file, EPERM/EACCES, malformed JSON, absent/wrong `profile.info_cache`, or genuinely empty cache all returned `chrome_profiles:[]` |
| `GET /api/browser/profiles` Chrome names | `integrations/browser.rs::list_chrome_profiles` formerly returned `vec![]` on read, parse, or cache-shape failures | Same causes as routing/config; not affected by saved registry or worker scopes |
| Same API saved `profiles` | `registry_load` formerly swallowed read/parse/object-shape errors with `.ok()`/`unwrap_or_default`; `list_profiles` ignored `read_dir` errors with `if let Ok`, entry errors with `flatten`, and metadata errors through `is_dir` | No registry entries, no named Amux directories and no default directory is legitimate. Inaccessible/corrupt registry or unreadable directories previously looked the same |
| CLI `browser profiles [--all]` | Counts API **saved** `profiles`, not `chrome_profiles` | Printed `0 profile(s) discovered` whenever saved rows were empty, regardless of Chrome read failure. QA/deprecated rows are hidden unless `--all`, but remain in the count. Denied scope rows remain visible with `x` |
| Routing request Chrome identity | `api/browser_routing.rs::request` had a second read/parse `.ok()`/null chain and identity default | Could silently erase identity after a failed read; now uses the shared reader and adds `chrome_discovery` to driver context |

The root comes from `AMUX_BROWSER_CHROME_USER_DATA_DIR`, otherwise macOS
`$HOME/Library/Application Support/Google/Chrome`, Windows
`$LOCALAPPDATA/Google/Chrome/User Data` (HOME fallback), or Linux
`$HOME/.config/google-chrome`. Discovery reads `Local State`, parses JSON
`profile.info_cache`, and uses its keys as directory names. Routing labels come
from each entry's `name`, identity from `user_name`; `on_disk` checks the selected
profile directory. No worker/team filter applies to either discovery list.

The CLI never imported Chrome profiles into saved Amux inventory. A readable
Chrome cache alone does not create saved rows.

Related source paths audited but outside these two listing fixes:

| Path | Silent handling and consequence |
|---|---|
| `api/browser_import.rs::browser_root`, `discover_chromium_profiles`, `discover` | `is_dir` masks inaccessible roots; Local State read/parse `.ok()?`, optional cache/name fields and `unwrap_or_default` erase display names; failed `read_dir` returns empty; `flatten` drops entries; cookie `is_file` checks omit inaccessible/missing-cookie profiles; endpoint omits empty sources. This separate multi-browser import endpoint still needs its own diagnostics |
| `integrations/browser_login_sync.rs::chrome_profile_for_identity` | read/parse `.ok()?`, optional cache and `hits.first()` turn unreadable state, malformed state and no matching identity into `None`; this separate login-sync selector is unchanged |
| `integrations/browser.rs::dir_mtime_unix`, `dir_size_bytes` | metadata `.ok()?`, `let Ok`/continue and `flatten` lose timestamps/size entries; affect row enrichment, not row count |
| `api/browser.rs::profile_contents`, `profile_jar` | optional cookie DB, copy/query errors, `filter_map(Result::ok)` and cleanup `let _` can yield unknown/empty cookie evidence; do not remove profile rows. Serialization `unwrap_or(Value::Null)` and enrichment defaults also do not filter rows |
| `api/browser_routing.rs::load` | config read/parse `.ok()`/`unwrap_or_default` defaults missing or unreadable Amux route config; does not determine Chrome discovery count |

## Result and log contract

One shared reader now returns `profiles` and `discovery`. Both listing APIs expose
`discovery`; `/profiles` also exposes `saved_discovery` for its independent saved
inventory. Routing/config's existing top-level `measured` and `n_considered` now
reflect Chrome discovery rather than a hardcoded success.

Status schema:

```json
{
  "measured": false,
  "n_considered": 0,
  "why_unmeasured": "permission_denied: /synthetic/Local State",
  "empty_reason": null,
  "owner_step": "Grant the amux server process Full Disk Access ..."
}
```

Read errors distinguish `permission_denied` (including EPERM/EACCES), `not_found`,
and other `io_failure`; JSON errors use `parse_failure`; absent/invalid cache
schema uses `invalid_profile_cache`. Successful empty caches use
`measured:true`, `empty_reason:no_chrome_profiles_configured`; a missing/empty
saved inventory uses `no_saved_profiles_configured`. Missing saved registry or
profile directories is normal for an unconfigured Amux inventory. Other saved
read/parse failures mark the saved measurement incomplete while retaining any
rows found from the other source.

Each failed shared read emits one WARN with
`verdict="browser_profiles_unreadable"`, `measured=false`, count, reason and path;
no Local State contents or account identities are logged. This is per probe,
not a permanent once-only latch, so later failures remain observable.

CLI incomplete discovery names the failing source, reason and owner step,
reports returned saved rows, and exits 1; it never prints a successful zero
when Chrome cannot be read. For a macOS permission refusal the proposed owner
step is granting **the amux server process** Full Disk Access, then retrying.
This task made no permission changes or grant attempts. Discovery reports the
OS refusal; it does not claim to distinguish TCC from ordinary file permissions.

## Identity and scope configuration

| Concern | API / storage | Schema and resolution |
|---|---|---|
| Saved identity/purpose | `POST /api/browser/profile/meta`; `$AMUX_HOME/playwright-auth/profiles.json` (normally `~/.amux`) | Body `{name, identity?, role?, label?, domains?: string[], sync?}` (`profile` aliases `name`). Registry is an object keyed by profile name; entry has those metadata fields, `updated`, `updated_by`, optional owner/created_by/saved_by |
| Roles | `integrations/browser_logins.rs::ROLES` | `primary`, `personal`, `restricted`, `customer`, empty string, `test`, `deprecated`; labels/roles describe purpose and never grant access |
| Optional sync | Same metadata API | `{from:"chrome", chrome_identity?:string, chrome_profile?:string, sites: string[]}`; at least one selector and nonempty sites required; null removes sync |
| Chrome account identity | `Local State` `profile.info_cache.<directory>.user_name` | Browser-owned metadata; routing/config reports it; Amux's declared identity is a separate registry field |
| Scope writes | `PUT /api/scope` | `{level:"global"|"group"|"worker", name:string, capability:"env", value:{AMUX_BROWSER_PROFILES_ALLOW:string, AMUX_BROWSER_PROFILES_DENY:string}}` |
| Scope storage | Global `~/.amux/amux.env`; group `~/.amux/env/<lowercase-group>.env`; worker `~/.amux/sessions/<worker>.env` | ALLOW/DENY values are comma/whitespace lists, quotes stripped, case-insensitive `*`/`?` globs. Groups come from worker `CC_TAGS` (comma-separated) |
| Scope inspection | `GET /api/browser/profile-access?level=worker|group|global&name=...` | Reports rules and effective per-profile verdicts; worker identity defaults from `X-Amux-Session` |
| Precedence | `api/browser_scope.rs::Policy` | Worker, all worker groups as one tier, global; matching deny wins at same tier; a defined allow list decides there and replaces broader lists; nonmatching deny alone defers. Unconfigured means allow; no worker identity means owner and allows all |
| Route choices | GET/POST `/api/browser/routing/config`; `~/.amux/browser-routing/config.json` | Owner-only save: `{native_profile:string, chrome_profile:string, cua_profile:string, allow_cua:bool}`. Server retains `profile_routes` keyed by saved native profile; clients cannot submit nonempty `profile_routes` |

Scope/identity filters can remove all candidates in the separate
`GET /api/browser/profile-for` selection endpoint (`api/browser.rs:2459`):
on-disk presence, optional identity/role constraints, default QA/deprecated
exclusion, worker access and site suitability. That endpoint is not the source
of either reported empty discovery list.

No scope or identity configuration was changed.

## Verification evidence and limits

| Command | Result |
|---|---|
| `amux-isolated python3 scripts/test-browser-profile-discovery-cli.py` | `Ran 4 tests ... OK`; synthetic API replies only, including both normal and `--all`, denied scope rows, separate inventory counts, missing/parse/permission failures |
| Same command with `--source <temporary HEAD:amux>` | Baseline regression control: exit 1, `FAILED (failures=10)` |
| `bash -n amux` | Exit 0 |
| `rustfmt --edition 2021 --check crates/amux-server/src/integrations/browser_profile_discovery.rs` | Exit 0 |
| `cargo check -p amux-server --locked --offline --target-dir /private/tmp/amux-profile-discovery-check` | Bounded to 45 seconds; dependency compilation started, then timeout. No completed Rust build verdict |
| `git diff --check` | Exit 0 |

New Rust unit tests are in `integrations/browser_profile_discovery.rs`: synthetic
valid Local State with two fake identities and directories; chmod-000 directory
(permissions restored before assertions, unprivileged runner required); missing
directory; malformed JSON; empty cache versus missing schema. Saved-inventory
unit tests cover a missing inventory and a malformed registry with a retained
synthetic directory. They were **not run**, per the prohibition on `cargo test`.
No server, e2e, GUI, browser launch, browser-data inspection, commit, or scope
mutation was performed. GitNexus was not available in PATH or this checkout;
source tracing used `rg` and bounded reads.
