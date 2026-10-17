#!/usr/bin/env bash
# Shared hermetic environment for any test that runs the `amux` CLI or talks to
# a server. Source it, then call `amux_test_isolate <tmpdir>` BEFORE the first
# CLI call:
#
#   source "$(dirname "$0")/lib/test-isolation.sh"
#   TMP=$(mktemp -d); amux_test_isolate "$TMP" || exit 1
#
# WHY (2026-10-16): a CLI test pointed only HOME at a temp dir. The CLI reads
# CC_HOME before HOME, every amux worker exports CC_HOME=~/.amux, and the CLI
# heals a loopback AMUX_API to $CC_HOME/endpoint.json's canonical_url. So the
# test's requests went to the LIVE server: five test workers and two tmux
# sessions were created on the real fleet. Setting HOME alone is not isolation.
#
# What it does:
#   - pins HOME, CC_HOME and AMUX_HOME into <tmpdir> (exported),
#   - clears every inherited AMUX_* and TMUX* variable (lane identity, worker
#     token, API/URL, tmux socket and pane), keeping only AMUX_BIN and
#     AMUX_TEST_*, which are the test's own inputs,
#   - points AMUX_API and AMUX_URL at a dead loopback port (unset, the CLI
#     falls back to the live https://localhost:8824),
#   - REFUSES (returns 1 with a reason on stderr) when <tmpdir> is the user's
#     real home or lies inside the real ~/.amux, or when an endpoint.json is
#     reachable from the resolved home, i.e. the CLI could still find a server.
#
# Set the test's own AMUX_API/AMUX_URL (a loopback recorder) AFTER calling it.
# `amux_test_assert_isolated` re-checks the invariants, for a test that builds
# its environment in steps.

# The account's real home from the password database: $HOME may already be
# overridden by the caller, which is the case this exists to catch.
_amux_real_home() {
  python3 -c 'import os, pwd; print(pwd.getpwuid(os.getuid()).pw_dir)' 2>/dev/null || printf '%s\n' ~
}

_amux_resolve() { (cd "$1" 2>/dev/null && pwd -P) || printf '%s\n' "$1"; }

amux_test_assert_isolated() {
  local real home cc
  real=$(_amux_resolve "$(_amux_real_home)")
  home=$(_amux_resolve "${HOME:-}")
  cc=$(_amux_resolve "${CC_HOME:-${HOME:-}/.amux}")
  if [ -z "${HOME:-}" ] || [ "$home" = "$real" ]; then
    echo "amux_test_isolate: REFUSING: HOME resolves to the real home ($real)" >&2; return 1
  fi
  case "$cc/" in
    "$real/.amux/"*) echo "amux_test_isolate: REFUSING: CC_HOME resolves into the real ~/.amux ($cc)" >&2; return 1 ;;
  esac
  case "$home/" in
    "$real/.amux/"*) echo "amux_test_isolate: REFUSING: HOME resolves into the real ~/.amux ($home)" >&2; return 1 ;;
  esac
  local ep
  for ep in "$cc/endpoint.json" "$home/.amux/endpoint.json" "${AMUX_HOME:-$cc}/endpoint.json"; do
    if [ -e "$ep" ]; then
      echo "amux_test_isolate: REFUSING: a server endpoint is reachable from the test home ($ep)" >&2; return 1
    fi
  done
  local v
  for v in AMUX_API AMUX_URL; do
    case "${!v:-}" in
      "") echo "amux_test_isolate: REFUSING: $v is unset; the CLI would fall back to the live https://localhost:8824" >&2; return 1 ;;
      *localhost:8824*|*127.0.0.1:8824*) echo "amux_test_isolate: REFUSING: $v points at the live server port (${!v})" >&2; return 1 ;;
    esac
  done
  for v in AMUX_SESSION AMUX_WORKER AMUX_WORKER_TOKEN TMUX TMUX_PANE; do
    if [ -n "${!v:-}" ]; then
      echo "amux_test_isolate: REFUSING: $v is still set; a lane identity would ride the test's requests" >&2; return 1
    fi
  done
  return 0
}

amux_test_isolate() {
  local tmp="${1:-}"
  if [ -z "$tmp" ] || [ ! -d "$tmp" ]; then
    echo "amux_test_isolate: needs an existing temp directory (got '${tmp}')" >&2; return 1
  fi
  local v
  for v in $(compgen -e); do
    case "$v" in
      AMUX_BIN|AMUX_TEST_*) ;;
      AMUX_*|TMUX|TMUX_*) unset "$v" ;;
    esac
  done
  export HOME="$tmp" CC_HOME="$tmp/.amux" AMUX_HOME="$tmp/.amux" AMUX_TEST_ISOLATED=1
  # A DEAD loopback endpoint, not an unset one: with AMUX_API/AMUX_URL unset
  # the CLI falls back to the literal https://localhost:8824, which is the
  # live server on every amux machine, reached as the owner. A test that
  # stands up a recorder exports its own URL after this.
  export AMUX_API="$AMUX_TEST_DEAD_URL" AMUX_URL="$AMUX_TEST_DEAD_URL"
  # The session hooks probe health at their own default (the live port).
  export AMUX_HEALTH_URL="$AMUX_TEST_DEAD_URL/health"
  mkdir -p "$CC_HOME"
  amux_test_assert_isolated
}

# For the few tests that exist to exercise a RUNNING server (they create and
# delete live cards, send through live connectors, reconcile into live
# history): they run only when the caller opts in with AMUX_TEST_LIVE=1, and
# otherwise skip with exit 2 and say why. Running one from a lane by accident
# is how a test writes into the fleet the owner is using.
amux_test_require_live() {
  if [ "${AMUX_TEST_LIVE:-}" != 1 ]; then
    echo "SKIP: $(basename "$0") acts on the LIVE amux server; run it deliberately with AMUX_TEST_LIVE=1." >&2
    exit 2
  fi
}

# Port 9 (discard) on loopback: nothing listens there, so a request fails fast.
AMUX_TEST_DEAD_URL="${AMUX_TEST_DEAD_URL:-http://127.0.0.1:9}"
