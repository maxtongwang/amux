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
//! processes holding the client end of THAT connection (both ends matched: the
//! server listens on 0.0.0.0, so a client end alone can belong to two
//! connections). A holder is a worker when its controlling tty is a worker
//! pane's tty, or an ancestor is the shell of a tmux pane in session
//! `amux-<worker>`. Then the request IS that worker, whatever it sent:
//!
//! - a missing worker header is filled with that worker, a different one is
//!   overridden (logged `worker_identity_overridden`), and a member marker is
//!   dropped, so it is never the owner, owner token or not;
//! - `x-amux-process-worker` carries the proof to `worker_identity::enforce`,
//!   which accepts it in place of the rule-10 token (process ancestry is the
//!   stronger evidence, and a sandbox can strip the token's env var too).
//!
//! FAIL CLOSED. The owner is allowed only on positive proof: the holders were
//! found by a full scan, their ancestry reached the root, a pane inventory read
//! now succeeded, and none is in a worker pane (the dashboard browser, the
//! owner's terminal, launchd jobs). A cached inventory or a remembered holder
//! may confirm a worker, never the owner. Anything unproven is refused for a
//! headerless `/api/` request: `worker_origin_unattributed`, or
//! `worker_identity_unavailable` when the pane inventory cannot be read.
//!
//! REMOTE. The owner's phone is remote two ways: a non-loopback peer that no
//! local process holds (LAN), or `tailscale serve`, a raw TCP forward by root
//! `tailscaled` to 127.0.0.1, whose sockets this user cannot list. A loopback
//! connection with no visible holder is that forward only when a root
//! tailscaled is among the unlistable processes and no worker (or unprovable
//! process) is connected to this host's own tailnet front door (its tailnet
//! address, port 443), the one way a local worker could ride the forward.
//!
//! On a platform without a resolver the layer does nothing and says so once
//! (`process_attribution_unavailable`, measured=false): that is today's
//! behaviour, reported instead of silently assumed.
//!
//! WHAT THIS DOES NOT STOP. Workers run as the owner's uid. A process that
//! daemonizes out of its pane AND drops its controlling tty (`setsid`, its own
//! tmux server) reads as outside. A hostile same-uid process can also remove
//! tmux's socket (tmux then reports no server), rename a session out of the
//! `amux-` prefix, drive the owner's browser, edit `~/.amux` directly, or reach
//! the front door from another tailnet host, or keep its connected socket in
//! flight (SCM_RIGHTS) so no process holds it during the scan and the forward
//! test cannot see it (tailscaled's own sockets are unlistable, so its holding
//! the matching connection cannot be confirmed; same-user, issue #136). This
//! closes the accidental and
//! naive paths (stripped env, `env -i`, a backgrounded send), which are the
//! measured ones; a hostile same-uid process needs an OS sandbox, not an HTTP
//! check.
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

/// The server's own end of an accepted connection, stamped on every request by
/// the TLS acceptor (tls.rs). With the peer from ConnectInfo it names the exact
/// connection: the server listens on 0.0.0.0, so one client end can be
/// connected to two server addresses at once (review B2, 6c98cf27).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ServerLocal(pub SocketAddr);

/// One TCP connection: the client's end (`peer`) and the server's end.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct Conn {
    pub peer: SocketAddr,
    pub server: Option<SocketAddr>,
}

/// Who opened the connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Origin {
    /// A process inside the tmux pane of worker session `amux-<worker>`.
    Worker(String),
    /// A local process whose whole ancestry is outside every amux pane.
    Outside,
    /// The owner's remote device: no local process owns the client end and the
    /// peer is not loopback, or the connection was forwarded by this host's root
    /// `tailscaled` (tailscale serve) with no worker at the tailnet front door.
    Remote,
    /// Local, but the owner or its ancestry could not be established.
    Unattributed(String),
    /// The tmux pane inventory could not be read, so whether this process is
    /// inside a worker pane is unknown. Never the owner (fail closed).
    IdentityUnavailable(String),
    /// This platform has no resolver.
    Unavailable(&'static str),
}

/// Resolves a connection to an [`Origin`]. A trait so the attribution policy is
/// tested with a fake while the OS lookups are tested for what they read.
pub(crate) trait PeerResolver: Send + Sync {
    fn resolve(&self, conn: Conn) -> Origin;
}

/// The pane inventory: worker sessions by pane shell pid, and by pane tty.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Panes {
    pub by_pid: HashMap<u32, String>,
    pub by_tty: HashMap<u64, String>,
}

/// The pure classification. A process is a worker when its controlling tty is
/// a worker pane's tty (it stays so when reparented to launchd: `amux send &`
/// from a one-shot shell, review N1), or when an ancestor is a worker pane's
/// shell. A pane of a tmux session NOT named `amux-*` is the owner's own tmux
/// and the walk continues past it. A chain that breaks, or runs out of steps,
/// is unattributed, never outside: outside must be proven.
pub(crate) fn classify(
    owners: &[u32],
    panes: &Panes,
    ppid_of: &dyn Fn(u32) -> Option<u32>,
    tty_of: &dyn Fn(u32) -> Option<u64>,
) -> Origin {
    if owners.is_empty() {
        return Origin::Unattributed("no local process owns the client socket".into());
    }
    let worker_of = |session: &String| session.strip_prefix("amux-").filter(|w| !w.is_empty()).map(str::to_string);
    let mut broken = None;
    for &owner in owners {
        if let Some(worker) = tty_of(owner).and_then(|t| panes.by_tty.get(&t)).and_then(worker_of) {
            return Origin::Worker(worker);
        }
        let mut cur = owner;
        let mut proven = false;
        for _ in 0..4096 {
            if let Some(worker) = panes.by_pid.get(&cur).and_then(worker_of) {
                return Origin::Worker(worker);
            }
            if cur <= 1 {
                proven = true;
                break;
            }
            match ppid_of(cur) {
                Some(p) if p != cur => cur = p,
                _ => break,
            }
        }
        if !proven {
            broken = Some(format!("ancestry of pid {owner} could not be walked to the root (stopped at pid {cur})"));
        }
    }
    match broken {
        Some(why) => Origin::Unattributed(why),
        None => Origin::Outside,
    }
}

/// What one full scan of the process table found for a connection.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Scan {
    /// Pids holding a socket whose local end is the connection's client end and
    /// whose remote end is the connection's server end: this exact connection.
    pub holders: Vec<u32>,
    /// Pids whose sockets this user may not list (EPERM): another uid's.
    pub uninspectable: Vec<u32>,
    /// Inspectable pids holding a connection to this host's own tailnet front
    /// door (remote = this host's tailnet address, port 443).
    pub front_door: Vec<u32>,
}

/// The lookups [`decide`] consults, behind a trait so every branch, including
/// the failure and forwarding ones, is tested through the shipped decision.
pub(crate) trait Lookups {
    fn scan(&self, client: SocketAddr, server: SocketAddr) -> Result<Scan, os::LookupError>;
    /// Pids of root-owned processes named `tailscaled`.
    fn root_tailscaled(&self) -> Result<Vec<u32>, String>;
    fn panes(&self, fresh: bool) -> Result<Panes, String>;
    fn ppid_of(&self, pid: u32) -> Option<u32>;
    fn tty_of(&self, pid: u32) -> Option<u64>;
}

/// The resolver's decision.
///
/// FAIL CLOSED. Outside (and with it owner authority) is granted only on
/// positive proof: the holders of THIS connection (both ends matched) were
/// found by a full scan, their ancestry and controlling tty were read, a pane
/// inventory read NOW succeeded, and none of them is in a worker pane. A cached
/// inventory may only confirm a worker. Any inventory failure is
/// IdentityUnavailable.
///
/// FORWARDED (review B1, 6c98cf27). `tailscale serve` here is a raw TCP
/// forward to 127.0.0.1:<port> by root `tailscaled`, whose sockets this user
/// cannot list, so the owner's phone arrives as a loopback connection with no
/// visible holder. It is Remote only when that is positively the case: no
/// visible holder, a root `tailscaled` among the uninspectable processes, and
/// no worker (or unprovable process) connected to this host's own tailnet
/// front door, which is the one way a local worker could ride the forward.
fn decide(conn: Conn, l: &dyn Lookups) -> Origin {
    let peer = SocketAddr::new(conn.peer.ip().to_canonical(), conn.peer.port());
    let Some(server) = conn.server.map(|s| SocketAddr::new(s.ip().to_canonical(), s.port())) else {
        return if peer.ip().is_loopback() {
            Origin::Unattributed("the server end of this connection is unknown".into())
        } else {
            Origin::Remote
        };
    };
    let scan = match l.scan(peer, server) {
        Ok(s) => s,
        Err(os::LookupError::Unsupported(why)) => return Origin::Unavailable(why),
        Err(os::LookupError::Failed(why)) => {
            return if peer.ip().is_loopback() { Origin::Unattributed(why) } else { Origin::Remote };
        }
    };
    if scan.holders.is_empty() {
        if !peer.ip().is_loopback() {
            return Origin::Remote;
        }
        return forwarded(peer, &scan, l);
    }
    if let Ok(panes) = l.panes(false) {
        if let worker @ Origin::Worker(_) = classify(&scan.holders, &panes, &|p| l.ppid_of(p), &|p| l.tty_of(p)) {
            return worker;
        }
    }
    match l.panes(true) {
        Ok(panes) => classify(&scan.holders, &panes, &|p| l.ppid_of(p), &|p| l.tty_of(p)),
        Err(why) => inventory_failed(peer, scan.holders.len(), &why),
    }
}

fn inventory_failed(peer: SocketAddr, n: usize, why: &str) -> Origin {
    tracing::warn!(%peer, why = %why, verdict = "process_attribution_pane_inventory_failed",
        measured = false, why_unmeasured = %why, n_considered = n,
        "tmux pane inventory failed; this local request cannot be proven outside amux panes");
    Origin::IdentityUnavailable(format!("tmux pane inventory failed: {why}"))
}

/// A loopback connection with no visible holder: the owner's phone through
/// tailscale serve, or nothing provable.
fn forwarded(peer: SocketAddr, scan: &Scan, l: &dyn Lookups) -> Origin {
    let forwarder = match l.root_tailscaled() {
        Ok(pids) => pids.into_iter().find(|p| scan.uninspectable.contains(p)),
        Err(why) => return Origin::Unattributed(format!("no visible holder, and the tailscale forwarder could not be identified: {why}")),
    };
    let Some(forwarder) = forwarder else {
        return Origin::Unattributed("no local process visibly owns the client socket and no root tailscaled forwarder is running".into());
    };
    if !scan.front_door.is_empty() {
        let panes = match l.panes(true) {
            Ok(p) => p,
            Err(why) => return inventory_failed(peer, scan.front_door.len(), &why),
        };
        for &pid in &scan.front_door {
            let origin = classify(&[pid], &panes, &|p| l.ppid_of(p), &|p| l.tty_of(p));
            if origin != Origin::Outside {
                tracing::warn!(%peer, front_door_pid = pid, origin = ?origin, verdict = "tailscale_forward_ambiguous",
                    measured = true, n_considered = scan.front_door.len(),
                    "a forwarded request arrived while a worker (or an unprovable process) is connected to this host's tailnet front door");
                return Origin::IdentityUnavailable(format!(
                    "this request was forwarded by tailscale serve while pid {pid} ({origin:?}) is connected to this host's tailnet front door, so the forward could be that process"));
            }
        }
    }
    tracing::debug!(%peer, forwarder, front_door = scan.front_door.len(), verdict = "tailscale_forward_remote",
        measured = true, n_considered = 1, "forwarded by tailscale serve with no worker at the front door: the owner's remote device");
    Origin::Remote
}

/// The OS lookups.
struct OsLookups;

impl Lookups for OsLookups {
    fn scan(&self, client: SocketAddr, server: SocketAddr) -> Result<Scan, os::LookupError> {
        os::scan(client, server)
    }
    fn root_tailscaled(&self) -> Result<Vec<u32>, String> {
        root_tailscaled_cached()
    }
    fn panes(&self, fresh: bool) -> Result<Panes, String> {
        pane_map(fresh)
    }
    fn ppid_of(&self, pid: u32) -> Option<u32> {
        os::ppid_of(pid)
    }
    fn tty_of(&self, pid: u32) -> Option<u64> {
        os::tty_of(pid)
    }
}

/// The OS resolver: a per-connection memo that may only CONFIRM a worker (the
/// remembered holders must still hold this exact connection, both ends, and a
/// cached inventory must place one in a worker pane), then the full decision.
/// An owner verdict never comes from the memo.
struct OsResolver;

type HolderMemo = Mutex<HashMap<Conn, (Instant, Vec<u32>)>>;
const MEMO_TTL: Duration = Duration::from_secs(30);

fn holder_memo() -> &'static HolderMemo {
    static MEMO: OnceLock<HolderMemo> = OnceLock::new();
    MEMO.get_or_init(|| Mutex::new(HashMap::new()))
}

impl PeerResolver for OsResolver {
    fn resolve(&self, conn: Conn) -> Origin {
        let conn = Conn {
            peer: SocketAddr::new(conn.peer.ip().to_canonical(), conn.peer.port()),
            server: conn.server.map(|s| SocketAddr::new(s.ip().to_canonical(), s.port())),
        };
        if let Some(server) = conn.server {
            let memo = holder_memo().lock().ok()
                .and_then(|m| m.get(&conn).filter(|(at, _)| at.elapsed() < MEMO_TTL).map(|(_, h)| h.clone()));
            if let Some(holders) = memo {
                if !holders.is_empty() && holders.iter().all(|&p| os::holds(p, conn.peer, server)) {
                    if let Ok(panes) = pane_map(false) {
                        if let worker @ Origin::Worker(_) = classify(&holders, &panes, &os::ppid_of, &os::tty_of) {
                            return worker;
                        }
                    }
                }
            }
        }
        let origin = decide(conn, &OsLookups);
        if let (Origin::Worker(_), Some(server)) = (&origin, conn.server) {
            if let Ok(scan) = os::scan(conn.peer, server) {
                if let Ok(mut m) = holder_memo().lock() {
                    if m.len() > 4096 {
                        m.retain(|_, (at, _)| at.elapsed() < MEMO_TTL);
                    }
                    m.insert(conn, (Instant::now(), scan.holders));
                }
            }
        }
        origin
    }
}

type PaneCache = Mutex<Option<(Instant, Panes)>>;

/// The pane inventory. `fresh` always runs tmux (an owner verdict needs it);
/// otherwise a successful read under 2s old is reused, which can only confirm a
/// worker. A failure is never cached and never "no panes". tmux gets a bounded
/// wait (review N3): a hung server is a failed inventory, not a stuck request.
fn pane_map(fresh: bool) -> Result<Panes, String> {
    static CACHE: OnceLock<PaneCache> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(None));
    if let (false, Ok(g)) = (fresh, cache.lock()) {
        if let Some((at, map)) = g.as_ref() {
            if at.elapsed() < Duration::from_secs(2) {
                return Ok(map.clone());
            }
        }
    }
    let (ok, stdout, stderr) = run_bounded(
        std::process::Command::new("tmux").args(["list-panes", "-a", "-F", "#{pane_pid} #{pane_tty} #{session_name}"]),
        TMUX_TIMEOUT,
    )?;
    let map = panes_from_output(ok, &stdout, &stderr)?;
    if let Ok(mut g) = cache.lock() {
        *g = Some((Instant::now(), map.clone()));
    }
    Ok(map)
}

const TMUX_TIMEOUT: Duration = Duration::from_secs(2);

/// Run a command with a deadline: (exit ok, stdout, stderr), or an error when
/// it cannot start or outlives `limit` (it is then killed). Both pipes are
/// drained while it runs (review round 7: a full pipe blocked the child, which
/// then could not exit and was killed as hung).
fn run_bounded(cmd: &mut std::process::Command, limit: Duration) -> Result<(bool, String, String), String> {
    use std::io::Read;
    use wait_timeout::ChildExt;
    let program = format!("{:?}", cmd.get_program());
    let mut child = cmd
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not run {program}: {e}"))?;
    let drain = |pipe: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut p) = pipe {
                let _ = p.read_to_end(&mut buf);
            }
            String::from_utf8_lossy(&buf).into_owned()
        })
    };
    let out = drain(child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    let err = drain(child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    let status = match child.wait_timeout(limit) {
        Ok(Some(status)) => status,
        Ok(None) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("{program} did not answer within {}ms", limit.as_millis()));
        }
        Err(e) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("could not wait for {program}: {e}"));
        }
    };
    let out = out.join().map_err(|_| format!("{program}: stdout reader failed"))?;
    let err = err.join().map_err(|_| format!("{program}: stderr reader failed"))?;
    Ok((status.success(), out, err))
}

/// A `tmux list-panes` result as an inventory. Default: any failure is an
/// error. The only exception is tmux saying explicitly that no server exists:
/// "no server running", or (tmux 3.6a, measured) a connection error ending
/// "(No such file or directory)", the socket being absent. New or unknown
/// diagnostics therefore refuse without anyone listing them.
fn panes_from_output(ok: bool, stdout: &str, stderr: &str) -> Result<Panes, String> {
    if ok {
        return parse_panes(stdout, &|path| {
            use std::os::unix::fs::MetadataExt;
            std::fs::metadata(path).map(|m| m.rdev()).map_err(|e| format!("cannot stat pane tty {path}: {e}"))
        });
    }
    let err = stderr.trim();
    let socket_missing = err.starts_with("error connecting to") && err.ends_with("(No such file or directory)");
    if err.starts_with("no server running") || socket_missing {
        return Ok(Panes::default());
    }
    Err(if err.is_empty() { "tmux list-panes failed with no message".into() } else { err.chars().take(200).collect() })
}

/// `<pane_pid> <pane_tty> <session name>` lines. Fail closed: a line that does
/// not parse, or a pane tty that cannot be read, could hide a worker pane, so
/// it makes the whole inventory unreadable rather than being skipped.
pub(crate) fn parse_panes(text: &str, rdev: &dyn Fn(&str) -> Result<u64, String>) -> Result<Panes, String> {
    let mut panes = Panes::default();
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let mut parts = line.splitn(3, ' ');
        let (pid, tty, sess) = (parts.next(), parts.next(), parts.next().map(str::trim));
        match (pid.and_then(|p| p.parse::<u32>().ok()), tty, sess) {
            (Some(pid), Some(tty), Some(sess)) if !sess.is_empty() && tty.starts_with('/') => {
                panes.by_tty.insert(rdev(tty)?, sess.to_string());
                panes.by_pid.insert(pid, sess.to_string());
            }
            _ => return Err(format!("unparseable tmux pane line: {}", line.chars().take(80).collect::<String>())),
        }
    }
    Ok(panes)
}

type ForwarderCache = Mutex<Option<(Instant, Vec<u32>)>>;

/// Root `tailscaled` pids, from the process table. libproc refuses another
/// uid's name and uid (EPERM, measured for pid 416), but `ps` reads them from
/// sysctl(KERN_PROC). Consulted only for a connection no visible process
/// holds; a successful answer is reused for 10s.
fn root_tailscaled_cached() -> Result<Vec<u32>, String> {
    static CACHE: OnceLock<ForwarderCache> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(None));
    if let Ok(g) = cache.lock() {
        if let Some((at, pids)) = g.as_ref() {
            if at.elapsed() < Duration::from_secs(10) {
                return Ok(pids.clone());
            }
        }
    }
    let pids = os::root_tailscaled()?;
    if let Ok(mut g) = cache.lock() {
        *g = Some((Instant::now(), pids.clone()));
    }
    Ok(pids)
}

/// `ps -A -o pid=,uid=,ucomm=` rows naming root `tailscaled` (macOS reads
/// the table with ps; Linux reads /proc directly).
#[cfg(any(target_os = "macos", test))]
pub(crate) fn parse_root_tailscaled(text: &str) -> Vec<u32> {
    text.lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            (f.len() == 3 && f[1] == "0" && f[2] == "tailscaled").then(|| f[0].parse().ok()).flatten()
        })
        .collect()
}

/// This host's tailnet ranges (100.64.0.0/10, fd7a:115c:a1e0::/48).
pub(crate) fn is_tailnet(ip: std::net::IpAddr) -> bool {
    match ip.to_canonical() {
        std::net::IpAddr::V4(v4) => {
            let o = v4.octets();
            o[0] == 100 && (o[1] & 0xc0) == 64
        }
        std::net::IpAddr::V6(v6) => {
            let s = v6.segments();
            s[0] == 0xfd7a && s[1] == 0x115c && s[2] == 0xa1e0
        }
    }
}

/// The ports `tailscale serve` exposes on this host's tailnet addresses, or
/// None when they cannot be read (then every tailnet port counts: fail closed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ServedPorts {
    Known(std::collections::BTreeSet<u16>),
    Unknown,
}

/// The port set from `tailscale serve status --json`: the `TCP` map's keys and
/// the port of each `Web` host ("name:port"). Unparseable JSON is Unknown.
pub(crate) fn parse_served_ports(json: &str) -> ServedPorts {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else { return ServedPorts::Unknown };
    let mut ports = std::collections::BTreeSet::new();
    if let Some(tcp) = v.get("TCP").and_then(|t| t.as_object()) {
        for k in tcp.keys() {
            match k.parse::<u16>() {
                Ok(p) => {
                    ports.insert(p);
                }
                Err(_) => return ServedPorts::Unknown,
            }
        }
    }
    if let Some(web) = v.get("Web").and_then(|w| w.as_object()) {
        for k in web.keys() {
            match k.rsplit_once(':').and_then(|(_, p)| p.parse::<u16>().ok()) {
                Some(p) => {
                    ports.insert(p);
                }
                None => return ServedPorts::Unknown,
            }
        }
    }
    ServedPorts::Known(ports)
}

type ServedCache = Mutex<Option<(Instant, ServedPorts)>>;

/// Served ports, read at most every 10s. A failed read is Unknown and not
/// cached, so the next connection tries again.
pub(crate) fn served_ports_cached() -> ServedPorts {
    static CACHE: OnceLock<ServedCache> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(None));
    if let Ok(g) = cache.lock() {
        if let Some((at, ports)) = g.as_ref() {
            if at.elapsed() < Duration::from_secs(10) {
                return ports.clone();
            }
        }
    }
    let read = ["tailscale", "/usr/local/bin/tailscale", "/Applications/Tailscale.app/Contents/MacOS/Tailscale"]
        .iter()
        .find_map(|bin| {
            let (ok, out, _) = run_bounded(std::process::Command::new(bin).args(["serve", "status", "--json"]), Duration::from_secs(2)).ok()?;
            ok.then(|| parse_served_ports(&out))
        });
    match read {
        Some(ports @ ServedPorts::Known(_)) => {
            if let Ok(mut g) = cache.lock() {
                *g = Some((Instant::now(), ports.clone()));
            }
            ports
        }
        _ => ServedPorts::Unknown,
    }
}

/// A socket that may be connected to this host's tailscale serve front door:
/// a tailnet address on a served port, whatever the local end (review round 7:
/// a worker can bind loopback or a LAN address and still reach this host's
/// tailnet IP). Other tailnet hosts on those ports match too; that only makes a
/// forward ambiguous, which fails closed. Served ports unknown: any tailnet port.
pub(crate) fn is_front_door(_local: SocketAddr, remote: SocketAddr, served: &ServedPorts) -> bool {
    is_tailnet(remote.ip())
        && match served {
            ServedPorts::Known(ports) => ports.contains(&remote.port()),
            ServedPorts::Unknown => true,
        }
}

type Shared = Arc<dyn PeerResolver>;

fn resolver_slot() -> &'static RwLock<Shared> {
    static SLOT: OnceLock<RwLock<Shared>> = OnceLock::new();
    SLOT.get_or_init(|| RwLock::new(Arc::new(OsResolver)))
}

/// Resolve on every request: an Origin is never memoised.
async fn resolve_cached(conn: Conn) -> Origin {
    let resolver = resolver_slot().read().map(|r| r.clone()).unwrap_or_else(|p| p.into_inner().clone());
    tokio::task::spawn_blocking(move || resolver.resolve(conn))
        .await
        .unwrap_or_else(|e| Origin::Unattributed(format!("resolver task failed: {e}")))
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
    let conn = Conn { peer, server: req.extensions().get::<ServerLocal>().map(|s| s.0) };
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
    match resolve_cached(conn).await {
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
        Origin::IdentityUnavailable(why) => {
            if claimed(&req).is_empty() && req.uri().path().starts_with("/api/") {
                tracing::warn!(%peer, path = %req.uri().path(), why = %why, verdict = "worker_identity_unavailable",
                    measured = false, why_unmeasured = %why, n_considered = 1,
                    "identity unavailable: the pane inventory could not be read, so this local request is not the owner");
                return (StatusCode::FORBIDDEN, Json(json!({
                    "ok": false,
                    "code": "worker_identity_unavailable",
                    "error": "identity unavailable: amux could not read its tmux pane inventory, so it cannot prove this request is from outside every worker pane; owner access requires that proof",
                    "why": why,
                }))).into_response();
            }
            // A claimed worker identity is not the owner either way; rule 10
            // and the group policy judge the claim as they would without proof.
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
    /// one at a time.
    pub(crate) struct ResolverGuard {
        _serial: std::sync::MutexGuard<'static, ()>,
    }

    impl Drop for ResolverGuard {
        fn drop(&mut self) {
            if let Ok(mut s) = resolver_slot().write() {
                *s = Arc::new(OsResolver);
            }
        }
    }

    pub(crate) fn set_resolver(r: Shared) -> ResolverGuard {
        let serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
        if let Ok(mut s) = resolver_slot().write() {
            *s = r;
        }
        ResolverGuard { _serial: serial }
    }

    /// A resolver that answers by client port.
    pub(crate) struct ByPort(pub HashMap<u16, Origin>);

    impl PeerResolver for ByPort {
        fn resolve(&self, conn: Conn) -> Origin {
            self.0.get(&conn.peer.port()).cloned().unwrap_or(Origin::Outside)
        }
    }
}

/// Platform lookups. `scan` walks the process table once for a connection
/// (client end + server end): the exact connection's holders, the processes
/// whose sockets this user may not list, and the front-door connections.
pub(crate) mod os {
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    use std::net::SocketAddr;

    #[derive(Debug)]
    pub(crate) enum LookupError {
        #[cfg_attr(any(target_os = "macos", target_os = "linux"), allow(dead_code))]
        Unsupported(&'static str),
        Failed(String),
    }

    #[cfg(target_os = "macos")]
    pub(super) use super::macos::{holds, ppid_of, root_tailscaled, scan, tty_of};
    #[cfg(target_os = "linux")]
    pub(super) use super::linux::{holds, ppid_of, root_tailscaled, scan, tty_of};

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    pub(super) fn scan(_client: SocketAddr, _server: SocketAddr) -> Result<super::Scan, LookupError> {
        Err(LookupError::Unsupported("no socket-owner lookup on this platform"))
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    pub(super) fn ppid_of(_pid: u32) -> Option<u32> {
        None
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    pub(super) fn tty_of(_pid: u32) -> Option<u64> {
        None
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    pub(super) fn holds(_pid: u32, _client: SocketAddr, _server: SocketAddr) -> bool {
        false
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    pub(super) fn root_tailscaled() -> Result<Vec<u32>, String> {
        Err("no process table lookup on this platform".into())
    }
}

/// macOS: libproc. `proc_pidfdinfo(PROC_PIDFDSOCKETINFO)` fills a
/// `struct socket_fdinfo` (sys/proc_info.h), which libc does not declare; the
/// prefix up to `soi_proto` and `in_sockinfo` are declared here with C layout,
/// and the union is over-allocated (the kernel requires buffersize >= its
/// size). `os_lookup_matches_the_exact_connection` proves the layout.
#[cfg(target_os = "macos")]
#[allow(dead_code)] // C layout fields the kernel fills but this lookup never reads
mod macos {
    use super::os::LookupError;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

    const PROC_PIDFDSOCKETINFO: libc::c_int = 3;
    const SOCKINFO_IN: i32 = 1;
    const SOCKINFO_TCP: i32 = 2;
    const INI_IPV4: u8 = 0x1;
    const NODEV: u32 = u32::MAX;

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
    const _: () = assert!(std::mem::offset_of!(InSockinfo, insi_faddr) == 32);
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

    /// A pid's socket fds, or the errno when the kernel refuses to list them
    /// (EPERM: another uid's process).
    fn socket_fds(pid: i32) -> Result<Vec<i32>, i32> {
        // SAFETY: as above; the kernel writes whole proc_fdinfo records.
        let need = unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDLISTFDS, 0, std::ptr::null_mut(), 0) };
        if need <= 0 {
            return Err(std::io::Error::last_os_error().raw_os_error().unwrap_or(0));
        }
        let size = std::mem::size_of::<libc::proc_fdinfo>();
        let mut buf: Vec<libc::proc_fdinfo> = Vec::with_capacity(need as usize / size + 16);
        let cap = (buf.capacity() * size) as libc::c_int;
        let got = unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDLISTFDS, 0, buf.as_mut_ptr().cast(), cap) };
        if got <= 0 {
            return Err(std::io::Error::last_os_error().raw_os_error().unwrap_or(0));
        }
        // SAFETY: `got` bytes of initialised records were written.
        unsafe { buf.set_len(got as usize / size) };
        Ok(buf.iter()
            .filter(|f| f.proc_fdtype as libc::c_int == libc::PROX_FDTYPE_SOCKET)
            .map(|f| f.proc_fd)
            .collect())
    }

    fn addr(bytes: &[u8; 16], v4: bool, port: i32) -> SocketAddr {
        let ip = if v4 {
            IpAddr::V4(Ipv4Addr::new(bytes[12], bytes[13], bytes[14], bytes[15]))
        } else {
            IpAddr::V6(Ipv6Addr::from(*bytes)).to_canonical()
        };
        SocketAddr::new(ip, u16::from_be(port as u16))
    }

    /// (local end, remote end) of an inet socket fd.
    fn endpoints(pid: i32, fd: i32) -> Option<(SocketAddr, SocketAddr)> {
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
        let v4 = ini.insi_vflag & INI_IPV4 != 0;
        Some((addr(&ini.insi_laddr, v4, ini.insi_lport), addr(&ini.insi_faddr, v4, ini.insi_fport)))
    }

    pub(crate) fn scan(client: SocketAddr, server: SocketAddr) -> Result<super::Scan, LookupError> {
        let pids = all_pids();
        if pids.is_empty() {
            return Err(LookupError::Failed("proc_listallpids returned nothing".into()));
        }
        let served = super::served_ports_cached();
        let mut out = super::Scan::default();
        for pid in pids.into_iter().filter(|&p| p > 0) {
            let fds = match socket_fds(pid) {
                Ok(fds) => fds,
                Err(libc::EPERM) => {
                    out.uninspectable.push(pid as u32);
                    continue;
                }
                Err(_) => continue, // exited, or no fds to list
            };
            let (mut holds, mut door) = (false, false);
            for fd in fds {
                if let Some((local, remote)) = endpoints(pid, fd) {
                    holds |= local == client && remote == server;
                    door |= super::is_front_door(local, remote, &served);
                }
            }
            if holds {
                out.holders.push(pid as u32);
            }
            if door {
                out.front_door.push(pid as u32);
            }
        }
        Ok(out)
    }

    /// Whether `pid` still holds this exact connection (both ends).
    pub(crate) fn holds(pid: u32, client: SocketAddr, server: SocketAddr) -> bool {
        socket_fds(pid as i32)
            .unwrap_or_default()
            .into_iter()
            .any(|fd| endpoints(pid as i32, fd) == Some((client, server)))
    }

    fn bsdinfo(pid: u32) -> Option<libc::proc_bsdinfo> {
        // SAFETY: zeroed POD; the kernel writes exactly one proc_bsdinfo.
        let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
        let got = unsafe {
            libc::proc_pidinfo(pid as i32, libc::PROC_PIDTBSDINFO, 0, (&mut info as *mut libc::proc_bsdinfo).cast(), size)
        };
        (got == size).then_some(info)
    }

    pub(crate) fn ppid_of(pid: u32) -> Option<u32> {
        bsdinfo(pid).map(|i| i.pbi_ppid)
    }

    /// The controlling tty's device number, or None without one.
    pub(crate) fn tty_of(pid: u32) -> Option<u64> {
        bsdinfo(pid).map(|i| i.e_tdev).filter(|&d| d != NODEV && d != 0).map(u64::from)
    }

    pub(crate) fn root_tailscaled() -> Result<Vec<u32>, String> {
        let (ok, out, err) = super::run_bounded(
            std::process::Command::new("/bin/ps").args(["-A", "-o", "pid=,uid=,ucomm="]),
            std::time::Duration::from_secs(2),
        )?;
        if !ok {
            return Err(format!("ps failed: {}", err.trim()));
        }
        Ok(super::parse_root_tailscaled(&out))
    }
}

/// Linux: `/proc/net/tcp{,6}` gives each socket's endpoints and inode;
/// `/proc/<pid>/fd` links name the holders; `/proc/<pid>/stat` the parent and
/// controlling tty.
#[cfg(target_os = "linux")]
mod linux {
    use super::os::LookupError;
    use std::collections::HashSet;
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

    /// Every socket row: (local, remote, inode).
    fn sockets() -> Result<Vec<(SocketAddr, SocketAddr, u64)>, LookupError> {
        let mut rows = vec![];
        let mut read_any = false;
        for table in ["/proc/net/tcp", "/proc/net/tcp6"] {
            let Ok(text) = std::fs::read_to_string(table) else { continue };
            read_any = true;
            for line in text.lines().skip(1) {
                let cols: Vec<&str> = line.split_whitespace().collect();
                if cols.len() <= 9 {
                    continue;
                }
                if let (Some(l), Some(r), Ok(inode)) = (parse_endpoint(cols[1]), parse_endpoint(cols[2]), cols[9].parse::<u64>()) {
                    if inode != 0 {
                        rows.push((l, r, inode));
                    }
                }
            }
        }
        if read_any { Ok(rows) } else { Err(LookupError::Failed("/proc/net/tcp unreadable".into())) }
    }

    fn socket_inodes(pid: &str) -> Result<HashSet<u64>, std::io::ErrorKind> {
        let fds = std::fs::read_dir(format!("/proc/{pid}/fd")).map_err(|e| e.kind())?;
        Ok(fds.flatten()
            .filter_map(|fd| std::fs::read_link(fd.path()).ok())
            .filter_map(|l| l.to_str()?.strip_prefix("socket:[")?.strip_suffix(']')?.parse().ok())
            .collect())
    }

    pub(crate) fn scan(client: SocketAddr, server: SocketAddr) -> Result<super::Scan, LookupError> {
        let rows = sockets()?;
        let target: HashSet<u64> = rows.iter().filter(|(l, r, _)| *l == client && *r == server).map(|(_, _, i)| *i).collect();
        let served = super::served_ports_cached();
        let doors: HashSet<u64> = rows.iter().filter(|(l, r, _)| super::is_front_door(*l, *r, &served)).map(|(_, _, i)| *i).collect();
        let mut out = super::Scan::default();
        let procs = std::fs::read_dir("/proc").map_err(|e| LookupError::Failed(e.to_string()))?;
        for entry in procs.flatten() {
            let Some(name) = entry.file_name().to_str().map(str::to_string) else { continue };
            let Ok(pid) = name.parse::<u32>() else { continue };
            match socket_inodes(&name) {
                Ok(inodes) => {
                    if !target.is_disjoint(&inodes) {
                        out.holders.push(pid);
                    }
                    if !doors.is_disjoint(&inodes) {
                        out.front_door.push(pid);
                    }
                }
                Err(std::io::ErrorKind::PermissionDenied) => out.uninspectable.push(pid),
                Err(_) => {}
            }
        }
        Ok(out)
    }

    pub(crate) fn holds(pid: u32, client: SocketAddr, server: SocketAddr) -> bool {
        let Ok(rows) = sockets() else { return false };
        let target: HashSet<u64> = rows.iter().filter(|(l, r, _)| *l == client && *r == server).map(|(_, _, i)| *i).collect();
        socket_inodes(&pid.to_string()).is_ok_and(|inodes| !target.is_disjoint(&inodes))
    }

    /// Fields after the `(comm)` of /proc/<pid>/stat: state, ppid, pgrp,
    /// session, tty_nr, ...
    fn stat_fields(pid: u32) -> Option<Vec<String>> {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        Some(stat[stat.rfind(')')? + 1..].split_whitespace().map(str::to_string).collect())
    }

    pub(crate) fn ppid_of(pid: u32) -> Option<u32> {
        stat_fields(pid)?.get(1)?.parse().ok()
    }

    pub(crate) fn tty_of(pid: u32) -> Option<u64> {
        stat_fields(pid)?.get(4)?.parse::<u64>().ok().filter(|&t| t != 0)
    }

    pub(crate) fn root_tailscaled() -> Result<Vec<u32>, String> {
        let procs = std::fs::read_dir("/proc").map_err(|e| e.to_string())?;
        Ok(procs.flatten()
            .filter_map(|e| e.file_name().to_str()?.parse::<u32>().ok())
            .filter(|pid| std::fs::read_to_string(format!("/proc/{pid}/comm")).is_ok_and(|c| c.trim() == "tailscaled"))
            .filter(|pid| std::fs::read_to_string(format!("/proc/{pid}/status"))
                .is_ok_and(|s| s.lines().any(|l| l.starts_with("Uid:") && l.split_whitespace().nth(1) == Some("0"))))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Process table for the pure tests:
    //   101 -> 100 (amux-dev-a pane shell) -> 50 (tmux server) -> 1
    //   201 -> 200 (the owner's own tmux pane "personal") -> 50 -> 1
    //   301 -> 300 -> 1 (owner terminal);  401 -> unreadable parent
    //   501 -> 1 (reparented to launchd) but its controlling tty is dev-a's pane tty
    //   601 <-> 602 (a ppid cycle: the walk runs out of steps)
    fn chain(pid: u32) -> Option<u32> {
        match pid {
            101 => Some(100),
            100 | 200 => Some(50),
            50 | 300 | 501 => Some(1),
            201 => Some(200),
            301 => Some(300),
            601 => Some(602),
            602 => Some(601),
            _ => None,
        }
    }
    const DEV_A_TTY: u64 = 0x1000_0007;
    fn tty(pid: u32) -> Option<u64> {
        (pid == 501).then_some(DEV_A_TTY)
    }
    fn panes() -> Panes {
        Panes {
            by_pid: HashMap::from([(100, "amux-dev-a".to_string()), (200, "personal".to_string())]),
            by_tty: HashMap::from([(DEV_A_TTY, "amux-dev-a".to_string())]),
        }
    }

    #[test]
    fn a_descendant_of_an_amux_pane_is_that_worker_and_other_chains_are_not() {
        let p = panes();
        assert_eq!(classify(&[101], &p, &chain, &tty), Origin::Worker("dev-a".into()));
        assert_eq!(classify(&[100], &p, &chain, &tty), Origin::Worker("dev-a".into()), "the pane shell itself");
        assert_eq!(classify(&[201], &p, &chain, &tty), Origin::Outside, "the owner's own tmux is not a worker");
        assert_eq!(classify(&[301], &p, &chain, &tty), Origin::Outside);
        assert_eq!(classify(&[301, 101], &p, &chain, &tty), Origin::Worker("dev-a".into()), "any worker holder");
    }

    /// Review N1: `amux send &` from a one-shot shell is reparented to launchd,
    /// so its ancestry reads outside; its controlling tty still names the pane.
    #[test]
    fn a_worker_process_reparented_to_launchd_is_still_that_worker_by_its_tty() {
        assert_eq!(chain(501), Some(1));
        assert_eq!(classify(&[501], &panes(), &chain, &tty), Origin::Worker("dev-a".into()));
    }

    #[test]
    fn outside_must_be_proven_never_assumed() {
        let p = panes();
        assert!(matches!(classify(&[], &p, &chain, &tty), Origin::Unattributed(_)), "no owner");
        assert!(matches!(classify(&[401], &p, &chain, &tty), Origin::Unattributed(_)), "broken chain");
        assert!(matches!(classify(&[301, 401], &p, &chain, &tty), Origin::Unattributed(_)), "one unproven holder");
        // Review N4: running out of steps is not proof of reaching the root.
        assert!(matches!(classify(&[601], &p, &chain, &tty), Origin::Unattributed(_)), "a cycle");
    }

    fn rdev_ok(path: &str) -> Result<u64, String> {
        Ok(path.trim_start_matches("/dev/ttys").parse::<u64>().map_err(|e| e.to_string())? + 0x1000_0000)
    }

    #[test]
    fn tmux_pane_lines_parse_and_any_unreadable_line_fails_the_inventory() {
        let m = parse_panes("123 /dev/ttys007 amux-dev-a\n456 /dev/ttys009 personal session\n\n", &rdev_ok).unwrap();
        assert_eq!(m.by_pid.get(&123).map(String::as_str), Some("amux-dev-a"));
        assert_eq!(m.by_pid.get(&456).map(String::as_str), Some("personal session"));
        assert_eq!(m.by_tty.get(&DEV_A_TTY).map(String::as_str), Some("amux-dev-a"));
        for bad in ["123 /dev/ttys007 amux-dev-a\nbad\n", "x /dev/ttys1 amux-dev-a\n", "123\n", "123 /dev/ttys1\n", "123 tty amux-a\n"] {
            assert!(parse_panes(bad, &rdev_ok).is_err(), "an unreadable line could hide a worker pane: {bad:?}");
        }
        let unstatable = |p: &str| -> Result<u64, String> { Err(format!("cannot stat {p}")) };
        assert!(parse_panes("1 /dev/ttys1 amux-a\n", &unstatable).is_err(), "a pane tty that cannot be read fails closed");
    }

    #[test]
    fn tmux_output_is_an_inventory_only_when_it_succeeded_or_no_server_exists() {
        assert!(panes_from_output(false, "", "no server running on /tmp/tmux-501/default\n").unwrap().by_pid.is_empty());
        assert!(panes_from_output(false, "", "error connecting to /tmp/tmux-501/default (No such file or directory)\n").unwrap().by_pid.is_empty(),
            "no socket: tmux 3.6a's no-server message");
        let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        for denied in [
            "error connecting to /tmp/tmux-501/default (Permission denied)",
            "error connecting to /tmp/tmux-501/default (Connection refused)",
            "error connecting to /tmp/tmux-501/default (No such file or directory) and more",
            "lost server",
            "",
            &format!("tmux {nonce:x}: an error message no version of tmux has printed yet"),
        ] {
            assert!(panes_from_output(false, "", denied).is_err(), "a failed read proves nothing: {denied:?}");
        }
    }

    #[test]
    fn a_hung_command_is_cut_off_and_reported() {
        let start = Instant::now();
        let r = run_bounded(std::process::Command::new("/bin/sleep").arg("5"), Duration::from_millis(100));
        assert!(r.is_err(), "{r:?}");
        assert!(start.elapsed() < Duration::from_secs(2), "the deadline held");
        assert_eq!(run_bounded(std::process::Command::new("/bin/echo").arg("hi"), Duration::from_secs(2)).unwrap().1.trim(), "hi");
        // Output far larger than a pipe buffer, on both streams, still completes.
        let big = run_bounded(std::process::Command::new("/bin/sh").args(["-c",
            "i=0; while [ $i -lt 20000 ]; do echo 12345 501 process-name; echo err-line >&2; i=$((i+1)); done"]), Duration::from_secs(10)).unwrap();
        assert!(big.0, "exited cleanly");
        assert_eq!(big.1.lines().count(), 20000);
        assert_eq!(big.2.lines().count(), 20000);
    }

    #[test]
    fn root_tailscaled_is_read_from_the_process_table_and_the_front_door_is_this_hosts_tailnet_443() {
        assert_eq!(parse_root_tailscaled("  416     0 tailscaled\n  417   501 tailscaled\n  418     0 tailscale\n"), vec![416]);
        let a = |s: &str| s.parse::<SocketAddr>().unwrap();
        let served = parse_served_ports(r#"{"TCP":{"443":{"TCPForward":"127.0.0.1:8824"}}}"#);
        assert_eq!(served, ServedPorts::Known([443].into()));
        assert!(is_front_door(a("100.85.233.44:50000"), a("100.85.233.44:443"), &served));
        assert!(is_front_door(a("[fd7a:115c:a1e0::b132:e92c]:50000"), a("[fd7a:115c:a1e0::b132:e92c]:443"), &served));
        assert!(is_front_door(a("192.168.1.207:50000"), a("100.85.233.44:443"), &served), "a LAN-bound worker reaching the tailnet IP");
        assert!(is_front_door(a("127.0.0.1:50000"), a("100.85.233.44:443"), &served), "a loopback-bound worker reaching the tailnet IP");
        assert!(is_front_door(a("100.85.233.44:50000"), a("100.85.233.45:443"), &served), "another tailnet host: ambiguous, fails closed");
        assert!(!is_front_door(a("100.85.233.44:50000"), a("100.85.233.44:8824"), &served), "not a served port");
        assert!(!is_front_door(a("192.168.1.207:50000"), a("192.168.1.207:443"), &served), "not tailnet");
        // Every served port counts, including web handlers.
        let more = parse_served_ports(r#"{"TCP":{"443":{},"8443":{}},"Web":{"macbook.tail213292.ts.net:10000":{}}}"#);
        assert_eq!(more, ServedPorts::Known([443, 8443, 10000].into()));
        assert!(is_front_door(a("127.0.0.1:1"), a("100.85.233.44:10000"), &more));
        // Unreadable serve status: every tailnet port counts.
        assert_eq!(parse_served_ports("not json"), ServedPorts::Unknown);
        assert_eq!(parse_served_ports(r#"{"TCP":{"x":{}}}"#), ServedPorts::Unknown);
        assert!(is_front_door(a("127.0.0.1:1"), a("100.85.233.44:8824"), &ServedPorts::Unknown));
        assert!(!is_front_door(a("127.0.0.1:1"), a("192.168.1.207:443"), &ServedPorts::Unknown));
    }

    /// Fake lookups for the shipped decision.
    #[derive(Clone)]
    struct Fake {
        scan: Result<Scan, String>,
        tailscaled: Result<Vec<u32>, String>,
        cached: Result<Panes, String>,
        fresh: Result<Panes, String>,
    }

    impl Fake {
        fn holders(h: &[u32]) -> Self {
            Fake {
                scan: Ok(Scan { holders: h.to_vec(), ..Default::default() }),
                tailscaled: Ok(vec![]),
                cached: Ok(panes()),
                fresh: Ok(panes()),
            }
        }
        /// The deployed phone path: nobody visible holds the connection, root
        /// tailscaled (pid 416) is running and its sockets are unlistable.
        fn forwarded(front_door: &[u32]) -> Self {
            Fake {
                scan: Ok(Scan { holders: vec![], uninspectable: vec![416], front_door: front_door.to_vec() }),
                tailscaled: Ok(vec![416]),
                cached: Ok(panes()),
                fresh: Ok(panes()),
            }
        }
    }

    impl Lookups for Fake {
        fn scan(&self, _: SocketAddr, _: SocketAddr) -> Result<Scan, os::LookupError> {
            self.scan.clone().map_err(os::LookupError::Failed)
        }
        fn root_tailscaled(&self) -> Result<Vec<u32>, String> {
            self.tailscaled.clone()
        }
        fn panes(&self, fresh: bool) -> Result<Panes, String> {
            if fresh { self.fresh.clone() } else { self.cached.clone() }
        }
        fn ppid_of(&self, pid: u32) -> Option<u32> {
            chain(pid)
        }
        fn tty_of(&self, pid: u32) -> Option<u64> {
            tty(pid)
        }
    }

    fn lo(port: u16) -> Conn {
        Conn { peer: SocketAddr::from(([127, 0, 0, 1], port)), server: Some(SocketAddr::from(([127, 0, 0, 1], 8824))) }
    }

    #[test]
    fn the_decision_grants_outside_only_on_a_fresh_inventory() {
        assert_eq!(decide(lo(1), &Fake::holders(&[301])), Origin::Outside);
        assert_eq!(decide(lo(1), &Fake::holders(&[101])), Origin::Worker("dev-a".into()));
        // Stale cached inventory without the pane; the fresh read finds it.
        let stale = Fake { cached: Ok(Panes::default()), ..Fake::holders(&[101]) };
        assert_eq!(decide(lo(1), &stale), Origin::Worker("dev-a".into()));
        // The fresh read fails: identity unavailable, whatever the cache said.
        let failed = Fake { cached: Ok(Panes::default()), fresh: Err("tmux gone".into()), ..Fake::holders(&[301]) };
        assert!(matches!(decide(lo(1), &failed), Origin::IdentityUnavailable(_)));
        // The server end unknown on a loopback connection: never owner.
        assert!(matches!(decide(Conn { server: None, ..lo(1) }, &Fake::holders(&[301])), Origin::Unattributed(_)));
        // A genuinely remote peer (no local holder, not loopback).
        let remote = Conn { peer: "192.168.1.50:5000".parse().unwrap(), server: Some("192.168.1.207:8824".parse().unwrap()) };
        assert_eq!(decide(remote, &Fake::holders(&[])), Origin::Remote);
    }

    /// Review B1 (6c98cf27), dev-lead's tests 1-3.
    #[test]
    fn the_owners_phone_through_tailscale_serve_is_remote_and_only_then() {
        // (1) the forwarded owner phone path.
        assert_eq!(decide(lo(53514), &Fake::forwarded(&[])), Origin::Remote);
        // An owner process at the front door (this Mac's browser on the ts.net
        // URL) leaves the forward provably free of workers.
        assert_eq!(decide(lo(53514), &Fake::forwarded(&[301])), Origin::Remote);
        // (2) a local worker connected to this host's front door: refused.
        assert!(matches!(decide(lo(53514), &Fake::forwarded(&[101])), Origin::IdentityUnavailable(_)));
        assert!(matches!(decide(lo(53514), &Fake::forwarded(&[501])), Origin::IdentityUnavailable(_)), "by tty too");
        assert!(matches!(decide(lo(53514), &Fake::forwarded(&[401])), Origin::IdentityUnavailable(_)), "unprovable");
        let blind = Fake { fresh: Err("tmux gone".into()), ..Fake::forwarded(&[301]) };
        assert!(matches!(decide(lo(53514), &blind), Origin::IdentityUnavailable(_)), "front door unreadable");
        // (3) tailscaled absent, not the uninspectable holder, or unidentifiable.
        let absent = Fake { tailscaled: Ok(vec![]), ..Fake::forwarded(&[]) };
        assert!(matches!(decide(lo(53514), &absent), Origin::Unattributed(_)));
        let inspectable = Fake { scan: Ok(Scan { uninspectable: vec![], ..Default::default() }), ..Fake::forwarded(&[]) };
        assert!(matches!(decide(lo(53514), &inspectable), Origin::Unattributed(_)), "a visible tailscaled holds nothing here");
        let unknown = Fake { tailscaled: Err("ps failed".into()), ..Fake::forwarded(&[]) };
        assert!(matches!(decide(lo(53514), &unknown), Origin::Unattributed(_)));
    }

    /// The real OS scan: holders are matched on BOTH ends of the connection.
    /// The server listens on 0.0.0.0, so this process opens two connections
    /// that share one client end (SO_REUSEADDR) to two server addresses, as
    /// review B2's worker did; each server end names exactly its own
    /// connection, and once the first closes it is no longer held although the
    /// second still holds the same client end (dev-lead's test 4).
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn os_lookup_matches_the_exact_connection_on_both_ends() {
        use std::os::fd::FromRawFd;
        let listener = std::net::TcpListener::bind("0.0.0.0:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let a = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        let client_end = a.local_addr().unwrap();
        let (_a_srv, _) = listener.accept().unwrap();
        let server_a = SocketAddr::from(([127, 0, 0, 1], port));
        let me = std::process::id();
        let scan_a = os::scan(client_end, server_a).expect("scan on a supported platform");
        assert_eq!(scan_a.holders, vec![me], "the exact connection, by this pid");
        assert!(os::holds(me, client_end, server_a));
        // A second server address on this host for the same client end.
        let other_ip = local_non_loopback_ip();
        let Some(other_ip) = other_ip else {
            panic!("this test needs a non-loopback address on the host to reproduce a shared client end");
        };
        let server_b = SocketAddr::new(other_ip, port);
        // SAFETY: a fresh socket, configured with plain setsockopt/bind/connect,
        // then owned by a TcpStream that closes it.
        let b = unsafe {
            let fd = libc::socket(libc::AF_INET, libc::SOCK_STREAM, 0);
            assert!(fd >= 0);
            let one: libc::c_int = 1;
            let sz = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
            assert_eq!(libc::setsockopt(fd, libc::SOL_SOCKET, libc::SO_REUSEADDR, (&one as *const libc::c_int).cast(), sz), 0);
            let sin = |ip: std::net::Ipv4Addr, p: u16| libc::sockaddr_in {
                #[cfg(target_os = "macos")]
                sin_len: std::mem::size_of::<libc::sockaddr_in>() as u8,
                sin_family: libc::AF_INET as libc::sa_family_t,
                sin_port: p.to_be(),
                sin_addr: libc::in_addr { s_addr: u32::from_ne_bytes(ip.octets()) },
                sin_zero: [0; 8],
            };
            let std::net::IpAddr::V4(other4) = other_ip else { panic!("v4 expected") };
            let local = sin(std::net::Ipv4Addr::LOCALHOST, client_end.port());
            let len = std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t;
            assert_eq!(libc::bind(fd, (&local as *const libc::sockaddr_in).cast(), len), 0, "share the client end");
            let remote = sin(other4, port);
            assert_eq!(libc::connect(fd, (&remote as *const libc::sockaddr_in).cast(), len), 0, "connect to the other server address");
            std::net::TcpStream::from_raw_fd(fd)
        };
        let (_b_srv, b_peer) = listener.accept().unwrap();
        assert_eq!(b_peer, client_end, "the server sees the same client end for both connections");
        assert!(os::holds(me, client_end, server_b));
        assert_eq!(os::scan(client_end, server_b).unwrap().holders, vec![me]);
        // Close A: its connection is no longer held, although B still holds the
        // same client end. Local-end-only matching would still say "held".
        drop(a);
        drop(_a_srv);
        std::thread::sleep(Duration::from_millis(100));
        assert!(!os::holds(me, client_end, server_a), "a closed connection is not inherited by another sharing its client end");
        assert!(os::scan(client_end, server_a).unwrap().holders.is_empty());
        assert!(os::holds(me, client_end, server_b));
        drop(b);
        // This process's ancestry walks to the root and its tty (if any) reads.
        assert!(os::ppid_of(me).is_some(), "parent of this process must be readable");
        let _ = os::tty_of(me);
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn local_non_loopback_ip() -> Option<std::net::IpAddr> {
        // A UDP "connect" sends nothing; it only picks the outbound address.
        let s = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
        s.connect("192.0.2.1:9").ok()?;
        let ip = s.local_addr().ok()?.ip();
        (!ip.is_loopback() && !ip.is_unspecified()).then_some(ip)
    }

    /// The memo only ever confirms a worker: an Outside verdict leaves nothing
    /// remembered, so the next request is decided by a full scan again.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn an_owner_verdict_is_never_remembered() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let client = std::net::TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (_srv, peer) = listener.accept().unwrap();
        let conn = Conn { peer, server: Some(listener.local_addr().unwrap()) };
        let origin = OsResolver.resolve(conn);
        let remembered = holder_memo().lock().unwrap().contains_key(&conn);
        assert_eq!(remembered, matches!(origin, Origin::Worker(_)), "memo written only for a worker ({origin:?})");
        drop(client);
    }

    /// Live check, run by hand on a host serving amux (`--ignored`): every
    /// loopback connection the running server holds right now, decided by the
    /// shipped OS lookups. Through `tailscale serve` (the owner's phone) the
    /// expected verdict is Remote; a local client's is Outside or Worker.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore]
    fn live_server_connections_are_decided() {
        let (_, out, _) = run_bounded(std::process::Command::new("/usr/sbin/lsof").args(["-nP", "-iTCP:8824", "-sTCP:ESTABLISHED"]), Duration::from_secs(5)).unwrap();
        let server_side: Vec<(SocketAddr, SocketAddr)> = out.lines().skip(1)
            .filter_map(|l| l.split_whitespace().nth(8))
            .filter_map(|n| n.split_once("->"))
            .filter_map(|(a, b)| Some((a.parse::<SocketAddr>().ok()?, b.parse::<SocketAddr>().ok()?)))
            .filter(|(srv, _)| srv.port() == 8824)
            .collect();
        assert!(!server_side.is_empty(), "no live connections to decide");
        for (server, peer) in server_side {
            let origin = decide(Conn { peer, server: Some(server) }, &OsLookups);
            let scan = os::scan(peer, server).ok();
            println!("LIVE {peer} -> {server}: {origin:?} holders={:?}", scan.map(|s| s.holders));
        }
    }

    // ---- the policy through the real send gate --------------------------
    //
    // A paused target holds a send in steering_queue after the group policy
    // runs, so these reach the shipped send_post without a tmux pane. Each
    // request carries ConnectInfo (and the server end) like a served socket.

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
        req.extensions_mut().insert(ServerLocal(SocketAddr::from(([127, 0, 0, 1], 8824))));
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

    /// The shipped decision behind the middleware and send gate, with fake
    /// lookups: what each OS answer becomes at the HTTP boundary.
    struct DecideWith(Fake);

    impl PeerResolver for DecideWith {
        fn resolve(&self, conn: Conn) -> Origin {
            decide(conn, &self.0)
        }
    }

    #[tokio::test]
    async fn os_answers_reach_the_send_gate_fail_closed() {
        let (state, dir, guard) = fixture();
        let _h = crate::api::settings::test_env::set_home(dir.path());
        drop(guard);
        let worker_panes = Panes { by_pid: HashMap::from([(100, "amux-caller".to_string())]), by_tty: HashMap::new() };
        let cases: Vec<(&str, Fake, &str)> = vec![
            ("failed inventory", Fake { cached: Err("x".into()), fresh: Err("tmux exited 1".into()), ..Fake::holders(&[301]) }, "worker_identity_unavailable"),
            ("unknown tmux error", Fake { fresh: panes_from_output(false, "", "tmux: something new"), ..Fake::holders(&[301]) }, "worker_identity_unavailable"),
            ("stale cache, fresh finds the worker", Fake { cached: Ok(Panes::default()), fresh: Ok(worker_panes.clone()), ..Fake::holders(&[101]) }, "worker_group_boundary"),
            ("worker at the front door", Fake { fresh: Ok(worker_panes.clone()), ..Fake::forwarded(&[101]) }, "worker_identity_unavailable"),
            ("no forwarder", Fake { tailscaled: Ok(vec![]), ..Fake::forwarded(&[]) }, "worker_origin_unattributed"),
        ];
        for (label, fake, code) in cases {
            let r = test_support::set_resolver(Arc::new(DecideWith(fake)));
            let (st, v) = post(&app(state.clone()), OWNER, &[]).await;
            assert_eq!((st, v["code"].clone()), (StatusCode::FORBIDDEN, json!(code)), "{label}: {v}");
            drop(r);
        }
        assert!(queued_senders(&state).is_empty(), "no failure lets a request through as the owner");
        // The owner's phone through tailscale serve sends as the owner.
        let r = test_support::set_resolver(Arc::new(DecideWith(Fake::forwarded(&[]))));
        let (st, v) = post(&app(state.clone()), OWNER, &[]).await;
        assert_eq!(st, StatusCode::OK, "forwarded phone: {v}");
        assert_eq!(queued_senders(&state), vec![String::new()]);
        drop(r);
    }

    /// The proof header is server-set only: a client cannot assert it.
    #[tokio::test]
    async fn a_client_supplied_process_proof_is_stripped() {
        let (_state, dir, _r) = fixture();
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
