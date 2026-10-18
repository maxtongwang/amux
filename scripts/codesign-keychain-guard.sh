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

# Fresh CI runners do not search a newly created keychain for a named identity,
# even when codesign receives --keychain. Keep the disposable keychain visible
# only for this test, and put the user's original search list back on exit.
codesign_ci_add_test_keychain() {
  [ "${GITHUB_ACTIONS:-}" = true ] || return 0
  require_test_keychain "$1" || return 2
  local listing line
  listing=$(run_sensitive security list-keychains -d user) || return $?
  CODESIGN_CI_KEYCHAINS=()
  while IFS= read -r line; do
    line=${line#"${line%%[![:space:]]*}"}
    line=${line#\"}
    line=${line%\"}
    [ -n "$line" ] && CODESIGN_CI_KEYCHAINS+=("$line")
  done <<< "$listing"
  CODESIGN_CI_SEARCH_CHANGED=1
  run_sensitive security list-keychains -d user -s ${CODESIGN_CI_KEYCHAINS[@]+"${CODESIGN_CI_KEYCHAINS[@]}"} "$1"
  echo 'CI: disposable signing keychain added to user search list'
}

codesign_ci_restore_keychains() {
  [ "${CODESIGN_CI_SEARCH_CHANGED:-0}" = 1 ] || return 0
  run_sensitive security list-keychains -d user -s ${CODESIGN_CI_KEYCHAINS[@]+"${CODESIGN_CI_KEYCHAINS[@]}"} || return $?
  CODESIGN_CI_SEARCH_CHANGED=0
  echo 'CI: original user keychain search list restored'
}
