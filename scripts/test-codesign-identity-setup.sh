#!/usr/bin/env bash
# The one-time owner setup, against a DISPOSABLE keychain only: its dry-run plan must be free of
# trust, login-keychain and UI operations; then it runs for real twice (create, then verify only).
set -euo pipefail
repo="$(cd "$(dirname "$0")/.." && pwd)"
[ "$(uname -s)" = Darwin ] || { echo 'SKIP: macOS only'; exit 0; }
work=$(mktemp -d "${TMPDIR:-/tmp}/amux-codesign.XXXXXX")
work=$(cd "$work" && pwd -P)
keychain="$work/k.keychain-db"
password=$(openssl rand -hex 16)
source "$repo/scripts/codesign-keychain-guard.sh"
cleanup() {
  local status=0
  codesign_ci_restore_keychains || status=$?
  security delete-keychain "$keychain" >/dev/null 2>&1 || status=$?
  rm -rf "$work"
  return "$status"
}
trap cleanup EXIT
export AMUX_CODESIGN_TEST_DIR="$work" AMUX_CODESIGN_TEST_PASSWORD="$password"
plan=$(DRY_RUN=1 bash "$repo/scripts/codesign-identity-setup.sh" 2>&1)
forbidden='add-'"trusted"'|trust-'"settings"'|login.'"keychain"'|/Library/Keychains/'"System"'|default-'"keychain"'|list-'"keychains"'|login-'"keychain"'|osa'"script"'|tcc'"util"'|open -a'
if printf '%s\n' "$plan" | grep -Eiq "$forbidden"; then echo 'FAIL: setup plan contains a forbidden operation'; printf '%s\n' "$plan"; exit 1; fi
printf '%s\n' "$plan" | grep -q 'PLAN: security import' || { echo 'FAIL: plan has no import step'; exit 1; }
echo 'PASS: setup plan touches only the disposable keychain'
security create-keychain -p "$password" "$keychain"
security unlock-keychain -p "$password" "$keychain"
codesign_ci_add_test_keychain "$keychain"
out1=$(perl -e 'alarm 60; exec @ARGV' bash "$repo/scripts/codesign-identity-setup.sh" 2>&1) || { echo "FAIL: first run"; printf '%s\n' "$out1"; exit 1; }
grep -q 'creating the identity' <<<"$out1" && grep -q 'OK: the identity is present and signs' <<<"$out1" && grep -q 'Full Disk Access' <<<"$out1" \
  && echo 'PASS: first run creates the identity, verifies a signature and prints the Full Disk Access step' \
  || { echo 'FAIL: first run output'; printf '%s\n' "$out1"; exit 1; }
out2=$(perl -e 'alarm 60; exec @ARGV' bash "$repo/scripts/codesign-identity-setup.sh" 2>&1) || { echo "FAIL: second run"; printf '%s\n' "$out2"; exit 1; }
grep -q 'already exists' <<<"$out2" && grep -q 'OK: the identity is present and signs' <<<"$out2" \
  && echo 'PASS: a second run keeps the identity and only verifies' || { echo 'FAIL: second run output'; printf '%s\n' "$out2"; exit 1; }
n=$(security find-identity -p codesigning "$keychain" | grep -c '"amux local code signing"')
[ "$n" -eq 1 ] && echo 'PASS: exactly one identity after two runs' || { echo "FAIL: $n identities"; exit 1; }
