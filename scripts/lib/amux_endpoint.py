"""Isolated-run endpoint guard shared by amux's Python clients and hooks.

Under amux-isolated (AMUX_ISOLATED_RUN=1) a client must never address the live
server. Each client computes its URL as it always has (AMUX_URL, endpoint.json
heal, built-in https://localhost:8824) and passes the result through guard():
an empty URL or one naming a loopback 8824/8822 becomes a dead port, and the
refusal is logged. Outside an isolated run guard() returns its input unchanged.
Same rule as the Bash CLI's _amux_isolated_guard and scripts/lib/amux-endpoint.sh.
2026-10-19: a test that unset AMUX_API/AMUX_URL sent a real request to 8824.

Installed copies of hooks get this file beside them (scripts/install-hooks.sh).
"""
import os
import sys
from urllib.parse import urlsplit

DEAD_URL = "http://127.0.0.1:9"
LIVE_PORTS = (8824, 8822)
LOOPBACK = ("localhost", "127.0.0.1", "::1")


def is_live(url):
    """True when url names this host's live (8824) or retired (8822) amux port."""
    try:
        sp = urlsplit((url or "").strip())
        return sp.hostname in LOOPBACK and sp.port in LIVE_PORTS
    except ValueError:
        return False


def isolated():
    return os.environ.get("AMUX_ISOLATED_RUN") == "1"


def guard(url, client):
    """url unchanged, unless an isolated run would reach the live server."""
    if not isolated():
        return url
    if url and not is_live(url):
        return url
    reason = "live_endpoint" if url else "unset_would_use_builtin"
    print("[amux] verdict=isolated_live_endpoint_refused client=%s from=%s reason=%s "
          "to=%s measured=true n_considered=1" % (client, url or "<unset>", reason, DEAD_URL),
          file=sys.stderr)
    return DEAD_URL
