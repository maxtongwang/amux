#!/usr/bin/env python3
"""Claude PreToolUse guard for AskUserQuestion while a /goal is active (AMUX-5234).

A lane driving a /goal is running because the owner is NOT at the keyboard. An
AskUserQuestion picker parks it until he comes back: gs-4-gke-minimization sat
on one for ~19 minutes on 2026-09-26, then found more levers it could pull
alone. This hook asks the amux server (POST /api/sessions/<lane>/owner-ask)
whether a goal is active. If it is, the server files the question as a
needsyou card and this hook blocks the tool with the server's reason, which
tells the worker to proceed on its recommended option, or, for a question that
touches money, an external send, production data or a foreign push, to leave
that step alone and do other work.

The server owns every decision (kill switch AMUX_OWNER_ASK_STEER, isolation,
goal detection, card dedupe). This script only transports. It FAILS OPEN: any
error, timeout, or answer other than an explicit deny lets the question
through, because a guard that blocks a real question on a transport fault is
worse than the stall it replaces. Every run appends one line to
~/.amux/logs/ask-guard.jsonl with a `verdict`.
"""

from __future__ import annotations

# >>> amux-endpoint-guard (rule: scripts/lib/amux_endpoint.py; every copy identical, test-enforced)
import os, sys  # noqa: E401
_AE_HERE = os.path.dirname(os.path.realpath(__file__))
for _ae_dir in (_AE_HERE, os.path.join(_AE_HERE, "lib"), os.path.join(_AE_HERE, os.pardir, "lib"),
                os.path.join(__import__("pwd").getpwuid(os.getuid()).pw_dir, ".amux", "hooks")):
    if _ae_dir not in sys.path:
        sys.path.append(_ae_dir)
try:
    from amux_endpoint import guard as _endpoint_guard
except ImportError:  # no shared file anywhere: the parsed-port rule on the fixed live ports
    def _endpoint_guard(url, client):
        import re
        u = (url or "").strip().lower()
        a = re.split(r"[/?#]", u.split("://", 1)[-1], 1)[0].rsplit("@", 1)[-1]
        p = a.partition("]")[2] if a.startswith("[") else a
        p = p.rpartition(":")[2] if ":" in p else ""
        if os.environ.get("AMUX_ISOLATED_RUN") == "1" and (not u or (p.isdigit() and int(p) in (8824, 8822))):
            print("[amux] verdict=isolated_live_endpoint_refused client=%s from=%s reason=stand_in "
                  "to=http://127.0.0.1:9 measured=true n_considered=1" % (client, url or "<unset>"),
                  file=sys.stderr)
            return "http://127.0.0.1:9"
        return url
# <<< amux-endpoint-guard

import json
import os
from pathlib import Path
import ssl
import sys
import time
import urllib.request


def env_on(name: str) -> bool:
    return os.environ.get(name, "").strip().lower() in {"1", "true", "yes", "on"}


def audit(verdict: str, **fields) -> None:
    try:
        home = Path(os.environ.get("AMUX_HOME") or Path.home() / ".amux")
        log = home / "logs" / "ask-guard.jsonl"
        log.parent.mkdir(parents=True, exist_ok=True)
        if log.exists() and log.stat().st_size > 2 * 1024 * 1024:
            os.replace(log, log.with_suffix(".jsonl.1"))
        record = {"ts": time.time(), "verdict": verdict,
                  "session": os.environ.get("AMUX_SESSION", ""), **fields}
        with log.open("a", encoding="utf-8") as stream:
            stream.write(json.dumps(record, separators=(",", ":")) + "\n")
    except Exception:
        pass


def base_url() -> str:
    url = os.environ.get("AMUX_URL", "").strip()
    if not url:
        try:
            endpoint = json.loads((Path.home() / ".amux" / "endpoint.json").read_text())
            url = endpoint.get("canonical_url", "")
        except Exception:
            url = ""
    return _endpoint_guard(url.rstrip("/"), "ask-guard")


def main() -> int:
    session = os.environ.get("AMUX_SESSION", "").strip()
    # Only amux-managed, non-isolated workers. The installer wires this into
    # the operator's global Claude settings, so an unrelated Claude process
    # reaches here too and must be left alone.
    if not session or env_on("CC_ISOLATED"):
        return 0
    try:
        data = json.load(sys.stdin)
    except Exception:
        return 0
    if str(data.get("tool_name", "")).split(".")[-1] != "AskUserQuestion":
        return 0
    url = base_url()
    if not url:
        audit("no_endpoint")
        return 0
    body = json.dumps({
        "tool_input": data.get("tool_input") or {},
        "session_id": data.get("session_id") or "",
        "transcript_path": data.get("transcript_path") or "",
    }).encode()
    req = urllib.request.Request(
        f"{url}/api/sessions/{session}/owner-ask", data=body, method="POST",
        headers={"Content-Type": "application/json", "X-Amux-Session": session,
                 # Contract rule 10 (AH-387): the lane's minted identity.
                 **({"X-Amux-Worker-Token": os.environ["AMUX_WORKER_TOKEN"]} if os.environ.get("AMUX_WORKER_TOKEN") else {})})
    try:
        ctx = ssl.create_default_context()
        ctx.check_hostname = False
        ctx.verify_mode = ssl.CERT_NONE
        with urllib.request.urlopen(req, context=ctx, timeout=6) as resp:
            answer = json.load(resp)
    except Exception as exc:
        audit("server_unreachable", error=type(exc).__name__)
        return 0
    if answer.get("decision") != "deny" or not answer.get("reason"):
        audit("allowed", why=answer.get("why", ""))
        return 0
    audit("denied", card=answer.get("card", ""))
    sys.stderr.write(str(answer["reason"]) + "\n")
    return 2


if __name__ == "__main__":
    raise SystemExit(main())
