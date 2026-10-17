//! Source audit: test binaries must not raise the macOS incoming-connections
//! dialog. Scan Rust bind/listen arguments, including local address aliases;
//! only the production HTTP listener may deliberately accept all interfaces.
//! This is a lexical guard, not whole-program dataflow: computed addresses and
//! parameters still need review (notably libc sockaddr construction).

use std::path::{Path, PathBuf};

#[derive(Debug)]
struct Token<'a> {
    text: &'a str,
    at: usize,
}

// Keep strings opaque (including raw fixture strings), discard nested comments,
// and preserve byte offsets so every diagnostic points at the actual call.
fn tokens(src: &str) -> Vec<Token<'_>> {
    let b = src.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let start = i;
        if b[i].is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if b[i..].starts_with(b"//") {
            i += 2;
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if b[i..].starts_with(b"/*") {
            i += 2;
            let mut depth = 1;
            while i < b.len() && depth > 0 {
                if b[i..].starts_with(b"/*") {
                    depth += 1;
                    i += 2;
                } else if b[i..].starts_with(b"*/") {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
            assert_eq!(depth, 0, "unterminated source comment at {start}");
            continue;
        }
        let mut quote = i;
        if b[i] == b'r' {
            quote += 1;
            while quote < b.len() && b[quote] == b'#' {
                quote += 1;
            }
        }
        if b.get(quote) == Some(&b'"') {
            if quote > i {
                let end = format!("\"{}", "#".repeat(quote - i - 1));
                let tail = quote + 1;
                i = tail + src[tail..].find(&end).expect("unterminated raw string") + end.len();
            } else {
                i += 1;
                while i < b.len() && b[i] != b'"' {
                    i += if b[i] == b'\\' { 2 } else { 1 };
                }
                assert!(i < b.len(), "unterminated string at {start}");
                i += 1;
            }
        } else if b[i] == b'\'' && b.get(i + 1) == Some(&b'\\') {
            // Escaped character literal, not a lifetime.
            i += 1;
            while i < b.len() && b[i] != b'\'' {
                i += if b[i] == b'\\' { 2 } else { 1 };
            }
            assert!(i < b.len(), "unterminated character at {start}");
            i += 1;
        } else if b[i] == b'\''
            && src[i + 1..]
                .chars()
                .next()
                .is_some_and(|c| b.get(i + 1 + c.len_utf8()) == Some(&b'\''))
        {
            i += 2 + src[i + 1..].chars().next().unwrap().len_utf8();
        } else if b[i].is_ascii_alphanumeric() || b[i] == b'_' {
            i += 1;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                i += 1;
            }
        } else {
            i += src[i..].chars().next().unwrap().len_utf8();
        }
        out.push(Token {
            text: &src[start..i],
            at: start,
        });
    }
    out
}

fn compact(ts: &[Token<'_>]) -> String {
    ts.iter().map(|t| t.text).collect()
}

fn wildcard(expr: &str) -> bool {
    // These are address forms, not arbitrary zero-filled buffers. Called only
    // on bind/listen argument expressions or their local initializers.
    let patterns = [
        r#""(?:0\.0\.0\.0(?::[^"]*)?|\[::\](?::[^"]*)?|::)""#,
        r"Ipv[46]Addr::UNSPECIFIED\b",
        r"Ipv4Addr::new\(0,0,0,0,?\)",
        r"Ipv6Addr::new\(0,0,0,0,0,0,0,0,?\)",
        r"\[(?:(?:0,){3}0,?|(?:0,){7}0,?|(?:0,){15}0,?|0;(?:4|8|16))\]",
        r"\(0,0,0,0(?:,0,0,0,0)?,?\)",
        r"(?:Ipv[46]Addr|IpAddr|SocketAddr[V46]*)::(?:from\(0(?:u32|u128)?\)|default\(\))",
    ];
    // Typed and radix-spelled zeroes denote the same wildcard address.
    static ZERO: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let expr = ZERO
        .get_or_init(|| {
            regex::Regex::new(r"\b(?:0x0+|0o0+|0b0+|0)(?:[ui](?:8|16|32|64|128|size))?\b").unwrap()
        })
        .replace_all(expr, "0");
    static PATTERNS: std::sync::OnceLock<Vec<regex::Regex>> = std::sync::OnceLock::new();
    PATTERNS
        .get_or_init(|| {
            patterns
                .iter()
                .map(|p| regex::Regex::new(p).unwrap())
                .collect()
        })
        .iter()
        .any(|p| p.is_match(&expr))
}

// Follow simple `let addr = ...; bind(addr)` chains, using the nearest preceding
// declaration. Stop at parameters/other computed values; never read fixture text
// as Rust. No runtime sockets are opened by this audit.
fn address_expr(ts: &[Token<'_>], before: usize, expr: &[Token<'_>], depth: usize) -> String {
    let mut value = compact(expr);
    assert!(depth < 16, "address alias chain is too deep to audit");
    for token in expr {
        if !token
            .text
            .starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        {
            continue;
        }
        if let Some(start) = (0..before.saturating_sub(2)).rev().find(|&i| {
            matches!(ts[i].text, "let" | "const" | "static")
                && (ts[i + 1].text == token.text
                    || (ts[i + 1].text == "mut" && ts[i + 2].text == token.text))
        }) {
            let tail = &ts[start..before];
            let mut nesting = 0;
            let end = tail
                .iter()
                .position(|t| {
                    match t.text {
                        "(" | "[" | "{" => nesting += 1,
                        ")" | "]" | "}" => nesting -= 1,
                        ";" if nesting == 0 => return true,
                        _ => {}
                    }
                    false
                })
                .unwrap_or(tail.len());
            if let Some(eq) = tail[..end].iter().position(|t| t.text == "=") {
                value.push_str(&address_expr(ts, start, &tail[eq + 1..end], depth + 1));
            }
        }
    }
    value
}

fn scan(src: &str) -> Vec<(usize, String)> {
    let ts = tokens(src);
    let mut bad = Vec::new();
    for i in 0..ts.len().saturating_sub(1) {
        if !(matches!(ts[i].text, "bind" | "listen" | "adopt_or_bind") && ts[i + 1].text == "(") {
            continue;
        }
        // Exclude function declarations; their callers are what we audit.
        if i > 0 && ts[i - 1].text == "fn" {
            continue;
        }
        let mut depth = 1;
        let mut end = i + 2;
        while end < ts.len() && depth > 0 {
            match ts[end].text {
                "(" => depth += 1,
                ")" => depth -= 1,
                _ => {}
            }
            if depth > 0 {
                end += 1;
            }
        }
        assert_eq!(depth, 0, "unclosed bind/listen call");
        let expr = address_expr(&ts, i, &ts[i + 2..end], 0);
        if wildcard(&expr) {
            bad.push((ts[i].at, compact(&ts[i..=end])));
        }
    }
    bad
}

fn rust_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_sources(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn no_wildcard_bind_outside_the_production_listener() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let mut files = Vec::new();
    rust_sources(root, &mut files);
    assert!(!files.is_empty(), "no Rust sources audited");
    files.sort();
    let mut bad = Vec::new();
    let mut allowed = 0;
    for file in files {
        let src = std::fs::read_to_string(&file).unwrap();
        for (at, call) in scan(&src) {
            // HTTP must be reachable from the LAN/cloud gateway. lib.rs builds
            // ([0,0,0,0], cfg.port); listener_handoff binds it or adopts its fd.
            // Exact file AND expression, never a whole-file exemption for tests.
            if file.strip_prefix(root).unwrap() == Path::new("amux-server/src/lib.rs")
                && call == "adopt_or_bind(addr)"
            {
                allowed += 1;
                continue;
            }
            let line = src[..at].bytes().filter(|&b| b == b'\n').count() + 1;
            bad.push(format!("{}:{line}: wildcard {call}", file.display()));
        }
    }
    assert_eq!(allowed, 1, "production listener allow-list is stale");
    assert!(
        bad.is_empty(),
        "wildcard binds can raise the macOS firewall dialog:\n{}",
        bad.join("\n")
    );
}

#[test]
fn audit_detects_address_forms_and_aliases_but_ignores_comments_and_fixtures() {
    for expr in [
        "\"0.0.0.0:0\"",
        "(\"0.0.0.0\", 0)",
        "\"[::]:0\"",
        "(\"::\", 0)",
        "(Ipv4Addr::UNSPECIFIED, 0)",
        "(Ipv6Addr::UNSPECIFIED, 0)",
        "SocketAddr::from(([0, 0, 0, 0], 0))",
        "([0; 4], 0)",
        "([0; 8], 0)",
        "([0; 16], 0)",
        "([0u8; 4], 0)",
        "([0x00, 0, 0, 0], 0)",
        "([0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0], 0)",
        "((0,0,0,0), 0)",
        "(Ipv4Addr::from(0u32), 0)",
        "(Ipv6Addr::from(0u128), 0)",
        "(Ipv4Addr::default(), 0)",
        "(Ipv6Addr::default(), 0)",
        r##"r#"0.0.0.0:0"#"##,
        "([0,0,0,0,0,0,0,0], 0)",
        "(Ipv4Addr::new(0,0,0,0), 0)",
        "(Ipv6Addr::new(0,0,0,0,0,0,0,0), 0)",
    ] {
        for verb in ["TcpListener::bind", "socket.bind", "listen"] {
            let direct = format!("{verb}(\n{expr}\n);");
            assert_eq!(scan(&direct).len(), 1, "missed {direct}");
            let alias = format!("let ip = {expr}; let addr = ip; {verb}(addr);");
            assert_eq!(scan(&alias).len(), 1, "missed {alias}");
            let constant = format!("const ADDR: SocketAddr = {expr}; {verb}(ADDR);");
            assert_eq!(scan(&constant).len(), 1, "missed {constant}");
        }
    }
    let safe = r####"
        // TcpListener::bind("0.0.0.0:0");
        /* outer /* socket.bind(Ipv6Addr::UNSPECIFIED); */ listen("[::]:0"); */
        let fixture = r###"TcpListener::bind("0.0.0.0:0");"###;
        let url = "http://example.test/*not a comment*/";
        TcpListener::bind("127.0.0.1:0");
        TcpListener::bind("[::1]:0");
        let addr = ([127,0,0,1], 0); socket.bind(addr);
    "####;
    assert!(
        scan(safe).is_empty(),
        "comments, fixtures and loopback are safe"
    );
    assert_eq!(
        scan("// ignored\nTcpListener::bind(\"0.0.0.0:0\");")[0].0,
        24
    );
}
