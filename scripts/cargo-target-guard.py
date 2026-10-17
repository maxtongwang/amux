#!/usr/bin/env python3
"""ATE-92: one Cargo cleanup guard for the builder and the embedded reclaim API.

The lifetime lease lives OUTSIDE target/ and survives exec into cargo and tests.
Reclaim also takes Cargo's native locks and probes processes for older/unwrapped
builds. Native lock files and their ancestors never move or disappear. Failed
probes/locks defer cleanup, regardless of free disk; the next idle tick retries.
"""
import argparse
import contextlib
import errno
import fcntl
import json
import math
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import time


class Deferred(Exception):
    def __init__(self, reason, measured=False, considered=0, contended=False):
        super().__init__(reason)
        self.measured = measured
        self.considered = considered
        # True only when a lock was measured as HELD by someone else, as opposed
        # to unopenable or unmeasured: the one condition waiting can resolve.
        self.contended = contended


def overlaps(left, right):
    return left == right or left in right.parents or right in left.parents


def lease_path(target):
    return target.parent / ('.' + target.name + '.reclaim.lock')


# A DEFERRAL STREAK ON THE SAME CONDITION IS A WEDGE, NOT A PAUSE (AMUX-4944).
#
# The deferral logs with measured:true and a real pid, so it reads as the guard
# working. Nothing counted CONSECUTIVE deferrals, so 243 identical refusals
# looked exactly like one and the fleet's deploys were dark for days before
# anyone asked why their own work was not live.
WEDGE_STREAK = int(os.environ.get('AMUX_CARGO_GUARD_WEDGE_STREAK', '10'))


def streak_path(target):
    return target.parent / ('.' + target.name + '.reclaim.streak')


def bump_streak(targets, reason, now=None, *, contended=False):
    """Consecutive deferrals for the SAME reason, as a count.

    Keyed on the reason text, so a DIFFERENT blocker resets it: a new pid is a
    new condition, and calling that a continuing wedge would be the same class
    of lie as not counting at all. Both fields are computed from stored state;
    neither can disagree with the run that produced it.
    """
    if not targets:
        return None
    path = streak_path(sorted(targets)[0])
    now = int(time.time()) if now is None else now
    try:
        previous = json.loads(path.read_text())
    except (OSError, ValueError):
        previous = {}
    if previous.get('reason') == reason:
        count = int(previous.get('count', 0)) + 1
        since = int(previous.get('since', now))
    else:
        count, since = 1, now
    priority_count = (int(previous.get('priority_count', 0)) + 1
                      if contended and previous.get('reason') == reason else int(contended))
    try:
        path.write_text(json.dumps({'reason': reason, 'count': count, 'since': since,
                                    'priority_count': priority_count}))
    except OSError:
        pass  # an unwritable streak file must never fail the reclaim itself
    return {'deferral_streak': count, 'priority_streak': priority_count, 'streak_since': since,
            'wedged': count >= WEDGE_STREAK}


def clear_streak(targets):
    """A reclaim that got through ends the streak: the condition cleared."""
    for target in targets:
        try:
            streak_path(target).unlink(missing_ok=True)
        except OSError:
            pass


def open_lock(path, shared=False, wait=False):
    path.parent.mkdir(parents=True, exist_ok=True)
    fd = os.open(path, os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600)
    try:
        fcntl.flock(fd, (fcntl.LOCK_SH if shared else fcntl.LOCK_EX)
                    | (0 if wait else fcntl.LOCK_NB))
    except OSError as error:
        os.close(fd)
        raise Deferred('lock busy or unmeasured: ' + str(path),
                       contended=error.errno in (errno.EWOULDBLOCK, errno.EAGAIN)) from error
    return fd


# WRITER PREFERENCE FOR A STARVED RECLAIM (DMIW-7 follow-up to PR #11).
#
# flock has no writer preference. Builds take the lease SHARED and back-to-back
# builds overlap, so the reclaim's LOCK_EX|LOCK_NB found it held on every try:
# deferral_streak 15, debug/ at 40.1 GiB over its 40 budget, and every new build
# refused with exit 75 while the disk had 734 GB free. A reclaim that can never
# run is a refusal that can never clear (ethos rule 3).
#
# So once a reclaim has deferred on a HELD lock PRIORITY_AFTER times in a row, it
# writes a pending marker beside the lease and waits, bounded, for the running
# builds to drain. New builds (`run`) see the fresh marker and wait, bounded,
# instead of taking a shared lease. Nothing already holding a lease is touched.
# The marker holds an flock until the wait ends. Process exit releases it even
# if the pid is reused; the age bound also limits a stuck live reclaimer.
#
# Settings are read per call so a caller's environment (and a test) controls them.
# PRIORITY_AFTER 0 turns the preference off.
PRIORITY_SETTINGS = {
    'after': ('AMUX_CARGO_RECLAIM_PRIORITY_AFTER', 3),
    'backoff_s': ('AMUX_CARGO_RECLAIM_PRIORITY_BACKOFF_S', 1800),
    'wait_s': ('AMUX_CARGO_RECLAIM_PRIORITY_WAIT_S', 1800),  # the reclaimer's wait for running builds
    'build_wait_s': ('AMUX_CARGO_RECLAIM_BUILD_WAIT_S', 3600),  # a new build's wait behind the marker
    'max_age_s': ('AMUX_CARGO_RECLAIM_PENDING_MAX_AGE_S', 3600),  # older markers are ignored
    'poll_s': ('AMUX_CARGO_RECLAIM_POLL_S', 2),
}
# The targets an ancestor `run` already leases. A build or reclaim nested inside
# that build must not wait for a lease its own ancestor holds.
LEASED_ENV = 'AMUX_CARGO_GUARD_LEASED'


def priority_setting(key):
    name, default = PRIORITY_SETTINGS[key]
    try:
        value = float(os.environ.get(name, default))
    except ValueError:
        raise Deferred('invalid %s: %r' % (name, os.environ.get(name))) from None
    if not math.isfinite(value) or value < 0 or (key == 'poll_s' and value == 0):
        raise Deferred('invalid %s: must be finite and nonnegative (poll must be positive)' % name)
    return value


def inherited_leases():
    return {Path(p) for p in os.environ.get(LEASED_ENV, '').split(os.pathsep) if p}


def pending_path(target):
    return target.parent / ('.' + target.name + '.reclaim.pending')


# Held marker descriptors belong to this process, never to a pid read from disk.
_PENDING = {}


@contextlib.contextmanager
def pending_transaction(target):
    """Serialize publication and unlink on a stable inode, never the replaced marker."""
    fd = open_lock(Path(str(pending_path(target)) + '.lock'), wait=True)
    try:
        yield
    finally:
        os.close(fd)


def marker_unlocked(fd):
    try:
        fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        return True
    except BlockingIOError:
        return False


def write_pending(targets, streak, now):
    """Publish complete, already-locked markers; never replace a live peer's inode."""
    payload = json.dumps({'pid': os.getpid(), 'since': now,
                          'deferral_streak': (streak or {}).get('priority_streak',
                                                                  (streak or {}).get('deferral_streak'))})
    try:
        for target in targets:
            path = pending_path(target)
            with pending_transaction(target):
                try:
                    with path.open('r') as old:
                        if not marker_unlocked(old.fileno()):
                            continue
                except FileNotFoundError:
                    pass
                fd, temporary = tempfile.mkstemp(dir=path.parent, prefix=path.name + '.')
                try:
                    fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
                    os.write(fd, payload.encode())
                    os.replace(temporary, path)
                    _PENDING[path] = fd
                except BaseException:
                    os.close(fd)
                    Path(temporary).unlink(missing_ok=True)
                    raise
    except BaseException:
        clear_pending(targets)
        raise


def clear_pending(targets):
    """Only the owner may clear; a peer holding the inode lock always wins."""
    for target in targets:
        path = pending_path(target)
        owned = _PENDING.pop(path, None)
        try:
            with pending_transaction(target):
                with path.open('r') as handle:
                    # Release our own lifetime lock while holding the publication
                    # mutex, then require the same LOCK_NB proof as any peer.
                    if owned is not None:
                        os.close(owned)
                        owned = None
                    if not marker_unlocked(handle.fileno()):
                        continue
                    if json.load(handle).get('pid') == os.getpid():
                        path.unlink()
        except (OSError, ValueError, AttributeError):
            pass
        finally:
            if owned is not None:
                os.close(owned)


def read_pending(target, now, max_age_s):
    """A marker is live only while its inode is locked, regardless of pid reuse."""
    path = pending_path(target)
    try:
        with pending_transaction(target):
            with path.open('r') as handle:
                marker = json.load(handle)
                since = float(marker['since'])
                if marker_unlocked(handle.fileno()):
                    path.unlink()
                    return None, 'reclaimer lock of %s is gone' % path
                age = now - since
                if abs(age) > max_age_s:
                    return None, 'stale %s: age %ds is beyond %ds' % (path, age, max_age_s)
                return marker, None
    except FileNotFoundError:
        return None, None
    except (OSError, ValueError, KeyError, TypeError) as error:
        return None, 'unreadable %s: %s' % (path, error)


def priority_timeout_path(target):
    return target.parent / ('.' + target.name + '.reclaim.timeout')


def priority_backoff(targets, backoff_s):
    remaining = 0
    for target in targets:
        try:
            ended = float(priority_timeout_path(target).read_text())
            remaining = max(remaining, ended + backoff_s - time.time())
        except FileNotFoundError:
            pass
        except (OSError, ValueError) as error:
            raise Deferred('priority backoff unmeasured: ' + str(path)) from error
    return max(0, remaining)


def record_priority_timeout(targets):
    for target in targets:
        path = priority_timeout_path(target)
        fd, temporary = tempfile.mkstemp(dir=path.parent, prefix=path.name + '.')
        try:
            with os.fdopen(fd, 'w') as handle:
                handle.write(str(time.time()))
            os.replace(temporary, path)
        finally:
            Path(temporary).unlink(missing_ok=True)


def acquire_build_leases(targets):
    """A new build's shared leases, taken only once no target has a fresh pending reclaim.

    Every target is checked before ANY lease is taken: holding one lease while
    waiting on another target's marker would stall a reclaimer of both. Bounded
    by build_wait_s: past it the build leases anyway and says so. Markers are
    re-read AFTER the leases are held, so a reclaim marked between the look and
    the lease still gets this build to back off.
    """
    inherited = inherited_leases()
    watched = [t for t in targets if t not in inherited]
    wait_s, poll_s, max_age_s = (priority_setting(k) for k in ('build_wait_s', 'poll_s', 'max_age_s'))
    started = time.time()
    waited = False
    reported = set()

    def fresh():
        markers = []
        for target in watched:
            marker, ignored = read_pending(target, time.time(), max_age_s)
            if ignored and ignored not in reported:
                reported.add(ignored)
                print('cargo_reclaim_pending_ignored: ' + ignored, file=sys.stderr)
            if marker:
                markers.append((target, marker))
        return markers

    while True:
        markers = fresh()
        elapsed = time.time() - started
        if markers and elapsed < wait_s:
            if not waited:
                print('cargo_reclaim_pending: %s; waiting up to %ds before taking a shared lease'
                      % ('; '.join('reclaim pid %s marked %s after %s busy deferrals'
                                   % (m.get('pid'), pending_path(t), m.get('deferral_streak'))
                                   for t, m in markers), wait_s), file=sys.stderr)
                waited = True
            time.sleep(min(poll_s, wait_s - elapsed))
            continue
        if markers:
            print('cargo_reclaim_pending_timeout: waited %.1fs for reclaim pid(s) %s; '
                  'taking a shared lease anyway'
                  % (elapsed, ','.join(str(m.get('pid')) for _, m in markers)), file=sys.stderr)
        fds = []
        try:
            for target in targets:
                fds.append(open_lock(lease_path(target), shared=True, wait=True))
        except BaseException:
            for fd in fds:
                os.close(fd)
            raise
        if not markers and fresh() and time.time() - started < wait_s:
            for fd in fds:  # a reclaim was marked while these leases were being taken: yield
                os.close(fd)
            continue
        if waited and not markers:
            print('cargo_reclaim_pending_cleared: waited %.1fs; building'
                  % (time.time() - started), file=sys.stderr)
        return fds


def native_locks(target):
    # Cargo profiles (debug/release/custom) and target triples. Do not traverse
    # deps/incremental/build: these can contain millions of artifacts, not locks.
    result = set()
    frontier = [(target, 0)]
    while frontier:
        directory, depth = frontier.pop()
        if not directory.exists():
            continue
        for entry in directory.iterdir():
            if entry.name in ('.cargo-lock', '.cargo-build-lock', '.cargo-artifact-lock'):
                result.add(entry)
            elif depth < 2 and entry.is_dir() and not entry.is_symlink() and entry.name not in (
                    'deps', 'incremental', 'build', 'examples', '.fingerprint', 'doc'):
                frontier.append((entry, depth + 1))
    # Create locks in existing standard profiles too, so a just-starting Cargo
    # cannot acquire a different inode after the process snapshot.
    for profile in ('debug', 'release'):
        if (target / profile).is_dir():
            result.add(target / profile / '.cargo-lock')
    return sorted(result)


def process_executable(pid, command, *, linux=None, proc_root=Path('/proc'),
                       readlink=None, which=None):
    """Resolve one process without treating an unreadable /proc link as a build.

    Hardened Linux runners can deny ``/proc/<pid>/exe`` for ordinary sibling
    processes.  ``ps comm`` is still measured evidence: when its complete name
    resolves through PATH (for example ``sleep`` or ``bash``), that executable
    cannot be a test artifact under a Cargo target.  Truncated/custom names do
    not resolve and therefore continue to fail closed.
    """
    executable = Path(command.removesuffix(' (deleted)'))
    linux = sys.platform.startswith('linux') if linux is None else linux
    if not linux:
        return executable
    readlink = os.readlink if readlink is None else readlink
    which = shutil.which if which is None else which
    proc = proc_root / pid
    try:
        if proc.stat().st_uid == os.getuid():
            executable = Path(readlink(proc / 'exe').removesuffix(' (deleted)'))
    except FileNotFoundError:
        return None  # process exited during the snapshot
    except PermissionError as error:
        known = which(executable.name)
        if known:
            known = Path(known).resolve()
            if known.name == executable.name:
                return known
        raise Deferred('process executable unmeasured: pid ' + pid) from error
    return executable


# A BUILD EXITS; A DAEMON DOES NOT (AMUX-4944).
#
# Anything older than this that merely LIVES under a target dir is not treated
# as an in-flight build. Compilers are exempt by name below: a long compile is
# still a compile. Deliberately generous — the longest build recorded on this
# hardware is 26m48s (CLAUDE.md) and the CI lib suite is ~7m — so no real build
# is near it, while the process that deadlocked the fleet had been up for days.
#
# RESIDUAL EXPOSURE, stated rather than hidden: an UNWRAPPED build older than
# this would no longer defer cleanup. Anything started through `guard run`
# holds a shared lease fd that `exclusive()` blocks on regardless of age, so
# the exposure is only a bare `cargo` invocation running over four hours.
STALE_BUILD_AGE_S = int(os.environ.get('AMUX_CARGO_GUARD_MAX_BUILD_AGE_S', '14400'))


def parse_etime(text):
    """`ps -o etime` as seconds, or None when it does not parse.

    `[[DD-]HH:]MM:SS`, the one format macOS and procps agree on — `etimes`
    (seconds) is Linux-only and macOS `ps` rejects the keyword outright.

    Pure and separate from the probe so the age rule can be pinned without
    spawning a process that has been running for four hours.
    """
    days = 0
    text = text.strip()
    if '-' in text:
        head, _, text = text.partition('-')
        if not head.isdigit():
            return None
        days = int(head)
    parts = text.split(':')
    if not (2 <= len(parts) <= 3) or not all(p.isdigit() for p in parts):
        return None
    parts = [int(p) for p in parts]
    hours, minutes, seconds = ([0] + parts) if len(parts) == 2 else parts
    return days * 86400 + hours * 3600 + minutes * 60 + seconds


def active_processes(targets):
    try:
        output = subprocess.run(['ps', '-A', '-ww', '-o', 'pid=,etime=,comm='],
                                capture_output=True, text=True, check=True, timeout=5).stdout
    except (OSError, subprocess.SubprocessError) as error:
        raise Deferred('process probe unmeasured: ' + str(error)) from error
    rows = [line.split(None, 2) for line in output.splitlines() if line.strip()]
    if not rows or any(len(row) != 3 or not row[0].isdigit() for row in rows):
        raise Deferred('process probe unmeasured: empty or malformed ps output')
    active = []
    stale = []
    for pid, etime, command in rows:
        # Linux ps comm is truncated. Resolve same-user executable paths so a
        # directly launched test binary is protected after its Cargo parent exits.
        executable = process_executable(pid, command)
        if executable is None:
            continue
        if executable.name in ('cargo', 'rustc', 'rustdoc', 'clippy-driver'):
            active.append(pid)
            continue
        if any(executable == root or root in executable.parents for root in targets):
            age = parse_etime(etime)
            # FAIL CLOSED on an unparseable age: ATE-92 made this refuse rather
            # than delete a live build, and an unmeasured age is not evidence
            # that the process is stale.
            if age is None or age < STALE_BUILD_AGE_S:
                active.append(pid)
            else:
                stale.append((pid, age, executable))
    if stale:
        # SURFACED, not silently dropped: this is the judgement that unblocks
        # the reclaim, so it has to be auditable in the builder log.
        print('cargo_guard_stale_under_target: not treating as in-flight build(s) '
              + ', '.join('pid %s age %ds %s' % (p, a, e) for p, a, e in stale),
              file=sys.stderr)
    return active, len(rows)


@contextlib.contextmanager
def exclusive(targets, process_probe=active_processes):
    descriptors = []
    locks = []
    try:
        for target in sorted(set(targets)):
            descriptors.append(open_lock(lease_path(target)))
            for lock in native_locks(target):
                descriptors.append(open_lock(lock))
                locks.append(lock)
        active, considered = process_probe(targets)
        if active:
            raise Deferred('active cargo/rustc/test process(es): ' + ','.join(active), True, considered)
        yield locks, considered
    finally:
        for descriptor in reversed(descriptors):
            os.close(descriptor)


def clear_preserving_locks(path, locks):
    if path in locks:
        return
    if any(path in lock.parents for lock in locks):
        for child in path.iterdir():
            clear_preserving_locks(child, locks)
    elif path.is_dir() and not path.is_symlink():
        shutil.rmtree(path)
    else:
        path.unlink(missing_ok=True)


def mutate(action, path, destination, targets, protected=(), dry_run=False,
           process_probe=active_processes):
    path = path.resolve()
    destination = destination.resolve() if destination else None
    touched = [root for root in targets if any(overlaps(root, candidate) for candidate in
               [path, *protected, *([destination] if destination else [])])]
    # A quarantine batch may contain artifacts moved by an older, unsafe server.
    # Probe those staged executable paths too, while locking their original roots.
    guard = exclusive(touched, lambda roots: process_probe([*roots, path])) if touched else contextlib.nullcontext(([], 0))
    with guard as (locks, considered):
        if action == 'move' and any(overlaps(path, lock) and (path == lock or path in lock.parents)
                                    for lock in locks):
            raise Deferred('Cargo lock directories cannot rotate; select obsolete artifact subdirectories')
        if any(path == lease_path(root) for root in targets):
            raise Deferred('Cargo lifetime lease cannot be reclaimed')
        if action == 'move' and destination.exists():
            raise Deferred('destination already exists')
        if action == 'move' and any(path == root or destination == root for root in touched):
            raise Deferred('Cargo target roots cannot rotate; select obsolete artifact subdirectories')
        if not dry_run:
            if action == 'clear':
                clear_preserving_locks(path, locks)
            elif action == 'move':
                destination.parent.mkdir(parents=True, exist_ok=True)
                path.rename(destination)
            elif action == 'purge':
                if path.exists():
                    clear_preserving_locks(path, locks)
        return {'verdict': 'dry_run' if dry_run else 'reclaimed',
                'measured': bool(touched), 'n_considered': considered}


# AUTOMATIC RECLAIM (DMIW-7). The budget in cargo-budget.py REFUSED a build when
# the shared target was over its cap, and nothing ever shrank it: debug/ reached
# 60 GB (deps 44, incremental 14) with the disk at 99%, and every lane's local
# gate failed with exit 75 until a human cleared it. A refusal that cannot clear
# itself is ethos rule 3. These two actions are the clearing half, under the
# same exclusive lease, Cargo locks and process probe as every other mutation.
#
# PROTECTED: deploy/ (the auto-builder's live server build) and release/. Only
# the other profiles (debug/, custom test profiles) are ever reclaimed.
PROTECTED_PROFILES = ('deploy', 'release')
GB = 1024 ** 3
SWEEP_IDLE_S = int(os.environ.get('AMUX_CARGO_SWEEP_IDLE_S', str(2 * 86400)))
# A Cargo output's unit: `<crate>-<16 hex>` before its extension or codegen-unit
# suffix. `lib` is a prefix only on library outputs, so `libc-<hash>.d` and
# `liblibc-<hash>.rlib` are one unit.
UNIT = re.compile(r'^(.+?-[0-9a-f]{16})(?:[.-]|$)')
LIB_SUFFIXES = ('.rlib', '.rmeta', '.a', '.so', '.dylib', '.dll', '.lib')


def unit_key(name):
    if name.startswith('lib') and name.endswith(LIB_SUFFIXES):
        name = name[3:]
    match = UNIT.match(name)
    return match.group(1) if match else name


def entry_bytes(path):
    """Allocated bytes under path, never following a symlink (st_blocks, as du counts)."""
    try:
        st = path.lstat()
    except OSError:
        return 0
    if not path.is_dir() or path.is_symlink():
        return st.st_blocks * 512
    total = st.st_blocks * 512
    for root, dirs, files in os.walk(path):
        for name in dirs + files:
            try:
                total += os.lstat(os.path.join(root, name)).st_blocks * 512
            except OSError:
                pass
    return total


def last_used(path):
    """Newest access or modification under path. rustc memory-maps the libraries it
    links, which updates their atime, so a dependency used by every build but not
    rebuilt for days still counts as used (sweep relies on this only for deps/)."""
    newest = 0.0
    paths = [path]
    if path.is_dir() and not path.is_symlink():
        paths += [Path(r) / n for r, ds, fs in os.walk(path) for n in ds + fs]
    for item in paths:
        try:
            st = item.lstat()
        except OSError:
            continue
        newest = max(newest, st.st_atime, st.st_mtime)
    return newest


# Where one compiled unit leaves its outputs inside a profile. A unit is removed
# from all of them together, so no half-unit is left for Cargo to trip over: a
# missing output makes Cargo rebuild that unit, which is the intended cost.
UNIT_DIRS = ('deps', 'build', '.fingerprint', 'examples')


def reclaimable_profiles(target):
    if not target.is_dir():
        return []
    return sorted(p for p in target.iterdir()
                  if p.is_dir() and not p.is_symlink() and not p.name.startswith('.')
                  and p.name not in PROTECTED_PROFILES
                  and any((p / d).is_dir() for d in (*UNIT_DIRS, 'incremental')))


def dep_units(profiles):
    """{(profile, unit key): [paths]} for every compiled unit's outputs in a profile."""
    units = {}
    for profile in profiles:
        for sub in UNIT_DIRS:
            folder = profile / sub
            if folder.is_dir() and not folder.is_symlink():
                for entry in folder.iterdir():
                    units.setdefault((profile, unit_key(entry.name)), []).append(entry)
    return units


def remove(path, locks):
    if path.exists() or path.is_symlink():
        clear_preserving_locks(path, locks)


def reclaim(targets, max_bytes, to_bytes, *, process_probe=active_processes):
    """Over max_bytes of reclaimable profiles: clear incremental/ first, then the
    least recently used dependency units, until at or under to_bytes."""
    with exclusive(targets, process_probe) as (locks, considered):
        profiles = [p for t in targets for p in reclaimable_profiles(t)]
        before = sum(entry_bytes(p) for p in profiles)
        result = {'measured': True, 'n_considered': considered, 'before_bytes': before,
                  'max_bytes': max_bytes, 'to_bytes': to_bytes,
                  'protected': list(PROTECTED_PROFILES), 'profiles': [str(p) for p in profiles]}
        if before <= max_bytes:
            return {'verdict': 'cargo_reclaim_not_needed', 'after_bytes': before, **result}
        size, cleared = before, 0
        for profile in profiles:
            incremental = profile / 'incremental'
            if incremental.is_dir():
                freed = entry_bytes(incremental)
                remove(incremental, locks)
                size, cleared = size - freed, cleared + freed
        units = sorted(dep_units(profiles).items(),
                       key=lambda kv: max(last_used(p) for p in kv[1]))
        removed = 0
        for _, paths in units:
            if size <= to_bytes:
                break
            freed = sum(entry_bytes(p) for p in paths)
            for path in paths:
                remove(path, locks)
            size, removed = size - freed, removed + 1
        after = sum(entry_bytes(p) for p in profiles)
        return {'verdict': 'cargo_reclaimed', 'after_bytes': after, 'incremental_bytes': cleared,
                'units_removed': removed, 'reached_goal': after <= to_bytes, **result}


def prioritized_reclaim(targets, max_bytes, to_bytes, *, process_probe=active_processes):
    """reclaim(), which after PRIORITY_AFTER consecutive deferrals on a HELD lock
    marks itself pending and waits, bounded, for the running builds to drain.

    Every attempt is the ordinary reclaim: the same lease, Cargo locks, process
    probe and protected profiles. The wait only stops NEW builds from joining;
    it never breaks or bypasses a lock someone holds. A deferral raised from
    here carries its streak (counted once per invocation) and, when it waited,
    the priority fields for the verdict.
    """
    after = priority_setting('after')
    try:
        return {**reclaim(targets, max_bytes, to_bytes, process_probe=process_probe), 'priority': False}
    except Deferred as error:
        if not error.contended:
            raise
        error.streak = streak = bump_streak(targets, str(error), now=int(time.time()), contended=True)
        remaining = priority_backoff(targets, priority_setting('backoff_s'))
        if remaining:
            error.priority = {'priority': False, 'priority_backoff_remaining_s': round(remaining, 3)}
            raise
        nested = inherited_leases() & set(targets)
        if after <= 0 or not streak or streak['priority_streak'] < after or nested:
            raise
    wait_s, poll_s = priority_setting('wait_s'), priority_setting('poll_s')
    started = time.time()
    write_pending(targets, streak, started)
    print('cargo_reclaim_priority: %d consecutive deferrals on a held lock; marked %s pending, '
          'new builds wait; waiting up to %ds for running builds to finish'
          % (streak['priority_streak'], ', '.join(str(pending_path(t)) for t in targets), wait_s),
          file=sys.stderr)
    try:
        while True:
            try:
                result = reclaim(targets, max_bytes, to_bytes, process_probe=process_probe)
            except Deferred as error:
                waited = time.time() - started
                if waited < wait_s:
                    time.sleep(min(poll_s, wait_s - waited))
                    continue
                error.streak = streak
                record_priority_timeout(targets)
                print('cargo_reclaim_priority_timeout: backing off for %ss'
                      % priority_setting('backoff_s'), file=sys.stderr)
                error.priority = {'priority': True, 'waited_s': round(waited, 3),
                                  'priority_backoff_remaining_s': priority_setting('backoff_s')}
                raise
            return {**result, 'priority': True, 'waited_s': round(time.time() - started, 3),
                    'pending_streak': streak['priority_streak']}
    finally:
        clear_pending(targets)


def sweep(targets, idle_s=SWEEP_IDLE_S, *, now=None, process_probe=active_processes):
    """Remove incremental sessions and dependency units unused for idle_s."""
    now = time.time() if now is None else now
    cutoff = now - idle_s
    with exclusive(targets, process_probe) as (locks, considered):
        profiles = [p for t in targets for p in reclaimable_profiles(t)]
        before = sum(entry_bytes(p) for p in profiles)
        swept = []
        for profile in profiles:
            incremental = profile / 'incremental'
            if incremental.is_dir():
                swept += [[s] for s in incremental.iterdir() if last_used(s) < cutoff]
        # Only units with compiled outputs in deps/: rustc memory-maps those, which
        # keeps their atime current. A build script's out/ dir is read by include!
        # and plain reads do not move atime on this Mac (measured in review), so it
        # would look idle while in use; size-driven reclaim may still take it.
        swept += [paths for paths in dep_units(profiles).values()
                  if any(p.parent.name == 'deps' for p in paths)
                  and max(last_used(p) for p in paths) < cutoff]
        for paths in swept:
            for path in paths:
                remove(path, locks)
        after = sum(entry_bytes(p) for p in profiles)
        return {'verdict': 'cargo_swept', 'measured': True, 'n_considered': considered,
                'before_bytes': before, 'after_bytes': after, 'idle_s': idle_s,
                'units_removed': len(swept), 'protected': list(PROTECTED_PROFILES),
                'profiles': [str(p) for p in profiles]}


def test_probe(targets, mutation_paths):
    """The temporary-fixture process-probe seam, or None. Tests may isolate host
    processes ONLY for throwaway target trees; locks still run, and no real shared
    target can pass these constraints."""
    fixture = os.environ.get('AMUX_CARGO_GUARD_TEST_FIXTURE')
    if not fixture:
        return None
    fixture = Path(fixture).resolve()
    temp_roots = [Path(tempfile.gettempdir()).resolve(), Path('/tmp').resolve()]
    if (os.environ.get('AMUX_RS_DISK_CLEAR_ONLY') != '1'
            or not fixture.name.startswith('amux-cargo-guard-test.')
            or not any(root in fixture.parents for root in temp_roots)
            or not all(fixture in root.parents for root in targets)
            or not all(fixture in path.parents for path in mutation_paths)):
        raise Deferred('invalid temporary fixture process-probe seam')
    return lambda _: ([], 1)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('action', choices=['run', 'clear', 'move', 'purge', 'reclaim', 'sweep'])
    parser.add_argument('--target', action='append', required=True)
    parser.add_argument('--path')
    parser.add_argument('--destination')
    parser.add_argument('--protected-path', action='append', default=[])
    parser.add_argument('--dry-run', action='store_true')
    parser.add_argument('--exclusive-run', action='store_true', help='run a target-mutating command only when no consumer lease is held')
    parser.add_argument('--max-gb', type=float, default=float(os.environ.get('AMUX_CARGO_MAX_TARGET_GB', 40)),
                        help='reclaim: act only when the reclaimable profiles exceed this')
    parser.add_argument('--to-gb', type=float, default=float(os.environ.get('AMUX_CARGO_RECLAIM_TO_GB', 25)),
                        help='reclaim: stop at or under this')
    parser.add_argument('--idle-s', type=int, default=SWEEP_IDLE_S, help='sweep: unused for this long')
    parser.add_argument('--now', type=float, help='sweep: the current time (a test clock)')
    # Parse command separately: argparse REMAINDER would swallow guard options.
    argsv = sys.argv[1:]
    command = []
    if '--' in argsv:
        split = argsv.index('--')
        argsv, command = argsv[:split], argsv[split + 1:]
    args = parser.parse_args(argsv)
    targets = sorted(set(Path(root).resolve() for root in args.target))
    try:
        settings = {key: priority_setting(key) for key in PRIORITY_SETTINGS}
        if settings['wait_s'] > settings['max_age_s']:
            parser.error('AMUX_CARGO_RECLAIM_PRIORITY_WAIT_S must not exceed AMUX_CARGO_RECLAIM_PENDING_MAX_AGE_S')
    except Deferred as error:
        parser.error(str(error))
    try:
        if args.action == 'run':
            if not command:
                raise Deferred('no build command')
            # Acquired inside the systemd scope, and inherited by cargo and its
            # children. No parent polling window between build and test binaries.
            # cargo clean is a mutation too. A shared lease let it delete
            # the next test executable after Cargo released its build lock.
            fds = ([open_lock(lease_path(target)) for target in targets] if args.exclusive_run
                   else acquire_build_leases(targets))
            for fd in fds:
                os.set_inheritable(fd, True)
            os.environ[LEASED_ENV] = os.pathsep.join(
                sorted(str(p) for p in inherited_leases() | set(targets)))
            os.execvp(command[0], command)
        if args.action in ('reclaim', 'sweep'):
            probe = test_probe(targets, []) or active_processes
            result = (prioritized_reclaim(targets, int(args.max_gb * GB), int(args.to_gb * GB), process_probe=probe)
                      if args.action == 'reclaim' else sweep(targets, args.idle_s, now=args.now, process_probe=probe))
            clear_streak(targets)
            print(json.dumps(result))
            return 0
        if not args.path or (args.action == 'move' and not args.destination):
            raise Deferred('missing mutation path or destination')
        mutation_paths = [Path(args.path).resolve(),
                          *[Path(p).resolve() for p in args.protected_path],
                          *([Path(args.destination).resolve()] if args.destination else [])]
        probe = test_probe(targets, mutation_paths) or active_processes
        result = mutate(args.action, Path(args.path),
                        Path(args.destination) if args.destination else None, targets,
                        [Path(p).resolve() for p in args.protected_path], args.dry_run, probe)
        clear_streak(targets)
        print(json.dumps(result))
    except (Deferred, OSError) as error:
        payload = {'verdict': 'cargo_reclaim_deferred', 'measured': getattr(error, 'measured', False),
                   'n_considered': getattr(error, 'considered', 0), 'reason': str(error)}
        # A prioritized reclaim already counted this invocation's deferral.
        streak = (getattr(error, 'streak', None) if args.action == 'run' or hasattr(error, 'streak')
                  else bump_streak(targets, str(error)))
        payload.update(streak or {})
        payload.update(getattr(error, 'priority', None) or {})
        print(json.dumps(payload))
        return 75
    return 0


if __name__ == '__main__':
    sys.exit(main())
