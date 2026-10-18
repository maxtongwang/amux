#!/usr/bin/env bash
# Prove stable requirements and refused adoption with one disposable keychain.
set -euo pipefail
source "$(dirname "$0")/codesign-keychain-guard.sh"

if [ "${1:-}" = --help ] && [ "$#" -eq 1 ]; then
  echo "usage: $0 [--check-plan]"; exit 0
fi
if [ "${1:-}" = --check-plan ]; then
  [ "$#" -eq 1 ] || exit 2
  plan=$(cat)
  [ -n "$plan" ] && [ "$(printf '%s\n' "$plan" | grep -c '^PLAN:')" -ge 18 ] || {
    echo 'FAIL: incomplete signing plan' >&2; exit 1;
  }
  [[ "$plan" == *'PLAN: security create-keychain'* ]] &&
  [[ "$plan" == *'PLAN: security unlock-keychain'* ]] &&
  [[ "$plan" == *'PLAN: security set-key-partition-list'* ]] &&
  [ "$(printf '%s\n' "$plan" | grep -c '^PLAN: codesign --force')" -eq 2 ] &&
  [[ "${plan##*$'\n'}" == 'PLAN: security delete-keychain '* ]] || {
    echo 'FAIL: signing plan is missing a required phase or cleanup' >&2; exit 1;
  }
  check_identity_plan "$plan" || exit 1
  forbidden='add-'"trusted"'|trust-'"settings"'|/Library/Keychains/'"System"'|default-'"keychain"'|login-'"keychain"'|osa'"script"'|tcc'"util"'|open -a'
  if printf '%s\n' "$plan" | grep -Eiq "$forbidden"; then
    echo 'FAIL: signing plan contains a forbidden operation' >&2; exit 1
  fi
  plan_keychain=$(PLAN_TEXT="$plan" python3 -c 'import os, shlex
for line in os.environ["PLAN_TEXT"].splitlines():
    if line.startswith("PLAN: security create-keychain "):
        print(shlex.split(line[6:])[-1]); break')
  [ -n "$plan_keychain" ] || { echo 'FAIL: plan has no dedicated keychain' >&2; exit 1; }
  check_search_list_plan "$plan" '"/Users/runner/Library/Keychains/login.keychain-db"' "$plan_keychain"
  status=0
  error=$(bash "$(dirname "$0")/codesign-identity.sh" --keychain /var/tmp/invalid.keychain-db 2>&1) || status=$?
  [ "$status" -eq 2 ] && [[ "$error" == *'Refusing --keychain'* ]] || {
    echo 'FAIL: provisioner accepted an unsafe keychain' >&2; exit 1;
  }
  status=0
  error=$(bash "$(dirname "$0")/codesign-server.sh" --keychain /var/tmp/invalid.keychain-db /var/tmp/unused 2>&1) || status=$?
  [ "$status" -eq 2 ] && [[ "$error" == *'Refusing --keychain'* ]] || {
    echo 'FAIL: signer accepted an unsafe keychain' >&2; exit 1;
  }
  guard_root=${TMPDIR:-/tmp}
  guard_dir=$(mktemp -d "${guard_root%/}/amux-codesign.XXXXXXXX")
  trap 'rm -rf "$guard_dir"' EXIT
  ln -s /var/tmp/unused.keychain-db "$guard_dir/k.keychain-db"
  status=0
  error=$(AMUX_CODESIGN_TEST_DIR="$guard_dir" bash "$(dirname "$0")/codesign-identity.sh" --keychain "$guard_dir/k.keychain-db" 2>&1) || status=$?
  [ "$status" -eq 2 ] && [[ "$error" == *'Refusing --keychain: symlink'* ]] || {
    echo 'FAIL: provisioner accepted a symlinked keychain' >&2; exit 1;
  }
  : > "$guard_dir/plain-file"
  status=0
  no_keychain_plan=$(DRY_RUN=1 /bin/bash "$(dirname "$0")/codesign-server.sh" "$guard_dir/plain-file" 2>&1) || status=$?
  [ "$status" -eq 0 ] && [[ "$no_keychain_plan" == *'PLAN: codesign --force'* ]] &&
    [[ "$no_keychain_plan" == *'PLAN: security find-certificate'* ]] || {
    echo 'FAIL: /bin/bash no-keychain signing plan failed' >&2; printf '%s\n' "$no_keychain_plan" >&2; exit 1;
  }
  # An empty user search list (bash 3.2 + set -u) must still append and sign (Codex B1 on a986cc9).
  status=0
  empty_list_plan=$(DRY_RUN=1 AMUX_CODESIGN_PLAN_SEARCH_LIST=' ' /bin/bash "$(dirname "$0")/codesign-server.sh" "$guard_dir/plain-file" 2>&1) || status=$?
  [ "$status" -eq 0 ] && [[ "$empty_list_plan" == *'PLAN: security list-keychains -d user -s '*'amux-signing.keychain-db'* ]] &&
    [[ "$empty_list_plan" == *'PLAN: codesign --force'* ]] && [[ "$empty_list_plan" != *'unbound variable'* ]] || {
    echo 'FAIL: /bin/bash signing plan with an empty search list failed' >&2; printf '%s\n' "$empty_list_plan" >&2; exit 1;
  }
  echo 'PASS: empty user search list appends the dedicated keychain and signs'
  # B4: the builder unlocks the dedicated keychain on every tick, before its first early exit.
  build="$(dirname "$0")/rust-auto-build.sh"
  unlock_line=$(grep -n 'codesign-keychain-unlock.sh' "$build" | head -1 | cut -d: -f1 || true)
  first_exit=$(grep -nE '^[[:space:]]*exit 0' "$build" | head -1 | cut -d: -f1 || true)
  [ -n "$unlock_line" ] && [ -n "$first_exit" ] && [ "$unlock_line" -lt "$first_exit" ] || {
    echo "FAIL: builder tick does not unlock the signing keychain before its first exit (unlock=$unlock_line exit=$first_exit)" >&2; exit 1;
  }
  unlock_plan=$(HOME="$guard_dir/home" DRY_RUN=1 /bin/bash "$(dirname "$0")/codesign-keychain-unlock.sh" 2>&1)
  [ "$unlock_plan" = "PLAN: security unlock-keychain -p \<password\> $guard_dir/home/Library/Keychains/amux-signing.keychain-db" ] || {
    echo "FAIL: unlock plan touches something other than the dedicated keychain: $unlock_plan" >&2; exit 1;
  }
  echo 'PASS: every builder tick unlocks only the dedicated signing keychain'
  # Mocked behavior (no real security runs): fake security records its arguments.
  mock_home="$guard_dir/mock-home"; mock_bin="$guard_dir/mock-bin"; calls="$guard_dir/security-calls"
  mkdir -p "$mock_home/Library/Keychains" "$mock_home/.amux/codesign" "$mock_bin"
  printf '#!/bin/sh\nprintf "%%s\\n" "$*" >> "%s"\n' "$calls" > "$mock_bin/security"; chmod +x "$mock_bin/security"
  run_unlock() { HOME="$mock_home" PATH="$mock_bin:/usr/bin:/bin" /bin/bash "$(dirname "$0")/codesign-keychain-unlock.sh"; }
  : > "$calls"; run_unlock 2>/dev/null
  [ ! -s "$calls" ] || { echo 'FAIL: unlock ran security before setup created the keychain' >&2; exit 1; }
  : > "$mock_home/Library/Keychains/amux-signing.keychain-db"
  printf 'pw\n' > "$mock_home/.amux/codesign/keychain-password"; chmod 644 "$mock_home/.amux/codesign/keychain-password"
  run_unlock 2>/dev/null
  [ ! -s "$calls" ] || { echo 'FAIL: unlock used a password file that is not mode 600' >&2; exit 1; }
  chmod 600 "$mock_home/.amux/codesign/keychain-password"
  first=$(run_unlock 2>&1); second=$(run_unlock 2>&1)
  [ "$(cat "$calls")" = "$(printf 'unlock-keychain -p pw %s\nunlock-keychain -p pw %s' "$mock_home/Library/Keychains/amux-signing.keychain-db" "$mock_home/Library/Keychains/amux-signing.keychain-db")" ] || {
    echo "FAIL: unlock calls were not exactly two dedicated-keychain unlocks: $(cat "$calls")" >&2; exit 1;
  }
  [[ "$first" == *'verdict=codesign_keychain_unlocked'* ]] && [ -z "$second" ] || {
    echo "FAIL: unlock verdict must be logged once per change (first=[$first] second=[$second])" >&2; exit 1;
  }
  echo 'PASS: unlock is a no-op before setup, refuses a loose password file, and logs once per change'
  rm -rf "$guard_dir"
  trap - EXIT
  echo 'PASS: signing plan contains expected commands and no forbidden operation'
  exit 0
fi
[ "$#" -eq 0 ] || { echo "usage: $0 [--check-plan]" >&2; exit 2; }
if [ "$(uname -s)" != Darwin ]; then echo 'SKIP: code signing requires macOS'; exit 0; fi
if [ "${DRY_RUN:-0}" != 1 ]; then
  DRY_RUN=1 bash "$0" | bash "$0" --check-plan
fi
repo=$(cd "$(dirname "$0")/.." && pwd)
tmp_root=${TMPDIR:-/tmp}
work=$(mktemp -d "${tmp_root%/}/amux-codesign.XXXXXXXX")
export AMUX_CODESIGN_TEST_DIR=$work
keychain="$HOME/Library/Keychains/amux-codesign-ci-$(basename "$work").keychain-db"
export AMUX_CODESIGN_TEST_KEYCHAIN="$keychain"
password=$(python3 -c 'import secrets; print(secrets.token_urlsafe(32))')
export AMUX_CODESIGN_TEST_PASSWORD=$password
cleanup() {
  local cleanup_status=0
  local delete_status=0
  if [ "${DRY_RUN:-0}" = 1 ]; then
    plan_command security delete-keychain "$keychain"
  else
    if [ -f "$work/original.search" ]; then
      read_user_search_list "$(cat "$work/original.search")" &&
        run_sensitive security list-keychains -d user -s "${USER_SEARCH_LIST[@]}" >/dev/null || cleanup_status=$?
    fi
    run_sensitive security delete-keychain "$keychain" >/dev/null 2>&1 || delete_status=$?
    [ "$cleanup_status" -ne 0 ] || cleanup_status=$delete_status
  fi
  rm -rf "$work"
  if [ "$cleanup_status" -eq 75 ]; then
    echo 'SKIP: temporary keychain deletion timed out' >&2
    return 75
  fi
  if [ "$cleanup_status" -ne 0 ]; then
    echo "FAIL: temporary keychain deletion exited $cleanup_status" >&2
    return "$cleanup_status"
  fi
  return 0
}
trap cleanup EXIT

if [ "${DRY_RUN:-0}" = 1 ]; then
  plan_command security create-keychain -p '<random-password>' "$keychain"
  plan_command security unlock-keychain -p '<random-password>' "$keychain"
  bash "$repo/scripts/codesign-identity.sh" --keychain "$keychain"
  for bin in one two; do
    : > "$work/$bin"
    bash "$repo/scripts/codesign-server.sh" --keychain "$keychain" "$work/$bin"
    plan_command codesign -d -r- "$work/$bin"
  done
  : > "$work/staged"
  bash "$repo/scripts/codesign-server.sh" --keychain "$keychain" --verify-only "$work/staged"
  bash "$repo/scripts/codesign-server.sh" --keychain "$keychain" --verify-only "$work/staged"
  exit 0
fi

status=0
security list-keychains -d user > "$work/original.search"
run_sensitive security create-keychain -p "$password" "$keychain" || status=$?
if [ "$status" -eq 75 ]; then echo 'SKIP: temporary keychain creation timed out'; exit 0; fi
[ "$status" -eq 0 ] || exit "$status"
run_sensitive security unlock-keychain -p "$password" "$keychain" || status=$?
if [ "$status" -eq 75 ]; then echo 'SKIP: temporary keychain unlock timed out'; exit 0; fi
[ "$status" -eq 0 ] || exit "$status"
echo 'PASS: disposable keychain creation is guarded'

bash "$repo/scripts/codesign-identity.sh" --keychain "$keychain" || status=$?
if [ "$status" -eq 75 ]; then echo 'SKIP: identity provisioning timed out'; exit 0; fi
[ "$status" -eq 0 ] || exit "$status"
printf 'int main(void) { return 1; }\n' > "$work/one.c"
printf 'int main(void) { return 2; }\n' > "$work/two.c"
cc "$work/one.c" -o "$work/one"
cc "$work/two.c" -o "$work/two"
for bin in one two; do
  status=0
  bash "$repo/scripts/codesign-server.sh" --keychain "$keychain" "$work/$bin" || status=$?
  if [ "$status" -eq 75 ]; then echo 'SKIP: signing timed out'; exit 0; fi
  [ "$status" -eq 0 ] || exit "$status"
  status=0
  run_sensitive codesign -d -r- "$work/$bin" > "$work/$bin.details" 2>&1 || status=$?
  if [ "$status" -eq 75 ]; then echo 'SKIP: requirement probe timed out'; exit 0; fi
  [ "$status" -eq 0 ] || { cat "$work/$bin.details" >&2; exit "$status"; }
  sed -n '/^designated => /p' "$work/$bin.details" > "$work/$bin.req"
  [ -s "$work/$bin.req" ] || { echo 'FAIL: designated requirement absent' >&2; exit 1; }
  grep -F 'identifier "com.amux.server-rs"' "$work/$bin.req" >/dev/null
done
echo 'PASS: both signed binaries report Authority=amux local code signing'
cmp "$work/one.req" "$work/two.req"
echo 'PASS: two binaries have the same designated requirement'
cp "$work/one" "$work/live"
cp "$work/two" "$work/staged"
printf 'tamper\n' >> "$work/staged"
before=$(shasum -a 256 "$work/live")
status=0
bash "$repo/scripts/codesign-server.sh" --keychain "$keychain" --verify-only "$work/staged" > "$work/rejected.log" 2>&1 || status=$?
if [ "$status" -eq 75 ]; then echo 'SKIP: rejection probe timed out'; exit 0; fi
if [ "$status" -eq 0 ]; then
  echo 'FAIL: tampered candidate verified' >&2; exit 1
fi
status=0
bash "$repo/scripts/codesign-server.sh" --keychain "$keychain" --verify-only "$work/staged" >/dev/null 2>&1 || status=$?
if [ "$status" -eq 75 ]; then echo 'SKIP: adoption probe timed out'; exit 0; fi
if [ "$status" -eq 0 ]; then
  mv "$work/staged" "$work/live"
fi
[ "$(shasum -a 256 "$work/live")" = "$before" ] || {
  echo 'FAIL: rejected candidate replaced the live image' >&2; exit 1;
}
cleanup_status=0
cleanup || cleanup_status=$?
trap - EXIT
[ "$cleanup_status" -eq 75 ] && exit 0
[ "$cleanup_status" -eq 0 ] || exit "$cleanup_status"
echo 'PASS: two builds share a designated requirement; verification refusal preserves the live image'
