#!/usr/bin/env bash
# test-reviewing-status — Verify the "reviewing" status lifecycle.
#
# Tests that the status file reflects "reviewing" while reviewers run,
# transitions to "complete" when all reviewers pass, and transitions
# back to "executing" before the author is restarted on failure.
#
# Sources the factory script in library mode. Mocks claude to write
# review artifacts with controlled verdicts. Mocks launch_author for
# session loop integration tests.
#
# Covers:
#   - Status is "reviewing" while reviewers are running
#   - Status transitions to "complete" when all reviewers pass
#   - Status transitions to "executing" before author restarts on failure
#
# Usage:
#   tests/behaviors/operations/test-reviewing-status.sh

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
  TEST_DIR="$(mktemp -d -t factory-test-reviewing-XXXXXX)"
  RUN_ID="test-reviewing"
  # Path must contain .factory/runs/ for run_single_reviewer cd pattern
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

record_call() {
  N=$(( $(cat "${TEST_DIR}/call-count") + 1 ))
  printf '%s' "$N" > "${TEST_DIR}/call-count"
  printf '%s' "$1" > "${TEST_DIR}/prompt-${N}"
}

call_count() { cat "${TEST_DIR}/call-count"; }

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
# Tests
# -------------------------------------------------------------------------

# WHILE reviewers are running, THE SYSTEM SHALL set the run status to "reviewing."
test_status_is_reviewing_while_reviewers_run() {
  setup_test_run

  claude() {
    # Capture status at the moment this reviewer executes (via file, since
    # reviewers run in background subprocesses and cannot write to parent vars)
    cat "${RUN_DIR}/status" 2>/dev/null > "${TEST_DIR}/status-during-review"
    mkdir -p "${REVIEWER_RUN_DIR}/reviews"
    printf 'Verdict: pass\n\nLooks good.' > "${REVIEWER_RUN_DIR}/reviews/review-${REVIEWER_NAME}.md"
  }

  launch_author() {
    printf 'complete' > "${RUN_DIR}/status"
  }

  run_session_loop > /dev/null 2>&1

  RESULT=0
  STATUS_DURING_REVIEW="$(cat "${TEST_DIR}/status-during-review" 2>/dev/null || printf 'unknown')"
  assert_eq "$STATUS_DURING_REVIEW" "reviewing" || RESULT=1

  cleanup_test_run
  return $RESULT
}

# WHEN all reviewers pass, THE SYSTEM SHALL set the run status to "complete."
test_status_complete_when_all_reviewers_pass() {
  setup_test_run

  claude() {
    mkdir -p "${REVIEWER_RUN_DIR}/reviews"
    printf 'Verdict: pass\n\nLooks good.' > "${REVIEWER_RUN_DIR}/reviews/review-${REVIEWER_NAME}.md"
  }

  launch_author() {
    printf 'complete' > "${RUN_DIR}/status"
  }

  run_session_loop > /dev/null 2>&1

  RESULT=0
  assert_eq "$(cat "${RUN_DIR}/status")" "complete" || RESULT=1

  cleanup_test_run
  return $RESULT
}

# WHEN any reviewer fails, THE SYSTEM SHALL set the run status back to
# "executing" before restarting the author.
test_status_executing_before_author_restart_on_failure() {
  setup_test_run

  STATUS_AT_SECOND_AUTHOR_CALL=""

  claude() {
    mkdir -p "${REVIEWER_RUN_DIR}/reviews"
    if [ "$REVIEWER_NAME" = "tests" ]; then
      printf 'Verdict: fail\n\n1. Missing coverage.' > "${REVIEWER_RUN_DIR}/reviews/review-${REVIEWER_NAME}.md"
    else
      printf 'Verdict: pass\n\nLooks good.' > "${REVIEWER_RUN_DIR}/reviews/review-${REVIEWER_NAME}.md"
    fi
  }

  launch_author() {
    N=$(( $(cat "${TEST_DIR}/call-count") + 1 ))
    printf '%s' "$N" > "${TEST_DIR}/call-count"
    if [ "$N" -eq 2 ]; then
      # Record the status at the moment the author is restarted
      STATUS_AT_SECOND_AUTHOR_CALL="$(cat "${RUN_DIR}/status" 2>/dev/null || printf 'unknown')"
      printf 'needs-user' > "${RUN_DIR}/status"
    else
      printf 'complete' > "${RUN_DIR}/status"
    fi
  }

  run_session_loop > /dev/null 2>&1

  RESULT=0
  assert_eq "$STATUS_AT_SECOND_AUTHOR_CALL" "executing" || RESULT=1

  cleanup_test_run
  return $RESULT
}

# -------------------------------------------------------------------------
# Run all tests
# -------------------------------------------------------------------------

printf 'test-reviewing-status\n\n'

run_test "status is reviewing while reviewers run" test_status_is_reviewing_while_reviewers_run
run_test "status transitions to complete when all pass" test_status_complete_when_all_reviewers_pass
run_test "status is executing before author restarts on failure" test_status_executing_before_author_restart_on_failure

printf '\n  %d passed, %d failed\n' "$PASS" "$FAIL"

if [ "$FAIL" -gt 0 ]; then
  printf '\n  Failures:%b\n' "$ERRORS"
  exit 1
fi
