//! A unit test in `src/` may never talk to the default tmux server.
//!
//! The 2026-10-17 outage was a test killing the live tmux server, so every tmux
//! call the server makes in a test build goes to a private per-process socket
//! (`run_cmd`, `tmux_command()`, `tmux_command_async()` in api/session_verbs.rs).
//! A test that spells `Command::new("tmux")` itself bypasses that: on
//! 2026-10-19 eight sites in session_verbs.rs's tests did, three of them
//! creating and `kill-session`-ing sessions on whatever server the default
//! socket names, which from a worker pane is the live fleet's.
//!
//! So: inside a `#[cfg(test)] mod`, a hand-built tmux command must name its own
//! socket in the same statement (`-S`, `-L`, or a `TMUX_TMPDIR` env) or only ask
//! `-V`. Anything else goes through the helpers.

use std::path::{Path, PathBuf};

fn rust_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            rust_sources(&p, out);
        } else if p.extension().and_then(|s| s.to_str()) == Some("rs") {
            out.push(p);
        }
    }
}

/// Byte ranges of every `#[cfg(test)] mod name { ... }` body. Braces inside
/// strings, raw strings, char literals and comments are skipped.
fn test_mod_ranges(src: &str) -> Vec<(usize, usize)> {
    let b = src.as_bytes();
    let mut ranges = Vec::new();
    let mut from = 0;
    while let Some(rel) = src[from..].find("#[cfg(test)]") {
        let at = from + rel;
        from = at + 1;
        let rest = &src[at + "#[cfg(test)]".len()..];
        let trimmed = rest.trim_start();
        let after_attrs = skip_attributes(trimmed);
        let after_pub = after_attrs.strip_prefix("pub(crate) ").unwrap_or(after_attrs);
        let Some(m) = after_pub.strip_prefix("mod ") else { continue };
        let Some(open_rel) = m.find('{') else { continue };
        if m[..open_rel].contains(';') {
            continue;
        }
        let open = src.len() - m.len() + open_rel;
        if let Some(close) = matching_brace(b, open) {
            ranges.push((open, close));
            from = close;
        }
    }
    ranges
}

fn skip_attributes(mut s: &str) -> &str {
    while s.starts_with("#[") {
        match s.find(']') {
            Some(i) => s = s[i + 1..].trim_start(),
            None => break,
        }
    }
    s
}

fn matching_brace(b: &[u8], open: usize) -> Option<usize> {
    let mut depth = 0i32;
    let mut i = open;
    while i < b.len() {
        match b[i] {
            b'/' if b.get(i + 1) == Some(&b'/') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                    i += 1;
                }
                i += 1;
            }
            b'r' if matches!(b.get(i + 1), Some(b'#') | Some(b'"')) && !is_ident(b.get(i.wrapping_sub(1))) => {
                let mut j = i + 1;
                let mut hashes = 0;
                while b.get(j) == Some(&b'#') {
                    hashes += 1;
                    j += 1;
                }
                if b.get(j) != Some(&b'"') {
                    i += 1;
                    continue;
                }
                j += 1;
                loop {
                    if j >= b.len() {
                        return None;
                    }
                    if b[j] == b'"' && (0..hashes).all(|k| b.get(j + 1 + k) == Some(&b'#')) {
                        i = j + hashes;
                        break;
                    }
                    j += 1;
                }
            }
            b'"' => {
                i += 1;
                while i < b.len() && b[i] != b'"' {
                    if b[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
            }
            // A char literal ('x', '\n', '\''), not a lifetime ('a).
            b'\'' if b.get(i + 2) == Some(&b'\'') || b.get(i + 1) == Some(&b'\\') => {
                i += 1;
                while i < b.len() && b[i] != b'\'' {
                    if b[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
            }
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

fn is_ident(c: Option<&u8>) -> bool {
    matches!(c, Some(c) if c.is_ascii_alphanumeric() || *c == b'_')
}

/// Every hand-built tmux command in a test module that names no private socket.
fn offenders_in(src: &str) -> Vec<usize> {
    let mut bad = Vec::new();
    for (start, end) in test_mod_ranges(src) {
        let body = &src[start..end];
        let mut from = 0;
        while let Some(rel) = body[from..].find("Command::new(\"tmux\")") {
            let at = from + rel;
            from = at + 1;
            let stmt_end = body[at..].find(';').map_or(body.len(), |i| at + i);
            let stmt = &body[at..stmt_end];
            let private = ["\"-S\"", "\"-L\"", "\"-V\"", "TMUX_TMPDIR"].iter().any(|k| stmt.contains(k));
            if !private {
                bad.push(start + at);
            }
        }
    }
    bad
}

#[test]
fn no_test_module_talks_to_the_default_tmux_server() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_sources(&root, &mut files);
    assert!(!files.is_empty(), "found no sources under {}", root.display());
    let mut bad = Vec::new();
    let mut test_mods = 0;
    for f in files {
        let src = std::fs::read_to_string(&f).unwrap_or_default();
        test_mods += test_mod_ranges(&src).len();
        for at in offenders_in(&src) {
            let line = src[..at].matches('\n').count() + 1;
            bad.push(format!("{}:{line}", f.display()));
        }
    }
    assert!(test_mods > 50, "the scanner found only {test_mods} test modules; it has stopped seeing them");
    assert!(
        bad.is_empty(),
        "a test builds a tmux command against the DEFAULT server; use tmux_command() / \
         tmux_command_async() (private socket) or name a socket with -S/-L:\n{}",
        bad.join("\n")
    );
}

/// The scanner must find a planted offender inside a test module, and leave
/// production code and socket-scoped test calls alone.
#[test]
fn the_audit_flags_a_planted_default_server_call() {
    let planted = r#"
fn prod() { let _ = std::process::Command::new("tmux").args(["kill-session", "-t", &st]).output(); }
#[cfg(test)]
mod tests {
    fn s() -> &'static str { "}" }
    #[test]
    fn bad() {
        let _ = std::process::Command::new("tmux").args(["kill-session", "-t", &st]).output();
        let _ = std::process::Command::new("tmux").arg("-S").arg(&sock).arg("kill-server").output();
        let _ = std::process::Command::new("tmux").arg("-V").output();
        let c = '{';
    }
}
"#;
    let found = offenders_in(planted);
    assert_eq!(found.len(), 1, "exactly the bare kill-session in the test module; got {found:?}");
    assert!(planted[found[0]..].starts_with("Command::new(\"tmux\").args([\"kill-session\""));
}
