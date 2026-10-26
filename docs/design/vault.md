# Vault: secrets and payment methods

The Vault page is where the owner creates, views, edits, rotates, reveals and
deletes the secrets workers receive, and sees exactly which workers hold each
one. It is built on the vault that already existed (AMUX-5375). Nothing here is
a new store, a new crypto scheme or a new primitive.

Open it from **Settings > Connect > Open Vault**, or from a worker's or group's
Scope tab (**Manage in Vault**, which opens the page filtered to that scope).

## What existed, and what this reuses

| Mechanism | Where | Values at rest | Reaches workers | Used here |
| --- | --- | --- | --- | --- |
| Vault secrets | `api/vault_secrets.rs`, `~/.amux/vault/secrets.json` | AES-256-GCM per value; master key in the login Keychain (one account per amux home) or a 0600 file with `AMUX_VAULT_KEYSTORE=file` | Terminal launch: `tmux set-environment`, imported by name (AMUX-4803); delegates: process env | **The store.** Extended, not replaced |
| Vault cards and logins | `api/vault.rs`, `~/.amux/vault/items.json` | 0600 file | Never: amux types the card into the worker's page (`amux order`, `/api/browser/complete-order`) | **Payment Methods tab**, unchanged backend |
| Vault audit | `~/.amux/logs/vault-audit.jsonl` | Names, scopes, ids, never values | | **Audit log view**, one file for both kinds |
| Scope env files | `amux.env`, `env/<group>.env`, `sessions/<worker>.env` | Plaintext | Sourced by the worker shell, global then groups then worker | Same precedence model; the vault wins over a plaintext line for the same key |
| Provider keys | `server.env` via Settings | Plaintext | `deferred_secrets` (set-environment) | Untouched |
| Connector credentials | `server.env` via `/api/connectors/<id>/credentials` | Plaintext | Brokered, never in a worker env | Untouched |
| Worker identity token | `worker_identity.rs` | sha256 only | Rides the vault channel at launch | Untouched |
| sops / central-secrets | not on main | | | Not used: there is nothing to reuse |

Plaintext `server.env`, scope env files and connector credentials are still
plaintext. That gap is unchanged by this work. The existing opt-in import
(`POST /api/vault/secrets/import`) copies one key at a time from those files
into the encrypted vault and leaves the file for the owner to edit.

## Storage and encryption

Unchanged from AMUX-5375. Each value is sealed with AES-256-GCM under a random
nonce. The associated data binds the ciphertext to the item's id, name and
scope, so a ciphertext copied onto another item fails to open. Editing a name
or scope re-seals the same item under the new binding.

What it protects against: a copied or backed-up amux home, a stray `cat`, a grep
over the home directory, a worker reading the store file. What it does not: a
process running as the same OS user that asks the Keychain, which is what the
server itself does. The Vault page says so when the key is a file.

New on disk: `vault/delivered.json` (0600) records, per terminal worker, the
item id and version of each secret its current process was launched with. It
holds no values.

## API

All under `/api/vault/secrets`. Only `reveal` and the separately authenticated job delivery route below carry values.

| Method and path | Who | What |
| --- | --- | --- |
| `GET /` | anyone | Metadata. A worker origin sees only items that apply to it. The owner also gets `effective_for` / `overridden_for` per item, the worker list and the group list |
| `GET /?worker=<w>` | owner | That worker's provisioned secrets: name, source scope, version, and `state` of its running process (below), plus `revoked_held` and `isolated` |
| `POST /` | owner | Add. `key`, `scope`, `target`, `value`, `description` (200 max) |
| `PATCH /{id}` | owner | Edit description, name or scope; an optional `value` rotates too |
| `PUT /{id}` | owner | Rotate: new value, version + 1. Returns `running_stale`, the running workers still holding the old value |
| `DELETE /{id}` | owner | Revoke. Returns `still_held_by` |
| `POST /{id}/reveal` | owner | The value, `Cache-Control: no-store`, audited, `remask_after_s: 20` |
| `GET /{id}/access` | owner | Every worker the item reaches, why, whether it is overridden, and the state of each running process |
| `GET /audit?limit=&key=&id=` | owner | Vault secret events newest first, projected to named fields only. `measured` / `n_considered` |
| `POST /import` | owner | Unchanged opt-in import |

### Authorization

"Owner" means all three:

1. no worker origin (`X-Amux-Session` / `X-Amux-Worker`),
2. not an invited member (`org::is_verified_local_member`),
3. on a host with an auth token, the owner bearer itself
   (`auth::has_owner_token`). The dashboard sends it on every call.

The third rule is new. Before it, a worker shell could become "the owner" by
leaving its session header off a `curl`. Now that request is refused 403 with
`vault_owner_credential_missing`. On a host run with `AMUX_AUTH_TOKEN=none`
there is no credential to present, so rules 1 and 2 apply and every such
request logs `vault_owner_by_absence`.

A worker can never write, reveal, read the audit log or read another worker's
access. Even the worker a secret belongs to cannot reveal it: it already has
the value in its environment.

## Precedence

For one name, a worker receives one value: **worker > group > global**. Among
the worker's groups, the group that sorts last wins, the same rule as the scope
env files (`scope_env_layers`). Resolution is a pure function of the store and
the worker's `CC_TAGS`, so the order items were written in never matters. The
page shows "overridden for N" on a less specific item, and the access view names
the item that overrides it for each worker.

## Delivery on every execution path

| Path | When resolved | How it arrives | Audit `path` |
| --- | --- | --- | --- |
| Terminal start | at start | `tmux set-environment`, then imported by name after the scope env files, so the vault wins | `launch` |
| Terminal restart (new or surviving tmux session) | at restart | same; keys delivered last time and gone now are `unset` | `launch` |
| tmux schedule | the prompt goes to the running worker | the worker's process environment from its last start | none (no new process) |
| Shell schedule with an owner `session` | every run | the child's environment; never argv | `shell_schedule` |
| Shell schedule with no owner | | nothing: it is no worker's, so no grant covers it | |
| Chat tab turn | every turn | staged as `AMUX_VAULT_STAGE_<KEY>` in the process env, moved to its real name after the prelude sources the scope files | `chat_turn` |
| Delegate job | every job | same staging as a Chat turn | `delegate` |
| Isolated worker | as above | as above | as above |

**Isolated workers receive their provisioned secrets** (owner decision,
2026-10-14: "isolated workers can access secrets"). Secrets are environment the
owner configured, not an injected prompt, hook, steering or routing, so this is
consistent with what isolation removes. The same scoping and precedence apply:
an isolated worker gets the global items, its groups' items and its own, and
nothing scoped elsewhere. The per-worker view and the access view mark the
worker `isolated`.

Shell-schedule output is masked: any delivered value of four or more characters
that appears in stdout or stderr is replaced with `[vault:KEY]` in the run log
and the run row, per chunk as it streams and again over the whole file at exit.
A terminal pane, a Chat transcript or a delegate answer that prints a value is
the worker's output, not amux's record, and is not rewritten.

## Rotation and revocation

A running process keeps the environment it started with, and amux does not
reach into a live process. So:

- A rotation, edit or deletion applies to every **new** process at once:
  the next start, restart, shell-schedule run, Chat turn and delegate.
- A **running** terminal worker keeps the old value until it restarts. The
  Vault says so instead of pretending the change is live: the save response
  names the workers (`running_stale` / `still_held_by`), the page offers a
  one-click restart through the dashboard's existing restart, and the access
  view shows each worker's state:

| State | Meaning |
| --- | --- |
| `current` | running, holds this item at this version |
| `stale` | running, holds an older version or a different item for this name |
| `not_in_process` | running, the name was added or moved here after it started |
| `next_launch` | not running; it receives this at its next start |
| `unknown` | running, started before delivery was tracked |
| `overridden` | a more specific item with this name wins for this worker |

`revoked_held` lists names a running worker still holds whose item is gone.

## The page

Secrets tab, per the mockup: security banner, search, scope / group / worker
filters, sort, a table of name and description, masked value with reveal, scope
chip, provisioned to (with count and overrides), updated (with version) and an
actions menu (Edit, Rotate value, Who has access, Audit history, Delete).
**Add Secret** and **View Audit Log** sit above the table. Picking a worker in
the worker filter shows its provisioned view.

The Add / Edit dialog follows the mockup: Name (normalised to an environment
variable name as you type; harness names refused beside the field),
Description with a 200 counter, Value masked with an eye toggle (blank on Edit
means unchanged; typing rotates), and Scope as Global / Group with a group
picker / Worker with a worker picker, plus a live "reaches N workers" line.

Reveal shows the value in its cell for 20 seconds, then masks it again; the eye
masks it at once. Closing the page drops every revealed value. Delete asks for
confirmation and says how many workers it reaches. At 375 px the table becomes
cards, controls are 44 px, and nothing scrolls sideways.

The Payment Methods tab renders the existing card store (`vault.rs`) unchanged.

## Logging

Every mutation and delivery logs a verdict with names, scopes and counts, never
a value: `vault_secret_added`, `vault_secret_updated`, `vault_secret_rotated`,
`vault_secret_deleted`, `vault_secret_revealed`, `vault_delivered` (with
`path`), `vault_delivery_failed`, `vault_write_refused_worker_origin`,
`vault_owner_credential_missing`, `vault_refused_member`,
`vault_ledger_write_failed`.

## Tests

- `crates/amux-server/src/api/vault_secrets.rs` unit tests: sealing binds to the
  item, re-seal on move, precedence (and write-order independence), delivery
  audit carries keys only, staging moves the value after a plaintext line,
  redaction, process states, ledger and holders, the audit projection.
- `crates/amux-server/tests/vault_secrets_api.rs`: the real router with an
  owner token: authz (worker origin, no bearer) on every route, precedence
  including an isolated worker, edit and move, rotate, reveal and `no-store`,
  access, delete, audit, then a canary sweep over the tracing output, the audit
  file and the store.
- `e2e/chaos/vault-lifecycle.mjs`: a private server and tmux with real worker
  processes (a fake `claude` that reports SHA-256 of each variable): launch,
  restart, tmux schedule, shell schedule (owned, unauthorized, unowned), Chat
  turn, delegate, an isolated worker, rotation and revocation while running,
  and a leak sweep over the whole amux home, the server log, `ps` and every
  pane.
- `e2e/chaos/vault-secrets.mjs` and `e2e/chaos/vault-card.mjs`: the earlier
  proofs, moved to the Vault page and the owner bearer.
- `e2e/vault-page.spec.ts`: the page in a browser, desktop and 375 px.

## Not done

- Live re-injection into a running process. Not possible for an environment
  variable without restarting the process; the restart offer is the honest path.
- Plaintext `server.env`, scope env files and connector credentials stay
  plaintext; import is opt-in per key.
- herdr-backed workers: `running` is read from tmux, so a herdr worker reads as
  not running (`next_launch`) in the access view.

## Launchd job secrets

`amux secret-exec --job <label> --names A,B -- <command> [args...]` fetches only
those names, then replaces itself with the command using an argument vector and
child environment. Requires Python 3 (the existing Bash CLI dependency). The
wrapper preserves stdin, argument bytes, and the child's exit status. It never
prints values; the child must also avoid printing its environment. The normal
CLI endpoint discovery applies; `AMUX_API` overrides it. Delivery accepts HTTP or
HTTPS at `localhost`, `127.0.0.1`, or `::1`, with no proxy or redirects. HTTPS
pins the peer's exact leaf certificate to the first certificate in the active
local `tls/connection.pem` bundle, or `tls/cert.pem` when no bundle exists.
This pin replaces CA/hostname verification, supporting both public DNS-only
certificates and self-signed dev certificates over loopback. The TLS connection
is opened and checked before sending the token header; a different certificate
fails with `server_certificate_mismatch`. A stopped server fails closed.

An owner registers each label in `$AMUX_HOME/secret-jobs.json` (default
`~/.amux/secret-jobs.json`), a user-owned regular file with mode 0600:

```json
{
  "jobs": [
    {
      "label": "com.example.buyer-match",
      "groups": ["business"],
      "allowed_names": ["MAX_APILLOW_API_KEY"],
      "token_sha256": "<64 lowercase hex characters: SHA-256 of the token>"
    }
  ]
}
```

The server only reads this registry. There is no registration, self-approval, or
scope-override API. Groups are lowercase; labels and allowed names must be unique
within their respective lists. Existing one-shot `grants.rs` permissions do not
widen a job's standing allowlist or group scope. Registry removal revokes the
next request immediately. Rotation replaces the token and hash; it does not
change a running child's environment.

Registration is an owner operation on disk, outside workers' writable sandbox.
Generate a fresh 32-byte random token as 64 lowercase hex characters, save it
without a newline to `job-tokens/<label>.token` (0600), and store its SHA-256 in
the registry. This example prepares a **new** credential and approval entry in
an existing amux home; the owner chooses the label, groups and names first:

```python
import hashlib, json, os, pathlib, secrets, tempfile
home = pathlib.Path(os.environ.get("AMUX_HOME", pathlib.Path.home() / ".amux"))
label = "com.example.buyer-match"
groups = ["business"]
names = ["MAX_APILLOW_API_KEY"]
registry = home / "secret-jobs.json"
entries = json.loads(registry.read_text()) if registry.exists() else {"jobs": []}
assert not any(j["label"] == label for j in entries["jobs"]), "already registered"
tokens = home / "job-tokens"
tokens.mkdir(mode=0o700, exist_ok=True)
token = secrets.token_hex(32)
fd = os.open(tokens / (label + ".token"), os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
with os.fdopen(fd, "w") as stream:
    stream.write(token)
entries["jobs"].append(dict(label=label, groups=groups, allowed_names=names,
                          token_sha256=hashlib.sha256(token.encode()).hexdigest()))
fd, pending = tempfile.mkstemp(dir=home, prefix=".secret-jobs-")
with os.fdopen(fd, "w") as stream:
    json.dump(entries, stream)
os.replace(pending, registry)
```

Serialize owner registry edits. If registration stops after writing the token,
there is no approval yet: finish the registry entry using that token's hash or
remove that unregistered token before retrying. No API will adopt it implicitly.
Keep the registry, token directory and vault outside agent-writable sandboxes.

The credential uses worker identity's SHA-256 and constant-time comparison,
unconditionally (no worker rule-10 rollout bypass). The client presents it in
`X-Amux-Worker-Token`; a label alone, the owner bearer, or another job's token
cannot authorize delivery. This proves possession of the registered credential,
**not launchd process ancestry**. As with existing worker tokens and grants,
processes with unrestricted access as the same OS user can read each other's
credentials. File permissions protect against other OS users; OS sandboxing is
needed to distinguish agents under the same user. Do not describe this as a
security boundary against an unsandboxed same-user process.

A plist's `ProgramArguments` uses absolute executable paths and separate array
entries (no shell, no secret in the plist):

```xml
<key>ProgramArguments</key>
<array>
  <string>/absolute/path/to/amux</string>
  <string>secret-exec</string>
  <string>--job</string><string>com.example.buyer-match</string>
  <string>--names</string><string>MAX_APILLOW_API_KEY</string>
  <string>--</string>
  <string>/absolute/path/to/real-command</string>
  <string>argument with spaces</string>
</array>
```

The agent surface is `POST /api/vault/job-secrets`, JSON
`{"job":"com.example.buyer-match","names":["MAX_APILLOW_API_KEY"]}` and the token
header above. Real socket loopback metadata is mandatory. A local proxy such as tailscale serve
can forward remote clients from loopback; the job token is the authorization control. Success is 200 with
`{"values":{"MAX_APILLOW_API_KEY":"..."}}` and `Cache-Control: no-store`.
Resolution uses vault global/group precedence, including the last sorted group
winning; worker-specific secrets never apply to jobs, even for an equal label.
Every requested name must be both allowed and resolvable before any is returned.
Failure JSON is `{"error":"<code>","names":["<affected name>"]}`; codes include
`job_identity_refused` (also for unknown labels or unavailable/invalid registry state), `name_not_allowed`,
`missing_or_wrong_group` and vault errors. Registry error details are local audit/log signals only. No error includes
values. The CLI exits nonzero and reports a fixed code and requested names;
`server_unavailable` means no usable server response. It refuses unexpected or
missing response names. Successful delivery is audited by label and names;
refusals emit `verdict=secret_job_refused` and a value-free `secrets_job_refused`
audit record with a fixed reason and requested-name count for sweeps.
