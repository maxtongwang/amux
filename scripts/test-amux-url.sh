#!/usr/bin/env bash
# AMUX-3046 — `amux url` resolves the canonical amux base past a STALE $AMUX_URL.
#
# A lane started before a port move (8822 retired 2026-08-11, the compat bind
# then dropped) carries the dead address in its PROCESS env for its whole life —
# env cannot be re-read live — so `curl $AMUX_URL/...` fails at connect and reads
# as "server down". `amux url` reads the server-written ~/.amux/endpoint.json,
# which self-updates every boot, so `$(amux url)` survives the NEXT port move too.
#
# These pin the three fail-to-stale traps from the resolver spec, PLUS the
# discriminating case rule 7 demands: a resolver that ALWAYS returns :8824 would
# pass (a)-(e) while being broken — case (f) proves a live, non-retired env is
# actually honoured, so the tests can tell a real resolver from `echo :8824`.
#
# Runs the REAL dispatch path as a subprocess with HOME + $AMUX_URL controlled,
# so it exercises the shipped `url)` case, not a paraphrase of it.
set -euo pipefail
# Private tmux server, never the host fleet's (lib/test-isolation.sh).
# shellcheck source=lib/test-isolation.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib/test-isolation.sh"; amux_test_isolate_tmux "$(mktemp -d)" || exit 1
cd "$(dirname "$0")/.."
AMUX_BIN="${AMUX_BIN:-./amux}"
PASS=0; FAIL=0
check() { # label expected actual
  if [ "$2" = "$3" ]; then PASS=$((PASS+1));
  else FAIL=$((FAIL+1)); echo "FAIL: $1 — expected '$2' got '$3'"; fi
}

TMP=$(mktemp -d); mkdir -p "$TMP/.amux"
# Quiet + isolate the stale-URL warning (it probes a dead port for 2s otherwise).
export AMUX_SESSION="urltest"; export TMPDIR="$TMP"; touch "$TMP/amux-url-warn-urltest"
# curl is a refusing stub: the CLI's transport-breadcrumb flush must not reach
# any server from here, whatever the resolver answers.
mkdir -p "$TMP/stub"; printf '#!/bin/sh\nexit 7\n' > "$TMP/stub/curl"; chmod +x "$TMP/stub/curl"
export PATH="$TMP/stub:$PATH"
# Under amux-isolated the same cases pin the ISOLATED resolver instead: no
# endpoint.json is followed, a live port (8824, 8822, the fixtures' ports)
# resolves to the dead port, and any other env URL is kept.
ISO=0; [ "${AMUX_ISOLATED_RUN:-}" = 1 ] && ISO=1
DEAD="http://127.0.0.1:9"
want() { if [ "$ISO" = 1 ]; then printf '%s' "$2"; else printf '%s' "$1"; fi; } # normal isolated
run() { CC_HOME="$TMP/.amux" HOME="$TMP" AMUX_URL="$1" "$AMUX_BIN" url 2>/dev/null; }
write_ep() { printf '{"canonical_port":%s,"canonical_url":"%s","retired_ports":[8822]}\n' "$2" "$1" > "$TMP/.amux/endpoint.json"; }

# (a) file MISSING + env=retired  -> the :8824 literal (unconditional fallback).
rm -f "$TMP/.amux/endpoint.json"
check "(a) missing file + retired env -> literal 8824" "$(want https://localhost:8824 "$DEAD")" "$(run 'https://localhost:8822')"

# (b) file PRESENT + env live-but-different + NOT retired -> canonical (port-move window).
write_ep "https://localhost:8824" 8824
check "(b) present file + live-different env -> canonical" "$(want https://localhost:8824 https://localhost:9001)" "$(run 'https://localhost:9001')"

# (c) file PRESENT + env=retired -> canonical wins regardless of env.
check "(c) present file + retired env -> canonical" "$(want https://localhost:8824 "$DEAD")" "$(run 'https://localhost:8822')"

# (d) file PRESENT + no env -> canonical.
check "(d) present file + empty env -> canonical" "$(want https://localhost:8824 "$DEAD")" "$(run '')"

# (e) file MISSING + no env -> literal (never returns empty).
rm -f "$TMP/.amux/endpoint.json"
check "(e) missing file + empty env -> literal 8824" "$(want https://localhost:8824 "$DEAD")" "$(run '')"

# (f) file MISSING + env live + NOT retired -> env honoured (proves it isn't `echo :8824`).
check "(f) missing file + live non-retired env -> env honoured" "https://localhost:9001" "$(run 'https://localhost:9001')"

# A dedicated home must never consult the production endpoint in HOME.
write_ep "https://localhost:8824" 8824
mkdir -p "$TMP/lab"
printf '{"canonical_url":"https://localhost:19973"}\n' > "$TMP/lab/endpoint.json"
check "(g) explicit CC_HOME isolates endpoint discovery" "$(want https://localhost:19973 https://localhost:9001)" "$(CC_HOME="$TMP/lab" HOME="$TMP" AMUX_URL=https://localhost:9001 "$AMUX_BIN" url 2>/dev/null)"
rm "$TMP/lab/endpoint.json"
check "(h) empty isolated home uses its env, not production" "https://localhost:19973" "$(CC_HOME="$TMP/lab" HOME="$TMP" AMUX_URL=https://localhost:19973 "$AMUX_BIN" url 2>/dev/null)"

rm -rf "$TMP"
echo "amux url resolver ($([ "$ISO" = 1 ] && echo isolated || echo normal)): $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
