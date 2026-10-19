//! A hardcoded `…:8822` must not come back (2026-08-10).
//!
//! 8824 is the address. 8822 is the RETIRED one, answered only by a countdown
//! compatibility bind for sessions started before the cutover, whose process
//! env cannot be rotated (`amux_server::legacy_port`, and the exit condition in
//! docs/rust-migration/server-boundary.md).
//!
//! Retiring it took ~180 edits across two CLIs, the dashboard, the scripts, the
//! e2e configs, the docs and the fleet's agent instructions — because a port
//! literal is the cheapest thing in the world to type and nothing was watching.
//! One `format!("...localhost:8822")` in `session_verbs.rs` was re-minting the
//! retired address into every newly-spawned session's env, which is the whole
//! reason the population never shrank; the cloud image's Dockerfile then had to
//! bind 8822 *to match that literal*, and said so in a comment. A single
//! hardcoded port had pinned two deployments.
//!
//! So this is the tmux-target-audit shape: enumerate the tree, fail on the
//! literal, and keep an allowlist that has to state a REASON per entry. The
//! allowlist is the interesting part — every row is something that legitimately
//! names the old port, and a row with no reason is how the guard rots into a
//! rubber stamp.
//!
//! # What counts as a violation
//!
//! An ADDRESS: `:8822` immediately preceded by a host token
//! (`localhost:8822`, `your-server:8822`, `127.0.0.1:8822`, `foo.ts.net:8822`).
//!
//! NOT prose: "the Python server on :8822", "answers 8822 and 8824". Narrative
//! about the migration is history and teaches nobody to dial it; an address is
//! a thing a client will actually use. Guarding the prose too would mean every
//! historical comment needed an allowlist row, the allowlist would become
//! noise, and noise is how a guard stops being read.

use std::path::{Path, PathBuf};

/// Files permitted to contain a literal legacy address, each with the reason.
/// When the bind is finally dropped, most of these disappear with it.
///
/// THE BIND WAS DROPPED 2026-08-11 (Ethan: "no more 8822 just rust"), and
/// lib.rs's row went with it — exactly as this comment predicted. The mirror
/// check below is what forced it: it failed with "allowlisted but no longer
/// contains a legacy address — delete the row so the file is guarded again",
/// which is the whole point of checking an allowlist in both directions. A
/// stale exemption is a file quietly outside the guard.
const ALLOW: &[(&str, &str)] = &[
    // -- the dual-bind implementation itself --
    (
        "crates/amux-server/src/legacy_port.rs",
        "the retirement instrument; it exists to talk about this port",
    ),
    (
        "crates/amux-server/src/config.rs",
        "canonical_port()'s doc, which names the literal it replaced",
    ),
    (
        "crates/amux-server/tests/legacy_port_guard.rs",
        "this guard — it must contain the pattern it searches for",
    ),
    (
        "scripts/test-endpoint-clients-isolated.py",
        "the isolated-run endpoint guard's test: its allowlist quotes docstring prose that names the retired address, and it probes that every guard refuses it",
    ),
    // -- prose ABOUT the retired port, in code that does not USE it --
    //
    // Both of these arrived on main already red (2026-08-10) and made
    // `cargo test -p amux-server` fail for every lane, which is the shape this
    // guard is least entitled to have: it polices a real hazard and it caught
    // two comments. Allowlisted rather than reworded because the sentences are
    // load-bearing PROVENANCE — each explains why a pre-cutover client still
    // arrives at 8822, and deleting the number from the explanation is how the
    // next person re-derives it from scratch.
    (
        "crates/amux-server/src/api/static_files.rs",
        "the PWA install-origin comment: records WHERE the phone installed from, a historical \
         fact, not an address this code dials",
    ),
    (
        "scripts/git-hooks/amux-staged-guard",
        "explains why pre-cutover sessions carry AMUX_URL=https://localhost:8822 in their \
         process env — the hook itself resolves the URL, it does not hardcode one",
    ),
    (
        "scripts/git-hooks/git-shared-guard.py",
        "same docstring as amux-staged-guard, in the versioned copy of the shared-guard hook \
         (AMUX-3033): it explains why pre-cutover sessions carry AMUX_URL=https://localhost:8822 \
         and self-heals it, hardcoding nothing. Must stay byte-identical to the runtime hook — \
         the AMUX-3033 drift invariant shas them — so it cannot be reworded, only allowlisted",
    ),
    (
        "scripts/test-amux-url.sh",
        "feeds a retired https://localhost:8822 to `amux url` as TEST INPUT, to prove the \
         resolver ignores the dead port and returns the canonical one; the literal is the \
         subject under test (AMUX-3046), not an address this script dials",
    ),
    // NOTE: crates/amux-cli/tests/url_resolution.rs is deliberately NOT here.
    // It contains `!err.contains(":8822")`, where the quote before the colon
    // means it is not an address — so it needs no exemption. The first draft
    // allowlisted it anyway and `allowlist_rows_are_live_and_reasoned` caught
    // the row as dead, which is that test earning its keep on day one.
    // NOTE: crates/amux-server/src/api/gmail_auth.rs was allowlisted here while
    // GMAIL_REDIRECT_URI hardcoded the retired address. AMUX-3026 replaced that
    // constant with gmail_redirect_uri(), which derives the callback from
    // canonical_port() (overridable via the GMAIL_REDIRECT_URI env), so the
    // literal is gone and the row went with it. The console re-registration the
    // old reason described is now the OWNER step handed off on AMUX-3026, not a
    // literal frozen into the tree.
    // NOTE: crates/amux-server/src/api/py_proxy.rs was allowlisted here for its
    // `AMUX_PY_URL` fallback, with the reasoning "this default is unreachable;
    // it dies with the module". It did — AMUX-2906 deleted the forwarder, so
    // the literal is gone and this row went with it. That is
    // `allowlist_rows_are_live_and_reasoned` doing its job a second time: the
    // row could not outlive the code it excused.
    // -- historical / dead code --
    (
        "crates/amux-server/src/api/request_log.rs",
        "provenance comments recording the URL a golden fixture was captured from — \
         changing them would falsify where the recorded bytes came from",
    ),
    (
        "crates/amux-dashboard/static/app.js",
        "one historical comment about self-signed origins (mirrors offline_origin.rs)",
    ),
];

/// Directories scanned. Deliberately the trees that produce BEHAVIOUR — code,
/// scripts and e2e — rather than `docs/` and `site/`, where the old port is
/// legitimately part of the migration record.
const ROOTS: &[&str] = &["crates", "scripts", "e2e", "tests"];

/// Skipped wholesale: build output and vendored trees are not authored here.
fn is_skipped_dir(name: &str) -> bool {
    matches!(name, "target" | "node_modules" | ".git" | "test-results")
}

/// Only text we would author. A `.png` containing the byte pattern is noise.
fn is_scanned_file(p: &Path) -> bool {
    match p.extension().and_then(|e| e.to_str()) {
        Some(e) => matches!(
            e,
            "rs" | "js" | "mjs" | "cjs" | "ts" | "tsx" | "sh" | "py" | "html" | "json" | "toml"
        ),
        // Extensionless files in scripts/ are hooks and helpers — exactly the
        // kind of thing that carries a client default (scripts/git-hooks/*).
        None => true,
    }
}

/// Is this `:8822` an ADDRESS (host immediately before the colon) or prose?
///
/// The whole discrimination of this guard lives here, so it is exercised
/// directly by [`the_detector_can_actually_fail`] rather than only through the
/// tree walk — a walk over a clean tree passes whether or not the detector
/// works, which is the definition of a check that cannot fail.
fn address_hits(text: &str) -> Vec<usize> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(rel) = text[from..].find(":8822") {
        let at = from + rel;
        from = at + 5;
        // The char AFTER must not be another digit, or this is `:88220`.
        if let Some(&next) = bytes.get(at + 5) {
            if next.is_ascii_digit() {
                continue;
            }
        }
        // A host token ends in alphanumeric, `.` or `-`.
        let Some(&prev) = at.checked_sub(1).and_then(|i| bytes.get(i)) else {
            continue;
        };
        if prev.is_ascii_alphanumeric() || prev == b'.' || prev == b'-' {
            out.push(at);
        }
    }
    out
}

fn workspace_root() -> PathBuf {
    // crates/amux-server -> up two.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root")
        .to_path_buf()
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().into_owned();
        if p.is_dir() {
            if !is_skipped_dir(&name) {
                walk(&p, out);
            }
        } else if is_scanned_file(&p) {
            out.push(p);
        }
    }
}

#[test]
fn no_hardcoded_legacy_port_address_in_code_scripts_or_e2e() {
    let root = workspace_root();
    let mut files = Vec::new();
    for r in ROOTS {
        let d = root.join(r);
        if d.is_dir() {
            walk(&d, &mut files);
        }
    }
    assert!(
        files.len() > 50,
        "the walk found only {} files — the scan is broken, not the tree clean. \
         (A guard that silently searches nothing is the failure mode this asserts against.)",
        files.len()
    );

    let mut violations = Vec::new();
    for f in &files {
        let rel = f
            .strip_prefix(&root)
            .unwrap_or(f)
            .to_string_lossy()
            .replace('\\', "/");
        if ALLOW.iter().any(|(a, _)| *a == rel) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(f) else {
            continue;
        };
        for at in address_hits(&text) {
            let line_no = text[..at].matches('\n').count() + 1;
            let line = text.lines().nth(line_no - 1).unwrap_or("").trim();
            violations.push(format!("  {rel}:{line_no}  {line}"));
        }
    }

    assert!(
        violations.is_empty(),
        "\nHardcoded legacy port address (:8822) found in {} place(s):\n{}\n\n\
         8822 is RETIRED. Use 8824 — or better, do not hardcode a port at all:\n\
           * Rust server side ....... crate::config::canonical_port()\n\
           * Rust CLI ............... $AMUX_URL / --url (DEFAULT_CLIENT_URL is the last resort)\n\
           * shell / curl recipes ... \"$AMUX_URL\"\n\
           * node / e2e ............. process.env.AMUX_URL\n\n\
         The 8822 bind is a countdown for pre-cutover processes whose env cannot be\n\
         rotated, not an address to build on. See docs/rust-migration/server-boundary.md\n\
         for the exit condition, and GET /api/debug/legacy-port for who still calls it.\n\n\
         If a literal is genuinely required (an externally-registered URL, a golden\n\
         fixture's provenance), add it to ALLOW in this file WITH THE REASON.\n",
        violations.len(),
        violations.join("\n")
    );
}

/// Ethos rule 7: a guard that cannot fail is theatre, and a tree-walk over an
/// already-clean tree passes identically whether the detector works or is
/// `return vec![]`. So drive the detector directly, on both sides of the line
/// it draws — including the near-misses that decide whether the allowlist stays
/// short enough to be read.
#[test]
fn the_detector_can_actually_fail() {
    // POSITIVE: constructed first, per "say what a positive would look like and
    // confirm the probe can produce it".
    for bad in [
        r#"const URL = "https://localhost:8822";"#,
        r#"AMUX_API="${AMUX_API:-https://localhost:8822}""#,
        "curl -sk https://127.0.0.1:8822/api/board",
        "open https://your-server:8822 in Safari",
        "https://desktop.tail5ce8f5.ts.net:8822/health",
        r#"format!("AMUX_URL={scheme}://localhost:8822")"#,
    ] {
        assert_eq!(
            address_hits(bad).len(),
            1,
            "must flag an address literal: {bad}"
        );
    }

    // NEGATIVE: migration narrative, and the neighbouring port.
    for ok in [
        "//! The Python server on :8822 keeps running and OWNS schedule firing.",
        "the same binary answers 8822 and 8824",
        "a live Python server on :8822). The",
        "https://localhost:8824/api/board",
        "port 8822 was retired", // no colon-adjacent host
    ] {
        assert!(
            address_hits(ok).is_empty(),
            "must NOT flag migration prose: {ok}"
        );
    }

    // Near-miss that would make the guard cry wolf on unrelated numbers.
    assert!(
        address_hits("localhost:88220").is_empty(),
        ":88220 is a different port"
    );

    // And the walk itself must reach real files, or the main test is vacuous
    // for a reason unrelated to the detector.
    let root = workspace_root();
    let mut files = Vec::new();
    walk(&root.join("crates"), &mut files);
    assert!(
        files.iter().any(|f| f.ends_with("legacy_port.rs")),
        "the walk must reach crates/amux-server/src/legacy_port.rs"
    );
}

/// Every allowlist row must name a file that exists AND still contain the
/// pattern. A stale row is how an allowlist quietly becomes permission to
/// reintroduce the literal in a file nobody re-checks.
#[test]
fn allowlist_rows_are_live_and_reasoned() {
    let root = workspace_root();
    for (rel, reason) in ALLOW {
        let p = root.join(rel);
        assert!(p.is_file(), "allowlisted file no longer exists: {rel}");
        assert!(
            reason.len() > 20,
            "allowlist row {rel} needs a real reason, got {reason:?}"
        );
        let text = std::fs::read_to_string(&p).unwrap_or_default();
        assert!(
            !address_hits(&text).is_empty(),
            "{rel} is allowlisted but no longer contains a legacy address — \
             delete the row so the file is guarded again"
        );
    }
}
