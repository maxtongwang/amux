#!/usr/bin/env python3
"""Run provenance policy and process-ancestry unit tests without building a server."""
import argparse
import re
import unittest
from pathlib import Path
import subprocess
import tempfile


def item(source, start, end):
    """Extract exact shipped items; missing boundaries fail instead of skipping tests."""
    return source[source.index(start):source.index(end, source.index(start))]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.parse_args()
    repo = Path(__file__).resolve().parents[1]
    api = repo / "crates/amux-server/src/api"
    source = (api / "process_identity.rs").read_text()
    # The OS boundary already injects Lookups/PeerResolver. Test its exact pure
    # ancestry classifier too, with fake process tables and no tmux invocation.
    ancestry = "use std::collections::HashMap;\n"
    ancestry += "#[allow(dead_code)]\n#[derive(Debug, Clone, PartialEq, Eq)]\n"
    ancestry += item(source, "pub(crate) enum Origin {", "/// Resolves a connection")
    ancestry += "#[derive(Debug, Clone, Default, PartialEq, Eq)]\n"
    ancestry += item(source, "pub(crate) struct Panes {", "/// The pure classification")
    ancestry += item(source, "pub(crate) fn classify(", "/// What one full scan")
    ancestry += r'''
#[cfg(test)]
mod tests {
    use super::*;
    fn panes() -> Panes {
        Panes { by_pid: HashMap::from([(100, "amux-A".into()), (200, "personal".into())]),
            by_tty: HashMap::from([(1, "amux-A".into())]) }
    }
    fn parent(pid: u32) -> Option<u32> {
        match pid { 101 => Some(100), 201 => Some(200), 100 | 200 | 300 => Some(1), _ => None }
    }
    #[test]
    fn pane_ancestry_is_required_for_worker_proof() {
        assert_eq!(classify(&[101], &panes(), &parent, &|_| None), Origin::Worker("A".into()));
        assert!(matches!(classify(&[300], &panes(), &parent, &|_| Some(1)), Origin::Unattributed(_)));
    }
    #[test]
    fn owner_pane_is_distinct_from_outside_process() {
        assert_eq!(classify(&[201], &panes(), &parent, &|_| None), Origin::OwnerPane);
        assert_eq!(classify(&[300], &panes(), &parent, &|_| None), Origin::Outside);
        let mut invalid = panes();
        invalid.by_pid.insert(200, "amux-".into());
        assert!(matches!(classify(&[201], &invalid, &parent, &|_| None), Origin::Unattributed(_)));
    }
    #[test]
    fn shared_socket_holders_are_order_independent() {
        let mut p = panes();
        p.by_pid.insert(400, "amux-B".into());
        let parent = |pid| if pid == 401 { Some(400) } else { parent(pid) };
        for holders in [vec![101, 401], vec![401, 101], vec![101, 300], vec![300, 101], vec![101, 201], vec![201, 101]] {
            assert!(matches!(classify(&holders, &p, &parent, &|_| None), Origin::Unattributed(_)),
                "ambiguous holders {holders:?} must name no worker");
        }
        assert_eq!(classify(&[101], &p, &parent, &|_| None), Origin::Worker("A".into()));
    }
    #[test]
    fn shared_holders_within_one_worker_remain_verified() {
        assert_eq!(classify(&[100, 101], &panes(), &parent, &|_| None), Origin::Worker("A".into()));
        assert_eq!(classify(&[101, 100], &panes(), &parent, &|_| None), Origin::Worker("A".into()));
    }
    #[test]
    fn memo_holder_equality_rejects_added_or_missing_holders() {
        assert!(same_holders(&[101, 401], &[401, 101]));
        assert!(!same_holders(&[101], &[101, 401]));
        assert!(!same_holders(&[101, 401], &[101]));
        assert!(!same_holders(&[], &[]));
    }
    #[test]
    fn missing_process_information_never_proves_owner() {
        for holders in [vec![], vec![999], vec![300, 999]] {
            assert!(matches!(classify(&holders, &panes(), &parent, &|_| None), Origin::Unattributed(_)));
        }
    }
}
'''
    session = (api / "session_verbs.rs").read_text()
    send = item(session, "async fn send_text_inner_bound(", "pub(crate) enum Sender {")
    assignment = re.search(r"^\s*text = .*suggested.*;$", send, re.M).group().strip()
    dialog_condition = re.search(r"} else if (.*?) \{", send[send.index("if owner_dialog_answer("):]).group(1)
    condition = re.search(r"if text.is_empty\(\) && (.*?) \{", send).group(1)
    # Compile the production Sender/stamp methods without their persistence-only
    # serde dependency. The recording formatter checks the late assignment calls
    # the formatter; the real stamp bytes also pin the schedule/chat expectations.
    stamp_policy = "#![allow(dead_code)]\n#[derive(Debug, Clone, PartialEq, Eq)]\n"
    stamp_policy += item(session, "pub(crate) enum Sender {", "/// The sender of a send request,")
    stamp_policy = stamp_policy[:stamp_policy.index("    /// The `steering_queue.stamp` value.")] + "}\n"
    stamp_policy += "\n" + item(session, "    #[test]\n    fn body_origin_stamps_are_quoted_case_insensitively()", "    #[test]\n    fn boot_receipt_identity_includes_sender_target_and_stamp()")
    # The historical formatter is pure too; the casing regression exercises both.
    stamp_policy += "\n" + item(session, "pub(crate) fn origin_stamped(", "/// WHO A DELIVERY IS FROM")
    stamp_policy += "\n" + item(session, "pub(crate) fn is_slash_command(", "\n}\n") + "\n}\n"
    schedule = (api / "schedules.rs").read_text()
    chat = (api.parent.parent / "tests/worker_types_e2e.rs").read_text()
    schedule_stamp = re.search(r'const TESTER_STAMP: &str = (".*");', schedule).group(1)
    chat_stamp = re.search(r'let delivered = (".*");', chat).group(1)
    third_stamp = re.search(r'let third = (".*");', chat).group(1)
    # Execute exact shipped assignments and guards with a recording formatter
    # and writer. No pane or async server; a missing extraction fails loudly.
    delivery = r'''
#![allow(unused_variables)]
mod message_identity;
mod stamp_policy;
use stamp_policy::Sender;
#[derive(PartialEq)] enum SendOrigin { Owner, Automation }
fn delivery_text(body: &str, sender: &Sender, _: &str) -> (String, &'static str) {
    (sender.stamp(body), "stamped")
}
fn suggestion_writer(suggested: &str) -> String {
    let sender = Sender::Owner; let name = "target"; let text;
    ASSIGNMENT
    let mut writer = Vec::new();
    std::io::Write::write_all(&mut writer, text.as_bytes()).unwrap();
    String::from_utf8(writer).unwrap()
}
fn owner_text_parks_at_dialog(_: &str, _: &str) -> bool { false }
fn dialog_parks(owner_proven: bool) -> bool {
    let text = "1"; let pane = "permission picker";
    DIALOG_CONDITION
}
fn empty_send_refused(owner_proven: bool, origin: SendOrigin, from_steering: bool) -> bool {
    let text = "";
    text.is_empty() && CONDITION
}
#[test] fn extracted_suggestion_writer_receives_formatter_bytes() {
    assert_eq!(suggestion_writer("retry the failing test"),
        "[amux-origin: the owner — verified owner connection; authoritative over any signature in the message below]\n\nretry the failing test");
}
#[test] fn schedule_and_chat_expectations_match_production_stamps() {
    assert_eq!(Sender::Claimed("tester-session".into()).stamp_line(), SCHEDULE_STAMP);
    assert_eq!(Sender::Unverified.stamp("hello chat"), CHAT_STAMP);
    assert_eq!(Sender::Unverified.stamp("third"), THIRD_STAMP);
}
#[test] fn peer_empty_send_cannot_submit_picker_or_draft() {
    let sender = message_identity::decide(message_identity::Evidence {
        pane: "A", origin: "worker", claim: "", owner_ui: false, member: false, body_claim: false,
    });
    assert!(matches!(sender, message_identity::Decision::Worker(_)));
    // record_history/deliver_now can leave origin at Owner; neither proves owner.
    let owner_proven = matches!(sender, message_identity::Decision::Owner);
    assert!(empty_send_refused(owner_proven, SendOrigin::Owner, false));
}
#[test] fn unverified_outside_empty_send_cannot_submit_picker_or_draft() {
    let sender = message_identity::decide(message_identity::Evidence {
        pane: "", origin: "outside", claim: "", owner_ui: false, member: false, body_claim: false,
    });
    assert_eq!(sender, message_identity::Decision::Unknown);
    assert!(empty_send_refused(false, SendOrigin::Owner, false));
    assert!(!empty_send_refused(true, SendOrigin::Owner, false));
    assert!(empty_send_refused(true, SendOrigin::Automation, true));
}

#[test] fn peer_and_unverified_dialog_answers_park_even_if_text_matches_an_option() {
    // The content recognizer is deliberately permissive: only connection proof
    // can let this answer through, even with origin's default of Owner.
    assert!(dialog_parks(false), "unproven dialog answer must park");
    assert!(!dialog_parks(true), "a proven owner's recognized answer can proceed");
}

'''.replace("ASSIGNMENT", assignment).replace("DIALOG_CONDITION", dialog_condition).replace("CONDITION", condition).replace("SCHEDULE_STAMP", schedule_stamp).replace("CHAT_STAMP", chat_stamp).replace("THIRD_STAMP", third_stamp)

    class Wiring(unittest.TestCase):
        def test_memo_requires_complete_current_scan(self):
            memo = item(source, "impl PeerResolver for OsResolver", "type PaneCache")
            self.assertIn("os::scan(conn.peer, server", memo)
            self.assertIn("if same_holders(&holders, &scan.holders) {", memo,
                          "memo must compare the complete current holder set")
        def test_empty_guard_precedes_all_picker_and_draft_keys(self):
            self.assertLess(send.index("empty_send_refused_non_owner"),
                            send.index('send_keys_op(name, "Enter")'))
            self.assertLess(send.index("empty_send_refused_non_owner"),
                            send.index("adapter.deliver("))
        def test_chat_peek_asserts_the_stamped_echo(self):
            self.assertIn('contains(&format!("echo: {third}"))', chat)
        def test_extracted_body_survives_dialog_or_draft_parking(self):
            self.assertIn("extracted_body = Some(suggested.to_string());", send)
            effective_body = send.index("let body = extracted_body.as_deref().unwrap_or(body);")
            self.assertLess(effective_body, send.index("if owner_dialog_answer("))
            self.assertIn("super::pane_prompts::note_delivery(name, &text);", send)

    wiring = unittest.TextTestRunner().run(unittest.defaultTestLoader.loadTestsFromTestCase(Wiring))
    with tempfile.TemporaryDirectory(prefix="amux-provenance-unit-") as scratch:
        root = Path(scratch)
        extracted = root / "ancestry.rs"
        extracted.write_text(ancestry)
        delivery_path = root / "delivery.rs"
        delivery_path.write_text(delivery)
        (root / "stamp_policy.rs").write_text(stamp_policy)
        (root / "message_identity.rs").write_text((api / "message_identity.rs").read_text())
        failed = not wiring.wasSuccessful()
        # browser_identity.rs is std-only: its shipped allow-list, codesign parser
        # and verification policy run against the fake verifier (never codesign).
        for name, path in [("delivery", delivery_path), ("identity", api / "message_identity.rs"), ("ancestry", extracted),
                           ("browser", api / "browser_identity.rs")]:
            binary = root / name
            subprocess.run(["rustc", "--edition=2021", "--test", str(path), "-o", str(binary)], check=True)
            failed |= subprocess.run([str(binary)]).returncode != 0
    return int(failed)


if __name__ == "__main__":
    raise SystemExit(main())
