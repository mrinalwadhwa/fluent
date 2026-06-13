#!/usr/bin/env bash
# test-log-harness — Self-test for the shared run_test logging harness.
#
# Verifies:
#   - Passing case creates a log file with expected structure
#   - Failing case creates a log file and appends to .failed sentinel
#   - FACTORY_TESTS_SKIP_LOG=1 bypasses log writing entirely

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$(dirname "$(dirname "$SCRIPT_DIR")")")"

TEST_LOG_DIR="$(mktemp -d -t factory-test-log-harness-XXXXXX)"

source "${PROJECT_DIR}/tests/lib/run_test.sh"
LOG_DIR="${TEST_LOG_DIR}/test-log-harness"

# -------------------------------------------------------------------------
# Test functions exercised by run_test
# -------------------------------------------------------------------------

test_passing_case() {
  printf 'hello from passing case\n'
  return 0
}

test_failing_case() {
  printf 'hello from failing case\n'
  return 1
}

# -------------------------------------------------------------------------
# Assertions
# -------------------------------------------------------------------------

SELF_PASS=0
SELF_FAIL=0
SELF_ERRORS=""

assert_file_exists() {
  if [ ! -f "$1" ]; then
    printf '    FAIL: expected file to exist: %s\n' "$1"
    SELF_FAIL=$((SELF_FAIL + 1))
    SELF_ERRORS="${SELF_ERRORS}\n  - $2"
    return 1
  fi
}

assert_file_not_exists() {
  if [ -f "$1" ]; then
    printf '    FAIL: expected file NOT to exist: %s\n' "$1"
    SELF_FAIL=$((SELF_FAIL + 1))
    SELF_ERRORS="${SELF_ERRORS}\n  - $2"
    return 1
  fi
}

assert_file_contains() {
  if ! grep -q "$2" "$1" 2>/dev/null; then
    printf '    FAIL: %s does not contain "%s"\n' "$1" "$2"
    SELF_FAIL=$((SELF_FAIL + 1))
    SELF_ERRORS="${SELF_ERRORS}\n  - $3"
    return 1
  fi
}

# -------------------------------------------------------------------------
# Test 1: passing case creates log file
# -------------------------------------------------------------------------

printf 'test-log-harness\n\n'
printf '  passing case creates log file ... '

run_test "passing case" test_passing_case > /dev/null 2>&1

RESULT=0
log_path="${LOG_DIR}/passing_case.log"
if ! assert_file_exists "$log_path" "passing case log exists"; then
  RESULT=1
elif ! assert_file_contains "$log_path" "=== passing case ===" "passing case log header"; then
  RESULT=1
elif ! assert_file_contains "$log_path" "hello from passing case" "passing case log body"; then
  RESULT=1
fi

if [ "$RESULT" -eq 0 ]; then
  printf 'PASS\n'
  SELF_PASS=$((SELF_PASS + 1))
fi

# -------------------------------------------------------------------------
# Test 2: failing case creates log file and appends to .failed
# -------------------------------------------------------------------------

printf '  failing case creates log and appends to .failed sentinel ... '

run_test "failing case" test_failing_case > /dev/null 2>&1

RESULT=0
log_path="${LOG_DIR}/failing_case.log"
if ! assert_file_exists "$log_path" "failing case log exists"; then
  RESULT=1
elif ! assert_file_contains "$log_path" "hello from failing case" "failing case log body"; then
  RESULT=1
fi

failed_path="${LOG_DIR}/.failed"
if ! assert_file_exists "$failed_path" ".failed sentinel exists"; then
  RESULT=1
elif ! grep -q "failing_case.log" "$failed_path" 2>/dev/null; then
  printf '    FAIL: .failed sentinel does not reference failing_case.log\n'
  SELF_FAIL=$((SELF_FAIL + 1))
  RESULT=1
fi

if [ "$RESULT" -eq 0 ]; then
  printf 'PASS\n'
  SELF_PASS=$((SELF_PASS + 1))
fi

# -------------------------------------------------------------------------
# Test 3: FACTORY_TESTS_SKIP_LOG=1 bypasses log writing
# -------------------------------------------------------------------------

printf '  FACTORY_TESTS_SKIP_LOG=1 bypasses log writing ... '

SKIP_DIR="$(mktemp -d -t factory-test-skip-XXXXXX)"

(
  export FACTORY_TESTS_SKIP_LOG=1
  source "${PROJECT_DIR}/tests/lib/run_test.sh"
  LOG_DIR="${SKIP_DIR}/test-skip"

  run_test "skipped case" test_passing_case > /dev/null 2>&1
)

RESULT=0
if [ -d "${SKIP_DIR}/test-skip" ] && [ -n "$(ls -A "${SKIP_DIR}/test-skip" 2>/dev/null)" ]; then
  printf '    FAIL: log directory should be empty when skip is set\n'
  SELF_FAIL=$((SELF_FAIL + 1))
  RESULT=1
fi

if [ "$RESULT" -eq 0 ]; then
  printf 'PASS\n'
  SELF_PASS=$((SELF_PASS + 1))
fi

# -------------------------------------------------------------------------
# Cleanup and summary
# -------------------------------------------------------------------------

rm -rf "$TEST_LOG_DIR" "$SKIP_DIR"

printf '\n  %d passed, %d failed\n' "$SELF_PASS" "$SELF_FAIL"

if [ "$SELF_FAIL" -gt 0 ]; then
  printf '\n  Failures:%b\n' "$SELF_ERRORS"
  exit 1
fi
