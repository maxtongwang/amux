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
  expected=${AMUX_CODESIGN_TEST_KEYCHAIN:-$AMUX_CODESIGN_TEST_DIR/k.keychain-db}
  if [ -n "${AMUX_CODESIGN_TEST_KEYCHAIN:-}" ]; then
    case "$expected" in
      "$HOME"/Library/Keychains/amux-codesign-ci-*.keychain-db) ;;
      *) echo 'Refusing --keychain: test keychain must be disposable under Library/Keychains' >&2; return 2 ;;
    esac
  fi
  [ "$1" = "$expected" ] || {
    echo 'Refusing --keychain: path must be the disposable test keychain' >&2; return 2;
  }
}

plan_command() { printf 'PLAN:'; printf ' %q' "$@"; printf '\n'; }

# Decode security's quoted entries without splitting paths containing spaces.
read_user_search_list() {
  local output=$1 file entry
  file=$(mktemp) || return 1
  if ! SEARCH_LIST_OUTPUT="$output" python3 -c 'import os, shlex, sys
try:
    entries = shlex.split(os.environ["SEARCH_LIST_OUTPUT"])
    if any(not entry for entry in entries): raise ValueError("empty keychain path")
    for entry in entries: sys.stdout.buffer.write(os.fsencode(entry) + b"\0")
except ValueError as error:
    print(f"Invalid user keychain search list: {error}", file=sys.stderr)
    sys.exit(1)' > "$file"; then
    rm -f "$file"
    return 1
  fi
  USER_SEARCH_LIST=()
  while IFS= read -r -d '' entry; do USER_SEARCH_LIST+=("$entry"); done < "$file"
  rm -f "$file"
}

ensure_signing_search_list() {
  local keychain=$1 listed entry
  if [ "${DRY_RUN:-0}" = 1 ]; then
    plan_command security list-keychains -d user
    listed=${AMUX_CODESIGN_PLAN_SEARCH_LIST:-'"/Users/runner/Library/Keychains/login.keychain-db"'}
  else
    listed=$(security list-keychains -d user) || {
      echo 'WARN verdict=codesign_search_list_read_failed' >&2; return 1;
    }
  fi
  read_user_search_list "$listed" || {
    echo 'WARN verdict=codesign_search_list_parse_failed' >&2; return 1;
  }
  for entry in "${USER_SEARCH_LIST[@]}"; do
    if [ "$entry" = "$keychain" ]; then
      echo 'INFO verdict=codesign_search_list_present' >&2
      return 0
    fi
  done
  if [ "${DRY_RUN:-0}" = 1 ]; then
    plan_command security list-keychains -d user -s "${USER_SEARCH_LIST[@]}" "$keychain"
  else
    security list-keychains -d user -s "${USER_SEARCH_LIST[@]}" "$keychain" || {
      echo 'WARN verdict=codesign_search_list_append_failed' >&2; return 1;
    }
  fi
  echo 'INFO verdict=codesign_search_list_appended' >&2
}

check_search_list_plan() {
  SEARCH_LIST_PLAN=$1 SEARCH_LIST_PRIOR=$2 SEARCH_LIST_KEYCHAIN=$3 python3 -c 'import os, shlex, sys
try:
    prior = shlex.split(os.environ["SEARCH_LIST_PRIOR"])
    commands = [shlex.split(line[6:]) for line in os.environ["SEARCH_LIST_PLAN"].splitlines() if line.startswith("PLAN:")]
    search_commands = [cmd for cmd in commands if cmd[:2] == ["security", "list-keychains"]]
    expected = ["security", "list-keychains", "-d", "user", "-s", *prior, os.environ["SEARCH_LIST_KEYCHAIN"]]
    read = ["security", "list-keychains", "-d", "user"]
    assert read in search_commands, "search list read missing"
    assert expected in search_commands and all(cmd in (read, expected) for cmd in search_commands), "search list write must append the dedicated keychain after the prior list"
    for cmd in commands:
        if cmd[:2] in (["security", "default-keychain"], ["security", "add-trusted-cert"], ["security", "trust-settings"]):
            raise ValueError("forbidden keychain mutation")
        if cmd[:2] == ["security", "delete-keychain"] and (cmd[-1] != os.environ["SEARCH_LIST_KEYCHAIN"] or "amux-codesign-ci-" not in cmd[-1]):
            raise ValueError("delete-keychain outside the disposable keychain")
        if cmd[:1] == ["security"] and cmd[:2] != ["security", "list-keychains"] and any("login.keychain" in arg for arg in cmd[2:]):
            raise ValueError("login keychain mutation")
except (ValueError, AssertionError) as error:
    print(f"FAIL: {error}", file=sys.stderr)
    sys.exit(1)'
}

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
