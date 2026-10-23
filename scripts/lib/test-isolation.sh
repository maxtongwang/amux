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
#   - puts tmux on a PRIVATE server: TMUX and TMUX_PANE unset, TMUX_TMPDIR set
#     to <tmpdir>/tmux, and that server killed when the test shell exits
#     (2026-10-16: a test's rename probe renamed the live amux-dev-lead session
#     through the default socket),
#   - REFUSES (returns 1 with a reason on stderr) when <tmpdir> is the user's
#     real home or lies inside the real ~/.amux, when an endpoint.json is
#     reachable from the resolved home (the CLI could still find a server), or
#     when tmux would resolve the real user's default socket (TMUX_TMPDIR unset
#     or the system tmp).
#
# Never call tmux on the default socket from a test, not even read-only. Call
# tmux as `amux_tmux ...`: it clears TMUX/TMUX_PANE and names the private
# socket (`-S $AMUX_TMUX_SOCKET`). TMUX_TMPDIR alone is NOT isolation: inside a
# pane an inherited $TMUX wins, and a test's `TMUX_TMPDIR=... tmux kill-server`
# killed the live server that way (2026-10-16 outage). The amux CLI under test
# wraps its own tmux calls the same way when AMUX_TMUX_SOCKET is set.
# scripts/lint-tmux-socket.py enforces the explicit socket in CI.
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

# Is $1 a tmux socket directory other than the real user's default? The
# default is TMUX_TMPDIR unset (tmux uses /tmp) or the system tmp itself.
_amux_private_tmux_dir() {
  local d="${1:-}" r s
  [ -n "$d" ] && [ -d "$d" ] || return 1
  r=$(_amux_resolve "$d")
  for s in / /tmp /private/tmp /var/tmp "${TMPDIR:-/tmp}"; do
    [ "$r" = "$(_amux_resolve "$s")" ] && return 1
  done
  return 0
}

# The explicit socket for a private dir: the path tmux itself resolves for
# TMUX_TMPDIR=$1 (<dir>/tmux-<uid>/default), so -S and TMUX_TMPDIR callers
# reach the same private server. Creates its directory 0700.
_amux_tmux_socket_in() {
  local d="$1/tmux-$(id -u)"
  mkdir -p "$d" && chmod 700 "$d"
  printf '%s/default\n' "$d"
}

# Kill the private tmux server whose sockets live in $1 (never the default).
# Explicit -S, TMUX/TMUX_PANE cleared: TMUX_TMPDIR alone loses to an inherited
# TMUX, which is how a test's kill-server killed the live server (2026-10-16).
_amux_kill_private_tmux() {
  _amux_private_tmux_dir "${1:-}" || return 0
  local bin; bin=$(command -v tmux 2>/dev/null) || return 0
  env -u TMUX -u TMUX_PANE "$bin" -S "$(_amux_tmux_socket_in "$1")" kill-server >/dev/null 2>&1 || true
}

# THE way a test calls tmux: TMUX/TMUX_PANE cleared and the private socket
# named explicitly. Every tmux a test runs itself goes through this (the CLI
# under test picks up AMUX_TMUX_SOCKET the same way).
amux_tmux() {
  if [ -z "${AMUX_TMUX_SOCKET:-}" ]; then
    echo "amux_tmux: REFUSING: no private socket (call amux_test_isolate first)" >&2; return 1
  fi
  env -u TMUX -u TMUX_PANE tmux -S "$AMUX_TMUX_SOCKET" "$@"
}

# Explicit teardown, for a test that wants the server gone before it exits.
amux_test_teardown() { _amux_kill_private_tmux "${TMUX_TMPDIR:-}"; }

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
  if ! _amux_private_tmux_dir "${TMUX_TMPDIR:-}"; then
    echo "amux_test_isolate: REFUSING: TMUX_TMPDIR='${TMUX_TMPDIR:-}' resolves to the real user's default tmux socket" >&2; return 1
  fi
  case "${AMUX_TMUX_SOCKET:-}" in
    "$TMUX_TMPDIR"/*) ;;
    *) echo "amux_test_isolate: REFUSING: AMUX_TMUX_SOCKET='${AMUX_TMUX_SOCKET:-}' is not the private socket under $TMUX_TMPDIR" >&2; return 1 ;;
  esac
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
  amux_test_isolate_tmux "$tmp" || return 1
  amux_test_assert_isolated
}

# The tmux half alone, for a test that already pins its own homes: TMUX and
# TMUX_PANE unset, TMUX_TMPDIR=<tmpdir>/tmux, refuse the default socket, and
# kill the private server when the test shell exits.
amux_test_isolate_tmux() {
  local tmp="${1:-}"
  if [ -z "$tmp" ] || [ ! -d "$tmp" ]; then
    echo "amux_test_isolate_tmux: needs an existing temp directory (got '${tmp}')" >&2; return 1
  fi
  unset TMUX TMUX_PANE
  mkdir -p "$tmp/tmux"
  export TMUX_TMPDIR="$tmp/tmux"
  if ! _amux_private_tmux_dir "$TMUX_TMPDIR"; then
    echo "amux_test_isolate: REFUSING: TMUX_TMPDIR='$TMUX_TMPDIR' resolves to the real user's default tmux socket" >&2; return 1
  fi
  # The explicit socket every tmux call names: amux_tmux, and the amux CLI
  # under test (it wraps its own tmux calls with -S when this is set).
  AMUX_TMUX_SOCKET=$(_amux_tmux_socket_in "$TMUX_TMPDIR") || return 1
  export AMUX_TMUX_SOCKET
  # Teardown that survives the test's own EXIT trap (which may replace any
  # trap set here): a detached watcher kills the private server once this
  # shell is gone.
  local dir="$TMUX_TMPDIR" parent=$$
  ( while kill -0 "$parent" 2>/dev/null; do sleep 1; done; _amux_kill_private_tmux "$dir" ) </dev/null >/dev/null 2>&1 &
  disown 2>/dev/null || true
  return 0
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
