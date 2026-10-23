//! Files API (RR-0093): browse / download / upload rooted at `$HOME`
//! (override: `AMUX_FILES_ROOT`, which is also how tests pin a temp root).
//! Uploads refuse dotfiles, sensitive paths, code-execution targets, symlinks,
//! existing executables and files with other hard links (refs maxtongwang/ai-config#218).
//!
//! Routes (nested at `/api/files`):
//! - `GET  /api/files?path=<rel>`            — directory listing
//!   `{path, entries:[{name,size,mtime,kind}]}` (kind: file|dir|symlink|other)
//! - `GET  /api/files/download?path=<rel>`   — raw bytes, attachment headers
//! - `POST /api/files/upload?path=<rel>`     — RAW REQUEST BODY, not
//!   multipart: the workspace's axum has no `multipart` feature (checked —
//!   default features + http2 only), and adding one for a single endpoint is
//!   not worth the surface. Callers send the file bytes as the body:
//!   `curl --data-binary @file '$URL/api/files/upload?path=dir/file'`.
//!
//! Size limits are honest: 50MB on upload (413 beyond, enforced by both the
//! body-limit layer and an explicit check) and the same cap on download —
//! the handler buffers the file, so serving a 4GB file would really mean
//! buffering 4GB; refusing with 413 tells the truth instead.
//!
//! Path traversal: every request path is joined to the root, canonicalized
//! (symlinks resolved), and prefix-checked against the canonicalized root.
//! `../` escapes and symlinks pointing outside the root both fail the same
//! check. Uploads additionally reject `..`/absolute components lexically
//! BEFORE any filesystem work, because the target may not exist yet, check
//! the created parent the same way, and replace (never follow) a symlink at
//! the target itself.

use super::AppState;
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Query};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::{Component, Path, PathBuf};

pub const MAX_BYTES: usize = 50 * 1024 * 1024;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/", get(list))
        .route("/download", get(download))
        // Slack above the cap so the explicit 413 below (with a JSON body
        // naming the limit) fires before the layer's bare-text rejection.
        .route(
            "/upload",
            post(upload).layer(DefaultBodyLimit::max(MAX_BYTES + 64 * 1024)),
        )
}

fn err(status: StatusCode, body: Value) -> Response {
    (status, Json(body)).into_response()
}

/// The files root: `AMUX_FILES_ROOT` (tests, containers) else `$HOME`.
fn files_root() -> PathBuf {
    std::env::var("AMUX_FILES_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            std::env::var("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|_| "/".into())
        })
}

/// Resolve a request path that must EXIST (list/download): join, canonicalize
/// (resolving `..` and symlinks), then require the result to stay under the
/// canonicalized root. Returns a caller-safe error string.
pub fn resolve_existing(root: &Path, rel: &str) -> Result<PathBuf, String> {
    let canon_root = root
        .canonicalize()
        .map_err(|e| format!("files root {} unusable: {e}", root.display()))?;
    let rel = rel.trim();
    let joined = if rel.is_empty() {
        canon_root.clone()
    } else {
        let p = Path::new(rel);
        // Absolute paths are allowed but get the identical containment check.
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            canon_root.join(p)
        }
    };
    let canon = joined
        .canonicalize()
        .map_err(|_| format!("no such path: {rel}"))?;
    if !canon.starts_with(&canon_root) {
        return Err("path escapes the files root".into());
    }
    Ok(canon)
}

/// Lexical validation for upload targets (which may not exist yet): every
/// component must be a normal name — no `..`, no absolute prefix, no `.`.
pub fn validate_upload_rel(rel: &str) -> Result<PathBuf, String> {
    let rel = rel.trim();
    if rel.is_empty() {
        return Err("path required".into());
    }
    let p = Path::new(rel);
    if p.is_absolute() {
        return Err("upload path must be relative to the files root".into());
    }
    let mut clean = PathBuf::new();
    for c in p.components() {
        match c {
            Component::Normal(seg) => clean.push(seg),
            Component::CurDir => {}
            _ => return Err("upload path may not contain '..' or a root prefix".into()),
        }
    }
    if clean.as_os_str().is_empty() {
        return Err("path required".into());
    }
    Ok(clean)
}

#[derive(Deserialize)]
pub struct PathParam {
    #[serde(default)]
    path: String,
}

#[derive(serde::Serialize)]
pub struct FileEntry {
    pub name: String,
    pub size: u64,
    pub mtime: i64,
    pub kind: &'static str,
}

fn entry_kind(meta: &std::fs::Metadata) -> &'static str {
    let ft = meta.file_type();
    if ft.is_dir() {
        "dir"
    } else if ft.is_file() {
        "file"
    } else if ft.is_symlink() {
        "symlink"
    } else {
        "other"
    }
}

/// Directory listing, sorted dirs-first then by name (the shape the
/// dashboard's file browser renders).
pub fn list_dir(dir: &Path) -> std::io::Result<Vec<FileEntry>> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir)? {
        let e = e?;
        // symlink_metadata so a symlink is reported AS a symlink instead of
        // silently followed into whatever it points at.
        let Ok(meta) = e.path().symlink_metadata() else {
            continue;
        };
        out.push(FileEntry {
            name: e.file_name().to_string_lossy().into_owned(),
            size: meta.len(),
            mtime: meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0),
            kind: entry_kind(&meta),
        });
    }
    out.sort_by(|a, b| {
        (b.kind == "dir")
            .cmp(&(a.kind == "dir"))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok(out)
}

async fn list(Query(q): Query<PathParam>) -> Response {
    let root = files_root();
    let dir = match resolve_existing(&root, &q.path) {
        Ok(d) => d,
        Err(e) => return err(StatusCode::BAD_REQUEST, json!({ "error": e })),
    };
    if !dir.is_dir() {
        return err(
            StatusCode::BAD_REQUEST,
            json!({ "error": "not a directory", "path": q.path }),
        );
    }
    let entries = match crate::db::interactions::spawn_blocking(move || list_dir(&dir)).await {
        Ok(Ok(entries)) => entries,
        Ok(Err(e)) => {
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                json!({ "error": e.to_string() }),
            )
        }
        Err(e) => {
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                json!({ "error": e.to_string() }),
            )
        }
    };
    Json(json!({ "path": q.path, "entries": entries })).into_response()
}

async fn download(Query(q): Query<PathParam>) -> Response {
    let root = files_root();
    let file = match resolve_existing(&root, &q.path) {
        Ok(f) => f,
        Err(e) => return err(StatusCode::NOT_FOUND, json!({ "error": e })),
    };
    if !file.is_file() {
        return err(
            StatusCode::BAD_REQUEST,
            json!({ "error": "not a regular file", "path": q.path }),
        );
    }
    match file.metadata() {
        Ok(m) if m.len() > MAX_BYTES as u64 => {
            return err(
                StatusCode::PAYLOAD_TOO_LARGE,
                json!({
                    "error": format!(
                        "file is {} bytes; this endpoint buffers whole files and caps at {} bytes",
                        m.len(), MAX_BYTES
                    ),
                    "limit": MAX_BYTES,
                }),
            );
        }
        Err(e) => {
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                json!({ "error": e.to_string() }),
            )
        }
        _ => {}
    }
    let name = file
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "download".into());
    match tokio::fs::read(&file).await {
        Ok(bytes) => (
            [
                (header::CONTENT_TYPE, "application/octet-stream".to_string()),
                (
                    header::CONTENT_DISPOSITION,
                    format!("attachment; filename=\"{}\"", name.replace('"', "_")),
                ),
            ],
            bytes,
        )
            .into_response(),
        Err(e) => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            json!({ "error": e.to_string() }),
        ),
    }
}

/// A refusal decided at the write itself, carried out of `atomic_write_checked`.
#[derive(Debug)]
struct UploadRefused(&'static str, &'static str);

impl std::fmt::Display for UploadRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.1)
    }
}

impl std::error::Error for UploadRefused {}

/// What may sit at an upload's target when it is written: nothing, or a
/// regular file with one hard link and no execute bit. Never a symlink, which would be followed
/// or replaced on the caller's say-so (ai-config #227).
fn upload_target_check(at: Option<&std::fs::Metadata>) -> std::io::Result<()> {
    let refused = |code, error| Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied, UploadRefused(code, error)));
    let Some(m) = at else { return Ok(()) };
    if m.file_type().is_symlink() {
        return refused("upload_symlink_refused", "Uploads may not write through or replace a symbolic link.");
    }
    if !m.file_type().is_file() {
        return refused("upload_not_a_file_refused", "Uploads may only replace regular files.");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if m.nlink() > 1 {
            return refused("upload_hardlink_refused", "Uploads may not overwrite a file that has other hard links.");
        }
        if m.permissions().mode() & 0o111 != 0 {
            return refused("write_executable_refused", "Uploads may not overwrite existing executable files.");
        }
    }
    Ok(())
}

async fn upload(
    headers: axum::http::HeaderMap,
    Query(q): Query<PathParam>,
    body: Bytes,
) -> Response {
    upload_into(files_root(), &headers, &q, body).await
}

/// The upload itself, with the root passed in so tests never touch `$HOME`.
async fn upload_into(root: PathBuf, headers: &axum::http::HeaderMap, q: &PathParam, body: Bytes) -> Response {
    if body.len() > MAX_BYTES {
        return err(
            StatusCode::PAYLOAD_TOO_LARGE,
            json!({ "error": format!("body is {} bytes; upload cap is {} bytes", body.len(), MAX_BYTES), "limit": MAX_BYTES }),
        );
    }
    let rel = match validate_upload_rel(&q.path) {
        Ok(r) => r,
        Err(e) => return err(StatusCode::BAD_REQUEST, json!({ "error": e })),
    };
    let canon_root = match root.canonicalize() {
        Ok(r) => r,
        Err(e) => {
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                json!({ "error": e.to_string() }),
            )
        }
    };
    let target = canon_root.join(&rel);
    let refuse = |code: &'static str, error: &'static str| {
        let origin = crate::api::session_verbs::worker_origin(headers);
        tracing::warn!(target: "amux::files", path = ?rel, worker = ?origin,
            verdict = code, measured = true, n_considered = 1,
            "file upload refused: {error}");
        err(
            StatusCode::FORBIDDEN,
            json!({ "ok": false, "code": code, "error": error }),
        )
    };
    // Judge the path the write will really land on as well as the one asked
    // for: a symlinked directory in the existing prefix (`cfg` -> `.config`)
    // moves it where the lexical checks cannot see (ai-config #227). Resolved
    // before anything is created, so a refused upload creates nothing.
    let Some(name) = target.file_name().map(|n| n.to_os_string()) else {
        return err(StatusCode::BAD_REQUEST, json!({ "error": "upload path names no file" }));
    };
    let real_parent = crate::api::fs::resolve_nonstrict(target.parent().unwrap_or(&canon_root));
    let real_target = real_parent.join(&name);
    let Ok(real_rel) = real_target.strip_prefix(&canon_root).map(Path::to_path_buf) else {
        return err(
            StatusCode::BAD_REQUEST,
            json!({ "error": "upload path resolves outside the files root (symlink)" }),
        );
    };
    for r in [&rel, &real_rel] {
        if let Some((code, error)) = crate::api::fs::write_refusal(r, &target) {
            return refuse(code, error);
        }
    }
    let target = real_target;
    if let Some(parent) = target.parent() {
        if let Err(e) = tokio::fs::create_dir_all(parent).await {
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                json!({ "error": e.to_string() }),
            );
        }
        // Post-create containment check: the lexical filter above blocks
        // `..`, but a symlink INSIDE the root can still point outside it —
        // canonicalize the now-existing parent and re-verify.
        match parent.canonicalize() {
            Ok(real_parent) if real_parent.starts_with(&canon_root) => {}
            Ok(_) => {
                return err(
                    StatusCode::BAD_REQUEST,
                    json!({ "error": "upload path resolves outside the files root (symlink)" }),
                )
            }
            Err(e) => {
                return err(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    json!({ "error": e.to_string() }),
                )
            }
        }
    }
    let size = body.len();
    // Not `fs::write`: that followed a symlink AT the target, which the parent
    // check above never sees, and wrote outside the root (PR #48 round-2 B2,
    // ai-config #211). And the link and executable rules are judged on what
    // is really there at the write, not on the earlier path check, which a
    // swap in between would pass (ai-config #227).
    let wrote = tokio::task::spawn_blocking(move || {
        crate::atomic_write::atomic_write_checked(&target, &body, &upload_target_check)
    })
    .await
    .unwrap_or_else(|e| Err(std::io::Error::other(e)));
    if let Some(r) = wrote.as_ref().err().and_then(|e| e.get_ref()).and_then(|e| e.downcast_ref::<UploadRefused>()) {
        return refuse(r.0, r.1);
    }
    match wrote {
        Ok(_) => Json(json!({ "ok": true, "path": rel.display().to_string(), "size": size }))
            .into_response(),
        Err(e) => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            json!({ "error": e.to_string() }),
        ),
    }
}

// ---- tests ----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    /// Upload `body` to `rel` under `root`; the status and the JSON `code`.
    async fn put(root: &Path, rel: &str, body: &'static [u8]) -> (StatusCode, Value) {
        let r = upload_into(root.into(), &axum::http::HeaderMap::new(), &PathParam { path: rel.into() }, Bytes::from_static(body)).await;
        let status = r.status();
        let bytes = axum::body::to_bytes(r.into_body(), 4096).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn write_refusal_draw_rooted_upload_requires_both_paths_inside_draw() {
        let dir = root();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let home = root.join(".amux");
        let _guard = crate::api::settings::test_env::set_home(&home);
        let (status, body) = put(&root, ".amux/draw/drawings/x.excalidraw", b"new").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(std::fs::read(home.join("draw/drawings/x.excalidraw")).unwrap(), b"new");
        std::os::unix::fs::symlink(home.join("draw"), root.join("alias")).unwrap();
        let (status, body) = put(&root, "alias/outside.txt", b"bad").await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
        assert_eq!(body["code"], "write_dotfile_refused");
        assert!(!home.join("draw/outside.txt").exists());
    }

    /// PR #48 round-2 B2 and ai-config #227 case 4: only the parent was checked
    /// against the root, and the write followed a symlink at the target, so an
    /// upload to `a.txt` -> `<outside>/victim` wrote outside the root.
    #[cfg(unix)]
    #[tokio::test]
    async fn an_upload_never_writes_through_a_final_symlink_pointing_outside_the_root() {
        let dir = root();
        let outside = tempfile::tempdir().unwrap();
        let victim = outside.path().join("victim");
        std::fs::write(&victim, "keep").unwrap();
        std::os::unix::fs::symlink(&victim, dir.path().join("a.txt")).unwrap();
        let (status, v) = put(dir.path(), "a.txt", b"pwned").await;
        assert_eq!((status, v["code"].as_str()), (StatusCode::FORBIDDEN, Some("upload_symlink_refused")), "{v}");
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep", "nothing outside the root was written");
        assert!(std::fs::symlink_metadata(dir.path().join("a.txt")).unwrap().file_type().is_symlink());
    }

    /// PR #48 B1: a hard link shares the outside file's inode even though
    /// the upload path stays inside the root.
    #[cfg(unix)]
    #[tokio::test]
    async fn an_upload_never_writes_through_a_hard_link_pointing_outside_the_root() {
        let dir = root();
        let outside = tempfile::tempdir().unwrap();
        let victim = outside.path().join("victim");
        std::fs::write(&victim, "outside").unwrap();
        std::fs::hard_link(&victim, dir.path().join("safe.txt")).unwrap();
        let (status, v) = put(dir.path(), "safe.txt", b"evil").await;
        assert_eq!((status, v["code"].as_str()), (StatusCode::FORBIDDEN, Some("upload_hardlink_refused")), "{v}");
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "outside", "nothing outside the root was written");
    }

    /// ai-config #227 case 1: a link inside the root to a dotfile got 200 and
    /// overwrote the dotfile.
    #[cfg(unix)]
    #[tokio::test]
    async fn an_upload_never_writes_through_a_link_to_a_dotfile() {
        let dir = root();
        let gc = dir.path().join(".gitconfig");
        std::fs::write(&gc, "[user]").unwrap();
        std::os::unix::fs::symlink(&gc, dir.path().join("gc")).unwrap();
        let (status, v) = put(dir.path(), "gc", b"[core]\nhooksPath=/tmp/x").await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{v}");
        assert_eq!(std::fs::read_to_string(&gc).unwrap(), "[user]");
    }

    /// ai-config #227 case 2: a link to an executable got 200, overwrote it and
    /// left it executable.
    #[cfg(unix)]
    #[tokio::test]
    async fn an_upload_never_writes_through_a_link_to_an_executable() {
        use std::os::unix::fs::PermissionsExt;
        let dir = root();
        std::fs::create_dir(dir.path().join("tools")).unwrap();
        let exe = dir.path().join("tools/run.sh");
        std::fs::write(&exe, "#!/bin/sh\necho ok").unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::os::unix::fs::symlink(&exe, dir.path().join("run")).unwrap();
        let (status, v) = put(dir.path(), "run", b"#!/bin/sh\necho pwned").await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{v}");
        assert_eq!(std::fs::read_to_string(&exe).unwrap(), "#!/bin/sh\necho ok");
    }

    /// ai-config #227 case 3: a directory link into `.config` passed the
    /// lexical dotfile rule and the parent containment check (still inside
    /// the root) and wrote into the hidden directory.
    #[cfg(unix)]
    #[tokio::test]
    async fn an_upload_through_a_directory_link_is_judged_where_it_lands() {
        let dir = root();
        std::fs::create_dir(dir.path().join(".config")).unwrap();
        std::os::unix::fs::symlink(dir.path().join(".config"), dir.path().join("cfg")).unwrap();
        let (status, v) = put(dir.path(), "cfg/tool.toml", b"cmd = 'pwned'").await;
        assert_eq!((status, v["code"].as_str()), (StatusCode::FORBIDDEN, Some("write_dotfile_refused")), "{v}");
        assert!(!dir.path().join(".config/tool.toml").exists());
        let (status, v) = put(dir.path(), "cfg/new/tool.toml", b"x").await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{v}");
        assert!(!dir.path().join(".config/new").exists(), "a refused upload creates no directory");
    }

    /// The same rules, judged at the write: an executable that a path check
    /// would have seen is refused by the check on the opened file too.
    #[cfg(unix)]
    #[test]
    fn the_write_time_check_refuses_links_executables_and_non_files() {
        use std::os::unix::fs::PermissionsExt;
        let dir = root();
        let code = |p: &Path| {
            let e = crate::atomic_write::atomic_write_checked(p, b"x", &upload_target_check).unwrap_err();
            e.get_ref().and_then(|e| e.downcast_ref::<UploadRefused>()).map(|r| r.0)
        };
        let exe = dir.path().join("x.sh");
        std::fs::write(&exe, "#!/bin/sh").unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(code(&exe), Some("write_executable_refused"));
        std::os::unix::fs::symlink(&exe, dir.path().join("l")).unwrap();
        assert_eq!(code(&dir.path().join("l")), Some("upload_symlink_refused"));
        std::fs::create_dir(dir.path().join("d")).unwrap();
        assert_eq!(code(&dir.path().join("d")), Some("upload_not_a_file_refused"));
        let linked = dir.path().join("linked.txt");
        std::fs::write(&linked, "keep").unwrap();
        std::fs::hard_link(&linked, dir.path().join("other.txt")).unwrap();
        assert_eq!(code(&linked), Some("upload_hardlink_refused"));
        assert_eq!(std::fs::read_to_string(&linked).unwrap(), "keep");
        crate::atomic_write::atomic_write_checked(&dir.path().join("ok.txt"), b"x", &upload_target_check).unwrap();
    }

    #[test]
    fn traversal_escape_is_rejected() {
        let dir = root();
        std::fs::write(dir.path().join("inside.txt"), "ok").unwrap();

        // The classic ../ escape — must be rejected even though /etc exists.
        assert!(resolve_existing(dir.path(), "../../../../etc").is_err());
        assert!(resolve_existing(dir.path(), "../").is_err());
        // Absolute path outside the root: same check, same refusal.
        assert!(resolve_existing(dir.path(), "/etc/hosts").is_err());
        // Mixed: legit prefix, escaping suffix.
        assert!(resolve_existing(dir.path(), "a/../../outside").is_err());
        // Symlink inside the root pointing outside must not resolve.
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("/etc", dir.path().join("sneaky")).unwrap();
            assert!(
                resolve_existing(dir.path(), "sneaky/hosts").is_err(),
                "symlink escape"
            );
        }
        // ...while honest paths resolve.
        assert!(resolve_existing(dir.path(), "inside.txt").is_ok());
        assert!(
            resolve_existing(dir.path(), "").is_ok(),
            "empty path = root"
        );
    }

    #[test]
    fn upload_rel_validation_is_lexical() {
        assert!(validate_upload_rel("notes/a.txt").is_ok());
        assert!(validate_upload_rel("./a.txt").is_ok());
        assert!(validate_upload_rel("").is_err());
        assert!(validate_upload_rel("   ").is_err());
        assert!(validate_upload_rel("../x").is_err());
        assert!(validate_upload_rel("a/../../x").is_err());
        assert!(validate_upload_rel("/abs/path").is_err());
    }

    #[test]
    fn upload_refuses_dotfiles_and_dot_directories() {
        let dir = root();
        for rel in [
            ".bashrc",
            ".zshrc",
            ".gitconfig",
            ".ssh/authorized_keys",
            ".git/hooks/pre-commit",
            "notes/.envrc",
        ] {
            assert_eq!(
                crate::api::fs::write_refusal(Path::new(rel), &dir.path().join(rel)).map(|(code, _)| code),
                Some("write_dotfile_refused"),
                "{rel}"
            );
        }
    }

    #[test]
    fn upload_refuses_sensitive_paths() {
        // Pure path only: never create or write anything under the real HOME.
        let target = PathBuf::from(std::env::var("HOME").unwrap()).join(".ssh/x");
        assert!(!crate::api::fs::is_path_allowed(&target));
        assert_eq!(
            crate::api::fs::write_refusal(Path::new("x"), &target).map(|(code, _)| code),
            Some("write_path_not_allowed")
        );
    }

    #[test]
    fn upload_refuses_dangerous_write_targets() {
        let dir = root();
        for rel in ["Library/LaunchAgents/evil.plist", "bin/run", "x/evil.command"] {
            assert_eq!(
                crate::api::fs::write_refusal(Path::new(rel), &dir.path().join(rel)).map(|(code, _)| code),
                Some("write_dangerous_write_refused"),
                "{rel}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn upload_refuses_existing_executables() {
        use std::os::unix::fs::PermissionsExt;

        let dir = root();
        let target = dir.path().join("tool.sh");
        std::fs::write(&target, "echo test").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            crate::api::fs::write_refusal(Path::new("tool.sh"), &target).map(|(code, _)| code),
            Some("write_executable_refused")
        );
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(crate::api::fs::write_refusal(Path::new("tool.sh"), &target), None);
    }

    #[tokio::test]
    async fn the_endpoint_refuses_a_dotfile_without_creating_it_and_still_uploads_files() {
        let dir = root();
        let up = |path: &str| PathParam { path: path.into() };
        let h = axum::http::HeaderMap::new();
        let r = upload_into(dir.path().into(), &h, &up(".bashrc"), Bytes::from_static(b"evil")).await;
        assert_eq!(r.status(), StatusCode::FORBIDDEN);
        let body = axum::body::to_bytes(r.into_body(), 4096).await.unwrap();
        let v: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["code"], "write_dotfile_refused", "{v}");
        assert!(!dir.path().join(".bashrc").exists());
        let r = upload_into(dir.path().into(), &h, &up(".ssh/authorized_keys"), Bytes::from_static(b"k")).await;
        assert_eq!(r.status(), StatusCode::FORBIDDEN);
        assert!(!dir.path().join(".ssh").exists(), "a refused upload must not create its dot-directory");
        let r = upload_into(dir.path().into(), &h, &up("notes/a.txt"), Bytes::from_static(b"ok")).await;
        assert_eq!(r.status(), StatusCode::OK);
        assert_eq!(std::fs::read(dir.path().join("notes/a.txt")).unwrap(), b"ok");
    }

    #[test]
    fn upload_allows_ordinary_files() {
        let dir = root();
        for rel in ["notes/a.txt", "Documents/report.pdf"] {
            assert_eq!(
                crate::api::fs::write_refusal(Path::new(rel), &dir.path().join(rel)),
                None,
                "{rel}"
            );
        }
    }

    #[test]
    fn dir_listing_shape() {
        let dir = root();
        std::fs::write(dir.path().join("b.txt"), "12345").unwrap();
        std::fs::create_dir(dir.path().join("adir")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("b.txt", dir.path().join("link")).unwrap();

        let entries = list_dir(dir.path()).unwrap();
        // dirs first, then names
        assert_eq!(entries[0].name, "adir");
        assert_eq!(entries[0].kind, "dir");
        let file = entries.iter().find(|e| e.name == "b.txt").unwrap();
        assert_eq!(file.kind, "file");
        assert_eq!(file.size, 5);
        assert!(file.mtime > 0, "mtime populated");
        #[cfg(unix)]
        {
            let link = entries.iter().find(|e| e.name == "link").unwrap();
            assert_eq!(
                link.kind, "symlink",
                "symlinks reported as symlinks, not followed"
            );
        }
    }
}
