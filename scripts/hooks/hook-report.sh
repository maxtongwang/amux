#!/bin/bash
# amux report hook (AMUX-2829). Reads Claude Code's hook payload on STDIN,
# extracts the transcript path, and reports REAL context usage with the state.
#
# WHY THIS EXISTS: the hooks used to POST a fixed body with no tokens, so the
# server never knew any lane's context size. Nothing could emit ContextLow, so
# orchestrator/compaction.rs's policy was NEVER CALLED and lanes ran to the wall
# and stopped. Ethan, 2026-08-10: "theres no reason amux should ever stop."
#
# Fails open in every direction: no stdin, no transcript, no python -> report
# the state anyway with tokens omitted. A liveness report is worth more than a
# token count, and this must never be the reason a hook fails.
# MODE is either a main-turn state or one of the two event-driven subagent
# lifecycle modes. Claude Code exposes SubagentStart/SubagentStop directly;
# routing both through this one canonical reporter keeps attribution,
# conversation adoption, endpoint resolution and failure logging identical.
#
# ATE-45: lifecycle delivery is a durable FIFO, and main-turn state is a
# durable latest-wins row. A server rebuild used to turn either into a one-shot
# http=000 log line, leaving the authoritative state false until another hook
# happened or its trust window expired. The detached drain below survives the
# hook invocation and retries the same row after response loss. Lifecycle edges
# keep order; a newer main state replaces the older row so replayed idle can
# never overwrite a later active turn. The hook still always exits zero: amux
# must never block the model.
# NATIVE ARCH (MO-3622). This script runs on every tool call of every lane
# (about 8 times a second across the fleet) and forks ~20 processes per run.
# On an Apple Silicon Mac whose tmux server is an Intel build, the whole pane
# tree inherits x86_64 and runs under Rosetta, where each of those forks costs
# about 12x the CPU of a native one (measured 2026-09-26: 400 spawns = 7.7 CPU-s
# translated, 0.65 native; this script 0.6 s CPU and 0.7 s wall per call
# translated, 0.14 s and 0.17 s native). Re-exec ONCE under `arch -arm64
# -x86_64`: native wherever a native slice exists, unchanged where it does not
# (an Intel Mac, an Intel-only bash). stdin and argv pass through untouched, so
# the payload the harness pipes in still reaches the `cat` below. `execfail`
# keeps a failed exec from ending a script whose contract is to never be the
# reason a hook fails; the guard variable stops a loop. AMUX_NATIVE_ARCH=0 opts
# out. The bash builtins $OSTYPE/$HOSTTYPE decide, so a native run pays nothing.
case "$OSTYPE:$HOSTTYPE" in
  darwin*:x86_64)
    if [ "${AMUX_NATIVE_ARCH:-1}" != 0 ] && [ -z "${_AMUX_NATIVE_REEXEC:-}" ] && [ -x /usr/bin/arch ]; then
      export _AMUX_NATIVE_REEXEC=1
      shopt -s execfail
      exec /usr/bin/arch -arm64 -x86_64 "$BASH" "$0" "$@"
    fi
    ;;
esac
if [ "${1:-}" = "--drain-subagents" ]; then
  /usr/bin/python3 - "${2:-}" "${3:-}" "${4:-}" "${5:-lifecycle_queue}" <<'PY'
import fcntl,json,os,ssl,sys,tempfile,time,urllib.error,urllib.request
queue,url,session,queue_kind=sys.argv[1:5]
if not queue or not url or not session: raise SystemExit(0)
if queue_kind not in ("lifecycle_queue","state_queue"): queue_kind="lifecycle_queue"
lock=queue+".drain.lock"
os.makedirs(os.path.dirname(queue),mode=0o700,exist_ok=True)
lf=open(lock,"a+")
# A replacement drain can start while the prior drain is between its final
# empty read and releasing this lock. Wait briefly for that handoff instead of
# losing the only wakeup for a final SubagentStop. A drain retrying a real
# outage holds the lock far longer than this bound, so ordinary hooks do not
# accumulate an unbounded pile of waiting processes.
lock_deadline=time.monotonic()+3.0
while True:
    try:
        fcntl.flock(lf,fcntl.LOCK_EX|fcntl.LOCK_NB)
        break
    except BlockingIOError:
        if time.monotonic() >= lock_deadline: raise SystemExit(0)
        time.sleep(.05)

def corrupt_note(preserved,exc):
    path=os.path.expanduser("~/.amux/logs/hook-report-failures.log")
    try:
        os.makedirs(os.path.dirname(path),exist_ok=True)
        with open(path,"a") as stream:
            stream.write(time.strftime("%Y-%m-%dT%H:%M:%SZ",time.gmtime())+
                f" {session} {queue_kind}=corrupt verdict=preserved_corrupt_queue "+
                f"queue={queue} preserved={preserved} error={type(exc).__name__}\n")
    except Exception: pass

def locked_rows(change=None,release_on_empty=False):
    qlock=queue+".lock"
    with open(qlock,"a+") as guard:
        fcntl.flock(guard,fcntl.LOCK_EX)
        try:
            with open(queue) as stream: rows=json.load(stream)
            if not isinstance(rows,list) or any(
                not isinstance(row,dict) or not isinstance(row.get("body"),dict)
                for row in rows
            ):
                raise ValueError("invalid lifecycle queue schema")
        except FileNotFoundError:
            rows=[]
        except Exception as exc:
            preserved=queue+f".corrupt.{time.time_ns()}"
            try: os.replace(queue,preserved)
            except FileNotFoundError: preserved="missing-before-preserve"
            except Exception as move_exc:
                preserved=queue+f":preserve_failed:{type(move_exc).__name__}"
            corrupt_note(preserved,exc)
            rows=[]
        import glob
        spooled=sorted(glob.glob(queue+".spool/*.json"))
        for path in spooled:
            with open(path) as stream: pending=json.load(stream)
            if not any(r.get("event_id")==pending["event_id"] for r in rows):
                rows.append(pending)
        if spooled:
            rows.sort(key=lambda row: row.get("queued_ns",0))
            if queue_kind == "state_queue": rows=rows[-1:]
        if change is not None or spooled:
            if change is not None: rows=change(rows)
            fd,tmp=tempfile.mkstemp(prefix="queue.",dir=os.path.dirname(queue))
            try:
                os.fchmod(fd,0o600)
                with os.fdopen(fd,"w") as stream:
                    json.dump(rows,stream,separators=(",",":")); stream.write("\n")
                    stream.flush(); os.fsync(stream.fileno())
                os.replace(tmp,queue)
                for path in spooled: os.unlink(path)
            except BaseException:
                try: os.unlink(tmp)
                except FileNotFoundError: pass
                raise
        # Close the enqueue-vs-exit race: release the drain ownership while
        # still holding the queue lock after the definitive empty read. A
        # producer cannot append and launch its replacement until this drain
        # lock is available to that replacement.
        if release_on_empty and not rows:
            fcntl.flock(lf,fcntl.LOCK_UN)
        return rows

def note(kind,row,code,attempt):
    path=os.path.expanduser("~/.amux/logs/hook-report-failures.log")
    try:
        os.makedirs(os.path.dirname(path),exist_ok=True)
        with open(path,"a") as stream:
            verdict=("non_retryable_http" if kind == "dead_letter" else
                     "replayed_state" if kind == "delivered" else "retryable")
            stream.write(time.strftime("%Y-%m-%dT%H:%M:%SZ",time.gmtime())+
                f" {session} source={row.get('body',{}).get('source','subagent-hook')} url={url} "+
                f"http={code} {queue_kind}={kind} attempt={attempt} verdict={verdict} "+
                f"event_id={row.get('event_id','')} "+
                f"lifecycle_session={row.get('body',{}).get('session_id','')} "+
                f"agent_id={row.get('body',{}).get('agent_id','')} "+
                f"event={row.get('body',{}).get('subagent','')} "+
                f"state={row.get('body',{}).get('state','')}\n")
    except Exception: pass

ctx=ssl._create_unverified_context()
retry_failures=0
while True:
    rows=locked_rows(release_on_empty=True)
    if not rows: raise SystemExit(0)
    row=rows[0]
    attempt=max(0,int(row.get("attempts",0)))+1
    body=dict(row.get("body") or {})
    body["delivery_attempt"]=attempt
    code="000"
    try:
        # The first ATE-45 build persisted the server root in each queue row.
        # A queue surviving that build must heal under the corrected hook, so
        # stale row metadata never overrides this invocation's canonical route.
        req=urllib.request.Request(url,
            data=json.dumps(body,separators=(",",":")).encode(),method="POST",
            headers={"Content-Type":"application/json","X-Amux-Session":session,
                     **({"X-Amux-Worker-Token":os.environ["AMUX_WORKER_TOKEN"]} if os.environ.get("AMUX_WORKER_TOKEN") else {})})
        with urllib.request.urlopen(req,timeout=3,context=ctx) as response:
            code=str(response.status)
            response.read()
    except urllib.error.HTTPError as exc:
        code=str(exc.code)
    except Exception:
        code="000"
    if code.startswith("2"):
        event_id=row.get("event_id","")
        def delivered(current):
            if current and current[0].get("event_id","")==event_id: return current[1:]
            return current
        locked_rows(delivered)
        if queue_kind == "state_queue" and attempt > 1:
            note("delivered",row,code,attempt)
        retry_failures=0
        continue
    retryable_4xx={"408","409","425","429"}
    if code.startswith("4") and code not in retryable_4xx:
        note("dead_letter",row,code,attempt)
        event_id=row.get("event_id","")
        def dead_letter(current):
            if current and current[0].get("event_id","")==event_id: return current[1:]
            return current
        locked_rows(dead_letter)
        retry_failures=0
        continue
    def failed(current):
        if current and current[0].get("event_id","")==row.get("event_id",""):
            current[0]["attempts"]=attempt
        return current
    locked_rows(failed)
    if attempt in (1,5,15,45,90): note("retrying",row,code,attempt)
    retry_failures+=1
    if retry_failures >= 90: raise SystemExit(0)
    time.sleep(2)
PY
  exit 0
fi
MODE="${1:-idle}"; SRC="${2:-stop-hook}"
DERIVED=0
# Outside a worker, an unqualified tmux display-message names the last selected
# session, not this process. Never let an intake/helper Claude hook mark an
# unrelated Codex project executor busy or adopt its conversation identity.
if [ -z "$AMUX_SESSION" ] && [ -n "${TMUX_PANE:-}" ]; then
  # MR-43: the var can go missing INSIDE a lane that IS running in its
  # amux-launched pane (spawn always injects it — session_verbs.rs — so this
  # is loss in-process, not absence at launch). Recover it from tmux so the
  # lane is not invisible to its own liveness report, and flag the recovery
  # in the body so /api/logs/analyze can count how often this happens instead
  # of a human noticing a lane that silently never reported.
  TNAME=$(tmux display-message -p -t "$TMUX_PANE" '#S' 2>/dev/null)
  case "$TNAME" in
    amux-*) export AMUX_SESSION="${TNAME#amux-}"; DERIVED=1 ;;
  esac
fi
# AMUX-4033: a STALE $AMUX_SESSION, which the empty check above cannot see.
#
# Renaming a running worker moves its env file and renames its tmux session,
# then re-exports AMUX_SESSION into the tmux SESSION environment — and that only
# reaches panes started afterwards. The agent already running keeps the OLD name
# for its entire life, so every report it sends names a session that no longer
# exists. Measured 2026-09-02, one minute after leadership-coaching became
# leadership-coach: `leadership-coaching source=prompt-hook http=404` and
# `source=stop-hook http=404`, while tmux itself already read leadership-coach.
# The worker looked renamed and was quietly reporting nothing.
#
# The correction is NARROW on purpose: only when the claimed name has NO session
# file AND the pane's own name HAS one. That covers the rename and the typo this
# file's own POST block describes (4h15m as `amax-gtm`, 138 reports, every one a
# 404, zero 200s). It cannot capture a deliberate cross-session claim, because
# there the claimed session exists and this leaves it alone.
CORRECTED=0
if [ -n "$AMUX_SESSION" ] && [ -n "${TMUX_PANE:-}" ] && [ ! -f "$HOME/.amux/sessions/$AMUX_SESSION.env" ]; then
  _TN=$(tmux display-message -p -t "$TMUX_PANE" '#S' 2>/dev/null)
  case "$_TN" in
    amux-*)
      _TRUE="${_TN#amux-}"
      if [ "$_TRUE" != "$AMUX_SESSION" ] && [ -f "$HOME/.amux/sessions/$_TRUE.env" ]; then
        STALE_FROM="$AMUX_SESSION"
        export AMUX_SESSION="$_TRUE"
        CORRECTED=1
      fi
      ;;
  esac
fi
[ -n "$AMUX_SESSION" ] || exit 0
# ISOLATED WORKERS (AMUX-3232). An isolated worker has AMUX_SESSION stripped at
# spawn, so DERIVED=1 is the discriminator. But "derived" also covers a real lane
# that lost its env var mid-run (the original MR-43 case). The two are separated
# by CC_ISOLATED in the session file: an isolated worker set it intentionally, a
# real lane that lost its var did not. Skip reporting for isolated workers only —
# a real lane that derived its session still gets a liveness report.
if [ "$DERIVED" = "1" ]; then
  _SF="$HOME/.amux/sessions/$AMUX_SESSION.env"
  if grep -qE '^CC_ISOLATED="?1"?' "$_SF" 2>/dev/null; then
    exit 0
  fi
fi
# >>> amux-endpoint-guard (rule: scripts/lib/amux-endpoint.sh; every copy identical, test-enforced)
_ae_dir=$(dirname "$0")
for _ae in "$_ae_dir/amux-endpoint.sh" "$_ae_dir/lib/amux-endpoint.sh" "$_ae_dir/../lib/amux-endpoint.sh" \
           "$(eval echo "~$(id -un 2>/dev/null)" 2>/dev/null)/.amux/hooks/amux-endpoint.sh"; do
  [ -r "$_ae" ] && . "$_ae" && break
done
command -v amux_endpoint_guard >/dev/null 2>&1 || amux_endpoint_guard() {  # stand-in: same shape, fixed live ports
  _aeg_bad=""
  if [ "${AMUX_ISOLATED_RUN:-}" = 1 ]; then
    if [ "$(printf '%s' "$1" | wc -l | tr -d ' ')" != 0 ] || ! printf '%s\n' "$1" | LC_ALL=C grep -Eq \
        '^(https?://(127\.0\.0\.1|localhost|\[::1\]):[1-9][0-9]{0,4}(/[^[:space:][:cntrl:]\\@]*)?|file:///[^[:space:][:cntrl:]\\@]*)$'; then
      _aeg_bad=1
    else
      case "$1" in file:*) ;; *)
        _aeg_p=${1#*://}; _aeg_p=${_aeg_p%%/*}; _aeg_p=${_aeg_p##*:}
        if [ "$_aeg_p" -gt 65535 ] || [ "$_aeg_p" = 8824 ] || [ "$_aeg_p" = 8822 ]; then _aeg_bad=1; fi ;;
      esac
    fi
  fi
  if [ -n "$_aeg_bad" ]; then
    printf '[amux] verdict=isolated_live_endpoint_refused client=%s from=%s reason=stand_in to=http://127.0.0.1:9 measured=true n_considered=1\n' "$2" "${1:-<unset>}" >&2
    echo "http://127.0.0.1:9"
  else
    printf '%s\n' "$1"
  fi
}
# <<< amux-endpoint-guard
IN=""; IFS= read -r -d '' IN || true
E="$HOME/.amux/endpoint.json"
C=$(sed -n 's/.*"canonical_url":"\([^"]*\)".*/\1/p' "$E" 2>/dev/null)
L=$(sed -n 's/.*"legacy_port":\([0-9]*\).*/\1/p' "$E" 2>/dev/null)
U="${AMUX_URL:-$C}"
case "$U" in *localhost:$L|*127.0.0.1:$L) U="${C:-$U}";; esac
# Any other localhost port endpoint.json does not name is stale too (a reboot
# race left every lane on 8823, 2026-10-05); the CLI heals and logs the same.
case "$U" in *://localhost:*|*://127.0.0.1:*) [ -n "$C" ] && U="$C";; esac
U=$(amux_endpoint_guard "${U%/}" hook-report)
REPORT_URL="$U/api/sessions/$AMUX_SESSION/report"
# AMUX-4024: THE SUBAGENT LIFECYCLE PRODUCER.
#
# `subagent_event_post` (session_verbs.rs) has accepted {"subagent":"start"} /
# {"subagent":"stop"} since AMUX-3048, and `FleetSignals::subagents_working`
# has read the resulting count ever since. NOTHING EVER SENT ONE. Measured
# 2026-09-02: `subagents_live` was null for 125 of 125 lanes, so the durable
# signal that whole cluster (AMUX-2646/2904/2959/2952/3022/3030/3047) was
# built around had a reader, a store, a test and no producer — the same shape
# this file's own `active_model` comment describes thirty lines below.
#
# That is also why the count could not simply be switched on: an mtime cannot
# tell "thinking, will write in 90s" from "finished 30s ago", and with no
# events there was nothing better to prefer. Two live specimens, one in each
# direction, on the same afternoon: tubescience read IDLE while blocked on a
# background agent, and mvs-pitr read WORKING with an AGENTS badge over an
# empty composer, its subagents long finished and their transcripts still
# inside the 240s window.
#
# THE MATCHER IS ANCHORED (`^(Task|Agent)$`) and that is load-bearing. Claude
# Code matches the matcher against the TOOL NAME as an unanchored regex, and
# `TaskOutput` and `TaskStop` are separate tools you call WHILE polling
# background work — a bare `Task|Agent` matches both, so every poll of a running
# agent would have incremented the count again and pinned the lane WORKING. That
# is the exact false-WORKING this card is fixing, reintroduced by its own fix.
#
# A subagent event says nothing about the main turn, so this branch skips the
# transcript/token extraction entirely and posts only the lifecycle fact. It
# reuses the POST + failure-logging below rather than adding a second sender,
# so a refused subagent event is visible in the same log as every other
# refused report.
# BOTH SPELLINGS ARE ACCEPTED, and that is not politeness. Two independent
# implementations of this producer landed on the same day: this one uses
# `subagent:start` (colon) and #182 used `subagent-start` (hyphen). The
# settings.json wired on this box and verified live calls the COLON form, so
# dropping it would silently stop the counting — the hook would post
# {"state":"subagent:start"}, the server would refuse it, and the only symptom
# would be lanes reading WORKING for four minutes again.
case "${MODE/subagent-/subagent:}" in
  blocked)
    # NOTIFICATION FIRES FOR SEVERAL THINGS AND ONLY ONE OF THEM IS "a human is
    # being asked" (AMUX-4723). The event's own payload carries the
    # discriminator: `hook_event_name:"Notification" ... notification_type:<t>`,
    # where t is one of permission_prompt, idle_prompt, auth_success,
    # elicitation_dialog, agent_needs_input, agent_completed and others
    # (verified against the installed 2.1.274 bundle, not inferred).
    #
    # `idle_prompt` is the dangerous one: it fires ~60s after Claude finishes
    # when nobody has typed. Reporting THAT as blocked would mark every quiet
    # lane unreachable to automation for the 24h `blocked` trust window
    # (sessions_legacy.rs:137), which is a far worse failure than the one this
    # card fixes. So the state is claimed ONLY on the one type that means it.
    #
    # FAILS CLOSED, deliberately, and this is the direction that matters: an
    # unrecognised or empty payload exits without reporting, leaving today's
    # behaviour exactly as it is. Being unable to prove a human is waiting must
    # never be reported as a human waiting.
    case "$IN" in
      *'"notification_type":"permission_prompt"'*|*'"notification_type": "permission_prompt"'*) ;;
      *) exit 0 ;;
    esac
    ;;
  subagent:reset)
    # SessionStart fires for startup, resume AND compact, and only the first two
    # mean a NEW process. A compact keeps the same process, so its background
    # agents are still running and zeroing the count here would invent the very
    # false-idle this card is fixing. Skip on compact; the payload says which.
    case "$IN" in
      *'"source":"compact"'*|*'"source": "compact"'*) exit 0 ;;
    esac
    ;;
esac
# State and lifecycle share one atomic producer; only their merge policy differs.
QD="$HOME/.amux/hook-report-queue"
QF="$QD/$AMUX_SESSION.json"
KIND=lifecycle_queue
case "${MODE/subagent-/subagent:}" in
  subagent:*) ;;
  *)
    # Later hooks also revive stranded lifecycle queues.
    if [ -s "$QF" ] || [ -d "$QF.spool" ]; then
      nohup bash "$0" --drain-subagents "$QF" "$REPORT_URL" "$AMUX_SESSION" \
        </dev/null >/dev/null 2>&1 &
      disown "$!" 2>/dev/null || true
    fi
    QF="$QD/$AMUX_SESSION.state.json"; KIND=state_queue
    ;;
esac
QUEUE_NOTE=$(printf '%s' "$IN" | /usr/bin/python3 -c '
import fcntl,json,sys,os,time,uuid,tempfile
raw=sys.stdin.read()
mode,src,path,derived,corrected,stale=sys.argv[1:7]
norm=mode.replace("subagent-","subagent:",1)
if norm.startswith("subagent:"):
    out={"subagent":norm.split(":",1)[1],"source":src}
else:
    out={"state":mode,"source":src}
out["provider"]="claude"
h={}; tp=""; nlines=0; err=""
try:
    h=json.loads(raw) if raw.strip() else {}
    tp=h.get("transcript_path") or ""
    m=h.get("model") or {}
    if isinstance(m,dict): m=m.get("id") or m.get("display_name") or ""
    if m: out["model"]=m
    # CONVERSATION ID (AMUX-2936, 2026-08-15). Free: already in the payload we
    # parse. It closes the blind-cotenant window in the staged-commit guard.
    #
    # The guard calls a lane BLIND when it cannot resolve that lane transcript,
    # and blind is the ONLY class where a commit absorbing another session work
    # passes silently: foreign exits 1 and blocks, unclaimed does not. Measured
    # 2026-08-15, 321 blind warnings in 8h53m on ~/Dev/mixpeek, 304 naming ONE
    # running lane whose meta carried an empty cc_conversation_id. The server
    # was scanning 162 transcript files to re-derive an id the harness hands us
    # on stdin and we dropped on the floor.
    #
    # Sent from HERE because the server cannot derive it safely: ~/Dev/mixpeek
    # hosts ~30 lanes, so "newest jsonl in the project dir" is whichever
    # neighbour spoke last. On 2026-08-09 that stamped the amux lane with the
    # LIVE conversation of amux-rust. The harness knows; nothing else does.
    #
    # NO APOSTROPHES ANYWHERE IN THIS BLOCK. The whole python program is a
    # single-quoted shell argument, so one apostrophe ends it and the file dies
    # with "unexpected EOF while looking for matching )". Adding this comment
    # did exactly that on the first attempt, and a broken hook-report.sh breaks
    # state reporting for every already-running lane, since amux-report.sh
    # execs this path.
    sid=h.get("session_id") or ""
    if not sid and tp and tp.endswith(".jsonl"):
        sid=os.path.basename(tp)[:-6]
    if sid: out["session_id"]=sid
    if "subagent" in out:
        sub=out.get("subagent") or ""
        aid=h.get("agent_id") or h.get("agentId") or ""
        if aid: out["agent_id"]=str(aid)
        out["event_ts"]=time.time()
        native=h.get("event_id") or h.get("eventId") or ""
        if native:
            event_id=str(native)
        elif sub != "reset" and sid and aid:
            # Provider lifecycle identity: stable across a duplicate hook
            # callback and across a response-loss retry.
            event_id=f"{sid}:{aid}:{sub}"
        elif sub == "reset":
            # The same conversation may be resumed by a NEW process. Each
            # SessionStart is therefore a distinct reset; response-loss retries
            # keep this generated id in the durable queue row below.
            event_id=f"{sid or os.environ.get('AMUX_SESSION','')}:reset:{time.time_ns()}"
        else:
            # No provider identity means no safe equivalence relation. Two
            # empty-payload starts may be two concurrent agents, so each hook
            # invocation gets a distinct id; retries reuse the queued body.
            event_id=f"anonymous:{time.time_ns()}:{os.getpid()}:{uuid.uuid4().hex}"
        out["event_id"]=event_id
except Exception as e:
    err="payload:"+type(e).__name__
# Malformed provider JSON is still one lifecycle invocation. It must not fall
# through with an empty dedupe key, because two malformed starts may represent
# two concurrent agents just as two valid empty objects do.
if "subagent" in out and not out.get("event_id"):
    out["event_ts"]=time.time()
    out["event_id"]=f"anonymous:{time.time_ns()}:{os.getpid()}:{uuid.uuid4().hex}"
# TRANSCRIPT READ GETS ITS OWN try (2026-08-11). It used to sit inside the outer
# one, so a missing or unreadable transcript threw straight past the diagnostic
# below — skipping the log in exactly the case the log exists to explain. Caught
# by building a failing payload and checking the log stayed empty, which is the
# only way that class of hole shows up: everything looked like it ran.
if tp and "subagent" not in out:
    try:
        tot=0
        # MODEL FROM THE TRANSCRIPT, not only from the hook payload: the payload
        # shape is the harness own and may or may not carry it, while every
        # assistant message records the model it was produced by. This is the
        # field /api/sessions turns into active_model (AMUX-2828) — the consumer
        # has always existed (sessions_legacy.rs:1292); nothing ever sent it, so
        # the dashboard fell back to CC_FLAGS, the model amux ASKED for at spawn.
        # social-media showed "opus" while running Fable 5 AND rate-limited on it.
        for l in open(tp):
            nlines+=1
            try: d=json.loads(l)
            except Exception: continue
            msg=d.get("message") or {}
            mm=msg.get("model")
            if mm: out["model"]=mm
            u=msg.get("usage") or d.get("usage")
            if isinstance(u,dict):
                tot=(u.get("input_tokens",0)+u.get("cache_read_input_tokens",0)
                     +u.get("cache_creation_input_tokens",0)+u.get("output_tokens",0))
        if tot: out["tokens"]=tot
    except Exception as e:
        err="transcript:"+type(e).__name__
# WHY-IT-FAILED DIAGNOSTIC. model reached 2 of 42 reporting lanes and tokens 1
# of 42, and nothing could say WHICH step dropped them: absent transcript_path,
# unreadable file, and a transcript carrying neither field all produced the
# identical empty report. Written only when something is missing, so it
# self-extinguishes as this gets fixed instead of growing across 47 lanes.
if "subagent" not in out and (not out.get("model") or not out.get("tokens")):
    try:
        d2={"s":os.environ.get("AMUX_SESSION",""),"src":src,"keys":sorted(h.keys())[:12],
            "tp":bool(tp),"tp_exists":bool(tp) and os.path.exists(tp),"lines":nlines,
            "err":err,"got_model":bool(out.get("model")),"got_tokens":bool(out.get("tokens"))}
        lg=os.path.expanduser("~/.amux/logs/hook-extract.jsonl")
        if os.path.exists(lg) and os.path.getsize(lg)>2000000: os.remove(lg)
        with open(lg,"a") as f: f.write(json.dumps(d2)+"\n")
    except Exception: pass
if derived == "1": out["amux_session_derived"]=True
if corrected == "1": out["amux_session_corrected_from"]=stale
is_state="subagent" not in out
queued_ns=time.time_ns()
event_id=out.get("event_id") or f"state:{queued_ns}:{uuid.uuid4().hex}"
row={"event_id":event_id,"body":out,"attempts":0,"queued_ns":queued_ns}
folder=os.path.dirname(path)
os.makedirs(folder,mode=0o700,exist_ok=True)

def publish(value,destination,prefix):
    fd,tmp=tempfile.mkstemp(prefix=prefix,dir=os.path.dirname(destination))
    try:
        with os.fdopen(fd,"w") as stream:
            json.dump(value,stream,separators=(",",":")); stream.write("\n")
            stream.flush(); os.fsync(stream.fileno())
        os.replace(tmp,destination)
    except BaseException:
        try: os.unlink(tmp)
        except FileNotFoundError: pass
        raise

def note(message):
    session=os.environ.get("AMUX_SESSION", "")
    log=os.path.expanduser("~/.amux/logs/hook-report-failures.log")
    os.makedirs(os.path.dirname(log),exist_ok=True)
    with open(log,"a") as stream:
        stream.write(f"{time.time()} {session} queue={path} {message}\n")

with open(path+".lock","a+") as guard:
    try:
        fcntl.flock(guard,fcntl.LOCK_EX|fcntl.LOCK_NB)
    except BlockingIOError:
        spool=path+".spool"
        os.makedirs(spool,mode=0o700,exist_ok=True)
        publish(row,os.path.join(spool,f"{queued_ns:020d}-{uuid.uuid4().hex}.json"),"pending.")
        note("verdict=queue_lock_contended spooled=1")
        print("1 0 128")
        raise SystemExit(0)
    try:
        with open(path) as stream: rows=json.load(stream)
        if not isinstance(rows,list) or any(
            not isinstance(item,dict) or not isinstance(item.get("body"),dict) for item in rows
        ): raise ValueError("invalid hook queue schema")
    except FileNotFoundError:
        rows=[]
    except Exception as exc:
        preserved=path+f".corrupt.{time.time_ns()}"
        os.replace(path,preserved)
        kind="state_queue" if is_state else "lifecycle_queue"
        note(f"{kind}=corrupt verdict=preserved_corrupt_queue preserved={preserved} error={type(exc).__name__}")
        rows=[]
    if is_state:
        row["attempts"]=max(0,int(rows[0].get("attempts",0))) if rows else 0
        rows=[row]
    elif not any(item.get("event_id")==event_id for item in rows):
        rows.append(row)
    try: limit=max(1,min(128,int(os.environ.get("AMUX_HOOK_QUEUE_LIMIT","128"))))
    except Exception: limit=128
    dropped=max(0,len(rows)-limit)
    if dropped: rows=rows[-limit:]
    publish(rows,path,"queue.")
print(f"{len(rows)} {dropped} {limit}")

' "$MODE" "$SRC" "$QF" "$DERIVED" "$CORRECTED" "${STALE_FROM:-}" 2>/dev/null)
D="$HOME/.amux/logs"
if [ -z "$QUEUE_NOTE" ]; then
  mkdir -p "$D" 2>/dev/null
  printf '%s %s source=%s %s=enqueue_failed verdict=enqueue_failed\n' \
    "$(date -u '+%Y-%m-%dT%H:%M:%SZ')" "$AMUX_SESSION" "$SRC" "$KIND" \
    >> "$D/hook-report-failures.log" 2>/dev/null
  exit 0
fi
set -- $QUEUE_NOTE
if [ "${2:-0}" -gt 0 ] 2>/dev/null; then
  mkdir -p "$D" 2>/dev/null
  printf '%s %s source=%s %s=overflow pending=%s dropped=%s limit=%s\n' \
    "$(date -u '+%Y-%m-%dT%H:%M:%SZ')" "$AMUX_SESSION" "$SRC" "$KIND" "$1" "$2" "$3" \
    >> "$D/hook-report-failures.log" 2>/dev/null
fi
nohup bash "$0" --drain-subagents "$QF" "$REPORT_URL" "$AMUX_SESSION" "$KIND" \
  </dev/null >/dev/null 2>&1 &
disown "$!" 2>/dev/null || true
exit 0
