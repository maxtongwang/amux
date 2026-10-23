#!/usr/bin/env bash
# test-land-heal-core-worktree.sh [amux]: a push hook writes core.worktree=<the
# holder's worktree> into the SHARED config (the way `git init` under GIT_DIR +
# GIT_WORK_TREE does; seen twice on Mixpeek on 2026-10-03), and the land
# holder removes it after the push and logs the phase.
set -uo pipefail
AM="${1:-$(cd "$(dirname "$0")/.." && pwd)/amux}"
AM="$(cd "$(dirname "$AM")" && pwd)/$(basename "$AM")"
H="$(mktemp -d "${TMPDIR:-/tmp}/lhc.XXXXXX")"; mkdir -p "$H/.amux/logs/land"
# Hermetic: never the live server or the real ~/.amux (lib/test-isolation.sh).
# shellcheck source=lib/test-isolation.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib/test-isolation.sh"; amux_test_isolate "$H" || exit 1
cd "$H" || exit 1
git init -q --bare --initial-branch=main origin.git
git clone -q origin.git main 2>/dev/null; cd main || exit 1
git config user.email t@t; git config user.name t
echo a > f; git add f; git commit -qm base; git push -q origin HEAD:main 2>/dev/null
git worktree add -q "$H/wt" -b lane 2>/dev/null
cd "$H/wt" || exit 1
echo b > f; git commit -qm change -- f
CD="$(git rev-parse --path-format=absolute --git-common-dir)"
mkdir -p "$CD/hooks"
printf '#!/bin/sh\ngit --git-dir="%s" --work-tree="%s" init -q >/dev/null 2>&1\nexit 0\n' "$CD" "$H/wt" > "$CD/hooks/pre-push"
chmod +x "$CD/hooks/pre-push"
HOME="$H" AMUX_LAND_NOTIFY=0 AMUX_WORKER=lhc bash "$AM" land >/dev/null 2>&1; rc=$?
fail=0
[ "$rc" = 0 ] && echo "ok   the land pushed" || { echo "FAIL land rc=$rc"; fail=1; }
v="$(git config --file "$CD/config" --get core.worktree || true)"
[ -z "$v" ] && echo "ok   the shared config has no core.worktree after the land" || { echo "FAIL core.worktree left: $v"; fail=1; }
grep -q "WARN removed core.worktree=.* after the push and its hook" "$H/.amux/logs/land.log" && echo "ok   land.log names the phase that wrote it" || { echo "FAIL no heal log line"; fail=1; }
cd / && rm -rf -- "${H:?}"
exit $fail
