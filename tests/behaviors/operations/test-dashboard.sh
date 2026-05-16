#!/usr/bin/env bash
# test-dashboard — Verify dashboard edge cases.
#
# Tests:
#   - Dashboard exits gracefully with no runs
#   - Dashboard handles invalid run-id
#   - Dashboard does not modify run state
#
# Usage:
#   tests/behaviors/operations/test-dashboard.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$(dirname "$(dirname "$SCRIPT_DIR")")")"
FACTORY_BIN="${PROJECT_DIR}/target/debug/factory"

PASS=0
FAIL=0
ERRORS=""

# -------------------------------------------------------------------------
# Helpers
# -------------------------------------------------------------------------

run_test() {
  TEST_NAME="$1"
  printf '  %s ... ' "$TEST_NAME"
  if ( eval "$2" ) 2>&1; then
    printf 'PASS\n'
    PASS=$((PASS + 1))
  else
    printf '\n'
    FAIL=$((FAIL + 1))
    ERRORS="${ERRORS}\n  - ${TEST_NAME}"
  fi
}

# -------------------------------------------------------------------------
# Tests
# -------------------------------------------------------------------------

test_dashboard_exits_gracefully_with_no_runs() {
  TEST_DIR="$(mktemp -d -t factory-test-dash-XXXXXX)"
  mkdir -p "${TEST_DIR}/.factory/runs"

  # Dashboard should exit non-zero (no runs) but not crash/segfault
  OUTPUT="$(cd "$TEST_DIR" && "$FACTORY_BIN" dashboard 2>&1 || true)"

  RESULT=0
  # Should mention no runs or exit cleanly — must not panic
  if echo "$OUTPUT" | grep -qi "panic"; then
    printf '    FAIL: dashboard panicked with no runs\n'
    RESULT=1
  fi

  rm -rf "$TEST_DIR"
  return $RESULT
}

test_dashboard_handles_invalid_run_id() {
  TEST_DIR="$(mktemp -d -t factory-test-dash-XXXXXX)"
  mkdir -p "${TEST_DIR}/.factory/runs/valid-run"
  printf 'planned' > "${TEST_DIR}/.factory/runs/valid-run/status"
  printf 'Test brief' > "${TEST_DIR}/.factory/runs/valid-run/brief.md"

  # Request a non-existent run-id — should not crash
  OUTPUT="$(cd "$TEST_DIR" && "$FACTORY_BIN" dashboard --run-id nonexistent 2>&1 || true)"

  RESULT=0
  if echo "$OUTPUT" | grep -qi "panic"; then
    printf '    FAIL: dashboard panicked with invalid run-id\n'
    RESULT=1
  fi

  rm -rf "$TEST_DIR"
  return $RESULT
}

test_dashboard_does_not_modify_run_state() {
  TEST_DIR="$(mktemp -d -t factory-test-dash-XXXXXX)"
  mkdir -p "${TEST_DIR}/.factory/runs/state-check"
  printf 'executing' > "${TEST_DIR}/.factory/runs/state-check/status"
  printf 'Test brief' > "${TEST_DIR}/.factory/runs/state-check/brief.md"

  # Record file state before
  BEFORE="$(find "${TEST_DIR}/.factory" -type f -exec md5 {} \; | sort)"

  # Run dashboard briefly (it will fail without a terminal, but should not
  # modify state regardless)
  cd "$TEST_DIR" && timeout 1 "$FACTORY_BIN" dashboard 2>/dev/null || true

  # Record file state after
  AFTER="$(find "${TEST_DIR}/.factory" -type f -exec md5 {} \; | sort)"

  RESULT=0
  if [ "$BEFORE" != "$AFTER" ]; then
    printf '    FAIL: dashboard modified run state files\n'
    RESULT=1
  fi

  rm -rf "$TEST_DIR"
  return $RESULT
}

# -------------------------------------------------------------------------
# Run all tests
# -------------------------------------------------------------------------

printf 'test-dashboard\n\n'

run_test "dashboard exits gracefully with no runs" test_dashboard_exits_gracefully_with_no_runs
run_test "dashboard handles invalid run-id" test_dashboard_handles_invalid_run_id
run_test "dashboard does not modify run state" test_dashboard_does_not_modify_run_state

printf '\n  %d passed, %d failed\n' "$PASS" "$FAIL"

if [ "$FAIL" -gt 0 ]; then
  printf '\n  Failures:%b\n' "$ERRORS"
  exit 1
fi
