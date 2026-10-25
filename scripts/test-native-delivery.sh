#!/usr/bin/env bash
# Native delivery fixtures — isolate Amux state before invoking the Cargo guard.
set -euo pipefail
if [[ "${1:-}" == "--help" || "${1:-}" == "-h" ]]; then
  echo 'Usage: scripts/test-native-delivery.sh [cargo test options, e.g. --release --offline]'
  exit 0
fi
fixture_home=$(mktemp -d "${TMPDIR:-/tmp}/native-delivery.XXXXXX")
fixture_home=$(cd "$fixture_home" && pwd -P)
chmod 700 "$fixture_home"
touch "$fixture_home/.native-delivery-fixture"
export AMUX_HOME="$fixture_home"
# Exercise the shipped feature-disabled resolver regardless of the server flag.
export AMUX_NATIVE_CLAUDE_DELIVERY=0
export CARGO_TARGET_DIR="$HOME/.amux/rust-build-target/verification"
finish() {
  local result=$?
  if [[ "$result" == 0 ]]; then
    rm -rf -- "$fixture_home"
  else
    echo "Native delivery failed evidence retained at $fixture_home" >&2
  fi
}
trap finish EXIT
"$(dirname "$0")/safe-cargo.sh" test -p amux-server --lib api::delivery::tests "$@"
