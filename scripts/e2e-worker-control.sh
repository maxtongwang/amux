#!/usr/bin/env bash
# Hosted e2e worker-concurrency control: dispatch rust.yml at ONE SHA with
# different per-shard Playwright worker counts, then report per-case outcomes
# and wall times for each setting from the runs' own e2e-evidence artifacts.
set -euo pipefail

usage() {
  cat <<'EOF'
usage:
  scripts/e2e-worker-control.sh dispatch <sha> <repetitions> [--workers "1 2"] [--max-inflight K] [--out FILE] [--dry-run]
  scripts/e2e-worker-control.sh report <runs.tsv> [--dir DIR] [--all] [--json]

dispatch  Pushes branch e2e-control/<sha12> pointing at <sha> (workflow_dispatch
          needs a branch or tag, not a bare SHA; rust.yml's push trigger is main
          only, so this branch runs nothing by itself), then dispatches rust.yml
          <repetitions> times per worker setting, interleaved (1,2,1,2,...) so
          both settings meet the same runner conditions. Each run's id is
          appended to FILE (default e2e-control-<sha12>.tsv) as
          "workers<TAB>run_id<TAB>sha<TAB>branch". --dry-run prints the git/gh
          commands and dispatches nothing.

          --max-inflight K (default 4, 1-20) caps concurrent control runs: before
          each dispatch, rust.yml runs on the control branch that are not yet
          completed are counted, and the script waits (one check per minute, at
          most 3 hours per slot) until fewer than K remain. If a slot does not
          free in time it exits 1; the runs already dispatched are in FILE.

          If a dispatch starts but its run id cannot be found, the row is written
          with run_id "unknown" and the script exits 1. Replace "unknown" with the
          id from `gh run list --branch <branch>` (or delete the row) before
          running report.

          Preconditions: rust.yml on the DEFAULT branch must declare
          workflow_dispatch (GitHub refuses a dispatch otherwise), and rust.yml
          AT <sha> must declare the e2e_workers input, since the run uses the
          ref's copy of the workflow. Each dispatch is a full rust.yml run (cargo
          jobs included), so budget minutes for 2 x <repetitions> full runs.

report    For every run in runs.tsv: saves `gh run view --json` metadata and
          downloads its e2e-evidence-* artifacts into DIR/<run_id>/ (default DIR
          e2e-control-report), then prints, per worker setting, run counts
          (complete / partial / unmeasured), shard execution and job wall times
          against the 20 min execution / 30 min job budgets, and every case that
          failed or did not finish in any run as pass/fail/unfinished/unmeasured
          counts (--all prints every case, --json the full aggregate). A run or
          shard with no evidence artifact is UNMEASURED, never a pass. Rerunning
          is safe: existing downloads are reused. Exits 1 when no run was
          measured (no run had any evidence artifact), with the reason.

Environment: GH_REPO selects the repository (gh's own variable); REMOTE the git
remote to push the control branch to (default origin).

Exit status: 0 success; 1 failure (git/gh error, or an integrity error in the
report such as mixed SHAs or disagreeing worker settings); 2 usage error.

Control procedure and decision rule: VERIFY.md, "e2e worker control".
EOF
}

die() { echo "e2e-worker-control: $*" >&2; exit 1; }
usage_error() { echo "e2e-worker-control: $*" >&2; usage >&2; exit 2; }
run() { if [ "${DRY_RUN:-0}" = 1 ]; then printf 'DRY:'; printf ' %q' "$@"; printf '\n'; else "$@"; fi; }

ROOT=$(git rev-parse --show-toplevel 2>/dev/null) || die "run inside the amux checkout"
SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)
REMOTE=${REMOTE:-origin}
WORKFLOW=rust.yml

dispatch() {
  local sha_arg=${1:-} reps=${2:-}; shift 2 || usage_error "dispatch needs <sha> <repetitions>"
  local workers="1 2" out="" max_inflight=4 DRY_RUN=0
  while [ $# -gt 0 ]; do
    case "$1" in
      --workers) [ $# -ge 2 ] || usage_error "--workers needs a value"; workers=$2; shift 2 ;;
      --out) [ $# -ge 2 ] || usage_error "--out needs a value"; out=$2; shift 2 ;;
      --max-inflight) [ $# -ge 2 ] || usage_error "--max-inflight needs a value"; max_inflight=$2; shift 2 ;;
      --dry-run) DRY_RUN=1; shift ;;
      *) usage_error "unknown option: $1" ;;
    esac
  done
  [[ "$reps" =~ ^[1-9][0-9]?$ ]] || usage_error "repetitions must be 1-99, got '$reps'"
  [[ "$max_inflight" =~ ^[1-9][0-9]?$ ]] && [ "$max_inflight" -le 20 ] || usage_error "--max-inflight must be 1-20, got '$max_inflight'"
  local w
  for w in $workers; do [[ "$w" =~ ^[1-4]$ ]] || usage_error "worker settings must be 1-4 (rust.yml's choices), got '$w'"; done
  local sha
  sha=$(git -C "$ROOT" rev-parse --verify --quiet "${sha_arg}^{commit}") || die "unknown commit '$sha_arg' (git fetch first?)"
  # Captured first, not piped: under pipefail `git show | grep -q` fails when
  # grep exits early and git show dies of SIGPIPE, refusing a valid SHA.
  local workflow_text
  workflow_text=$(git -C "$ROOT" show "$sha:.github/workflows/$WORKFLOW") || die "no .github/workflows/$WORKFLOW at ${sha:0:12}"
  grep -qE '^[[:space:]]+e2e_workers:' <<<"$workflow_text" \
    || die "$WORKFLOW at ${sha:0:12} has no e2e_workers input; a dispatch there would ignore the worker setting"
  local branch="e2e-control/${sha:0:12}"
  out=${out:-e2e-control-${sha:0:12}.tsv}

  local remote_sha
  remote_sha=$(git -C "$ROOT" ls-remote --heads "$REMOTE" "refs/heads/$branch" | cut -f1) || die "cannot reach remote $REMOTE"
  if [ -z "$remote_sha" ]; then
    run git -C "$ROOT" push "$REMOTE" "$sha:refs/heads/$branch" || die "could not push $branch"
  elif [ "$remote_sha" != "$sha" ]; then
    die "$REMOTE/$branch exists at $remote_sha, not $sha; delete it or pick another SHA"
  fi
  [ "$DRY_RUN" = 1 ] || [ -s "$out" ] || printf 'workers\trun_id\tsha\tbranch\n' > "$out"

  local i id rc
  for ((i = 1; i <= reps; i++)); do
    for w in $workers; do
      if [ "$DRY_RUN" = 1 ]; then
        run gh workflow run "$WORKFLOW" --ref "$branch" -f e2e_workers="$w"
        continue
      fi
      wait_for_slot "$branch" "$max_inflight" || die "dispatch $i (workers=$w) not started; runs so far are in $out"
      rc=0
      id=$(dispatch_one "$branch" "$w") || rc=$?
      case "$rc" in
        0) printf '%s\t%s\t%s\t%s\n' "$w" "$id" "$sha" "$branch" | tee -a "$out" ;;
        3) printf '%s\t%s\t%s\t%s\n' "$w" unknown "$sha" "$branch" | tee -a "$out"
           die "dispatch $i (workers=$w) started but its run id was not found; recorded as 'unknown' in $out. Find it with: gh run list --workflow $WORKFLOW --branch $branch" ;;
        *) die "dispatch $i (workers=$w) failed; runs so far are in $out" ;;
      esac
    done
  done
  [ "$DRY_RUN" = 1 ] && return 0
  echo "dispatched $((reps * $(wc -w <<<"$workers"))) run(s) at $sha; when they finish: scripts/e2e-worker-control.sh report $out" >&2
  echo "afterwards remove the branch: git push $REMOTE --delete $branch" >&2
}

INFLIGHT_POLL_SECONDS=60
INFLIGHT_MAX_POLLS=180 # 3 hours per slot

# Returns once fewer than <max> rust.yml runs on <branch> are not completed
# (queued, in_progress, waiting, pending, ...). Returns 1 if gh fails or no
# slot frees within INFLIGHT_MAX_POLLS checks.
wait_for_slot() {
  local branch=$1 max=$2 polls=0 statuses n
  while :; do
    statuses=$(gh run list --workflow "$WORKFLOW" --branch "$branch" --limit 200 --json status --jq '.[].status') \
      || { echo "gh run list failed; cannot count in-flight runs on $branch" >&2; return 1; }
    n=$(grep -cvx -e completed -e '' <<<"$statuses" || true)
    [ "$n" -lt "$max" ] && return 0
    if [ "$polls" -ge "$INFLIGHT_MAX_POLLS" ]; then
      echo "still $n run(s) in flight on $branch (cap $max) after $((INFLIGHT_MAX_POLLS * INFLIGHT_POLL_SECONDS / 60)) min; giving up" >&2
      return 1
    fi
    echo "$n run(s) in flight on $branch (cap $max); checking again in ${INFLIGHT_POLL_SECONDS}s" >&2
    polls=$((polls + 1))
    sleep "$INFLIGHT_POLL_SECONDS"
  done
}

# Prints the new run's id. Newer gh prints the run URL; older gh does not, so
# fall back to the one workflow_dispatch run on the branch not seen before.
# Exit 1: nothing was dispatched. Exit 3: the run was dispatched but its id
# could not be determined.
dispatch_one() {
  local branch=$1 w=$2 before output id tries
  before=$(gh run list --workflow "$WORKFLOW" --branch "$branch" --event workflow_dispatch --limit 200 --json databaseId --jq '.[].databaseId') \
    || return 1
  output=$(gh workflow run "$WORKFLOW" --ref "$branch" -f e2e_workers="$w" 2>&1) || { echo "$output" >&2; return 1; }
  id=$(grep -oE '/actions/runs/[0-9]+' <<<"$output" | head -1 | grep -oE '[0-9]+$' || true)
  local fresh
  for ((tries = 0; tries < 12; tries++)); do
    [ -z "$id" ] || break
    sleep 10
    fresh=$(gh run list --workflow "$WORKFLOW" --branch "$branch" --event workflow_dispatch --limit 200 --json databaseId --jq '.[].databaseId' \
      | grep -vxF -f <(printf '%s\n' "$before") || true)
    [ "$(grep -c . <<<"$fresh")" -le 1 ] || { echo "more than one new run appeared at once: $fresh" >&2; return 3; }
    id=$fresh
  done
  [ -n "$id" ] || { echo "the dispatched run did not appear within 2 minutes" >&2; return 3; }
  echo "$id"
}

report() {
  local tsv=${1:-}; shift || usage_error "report needs <runs.tsv>"
  local dir=e2e-control-report
  local -a pass=()
  while [ $# -gt 0 ]; do
    case "$1" in
      --dir) [ $# -ge 2 ] || usage_error "--dir needs a value"; dir=$2; shift 2 ;;
      --all|--json) pass+=("$1"); shift ;;
      *) usage_error "unknown option: $1" ;;
    esac
  done
  [ -f "$tsv" ] || usage_error "no such file: $tsv"
  mkdir -p "$dir"
  local w id sha branch rd n=0
  while IFS=$'\t' read -r w id sha branch; do
    [ "$w" = workers ] && continue
    [ "$id" = unknown ] && die "a workers=$w dispatch in $tsv has run id 'unknown'; replace it with the id from 'gh run list --workflow $WORKFLOW --branch $branch', or delete the row"
    [[ "$id" =~ ^[0-9]+$ && "$w" =~ ^[0-9]+$ && "$sha" =~ ^[0-9a-f]{40}$ && "$branch" =~ ^[A-Za-z0-9._/-]+$ ]] \
      || die "malformed line in $tsv: '$w $id $sha $branch'"
    rd="$dir/$id"; mkdir -p "$rd"; n=$((n + 1))
    printf '{"workers": %s, "sha": "%s", "branch": "%s"}\n' "$w" "$sha" "$branch" > "$rd/dispatch.json"
    gh run view "$id" --json databaseId,headSha,status,conclusion,createdAt,updatedAt,event,jobs > "$rd/run.json.tmp" \
      && mv "$rd/run.json.tmp" "$rd/run.json" \
      || { rm -f "$rd/run.json.tmp"; echo "run $id: gh run view failed; reporting it from artifacts only" >&2; }
    if [ ! -d "$rd/artifacts" ] || [ -z "$(ls -A "$rd/artifacts")" ]; then
      gh run download "$id" --pattern 'e2e-evidence-*' --dir "$rd/artifacts" \
        || echo "run $id: no e2e-evidence artifacts downloaded; it reports as unmeasured" >&2
    fi
  done < "$tsv"
  [ "$n" -gt 0 ] || die "$tsv lists no runs"
  node "$SCRIPT_DIR/e2e-worker-control.mjs" "$dir" ${pass[@]+"${pass[@]}"}
}

case "${1:-}" in
  dispatch) shift; dispatch "$@" ;;
  report) shift; report "$@" ;;
  -h|--help|help) usage ;;
  *) usage_error "unknown command '${1:-}'" ;;
esac
