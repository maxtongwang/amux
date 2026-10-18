#!/usr/bin/env bash
# The builder installs the elected commit's scripts/worker-bin/tmux into <amux home>/worker-bin
# (DMIW-11): installs it when absent, replaces it by rename when it differs (new inode), writes
# nothing when it matches, leaves a symlinked copy alone, and refuses bytes that do not parse.
# Runs the shipped function lifted from rust-auto-build.sh, not a paraphrase. No tmux runs.
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
TMP=$(mktemp -d); trap 'rm -rf "$TMP"' EXIT
PASS=0; FAIL=0
expect() { if "$@"; then PASS=$((PASS + 1)); else FAIL=$((FAIL + 1)); echo "FAIL: $*"; fi; }

awk '/^sync_worker_bin_guard\(\) \{/,/^}$/' "$ROOT/scripts/rust-auto-build.sh" > "$TMP/fn.sh"
expect test -s "$TMP/fn.sh"
expect grep -q '^  sync_worker_bin_guard "\$built_sha"$' "$ROOT/scripts/rust-auto-build.sh"
REPO="$TMP/repo"; LOG="$TMP/log"; DIR="$TMP/home/worker-bin"; DEST="$DIR/tmux"
git init -q "$REPO"; mkdir -p "$REPO/scripts/worker-bin"
commit() { printf '%s\n' "$1" > "$REPO/scripts/worker-bin/tmux"
  git -C "$REPO" add -A; git -C "$REPO" -c user.name=t -c user.email=t@e commit -qm x; git -C "$REPO" rev-parse HEAD; }
run() { REPO="$REPO" LOG="$LOG" AMUX_WORKER_BIN_DIR="$DIR" bash -c "source '$TMP/fn.sh'; sync_worker_bin_guard $1"; }
inode() { stat -f %i "$1" 2>/dev/null || stat -c %i "$1"; }
mode() { stat -f %Lp "$1" 2>/dev/null || stat -c %a "$1"; }

V1=$(commit 'echo guard v1')
run "$V1"                                        # absent: the bug this fixes
expect grep -qx 'echo guard v1' "$DEST"
expect test "$(mode "$DEST")" = 755
expect grep -q 'WORKER GUARD installed.*verdict=worker_guard_synced' "$LOG"

: > "$LOG"; run "$V1"                            # identical: no write, no log
expect test ! -s "$LOG"

V2=$(commit 'echo guard v2')
INODE=$(inode "$DEST")
run "$V2"
expect grep -qx 'echo guard v2' "$DEST"
expect test "$(inode "$DEST")" != "$INODE"
expect grep -q 'WORKER GUARD updated.*verdict=worker_guard_synced' "$LOG"

BAD=$(commit 'if then fi (')
run "$BAD"
expect grep -qx 'echo guard v2' "$DEST"
expect grep -q 'verdict=worker_guard_invalid' "$LOG"

echo 'echo pinned' > "$TMP/pinned"; rm -f "$DEST"; ln -s "$TMP/pinned" "$DEST"
run "$V1"
expect test -L "$DEST"
expect grep -qx 'echo pinned' "$TMP/pinned"
expect grep -q 'verdict=worker_guard_symlink_left' "$LOG"

expect test -z "$(find "$DIR" -name '.tmux.*' -print)"   # no temp files left behind

echo "test-worker-guard-sync: $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
