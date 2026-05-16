#!/usr/bin/env bash
# test-review-round-limit — Verify review round limit behavior.
#
# Tests:
#   - review round limit sets failed after 10 review-fix cycles
#
# Uses factory bash library mode to drive the session loop with mocked
# claude (always-fail reviews) and mocked launch_author (always completes).
#
# Usage:
#   tests/behaviors/operations/test-review-round-limit.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$(dirname "$(dirname "$SCRIPT_DIR")")")"
FACTORY="${PROJECT_DIR}/scripts/factory"

PASS=0
FAIL=0
ERRORS=""

# Source factory functions (library mode — no dispatch)
PROMPTS_DIR="${PROJECT_DIR}/prompts"
FACTORY_LIB=1 . "$FACTORY"

# Override functions that interact with external systems
sleep() { :; }
capture_snapshot() { :; }
generate_report() { :; }

# -------------------------------------------------------------------------
# Helpers
# -------------------------------------------------------------------------

setup_test_run() {
  TEST_DIR="$(mktemp -d -t factory-test-round-XXXXXX)"
  RUN_ID="test-round-limit"
  RUN_DIR="${TEST_DIR}/project/.factory/runs/${RUN_ID}"
  mkdir -p "${RUN_DIR}/reviews"
  printf 'Test brief' > "${RUN_DIR}/brief.md"
  printf 'planned' > "${RUN_DIR}/status"
  printf '0' > "${TEST_DIR}/call-count"
  PRE_SESSION_HOOK=""
}

cleanup_test_run() {
  rm -rf "$TEST_DIR"
}

assert_eq() {
  if [ "$1" != "$2" ]; then
    printf '    FAIL: got "%s", expected "%s"\n' "$1" "$2"
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
# Test
# -------------------------------------------------------------------------

test_review_round_limit_completes() {
  setup_test_run

  # Reviewers always fail
  claude() {
    mkdir -p "${REVIEWER_RUN_DIR}/reviews"
    printf 'Verdict: fail\n\n1. Always failing.' > "${REVIEWER_RUN_DIR}/reviews/review-${REVIEWER_NAME}.md"
  }

  AUTHOR_CALLS=0
  launch_author() {
    AUTHOR_CALLS=$((AUTHOR_CALLS + 1))
    printf 'complete' > "${RUN_DIR}/status"
  }

  run_session_loop > /dev/null 2>&1

  FINAL_STATUS="$(cat "${RUN_DIR}/status")"

  RESULT=0
  # After 10 review-fix cycles, run should complete (accept current state)
  if [ "$FINAL_STATUS" != "complete" ]; then
    printf '    FAIL: expected status "complete" after review round limit, got "%s"\n' "$FINAL_STATUS"
    RESULT=1
  fi
  # 1 initial author call + 10 review-fix cycles = 11 author calls total.
  # The limit fires on the 11th completion (REVIEW_ROUND > 10).
  if [ "$AUTHOR_CALLS" -ne 11 ]; then
    printf '    FAIL: launch_author called %d times, expected 11 (1 initial + 10 fix cycles)\n' "$AUTHOR_CALLS"
    RESULT=1
  fi

  cleanup_test_run
  return $RESULT
}

# -------------------------------------------------------------------------
# Run all tests
# -------------------------------------------------------------------------

printf 'test-review-round-limit\n\n'

run_test "review round limit completes after 10 cycles" test_review_round_limit_completes

printf '\n  %d passed, %d failed\n' "$PASS" "$FAIL"

if [ "$FAIL" -gt 0 ]; then
  printf '\n  Failures:%b\n' "$ERRORS"
  exit 1
fi
