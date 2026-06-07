#!/usr/bin/env bash
# test-resume-edges — Verify resume edge cases.
#
# Tests resume behaviors not covered by test-resume-resolve:
#   - Resume with no runs at all
#   - Resume with only planned runs (should not resume)
#   - Resume prefers needs-user over failed when both exist
#
# Usage:
#   tests/behaviors/operations/test-resume-edges.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$(dirname "$(dirname "$SCRIPT_DIR")")")"
FACTORY="${PROJECT_DIR}/target/debug/factory"

if [ ! -x "$FACTORY" ]; then
  (cd "$PROJECT_DIR" && cargo build --quiet)
fi

PASS=0
FAIL=0
ERRORS=""

# -------------------------------------------------------------------------
# Helpers
# -------------------------------------------------------------------------

setup_test_project() {
  TEST_DIR="$(mktemp -d -t factory-test-resume-edge-XXXXXX)"
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

test_resume_with_no_runs() {
  setup_test_project
  mkdir -p ".factory/runs"

  # No runs at all — resume should not crash
  OUTPUT="$("$FACTORY" resume 2>&1 | head -5 || true)"

  RESULT=0
  # Should not say "Resuming run"
  if echo "$OUTPUT" | grep -q "Resuming run"; then
    printf '    FAIL: resume should not find any run to resume\n'
    RESULT=1
  fi

  cleanup_test_project
  return $RESULT
}

test_resume_skips_planned() {
  setup_test_project

  mkdir -p ".factory/runs/run-planned"
  printf 'planned' > ".factory/runs/run-planned/status"
  printf 'Planned run' > ".factory/runs/run-planned/brief.md"

  # Only a planned run — resume should not target it
  OUTPUT="$("$FACTORY" resume 2>&1 | head -5 || true)"

  RESULT=0
  if echo "$OUTPUT" | grep -q "Resuming run run-planned"; then
    printf '    FAIL: resume should not target a planned run\n'
    RESULT=1
  fi

  cleanup_test_project
  return $RESULT
}

test_resume_prefers_needs_user_over_failed() {
  setup_test_project

  mkdir -p ".factory/runs/run-failed"
  printf 'failed' > ".factory/runs/run-failed/status"
  printf 'Failed run' > ".factory/runs/run-failed/brief.md"

  mkdir -p ".factory/runs/run-needs-user"
  printf 'needs-user' > ".factory/runs/run-needs-user/status"
  printf 'Paused run' > ".factory/runs/run-needs-user/brief.md"

  # Resume should prefer needs-user over failed
  OUTPUT="$("$FACTORY" resume 2>&1 | head -5 || true)"

  RESULT=0
  if ! echo "$OUTPUT" | grep -q "Resuming run run-needs-user"; then
    printf '    FAIL: resume should select run-needs-user, got: %s\n' "$OUTPUT"
    RESULT=1
  fi

  cleanup_test_project
  return $RESULT
}

# -------------------------------------------------------------------------
# Run all tests
# -------------------------------------------------------------------------

printf 'test-resume-edges\n\n'

run_test "resume with no runs" test_resume_with_no_runs
run_test "resume skips planned runs" test_resume_skips_planned
run_test "resume prefers needs-user over failed" test_resume_prefers_needs_user_over_failed

printf '\n  %d passed, %d failed\n' "$PASS" "$FAIL"

if [ "$FAIL" -gt 0 ]; then
  printf '\n  Failures:%b\n' "$ERRORS"
  exit 1
fi
