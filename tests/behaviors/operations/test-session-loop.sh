#!/usr/bin/env bash
# test-session-loop — Verify session loop control flow.
#
# Tests the session loop's status-based branching, consecutive failure
# tracking, and session limit enforcement.
#
# Sources the factory script in library mode to call run_session_loop
# directly. Mocks launch_author to write controlled status files.
# Overrides sleep, capture_snapshot, and generate_report as no-ops.
#
# Covers:
#   - Loop uses brief as initial prompt
#   - Loop uses handoff when present
#   - Executing status restarts the agent
#   - needs-user status stops the loop
#   - failed status stops the loop
#   - rate-limited status restarts after waiting
#   - 3 consecutive non-zero exits set failed
#   - Successful exit resets failure counter
#   - Session count exceeding max sets failed
#
# Usage:
#   tests/behaviors/operations/test-session-loop.sh

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
  TEST_DIR="$(mktemp -d -t factory-test-loop-XXXXXX)"
  RUN_ID="test-loop"
  RUN_DIR="${TEST_DIR}/project/.factory/runs/${RUN_ID}"
  mkdir -p "$RUN_DIR"
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
# Tests
# -------------------------------------------------------------------------

test_loop_initial_prompt_uses_brief() {
  setup_test_run

  launch_author() {
    record_call "$1"
    printf 'needs-user' > "${RUN_DIR}/status"
  }

  run_session_loop > /dev/null 2>&1

  RESULT=0
  assert_contains "$(prompt_for 1)" "brief" || RESULT=1
  assert_contains "$(prompt_for 1)" "$RUN_ID" || RESULT=1

  cleanup_test_run
  return $RESULT
}

test_loop_initial_prompt_uses_handoff() {
  setup_test_run
  printf 'Previous work handoff' > "${RUN_DIR}/handoff.md"

  launch_author() {
    record_call "$1"
    printf 'needs-user' > "${RUN_DIR}/status"
  }

  run_session_loop > /dev/null 2>&1

  RESULT=0
  assert_contains "$(prompt_for 1)" "handoff" || RESULT=1

  cleanup_test_run
  return $RESULT
}

test_loop_stops_on_needs_user() {
  setup_test_run

  launch_author() {
    record_call "$1"
    printf 'needs-user' > "${RUN_DIR}/status"
  }

  run_session_loop > /dev/null 2>&1

  RESULT=0
  assert_eq "$(call_count)" "1" || RESULT=1
  assert_eq "$(cat "${RUN_DIR}/status")" "needs-user" || RESULT=1

  cleanup_test_run
  return $RESULT
}

test_loop_stops_on_failed() {
  setup_test_run

  launch_author() {
    record_call "$1"
    printf 'failed' > "${RUN_DIR}/status"
  }

  run_session_loop > /dev/null 2>&1

  RESULT=0
  assert_eq "$(call_count)" "1" || RESULT=1
  assert_eq "$(cat "${RUN_DIR}/status")" "failed" || RESULT=1

  cleanup_test_run
  return $RESULT
}

test_loop_restarts_on_executing() {
  setup_test_run

  launch_author() {
    record_call "$1"
    N="$(call_count)"
    if [ "$N" -lt 3 ]; then
      printf 'executing' > "${RUN_DIR}/status"
    else
      printf 'needs-user' > "${RUN_DIR}/status"
    fi
  }

  run_session_loop > /dev/null 2>&1

  RESULT=0
  assert_eq "$(call_count)" "3" || RESULT=1
  assert_eq "$(cat "${RUN_DIR}/status")" "needs-user" || RESULT=1

  cleanup_test_run
  return $RESULT
}

test_loop_restarts_on_rate_limited() {
  setup_test_run

  launch_author() {
    record_call "$1"
    N="$(call_count)"
    if [ "$N" -eq 1 ]; then
      printf 'rate-limited' > "${RUN_DIR}/status"
    else
      printf 'needs-user' > "${RUN_DIR}/status"
    fi
  }

  run_session_loop > /dev/null 2>&1

  RESULT=0
  assert_eq "$(call_count)" "2" || RESULT=1

  cleanup_test_run
  return $RESULT
}

test_loop_consecutive_failures_set_failed() {
  setup_test_run
  printf 'executing' > "${RUN_DIR}/status"

  launch_author() {
    record_call "$1"
    return 1
  }

  run_session_loop > /dev/null 2>&1

  RESULT=0
  assert_eq "$(call_count)" "3" || RESULT=1
  assert_eq "$(cat "${RUN_DIR}/status")" "failed" || RESULT=1

  cleanup_test_run
  return $RESULT
}

test_loop_success_resets_failure_counter() {
  setup_test_run
  printf 'executing' > "${RUN_DIR}/status"

  launch_author() {
    record_call "$1"
    N="$(call_count)"
    case "$N" in
      1|2) return 1 ;;     # Two failures
      3) return 0 ;;        # Success — resets counter
      4|5) return 1 ;;     # Two more failures (not three)
      *) printf 'needs-user' > "${RUN_DIR}/status"; return 0 ;;
    esac
  }

  run_session_loop > /dev/null 2>&1

  RESULT=0
  N="$(call_count)"
  if [ "$N" -le 5 ]; then
    printf '    FAIL: expected >5 calls, got %s (counter not reset)\n' "$N"
    RESULT=1
  fi

  cleanup_test_run
  return $RESULT
}

test_loop_max_sessions_sets_failed() {
  setup_test_run

  launch_author() {
    record_call "$1"
    printf 'executing' > "${RUN_DIR}/status"
  }

  run_session_loop > /dev/null 2>&1

  RESULT=0
  assert_eq "$(call_count)" "50" || RESULT=1
  assert_eq "$(cat "${RUN_DIR}/status")" "failed" || RESULT=1

  cleanup_test_run
  return $RESULT
}

# -------------------------------------------------------------------------
# Run all tests
# -------------------------------------------------------------------------

printf 'test-session-loop\n\n'

run_test "loop initial prompt uses brief" test_loop_initial_prompt_uses_brief
run_test "loop initial prompt uses handoff" test_loop_initial_prompt_uses_handoff
run_test "loop stops on needs-user" test_loop_stops_on_needs_user
run_test "loop stops on failed" test_loop_stops_on_failed
run_test "loop restarts on executing" test_loop_restarts_on_executing
run_test "loop restarts on rate-limited" test_loop_restarts_on_rate_limited
run_test "consecutive failures set failed" test_loop_consecutive_failures_set_failed
run_test "success resets failure counter" test_loop_success_resets_failure_counter
run_test "max sessions sets failed" test_loop_max_sessions_sets_failed

printf '\n  %d passed, %d failed\n' "$PASS" "$FAIL"

if [ "$FAIL" -gt 0 ]; then
  printf '\n  Failures:%b\n' "$ERRORS"
  exit 1
fi
