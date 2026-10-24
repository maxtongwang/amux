# shellcheck shell=bash
# Fork-only build-speed defaults (refs maxtongwang/ai-config#261), sourced by
# safe-cargo.sh BEFORE its own defaults so these win only where the caller
# set nothing. Kept in this one file so upstream's scripts stay a one-line
# diff to rebase. Every value is an env default: a caller's setting wins.

# Incremental compilation: without it every edit to amux-server recompiles the
# whole 435k-line crate. A memory-constrained box can still set
# CARGO_INCREMENTAL=0 (upstream FRONT-2).
export CARGO_INCREMENTAL="${CARGO_INCREMENTAL:-1}"

# File/line backtraces without full DWARF.
export CARGO_PROFILE_DEV_DEBUG="${CARGO_PROFILE_DEV_DEBUG:-line-tables-only}"
export CARGO_PROFILE_TEST_DEBUG="${CARGO_PROFILE_TEST_DEBUG:-line-tables-only}"

# sccache refuses to run while CARGO_INCREMENTAL=1 ("incremental compilation
# is prohibited", sccache 0.18), so it applies only to non-incremental runs
# (CARGO_INCREMENTAL=0, e.g. cold Local-CI builds), where it caches every crate
# across target dirs. A set RUSTC_WRAPPER (even empty) wins;
# AMUX_CARGO_NO_SCCACHE=1 opts out.
if [ "$CARGO_INCREMENTAL" = 0 ] && [ -z "${RUSTC_WRAPPER+x}" ] &&
  [ "${AMUX_CARGO_NO_SCCACHE:-0}" != 1 ] && command -v sccache >/dev/null 2>&1; then
  export RUSTC_WRAPPER=sccache
fi
