#!/usr/bin/env bash
# test-dashboard-activity — Verify dashboard activity signaling behaviors.
#
# Tests:
#   - Dashboard does not crash when run is actively executing (active indicator scenario)
#   - Dashboard does not crash when run is complete (completion signal scenario)
#   - Dashboard does not crash when reviewers are running (transcript without review file)
#   - Dashboard does not crash when reviewer verdict arrives (transcript with review file)
#   - Dashboard does not crash when run has failed
#   - Dashboard does not crash when run needs user input
#
# Note: Visual rendering assertions (spinner presence, phase labels, "Complete" text)
# are verified by the Rust TestBackend unit tests:
#   cargo test --lib dashboard
#
# Usage:
#   tests/behaviors/operations/test-dashboard-activity.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$(dirname "$(dirname "$SCRIPT_DIR")")")"
FACTORY_BIN="${FACTORY_BIN_OVERRIDE:-${PROJECT_DIR}/target/debug/factory}"

source "${PROJECT_DIR}/tests/lib/run_test.sh"
LOG_DIR="${PROJECT_DIR}/tests/output/$(basename "$0" .sh)"

# -------------------------------------------------------------------------
# Helpers
# -------------------------------------------------------------------------

no_panic() {
  local output="$1"
  if echo "$output" | grep -qi "panic"; then
    printf '    FAIL: dashboard panicked\n'
    return 1
  fi
  return 0
}

not_crashed() {
  local exit_code="$1"
  if [ "$exit_code" -gt 128 ]; then
    printf '    FAIL: dashboard crashed with signal %d\n' $((exit_code - 128))
    return 1
  fi
  return 0
}

# -------------------------------------------------------------------------
# Tests
# -------------------------------------------------------------------------

# Behavior 1: WHILE a run is actively executing (author running), the system
# shall show a visual indicator distinguishing "active" from "idle".
# Shell test: dashboard handles executing state without crashing.
test_dashboard_no_crash_when_executing() {
  TEST_DIR="$(mktemp -d -t factory-test-dash-act-XXXXXX)"
  mkdir -p "${TEST_DIR}/.factory/runs/active-run/sessions/session-1"
  printf 'executing' > "${TEST_DIR}/.factory/runs/active-run/status"
  printf 'Test brief for active run' > "${TEST_DIR}/.factory/runs/active-run/brief.md"
  # Transcript indicates an active session
  printf '{}' > "${TEST_DIR}/.factory/runs/active-run/sessions/session-1/transcript.jsonl"

  set +e
  OUTPUT="$(cd "$TEST_DIR" && timeout 1 "$FACTORY_BIN" dashboard 2>&1)"
  EXIT_CODE=$?
  set -e

  local RESULT=0
  no_panic "$OUTPUT" || RESULT=1
  not_crashed "$EXIT_CODE" || RESULT=1

  rm -rf "$TEST_DIR"
  return $RESULT
}

# Behavior 2: WHEN everything is done (terminal status), the system shall
# make completion obvious — no ambiguity about whether something is in progress.
# Shell test: dashboard handles complete state without crashing.
test_dashboard_no_crash_when_complete() {
  TEST_DIR="$(mktemp -d -t factory-test-dash-act-XXXXXX)"
  mkdir -p "${TEST_DIR}/.factory/runs/done-run/reviews"
  printf 'complete' > "${TEST_DIR}/.factory/runs/done-run/status"
  printf 'Test brief for done run' > "${TEST_DIR}/.factory/runs/done-run/brief.md"
  # Review file indicates a completed review
  printf 'Verdict: pass\n' > "${TEST_DIR}/.factory/runs/done-run/reviews/review-behaviors.md"

  set +e
  OUTPUT="$(cd "$TEST_DIR" && timeout 1 "$FACTORY_BIN" dashboard 2>&1)"
  EXIT_CODE=$?
  set -e

  local RESULT=0
  no_panic "$OUTPUT" || RESULT=1
  not_crashed "$EXIT_CODE" || RESULT=1

  rm -rf "$TEST_DIR"
  return $RESULT
}

# Behavior 3a: WHEN a reviewer is running (transcript exists, no review file),
# the system shall show active review state without crashing.
test_dashboard_no_crash_when_reviewers_running() {
  TEST_DIR="$(mktemp -d -t factory-test-dash-act-XXXXXX)"
  mkdir -p "${TEST_DIR}/.factory/runs/review-run/reviews"
  printf 'complete' > "${TEST_DIR}/.factory/runs/review-run/status"
  printf 'Test brief for review run' > "${TEST_DIR}/.factory/runs/review-run/brief.md"
  # Transcript present but no review file = reviewer is still running
  printf '{}' > "${TEST_DIR}/.factory/runs/review-run/reviews/transcript-behaviors.jsonl"

  set +e
  OUTPUT="$(cd "$TEST_DIR" && timeout 1 "$FACTORY_BIN" dashboard 2>&1)"
  EXIT_CODE=$?
  set -e

  local RESULT=0
  no_panic "$OUTPUT" || RESULT=1
  not_crashed "$EXIT_CODE" || RESULT=1

  rm -rf "$TEST_DIR"
  return $RESULT
}

# Behavior 3b: WHEN a reviewer finishes (review file appears), the system shall
# reflect the new verdict without crashing.
test_dashboard_no_crash_when_reviewer_verdict_arrives() {
  TEST_DIR="$(mktemp -d -t factory-test-dash-act-XXXXXX)"
  mkdir -p "${TEST_DIR}/.factory/runs/verdict-run/reviews"
  printf 'complete' > "${TEST_DIR}/.factory/runs/verdict-run/status"
  printf 'Test brief for verdict run' > "${TEST_DIR}/.factory/runs/verdict-run/brief.md"
  # Both transcript and review file present = reviewer finished with verdict
  printf '{}' > "${TEST_DIR}/.factory/runs/verdict-run/reviews/transcript-behaviors.jsonl"
  printf 'Verdict: pass\n' > "${TEST_DIR}/.factory/runs/verdict-run/reviews/review-behaviors.md"

  set +e
  OUTPUT="$(cd "$TEST_DIR" && timeout 1 "$FACTORY_BIN" dashboard 2>&1)"
  EXIT_CODE=$?
  set -e

  local RESULT=0
  no_panic "$OUTPUT" || RESULT=1
  not_crashed "$EXIT_CODE" || RESULT=1

  rm -rf "$TEST_DIR"
  return $RESULT
}

# Behavior 4a: WHEN dashboard is displayed for a failed run, the system shall
# show a phase label for the failed state without crashing.
test_dashboard_no_crash_when_failed() {
  TEST_DIR="$(mktemp -d -t factory-test-dash-act-XXXXXX)"
  mkdir -p "${TEST_DIR}/.factory/runs/failed-run"
  printf 'failed' > "${TEST_DIR}/.factory/runs/failed-run/status"
  printf 'Test brief for failed run' > "${TEST_DIR}/.factory/runs/failed-run/brief.md"

  set +e
  OUTPUT="$(cd "$TEST_DIR" && timeout 1 "$FACTORY_BIN" dashboard 2>&1)"
  EXIT_CODE=$?
  set -e

  local RESULT=0
  no_panic "$OUTPUT" || RESULT=1
  not_crashed "$EXIT_CODE" || RESULT=1

  rm -rf "$TEST_DIR"
  return $RESULT
}

# Behavior 4b: WHEN dashboard is displayed for a needs-user run, the system
# shall show a phase label for the needs-input state without crashing.
test_dashboard_no_crash_when_needs_user() {
  TEST_DIR="$(mktemp -d -t factory-test-dash-act-XXXXXX)"
  mkdir -p "${TEST_DIR}/.factory/runs/blocked-run"
  printf 'needs-user' > "${TEST_DIR}/.factory/runs/blocked-run/status"
  printf 'Test brief for blocked run' > "${TEST_DIR}/.factory/runs/blocked-run/brief.md"

  set +e
  OUTPUT="$(cd "$TEST_DIR" && timeout 1 "$FACTORY_BIN" dashboard 2>&1)"
  EXIT_CODE=$?
  set -e

  local RESULT=0
  no_panic "$OUTPUT" || RESULT=1
  not_crashed "$EXIT_CODE" || RESULT=1

  rm -rf "$TEST_DIR"
  return $RESULT
}

# Behavior 4c: WHEN dashboard is displayed for multiple runs in different states,
# the system shall handle all phase labels without crashing.
test_dashboard_no_crash_with_mixed_states() {
  TEST_DIR="$(mktemp -d -t factory-test-dash-act-XXXXXX)"
  mkdir -p "${TEST_DIR}/.factory/runs/run-executing"
  mkdir -p "${TEST_DIR}/.factory/runs/run-complete"
  mkdir -p "${TEST_DIR}/.factory/runs/run-failed"

  printf 'executing' > "${TEST_DIR}/.factory/runs/run-executing/status"
  printf 'Executing run brief' > "${TEST_DIR}/.factory/runs/run-executing/brief.md"

  printf 'complete' > "${TEST_DIR}/.factory/runs/run-complete/status"
  printf 'Complete run brief' > "${TEST_DIR}/.factory/runs/run-complete/brief.md"

  printf 'failed' > "${TEST_DIR}/.factory/runs/run-failed/status"
  printf 'Failed run brief' > "${TEST_DIR}/.factory/runs/run-failed/brief.md"

  set +e
  OUTPUT="$(cd "$TEST_DIR" && timeout 1 "$FACTORY_BIN" dashboard 2>&1)"
  EXIT_CODE=$?
  set -e

  local RESULT=0
  no_panic "$OUTPUT" || RESULT=1
  not_crashed "$EXIT_CODE" || RESULT=1

  rm -rf "$TEST_DIR"
  return $RESULT
}

# -------------------------------------------------------------------------
# Run all tests
# -------------------------------------------------------------------------

printf 'test-dashboard-activity\n\n'

run_test "no crash when run is actively executing" test_dashboard_no_crash_when_executing
run_test "no crash when run is complete" test_dashboard_no_crash_when_complete
run_test "no crash when reviewers are running" test_dashboard_no_crash_when_reviewers_running
run_test "no crash when reviewer verdict arrives" test_dashboard_no_crash_when_reviewer_verdict_arrives
run_test "no crash when run has failed" test_dashboard_no_crash_when_failed
run_test "no crash when run needs user input" test_dashboard_no_crash_when_needs_user
run_test "no crash with mixed run states" test_dashboard_no_crash_with_mixed_states

summarize_and_exit
