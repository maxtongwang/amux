//! Read-only Chrome discovery: preserve measurement failures beside empty results.
use serde::Serialize;
use std::path::Path;

#[derive(Debug, Serialize)]
pub struct DiscoveryStatus {
    pub measured: bool,
    pub n_considered: usize,
    pub why_unmeasured: Option<String>,
    pub empty_reason: Option<String>,
    pub owner_step: Option<String>,
}

impl DiscoveryStatus {
    pub fn measured(n: usize, empty_reason: &str) -> Self {
        Self {
            measured: true,
            n_considered: n,
            why_unmeasured: None,
            empty_reason: (n == 0).then(|| empty_reason.to_owned()),
            owner_step: None,
        }
    }

    pub fn failed(reason: &str, path: &Path) -> Self {
        let owner_step = match reason {
            "parse_failure" | "invalid_registry" if path.file_name() == Some(std::ffi::OsStr::new("profiles.json")) =>
                "Repair the saved Amux profiles.json registry at the reported path, then retry discovery; existing profile directories are retained.",
            "permission_denied" if cfg!(target_os = "macos") =>
                "Grant the amux server process Full Disk Access in System Settings > Privacy & Security > Full Disk Access, then retry discovery.",
            "permission_denied" => "Grant the amux server process read and directory traversal access to the selected profile root, then retry discovery.",
            "not_found" => "Set AMUX_BROWSER_CHROME_USER_DATA_DIR on the server to the intended Chrome user data directory, then retry discovery.",
            "parse_failure" | "invalid_profile_cache" => "Select a valid Chrome Local State fixture or user data root with AMUX_BROWSER_CHROME_USER_DATA_DIR, then retry discovery.",
            _ => "Resolve the reported filesystem error for the amux server process, then retry discovery.",
        };
        let why = format!("{reason}: {}", path.display());
        tracing::warn!(verdict = "browser_profiles_unreadable", measured = false,
            n_considered = 0, reason = %why, "browser profile discovery could not measure its source");
        Self {
            measured: false,
            n_considered: 0,
            why_unmeasured: Some(why),
            empty_reason: None,
            owner_step: Some(owner_step.into()),
        }
    }

    pub fn io_failure(error: &std::io::Error, path: &Path) -> Self {
        let reason = match error.kind() {
            std::io::ErrorKind::PermissionDenied => "permission_denied",
            std::io::ErrorKind::NotFound => "not_found",
            _ => "io_failure",
        };
        Self::failed(reason, path)
    }
}

#[derive(Debug, Serialize)]
pub struct ChromeProfile {
    pub name: String,
    pub label: Option<String>,
    pub identity: Option<String>,
    pub on_disk: bool,
}

#[derive(Debug, Serialize)]
pub struct ChromeDiscovery {
    pub profiles: Vec<ChromeProfile>,
    pub discovery: DiscoveryStatus,
}

/// Explicit root keeps tests independent of HOME and the owner's browser data.
pub fn discover_chrome_profiles_in(root: &Path) -> ChromeDiscovery {
    let path = root.join("Local State");
    let result = (|| {
        let bytes = std::fs::read(&path).map_err(|e| DiscoveryStatus::io_failure(&e, &path))?;
        let value: serde_json::Value = serde_json::from_slice(&bytes)
            .map_err(|_| DiscoveryStatus::failed("parse_failure", &path))?;
        let cache = value
            .pointer("/profile/info_cache")
            .and_then(|v| v.as_object())
            .ok_or_else(|| DiscoveryStatus::failed("invalid_profile_cache", &path))?;
        let mut profiles = Vec::with_capacity(cache.len());
        for (name, meta) in cache {
            let mut components = Path::new(name).components();
            if !matches!(components.next(), Some(std::path::Component::Normal(_)))
                || components.next().is_some()
                || !meta.is_object()
            {
                return Err(DiscoveryStatus::failed("invalid_profile_cache", &path));
            }
            let profile_path = root.join(name);
            let on_disk = match std::fs::metadata(&profile_path) {
                Ok(m) => m.is_dir(),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
                Err(e) => return Err(DiscoveryStatus::io_failure(&e, &profile_path)),
            };
            profiles.push(ChromeProfile {
                name: name.clone(),
                label: meta.get("name").and_then(|v| v.as_str()).map(str::to_owned),
                identity: meta
                    .get("user_name")
                    .and_then(|v| v.as_str())
                    .map(str::to_owned),
                on_disk,
            });
        }
        profiles.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(profiles)
    })();
    match result {
        Ok(profiles) => ChromeDiscovery {
            discovery: DiscoveryStatus::measured(profiles.len(), "no_chrome_profiles_configured"),
            profiles,
        },
        Err(discovery) => ChromeDiscovery {
            profiles: vec![],
            discovery,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(root: &Path) {
        std::fs::write(
            root.join("Local State"),
            br#"{"profile":{"info_cache":{
            "Default":{"name":"Synthetic Work","user_name":"work@example.invalid"},
            "Profile 1":{"name":"Synthetic Personal","user_name":"personal@example.invalid"}
        }}}"#,
        )
        .unwrap();
        std::fs::create_dir(root.join("Default")).unwrap();
        std::fs::create_dir(root.join("Profile 1")).unwrap();
    }

    #[test]
    fn valid_fixture_lists_profiles_and_identities() {
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path());
        let out = discover_chrome_profiles_in(dir.path());
        assert!(out.discovery.measured);
        assert_eq!(out.discovery.n_considered, 2);
        assert!(out.discovery.why_unmeasured.is_none());
        assert_eq!(
            out.profiles
                .iter()
                .map(|p| p.name.as_str())
                .collect::<Vec<_>>(),
            ["Default", "Profile 1"]
        );
        assert_eq!(
            out.profiles[0].identity.as_deref(),
            Some("work@example.invalid")
        );
        assert!(out.profiles.iter().all(|p| p.on_disk));
    }

    fn assert_failure(root: &Path, reason: &str) {
        let out = discover_chrome_profiles_in(root);
        assert!(!out.discovery.measured);
        assert_eq!(out.discovery.n_considered, 0);
        assert_eq!(
            out.discovery.why_unmeasured.as_deref(),
            Some(format!("{reason}: {}", root.join("Local State").display()).as_str())
        );
        assert!(out.discovery.owner_step.is_some());
        assert!(out.profiles.is_empty());
    }

    #[test]
    fn missing_directory_is_not_a_measured_zero() {
        let dir = tempfile::tempdir().unwrap();
        assert_failure(&dir.path().join("missing"), "not_found");
    }

    #[test]
    fn malformed_json_is_not_a_measured_zero() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Local State"), b"{malformed").unwrap();
        assert_failure(dir.path(), "parse_failure");
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_directory_is_not_a_measured_zero() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path());
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o000)).unwrap();
        // Restore permissions even if an assertion fails. Privileged runners must
        // run this test as an unprivileged user; never silently skip the assertion.
        let out = discover_chrome_profiles_in(dir.path());
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(
            !out.discovery.measured,
            "chmod 000 requires an unprivileged test runner"
        );
        assert_eq!(out.discovery.n_considered, 0);
        assert!(out
            .discovery
            .why_unmeasured
            .unwrap()
            .starts_with("permission_denied: "));
        assert!(out
            .discovery
            .owner_step
            .unwrap()
            .contains("amux server process"));
        assert!(out.profiles.is_empty());
    }

    #[test]
    fn empty_cache_and_missing_schema_are_distinct() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Local State"),
            br#"{"profile":{"info_cache":{}}}"#,
        )
        .unwrap();
        let out = discover_chrome_profiles_in(dir.path());
        assert!(out.discovery.measured);
        assert_eq!(
            out.discovery.empty_reason.as_deref(),
            Some("no_chrome_profiles_configured")
        );
        std::fs::write(dir.path().join("Local State"), b"{}").unwrap();
        assert_failure(dir.path(), "invalid_profile_cache");
    }
}
