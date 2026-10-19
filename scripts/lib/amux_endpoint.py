"""Isolated-run endpoint guard shared by amux's Python clients and hooks.

Under amux-isolated (AMUX_ISOLATED_RUN=1) a client must never address the live
server. Each client computes its URL as it always has and passes the result
through guard(). The rule matches the PARSED PORT, never a host spelling: an
empty URL, or one whose port (scheme default if absent, leading zeros dropped)
is a live port, becomes a dead port and the refusal is logged. Live ports are
8824, 8822, and every port named by an endpoint.json the server wrote (the
real user's ~/.amux, $AMUX_HOME, $CC_HOME, $HOME/.amux). The server binds
0.0.0.0, so any host on a live port is the live server, which is why the host
is not consulted. Outside an isolated run guard() returns its input unchanged.

2026-10-19: a test that unset AMUX_API/AMUX_URL sent a real request to 8824, and
two reviews then escaped spelling-based checks with LOCALHOST, 0.0.0.0, 127.1,
[::ffff:127.0.0.1], user@host, :08824, the hostname and a non-8824 canonical.

Same rule in scripts/lib/amux-endpoint.sh (shell, and the Bash CLI's copy),
crates/amux-cli (Rust), and the inline fallback each client carries for copies
that run without this file. test-endpoint-clients-isolated.py drives every
implementation over one probe table.
"""
import json
import os
import pwd
import re
import sys

DEAD_URL = "http://127.0.0.1:9"
FIXED_LIVE_PORTS = (8824, 8822)
_DEFAULT_PORTS = {"http": 80, "https": 443, "ws": 80, "wss": 443}


def url_port(url):
    """The port a client would connect to, or None when it cannot be parsed."""
    u = (url or "").strip().lower()
    scheme = ""
    m = re.match(r"([a-z][a-z0-9+.-]*)://", u)
    if m:
        scheme, u = m.group(1), u[m.end():]
    authority = re.split(r"[/?#]", u, 1)[0].rsplit("@", 1)[-1]
    if authority.startswith("["):
        port = authority.partition("]")[2][1:] if "]:" in authority else ""
    else:
        port = authority.rpartition(":")[2] if ":" in authority else ""
    if port.isdigit():
        return int(port)
    return _DEFAULT_PORTS.get(scheme) if port == "" else None


def endpoint_files():
    homes = []
    try:
        homes.append(os.path.join(pwd.getpwuid(os.getuid()).pw_dir, ".amux"))
    except KeyError:
        pass
    for var in ("AMUX_HOME", "CC_HOME"):
        if os.environ.get(var):
            homes.append(os.environ[var])
    if os.environ.get("HOME"):
        homes.append(os.path.join(os.environ["HOME"], ".amux"))
    return [os.path.join(h, "endpoint.json") for h in dict.fromkeys(homes)]


def live_ports():
    ports = set(FIXED_LIVE_PORTS)
    for path in endpoint_files():
        try:
            with open(path) as fh:
                ep = json.load(fh)
        except (OSError, ValueError):
            continue
        for key in ("canonical_port", "legacy_port"):
            if isinstance(ep.get(key), int):
                ports.add(ep[key])
        ports.update(p for p in ep.get("retired_ports") or [] if isinstance(p, int))
        port = url_port(ep.get("canonical_url") or "")
        if port:
            ports.add(port)
    return ports


def isolated():
    return os.environ.get("AMUX_ISOLATED_RUN") == "1"


def guard(url, client):
    """url unchanged, unless an isolated run would reach the live server."""
    if not isolated():
        return url
    if not (url or "").strip():
        reason = "unset_would_use_builtin"
    elif url_port(url) in live_ports():
        reason = "live_port"
    else:
        return url
    print("[amux] verdict=isolated_live_endpoint_refused client=%s from=%s reason=%s "
          "to=%s measured=true n_considered=1" % (client, url or "<unset>", reason, DEAD_URL),
          file=sys.stderr)
    return DEAD_URL
