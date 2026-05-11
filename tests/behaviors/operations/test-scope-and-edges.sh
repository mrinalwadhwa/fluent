#!/usr/bin/env bash
# test-scope-and-edges — Verify scope file handling and edge cases.
#
# Tests operational behaviors that involve the scope file (used for
# review targeting) and edge cases in run-id resolution and status.
#
# Sources the factory script in library mode to call functions directly.
#
# Covers:
#   - Worktree copies scope file when present
#   - Run-id scan finds planned and executing as active
#   - Run-id scan skips failed and needs-user
#   - Factory status with no runs
#
# Usage:
#   tests/behaviors/operations/test-scope-and-edges.sh

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
  TEST_DIR="$(mktemp -d -t factory-test-scope-XXXXXX)"
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
  if [ -d "${TEST_DIR}/main/.git" ]; then
    git -C "${TEST_DIR}/main" worktree list --porcelain 2>/dev/null | \
      grep '^worktree ' | awk '{print $2}' | \
      grep -v "${TEST_DIR}/main" | while read -r wt; do
      git -C "${TEST_DIR}/main" worktree remove --force "$wt" 2>/dev/null || true
    done
  fi
  rm -rf "$TEST_DIR"
}

assert_file_exists() {
  if [ ! -f "$1" ]; then
    printf '    FAIL: expected file %s to exist\n' "$1"
    return 1
  fi
}

assert_file_contains() {
  if [ ! -f "$1" ]; then
    printf '    FAIL: file %s does not exist\n' "$1"
    return 1
  fi
  CONTENT="$(cat "$1")"
  if [ "$CONTENT" != "$2" ]; then
    printf '    FAIL: %s contains "%s", expected "%s"\n' "$1" "$CONTENT" "$2"
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

test_worktree_copies_scope_file() {
  setup_test_project

  RUN_ID="test-scope-copy"
  mkdir -p ".factory/runs/${RUN_ID}"
  printf 'Scope brief' > ".factory/runs/${RUN_ID}/brief.md"
  printf 'planned' > ".factory/runs/${RUN_ID}/status"
  printf 'review' > ".factory/runs/${RUN_ID}/mode"
  printf 'documentation/' > ".factory/runs/${RUN_ID}/scope"

  RUN_DIR="$(pwd)/.factory/runs/${RUN_ID}"
  setup_run_worktree "$(pwd)"

  RESULT=0
  assert_file_exists "${WORKTREE_DIR}/.factory/runs/${RUN_ID}/scope" || RESULT=1
  assert_file_contains "${WORKTREE_DIR}/.factory/runs/${RUN_ID}/scope" "documentation/" || RESULT=1

  cleanup_test_project
  return $RESULT
}

test_scan_finds_executing_as_active() {
  setup_test_project

  mkdir -p ".factory/runs/run-exec"
  printf 'executing' > ".factory/runs/run-exec/status"
  printf 'Brief' > ".factory/runs/run-exec/brief.md"

  RUN_ID=""
  resolve_run_id "$(pwd)"

  RESULT=0
  [ "$RUN_ID" = "run-exec" ] || { printf '    FAIL: RUN_ID=%s, expected run-exec\n' "$RUN_ID"; RESULT=1; }

  cleanup_test_project
  return $RESULT
}

test_scan_skips_needs_user() {
  setup_test_project

  mkdir -p ".factory/runs/run-nu" ".factory/runs/run-plan"
  printf 'needs-user' > ".factory/runs/run-nu/status"
  printf 'Paused' > ".factory/runs/run-nu/brief.md"
  printf 'planned' > ".factory/runs/run-plan/status"
  printf 'Ready' > ".factory/runs/run-plan/brief.md"

  RUN_ID=""
  resolve_run_id "$(pwd)"

  RESULT=0
  [ "$RUN_ID" = "run-plan" ] || { printf '    FAIL: RUN_ID=%s, expected run-plan (should skip needs-user)\n' "$RUN_ID"; RESULT=1; }

  cleanup_test_project
  return $RESULT
}

test_scan_skips_failed() {
  setup_test_project

  mkdir -p ".factory/runs/run-fail" ".factory/runs/run-plan"
  printf 'failed' > ".factory/runs/run-fail/status"
  printf 'Fail brief' > ".factory/runs/run-fail/brief.md"
  printf 'planned' > ".factory/runs/run-plan/status"
  printf 'Plan brief' > ".factory/runs/run-plan/brief.md"

  RUN_ID=""
  resolve_run_id "$(pwd)"

  RESULT=0
  [ "$RUN_ID" = "run-plan" ] || { printf '    FAIL: RUN_ID=%s, expected run-plan (should skip failed)\n' "$RUN_ID"; RESULT=1; }

  cleanup_test_project
  return $RESULT
}

test_status_with_no_runs() {
  setup_test_project

  mkdir -p ".factory/runs"
  # No run directories — status should not error
  OUTPUT="$(cmd_status "$(pwd)" 2>&1 || true)"

  RESULT=0
  # Should produce some output without crashing
  if [ -z "$OUTPUT" ]; then
    printf '    FAIL: status produced no output\n'
    RESULT=1
  fi

  cleanup_test_project
  return $RESULT
}

# -------------------------------------------------------------------------
# Run all tests
# -------------------------------------------------------------------------

printf 'test-scope-and-edges\n\n'

run_test "worktree copies scope file" test_worktree_copies_scope_file
run_test "scan finds executing as active" test_scan_finds_executing_as_active
run_test "scan skips needs-user runs" test_scan_skips_needs_user
run_test "scan skips failed runs" test_scan_skips_failed
run_test "status with no runs" test_status_with_no_runs

printf '\n  %d passed, %d failed\n' "$PASS" "$FAIL"

if [ "$FAIL" -gt 0 ]; then
  printf '\n  Failures:%b\n' "$ERRORS"
  exit 1
fi
