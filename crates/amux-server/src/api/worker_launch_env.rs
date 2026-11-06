//! Import launch secrets at the provider boundary and verify process inheritance.
//! Environment values are consumed in memory only; diagnostics expose names/status.
use std::collections::BTreeSet;
use std::io;

// Separate setup refusal from a provider's own exit status.
const IMPORT_REFUSED_EXIT: i32 = 125;

pub(super) fn valid_import_key(key: &str) -> bool {
    key == super::worker_identity::TOKEN_ENV || super::vault_secrets::valid_key(key).is_ok()
}

/// The tmux command's status must be checked before eval: eval of empty output
/// succeeds even when tmux failed. Run this inside the provider's launch shell,
/// after profiles/scope files, on initial launches and fallback invocations alike.
pub(super) fn import_command(target: &str, keys: &[String], stale: &[String]) -> String {
    use super::session_verbs::sh_quote;
    let mut command = String::new();
    for key in keys {
        assert!(valid_import_key(key));
        command.push_str(&format!(
            "unset {key}; _amux_vault_env=$(tmux show-environment -s -t {} {key} 2>/dev/null) && \
             [ -n \"$_amux_vault_env\" ] && eval \"$_amux_vault_env\" 2>/dev/null && \
             [ \"${{{key}+present}}\" = present ] && export {key} || \
             {{ printf '%s\\n' 'verdict=vault_env_import_failed key={key}' >&2; exit {IMPORT_REFUSED_EXIT}; }}; ",
            sh_quote(target)
        ));
    }
    command.push_str("unset _amux_vault_env; ");
    for key in stale {
        assert!(valid_import_key(key));
        command.push_str(&format!("unset {key}; "));
    }
    command
}

struct ProcessEnvironment {
    argv: Vec<String>,
    names: BTreeSet<String>,
}

fn env_names(bytes: &[u8]) -> BTreeSet<String> {
    bytes
        .split(|b| *b == 0)
        .filter_map(|entry| {
            let end = entry.iter().position(|b| *b == b'=')?;
            std::str::from_utf8(&entry[..end]).ok().map(str::to_owned)
        })
        .collect()
}

#[cfg(target_os = "macos")]
fn process_environment(pid: u32) -> io::Result<ProcessEnvironment> {
    // KERN_PROCARGS2: argc, executable NUL, padding NULs, argc argv strings,
    // then NUL-separated environment entries. Never decode the value half.
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid as libc::c_int];
    let mut len = 0;
    unsafe {
        if libc::sysctl(
            mib.as_mut_ptr(),
            3,
            std::ptr::null_mut(),
            &mut len,
            std::ptr::null_mut(),
            0,
        ) != 0
        {
            return Err(io::Error::last_os_error());
        }
    }
    let mut bytes = vec![0u8; len];
    unsafe {
        if libc::sysctl(
            mib.as_mut_ptr(),
            3,
            bytes.as_mut_ptr().cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        ) != 0
        {
            return Err(io::Error::last_os_error());
        }
    }
    bytes.truncate(len);
    parse_procargs(&bytes)
}

#[cfg(any(target_os = "macos", test))]
fn parse_procargs(bytes: &[u8]) -> io::Result<ProcessEnvironment> {
    let invalid = || {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid process environment layout",
        )
    };
    let header: [u8; 4] = bytes
        .get(..4)
        .ok_or_else(invalid)?
        .try_into()
        .map_err(|_| invalid())?;
    let argc = i32::from_ne_bytes(header);
    if argc <= 0 {
        return Err(invalid());
    }
    let mut rest = bytes.get(4..).ok_or_else(invalid)?;
    rest = rest
        .get(rest.iter().position(|b| *b == 0).ok_or_else(invalid)? + 1..)
        .ok_or_else(invalid)?;
    while rest.first() == Some(&0) {
        rest = &rest[1..];
    }
    let mut argv = Vec::new();
    for _ in 0..argc {
        let end = rest.iter().position(|b| *b == 0).ok_or_else(invalid)?;
        argv.push(String::from_utf8_lossy(&rest[..end]).into_owned());
        rest = &rest[end + 1..];
    }
    Ok(ProcessEnvironment {
        argv,
        names: env_names(rest),
    })
}

#[cfg(target_os = "linux")]
fn process_environment(pid: u32) -> io::Result<ProcessEnvironment> {
    let argv = std::fs::read(format!("/proc/{pid}/cmdline"))?
        .split(|b| *b == 0)
        .filter(|arg| !arg.is_empty())
        .map(|arg| String::from_utf8_lossy(arg).into_owned())
        .collect();
    Ok(ProcessEnvironment {
        argv,
        names: env_names(&std::fs::read(format!("/proc/{pid}/environ"))?),
    })
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn process_environment(_pid: u32) -> io::Result<ProcessEnvironment> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "process environment probe unsupported",
    ))
}

fn is_provider(argv: &[String], binary: &str) -> bool {
    let base = |arg: &str| {
        std::path::Path::new(arg)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string()
    };
    let Some(executable) = argv.first().map(|arg| base(arg)) else {
        return false;
    };
    if executable == binary {
        return true;
    }
    // Match only an interpreter's entry point, never a shell's -c text or
    // another tool's ordinary argument that happens to name the provider.
    let interpreter = ["node", "nodejs", "bun", "python", "python3"].contains(&executable.as_str())
        || executable.starts_with("python3.");
    interpreter
        && argv.get(1).is_some_and(|arg| {
            std::path::Path::new(arg)
                .file_stem()
                .is_some_and(|stem| stem == binary)
        })
}

fn report(lane: &str, pid: u32, names: &BTreeSet<String>) {
    if !names.contains(super::worker_identity::TOKEN_ENV) {
        tracing::warn!(
            lane,
            pid,
            measured = true,
            n_considered = 1,
            verdict = "worker_token_missing_in_process",
            "rule 10: provider process did not inherit its worker token"
        );
    } else {
        tracing::info!(
            lane,
            pid,
            measured = true,
            n_considered = 1,
            verdict = "worker_token_present_in_process",
            "rule 10: provider token inheritance verified"
        );
    }
}

/// The caller supplies only descendants of this launch's pane. Select the
/// provider itself, never its parent shell or a later tool process. Unreadable
/// processes and unmatched custom commands are unmeasured, never a clean bill.
pub(super) fn check(lane: &str, binary: &str, pids: &[u32]) {
    for &pid in pids {
        if let Ok(env) = process_environment(pid) {
            if is_provider(&env.argv, binary) {
                report(lane, pid, &env.names);
                return;
            }
        }
    }
    tracing::warn!(
        lane,
        measured = false,
        n_considered = 0,
        verdict = "worker_token_process_check_unmeasured",
        "rule 10: provider pid or its environment could not be read"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn shell_import(tmux_body: &str, after: &str) -> std::process::Output {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let tmux = root.path().join("tmux");
        std::fs::write(&tmux, format!("#!/bin/sh\n{tmux_body}\n")).unwrap();
        std::fs::set_permissions(&tmux, std::fs::Permissions::from_mode(0o700)).unwrap();
        let import = import_command(
            "=amux-fixture",
            &[super::super::worker_identity::TOKEN_ENV.into()],
            &["REMOVED_KEY".into()],
        );
        let program = super::super::session_verbs::provider_command_in_workspace(
            root.path().to_str().unwrap(),
            after,
            false,
            &import,
        );
        Command::new("/bin/sh")
            .args(["-c", &program])
            .env_clear()
            .env("PATH", format!("{}:/usr/bin:/bin", root.path().display()))
            .env("HOME", root.path())
            .env("REMOVED_KEY", "stale")
            .env("AMUX_WORKER_TOKEN", "obsolete-inherited-token")
            .output()
            .unwrap()
    }

    #[test]
    fn worker_token_failed_tmux_import_refuses_provider() {
        let out = shell_import("exit 1", "printf provider-started");
        assert_eq!(out.status.code(), Some(IMPORT_REFUSED_EXIT));
        assert!(
            out.stdout.is_empty(),
            "provider must not launch after an unreadable vault"
        );
        assert!(String::from_utf8_lossy(&out.stderr)
            .contains("verdict=vault_env_import_failed key=AMUX_WORKER_TOKEN"));
    }

    #[test]
    fn worker_token_empty_or_unset_tmux_output_refuses_obsolete_token() {
        for output in [
            "exit 0",
            "printf '%s\\n' 'unset AMUX_WORKER_TOKEN;'",
            "printf '%s\\n' '# no assignment'",
        ] {
            let out = shell_import(output, "printf provider-started");
            assert_eq!(out.status.code(), Some(IMPORT_REFUSED_EXIT));
            assert!(out.stdout.is_empty());
        }
    }

    #[test]
    fn worker_token_import_exports_token_and_removes_stale_keys() {
        // A child process must inherit it; checking shell expansion alone is
        // insufficient to reproduce a variable missing from provider env.
        let out = shell_import("printf '%s\\n' 'AMUX_WORKER_TOKEN=fixture-token; export AMUX_WORKER_TOKEN;'",
            "/bin/sh -c 'test \"$AMUX_WORKER_TOKEN\" = fixture-token && test \"${REMOVED_KEY+set}\" != set && printf inherited'");
        assert!(out.status.success());
        assert_eq!(out.stdout, b"inherited");
        assert!(out.stderr.is_empty());
    }

    #[test]
    fn worker_token_process_parser_reads_names_after_exact_argc_only() {
        let mut bytes = 3_i32.to_ne_bytes().to_vec();
        bytes.extend_from_slice(b"/bin/python3\0\0python3\0/private/fixture/claude\0--resume\0AMUX_WORKER_TOKEN=secret with spaces=extra\0OTHER_KEY=value\0\0");
        let env = parse_procargs(&bytes).unwrap();
        assert!(is_provider(&env.argv, "claude"));
        assert_eq!(
            env.names,
            BTreeSet::from(["AMUX_WORKER_TOKEN".into(), "OTHER_KEY".into()])
        );
        assert!(!is_provider(
            &["/bin/sh".into(), "-c".into(), "claude".into()],
            "claude"
        ));
        assert!(!is_provider(
            &["amux".into(), "send".into(), "claude".into()],
            "claude"
        ));
        for malformed in [&bytes[..3], &bytes[..10], &bytes[..30]] {
            assert!(parse_procargs(malformed).is_err());
        }
    }
}
