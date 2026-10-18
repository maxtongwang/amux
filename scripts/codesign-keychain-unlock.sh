#!/usr/bin/env bash
# Keep the dedicated signing keychain unlocked, run on every builder tick (Claude review B4 on 24b1fc1).
# After a reboot or logout the keychain is locked but still on the user search list, and any app that
# searches the list could then ask Max for a password he never set. Unlocking it from the 0600
# password file on every tick closes that window. No keychain yet (before setup): nothing to do.
# Usage: scripts/codesign-keychain-unlock.sh   (DRY_RUN=1 prints the plan)
# Exit: 0 always (a tick must never fail on this); the verdict goes to stderr only when it changes.
set -uo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
source "$here/codesign-keychain-guard.sh"

[ "$(uname -s)" = Darwin ] || exit 0
keychain="$HOME/Library/Keychains/amux-signing.keychain-db"
password_file="$HOME/.amux/codesign/keychain-password"
state_file="$HOME/.amux/codesign/unlock-verdict"

report() {
  local verdict=$1 previous=''
  previous=$(cat "$state_file" 2>/dev/null || true)
  [ "$verdict" = "$previous" ] && return 0
  printf '%s\n' "$verdict" > "$state_file" 2>/dev/null || true
  echo "INFO verdict=$verdict keychain=$keychain" >&2
}

if [ "${DRY_RUN:-0}" = 1 ]; then
  plan_command security unlock-keychain -p '<password>' "$keychain"
  exit 0
fi
[ -e "$keychain" ] || exit 0
if [ -L "$keychain" ] || [ ! -f "$password_file" ] || [ -L "$password_file" ] ||
   [ "$(stat -f %Lp "$password_file" 2>/dev/null)" != 600 ]; then
  report codesign_keychain_unlock_skipped_password_file
  exit 0
fi
if run_sensitive security unlock-keychain -p "$(cat "$password_file")" "$keychain" >/dev/null 2>&1; then
  report codesign_keychain_unlocked
else
  report codesign_keychain_unlock_failed
fi
exit 0
