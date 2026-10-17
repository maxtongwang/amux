"""The Python half of scripts/lib/test-isolation.sh, for tests that run the
`amux` CLI through subprocess. `isolated_env(tmp, **extra)` returns the
environment to pass: HOME, CC_HOME and AMUX_HOME pinned into `tmp`, inherited
AMUX_*/TMUX* identity dropped (AMUX_BIN and AMUX_TEST_* kept), AMUX_API,
AMUX_URL and AMUX_HEALTH_URL pointed at a dead loopback port, then `extra`
applied. It raises when `tmp` is the real home or inside the real ~/.amux, or
when an endpoint.json is reachable from it: a test that pinned only HOME once
created workers on the live server (2026-10-16), because the CLI reads CC_HOME
first and heals a loopback API to $CC_HOME/endpoint.json.
"""
import os
import pwd
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
    env.update(HOME=str(home), CC_HOME=str(cc), AMUX_HOME=str(cc),
               AMUX_API=DEAD_URL, AMUX_URL=DEAD_URL, AMUX_HEALTH_URL=DEAD_URL + '/health')
    env.update({k: str(v) for k, v in extra.items()})
    return env
