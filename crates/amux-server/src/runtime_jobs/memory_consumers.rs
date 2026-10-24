//! Pressure diagnostics must include compressed memory on macOS. RSS alone
//! hid a 24 GiB menu-bar process behind 70 MiB resident during AMUX-4417.
//! This snapshot is diagnostic only; it never selects processes to terminate.

use std::process::Command;
#[cfg(target_os = "macos")]
use std::{process::Stdio, time::Duration};
#[cfg(target_os = "macos")]
use wait_timeout::ChildExt;

#[derive(Debug, serde::Serialize)]
pub(super) struct Snapshot {
    pub measured: bool,
    pub n_considered: usize,
    pub metric: &'static str,
    pub why_unmeasured: Option<String>,
    pub consumers: Vec<Consumer>,
}

#[derive(Debug, serde::Serialize)]
pub(super) struct Consumer {
    pid: u32,
    command: String,
    bytes: u64,
    compressed_bytes: Option<u64>,
}

#[cfg(any(target_os = "macos", test))]
fn size_bytes(raw: &str) -> Option<u64> {
    let raw = raw.trim_end_matches(['+', '-']);
    let unit = raw.chars().last()?;
    let power = match unit {
        'B' => 0,
        'K' => 1,
        'M' => 2,
        'G' => 3,
        'T' => 4,
        _ => return None,
    };
    let number = raw[..raw.len() - 1].parse::<f64>().ok()?;
    let bytes = number * 1024_f64.powi(power);
    (bytes.is_finite() && bytes >= 0.0 && bytes < u64::MAX as f64).then_some(bytes as u64)
}

#[cfg(any(target_os = "macos", test))]
fn parse_top(raw: &str) -> Result<Vec<Consumer>, String> {
    let mut rows = Vec::new();
    let mut header = false;
    for line in raw.lines() {
        let mut fields = line.split_whitespace();
        if !header {
            header = fields.collect::<Vec<_>>() == ["PID", "MEM", "CMPRS", "COMMAND"];
            continue;
        }
        let Some(pid) = fields.next() else { continue };
        let row = (|| {
            let pid = pid.trim_end_matches(['*', '+', '-']).parse::<u32>().ok()?;
            let bytes = size_bytes(fields.next()?)?;
            let compressed_bytes = Some(size_bytes(fields.next()?)?);
            let command = fields.collect::<Vec<_>>().join(" ");
            (!command.is_empty()).then_some(Consumer {
                pid,
                command,
                bytes,
                compressed_bytes,
            })
        })()
        .ok_or_else(|| {
            format!(
                "top returned a malformed process row: {}",
                line.chars().take(160).collect::<String>()
            )
        })?;
        rows.push(row);
    }
    if !header || rows.is_empty() {
        return Err("top returned no measurable process rows".into());
    }
    Ok(rows)
}

/// How long `top` gets before the snapshot gives up (AMUX-4790).
///
/// 5s, and this host could not meet it: `/usr/bin/top -l 1` was timed at 8.1s
/// and 8.3s at load average 14, and 36.3s / 28.2s / 34.0s at load 38. So
/// `snapshot()` answered `measured: false` here permanently, and mac-health's
/// memory-pressure WARN — whose entire job is to NAME the top consumers, "a
/// human's call, not this job's" — would have named nobody, at exactly the
/// moment it matters, since the host is slowest at `top` precisely when it is
/// under the pressure that triggers that arm.
///
/// 60s, NOT the 30s AMUX-4790's own next_action proposed. That card called 30s
/// "comfortably above the 36.3s worst case", which is simply wrong: 30 < 36.3,
/// so the proposed replacement could not meet the measurement that motivated
/// it. The assertion in this module's test is what caught it, which is the
/// argument for asserting a budget against its measurement rather than against
/// a number someone reasoned to.
///
/// 60s is 1.65x the 36.3s worst case and ~3.3% of the default 1800s mac-health
/// tick, on an arm only reached under pressure. `warn_on_thin_headroom` below
/// fires above 30s, so the next host to grow into this announces itself while
/// the probe still answers: any fixed budget becomes a silent cliff on the day
/// a host grows past it (the AMUX-4791 lesson, one dimension over).
///
/// Affordable because `one_pass` runs inside `tokio::task::spawn_blocking`
/// (mac_health.rs), so a long probe costs a blocking-pool thread and not a
/// runtime worker. I had this backwards at first and the card records it.
#[cfg(target_os = "macos")]
const PROBE_TIMEOUT: Duration = Duration::from_secs(60);

/// The one reason an unmeasured snapshot is the HOST's fault rather than this
/// module's. DERIVED from the deadline: the string used to spell "5s" beside a
/// separate `from_secs(5)`, so raising one silently made the other a lie.
#[cfg(target_os = "macos")]
fn probe_timeout_reason() -> String {
    format!("memory probe timed out after {}s", PROBE_TIMEOUT.as_secs())
}

/// Announce a probe that is closing on its deadline, while it still answers.
///
/// Same shape as the byte-cap warning in `log_retention` (AMUX-4791): the
/// failure this comes from had no early signal, it simply stopped measuring.
/// Half the budget is arbitrary; announcing BEFORE the cliff is not.
#[cfg(target_os = "macos")]
fn warn_on_thin_headroom(elapsed: Duration, deadline: Duration) -> bool {
    if elapsed.saturating_mul(2) <= deadline {
        return false;
    }
    tracing::warn!(
        probe_ms = elapsed.as_millis() as u64,
        deadline_ms = deadline.as_millis() as u64,
        pct_of_deadline = (elapsed.as_millis() as u64 * 100)
            .checked_div(deadline.as_millis() as u64)
            .unwrap_or(0),
        "memory probe is past half its deadline; the snapshot stops measuring entirely once it is exceeded (AMUX-4790)"
    );
    true
}

#[cfg(target_os = "macos")]
fn bounded_output(cmd: &mut Command) -> Result<String, String> {
    // Only the top five rows are requested, keeping output below pipe capacity.
    // Absolute executable paths and C locale work under launchd as well.
    let mut child = cmd
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    let started = std::time::Instant::now();
    match child.wait_timeout(PROBE_TIMEOUT) {
        Ok(Some(status)) if status.success() => {}
        Ok(Some(status)) => return Err(format!("memory probe exited {status}")),
        result => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(match result {
                Err(error) => format!("memory probe wait failed: {error}"),
                _ => probe_timeout_reason(),
            });
        }
    }
    warn_on_thin_headroom(started.elapsed(), PROBE_TIMEOUT);
    let output = child.wait_with_output().map_err(|e| e.to_string())?;
    String::from_utf8(output.stdout).map_err(|e| e.to_string())
}

pub(super) fn snapshot() -> Snapshot {
    snapshot_with_probe(|| {
        #[cfg(target_os = "macos")]
        {
            bounded_output(Command::new("/usr/bin/top").args([
                "-l",
                "1",
                "-o",
                "mem",
                "-n",
                "5",
                "-stats",
                "pid,mem,cmprs,command",
            ]))
            .and_then(|raw| parse_top(&raw))
        }
        #[cfg(not(target_os = "macos"))]
        {
            rss_snapshot()
        }
    })
}

/// Acquisition is injectable; measurement/error semantics and metric naming
/// are shared with production even when the host forbids process inspection.
fn snapshot_with_probe(probe: impl FnOnce() -> Result<Vec<Consumer>, String>) -> Snapshot {
    #[cfg(target_os = "macos")]
    let metric = "macos_top_mem_includes_compressed";
    #[cfg(not(target_os = "macos"))]
    let metric = "rss_only";
    let result = probe();
    match result {
        Ok(consumers) => Snapshot {
            measured: true,
            n_considered: consumers.len(),
            metric,
            why_unmeasured: None,
            consumers,
        },
        Err(error) => {
            tracing::warn!(verdict="memory_consumer_probe_unmeasured", measured=false, n_considered=0,
                metric, reason=%error, "memory consumer probe unavailable; no consumers measured");
            Snapshot {
                measured: false,
                n_considered: 0,
                metric,
                why_unmeasured: Some(error),
                consumers: Vec::new(),
            }
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn rss_snapshot() -> Result<Vec<Consumer>, String> {
    let output = Command::new("ps")
        .args(["-eo", "pid=,rss=,comm="])
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(format!("ps exited {}", output.status));
    }
    let mut rows = Vec::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let mut fields = line.split_whitespace();
        let row = (|| {
            Some(Consumer {
                pid: fields.next()?.parse().ok()?,
                bytes: fields.next()?.parse::<u64>().ok()?.checked_mul(1024)?,
                command: fields.collect::<Vec<_>>().join(" "),
                compressed_bytes: None,
            })
        })()
        .ok_or_else(|| "ps returned a malformed process row".to_string())?;
        rows.push(row);
    }
    rows.sort_by_key(|r| std::cmp::Reverse(r.bytes));
    rows.truncate(5);
    if rows.is_empty() {
        return Err("ps returned no process rows".into());
    }
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compressed_hogs_and_spaced_names_survive_the_snapshot() {
        let rows = parse_top("Processes: 1206 total\nPhysMem: 95G used\n\nPID MEM CMPRS COMMAND\n567* 54G 49G fseventsd\n1715+ 24G 24G Python\n76850- 12G 12G Activity Monitor\n").unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].bytes, 54 * 1024_u64.pow(3));
        assert_eq!(rows[1].compressed_bytes, Some(24 * 1024_u64.pow(3)));
        assert_eq!(rows[2].command, "Activity Monitor");
        assert_eq!(rows[2].pid, 76850);
    }

    #[test]
    fn missing_or_partial_measurements_cannot_look_healthy() {
        for text in [
            "",
            "permission denied",
            "PID MEM CMPRS COMMAND\n",
            "PID MEM CMPRS COMMAND\n1 24G ? Python",
            "PID MEM CMPRS COMMAND\n1 24G 23G Python\n2 broken",
        ] {
            assert!(parse_top(text).is_err(), "{text}");
        }
        for size in ["", "-1G", "NaNG", "infG", "99P", "1e50T", "10"] {
            assert_eq!(size_bytes(size), None, "{size}");
        }
        assert_eq!(size_bytes("1.5G+"), Some(1_610_612_736));
        assert_eq!(size_bytes("12M-"), Some(12 * 1024 * 1024));
        assert_eq!(size_bytes("0B"), Some(0));
    }

    /// AMUX-4790: the deadline is ONE fact, and its approach is announced.
    ///
    /// The reason string used to spell "5s" beside a separate `from_secs(5)`,
    /// so raising the deadline would have left the message asserting a number
    /// the code no longer used — the kind of wrong that reads as measured
    /// because it sits inside an otherwise-computed sentence.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_probe_deadline_is_one_fact_and_its_approach_is_announced() {
        // DERIVED: the message must name the deadline actually in force, so a
        // future change to one cannot leave the other behind.
        assert_eq!(
            probe_timeout_reason(),
            format!("memory probe timed out after {}s", PROBE_TIMEOUT.as_secs())
        );
        assert!(
            probe_timeout_reason().contains(&PROBE_TIMEOUT.as_secs().to_string()),
            "{}",
            probe_timeout_reason()
        );

        // The deadline must clear the worst case this card was opened on:
        // `top -l 1` at 36.3s under load average 38 on this host. This cell
        // caught the card's own next_action, which proposed 30s and described
        // it as "comfortably above the 36.3s worst case" — a budget must be
        // asserted against its MEASUREMENT, not against a number reasoned to.
        const MEASURED_WORST_CASE: Duration = Duration::from_millis(36_300);
        assert!(
            PROBE_TIMEOUT > MEASURED_WORST_CASE,
            "5s could not be met here; a replacement that still cannot is not a fix: \
             {PROBE_TIMEOUT:?} vs measured {MEASURED_WORST_CASE:?}"
        );

        // The SHIPPED headroom predicate, called rather than restated.
        // Strictly more than half, so an exactly-half probe stays quiet and a
        // line in the log cannot be read as "any slow probe".
        for (elapsed_ms, deadline_ms, want) in [
            (30_000u64, 60_000u64, false),
            (30_001, 60_000, true),
            (0, 60_000, false),
            // The measurements that opened this card, against the old and new
            // deadlines: 8.3s at load 14 and 36.3s at load 38.
            (8_300, 5_000, true),
            (8_300, 60_000, false),
            (36_300, 60_000, true),
        ] {
            assert_eq!(
                warn_on_thin_headroom(
                    Duration::from_millis(elapsed_ms),
                    Duration::from_millis(deadline_ms)
                ),
                want,
                "elapsed={elapsed_ms}ms deadline={deadline_ms}ms"
            );
        }
    }

    /// Drive the production probe boundary with deterministic process output.
    /// Live host permissions and load cannot stand in for parser correctness.
    #[test]
    fn native_memory_snapshot_is_measured_and_names_its_metric() {
        let result =
            snapshot_with_probe(|| parse_top("PID MEM CMPRS COMMAND\n42 2G 1G fixture-process\n"));
        // UNCONDITIONAL: the metric names which probe ran, which is a property
        // of this code and never of the host, so no load excuses it.
        #[cfg(target_os = "macos")]
        assert_eq!(
            result.metric, "macos_top_mem_includes_compressed",
            "{result:?}"
        );
        #[cfg(not(target_os = "macos"))]
        assert_eq!(result.metric, "rss_only", "{result:?}");

        assert!(
            result.measured,
            "fixture must exercise the measured branch: {result:?}"
        );
        assert!(result.n_considered > 0, "{result:?}");
        assert_eq!(result.n_considered, result.consumers.len(), "{result:?}");
        assert!(result.consumers.iter().any(|r| r.bytes > 0), "{result:?}");
        assert!(
            result.why_unmeasured.is_none(),
            "a measured snapshot must not also carry a reason it failed: {result:?}"
        );
        #[cfg(target_os = "macos")]
        assert!(
            result
                .consumers
                .iter()
                .all(|r| r.compressed_bytes.is_some()),
            "{result:?}"
        );
    }
    #[test]
    fn denied_and_malformed_memory_probes_remain_explicitly_unmeasured() {
        let denied = "Operation not permitted (os error 1)".to_string();
        let malformed =
            parse_top("PID MEM CMPRS COMMAND\n42 broken 0B fixture-process\n").unwrap_err();
        for error in [denied, malformed] {
            let result = snapshot_with_probe(|| Err(error.clone()));
            assert!(!result.measured, "{result:?}");
            assert_eq!(result.n_considered, 0, "{result:?}");
            assert!(result.consumers.is_empty(), "{result:?}");
            assert_eq!(result.why_unmeasured.as_deref(), Some(error.as_str()));
        }
    }
}
