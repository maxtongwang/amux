#!/usr/bin/env bash
# Tests main-ci-watch.sh against a fake gh: a repeat red run updates the one
# issue, green closes it with a note, red again opens one new issue, and an
# unreadable tracker files nothing. Never touches real GitHub.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
T=$(mktemp -d); trap 'find "$T" -mindepth 1 -delete 2>/dev/null; rmdir "$T" 2>/dev/null || true' EXIT
mkdir -p "$T/bin"
cat > "$T/bin/gh" <<'G'
#!/usr/bin/env bash
# Fake gh. RUN_ROW is the newest run (tsv); issues live in $FAKE/issues (number<TAB>body-key).
case "$1 $2" in
  "api repos/mixpeek/amux/actions/workflows/rust.yml/runs"*) printf '%s\n' "$RUN_ROW" ;;
  "api repos/mixpeek/amux/actions/workflows/checks.yml/runs"*) : ;;
  "api repos/"*) : ;;
  "issue list") [ -e "$FAKE/fail" ] && exit 1; grep "ci-main|rust" "$FAKE/issues" 2>/dev/null | cut -f1 || true ;;
  "issue create") cat >/dev/null; n=$(( $(wc -l < "$FAKE/issues" 2>/dev/null || echo 0) + 100 )); printf '%s\tci-main|rust\n' "$n" > "$FAKE/issues"; echo create >> "$FAKE/calls" ;;
  "issue edit") cat >/dev/null; echo edit >> "$FAKE/calls" ;;
  "issue close") : > "$FAKE/issues"; echo close >> "$FAKE/calls" ;;
esac
G
chmod +x "$T/bin/gh"
printf '#!/usr/bin/env bash\nexit 0\n' > "$T/tok"; chmod +x "$T/tok"
export FAKE="$T" PATH="$T/bin:$PATH" AMUX_GET_TOKEN="$T/tok" MAIN_CI_WATCH_STATE="$T/state.json" AMUX_HOME="$T"
: > "$T/issues"; : > "$T/calls"
run() { RUN_ROW="$1" bash "$here/main-ci-watch.sh" >/dev/null 2>&1 || true; }
count() { grep -c "^$1$" "$T/calls" || true; }
fail=0
check() { if [ "$2" = "$3" ]; then echo "ok   $1"; else echo "FAIL $1: got $2 want $3"; fail=1; fi; }

run $'1\tfailure\tabcdef123456\thttp://x/1'
check "first red opens one issue" "$(count create)" 1
run $'2\tfailure\tabcdef123457\thttp://x/2'
run $'3\tfailure\tabcdef123458\thttp://x/3'
check "repeat red opens no new issue" "$(count create)" 1
check "repeat red updates in place" "$(count edit)" 2
run $'4\tsuccess\tabcdef123459\thttp://x/4'
check "green closes the issue" "$(count close)" 1
run $'5\tfailure\tabcdef12345a\thttp://x/5'
check "red after close opens one new issue" "$(count create)" 2
touch "$T/fail"
run $'6\tfailure\tabcdef12345b\thttp://x/6'
check "unreadable tracker files nothing" "$(count create)" 2
exit $fail
