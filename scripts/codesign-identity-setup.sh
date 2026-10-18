#!/usr/bin/env bash
# One-time owner setup of the stable amux signing identity, so macOS keeps amux-server-rs's
# permissions across every deploy. Run in a Terminal window: the only input is the Mac password.
# Idempotent: an existing identity has its key access repaired and is verified.
#
# Usage: scripts/codesign-identity-setup.sh            (the owner, in a Terminal window)
#        DRY_RUN=1 scripts/codesign-identity-setup.sh  (print the commands, run nothing)
# Tests only: AMUX_CODESIGN_TEST_DIR=<mktemp dir> AMUX_CODESIGN_TEST_PASSWORD=<pw> with the
#        disposable keychain <dir>/k.keychain-db (enforced by codesign-keychain-guard.sh).
# Exit: 0 ready, 1 failed, 2 bad usage or not interactive.
set -euo pipefail
umask 077
here="$(cd "$(dirname "$0")" && pwd)"
source "$here/codesign-keychain-guard.sh"

identity='amux local code signing'
work=''
probe_dir=''
cleanup() {
  [ -z "$work" ] || rm -rf "$work"
  [ -z "$probe_dir" ] || rm -rf "$probe_dir"
}
trap cleanup EXIT
say() { printf '\n%s\n' "$*"; }
run() { if [ "${DRY_RUN:-0}" = 1 ]; then plan_command "$@"; else "$@"; fi; }

[ "$(uname -s)" = Darwin ] || { echo 'This setup is for macOS only.' >&2; exit 2; }
if [ -n "${AMUX_CODESIGN_TEST_DIR:-}" ]; then
  keychain="$AMUX_CODESIGN_TEST_DIR/k.keychain-db"
  require_test_keychain "$keychain" || exit 2
  password="${AMUX_CODESIGN_TEST_PASSWORD:?test password required}"
else
  keychain="$HOME/Library/Keychains/login.keychain-db"
  if [ "${DRY_RUN:-0}" != 1 ] && { [ ! -t 0 ] || [ ! -t 1 ]; }; then
    echo 'Run this in a Terminal window: it asks for your Mac password.' >&2; exit 2
  fi
  password=''
fi

say "Setting up the amux signing identity. This happens once; after it, macOS keeps amux's permissions across updates."

# Without -v: the certificate is deliberately not trusted (no trust settings are written), and
# codesign signs with an untrusted identity; -v would list only trusted ones.
have_identity() { security find-identity -p codesigning "$keychain" 2>/dev/null | grep -qF "\"$identity\""; }

apply_partition_list() {
  local attempt password_to_try partition_error partition_status
  for attempt in 1 2 3; do
    if [ -n "${AMUX_CODESIGN_TEST_DIR:-}" ]; then
      password_to_try=$password
      if [ "$attempt" -eq 1 ] && [ "${AMUX_CODESIGN_TEST_WRONG_FIRST:-0}" = 1 ]; then
        password_to_try="wrong-$password"
      fi
    elif [ "${DRY_RUN:-0}" = 1 ]; then
      password_to_try='<password>'
    else
      password_to_try=''
      while [ -z "$password_to_try" ]; do
        read -r -s -p 'Type your Mac login keychain password and press Return: ' password_to_try < /dev/tty
        printf '\n'
        [ -n "$password_to_try" ] || say 'The password cannot be empty. Try again.'
      done
    fi
    if [ "${DRY_RUN:-0}" = 1 ]; then
      plan_command security set-key-partition-list -S apple-tool:,apple:,codesign: -s -t private -l "$identity" -k '<password>' "$keychain"
      return 0
    fi
    if partition_error=$(security set-key-partition-list -S apple-tool:,apple:,codesign: -s -t private -l "$identity" -k "$password_to_try" "$keychain" 2>&1 >/dev/null); then
      return 0
    else
      partition_status=$?
    fi
    if [[ "$partition_error" == *passphrase*not\ correct* ]] || [[ "$partition_error" == *password*not\ correct* ]]; then
      say "That password did not match the keychain. Try again ($attempt of 3)."
      continue
    fi
    printf 'Could not set signing key access (security exit %s): %s\n' "$partition_status" "$partition_error" >&2
    return 1
  done
  say 'Could not set signing key access after 3 tries. Run this setup again to repair the existing identity.'
  return 1
}

if [ "${DRY_RUN:-0}" != 1 ] && have_identity; then
  say "Step 1 of 3: the identity already exists. Repairing signing key access."
else
  say "Step 1 of 3: creating the identity."
  work=$(mktemp -d)
  p12_password='<random-password>'
  if [ "${DRY_RUN:-0}" != 1 ]; then p12_password=$(openssl rand -hex 24); fi
  run openssl req -x509 -newkey rsa:2048 -nodes -days 3650 -keyout "$work/key.pem" -out "$work/cert.pem" \
    -subj "/CN=$identity/O=amux local build" -addext basicConstraints=critical,CA:false \
    -addext keyUsage=critical,digitalSignature -addext extendedKeyUsage=critical,codeSigning
  run openssl pkcs12 -export -name "$identity" -inkey "$work/key.pem" -in "$work/cert.pem" -out "$work/identity.p12" \
    -passout "pass:$p12_password" -keypbe PBE-SHA1-3DES -certpbe PBE-SHA1-3DES -macalg sha1
  run security import "$work/identity.p12" -k "$keychain" -P "$p12_password"
fi

# This is the first operation that checks the typed login keychain password.
# Reapply it on reruns so a failed first setup is repairable.
apply_partition_list

say "Step 2 of 3: checking that it signs."
probe_dir=$(mktemp -d)
cp /usr/bin/true "$probe_dir/probe"
run codesign --force --sign "$identity" --identifier com.amux.codesign-probe --keychain "$keychain" "$probe_dir/probe"
if [ "${DRY_RUN:-0}" != 1 ]; then
  shown=$(codesign -dvv "$probe_dir/probe" 2>&1 || true)
  if ! have_identity || ! grep -qF "Authority=$identity" <<<"$shown"; then
    say "Setup did not finish: the identity could not sign. Nothing was changed outside your keychain; tell the agent and it will look.
Details for the agent:
$(security find-identity -p codesigning "$keychain" 2>&1 | head -5)
$(grep -E 'Authority|Signature|Identifier' <<<"$shown")"
    exit 1
  fi
fi
say "OK: the identity is present and signs."

say "Step 3 of 3: one switch in System Settings.
  1. The next amux update signs amux-server-rs with this identity (within a few minutes).
  2. Open System Settings > Privacy & Security > Full Disk Access.
  3. Turn on the switch next to amux-server-rs.
     If it is not in the list, click +, press Command-Shift-G, type ~/.local/bin/amux-server-rs, and click Open.
That's all. You can close this window."
exit 0
