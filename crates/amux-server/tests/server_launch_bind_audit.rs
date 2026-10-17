//! Fixture server launches must explicitly bind IPv4 loopback: unsigned macOS
//! test binaries listening on LAN interfaces raise an owner-visible firewall dialog.
//! This audits launcher source without starting a server or touching the firewall.

use regex::Regex;
use std::path::{Path, PathBuf};

fn sources(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("read audit directory") {
        let path = entry.expect("read audit entry").path();
        if path.is_dir() {
            if !matches!(
                path.file_name().and_then(|n| n.to_str()),
                Some("node_modules" | "test-results" | "playwright-report" | "__pycache__")
            ) {
                sources(&path, out);
            }
        } else if matches!(
            path.extension().and_then(|n| n.to_str()),
            Some("rs" | "ts" | "js" | "mjs" | "cjs" | "py" | "sh")
        ) {
            out.push(path);
        }
    }
}

fn without_line_comments(src: &str) -> String {
    // Preserve byte offsets while excluding whole-line comments from statement
    // boundaries (a semicolon in prose is not the end of a command builder).
    src.split_inclusive('\n')
        .map(|line| {
            if line.trim_start().starts_with("//") || line.trim_start().starts_with('#') {
                " ".repeat(line.len())
            } else {
                line.to_string()
            }
        })
        .collect()
}

fn scan(extension: &str, src: &str) -> Vec<usize> {
    let code = without_line_comments(src);
    let src = code.as_str();
    let mut bad = Vec::new();
    if extension == "rs" {
        // A command builder's port and bind must be in the SAME statement;
        // another launch elsewhere in the file cannot satisfy this assertion.
        let port = Regex::new(r#"\.env\(\s*"AMUX_RS_PORT"\s*,"#).unwrap();
        let bind = Regex::new(r#"\.env\(\s*"AMUX_BIND"\s*,\s*"127\.0\.0\.1"\s*\)"#).unwrap();
        for m in port.find_iter(src) {
            let start = src[..m.start()].rfind(';').map_or(0, |i| i + 1);
            let end = src[m.end()..].find(';').map_or(src.len(), |i| m.end() + i);
            if !bind.is_match(&src[start..end]) {
                bad.push(m.start());
            }
        }
    } else if matches!(extension, "ts" | "js" | "mjs" | "cjs") {
        // Env object literals passed to Playwright or Object.assign(env, ...).
        // Port-only updates (env.AMUX_RS_PORT = ...) retain the original env.
        let port = Regex::new(r"\bAMUX_RS_PORT\s*:").unwrap();
        let bind = Regex::new(r#"\bAMUX_BIND\s*:\s*['"]127\.0\.0\.1['"]"#).unwrap();
        for m in port.find_iter(src) {
            let start = src[..m.start()].rfind('{').unwrap_or(0);
            let end = src[m.end()..].find('}').map_or(src.len(), |i| m.end() + i);
            if !bind.is_match(&src[start..end]) {
                bad.push(m.start());
            }
        }
    } else {
        let port = Regex::new(r"\bAMUX_RS_PORT=").unwrap();
        let bind = Regex::new(r#"\bAMUX_BIND=['"]?127\.0\.0\.1(?:['"]|\s|,|$)"#).unwrap();
        let mut exported = false;
        let mut offset = 0;
        for line in src.lines() {
            if extension == "sh" && line.trim() == "export AMUX_BIND=127.0.0.1" {
                exported = true;
            }
            if !line.trim_start().starts_with('#')
                && port.is_match(line)
                && !exported
                && !bind.is_match(line)
            {
                bad.push(offset);
            }
            offset += line.len() + 1;
        }
    }
    bad
}

#[test]
fn every_fixture_server_launch_selects_loopback() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let mut files = Vec::new();
    for dir in ["e2e", "crates/amux-server/tests", "scripts"] {
        sources(&root.join(dir), &mut files);
    }
    // Launchers that inherit the port instead of spelling a port assignment.
    // Keep explicit assertions so deleting both env settings cannot erase coverage.
    let launchers = [
        "e2e/playwright.config.ts",
        "e2e/serve-head.sh",
        "e2e/lifecycle/playwright.config.ts",
        "e2e/lifecycle/serve.sh",
        "e2e/project-lifecycle/serve.py",
        "e2e/chat-streaming.mjs",
        "e2e/chaos/harness.mjs",
        "e2e/chaos/slash-real.mjs",
        "crates/amux-server/tests/restart_persistence.rs",
        "crates/amux-server/tests/connector_matrix.rs",
        "crates/amux-server/tests/connector_maintenance_process.rs",
        "scripts/perf-baseline.sh",
        "scripts/perf-ci.sh",
        "scripts/soak-probe.sh",
    ];
    let literal_bind =
        Regex::new(r#"AMUX_BIND(?:"\s*,\s*"|\s*:\s*['"]|=['"]?)127\.0\.0\.1"#).unwrap();
    let direct_launch = Regex::new(r#"Command::new\([^;\n]*(?:amux-server|server_bin|self\.binary)|spawn\(\s*(?:bin(?:ary)?\b|process\.env\.AMUX_[A-Z_]*BIN(?:ARY)?)|os\.execve\([^\n]*binary|exec\s+"\$[^"\n]*(?:BIN|BINARY|amux-server)""#).unwrap();
    let mut bad = Vec::new();
    for path in launchers {
        let src = without_line_comments(
            &std::fs::read_to_string(root.join(path)).expect("read known launcher"),
        );
        assert!(
            literal_bind.is_match(&src),
            "{path}: missing explicit AMUX_BIND loopback setting"
        );
    }
    let mut audited = 0;
    for file in files {
        if file.file_name().unwrap() == "server_launch_bind_audit.rs" {
            continue; // Synthetic negative controls below deliberately omit the setting.
        }
        let src = std::fs::read_to_string(&file).expect("read source to audit");
        // Production service templates are not fixture launchers (and are not .sh).
        let extension = file.extension().unwrap().to_str().unwrap();
        // Also discover direct binary launches even if somebody deletes the port
        // setting, or adds a launcher outside the known inventory above.

        let code = without_line_comments(&src);
        if direct_launch.is_match(&code) && !literal_bind.is_match(&code) {
            bad.push(format!(
                "{}: direct server launch lacks explicit AMUX_BIND=127.0.0.1",
                file.strip_prefix(root).unwrap().display()
            ));
        }
        for at in scan(extension, &src) {
            bad.push(format!(
                "{}:{}: server port configured without AMUX_BIND=127.0.0.1",
                file.strip_prefix(root).unwrap().display(),
                src[..at].lines().count() + 1
            ));
        }
        audited += 1;
    }
    assert!(
        audited > launchers.len(),
        "source discovery must reach beyond the known launchers"
    );
    assert!(
        bad.is_empty(),
        "fixture listeners may expose LAN interfaces:\n{}",
        bad.join("\n")
    );
}

#[test]
fn comments_and_late_exports_cannot_satisfy_the_audit() {
    let missing = "AMUX_RS_PORT=1 server &\n";
    assert_eq!(
        scan("sh", &format!("# AMUX_BIND=127.0.0.1\n{missing}")).len(),
        1
    );
    assert_eq!(
        scan("sh", &format!("{missing}export AMUX_BIND=127.0.0.1\n")).len(),
        1
    );
    assert!(scan("sh", &format!("export AMUX_BIND=127.0.0.1\n{missing}")).is_empty());
    assert!(!without_line_comments("// AMUX_BIND=127.0.0.1\n").contains("AMUX_BIND"));
}

#[test]
fn scanner_rejects_missing_wrong_and_sibling_settings() {
    for (extension, good, missing) in [
        (
            "rs",
            r#"cmd.env("AMUX_BIND", "127.0.0.1").env("AMUX_RS_PORT", "1").spawn();"#,
            r#"cmd.env("AMUX_RS_PORT", "2").spawn();"#,
        ),
        (
            "ts",
            "const env = { AMUX_BIND: '127.0.0.1', AMUX_RS_PORT: '1' };",
            "const other = { AMUX_RS_PORT: '2' };",
        ),
        (
            "py",
            "env.update(AMUX_BIND='127.0.0.1',AMUX_RS_PORT='1')\n",
            "other.update(AMUX_RS_PORT='2')\n",
        ),
        (
            "sh",
            "AMUX_BIND=127.0.0.1 AMUX_RS_PORT=1 server &\n",
            "AMUX_RS_PORT=2 server &\n",
        ),
    ] {
        assert!(
            scan(extension, good).is_empty(),
            "{extension}: good setting rejected"
        );
        assert_eq!(
            scan(extension, missing).len(),
            1,
            "{extension}: missing setting undetected"
        );
        assert_eq!(
            scan(extension, &good.replace("127.0.0.1", "0.0.0.0")).len(),
            1,
            "{extension}: wildcard setting undetected"
        );
        assert_eq!(
            scan(extension, &format!("{good}\n{missing}")).len(),
            1,
            "{extension}: one good launcher must not hide a bad sibling"
        );
    }
}
