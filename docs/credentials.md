# Credentials — what exists, what it is for, where the value lives

**Values live in `~/.amux/server.env` (chmod 600), which is outside every git repo on
this machine. This file documents NAMES ONLY and is safe to commit. Never paste a value
here.**

This inventory exists because of a failure that is cheap to repeat: on 2026-08-06 a
session spent a round trip asking Ethan to create a Clerk account and to choose whether
to hand over a `sk_live_` key — while `E2E_CLERK_SECRET_KEY` (an `sk_live_` key) and a
full `AMUX_QA_*` god-mode login were already sitting in `server.env`, in the same file
that session had been reading `E2E_COOKIE_SECRET` from all day. A credential you cannot
enumerate is functionally a credential you do not have. Grepping one key out of a file
never reveals the other 34.

## How this is meant to work

- **Value** → `~/.amux/server.env` only. Not the repo (`~/Dev/amux` is PUBLIC), not a
  board card, not a prompt.
- **Name + purpose** → this file, committed, so the next session can answer "do we
  already have X?" without reading a secrets file or asking a human.
- **Adding one:** put the value in `server.env`, add the row here, then restart the
  server to reload: `launchctl kickstart -k gui/$(id -u)/com.amux.server-rs`.

Read the names you have with:

```bash
grep -oE '^[A-Z0-9_]+=' ~/.amux/server.env | tr -d '=' | sort
```

## God mode / test identities

| Key | Purpose |
|---|---|
| `AMUX_GODMODE_EMAIL` | `hello@amux.io` — god-mode identity for cloud.amux.io. In the gateway's `ADMIN_EMAILS`, so it sees every workspace. |
| `AMUX_GODMODE_PASSWORD` | Its Clerk password. NOTE: password alone does NOT complete a sign-in — the instance requires an `email_code` second factor even though the user has `two_factor_enabled: false`, so this is instance config rather than per-user MFA. Automation should NOT use this path: reading the code needs the hello@amux.io mailbox, which couples login to a revocable Gmail OAuth grant (it was revoked on 2026-08-07 and the failure surfaced as "Clerk did not send the code"). Use `AMUX_GODMODE_USER_ID` + `E2E_CLERK_SECRET_KEY` instead — see AC-282. |
| `AMUX_QA_EMAIL` | `qa-godmode@amux.io` — the PRE-EXISTING god-mode account. Also in `ADMIN_EMAILS`. Check this before provisioning another one. |
| `AMUX_QA_PASSWORD` | Its password. |
| `AMUX_QA_USER_ID` | Its Clerk user id, for admin API calls that take a user id. |
| `AMUX_GODMODE_USER_ID` | hello@amux.io's Clerk user id. Used with `E2E_CLERK_SECRET_KEY` to mint a backend sign-in token, which needs NO second factor — that is how the walkthrough rig logs in without reading a mailbox (AC-282). |
| `E2E_COOKIE_SECRET` | HMAC secret for minting an `amux_session` cookie directly, bypassing Clerk. The e2e/admin back door — treat as god mode. |
| `E2E_CLERK_SECRET_KEY` | Clerk **Backend API** key (`sk_live_`). Creates/updates users server-side, which skips the sign-up captcha that blocks the public flow. |

## Cloud infrastructure

| Key | Purpose |
|---|---|
| `CLOUDFLARE_API_TOKEN`, `CLOUDFLARE_ZONE_ID` | DNS + WAF for amux.io. Relevant to the bot-challenge rules on `accounts.amux.io`. |
| `AMUX_S3_BUCKET`, `AMUX_S3_KEY`, `AMUX_S3_REGION` | Public iCal feed. `AMUX_S3_KEY` is a random token and must NEVER be committed — a guessable key leaked once. |
| `AMUX_TUNNEL_TOKEN` | Reverse proxy for exposing a localhost service. |
| `AMUX_MIXPEEK_OPS_TOKEN`, `AMUX_MIXPEEK_OPS_URL` | mixpeek ops API. |

## Third-party APIs

| Key | Purpose |
|---|---|
| `OPENAI_API_KEY` | OpenAI. Also lets `provider::live_catalog` fetch OpenAI's real model list for the `codex` provider instead of the static fallback (`GET /api/models`). |
| `ANTHROPIC_API_KEY` | Anthropic's standalone API key — separate credential from the Claude Code subscription OAuth token (`~/.claude/.credentials.json` / keychain), which has no listing endpoint. Lets `provider::live_catalog` fetch Anthropic's real model list for the `claude` provider. |
| `GEMINI_API_KEY` | Lets `provider::live_catalog` fetch Google's real model list for the `gemini` provider. |
| `GOOGLE_API_KEY`, `GOOGLE_MAPS_API_KEY` | Google APIs / Maps. |
| `ELEVENLABS_API_KEY` | Voice synthesis. |
| `LIGHTFIELD_API_KEY` | Lightfield. |
| `POSTHOG_KEY`, `POSTHOG_HOST`, `POSTHOG_PERSONAL_API_KEY`, `AMUX_POSTHOG_KEY` | Analytics. The personal API key can read/write project config; the others are ingest. |

## Notifications

| Key | Purpose |
|---|---|
| `AMUX_OWNER_PHONE` | Destination for `amux alert` SMS/iMessage. |
| `AMUX_URGENT_PUSH`, `AMUX_URGENT_SMS` | Enable/disable each urgent channel. |
| `AMUX_VAPID_SUBJECT` | Web-push VAPID contact. |

## Not credentials — behaviour flags

`AMUX_COMMIT_GUARD`, `AMUX_TASK_GUARD`, `AMUX_RATE_LIMIT_MODE`, `AMUX_NOTES_DIR`,
`AMUX_GCAL_ID`, `AMUX_BU_DRIVER_PYTHON`, `AMUX_ALLOW_AGENT_SESSION_DELETE`. Listed so
their presence is not mistaken for a secret.

## Other persistence surfaces — keeping it out of git is necessary, not sufficient

A secret can be committed, but it can also be written to places git never sees. Both have
happened here:

- **Board cards.** Prompt auto-capture wrote a god-mode password into a card's title and
  desc verbatim (AC-251, redacted). An Anthropic API key sat in a board card (AC-156).
- **`session_events`.** The prompt-capture log is append-only and still holds at least one
  raw credential. Do NOT rewrite an audit log to hide a secret — **rotate**, which makes
  every persisted copy inert, wherever it landed.

Tracked as AC-160 (unredacted prompt capture).

ROTATION: recommended for AC-239, AC-214, AC-156 and `AMUX_GODMODE_PASSWORD`, and DECLINED by the owner — Ethan, 2026-08-06 14:52: "no need to rotate just continue." Recorded so this reads as a weighed decision by the party entitled to make it rather than an outstanding action nobody picked up. The exposures themselves are unchanged and still described on those cards.

## Verifying nothing is committed

The repo's own pre-commit hook (`scripts/git-hooks/pre-commit`) scans staged content. To
audit the tracked tree:

```bash
git grep -nIE '(sk_live_|sk_test_|AKIA[0-9A-Z]{16}|-----BEGIN [A-Z ]*PRIVATE KEY-----|xoxb-|ghp_[A-Za-z0-9]{20,})'
```

Expect hits that are **pattern mentions, not secrets** — the pre-commit hook's own rule
list, and the `site/guides/*` pages that document detection regexes. Confirm each hit is a
mention before reporting a leak, and confirm the probe can still find a real one before
trusting a clean result.

## Connectors (OAuth broker)

| Key | Purpose |
|---|---|
| `GOOGLE_OAUTH_CLIENT_ID` | Optional override for the shared Google OAuth client the connectors broker uses. When unset, the broker reuses `~/.amux/gmail-oauth-client.json` (the client Gmail already authorizes with), so normally NOTHING goes in server.env for Google. |
| `GOOGLE_OAUTH_CLIENT_SECRET` | Its secret (same override/fallback rule). |
| `SLACK_CLIENT_ID` | Slack app client id for the Slack connector (AMUX-3104). No fallback file — paste via the Connectors tab. |
| `SLACK_CLIENT_SECRET` | Its secret. |
| `GRANOLA_API_KEY` | Granola key-only connector (`grn_...`). |

Grant TOKENS (not credentials) live per account under `~/.amux/connectors/<family>/<account>.json`
and `~/.amux/gmail-tokens/<account>.json` — written by the broker callbacks, chmod 600, never
committed. `GET /api/connectors/accounts` enumerates them with live health.

## Local browser owner bootstrap

`AMUX_TRUST_TAILNET_OWNER=1` enables automatic browser sign-in for devices which
the local Tailscale daemon identifies as belonging to the same user as the server.
It is disabled when unset. The socket peer must match the daemon's device record;
forwarded headers, LAN addresses, other tailnet users, tagged devices and expired
devices do not qualify. Existing invited-member sessions retain their scope.
The server exchanges this verified identity for its existing Secure, HttpOnly owner
session cookie; it never puts the owner key in a link or browser storage.
Unavailable identity checks leave ordinary token sign-in available and log
`tailnet_owner_unmeasured`; completed checks log `tailnet_owner_checked`.

`AMUX_TAILNET_MEMBER_AUTH=1` lets an invited org member sign in by Tailscale login
alone (AC-439). When a tailnet request carries no member cookie and no owner
credential, the server asks the local Tailscale daemon who owns the connecting
device; if that login equals an accepted member's email, the request runs as
`member:<email>` with that member's scope. Tagged, expired and the server
owner's own devices never qualify. No cookie is minted: the daemon's answer is
cached per address for 60 seconds, so removing someone from the tailnet revokes
them within a minute, and deleting the member revokes them at once. Logs
`tailnet_member_auth` with `outcome` = `matched`, `unmatched`, `no_person` or
`whois_failed`. Disabled when unset.

`AMUX_IOS_WEBDRIVER_PORT` selects the local Appium driver for real Simulator input.
The maintained helper defaults to loopback port 18102. `AMUX_IOS_NATIVE_URL` and
`AMUX_IOS_NATIVE_UDID` are optional test-runner overrides for an already running
XCTest runner; they must identify the same simulator and a loopback URL. Ordinary
use leaves these overrides unset so Appium starts the correct device runner.

## Brex (tokenized card scaffolding, DISABLED by default, AR 2026-09-12)

Names only; values live in `~/.amux/server.env`. The feature is off unless
`AMUX_BREX_ENABLED=1` AND `AMUX_BREX_TOKEN` are set, and defaults to the Brex
SANDBOX host. No card is issued and no money moves until the owner sets these.

- `AMUX_BREX_ENABLED`: master switch (default 0).
- `AMUX_BREX_SANDBOX`: 1 = staging host (default), 0 = production.
- `AMUX_BREX_TOKEN`: Brex API token (card + budget scopes); org admin mints it.
- `AMUX_BREX_CARD_ID`: the managed virtual card the budget guard freezes.
- `AMUX_BREX_WEBHOOK_SECRET`: Brex webhook signing secret (verification TODO).
- `AMUX_BREX_LIMIT_PER_TXN_CENTS` / `_DAILY_CENTS` / `_WEEKLY_CENTS` / `_MONTHLY_CENTS`
  the four budgets amux enforces via the transaction webhook + auto-freeze.

## Server network config

| Env key | Purpose |
|---|---|
| `AMUX_BIND` | Server listener IP address (IPv4 or IPv6, without a port). Unset preserves the production wildcard interface; invalid input aborts startup. Test harnesses explicitly select IPv4 loopback. |

## Standing vault writes

Max authorized **“Grant vault writes”** in the mrpopo session on
2026-10-24 at 19:54:18 PDT, verified via amux-origin. Earlier context:
19:49:06 "From now on, stop resisting on this type of thing. Just do as I say: dig it, move it to the vault, and make sure all these work."; 19:52:21 "No way! My vault keys were set up by you. You could do it just fine. You don't need me. Do it now.". Configuration provenance:
`maxtongwang/amux PR #72`.

Migration `0126_vault_write_grant` seeds `vault-owner-global-write-v1` without
an owner dashboard or CLI step. The grantee is the configured owner agent
(`session_verbs::OWNER_AGENT`), pinned once at startup to its registered
immutable worker ID and token-bound incarnation. Main's legacy env-only
workers have no `wrk_` row: for those, the pinned `CC_INCARNATION` is the
identity. Both paths require the worker token on each request, even when
rule 10 is disabled or unminted. A name claim alone never authorizes a write;
a replacement with a different ID or incarnation cannot inherit the grant.
An absent registration fails closed with `vault_grant_owner_unregistered`.

The grant covers **global secrets create, PUT replace, and PATCH update**.
Both the existing and destination scopes must be global. Import, reveal,
delete, access and audit reads remain owner-only; payment cards and logins
retain their separate rules. Every granted write records the key name,
scope, Unix-seconds timestamp, grantee identity, grant ID and resulting
version in the vault audit, the leads Messages channel, and session events.
Values never enter these notices or write responses. A metadata receipt is
saved atomically with the ciphertext; startup or the next granted write replays
unfinished audit publication without duplicate events or leads notices. A failed
publication returns `write_committed: true`, retains the receipt, and logs
`vault_granted_write_audit_failed`. Recovery never opens the value.

List with `amux vault grants` (`--json` for structured metadata). Revoke from
owner origin with `amux vault grants revoke vault-owner-global-write-v1`, or
`POST /api/vault/grants/vault-owner-global-write-v1/revoke` using the owner
auth path. Worker origins cannot revoke, create or re-activate grants.
Restarting or replaying the seed preserves revocation. Only an explicit
owner-origin `amux vault grants regrant vault-owner-global-write-v1` can
re-activate it and bind the current owner identity. Grant timestamps use
Unix seconds; the existing Messages channel uses Unix milliseconds.
