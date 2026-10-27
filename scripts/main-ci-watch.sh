#!/usr/bin/env bash
# Main's CI is the full gate now that PRs run only affected tests (#317), and
# the builder deploys every main commit without waiting for it. So a red main
# must be noticed by someone who will fix it. Run as a shell schedule every few
# minutes: for each workflow, look at main's newest completed run and keep ONE
# GitHub issue per fault key (`ci-main|<workflow>`) in the alert repo (maxtongwang/hq by default): a repeat
# red run updates it, a green run closes it with a note, and red after that
# opens one new issue. The key is stored in the issue body as a stable marker,
# the same one the server's fault tracker writes. DRY=1 prints instead of filing.
set -euo pipefail
REPO=mixpeek/amux
ALERT_REPO="${AMUX_ALERT_REPO:-maxtongwang/hq}"
ALERT_SOURCE="${AMUX_ALERT_SOURCE:-amux}"
ALERT_LABELS="kind:alert,team:dev,repo:$ALERT_SOURCE"
KEY_PREFIX="<!-- amux-fault-key: "
GET_TOKEN="${AMUX_GET_TOKEN:-$HOME/.amux/github-app/get-token.sh}"
STATE="${MAIN_CI_WATCH_STATE:-${AMUX_HOME:-$HOME/.amux}/state/main-ci-watch.json}"
mkdir -p "$(dirname "$STATE")"; [ -f "$STATE" ] || echo '{}' > "$STATE"
eval "$($GET_TOKEN --min-remaining 300)"
# Alert issues are written with the user identity: the App token reads CI but
# is not granted the alert repo. A missing scope fails loudly below.
ghi() { env -u GH_TOKEN -u GITHUB_TOKEN gh "$@"; }
# Strip what must not reach a shared tracker: home paths, e-mail, IPv4, private hosts.
sanitize() {
  sed -E -e 's#/(Users|home)/[A-Za-z0-9._-]+#~#g' \
         -e 's#[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}#<email>#g' \
         -e 's#\b[0-9]{1,3}(\.[0-9]{1,3}){3}\b#<ip>#g' \
         -e 's#\b[A-Za-z0-9.-]+\.(ts\.net|local|lan|internal)\b#<host>#g'
}
# Open issue number for a fault key; empty when none. Fails (non-zero) when the
# tracker cannot be read, so a read error is never taken for "no issue".
find_issue() {
  local key=$1 out
  out=$(ghi issue list --repo "$ALERT_REPO" --label kind:alert --label "repo:$ALERT_SOURCE" --state open --limit 100 \
        --json number,body --jq ".[] | select(.body | contains(\"${KEY_PREFIX}${key} -->\")) | .number") || return 1
  printf '%s\n' "$out" | head -1
}
filed=0 updated=0 closed=0 considered=0 refused=0
for wf in rust.yml checks.yml; do
  row=$(gh api "repos/$REPO/actions/workflows/$wf/runs?branch=main&status=completed&per_page=1" \
        --jq '.workflow_runs[0] | [.id, .conclusion, .head_sha, .html_url] | @tsv') || continue
  [ -n "$row" ] || continue
  considered=$((considered + 1))
  IFS=$'\t' read -r id conclusion sha url <<<"$row"
  seen=$(python3 -c "import json,sys; s=json.load(open('$STATE')); print(s.get('$wf',{}).get('seen',''))")
  [ "$seen" = "$id" ] && continue
  if [ "$conclusion" = success ]; then
    if [ "${DRY:-0}" != 1 ]; then
      if ! num=$(find_issue "ci-main|${wf%.yml}"); then
        echo "could not read alert issues; green run not marked seen" >&2; refused=$((refused + 1)); continue
      fi
      if [ -n "$num" ]; then
        if ghi issue close "$num" --repo "$ALERT_REPO" \
             --comment "Fault cleared at $(date -u +%FT%TZ): main's ${wf%.yml} run $id is green at ${sha:0:9}. Closed automatically; a new red run opens a new issue." >/dev/null; then
          closed=$((closed + 1))
        else echo "could not close alert issue #$num" >&2; refused=$((refused + 1)); continue; fi
      fi
    fi
    python3 - "$STATE" "$wf" "$id" "$sha" <<'PY'
import json,sys
p,wf,i,sha=sys.argv[1:]; s=json.load(open(p)); s[wf]={'seen':i,'green':sha}; json.dump(s,open(p,'w'))
PY
    continue
  fi
  [ "$conclusion" = failure ] || { python3 - "$STATE" "$wf" "$id" <<'PY'
import json,sys
p,wf,i=sys.argv[1:]; s=json.load(open(p)); s.setdefault(wf,{})['seen']=i; json.dump(s,open(p,'w'))
PY
    continue; }
  green=$(python3 -c "import json; print(json.load(open('$STATE')).get('$wf',{}).get('green',''))")
  tests=$(gh api "repos/$REPO/actions/runs/$id/jobs" --jq '.jobs[]|select(.conclusion=="failure")|.id' | head -6 | while read -r j; do
      gh api "repos/$REPO/actions/jobs/$j/logs" 2>/dev/null | grep -av WebServer \
        | grep -aE '^\S+\s+[0-9]+\) \[|^\S+ +test .* FAILED|^\S+ +not ok|  FAIL  ' | sed 's/^[0-9TZ:.-]* //' | head -3 || true
    done | sed 's/^ *//' | sort -u | head -10 || true)
  commits=""
  if [ -n "$green" ]; then
    commits=$(gh api "repos/$REPO/compare/$green...$sha" --jq '.commits[] | "\(.sha[0:9]) \(.commit.message | split("\n")[0])"' 2>/dev/null | tail -15 || true)
  fi
  first=$(printf '%s\n' "$tests" | head -1 | cut -c1-90)
  title="main CI red: ${wf%.yml} on ${sha:0:9}${first:+: $first}"
  desc=$(printf 'Run: %s\n\nFailing:\n%s\n\nCommits since the last green %s run (%s):\n%s\n\nPRs now run only affected tests (#317), so main is the full gate and the builder already deployed this commit. Fix forward or revert the culprit.' \
    "$url" "${tests:-see the run}" "${wf%.yml}" "${green:0:9}" "${commits:-unknown: no green run recorded yet}")
  key="ci-main|${wf%.yml}"
  title=$(printf '%s' "$title" | sanitize)
  body=$(printf '%s%s -->\n%s\n\nLast seen: %s (run %s)\n' "$KEY_PREFIX" "$key" "$(printf '%s' "$desc" | sanitize)" "$(date -u +%FT%TZ)" "$id")
  landed=0
  if [ "${DRY:-0}" = 1 ]; then
    printf 'DRY issue (%s): %s\n%s\n' "$key" "$title" "$body"; landed=1
  elif num=$(find_issue "$key"); then
    if [ -n "$num" ]; then
      # Same fault again: update in place, open nothing.
      if printf '%s' "$body" | ghi issue edit "$num" --repo "$ALERT_REPO" --title "$title" --body-file - >/dev/null; then
        updated=$((updated + 1)); landed=1
      else echo "could not update alert issue #$num" >&2; fi
    else
      if printf '%s' "$body" | ghi issue create --repo "$ALERT_REPO" --title "$title" --label "$ALERT_LABELS" --body-file - >/dev/null; then
        filed=$((filed + 1)); landed=1
      else echo "could not open the alert issue (token scope on $ALERT_REPO?)" >&2; fi
    fi
  else
    echo "could not read alert issues on $ALERT_REPO; nothing filed (token scope?)" >&2
  fi
  [ "$landed" = 1 ] || refused=$((refused + 1))
  # Mark the run seen only once someone was told; an alert that never landed
  # is retried on the next tick.
  if [ "$landed" = 1 ]; then
    python3 - "$STATE" "$wf" "$id" <<'PY'
import json,sys
p,wf,i=sys.argv[1:]; s=json.load(open(p)); s.setdefault(wf,{})['seen']=i; json.dump(s,open(p,'w'))
PY
  fi
done
echo "{\"verdict\":\"main_ci_watch\",\"measured\":true,\"n_considered\":$considered,\"issues_opened\":$filed,\"issues_updated\":$updated,\"issues_closed\":$closed,\"alerts_not_delivered\":$refused}"
# A red run nobody was told about is a failed tick, so the schedule reports it.
[ "$refused" = 0 ]
