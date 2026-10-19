#!/usr/bin/env bash
# test-cli-isolated-endpoint.sh [amux]: under amux-isolated (AMUX_ISOLATED_RUN=1)
# the Bash CLI never addresses the live server. An unset AMUX_API/AMUX_URL (the
# variable-unsetting path a test takes with `env -u`) and any URL whose PARSED
# port is live (8824, 8822, an endpoint.json port; every host spelling) resolve
# to the dead port with a logged refusal; endpoint.json is never followed (no
# heal, no `amux url` lookup); a test server's own URL is kept.
# 2026-10-19: test-cli-launch-unbound.sh unset both variables and sent a real
# `start` request to the live server on 8824.
# curl is stubbed on PATH: it records each URL and fails like a refused
# connection, so no case here can reach any server.
set -euo pipefail
AM="${1:-$(cd "$(dirname "$0")/.." && pwd)/amux}"
AM="$(cd "$(dirname "$AM")" && pwd)/$(basename "$AM")"
W="$(mktemp -d "${TMPDIR:-/tmp}/cie.XXXXXX")"
trap 'cd /; rm -rf -- "${W:?}"' EXIT
mkdir -p "$W/home/.amux/state" "$W/stub"
# endpoint.json in the server's own format, naming a NON-8824 live port.
printf '{"canonical_port":9824,"canonical_url":"https://localhost:9824","legacy_port":null,"retired_ports":[8822]}\n' \
  > "$W/home/.amux/endpoint.json"
cat > "$W/stub/curl" <<'EOF'
#!/usr/bin/env bash
for a in "$@"; do case "$a" in http://*|https://*|[A-Za-z]*://*) printf '%s\n' "$a" >> "$CURL_LOG" ;; esac; done
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
    bash "$AM" "$@" >"$W/out" 2>"$W/err" || true
}
# Any request whose parsed port is a live one (8824, 8822, the fixture's 9824).
live_hits() { grep -ciE '^[^/]*//([^/@]*@)?(\[[^]]*\]|[^/:?#]*):0*(8824|8822|9824)([/?#]|$)' "$CURL_LOG" || true; }
refused() { grep -c "verdict=isolated_live_endpoint_refused.*reason=$1" "$W/err" || true; }

# 1. Variable-unsetting path: no AMUX_API/AMUX_URL at all.
run 1 - - url --verify
[ "$(head -1 "$W/out")" = "http://127.0.0.1:9" ] && ok "unset vars: amux url resolves to the dead port" \
  || bad "unset vars: amux url printed $(head -1 "$W/out")" "$(cat "$W/err")"
[ "$(live_hits)" = 0 ] && grep -q '127.0.0.1:9/health' "$CURL_LOG" && ok "unset vars: the only request goes to the dead port" \
  || bad "unset vars: requests reached a live endpoint" "$(cat "$CURL_LOG")"
[ "$(refused unset_would_use_builtin)" -ge 1 ] && ok "unset vars: the refusal is logged" \
  || bad "unset vars: no unset_would_use_builtin refusal" "$(cat "$W/err")"
grep -q 'verdict=isolated_endpoint_json_not_followed use=url' "$W/err" && ok "endpoint.json is not followed by amux url, and that is logged" \
  || bad "amux url did not log that endpoint.json was skipped" "$(cat "$W/err")"

run 1 - - board ls
[ -s "$CURL_LOG" ] && [ "$(live_hits)" = 0 ] && ok "unset vars: a server verb (board ls) never addresses a live port" \
  || bad "unset vars: board ls requests" "$(cat "$CURL_LOG")"

# 2. Every spelling two reviews used to escape the old host lists, as AMUX_API and AMUX_URL.
# Fail closed: anything but a clean test URL is refused, live port or not.
for u in https://LOCALHOST:8824 HTTPS://localhost:8824 http://0.0.0.0:8824 https://127.1:8824 \
         'https://[::ffff:127.0.0.1]:8824' https://localhost.:8824 https://user@localhost:8824 \
         https://localhost:08824 "https://$(hostname):8824" https://localhost:9824 'https://[::1]:8822' \
         'http://127.0.0.1:+8824' 'http://127.0.0.1: 8824' 'http://127.0.0.1:88_24' 'http://127.0.0.1:8824\x' \
         'http://127.0.0.1:8824\@localhost:9999' "$(printf 'http://127.0.0.1:88\t24')" https://cloud.example; do
  run 1 "$u" "$u" board ls
  if [ -s "$CURL_LOG" ] && [ "$(live_hits)" = 0 ] && [ "$(refused '')" -ge 2 ]; then
    ok "refused and logged: $u"
  else
    bad "not refused: $u" "$(cat "$CURL_LOG") | $(cat "$W/err")"
  fi
done

# 3. The stale-URL heal never runs under isolation: the dead port stays dead.
run 1 http://127.0.0.1:9 http://127.0.0.1:9 board ls
[ -s "$CURL_LOG" ] && [ "$(live_hits)" = 0 ] && grep -q 'verdict=isolated_endpoint_json_not_followed use=heal' "$W/err" \
  && ok "the endpoint.json heal is skipped and logged" \
  || bad "heal path" "$(cat "$CURL_LOG") | $(cat "$W/err")"

# 4. A test server's own URL is honoured.
run 1 http://127.0.0.1:41234 http://127.0.0.1:41234 board ls
grep -q '127.0.0.1:41234' "$CURL_LOG" && [ "$(live_hits)" = 0 ] && ok "a test server's own URL is kept" \
  || bad "test server URL" "$(cat "$CURL_LOG")"

# 5. No transport-breadcrumb POST under isolation, even with a backlog queued.
printf '{"ts":1,"curl_exit":7,"method":"GET","url":"https://localhost:8824/x","session":"cie","cli":"bash"}\n' \
  > "$W/home/.amux/cli-transport.jsonl"
run 1 http://127.0.0.1:41234 http://127.0.0.1:41234 board ls
! grep -q 'client-debug' "$CURL_LOG" && [ -s "$W/home/.amux/cli-transport.jsonl" ] \
  && ok "no breadcrumb flush under isolation; the backlog is kept" \
  || bad "breadcrumbs were flushed under isolation" "$(cat "$CURL_LOG")"
rm -f "$W/home/.amux/cli-transport.jsonl"

# 6. Control: without AMUX_ISOLATED_RUN the same unset path does use endpoint.json,
#    so the cases above can fail. Stubbed curl only.
run 0 - - url --verify
[ "$(live_hits)" -ge 1 ] && ! grep -q isolated_live_endpoint_refused "$W/err" \
  && ok "control: a normal run still resolves the live URL (the probe can see a hit)" \
  || bad "control: a normal run did not address the live URL" "$(cat "$CURL_LOG")"

echo
echo "  $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
