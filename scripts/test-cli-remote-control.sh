#!/usr/bin/env bash
# Preview the real local launch without contacting tmux or starting a server.
set -euo pipefail
case "${1:-}" in
  -h|--help) echo "Usage: $0 — verify Remote Control in local launch previews"; exit 0 ;;
  "") ;;
  *) echo "Unexpected argument: $1" >&2; exit 2 ;;
esac
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d)"
trap 'rm -rf -- "${TMP:?}"' EXIT
export AMUX_HOME="$TMP/home" CC_HOME="$TMP/home" HOME="$TMP/user"
unset AMUX_SESSION AMUX_WORKER AMUX_CLAUDE_CMD CC_PROVIDER CC_FLAGS CC_DEFAULT_FLAGS CC_ISOLATED
export AMUX_API=http://127.0.0.1:1 AMUX_URL=http://127.0.0.1:1
mkdir -p "$CC_HOME/sessions" "$HOME" "$TMP/bin"
for tool in tmux curl; do
  cat > "$TMP/bin/$tool" <<'STUB'
#!/usr/bin/env bash
echo forbidden >> "$AMUX_HOME/forbidden"
exit 97
STUB
  chmod +x "$TMP/bin/$tool"
done
export PATH="$TMP/bin:$PATH"
# Explicit failures: bash 3.2 (macOS /bin/bash) does not exit on a false [[ ]] under set -e.
fail() { echo "FAIL: $*" >&2; exit 1; }
for isolated in 0 1 true yes; do
  cat > "$CC_HOME/sessions/rc-worker.env" <<ENV
CC_DIR="$TMP"
CC_PROVIDER="claude"
CC_ISOLATED="$isolated"
ENV
  output="$(bash "$ROOT/amux" start rc-worker --dry-run)"
  [[ "$output" == *"--name rc-worker"* ]] || fail "CC_ISOLATED=$isolated: no --name: $output"
  if [[ "$isolated" == 0 ]]; then
    [[ "$output" == *"--remote-control rc-worker"* ]] || fail "no --remote-control: $output"
  else
    [[ "$output" != *"--remote-control"* ]] || fail "isolated worker got --remote-control: $output"
  fi
done
[[ ! -e "$AMUX_HOME/forbidden" ]] || fail "the preview reached tmux or curl"
echo "PASS: Claude launch preview names Remote Control and respects isolation"
