#!/usr/bin/env bash
# test-run-state — Verify run state behaviors not covered by test-run.
#
# Tests operational behaviors from documentation/behaviors.md that
# involve run state, worktree contents, and status display.
#
# Sources the factory script in library mode to call functions directly.
#
# Covers:
#   - Worktree copies all run state files (brief.md, behaviors.diff.md,
#     approach.md, plan.md, status)
#   - Status display includes backend and brief summary
#   - Run-id scan ignores completed runs
#   - Worktree records source-branch and worktree path
#
# Usage:
#   tests/behaviors/operations/test-run-state.sh

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
  TEST_DIR="$(mktemp -d -t factory-test-state-XXXXXX)"
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

assert_output_contains() {
  if ! echo "$1" | grep -q "$2"; then
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

test_worktree_copies_all_run_state() {
  setup_test_project

  RUN_ID="test-full-state"
  mkdir -p ".factory/runs/${RUN_ID}"
  printf 'Test brief content' > ".factory/runs/${RUN_ID}/brief.md"
  printf '## New behaviors\nWHEN x THE SYSTEM SHALL y' > ".factory/runs/${RUN_ID}/behaviors.diff.md"
  printf '## Approach\nDo the thing' > ".factory/runs/${RUN_ID}/approach.md"
  printf '## Plan\n1. Step one' > ".factory/runs/${RUN_ID}/plan.md"
  printf 'planned' > ".factory/runs/${RUN_ID}/status"

  RUN_DIR="$(pwd)/.factory/runs/${RUN_ID}"
  setup_run_worktree "$(pwd)"

  RESULT=0
  assert_file_exists "${WORKTREE_DIR}/.factory/runs/${RUN_ID}/brief.md" || RESULT=1
  assert_file_exists "${WORKTREE_DIR}/.factory/runs/${RUN_ID}/behaviors.diff.md" || RESULT=1
  assert_file_exists "${WORKTREE_DIR}/.factory/runs/${RUN_ID}/approach.md" || RESULT=1
  assert_file_exists "${WORKTREE_DIR}/.factory/runs/${RUN_ID}/plan.md" || RESULT=1
  assert_file_exists "${WORKTREE_DIR}/.factory/runs/${RUN_ID}/status" || RESULT=1
  assert_file_contains "${WORKTREE_DIR}/.factory/active-run" "$RUN_ID" || RESULT=1

  cleanup_test_project
  return $RESULT
}

test_worktree_records_source_branch_and_path() {
  setup_test_project

  RUN_ID="test-branch-record"
  mkdir -p ".factory/runs/${RUN_ID}"
  printf 'Brief' > ".factory/runs/${RUN_ID}/brief.md"
  printf 'planned' > ".factory/runs/${RUN_ID}/status"

  RUN_DIR="$(pwd)/.factory/runs/${RUN_ID}"
  setup_run_worktree "$(pwd)"

  RESULT=0
  # source-branch should be recorded
  assert_file_exists "${RUN_DIR}/source-branch" || RESULT=1
  assert_file_contains "${RUN_DIR}/source-branch" "main" || RESULT=1

  # worktree path should be recorded
  assert_file_exists "${RUN_DIR}/worktree" || RESULT=1

  # The worktree path should be a real directory
  WT_PATH="$(cat "${RUN_DIR}/worktree")"
  if [ ! -d "$WT_PATH" ]; then
    printf '    FAIL: worktree path %s is not a directory\n' "$WT_PATH"
    RESULT=1
  fi

  cleanup_test_project
  return $RESULT
}

test_resolve_run_id_scan_ignores_complete() {
  setup_test_project

  # Create a completed run and an active run
  mkdir -p ".factory/runs/run-done" ".factory/runs/run-active"
  printf 'complete' > ".factory/runs/run-done/status"
  printf 'Brief' > ".factory/runs/run-done/brief.md"
  printf 'planned' > ".factory/runs/run-active/status"
  printf 'Brief' > ".factory/runs/run-active/brief.md"
  # No active-run file — force scan

  RUN_ID=""
  resolve_run_id "$(pwd)"

  RESULT=0
  [ "$RUN_ID" = "run-active" ] || { printf '    FAIL: RUN_ID=%s, expected run-active (should skip complete)\n' "$RUN_ID"; RESULT=1; }

  cleanup_test_project
  return $RESULT
}

test_status_display_includes_backend() {
  setup_test_project

  mkdir -p ".factory/runs/run-backend-test"
  printf 'executing' > ".factory/runs/run-backend-test/status"
  printf 'Testing backend display' > ".factory/runs/run-backend-test/brief.md"
  printf 'local' > ".factory/runs/run-backend-test/backend"

  OUTPUT="$(cmd_status "$(pwd)" 2>&1 || true)"

  RESULT=0
  assert_output_contains "$OUTPUT" "run-backend-test" || RESULT=1
  assert_output_contains "$OUTPUT" "executing" || RESULT=1
  assert_output_contains "$OUTPUT" "local" || RESULT=1

  cleanup_test_project
  return $RESULT
}

test_status_display_includes_brief_summary() {
  setup_test_project

  mkdir -p ".factory/runs/run-brief-test"
  printf 'planned' > ".factory/runs/run-brief-test/status"
  printf 'Add a timeout flag to the factory command' > ".factory/runs/run-brief-test/brief.md"
  printf 'local' > ".factory/runs/run-brief-test/backend"

  OUTPUT="$(cmd_status "$(pwd)" 2>&1 || true)"

  RESULT=0
  assert_output_contains "$OUTPUT" "run-brief-test" || RESULT=1
  # Status display should include some part of the brief content
  assert_output_contains "$OUTPUT" "timeout" || RESULT=1

  cleanup_test_project
  return $RESULT
}

# -------------------------------------------------------------------------
# Run all tests
# -------------------------------------------------------------------------

printf 'test-run-state\n\n'

run_test "worktree copies all run state files" test_worktree_copies_all_run_state
run_test "worktree records source-branch and path" test_worktree_records_source_branch_and_path
run_test "run-id scan ignores completed runs" test_resolve_run_id_scan_ignores_complete
run_test "status display includes backend" test_status_display_includes_backend
run_test "status display includes brief summary" test_status_display_includes_brief_summary

printf '\n  %d passed, %d failed\n' "$PASS" "$FAIL"

if [ "$FAIL" -gt 0 ]; then
  printf '\n  Failures:%b\n' "$ERRORS"
  exit 1
fi
