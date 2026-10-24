#!/bin/bash
# Checks scripts/fork-build-speed.sh: its defaults apply, a caller's settings
# win, and the sccache opt-outs hold (refs maxtongwang/ai-config#261).
set -u
here="$(cd "$(dirname "$0")" && pwd)"
fail=0
probe() { # probe <env assignments...> -> prints the four resulting values
  env -i PATH="$PATH" HOME="$HOME" "$@" bash -c '
    . "$1"; printf "%s|%s|%s|%s\n" "$CARGO_INCREMENTAL" "$CARGO_PROFILE_DEV_DEBUG" \
      "$CARGO_PROFILE_TEST_DEBUG" "${RUSTC_WRAPPER-unset}"' _ "$here/fork-build-speed.sh"
}
check() { # check <name> <expected> <env assignments...>
  local name=$1 want=$2; shift 2
  local got; got="$(probe "$@")"
  if [ "$got" = "$want" ]; then echo "ok   $name"; else echo "FAIL $name: got $got want $want"; fail=1; fi
}
fakebin="$(mktemp -d)"; trap 'rm -rf "$fakebin"' EXIT
printf '#!/bin/sh\n' > "$fakebin/sccache"; chmod +x "$fakebin/sccache"
nosc="/usr/bin:/bin"  # a PATH without sccache
check "defaults: incremental, no sccache (it refuses incremental)" "1|line-tables-only|line-tables-only|unset" PATH="$fakebin:$nosc"
check "incremental off: sccache on PATH used" "0|line-tables-only|line-tables-only|sccache" PATH="$fakebin:$nosc" CARGO_INCREMENTAL=0
check "incremental off, no sccache on PATH" "0|line-tables-only|line-tables-only|unset" PATH="$nosc" CARGO_INCREMENTAL=0
check "caller settings win" "0|0|2|unset" PATH="$fakebin:$nosc" CARGO_INCREMENTAL=0 \
  CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=2 AMUX_CARGO_NO_SCCACHE=1
check "empty RUSTC_WRAPPER kept" "0|line-tables-only|line-tables-only|" PATH="$fakebin:$nosc" CARGO_INCREMENTAL=0 RUSTC_WRAPPER=
check "custom RUSTC_WRAPPER kept" "0|line-tables-only|line-tables-only|mywrap" PATH="$fakebin:$nosc" CARGO_INCREMENTAL=0 RUSTC_WRAPPER=mywrap
# safe-cargo must source the file before its own defaults, or they never apply.
sc="$here/safe-cargo.sh"
src_line=$(grep -n 'fork-build-speed.sh"' "$sc" | head -1 | cut -d: -f1)
def_line=$(grep -n '^export CARGO_INCREMENTAL=' "$sc" | head -1 | cut -d: -f1)
if [ -n "$src_line" ] && [ -n "$def_line" ] && [ "$src_line" -lt "$def_line" ]; then
  echo "ok   safe-cargo sources it before its defaults"
else echo "FAIL safe-cargo source order (source=${src_line:-none} defaults=${def_line:-none})"; fail=1; fi
exit $fail
