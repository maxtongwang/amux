#!/usr/bin/env bash
# Provision the stable amux identity in its own keychain without touching login keychain state.
# Usage: scripts/codesign-identity-setup.sh (DRY_RUN=1 prints the command plan)
set -euo pipefail
umask 077
here="$(cd "$(dirname "$0")" && pwd)"
source "$here/codesign-keychain-guard.sh"
identity='amux local code signing'
work=''
cleanup() { [ -z "$work" ] || rm -rf "$work"; }
trap cleanup EXIT
run() { if [ "${DRY_RUN:-0}" = 1 ]; then plan_command "$@"; else "$@"; fi; }
[ "$(uname -s)" = Darwin ] || { echo 'macOS only' >&2; exit 2; }
if [ -n "${AMUX_CODESIGN_TEST_DIR:-}" ]; then
  keychain="$AMUX_CODESIGN_TEST_DIR/k.keychain-db"
  require_test_keychain "$keychain" || exit 2
  password_file="$AMUX_CODESIGN_TEST_DIR/keychain-password"
else
  keychain="$HOME/Library/Keychains/amux-signing.keychain-db"
  password_file="$HOME/.amux/codesign/keychain-password"
fi
[ ! -L "$keychain" ] && [ ! -L "$password_file" ] || { echo 'Refusing symlinked signing state' >&2; exit 2; }
if [ "${DRY_RUN:-0}" = 1 ]; then
  plan_command mkdir -p "$(dirname "$password_file")"
  plan_command openssl rand -hex 32
  plan_command security create-keychain -p '<random-password>' "$keychain"
  plan_command security set-keychain-settings "$keychain"
  plan_command security unlock-keychain -p '<password>' "$keychain"
  password='<password>'
else
  mkdir -p "$(dirname "$password_file")"
  chmod 700 "$(dirname "$password_file")"
  if [ -e "$keychain" ] && [ ! -f "$password_file" ]; then
    preserved="$keychain.unpaired-$(date +%Y%m%d%H%M%S)-$$"
    mv "$keychain" "$preserved"
    echo "WARN verdict=codesign_keychain_unpaired preserved=$preserved; recreating signing identity" >&2
  fi
  if [ ! -e "$keychain" ]; then
    password=$(openssl rand -hex 32)
    printf '%s\n' "$password" > "$password_file"
    chmod 600 "$password_file"
    security create-keychain -p "$password" "$keychain"
    echo 'INFO verdict=codesign_keychain_created' >&2
  else
    [ -f "$password_file" ] || { echo 'Missing keychain password' >&2; exit 1; }
    [ "$(stat -f %Lp "$password_file")" = 600 ] || { echo 'Keychain password file must be mode 600' >&2; exit 1; }
    password=$(cat "$password_file")
  fi
fi
run security set-keychain-settings "$keychain"
run security unlock-keychain -p "$password" "$keychain"
if [ "${DRY_RUN:-0}" = 1 ]; then
  plan_command security find-identity -p codesigning "$keychain"
  need_import=1
else
  identities=$(security find-identity -p codesigning "$keychain")
  count=$(printf '%s\n' "$identities" | awk '/^[[:space:]]*[0-9]+\)/ {n++} END {print n+0}')
  [ "$count" -le 1 ] || { echo "WARN verdict=codesign_keychain_contaminated identities=$count" >&2; exit 1; }
  if [ "$count" -eq 1 ]; then
    grep -qF "\"$identity\"" <<< "$identities" || { echo 'WARN verdict=codesign_keychain_wrong_identity' >&2; exit 1; }
    need_import=0
  else
    need_import=1
  fi
fi
if [ "$need_import" -eq 1 ]; then
  work=$(mktemp -d)
  p12_password='<random-password>'
  [ "${DRY_RUN:-0}" = 1 ] || p12_password=$(openssl rand -hex 24)
  run openssl req -x509 -newkey rsa:2048 -nodes -days 3650 -keyout "$work/key.pem" -out "$work/cert.pem" \
    -subj "/CN=$identity/O=amux local build" -addext basicConstraints=critical,CA:false \
    -addext keyUsage=critical,digitalSignature -addext extendedKeyUsage=critical,codeSigning
  run openssl pkcs12 -export -name "$identity" -inkey "$work/key.pem" -in "$work/cert.pem" -out "$work/identity.p12" \
    -passout "pass:$p12_password" -keypbe PBE-SHA1-3DES -certpbe PBE-SHA1-3DES -macalg sha1
  run security import "$work/identity.p12" -k "$keychain" -P "$p12_password" -T /usr/bin/codesign
fi
# Unscoped ACL rewrite is safe only after proving this keychain contains exactly our identity.
if [ "${DRY_RUN:-0}" = 1 ]; then
  plan_command security find-identity -p codesigning "$keychain"
else
  identities=$(security find-identity -p codesigning "$keychain")
  count=$(printf '%s\n' "$identities" | awk '/^[[:space:]]*[0-9]+\)/ {n++} END {print n+0}')
  [ "$count" -eq 1 ] && grep -qF "\"$identity\"" <<< "$identities" || {
    echo "WARN verdict=codesign_keychain_contaminated identities=$count" >&2; exit 1;
  }
fi
run security set-key-partition-list -S apple-tool:,apple:,codesign: -s -k "$password" "$keychain"
if [ "${DRY_RUN:-0}" = 1 ]; then
  plan_command codesign --force --sign '<identity-hash>' --identifier com.amux.codesign-probe --keychain "$keychain" '<probe>'
else
  probe_dir=$(mktemp -d)
  cp /usr/bin/true "$probe_dir/probe"
  identity_hash=$(printf '%s\n' "$identities" | awk -v label="\"$identity\"" 'index($0,label) {print $2; exit}')
  [ -n "$identity_hash" ] || { echo 'WARN verdict=codesign_identity_hash_missing' >&2; exit 1; }
  codesign --force --sign "$identity_hash" --identifier com.amux.codesign-probe --keychain "$keychain" "$probe_dir/probe"
  shown=$(codesign -dvv "$probe_dir/probe" 2>&1)
  rm -rf "$probe_dir"
  grep -qF "Authority=$identity" <<< "$shown" || { echo 'WARN verdict=codesign_probe_failed' >&2; exit 1; }
fi
echo 'PASS: dedicated signing identity signs; setup is repairable on rerun'
echo 'Full Disk Access: Open System Settings > Privacy & Security > Full Disk Access and enable amux-server-rs (~/.local/bin/amux-server-rs).'
