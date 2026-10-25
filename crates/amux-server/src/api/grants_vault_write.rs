//! Standing, reviewed vault-write configuration (ai-config #276).
//! Kept separate from grants.rs's expiring, one-shot approvals. No request can seed it.

use crate::api::{vault_secrets, worker_identity, AppState};
use crate::config::{amux_home, now_f64};
use crate::db::{SharedStore, WriteOutcome};
use axum::extract::{Path as AxPath, State};
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use rusqlite::{Connection, OptionalExtension};
use serde_json::{json, Value};
use std::path::Path;

pub(crate) const ID: &str = "vault-owner-global-write-v1";

// Granted global values reach every worker; allow credentials, never process
// controls or implicit client authentication/endpoint settings. MAX_ names
// require tools to opt in instead of overriding their installed authentication.
const GRANTED_KEY_PREFIX: &str = "MAX_";
const GRANTED_KEY_SUFFIXES: &[&str] = &[
    "_API_KEY",
    "_TOKEN",
    "_SECRET",
    "_CLIENT_ID",
    "_CLIENT_SECRET",
];
const GRANTED_CLIENT_AUTH_OR_ENDPOINT: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "OPENAI_API_KEY",
    "GEMINI_API_KEY",
    "GOOGLE_API_KEY",
    "*_BASE_URL",
    "*_API_BASE",
    "CLAUDE_*",
    "CODEX_*",
];
const GRANTED_INJECTION_SENSITIVE: &[&str] = &[
    "PATH",
    "HOME",
    "SHELL",
    "USER",
    "TMPDIR",
    "ENV",
    "ZDOTDIR",
    "LD_*",
    "DYLD_*",
    "NODE_*",
    "PYTHON*",
    "BASH_*",
    "PROMPT_COMMAND",
    "GIT_*",
    "SSH_*",
    "PERL5*",
    "RUBY*",
    "CC_*",
    "AMUX_*",
];

fn key_matches(key: &str, pattern: &str) -> bool {
    pattern
        .split_once('*')
        .map_or(key == pattern, |(prefix, suffix)| {
            key.starts_with(prefix) && key.ends_with(suffix)
        })
}

fn key_refusal_rule(key: &str) -> Option<&'static str> {
    let denied = [
        ("client_auth_or_endpoint", GRANTED_CLIENT_AUTH_OR_ENDPOINT),
        ("injection_sensitive", GRANTED_INJECTION_SENSITIVE),
    ]
    .into_iter()
    .find_map(|(rule, patterns)| patterns.iter().any(|p| key_matches(key, p)).then_some(rule));
    if denied.is_some() {
        return denied;
    }
    let Some(name) = key.strip_prefix(GRANTED_KEY_PREFIX) else {
        return Some("not_max_prefixed");
    };
    let credential_shaped = name
        .starts_with(|c: char| c.is_ascii_uppercase() || c.is_ascii_digit())
        && name
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
        && GRANTED_KEY_SUFFIXES.iter().any(|s| key.ends_with(*s));
    (!credential_shaped).then_some("not_credential_shaped")
}

pub(crate) fn permitted_keys(
    home: &Path,
    grant: Option<&GrantUse>,
    keys: &[&str],
) -> Result<(), (StatusCode, Value)> {
    let Some(grant) = grant else {
        return Ok(());
    };
    for key in keys {
        if let Some(rule) = key_refusal_rule(key) {
            // Neither the submitted name nor value is needed in this refusal.
            let (status, mut body) = refusal(home, &grant.name, "vault_grant_key_not_permitted");
            body["rule"] = json!(rule);
            body["error"] =
                json!("granted writes require a permitted MAX_ prefixed credential name");
            tracing::warn!(
                rule,
                verdict = "vault_grant_key_not_permitted",
                measured = true,
                n_considered = 1,
                "standing vault key policy refused a write"
            );
            return Err((status, body));
        }
    }
    Ok(())
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct GrantUse {
    pub name: String,
    id: String,
}

/// Main has two registries: managed workers have immutable wrk_ IDs; legacy
/// env-only workers have CC_INCARNATION (stable across renames, fresh on copy or
/// recreation). Always pin the incarnation, and also the wrk_ ID when present.
fn owner_identity(
    conn: &Connection,
    home: &Path,
) -> rusqlite::Result<Option<(String, String, String)>> {
    let owner = crate::api::session_verbs::OWNER_AGENT;
    let Some(inc) = crate::api::rename_identity::incarnation(home, owner) else {
        return Ok(None);
    };
    let row = crate::db::queries::get_worker(conn, owner)?;
    match row {
        Some(w) if w.display_name == owner => Ok(Some((w.id, "worker_id".into(), inc))),
        Some(_) => Ok(None), // An alias is not the configured owner registration.
        None => Ok(Some((inc.clone(), "incarnation".into(), inc))),
    }
}

/// Called at startup, never by the grantee. The first registered identity is
/// pinned permanently. Restarts cannot follow a replacement or revive a revoke.
pub(crate) fn bind_seed(store: &SharedStore, home: &Path) -> anyhow::Result<()> {
    let home = home.to_path_buf();
    store.write(move |conn| {
        let binding = owner_identity(conn, &home)?;
        let changed = if let Some((id, kind, incarnation)) = binding {
            conn.execute("UPDATE vault_write_grants SET grantee_id=?1, identity_kind=?2, incarnation=?3
                WHERE id=?4 AND grantee='configured_owner' AND grantee_id IS NULL AND revoked_at IS NULL",
                rusqlite::params![id, kind, incarnation, ID])?
        } else {
            tracing::warn!(grant_id = ID, verdict = "vault_grant_owner_unregistered", measured = true,
                n_considered = 1, "standing vault grant has no registered owner identity; writes fail closed");
            0
        };
        if changed > 0 {
            tracing::info!(grant_id = ID, verdict = "vault_grant_bound", measured = true,
                n_considered = changed, "standing vault grant pinned to the configured owner's registered identity");
        }
        Ok(WriteOutcome { applied: changed > 0, events: vec![] })
    })?;
    Ok(())
}

fn refusal(home: &Path, name: &str, code: &'static str) -> (StatusCode, Value) {
    let ts = now_f64();
    tracing::warn!(
        grantee = name,
        grant_id = ID,
        ts,
        scope = "global",
        verdict = code,
        measured = true,
        n_considered = 1,
        "standing vault write refused"
    );
    crate::api::vault::audit_line(
        home,
        json!({"ts": ts, "event": "secret_grant_refused", "by": name,
        "grant_id": ID, "code": code}),
    );
    (
        StatusCode::FORBIDDEN,
        json!({"ok": false, "code": code,
        "error": "an active, identity-bound vault grant permits only global secret create and update"}),
    )
}

/// Strict token verification even without rule 10, ConnectInfo, or a minted
/// token rollout exception. A valid name claim is not credential verification.
pub(crate) fn check(
    state: &AppState,
    home: &Path,
    headers: &HeaderMap,
) -> Result<GrantUse, (StatusCode, Value)> {
    let name = worker_identity::verified_worker(headers)
        .ok()
        .flatten()
        .filter(|n| n == crate::api::session_verbs::OWNER_AGENT)
        .ok_or_else(|| {
            refusal(
                home,
                &worker_identity::claimed_worker(headers),
                "vault_grant_required",
            )
        })?;
    if crate::api::org::is_verified_local_member(headers) {
        return Err(refusal(home, &name, "vault_grant_required"));
    }
    let incarnation = worker_identity::authenticated_incarnation(home, headers, &name)
        .ok_or_else(|| refusal(home, &name, "vault_grant_required"))?;
    let allowed = (|| -> anyhow::Result<Option<String>> {
        let conn = state.store.read()?;
        let registered = owner_identity(&conn, home)?;
        let pinned = conn
            .query_row(
                "SELECT grantee_id, identity_kind, incarnation FROM vault_write_grants
            WHERE id=?1 AND grantee='configured_owner' AND scope='global' AND revoked_at IS NULL",
                [ID],
                |r| {
                    Ok((
                        r.get::<_, Option<String>>(0)?,
                        r.get::<_, Option<String>>(1)?,
                        r.get::<_, Option<String>>(2)?,
                    ))
                },
            )
            .optional()?;
        Ok(match (pinned, registered) {
            (
                Some((Some(id), Some(kind), Some(inc))),
                Some((registered_id, registered_kind, registered_inc)),
            ) => (id == registered_id
                && kind == registered_kind
                && inc == registered_inc
                && inc == incarnation)
                .then_some(id),
            _ => None,
        })
    })();
    match allowed {
        Ok(Some(id)) => Ok(GrantUse { name, id }),
        Ok(None) => Err(refusal(home, &name, "vault_grant_required")),
        Err(error) => {
            tracing::warn!(%error, verdict = "vault_grant_check_failed", measured = false, n_considered = 0,
                "standing vault grant lookup failed closed");
            Err((
                StatusCode::SERVICE_UNAVAILABLE,
                json!({"code": "vault_grant_unavailable", "error": "grant lookup unavailable"}),
            ))
        }
    }
}

/// The under-lock proof must still name the identity checked at route entry.
pub(crate) fn recheck(
    state: &AppState,
    home: &Path,
    headers: &HeaderMap,
    expected: &GrantUse,
) -> Result<(), (StatusCode, Value)> {
    if check(state, home, headers)? != *expected {
        return Err(refusal(home, &expected.name, "vault_grant_required"));
    }
    Ok(())
}

pub(crate) fn global_only(
    home: &Path,
    grant: Option<&GrantUse>,
    scopes: &[&vault_secrets::Scope],
) -> Result<(), (StatusCode, Value)> {
    let Some(g) = grant else {
        return Ok(());
    };
    if scopes.iter().any(|s| **s != vault_secrets::Scope::Global) {
        return Err(refusal(home, &g.name, "vault_scope_not_granted"));
    }
    Ok(())
}

/// Saved in the SAME atomic file replacement as the ciphertext, so a crash
/// cannot retain a granted write while losing the facts needed to audit it.
pub(crate) fn receipt(grant: &GrantUse, item: &Value) -> Value {
    json!({"receipt_id": format!("vault-grant:{}", ulid::Ulid::new()),
        "ts": now_f64(), "event": "secret_granted_write", "key": item["key"], "scope": item["scope"],
        "item": item["id"], "version": item["version"], "grantee": grant.name, "grantee_id": grant.id,
        "grant_id": ID})
}

/// Replay only known metadata, never arbitrary fields from a stored document.
/// The event's receipt ID deduplicates the feed in the same DB transaction.
pub(crate) fn validated_receipt(receipt: &Value) -> anyhow::Result<Value> {
    let fields = [
        "receipt_id",
        "ts",
        "event",
        "key",
        "scope",
        "item",
        "version",
        "grantee",
        "grantee_id",
        "grant_id",
    ];
    let entry = Value::Object(
        fields
            .into_iter()
            .map(|k| (k.to_string(), receipt[k].clone()))
            .collect(),
    );
    entry["receipt_id"]
        .as_str()
        .filter(|s| s.starts_with("vault-grant:"))
        .ok_or_else(|| anyhow::anyhow!("invalid vault grant receipt id"))?;
    anyhow::ensure!(
        entry["event"] == "secret_granted_write"
            && ["key", "item", "grantee", "grantee_id"]
                .iter()
                .all(|k| entry[*k].is_string()),
        "invalid vault grant receipt fields"
    );
    entry["ts"]
        .as_f64()
        .ok_or_else(|| anyhow::anyhow!("invalid receipt time"))?;
    anyhow::ensure!(
        entry["scope"] == "global"
            && entry["grant_id"] == ID
            && entry["version"].as_u64().is_some(),
        "invalid vault grant receipt metadata"
    );
    Ok(entry)
}

pub(crate) fn publish_receipt(
    state: &AppState,
    home: &Path,
    receipt: &Value,
) -> anyhow::Result<()> {
    use std::io::Write;
    let entry = validated_receipt(receipt)?;
    let id = entry["receipt_id"]
        .as_str()
        .expect("validated receipt id")
        .to_string();
    let ts = entry["ts"].as_f64().expect("validated receipt time");
    let notice = format!(
        "Vault write: {} ({}) wrote {} at global scope, version {} (grant {}, Unix seconds {}).",
        entry["grantee"].as_str().unwrap_or(""),
        entry["grantee_id"].as_str().unwrap_or(""),
        entry["key"].as_str().unwrap_or(""),
        entry["version"],
        ID,
        ts
    );
    let (body, grantee, idem) = (
        entry.to_string(),
        entry["grantee"].as_str().unwrap_or("").to_string(),
        id.clone(),
    );
    state.store.write(move |c| {
        let recorded = c.query_row("SELECT 1 FROM session_events WHERE idem=?1", [&idem], |_| Ok(())).optional()?.is_some();
        if recorded { return Ok(WriteOutcome { applied: false, events: vec![] }); }
        c.execute("INSERT INTO conversation_posts(conv,author_key,author_name,body,ts) VALUES(?1,'system:vault','amux vault',?2,?3)",
            rusqlite::params![format!("g:{}", crate::api::session_verbs::LEADS_GROUP), notice, chrono::Utc::now().timestamp_millis()])?;
        c.execute("INSERT INTO session_events(ts,session,type,data,source,idem) VALUES(?1,?2,'vault.granted_write',?3,'vault',?4)",
            rusqlite::params![ts, grantee, body, idem])?;
        Ok(WriteOutcome { applied: true, events: vec![] })
    })?;
    let log_dir = home.join("logs");
    std::fs::create_dir_all(&log_dir)?;
    let path = log_dir.join("vault-audit.jsonl");
    let prior = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e.into()),
    };
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    if !prior.lines().any(|line| {
        serde_json::from_str::<Value>(line)
            .ok()
            .is_some_and(|v| v["receipt_id"] == id)
    }) {
        // Preserve a crash-truncated line as failed evidence; start the replay
        // on its own line so it remains a parseable audit record.
        if !prior.is_empty() && !prior.ends_with('\n') {
            writeln!(file)?;
        }
        writeln!(file, "{entry}")?;
    }
    file.sync_data()?;
    tracing::info!(key = %entry["key"], scope = "global", ts, grantee = %entry["grantee"],
        grantee_id = %entry["grantee_id"], grant_id = ID, version = %entry["version"],
        measured = true, n_considered = 1, verdict = "vault_granted_write", "standing vault write committed");
    Ok(())
}

async fn list(State(state): State<AppState>, uri: Uri, headers: HeaderMap) -> Response {
    // Unlike mutation bootstrap on a tokenless host, listing pinned identities
    // always requires an owner credential, including from loopback clients.
    if !super::auth::has_owner_token(&state, &headers, &uri) {
        tracing::warn!(
            verdict = "vault_grants_auth_required",
            measured = true,
            n_considered = 1,
            "vault grant listing requires owner authentication"
        );
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"code": "vault_grants_auth_required",
            "error": "send the configured owner bearer to list vault grants"})),
        )
            .into_response();
    }
    if let Err(r) = vault_secrets::owner_only(&state, &headers, &uri, &amux_home(), "grant-list") {
        return *r;
    }
    let result = state.store.read_async(|conn| {
        // Names, IDs, scopes, timestamps only. No arbitrary JSON from disk.
        let mut stmt = conn.prepare("SELECT id, grantee_id, scope, created_at, revoked_at FROM vault_write_grants ORDER BY id")?;
        let grants = stmt.query_map([], |r| Ok(json!({"id": r.get::<_, String>(0)?,
            "grantee": crate::api::session_verbs::OWNER_AGENT, "grantee_id": r.get::<_, Option<String>>(1)?,
            "scope": r.get::<_, String>(2)?,
            "created_at": r.get::<_, f64>(3)?, "revoked_at": r.get::<_, Option<f64>>(4)?})))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(grants)
    }).await;
    match result {
        Ok(grants) => Json(
            json!({"measured": true, "n_considered": grants.len(), "grants": grants,
            "timestamp_unit": "unix_seconds"}),
        )
        .into_response(),
        Err(e) => crate::api::internal(e),
    }
}

async fn refuse_create(State(state): State<AppState>, uri: Uri, headers: HeaderMap) -> Response {
    if let Err(r) = vault_secrets::owner_only(&state, &headers, &uri, &amux_home(), "grant-create")
    {
        return *r;
    }
    (StatusCode::METHOD_NOT_ALLOWED, Json(json!({"code": "vault_grants_seeded", "error": "grants are reviewed configuration; use regrant for the seeded grant"}))).into_response()
}

#[derive(Debug, thiserror::Error)]
enum ChangeError {
    #[error("the configured owner agent has no registered identity")]
    OwnerUnregistered,
    #[error(transparent)]
    Storage(#[from] anyhow::Error),
}

fn change(state: &AppState, home: &Path, id: &str, regrant: bool) -> Result<bool, ChangeError> {
    let _guard = vault_secrets::store_lock()
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let binding = if regrant {
        let conn = state.store.read()?;
        Some(
            owner_identity(&conn, home)
                .map_err(anyhow::Error::from)?
                .ok_or(ChangeError::OwnerUnregistered)?,
        )
    } else {
        None
    };
    let id = id.to_string();
    let result = state.store.write(move |conn| {
        let changed = if let Some((worker_id, kind, inc)) = binding {
            conn.execute("UPDATE vault_write_grants SET grantee_id=?1, identity_kind=?2, incarnation=?3, revoked_at=NULL WHERE id=?4",
                rusqlite::params![worker_id, kind, inc, id])?
        } else {
            conn.execute("UPDATE vault_write_grants SET revoked_at=COALESCE(revoked_at,?1) WHERE id=?2", rusqlite::params![now_f64(), id])?
        };
        Ok(WriteOutcome { applied: changed > 0, events: vec![] })
    })?;
    tracing::info!(
        grant_id = ID,
        regrant,
        measured = true,
        n_considered = usize::from(result.applied),
        verdict = "vault_grant_owner_changed",
        "owner changed the standing vault grant"
    );
    Ok(result.applied)
}

async fn revoke(
    State(state): State<AppState>,
    uri: Uri,
    headers: HeaderMap,
    AxPath(id): AxPath<String>,
) -> Response {
    change_route(state, uri, headers, id, false)
}
async fn regrant(
    State(state): State<AppState>,
    uri: Uri,
    headers: HeaderMap,
    AxPath(id): AxPath<String>,
) -> Response {
    change_route(state, uri, headers, id, true)
}
fn change_route(
    state: AppState,
    uri: Uri,
    headers: HeaderMap,
    id: String,
    regrant: bool,
) -> Response {
    let home = amux_home();
    if let Err(r) = vault_secrets::owner_only(&state, &headers, &uri, &home, "grant-change") {
        return *r;
    }
    match change(&state, &home, &id, regrant) {
        Ok(true) => Json(json!({"ok": true, "id": id, "revoked": !regrant})).into_response(),
        Ok(false) => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "no such grant"})),
        )
            .into_response(),
        Err(ChangeError::OwnerUnregistered) => {
            tracing::warn!(
                verdict = "vault_grant_owner_unregistered",
                measured = true,
                n_considered = 1,
                "cannot regrant without a registered owner agent"
            );
            (
                StatusCode::CONFLICT,
                Json(json!({"code": "vault_grant_owner_unregistered",
                "error": "register the configured owner agent before regranting"})),
            )
                .into_response()
        }
        Err(ChangeError::Storage(e)) => crate::api::internal(e),
    }
}

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/vault/grants", get(list).post(refuse_create))
        .route("/api/vault/grants/{id}/revoke", post(revoke))
        .route("/api/vault/grants/{id}/regrant", post(regrant))
}
