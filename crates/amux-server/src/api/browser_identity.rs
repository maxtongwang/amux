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
    /// Code identifiers: each matches itself and its `.`-suffixed helpers.
    pub identifiers: &'static [&'static str],
    /// The requirement the signature must strictly satisfy.
    pub requirement: &'static str,
}

pub(crate) const BROWSERS: &[Browser] = &[
    Browser {
        name: "Google Chrome",
        team: Some("EQHXZ8M8AV"),
        identifiers: &["com.google.Chrome"],
        requirement: "anchor apple generic and certificate leaf[subject.OU] = \"EQHXZ8M8AV\"",
    },
    Browser {
        name: "Microsoft Edge",
        team: Some("UBF8T346G9"),
        identifiers: &["com.microsoft.edgemac"],
        requirement: "anchor apple generic and certificate leaf[subject.OU] = \"UBF8T346G9\"",
    },
    // Safari's sockets are held by WebKit's networking XPC service, a launchd
    // child signed by Apple like Safari itself.
    Browser {
        name: "Apple Safari",
        team: None,
        identifiers: &["com.apple.Safari", "com.apple.WebKit.Networking"],
        requirement: "anchor apple",
    },
];

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
        .find(|b| b.identifiers.iter().any(|id| identifier_matches(id, &facts.identifier)))
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
            .signed("/safari-net", "com.apple.WebKit.Networking", None, "Software Signing");
        assert_eq!(verify_browser(&exe(CHROME), &good).unwrap().name, "Google Chrome");
        assert_eq!(verify_browser(&exe("/helper"), &good).unwrap().name, "Google Chrome", "a helper of the same vendor");
        assert_eq!(verify_browser(&exe("/safari-net"), &good).unwrap().name, "Apple Safari");

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
