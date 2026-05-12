#!/usr/bin/env bash
# test-review-phase — Verify review phase and review run behaviors.
#
# Tests that reviews run correctly when the author completes, that
# passing/failing verdicts produce the right outcomes, and that
# review-mode runs execute reviewers before the author.
#
# Sources the factory script in library mode. Mocks claude to write
# review artifacts with controlled verdicts. Mocks launch_agent for
# session loop integration tests.
#
# Covers:
#   - All reviewers pass: run_reviews returns 0
#   - Any reviewer fails: run_reviews returns non-zero
#   - Uncertain verdict treated as failure
#   - Complete status triggers reviews before accepting
#   - All reviews pass: run completes
#   - Review failure restarts author with executing status
#   - Review run with all pass: complete without launching author
#   - Review run with findings: launch author with findings prompt
#
# Usage:
#   tests/behaviors/operations/test-review-phase.sh

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
  TEST_DIR="$(mktemp -d -t factory-test-review-XXXXXX)"
  RUN_ID="test-review"
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

prompt_for() { cat "${TEST_DIR}/prompt-${1}" 2>/dev/null || true; }

assert_eq() {
  if [ "$1" != "$2" ]; then
    printf '    FAIL: got "%s", expected "%s"\n' "$1" "$2"
    return 1
  fi
}

assert_contains() {
  if ! printf '%s' "$1" | grep -q "$2"; then
    printf '    FAIL: output does not contain "%s"\n' "$2"
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
# Tests: run_reviews direct
# -------------------------------------------------------------------------

test_reviews_all_pass() {
  setup_test_run

  claude() {
    mkdir -p "${REVIEWER_RUN_DIR}/reviews"
    printf 'Verdict: pass\n\nLooks good.' > "${REVIEWER_RUN_DIR}/reviews/review-${REVIEWER_NAME}.md"
  }

  # run_reviews clobbers RESULT (global), so avoid using it as the
  # test outcome variable. Check the return code directly.
  if ! run_reviews "$RUN_DIR" "$RUN_ID" "" "run-scoped" > /dev/null 2>&1; then
    printf '    FAIL: run_reviews returned non-zero when all pass\n'
    cleanup_test_run
    return 1
  fi

  cleanup_test_run
}

test_reviews_fail_returns_nonzero() {
  setup_test_run

  claude() {
    mkdir -p "${REVIEWER_RUN_DIR}/reviews"
    if [ "$REVIEWER_NAME" = "tests" ]; then
      printf 'Verdict: fail\n\n1. Missing coverage.' > "${REVIEWER_RUN_DIR}/reviews/review-${REVIEWER_NAME}.md"
    else
      printf 'Verdict: pass\n\nLooks good.' > "${REVIEWER_RUN_DIR}/reviews/review-${REVIEWER_NAME}.md"
    fi
  }

  if run_reviews "$RUN_DIR" "$RUN_ID" "" "run-scoped" > /dev/null 2>&1; then
    printf '    FAIL: run_reviews returned 0 when one reviewer failed\n'
    cleanup_test_run
    return 1
  fi

  cleanup_test_run
}

test_reviews_uncertain_returns_nonzero() {
  setup_test_run

  claude() {
    mkdir -p "${REVIEWER_RUN_DIR}/reviews"
    if [ "$REVIEWER_NAME" = "behaviors" ]; then
      printf 'Verdict: uncertain\n\nNeed more info.' > "${REVIEWER_RUN_DIR}/reviews/review-${REVIEWER_NAME}.md"
    else
      printf 'Verdict: pass\n\nLooks good.' > "${REVIEWER_RUN_DIR}/reviews/review-${REVIEWER_NAME}.md"
    fi
  }

  if run_reviews "$RUN_DIR" "$RUN_ID" "" "run-scoped" > /dev/null 2>&1; then
    printf '    FAIL: run_reviews returned 0 when one reviewer was uncertain\n'
    cleanup_test_run
    return 1
  fi

  cleanup_test_run
}

# -------------------------------------------------------------------------
# Tests: session loop + review integration
# -------------------------------------------------------------------------

test_complete_with_passing_reviews_stops() {
  setup_test_run

  claude() {
    mkdir -p "${REVIEWER_RUN_DIR}/reviews"
    printf 'Verdict: pass\n\nLooks good.' > "${REVIEWER_RUN_DIR}/reviews/review-${REVIEWER_NAME}.md"
  }

  launch_agent() {
    record_call "$1"
    printf 'complete' > "${RUN_DIR}/status"
  }

  run_session_loop > /dev/null 2>&1

  RESULT=0
  assert_eq "$(call_count)" "1" || RESULT=1
  assert_eq "$(cat "${RUN_DIR}/status")" "complete" || RESULT=1

  cleanup_test_run
  return $RESULT
}

test_review_failure_restarts_author() {
  setup_test_run

  claude() {
    mkdir -p "${REVIEWER_RUN_DIR}/reviews"
    if [ "$REVIEWER_NAME" = "tests" ]; then
      printf 'Verdict: fail\n\n1. Missing coverage.' > "${REVIEWER_RUN_DIR}/reviews/review-${REVIEWER_NAME}.md"
    else
      printf 'Verdict: pass\n\nLooks good.' > "${REVIEWER_RUN_DIR}/reviews/review-${REVIEWER_NAME}.md"
    fi
  }

  launch_agent() {
    record_call "$1"
    N="$(call_count)"
    if [ "$N" -eq 1 ]; then
      printf 'complete' > "${RUN_DIR}/status"
    else
      printf 'needs-user' > "${RUN_DIR}/status"
    fi
  }

  run_session_loop > /dev/null 2>&1

  RESULT=0
  assert_eq "$(call_count)" "2" || RESULT=1
  assert_eq "$(cat "${RUN_DIR}/status")" "needs-user" || RESULT=1

  cleanup_test_run
  return $RESULT
}

test_review_run_all_pass_completes_without_author() {
  setup_test_run
  printf 'review' > "${RUN_DIR}/mode"

  claude() {
    mkdir -p "${REVIEWER_RUN_DIR}/reviews"
    printf 'Verdict: pass\n\nLooks good.' > "${REVIEWER_RUN_DIR}/reviews/review-${REVIEWER_NAME}.md"
  }

  launch_agent() {
    record_call "$1"
    printf 'needs-user' > "${RUN_DIR}/status"
  }

  run_session_loop > /dev/null 2>&1

  RESULT=0
  assert_eq "$(call_count)" "0" || RESULT=1
  assert_eq "$(cat "${RUN_DIR}/status")" "complete" || RESULT=1

  cleanup_test_run
  return $RESULT
}

test_review_run_findings_launch_author() {
  setup_test_run
  printf 'review' > "${RUN_DIR}/mode"

  claude() {
    mkdir -p "${REVIEWER_RUN_DIR}/reviews"
    if [ "$REVIEWER_NAME" = "tests" ]; then
      printf 'Verdict: fail\n\n1. Issue found.' > "${REVIEWER_RUN_DIR}/reviews/review-${REVIEWER_NAME}.md"
    else
      printf 'Verdict: pass\n\nLooks good.' > "${REVIEWER_RUN_DIR}/reviews/review-${REVIEWER_NAME}.md"
    fi
  }

  launch_agent() {
    record_call "$1"
    printf 'needs-user' > "${RUN_DIR}/status"
  }

  run_session_loop > /dev/null 2>&1

  RESULT=0
  N="$(call_count)"
  if [ "$N" -lt 1 ]; then
    printf '    FAIL: expected launch_agent to be called, got 0 calls\n'
    RESULT=1
  fi
  assert_contains "$(prompt_for 1)" "review" || RESULT=1

  cleanup_test_run
  return $RESULT
}

# -------------------------------------------------------------------------
# Run all tests
# -------------------------------------------------------------------------

printf 'test-review-phase\n\n'

run_test "all reviewers pass returns zero" test_reviews_all_pass
run_test "reviewer fail returns non-zero" test_reviews_fail_returns_nonzero
run_test "reviewer uncertain returns non-zero" test_reviews_uncertain_returns_nonzero
run_test "complete with passing reviews stops loop" test_complete_with_passing_reviews_stops
run_test "review failure restarts author" test_review_failure_restarts_author
run_test "review run all pass completes without author" test_review_run_all_pass_completes_without_author
run_test "review run findings launch author" test_review_run_findings_launch_author

printf '\n  %d passed, %d failed\n' "$PASS" "$FAIL"

if [ "$FAIL" -gt 0 ]; then
  printf '\n  Failures:%b\n' "$ERRORS"
  exit 1
fi
