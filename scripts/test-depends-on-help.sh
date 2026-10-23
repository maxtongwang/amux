#!/usr/bin/env bash
# AF-621 — a lane declaring `--depends-on` must be told what the link does NOT do.
#
# THE DEFECT: `board request` says "a terminal callback to the requester is armed
# by default", and `board add --depends-on` said only "link the new card to one it
# depends on". A reader carries the symmetry across and expects the board to push
# when a declared dependency clears. It does not: the parent-walk lives inside the
# terminal-callback dispatcher, behind `let Some(target) = row.callback_session`,
# and a lane cannot set that field by declaring a dependency. The card becomes
# eligible and board-drive picks it up on a later tick, so the real behaviour is
# latency, not loss — but nothing said so, and primis held a 45-minute heartbeat
# as a fallback specifically because they could not tell which it was.
#
# ASSERTS ON THE RENDERED OUTPUT, not the source. Both usage blocks are
# `cat <<EOF` with an UNQUOTED delimiter, so backticks in them are command
# substitution: my first cut wrote "use `board request`" and the help rendered
# "use ,". The source looked right and `bash -n` passed. Only running it shows it.
#
# Exit 0 = all pass, 1 = a failure.
set -euo pipefail
cd "$(dirname "$0")/.."
# Hermetic: never the live server or the real ~/.amux (lib/test-isolation.sh).
# shellcheck source=lib/test-isolation.sh
source scripts/lib/test-isolation.sh; amux_test_isolate "$(mktemp -d)" || exit 1
AMUX_BIN="${AMUX_BIN:-./amux}"
PASS=0; FAIL=0

check() {  # check <label> <haystack> <needle>
  if [[ "$2" == *"$3"* ]]; then PASS=$((PASS+1))
  else FAIL=$((FAIL+1)); printf 'FAIL %s: missing %q\n' "$1" "$3" >&2; fi
}
refute() {
  if [[ "$2" != *"$3"* ]]; then PASS=$((PASS+1))
  else FAIL=$((FAIL+1)); printf 'FAIL %s: found %q and should not have\n' "$1" "$3" >&2; fi
}

# `board request --help` exits 1 (usage-then-fail), `board add --help` exits 0.
# Under `set -e` an unguarded capture of the former kills the script before a
# single assertion runs, and the whole file reports nothing at all rather than a
# failure — which is how the first draft of this file "passed" by dying silently.
add_help="$("$AMUX_BIN" board add --help 2>&1 || true)"
req_help="$("$AMUX_BIN" board request --help 2>&1 || true)"

# (a) `add` states the non-push behaviour, and names the verb that DOES push.
check add-blocks   "$add_help" "BLOCKS the claim until the"
check add-nopush   "$add_help" "does NOT"
check add-remedy   "$add_help" "board request"

# (b) `request` separates ITS callback from the depends-on link, because that
#     block promises a callback two paragraphs earlier.
check req-separates "$req_help" "not this"
check req-nopush    "$req_help" "does not push"

# (c) THE RENDER CONTROL. A swallowed backtick leaves the sentence grammatical
#     and the recommendation absent, which is worse than a missing line: it
#     reads as complete. This is the exact failure this file was written after.
refute add-swallowed "$add_help" 'use ,'
refute req-swallowed "$req_help" 'use ,'

# (d) POSITIVE CONTROL. If --help stopped emitting anything, every check above
#     that uses `refute` would still pass and half the file would look green.
check add-nonempty "$add_help" "--depends-on"
check req-nonempty "$req_help" "--depends-on"

printf 'depends-on help: %d passed, %d failed\n' "$PASS" "$FAIL"
[[ "$FAIL" -eq 0 ]]
