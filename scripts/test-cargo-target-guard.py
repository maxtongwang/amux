#!/usr/bin/env python3
"""ATE-92: real process/lock lifetimes, preserved artifacts, and idle retry."""
import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import queue
import shutil
import signal
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from unittest import mock

SCRIPT = Path(__file__).with_name('cargo-target-guard.py').resolve()
spec = importlib.util.spec_from_file_location('guard', SCRIPT)
guard = importlib.util.module_from_spec(spec)
spec.loader.exec_module(guard)


def quiet(_):
    return [], 1


class CargoReclaimTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.base = Path(self.temp.name).resolve()
        self.root = self.base / 'rust-build-target'
        self.artifacts = self.root / 'debug' / 'deps'
        self.artifacts.mkdir(parents=True)
        self.marker = self.artifacts / 'artifact'
        self.marker.write_text('must survive active builds')

    def mutate(self, action='clear', path=None, destination=None, probe=quiet, protected=()):
        return guard.mutate(action, path or self.root, destination, [self.root], protected,
                            process_probe=probe)

    def test_active_build_lease_blocks_clear_and_rotation_then_idle_retry_reclaims(self):
        # The actual sanctioned wrapper with a deterministic Cargo executable.
        # It execs into the tool so the test pins lease inheritance, not a parent
        # shell that happens to remain alive during a subprocess.
        bin_dir = self.base / 'bin'
        bin_dir.mkdir()
        # Exercise the Linux scope command without requiring a CI user manager.
        scope = bin_dir / 'systemd-run'
        scope.write_text('#!/bin/sh\nwhile [ "$1" != -- ]; do shift; done\nshift\nexec "$@"\n')
        scope.chmod(0o755)
        cargo = bin_dir / 'cargo'
        cargo.write_text('#!/bin/sh\nprintf "ready\\n"\nread -r release\n')
        cargo.chmod(0o755)
        env = {**os.environ, 'PATH': str(bin_dir) + ':' + os.environ['PATH'],
               'CARGO_TARGET_DIR': str(self.root), 'HOME': str(self.base / 'fixture-home')}
        proc = subprocess.Popen(['bash', str(SCRIPT.with_name('safe-cargo.sh')), 'check'],
                                env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                stderr=subprocess.DEVNULL, text=True)
        try:
            self.assertEqual(proc.stdout.readline().strip(), 'ready')
            for action, path, dest in [('clear', self.root, None),
                                       ('move', self.artifacts, self.base / 'staged')]:
                with self.assertRaisesRegex(guard.Deferred, 'lock busy'):
                    self.mutate(action, path, dest)
                self.assertTrue(self.marker.exists())
        finally:
            proc.communicate('finished\n', timeout=10)
        self.assertEqual(proc.returncode, 0)
        self.mutate()
        self.assertFalse(self.marker.exists())
        self.assertTrue((self.root / 'debug' / '.cargo-lock').exists())
        self.assertTrue(guard.lease_path(self.root).exists())

    def test_cargo_clean_cannot_delete_a_leased_test_artifact_and_idle_retry_succeeds(self):
        bin_dir = self.base / 'clean-bin'; bin_dir.mkdir()
        scope = bin_dir / 'systemd-run'
        scope.write_text('#!/bin/sh\nwhile [ "$1" != -- ]; do shift; done\nshift\nexec "$@"\n'); scope.chmod(0o755)
        cargo = bin_dir / 'cargo'
        cargo.write_text('#!/bin/sh\nif [ "$1" = clean ]; then rm "$CLEAN_MARKER"; exit 0; fi\nprintf "ready\\n"\nread -r release\n'); cargo.chmod(0o755)
        env = {**os.environ, 'PATH':str(bin_dir)+':'+os.environ['PATH'], 'CARGO_TARGET_DIR':str(self.root), 'CLEAN_MARKER':str(self.marker), 'AMUX_HOME':str(self.base/'fixture-home'), 'HOME':str(self.base/'fixture-home'), 'AMUX_SESSION':'target-guard-fixture'}
        wrapper = str(SCRIPT.with_name('safe-cargo.sh'))
        holder = subprocess.Popen(['bash',wrapper,'test'], env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True)
        try:
            self.assertEqual(holder.stdout.readline().strip(), 'ready')
            held = subprocess.run(['bash',wrapper,'clean'], env=env, capture_output=True, text=True, timeout=15)
            self.assertEqual(held.returncode,75, held.stdout+held.stderr)
            self.assertIn('lock busy',held.stdout)
            self.assertTrue(self.marker.exists(), 'clean must not remove another consumer executable')
        finally:
            holder.communicate('finished\n',timeout=10)
        self.assertEqual(holder.returncode,0)
        idle = subprocess.run(['bash',wrapper,'clean'], env=env, capture_output=True, text=True, timeout=15)
        self.assertEqual(idle.returncode,0,idle.stdout+idle.stderr)
        self.assertFalse(self.marker.exists(), 'the same cleanup works after lease release')

    def test_native_cargo_lock_blocks_unwrapped_build(self):
        lock = self.root / 'debug' / '.cargo-lock'
        fd = guard.open_lock(lock)
        try:
            with self.assertRaisesRegex(guard.Deferred, 'lock busy'):
                self.mutate()
            self.assertTrue(self.marker.exists())
        finally:
            os.close(fd)
        before = lock.stat().st_ino
        self.mutate()
        self.assertEqual(lock.stat().st_ino, before)
        self.assertFalse(self.marker.exists())

    def measured_active(self, targets):
        """`active_processes` over the AMBIENT table, or a skip naming why not.

        These two cells spawn a real process and assert about the real probe,
        which is the point of them. But the probe walks EVERY pid, and on a
        hardened runner one uninspectable `/proc/<pid>/exe` makes it fail
        closed for the whole table before it can answer about our own pid.
        That is deliberate (process_executable's docstring says so) and it is
        not something these cells can assert through.

        Measured 2026-09-08: both errored on GitHub with
        `PermissionError: [Errno 13] Permission denied: '/proc/1226/exe'`,
        reddening `checks` on main and on every PR that merged main. They pass
        on this project's dev box only because it is macOS, where
        process_executable's linux branch never runs at all, so the ambient
        table is never walked. Green on the author's machine by construction.

        A test cannot assert from a probe that could not run (ethos rule 4), so
        an unmeasured table is a SKIP that names the pid, not a pass and not a
        failure. The hardened path itself stays covered deterministically by
        test_hardened_linux_proc_uses_known_command_identity and
        test_hardened_linux_proc_still_fails_closed_for_unknown_binary, which
        inject readlink/which instead of reading the host.
        """
        try:
            return guard.active_processes(targets)
        except guard.Deferred as unmeasured:
            self.skipTest('ambient process table unmeasurable on this host: %s' % unmeasured)

    def test_direct_test_binary_without_cargo_parent_prevents_deletion(self):
        binary = self.artifacts / 'orphan-test-0123456789abcdef'
        source = self.base / 'test.c'
        source.write_text('#include <stdio.h>\nint main(void) { getchar(); return 0; }\n')
        subprocess.run(['cc', str(source), '-o', str(binary)], check=True, capture_output=True)
        proc = subprocess.Popen([str(binary)], stdin=subprocess.PIPE)
        try:
            active, count = self.measured_active([self.root])
            self.assertIn(str(proc.pid), active)
            self.assertGreater(count, 0)
            with self.assertRaises(guard.Deferred):
                self.mutate(probe=guard.active_processes)
            self.assertTrue(binary.exists())
        finally:
            proc.communicate(b'\n', timeout=10)
        self.mutate()
        self.assertFalse(binary.exists())

    def test_unmeasured_probe_fails_closed_and_idle_move_restore_purge_work(self):
        def failed(_):
            raise guard.Deferred('process probe unmeasured')
        with self.assertRaisesRegex(guard.Deferred, 'unmeasured'):
            self.mutate(probe=failed)
        staged = self.base / 'quarantine' / 'deps'
        self.mutate('move', self.artifacts, staged)
        self.assertTrue((staged / 'artifact').exists())
        self.mutate('move', staged, self.artifacts)
        self.assertTrue(self.marker.exists())
        self.mutate('move', self.artifacts, staged)
        with self.assertRaisesRegex(guard.Deferred, 'unmeasured'):
            self.mutate('purge', staged, protected=[self.artifacts], probe=failed)
        self.assertTrue(staged.exists())
        self.mutate('purge', staged, protected=[self.artifacts])
        self.assertFalse(staged.exists())

    def test_cargo_lock_directories_never_rotate(self):
        for path in (self.root, self.root / 'debug'):
            with self.assertRaisesRegex(guard.Deferred, 'cannot rotate'):
                self.mutate('move', path, self.base / 'staged')
        self.assertTrue(self.marker.exists())

    def test_fixture_probe_seam_cannot_authorize_production_mutation(self):
        env = {**os.environ, 'AMUX_CARGO_GUARD_TEST_FIXTURE': str(self.base),
               'AMUX_RS_DISK_CLEAR_ONLY': '1'}
        result = subprocess.run([sys.executable, str(SCRIPT), 'clear', '--target',
                                 str(self.root), '--path', str(self.root)],
                                env=env, capture_output=True, text=True)
        self.assertEqual(result.returncode, 75)
        self.assertIn('invalid temporary fixture', result.stdout)
        self.assertTrue(self.marker.exists())

    def test_valid_fixture_cannot_move_or_restore_across_its_boundary(self):
        with tempfile.TemporaryDirectory(prefix='amux-cargo-guard-test.') as fixture_dir:
            fixture = Path(fixture_dir).resolve()
            target = fixture / 'target'
            artifacts = target / 'debug' / 'deps'
            artifacts.mkdir(parents=True)
            marker = artifacts / 'artifact'
            marker.write_text('fixture must stay inside its boundary')
            env = {**os.environ, 'AMUX_CARGO_GUARD_TEST_FIXTURE': str(fixture),
                   'AMUX_RS_DISK_CLEAR_ONLY': '1'}
            outside = self.base / 'outside'
            for action, extra in [
                    ('move', ['--destination', str(outside)]),
                    ('purge', ['--protected-path', str(outside)])]:
                with self.subTest(action=action):
                    result = subprocess.run([sys.executable, str(SCRIPT), action,
                                             '--target', str(target), '--path', str(artifacts),
                                             *extra], env=env, capture_output=True, text=True)
                    self.assertEqual(result.returncode, 75, result.stdout)
                    self.assertIn('invalid temporary fixture', result.stdout)
                    self.assertTrue(marker.exists())
                    self.assertFalse(outside.exists())

    def test_command_line_mentions_do_not_become_builds(self):
        proc = subprocess.Popen(['sh', '-c', 'read line # cargo rustc build'], stdin=subprocess.PIPE)
        try:
            active, _ = self.measured_active([self.root])
            self.assertNotIn(str(proc.pid), active)
        finally:
            proc.communicate(b'\n', timeout=10)

    def test_hardened_linux_proc_uses_known_command_identity(self):
        proc = self.base / 'proc' / '1177'
        proc.mkdir(parents=True)

        def denied(_):
            raise PermissionError('hardened procfs')

        # A real file of our own, not the host's /usr/bin/sleep: process_executable
        # resolves symlinks, and runner images now link sleep into Rust coreutils.
        known = self.base / 'bin' / 'sleep'
        known.parent.mkdir()
        known.write_text('')
        resolved = guard.process_executable(
            '1177', 'sleep', linux=True, proc_root=self.base / 'proc',
            readlink=denied, which=lambda name: str(known) if name == 'sleep' else None)
        self.assertEqual(resolved, known.resolve())

    def test_hardened_linux_proc_still_fails_closed_for_unknown_binary(self):
        proc = self.base / 'proc' / '1178'
        proc.mkdir(parents=True)

        def denied(_):
            raise PermissionError('hardened procfs')

        with self.assertRaisesRegex(guard.Deferred, 'process executable unmeasured'):
            guard.process_executable(
                '1178', 'orphan-test-012', linux=True, proc_root=self.base / 'proc',
                readlink=denied, which=lambda _: None)

    def test_etime_parses_every_shape_ps_emits_and_refuses_the_rest(self):
        """AMUX-4944. The age rule is only as good as the parse under it.

        `[[DD-]HH:]MM:SS` is the one format macOS and procps agree on; `etimes`
        is Linux-only and macOS `ps` rejects the keyword outright, which is why
        this parses rather than reads seconds.
        """
        self.assertEqual(guard.parse_etime('00:07'), 7)
        self.assertEqual(guard.parse_etime('30:23'), 30 * 60 + 23)
        self.assertEqual(guard.parse_etime('04:47:38'), 4 * 3600 + 47 * 60 + 38)
        self.assertEqual(guard.parse_etime('3-04:47:38'), 3 * 86400 + 4 * 3600 + 47 * 60 + 38)
        self.assertEqual(guard.parse_etime('  30:23  '), 30 * 60 + 23)
        # Unparseable must be None, NOT zero: the probe fails CLOSED on None and
        # would treat a zero as a brand-new build. Opposite outcomes.
        for junk in ('', '-', 'nope', '1:2:3:4', 'aa:bb', '-04:47:38', '30'):
            self.assertIsNone(guard.parse_etime(junk), junk)

    def test_process_age_tells_a_daemon_from_an_in_flight_build(self):
        """AMUX-4944. The same real process, two bounds, opposite verdicts.

        pid 13664 was an orphaned `debug/amux-server` from a PAUSED lane. It
        deferred the builder's cleanup on every tick (243 recorded deferrals),
        debug/ grew 65.5 -> 72.0 GB, the budget then refused every build, and
        no commit deployed fleet-wide for days. The predicate could not express
        the difference between a build using the directory and a daemon that
        happens to live in it.
        """
        binary = self.artifacts / 'daemon-0123456789abcdef'
        source = self.base / 'daemon.c'
        source.write_text('#include <stdio.h>\nint main(void) { getchar(); return 0; }\n')
        subprocess.run(['cc', str(source), '-o', str(binary)], check=True, capture_output=True)
        proc = subprocess.Popen([str(binary)], stdin=subprocess.PIPE)
        try:
            # CONTROL, and the behaviour ATE-92 exists to protect: a brand-new
            # binary under the target IS an in-flight build and still defers.
            active, _ = self.measured_active([self.root])
            self.assertIn(str(proc.pid), active)

            original = guard.STALE_BUILD_AGE_S
            guard.STALE_BUILD_AGE_S = 0
            self.addCleanup(setattr, guard, 'STALE_BUILD_AGE_S', original)
            aged, _ = self.measured_active([self.root])
            self.assertNotIn(str(proc.pid), aged,
                             'past the build-age bound this is a daemon, not an in-flight build')
            # ...and the reclaim it had been deadlocking now gets through.
            active, _ = self.measured_active([self.root])
            if active:
                with self.assertRaisesRegex(guard.Deferred, 'active cargo/rustc/test'):
                    self.mutate(probe=guard.active_processes)
                self.assertTrue(self.marker.exists(), 'an ambient compiler still fences reclamation')
            else:
                self.mutate(probe=guard.active_processes)
                self.assertFalse(self.marker.exists())
        finally:
            proc.communicate(b'\n', timeout=10)

    def test_a_compiler_is_exempt_by_name_however_old_it_is(self):
        """A long compile is still a compile, so age must not reach it.

        Without this, the age bound would silently become a licence to delete
        the target dir out from under a genuinely slow build, which is the
        failure ATE-92 made this guard refuse in the first place.
        """
        binary = self.artifacts / 'cargo'
        source = self.base / 'slow.c'
        source.write_text('#include <stdio.h>\nint main(void) { getchar(); return 0; }\n')
        subprocess.run(['cc', str(source), '-o', str(binary)], check=True, capture_output=True)
        proc = subprocess.Popen([str(binary)], stdin=subprocess.PIPE)
        try:
            original = guard.STALE_BUILD_AGE_S
            guard.STALE_BUILD_AGE_S = 0
            self.addCleanup(setattr, guard, 'STALE_BUILD_AGE_S', original)
            active, _ = self.measured_active([self.root])
            self.assertIn(str(proc.pid), active,
                          'a compiler is active by NAME; the age bound must not reach it')
        finally:
            proc.communicate(b'\n', timeout=10)

    def test_consecutive_deferrals_on_one_condition_count_up_and_reset_on_a_new_one(self):
        """AMUX-4944. 243 identical refusals logged exactly like one.

        The deferral carries measured:true and a real pid, so it reads as the
        guard working. Without a count, a wedge and a pause are the same line.
        """
        first = guard.bump_streak([self.root], 'active cargo/rustc/test process(es): 13664')
        self.assertEqual(first['deferral_streak'], 1)
        self.assertFalse(first['wedged'])

        for expected in range(2, guard.WEDGE_STREAK + 1):
            latest = guard.bump_streak([self.root], 'active cargo/rustc/test process(es): 13664')
            self.assertEqual(latest['deferral_streak'], expected)
        self.assertTrue(latest['wedged'], 'a run of identical refusals must declare itself a wedge')
        self.assertEqual(latest['streak_since'], first['streak_since'], 'the streak keeps its start')

        # A DIFFERENT blocker is a different condition, not a continuing wedge.
        moved = guard.bump_streak([self.root], 'active cargo/rustc/test process(es): 99999')
        self.assertEqual(moved['deferral_streak'], 1)
        self.assertFalse(moved['wedged'])

        # A reclaim that got through ends it.
        guard.clear_streak([self.root])
        self.assertEqual(guard.bump_streak([self.root], 'anything')['deferral_streak'], 1)


MB = 1024 * 1024
DAY = 86400.0
NOW = 1_800_000_000.0  # the fake clock


def put(path, size, used_at):
    """A file of `size` real bytes whose access and modification time is used_at."""
    path.parent.mkdir(parents=True, exist_ok=True)
    with open(path, 'wb') as handle:
        handle.write(os.urandom(size))
    os.utime(path, (used_at, used_at))
    return path


class ReclaimAndSweepTests(unittest.TestCase):
    """DMIW-7: the budget refused an over-cap target and nothing ever shrank it.
    Fake target, fake clock; the live target is never touched."""

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve() / 'rust-build-target'
        debug = self.root / 'debug'
        # incremental: 2 MB; three dependency units of 1 MB each, used 5, 3 and 1 days ago,
        # each with a .fingerprint entry and (the oldest) a build/ dir.
        self.incremental = put(debug / 'incremental' / 'amux_server-abc' / 's-1' / 'dep-graph.bin', 2 * MB, NOW - 1 * DAY)
        self.old = [put(debug / 'deps' / 'libold-0123456789abcdef.rlib', MB, NOW - 5 * DAY),
                    put(debug / '.fingerprint' / 'old-0123456789abcdef' / 'lib-old', 10, NOW - 5 * DAY),
                    put(debug / 'build' / 'old-0123456789abcdef' / 'out' / 'gen.rs', 10, NOW - 5 * DAY)]
        self.mid = [put(debug / 'deps' / 'libmid-1111111111111111.rlib', MB, NOW - 3 * DAY),
                    put(debug / 'deps' / 'mid-1111111111111111.d', 10, NOW - 3 * DAY)]
        self.new = [put(debug / 'deps' / 'new-2222222222222222', MB, NOW - 1 * DAY)]
        # a build script's output: no deps/ files, looks idle (plain reads keep no atime)
        self.script_out = put(debug / 'build' / 'gen-3333333333333333' / 'out' / 'bindings.rs', 10, NOW - 9 * DAY)
        # protected profiles, older than everything and larger than everything
        self.protected = [put(self.root / 'deploy' / 'amux-server', 3 * MB, NOW - 30 * DAY),
                          put(self.root / 'release' / 'deps' / 'libold-0123456789abcdef.rlib', 3 * MB, NOW - 30 * DAY)]

    def assert_protected(self):
        for path in self.protected:
            self.assertTrue(path.exists(), path)

    def test_unit_key_pairs_a_units_outputs(self):
        self.assertEqual(guard.unit_key('libold-0123456789abcdef.rlib'), 'old-0123456789abcdef')
        self.assertEqual(guard.unit_key('old-0123456789abcdef.d'), 'old-0123456789abcdef')
        self.assertEqual(guard.unit_key('liblibc-0123456789abcdef.rmeta'), 'libc-0123456789abcdef')
        self.assertEqual(guard.unit_key('libc-0123456789abcdef.d'), 'libc-0123456789abcdef')
        self.assertEqual(guard.unit_key('adler2-0123456789abcdef.adler2.5f4ad9222612777e-cgu.0.rcgu.o'),
                         'adler2-0123456789abcdef')

    def test_under_the_cap_nothing_is_touched(self):
        result = guard.reclaim([self.root], 100 * MB, 1 * MB, process_probe=quiet)
        self.assertEqual(result['verdict'], 'cargo_reclaim_not_needed')
        self.assertTrue(self.incremental.exists() and all(p.exists() for p in self.old))

    def test_incremental_goes_first_and_may_be_enough(self):
        # ~5 MB reclaimable; cap 4 MB, goal 3.5 MB: clearing incremental (2 MB) suffices.
        result = guard.reclaim([self.root], 4 * MB, int(3.5 * MB), process_probe=quiet)
        self.assertEqual(result['verdict'], 'cargo_reclaimed')
        self.assertFalse(self.incremental.exists())
        self.assertEqual(result['units_removed'], 0)
        self.assertTrue(all(p.exists() for p in self.old + self.mid + self.new))
        self.assertTrue(result['reached_goal'])
        self.assert_protected()

    def test_then_least_recently_used_units_whole_until_the_goal(self):
        # goal 1.5 MB: incremental, then the build-script output (9 days), old (5 days),
        # mid (3 days); new (1 day) stays. Reclaim, unlike sweep, may take script output.
        result = guard.reclaim([self.root], 4 * MB, int(1.5 * MB), process_probe=quiet)
        self.assertEqual(result['verdict'], 'cargo_reclaimed')
        self.assertEqual(result['units_removed'], 3)
        self.assertFalse(self.script_out.exists())
        self.assertTrue(all(not p.exists() for p in self.old + self.mid), 'every output of a unit goes together')
        self.assertTrue(all(p.exists() for p in self.new))
        self.assertTrue(result['reached_goal'])
        self.assertLess(result['after_bytes'], result['before_bytes'])
        self.assert_protected()

    def test_an_active_build_defers_and_removes_nothing(self):
        with self.assertRaisesRegex(guard.Deferred, 'active cargo'):
            guard.reclaim([self.root], 0, 0, process_probe=lambda _: (['4242'], 9))
        with self.assertRaisesRegex(guard.Deferred, 'active cargo'):
            guard.sweep([self.root], now=NOW, process_probe=lambda _: (['4242'], 9))
        self.assertTrue(self.incremental.exists() and all(p.exists() for p in self.old))

    def test_a_held_lease_defers(self):
        fd = guard.open_lock(guard.lease_path(self.root), shared=True)
        try:
            with self.assertRaisesRegex(guard.Deferred, 'lock busy'):
                guard.reclaim([self.root], 0, 0, process_probe=quiet)
        finally:
            os.close(fd)
        self.assertTrue(all(p.exists() for p in self.old))

    def test_sweep_removes_only_what_is_unused_for_two_days(self):
        result = guard.sweep([self.root], 2 * DAY, now=NOW, process_probe=quiet)
        self.assertEqual(result['verdict'], 'cargo_swept')
        self.assertTrue(all(not p.exists() for p in self.old + self.mid))
        self.assertTrue(all(p.exists() for p in self.new))
        self.assertTrue(self.incremental.exists(), 'a session used yesterday stays')
        self.assertTrue(self.script_out.exists(), 'build-script output is left to the size-driven reclaim')
        self.assertEqual(result['units_removed'], 2)
        self.assert_protected()
        # a day later the rest is idle too
        guard.sweep([self.root], 2 * DAY, now=NOW + 1.5 * DAY, process_probe=quiet)
        self.assertFalse(self.incremental.exists() or any(p.exists() for p in self.new))
        self.assertTrue(self.script_out.exists())
        self.assert_protected()

    def test_a_read_counts_as_use(self):
        # rebuilt 5 days ago, read yesterday: atime keeps it
        os.utime(self.old[0], (NOW - 1 * DAY, NOW - 5 * DAY))
        guard.sweep([self.root], 2 * DAY, now=NOW, process_probe=quiet)
        self.assertTrue(self.old[0].exists())


class SafeCargoReclaimTests(unittest.TestCase):
    """The shipped wrapper: a budget refusal on size reclaims and retries once."""

    def setUp(self):
        self.base = Path(tempfile.mkdtemp(prefix='amux-cargo-guard-test.')).resolve()
        self.addCleanup(shutil.rmtree, self.base)
        self.root = self.base / 'rust-build-target'
        for i in range(3):  # 3 MB of old dependency units over a 2 MB cap
            put(self.root / 'debug' / 'deps' / ('libdep%d-%016x.rlib' % (i, i)), MB, NOW - (5 - i) * DAY)
        self.keep = put(self.root / 'release' / 'amux-server', MB, NOW - 30 * DAY)
        bin_dir = self.base / 'bin'
        bin_dir.mkdir()
        (bin_dir / 'systemd-run').write_text('#!/bin/sh\nwhile [ "$1" != -- ]; do shift; done\nshift\nexec "$@"\n')
        self.runs = self.base / 'cargo-runs'
        (bin_dir / 'cargo').write_text('#!/bin/sh\necho run >> "%s"\n' % self.runs)
        for name in ('systemd-run', 'cargo'):
            (bin_dir / name).chmod(0o755)
        self.env = {**os.environ, 'PATH': str(bin_dir) + ':' + os.environ['PATH'],
                    'CARGO_TARGET_DIR': str(self.root), 'HOME': str(self.base / 'home'),
                    'AMUX_CARGO_MAX_TARGET_MB': '2', 'AMUX_CARGO_RECLAIM_TO_GB': str(1.5 / 1024),
                    'AMUX_CARGO_MIN_FREE_GB': '1',
                    # the guard's temporary-fixture probe seam: host builds cannot defer this
                    'AMUX_CARGO_GUARD_TEST_FIXTURE': str(self.base), 'AMUX_RS_DISK_CLEAR_ONLY': '1'}

    def run_wrapper(self):
        return subprocess.run(['bash', str(SCRIPT.with_name('safe-cargo.sh')), 'check'], env=self.env,
                              capture_output=True, text=True, timeout=120)

    def test_refusal_on_size_reclaims_then_the_build_runs(self):
        done = self.run_wrapper()
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertIn('cargo_budget_refused', done.stderr)
        self.assertIn('cargo_reclaimed', done.stderr)
        self.assertEqual(self.runs.read_text().count('run'), 1, 'refused once, then ran once')
        self.assertTrue(self.keep.exists(), 'release/ is protected')
        self.assertEqual(len(list((self.root / 'debug' / 'deps').iterdir())), 1, 'the newest unit stays')

    def test_a_held_lease_defers_the_reclaim_and_the_refusal_stands(self):
        fd = guard.open_lock(guard.lease_path(self.root), shared=True)
        try:
            # the wrapper's own run takes a shared lease too, so it is not blocked; the
            # exclusive reclaim is, and the build must not run on an over-cap target
            done = self.run_wrapper()
        finally:
            os.close(fd)
        self.assertEqual(done.returncode, 75, done.stderr)
        self.assertIn('refusal stands', done.stderr)
        self.assertFalse(self.runs.exists())
        self.assertEqual(len(list((self.root / 'debug' / 'deps').iterdir())), 3)

    def test_a_pending_reclaim_holds_the_wrapper_build_until_it_clears(self):
        """safe-cargo's build waits behind a pending reclaim, then runs."""
        pending = guard.pending_path(self.root)
        reclaimer = subprocess.Popen(
            [sys.executable, '-c',
             'import fcntl,sys; f=open(sys.argv[1], "w"); '
             'fcntl.flock(f, fcntl.LOCK_EX); f.write(sys.argv[2]); f.flush(); '
             'print("ready", flush=True); sys.stdin.readline(); f.close()',
             str(pending), json.dumps({'pid': os.getpid(), 'since': time.time(), 'deferral_streak': 3})],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
        self.assertEqual(reclaimer.stdout.readline().strip(), 'ready')
        env = {**self.env, 'AMUX_CARGO_MAX_TARGET_MB': '1024', 'AMUX_CARGO_RECLAIM_POLL_S': '0.05',
               'AMUX_CARGO_RECLAIM_BUILD_WAIT_S': '60'}
        wrapper = subprocess.Popen(['bash', str(SCRIPT.with_name('safe-cargo.sh')), 'check'], env=env,
                                   stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        err = Lines(wrapper.stderr)
        try:
            err.wait_for('cargo_reclaim_pending:')
            self.assertFalse(self.runs.exists(), 'the build waits while the reclaim is pending')
        finally:
            reclaimer.communicate('done\n', timeout=10)  # the reclaimer ends; its marker is now inert
        wrapper.wait(timeout=60)
        err.close()
        wrapper.stdout.close()
        self.assertEqual(wrapper.returncode, 0, err.rest())
        self.assertIn('cargo_reclaim_pending_ignored', err.rest())
        self.assertEqual(self.runs.read_text().count('run'), 1)


class Lines:
    """Every line a fixture process writes, read on a thread, so a test can wait
    for one with a bound instead of hanging on a pipe that never speaks."""

    def __init__(self, stream):
        self.lines = queue.Queue()
        self.seen = []
        self.stream = stream
        self.thread = threading.Thread(target=self._pump, args=(stream,), daemon=True)
        self.thread.start()

    def close(self):
        self.thread.join(timeout=5)
        if self.thread.is_alive():
            raise AssertionError('fixture output reader did not finish')
        self.stream.close()

    def _pump(self, stream):
        for line in stream:
            self.lines.put(line.rstrip('\n'))

    def wait_for(self, needle, timeout=15):
        deadline = time.monotonic() + timeout
        while True:
            left = deadline - time.monotonic()
            if left <= 0:
                raise AssertionError('no line containing %r within %ss; saw %r' % (needle, timeout, self.seen))
            try:
                line = self.lines.get(timeout=left)
            except queue.Empty:
                continue
            self.seen.append(line)
            if needle in line:
                return line

    def rest(self):
        while True:
            try:
                self.seen.append(self.lines.get_nowait())
            except queue.Empty:
                return '\n'.join(self.seen)


HOLDER = 'import sys; print("ready", flush=True); sys.stdin.readline()'


class FixtureBuild:
    """A build through the guard's real `run`: it takes (or waits for) the shared
    lease, then execs into a holder that keeps the inherited lease until told."""

    def __init__(self, root, env, extra_targets=()):
        targets = [arg for t in (root, *extra_targets) for arg in ('--target', str(t))]
        self.proc = subprocess.Popen(
            [sys.executable, str(SCRIPT), 'run', *targets, '--', sys.executable, '-c', HOLDER],
            env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        self.out = Lines(self.proc.stdout)
        self.err = Lines(self.proc.stderr)

    def ready(self, timeout=15):
        self.out.wait_for('ready', timeout)

    def finish(self):
        """End the build normally: it was never interrupted, so it exits 0."""
        self.proc.stdin.write('done\n')
        self.proc.stdin.close()
        self.proc.wait(timeout=15)
        return self.proc.returncode

    def close(self):
        if self.proc.poll() is None:
            self.finish()
        self.proc.stdin.close()
        self.out.close()
        self.err.close()


class FakeClock:
    """Stands in for the guard's `time` module: time() reads it, sleep() advances
    it and runs the scenario's next step, so a wait of minutes costs nothing."""

    def __init__(self, start, on_sleep=None):
        self.now = float(int(start))  # whole seconds: waited_s and the sleeps compare exactly
        self.on_sleep = on_sleep
        self.sleeps = []

    def time(self):
        return self.now

    def sleep(self, seconds):
        self.sleeps.append(seconds)
        self.now += seconds
        if self.on_sleep:
            self.on_sleep(self)


class ReclaimPriorityTests(unittest.TestCase):
    """DMIW-7 follow-up to PR #11: back-to-back builds starved the exclusive reclaim.

    flock has no writer preference, so new shared leases kept arriving while the
    reclaim's LOCK_EX|LOCK_NB kept failing: deferral_streak 15, the debug target
    40.1 GiB over its 40 budget, and every new build refused with 734 GB free.
    Fake target, fake leases held by fixture processes, fake clock.
    """

    def setUp(self):
        # The guard's temporary-fixture probe seam needs this prefix under tmp, so
        # the host's real cargo/rustc processes cannot defer a fixture reclaim.
        self.base = Path(tempfile.mkdtemp(prefix='amux-cargo-guard-test.')).resolve()
        self.addCleanup(shutil.rmtree, self.base)
        self.root = self.base / 'rust-build-target'
        self.units = [put(self.root / 'debug' / 'deps' / ('libdep%d-%016x.rlib' % (i, i)), MB, NOW - (5 - i) * DAY)
                      for i in range(3)]
        self.keep = put(self.root / 'release' / 'amux-server', MB, NOW - 30 * DAY)
        self.env = {**os.environ,
                    'AMUX_CARGO_GUARD_TEST_FIXTURE': str(self.base), 'AMUX_RS_DISK_CLEAR_ONLY': '1',
                    'AMUX_CARGO_RECLAIM_PRIORITY_AFTER': '3',
                    'AMUX_CARGO_RECLAIM_PRIORITY_WAIT_S': '600',
                    'AMUX_CARGO_RECLAIM_PENDING_MAX_AGE_S': '1200',
                    'AMUX_CARGO_RECLAIM_BUILD_WAIT_S': '60',
                    'AMUX_CARGO_RECLAIM_POLL_S': '0.05'}
        self.env.pop('AMUX_CARGO_GUARD_LEASED', None)
        self.builds = []
        self.addCleanup(lambda: [b.close() for b in self.builds])

    def build(self, **env):
        build = FixtureBuild(self.root, {**self.env, **env})
        self.builds.append(build)
        return build

    def reclaim(self, clock, **env):
        """One `cargo-target-guard.py reclaim` through the shipped main(), on the fake clock."""
        out, err = io.StringIO(), io.StringIO()
        argv = [str(SCRIPT), 'reclaim', '--target', str(self.root),
                '--max-gb', str(2 / 1024), '--to-gb', str(1.5 / 1024)]
        with mock.patch.dict(os.environ, {**self.env, **env}, clear=True), \
                mock.patch.object(guard, 'time', clock), mock.patch.object(sys, 'argv', argv), \
                contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            code = guard.main()
        return code, json.loads(out.getvalue()), err.getvalue()

    def pending(self):
        return self.root.parent / ('.' + self.root.name + '.reclaim.pending')

    def assert_untouched(self):
        self.assertTrue(all(u.exists() for u in self.units), 'a deferred reclaim removes nothing')
        self.assertTrue(self.keep.exists())

    def test_back_to_back_builds_no_longer_starve_the_reclaim(self):
        """THE BUG. Every attempt finds a lease held, because the next build always
        takes its lease before the previous one releases. Before the fix this
        defers on every attempt; after it, the Nth deferral marks a reclaim
        pending, a new build waits behind it, the running build drains, the
        reclaim runs, and the waiting build then proceeds."""
        current = self.build()
        current.ready()
        late = []

        def drain(clock):
            if late:
                return
            # A NEW build arrives while the reclaim is pending: it must wait, not lease.
            arrival = self.build()
            late.append(arrival)
            arrival.err.wait_for('cargo_reclaim_pending:')
            self.assertTrue(self.pending().exists())
            # The build that already held a lease runs to its normal end.
            self.assertEqual(current.finish(), 0, 'a leased build is never interrupted')

        clock = FakeClock(time.time(), drain)
        verdicts = []
        for _ in range(8):
            code, verdict, _ = self.reclaim(clock)
            verdicts.append(verdict['verdict'])
            if code == 0:
                break
            self.assert_untouched()
            # No gap: the next build leases before the previous one releases.
            nxt = self.build()
            nxt.ready()
            self.assertEqual(current.finish(), 0)
            current = nxt
        self.assertEqual(verdict['verdict'], 'cargo_reclaimed',
                         'writer starvation: deferred on %d of %d attempts %r'
                         % (verdicts.count('cargo_reclaim_deferred'), len(verdicts), verdicts))
        self.assertEqual(len(verdicts), 3, 'the reclaim that reached the streak got through')
        self.assertIs(verdict['priority'], True)
        self.assertGreater(verdict['waited_s'], 0)
        self.assertTrue(verdict['reached_goal'])
        self.assertTrue(self.keep.exists(), 'release/ is protected')
        self.assertFalse(self.pending().exists(), 'the marker is cleared after the reclaim')
        # Only now does the build that arrived during the pending reclaim start.
        late[0].ready()
        late[0].err.wait_for('cargo_reclaim_pending_cleared')
        self.assertEqual(late[0].finish(), 0)

    def test_below_the_streak_a_busy_lease_still_defers_at_once(self):
        holder = self.build()
        holder.ready()
        clock = FakeClock(time.time())
        for streak in (1, 2):
            code, verdict, _ = self.reclaim(clock)
            self.assertEqual(code, 75)
            self.assertEqual(verdict['deferral_streak'], streak)
            self.assertIn('lock busy', verdict['reason'])
            self.assertIs(verdict.get('priority', False), False)
            self.assertFalse(self.pending().exists())
        self.assertEqual(clock.sleeps, [], 'below the streak nothing waits')
        self.assert_untouched()

    def test_the_streak_threshold_and_the_reclaimer_wait_are_configurable(self):
        holder = self.build()
        holder.ready()
        clock = FakeClock(time.time())
        env = {'AMUX_CARGO_RECLAIM_PRIORITY_AFTER': '5', 'AMUX_CARGO_RECLAIM_PRIORITY_WAIT_S': '7',
               'AMUX_CARGO_RECLAIM_POLL_S': '2'}
        for _ in range(4):
            code, verdict, _ = self.reclaim(clock, **env)
            self.assertEqual((code, clock.sleeps), (75, []))
            self.assertFalse(self.pending().exists())
        code, verdict, err = self.reclaim(clock, **env)
        self.assertEqual(code, 75)
        self.assertEqual(verdict['deferral_streak'], 5)
        self.assertIs(verdict['priority'], True)
        self.assertEqual(verdict['waited_s'], 7)
        self.assertEqual(clock.sleeps, [2, 2, 2, 1], 'a bounded wait: polls up to exactly the limit')
        self.assertIn('cargo_reclaim_priority', err)
        self.assertFalse(self.pending().exists(), 'a reclaim that gives up clears its marker too')
        self.assertIsNone(holder.proc.poll(), 'the running build was not interrupted')
        self.assertEqual(holder.finish(), 0)
        self.assert_untouched()

    def test_timeout_backoff_survives_plain_deferrals(self):
        fd = guard.open_lock(guard.lease_path(self.root), shared=True)
        self.addCleanup(os.close, fd)
        clock = FakeClock(NOW)
        env = dict(AMUX_CARGO_RECLAIM_PRIORITY_AFTER='1',
                   AMUX_CARGO_RECLAIM_PRIORITY_WAIT_S='2',
                   AMUX_CARGO_RECLAIM_PRIORITY_BACKOFF_S='10',
                   AMUX_CARGO_RECLAIM_POLL_S='1')
        _, first, _ = self.reclaim(clock, **env)
        self.assertTrue(first['priority'])
        ended = clock.now
        for advance in (0, 9):
            clock.now = ended + advance
            sleeps = len(clock.sleeps)
            _, verdict, _ = self.reclaim(clock, **env)
            self.assertFalse(verdict.get('priority', False))
            self.assertEqual(verdict['priority_backoff_remaining_s'], 10 - advance)
            self.assertEqual(len(clock.sleeps), sleeps)
            self.assertFalse(self.pending().exists())
        clock.now = ended + 10
        _, last, _ = self.reclaim(clock, **env)
        self.assertTrue(last['priority'])

    def test_reused_pid_without_marker_lock_is_stale(self):
        self.write_marker(os.getpid(), NOW)
        marker, reason = guard.read_pending(self.root, NOW, 1200)
        self.assertIsNone(marker)
        self.assertIn('gone', reason)

    def cli_reclaim(self, **env):
        result = subprocess.run(
            [sys.executable, str(SCRIPT), 'reclaim', '--target', str(self.root)],
            env={**self.env, 'AMUX_CARGO_RECLAIM_PRIORITY_AFTER': '0', **env},
            capture_output=True, text=True, timeout=10)
        self.assertEqual(result.returncode, 75, result.stderr)
        self.assertNotIn('Traceback', result.stderr)
        verdict = json.loads(result.stdout)
        self.assertEqual(verdict['verdict'], 'cargo_reclaim_deferred')
        self.assert_untouched()
        return verdict

    def test_corrupt_cooldown_cli_defers_with_diagnostic(self):
        fd = guard.open_lock(guard.lease_path(self.root), shared=True)
        self.addCleanup(os.close, fd)
        path = guard.priority_timeout_path(self.root)
        for value in (b'interrupted-write', b'\xff', b'nan', b'inf', b'-inf'):
            with self.subTest(value=value):
                path.write_bytes(value)
                verdict = self.cli_reclaim()
                self.assertIn('priority backoff unmeasured', verdict['reason'])
                self.assertIn(str(path), verdict['reason'])

    def test_unreadable_cooldown_cli_defers_with_diagnostic(self):
        fd = guard.open_lock(guard.lease_path(self.root), shared=True)
        self.addCleanup(os.close, fd)
        # A directory gives a real read error even when the suite runs as root.
        path = guard.priority_timeout_path(self.root)
        path.mkdir()
        verdict = self.cli_reclaim()
        self.assertIn('priority backoff unmeasured', verdict['reason'])
        self.assertIn(str(path), verdict['reason'])

    def test_corrupt_streak_cli_resets_and_reports(self):
        fd = guard.open_lock(guard.lease_path(self.root), shared=True)
        self.addCleanup(os.close, fd)
        path = guard.streak_path(self.root)
        reason = self.cli_reclaim()['reason']
        cases = ['interrupted-write', '[]', 'null', '42']
        for field in ('count', 'since', 'priority_count'):
            for value in (None, [], {}, 'bad', float('inf')):
                cases.append(json.dumps({'reason': reason, field: value}))
        for value in cases:
            with self.subTest(value=value):
                path.write_text(value)
                verdict = self.cli_reclaim()
                self.assertEqual(verdict['deferral_streak'], 1)

    def test_unreadable_streak_cli_still_defers(self):
        fd = guard.open_lock(guard.lease_path(self.root), shared=True)
        self.addCleanup(os.close, fd)
        guard.streak_path(self.root).mkdir()
        self.assertEqual(self.cli_reclaim()['deferral_streak'], 1)
        guard.clear_streak([self.root])  # failed unlink is best effort too

    def test_corrupt_markers_are_ignored_and_clear_is_safe(self):
        for value in ('broken', '[]', 'null', '{}', '{"since": null}',
                      '{"since": NaN}', '{"since": Infinity}',
                      json.dumps({'since': 10 ** 400})):
            with self.subTest(value=value):
                self.pending().write_text(value)
                fd = guard.open_lock(self.pending())
                try:
                    marker, reason = guard.read_pending(self.root, NOW, 1200)
                    self.assertIsNone(marker)
                    self.assertIn('unreadable', reason)
                finally:
                    os.close(fd)
                guard.clear_pending([self.root])

    def test_unreadable_marker_cli_defers_and_clear_is_safe(self):
        self.pending().mkdir()
        marker, reason = guard.read_pending(self.root, NOW, 1200)
        self.assertIsNone(marker)
        self.assertIn('unreadable', reason)
        guard.clear_pending([self.root])
        fd = guard.open_lock(guard.lease_path(self.root), shared=True)
        self.addCleanup(os.close, fd)
        self.cli_reclaim(AMUX_CARGO_RECLAIM_PRIORITY_AFTER='1')

    def test_unwritable_cooldown_cli_defers_and_clears_marker(self):
        fd = guard.open_lock(guard.lease_path(self.root), shared=True)
        self.addCleanup(os.close, fd)
        path = guard.priority_timeout_path(self.root)
        path.mkdir()
        with mock.patch.object(guard, 'priority_backoff', return_value=0):
            code, verdict, _ = self.reclaim(FakeClock(NOW),
                AMUX_CARGO_RECLAIM_PRIORITY_AFTER='1', AMUX_CARGO_RECLAIM_PRIORITY_WAIT_S='0')
        self.assertEqual(code, 75)
        self.assertEqual(verdict['verdict'], 'cargo_reclaim_deferred')
        self.assertIn(str(path), verdict['reason'])
        self.assertFalse(self.pending().exists())

    def test_failed_marker_publication_defers_and_removes_temporary(self):
        fd = guard.open_lock(guard.lease_path(self.root), shared=True)
        self.addCleanup(os.close, fd)
        with mock.patch.object(guard.os, 'replace', side_effect=OSError('fixture publish failed')):
            code, verdict, _ = self.reclaim(FakeClock(NOW),
                                          AMUX_CARGO_RECLAIM_PRIORITY_AFTER='1')
        self.assertEqual(code, 75)
        self.assertEqual(verdict['verdict'], 'cargo_reclaim_deferred')
        self.assertIn('fixture publish failed', verdict['reason'])
        self.assertFalse(self.pending().exists())
        self.assertNotIn(self.pending(), guard._PENDING)
        self.assertEqual(list(self.base.glob(self.pending().name + '.*')),
                         [Path(str(self.pending()) + '.lock')])
        self.assert_untouched()

    def test_peer_cannot_delete_locked_marker_even_with_same_pid(self):
        self.write_marker(os.getpid(), NOW)
        fd = guard.open_lock(self.pending())
        try:
            guard.clear_pending([self.root])
            self.assertTrue(self.pending().exists())
        finally:
            os.close(fd)

    def test_marker_is_complete_and_locked_before_publication(self):
        replace = os.replace
        def publish(source, destination):
            with open(source) as handle:
                self.assertEqual(json.load(handle)['pid'], os.getpid())
                with self.assertRaises(BlockingIOError):
                    guard.fcntl.flock(handle.fileno(), guard.fcntl.LOCK_EX | guard.fcntl.LOCK_NB)
            replace(source, destination)
        try:
            with mock.patch.object(guard.os, 'replace', side_effect=publish):
                guard.write_pending([self.root], {}, NOW)
            marker, ignored = guard.read_pending(self.root, NOW, 1200)
            self.assertIsNotNone(marker)
            self.assertIsNone(ignored)
        finally:
            guard.clear_pending([self.root])

    def test_publication_preserves_a_live_peers_inode(self):
        self.write_marker(os.getpid() + 100000, NOW)
        fd = guard.open_lock(self.pending())
        before = self.pending().stat().st_ino
        try:
            guard.write_pending([self.root], {}, NOW)
            self.assertEqual(self.pending().stat().st_ino, before)
            guard.clear_pending([self.root])
            self.assertEqual(self.pending().stat().st_ino, before)
        finally:
            os.close(fd)

    def test_clear_pending_checks_owner_before_deleting(self):
        self.write_marker(os.getpid() + 100000, NOW)
        guard.clear_pending([self.root])
        self.assertTrue(self.pending().exists())

    def test_marker_arriving_during_lease_acquisition_yields(self):
        clock = FakeClock(NOW)
        original = guard.open_lock
        arrived = []
        def acquire(path, **kwargs):
            fd = original(path, **kwargs)
            if path == guard.lease_path(self.root) and not arrived:
                arrived.append(True)
                guard.write_pending([self.root], {}, NOW)
            return fd
        def drain(_):
            # The build must have released its lease BEFORE sleeping.
            os.close(original(guard.lease_path(self.root)))
            guard.clear_pending([self.root])
        clock.on_sleep = drain
        with mock.patch.dict(os.environ, self.env, clear=True), \
                mock.patch.object(guard, 'time', clock), \
                mock.patch.object(guard, 'open_lock', side_effect=acquire):
            fds = guard.acquire_build_leases([self.root])
        for fd in fds:
            os.close(fd)
        guard.clear_pending([self.root])
        self.assertTrue(clock.sleeps, 'post-lease marker must make the build yield')

    def test_only_contended_deferrals_can_prioritize(self):
        for error in (guard.Deferred('unmeasured'),
                      guard.Deferred('active processes', measured=True, considered=1)):
            with self.subTest(reason=str(error)), \
                    mock.patch.dict(os.environ, {**self.env, 'AMUX_CARGO_RECLAIM_PRIORITY_AFTER': '1'}), \
                    mock.patch.object(guard, 'time', FakeClock(NOW)), \
                    mock.patch.object(guard, 'reclaim', side_effect=error), \
                    mock.patch.object(guard, 'write_pending') as write:
                with self.assertRaises(guard.Deferred):
                    guard.prioritized_reclaim([self.root], 0, 0)
                write.assert_not_called()
                self.assertFalse(guard.streak_path(self.root).exists())

    def test_unmeasured_lock_streak_does_not_seed_priority(self):
        clock = FakeClock(NOW)
        reason = 'lock busy or unmeasured: same-path'
        for _ in range(3):
            with mock.patch.object(guard, 'reclaim', side_effect=guard.Deferred(reason)):
                self.reclaim(clock)
        with mock.patch.object(guard, 'reclaim', side_effect=guard.Deferred(reason, contended=True)):
            _, verdict, _ = self.reclaim(clock)
        self.assertFalse(verdict.get('priority', False))
        self.assertEqual(clock.sleeps, [])

    def test_zero_poll_is_usage_error(self):
        result = subprocess.run([sys.executable, str(SCRIPT), 'run', '--target',
                                 str(self.root), '--', 'true'], capture_output=True, text=True,
                                env={**self.env, 'AMUX_CARGO_RECLAIM_POLL_S': '0'})
        self.assertEqual(result.returncode, 2)
        self.assertIn('AMUX_CARGO_RECLAIM_POLL_S', result.stderr)

    def test_wait_exceeding_marker_age_is_usage_error(self):
        result = subprocess.run([sys.executable, str(SCRIPT), 'reclaim', '--target',
                                 str(self.root)], capture_output=True, text=True,
                                env={**self.env, 'AMUX_CARGO_RECLAIM_PRIORITY_WAIT_S': '1201'})
        self.assertEqual(result.returncode, 2)
        self.assertIn('AMUX_CARGO_RECLAIM_PRIORITY_WAIT_S', result.stderr)

    def test_run_names_the_lease_fds_it_passes_to_cargo(self):
        # A test process marks these close-on-exec, so it must get the
        # descriptor numbers, and the new one must be the lease itself.
        probe = ('import os; fds = os.environ["AMUX_CARGO_GUARD_LEASE_FDS"]; print(fds); '
                 'print(" ".join("%d:%d" % (os.fstat(int(f)).st_dev, os.fstat(int(f)).st_ino) '
                 'for f in fds.split(",") if f != "1"))')
        result = subprocess.run([sys.executable, str(SCRIPT), 'run', '--target', str(self.root), '--',
                                 sys.executable, '-c', probe],
                                capture_output=True, text=True, env={**self.env, 'AMUX_CARGO_GUARD_LEASE_FDS': '1'})
        self.assertEqual(result.returncode, 0, result.stderr)
        listed, held = result.stdout.splitlines()
        self.assertIn('1', listed.split(','), "an ancestor run's descriptor stays listed")
        lease = os.stat(guard.lease_path(self.root))
        self.assertEqual(held.split(), ['%d:%d' % (lease.st_dev, lease.st_ino)])

    def test_refused_exclusive_run_does_not_count_as_reclaim(self):
        fd = guard.open_lock(guard.lease_path(self.root), shared=True)
        try:
            result = subprocess.run([sys.executable, str(SCRIPT), 'run', '--exclusive-run',
                                     '--target', str(self.root), '--', 'true'],
                                    capture_output=True, text=True, env=self.env)
        finally:
            os.close(fd)
        self.assertEqual(result.returncode, 75)
        self.assertFalse(guard.streak_path(self.root).exists())

    def test_zero_disables_the_priority(self):
        holder = self.build()
        holder.ready()
        clock = FakeClock(time.time())
        for _ in range(4):
            code, verdict, _ = self.reclaim(clock, AMUX_CARGO_RECLAIM_PRIORITY_AFTER='0')
            self.assertEqual(code, 75)
        self.assertEqual(clock.sleeps, [])
        self.assertFalse(self.pending().exists())

    def test_a_held_exclusive_lease_is_never_broken(self):
        # Another writer (cargo clean --exclusive-run, a sweep) holds the lease exclusively.
        fd = guard.open_lock(guard.lease_path(self.root))
        try:
            clock = FakeClock(time.time())
            code, verdict, _ = self.reclaim(clock, AMUX_CARGO_RECLAIM_PRIORITY_AFTER='1',
                                            AMUX_CARGO_RECLAIM_PRIORITY_WAIT_S='30',
                                            AMUX_CARGO_RECLAIM_POLL_S='5')
            self.assertEqual(code, 75)
            self.assertIs(verdict['priority'], True)
            self.assertIn('lock busy', verdict['reason'])
            self.assertEqual((sum(clock.sleeps), verdict['waited_s']), (30, 30))
            with self.assertRaisesRegex(guard.Deferred, 'lock busy'):
                os.close(guard.open_lock(guard.lease_path(self.root), shared=True))
        finally:
            os.close(fd)
        self.assertFalse(self.pending().exists())
        self.assert_untouched()

    def write_marker(self, pid, since):
        self.pending().write_text(json.dumps({'pid': pid, 'since': since, 'deferral_streak': 3}))

    def test_a_fresh_marker_holds_a_new_build_only_up_to_its_bound(self):
        guard.write_pending([self.root], {}, time.time())
        self.addCleanup(guard.clear_pending, [self.root])
        started = time.monotonic()
        build = self.build(AMUX_CARGO_RECLAIM_BUILD_WAIT_S='0.5')
        build.err.wait_for('cargo_reclaim_pending:')
        build.ready()
        self.assertGreaterEqual(time.monotonic() - started, 0.5)
        build.err.wait_for('cargo_reclaim_pending_timeout')
        self.assertEqual(build.finish(), 0)

    def test_a_build_of_two_targets_holds_no_lease_while_it_waits(self):
        """Holding the first target's lease while waiting on the second's marker
        would stall a reclaimer of both until its own bound ran out."""
        other = self.base / 'rust-build-target-e2e-head'
        other.mkdir()
        marker = other.parent / ('.' + other.name + '.reclaim.pending')
        guard.write_pending([other], {}, time.time())
        self.addCleanup(guard.clear_pending, [other])
        build = FixtureBuild(self.root, self.env, extra_targets=[other])
        self.builds.append(build)
        build.err.wait_for('cargo_reclaim_pending:')
        for target in (self.root, other):
            os.close(guard.open_lock(guard.lease_path(target)))  # exclusive: no lease is held
        guard.clear_pending([other])
        build.ready()
        build.err.wait_for('cargo_reclaim_pending_cleared')
        self.assertEqual(build.finish(), 0)

    def test_a_stale_marker_is_ignored(self):
        guard.write_pending([self.root], {}, time.time() - 1200 - 60)
        self.addCleanup(guard.clear_pending, [self.root])
        build = self.build()
        build.ready()
        self.assertIn('stale', build.err.wait_for('cargo_reclaim_pending_ignored'))
        self.assertEqual(build.finish(), 0)

    def test_a_reclaimer_that_dies_leaves_no_block(self):
        # The real marker writer, in a process that then dies without its cleanup.
        crash = ('import importlib.util, os, sys, time; from pathlib import Path\n'
                 'spec = importlib.util.spec_from_file_location("guard", sys.argv[1])\n'
                 'guard = importlib.util.module_from_spec(spec); spec.loader.exec_module(guard)\n'
                 'guard.write_pending([Path(sys.argv[2])], {"deferral_streak": 3}, time.time())\n'
                 'os._exit(9)\n')
        died = subprocess.run([sys.executable, '-c', crash, str(SCRIPT), str(self.root)], env=self.env)
        self.assertEqual(died.returncode, 9)
        self.assertTrue(self.pending().exists(), 'the crash left its marker behind')
        build = self.build()
        build.ready()
        self.assertIn('gone', build.err.wait_for('cargo_reclaim_pending_ignored'))
        self.assertEqual(build.finish(), 0)

    def test_a_sigkilled_reclaimer_leaves_no_block(self):
        crash = ('import importlib.util, sys, time; from pathlib import Path\n'
                 'spec = importlib.util.spec_from_file_location("guard", sys.argv[1])\n'
                 'guard = importlib.util.module_from_spec(spec); spec.loader.exec_module(guard)\n'
                 'guard.write_pending([Path(sys.argv[2])], {}, time.time())\n'
                 'print("ready", flush=True); sys.stdin.readline()\n')
        with subprocess.Popen([sys.executable, '-c', crash, str(SCRIPT), str(self.root)],
                              env=self.env, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                              text=True) as proc:
            try:
                self.assertEqual(proc.stdout.readline().strip(), 'ready')
                marker, reason = guard.read_pending(self.root, time.time(), 1200)
                self.assertIsNotNone(marker)
                self.assertIsNone(reason)
                proc.kill()  # SIGKILL only this fixture, after locked publication
                self.assertEqual(proc.wait(timeout=10), -signal.SIGKILL)
            finally:
                if proc.poll() is None:
                    proc.kill()
                    proc.wait(timeout=10)
        self.assertTrue(self.pending().exists(), 'SIGKILL leaves the published marker')
        build = self.build()
        build.ready()
        self.assertIn('gone', build.err.wait_for('cargo_reclaim_pending_ignored'))
        self.assertEqual(build.finish(), 0)

    def test_a_build_nested_in_a_leased_build_does_not_wait_on_its_own_ancestor(self):
        """A test spawning safe-cargo on the same target already runs under a lease;
        making it wait would hold that lease until the reclaimer gave up."""
        guard.write_pending([self.root], {}, time.time())
        self.addCleanup(guard.clear_pending, [self.root])
        build = self.build(AMUX_CARGO_GUARD_LEASED=str(self.root))
        build.ready()
        self.assertEqual(build.finish(), 0)
        clock = FakeClock(time.time())
        fd = guard.open_lock(guard.lease_path(self.root), shared=True)
        try:
            code, verdict, _ = self.reclaim(clock, AMUX_CARGO_GUARD_LEASED=str(self.root),
                                            AMUX_CARGO_RECLAIM_PRIORITY_AFTER='1')
        finally:
            os.close(fd)
        self.assertEqual(code, 75)
        self.assertIs(verdict.get('priority', False), False)
        self.assertEqual(clock.sleeps, [], 'a reclaim inside a leased build cannot wait for that lease')


if __name__ == '__main__':
    unittest.main()
