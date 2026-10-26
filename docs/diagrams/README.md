# Amux Diagrams

| Kind | View |
| --- | --- |
| Component/topology | [Component/topology](topology.html) |
| Main runtime sequence | [Main runtime sequence](runtime-sequence.html) |
| Data flow | [Data flow](data-flow.html) |

Open each HTML file in a browser. These static, accessible figures use Diagram Design's default light palette and inline SVG. Google Fonts supplies Instrument Serif, Geist, and Geist Mono; serif, sans-serif, and monospace fallbacks work offline. Below 720 px each page scales the figure to the screen (pinch to zoom) and shows a phone view under it: the same boxes and arrows as a stacked list, generated from the figure by `python3 scripts/diagram-checks.py phone --write`. The set covers the Rust server (`crates/amux-server`) and the callers and workers around it, with one burnt-orange authority boundary: the HTTP API that stamps the true sender. The runtime view follows one message through `POST /api/workers/{id}/send`, the ledger, and the worker's Stop-hook report back to the dashboard over SSE. Connectors, the Projects and board features, and the cloud and mobile apps are not drawn.

[README.md](../../README.md#architecture) owns the crate overview and the eight primitives. [CLAUDE.md](../../CLAUDE.md) owns the source layout and build and test procedures. The messaging authority rules live in `crates/amux-server/src/api/worker_messaging.rs`, and the single-writer store contract lives in `crates/amux-server/src/db/mod.rs`.

## Check Contract

`scripts/check-diagrams.py` is the guard. It is vendored byte-identical from maxtongwang/ai-config@777a3b2b65e491f96bbecfe55f624fdbb240bd9f (sha256 `151491d8f6037dd1c2a298371997b8d2effa4e1eed9411dc4230f837472af1a2`) because ai-config is private and hosted CI cannot fetch it. To update, copy the new ai-config version and record its SHA and sha256 here.

Run `python3 scripts/check-diagrams.py --repo <checkout> --base <ref> [--head <commit>]` from the repo root. `DIAGRAMS_BASE` supplies the default base, followed by `origin/HEAD`; the candidate defaults to `HEAD`. JSON reports `status`, resolved `head` and `merge_base`, matching `architecture_paths`, `diagrams_updated`, and `errors`. Exit codes: 0 pass or `not-opted-in`, 1 policy failure, 2 invalid input/history (JSON `status: error`; argparse usage errors go to stderr).

The inventory uses the exact three Markdown link labels in the table above, with distinct local `.html` targets relative to this directory. Paths cannot escape this directory; symlinks do not count. Root `README.md` must have an inline Markdown link to `docs/diagrams/README.md`. Examples in fenced code blocks and HTML comments do not count.

[ARCHITECTURE_PATHS](ARCHITECTURE_PATHS) contains one case-sensitive Python `fnmatch` glob per line; blank lines and `#` comments are ignored. Patterns match repository-relative paths, with `*` also matching `/`. An inventory with no patterns is an error. The check uses patterns from both ends of the range and diffs from the merge base, treating renames as deletion plus addition. Matching changes require any committed change under `docs/diagrams/`; reviewers still verify that the diagrams accurately describe the change. Uncommitted edits do not count.

Repos with no diagram directory at either end pass as `not-opted-in`. Once introduced, the directory must contain this index, its three HTML targets, and `ARCHITECTURE_PATHS`; deleting the set within a range fails. The `checks` job in `.github/workflows/checks.yml` runs the guard on every pull request.

## Inventory and Phone View

[ARCHITECTURE_PATHS](ARCHITECTURE_PATHS) was derived from the entrypoints the diagrams show, following each one to the scripts and modules it calls (one level of imports). Every other tracked file is listed there as `# exclude: <glob> — <reason>`, so a new file has to be classified before CI passes.

`python3 scripts/diagram-checks.py` runs in CI after the guard. It fails (exit 1) when a tracked file is neither an architecture path nor excluded, when a glob or exclusion matches no tracked file, when a diagram cites a repo path that is not an architecture path, or when a phone view is missing or out of date. It also builds a scratch Git fixture and commits a one-line comment to one file per glob without a diagram change: each must fail the guard, and the same change to an excluded file must pass. Exit 2 means it could not read the inputs. The script is the same file in every repo of ours that keeps a diagram set (sha256 `2bb7c200d56d19e9813dc4797b097640a280c365e6d6928a1589d0fe28538356`); change every copy together.
