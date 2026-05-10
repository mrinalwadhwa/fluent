#!/usr/bin/env bash
# test-review-mode — Verify review-mode run state behaviors.
#
# Tests that review-mode runs have proper state files and that
# factory status displays them correctly.
#
# Sources the factory script in library mode to call functions directly.
#
# Covers:
#   - Worktree copies mode file for review runs
#   - Status display works with review-mode runs
#   - Reviewers file is copied to worktree when present
#
# Usage:
#   tests/behaviors/operations/test-review-mode.sh

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
  TEST_DIR="$(mktemp -d -t factory-test-review-XXXXXX)"
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

test_worktree_copies_mode_file() {
  setup_test_project

  RUN_ID="test-review-mode"
  mkdir -p ".factory/runs/${RUN_ID}"
  printf 'Review brief' > ".factory/runs/${RUN_ID}/brief.md"
  printf 'planned' > ".factory/runs/${RUN_ID}/status"
  printf 'review' > ".factory/runs/${RUN_ID}/mode"

  RUN_DIR="$(pwd)/.factory/runs/${RUN_ID}"
  setup_run_worktree "$(pwd)"

  RESULT=0
  assert_file_exists "${WORKTREE_DIR}/.factory/runs/${RUN_ID}/mode" || RESULT=1
  assert_file_contains "${WORKTREE_DIR}/.factory/runs/${RUN_ID}/mode" "review" || RESULT=1

  cleanup_test_project
  return $RESULT
}

test_worktree_copies_reviewers_file() {
  setup_test_project

  RUN_ID="test-reviewers-file"
  mkdir -p ".factory/runs/${RUN_ID}"
  printf 'Review brief' > ".factory/runs/${RUN_ID}/brief.md"
  printf 'planned' > ".factory/runs/${RUN_ID}/status"
  printf 'review' > ".factory/runs/${RUN_ID}/mode"
  printf 'review-documentation,review-behaviors' > ".factory/runs/${RUN_ID}/reviewers"

  RUN_DIR="$(pwd)/.factory/runs/${RUN_ID}"
  setup_run_worktree "$(pwd)"

  RESULT=0
  assert_file_exists "${WORKTREE_DIR}/.factory/runs/${RUN_ID}/reviewers" || RESULT=1
  assert_file_contains "${WORKTREE_DIR}/.factory/runs/${RUN_ID}/reviewers" "review-documentation,review-behaviors" || RESULT=1

  cleanup_test_project
  return $RESULT
}

test_status_display_with_review_run() {
  setup_test_project

  mkdir -p ".factory/runs/review-test"
  printf 'executing' > ".factory/runs/review-test/status"
  printf 'Full review' > ".factory/runs/review-test/brief.md"
  printf 'local' > ".factory/runs/review-test/backend"
  printf 'review' > ".factory/runs/review-test/mode"

  OUTPUT="$(cmd_status "$(pwd)" 2>&1 || true)"

  RESULT=0
  assert_output_contains "$OUTPUT" "review-test" || RESULT=1
  assert_output_contains "$OUTPUT" "executing" || RESULT=1
  assert_output_contains "$OUTPUT" "local" || RESULT=1

  cleanup_test_project
  return $RESULT
}

# -------------------------------------------------------------------------
# Run all tests
# -------------------------------------------------------------------------

printf 'test-review-mode\n\n'

run_test "worktree copies mode file" test_worktree_copies_mode_file
run_test "worktree copies reviewers file" test_worktree_copies_reviewers_file
run_test "status display with review run" test_status_display_with_review_run

printf '\n  %d passed, %d failed\n' "$PASS" "$FAIL"

if [ "$FAIL" -gt 0 ]; then
  printf '\n  Failures:%b\n' "$ERRORS"
  exit 1
fi
