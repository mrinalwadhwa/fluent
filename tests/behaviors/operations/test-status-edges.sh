#!/usr/bin/env bash
# test-status-edges — Verify factory status edge cases via the Rust binary.
#
# Tests status display behaviors:
#   - Status when runtime file is missing
#   - Status when brief.md is missing
#   - Status with all known status values
#   - Status with review-mode run
#   - Status with no runs
#
# Usage:
#   tests/behaviors/operations/test-status-edges.sh

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

assert_output_contains() {
  if ! echo "$1" | grep -q "$2"; then
    printf '    FAIL: output does not contain "%s"\n' "$2"
    return 1
  fi
}

# -------------------------------------------------------------------------
# Tests
# -------------------------------------------------------------------------

test_status_missing_runtime_file() {
  TEST_DIR="$(mktemp -d -t factory-test-status-edge-XXXXXX)"

  mkdir -p "${TEST_DIR}/.factory/runs/run-no-runtime"
  printf 'planned' > "${TEST_DIR}/.factory/runs/run-no-runtime/status"
  printf 'A run without runtime file' > "${TEST_DIR}/.factory/runs/run-no-runtime/brief.md"

  OUTPUT="$(cd "$TEST_DIR" && "$FACTORY_BIN" status 2>&1 || true)"

  RESULT=0
  assert_output_contains "$OUTPUT" "run-no-runtime" || RESULT=1
  assert_output_contains "$OUTPUT" "planned" || RESULT=1

  rm -rf "$TEST_DIR"
  return $RESULT
}

test_status_missing_brief_file() {
  TEST_DIR="$(mktemp -d -t factory-test-status-edge-XXXXXX)"

  mkdir -p "${TEST_DIR}/.factory/runs/run-no-brief"
  printf 'executing' > "${TEST_DIR}/.factory/runs/run-no-brief/status"
  printf 'local' > "${TEST_DIR}/.factory/runs/run-no-brief/runtime"

  OUTPUT="$(cd "$TEST_DIR" && "$FACTORY_BIN" status 2>&1 || true)"

  RESULT=0
  assert_output_contains "$OUTPUT" "run-no-brief" || RESULT=1
  assert_output_contains "$OUTPUT" "executing" || RESULT=1

  rm -rf "$TEST_DIR"
  return $RESULT
}

test_status_all_known_statuses() {
  TEST_DIR="$(mktemp -d -t factory-test-status-edge-XXXXXX)"

  for STATUS in briefed behaviors-defined approach-designed planned executing rate-limited needs-user complete failed; do
    mkdir -p "${TEST_DIR}/.factory/runs/run-${STATUS}"
    printf '%s' "$STATUS" > "${TEST_DIR}/.factory/runs/run-${STATUS}/status"
    printf 'Brief for %s' "$STATUS" > "${TEST_DIR}/.factory/runs/run-${STATUS}/brief.md"
    printf 'local' > "${TEST_DIR}/.factory/runs/run-${STATUS}/runtime"
  done

  OUTPUT="$(cd "$TEST_DIR" && "$FACTORY_BIN" status 2>&1 || true)"

  RESULT=0
  for STATUS in briefed behaviors-defined approach-designed planned executing rate-limited needs-user complete failed; do
    assert_output_contains "$OUTPUT" "$STATUS" || RESULT=1
  done

  rm -rf "$TEST_DIR"
  return $RESULT
}

test_status_with_review_run() {
  TEST_DIR="$(mktemp -d -t factory-test-status-edge-XXXXXX)"

  mkdir -p "${TEST_DIR}/.factory/runs/review-test"
  printf 'executing' > "${TEST_DIR}/.factory/runs/review-test/status"
  printf 'Full review' > "${TEST_DIR}/.factory/runs/review-test/brief.md"
  printf 'local' > "${TEST_DIR}/.factory/runs/review-test/runtime"
  printf 'review' > "${TEST_DIR}/.factory/runs/review-test/mode"

  OUTPUT="$(cd "$TEST_DIR" && "$FACTORY_BIN" status 2>&1 || true)"

  RESULT=0
  assert_output_contains "$OUTPUT" "review-test" || RESULT=1
  assert_output_contains "$OUTPUT" "executing" || RESULT=1
  assert_output_contains "$OUTPUT" "local" || RESULT=1

  rm -rf "$TEST_DIR"
  return $RESULT
}

test_status_with_no_runs() {
  TEST_DIR="$(mktemp -d -t factory-test-status-edge-XXXXXX)"

  mkdir -p "${TEST_DIR}/.factory/runs"

  OUTPUT="$(cd "$TEST_DIR" && "$FACTORY_BIN" status 2>&1 || true)"

  RESULT=0
  # Should produce some output without crashing
  if [ -z "$OUTPUT" ]; then
    printf '    FAIL: status produced no output\n'
    RESULT=1
  fi

  rm -rf "$TEST_DIR"
  return $RESULT
}

# -------------------------------------------------------------------------
# Run all tests
# -------------------------------------------------------------------------

printf 'test-status-edges\n\n'

run_test "status with missing runtime file" test_status_missing_runtime_file
run_test "status with missing brief file" test_status_missing_brief_file
run_test "status displays all known status values" test_status_all_known_statuses
run_test "status with review-mode run" test_status_with_review_run
run_test "status with no runs" test_status_with_no_runs

printf '\n  %d passed, %d failed\n' "$PASS" "$FAIL"

if [ "$FAIL" -gt 0 ]; then
  printf '\n  Failures:%b\n' "$ERRORS"
  exit 1
fi
