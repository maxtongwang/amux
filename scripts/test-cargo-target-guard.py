#!/usr/bin/env python3
"""ATE-92: real process/lock lifetimes, preserved artifacts, and idle retry."""
import importlib.util
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

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
        source.write_text('#include <unistd.h>\nint main(void) { sleep(60); return 0; }\n')
        subprocess.run(['cc', str(source), '-o', str(binary)], check=True, capture_output=True)
        proc = subprocess.Popen([str(binary)])
        try:
            active, count = self.measured_active([self.root])
            self.assertIn(str(proc.pid), active)
            self.assertGreater(count, 0)
            with self.assertRaises(guard.Deferred):
                self.mutate(probe=guard.active_processes)
            self.assertTrue(binary.exists())
        finally:
            proc.terminate()
            proc.wait(timeout=10)
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
        proc = subprocess.Popen(['sh', '-c', 'sleep 30 # cargo rustc build'])
        try:
            active, _ = self.measured_active([self.root])
            self.assertNotIn(str(proc.pid), active)
        finally:
            proc.terminate()
            proc.wait(timeout=10)

    def test_hardened_linux_proc_uses_known_command_identity(self):
        proc = self.base / 'proc' / '1177'
        proc.mkdir(parents=True)

        def denied(_):
            raise PermissionError('hardened procfs')

        resolved = guard.process_executable(
            '1177', 'sleep', linux=True, proc_root=self.base / 'proc',
            readlink=denied, which=lambda name: '/usr/bin/sleep' if name == 'sleep' else None)
        self.assertEqual(resolved, Path('/usr/bin/sleep'))

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
        source.write_text('#include <unistd.h>\nint main(void) { sleep(60); return 0; }\n')
        subprocess.run(['cc', str(source), '-o', str(binary)], check=True, capture_output=True)
        proc = subprocess.Popen([str(binary)])
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
            proc.kill()
            proc.wait()

    def test_a_compiler_is_exempt_by_name_however_old_it_is(self):
        """A long compile is still a compile, so age must not reach it.

        Without this, the age bound would silently become a licence to delete
        the target dir out from under a genuinely slow build, which is the
        failure ATE-92 made this guard refuse in the first place.
        """
        binary = self.artifacts / 'cargo'
        source = self.base / 'slow.c'
        source.write_text('#include <unistd.h>\nint main(void) { sleep(60); return 0; }\n')
        subprocess.run(['cc', str(source), '-o', str(binary)], check=True, capture_output=True)
        proc = subprocess.Popen([str(binary)])
        try:
            original = guard.STALE_BUILD_AGE_S
            guard.STALE_BUILD_AGE_S = 0
            self.addCleanup(setattr, guard, 'STALE_BUILD_AGE_S', original)
            active, _ = self.measured_active([self.root])
            self.assertIn(str(proc.pid), active,
                          'a compiler is active by NAME; the age bound must not reach it')
        finally:
            proc.kill()
            proc.wait()

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
        self.addCleanup(lambda: subprocess.run(['rm', '-rf', str(self.base)]))
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


if __name__ == '__main__':
    unittest.main()
