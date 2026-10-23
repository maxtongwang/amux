//! Which code-signed GUI browsers count as the owner's dashboard (#10, B1).
//! Dependency-free (std only) so the shipped policy also compiles under plain rustc.
//!
//! THE RULE. The dashboard UI token alone is not owner proof: any same-user
//! process can derive it. An owner send also needs the connection to come from
//! the owner's GUI browser or from a remote tailnet peer. A browser is judged by
//! its CODE SIGNATURE, never its name or path: the signing team (or Apple's own
//! anchor for Safari), the code identifier, and a strict verification against a
//! requirement anchored at Apple's CA. `process_identity` walks the socket
//! holder's ancestry and asks this module about every process on it.
//!
//! The signature itself is read through [`SignatureVerifier`] so tests inject a
//! fake; production shells out to /usr/bin/codesign, which never shows UI.

use std::path::PathBuf;

/// One executable as the verification cache keys it. Replacing the file gives
/// it a new inode, so a cached verdict never outlives the bytes it judged.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct Exe {
    pub path: PathBuf,
    pub inode: u64,
}

/// What `codesign -dv --verbose=2` reports about a signature.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct CodeFacts {
    pub identifier: String,
    /// `TeamIdentifier`; None when codesign says "not set" (Apple platform code).
    pub team: Option<String>,
    /// The certificate chain, leaf first.
    pub authorities: Vec<String>,
}

/// The two codesign questions, behind a trait so tests never run codesign.
pub(crate) trait SignatureVerifier: Send + Sync {
    /// The signature's facts; Err when the code is unsigned or unreadable.
    fn describe(&self, exe: &Exe) -> Result<CodeFacts, String>;
    /// Whether `codesign --verify --strict` passes against `requirement`.
    fn satisfies(&self, exe: &Exe, requirement: &str) -> Result<bool, String>;
}

/// One allow-listed browser vendor.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Browser {
    pub name: &'static str,
    /// The signing team; None means Apple platform code (no team, Apple anchor).
    pub team: Option<&'static str>,
    /// Code identifiers, with optional dot-suffixed helpers.
    pub identifiers: &'static [&'static str],
    /// Whether dot-suffixed helper identifiers are accepted.
    pub helpers: bool,
    /// The requirement the signature must strictly satisfy.
    pub requirement: &'static str,
}

pub(crate) const BROWSERS: &[Browser] = &[
    Browser {
        name: "Google Chrome",
        team: Some("EQHXZ8M8AV"),
        identifiers: &["com.google.Chrome"],
        helpers: true,
        requirement: "anchor apple generic and certificate leaf[subject.OU] = \"EQHXZ8M8AV\"",
    },
    Browser {
        name: "Microsoft Edge",
        team: Some("UBF8T346G9"),
        identifiers: &["com.microsoft.edgemac"],
        helpers: true,
        requirement: "anchor apple generic and certificate leaf[subject.OU] = \"UBF8T346G9\"",
    },
    // Only Safari's own bundle qualifies. WebKit.Networking serves arbitrary
    // WKWebView apps; its launchd ancestry cannot establish the responsible app.
    Browser {
        name: "Apple Safari",
        team: None,
        identifiers: &["com.apple.Safari"],
        helpers: false,
        requirement: "anchor apple",
    },
];

/// Automation is never owner input, even after launchd adopts it on restart.
/// `owns_profile` uses the launcher's authoritative profile ownership predicate.
pub(crate) fn verify_interactive_browser(
    argv: &[String],
    launched_by_amux: bool,
    owns_profile: impl Fn(&std::path::Path) -> bool,
) -> Result<(), String> {
    if launched_by_amux {
        return Err("amux-launched browser is automation".into());
    }
    if argv.is_empty() {
        return Err("browser argv is empty".into());
    }
    for (i, arg) in argv.iter().enumerate().skip(1) {
        let flag = arg.split('=').next().unwrap_or(arg);
        if matches!(flag, "--remote-debugging-port" | "--remote-debugging-pipe") {
            return Err("remote-debugging browser is automation".into());
        }
        if flag == "--user-data-dir" {
            let dir = arg.strip_prefix("--user-data-dir=")
                .or_else(|| argv.get(i + 1).map(String::as_str))
                .filter(|s| !s.is_empty() && !s.starts_with("--"))
                .ok_or("browser profile argument is unreadable")?;
            if owns_profile(std::path::Path::new(dir)) {
                return Err("amux-owned browser profile is automation".into());
            }
        }
    }
    Ok(())
}

/// Decode macOS KERN_PROCARGS2: argc, executable, padding, exactly argc argv.
/// Environment bytes are never interpreted as arguments or logged.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn parse_procargs(bytes: &[u8]) -> Result<Vec<String>, String> {
    let malformed = || "malformed browser process argv".to_string();
    let count = i32::from_ne_bytes(bytes.get(..4).ok_or_else(malformed)?.try_into().map_err(|_| malformed())?);
    if count <= 0 || count as usize > bytes.len() {
        return Err(malformed());
    }
    let rest = &bytes[4..];
    let exe_end = rest.iter().position(|b| *b == 0).ok_or_else(malformed)?;
    let rest = &rest[exe_end..];
    let start = rest.iter().position(|b| *b != 0).ok_or_else(malformed)?;
    let mut rest = &rest[start..];
    let mut argv = Vec::new();
    for _ in 0..count {
        let end = rest.iter().position(|b| *b == 0).ok_or_else(malformed)?;
        argv.push(std::str::from_utf8(&rest[..end]).map_err(|_| malformed())?.to_string());
        rest = &rest[end + 1..];
    }
    Ok(argv)
}

/// Parse `codesign -dv --verbose=2` output (codesign writes it to stderr).
pub(crate) fn parse_codesign_display(text: &str) -> Result<CodeFacts, String> {
    let mut facts = CodeFacts::default();
    for line in text.lines() {
        if let Some(v) = line.strip_prefix("Identifier=") {
            facts.identifier = v.trim().to_string();
        } else if let Some(v) = line.strip_prefix("TeamIdentifier=") {
            let v = v.trim();
            facts.team = (!v.is_empty() && v != "not set").then(|| v.to_string());
        } else if let Some(v) = line.strip_prefix("Authority=") {
            facts.authorities.push(v.trim().to_string());
        }
    }
    if facts.identifier.is_empty() {
        return Err("codesign reported no code identifier".into());
    }
    Ok(facts)
}

fn identifier_matches(allowed: &str, identifier: &str) -> bool {
    identifier == allowed || identifier.strip_prefix(allowed).is_some_and(|rest| rest.starts_with('.'))
}

/// The allow-listed browser `exe` is, or why it is not one. Every check must
/// pass: an allow-listed identifier, that vendor's team (or Apple platform
/// signing), and strict verification against the vendor's requirement.
pub(crate) fn verify_browser(exe: &Exe, v: &dyn SignatureVerifier) -> Result<&'static Browser, String> {
    let facts = v.describe(exe)?;
    let browser = BROWSERS
        .iter()
        .find(|b| b.identifiers.iter().any(|id| *id == facts.identifier || (b.helpers && identifier_matches(id, &facts.identifier))))
        .ok_or_else(|| format!("{} is not an allow-listed browser", facts.identifier))?;
    match browser.team {
        Some(team) if facts.team.as_deref() != Some(team) => {
            return Err(format!("{} is signed by team {:?}, not {}'s {team}", facts.identifier, facts.team, browser.name));
        }
        None if facts.team.is_some() || facts.authorities.first().map(String::as_str) != Some("Software Signing") => {
            return Err(format!("{} is not Apple platform code (team {:?})", facts.identifier, facts.team));
        }
        _ => {}
    }
    if !v.satisfies(exe, browser.requirement)? {
        return Err(format!("{}'s signature does not verify against {}", facts.identifier, browser.requirement));
    }
    Ok(browser)
}

#[cfg(test)]
pub(crate) mod fake {
    use super::*;
    use std::collections::HashMap;

    /// A fake codesign: facts and verification results per executable path.
    #[derive(Default, Clone)]
    pub(crate) struct FakeSignatures {
        pub facts: HashMap<PathBuf, Result<CodeFacts, String>>,
        pub verifies: HashMap<PathBuf, bool>,
    }

    impl FakeSignatures {
        pub(crate) fn signed(mut self, path: &str, identifier: &str, team: Option<&str>, authority: &str) -> Self {
            self.facts.insert(path.into(), Ok(CodeFacts {
                identifier: identifier.into(),
                team: team.map(str::to_string),
                authorities: vec![authority.into()],
            }));
            self.verifies.insert(path.into(), true);
            self
        }
        pub(crate) fn unsigned(mut self, path: &str) -> Self {
            self.facts.insert(path.into(), Err("code object is not signed at all".into()));
            self
        }
        pub(crate) fn tampered(mut self, path: &str) -> Self {
            self.verifies.insert(path.into(), false);
            self
        }
    }

    impl SignatureVerifier for FakeSignatures {
        fn describe(&self, exe: &Exe) -> Result<CodeFacts, String> {
            self.facts.get(&exe.path).cloned().unwrap_or_else(|| Err("no such executable".into()))
        }
        fn satisfies(&self, exe: &Exe, _requirement: &str) -> Result<bool, String> {
            Ok(self.verifies.get(&exe.path).copied().unwrap_or(false))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::FakeSignatures;
    use super::*;

    fn exe(path: &str) -> Exe {
        Exe { path: path.into(), inode: 7 }
    }

    const CHROME: &str = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";

    #[test]
    fn browser_automation_arguments_are_never_interactive_owner_input() {
        let owned = |dir: &std::path::Path| dir.starts_with("/fixture/amux/playwright-auth");
        let args = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(verify_interactive_browser(&args(&[CHROME]), false, owned).is_ok());
        assert!(verify_interactive_browser(&args(&[CHROME]), true, owned).is_err());
        assert!(verify_interactive_browser(&args(&[CHROME, "--user-data-dir=/fixture/human chrome"]), false, owned).is_ok());
        for option in ["--remote-debugging-port=0", "--remote-debugging-pipe", "--user-data-dir=/fixture/amux/playwright-auth/profile", "--user-data-dir"] {
            assert!(verify_interactive_browser(&args(&[CHROME, option]), false, owned).is_err(), "{option}");
        }
        assert!(verify_interactive_browser(&[], false, owned).is_err());
    }

    #[test]
    fn procargs_preserves_spaces_and_stops_before_environment() {
        let mut bytes = 3_i32.to_ne_bytes().to_vec();
        bytes.extend_from_slice(b"/Applications/Google Chrome\0\0/Applications/Google Chrome\0--user-data-dir\0/fixture/human chrome\0TOKEN=private\0");
        assert_eq!(parse_procargs(&bytes).unwrap(), ["/Applications/Google Chrome", "--user-data-dir", "/fixture/human chrome"]);
        for malformed in [&bytes[..3], &bytes[..bytes.len() - 40], &0_i32.to_ne_bytes()] {
            assert!(parse_procargs(malformed).is_err());
        }
    }

    #[test]
    fn the_codesign_display_is_parsed_for_identifier_team_and_chain() {
        let chrome = "Executable=/Applications/Google Chrome.app/Contents/MacOS/Google Chrome\n\
            Identifier=com.google.Chrome\nFormat=app bundle with Mach-O universal (x86_64 arm64)\n\
            Authority=Developer ID Application: Google LLC (EQHXZ8M8AV)\n\
            Authority=Developer ID Certification Authority\nAuthority=Apple Root CA\n\
            TeamIdentifier=EQHXZ8M8AV\n";
        let f = parse_codesign_display(chrome).unwrap();
        assert_eq!(f.identifier, "com.google.Chrome");
        assert_eq!(f.team.as_deref(), Some("EQHXZ8M8AV"));
        assert_eq!(f.authorities.len(), 3);
        let safari = "Identifier=com.apple.Safari\nAuthority=Software Signing\nAuthority=Apple Code Signing Certification Authority\nAuthority=Apple Root CA\nTeamIdentifier=not set\n";
        assert_eq!(parse_codesign_display(safari).unwrap().team, None, "platform code has no team");
        assert!(parse_codesign_display("code object is not signed at all").is_err());
    }

    #[test]
    fn a_browser_needs_identifier_team_and_a_verifying_signature() {
        let good = FakeSignatures::default()
            .signed(CHROME, "com.google.Chrome", Some("EQHXZ8M8AV"), "Developer ID Application: Google LLC (EQHXZ8M8AV)")
            .signed("/helper", "com.google.Chrome.helper", Some("EQHXZ8M8AV"), "Developer ID Application: Google LLC (EQHXZ8M8AV)")
            .signed("/safari", "com.apple.Safari", None, "Software Signing")
            .signed("/safari-helper", "com.apple.Safari.helper", None, "Software Signing")
            .signed("/safari-net", "com.apple.WebKit.Networking", None, "Software Signing");
        assert_eq!(verify_browser(&exe(CHROME), &good).unwrap().name, "Google Chrome");
        assert_eq!(verify_browser(&exe("/helper"), &good).unwrap().name, "Google Chrome", "a helper of the same vendor");
        assert_eq!(verify_browser(&exe("/safari"), &good).unwrap().name, "Apple Safari");
        assert!(verify_browser(&exe("/safari-helper"), &good).is_err());
        assert!(verify_browser(&exe("/safari-net"), &good).is_err());

        let wrong_team = FakeSignatures::default().signed(CHROME, "com.google.Chrome", Some("ABCDE12345"), "Developer ID Application: Someone (ABCDE12345)");
        assert!(verify_browser(&exe(CHROME), &wrong_team).unwrap_err().contains("not Google Chrome's EQHXZ8M8AV"));
        let adhoc = FakeSignatures::default().signed(CHROME, "com.google.Chrome", None, "");
        assert!(verify_browser(&exe(CHROME), &adhoc).is_err(), "an ad-hoc signature has no team");
        let fake_apple = FakeSignatures::default().signed("/s", "com.apple.Safari", Some("ABCDE12345"), "Software Signing");
        assert!(verify_browser(&exe("/s"), &fake_apple).is_err(), "Apple platform code carries no team");
        let not_apple = FakeSignatures::default().signed("/s", "com.apple.Safari", None, "Developer ID Application: x");
        assert!(verify_browser(&exe("/s"), &not_apple).is_err(), "Safari must be Apple-signed");
        let tampered = good.clone().tampered(CHROME);
        assert!(verify_browser(&exe(CHROME), &tampered).unwrap_err().contains("does not verify"));
        let unsigned = FakeSignatures::default().unsigned(CHROME);
        assert!(verify_browser(&exe(CHROME), &unsigned).is_err());
        let curl = FakeSignatures::default().signed("/usr/bin/curl", "com.apple.curl", None, "Software Signing");
        assert!(verify_browser(&exe("/usr/bin/curl"), &curl).unwrap_err().contains("not an allow-listed browser"));
        let lookalike = FakeSignatures::default().signed("/x", "com.google.ChromeX", Some("EQHXZ8M8AV"), "Developer ID Application: Google LLC (EQHXZ8M8AV)");
        assert!(verify_browser(&exe("/x"), &lookalike).is_err(), "an identifier prefix is not a helper");
    }
}
