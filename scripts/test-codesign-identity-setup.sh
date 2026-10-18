#!/usr/bin/env bash
# The one-time owner setup, against a DISPOSABLE keychain only: its dry-run plan must be free of
# trust, login-keychain and UI operations; then it checks failed setup and repair.
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
printf '%s\n' "$plan" | grep -q 'PLAN: security set-key-partition-list' || { echo 'FAIL: plan has no partition-list step'; exit 1; }
check_identity_plan "$plan" || exit 1
echo 'PASS: setup plan touches only the disposable keychain'
require_test_keychain "$keychain" || exit 2
security create-keychain -p "$password" "$keychain"
security unlock-keychain -p "$password" "$keychain"
codesign_ci_add_test_keychain "$keychain"

# The second signing key must retain its distinct access policy through failed setup and reruns.
unrelated='unrelated CI signing key'
openssl req -x509 -newkey rsa:2048 -nodes -days 1 -keyout "$work/unrelated.key" \
  -out "$work/unrelated.crt" -subj "/CN=$unrelated" >/dev/null 2>&1
openssl pkcs12 -export -name "$unrelated" -inkey "$work/unrelated.key" \
  -in "$work/unrelated.crt" -out "$work/unrelated.p12" -passout "pass:$password" \
  -keypbe PBE-SHA1-3DES -certpbe PBE-SHA1-3DES -macalg sha1
security import "$work/unrelated.p12" -k "$keychain" -P "$password" -T /usr/bin/codesign >/dev/null
security set-key-partition-list -S apple-tool:,apple: -s -t private -l "$unrelated" -k "$password" "$keychain" >/dev/null
security dump-keychain -a "$keychain" > "$work/before.dump"
partition_for_label() {
  python3 "$repo/scripts/keychain-partitions.py" "$1" "$2"
}
unrelated_before=$(partition_for_label "$unrelated" "$work/before.dump")
[[ "$unrelated_before" == *'apple-tool:'* ]] &&
  [[ "$unrelated_before" == *'apple:'* ]] &&
  [[ "$unrelated_before" != *'codesign:'* ]] || {
    echo "FAIL: unrelated key did not start with a distinct partition list: $unrelated_before" >&2
    exit 1
  }
status=0
out1=$(AMUX_CODESIGN_TEST_PASSWORD="wrong-$password" perl -e 'alarm 60; exec @ARGV' /bin/bash "$repo/scripts/codesign-identity-setup.sh" 2>&1) || status=$?
[ "$status" -eq 1 ] && grep -q 'creating the identity' <<<"$out1" && grep -q 'after 3 tries' <<<"$out1" \
  && echo 'PASS: wrong password leaves a repairable imported identity and exits 1' \
  || { echo 'FAIL: wrong-password setup'; printf '%s\n' "$out1"; exit 1; }
out2=$(AMUX_CODESIGN_TEST_WRONG_FIRST=1 perl -e 'alarm 60; exec @ARGV' /bin/bash "$repo/scripts/codesign-identity-setup.sh" 2>&1) || { echo 'FAIL: repair run'; printf '%s\n' "$out2"; exit 1; }
grep -q 'already exists' <<<"$out2" && grep -q 'did not match' <<<"$out2" && grep -q 'OK: the identity is present and signs' <<<"$out2" && grep -q 'Full Disk Access' <<<"$out2" \
  && echo 'PASS: rerun retries a wrong first password, repairs key access and verifies signing' \
  || { echo 'FAIL: repair run output'; printf '%s\n' "$out2"; exit 1; }
out2=$(perl -e 'alarm 60; exec @ARGV' bash "$repo/scripts/codesign-identity-setup.sh" 2>&1) || { echo "FAIL: second run"; printf '%s\n' "$out2"; exit 1; }
grep -q 'already exists' <<<"$out2" && grep -q 'OK: the identity is present and signs' <<<"$out2" \
  && echo 'PASS: a further run reapplies key access and verifies' || { echo 'FAIL: second run output'; printf '%s\n' "$out2"; exit 1; }
n=$(security find-identity -p codesigning "$keychain" | grep -c '"amux local code signing"')
[ "$n" -eq 1 ] && echo 'PASS: exactly one identity after three runs' || { echo "FAIL: $n identities"; exit 1; }
security dump-keychain -a "$keychain" > "$work/after.dump"
unrelated_after=$(partition_for_label "$unrelated" "$work/after.dump")
[ "$unrelated_before" = "$unrelated_after" ] || {
  echo 'FAIL: unrelated signing key partition list changed' >&2; exit 1;
}
amux_partition=$(partition_for_label 'amux local code signing' "$work/after.dump")
[[ "$amux_partition" == *'apple-tool:'* ]] &&
  [[ "$amux_partition" == *'apple:'* ]] &&
  [[ "$amux_partition" == *'codesign:'* ]] || {
  echo "FAIL: amux signing key lacks expected partition list: $amux_partition" >&2; exit 1;
}
echo "PASS: unrelated key partition list unchanged ($unrelated_after); amux key has signing access ($amux_partition)"
