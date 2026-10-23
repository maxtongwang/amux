#!/usr/bin/env bash
# test-land-cancel-shared-checkout.sh [amux]: in a SHARED checkout, a plain
# `amux land --cancel` stops only the caller's own lands. gtm-videos' cancel
# stopped mixpeek-override's two detached lands at 20:14Z on 2026-10-02
# because the unrecorded-land fallback matched on cwd alone. Three fake lands
# run from one checkout: another lane's (detached record), the caller's own
# (queue ticket), and one nobody recorded. Only the caller's may stop.
set -uo pipefail
AM="${1:-$(cd "$(dirname "$0")/.." && pwd)/amux}"
AM="$(cd "$(dirname "$AM")" && pwd)/$(basename "$AM")"
H="$(mktemp -d "${TMPDIR:-/tmp}/lcs.XXXXXX")"
# Hermetic: never the live server or the real ~/.amux (lib/test-isolation.sh).
# shellcheck source=lib/test-isolation.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib/test-isolation.sh"; amux_test_isolate "$H" || exit 1
mkdir -p "$H/.amux/locks/detached" "$H/.amux/locks/land-k.q" "$H/.amux/logs/land" "$H/repo"
git -C "$H/repo" init -q
printf '#!/usr/bin/env bash\nsleep 300\n' > "$H/amux"; chmod +x "$H/amux"
# $! is the subshell's pid, not the land's, so each fake carries a tag and its
# real pid is read back from ps.
start() { (cd "$H/repo" && perl -e 'setpgrp(0,0); exec @ARGV' bash "$H/amux" land "$1") >/dev/null 2>&1 & }
start tag-other; start tag-mine; start tag-nobody
sleep 1
pidof_tag() { ps -Ao pid=,command= | awk -v t="$H/amux land $1" 'index($0, t) {print $1; exit}'; }
OTHER="$(pidof_tag tag-other)"; MINE="$(pidof_tag tag-mine)"; NOBODY="$(pidof_tag tag-nobody)"
printf 'lane-other\n%s\n%s\n' "$H/x.log" "$H/repo" > "$H/.amux/locks/detached/$OTHER"
printf 'me\nabc\n%s/.git\n1\n2026-10-02.31\n' "$H/repo" > "$H/.amux/locks/land-k.q/001790000000-$MINE"
out="$(cd "$H/repo" && HOME="$H" AMUX_WORKER=me bash "$AM" land --cancel 2>&1)"
sleep 4
fail=0
alive() { kill -0 "$1" 2>/dev/null && echo yes || echo no; }
[ "$(alive "$OTHER")" = yes ] && echo "ok   another lane's detached land left alone" || { echo "FAIL another lane's land was stopped"; fail=1; }
[ "$(alive "$NOBODY")" = yes ] && echo "ok   a land nobody recorded left alone" || { echo "FAIL an unrecorded land was stopped"; fail=1; }
[ "$(alive "$MINE")" = no ] && echo "ok   the caller's own land stopped" || { echo "FAIL the caller's land is still running: $out"; fail=1; }
printf '%s\n' "$out" | grep -q "belongs to lane-other; not cancelled" && echo "ok   the refusal names the owner" || { echo "FAIL no owner named: $out"; fail=1; }
kill -TERM -- "-$OTHER" "-$NOBODY" "-$MINE" 2>/dev/null; wait 2>/dev/null
rm -rf -- "${H:?}"
exit $fail
