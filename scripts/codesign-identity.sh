#!/usr/bin/env bash
# Provision only disposable signing keys; print the owner's separate login step.
set -euo pipefail
umask 077
source "$(dirname "$0")/codesign-keychain-guard.sh"

identity='amux local code signing'
usage() { echo "usage: $0 --owner-provision | --keychain PATH"; }
if [ "${1:-}" = --help ]; then usage; exit 0; fi
if [ "${1:-}" = --owner-provision ] && [ "$#" -eq 1 ]; then
  cat <<'OWNER'
Owner step (run once in an interactive Terminal; the password stays out of shell history):
bash <<'PROVISION'
set -euo pipefail
umask 077
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
read -r -s -p 'Login keychain password: ' keychain_password < /dev/tty; printf '\n'
openssl req -x509 -newkey rsa:2048 -nodes -days 3650 -keyout "$work/key.pem" -out "$work/cert.pem" -subj '/CN=amux local code signing/O=amux local build' -addext 'basicConstraints=critical,CA:false' -addext 'keyUsage=critical,digitalSignature' -addext 'extendedKeyUsage=critical,codeSigning'
openssl pkcs12 -export -inkey "$work/key.pem" -in "$work/cert.pem" -out "$work/identity.p12" -passout "pass:$keychain_password" -keypbe PBE-SHA1-3DES -certpbe PBE-SHA1-3DES -macalg sha1
security import "$work/identity.p12" -k "$HOME/Library/Keychains/login.keychain-db" -P "$keychain_password" -T /usr/bin/codesign
security set-key-partition-list -S apple-tool:,apple:,codesign: -s -k "$keychain_password" "$HOME/Library/Keychains/login.keychain-db"
PROVISION
OWNER
  exit 3
fi
if [ "${1:-}" != --keychain ] || [ "$#" -ne 2 ]; then usage >&2; exit 2; fi
keychain=$2
require_test_keychain "$keychain" || exit 2
[ "$(uname -s)" = Darwin ] || { echo 'macOS only' >&2; exit 2; }
work=$AMUX_CODESIGN_TEST_DIR
password=${AMUX_CODESIGN_TEST_PASSWORD:-}
[ -n "$password" ] || { echo 'Disposable keychain password required' >&2; exit 2; }
if [ "${DRY_RUN:-0}" = 1 ]; then
  plan_command openssl req -x509 -newkey rsa:2048 -nodes -days 3650 -keyout "$work/key.pem" -out "$work/cert.pem" -subj "/CN=$identity/O=amux local build" -addext basicConstraints=critical,CA:false -addext keyUsage=critical,digitalSignature -addext extendedKeyUsage=critical,codeSigning
  plan_command openssl pkcs12 -export -inkey "$work/key.pem" -in "$work/cert.pem" -out "$work/identity.p12" -passout 'pass:<random-password>' -keypbe PBE-SHA1-3DES -certpbe PBE-SHA1-3DES -macalg sha1
  plan_command security import "$work/identity.p12" -k "$keychain" -P '<random-password>' -T /usr/bin/codesign
  plan_command security set-key-partition-list -S apple-tool:,apple:,codesign: -s -k '<random-password>' "$keychain"
  exit 0
fi
run_sensitive openssl req -x509 -newkey rsa:2048 -nodes -days 3650 \
  -keyout "$work/key.pem" -out "$work/cert.pem" -subj "/CN=$identity/O=amux local build" \
  -addext 'basicConstraints=critical,CA:false' -addext 'keyUsage=critical,digitalSignature' \
  -addext 'extendedKeyUsage=critical,codeSigning' >/dev/null 2>&1
chmod 600 "$work/key.pem"
run_sensitive openssl pkcs12 -export -inkey "$work/key.pem" -in "$work/cert.pem" \
  -out "$work/identity.p12" -passout "pass:$password" \
  -keypbe PBE-SHA1-3DES -certpbe PBE-SHA1-3DES -macalg sha1 >/dev/null 2>&1
run_sensitive security import "$work/identity.p12" -k "$keychain" -P "$password" -T /usr/bin/codesign >/dev/null
run_sensitive security set-key-partition-list -S apple-tool:,apple:,codesign: -s -k "$password" "$keychain" >/dev/null
echo 'Disposable signing identity provisioned'
