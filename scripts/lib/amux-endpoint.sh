# amux-endpoint.sh — isolated-run endpoint guard for amux's shell clients (sourced, POSIX sh).
# amux_endpoint_guard <url> <client> prints <url>, unless AMUX_ISOLATED_RUN=1 and <url>
# is empty or its PARSED port is live: then it prints the dead port and logs the refusal.
# Live ports: 8824, 8822 and every port in an endpoint.json the server wrote. The host
# is never consulted (the server binds 0.0.0.0). Same rule as scripts/lib/amux_endpoint.py;
# the Bash CLI carries a byte-identical copy of the block below, and
# test-endpoint-clients-isolated.py drives every implementation over one probe table.
# 2026-10-19: a test that unset AMUX_API/AMUX_URL sent a real request to 8824.
# >>> amux-endpoint-sh (identical in the amux CLI)
AMUX_ENDPOINT_DEAD_URL="http://127.0.0.1:9"
amux_endpoint_port() { # url -> the port a client connects to, or nothing
  _aep_u=$(printf '%s' "$1" | tr '[:upper:]' '[:lower:]' | sed 's/^[[:space:]]*//; s/[[:space:]]*$//')
  _aep_s=""
  case "$_aep_u" in *://*) _aep_s=${_aep_u%%://*}; _aep_u=${_aep_u#*://} ;; esac
  _aep_a=${_aep_u%%[/?#]*}; _aep_a=${_aep_a##*@}
  case "$_aep_a" in
    \[*\]:*) _aep_p=${_aep_a##*\]:} ;;
    \[*) _aep_p="" ;;
    *:*) _aep_p=${_aep_a##*:} ;;
    *) _aep_p="" ;;
  esac
  if [ -z "$_aep_p" ]; then
    case "$_aep_s" in http|ws) echo 80 ;; https|wss) echo 443 ;; esac
    return 0
  fi
  case "$_aep_p" in *[!0-9]*) return 0 ;; esac
  _aep_p=$(printf '%s' "$_aep_p" | sed 's/^0*//'); echo "${_aep_p:-0}"
}
amux_endpoint_live_ports() { # 8824, 8822 and every endpoint.json port, one per line
  echo 8824; echo 8822
  _aep_real=$(eval echo "~$(id -un 2>/dev/null)" 2>/dev/null)
  for _aep_f in "$_aep_real/.amux/endpoint.json" "${AMUX_HOME:+$AMUX_HOME/endpoint.json}" \
                "${CC_HOME:+$CC_HOME/endpoint.json}" "${HOME:+$HOME/.amux/endpoint.json}"; do
    [ -n "$_aep_f" ] && [ -r "$_aep_f" ] || continue
    tr -d '\n' < "$_aep_f" | grep -oE '"(canonical_port|legacy_port)"[[:space:]]*:[[:space:]]*[0-9]+|"retired_ports"[[:space:]]*:[[:space:]]*\[[^]]*\]|"canonical_url"[[:space:]]*:[[:space:]]*"[^"]*"' \
      | while IFS= read -r _aep_kv; do
          case "$_aep_kv" in
            *canonical_url*) amux_endpoint_port "$(printf '%s' "${_aep_kv#*:}" | tr -d ' "')" ;;
            *) printf '%s\n' "${_aep_kv#*:}" | grep -oE '[0-9]+' || true ;;
          esac
        done || true
  done
  return 0
}
amux_endpoint_guard() { # url client -> url, or the dead port under isolation
  _aeg_reason=""
  if [ "${AMUX_ISOLATED_RUN:-}" = 1 ]; then
    if [ -z "$(printf '%s' "$1" | tr -d '[:space:]')" ]; then
      _aeg_reason=unset_would_use_builtin
    else
      _aeg_port=$(amux_endpoint_port "$1")
      # A word match, not `| grep -q`: under pipefail grep's early exit
      # SIGPIPEs the writer and the pipeline reads as false.
      _aeg_ports=" $(amux_endpoint_live_ports | tr '\n' ' ') "
      case "$_aeg_ports" in *" $_aeg_port "*) [ -n "$_aeg_port" ] && _aeg_reason=live_port ;; esac
    fi
  fi
  if [ -n "$_aeg_reason" ]; then
    printf '[amux] verdict=isolated_live_endpoint_refused client=%s from=%s reason=%s to=%s measured=true n_considered=1\n' \
      "$2" "${1:-<unset>}" "$_aeg_reason" "$AMUX_ENDPOINT_DEAD_URL" >&2
    printf '%s\n' "$AMUX_ENDPOINT_DEAD_URL"
  else
    printf '%s\n' "$1"
  fi
}
# <<< amux-endpoint-sh
