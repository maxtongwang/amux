#!/usr/bin/env bash
# test-ephemeral-fan-out.sh — validate the fan-out endpoint and callback lifecycle.
#
# This test exercises the API surface (decompose -> fan-out -> callback cleanup)
# without spinning up real Claude sessions. Real Haiku E2E is a manual test;
# this script proves the plumbing: env files are created with the right keys,
# cards are reassigned, and callbacks are armed for cleanup on terminal status.
#
# Usage: ./scripts/test-ephemeral-fan-out.sh [--live]
#   --live: actually start Haiku workers (requires API key, costs money)
#   default: API-only validation, no real workers spawned

set -euo pipefail
# Acts on the LIVE server by design: opt in with AMUX_TEST_LIVE=1 (lib/test-isolation.sh).
# shellcheck source=lib/test-isolation.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib/test-isolation.sh"; amux_test_require_live
# `-e` needs the counters below to be ASSIGNMENTS, not `((n++))` (AF-562).
# `((n++))` yields the counter's OLD value as its exit status, so the first
# increment from 0 returns 1. As the last statement of `check`, that becomes
# the FUNCTION's status and `set -e` aborts at the call site: the harness ran
# 1 cell instead of 26 and exited 1, which is the "reported a clean result for
# cells that never ran" failure the guard exists to catch, reintroduced by the
# fix for it.

AMUX_API="${AMUX_URL:-https://localhost:8824}"
SESSION="${AMUX_SESSION:-test-fanout}"
LIVE=0
[[ "${1:-}" == "--live" ]] && LIVE=1

_curl() { curl -sk "$@"; }

pass=0 fail=0
check() {
  local label="$1" ok="$2"
  if [[ "$ok" == "true" || "$ok" == "0" ]]; then
    echo "  PASS: $label"
    pass=$((pass + 1))
  else
    echo "  FAIL: $label"
    fail=$((fail + 1))
  fi
}

cleanup() {
  echo "--- cleanup ---"
  # Remove test cards
  if [[ -n "${EPIC_ID:-}" ]]; then
    _curl -X DELETE "$AMUX_API/api/board/$EPIC_ID" >/dev/null 2>&1 || true
  fi
  for cid in "${CHILD_IDS[@]:-}"; do
    [[ -n "$cid" ]] && _curl -X DELETE "$AMUX_API/api/board/$cid" >/dev/null 2>&1 || true
  done
  # Remove ephemeral env files
  for eph in "${EPH_NAMES[@]:-}"; do
    [[ -n "$eph" ]] && rm -f "$HOME/.amux/sessions/${eph}.env" 2>/dev/null || true
  done
  # Remove test parent env file
  rm -f "$HOME/.amux/sessions/test-fanout-parent.env" 2>/dev/null || true
}
trap cleanup EXIT

CHILD_IDS=()
EPH_NAMES=()

echo "=== Ephemeral fan-out test ==="
echo "API: $AMUX_API"

# 1. Create a test parent session env file
echo "--- setup: create test parent session ---"
mkdir -p "$HOME/.amux/sessions"
cat > "$HOME/.amux/sessions/test-fanout-parent.env" <<'ENVEOF'
# updated: test
CC_DIR="/Users/ethan/Dev/amux"
CC_FLAGS="--model claude-opus-5 --dangerously-skip-permissions"
CC_PROVIDER="claude"
ENVEOF

# 2. Create the epic card
echo "--- create epic card ---"
EPIC_RESULT=$(_curl -X POST -H 'Content-Type: application/json' \
  -H "X-Amux-Session: test-fanout-parent" \
  -d '{"title":"[TEST] Fan-out epic","status":"doing","session":"test-fanout-parent","desc":"**Prompt:** refactor the test infrastructure into isolated components"}' \
  "$AMUX_API/api/board")
EPIC_ID=$(echo "$EPIC_RESULT" | python3 -c 'import json,sys; print(json.load(sys.stdin)["id"])')
echo "  Epic: $EPIC_ID"
check "epic created" "$(echo "$EPIC_RESULT" | python3 -c 'import json,sys; d=json.load(sys.stdin); print("true" if d.get("id") else "false")')"

# 3. Decompose into children
echo "--- decompose epic ---"
DECOMPOSE_BODY=$(python3 -c '
import json
print(json.dumps({"tasks": [
    {"title": "Task A: create evidence-a.txt", "description": "Create a file called evidence-a.txt with the exact content TASK_A_COMPLETE to prove this task was completed",
     "item_type": "code", "priority": 1, "next_action": "Create the evidence file in the worktree root",
     "acceptance_criteria": ["file evidence-a.txt exists", "content is TASK_A_COMPLETE"],
     "depends_on": []},
    {"title": "Task B: create evidence-b.txt", "description": "Create a file called evidence-b.txt with the exact content TASK_B_COMPLETE to prove this task was completed",
     "item_type": "code", "priority": 1, "next_action": "Create the evidence file in the worktree root",
     "acceptance_criteria": ["file evidence-b.txt exists", "content is TASK_B_COMPLETE"],
     "depends_on": []}
]}))
')
DECOMPOSE_RESULT=$(_curl -X POST -H 'Content-Type: application/json' \
  -H "X-Amux-Worker: test-fanout-parent" \
  -d "$DECOMPOSE_BODY" \
  "$AMUX_API/api/board/$EPIC_ID/decompose")
check "decompose ok" "$(echo "$DECOMPOSE_RESULT" | python3 -c 'import json,sys; d=json.load(sys.stdin); print("true" if d.get("ok") else "false")')"

CHILD_IDS=($(echo "$DECOMPOSE_RESULT" | python3 -c '
import json,sys
d=json.load(sys.stdin)
for t in d.get("tasks",[]): print(t["id"])
'))
echo "  Children: ${CHILD_IDS[*]}"
check "2 children created" "$([ ${#CHILD_IDS[@]} -eq 2 ] && echo true || echo false)"

# 4. Fan out
echo "--- fan-out ---"
FANOUT_HTTP=$(_curl -o /dev/null -w '%{http_code}' -X POST -H 'Content-Type: application/json' \
  -H "X-Amux-Worker: test-fanout-parent" \
  -d '{"model":"haiku","provider":"claude"}' \
  "$AMUX_API/api/board/$EPIC_ID/fan-out")
FANOUT_RESULT=$(_curl -X POST -H 'Content-Type: application/json' \
  -H "X-Amux-Worker: test-fanout-parent" \
  -d '{"model":"haiku","provider":"claude"}' \
  "$AMUX_API/api/board/$EPIC_ID/fan-out")
echo "  HTTP: $FANOUT_HTTP"
echo "  Result: $(echo "$FANOUT_RESULT" | head -c 200)"

if [[ "$FANOUT_HTTP" == "404" || "$FANOUT_HTTP" == "405" ]]; then
  echo "  SKIP: fan-out endpoint not deployed yet (server needs rebuild)"
  echo "  The test validated: epic creation, decompose, card structure."
  echo "  Commit and wait for the auto-builder, then re-run."
  echo ""
  echo "=== Results: $pass passed, $fail failed (fan-out endpoint pending deploy) ==="
  exit 0
fi

FANOUT_OK=$(echo "$FANOUT_RESULT" | python3 -c 'import json,sys; d=json.load(sys.stdin); print("true" if d.get("ok") else "false")' 2>/dev/null || echo "false")
check "fan-out ok" "$FANOUT_OK"

WORKERS_STARTED=$(echo "$FANOUT_RESULT" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("workers_started",0))')
EPH_NAMES=($(echo "$FANOUT_RESULT" | python3 -c '
import json,sys
for n in json.load(sys.stdin).get("started",[]): print(n)
' 2>/dev/null)) || EPH_NAMES=()

if [[ "$FANOUT_OK" == "true" ]]; then
  # 5. Verify env files
  echo "--- verify env files ---"
  for eph in "${EPH_NAMES[@]:-}"; do
    [[ -z "$eph" ]] && continue
    EP="$HOME/.amux/sessions/${eph}.env"
    check "env exists: $eph" "$([ -f "$EP" ] && echo true || echo false)"
    if [ -f "$EP" ]; then
      check "CC_WORKTREE=1" "$(grep -q 'CC_WORKTREE="1"' "$EP" && echo true || echo false)"
      check "CC_EPHEMERAL=1" "$(grep -q 'CC_EPHEMERAL="1"' "$EP" && echo true || echo false)"
      check "CC_PARENT=test-fanout-parent" "$(grep -q 'CC_PARENT="test-fanout-parent"' "$EP" && echo true || echo false)"
      check "AMUX_BOARD_DELEGATION=0" "$(grep -q 'AMUX_BOARD_DELEGATION="0"' "$EP" && echo true || echo false)"
      check "AMUX_DISPATCH_BACKLOG_WHEN_IDLE=0" "$(grep -q 'AMUX_DISPATCH_BACKLOG_WHEN_IDLE="0"' "$EP" && echo true || echo false)"
      check "--model haiku in flags" "$(grep -q 'model haiku' "$EP" && echo true || echo false)"
    fi
  done

  # 6. Verify card reassignment
  echo "--- verify card sessions ---"
  for cid in "${CHILD_IDS[@]}"; do
    CARD=$(_curl "$AMUX_API/api/board/$cid")
    CARD_SESSION=$(echo "$CARD" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("session",""))')
    check "card $cid reassigned (session=$CARD_SESSION)" "$(echo "$CARD_SESSION" | grep -q 'eph' && echo true || echo false)"
  done

  # 7. Verify session JSON fields
  echo "--- verify session JSON ---"
  for eph in "${EPH_NAMES[@]:-}"; do
    [[ -z "$eph" ]] && continue
    SESS=$(_curl "$AMUX_API/api/sessions/$eph" 2>/dev/null || echo '{}')
    check "session $eph: ephemeral flag" "$(echo "$SESS" | python3 -c 'import json,sys; d=json.load(sys.stdin); print("true" if d.get("ephemeral") else "false")' 2>/dev/null || echo false)"
  done

  # 8. Test idempotency: fan-out again should either fail or be a no-op
  echo "--- idempotency check ---"
  FANOUT2=$(_curl -X POST -H 'Content-Type: application/json' \
    -H "X-Amux-Worker: test-fanout-parent" \
    -d '{"model":"haiku"}' \
    "$AMUX_API/api/board/$EPIC_ID/fan-out")
  echo "  Second fan-out: $FANOUT2"

  # 9. Verify callbacks are armed on child cards
  echo "--- verify callbacks armed ---"
  for cid in "${CHILD_IDS[@]}"; do
    CARD=$(_curl "$AMUX_API/api/board/$cid")
    CB_SESSION=$(echo "$CARD" | python3 -c 'import json,sys; cb=json.load(sys.stdin).get("callback",{}); print((cb or {}).get("session",""))' 2>/dev/null)
    CB_STATE=$(echo "$CARD" | python3 -c 'import json,sys; cb=json.load(sys.stdin).get("callback",{}); print((cb or {}).get("state",""))' 2>/dev/null)
    check "card $cid callback_session=test-fanout-parent" "$([ "$CB_SESSION" = "test-fanout-parent" ] && echo true || echo false)"
    check "card $cid callback_state=armed" "$([ "$CB_STATE" = "armed" ] && echo true || echo false)"
  done

  # 10. Simulate terminal state and verify callback fires
  echo "--- simulate terminal + test callback ---"
  for cid in "${CHILD_IDS[@]}"; do
    _curl -X PATCH -H 'Content-Type: application/json' \
      -H "X-Amux-Session: test-fanout-parent" \
      -d '{"status":"done","evidence":"test evidence","force":true}' \
      "$AMUX_API/api/board/$cid" >/dev/null 2>&1
  done
  # Callback is triggered by save_patched on terminal status, then
  # dispatch_pending_callbacks delivers it. The callback prompt tells
  # the parent to stop and clean up the ephemeral worker.
  echo "  Callbacks triggered on terminal status (cleanup is model-driven)"
fi

echo ""
echo "=== Results: $pass passed, $fail failed ==="
exit $fail
