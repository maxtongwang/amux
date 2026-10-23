#!/usr/bin/env python3
"""Run the real slot throttle with fake guard/budget/cargo; never perform a build."""

from datetime import datetime
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import tempfile
import time
import unittest

SCRIPTS = Path(__file__).resolve().parent


class SafeCargoSlotsTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="safe-cargo-slots-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.home = self.root / "amux"
        self.scripts = self.root / "scripts"
        self.scripts.mkdir()
        self.worktree = self.root / 'worktree with "quotes"'
        self.worktree.mkdir()
        for name in ("safe-cargo.sh", "build-slot-timing.py"):
            shutil.copyfile(SCRIPTS / name, self.scripts / name)
        # Preserve the wrapper's exact argv chain while stubbing the unrelated guards.
        passthrough = ('import os,sys\nargs=sys.argv[sys.argv.index("--")+1:]\n'
                       'os.execvp(args[0],args)\n')
        for name in ("cargo-target-guard.py", "cargo-budget.py"):
            (self.scripts / name).write_text(passthrough)
        self.bin = self.root / "bin"
        self.bin.mkdir()
        (self.bin / "systemd-run").write_text("#!/usr/bin/env python3\n" + passthrough)
        (self.bin / "systemd-run").chmod(0o755)
        cargo = self.bin / "cargo"
        cargo.write_text('''#!/usr/bin/env python3
import os
from pathlib import Path
import time
if os.environ.get("FAKE_CARGO_READY"):
    Path(os.environ["FAKE_CARGO_READY"]).touch()
if os.environ.get("FAKE_CARGO_RELEASE"):
    while not Path(os.environ["FAKE_CARGO_RELEASE"]).exists():
        time.sleep(.02)
raise SystemExit(int(os.environ.get("FAKE_CARGO_RC", "0")))
''')
        cargo.chmod(0o755)
        self.env = dict(os.environ, AMUX_HOME=str(self.home), AMUX_CARGO_MAX_CONCURRENT="1",
                        AMUX_CARGO_THROTTLE_DIR=str(self.home / "cargo-throttle"),
                        PATH=str(self.bin) + os.pathsep + os.environ["PATH"],
                        CARGO_TARGET_DIR=str(self.root / "target"), _TC_RECEIPT="1")
        for name in ("FAKE_CARGO_READY", "FAKE_CARGO_RELEASE", "FAKE_CARGO_RC"):
            self.env.pop(name, None)
        self.log = self.home / "logs/build-slots.jsonl"
        self.children = []
        self.addCleanup(self.stop_children)

    def stop_children(self):
        for proc in self.children:
            if proc.poll() is None:
                # Every process group here was created by this test in its own session.
                os.killpg(proc.pid, signal.SIGKILL)
            proc.communicate(timeout=5)

    def launch(self, *args, **extra):
        proc = subprocess.Popen(["bash", str(self.scripts / "safe-cargo.sh"), *(args or ("check",)),
                                 "--", "synthetic-secret-never-log"], cwd=self.worktree,
                                env=dict(self.env, **extra), start_new_session=True,
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        self.children.append(proc)
        return proc

    def rows(self):
        return [json.loads(line) for line in self.log.read_text().splitlines()] if self.log.exists() else []

    def until(self, predicate):
        deadline = time.monotonic() + 8
        while time.monotonic() < deadline:
            if predicate():
                return
            time.sleep(.02)
        self.fail("fake cargo/timing event did not arrive before deadline")

    def assert_released(self, proc, expected):
        _, error = proc.communicate(timeout=10)
        self.assertEqual(proc.returncode, expected, error)
        rows = [r for r in self.rows() if r["pid"] == proc.pid]
        self.assertEqual([r["event"] for r in rows],
                         ["slot_wait_start", "slot_acquired", "slot_released"])
        self.assertEqual(len({r["run_id"] for r in rows}), 1)
        for row in rows:
            self.assertEqual(row["worktree"], str(self.worktree.resolve()))
            self.assertEqual(row["command"], "cargo check")
            self.assertEqual(row["subcommand"], "check")
            self.assertTrue(row["ts"].endswith("Z"))
        times = [datetime.fromisoformat(r["ts"].replace("Z", "+00:00")) for r in rows]
        self.assertEqual(times, sorted(times))
        self.assertNotIn("synthetic-secret", self.log.read_text())
        self.assertFalse((self.home / "cargo-throttle/slot-1").exists())
        self.assertFalse((self.home / "cargo-throttle/slot-1.pid").exists())

    def test_success_and_failure(self):
        self.assert_released(self.launch(), 0)
        self.assert_released(self.launch("+nightly", "--color", "never", "check"), 0)
        self.assert_released(self.launch(FAKE_CARGO_RC="42"), 42)

    def test_waits_for_slot(self):
        release, ready = self.root / "release", self.root / "ready"
        first = self.launch(FAKE_CARGO_RELEASE=str(release), FAKE_CARGO_READY=str(ready))
        self.until(ready.exists)
        second = self.launch()
        self.until(lambda: any(r["pid"] == second.pid for r in self.rows()))
        self.assertEqual([r["event"] for r in self.rows() if r["pid"] == second.pid], ["slot_wait_start"])
        release.touch()
        first.communicate(timeout=10)
        self.assert_released(second, 0)
        rows = self.rows()
        self.assertEqual(sum(r["pid"] == first.pid and r["event"] == "slot_released" for r in rows), 1)
        second_acquire = next(r["ts"] for r in rows if r["pid"] == second.pid and r["event"] == "slot_acquired")
        # Acquired cannot precede removal of the first slot. Allow logger scheduling
        # to reorder the release line itself; queue wait must still be positive.
        second_wait = next(r["ts"] for r in rows if r["pid"] == second.pid and r["event"] == "slot_wait_start")
        self.assertLess(second_wait, second_acquire)
        self.assertEqual(first.returncode, 0)

    def test_interrupt_releases_once(self):
        for sig in (signal.SIGTERM, signal.SIGINT, signal.SIGHUP):
            ready = self.root / f"ready-{sig}"
            proc = self.launch(FAKE_CARGO_READY=str(ready), FAKE_CARGO_RELEASE=str(self.root / "never"))
            self.until(ready.exists)
            proc.send_signal(sig)  # Only the wrapper; it must forward to its own child.
            self.assert_released(proc, 128 + sig)

    def test_logging_failure_does_not_fail_cargo(self):
        self.home.mkdir()
        (self.home / "logs").write_text("not a directory")
        proc = self.launch(FAKE_CARGO_RC="37")
        _, error = proc.communicate(timeout=10)
        self.assertEqual(proc.returncode, 37, error)
        self.assertEqual(error.count("slot timing append failed"), 3)
        self.assertFalse((self.home / "cargo-throttle/slot-1").exists())


if __name__ == "__main__":
    unittest.main()
