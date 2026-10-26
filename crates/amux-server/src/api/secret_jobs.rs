//! Owner-registered job credentials and least-privilege vault delivery.
//! refs maxtongwang/ai-config#310. No registration or grant override API.
use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{routing::post, Json, Router};
use serde::Deserialize;
use serde_json::json;
use std::collections::BTreeSet;
use std::net::SocketAddr;
use std::path::Path;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Job {
    label: String,
    groups: BTreeSet<String>,
    allowed_names: BTreeSet<String>,
    token_sha256: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Registry {
    jobs: Vec<Job>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    job: String,
    names: BTreeSet<String>,
}

fn registry(home: &Path) -> Result<Registry, &'static str> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(home.join("secret-jobs.json"))
        .map_err(|_| "job_registry_unavailable")?;
    let meta = file.metadata().map_err(|_| "job_registry_unavailable")?;
    // A registry is owner approval, not an agent-writable API resource.
    if !meta.is_file() || meta.mode() & 0o077 != 0 || meta.uid() != unsafe { libc::geteuid() } {
        return Err("job_registry_permissions");
    }
    let registry: Registry = serde_json::from_reader(file).map_err(|_| "job_registry_invalid")?;
    let mut labels = BTreeSet::new();
    for j in &registry.jobs {
        if !super::worker_identity::valid_identity(&j.label)
            || !labels.insert(&j.label)
            || j.token_sha256.len() != 64
            || !j.token_sha256.bytes().all(|b| b.is_ascii_hexdigit())
            || j.groups
                .iter()
                .any(|g| !super::worker_identity::valid_identity(g) || *g != g.to_lowercase())
            || j.allowed_names.is_empty()
            || j.allowed_names
                .iter()
                .any(|n| super::vault_secrets::valid_key(n).is_err())
        {
            return Err("job_registry_invalid");
        }
    }
    Ok(registry)
}

fn refuse(code: StatusCode, reason: &str, names: &BTreeSet<String>) -> Response {
    // Do not log submitted credentials or error strings from the keystore.
    tracing::warn!(
        verdict = "secret_job_refused",
        reason,
        n_considered = names.len(),
        measured = true,
        "job secret delivery refused"
    );
    (code, Json(json!({"error": reason, "names": names}))).into_response()
}

async fn deliver(
    State(_state): State<super::AppState>,
    peer: Result<ConnectInfo<SocketAddr>, axum::extract::rejection::ExtensionRejection>,
    headers: HeaderMap,
    body: Result<Json<Request>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Ok(Json(request)) = body else {
        return refuse(StatusCode::BAD_REQUEST, "invalid_request", &BTreeSet::new());
    };
    let names = &request.names;
    if !peer.is_ok_and(|p| p.0.ip().is_loopback()) {
        return refuse(StatusCode::FORBIDDEN, "loopback_required", names);
    }
    if names.is_empty()
        || names
            .iter()
            .any(|n| super::vault_secrets::valid_key(n).is_err())
    {
        return refuse(StatusCode::BAD_REQUEST, "invalid_names", &BTreeSet::new());
    }
    let home = crate::config::amux_home();
    let registry = match registry(&home) {
        Ok(r) => r,
        Err(reason) => return refuse(StatusCode::SERVICE_UNAVAILABLE, reason, names),
    };
    let Some(job) = registry.jobs.iter().find(|j| j.label == request.job) else {
        return refuse(StatusCode::FORBIDDEN, "unknown_job", names);
    };
    let token = headers
        .get(super::worker_identity::TOKEN_HEADER)
        .and_then(|v| v.to_str().ok());
    if !token.is_some_and(|t| super::worker_identity::token_matches(t, &job.token_sha256)) {
        return refuse(StatusCode::FORBIDDEN, "job_identity_refused", names);
    }
    let denied: BTreeSet<_> = names.difference(&job.allowed_names).cloned().collect();
    if !denied.is_empty() {
        return refuse(StatusCode::FORBIDDEN, "name_not_allowed", &denied);
    }
    match super::vault_secrets::deliver_job(&home, &job.groups, names) {
        Ok(values) => {
            super::vault::audit_line(
                &home,
                json!({"ts": crate::config::now_f64(),
                "event": "secrets_job_delivered", "by": job.label, "keys": names}),
            );
            tracing::info!(
                verdict = "secret_job_delivered",
                measured = true,
                n_considered = names.len(),
                "job secrets delivered"
            );
            (
                [("cache-control", "no-store"), ("pragma", "no-cache")],
                Json(json!({"values": values})),
            )
                .into_response()
        }
        Err((reason, missing)) => refuse(StatusCode::FORBIDDEN, reason, &missing),
    }
}

pub fn routes() -> Router<super::AppState> {
    Router::new().route("/api/vault/job-secrets", post(deliver))
}
