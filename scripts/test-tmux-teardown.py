#!/usr/bin/env python3
"""Prove test tmux teardown through Cargo, or compile just the exact std-only helpers."""

import argparse
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parent.parent
API = ROOT / "crates/amux-server/src/api"
LEAF = API / "session_verbs"
OWNER_RECEIPT = re.compile(r"verdict=test_tmux_owner_registered socket=(amux-test-\d+)")
PROBE_TIMEOUT_SECONDS = 5


def cargo_case(name):
    command = [str(ROOT / "scripts/test-contended.sh"), "-p", "amux-server", "--lib", name, "--", "--nocapture"]
    sockets = set()
    with subprocess.Popen(command, cwd=ROOT, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True) as child:
        for line in child.stdout:
            sys.stdout.write(line)
            sys.stdout.flush()
            sockets.update(OWNER_RECEIPT.findall(line))
        code = child.wait()
    if not sockets:
        raise RuntimeError(f"{name}: no ownership receipt; teardown was not measured (exit {code})")
    env = {key: value for key, value in os.environ.items() if key not in ["TMUX", "TMUX_PANE"]}
    for socket in sorted(sockets):
        out = subprocess.run(["tmux", "-L", socket, "list-sessions"], env=env, capture_output=True, text=True, timeout=PROBE_TIMEOUT_SECONDS)
        if out.returncode == 0 or not any(message in out.stderr for message in ["no server running", "No such file or directory"]):
            raise RuntimeError(f"{name}: private server remains after Cargo exited: {socket}: {out}")
        print(f"verdict=test_tmux_cargo_teardown_pass socket={socket} case={name}")
    if code:
        raise subprocess.CalledProcessError(code, command)


def function(source, name):
    start = source.index("pub(crate) fn " + name + "(")
    body = source.index("{", start)
    depth = 1
    end = body + 1
    while depth:
        depth += (source[end] == "{") - (source[end] == "}")
        end += 1
    return source[start:end]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--leaf", action="store_true", help="check the exact Rust helpers without building the server; excludes the guard-probe")
    parser.add_argument("--source-revision", help="with --leaf, compile the socket/command helpers from this Git revision against the current regression tests")
    args = parser.parse_args()
    if args.source_revision and not args.leaf:
        parser.error("--source-revision requires --leaf")
    if not args.leaf:
        for name in ["test_tmux_teardown_tests", "start_session_refuses_to_spawn_from_a_test_home"]:
            cargo_case(name)
        return

    if args.source_revision:
        revision = subprocess.check_output(["git", "rev-parse", "--verify", "--end-of-options", args.source_revision + "^{commit}"], cwd=ROOT, text=True).strip()
        source = subprocess.check_output(["git", "show", revision + ":crates/amux-server/src/api/session_verbs.rs"], cwd=ROOT, text=True)
    else:
        source = (API / "session_verbs.rs").read_text()
    helpers = "\n".join(function(source, name) for name in ["test_tmux_socket", "tmux_command"])
    owner = LEAF / "test_tmux.rs"
    prefix = f'#[path = "{owner}"] mod test_tmux;\n' if owner.exists() else ""
    harness = prefix + helpers + f'\n#[path = "{LEAF / "test_tmux_teardown_tests.rs"}"] mod test_tmux_teardown_tests;\n'
    with tempfile.TemporaryDirectory(prefix="amux-test-tmux-", dir="/tmp") as directory:
        harness_path = Path(directory) / "harness.rs"
        binary = Path(directory) / "teardown"
        harness_path.write_text(harness)
        subprocess.run(["rustc", "--edition=2021", "--test", str(harness_path), "-o", str(binary)], check=True)
        subprocess.run([str(binary), "--nocapture"], check=True)
    print("PASS: exact test tmux helpers; guard-probe and full crate remain unmeasured")


if __name__ == "__main__":
    main()
