#!/usr/bin/env bash
# test-land-regenerate-always.sh [amux] [when]: gs12-planes' 17:34Z batch on
# 2026-10-02 was refused because the COMPOSED tree left plane artifacts stale
# with no conflict anywhere. Declared generators now run on every pushed stack,
# in order. when=conflict is the control and must push the stale render.
# landregen2.sh <amux> [when]: no conflict, the lane's commit leaves gen/ stale,
# two chained generators. With when=always (default) the pushed tree must hold
# fresh renders of both; with when=conflict it must stay stale (the control).
set -euo pipefail
AM="${1:-$(cd "$(dirname "$0")/.." && pwd)/amux}"; WHEN="${2:-always}"
AM="$(cd "$(dirname "$AM")" && pwd)/$(basename "$AM")"
# Hermetic: never the live server or the real ~/.amux (lib/test-isolation.sh).
# shellcheck source=lib/test-isolation.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib/test-isolation.sh"; amux_test_isolate "$(mktemp -d)" || exit 1
T="$(mktemp -d "${TMPDIR:-/tmp}/lrg2.XXXXXX")"; cd "$T"
git init -q --bare --initial-branch=main origin.git
git clone -q origin.git main 2>/dev/null; cd main
git config user.email t@t; git config user.name t
printf 'cat in.txt | cksum > gen/out.txt\n' > gen1.sh
printf 'cksum < gen/out.txt > gen/two.txt\n' > gen2.sh
echo a > in.txt; mkdir gen; sh gen1.sh; sh gen2.sh
git add -A; git commit -qm base; git push -q origin HEAD:main 2>/dev/null
cd "$T"; git clone -q origin.git lane 2>/dev/null; cd lane
git config user.email t@t; git config user.name t
echo b > in.txt; git commit -qm "lane: change in.txt, no render" -- in.txt
L="$(git rev-parse HEAD)"
git config --add amux.landRegenerable 'gen/*'
if [ "$WHEN" = worktree ]; then
  # The generator's tool exists only in the PRIMARY checkout, untracked (like
  # server/.venv), and the land runs from a worktree: gs12-compute's lands
  # failed rc 127 at 23:26Z on 2026-10-02 when AMUX_LAND_REPO was the worktree.
  mkdir -p tools; cp gen1.sh tools/gen1-untracked.sh
  git config --add amux.landRegenerate 'sh "$AMUX_LAND_REPO"/tools/gen1-untracked.sh'
else
  git config --add amux.landRegenerate 'sh gen1.sh'
fi
git config --add amux.landRegenerate 'sh gen2.sh'
# when=failing: a generator that fails must not stop the land (land .27 stopped
# every Mixpeek holder at 20:16Z on 2026-10-02); the stack is pushed unrendered.
[ "$WHEN" = failing ] && git config --add amux.landRegenerate 'cat missing/components.overlay.yaml'
case "$WHEN" in always|failing|worktree) ;; *) git config amux.landRegenerateWhen "$WHEN" ;; esac
for p in gen1.sh gen2.sh in.txt gen; do git config --add amux.landRegenerateInput "$p"; done
if [ "$WHEN" = worktree ]; then
  git worktree add -q --detach "$T/wt" "$L" 2>/dev/null
  (cd "$T/wt" && AMUX_LAND_NOTIFY=0 AMUX_WORKER=lrg2 bash "$AM" land --sha "$L" >/dev/null 2>&1) || echo "land rc=$?"
else
  AMUX_LAND_NOTIFY=0 AMUX_WORKER=lrg2 bash "$AM" land --sha "$L" >/dev/null 2>&1 || echo "land rc=$?"
fi
git fetch -q origin main
landed=no; [ "$(git show origin/main:in.txt)" = b ] && landed=yes
stamped="$(git log -1 --format=%B origin/main | sed -n 's/^Amux-Session: //p')"
one="$(git show origin/main:gen/out.txt)"; two="$(git show origin/main:gen/two.txt)"
w1="$(echo b | cksum)"; w2="$(printf '%s\n' "$w1" | cksum)"
rm -rf -- "${T:?}"
if [ "$WHEN" = failing ]; then
  [ "$landed" = yes ] && { echo "ok   failing generator: the lane's commit still landed (pushed unrendered)"; exit 0; }
  echo "FAIL failing generator stopped the land"; exit 1
fi
if [ "$WHEN" = always ] || [ "$WHEN" = worktree ]; then
  [ "$stamped" = lrg2 ] || { echo "FAIL the regenerate commit carries no Amux-Session trailer (got '$stamped'); Mixpeek's session-stamp leg refuses it"; exit 1; }
  [ "$one" = "$w1" ] && [ "$two" = "$w2" ] && { echo "ok   always: both chained generators re-rendered on a stack with no conflict"; exit 0; }
  echo "FAIL always: out='$one' (want '$w1') two='$two' (want '$w2')"; exit 1
fi
[ "$one" != "$w1" ] && { echo "ok   control (when=$WHEN): no conflict, so the stale render is pushed"; exit 0; }
echo "FAIL control: rendered without a conflict under when=$WHEN"; exit 1
