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

check_identity_plan() {
  local plan=$1 line found_partition=0 found_export=0
  while IFS= read -r line; do
    case "$line" in
      'PLAN: security set-key-partition-list '*)
        found_partition=1
        [[ "$line" == *' -S apple-tool:\,apple:\,codesign: -s -k '* ]] &&
        [[ "$line" != *' -t '* ]] && [[ "$line" != *' -l '* ]] || {
          echo 'FAIL: partition-list plan must use the dedicated keychain unscoped' >&2; return 1;
        }
        ;;
      'PLAN: openssl pkcs12 -export '*)
        found_export=1
        [[ "$line" == *' -name amux\ local\ code\ signing '* ]] || {
          echo 'FAIL: PKCS#12 export plan must label the amux key' >&2; return 1;
        }
        ;;
    esac
  done <<< "$plan"
  [ "$found_partition" -eq 1 ] && [ "$found_export" -eq 1 ] || {
    echo 'FAIL: identity plan is missing export or partition-list step' >&2; return 1;
  }
}

run_sensitive() {
  local status=0
  perl -e 'alarm 30; exec @ARGV or die "exec failed: $!\n"' "$@" || status=$?
  if [ "$status" -eq 142 ]; then
    echo "SKIP: command timed out after 30 seconds: $1" >&2
    return 75
  fi
  return "$status"
}
