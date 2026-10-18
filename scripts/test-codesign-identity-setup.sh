#!/usr/bin/env bash
# Hosted macOS proof: setup uses only a disposable keychain and preserves login keychain state.
set -euo pipefail
repo="$(cd "$(dirname "$0")/.." && pwd)"
[ "$(uname -s)" = Darwin ] || { echo 'SKIP: macOS only'; exit 0; }
work=$(mktemp -d "${TMPDIR:-/tmp}/amux-codesign.XXXXXX")
work=$(cd "$work" && pwd -P)
export AMUX_CODESIGN_TEST_DIR="$work"
keychain="$HOME/Library/Keychains/amux-codesign-ci-$(basename "$work").keychain-db"
export AMUX_CODESIGN_TEST_KEYCHAIN="$keychain"
source "$repo/scripts/codesign-keychain-guard.sh"
require_test_keychain "$keychain" || exit 2
cleanup() {
  if [ -f "$work/before.search" ]; then
    read_user_search_list "$(cat "$work/before.search")" && security list-keychains -d user -s "${USER_SEARCH_LIST[@]}" >/dev/null
  fi
  security delete-keychain "$keychain" >/dev/null 2>&1 || true
  rm -rf "$work"
}
trap cleanup EXIT
snapshot() {
  security list-keychains -d user > "$work/$1.search"
  security default-keychain -d user > "$work/$1.default"
  login=$(security default-keychain -d user | tr -d '"[:space:]')
  security dump-keychain "$login" | shasum -a 256 > "$work/$1.login"
}
plan=$(DRY_RUN=1 bash "$repo/scripts/codesign-identity-setup.sh" 2>&1)
forbidden='add-trusted-cert|trust-settings|default-keychain|delete-keychain|osascript|tccutil|open -a'
if printf '%s\n' "$plan" | grep -Eiq "$forbidden"; then echo 'FAIL: forbidden real setup command'; exit 1; fi
check_identity_plan "$plan"
prior='"/Users/runner/Library/Keychains/login keychain-db" "/Users/runner/Library/Keychains/second.keychain-db"'
plan=$(AMUX_CODESIGN_PLAN_SEARCH_LIST="$prior" DRY_RUN=1 bash "$repo/scripts/codesign-identity-setup.sh" 2>&1)
check_search_list_plan "$plan" "$prior" "$keychain"
while IFS= read -r line; do
  case "$line" in
    'PLAN: security list-keychains -d user') ;;
    'PLAN: security '*|'PLAN: codesign --force '*)
      [[ "$line" == *"$keychain"* ]] || { echo "FAIL: command omits dedicated keychain: $line"; exit 1; }
      ;;
  esac
done <<< "$plan"
printf '%s\n' "$plan" | grep -q 'PLAN: security set-keychain-settings'
printf '%s\n' "$plan" | grep -q 'PLAN: security unlock-keychain'
echo 'PASS: real setup plan uses only the dedicated keychain and has no forbidden command'
snapshot before
out=$(perl -e 'alarm 90; exec @ARGV' bash "$repo/scripts/codesign-identity-setup.sh" 2>&1) || { printf '%s\n' "$out"; exit 1; }
grep -q 'PASS: dedicated signing identity signs' <<< "$out" || { printf '%s\n' "$out"; exit 1; }
grep -q 'verdict=codesign_search_list_appended' <<< "$out"
grep -q 'PASS: probe Authority=amux local code signing' <<< "$out"
echo 'PASS: setup created and signed from the disposable dedicated keychain'
snapshot after_first
read_user_search_list "$(cat "$work/before.search")"
before_entries=("${USER_SEARCH_LIST[@]}")
read_user_search_list "$(cat "$work/after_first.search")"
[ "${#USER_SEARCH_LIST[@]}" -eq "$((${#before_entries[@]} + 1))" ]
for i in "${!before_entries[@]}"; do [ "${USER_SEARCH_LIST[$i]}" = "${before_entries[$i]}" ]; done
[ "${USER_SEARCH_LIST[${#before_entries[@]}]}" = "$keychain" ]
cmp "$work/before.default" "$work/after_first.default"
echo 'PASS: search list preserves prior order, appends dedicated keychain last; default unchanged'
out=$(perl -e 'alarm 90; exec @ARGV' bash "$repo/scripts/codesign-identity-setup.sh" 2>&1) || { printf '%s\n' "$out"; exit 1; }
grep -q 'PASS: dedicated signing identity signs' <<< "$out" || { printf '%s\n' "$out"; exit 1; }
grep -q 'verdict=codesign_search_list_present' <<< "$out"
if grep -q 'verdict=codesign_search_list_appended' <<< "$out"; then echo 'FAIL: rerun rewrote search list'; exit 1; fi
snapshot after
cmp "$work/after_first.search" "$work/after.search"
echo 'PASS: rerun search list byte-identical; verdict=present and no list-keychains -s'
identities=$(security find-identity -p codesigning "$keychain")
count=$(printf '%s\n' "$identities" | awk '/^[[:space:]]*[0-9]+\)/ {n++} END {print n+0}')
[ "$count" -eq 1 ] && grep -qF '"amux local code signing"' <<< "$identities" || {
  echo "FAIL: unexpected identity count $count"; exit 1;
}
echo 'PASS: dedicated keychain contains exactly one amux identity'
for part in default login; do cmp "$work/before.$part" "$work/after.$part"; done
echo 'PASS: default and login keychain dump hash unchanged'
