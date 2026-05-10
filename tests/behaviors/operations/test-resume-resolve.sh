#!/usr/bin/env bash
# test-resume-resolve — Verify resume run-id resolution behaviors.
#
# Tests that `factory resume` finds runs with status `needs-user` or
# `failed` and ignores runs with other statuses.
#
# Covers:
#   - Resume finds a needs-user run
#   - Resume finds a failed run
#   - Resume skips complete and executing runs
#   - Resume finds either resumable status when both exist
#
# Usage:
#   tests/behaviors/operations/test-resume-resolve.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$(dirname "$(dirname "$SCRIPT_DIR")")")"
FACTORY="${PROJECT_DIR}/scripts/factory"

PASS=0
FAIL=0
ERRORS=""

# -------------------------------------------------------------------------
# Helpers
# -------------------------------------------------------------------------

setup_test_project() {
  TEST_DIR="$(mktemp -d -t factory-test-resume-XXXXXX)"
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

test_resume_finds_needs_user() {
  setup_test_project

  mkdir -p ".factory/runs/run-paused"
  printf 'needs-user' > ".factory/runs/run-paused/status"
  printf 'Paused run' > ".factory/runs/run-paused/brief.md"

  # Run factory resume and capture just the first line of output
  OUTPUT="$("$FACTORY" resume 2>&1 | head -1 || true)"

  RESULT=0
  if ! echo "$OUTPUT" | grep -q "run-paused"; then
    printf '    FAIL: resume did not find run-paused, got: %s\n' "$OUTPUT"
    RESULT=1
  fi

  cleanup_test_project
  return $RESULT
}

test_resume_finds_failed() {
  setup_test_project

  mkdir -p ".factory/runs/run-broken"
  printf 'failed' > ".factory/runs/run-broken/status"
  printf 'Broken run' > ".factory/runs/run-broken/brief.md"

  OUTPUT="$("$FACTORY" resume 2>&1 | head -1 || true)"

  RESULT=0
  if ! echo "$OUTPUT" | grep -q "run-broken"; then
    printf '    FAIL: resume did not find run-broken, got: %s\n' "$OUTPUT"
    RESULT=1
  fi

  cleanup_test_project
  return $RESULT
}

test_resume_skips_complete_and_executing() {
  setup_test_project

  mkdir -p ".factory/runs/run-done" ".factory/runs/run-active"
  printf 'complete' > ".factory/runs/run-done/status"
  printf 'Done' > ".factory/runs/run-done/brief.md"
  printf 'executing' > ".factory/runs/run-active/status"
  printf 'Active' > ".factory/runs/run-active/brief.md"

  # With only complete and executing runs, resume should not find a target
  OUTPUT="$("$FACTORY" resume 2>&1 | head -3 || true)"

  RESULT=0
  # Should not say "Resuming run run-done" or "Resuming run run-active"
  if echo "$OUTPUT" | grep -q "Resuming run run-done"; then
    printf '    FAIL: resume should not target complete run\n'
    RESULT=1
  fi
  if echo "$OUTPUT" | grep -q "Resuming run run-active"; then
    printf '    FAIL: resume should not target executing run\n'
    RESULT=1
  fi

  cleanup_test_project
  return $RESULT
}

test_resume_finds_either_resumable_status() {
  setup_test_project

  mkdir -p ".factory/runs/run-failed" ".factory/runs/run-paused"
  printf 'failed' > ".factory/runs/run-failed/status"
  printf 'Failed' > ".factory/runs/run-failed/brief.md"
  printf 'needs-user' > ".factory/runs/run-paused/status"
  printf 'Paused' > ".factory/runs/run-paused/brief.md"

  OUTPUT="$("$FACTORY" resume 2>&1 | head -1 || true)"

  RESULT=0
  if ! echo "$OUTPUT" | grep -qE "run-failed|run-paused"; then
    printf '    FAIL: resume should find a resumable run, got: %s\n' "$OUTPUT"
    RESULT=1
  fi

  cleanup_test_project
  return $RESULT
}

# -------------------------------------------------------------------------
# Run all tests
# -------------------------------------------------------------------------

printf 'test-resume-resolve\n\n'

run_test "resume finds needs-user run" test_resume_finds_needs_user
run_test "resume finds failed run" test_resume_finds_failed
run_test "resume skips complete and executing runs" test_resume_skips_complete_and_executing
run_test "resume finds either resumable status" test_resume_finds_either_resumable_status

printf '\n  %d passed, %d failed\n' "$PASS" "$FAIL"

if [ "$FAIL" -gt 0 ]; then
  printf '\n  Failures:%b\n' "$ERRORS"
  exit 1
fi
