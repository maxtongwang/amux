#!/usr/bin/env python3
"""Verify the shipped pure provenance predicate and CLI transport without Cargo.

HTTP route tests remain in delegation_reply_tests.rs. This script uses no tmux,
network, credentials, live config, or provider process. --source-root selects a
baseline for regression flips; all execution artifacts live in a temp directory.
"""
import argparse
from pathlib import Path
import re
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
SOURCE = ROOT


def function(path, name):
    text = path.read_text()
    matches = list(re.finditer(r"^(?:pub\(crate\) )?(?:async )?fn " + name + r"\([^\n]*", text, re.M))
    if len(matches) != 1:
        raise AssertionError((name, len(matches)))
    start = matches[0].start()
    end = text.index("\n}", start) + 2
    return text[start:end]


def shell_function(name):
    text = (SOURCE / "amux").read_text()
    matches = list(re.finditer(r"^" + name + r"\(\) \{\n.*?^}", text, re.M | re.S))
    if len(matches) != 1:
        raise AssertionError((name, len(matches)))
    return matches[0].group()


class OwnerOrigin(unittest.TestCase):
    def test_pane_requires_recorded_owner_process_origin(self):
        with tempfile.TemporaryDirectory(prefix="owner-origin-") as tmp:
            tmp = Path(tmp)
            predicate = function(SOURCE / "crates/amux-server/src/api/max_ref.rs", "human_source")
            code = '''#[derive(Debug,PartialEq)] enum HumanSource { Pane, OwnerSend }
''' + predicate + '''
#[test] fn pane_requires_server_owner_origin() {
    for origin in ["", "worker", "unattributed"] {
        assert_eq!(human_source("user","pane","pane",origin),None,"{origin}");
    }
    for origin in ["outside","remote","remote-forwarded"] {
        assert_eq!(human_source("user","pane","pane",origin),Some(HumanSource::Pane));
        assert_eq!(human_source("user","","direct",origin),Some(HumanSource::OwnerSend));
        assert_eq!(human_source("session","peer","direct",origin),None);
        assert_eq!(human_source("schedule","schedule:x","direct",origin),None);
    }
}
'''
            src, binary = tmp / "predicate.rs", tmp / "predicate-test"
            src.write_text(code)
            subprocess.run(["rustc", "--edition", "2021", "--test", str(src), "-o", str(binary)], check=True)
            subprocess.run([str(binary)], check=True)

    def test_owner_key_refusal_status_and_code(self):
        path=SOURCE / "crates/amux-server/src/api/session_verbs.rs"
        source=path.read_text()
        status=function(path,"send_failure_status")
        code=function(path,"send_failure_code")
        classifier=function(path,"owner_input_refusal")
        start=source.index("const OWNER_INPUT_REFUSALS:")
        end=source.index("\n];",start)+3
        declarations=source[start:end]
        variants=sorted(set(re.findall(r"StatusCode::([A-Z_]+)",status)))
        stub="#[allow(non_camel_case_types)] #[derive(Debug,PartialEq)] enum StatusCode {"+",".join(variants)+"}\n"
        for name in ["ISOLATED_REFUSAL_PREFIX","HERDR_NOT_ADDRESSABLE","BG_VIEW_REFUSAL_PREFIX"]:
            matches=re.findall(r"^(?:pub\(crate\) )?const "+name+r": &str =.*;$",source,re.M)
            self.assertEqual(len(matches),1,name)
            stub+=matches[0]+"\n"
        test='''
#[test] fn owner_refusals_are_forbidden_and_have_codes() {
    for code in ["owner_session_keys_refused","owner_session_unverified_command_refused"] {
        let message=format!("{code}: reason");
        assert_eq!(send_failure_status(&message).0,StatusCode::FORBIDDEN);
        assert_eq!(send_failure_code(&message),Some(code));
    }
}
'''
        with tempfile.TemporaryDirectory(prefix="owner-refusal-") as tmp:
            src,binary=Path(tmp)/"check.rs",Path(tmp)/"check"
            src.write_text(stub+declarations+"\n"+classifier+"\n"+status+"\n"+code+test)
            subprocess.run(["rustc","--edition","2021","--test",str(src),"-o",str(binary)],check=True)
            subprocess.run([str(binary)],check=True)

    def test_server_ancestry_is_automation_not_outside(self):
        path=SOURCE / "crates/amux-server/src/api/process_identity.rs"
        predicate=function(path,"classify_with_server")
        stub='''
use std::collections::HashMap;
#[derive(Debug,PartialEq)] enum Origin { Worker(String), Unattributed(String), Outside, Automation }
#[derive(Default)] struct Panes { by_pid:HashMap<u32,String>, by_tty:HashMap<u64,String> }
#[derive(Default)] struct InputBoundaries { server_pid:Option<u32>,owner_shells:Vec<u32> }
'''
        test='''
#[test] fn server_children_are_not_current_human_input() {
    let chain=|pid|match pid {400=>Some(300),300=>Some(10),10=>Some(1),444=>Some(1),_=>None};
    let mut panes=Panes::default();
    let boundaries=InputBoundaries {server_pid:Some(300),owner_shells:vec![]};
    assert_eq!(classify_with_server(&[400],&panes,&chain,&|_|None,&boundaries),Origin::Automation);
    assert_eq!(classify_with_server(&[444],&panes,&chain,&|_|None,&boundaries),Origin::Outside);
    assert_eq!(classify_with_server(&[400,444],&panes,&chain,&|_|None,&boundaries),Origin::Automation);
    assert!(matches!(classify_with_server(&[999],&panes,&chain,&|_|None,&boundaries),Origin::Unattributed(_)));
    let owner_pty=InputBoundaries {server_pid:Some(300),owner_shells:vec![400]};
    assert_eq!(classify_with_server(&[400],&panes,&chain,&|_|None,&owner_pty),Origin::Outside);
    panes.by_pid.insert(10,"amux-peer".into());
    assert_eq!(classify_with_server(&[400],&panes,&chain,&|_|None,&boundaries),Origin::Worker("peer".into()));
}
'''
        with tempfile.TemporaryDirectory(prefix="owner-ancestry-") as tmp:
            src,binary=Path(tmp)/"check.rs",Path(tmp)/"check"
            src.write_text(stub+predicate+test)
            subprocess.run(["rustc","--edition","2021","--test",str(src),"-o",str(binary)],check=True)
            subprocess.run([str(binary)],check=True)

    def test_cli_preserves_schedule_marker_without_server_ancestry(self):
        import json,os
        for origin in ["automation",""]:
            with self.subTest(origin=origin), tempfile.TemporaryDirectory(prefix="schedule-curl-") as tmp:
                tmp=Path(tmp); capture=tmp/"capture"; curl=tmp/"curl"
                curl.write_text('#!/usr/bin/env python3\nimport json,os,sys\nopen(os.environ["CAPTURE"],"w").write(json.dumps(sys.argv[1:]))\nprint("{}")\n')
                curl.chmod(0o755)
                env=dict(os.environ,PATH=str(tmp)+os.pathsep+os.environ["PATH"],CAPTURE=str(capture),AMUX_INPUT_SOURCE=origin)
                env.pop("AMUX_WORKER_TOKEN",None)
                script=shell_function("_curl")+'\n_curl -s http://127.0.0.1:9/api/health\n'
                result=subprocess.run(["bash","-c",script],env=env,text=True,capture_output=True)
                self.assertEqual(result.returncode,0,result.stderr)
                args=json.loads(capture.read_text())
                self.assertEqual("X-Amux-Input-Source: automation" in args,origin=="automation",args)

    def cli(self, response, status, keys):
        with tempfile.TemporaryDirectory(prefix="owner-keys-cli-") as tmp:
            tmp = Path(tmp)
            # Only these external boundaries are fixtures; cmd_send and its key
            # helper are extracted from the shipped CLI, never paraphrased.
            script = '''set -u
resolve_session() { printf mrpopo; }
session_backend() { printf tmux; }
need_tmux() { :; }
tmux_name() { printf amux-mrpopo; }
is_running() { return 0; }
tmux() { printf 'RAW TMUX\\n' >> "$CAPTURE"; }
_send_usage() { printf usage; }
die() { echo "$*" >&2; exit 1; }
_curl() { printf '%s\\n' "$*" >> "$CAPTURE"; printf '%s\\n%s' "$RESPONSE" "$STATUS"; }
'''
            script += shell_function("cmd_send") + "\n"
            # The baseline deliberately has no helper: cmd_send bypasses API.
            if "_send_keys_via_api() {" in (SOURCE / "amux").read_text():
                script += shell_function("_send_keys_via_api") + "\n"
            script += 'cmd_send mrpopo --keys "$@"\n'
            capture = tmp / "capture"
            import os
            env = dict(os.environ, CAPTURE=str(capture), RESPONSE=response, STATUS=status,
                       AMUX_WORKER="same-group-peer", AMUX_API="http://127.0.0.1:9", GREEN="", RESET="")
            result = subprocess.run(["bash", "-c", script, "fixture", *keys], env=env, text=True, capture_output=True)
            return result, capture.read_text() if capture.exists() else ""

    def test_cli_keys_refusal_has_no_tmux_fallback(self):
        result, capture = self.cli('{"ok":false,"code":"owner_session_keys_refused"}', "403", ["y", "Enter"])
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertNotIn("RAW TMUX", capture)
        self.assertEqual(capture.count("/api/workers/mrpopo/keys"), 1, capture)
        self.assertIn("owner_session_keys_refused", result.stderr)
        self.assertIn("X-Amux-Worker: same-group-peer", capture)

    def test_cli_owner_keys_keep_order_and_report_uncertainty(self):
        result, capture = self.cli('{"ok":true,"accepted":true,"effect":"unverified"}', "200", ["y", "Enter"])
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn("RAW TMUX", capture)
        self.assertEqual(capture.count("/api/workers/mrpopo/keys"), 2, capture)
        self.assertLess(capture.index('"y"'), capture.index('"Enter"'))
        self.assertIn("unverified", result.stdout)

    def test_cli_keys_unknown_or_malformed_receipt_never_falls_back(self):
        for response, status in [("", "000"), ("not-json", "200"), ('{"ok":true}', "200"), ('{"ok":false,"accepted":false}', "200")]:
            with self.subTest(response=response, status=status):
                result, capture = self.cli(response, status, ["Enter"])
                self.assertNotEqual(result.returncode, 0)
                self.assertNotIn("RAW TMUX", capture)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source-root", type=Path, default=ROOT)
    args, rest = parser.parse_known_args()
    SOURCE = args.source_root
    unittest.main(argv=[__file__, *rest])
