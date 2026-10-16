# Computer use (CUA sandboxes)

Rung 3 of the access ladder (AMUX-5300). Try these in order:

1. **amux browser**: `/api/browser/*` on an amux Chrome profile.
2. **CDP into the owner's real Chrome**: the `chrome-cdp` skill.
3. **amux computer**: a disposable Linux desktop driven by screenshots, clicks and keystrokes. Use it only when rungs 1 and 2 cannot reach the thing.

`amux help computer` prints the verbs plus the live context: limits, and which saved profile holds cookies for which sites (a cookie is not proof of a login; the screenshot is).

## Verbs

| CLI | API (`X-Amux-Session` required) |
|---|---|
| `amux computer start` | `POST /api/computer/start` |
| `amux computer status` | `GET /api/computer/status` (fleet-wide) |
| `amux computer context` | `GET /api/computer/context` |
| `amux computer screenshot` | `POST /api/computer/screenshot` returns `{path}`; Read it |
| `amux computer click X Y` | `POST /api/computer/click {x,y}` |
| `amux computer double-click X Y` | `POST /api/computer/double_click {x,y}` |
| `amux computer move X Y` | `POST /api/computer/move {x,y}` |
| `amux computer type TEXT` | `POST /api/computer/type {text}` |
| `amux computer key KEY` | `POST /api/computer/key {key}` (`enter`, `ctrl+l`) |
| `amux computer scroll DX DY` | `POST /api/computer/scroll {dx,dy}` (dy > 0 scrolls down) |
| `amux computer open URL [--profile NAME]` | `POST /api/computer/open {url, profile?}` |
| `amux computer stop` | `POST /api/computer/stop` |

The working loop: screenshot, read coordinates off the image, act, screenshot again to confirm.

## How it works

- **Image.** `trycua/cua-ubuntu` (CUA's Kasm Ubuntu desktop, published for arm64) with a small local layer on top: Playwright's Chromium (Google Chrome has no linux/arm64 build) and a startup script that skips upstream's per-boot `pip install --upgrade`. The layer is built on the first `start` and tagged `amux-computer:<hash of the recipe>`, so a recipe change rebuilds automatically. Recipe: `crates/amux-server/src/integrations/computer/Dockerfile`.
- **Driver.** The container runs CUA's `computer-server`. amux posts `{"command","params"}` to its `/cmd` HTTP endpoint from Rust. No Python on the host: the `cua-computer` SDK is a client of that same endpoint.
- **Ports.** computer-server, noVNC and a DevTools relay are published on `127.0.0.1` only, on Docker-chosen ports. `status` lists each sandbox's `vnc_url` if a human wants to watch.
- **State.** Docker is the registry: every container carries `amux-computer=<lane>`. The idle clock is a file per container under `~/.amux/computer/activity/`, so a server restart loses nothing.

## Signed-in Chromium (`open --profile`)

amux profiles are macOS Google Chrome user-data-dirs. Chrome encrypts cookie values with a key from the macOS keychain, which Linux Chromium cannot decrypt, so copying or mounting a profile would arrive signed out. Instead, `open --profile NAME` reads the decrypted cookies through the amux browser's CDP (`Storage.getCookies`), using the browser already running on that profile or a headless one started for the read and stopped after it. It then writes them into the sandbox Chromium with `Storage.setCookies`. The source profile is only read.

Cookies carry over. localStorage and IndexedDB do not, so a site that keeps its session there will still ask for a login.

## Limits and lifecycle

| Setting | Default | Scope |
|---|---|---|
| `AMUX_COMPUTER_MAX` | 2 running sandboxes fleet-wide | server.env |
| `AMUX_COMPUTER_MEMORY` | `4g` (swap capped at the same) | server.env |
| `AMUX_COMPUTER_CPUS` | `2` | server.env |
| `AMUX_COMPUTER_IDLE_S` | `900`; `0` disables idle stop | server.env |
| `AMUX_COMPUTER_BASE_IMAGE` | `trycua/cua-ubuntu:latest` | server.env |
| `AMUX_COMPUTER` | on; `0` refuses `start` | worker > group > global |
| `AMUX_COMPUTER_COLIMA_AUTOSTART` | off; `1` lets `start` run `colima start` | worker > group > global |
| `AMUX_COMPUTER_COLIMA_PROFILE` | derived from the docker context (`colima-<profile>`) | server.env |

The `computer-sandbox-reaper` job runs every 60s (`AMUX_COMPUTER_REAP_TICK_S`) and removes, by label only: idle sandboxes, exited ones, a second sandbox for one lane (keeps the newest), and any whose label names no lane. It never starts Docker. Screenshots keep the newest 30 per lane; the response reports how many were pruned.

Every start, reuse, refusal, stop and reap logs one `[computer] verdict=...` line in `~/.amux/logs/server-rs.log`. The mac cleanup tick prints a `computer sandboxes:` line beside the lima disk report.
