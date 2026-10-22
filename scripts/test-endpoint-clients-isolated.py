#!/usr/bin/env python3
"""Under amux-isolated, a variable-unsetting run reaches no live amux endpoint.

The rule (scripts/lib/amux_endpoint.py): with AMUX_ISOLATED_RUN=1, FAIL CLOSED.
Only a test URL of one strict shape (TEST_URL_RE) whose port is not live (8824,
8822, any endpoint.json port) is used; anything else becomes the dead port, with
a logged refusal. KNOWN LIMIT: a stand-in (a copy with no shared file anywhere)
knows only 8824/8822, so it misses a live server on a non-default port; it still
refuses every non-test shape. This test:

1. drives every implementation over one probe table: the Python and shell
   libraries, the Bash CLI (`amux url`), and the inline stand-ins (Python,
   shell, JS) that copies carry when the shared file is missing. Rust runs the
   same table in crates/amux-cli's unit tests;
2. requires every copy of a guard block to be identical to its siblings, and
   the CLI's shell block to match the library's;
3. runs the clients themselves with AMUX_URL/AMUX_API unset, endpoint.json naming
   a NON-8824 live port, and an `amux` stub answering it. Every TCP connect in
   this process is refused and recorded, and curl is a recording stub, so no
   case can reach a server;
4. sweeps client code for ANY 8824/8822 literal: each must sit on a guarded line
   or in the commented ALLOWLIST below, whose rows must still match.

Walks the tree with os.walk, not git, so a `git archive` extraction (the
local-CI fallback) runs it too.
2026-10-19: a test that unset AMUX_API/AMUX_URL sent a real request to 8824, and
two reviews then escaped spelling-based checks.
"""
import contextlib
import importlib.machinery
import io
import json
import os
import re
import shutil
import socket
import subprocess
import sys
import tempfile

ROOT = os.path.dirname(os.path.dirname(os.path.realpath(__file__)))
LIB = os.path.join(ROOT, "scripts", "lib")
DEAD = "http://127.0.0.1:9"
FIXTURE_PORT = 9824  # endpoint.json's canonical port: live, but not 8824
LIVE_HIT = re.compile(r"(?i)^[^/]*//([^/@]*@)?(\[[^]]*\]|[^/:?#]*):0*(8824|8822|%d)([/?#]|$)|:0*(8824|8822|%d)$"
                      % (FIXTURE_PORT, FIXTURE_PORT))
PASS = FAIL = 0


def check(label, cond, detail=""):
    global PASS, FAIL
    if cond:
        PASS += 1
        print("  ok   " + label)
    else:
        FAIL += 1
        print("  FAIL " + label + (("\n       " + str(detail)[:400]) if detail else ""))


W = tempfile.mkdtemp(prefix="endpoint-clients.")
HOME = os.path.join(W, "home")
STUB = os.path.join(W, "stub")
CURL_LOG = os.path.join(W, "curl.log")


def setup():
    os.makedirs(os.path.join(HOME, ".amux", "hooks"))
    with open(os.path.join(HOME, ".amux", "endpoint.json"), "w") as fh:
        json.dump({"canonical_port": FIXTURE_PORT, "canonical_url": "https://localhost:%d" % FIXTURE_PORT,
                   "legacy_port": None, "retired_ports": [8822]}, fh)
    os.makedirs(STUB)
    with open(os.path.join(STUB, "curl"), "w") as fh:
        fh.write('#!/usr/bin/env bash\nfor a in "$@"; do case "$a" in *://*) '
                 'printf "%s\\n" "$a" >> "$CURL_LOG" ;; esac; done\nexit 7\n')
    with open(os.path.join(STUB, "amux"), "w") as fh:
        fh.write('#!/usr/bin/env bash\n[ "${1:-}" = url ] && echo "${STUB_AMUX_URL:-https://localhost:%d}"\nexit 0\n' % FIXTURE_PORT)
    for f in ("curl", "amux"):
        os.chmod(os.path.join(STUB, f), 0o755)


def env_for(isolated=True, **extra):
    env = {k: v for k, v in os.environ.items()
           if k not in ("AMUX_URL", "AMUX_API", "AMUX_ISOLATED_RUN", "AMUX_SOAK_URL", "AMUX_HOME", "CC_HOME")}
    env.update(HOME=HOME, AMUX_HOME=os.path.join(HOME, ".amux"), CC_HOME=os.path.join(HOME, ".amux"),
               PATH=STUB + os.pathsep + env.get("PATH", ""), CURL_LOG=CURL_LOG, TMPDIR=W)
    if isolated:
        env["AMUX_ISOLATED_RUN"] = "1"
    env.update(extra)
    return env


# Refuse and record every TCP connect this process makes.
CONNECTS = []


def _refuse(addr):
    CONNECTS.append("%s:%s" % (addr[0], addr[1]))
    raise ConnectionRefusedError("refused by test-endpoint-clients-isolated")


socket.create_connection = lambda addr, *a, **k: _refuse(addr)
socket.socket.connect = lambda self, addr: _refuse(addr)

# ── 1. one probe table, every implementation ─────────────────────────────────
ESCAPES = ["", "  ", "https://LOCALHOST:8824", "HTTPS://localhost:8824", "http://0.0.0.0:8824",
           "https://127.1:8824", "https://127.0.0.2:8824", "https://[0:0:0:0:0:0:0:1]:8824",
           "https://[::ffff:127.0.0.1]:8824", "https://localhost.:8824", "https://user@localhost:8824",
           "https://localhost:08824", "https://%s:8824" % socket.gethostname(), " https://localhost:8824",
           "https://localhost:8824?x", "localhost:8824", "https://[::1]:8822"]
# The final review round's six: spellings a real client (urllib's int(), the
# WHATWG parser) still reads as the live port. Fail closed refuses them as
# "not a test URL", without parsing a port at all.
SIX = ["http://127.0.0.1:+8824", "http://127.0.0.1: 8824", "http://127.0.0.1:88_24",
       "http://127.0.0.1:8824\\x", "http://127.0.0.1:8824\\@localhost:9999", "http://127.0.0.1:88\t24"]
# Shapes the allow-list refuses even with a harmless port.
NOT_SHAPE = ["HTTP://127.0.0.1:41234", "http://127.0.0.1:041234", "http://127.0.0.1:88240",
             "https://cloud.example", "http://127.0.0.1:41234/a b", "ftp://127.0.0.1:41234",
             "http://127.0.0.1:41234\nhttps://localhost:8824", "http://127.0.0.1:41234/\x07",
             "http://127.0.0.1:41234/a@b", "http://127.0.0.1:41234/a\\b",
             " http://127.0.0.1:41234", "http://127.0.0.1:41234 "]
ESCAPES = ESCAPES + SIX + NOT_SHAPE
KEPT = ["http://127.0.0.1:41234", "https://localhost:65535/x?y=1", "http://[::1]:9", "file:///tmp/health.json",
        "http://127.0.0.1:41234/a-b_c.txt/sub?x=1&y=2#ok"]
ENDPOINT_PORT = ["https://localhost:%d" % FIXTURE_PORT, "http://0.0.0.0:0%d/x" % FIXTURE_PORT]


def block(path, marker):
    text = open(path).read()
    m = re.search(r"(?m)^[ \t]*(?:#|//) >>> %s.*?^[ \t]*(?:#|//) <<< %s" % (marker, marker), text, re.S)
    return m.group(0) if m else None


def py_stand_in():
    src = block(os.path.join(ROOT, "scripts", "frustrations_audit.py"), "amux-endpoint-guard")
    body = src[src.index("    def _endpoint_guard"):src.index("# <<<")]
    ns = {"os": os, "sys": sys}
    exec(compile("\n".join(l[4:] for l in body.splitlines()), "py-stand-in", "exec"), ns)
    return ns["_endpoint_guard"]


def sh_run(script, url, **extra):
    p = subprocess.run(["sh", "-c", script, "sh", url], env=env_for(**extra), capture_output=True, text=True)
    return p.stdout.strip(), p.stderr


SH_STAND_IN = None


def implementations():
    sys.path.insert(0, LIB)
    import amux_endpoint
    sh_lib = '. "%s"; amux_endpoint_guard "$1" t' % os.path.join(LIB, "amux-endpoint.sh")
    sh_blk = block(os.path.join(ROOT, "scripts", "git-hooks", "post-commit"), "amux-endpoint-guard")
    stand_in_sh = sh_blk.split("command -v amux_endpoint_guard >/dev/null 2>&1 || ", 1)[1]
    stand_in_sh = stand_in_sh[:stand_in_sh.rindex("# <<<")] + '\namux_endpoint_guard "$1" t'
    js_blk = block(os.path.join(ROOT, "skills", "chrome-cdp", "scripts", "cdp.mjs"), "amux-endpoint-guard")
    js_file = os.path.join(W, "guard.mjs")
    with open(js_file, "w") as fh:
        fh.write(js_blk + "\nprocess.stdout.write(String(amuxEndpointGuard(process.argv[2], 't')));\n")
    stand_in = py_stand_in()

    def py(url, full=True):
        saved = dict(os.environ)
        os.environ.clear()
        os.environ.update(env_for())
        try:
            with contextlib.redirect_stderr(io.StringIO()):
                return (amux_endpoint.guard if full else stand_in)(url, "t")
        finally:
            os.environ.clear()
            os.environ.update(saved)

    def cli(url):
        out = subprocess.run(["bash", os.path.join(ROOT, "amux"), "url"], env=env_for(AMUX_URL=url),
                             capture_output=True, text=True).stdout.strip()
        return out

    def js(url):
        if not shutil.which("node"):
            return None
        return subprocess.run(["node", js_file, url], env=env_for(), capture_output=True, text=True).stdout

    # (name, function, knows endpoint.json ports)
    return [("py-lib", lambda u: py(u), True), ("sh-lib", lambda u: sh_run(sh_lib, u)[0], True),
            ("bash-cli", cli, True), ("py-stand-in", lambda u: py(u, False), False),
            ("sh-stand-in", lambda u: sh_run(stand_in_sh, u)[0], False), ("js", js, True)]


def run_table():
    print("probe table (isolated):")
    for name, fn, knows_ep in implementations():
        bad = []
        for u in ESCAPES + (ENDPOINT_PORT if knows_ep else []):
            got = fn(u)
            if got is None:
                break
            if got != DEAD:
                bad.append("%r -> %r" % (u, got))
        for u in KEPT:
            got = fn(u)
            if got is None:
                break
            want = u
            if got != want:
                bad.append("kept %r -> %r" % (u, got))
        if fn(KEPT[0]) is None:
            print("  skip %s: node not installed" % name)
            continue
        n = len(ESCAPES) + (len(ENDPOINT_PORT) if knows_ep else 0)
        check("%s refuses all %d non-test or live URLs and keeps the %d clean test URLs" % (name, n, len(KEPT)),
              not bad, "; ".join(bad))


# ── 2. every copy of a block is identical ────────────────────────────────────
SKIP_DIRS = {".git", "node_modules", "target", "__pycache__", "dist", ".next", "fixtures", "tests"}


def walk(roots):
    for r in roots:
        top = os.path.join(ROOT, r)
        if os.path.isfile(top):
            yield r
            continue
        for d, dirs, files in os.walk(top):
            dirs[:] = [x for x in dirs if x not in SKIP_DIRS]
            for f in files:
                yield os.path.relpath(os.path.join(d, f), ROOT)


def normalise_ts(text):
    return (text.replace(": unknown", "").replace(": string", "").replace(": any", "")
            .replace("(process as any)", "process"))


def run_identity():
    print("block identity:")
    copies = {}
    for rel in walk(["scripts", "skills", "integrations", "business-ui", ".claude", "install.sh"]):
        try:
            text = open(os.path.join(ROOT, rel), errors="replace").read()
        except OSError:
            continue
        if ">>> amux-endpoint-guard" not in text or os.path.basename(rel).startswith(("test-", "test_")):
            continue
        b = block(os.path.join(ROOT, rel), "amux-endpoint-guard")
        if b is None:
            check("%s carries a complete guard block" % rel, False)
            continue
        lang = "js" if b.lstrip().startswith("//") else ("py" if "import os, sys" in b else "sh")
        copies.setdefault(lang, []).append((rel, normalise_ts(b) if lang == "js" else b))
    for lang, rows in sorted(copies.items()):
        first = rows[0][1]
        differ = [rel for rel, b in rows if b != first]
        check("%d %s copies of the client block are identical" % (len(rows), lang), not differ,
              "differ from %s: %s" % (rows[0][0], differ))
    check("the sweep found every language's copies (it can fail)",
          len(copies.get("py", [])) >= 9 and len(copies.get("sh", [])) >= 12 and len(copies.get("js", [])) >= 3,
          {k: len(v) for k, v in copies.items()})
    cli = block(os.path.join(ROOT, "amux"), "amux-endpoint-sh")
    lib = block(os.path.join(LIB, "amux-endpoint.sh"), "amux-endpoint-sh")
    check("the Bash CLI's guard block is byte-identical to scripts/lib/amux-endpoint.sh", cli and cli == lib)


# ── 3. the clients themselves ────────────────────────────────────────────────
def load(rel, name):
    return importlib.machinery.SourceFileLoader(name, os.path.join(ROOT, rel)).load_module()


def run_python_client(rel, attr, isolated=True):
    saved = dict(os.environ)
    os.environ.clear()
    os.environ.update(env_for(isolated))
    del CONNECTS[:]
    err = io.StringIO()
    result = None
    try:
        with contextlib.redirect_stderr(err), contextlib.redirect_stdout(io.StringIO()):
            mod = load(rel, "client_" + re.sub(r"\W", "_", rel))
            try:
                value = getattr(mod, attr)
                result = value() if callable(value) else value
            except Exception as e:  # a refused connect surfaces as an error: expected
                result = "raised %s" % type(e).__name__
    finally:
        os.environ.clear()
        os.environ.update(saved)
    urls = [result] if isinstance(result, str) and "://" in result else []
    return urls, list(CONNECTS), err.getvalue()


PY_CLIENTS = [  # (file, function or module attribute that resolves or uses the URL)
    ("scripts/autofix-recovery-sweep.py", "base_url"),
    ("scripts/disk-alarm.py", "leads"),
    ("scripts/frustrations-archive.py", "_api"),
    ("scripts/frustrations_audit.py", "fetch_board"),
    ("scripts/frustrations_audit.py", "fetch_sessions"),
    ("scripts/git-hooks/amux-staged-guard", "amux_base_url"),
    ("scripts/git-hooks/git-shared-guard.py", "amux_base_url"),
    ("scripts/watchdog.py", "HEALTH_URL"),
]


def run_clients():
    print("python clients (isolated, AMUX_URL/AMUX_API unset, endpoint.json on %d):" % FIXTURE_PORT)
    for rel, attr in PY_CLIENTS:
        urls, conns, err = run_python_client(rel, attr)
        hits = [x for x in urls + conns if LIVE_HIT.search(x)]
        name = "%s %s" % (os.path.basename(rel), attr)
        check(name + ": no live endpoint", not hits, "hits=%s" % hits)
        reached_dead = any(u.startswith(DEAD) for u in urls) or "127.0.0.1:9" in conns
        check(name + ": resolves to the dead port, refusal logged",
              reached_dead and "verdict=isolated_live_endpoint_refused" in err,
              "urls=%s conns=%s err=%s" % (urls, conns, err.strip()[-300:]))
    urls, _, _ = run_python_client("scripts/git-hooks/amux-staged-guard", "amux_base_url", isolated=False)
    check("control: a normal run resolves a live URL", any(LIVE_HIT.search(u) for u in urls), urls)

    print("shell clients (isolated, AMUX_URL/AMUX_API unset):")
    repo = os.path.join(W, "repo")
    subprocess.run(["git", "init", "-q", repo], check=True)
    hooks = os.path.join(repo, ".git", "hooks")
    for src, dst in (("scripts/git-hooks/post-commit", "post-commit"),
                     ("scripts/lib/amux-endpoint.sh", "amux-endpoint.sh")):
        shutil.copy(os.path.join(ROOT, src), os.path.join(hooks, dst))
    os.chmod(os.path.join(hooks, "post-commit"), 0o755)
    for label, keep_lib in (("post-commit (installed beside its guard)", True),
                            ("post-commit (copy without the guard: stand-in)", False)):
        if not keep_lib:
            os.remove(os.path.join(hooks, "amux-endpoint.sh"))
        open(CURL_LOG, "w").close()
        p = subprocess.run(["sh", os.path.join(hooks, "post-commit")], cwd=repo,
                           env=env_for(AMUX_SESSION="probe"), capture_output=True, text=True)
        subprocess.run(["git", "-C", repo, "-c", "user.email=t@t", "-c", "user.name=t", "-c",
                        "core.hooksPath=/dev/null", "commit", "-q", "--allow-empty", "-m", "x"], check=True)
        p = subprocess.run(["sh", os.path.join(hooks, "post-commit")], cwd=repo,
                           env=env_for(AMUX_SESSION="probe"), capture_output=True, text=True)
        curls = open(CURL_LOG).read()
        check(label + ": posts only to the dead port, refusal logged",
              DEAD in curls and not LIVE_HIT.search(curls) and "isolated_live_endpoint_refused" in p.stderr,
              "curl=%r err=%r" % (curls, p.stderr[-200:]))
    for rel, client in (("scripts/soak.sh", "soak"), ("scripts/amux-start-worker.sh", "amux-start-worker"),
                        ("skills/amux-worker/scripts/amux-worker.sh", "amux-worker")):
        open(CURL_LOG, "w").close()
        try:
            p = subprocess.run(["bash", os.path.join(ROOT, rel), "--help"] if "amux-worker" in rel
                               else ["bash", os.path.join(ROOT, rel)],
                               # a skill script runs outside the repo: its stand-in knows the fixed ports
                               env=env_for(STUB_AMUX_URL="https://LOCALHOST:08824"),
                               capture_output=True, text=True, timeout=2)
            err = p.stderr
        except subprocess.TimeoutExpired as e:
            err = e.stderr.decode() if isinstance(e.stderr, bytes) else (e.stderr or "")
        curls = open(CURL_LOG).read()
        check("%s: no live endpoint requested, refusal logged" % rel,
              not LIVE_HIT.search(curls) and "client=%s " % client in err and "isolated_live_endpoint_refused" in err,
              "curl=%r err=%r" % (curls, err[-300:]))


# ── 4. any 8824/8822 literal in client code is guarded or allowlisted ────────
NOT_CLIENT_FILES = {  # whole files that send nothing, with the reason
    "scripts/lib/amux_endpoint.py": "the guard itself",
    "scripts/lib/amux-endpoint.sh": "the guard itself",
    "scripts/seo-data.js": "marketing page text quoting `amux serve`'s address",
    "scripts/generate-seo-pages.js": "marketing page generator; the address is page text",
    "scripts/amux-xvfb.service.template": "systemd kiosk unit that opens the dashboard in a browser; no test runs it",
    "scripts/ci-affected.mjs": "a CI path filter naming legacy_port_guard's own pattern",
}
ALLOWLIST = [  # (file, substring of the line, reason): a literal that is not a request target
    ("scripts/watchdog.py", "AMUX_PORT = int(", "integer default; HEALTH_URL built from it is guarded two lines down"),
    ("scripts/frustrations_audit.py", 'endswith(":8822")', "retired-port test on the env URL; both branches are guarded"),
    ("scripts/frustrations_retire.py", "return 'https://localhost:8824'", "inside _SUPERSEDED_ORIGINAL, a string after sys.exit(2); never executed"),
    ("scripts/git-hooks/git-shared-guard.py", "predate the 8822 -> 8824 port cutover", "docstring prose about the cutover"),
    ("scripts/git-hooks/git-shared-guard.py", "`AMUX_URL=https://localhost:8822` in a process env", "docstring prose about the cutover"),
    ("scripts/git-hooks/amux-staged-guard", "8822 -> 8824 cutover and carry", "docstring prose about the cutover"),
    ("scripts/git-hooks/amux-staged-guard", "REVERSED in the cloud image (8822 is canonical", "docstring prose about the cloud image"),
    ("scripts/git-hooks/amux-staged-guard", "baked-in `8822 -> 8824` rewrite", "docstring prose about the cloud image"),
    ("skills/amux-worker/scripts/amux-worker.sh", "AMUX_URL      base URL (default:", "the script's --help usage text, printed and never requested"),
    ("integrations/procwarden/procwarden/config.py", '"amux_base": "https://localhost:8824"', "default config value; maintain._amux_post guards every use"),
    ("business-ui/lib/business.ts", ": 'https://localhost:8824/'", "a link the owner's browser opens; this code sends no request"),
    ("Makefile", "PORT      ?= 8824", "`make` run configuration, not a client request"),
    ("install.sh", 'PORT="${AMUX_RS_PORT:-8824}"', "the port the installer configures the server with"),
    ("amux-remote", "e.g. https://100.108.219.90:8824", "--help example of a REMOTE server address"),
]
GUARDED = re.compile(r"_endpoint_guard\(|amux_endpoint_guard |amuxEndpointGuard\(")
LITERAL = re.compile(r"(?<![0-9])0*88(24|22)(?![0-9])")
SCAN = ["scripts", "skills", "integrations", "business-ui", ".claude", "install.sh", "Makefile", "amux-remote"]


def run_sweep():
    print("literal sweep: any 8824/8822 in client code is guarded or allowlisted:")
    used = set()
    unguarded = []
    swept = 0
    for rel in walk(SCAN):
        base = os.path.basename(rel)
        if rel in NOT_CLIENT_FILES or base.startswith(("test-", "test_")) or rel.endswith((".md", ".html", ".json")):
            continue
        try:
            text = open(os.path.join(ROOT, rel), errors="strict").read()
        except (OSError, UnicodeDecodeError):
            continue
        text = re.sub(r"(?ms)^[ \t]*(#|//) >>> amux-endpoint-guard.*?^[ \t]*(#|//) <<< amux-endpoint-guard", "", text)
        hit_file = False
        for n, line in enumerate(text.splitlines(), 1):
            if not LITERAL.search(line) or line.lstrip().startswith(("#", "//", "*")):
                continue
            hit_file = True
            if GUARDED.search(line):
                continue
            row = next((i for i, (f, sub, _) in enumerate(ALLOWLIST) if f == rel and sub in line), None)
            if row is None:
                unguarded.append("%s: %s" % (rel, line.strip()[:120]))
            else:
                used.add(row)
        swept += hit_file
    check("no unguarded 8824/8822 literal in client code", not unguarded, "\n       ".join(unguarded))
    # A client that reads endpoint.json can reach the live server with no literal at all
    # (hook-report's heal, ask-guard's base_url), so every such file must call the guard.
    readers = []
    for rel in walk(SCAN):
        base = os.path.basename(rel)
        if rel in NOT_CLIENT_FILES or base.startswith(("test-", "test_")) or rel.endswith((".md", ".html", ".json")):
            continue
        try:
            text = open(os.path.join(ROOT, rel), errors="strict").read()
        except (OSError, UnicodeDecodeError):
            continue
        if "endpoint.json" in text:
            readers.append(rel)
            if not GUARDED.search(re.sub(r"(?ms)^[ \t]*(#|//) >>> amux-endpoint-guard.*?^[ \t]*(#|//) <<< amux-endpoint-guard", "", text)):
                unguarded.append(rel)
    missing = [r for r in readers if r in unguarded]
    check("every endpoint.json reader calls the guard (%d readers)" % len(readers), not missing and len(readers) >= 8, missing)
    stale = [ALLOWLIST[i][:2] for i in range(len(ALLOWLIST)) if i not in used]
    check("every allowlist row still matches a line (no stale exemptions)", not stale, stale)
    check("every allowlist row has a real reason", all(len(r) > 15 for _, _, r in ALLOWLIST))
    check("the sweep saw client files (it can fail)", swept >= 20, "swept=%d" % swept)


try:
    setup()
    run_table()
    run_identity()
    run_clients()
    run_sweep()
finally:
    shutil.rmtree(W, ignore_errors=True)
print("\n  %d passed, %d failed" % (PASS, FAIL))
sys.exit(1 if FAIL else 0)
