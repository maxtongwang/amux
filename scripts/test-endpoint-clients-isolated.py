#!/usr/bin/env python3
"""Under amux-isolated, a variable-unsetting run reaches no live amux endpoint.

Runs amux's Python and shell clients with AMUX_ISOLATED_RUN=1, AMUX_URL and
AMUX_API unset, a HOME whose endpoint.json names the live URL, and an `amux`
stub that answers the live URL (as `amux url` did before the CLI guard). Every
TCP connect in this process is refused and recorded, and `curl` is a stub that
records its URLs, so no case can reach a server. Each client must address only
the dead port and log verdict=isolated_live_endpoint_refused.

A static sweep then requires every client under scripts/ that names a loopback
8824 to route it through the guard, so a new client cannot skip it silently.
2026-10-19: a test that unset AMUX_API/AMUX_URL sent a real request to 8824.
"""
import contextlib
import importlib.machinery
import io
import os
import re
import socket
import subprocess
import sys
import tempfile

ROOT = os.path.dirname(os.path.dirname(os.path.realpath(__file__)))
LIB = os.path.join(ROOT, "scripts", "lib")
DEAD = "http://127.0.0.1:9"
LIVE = re.compile(r"(localhost|127\.0\.0\.1|\[::1\]|::1)\D{0,3}(8824|8822)\b")
PASS = FAIL = 0


def check(label, cond, detail=""):
    global PASS, FAIL
    if cond:
        PASS += 1
        print("  ok   " + label)
    else:
        FAIL += 1
        print("  FAIL " + label + (("\n       " + detail) if detail else ""))


W = tempfile.mkdtemp(prefix="endpoint-clients.")
HOME = os.path.join(W, "home")
os.makedirs(os.path.join(HOME, ".amux", "hooks"))
with open(os.path.join(HOME, ".amux", "endpoint.json"), "w") as fh:
    fh.write('{"canonical_port":8824,"canonical_url":"https://localhost:8824",'
             '"legacy_port":8822,"retired_ports":[8822]}\n')
STUB = os.path.join(W, "stub")
os.makedirs(STUB)
CURL_LOG = os.path.join(W, "curl.log")
with open(os.path.join(STUB, "curl"), "w") as fh:
    fh.write('#!/usr/bin/env bash\nfor a in "$@"; do case "$a" in http://*|https://*) '
             'printf "%s\\n" "$a" >> "$CURL_LOG" ;; esac; done\nexit 7\n')
with open(os.path.join(STUB, "amux"), "w") as fh:
    fh.write('#!/usr/bin/env bash\n[ "${1:-}" = url ] && echo https://localhost:8824\nexit 0\n')
for f in ("curl", "amux"):
    os.chmod(os.path.join(STUB, f), 0o755)


def isolated_env(isolated=True):
    env = {k: v for k, v in os.environ.items()
           if k not in ("AMUX_URL", "AMUX_API", "AMUX_ISOLATED_RUN", "AMUX_SOAK_URL")}
    env.update(HOME=HOME, AMUX_HOME=os.path.join(HOME, ".amux"), CC_HOME=os.path.join(HOME, ".amux"),
               PATH=STUB + os.pathsep + env.get("PATH", ""), CURL_LOG=CURL_LOG, TMPDIR=W)
    if isolated:
        env["AMUX_ISOLATED_RUN"] = "1"
    return env


# Refuse and record every TCP connect this process makes.
CONNECTS = []
_real_create = socket.create_connection
_real_connect = socket.socket.connect


def _refuse(addr):
    CONNECTS.append("%s:%s" % (addr[0], addr[1]))
    raise ConnectionRefusedError("refused by test-endpoint-clients-isolated")


socket.create_connection = lambda addr, *a, **k: _refuse(addr)
socket.socket.connect = lambda self, addr: _refuse(addr)


def load(rel, name):
    loader = importlib.machinery.SourceFileLoader(name, os.path.join(ROOT, rel))
    return loader.load_module()


def run_python_client(rel, fn, isolated=True):
    """Load a client and call its URL function; returns (urls, connects, stderr)."""
    saved = dict(os.environ)
    os.environ.clear()
    os.environ.update(isolated_env(isolated))
    del CONNECTS[:]
    err = io.StringIO()
    result = None
    try:
        with contextlib.redirect_stderr(err), contextlib.redirect_stdout(io.StringIO()):
            mod = load(rel, "client_" + re.sub(r"\W", "_", rel))
            try:
                result = getattr(mod, fn)()
            except Exception as e:  # a refused connect surfaces as an error: expected
                result = "raised %s" % type(e).__name__
    finally:
        os.environ.clear()
        os.environ.update(saved)
    urls = [result] if isinstance(result, str) and result.startswith("http") else []
    return urls, list(CONNECTS), err.getvalue()


PY_CLIENTS = [  # (file, function that resolves or uses the URL)
    ("scripts/autofix-recovery-sweep.py", "base_url"),
    ("scripts/disk-alarm.py", "leads"),
    ("scripts/frustrations-archive.py", "_api"),
    ("scripts/frustrations_audit.py", "fetch_board"),
    ("scripts/frustrations_audit.py", "fetch_sessions"),
    ("scripts/git-hooks/amux-staged-guard", "amux_base_url"),
    ("scripts/git-hooks/git-shared-guard.py", "amux_base_url"),
]

print("python clients (isolated, AMUX_URL/AMUX_API unset):")
for rel, fn in PY_CLIENTS:
    urls, conns, err = run_python_client(rel, fn)
    hits = [x for x in urls + conns if LIVE.search(x)]
    check("%s %s(): no live endpoint" % (os.path.basename(rel), fn), not hits, "hits=%s" % hits)
    reached_dead = any(u.startswith(DEAD) for u in urls) or "127.0.0.1:9" in conns
    check("%s %s(): resolves to the dead port, refusal logged" % (os.path.basename(rel), fn),
          reached_dead and "verdict=isolated_live_endpoint_refused" in err,
          "urls=%s conns=%s err=%s" % (urls, conns, err.strip()[-300:]))

# Control: the same client outside an isolated run still resolves the live URL,
# so the probe above can see a live hit when one happens.
urls, conns, _ = run_python_client("scripts/git-hooks/amux-staged-guard", "amux_base_url", isolated=False)
check("control: a normal run resolves the live URL", any(LIVE.search(u) for u in urls), "urls=%s" % urls)


def sh(cmd, cwd=None, timeout=20, isolated=True):
    open(CURL_LOG, "w").close()
    p = subprocess.run(cmd, cwd=cwd, env=isolated_env(isolated), capture_output=True, text=True,
                       timeout=timeout)
    with open(CURL_LOG) as fh:
        return p, fh.read()


print("shell clients (isolated, AMUX_URL/AMUX_API unset):")
# post-commit, installed the way install-hooks.sh installs it: beside its guard.
repo = os.path.join(W, "repo")
subprocess.run(["git", "init", "-q", repo], check=True)
hooks = os.path.join(repo, ".git", "hooks")
for src, dst in (("scripts/git-hooks/post-commit", "post-commit"),
                 ("scripts/lib/amux-endpoint.sh", "amux-endpoint.sh")):
    with open(os.path.join(ROOT, src)) as a, open(os.path.join(hooks, dst), "w") as b:
        b.write(a.read())
os.chmod(os.path.join(hooks, "post-commit"), 0o755)
p, curls = sh(["git", "-c", "user.email=t@t", "-c", "user.name=t", "commit", "-q",
               "--allow-empty", "-m", "probe"], cwd=repo)
p2 = subprocess.run(["sh", os.path.join(hooks, "post-commit")], cwd=repo,
                    env=dict(isolated_env(), AMUX_SESSION="probe"), capture_output=True, text=True)
with open(CURL_LOG) as fh:
    curls = fh.read()
check("post-commit: no live endpoint, posts to the dead port",
      curls and not LIVE.search(curls) and DEAD in curls, "curl=%r" % curls)
check("post-commit: refusal logged", "isolated_live_endpoint_refused" in p2.stderr, p2.stderr[-300:])

# soak.sh and amux-start-worker.sh loop and sleep; the guard runs first, so a
# short timeout is enough to see the refusal and every request it made.
for rel, client in (("scripts/soak.sh", "soak"), ("scripts/amux-start-worker.sh", "amux-start-worker")):
    open(CURL_LOG, "w").close()
    try:
        p = subprocess.run(["bash", os.path.join(ROOT, rel)], env=isolated_env(),
                           capture_output=True, text=True, timeout=2)
        err = p.stderr
    except subprocess.TimeoutExpired as e:
        err = (e.stderr or b"").decode() if isinstance(e.stderr, bytes) else (e.stderr or "")
    with open(CURL_LOG) as fh:
        curls = fh.read()
    check("%s: no live endpoint requested" % os.path.basename(rel), not LIVE.search(curls), curls)
    check("%s: refusal logged for client=%s" % (os.path.basename(rel), client),
          "client=%s " % client in err and "isolated_live_endpoint_refused" in err, err[-300:])

print("shared guard semantics:")
sys.path.insert(0, LIB)
import amux_endpoint  # noqa: E402
os.environ["AMUX_ISOLATED_RUN"] = "1"
for url, want in (("", DEAD), ("https://localhost:8824", DEAD), ("http://127.0.0.1:8824/", DEAD),
                  ("https://[::1]:8824", DEAD), ("https://localhost:8822", DEAD),
                  ("http://127.0.0.1:41234", "http://127.0.0.1:41234"),
                  ("https://cloud.example", "https://cloud.example")):
    with contextlib.redirect_stderr(io.StringIO()):
        got = amux_endpoint.guard(url, "t")
    check("py guard(%r) -> %s" % (url, want), got == want, "got %r" % got)
    p = subprocess.run(["sh", "-c", '. "$1"; amux_endpoint_guard "$2" t', "sh",
                        os.path.join(LIB, "amux-endpoint.sh"), url],
                       env=dict(os.environ), capture_output=True, text=True)
    check("sh guard(%r) -> %s" % (url, want), p.stdout.strip() == want, "got %r" % p.stdout)
del os.environ["AMUX_ISOLATED_RUN"]
check("py guard outside an isolated run is unchanged",
      amux_endpoint.guard("https://localhost:8824", "t") == "https://localhost:8824")

print("static sweep: every client naming a loopback 8824 routes it through the guard:")
NOT_CLIENTS = {  # docs, templates and the guards themselves, which send nothing
    "scripts/lib/amux_endpoint.py", "scripts/lib/amux-endpoint.sh",
    "scripts/seo-data.js", "scripts/generate-seo-pages.js", "scripts/amux-xvfb.service.template",
}
# A shell line that resolves through the (guarded) CLI needs no guard of its own.
VIA_CLI = re.compile(r'amux url|"\$AMUX_BIN" url')
out = subprocess.run(["git", "-C", ROOT, "ls-files", "scripts", "install.sh"],
                     capture_output=True, text=True).stdout.split()
swept = 0
for rel in sorted(out):
    base = os.path.basename(rel)
    if rel in NOT_CLIENTS or base.startswith(("test-", "test_")) or "/tests/" in rel:
        continue
    try:
        text = open(os.path.join(ROOT, rel), errors="replace").read()
    except (IsADirectoryError, FileNotFoundError):
        continue
    sites = [ln for ln in text.splitlines()
             if re.search(r"(localhost|127\.0\.0\.1):88(24|22)", ln)
             and not ln.lstrip().startswith("#")]
    unguarded = [ln for ln in sites if not VIA_CLI.search(ln)]
    if not sites:
        continue
    swept += 1
    ok = not unguarded or "_endpoint_guard(" in text or "amux_endpoint_guard " in text
    check("%s: its %d live-URL site(s) are guarded" % (rel, len(sites)), ok,
          "unguarded: " + "; ".join(u.strip()[:120] for u in unguarded[:3]))
check("the sweep saw clients (it can fail)", swept >= 15, "swept=%d" % swept)

print("\n  %d passed, %d failed" % (PASS, FAIL))
subprocess.run(["rm", "-rf", "--", W])
sys.exit(1 if FAIL else 0)
