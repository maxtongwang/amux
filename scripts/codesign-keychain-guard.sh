#!/usr/bin/env bash
# Guard disposable keychains and cap commands that could request UI authorization.

require_test_keychain() {
  local root
  [ -n "${AMUX_CODESIGN_TEST_DIR:-}" ] || { echo 'Refusing --keychain: test directory required' >&2; return 2; }
  root=$(cd "$AMUX_CODESIGN_TEST_DIR" 2>/dev/null && pwd -P) || { echo 'Refusing --keychain: test directory missing' >&2; return 2; }
  case "$root" in
    /tmp/amux-codesign.*|/private/tmp/amux-codesign.*|/private/var/folders/*/amux-codesign.*) ;;
    *) echo 'Refusing --keychain: use mktemp under TMPDIR or /tmp' >&2; return 2 ;;
  esac
  [ ! -L "$1" ] || { echo 'Refusing --keychain: symlink' >&2; return 2; }
  [ "$1" = "$AMUX_CODESIGN_TEST_DIR/k.keychain-db" ] || {
    echo 'Refusing --keychain: path must be the disposable test keychain' >&2; return 2;
  }
}

plan_command() { printf 'PLAN:'; printf ' %q' "$@"; printf '\n'; }

run_sensitive() {
  local status=0
  perl -e 'alarm 30; exec @ARGV or die "exec failed: $!\n"' "$@" || status=$?
  if [ "$status" -eq 142 ]; then
    echo "SKIP: command timed out after 30 seconds: $1" >&2
    return 75
  fi
  return "$status"
}
