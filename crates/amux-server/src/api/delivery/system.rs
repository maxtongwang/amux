//! Local registry and kernel identity boundary. Tests inject the host reader;
//! discovery never guesses a socket from a PID or executes Claude to probe it.
use super::{
    wire, MAX_REGISTRY_BYTES, MAX_SOCKET_PATH_BYTES, REQUIRED_FEATURES, REVIEWED_SHA256,
    WIRE_VERSION,
};
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt};
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct ProcessIdentity {
    pub pid: u32,
    pub uid: u32,
    /// Claude procStart / verifiedPeerProcStart: trimmed C-locale UTC ps lstart.
    pub registry_start: String,
    /// Kernel sec:usec token for Amux-only PID reuse checks, never Claude fields.
    pub instance: String,
    pub executable: PathBuf,
    pub executable_sha256: String,
}

pub(super) trait Host: Send + Sync {
    fn process(&self, pid: u32) -> Result<ProcessIdentity, &'static str>;
    fn uid(&self) -> u32;
}

pub(super) struct LocalHost;
impl Host for LocalHost {
    fn process(&self, pid: u32) -> Result<ProcessIdentity, &'static str> {
        process(pid)
    }
    fn uid(&self) -> u32 {
        // SAFETY: getuid has no preconditions.
        unsafe { libc::getuid() }
    }
}

#[derive(Debug)]
pub(super) enum DiscoveryError {
    Unavailable(&'static str),
    Quarantined(&'static str),
}

#[derive(Debug, Clone)]
pub(super) struct Target {
    pub process: ProcessIdentity,
    pub registry: PathBuf,
    pub registry_sha256: String,
    pub socket: PathBuf,
    pub session: String,
    pub config_dir: PathBuf,
}

pub(super) fn profile_key(process: &ProcessIdentity) -> String {
    wire::sha256(
        serde_json::json!([
            process.pid,
            process.uid,
            process.instance,
            process.executable_sha256
        ])
        .to_string()
        .as_bytes(),
    )
}

/// Liveness/status heartbeats do not change inbox identity. Pin routing and
/// protocol fields, so a routine registry heartbeat cannot strand a send.
fn registry_identity(record: &serde_json::Value) -> String {
    wire::sha256(
        serde_json::json!([
            record["pid"],
            record["procStart"],
            record["sessionId"],
            record["messagingSocketPath"],
            record["peerProtocol"],
            record["peerFeatures"]
        ])
        .to_string()
        .as_bytes(),
    )
}

/// Pin routing/capability fields as well as process and executable. Fresh discovery
/// is required for every attempt; there is deliberately no positive cache.
pub(super) fn discover(
    host: &dyn Host,
    config_dir: &Path,
    pids: &[u32],
    expected_session: Option<&str>,
) -> Result<Target, DiscoveryError> {
    use DiscoveryError::{Quarantined as Q, Unavailable as U};
    for &pid in pids {
        let registry = config_dir.join("sessions").join(format!("{pid}.json"));
        let bytes = match private_read(&registry, host.uid(), MAX_REGISTRY_BYTES) {
            Ok(bytes) => bytes,
            Err("missing") => continue,
            Err(reason) => return Err(Q(reason)),
        };
        let v: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|_| Q("registry_protocol"))?;
        let process = host.process(pid).map_err(Q)?;
        if process.uid != host.uid()
            || v["pid"].as_u64() != Some(pid as u64)
            || v["procStart"].as_str() != Some(process.registry_start.as_str())
        {
            tracing::warn!(
                pid_matches = v["pid"].as_u64() == Some(pid as u64),
                uid_matches = process.uid == host.uid(),
                lstart_matches = v["procStart"].as_str() == Some(process.registry_start.as_str()),
                measured = true,
                n_considered = 1,
                verdict = "native_registry_process_mismatch",
                "registry identity rejected before socket discovery"
            );
            return Err(Q("registry_process_mismatch"));
        }
        let session = v["sessionId"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or(Q("registry_session_missing"))?;
        if expected_session.is_some_and(|s| s != session) {
            return Err(Q("registry_session_mismatch"));
        }
        if process.executable_sha256 != REVIEWED_SHA256 {
            return Err(U("unsupported_executable_profile"));
        }
        // The hash identifies the reviewed version, rather than trusting a
        // claimed version or a globally installed `claude --version` result.
        if v["peerProtocol"].as_u64() != Some(WIRE_VERSION)
            || !REQUIRED_FEATURES.iter().all(|feature| {
                v["peerFeatures"]
                    .as_array()
                    .is_some_and(|items| items.iter().any(|s| s.as_str() == Some(*feature)))
            })
        {
            return Err(U("unsupported_registry_features"));
        }
        let socket = v["messagingSocketPath"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or(U("no_registered_inbox"))?;
        let socket = PathBuf::from(socket);
        vet_socket(&socket, host.uid()).map_err(Q)?;
        return Ok(Target {
            process,
            registry,
            registry_sha256: registry_identity(&v),
            socket,
            session: session.into(),
            config_dir: config_dir.into(),
        });
    }
    Err(U("no_verified_registry"))
}

/// /clear rotates the conversation inside the same pane-bound process. Resolve
/// its current, fully verified registry once before treating metadata as identity failure.
pub(super) fn discover_current(
    host: &dyn Host,
    config: &Path,
    pids: &[u32],
    expected: Option<&str>,
) -> Result<Target, DiscoveryError> {
    match discover(host, config, pids, expected) {
        Err(DiscoveryError::Quarantined("registry_session_mismatch")) => {
            let mut current = None;
            for &pid in pids {
                match discover(host, config, &[pid], None) {
                    Ok(target) => {
                        if current.is_some() {
                            return Err(DiscoveryError::Quarantined("registry_session_ambiguous"));
                        }
                        current = Some(target);
                    }
                    Err(DiscoveryError::Unavailable(_)) => continue,
                    Err(error) => return Err(error),
                }
            }
            let current = current.ok_or(DiscoveryError::Unavailable("no_verified_registry"))?;
            tracing::info!(
                endpoint_pid = current.process.pid,
                measured = true,
                n_considered = 1,
                verdict = "native_receiver_session_refreshed",
                "stale conversation re-resolved from the pane-bound registry"
            );
            Ok(current)
        }
        result => result,
    }
}

pub(super) fn revalidate(host: &dyn Host, target: &Target) -> Result<(), &'static str> {
    if host.process(target.process.pid)? != target.process {
        return Err("endpoint_process_changed");
    }
    let record = private_read(&target.registry, host.uid(), MAX_REGISTRY_BYTES)?;
    let record: serde_json::Value =
        serde_json::from_slice(&record).map_err(|_| "registry_protocol")?;
    if registry_identity(&record) != target.registry_sha256 {
        return Err("endpoint_registry_changed");
    }
    vet_socket(&target.socket, host.uid())
}

fn safe_components(path: &Path, uid: u32) -> Result<(), &'static str> {
    if !path.is_absolute()
        || path
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
    {
        return Err("unsafe_path");
    }
    let mut walked = PathBuf::new();
    for c in path.components() {
        walked.push(c);
        let m = std::fs::symlink_metadata(&walked).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                "missing"
            } else {
                "path_unreadable"
            }
        })?;
        if m.file_type().is_symlink() {
            // macOS publishes /tmp and /var aliases. Only root-owned
            // ancestor aliases may resolve through a separately vetted
            // target; the socket, private parent and data files cannot link.
            if walked == path || m.uid() != 0 {
                return Err("symlink_path");
            }
            let resolved =
                std::fs::canonicalize(&walked).map_err(|_| "ancestor_alias_unverifiable")?;
            safe_components(&resolved, uid)?;
            continue;
        }
        if m.uid() != uid && m.uid() != 0 {
            return Err("foreign_path_owner");
        }
        if m.is_dir() && m.mode() & 0o022 != 0 && m.mode() & 0o1000 == 0 {
            return Err("writable_ancestor");
        }
    }
    Ok(())
}

pub(super) fn vet_directory(path: &Path, uid: u32) -> Result<(), &'static str> {
    safe_components(path, uid)?;
    let m = std::fs::symlink_metadata(path).map_err(|_| "directory_unreadable")?;
    if !m.is_dir() || m.uid() != uid || m.mode() & 0o077 != 0 {
        return Err("unsafe_private_directory");
    }
    Ok(())
}

pub(super) fn vet_socket(path: &Path, uid: u32) -> Result<(), &'static str> {
    if path.as_os_str().len() > MAX_SOCKET_PATH_BYTES {
        return Err("socket_path_cap");
    }
    safe_components(path, uid)?;
    vet_directory(path.parent().ok_or("socket_parent")?, uid)?;
    let m = std::fs::symlink_metadata(path).map_err(|_| "socket_unreadable")?;
    if !m.file_type().is_socket() || m.uid() != uid || m.mode() & 0o077 != 0 {
        return Err("unsafe_socket");
    }
    Ok(())
}

pub(super) fn private_read(path: &Path, uid: u32, cap: usize) -> Result<Vec<u8>, &'static str> {
    safe_components(path, uid)?;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| "file_open")?;
    let m = file.metadata().map_err(|_| "file_stat")?;
    if !m.is_file() || m.uid() != uid || m.mode() & 0o077 != 0 {
        return Err("unsafe_private_file");
    }
    let mut bytes = Vec::new();
    file.take(cap as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "file_read")?;
    if bytes.len() > cap {
        return Err("file_cap");
    }
    Ok(bytes)
}

pub(super) fn auth_token(host: &dyn Host, target: &Target) -> Result<Option<String>, &'static str> {
    let key = target.config_dir.join("sessions").join(format!(
        "{}.{}.key",
        target.process.pid,
        wire::sha256(target.socket.as_os_str().as_encoded_bytes())
    ));
    let bytes = match private_read(&key, host.uid(), super::MAX_AUTH_FILE_BYTES) {
        Ok(bytes) => bytes,
        Err("missing") => return Ok(None), // optional auth in this macOS profile
        Err(reason) => return Err(reason),
    };
    let v: serde_json::Value = serde_json::from_slice(&bytes).map_err(|_| "auth_protocol")?;
    if v["procStart"].as_str() != Some(target.process.registry_start.as_str()) {
        return Err("auth_process_mismatch");
    }
    let token = v["peerToken"].as_str().ok_or("auth_protocol")?;
    if token.len() != super::AUTH_TOKEN_HEX_CHARS || !token.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("auth_protocol");
    }
    Ok(Some(token.into()))
}

pub(super) fn peer(stream: &tokio::net::UnixStream) -> Result<(u32, u32), &'static str> {
    let cred = stream
        .peer_cred()
        .map_err(|_| "endpoint_credentials_unavailable")?;
    let pid = cred
        .pid()
        .and_then(|p| u32::try_from(p).ok())
        .filter(|p| *p > 1)
        .ok_or("endpoint_pid_unavailable")?;
    Ok((pid, cred.uid()))
}

#[cfg(any(target_os = "macos", test))]
#[derive(Debug, PartialEq, Eq)]
struct ExecutableStamp {
    device: u64,
    inode: u64,
    size: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}
#[cfg(any(target_os = "macos", test))]
impl From<&std::fs::Metadata> for ExecutableStamp {
    fn from(m: &std::fs::Metadata) -> Self {
        Self {
            device: m.dev(),
            inode: m.ino(),
            size: m.len(),
            modified: (m.mtime(), m.mtime_nsec()),
            changed: (m.ctime(), m.ctime_nsec()),
        }
    }
}

#[cfg(any(target_os = "macos", test))]
#[derive(Default)]
pub(super) struct ExecutableHashes {
    entries: std::collections::HashMap<PathBuf, (ExecutableStamp, String)>,
    #[cfg(test)]
    pub hashes: usize,
}
#[cfg(any(target_os = "macos", test))]
impl ExecutableHashes {
    pub fn hash(
        &mut self,
        path: &Path,
        file: &mut std::fs::File,
        before: &std::fs::Metadata,
    ) -> Result<String, &'static str> {
        let stamp = ExecutableStamp::from(before);
        if let Some((cached, hash)) = self.entries.get(path) {
            if cached == &stamp {
                return Ok(hash.clone());
            }
        }
        use sha2::Digest;
        let mut hash = sha2::Sha256::new();
        let mut buf = [0u8; super::EXECUTABLE_HASH_BUFFER_BYTES];
        loop {
            let n = file.read(&mut buf).map_err(|_| "executable_read")?;
            if n == 0 {
                break;
            }
            hash.update(&buf[..n]);
        }
        let after = file.metadata().map_err(|_| "executable_stat")?;
        if stamp != ExecutableStamp::from(&after) {
            return Err("executable_changed_during_read");
        }
        let hash = hex::encode(hash.finalize());
        // Cache one fingerprint per path; bound retained versions across upgrades.
        const MAX_EXECUTABLE_PATHS: usize = 64;
        if self.entries.len() >= MAX_EXECUTABLE_PATHS {
            self.entries.clear();
        }
        self.entries.insert(path.into(), (stamp, hash.clone()));
        #[cfg(test)]
        {
            self.hashes += 1;
        }
        Ok(hash)
    }
}

#[cfg(target_os = "macos")]
fn process(pid: u32) -> Result<ProcessIdentity, &'static str> {
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of_val(&info) as i32;
    // SAFETY: correctly sized kernel output buffer; no pointers escape.
    let got = unsafe {
        libc::proc_pidinfo(
            pid as i32,
            libc::PROC_PIDTBSDINFO,
            0,
            (&mut info as *mut libc::proc_bsdinfo).cast(),
            size,
        )
    };
    if got != size {
        return Err("process_unverifiable");
    }
    let mut path = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    let n = unsafe { libc::proc_pidpath(pid as i32, path.as_mut_ptr().cast(), path.len() as u32) };
    if n <= 0 {
        return Err("executable_unverifiable");
    }
    path.truncate(path.iter().position(|b| *b == 0).unwrap_or(path.len()));
    use std::os::unix::ffi::OsStringExt;
    let executable = PathBuf::from(std::ffi::OsString::from_vec(path));
    let mut file = std::fs::File::open(&executable).map_err(|_| "executable_unreadable")?;
    let before = file.metadata().map_err(|_| "executable_stat")?;
    if before.mtime() > info.pbi_start_tvsec as i64 {
        return Err("executable_changed_since_start");
    }
    static HASHES: std::sync::OnceLock<std::sync::Mutex<ExecutableHashes>> =
        std::sync::OnceLock::new();
    let executable_sha256 = HASHES
        .get_or_init(Default::default)
        .lock()
        .map_err(|_| "executable_cache_poisoned")?
        .hash(&executable, &mut file, &before)?;
    use wait_timeout::ChildExt;
    let mut child = std::process::Command::new("/bin/ps")
        .args(["-o", "lstart=", "-p", &pid.to_string()])
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|_| "process_start_unreadable")?;
    let status = match child
        .wait_timeout(super::PROCESS_TIMEOUT)
        .map_err(|_| "process_start_wait")?
    {
        Some(status) => status,
        None => {
            let _ = child.kill();
            let _ = child.wait();
            return Err("process_start_timeout");
        }
    };
    if !status.success() {
        return Err("process_start_unreadable");
    }
    let mut output = Vec::new();
    child
        .stdout
        .take()
        .ok_or("process_start_output")?
        .take(super::MAX_PROCESS_START_BYTES)
        .read_to_end(&mut output)
        .map_err(|_| "process_start_read")?;
    let registry_start = String::from_utf8(output)
        .map_err(|_| "process_start_invalid")?
        .trim()
        .to_string();
    if registry_start.is_empty() {
        return Err("process_start_empty");
    }
    Ok(ProcessIdentity {
        pid,
        uid: info.pbi_uid,
        registry_start,
        instance: format!("{}:{}", info.pbi_start_tvsec, info.pbi_start_tvusec),
        executable,
        executable_sha256,
    })
}

#[cfg(not(target_os = "macos"))]
fn process(_: u32) -> Result<ProcessIdentity, &'static str> {
    Err("unreviewed_platform")
}
