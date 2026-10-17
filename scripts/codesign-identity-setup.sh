#!/usr/bin/env bash
# One-time owner setup of the stable amux signing identity, so macOS keeps amux-server-rs's
# permissions across every deploy. Run in a Terminal window: the only input is the Mac password.
# Idempotent: an existing identity is kept and only verified.
#
# Usage: scripts/codesign-identity-setup.sh            (the owner, in a Terminal window)
#        DRY_RUN=1 scripts/codesign-identity-setup.sh  (print the commands, run nothing)
# Tests only: AMUX_CODESIGN_TEST_DIR=<mktemp dir> AMUX_CODESIGN_TEST_PASSWORD=<pw> with the
#        disposable keychain <dir>/k.keychain-db (enforced by codesign-keychain-guard.sh).
# Exit: 0 ready, 1 failed (nothing half-done is left trusted), 2 bad usage or not interactive.
set -euo pipefail
umask 077
here="$(cd "$(dirname "$0")" && pwd)"
source "$here/codesign-keychain-guard.sh"

identity='amux local code signing'
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

if [ "${DRY_RUN:-0}" != 1 ] && have_identity; then
  say "Step 1 of 3: the identity already exists. Nothing to create."
else
  say "Step 1 of 3: creating the identity."
  work=$(mktemp -d)
  trap 'rm -rf "$work"' EXIT
  if [ -z "$password" ] && [ "${DRY_RUN:-0}" != 1 ]; then
    read -r -s -p 'Type your Mac password (the one you log in with) and press Return: ' password < /dev/tty
    printf '\n'
  fi
  pw="${password:-<password>}"
  run openssl req -x509 -newkey rsa:2048 -nodes -days 3650 -keyout "$work/key.pem" -out "$work/cert.pem" \
    -subj "/CN=$identity/O=amux local build" -addext basicConstraints=critical,CA:false \
    -addext keyUsage=critical,digitalSignature -addext extendedKeyUsage=critical,codeSigning
  run openssl pkcs12 -export -inkey "$work/key.pem" -in "$work/cert.pem" -out "$work/identity.p12" \
    -passout "pass:$pw" -keypbe PBE-SHA1-3DES -certpbe PBE-SHA1-3DES -macalg sha1
  run security import "$work/identity.p12" -k "$keychain" -P "$pw" -T /usr/bin/codesign
  # Lets codesign use the key without asking again on every build.
  run security set-key-partition-list -S apple-tool:,apple:,codesign: -s -k "$pw" "$keychain" >/dev/null
fi

say "Step 2 of 3: checking that it signs."
probe_dir=$(mktemp -d)
cp /usr/bin/true "$probe_dir/probe"
run codesign --force --sign "$identity" --identifier com.amux.codesign-probe --keychain "$keychain" "$probe_dir/probe"
if [ "${DRY_RUN:-0}" != 1 ]; then
  shown=$(codesign -dvv "$probe_dir/probe" 2>&1 || true)
  if ! have_identity || ! grep -qF "Authority=$identity" <<<"$shown"; then
    rm -rf "$probe_dir"
    say "Setup did not finish: the identity could not sign. Nothing was changed outside your keychain; tell the agent and it will look.
Details for the agent:
$(security find-identity -p codesigning "$keychain" 2>&1 | head -5)
$(grep -E 'Authority|Signature|Identifier' <<<"$shown")"
    exit 1
  fi
fi
rm -rf "$probe_dir"
say "OK: the identity is present and signs."

say "Step 3 of 3: one switch in System Settings.
  1. The next amux update signs amux-server-rs with this identity (within a few minutes).
  2. Open System Settings > Privacy & Security > Full Disk Access.
  3. Turn on the switch next to amux-server-rs.
     If it is not in the list, click +, press Command-Shift-G, type ~/.local/bin/amux-server-rs, and click Open.
That's all. You can close this window."
exit 0
