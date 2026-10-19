//! Source audit: nothing a test, example, script or fixture starts may listen on
//! a wildcard address, because a freshly built binary doing so raises the macOS
//! incoming-connections dialog. Fail-closed on the ADDRESS, not on function
//! names: every wildcard literal and every empty-host listen form is a finding
//! unless an ALLOW entry names it with a reason. Computed addresses (arithmetic,
//! values read at run time) are outside a lexical audit and still need review.

use std::path::{Path, PathBuf};

/// (repo-relative path, text on the finding's line, reason). Each entry must
/// match at least one finding, so a stale entry fails like a new finding does.
const ALLOW: &[(&str, &str, &str)] = &[
    ("crates/amux-server/src/lib.rs", "([0, 0, 0, 0], cfg.port)",
     "the production listener: reachable over Tailscale; AMUX_BIND (PR #30) makes it configurable"),
    ("crates/amux-server/src/api/process_identity.rs", "server listens on 0.0.0.0",
     "doc comment describing the production listener"),
    ("crates/amux-server/src/tls.rs", "on a 0.0.0.0 listener",
     "comment describing the production listener"),
    ("crates/amux-server/src/api/mdai.rs", "fan_in(serial_dir.path(), &[0, 0, 0, 0], None)",
     "four zero counters, not an address"),
    ("crates/amux-server/src/integrations/computer.rs", "x.starts_with(\"0.0.0.0\")",
     "reads a Docker port mapping; binds nothing"),
    ("crates/amux-server/src/api/proxies.rs", "validate(\"\", 8080)",
     "a proxy-config validation case with an empty host; binds nothing"),
    ("scripts/amux-playwright-mcp.sh", "--host 0.0.0.0",
     "Linux systemd Playwright MCP wrapper; localhost resolves IPv6-only there (see CLAUDE.md); never runs on macOS"),
    ("scripts/rebuild-rust-base.sh", "never 0.0.0.0",
     "comment"),
    ("e2e/operands-core.test.mjs", "matchesPath('::', ['/'])",
     "a path string, not an address"),
    // Live-endpoint guards: 0.0.0.0 is a client URL host they must refuse, not a bind.
    ("crates/amux-cli/src/main.rs", "\"LOCALHOST\", \"0.0.0.0\", \"127.1\"",
     "test input: host spellings the live-endpoint guard refuses"),
    ("scripts/lib/amux-endpoint.sh", "(the server binds 0.0.0.0)",
     "comment"),
    ("scripts/lib/amux_endpoint.py", "0.0.0.0, so any host on a live port",
     "docstring"),
    ("scripts/lib/amux_endpoint.py", "LOCALHOST, 0.0.0.0, 127.1,",
     "docstring"),
    ("scripts/test-cli-isolated-endpoint.sh", "http://0.0.0.0:8824",
     "client URL the guard must refuse"),
    ("scripts/test-endpoint-clients-isolated.py", "\"http://0.0.0.0:8824\",",
     "client URL the guard must refuse"),
    ("scripts/test-endpoint-clients-isolated.py", "\"http://0.0.0.0:0%d/x\"",
     "client URL the guard must refuse"),
];

const WILDCARD: &[&str] = &[
    r"0\.0\.0\.0",
    r"\[::\]",
    r#"["']::["']\s*,"#,
    r"\[\s*0\s*,\s*0\s*,\s*0\s*,\s*0\s*\]",
    // Only in a (address, port) tuple: a bare [0; 16] is usually a zeroed buffer.
    r"\(\s*\[\s*0\s*;\s*(?:4|16)\s*\]\s*,",
    r"Ipv[46]Addr::UNSPECIFIED",
    r"Ipv4Addr::new\(\s*0\s*,\s*0\s*,\s*0\s*,\s*0\s*\)",
    r"Ipv6Addr::new\(\s*0\s*(?:,\s*0\s*){7}\)",
    r"(?:Ipv[46]Addr|IpAddr)::from\(\s*0(?:u32|u128)?\s*\)",
    // Empty host with a port: Python ("", port), Rust ("", 0).
    r#"(?i)\(\s*(?:""|'')\s*,\s*(?:\d+|[\w.]*port\w*)\s*\)"#,
];

fn wildcard() -> &'static [regex::Regex] {
    static RE: std::sync::OnceLock<Vec<regex::Regex>> = std::sync::OnceLock::new();
    RE.get_or_init(|| WILDCARD.iter().map(|p| regex::Regex::new(p).unwrap()).collect())
}

fn netcat_listen() -> &'static regex::Regex {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"\bnc\b.*\s-l").unwrap())
}

/// A JS/TS `.listen(` whose host argument is missing listens on every interface.
/// Accept only a loopback literal or a host-named expression as the second
/// argument, or a `host:` key in an options object.
fn js_listen_without_host(line: &str) -> bool {
    let mut rest = line;
    while let Some(i) = rest.find(".listen(") {
        let args = &rest[i + ".listen(".len()..];
        let (mut depth, mut end) = (0i32, args.len());
        for (j, c) in args.char_indices() {
            match c {
                '(' | '[' | '{' => depth += 1,
                ')' | ']' | '}' if depth == 0 => { end = j; break; }
                ')' | ']' | '}' => depth -= 1,
                _ => {}
            }
        }
        let call = &args[..end];
        let host = if call.trim_start().starts_with('{') {
            call.contains("host")
        } else {
            let mut parts = call.splitn(3, ',');
            parts.next();
            parts.next().is_some_and(|h| {
                let h = h.trim();
                h.contains("127.0.0.1") || h.contains("'::1'") || h.contains("\"::1\"")
                    || h.contains("localhost") || h.to_ascii_lowercase().contains("host")
            })
        };
        if !host {
            return true;
        }
        rest = &args[end..];
    }
    false
}

/// Shell listeners whose default is every interface.
fn shell_listener_without_host(line: &str) -> bool {
    let loopback = line.contains("127.0.0.1") || line.contains("localhost");
    (line.contains("http.server") && line.contains("-m") && !line.contains("--bind") && !line.contains(" -b "))
        || (line.contains("TCP-LISTEN:") && !line.contains("bind="))
        || (netcat_listen().is_match(line) && !loopback)
}

fn scan(path: &str, src: &str) -> Vec<(usize, String)> {
    let wildcard = wildcard();
    let ext = path.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
    let js = matches!(ext, "js" | "mjs" | "cjs" | "ts" | "tsx");
    let shell = matches!(ext, "sh" | "bash") || src.starts_with("#!/bin/") || src.starts_with("#!/usr/bin/env bash");
    src.lines()
        .enumerate()
        .filter(|(_, line)| {
            wildcard.iter().any(|r| r.is_match(line))
                || (js && js_listen_without_host(line))
                || (shell && shell_listener_without_host(line))
        })
        .map(|(n, line)| (n + 1, line.trim().to_string()))
        .collect()
}

const EXTS: &[&str] = &["rs", "js", "mjs", "cjs", "ts", "tsx", "py", "sh", "bash"];
const SKIP_DIRS: &[&str] = &[".git", "target", "node_modules", ".claude", ".worktrees", "dist", "static"];

/// Tests, examples, scripts, fixtures and server sources. Production docs, the
/// site and the cloud gateway are not started by tests and are out of scope.
fn in_scope(rel: &str, src: &str) -> bool {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    let ext = name.rsplit_once('.').map(|(_, e)| e);
    let code = ext.is_some_and(|e| EXTS.contains(&e))
        || (ext.is_none() && rel.starts_with("scripts/") && src.starts_with("#!"));
    let place = ["scripts/", "tests/", "e2e/"].iter().any(|p| rel.starts_with(p))
        || ["/tests/", "/examples/", "/benches/"].iter().any(|p| rel.contains(p))
        || (rel.starts_with("crates/") && rel.contains("/src/"))
        || ["test", "spec", "fixture"].iter().any(|w| name.contains(w));
    code && place
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<(String, String)>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if path.is_dir() {
            if !SKIP_DIRS.contains(&name.as_str()) {
                walk(root, &path, out);
            }
            continue;
        }
        let rel = path.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
        let Ok(src) = std::fs::read_to_string(&path) else { continue };
        if in_scope(&rel, &src) {
            out.push((rel, src));
        }
    }
}

#[test]
fn no_wildcard_listener_outside_the_allow_list() {
    let root: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap();
    let mut files = Vec::new();
    walk(&root, &root, &mut files);
    files.sort();
    // The audit's own fixtures are the patterns it detects.
    files.retain(|(rel, _)| rel != "crates/amux-server/tests/no_wildcard_bind.rs");
    assert!(files.len() > 100, "audit saw only {} files; scope is broken", files.len());
    let mut used = vec![0usize; ALLOW.len()];
    let mut bad = Vec::new();
    for (rel, src) in &files {
        for (line, text) in scan(rel, src) {
            match ALLOW.iter().position(|(p, needle, _)| p == rel && text.contains(needle)) {
                Some(i) => used[i] += 1,
                None => bad.push(format!("{rel}:{line}: {text}")),
            }
        }
    }
    let stale: Vec<_> = ALLOW.iter().zip(&used).filter(|(_, n)| **n == 0).map(|(a, _)| format!("{}: {}", a.0, a.1)).collect();
    assert!(stale.is_empty() && bad.is_empty(),
        "wildcard listeners can raise the macOS firewall dialog; bind 127.0.0.1 or add a reasoned ALLOW entry:\n{}\n\
         allow-list entries that match nothing (remove them):\n{}",
        bad.join("\n"), stale.join("\n"));
}

#[test]
fn every_wildcard_form_is_detected_in_every_language() {
    let cases = [
        ("a.rs", r#"TcpListener::bind("0.0.0.0:0")"#),
        ("a.rs", r#"TcpListener::bind("[::]:0")"#),
        ("a.rs", r#"TcpListener::bind(("::", 0))"#),
        ("a.rs", "axum_server::bind_rustls(([0, 0, 0, 0], port).into(), tls)"),
        ("a.rs", "SocketAddr::from(([0; 4], 0))"),
        ("a.rs", "(Ipv4Addr::UNSPECIFIED, 0)"),
        ("a.rs", "(Ipv6Addr::UNSPECIFIED, 0)"),
        ("a.rs", "Ipv4Addr::new(0, 0, 0, 0)"),
        ("a.rs", "IpAddr::from(0u32)"),
        ("a.rs", r#"let addr = "0.0.0.0:0"; // later bound"#),
        ("a.mjs", "site.listen(0,'0.0.0.0',r)"),
        ("a.mjs", "site.listen(0, r)"),
        ("a.ts", "server.listen(port)"),
        ("a.mjs", "server.listen({ port: 0 })"),
        ("a.mjs", "server.listen(0, '::', cb)"),
        ("a.py", "socket.bind((\"\", 8080))"),
        ("a.py", "HTTPServer(('', port), H)"),
        ("a.py", "HTTPServer(('0.0.0.0', args.port), H)"),
        ("a.sh", "python3 -m http.server 8000"),
        ("a.sh", "socat TCP-LISTEN:9000,fork -"),
        ("a.sh", "nc -l 9000"),
    ];
    for (path, line) in cases {
        assert_eq!(scan(path, line).len(), 1, "{path}: not detected: {line}");
    }
}

#[test]
fn loopback_and_non_address_text_pass() {
    let cases = [
        ("a.rs", r#"TcpListener::bind("127.0.0.1:0")"#),
        ("a.rs", r#"TcpListener::bind("[::1]:0")"#),
        ("a.rs", r#"callee.split("::").collect()"#),
        ("a.rs", "let bytes: [u8; 16] = hash[..16].try_into().unwrap_or([0; 16]);"),
        ("a.rs", r#"if owner == "" { return; }"#),
        ("a.mjs", "site.listen(0, '127.0.0.1', r)"),
        ("a.mjs", "site.listen(0, process.env.AMUX_ROUTING_FIXTURE_HOST || '127.0.0.1', r)"),
        ("a.mjs", "server.listen(PORT, HOST, () => {})"),
        ("a.mjs", "server.listen({ port: 0, host: '127.0.0.1' })"),
        ("a.mjs", "parts.join('::')"),
        ("a.py", "owner in (\"\", \"(unknown)\")"),
        ("a.py", "ThreadingHTTPServer(('127.0.0.1', args.port), H)"),
        ("a.sh", "python3 -m http.server 8000 --bind 127.0.0.1"),
        ("a.sh", "socat TCP-LISTEN:9000,bind=127.0.0.1 -"),
        ("a.sh", "nc -l 127.0.0.1 9000"),
    ];
    for (path, line) in cases {
        assert!(scan(path, line).is_empty(), "{path}: false positive: {line}");
    }
}

#[test]
fn scope_covers_every_language_and_place() {
    for rel in ["tests/x.mjs", "e2e/x.spec.ts", "scripts/x.py", "scripts/x.sh",
        "crates/amux-server/examples/x.rs", "crates/amux-server/tests/x.rs",
        "crates/amux-server/src/api/x.rs", "business-ui/tests/fixture-server.mjs", "cloud/x_test.py"] {
        assert!(in_scope(rel, ""), "{rel} must be audited");
    }
    assert!(in_scope("scripts/worker-bin/tmux", "#!/bin/bash\n"));
    for rel in ["site/guides/x.md", "cloud/gateway/gateway.py", "README.md"] {
        assert!(!in_scope(rel, ""), "{rel} is out of scope");
    }
}
