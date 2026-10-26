//! Agent-initiated worker delete — the one exception to "deleting a worker is
//! the owner's" (Max, 2026-10-26, rule lift 1; ai-config #378, #387).
//!
//! A worker request may delete another worker only when all three hold:
//!
//! - **Caller.** The verified caller ([`worker_origin`], never a body field)
//!   is the lead of the target's team ([`lead_team`]: in `leads`, sharing a
//!   non-`leads` group with a target that is not itself a lead), or the owner
//!   agent [`OWNER_AGENT`], which may delete any worker but itself.
//! - **Stopped.** The target's agent is not running (`is_running` false).
//! - **Handoff saved.** [`handoff_path`], `<amux home>/handoffs/<name>.md`,
//!   is a regular, non-empty file. A symlink or a directory does not count.
//!
//! Every other delete stays the owner's (dashboard UI token or verified
//! member). Each decision, allowed or refused, is appended as one JSON line
//! to [`AUDIT_LOG`] and traced with the same fields. The delete itself runs in
//! `delete_post`, which closes the worker's tmux session on the server's own
//! socket, so agents never run tmux (the worker-pane tmux guard still refuses
//! them).
use super::session_verbs::{home, lead_team, valid_session_name, worker_origin, OWNER_AGENT};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;
use std::path::{Path, PathBuf};

/// Where saved handoffs live, under the amux home.
pub(crate) const HANDOFFS_DIR: &str = "handoffs";
/// The decision log, under the amux home: one JSON object per line.
pub(crate) const AUDIT_LOG: &str = "logs/worker-delete.jsonl";

/// The handoff a delete requires for `name`.
pub(crate) fn handoff_path(home: &Path, name: &str) -> PathBuf {
    home.join(HANDOFFS_DIR).join(format!("{name}.md"))
}

/// A saved handoff: a regular, non-empty file, not followed through a link.
pub(crate) fn handoff_saved(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_file() && m.len() > 0)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Verdict {
    Allowed,
    NotAuthorized,
    Running,
    NoHandoff,
}

impl Verdict {
    pub(crate) fn code(self) -> &'static str {
        match self {
            Verdict::Allowed => "agent_delete_allowed",
            Verdict::NotAuthorized => "agent_delete_not_authorized",
            Verdict::Running => "agent_delete_running",
            Verdict::NoHandoff => "agent_delete_no_handoff",
        }
    }

    fn status(self) -> StatusCode {
        match self {
            Verdict::Allowed => StatusCode::OK,
            Verdict::NotAuthorized => StatusCode::FORBIDDEN,
            Verdict::Running | Verdict::NoHandoff => StatusCode::CONFLICT,
        }
    }
}

/// Whether verified `origin` may delete `target` at all, before state checks.
fn caller_authorized(origin: &str, target: &str) -> bool {
    if !valid_session_name(origin) || !valid_session_name(target) || origin == target {
        return false;
    }
    let owner_agent = |n: &str| n.eq_ignore_ascii_case(OWNER_AGENT);
    if owner_agent(origin) {
        return !owner_agent(target);
    }
    lead_team(origin, target).is_some()
}

/// The decision, in check order: caller, then running, then handoff.
pub(crate) fn decide(origin: &str, target: &str, running: bool, handoff_saved: bool) -> Verdict {
    if !caller_authorized(origin, target) {
        Verdict::NotAuthorized
    } else if running {
        Verdict::Running
    } else if !handoff_saved {
        Verdict::NoHandoff
    } else {
        Verdict::Allowed
    }
}

fn audit(home: &Path, origin: &str, target: &str, handoff: &Path, verdict: Verdict) {
    use std::io::Write;
    let line = json!({
        "ts": chrono::Utc::now().to_rfc3339(),
        "caller": origin,
        "worker": target,
        "handoff": handoff.display().to_string(),
        "verdict": verdict.code(),
    });
    let path = home.join(AUDIT_LOG);
    let wrote = path.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|_| {
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&path)?;
        writeln!(f, "{line}")
    });
    if let Err(error) = wrote {
        tracing::warn!(path = %path.display(), %error, verdict = "agent_delete_audit_failed",
            "could not append the worker-delete decision log");
    }
    tracing::info!(caller = origin, worker = target, handoff = %handoff.display(), verdict = verdict.code(),
        measured = true, n_considered = 1, "agent worker-delete decision");
}

/// Gate an agent's delete of `target`. `Ok` lets `delete_post` proceed; `Err`
/// is the refusal to return. Called only when the owner's proof is absent.
pub(crate) async fn gate(headers: &HeaderMap, target: &str) -> Result<(), Response> {
    let origin = worker_origin(headers);
    let home = home();
    let handoff = handoff_path(&home, target);
    // Running and handoff are read only for an authorized caller, so a refused
    // caller learns nothing about another team's worker.
    let verdict = if caller_authorized(&origin, target) {
        decide(&origin, target, super::session_verbs::is_running(target).await, handoff_saved(&handoff))
    } else {
        Verdict::NotAuthorized
    };
    audit(&home, &origin, target, &handoff, verdict);
    let error = match verdict {
        Verdict::Allowed => return Ok(()),
        Verdict::NotAuthorized => format!(
            "deleting a worker must be initiated by a human in the dashboard, except: the lead of \
             '{target}''s team, or {OWNER_AGENT}, may delete it once it is stopped and its handoff \
             is saved (set AMUX_ALLOW_AGENT_SESSION_DELETE=1 in ~/.amux/server.env to allow automation)"
        ),
        Verdict::Running => format!("'{target}' is still running; stop it first (amux stop {target})"),
        Verdict::NoHandoff => format!("'{target}' has no saved handoff; write it to {} first", handoff.display()),
    };
    Err((verdict.status(), Json(json!({"ok": false, "code": verdict.code(), "error": error,
        "handoff": handoff.display().to_string()}))).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_handoff_is_a_regular_non_empty_file() {
        let home = tempfile::tempdir().unwrap();
        let p = handoff_path(home.path(), "dev-x");
        assert_eq!(p, home.path().join("handoffs/dev-x.md"));
        assert!(!handoff_saved(&p), "missing");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, "").unwrap();
        assert!(!handoff_saved(&p), "empty");
        std::fs::write(&p, "done; next: none\n").unwrap();
        assert!(handoff_saved(&p), "saved");
        let dir = handoff_path(home.path(), "dev-dir");
        std::fs::create_dir_all(&dir).unwrap();
        assert!(!handoff_saved(&dir), "a directory");
        #[cfg(unix)]
        {
            let link = handoff_path(home.path(), "dev-link");
            std::os::unix::fs::symlink(&p, &link).unwrap();
            assert!(!handoff_saved(&link), "a symlink to a saved handoff");
        }
    }

    #[test]
    fn checks_run_caller_first_then_running_then_handoff() {
        // A self-delete and an invalid origin are refused before state is read.
        assert_eq!(decide("dev-x", "dev-x", false, true), Verdict::NotAuthorized);
        assert_eq!(decide("", "dev-x", false, true), Verdict::NotAuthorized);
        assert_eq!(decide(super::super::worker_identity::UNVERIFIED_IDENTITY, "dev-x", false, true),
            Verdict::NotAuthorized);
        // The owner agent: any worker but itself, any case.
        assert_eq!(decide(OWNER_AGENT, "dev-x", true, true), Verdict::Running);
        assert_eq!(decide(OWNER_AGENT, "dev-x", false, false), Verdict::NoHandoff);
        assert_eq!(decide(OWNER_AGENT, "dev-x", false, true), Verdict::Allowed);
        assert_eq!(decide("MrPopo", OWNER_AGENT, false, true), Verdict::NotAuthorized);
    }

    #[test]
    fn every_decision_appends_one_audit_line() {
        let home = tempfile::tempdir().unwrap();
        let h = handoff_path(home.path(), "dev-x");
        audit(home.path(), "dev-lead", "dev-x", &h, Verdict::NoHandoff);
        audit(home.path(), "dev-lead", "dev-x", &h, Verdict::Allowed);
        let log = std::fs::read_to_string(home.path().join(AUDIT_LOG)).unwrap();
        let lines: Vec<serde_json::Value> = log.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(lines.len(), 2, "{log}");
        for (v, want) in lines.iter().zip(["agent_delete_no_handoff", "agent_delete_allowed"]) {
            assert_eq!((v["caller"].as_str(), v["worker"].as_str(), v["verdict"].as_str()),
                (Some("dev-lead"), Some("dev-x"), Some(want)), "{v}");
            assert_eq!(v["handoff"].as_str(), Some(h.to_str().unwrap()));
        }
    }
}
