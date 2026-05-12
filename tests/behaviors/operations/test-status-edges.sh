#!/usr/bin/env bash
# test-status-edges — Verify factory status edge cases.
#
# Tests status display behaviors not covered by other test suites:
#   - Status display when backend file is missing
#   - Status display when brief.md is missing
#   - Status display with all known status values
#
# Sources the factory script in library mode to call functions directly.
#
# Usage:
#   tests/behaviors/operations/test-status-edges.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$(dirname "$(dirname "$SCRIPT_DIR")")")"
FACTORY="${PROJECT_DIR}/scripts/factory"

PASS=0
FAIL=0
ERRORS=""

# Source factory functions (library mode — no dispatch)
FACTORY_LIB=1 . "$FACTORY"

# -------------------------------------------------------------------------
# Helpers
# -------------------------------------------------------------------------

setup_test_project() {
  TEST_DIR="$(mktemp -d -t factory-test-status-edge-XXXXXX)"
  mkdir -p "${TEST_DIR}/main"
  cd "${TEST_DIR}/main"
  git init -b main > /dev/null 2>&1
  git config commit.gpgsign false
  git config user.email "test@test"
  git config user.name "test"
  echo "test" > README.md
  git add . && git commit -m "init" > /dev/null 2>&1
}

cleanup_test_project() {
  cd /
  rm -rf "$TEST_DIR"
}

assert_output_contains() {
  if ! echo "$1" | grep -q "$2"; then
    printf '    FAIL: output does not contain "%s"\n' "$2"
    return 1
  fi
}

assert_output_not_empty() {
  if [ -z "$1" ]; then
    printf '    FAIL: output is empty\n'
    return 1
  fi
}

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

test_status_missing_backend_file() {
  setup_test_project

  mkdir -p ".factory/runs/run-no-backend"
  printf 'planned' > ".factory/runs/run-no-backend/status"
  printf 'A run without backend file' > ".factory/runs/run-no-backend/brief.md"
  # No backend file — status should still display the run

  OUTPUT="$(cmd_status "$(pwd)" 2>&1 || true)"

  RESULT=0
  assert_output_contains "$OUTPUT" "run-no-backend" || RESULT=1
  assert_output_contains "$OUTPUT" "planned" || RESULT=1

  cleanup_test_project
  return $RESULT
}

test_status_missing_brief_file() {
  setup_test_project

  mkdir -p ".factory/runs/run-no-brief"
  printf 'executing' > ".factory/runs/run-no-brief/status"
  printf 'local' > ".factory/runs/run-no-brief/backend"
  # No brief.md — status should still display the run

  OUTPUT="$(cmd_status "$(pwd)" 2>&1 || true)"

  RESULT=0
  assert_output_contains "$OUTPUT" "run-no-brief" || RESULT=1
  assert_output_contains "$OUTPUT" "executing" || RESULT=1

  cleanup_test_project
  return $RESULT
}

test_status_all_known_statuses() {
  setup_test_project

  # Create runs with every documented status value
  for STATUS in briefed behaviors-defined approach-designed planned executing rate-limited needs-user complete failed; do
    mkdir -p ".factory/runs/run-${STATUS}"
    printf '%s' "$STATUS" > ".factory/runs/run-${STATUS}/status"
    printf 'Brief for %s' "$STATUS" > ".factory/runs/run-${STATUS}/brief.md"
    printf 'local' > ".factory/runs/run-${STATUS}/backend"
  done

  OUTPUT="$(cmd_status "$(pwd)" 2>&1 || true)"

  RESULT=0
  for STATUS in briefed behaviors-defined approach-designed planned executing rate-limited needs-user complete failed; do
    assert_output_contains "$OUTPUT" "$STATUS" || RESULT=1
  done

  cleanup_test_project
  return $RESULT
}

# -------------------------------------------------------------------------
# Run all tests
# -------------------------------------------------------------------------

printf 'test-status-edges\n\n'

run_test "status with missing backend file" test_status_missing_backend_file
run_test "status with missing brief file" test_status_missing_brief_file
run_test "status displays all known status values" test_status_all_known_statuses

printf '\n  %d passed, %d failed\n' "$PASS" "$FAIL"

if [ "$FAIL" -gt 0 ]; then
  printf '\n  Failures:%b\n' "$ERRORS"
  exit 1
fi
