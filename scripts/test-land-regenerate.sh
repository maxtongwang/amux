#!/usr/bin/env bash
# test-land-regenerate.sh [amux]: a regenerable replay re-renders the derived
# files from the pushed tree when the repo declares amux.landRegenerate
# (gs12-data, 2026-10-02: the lane changed an INPUT of a generated file, main
# re-rendered it too, the replay kept main's copy and the push carried a stale
# render the hook refused, four times). Throwaway repos under a temp dir; the
# control (NO_REGEN=1) must push the stale render, so the check can fail.
set -euo pipefail
# Hermetic: never the live server or the real ~/.amux (lib/test-isolation.sh).
# shellcheck source=lib/test-isolation.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib/test-isolation.sh"; amux_test_isolate "$(mktemp -d)" || exit 1
AM="${1:-$(cd "$(dirname "$0")/.." && pwd)/amux}"; AM="$(cd "$(dirname "$AM")" && pwd)/$(basename "$AM")"; HOME_LOG="$HOME/.amux/logs"; mkdir -p "$HOME_LOG/land"; T="$(mktemp -d "${TMPDIR:-/tmp}/lrg.XXXXXX")"; cd "$T"
git init -q --bare --initial-branch=main origin.git
git clone -q origin.git main 2>/dev/null; cd main
git config user.email t@t; git config user.name t
printf 'cat in.txt other.txt | cksum > gen/out.txt\n' > gen.sh
echo a > in.txt; echo x > other.txt; mkdir gen; sh gen.sh
git add -A; git commit -qm base; git push -q origin HEAD:main 2>/dev/null
cd "$T"; git clone -q origin.git lane 2>/dev/null; cd lane
git config user.email t@t; git config user.name t
echo b > in.txt; sh gen.sh; git commit -qm "lane: change in.txt" -- in.txt gen/out.txt
L="$(git rev-parse HEAD)"
cd "$T/main"; echo y > other.txt; sh gen.sh; git commit -qm "main: change other.txt" -- other.txt gen/out.txt; git push -q origin HEAD:main 2>/dev/null
cd "$T/lane"
git config --add amux.landRegenerable 'gen/*'
[ "${NO_REGEN:-0}" = 1 ] || git config amux.landRegenerate 'sh gen.sh'
git config --add amux.landRegenerateInput gen.sh
git config --add amux.landRegenerateInput in.txt
git config --add amux.landRegenerateInput other.txt
git config --add amux.landRegenerateInput gen
AMUX_LAND_NOTIFY=0 AMUX_WORKER=lrg bash "$AM" land --sha "$L" >/dev/null 2>&1 || echo "land rc=$?"
git fetch -q origin main
got="$(git show origin/main:gen/out.txt)"
want="$(printf 'b\ny\n' | cksum)"
rm -rf -- "${T:?}"
if [ "${NO_REGEN:-0}" = 1 ]; then
  [ "$got" != "$want" ] && { echo "ok   control: without amux.landRegenerate the stale render is pushed"; exit 0; }
  echo "FAIL control: a fresh render without amux.landRegenerate means this test cannot fail"; exit 1
fi
[ "$got" = "$want" ] && { echo "ok   regenerable replay re-rendered the derived file from the pushed tree"; exit 0; }
echo "FAIL: pushed '$got', want '$want' (the regenerate step did not run or did not commit)"; exit 1

