#!/usr/bin/env bash
# The activation authority fetches $REF's remote before choosing a commit, so a green commit pushed
# to the remote is built without anyone fetching by hand; AMUX_AUTHORITY_FETCH=0 keeps the old
# behaviour. A bare repo stands in for GitHub and a stub builder records the elected commit.
# Usage: bash scripts/test-build-authority-fetch.sh   (exit 0 = all passed)
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
AUTHORITY="$ROOT/scripts/rust-auto-build-authority.sh"
TMP=$(mktemp -d); trap 'rm -rf "$TMP"' EXIT
PASS=0; FAIL=0
expect() { if eval "$1"; then PASS=$((PASS + 1)); else FAIL=$((FAIL + 1)); echo "FAIL: $2"; fi; }
g() { git -c user.email=t@t -c user.name=t "$@"; }

g init -q --bare -b main "$TMP/remote.git"
g init -q -b main "$TMP/seed"
mkdir -p "$TMP/seed/scripts"
cat > "$TMP/seed/scripts/rust-auto-build.sh" <<'SH'
#!/usr/bin/env bash
echo "$AMUX_RS_ACTIVATION_REF" > "$ELECTED"
SH
chmod +x "$TMP/seed/scripts/rust-auto-build.sh"
g -C "$TMP/seed" add -A && g -C "$TMP/seed" commit -q -m first
g -C "$TMP/seed" remote add origin "$TMP/remote.git" && g -C "$TMP/seed" push -q origin main
g clone -q "$TMP/remote.git" "$TMP/authority"
first=$(git -C "$TMP/authority" rev-parse HEAD)
echo two > "$TMP/seed/two" && g -C "$TMP/seed" add -A && g -C "$TMP/seed" commit -q -m second && g -C "$TMP/seed" push -q origin main
second=$(git -C "$TMP/seed" rev-parse HEAD)

run() {  # run <worktree-name> [env...]
  local wt="$TMP/$1"; shift
  env AMUX_AUTHORITY_REPO="$TMP/authority" AMUX_AUTHORITY_WORKTREE="$wt" AMUX_AUTHORITY_MARKER="$wt.authority" \
      AMUX_AUTHORITY_LOG="$TMP/log" AMUX_DEPLOY_CI_GATE=0 ELECTED="$TMP/elected" "$@" bash "$AUTHORITY"
}

run wt-off AMUX_AUTHORITY_FETCH=0
expect '[ "$(cat "$TMP/elected")" = "$first" ]' "with fetching off, the last fetched origin/main ($first) is built"
expect '[ "$(git -C "$TMP/authority" rev-parse origin/main)" = "$first" ]' "with fetching off, origin/main does not move"
rm -f "$TMP/elected"
run wt-on
expect '[ "$(cat "$TMP/elected")" = "$second" ]' "by default the remote is fetched and its new commit ($second) is built"
expect '! grep -q fetch_failed "$TMP/log"' "a working fetch logs no failure"
g -C "$TMP/authority" remote set-url origin "$TMP/missing.git"
rm -f "$TMP/elected"
run wt-fail
expect '[ "$(cat "$TMP/elected")" = "$second" ] && grep -q "verdict=fetch_failed" "$TMP/log"' "a failed fetch is logged and the last fetched origin/main is still built"

echo "$PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
