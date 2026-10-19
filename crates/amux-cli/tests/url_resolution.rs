//! AMUX-2672: a bare `amux-rs <verb>` must reach the server that is running.
//!
//! The default was `https://localhost:8823`, the port the Rust server used while
//! Python owned 8822. Python retired, the Rust server took over 8822 AND 8824,
//! and nothing has listened on 8823 since — so every bare invocation died on
//! connect. The failure mode is what made it expensive: a connection error looks
//! exactly like the server being down, so the CLI's own misconfiguration reads as
//! a server fault (it cost a wrong diagnosis on AMUX-2653).
//!
//! It then moved 8822 -> 8824 (2026-08-10): 8822 is the RETIRED address, alive
//! only via a countdown bind for pre-cutover processes, so defaulting there was
//! the same bug with a later expiry date. 8824 is what install.sh configures.
//!
//! No test here may contact the live server (2026-10-19). The two default-URL
//! cases used to run a bare `amux-rs health`, which reaches 8824 whenever a
//! server is up. They now run with AMUX_ISOLATED_RUN=1 set explicitly, so the
//! default is observed as the isolated refusal ("unset_would_use_builtin")
//! aimed at the dead port; the default's VALUE is pinned by the
//! `resolve_url_from` unit tests and by legacy_port_guard.rs. Every other case
//! names its own dead loopback port, so the flag never decides its result.
//!
//! End-to-end rather than unit, because the bug was in the wiring — `resolve_url`
//! could be perfect and the arg still bound to the wrong default.

use std::process::Command;

/// Runs `amux-rs health` with a controlled env and returns stderr.
/// `--help`-style verbs would not exercise the URL at all.
fn health_stderr(env: &[(&str, Option<&str>)]) -> String {
    let mut c = Command::new(env!("CARGO_BIN_EXE_amux-rs"));
    c.arg("health");
    // Start from a clean slate: the test process inherits a real AMUX_URL when
    // run inside amux, which would silently decide the result.
    c.env_remove("AMUX_URL").env_remove("AMUX_RS_URL").env_remove("AMUX_ISOLATED_RUN");
    for (k, v) in env {
        match v {
            Some(val) => {
                c.env(k, val);
            }
            None => {
                c.env_remove(k);
            }
        }
    }
    let out = c.output().expect("run amux-rs health");
    String::from_utf8_lossy(&out.stderr).to_string()
}

/// A bare invocation uses the built-in default, and under isolation that
/// default is refused before any request: this is also the test of the
/// AMUX_ISOLATED_RUN wiring (with it ignored, the child would try the live
/// default and print no refusal).
#[test]
fn bare_invocation_is_refused_under_isolation_and_targets_no_live_port() {
    let err = health_stderr(&[("AMUX_ISOLATED_RUN", Some("1"))]);
    assert!(
        err.contains("verdict=isolated_live_endpoint_refused reason=unset_would_use_builtin"),
        "the built-in default must be refused under isolation; stderr: {err}"
    );
    assert!(err.contains("127.0.0.1:9"), "the dead port must be the target; stderr: {err}");
    for port in [":8822", ":8823", ":8824"] {
        assert!(!err.contains(&format!("localhost{port}")), "targeted {port}; stderr: {err}");
    }
}

#[test]
fn a_live_port_in_any_spelling_is_refused_under_isolation() {
    let err = health_stderr(&[
        ("AMUX_ISOLATED_RUN", Some("1")),
        ("AMUX_URL", Some("HTTPS://user@LOCALHOST:08824/")),
    ]);
    assert!(
        err.contains("reason=live_port") && err.contains("127.0.0.1:9"),
        "a leading-zero, uppercase, userinfo spelling of the live port must be refused; stderr: {err}"
    );
}

#[test]
fn amux_url_is_honoured_so_a_session_reaches_its_own_server() {
    // The whole point of the fallback: every running amux session has AMUX_URL
    // in its env, including when the server is not on localhost.
    let err = health_stderr(&[("AMUX_URL", Some("https://127.0.0.1:9999"))]);
    assert!(
        err.contains("9999"),
        "AMUX_URL must be used when set; stderr: {err}"
    );
}

#[test]
fn amux_rs_url_outranks_amux_url() {
    let err = health_stderr(&[
        ("AMUX_URL", Some("https://127.0.0.1:9998")),
        ("AMUX_RS_URL", Some("https://127.0.0.1:9997")),
    ]);
    assert!(
        err.contains("9997") && !err.contains("9998"),
        "the explicit override must win; stderr: {err}"
    );
}

#[test]
fn an_empty_env_value_falls_through_instead_of_targeting_nothing() {
    // An exported-but-empty AMUX_URL is ordinary shell breakage; it must not
    // produce a request to "" (which fails with a useless parse error).
    let err = health_stderr(&[("AMUX_URL", Some("")), ("AMUX_ISOLATED_RUN", Some("1"))]);
    assert!(
        err.contains("reason=unset_would_use_builtin") && err.contains("127.0.0.1:9"),
        "empty AMUX_URL must fall through to the (refused) default; stderr: {err}"
    );
}
