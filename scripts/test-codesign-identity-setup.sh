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
forbidden='add-trusted-cert|trust-settings|login.keychain|list-keychains -s|default-keychain|delete-keychain|osascript|tccutil|open -a'
if printf '%s\n' "$plan" | grep -Eiq "$forbidden"; then echo 'FAIL: forbidden real setup command'; exit 1; fi
check_identity_plan "$plan"
while IFS= read -r line; do
  case "$line" in
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
echo 'PASS: setup created and signed from the disposable dedicated keychain'
out=$(perl -e 'alarm 90; exec @ARGV' bash "$repo/scripts/codesign-identity-setup.sh" 2>&1) || { printf '%s\n' "$out"; exit 1; }
grep -q 'PASS: dedicated signing identity signs' <<< "$out" || { printf '%s\n' "$out"; exit 1; }
echo 'PASS: second setup run is idempotent'
identities=$(security find-identity -p codesigning "$keychain")
count=$(printf '%s\n' "$identities" | awk '/^[[:space:]]*[0-9]+\)/ {n++} END {print n+0}')
[ "$count" -eq 1 ] && grep -qF '"amux local code signing"' <<< "$identities" || {
  echo "FAIL: unexpected identity count $count"; exit 1;
}
echo 'PASS: dedicated keychain contains exactly one amux identity'
snapshot after
for part in search default login; do cmp "$work/before.$part" "$work/after.$part"; done
echo 'PASS: login keychain search list, default, and dump hash unchanged'
