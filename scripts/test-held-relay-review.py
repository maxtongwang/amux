#!/usr/bin/env python3
"""Run held-relay core regressions with cached Rust deps when Cargo is unavailable.

Extracts production functions and their in-tree assertions without rewriting them.
The small adapter replaces only AppState/Store events and the home resolver; SQL
runs on a real scratch SQLite database. This is not a full-crate or tmux check.
"""
import argparse
import hashlib
import sys
from pathlib import Path
import re
import subprocess
import tempfile

REPO = Path(__file__).resolve().parents[1]
SOURCE = "crates/amux-server/src/api/session_verbs.rs"
FUNCTIONS = [
    "is_slash_command", "slash_command_split", "origin_stamp_split", "delivery_parts",
    "origin_stamped", "paste_chip_fits", "held_delivery_matching",
    "composer_holds_this_delivery", "stamped_held_delivery", "held_composer_delivery_matching",
    "transcript_unwrap", "stuck_rows_matching", "held_amux_composer", "held_amux_delivery",
    "amux_delivered_text", "reconcile_stuck_on_submit", "direct_draft_watch_budget",
    "load_meta", "save_meta", "reserve_held_relay_enter",
]
TESTS = [
    "owner_chip_matching_a_relay_line_count_never_authorises_keys",
    "owner_plain_copy_never_authorises_keys_and_stamps_must_be_in_position",
    "combined_receipt_consumes_only_the_submitted_sequence",
    "combined_enter_reconciles_each_held_verdict_and_excludes_other_rows",
    "ordered_relays_fill_the_whole_composer",
    "rows_are_consumed_once_and_owner_word_boundaries_are_not_substring_matches",
    "chips_and_wrapping_match_only_complete_known_deliveries",
    "short_and_slash_deliveries_never_authorise_idle_enter",
    "history_and_watch_window_remain_thirty_minutes",
    "enter_cap_survives_watch_respawn_and_logs_exhaustion_once",
    "every_recovery_enter_reserves_the_shared_sequence_budget",
    "attempt_persistence_failure_refuses_enter",
    "history_recovery_is_lane_scoped_recent_and_uses_durable_bytes",
]
SUPPORT = r'''
#![allow(dead_code)]
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};
#[derive(Clone)]
struct AppState {
    store: std::sync::Arc<db::Store>, started: std::time::Instant,
    build_hash: String, auth_token: Option<String>,
    reconciled: std::sync::Arc<std::sync::atomic::AtomicBool>,
}
mod amux_core { pub mod revision { pub enum EntityType { Message } pub enum MutationKind { Updated } } }
mod db {
    use super::*;
    pub struct PendingEvent { pub entity_type: amux_core::revision::EntityType, pub entity_id: String,
        pub mutation: amux_core::revision::MutationKind, pub payload: Option<Value> }
    pub struct WriteOutcome { pub applied: bool, pub events: Vec<PendingEvent> }
    pub struct Store(std::sync::Mutex<rusqlite::Connection>);
    impl Store {
        pub fn open(path: &Path) -> rusqlite::Result<Self> {
            let c = rusqlite::Connection::open(path)?;
            c.execute_batch("CREATE TABLE cmd_history(id INTEGER PRIMARY KEY, text TEXT NOT NULL,
                type TEXT DEFAULT 'user', session TEXT NOT NULL, origin TEXT NOT NULL DEFAULT '',
                ts INTEGER NOT NULL, submit_verdict TEXT, delivery TEXT, delivered_at INTEGER,
                reconciled_at INTEGER);")?;
            Ok(Self(std::sync::Mutex::new(c)))
        }
        pub fn read(&self) -> rusqlite::Result<std::sync::MutexGuard<'_, rusqlite::Connection>> { Ok(self.0.lock().unwrap()) }
        pub fn write(&self, f: impl FnOnce(&mut rusqlite::Connection) -> rusqlite::Result<WriteOutcome>) -> rusqlite::Result<WriteOutcome> {
            f(&mut self.0.lock().unwrap())
        }
        pub async fn write_async(&self, f: impl FnOnce(&mut rusqlite::Connection) -> rusqlite::Result<WriteOutcome>) -> rusqlite::Result<WriteOutcome> { self.write(f) }
    }
}
fn now_i64() -> i64 { std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64 }
fn sessions_dir() -> PathBuf { PathBuf::from(std::env::var_os("AMUX_HOME").expect("scratch AMUX_HOME required")).join("sessions") }
fn meta_path(name: &str) -> PathBuf { sessions_dir().join(format!("{name}.meta.json")) }
mod api { pub mod settings { pub mod test_env {
    pub struct HomeGuard(Option<std::ffi::OsString>);
    pub fn set_home(path: &std::path::Path) -> HomeGuard {
        let old = std::env::var_os("AMUX_HOME"); std::env::set_var("AMUX_HOME", path); HomeGuard(old)
    }
    impl Drop for HomeGuard { fn drop(&mut self) {
        if let Some(old) = &self.0 { std::env::set_var("AMUX_HOME", old); } else { std::env::remove_var("AMUX_HOME"); }
    } }
} } }
'''


def function(source, name):
    match = re.search(r"^([ ]*)(?:pub(?:\([^)]*\))? )?(?:async )?fn " + name + r"\(", source, re.M)
    if not match:
        raise ValueError(f"missing function {name}")
    first_line = source[match.start():].splitlines()[0]
    if first_line.rstrip().endswith("}"):
        return first_line
    end = source.index("\n" + match[1] + "}", match.start()) + len(match[1]) + 2
    return source[match.start():end]


def mutations():
    sequence = "if let Some(sent) = stamped_held_delivery(text, origin, name) {"
    changes = [
        ("owner-chip", "let prefix: String = prefix.split_whitespace().collect();", "let prefix: String = String::new();", TESTS[0]),
        ("owner-copy", sequence, "if let Some(sent) = Some(text.clone()) {", TESTS[1]),
        ("stamped-relay", sequence, "if let Some(sent) = None::<String> {", "chips_and_wrapping_match_only_complete_known_deliveries"),
        ("enter-cap", "if presses >= HELD_RELAY_MAX_ENTER_ATTEMPTS {", "if presses > HELD_RELAY_MAX_ENTER_ATTEMPTS {", "enter_cap_survives_watch_respawn_and_logs_exhaustion_once"),
        ("watcher-cap-wiring", "if !reserve_held_relay_enter(&name, &held) {", "if held.is_empty() {", "every_recovery_enter_reserves_the_shared_sequence_budget"),
        ("combined-receipt", "return ids;", "return ids.into_iter().take(1).collect();", "combined_receipt_consumes_only_the_submitted_sequence"),
        ("deferred-unverified-select", "SELECT id, text, origin FROM cmd_history WHERE session=?1 \\\n                     AND (submit_verdict IN ('stuck','deferred','unverified')", "SELECT id, text, origin FROM cmd_history WHERE session=?1 \\\n                     AND (submit_verdict IN ('stuck')", "combined_enter_reconciles_each_held_verdict_and_excludes_other_rows"),
        ("deferred-unverified-update", "reconciled_at=?2 WHERE id=?1 AND (submit_verdict IN ('stuck','deferred','unverified')", "reconciled_at=?2 WHERE id=?1 AND (submit_verdict IN ('stuck')", "combined_enter_reconciles_each_held_verdict_and_excludes_other_rows"),
        ("candidate-age", "const HELD_RELAY_WINDOW_S: i64 = 30 * 60;", "const HELD_RELAY_WINDOW_S: i64 = 6 * 3600;", "history_recovery_is_lane_scoped_recent_and_uses_durable_bytes"),
        ("persist-before-enter", "if load_meta(name).get(HELD_RELAY_ATTEMPTS_KEY) != meta.get(HELD_RELAY_ATTEMPTS_KEY) {", "if false && load_meta(name).get(HELD_RELAY_ATTEMPTS_KEY) != meta.get(HELD_RELAY_ATTEMPTS_KEY) {", "attempt_persistence_failure_refuses_enter"),
        ("warn-once", 'if row["warned"] != true {', 'if row["warned"] != false {', "enter_cap_survives_watch_respawn_and_logs_exhaustion_once"),
    ]
    original = hashlib.sha256((REPO / SOURCE).read_bytes()).digest()
    for label, old, new, case in changes:
        command = [str(REPO / "scripts/mutate.sh"), "run", SOURCE, old, new, "--", sys.executable, str(Path(__file__).resolve()), "--case", case]
        result = subprocess.run(command, cwd=REPO, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
        if hashlib.sha256((REPO / SOURCE).read_bytes()).digest() != original:
            raise RuntimeError(f"{label}: source not restored")
        if result.returncode != 101 or "test result: FAILED. 0 passed; 1 failed" not in result.stdout or case not in result.stdout or "mutate revert: LANDED" not in result.stdout:
            print(result.stdout)
            raise RuntimeError(f"{label}: not killed by its named assertion")
        print(f"{label}: KILLED by {case}; exit 101; source restored", flush=True)
    print(f"{len(changes)} mutations killed; source hash restored after each")
    return 0


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--mutations", action="store_true", help="break each required rule in turn and require its named assertion to fail")
    parser.add_argument("--source-ref", help="read production functions from a Git ref; assertions stay current")
    parser.add_argument("--case", action="append", choices=TESTS, help="run selected regression(s)")
    args = parser.parse_args()
    if args.mutations:
        if args.source_ref or args.case:
            parser.error("--mutations cannot be combined with --source-ref or --case")
        return mutations()
    current = (REPO / SOURCE).read_text()
    source = subprocess.check_output(["git", "show", f"{args.source_ref}:{SOURCE}"], cwd=REPO, text=True) if args.source_ref else current
    # Earlier revisions lack these helpers; use current transport unwrapping and
    # current cap implementation only when a selected test does not exercise it.
    chunks = [function(source if re.search(r"^.*fn " + name + r"\(", source, re.M) else current, name) for name in FUNCTIONS]
    constants = re.findall(r"^const (?:HELD_RELAY_[A-Z_]+|DIRECT_DRAFT_[A-Z_]+):[^\n]+", source, re.M)
    if not args.source_ref:
        assert len(constants) >= 3, "missing shipped relay bounds"
    if args.source_ref:
        constants += [c for c in re.findall(r"^const HELD_RELAY_[A-Z_]+:[^\n]+", current, re.M)]
    pane = (REPO / "crates/amux-server/src/api/pane_prompts.rs").read_text()
    helpers = 'mod pane_prompts { const CONTAINS_MIN_CHARS: usize = 16;\n' + function(pane, "normalize") + '\n' + function(pane, "same_text") + '\n}\n'
    cases = args.case or TESTS
    tests = '\n'.join(("#[tokio::test]\n" if re.match(r"\s*(?:pub(?:\([^)]*\))? )?async fn", function(current, name)) else "#[test]\n") + function(current, name) for name in cases)
    body = SUPPORT + '\n'.join(constants) + helpers + '\nmod production { use super::*;\n' + '\n'.join(chunks) + '\n' + function(current, "squashed") + '\n' + function(current, "test_state") + '\n' + tests + '\n}'
    deps = Path.home() / ".amux/rust-build-target/debug/deps"
    with tempfile.TemporaryDirectory(prefix="held-relay-review-") as scratch:
        root = Path(scratch)
        file = root / "regressions.rs"
        file.write_text(body)
        (root / "session_verbs.rs").write_text(source)
        command = ["rustc", "--edition=2021", "--test", str(file), "-L", f"dependency={deps}", "-o", str(root / "tests")]
        for name in ["regex", "serde_json", "rusqlite", "tokio", "sha2", "hex", "tempfile", "tracing", "ulid"]:
            libraries = sorted(deps.glob(f"lib{name}-*.rlib"))
            if not libraries:
                raise RuntimeError(f"cached {name} unavailable; run Cargo in a permitted environment")
            command += ["--extern", f"{name}={libraries[0]}"]
        subprocess.run(command, cwd=REPO, check=True)
        print("Extracted production core; scratch Store/home adapters; no tmux or server", flush=True)
        return subprocess.run([str(root / "tests"), "--test-threads=1", "--nocapture"], cwd=REPO).returncode


if __name__ == "__main__":
    raise SystemExit(main())
