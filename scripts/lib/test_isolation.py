"""The Python half of scripts/lib/test-isolation.sh, for tests that run the
`amux` CLI or tmux through subprocess.

`isolated_env(tmp, **extra)` returns the environment to pass: HOME, CC_HOME and
AMUX_HOME pinned into `tmp`, inherited AMUX_*/TMUX* identity dropped (AMUX_BIN
and AMUX_TEST_* kept), AMUX_API, AMUX_URL and AMUX_HEALTH_URL pointed at a dead
loopback port, tmux on a private server (`tmux_env`), then `extra` applied. It
raises when `tmp` is the real home or inside the real ~/.amux, or when an
endpoint.json is reachable from it: a test that pinned only HOME once created
workers on the live server (2026-10-16), because the CLI reads CC_HOME first
and heals a loopback API to $CC_HOME/endpoint.json.

`tmux_env(tmp, base)` is the tmux half alone, for a test that pins its own
homes; `kill_private_tmux(env)` is the teardown. Never call tmux on the default
socket from a test, not even read-only.
"""
import os
import pwd
import shutil
import subprocess
import tempfile
from pathlib import Path

DEAD_URL = os.environ.get('AMUX_TEST_DEAD_URL', 'http://127.0.0.1:9')


def isolated_env(tmp, **extra):
    tmp = Path(tmp)
    if not tmp.is_dir():
        raise RuntimeError(f'isolated_env: {tmp} is not an existing directory')
    real = Path(pwd.getpwuid(os.getuid()).pw_dir).resolve()
    home = tmp.resolve()
    if home == real or home == real / '.amux' or (real / '.amux') in home.parents:
        raise RuntimeError(f'isolated_env: REFUSING {home}: the real home or inside the real ~/.amux')
    cc = home / '.amux'
    if (cc / 'endpoint.json').exists():
        raise RuntimeError(f'isolated_env: REFUSING: a server endpoint is reachable ({cc / "endpoint.json"})')
    cc.mkdir(exist_ok=True)
    env = {k: v for k, v in os.environ.items()
           if k in ('AMUX_BIN',) or k.startswith('AMUX_TEST_')
           or not (k.startswith('AMUX_') or k == 'TMUX' or k.startswith('TMUX_'))}
    env.update(HOME=str(home), CC_HOME=str(cc), AMUX_HOME=str(cc), AMUX_TEST_ISOLATED='1',
               AMUX_API=DEAD_URL, AMUX_URL=DEAD_URL, AMUX_HEALTH_URL=DEAD_URL + '/health')
    env = tmux_env(home, env)
    env.update({k: str(v) for k, v in extra.items()})
    return env


def _private_tmux_dir(d):
    """A socket directory other than the real user's default (unset, or the system tmp)."""
    if not d:
        return False
    r = Path(d).resolve()
    system = {Path(p).resolve() for p in ('/', '/tmp', '/private/tmp', '/var/tmp', tempfile.gettempdir())}
    return r not in system


def tmux_env(tmp, base=None):
    """TMUX/TMUX_PANE dropped and TMUX_TMPDIR=<tmp>/tmux, so every tmux the
    child runs talks to a private server, never the host's (2026-10-16: a test
    renamed the live amux-dev-lead session). Raises on a directory that would
    be the real user's default socket."""
    env = dict(os.environ if base is None else base)
    env.pop('TMUX', None)
    env.pop('TMUX_PANE', None)
    d = Path(tmp) / 'tmux'
    d.mkdir(parents=True, exist_ok=True)
    if not _private_tmux_dir(d):
        raise RuntimeError(f"tmux_env: REFUSING {d}: the real user's default tmux socket")
    env['TMUX_TMPDIR'] = str(d)
    return env


def kill_private_tmux(env):
    """Kill the private tmux server `env` points at (never the default one)."""
    d = env.get('TMUX_TMPDIR')
    if not _private_tmux_dir(d) or not shutil.which('tmux'):
        return
    clean = {k: v for k, v in env.items() if k not in ('TMUX', 'TMUX_PANE')}
    subprocess.run(['tmux', 'kill-server'], env=clean, capture_output=True, check=False)
