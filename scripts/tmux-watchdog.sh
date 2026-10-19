#!/usr/bin/env bash
# tmux-watchdog: detect tmux server death and recover the fleet.
#
# The tmux server hosts every worker session. macOS can kill it under memory
# pressure because tmux is "not RunningBoard jetsam managed" (confirmed in
# system logs 2026-09-25). When it dies, all workers die instantly and nothing
# in the existing supervision stack notices: the Rust server watchdog monitors
# the API, fleet-boot runs once at login, and launchd has no agent for tmux.
#
# This script is called by com.amux.tmux-watchdog.plist on a 30s interval.
# It is idempotent: if tmux is up, it exits immediately. If tmux is down, it
# restarts the server and calls start-all to recover the fleet.
#
# A fleet recovery is logged to both its own log and the server log (via the
# /api/log endpoint) so the event is visible in the dashboard's log analyzer.

set -uo pipefail

# >>> amux-endpoint-guard (rule: scripts/lib/amux-endpoint.sh; every copy identical, test-enforced)
_ae_dir=$(dirname "$0")
for _ae in "$_ae_dir/amux-endpoint.sh" "$_ae_dir/lib/amux-endpoint.sh" "$_ae_dir/../lib/amux-endpoint.sh"            "$(eval echo "~$(id -un 2>/dev/null)" 2>/dev/null)/.amux/hooks/amux-endpoint.sh"; do
  [ -r "$_ae" ] && . "$_ae" && break
done
command -v amux_endpoint_guard >/dev/null 2>&1 || amux_endpoint_guard() {  # stand-in: fixed live ports
  _aeg_a=$(printf '%s' "$1" | tr '[:upper:]' '[:lower:]' | sed 's/^[[:space:]]*//; s/[[:space:]]*$//; s|^[a-z][a-z0-9+.-]*://||; s|[/?#].*||; s|.*@||; s|^\[.*\]||')
  case "$_aeg_a" in *:*) _aeg_p=$(printf '%s' "${_aeg_a##*:}" | sed 's/^0*//') ;; *) _aeg_p="" ;; esac
  if [ "${AMUX_ISOLATED_RUN:-}" = 1 ] && { [ -z "$(printf '%s' "$1" | tr -d '[:space:]')" ] || [ "$_aeg_p" = 8824 ] || [ "$_aeg_p" = 8822 ]; }; then
    printf '[amux] verdict=isolated_live_endpoint_refused client=%s from=%s reason=stand_in to=http://127.0.0.1:9 measured=true n_considered=1\n' "$2" "${1:-<unset>}" >&2
    echo "http://127.0.0.1:9"
  else
    printf '%s\n' "$1"
  fi
}
# <<< amux-endpoint-guard
LOG="${HOME}/.amux/logs/tmux-watchdog.log"
AMUX_BIN="${AMUX_BIN:-$(cd "$(dirname "$0")/.." && pwd)/amux}"
STAGGER="${AMUX_START_ALL_STAGGER:-2}"
COOLDOWN_FILE="${HOME}/.amux/tmux-watchdog-last-recovery"
COOLDOWN_SECONDS=120

mkdir -p "$(dirname "$LOG")"

log() { printf '%s %s\n' "$(date '+%Y-%m-%dT%H:%M:%S%z')" "$*" >> "$LOG"; }

# Happy path: tmux is alive, nothing to do.
if tmux list-sessions &>/dev/null; then
    exit 0
fi

# tmux is dead. Check cooldown so a repeated failure does not spam start-all
# in a loop (each invocation spawns processes and writes logs).
if [[ -f "$COOLDOWN_FILE" ]]; then
    last_recovery=$(cat "$COOLDOWN_FILE" 2>/dev/null || echo 0)
    now=$(date +%s)
    elapsed=$(( now - last_recovery ))
    if (( elapsed < COOLDOWN_SECONDS )); then
        log "tmux still down, but last recovery was ${elapsed}s ago (cooldown ${COOLDOWN_SECONDS}s) -- skipping"
        exit 1
    fi
fi

log "=== tmux server is DOWN -- starting recovery ==="
log "memory_pressure: $(memory_pressure 2>/dev/null | grep 'free percentage' || echo 'unavailable')"
log "swap: $(sysctl -n vm.swapusage 2>/dev/null || echo 'unavailable')"

# Start a keepalive session to bring the tmux server back.
tmux new-session -d -s keepalive 'exec bash' 2>/dev/null
sleep 1

if ! tmux list-sessions &>/dev/null; then
    log "FAILED to restart tmux server -- cannot recover"
    exit 1
fi

session_count=$(tmux list-sessions 2>/dev/null | wc -l | tr -d ' ')
log "tmux server restored (${session_count} sessions). Starting fleet..."

# Record recovery time for cooldown.
date +%s > "$COOLDOWN_FILE"

# Notify the amux server so this shows up in the log analyzer.
base=$(amux_endpoint_guard "$("$AMUX_BIN" url 2>/dev/null || echo "https://localhost:8824")" tmux-watchdog)
curl -sk --max-time 5 -X POST -H 'Content-Type: application/json' \
    -d '{"level":"warn","message":"tmux-watchdog: tmux server was dead, recovered and restarting fleet","source":"tmux-watchdog"}' \
    "$base/api/log" >/dev/null 2>&1 || true

# Restart the fleet with a stagger to avoid a memory spike that could kill
# tmux again immediately.
export AMUX_START_ALL_STAGGER="$STAGGER"
summary=$("$AMUX_BIN" start-all 2>&1 | sed 's/\x1b\[[0-9;]*m//g')
rc=$?

printf '%s\n' "$summary" >> "$LOG"
log "start-all exited rc=$rc"

# Verify against the API, same pattern as fleet-boot.sh.
if curl -sk --max-time 5 "$base/health" 2>/dev/null | grep -q '"status":"ok"'; then
    running=$(curl -sk --max-time 10 "$base/api/sessions" 2>/dev/null \
        | python3 -c "import json,sys; ss=json.load(sys.stdin); print(len([s for s in ss if not s.get('archived') and s.get('running')]))" 2>/dev/null || echo '?')
    total=$(curl -sk --max-time 10 "$base/api/sessions" 2>/dev/null \
        | python3 -c "import json,sys; ss=json.load(sys.stdin); print(len([s for s in ss if not s.get('archived')]))" 2>/dev/null || echo '?')
    log "verdict: ${running}/${total} non-archived workers running after recovery"
fi

log "=== tmux recovery complete ==="
exit "$rc"
