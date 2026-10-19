#!/usr/bin/env bash
# test-cli-isolated-endpoint.sh [amux]: under amux-isolated (AMUX_ISOLATED_RUN=1)
# the Bash CLI never addresses the live server. An unset AMUX_API/AMUX_URL (the
# variable-unsetting path a test takes with `env -u`), an explicit live URL, the
# endpoint.json stale-URL heal and `amux url` all resolve to the dead port, each
# with a logged refusal. A test server's own URL is kept.
# 2026-10-19: test-cli-launch-unbound.sh unset both variables and sent a real
# `start` request to the live server on 8824.
# curl is stubbed on PATH: it records each URL and fails like a refused
# connection, so no case here can reach any server.
set -uo pipefail
AM="${1:-$(cd "$(dirname "$0")/.." && pwd)/amux}"
AM="$(cd "$(dirname "$AM")" && pwd)/$(basename "$AM")"
W="$(mktemp -d "${TMPDIR:-/tmp}/cie.XXXXXX")"
trap 'cd /; rm -rf -- "${W:?}"' EXIT
mkdir -p "$W/home/.amux/state" "$W/stub"
# endpoint.json names the live URL, as on a real host.
printf '{"canonical_port":8824,"canonical_url":"https://localhost:8824","legacy_port":8822,"retired_ports":[8822]}\n' \
  > "$W/home/.amux/endpoint.json"
cat > "$W/stub/curl" <<'EOF'
#!/usr/bin/env bash
for a in "$@"; do case "$a" in http://*|https://*) printf '%s\n' "$a" >> "$CURL_LOG" ;; esac; done
exit 7
EOF
chmod +x "$W/stub/curl"
export CURL_LOG="$W/curl.log"
PASS=0; FAIL=0
ok()  { echo "  ok   $1"; PASS=$((PASS+1)); }
bad() { echo "  FAIL $1"; [ -n "${2:-}" ] && printf '       %s\n' "$2"; FAIL=$((FAIL+1)); }

# run <isolated:0|1> <AMUX_API or -> <AMUX_URL or -> <args...>; output in $W/out, $W/err
run() {
  local iso="$1" api="$2" url="$3"; shift 3
  : > "$CURL_LOG"
  local envs=(-u AMUX_API -u AMUX_URL -u AMUX_ISOLATED_RUN)
  [ "$iso" = 1 ] && envs+=(AMUX_ISOLATED_RUN=1)
  [ "$api" != - ] && envs+=("AMUX_API=$api")
  [ "$url" != - ] && envs+=("AMUX_URL=$url")
  env "${envs[@]}" PATH="$W/stub:$PATH" HOME="$W/home" CC_HOME="$W/home/.amux" \
    AMUX_SESSION=cie AMUX_WORKER=cie TMPDIR="$W" \
    bash "$AM" "$@" >"$W/out" 2>"$W/err"
}
live_hits() { grep -cE '(localhost|127\.0\.0\.1|\[::1\]):(8824|8822)' "$CURL_LOG" 2>/dev/null || true; }
refused() { grep -c "verdict=isolated_live_endpoint_refused.*reason=$1" "$W/err" 2>/dev/null || true; }

# 1. Variable-unsetting path: no AMUX_API/AMUX_URL at all.
run 1 - - url --verify
[ "$(head -1 "$W/out")" = "http://127.0.0.1:9" ] && ok "unset vars: amux url resolves to the dead port" \
  || bad "unset vars: amux url printed $(head -1 "$W/out")" "$(cat "$W/err")"
[ "$(live_hits)" = 0 ] && grep -q '127.0.0.1:9/health' "$CURL_LOG" && ok "unset vars: the only request goes to the dead port" \
  || bad "unset vars: requests reached a live endpoint" "$(cat "$CURL_LOG")"
[ "$(refused unset_would_use_builtin)" -ge 1 ] && ok "unset vars: the refusal is logged" \
  || bad "unset vars: no unset_would_use_builtin refusal" "$(cat "$W/err")"

run 1 - - board ls
[ -s "$CURL_LOG" ] && [ "$(live_hits)" = 0 ] && ok "unset vars: a server verb (board ls) never addresses 8824" \
  || bad "unset vars: board ls requests" "$(cat "$CURL_LOG")"

# 2. An explicit live URL is refused.
run 1 https://localhost:8824 https://127.0.0.1:8824 url --verify
[ "$(live_hits)" = 0 ] && [ "$(refused live_endpoint)" -ge 2 ] && ok "explicit live AMUX_API/AMUX_URL are refused and logged" \
  || bad "explicit live URLs" "$(cat "$CURL_LOG") | $(cat "$W/err")"

# 3. The stale-URL heal must not rewrite the dead port to endpoint.json's live URL.
run 1 http://127.0.0.1:9 http://127.0.0.1:9 board ls
[ -s "$CURL_LOG" ] && [ "$(live_hits)" = 0 ] && [ "$(refused heal_to_live_endpoint)" -ge 1 ] \
  && ok "the endpoint.json heal to a live URL is refused and logged" \
  || bad "heal path" "$(cat "$CURL_LOG") | $(cat "$W/err")"

# 4. A test server's own URL is honoured.
run 1 http://127.0.0.1:41234 http://127.0.0.1:41234 board ls
grep -q '127.0.0.1:41234' "$CURL_LOG" && [ "$(live_hits)" = 0 ] && ok "a test server's own URL is kept" \
  || bad "test server URL" "$(cat "$CURL_LOG")"

# 5. Control: without AMUX_ISOLATED_RUN the same unset path does use the
#    built-in/endpoint.json URL, so the cases above can fail. Stubbed curl only.
run 0 - - url --verify
[ "$(live_hits)" -ge 1 ] && ! grep -q isolated_live_endpoint_refused "$W/err" \
  && ok "control: a normal run still resolves the live URL (the probe can see a hit)" \
  || bad "control: a normal run did not address the live URL" "$(cat "$CURL_LOG")"

echo
echo "  $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
