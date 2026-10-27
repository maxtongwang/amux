#!/usr/bin/env bash
# AF-635 — `--folded-into` must be findable by a lane that goes looking.
#
# THE DEFECT: the flag has worked since 99763834 and appeared on NEITHER help
# surface. gtm-engine hit three specimens in one session where a hand-discarded
# capture shell produced "closed the request without resolving the dependency ...
# evidence: not recorded" while the work sat safely on a sibling card, and when
# they went looking for the flag that fixes it they found nothing. A capability
# nobody can name is a capability nobody has (ethos rule 1).
#
# ASSERTS ON RENDERED OUTPUT, because these are heredocs and a swallowed
# backtick or a dropped continuation is invisible in the source (AF-621).
#
# Exit 0 = all pass, 1 = a failure.
set -euo pipefail
cd "$(dirname "$0")/.."
# Hermetic: never the live server or the real ~/.amux (lib/test-isolation.sh).
# shellcheck source=lib/test-isolation.sh
source scripts/lib/test-isolation.sh; amux_test_isolate "$(mktemp -d)" || exit 1
AMUX_BIN="${AMUX_BIN:-./amux}"
PASS=0; FAIL=0
check()  { if [[ "$2" == *"$3"* ]]; then PASS=$((PASS+1)); else FAIL=$((FAIL+1)); printf 'FAIL %s: missing %q\n' "$1" "$3" >&2; fi; }
refute() { if [[ "$2" != *"$3"* ]]; then PASS=$((PASS+1)); else FAIL=$((FAIL+1)); printf 'FAIL %s: found %q and should not have\n' "$1" "$3" >&2; fi; }

# (a) THE VERB LIST — where a lane looks before it guesses.
verbs="$("$AMUX_BIN" board --help 2>&1 || true)"
check verbs-flag   "$verbs" "--folded-into <ID>"
check verbs-why    "$verbs" "work moved to another"

# (b) THE UNKNOWN-FLAG USAGE — where a lane lands when it guesses wrong.
usage="$("$AMUX_BIN" board discard AF-0 --nosuchflag 2>&1 || true)"
check usage-flag   "$usage" "--folded-into <ID>"
check usage-scope  "$usage" "discard only"

# (b2) THE PER-VERB HELP, which is the surface a lane reads while ALREADY
#      running the verb. gtm-engine used the flag five times in a row the moment
#      they were told it exists, and reported that this is the page they had
#      looked at: `amux board discard --help` prints its own usage line, and it
#      named every other flag but this one. Two surfaces were fixed first and
#      this was still missing, which is why it gets its own cell.
verb_help="$("$AMUX_BIN" board discard --help 2>&1 || true)"
check verbhelp-flag "$verb_help" "--folded-into <TARGET-ID>"
check verbhelp-why  "$verb_help" "work moved to another card"

# (b3) AND IT MUST NOT LEAK ONTO THE OTHER VERBS. `--folded-into` is meaningful
#      only for a discard; printing it under `done` or `verified` would advertise
#      a flag those paths ignore, which is worse than silence.
for verb in done verified review; do
  other="$("$AMUX_BIN" board "$verb" --help 2>&1 || true)"
  refute "verbhelp-scope-$verb" "$other" "--folded-into"
done

# (c) THE FLAG STILL WORKS. Help that documents a flag the parser rejects is
#     worse than silence, so this drives the real dispatch path. It must NOT
#     die on the flag itself; AMUX_API points nowhere so nothing mutates a board.
out="$(AMUX_API='https://127.0.0.1:1' "$AMUX_BIN" board discard AF-0 --folded-into AF-1 2>&1 || true)"
if [[ "$out" == *"unknown flag"* ]]; then
  FAIL=$((FAIL+1)); printf 'FAIL parser: help names --folded-into but the parser rejects it\n' >&2
else
  PASS=$((PASS+1))
fi

# (d) POSITIVE CONTROL. If --help stopped emitting anything, (a) and (b) would
#     fail loudly, but a check that only ever greps for absence would not. This
#     pins that the surfaces are alive at all.
check verbs-alive "$verbs" "amux board discard"
check usage-alive "$usage" "Usage: amux board"

# A CELL COUNT, because a cell that VANISHES does not fail.
#
# The first version of the per-verb cells called `refute`, which this file never
# defined: bash printed "command not found", returned 127, and the run reported
# "9 passed, 0 failed" with three checks silently absent. Neither PASS nor FAIL
# moves when a helper is missing, so the total is the only thing that notices.
EXPECTED=12   # 7 original + 2 per-verb + 3 scope refutes
if [[ "$((PASS + FAIL))" -ne "$EXPECTED" ]]; then
  FAIL=$((FAIL+1))
  printf 'FAIL cell-count: ran %d check(s), expected %d — a cell vanished rather than failed\n' \
    "$((PASS + FAIL - 1))" "$EXPECTED" >&2
fi

printf 'discard --folded-into help: %d passed, %d failed\n' "$PASS" "$FAIL"
[[ "$FAIL" -eq 0 ]]
