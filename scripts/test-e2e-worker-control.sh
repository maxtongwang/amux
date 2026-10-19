#!/usr/bin/env bash
# scripts/e2e-worker-control.sh, exercised for real (not --dry-run) against a
# synthetic repo whose only remote is a local bare repo, with a fake `gh` and a
# fake `sleep` first on PATH. Nothing here reaches GitHub: GH_REPO names a fake
# repository (cell 7 unsets it) and the fake gh exits 99 on any call it does not
# model; a GitHub URL on a remote is rewritten to the local bare repo.
#
# Cells: a capped real dispatch (4 runs, at most 2 in flight), the run-id
# parse loop (PR #39 B1), an untraceable dispatch recorded as 'unknown', the
# piped input check that refused valid SHAs under pipefail (N1), and report
# exiting nonzero when nothing was measured (N3), and the repository every gh
# call targets when GH_REPO is unset (7: the 2026-10-18 403 against upstream).
set -euo pipefail

SRC_REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
SCRIPT="$SRC_REPO/scripts/e2e-worker-control.sh"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
unset GH_TOKEN GITHUB_TOKEN
export GH_REPO=fake/fork
PASS=0
FAIL=0
check() {  # check <label> <0 = pass>
  if [ "$2" -eq 0 ]; then PASS=$((PASS + 1)); echo "  ok    $1"
  else FAIL=$((FAIL + 1)); echo "  FAIL  $1" >&2; fi
}

# ── fakes ────────────────────────────────────────────────────────────────────
# State: $STATE/runs holds "<id> <status>" per dispatched run; $STATE/gh.log
# records each `workflow run` with the in-flight count at that moment.
BIN="$TMP/bin"; STATE="$TMP/state"; mkdir -p "$BIN" "$STATE"
cat > "$BIN/gh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
S=$FAKE_GH_STATE
touch "$S/runs"
args=" $* "
case "$1 $2" in
  "workflow run")
    inflight=$(awk '$2 != "completed"' "$S/runs" | wc -l | tr -d ' ')
    n=$(( $(wc -l < "$S/runs") + 1001 ))
    echo "$n in_progress" >> "$S/runs"
    echo "workflow-run inflight=$inflight id=$n repo=${GH_REPO:-unset} args=$*" >> "$S/gh.log"
    [ "${FAKE_GH_NO_URL:-0}" = 1 ] || echo "https://github.com/fake/repo/actions/runs/$n"
    ;;
  "run list")
    case "$args" in
      *" --json status "*) awk '{print $2}' "$S/runs" ;;
      *" --json databaseId "*) [ "${FAKE_GH_HIDE_NEW:-0}" = 1 ] || awk '{print $1}' "$S/runs" ;;
      *) echo "fake gh: unmodelled run list: $*" >&2; exit 99 ;;
    esac ;;
  "run view"|"run download") echo "fake gh: no such run" >&2; exit 1 ;;
  *) echo "fake gh: unmodelled call: $*" >&2; exit 99 ;;
esac
EOF
# Each sleep stands for time passing: the oldest in-flight run completes.
cat > "$BIN/sleep" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
S=$FAKE_GH_STATE
echo "sleep $*" >> "$S/sleep.log"
[ "${FAKE_SLEEP_NOOP:-0}" = 1 ] && exit 0
touch "$S/runs"
awk 'BEGIN{d=0} !d && $2 != "completed" {print $1, "completed"; d=1; next} {print}' "$S/runs" > "$S/runs.tmp"
mv "$S/runs.tmp" "$S/runs"
EOF
chmod +x "$BIN/gh" "$BIN/sleep"
export PATH="$BIN:$PATH" FAKE_GH_STATE="$STATE"
reset_state() { rm -f "$STATE"/*; }

# ── fixture repo: origin is a local bare repo ────────────────────────────────
bare="$TMP/origin.git"
git init -q --bare "$bare"
repo="$TMP/repo"
git init -q "$repo"
git -C "$repo" config user.email t@example.com
git -C "$repo" config user.name t
mkdir -p "$repo/.github/workflows" "$repo/scripts"
# The pre-fix script resolved the report module from the checkout root, so the
# fixture carries a copy there too; both revisions then reach the same code.
cp "$SRC_REPO/scripts/e2e-worker-control.mjs" "$repo/scripts/"
printf 'name: rust\non:\n  workflow_dispatch:\n    inputs:\n      e2e_workers:\n        type: choice\n' > "$repo/.github/workflows/rust.yml"
git -C "$repo" add .github
git -C "$repo" -c core.hooksPath=/dev/null commit -qm with-input
sha=$(git -C "$repo" rev-parse HEAD)
branch="e2e-control/${sha:0:12}"
# N1's SHA: the input sits near the top of a large workflow, so `grep -q` exits
# while a piped `git show` is still writing, which made the race near-certain.
for i in $(seq 1 6000); do printf '# filler line %d keeps git show writing after grep has matched\n' "$i"; done >> "$repo/.github/workflows/rust.yml"
git -C "$repo" -c core.hooksPath=/dev/null commit -qam large-with-input
sha_large=$(git -C "$repo" rev-parse HEAD)
printf 'name: rust\n# e2e_workers: only a comment, not an input\non: push\n' > "$repo/.github/workflows/rust.yml"
git -C "$repo" -c core.hooksPath=/dev/null commit -qam without-input
sha_noinput=$(git -C "$repo" rev-parse HEAD)
git -C "$repo" remote add origin "$bare"

ctl() { (cd "$repo" && bash "$SCRIPT" "$@"); }
rows() { grep -vc '^workers' "$1" || true; }

echo "e2e-worker-control.sh"

# ── 1. real capped dispatch: dispatch <sha> 2 --max-inflight 2 ───────────────
reset_state
rc=0; ctl dispatch "$sha" 2 --max-inflight 2 --out "$TMP/capped.tsv" > "$TMP/capped.out" 2> "$TMP/capped.err" || rc=$?
check "capped dispatch exits 0 (got $rc)" "$([ "$rc" = 0 ] && echo 0 || echo 1)"
check "capped dispatch records 4 rows" "$([ "$(rows "$TMP/capped.tsv" 2>/dev/null)" = 4 ] && echo 0 || echo 1)"
expect=$(printf '1\t1001\t%s\t%s\n2\t1002\t%s\t%s\n1\t1003\t%s\t%s\n2\t1004\t%s\t%s' \
  "$sha" "$branch" "$sha" "$branch" "$sha" "$branch" "$sha" "$branch")
got=$(grep -v '^workers' "$TMP/capped.tsv" 2>/dev/null || true)
check "rows carry the run ids, interleaved settings, sha and branch" "$([ "$got" = "$expect" ] && echo 0 || echo 1)"
n_dispatch=$(grep -c '^workflow-run' "$STATE/gh.log" 2>/dev/null || true)
check "gh workflow run called 4 times (got $n_dispatch)" "$([ "$n_dispatch" = 4 ] && echo 0 || echo 1)"
over=$(awk '/^workflow-run/ {split($2, a, "="); if (a[2] + 0 >= 2) print}' "$STATE/gh.log" 2>/dev/null || true)
check "no dispatch started with 2 runs already in flight" "$([ "$n_dispatch" = 4 ] && [ -z "$over" ] && echo 0 || echo 1)"
check "the script waited for a slot before proceeding" \
  "$(grep -q 'in flight on .*(cap 2); checking again in 60s' "$TMP/capped.err" && grep -q '^sleep 60$' "$STATE/sleep.log" && echo 0 || echo 1)"
wrongref=$(grep '^workflow-run' "$STATE/gh.log" 2>/dev/null | grep -vF -- "--ref $branch" || true)
check "every dispatch targets the control branch" "$([ "$n_dispatch" = 4 ] && [ -z "$wrongref" ] && echo 0 || echo 1)"
pushed=$(git ls-remote --heads "$bare" "refs/heads/$branch" | cut -f1)
check "control branch pushed to the local bare remote at the SHA" "$([ "$pushed" = "$sha" ] && echo 0 || echo 1)"
check "the bare remote holds only the control branch" "$([ "$(git ls-remote --heads "$bare" | wc -l | tr -d ' ')" = 1 ] && echo 0 || echo 1)"

# ── 2. B1: the run-id loop header, URL and fallback paths ────────────────────
reset_state
rc=0; ctl dispatch "$sha" 1 --out "$TMP/b1.tsv" > /dev/null 2> "$TMP/b1.err" || rc=$?
check "B1: dispatch with the run URL printed exits 0 (got $rc)" "$([ "$rc" = 0 ] && echo 0 || echo 1)"
check "B1: both parsed run ids recorded" \
  "$([ "$(grep -v '^workers' "$TMP/b1.tsv" 2>/dev/null | cut -f1,2 | tr '\t\n' ': ')" = "1:1001 2:1002 " ] && echo 0 || echo 1)"
check "B1: no unbound-variable error" "$(grep -q 'unbound variable' "$TMP/b1.err" && echo 1 || echo 0)"
reset_state
rc=0; FAKE_GH_NO_URL=1 ctl dispatch "$sha" 1 --out "$TMP/b1-fallback.tsv" > /dev/null 2>&1 || rc=$?
check "B1: without a URL the id is found by listing runs, exit 0 (got $rc)" \
  "$([ "$rc" = 0 ] && [ "$(grep -v '^workers' "$TMP/b1-fallback.tsv" 2>/dev/null | cut -f2 | tr '\n' ' ')" = "1001 1002 " ] && echo 0 || echo 1)"

# ── 3. a dispatch whose run never shows up is recorded, not lost ─────────────
reset_state
rc=0; FAKE_GH_NO_URL=1 FAKE_GH_HIDE_NEW=1 ctl dispatch "$sha" 1 --out "$TMP/unknown.tsv" > /dev/null 2> "$TMP/unknown.err" || rc=$?
check "untraceable dispatch exits 1 (got $rc)" "$([ "$rc" = 1 ] && echo 0 || echo 1)"
check "untraceable dispatch is a row with run id 'unknown'" \
  "$([ "$(grep -v '^workers' "$TMP/unknown.tsv" 2>/dev/null)" = "$(printf '1\tunknown\t%s\t%s' "$sha" "$branch")" ] && echo 0 || echo 1)"
check "no further dispatch after the untraceable one" "$([ "$(grep -c '^workflow-run' "$STATE/gh.log" 2>/dev/null || true)" = 1 ] && echo 0 || echo 1)"
rc=0; ctl report "$TMP/unknown.tsv" --dir "$TMP/unknown-report" > /dev/null 2> "$TMP/unknown-report.err" || rc=$?
check "report refuses a TSV holding an 'unknown' run id" \
  "$([ "$rc" = 1 ] && grep -q "run id 'unknown'" "$TMP/unknown-report.err" && echo 0 || echo 1)"

# ── 4. in-flight cap gives up after its bound, dispatching nothing more ──────
reset_state
printf '900 in_progress\n901 queued\n' > "$STATE/runs"
rc=0; FAKE_SLEEP_NOOP=1 ctl dispatch "$sha" 1 --max-inflight 2 --out "$TMP/stuck.tsv" > /dev/null 2> "$TMP/stuck.err" || rc=$?
check "a slot that never frees exits 1 after 180 checks with no dispatch" \
  "$([ "$rc" = 1 ] && grep -q 'giving up' "$TMP/stuck.err" && [ "$(grep -c '^sleep 60$' "$STATE/sleep.log")" = 180 ] && [ ! -s "$STATE/gh.log" ] && echo 0 || echo 1)"
rc=0; ctl dispatch "$sha" 1 --max-inflight 0 --dry-run > /dev/null 2>&1 || rc=$?
check "--max-inflight 0 is a usage error (got $rc)" "$([ "$rc" = 2 ] && echo 0 || echo 1)"

# ── 5. N1: the input check never falsely refuses ─────────────────────────────
reset_state
refused=0
for i in $(seq 1 200); do
  ctl dispatch "$sha_large" 1 --dry-run > /dev/null 2>&1 || refused=$((refused + 1))
done
check "N1: 200 input checks on a SHA with the input, $refused refused" "$([ "$refused" = 0 ] && echo 0 || echo 1)"
rc=0; ctl dispatch "$sha_noinput" 1 --dry-run > /dev/null 2> "$TMP/noinput.err" || rc=$?
check "N1: a SHA whose only e2e_workers is a comment is refused" \
  "$([ "$rc" = 1 ] && grep -q 'has no e2e_workers input' "$TMP/noinput.err" && echo 0 || echo 1)"
check "the 200 dry runs called no gh and pushed nothing" \
  "$([ ! -e "$STATE/gh.log" ] && [ -z "$(git ls-remote --heads "$bare" "refs/heads/e2e-control/${sha_large:0:12}")" ] && echo 0 || echo 1)"

# ── 6. N3: report with no measured run exits nonzero and says why ────────────
printf 'workers\trun_id\tsha\tbranch\n1\t1001\t%s\t%s\n2\t1002\t%s\t%s\n' "$sha" "$branch" "$sha" "$branch" > "$TMP/n3.tsv"
rc=0; ctl report "$TMP/n3.tsv" --dir "$TMP/n3-report" > /dev/null 2> "$TMP/n3.err" || rc=$?
check "N3: report with no measured run exits 1 (got $rc)" "$([ "$rc" = 1 ] && echo 0 || echo 1)"
check "N3: report names why" "$(grep -q 'no run was measured' "$TMP/n3.err" && echo 0 || echo 1)"

# ── 7. GH_REPO unset: the repository comes from REMOTE, never gh's guess ─────
# The fixture mirrors the failing checkout: origin is the fork (a GitHub URL,
# rewritten to the local bare repo for git), upstream is mixpeek/amux.
git -C "$repo" remote set-url origin https://github.com/acme/fork.git
git -C "$repo" config url."$bare".insteadOf https://github.com/acme/fork.git
git -C "$repo" config --add url."$bare".insteadOf git@github.com:acme/fork.git
git -C "$repo" remote add upstream https://github.com/mixpeek/amux
sha7=$(git -C "$repo" rev-parse "$sha")
reset_state
rc=0; (unset GH_REPO; ctl dispatch "$sha7" 1 --workers 1 --max-inflight 2 --out "$TMP/seven.tsv") > /dev/null 2> "$TMP/seven.err" || rc=$?
check "7: dispatch with GH_REPO unset exits 0 (got $rc)" "$([ "$rc" = 0 ] && echo 0 || echo 1)"
check "7: every gh dispatch targeted the fork, none upstream or unset" \
  "$([ "$(grep -c '^workflow-run' "$STATE/gh.log" 2>/dev/null || true)" = 1 ] && ! grep '^workflow-run' "$STATE/gh.log" | grep -qv 'repo=acme/fork ' && echo 0 || echo 1)"
check "7: the resolved repository is printed" "$(grep -q 'repository acme/fork$' "$TMP/seven.err" && echo 0 || echo 1)"
git -C "$repo" push -q "$bare" --delete "e2e-control/${sha7:0:12}" 2>/dev/null || true
git -C "$repo" remote set-url origin git@github.com:acme/fork.git
rc=0; (unset GH_REPO; ctl dispatch "$sha7" 1 --dry-run) > /dev/null 2> "$TMP/seven-ssh.err" || rc=$?
check "7: an ssh remote resolves to the same repository" "$([ "$rc" = 0 ] && grep -q 'repository acme/fork$' "$TMP/seven-ssh.err" && echo 0 || echo 1)"
rc=0; (export GH_REPO=other/explicit; ctl dispatch "$sha7" 1 --dry-run) > /dev/null 2> "$TMP/seven-explicit.err" || rc=$?
check "7: an explicit GH_REPO wins" "$([ "$rc" = 0 ] && grep -q 'repository other/explicit$' "$TMP/seven-explicit.err" && echo 0 || echo 1)"
git -C "$repo" remote set-url origin https://github.com/acme/fork.git/
git -C "$repo" config --add url."$bare".insteadOf https://github.com/acme/fork.git/
rc=0; (unset GH_REPO; ctl dispatch "$sha7" 1 --dry-run) > /dev/null 2> "$TMP/seven-slash.err" || rc=$?
check "7: a URL ending .git/ resolves to the repository, not <repo>.git" "$([ "$rc" = 0 ] && grep -q 'repository acme/fork$' "$TMP/seven-slash.err" && echo 0 || echo 1)"
git -C "$repo" remote set-url origin https://x-token:SECRET-TOKEN@github.com/acme/fork.git
git -C "$repo" config --add url."$bare".insteadOf https://x-token:SECRET-TOKEN@github.com/acme/fork.git
rc=0; (unset GH_REPO; ctl dispatch "$sha7" 1 --dry-run) > /dev/null 2> "$TMP/seven-cred.err" || rc=$?
check "7: a credentialed URL resolves and its token is never printed" \
  "$([ "$rc" = 0 ] && grep -q 'repository acme/fork$' "$TMP/seven-cred.err" && ! grep -q 'SECRET-TOKEN' "$TMP/seven-cred.err" && echo 0 || echo 1)"
git -C "$repo" remote set-url origin "$bare"
rc=0; (unset GH_REPO; ctl dispatch "$sha7" 1 --dry-run) > /dev/null 2> "$TMP/seven-local.err" || rc=$?
check "7: a non-GitHub remote with GH_REPO unset is refused, not guessed (got $rc)" \
  "$([ "$rc" = 1 ] && grep -q 'is not a GitHub URL; set GH_REPO' "$TMP/seven-local.err" && echo 0 || echo 1)"

echo "test-e2e-worker-control: $PASS passed, $FAIL failed"
[ "$FAIL" = 0 ]
