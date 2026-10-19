# amux-endpoint.sh — isolated-run endpoint guard for amux's shell clients (sourced).
# amux_endpoint_guard <url> <client> prints <url>, unless AMUX_ISOLATED_RUN=1 and
# <url> is empty or names a loopback 8824/8822: then it prints the dead port and
# logs the refusal on stderr. Same rule as scripts/lib/amux_endpoint.py and the
# Bash CLI's _amux_isolated_guard (2026-10-19: a test that unset AMUX_API/AMUX_URL
# sent a real request to 8824). Installed hook copies get this file beside them.
AMUX_ENDPOINT_DEAD_URL="http://127.0.0.1:9"
amux_endpoint_guard() {
  _aeg_url="$1"; _aeg_reason=""
  if [ "${AMUX_ISOLATED_RUN:-}" = 1 ]; then
    if [ -z "$_aeg_url" ]; then
      _aeg_reason=unset_would_use_builtin
    else
      case "${_aeg_url%/}" in
        http://localhost:8824*|https://localhost:8824*|http://127.0.0.1:8824*|https://127.0.0.1:8824*|\
        http://\[::1\]:8824*|https://\[::1\]:8824*|\
        http://localhost:8822*|https://localhost:8822*|http://127.0.0.1:8822*|https://127.0.0.1:8822*)
          _aeg_reason=live_endpoint ;;
      esac
    fi
  fi
  if [ -n "$_aeg_reason" ]; then
    printf '[amux] verdict=isolated_live_endpoint_refused client=%s from=%s reason=%s to=%s measured=true n_considered=1\n' \
      "$2" "${_aeg_url:-<unset>}" "$_aeg_reason" "$AMUX_ENDPOINT_DEAD_URL" >&2
    _aeg_url="$AMUX_ENDPOINT_DEAD_URL"
  fi
  printf '%s\n' "$_aeg_url"
}
