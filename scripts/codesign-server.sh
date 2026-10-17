#!/usr/bin/env bash
# Sign and verify a staged server image before the auto-builder may adopt it.
set -euo pipefail
source "$(dirname "$0")/codesign-keychain-guard.sh"

IDENTITY='amux local code signing'
IDENTIFIER='com.amux.server-rs'

usage() { echo "usage: $0 [--keychain PATH] [--verify-only] staged-binary"; }
keychain=''
verify_only=0
if [ "${1:-}" = --help ]; then usage; exit 0; fi
if [ "${1:-}" = --keychain ]; then
  [ "$#" -ge 3 ] || { usage >&2; exit 2; }
  keychain=$2; shift 2
  require_test_keychain "$keychain" || exit 2
fi
if [ "${1:-}" = --verify-only ]; then verify_only=1; shift; fi
[ "$#" -eq 1 ] && [ -f "$1" ] || { usage >&2; exit 2; }
bin=$1
if [ "$(uname -s)" != Darwin ]; then echo 'SKIP: macOS only'; exit 0; fi
keychain_args=()
if [ -n "$keychain" ]; then keychain_args=(--keychain "$keychain"); fi
identity_list_args=()
if [ -n "$keychain" ]; then identity_list_args=("$keychain"); fi
if [ "${DRY_RUN:-0}" = 1 ]; then
  if [ "$verify_only" = 0 ]; then
    plan_command codesign --force --sign "$IDENTITY" --identifier "$IDENTIFIER" --options runtime --timestamp=none "${keychain_args[@]}" "$bin"
  fi
  plan_command codesign --verify --strict "$bin"
  plan_command codesign -dvvv "$bin"
  plan_command codesign -d -r- "$bin"
  plan_command security find-certificate -c "$IDENTITY" -Z "${identity_list_args[@]}"
  exit 0
fi
if [ "$verify_only" = 0 ]; then
  run_sensitive codesign --force --sign "$IDENTITY" --identifier "$IDENTIFIER" \
      --options runtime --timestamp=none "${keychain_args[@]}" "$bin" || {
    code=$?; [ "$code" -eq 75 ] && exit 75
    echo 'codesign signing failed' >&2; exit 1
  }
fi
run_sensitive codesign --verify --strict "$bin" || {
  code=$?; [ "$code" -eq 75 ] && exit 75
  echo 'codesign strict verification failed' >&2; exit 1
}
details=$(run_sensitive codesign -dvvv "$bin" 2>&1) || { code=$?; [ "$code" -eq 75 ] && exit 75; echo 'codesign metadata unreadable' >&2; exit 1; }
requirement=$(run_sensitive codesign -d -r- "$bin" 2>&1) || { code=$?; [ "$code" -eq 75 ] && exit 75; echo 'codesign requirement unreadable' >&2; exit 1; }
printf '%s\n' "$details" | grep -Fx "Identifier=$IDENTIFIER" >/dev/null || {
  echo 'codesign identifier mismatch' >&2; exit 1;
}
printf '%s\n' "$details" | grep -Fx "Authority=$IDENTITY" >/dev/null || {
  echo "codesign leaf certificate mismatch: $(printf '%s\n' "$details" | grep -E '^Authority=|^Signature=' | tr '\n' ' ')" >&2; exit 1;
}
printf '%s\n' "$requirement" | grep -F "identifier \"$IDENTIFIER\"" >/dev/null || {
  echo 'codesign designated requirement identifier mismatch' >&2; exit 1;
}
identity_hash=$(run_sensitive security find-certificate -c "$IDENTITY" -Z "${identity_list_args[@]}" \
  | awk '/^SHA-1 hash:/ {print $3; exit}')
[ -n "$identity_hash" ] && printf '%s\n' "$requirement" | grep -Fi "$identity_hash" >/dev/null || {
  echo 'codesign designated requirement certificate mismatch' >&2; exit 1;
}
echo "codesign verified identity=$IDENTITY identifier=$IDENTIFIER"
