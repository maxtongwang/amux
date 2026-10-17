//! Attribute a local request to the PROCESS that opened its socket, not to the
//! headers it chose to send (fix/worker-attribution).
//!
//! THE HOLE. The server read a worker's identity only from `X-Amux-Worker` /
//! `X-Amux-Session`, and a loopback request with neither is the owner (auth's
//! loopback bypass; the CLI also reads `~/.amux/auth_token`, which every worker
//! can read because every worker runs as the owner's user). Codex's default
//! `shell_environment_policy.inherit = "core"` strips `AMUX_*` and `TMUX_PANE`
//! from tool shells, so a Codex worker's `amux send` carried no identity and
//! crossed its group boundary as unstamped OWNER input. Measured 2026-10-16:
//! `env -i ... amux send re-ops-lead` from a `dev` worker's pane did the same.
//!
//! THE RULE. For every request over a real socket the server finds the local
//! process that owns the client end of the connection and walks its parent
//! chain. If an ancestor is the shell of a tmux pane in session `amux-<worker>`,
//! the request IS that worker, whatever it sent:
//!
//! - a missing worker header is filled with that worker, a different one is
//!   overridden (logged `worker_identity_overridden`), and a member marker is
//!   dropped, so it is never the owner, owner token or not;
//! - `x-amux-process-worker` carries the proof to `worker_identity::enforce`,
//!   which accepts it in place of the rule-10 token (process ancestry is the
//!   stronger evidence, and a sandbox can strip the token's env var too).
//!
//! A loopback request whose owning process or ancestry cannot be established is
//! NOT given the owner's authority: with no worker header, an `/api/` request is
//! refused 403 `worker_origin_unattributed`. The owner is allowed only when the
//! process is provably outside every amux pane (the dashboard browser, the
//! owner's own terminal, launchd jobs) or the peer is genuinely remote (no
//! local process owns the socket and the address is not loopback: the phone).
//!
//! On a platform without a resolver the layer does nothing and says so once
//! (`process_attribution_unavailable`, measured=false): that is today's
//! behaviour, reported instead of silently assumed.
//!
//! WHAT THIS DOES NOT STOP. Workers run as the owner's uid. A process that
//! daemonizes out of its pane (double fork, `setsid`, its own tmux server) is
//! re-parented to launchd and reads as outside; a worker can also drive tmux or
//! edit `~/.amux` files without the server at all. This closes the accidental
//! and naive paths (stripped env, `env -i`), which are the measured ones; a
//! hostile same-uid process needs an OS sandbox, not an HTTP check.
//!
//! COMPOSITION. This runs before auth and before any rename-alias resolution
//! (maxtongwang/amux#3): identity comes from the process first, and an alias
//! layer may then map the attributed name to its current one.

use axum::extract::{ConnectInfo, Request};
use axum::http::{HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant};

/// Internal proof header: set only by [`attribute`], stripped from every
/// inbound request first, so a client cannot assert it.
pub(crate) const PROCESS_WORKER_HEADER: &str = "x-amux-process-worker";

/// Who opened the connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Origin {
    /// A process inside the tmux pane of worker session `amux-<worker>`.
    Worker(String),
    /// A local process whose whole ancestry is outside every amux pane.
    Outside,
    /// No local process owns the client end and the peer is not loopback.
    Remote,
    /// Local, but the owner or its ancestry could not be established.
    Unattributed(String),
    /// This platform has no resolver.
    Unavailable(&'static str),
}

/// Resolves a peer address to an [`Origin`]. A trait so the attribution policy
/// is tested with a fake while the OS lookups are tested for what they read.
pub(crate) trait PeerResolver: Send + Sync {
    fn resolve(&self, peer: SocketAddr) -> Origin;
}

/// The pure classification: walk each owning process's parent chain until a
/// pane pid (`panes`: pane shell pid -> tmux session name) or the root.
///
/// A pane of a tmux session NOT named `amux-*` is the owner's own tmux and the
/// walk continues past it. A chain that breaks before the root (an unreadable
/// ancestor) is unattributed, never outside: outside must be proven.
pub(crate) fn classify(
    owners: &[u32],
    panes: &HashMap<u32, String>,
    ppid_of: &dyn Fn(u32) -> Option<u32>,
) -> Origin {
    if owners.is_empty() {
        return Origin::Unattributed("no local process owns the client socket".into());
    }
    let mut broken = None;
    for &owner in owners {
        let mut cur = owner;
        for _ in 0..4096 {
            if let Some(worker) = panes.get(&cur).and_then(|s| s.strip_prefix("amux-")) {
                if !worker.is_empty() {
                    return Origin::Worker(worker.to_string());
                }
            }
            if cur <= 1 {
                break;
            }
            match ppid_of(cur) {
                Some(p) if p != cur => cur = p,
                _ => {
                    broken = Some(format!("ancestry of pid {owner} breaks at pid {cur}"));
                    break;
                }
            }
        }
    }
    match broken {
        Some(why) => Origin::Unattributed(why),
        None => Origin::Outside,
    }
}

/// The OS resolver: socket owner lookup, parent lookups, and the tmux pane map.
struct OsResolver;

impl PeerResolver for OsResolver {
    fn resolve(&self, peer: SocketAddr) -> Origin {
        let peer = SocketAddr::new(peer.ip().to_canonical(), peer.port());
        decide(peer, os::socket_owners(peer), pane_map, &os::ppid_of)
    }
}

/// The resolver's decision from its three lookups, separated so a failed
/// lookup is tested through the same code the server runs. The pane inventory
/// is only consulted for a local socket, and a FAILED inventory is unproven
/// evidence: it never classifies a worker's process as outside.
fn decide(
    peer: SocketAddr,
    owners: Result<Vec<u32>, os::LookupError>,
    panes: impl FnOnce() -> Result<HashMap<u32, String>, String>,
    ppid_of: &dyn Fn(u32) -> Option<u32>,
) -> Origin {
    let owners = match owners {
        Ok(o) => o,
        Err(os::LookupError::Unsupported(why)) => return Origin::Unavailable(why),
        Err(os::LookupError::Failed(why)) => {
            return if peer.ip().is_loopback() { Origin::Unattributed(why) } else { Origin::Remote };
        }
    };
    if owners.is_empty() && !peer.ip().is_loopback() {
        return Origin::Remote;
    }
    match panes() {
        Ok(panes) => classify(&owners, &panes, ppid_of),
        Err(why) => {
            tracing::warn!(%peer, why = %why, verdict = "process_attribution_pane_inventory_failed",
                measured = false, why_unmeasured = %why, n_considered = owners.len(),
                "tmux pane inventory failed; this local request cannot be proven outside amux panes");
            Origin::Unattributed(format!("tmux pane inventory failed: {why}"))
        }
    }
}

type PaneCache = Mutex<Option<(Instant, HashMap<u32, String>)>>;

/// `pane shell pid -> tmux session name`, cached briefly: one `tmux` spawn per
/// window, not per request. Only a successful inventory is cached; a failure
/// is returned as an error every time so it can never stand in for "no panes".
fn pane_map() -> Result<HashMap<u32, String>, String> {
    static CACHE: OnceLock<PaneCache> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(None));
    if let Ok(g) = cache.lock() {
        if let Some((at, map)) = g.as_ref() {
            if at.elapsed() < Duration::from_secs(2) {
                return Ok(map.clone());
            }
        }
    }
    let out = std::process::Command::new("tmux")
        .args(["list-panes", "-a", "-F", "#{pane_pid} #{session_name}"])
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("could not run tmux: {e}"))?;
    let map = panes_from_output(out.status.success(), &String::from_utf8_lossy(&out.stdout), &String::from_utf8_lossy(&out.stderr))?;
    if let Ok(mut g) = cache.lock() {
        *g = Some((Instant::now(), map.clone()));
    }
    Ok(map)
}

/// A `tmux list-panes` result as an inventory. Only tmux reporting that no
/// server runs is an authoritative empty inventory; any other failure is an
/// error, never an empty map.
fn panes_from_output(ok: bool, stdout: &str, stderr: &str) -> Result<HashMap<u32, String>, String> {
    if ok {
        return Ok(parse_panes(stdout));
    }
    // Absence is established only when the socket is not there: tmux says "no
    // server running", or (tmux 3.6a, measured) "error connecting to <socket>
    // (No such file or directory)". The backend launches every worker on this
    // same default socket, so no socket means no worker pane. Any other
    // connection error (review round 2: "Permission denied" can hide a running
    // server; "Connection refused") is a FAILED lookup and an error.
    let err = stderr.trim();
    let socket_missing = err.starts_with("error connecting to") && err.ends_with("(No such file or directory)");
    if err.starts_with("no server running") || socket_missing {
        return Ok(HashMap::new());
    }
    Err(if err.is_empty() { "tmux list-panes failed with no message".into() } else { err.chars().take(200).collect() })
}

/// `<pane_pid> <session_name>` lines.
pub(crate) fn parse_panes(text: &str) -> HashMap<u32, String> {
    text.lines()
        .filter_map(|l| {
            let (pid, sess) = l.trim().split_once(' ')?;
            Some((pid.parse().ok()?, sess.trim().to_string()))
        })
        .collect()
}

type Shared = Arc<dyn PeerResolver>;

fn resolver_slot() -> &'static RwLock<Shared> {
    static SLOT: OnceLock<RwLock<Shared>> = OnceLock::new();
    SLOT.get_or_init(|| RwLock::new(Arc::new(OsResolver)))
}

type OriginCache = Mutex<HashMap<SocketAddr, (Instant, Origin)>>;

/// Per-connection memo: a keep-alive connection keeps its client port for its
/// life, so one lookup serves its requests. Short TTL bounds port reuse.
fn origin_cache() -> &'static OriginCache {
    static CACHE: OnceLock<OriginCache> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

const ORIGIN_TTL: Duration = Duration::from_secs(3);

async fn resolve_cached(peer: SocketAddr) -> Origin {
    if let Ok(c) = origin_cache().lock() {
        if let Some((at, o)) = c.get(&peer) {
            if at.elapsed() < ORIGIN_TTL {
                return o.clone();
            }
        }
    }
    let resolver = resolver_slot().read().map(|r| r.clone()).unwrap_or_else(|p| p.into_inner().clone());
    let origin = tokio::task::spawn_blocking(move || resolver.resolve(peer))
        .await
        .unwrap_or_else(|e| Origin::Unattributed(format!("resolver task failed: {e}")));
    if let Ok(mut c) = origin_cache().lock() {
        if c.len() > 4096 {
            c.retain(|_, (at, _)| at.elapsed() < ORIGIN_TTL);
        }
        c.insert(peer, (Instant::now(), origin.clone()));
    }
    origin
}

/// The claimed worker identity, as every handler reads it.
fn claimed(req: &Request) -> String {
    super::groups::hdr_worker(req.headers())
}

/// The middleware. Outermost after member identity, so every later layer and
/// handler (auth, rule 10, the send gate, membership) sees the attributed
/// identity.
pub async fn attribute(mut req: Request, next: Next) -> Response {
    req.headers_mut().remove(PROCESS_WORKER_HEADER);
    let Some(peer) = req.extensions().get::<ConnectInfo<SocketAddr>>().map(|c| c.0) else {
        // In-process calls (the server acting for a lane) have no socket.
        return next.run(req).await;
    };
    // The SERVER's own environment, never the request: a throwaway test server
    // launched from inside a worker pane would otherwise attribute its own
    // test client to that pane. Said once, at WARN, so it cannot pass unseen.
    if std::env::var("AMUX_PROCESS_ATTRIBUTION").is_ok_and(|v| v.trim() == "0") {
        static ONCE: OnceLock<()> = OnceLock::new();
        ONCE.get_or_init(|| {
            tracing::warn!(verdict = "process_attribution_disabled", measured = false, n_considered = 0,
                why_unmeasured = "AMUX_PROCESS_ATTRIBUTION=0 in the server environment",
                "local requests are attributed by headers only");
        });
        return next.run(req).await;
    }
    match resolve_cached(peer).await {
        Origin::Worker(worker) if !super::session_verbs::valid_session_name(&worker) => {
            // A pane in an `amux-*` session whose name is no valid worker name:
            // worker-originated, with no identity the server could act on.
            tracing::warn!(worker = %worker, path = %req.uri().path(), verdict = "worker_origin_invalid",
                measured = true, n_considered = 1,
                "a request from an amux pane carries no valid worker identity; refused, not treated as the owner");
            (StatusCode::FORBIDDEN, Json(json!({
                "ok": false,
                "code": "worker_origin_invalid",
                "error": "this request comes from an amux worker pane whose session name is not a valid worker name, so it is neither a worker nor the owner",
            }))).into_response()
        }
        Origin::Worker(worker) => {
            let claim = claimed(&req);
            if !claim.is_empty() && claim != worker {
                tracing::warn!(worker = %worker, claimed = %claim, path = %req.uri().path(),
                    verdict = "worker_identity_overridden", measured = true, n_considered = 1,
                    "a worker process claimed another identity; attributed to its own pane");
            }
            if let Ok(v) = HeaderValue::from_str(&worker) {
                let h = req.headers_mut();
                h.insert("x-amux-worker", v.clone());
                h.insert("x-amux-session", v.clone());
                h.insert(PROCESS_WORKER_HEADER, v);
            }
            super::org::strip_member_headers(req.headers_mut());
            next.run(req).await
        }
        Origin::Unattributed(why) => {
            let path = req.uri().path().to_string();
            if claimed(&req).is_empty() && path.starts_with("/api/") {
                tracing::warn!(%peer, path = %path, why = %why, verdict = "worker_origin_unattributed",
                    measured = true, n_considered = 1,
                    "a local request could not be attributed to a process outside amux panes; not treated as the owner");
                return (StatusCode::FORBIDDEN, Json(json!({
                    "ok": false,
                    "code": "worker_origin_unattributed",
                    "error": "this local request could not be traced to a process outside every amux worker pane, so it is not owner input",
                    "why": why,
                }))).into_response();
            }
            next.run(req).await
        }
        Origin::Unavailable(why) => {
            static ONCE: OnceLock<()> = OnceLock::new();
            ONCE.get_or_init(|| {
                tracing::warn!(why, verdict = "process_attribution_unavailable", measured = false,
                    why_unmeasured = why, n_considered = 0,
                    "local requests are attributed by headers only on this platform");
            });
            next.run(req).await
        }
        Origin::Outside | Origin::Remote => next.run(req).await,
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;

    static SERIAL: Mutex<()> = Mutex::new(());

    /// Install a fake resolver for the guard's life; tests that use it run
    /// one at a time and clear the per-connection memo on both edges.
    pub(crate) struct ResolverGuard {
        _serial: std::sync::MutexGuard<'static, ()>,
    }

    impl Drop for ResolverGuard {
        fn drop(&mut self) {
            if let Ok(mut s) = resolver_slot().write() {
                *s = Arc::new(OsResolver);
            }
            if let Ok(mut c) = origin_cache().lock() {
                c.clear();
            }
        }
    }

    pub(crate) fn set_resolver(r: Shared) -> ResolverGuard {
        let serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
        if let Ok(mut s) = resolver_slot().write() {
            *s = r;
        }
        if let Ok(mut c) = origin_cache().lock() {
            c.clear();
        }
        ResolverGuard { _serial: serial }
    }

    /// A resolver that answers by client port.
    pub(crate) struct ByPort(pub HashMap<u16, Origin>);

    impl PeerResolver for ByPort {
        fn resolve(&self, peer: SocketAddr) -> Origin {
            self.0.get(&peer.port()).cloned().unwrap_or(Origin::Outside)
        }
    }
}

/// Platform lookups. Each answers "which local pids hold a TCP socket whose
/// LOCAL end is `peer`" (the client end of a connection to this server) and
/// "who is this pid's parent".
pub(crate) mod os {
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    use std::net::SocketAddr;

    pub(crate) enum LookupError {
        #[cfg_attr(any(target_os = "macos", target_os = "linux"), allow(dead_code))]
        Unsupported(&'static str),
        Failed(String),
    }

    #[cfg(target_os = "macos")]
    pub(super) use super::macos::{ppid_of, socket_owners};
    #[cfg(target_os = "linux")]
    pub(super) use super::linux::{ppid_of, socket_owners};

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    pub(super) fn socket_owners(_peer: SocketAddr) -> Result<Vec<u32>, LookupError> {
        Err(LookupError::Unsupported("no socket-owner lookup on this platform"))
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    pub(super) fn ppid_of(_pid: u32) -> Option<u32> {
        None
    }
}

/// macOS: libproc. `proc_pidfdinfo(PROC_PIDFDSOCKETINFO)` fills a
/// `struct socket_fdinfo` (sys/proc_info.h), which libc does not declare; the
/// prefix up to `soi_proto` and `in_sockinfo` are declared here with C layout,
/// and the union is over-allocated (the kernel requires buffersize >= its
/// size). `macos_self_connection_resolves_to_this_process` proves the layout.
#[cfg(target_os = "macos")]
#[allow(dead_code)] // C layout fields the kernel fills but this lookup never reads
mod macos {
    use super::os::LookupError;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

    const PROC_PIDFDSOCKETINFO: libc::c_int = 3;
    const SOCKINFO_IN: i32 = 1;
    const SOCKINFO_TCP: i32 = 2;
    const INI_IPV4: u8 = 0x1;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct ProcFileInfo {
        fi_openflags: u32,
        fi_status: u32,
        fi_offset: i64,
        fi_type: i32,
        fi_guardflags: u32,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct VinfoStat {
        vst_dev: u32,
        vst_mode: u16,
        vst_nlink: u16,
        vst_ino: u64,
        vst_uid: u32,
        vst_gid: u32,
        vst_times: [i64; 8],
        vst_size: i64,
        vst_blocks: i64,
        vst_blksize: i32,
        vst_flags: u32,
        vst_gen: u32,
        vst_rdev: u32,
        vst_qspare: [i64; 2],
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct SockbufInfo {
        sbi_cc: u32,
        sbi_hiwat: u32,
        sbi_mbcnt: u32,
        sbi_mbmax: u32,
        sbi_lowat: u32,
        sbi_flags: i16,
        sbi_timeo: i16,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct InSockinfo {
        insi_fport: i32,
        insi_lport: i32,
        insi_gencnt: u64,
        insi_flags: u32,
        insi_flow: u32,
        insi_vflag: u8,
        insi_ip_ttl: u8,
        rfu_1: u32,
        insi_faddr: [u8; 16],
        insi_laddr: [u8; 16],
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    union SoiProto {
        pri_in: InSockinfo,
        _pad: [u64; 80],
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct SocketInfo {
        soi_stat: VinfoStat,
        soi_so: u64,
        soi_pcb: u64,
        soi_type: i32,
        soi_protocol: i32,
        soi_family: i32,
        soi_options: i16,
        soi_linger: i16,
        soi_state: i16,
        soi_qlen: i16,
        soi_incqlen: i16,
        soi_qlimit: i16,
        soi_timeo: i16,
        soi_error: u16,
        soi_oobmark: u32,
        soi_rcv: SockbufInfo,
        soi_snd: SockbufInfo,
        soi_kind: i32,
        rfu_1: u32,
        soi_proto: SoiProto,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct SocketFdInfo {
        pfi: ProcFileInfo,
        psi: SocketInfo,
    }

    // Layout pins against sys/proc_info.h offsets (lsof and libproc-rs agree).
    const _: () = assert!(std::mem::size_of::<VinfoStat>() == 136);
    const _: () = assert!(std::mem::offset_of!(SocketInfo, soi_kind) == 232);
    const _: () = assert!(std::mem::offset_of!(SocketInfo, soi_proto) == 240);
    const _: () = assert!(std::mem::offset_of!(InSockinfo, insi_laddr) == 48);

    fn all_pids() -> Vec<i32> {
        // SAFETY: a null buffer asks for the count; the second call writes at
        // most `buf.len()` pids into an owned, correctly sized buffer.
        let n = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
        if n <= 0 {
            return vec![];
        }
        let mut buf = vec![0i32; n as usize + 64];
        let bytes = (buf.len() * std::mem::size_of::<i32>()) as libc::c_int;
        let got = unsafe { libc::proc_listallpids(buf.as_mut_ptr().cast(), bytes) };
        if got <= 0 {
            return vec![];
        }
        buf.truncate(got as usize);
        buf
    }

    fn socket_fds(pid: i32) -> Vec<i32> {
        // SAFETY: as above; the kernel writes whole proc_fdinfo records.
        let need = unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDLISTFDS, 0, std::ptr::null_mut(), 0) };
        if need <= 0 {
            return vec![];
        }
        let size = std::mem::size_of::<libc::proc_fdinfo>();
        let mut buf: Vec<libc::proc_fdinfo> = Vec::with_capacity(need as usize / size + 16);
        let cap = (buf.capacity() * size) as libc::c_int;
        let got = unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDLISTFDS, 0, buf.as_mut_ptr().cast(), cap) };
        if got <= 0 {
            return vec![];
        }
        // SAFETY: `got` bytes of initialised records were written.
        unsafe { buf.set_len(got as usize / size) };
        buf.iter()
            .filter(|f| f.proc_fdtype as libc::c_int == libc::PROX_FDTYPE_SOCKET)
            .map(|f| f.proc_fd)
            .collect()
    }

    fn local_end(pid: i32, fd: i32) -> Option<SocketAddr> {
        // SAFETY: zeroed POD; the kernel writes at most size_of bytes.
        let mut info: SocketFdInfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<SocketFdInfo>() as libc::c_int;
        let got = unsafe {
            libc::proc_pidfdinfo(pid, fd, PROC_PIDFDSOCKETINFO, (&mut info as *mut SocketFdInfo).cast(), size)
        };
        if got <= 0 {
            return None;
        }
        let kind = info.psi.soi_kind;
        if kind != SOCKINFO_IN && kind != SOCKINFO_TCP {
            return None;
        }
        // SAFETY: both TCP and IN kinds begin the union with in_sockinfo.
        let ini = unsafe { info.psi.soi_proto.pri_in };
        let port = u16::from_be(ini.insi_lport as u16);
        let ip = if ini.insi_vflag & INI_IPV4 != 0 {
            let a = &ini.insi_laddr;
            IpAddr::V4(Ipv4Addr::new(a[12], a[13], a[14], a[15]))
        } else {
            IpAddr::V6(Ipv6Addr::from(ini.insi_laddr)).to_canonical()
        };
        Some(SocketAddr::new(ip, port))
    }

    pub(crate) fn socket_owners(peer: SocketAddr) -> Result<Vec<u32>, LookupError> {
        let pids = all_pids();
        if pids.is_empty() {
            return Err(LookupError::Failed("proc_listallpids returned nothing".into()));
        }
        let mut owners = vec![];
        for pid in pids {
            if pid <= 0 {
                continue;
            }
            if socket_fds(pid).into_iter().any(|fd| local_end(pid, fd) == Some(peer)) {
                owners.push(pid as u32);
            }
        }
        Ok(owners)
    }

    pub(crate) fn ppid_of(pid: u32) -> Option<u32> {
        // SAFETY: zeroed POD; the kernel writes exactly one proc_bsdinfo.
        let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
        let got = unsafe {
            libc::proc_pidinfo(pid as i32, libc::PROC_PIDTBSDINFO, 0, (&mut info as *mut libc::proc_bsdinfo).cast(), size)
        };
        (got == size).then_some(info.pbi_ppid)
    }
}

/// Linux: `/proc/net/tcp{,6}` gives the socket inode for a local endpoint;
/// `/proc/<pid>/fd` links name the holders; `/proc/<pid>/stat` the parent.
#[cfg(target_os = "linux")]
mod linux {
    use super::os::LookupError;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

    /// `0100007F:1F90` (v4) or 32 hex digits (v6), each 32-bit word printed
    /// in host order.
    pub(crate) fn parse_endpoint(field: &str) -> Option<SocketAddr> {
        let (addr, port) = field.split_once(':')?;
        let port = u16::from_str_radix(port, 16).ok()?;
        let word = |s: &str| u32::from_str_radix(s, 16).ok().map(u32::to_ne_bytes);
        let ip = match addr.len() {
            8 => IpAddr::V4(Ipv4Addr::from(word(addr)?)),
            32 => {
                let mut b = [0u8; 16];
                for i in 0..4 {
                    b[i * 4..i * 4 + 4].copy_from_slice(&word(&addr[i * 8..i * 8 + 8])?);
                }
                IpAddr::V6(Ipv6Addr::from(b)).to_canonical()
            }
            _ => return None,
        };
        Some(SocketAddr::new(ip, port))
    }

    fn inode_for(peer: SocketAddr) -> Result<Option<u64>, LookupError> {
        let mut read_any = false;
        for table in ["/proc/net/tcp", "/proc/net/tcp6"] {
            let Ok(text) = std::fs::read_to_string(table) else { continue };
            read_any = true;
            for line in text.lines().skip(1) {
                let cols: Vec<&str> = line.split_whitespace().collect();
                if cols.len() > 9 && parse_endpoint(cols[1]) == Some(peer) {
                    if let Ok(inode) = cols[9].parse::<u64>() {
                        if inode != 0 {
                            return Ok(Some(inode));
                        }
                    }
                }
            }
        }
        if read_any { Ok(None) } else { Err(LookupError::Failed("/proc/net/tcp unreadable".into())) }
    }

    pub(crate) fn socket_owners(peer: SocketAddr) -> Result<Vec<u32>, LookupError> {
        let Some(inode) = inode_for(peer)? else { return Ok(vec![]) };
        let want = format!("socket:[{inode}]");
        let mut owners = vec![];
        let procs = std::fs::read_dir("/proc").map_err(|e| LookupError::Failed(e.to_string()))?;
        for entry in procs.flatten() {
            let Some(pid) = entry.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else { continue };
            let Ok(fds) = std::fs::read_dir(entry.path().join("fd")) else { continue };
            if fds.flatten().any(|fd| std::fs::read_link(fd.path()).is_ok_and(|l| l.as_os_str() == want.as_str())) {
                owners.push(pid);
            }
        }
        Ok(owners)
    }

    pub(crate) fn ppid_of(pid: u32) -> Option<u32> {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let rest = &stat[stat.rfind(')')? + 1..];
        rest.split_whitespace().nth(1)?.parse().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn panes() -> HashMap<u32, String> {
        HashMap::from([(100, "amux-dev-a".to_string()), (200, "personal".to_string())])
    }

    fn chain(pid: u32) -> Option<u32> {
        // 101 -> 100 (amux-dev-a pane) -> 50 (tmux server) -> 1
        // 201 -> 200 (owner's own tmux pane) -> 50 -> 1
        // 301 -> 300 -> 1 (owner terminal)
        // 401 -> unreadable parent
        match pid {
            101 => Some(100),
            100 | 200 => Some(50),
            50 => Some(1),
            201 => Some(200),
            301 => Some(300),
            300 => Some(1),
            _ => None,
        }
    }

    #[test]
    fn a_descendant_of_an_amux_pane_is_that_worker_and_other_chains_are_not() {
        let p = panes();
        assert_eq!(classify(&[101], &p, &chain), Origin::Worker("dev-a".into()));
        assert_eq!(classify(&[100], &p, &chain), Origin::Worker("dev-a".into()), "the pane shell itself");
        assert_eq!(classify(&[201], &p, &chain), Origin::Outside, "the owner's own tmux is not a worker");
        assert_eq!(classify(&[301], &p, &chain), Origin::Outside);
        // Shared socket: any holder inside a worker pane makes it that worker.
        assert_eq!(classify(&[301, 101], &p, &chain), Origin::Worker("dev-a".into()));
    }

    #[test]
    fn outside_must_be_proven_never_assumed() {
        let p = panes();
        assert!(matches!(classify(&[], &p, &chain), Origin::Unattributed(_)), "no owner");
        assert!(matches!(classify(&[401], &p, &chain), Origin::Unattributed(_)), "broken chain");
        assert!(matches!(classify(&[301, 401], &p, &chain), Origin::Unattributed(_)), "one unproven holder");
    }

    #[test]
    fn tmux_pane_lines_parse() {
        let m = parse_panes("123 amux-dev-a\n456 personal session\nbad\n");
        assert_eq!(m.get(&123).map(String::as_str), Some("amux-dev-a"));
        assert_eq!(m.get(&456).map(String::as_str), Some("personal session"));
        assert_eq!(m.len(), 2);
    }

    /// The real OS lookup, not a paraphrase: a loopback connection made by
    /// this test process resolves to this pid, and its ancestry walks to the
    /// root without breaking (or finds a pane if the suite runs inside one).
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn os_lookup_resolves_a_loopback_connection_to_this_process() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let client = std::net::TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (_server_side, peer) = listener.accept().unwrap();
        assert_eq!(peer, client.local_addr().unwrap());
        let owners = match os::socket_owners(peer) {
            Ok(o) => o,
            Err(_) => panic!("socket lookup failed on a supported platform"),
        };
        assert!(owners.contains(&std::process::id()), "owners {owners:?} must include {}", std::process::id());
        let me = std::process::id();
        assert!(os::ppid_of(me).is_some(), "parent of this process must be readable");
        let origin = classify(&owners, &HashMap::new(), &os::ppid_of);
        assert_eq!(origin, Origin::Outside, "with no panes, a readable chain is outside");
        // An unrelated port nobody holds is not owned.
        let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap();
        assert!(matches!(os::socket_owners(free), Ok(v) if v.is_empty()));
    }

    // ---- the policy through the real send gate --------------------------
    //
    // A paused target holds a send in steering_queue after the group policy
    // runs, so these reach the shipped send_post without a tmux pane. Each
    // request carries ConnectInfo like a served socket; the fake resolver
    // answers by client port.

    use axum::body::Body;
    use axum::http::Request as HttpRequest;
    use tower::ServiceExt;

    const CLAUDE_WORKER: u16 = 41001; // in amux-caller's pane, sends its headers
    const CODEX_WORKER: u16 = 41002; // in amux-caller's pane, tool shell stripped of AMUX_*
    const SAME_GROUP_CODEX: u16 = 41003; // in amux-peer's pane, no AMUX_* either
    const OWNER: u16 = 41004; // the owner's own terminal or the dashboard browser
    const UNTRACEABLE: u16 = 41005; // local, ancestry unknown
    const BAD_PANE: u16 = 41006; // an amux-* pane whose name is no worker name

    fn fixture() -> (crate::api::AppState, tempfile::TempDir, test_support::ResolverGuard) {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::db::Store::open(&dir.path().join("t.db")).unwrap();
        let state = crate::api::AppState {
            store: Arc::new(store),
            started: Instant::now(),
            build_hash: "test".into(),
            auth_token: Some("owner-secret".into()),
            reconciled: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        };
        let sessions = dir.path().join("sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        std::fs::write(sessions.join("caller.env"), "CC_TAGS=alpha\nCC_SEND_ALLOW=\n").unwrap();
        std::fs::write(sessions.join("peer.env"), "CC_TAGS=beta\n").unwrap();
        std::fs::write(sessions.join("resting.env"), "CC_TAGS=beta\nCC_PAUSED=1\n").unwrap();
        let guard = test_support::set_resolver(Arc::new(test_support::ByPort(HashMap::from([
            (CLAUDE_WORKER, Origin::Worker("caller".into())),
            (CODEX_WORKER, Origin::Worker("caller".into())),
            (SAME_GROUP_CODEX, Origin::Worker("peer".into())),
            (OWNER, Origin::Outside),
            (UNTRACEABLE, Origin::Unattributed("test: chain unreadable".into())),
            (BAD_PANE, Origin::Worker("bad name".into())),
        ]))));
        (state, dir, guard)
    }

    fn app(state: crate::api::AppState) -> axum::Router {
        async fn send(
            axum::extract::State(st): axum::extract::State<crate::api::AppState>,
            axum::extract::Path(name): axum::extract::Path<String>,
            headers: axum::http::HeaderMap,
            axum::Json(body): axum::Json<serde_json::Value>,
        ) -> Response {
            super::super::session_verbs::send_post(&st, &name, &headers, &body).await
        }
        axum::Router::new()
            .route("/api/sessions/{name}/send", axum::routing::post(send))
            .with_state(state)
            .layer(axum::middleware::from_fn(attribute))
    }

    async fn post(app: &axum::Router, port: u16, headers: &[(&str, &str)]) -> (StatusCode, serde_json::Value) {
        let mut b = HttpRequest::builder()
            .method("POST")
            .uri("/api/sessions/resting/send")
            .header("content-type", "application/json")
            .header("authorization", "Bearer owner-secret");
        for (k, v) in headers {
            b = b.header(*k, *v);
        }
        let mut req = b.body(Body::from(json!({"text": "probe"}).to_string())).unwrap();
        req.extensions_mut().insert(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], port))));
        let res = app.clone().oneshot(req).await.unwrap();
        let status = res.status();
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null))
    }

    fn queued_senders(state: &crate::api::AppState) -> Vec<String> {
        let conn = state.store.read().unwrap();
        let mut q = conn.prepare("SELECT sender FROM steering_queue WHERE session='resting' ORDER BY queued_at").unwrap();
        let rows = q.query_map([], |r| r.get::<_, String>(0)).unwrap().flatten().collect();
        rows
    }

    #[tokio::test]
    async fn a_claude_style_worker_is_held_to_its_group() {
        let (state, dir, _r) = fixture();
        let _h = crate::api::settings::test_env::set_home(dir.path());
        let (st, v) = post(&app(state.clone()), CLAUDE_WORKER, &[("x-amux-session", "caller")]).await;
        assert_eq!((st, v["code"].clone()), (StatusCode::FORBIDDEN, json!("worker_group_boundary")), "{v}");
        assert!(queued_senders(&state).is_empty(), "a refused send queues nothing");
    }

    /// THE HOLE: a Codex tool shell (inherit="core") or `env -i amux send`
    /// sends no worker header but does send the owner bearer. Before this
    /// layer that was owner input and skipped the boundary entirely.
    #[tokio::test]
    async fn a_codex_style_worker_without_amux_env_is_still_that_worker_not_the_owner() {
        let (state, dir, _r) = fixture();
        let _h = crate::api::settings::test_env::set_home(dir.path());
        let (st, v) = post(&app(state.clone()), CODEX_WORKER, &[]).await;
        assert_eq!((st, v["code"].clone()), (StatusCode::FORBIDDEN, json!("worker_group_boundary")), "{v}");
        assert!(queued_senders(&state).is_empty(), "owner token from a worker pane is not owner input");
        // Inside its own group it is stamped as itself, never as the owner.
        let (st, v) = post(&app(state.clone()), SAME_GROUP_CODEX, &[]).await;
        assert_eq!(st, StatusCode::OK, "{v}");
        assert_eq!(queued_senders(&state), vec!["peer".to_string()]);
    }

    #[tokio::test]
    async fn the_true_owner_still_sends_as_the_owner() {
        let (state, dir, _r) = fixture();
        let _h = crate::api::settings::test_env::set_home(dir.path());
        let (st, v) = post(&app(state.clone()), OWNER, &[]).await;
        assert_eq!(st, StatusCode::OK, "{v}");
        assert_eq!(queued_senders(&state), vec![String::new()], "owner input carries no worker sender");
    }

    #[tokio::test]
    async fn a_worker_cannot_borrow_another_workers_name() {
        let (state, dir, _r) = fixture();
        let _h = crate::api::settings::test_env::set_home(dir.path());
        // caller (alpha) claims to be peer (beta) to reach a beta target.
        let (st, v) = post(&app(state.clone()), CLAUDE_WORKER, &[("x-amux-session", "peer"), ("x-amux-worker", "peer")]).await;
        assert_eq!((st, v["code"].clone()), (StatusCode::FORBIDDEN, json!("worker_group_boundary")), "{v}");
        assert!(queued_senders(&state).is_empty());
    }

    #[tokio::test]
    async fn unproven_or_invalid_worker_origins_are_refused_not_promoted_to_owner() {
        let (state, dir, _r) = fixture();
        let _h = crate::api::settings::test_env::set_home(dir.path());
        let (st, v) = post(&app(state.clone()), UNTRACEABLE, &[]).await;
        assert_eq!((st, v["code"].clone()), (StatusCode::FORBIDDEN, json!("worker_origin_unattributed")), "{v}");
        let (st, v) = post(&app(state.clone()), BAD_PANE, &[]).await;
        assert_eq!((st, v["code"].clone()), (StatusCode::FORBIDDEN, json!("worker_origin_invalid")), "{v}");
        assert!(queued_senders(&state).is_empty());
    }

    /// Review B1: a failed tmux inventory is unproven evidence. A worker's
    /// process whose pane cannot be listed must be refused, never let through
    /// as the owner, while the same process with a working inventory is that
    /// worker.
    struct InventoryResolver(Result<HashMap<u32, String>, String>);

    impl PeerResolver for InventoryResolver {
        fn resolve(&self, peer: SocketAddr) -> Origin {
            // pid 101's chain runs through pane 100 (amux-caller) when listed.
            let chain = |pid: u32| match pid {
                101 => Some(100),
                100 => Some(50),
                50 => Some(1),
                _ => None,
            };
            decide(peer, Ok(vec![101]), || self.0.clone(), &chain)
        }
    }

    #[tokio::test]
    async fn a_failed_pane_inventory_refuses_instead_of_promoting_a_worker_to_owner() {
        let (state, dir, guard) = fixture();
        let _h = crate::api::settings::test_env::set_home(dir.path());
        drop(guard);
        let failed = test_support::set_resolver(Arc::new(InventoryResolver(Err("tmux exited 1".into()))));
        let (st, v) = post(&app(state.clone()), OWNER, &[]).await;
        assert_eq!((st, v["code"].clone()), (StatusCode::FORBIDDEN, json!("worker_origin_unattributed")), "{v}");
        assert!(v["why"].as_str().unwrap_or("").contains("tmux pane inventory failed"), "{v}");
        assert!(queued_senders(&state).is_empty(), "nothing reaches the send gate as owner input");
        drop(failed);
        let _listed = test_support::set_resolver(Arc::new(InventoryResolver(Ok(HashMap::from([(100, "amux-caller".to_string())])))));
        let (st, v) = post(&app(state.clone()), OWNER, &[]).await;
        assert_eq!((st, v["code"].clone()), (StatusCode::FORBIDDEN, json!("worker_group_boundary")), "listed: it is caller: {v}");
    }

    /// Review round 2: the parser's verdict on real tmux diagnostics, carried
    /// through the shipped middleware. Only "no server running" may read as an
    /// empty inventory; a connection error must refuse.
    #[tokio::test]
    async fn tmux_connection_errors_refuse_through_the_middleware() {
        let (state, dir, guard) = fixture();
        let _h = crate::api::settings::test_env::set_home(dir.path());
        drop(guard);
        for stderr in [
            "error connecting to /tmp/tmux-501/default (Permission denied)",
            "error connecting to /tmp/tmux-501/default (Connection refused)",
            "lost server",
            "",
        ] {
            let inv = panes_from_output(false, "", stderr);
            let r = test_support::set_resolver(Arc::new(InventoryResolver(inv)));
            let (st, v) = post(&app(state.clone()), OWNER, &[]).await;
            assert_eq!((st, v["code"].clone()), (StatusCode::FORBIDDEN, json!("worker_origin_unattributed")), "{stderr:?}: {v}");
            drop(r);
        }
        assert!(queued_senders(&state).is_empty(), "no diagnostic lets a worker through as the owner");
        // The one authoritative absence: no tmux server means no worker pane,
        // so a readable chain with no pane is outside.
        let r = test_support::set_resolver(Arc::new(InventoryResolver(panes_from_output(false, "", "no server running on /tmp/tmux-501/default"))));
        let (st, v) = post(&app(state.clone()), OWNER, &[]).await;
        assert!(st != StatusCode::FORBIDDEN || v["code"] != json!("worker_origin_unattributed"), "no server: {v}");
        drop(r);
    }

    #[test]
    fn tmux_output_is_an_inventory_only_when_it_succeeded_or_no_server_runs() {
        assert_eq!(panes_from_output(true, "7 amux-a\n", "").unwrap().get(&7).map(String::as_str), Some("amux-a"));
        assert!(panes_from_output(false, "", "no server running on /tmp/tmux-501/default\n").unwrap().is_empty());
        assert!(panes_from_output(false, "", "error connecting to /tmp/tmux-501/default (No such file or directory)\n").unwrap().is_empty(),
            "no socket: tmux 3.6a's no-server message");
        for denied in [
            "error connecting to /tmp/tmux-501/default (Permission denied)",
            "error connecting to /tmp/tmux-501/default (Connection refused)",
            "error connecting to /tmp/tmux-501/default (No such file or directory) and more",
        ] {
            assert!(panes_from_output(false, "", denied).is_err(), "a failed connection proves nothing: {denied}");
        }
        assert!(panes_from_output(false, "", "lost server\n").is_err());
        assert!(panes_from_output(false, "", "").is_err(), "a silent failure is an error, not an empty fleet");
    }

    /// The proof header is server-set only: a client cannot assert it.
    #[tokio::test]
    async fn a_client_supplied_process_proof_is_stripped() {
        let (state, dir, _r) = fixture();
        let _h = crate::api::settings::test_env::set_home(dir.path());
        async fn echo(headers: axum::http::HeaderMap) -> String {
            headers.get(PROCESS_WORKER_HEADER).and_then(|v| v.to_str().ok()).unwrap_or("").to_string()
        }
        let router = axum::Router::new().route("/api/echo", axum::routing::get(echo)).layer(axum::middleware::from_fn(attribute));
        let mut req = HttpRequest::builder().uri("/api/echo").header(PROCESS_WORKER_HEADER, "peer").body(Body::empty()).unwrap();
        req.extensions_mut().insert(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], OWNER))));
        let res = router.clone().oneshot(req).await.unwrap();
        let body = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
        assert_eq!(&body[..], b"", "an owner request cannot claim process proof");
        drop(state);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn proc_net_endpoints_parse() {
        assert_eq!(linux::parse_endpoint("0100007F:1F90"), Some("127.0.0.1:8080".parse().unwrap()));
        assert_eq!(
            linux::parse_endpoint("00000000000000000000000001000000:0050"),
            Some("[::1]:80".parse().unwrap())
        );
    }
}
