#!/usr/bin/env bash
# test-land-regenerate-scoped.sh [amux]: a generator declared as
# `inputs=<pathspec> :: <cmd>` runs only when its own inputs changed; one broad
# input no longer runs every generator under the lock (MO-4354, 2026-10-04:
# 132 s of regenerate per landing while 16 lanes waited).
set -uo pipefail
AM="${1:-$(cd "$(dirname "$0")/.." && pwd)/amux}"
AM="$(cd "$(dirname "$AM")" && pwd)/$(basename "$AM")"
H="$(mktemp -d "${TMPDIR:-/tmp}/lrs.XXXXXX")"; mkdir -p "$H/.amux/logs/land" "$H/mark"
# Hermetic: never the live server or the real ~/.amux (lib/test-isolation.sh).
# shellcheck source=lib/test-isolation.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib/test-isolation.sh"; amux_test_isolate "$H" || exit 1
cd "$H" || exit 1
git init -q --bare --initial-branch=main origin.git
git clone -q origin.git lane 2>/dev/null; cd lane || exit 1
git config user.email t@t; git config user.name t
mkdir gen; echo a > a.txt; echo b > b.txt
printf 'touch "$MARK/ranA"; cksum < a.txt > gen/a.out\n' > genA.sh
printf 'touch "$MARK/ranB"; cksum < b.txt > gen/b.out\n' > genB.sh
cksum < a.txt > gen/a.out; cksum < b.txt > gen/b.out
git add a.txt b.txt genA.sh genB.sh gen/a.out gen/b.out; git commit -qm base; git push -q origin HEAD:main 2>/dev/null
echo a2 > a.txt; git commit -qm "lane: change a.txt only" -- a.txt
git config --add amux.landRegenerable 'gen/*'
git config --add amux.landRegenerate 'inputs=a.txt genA.sh :: sh genA.sh'
git config --add amux.landRegenerate 'inputs=b.txt genB.sh :: sh genB.sh'
for p in a.txt b.txt genA.sh genB.sh gen; do git config --add amux.landRegenerateInput "$p"; done
HOME="$H" MARK="$H/mark" AMUX_LAND_NOTIFY=0 AMUX_WORKER=lrs bash "$AM" land --tries 1 --no-batch >/dev/null 2>&1; rc=$?
git fetch -q origin
fail=0
[ "$rc" = 0 ] && [ "$(git show origin/main:a.txt)" = a2 ] && echo "ok   the lane landed" || { echo "FAIL land rc=$rc"; fail=1; }
[ -e "$H/mark/ranA" ] && echo "ok   the generator whose input changed ran" || { echo "FAIL genA did not run"; fail=1; }
[ ! -e "$H/mark/ranB" ] && echo "ok   the generator whose inputs did not change was skipped" || { echo "FAIL genB ran with unchanged inputs"; fail=1; }
[ "$(git show origin/main:gen/a.out)" = "$(echo a2 | cksum)" ] && echo "ok   the pushed render is fresh" || { echo "FAIL gen/a.out stale"; fail=1; }
grep -q 'regenerate legs: .*skipped sh genB.sh' "$H/.amux/logs/land.log" && echo "ok   land.log times each generator and names the skip" || { echo "FAIL no legs line: $(grep -a 'regenerate' "$H/.amux/logs/land.log" | tail -2)"; fail=1; }
cd / && rm -rf -- "${H:?}"
exit $fail
