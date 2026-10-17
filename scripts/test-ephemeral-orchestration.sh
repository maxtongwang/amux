#!/bin/bash
# Test script to verify ephemeral worker creation and execution
# Runs as part of the orchestration system tests

set -e
# Acts on the LIVE server by design: opt in with AMUX_TEST_LIVE=1 (lib/test-isolation.sh).
# shellcheck source=lib/test-isolation.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib/test-isolation.sh"; amux_test_require_live

worker="${AMUX_SESSION:-unknown}"
is_ephemeral="${CC_EPHEMERAL:-0}"
has_worktree="${CC_WORKTREE:-0}"

echo "=== Ephemeral Orchestration Test ==="
echo "Worker: $worker"
echo "Ephemeral: $is_ephemeral"
echo "Worktree: $has_worktree"

# Test 1: Verify environment
if [ "$is_ephemeral" = "1" ]; then
  echo "[PASS] Worker is ephemeral"
else
  echo "[FAIL] Worker is not ephemeral"
  exit 1
fi

# Test 2: Verify worktree isolation
if [ "$has_worktree" = "1" ]; then
  echo "[PASS] Worker has isolated worktree"
else
  echo "[FAIL] Worker doesn't have worktree"
  exit 1
fi

# Test 3: Verify git access
if git -C "$AMUX_REPO_DIR" status > /dev/null 2>&1; then
  echo "[PASS] Worker can access git repository"
else
  echo "[FAIL] Worker cannot access git"
  exit 1
fi

# Test 4: Verify board access
if curl -sk "$AMUX_URL/api/board/$worker" > /dev/null 2>&1; then
  echo "[PASS] Worker can access amux board API"
else
  echo "[FAIL] Worker cannot access board API"
  exit 1
fi

echo ""
echo "All tests passed! Ephemeral orchestration system working correctly."
