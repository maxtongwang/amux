#!/usr/bin/env bash
# The launchd builder must not execute whichever revision happens to be checked
# out in the shared developer tree. A second checkout can be locally ahead (or
# simply stale) while still pointing its timer at this path; that was the ATE-93
# takeover loop. This launcher is the single normal activation authority: it
# runs the committed builder only from a clean detached worktree at origin/main.
set -euo pipefail

REPO="${AMUX_AUTHORITY_REPO:-}"
REF="${AMUX_RS_ACTIVATION_REF:-origin/main}"
WORK="${AMUX_AUTHORITY_WORKTREE:-$HOME/.amux/activation-source}"
MARKER="${AMUX_AUTHORITY_MARKER:-${WORK}.authority}"
LOG="${AMUX_AUTHORITY_LOG:-$HOME/.amux/logs/rust-auto-build.log}"
mkdir -p "$(dirname "$LOG")"

say() { echo "== !! ACTIVATION AUTHORITY $*" >> "$LOG"; }
refuse() { say "REFUSED — $*"; exit 0; }

[ -n "$REPO" ] || refuse "AMUX_AUTHORITY_REPO is unset; no checkout may choose the live image"

# FETCH $REF's REMOTE FIRST (2026-10-16). On an install with no developer pushing from the authority
# checkout, nothing else fetches it, so $REF never moved and green commits on the remote were never
# seen: a merged fix sat uninstalled until someone fetched by hand. One quiet fetch per tick, never
# fatal: a failed fetch decides from the last fetched $REF, as before. Bounded for EVERY transport:
# a hard wall-clock limit (AMUX_AUTHORITY_FETCH_TIMEOUT, default 45 s; macOS has no timeout(1), so
# perl runs git in its own process group and kills the whole group, ssh included, on expiry), and
# for ssh remotes a batch, keepalive-bounded ssh unless the owner configured their own ssh command.
# launchd starts no new tick while one runs, so an unbounded fetch would stop every deploy.
# Opt out with AMUX_AUTHORITY_FETCH=0.
if [ "${AMUX_AUTHORITY_FETCH:-1}" != "0" ] && [[ "$REF" == */* ]]; then
  remote=${REF%%/*}
  if git -C "$REPO" remote get-url "$remote" >/dev/null 2>&1; then
    ssh_cmd="${GIT_SSH_COMMAND:-$(git -C "$REPO" config --get core.sshCommand 2>/dev/null || true)}"
    [ -n "$ssh_cmd" ] || ssh_cmd="ssh -o BatchMode=yes -o ConnectTimeout=15 -o ServerAliveInterval=10 -o ServerAliveCountMax=2"
    fetch_err=$(GIT_TERMINAL_PROMPT=0 GIT_SSH_COMMAND="$ssh_cmd" \
      GIT_HTTP_LOW_SPEED_LIMIT=1000 GIT_HTTP_LOW_SPEED_TIME=30 \
      perl -e '
        my $t = shift; my $pid = fork; defined $pid or exit 125;
        if (!$pid) { setpgrp(0, 0); exec @ARGV; exit 127 }
        $SIG{ALRM} = sub { kill "KILL", -$pid; waitpid($pid, 0); print STDERR "timed out after ${t}s\n"; exit 124 };
        alarm $t; waitpid($pid, 0); exit($? >> 8)' \
        "${AMUX_AUTHORITY_FETCH_TIMEOUT:-45}" git -C "$REPO" fetch --quiet "$remote" 2>&1 >/dev/null) \
      || say "FETCH of $remote failed ($(printf '%s' "$fetch_err" | tr '\n' ' ' | cut -c1-160)); deciding from the last fetched $REF verdict=fetch_failed"
  fi
fi

tip=$(git -C "$REPO" rev-parse --verify -q "${REF}^{commit}" 2>/dev/null) \
  || refuse "cannot resolve $REF in $REPO; refusing an unmeasured activation"

# ONLY CI-GREEN COMMITS GO LIVE (Ethan, 2026-10-15: "make sure all the amux
# shit goes thru cicd"). Until then this activated $REF's tip about a minute
# after any push, while its CI was still running or already red. Now the target
# is the newest first-parent commit of $REF whose required push workflows
# (AMUX_CI_REQUIRED_WORKFLOWS, default rust,checks) all completed with success. A tip that is pending or red leaves the last green image
# running; a commit that never had CI (an inner commit of a multi-commit push)
# is passed over. Verdicts come from GitHub check runs through the App token;
# final ones are cached per sha so a 60s timer costs about one API call while a
# run is in flight. Opt out with AMUX_DEPLOY_CI_GATE=0.
CI_CACHE="${AMUX_CI_VERDICT_CACHE:-$HOME/.amux/ci-verdicts-v2}"  # v2: workflow-run verdicts; v1 judged check runs
CI_DEPTH="${AMUX_CI_GATE_DEPTH:-40}"
ci_verdict() {   # green | red | pending | none | unmeasured
  local sha="$1" cached
  if [ -n "${AMUX_CI_VERDICT_CMD:-}" ]; then "$AMUX_CI_VERDICT_CMD" "$sha"; return; fi
  cached=$(cat "$CI_CACHE/$sha" 2>/dev/null || true)
  case "$cached" in green|red|none) echo "$cached"; return ;; esac
  local token v
  token=$(GH_TOKEN= "$HOME/.amux/github-app/get-token.sh" --min-remaining 300 2>/dev/null \
    | sed -n 's/^export GH_TOKEN=//p' | tr -d "'\"") || true
  [ -n "$token" ] || { echo unmeasured; return; }
  # Judge by WORKFLOW RUNS, not check runs (2026-10-15): 6325024c7 went live
  # with its rust run cancelled by the next push before any job started, so it
  # had no failing check run and "every check run passed" was true of the
  # checks workflow alone. Each required workflow must have completed with
  # success; a cancelled or skipped one leaves the commit without a verdict
  # (none), so the walk passes it over to an older, fully tested commit.
  v=$(curl -sf --max-time 15 -H "Authorization: Bearer $token" -H "Accept: application/vnd.github+json" \
      "https://api.github.com/repos/${AMUX_CI_REPO:-mixpeek/amux}/actions/runs?head_sha=$sha&event=push&per_page=100" \
    | AMUX_CI_REQUIRED="${AMUX_CI_REQUIRED_WORKFLOWS:-rust,checks}" python3 -c '
import json,os,sys
runs=json.load(sys.stdin)["workflow_runs"]
need=[w.strip() for w in os.environ["AMUX_CI_REQUIRED"].split(",") if w.strip()]
if not runs: print("none"); raise SystemExit
states=[]
for w in need:
    mine=sorted([r for r in runs if r["name"]==w], key=lambda r: r["created_at"])
    if not mine: states.append("none"); continue
    r=mine[-1]
    if r["status"]!="completed": states.append("pending")
    elif r["conclusion"]=="success": states.append("green")
    elif r["conclusion"] in ("cancelled","skipped","stale"): states.append("none")
    else: states.append("red")
for v in ("red","pending","none"):
    if v in states: print(v); raise SystemExit
print("green")' 2>/dev/null) || v=unmeasured
  # "none" is only final once the commit is old enough that CI would have started.
  if [ "$v" = green ] || [ "$v" = red ] || { [ "$v" = none ] && \
     [ $(( $(date +%s) - $(git -C "$REPO" log -1 --format=%ct "$sha") )) -gt 3600 ]; }; then
    mkdir -p "$CI_CACHE" && echo "$v" > "$CI_CACHE/$sha"
  fi
  echo "$v"
}

target="$tip"
if [ "${AMUX_DEPLOY_CI_GATE:-1}" != "0" ]; then
  target=""; tip_state=""; passed=0
  for sha in $(git -C "$REPO" rev-list --first-parent -n "$CI_DEPTH" "$tip"); do
    v=$(ci_verdict "$sha")
    [ -n "$tip_state" ] || tip_state="$v"
    if [ "$v" = green ]; then target="$sha"; break; fi
    if [ "$v" = unmeasured ]; then
      refuse "CI-GATE unmeasured for $sha (no App token or GitHub unreachable); keeping the live image verdict=ci_gate_unmeasured"
    fi
    passed=$((passed + 1))
  done
  [ -n "$target" ] || refuse "CI-GATE no green commit in the last $CI_DEPTH of $REF (tip $tip is $tip_state); keeping the live image verdict=ci_gate_no_green"
  if [ "$target" != "$tip" ]; then
    # NEVER ROLL BACK. If the live image already contains the newest green
    # commit, holding it is the gate's whole effect: activating the older green
    # commit would run an old binary over a newer schema (migrations only go
    # forward) and undo work nobody asked to undo.
    live=$(curl -sk --max-time 4 "${AMUX_RS_HEALTH_URL:-https://localhost:${AMUX_RS_PORT:-8824}/api/health}" 2>/dev/null \
      | python3 -c 'import json,sys; d=json.load(sys.stdin); print((d.get("commit_full") or d.get("commit") or "").replace("-dirty",""))' 2>/dev/null || true)
    if [ -n "$live" ] && live_full=$(git -C "$REPO" rev-parse --verify -q "${live}^{commit}" 2>/dev/null) \
       && [ "$live_full" != "$target" ] && git -C "$REPO" merge-base --is-ancestor "$target" "$live_full"; then
      refuse "CI-GATE tip $tip is $tip_state; newest green $target is older than the live image $live_full, holding it rather than rolling back verdict=ci_gate_no_rollback"
    fi
    say "CI-GATE tip $tip is $tip_state; activating newest green $target ($passed newer commit(s) not yet green) verdict=ci_gate_held_back"
  fi
fi

if [ -e "$WORK" ]; then
  [ -f "$MARKER" ] || refuse "$WORK exists without this launcher's marker"
  [ "$(cat "$MARKER")" = "$REPO" ] || refuse "$WORK marker names a different source repository"
  git -C "$WORK" rev-parse --is-inside-work-tree >/dev/null 2>&1 \
    || refuse "$WORK is not a Git worktree"
  [ -z "$(git -C "$WORK" status --porcelain --untracked-files=all)" ] \
    || refuse "$WORK is dirty; preserving it instead of activating unknown bytes"
  git -C "$WORK" checkout --detach -q "$target" \
    || refuse "could not advance clean authority worktree to $target"
else
  mkdir -p "$(dirname "$WORK")"
  git -C "$REPO" worktree add --detach "$WORK" "$target" >/dev/null \
    || refuse "could not create authority worktree at $WORK"
  printf '%s\n' "$REPO" > "$MARKER"
fi

say "BUILD $target from detached $REF worktree=$WORK (source checkout=$REPO)"
# The builder's own authority check compares against this ref, so hand it the
# ELECTED sha: passing $REF made it refuse every held-back green commit.
AMUX_REPO="$WORK" AMUX_RS_ACTIVATION_REF="$target" "$WORK/scripts/rust-auto-build.sh"
