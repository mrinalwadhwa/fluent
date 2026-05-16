#!/usr/bin/env bash
# test-land — Verify factory land behaviors.
#
# Tests that `factory land` completes the run lifecycle: rebases the run
# branch onto main, fast-forward merges, copies artifacts from the worktree
# back to the source run directory, removes the worktree, and deletes the
# branch.
#
# Covers:
#   - land refuses to land a run with status other than 'complete'
#   - land refuses to land when any review verdict is not 'pass'
#   - land allows landing when no reviews exist
#   - land copies sessions/, sessions.log, reviews/, report.md, status back
#   - land removes the worktree
#   - land deletes the run's branch
#   - land fast-forward merges run commits into main
#
# Usage:
#   tests/behaviors/operations/test-land.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$(dirname "$(dirname "$SCRIPT_DIR")")")"
BINARY="${PROJECT_DIR}/target/debug/factory"

PASS=0
FAIL=0
ERRORS=""

# -------------------------------------------------------------------------
# Helpers
# -------------------------------------------------------------------------

setup_test_project() {
  TEST_DIR="$(mktemp -d -t factory-test-land-XXXXXX)"
  mkdir -p "${TEST_DIR}/main"
  cd "${TEST_DIR}/main"
  git init -b main > /dev/null 2>&1
  git config commit.gpgsign false
  git config user.email "test@test"
  git config user.name "test"
  echo "test" > README.md
  git add README.md
  git commit -m "init" > /dev/null 2>&1
}

# Setup a complete run with a worktree, run branch, and artifacts.
# Usage: setup_run_with_worktree RUN_ID [review_verdict]
setup_run_with_worktree() {
  local run_id="$1"
  local verdict="${2:-pass}"

  # Create run branch with a commit
  git checkout -b "$run_id" > /dev/null 2>&1
  echo "run change" >> README.md
  git add README.md
  git commit -m "run commit for ${run_id}" > /dev/null 2>&1
  git checkout main > /dev/null 2>&1

  local wt_path="${TEST_DIR}/${run_id}-wt"
  git worktree add "$wt_path" "$run_id" > /dev/null 2>&1

  # Source run directory state (in main repo)
  mkdir -p ".factory/runs/${run_id}/reviews"
  printf 'complete' > ".factory/runs/${run_id}/status"
  printf 'Test brief for %s' "$run_id" > ".factory/runs/${run_id}/brief.md"
  printf 'main' > ".factory/runs/${run_id}/source-branch"
  printf '%s' "$wt_path" > ".factory/runs/${run_id}/worktree"
  printf 'Verdict: %s\n' "$verdict" > ".factory/runs/${run_id}/reviews/review-behaviors.md"
  printf '%s' "$run_id" > ".factory/active-run"

  # Artifacts in worktree (as if run executed there)
  mkdir -p "${wt_path}/.factory/runs/${run_id}/sessions/session-1"
  mkdir -p "${wt_path}/.factory/runs/${run_id}/reviews"
  printf 'complete' > "${wt_path}/.factory/runs/${run_id}/status"
  printf 'Session log from run' > "${wt_path}/.factory/runs/${run_id}/sessions.log"
  printf '{"event":"done"}' > "${wt_path}/.factory/runs/${run_id}/sessions/session-1/transcript.jsonl"
  printf 'Verdict: %s\n' "$verdict" > "${wt_path}/.factory/runs/${run_id}/reviews/review-behaviors.md"
  printf 'Report from run' > "${wt_path}/.factory/runs/${run_id}/report.md"
}

cleanup_test_project() {
  cd /
  if [ -d "${TEST_DIR}/main/.git" ]; then
    git -C "${TEST_DIR}/main" worktree list --porcelain 2>/dev/null | \
      grep '^worktree ' | awk '{print $2}' | \
      grep -v "${TEST_DIR}/main" | while read -r wt; do
      git -C "${TEST_DIR}/main" worktree remove --force "$wt" 2>/dev/null || true
    done || true
  fi
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

test_land_rejects_non_complete_status() {
  setup_test_project

  RUN_ID="run-not-complete"
  mkdir -p ".factory/runs/${RUN_ID}/reviews"
  printf 'executing' > ".factory/runs/${RUN_ID}/status"
  printf 'Test brief' > ".factory/runs/${RUN_ID}/brief.md"
  printf 'Verdict: pass\n' > ".factory/runs/${RUN_ID}/reviews/review-behaviors.md"

  set +e
  OUTPUT="$("$BINARY" land "$RUN_ID" 2>&1)"
  EXIT_CODE=$?
  set -e

  RESULT=0
  if [ "$EXIT_CODE" -eq 0 ]; then
    printf '    FAIL: land should exit non-zero for non-complete run, got exit 0\n'
    RESULT=1
  fi
  if ! echo "$OUTPUT" | grep -qi "executing"; then
    printf '    FAIL: output should mention the status, got: %s\n' "$OUTPUT"
    RESULT=1
  fi

  cleanup_test_project
  return $RESULT
}

test_land_rejects_failed_review() {
  setup_test_project

  RUN_ID="run-failed-review"
  mkdir -p ".factory/runs/${RUN_ID}/reviews"
  printf 'complete' > ".factory/runs/${RUN_ID}/status"
  printf 'Test brief' > ".factory/runs/${RUN_ID}/brief.md"
  printf 'Verdict: pass\n' > ".factory/runs/${RUN_ID}/reviews/review-a.md"
  printf 'Verdict: fail\n' > ".factory/runs/${RUN_ID}/reviews/review-b.md"

  set +e
  OUTPUT="$("$BINARY" land "$RUN_ID" 2>&1)"
  EXIT_CODE=$?
  set -e

  RESULT=0
  if [ "$EXIT_CODE" -eq 0 ]; then
    printf '    FAIL: land should exit non-zero when review has fail verdict\n'
    RESULT=1
  fi

  cleanup_test_project
  return $RESULT
}

test_land_rejects_uncertain_review() {
  setup_test_project

  RUN_ID="run-uncertain-review"
  mkdir -p ".factory/runs/${RUN_ID}/reviews"
  printf 'complete' > ".factory/runs/${RUN_ID}/status"
  printf 'Test brief' > ".factory/runs/${RUN_ID}/brief.md"
  printf 'Verdict: uncertain\n' > ".factory/runs/${RUN_ID}/reviews/review-a.md"

  set +e
  OUTPUT="$("$BINARY" land "$RUN_ID" 2>&1)"
  EXIT_CODE=$?
  set -e

  RESULT=0
  if [ "$EXIT_CODE" -eq 0 ]; then
    printf '    FAIL: land should exit non-zero when review has uncertain verdict\n'
    RESULT=1
  fi

  cleanup_test_project
  return $RESULT
}

test_land_copies_artifacts() {
  setup_test_project
  RUN_ID="run-copy-artifacts"
  setup_run_with_worktree "$RUN_ID" pass

  set +e
  "$BINARY" land "$RUN_ID" > /dev/null 2>&1
  EXIT_CODE=$?
  set -e

  RESULT=0
  if [ "$EXIT_CODE" -ne 0 ]; then
    printf '    FAIL: land command should succeed, exit code %d\n' "$EXIT_CODE"
    RESULT=1
  fi

  # sessions.log copied
  if [ ! -f ".factory/runs/${RUN_ID}/sessions.log" ]; then
    printf '    FAIL: sessions.log not copied back from worktree\n'
    RESULT=1
  elif ! grep -q "Session log from run" ".factory/runs/${RUN_ID}/sessions.log"; then
    printf '    FAIL: sessions.log content does not match worktree artifact\n'
    RESULT=1
  fi

  # report.md copied
  if [ ! -f ".factory/runs/${RUN_ID}/report.md" ]; then
    printf '    FAIL: report.md not copied back from worktree\n'
    RESULT=1
  fi

  # sessions/ directory copied
  if [ ! -d ".factory/runs/${RUN_ID}/sessions" ]; then
    printf '    FAIL: sessions/ directory not copied back from worktree\n'
    RESULT=1
  fi

  # reviews/ directory present
  if [ ! -d ".factory/runs/${RUN_ID}/reviews" ]; then
    printf '    FAIL: reviews/ directory not copied back from worktree\n'
    RESULT=1
  fi

  cleanup_test_project
  return $RESULT
}

test_land_removes_worktree() {
  setup_test_project
  RUN_ID="run-remove-wt"
  setup_run_with_worktree "$RUN_ID" pass

  WT_PATH="${TEST_DIR}/${RUN_ID}-wt"

  set +e
  "$BINARY" land "$RUN_ID" > /dev/null 2>&1
  set -e

  RESULT=0
  if [ -d "$WT_PATH" ]; then
    printf '    FAIL: worktree directory should have been removed\n'
    RESULT=1
  fi

  cleanup_test_project
  return $RESULT
}

test_land_deletes_branch() {
  setup_test_project
  RUN_ID="run-del-branch"
  setup_run_with_worktree "$RUN_ID" pass

  set +e
  "$BINARY" land "$RUN_ID" > /dev/null 2>&1
  set -e

  RESULT=0
  if git branch --list "$RUN_ID" | grep -q "$RUN_ID"; then
    printf '    FAIL: run branch should have been deleted after landing\n'
    RESULT=1
  fi

  cleanup_test_project
  return $RESULT
}

test_land_merges_to_main() {
  setup_test_project
  RUN_ID="run-merge-main"
  setup_run_with_worktree "$RUN_ID" pass

  set +e
  "$BINARY" land "$RUN_ID" > /dev/null 2>&1
  set -e

  RESULT=0
  # main should now contain the run's commit
  LOG="$(git log --oneline)"
  if ! echo "$LOG" | grep -q "run commit for ${RUN_ID}"; then
    printf '    FAIL: main should contain run commit after landing\n'
    RESULT=1
  fi

  cleanup_test_project
  return $RESULT
}

test_land_fails_on_rebase_conflict() {
  setup_test_project

  # Setup conflicting state: main has a commit that conflicts with run branch
  echo "line1" > README.md
  git add README.md
  git commit -m "base" > /dev/null 2>&1

  RUN_ID="run-conflict"
  git checkout -b "$RUN_ID" > /dev/null 2>&1
  printf "line1\nrun-change" > README.md
  git add README.md
  git commit -m "run commit" > /dev/null 2>&1
  git checkout main > /dev/null 2>&1

  # Add a conflicting commit on main after branching
  printf "line1\nmain-change" > README.md
  git add README.md
  git commit -m "main-parallel" > /dev/null 2>&1

  WT_PATH="${TEST_DIR}/${RUN_ID}-wt"
  git worktree add "$WT_PATH" "$RUN_ID" > /dev/null 2>&1

  mkdir -p ".factory/runs/${RUN_ID}/reviews"
  printf 'complete' > ".factory/runs/${RUN_ID}/status"
  printf 'Conflict test' > ".factory/runs/${RUN_ID}/brief.md"
  printf 'main' > ".factory/runs/${RUN_ID}/source-branch"
  printf '%s' "$WT_PATH" > ".factory/runs/${RUN_ID}/worktree"
  printf 'Verdict: pass\n' > ".factory/runs/${RUN_ID}/reviews/review-behaviors.md"

  mkdir -p "${WT_PATH}/.factory/runs/${RUN_ID}/reviews"
  printf 'complete' > "${WT_PATH}/.factory/runs/${RUN_ID}/status"
  printf 'log' > "${WT_PATH}/.factory/runs/${RUN_ID}/sessions.log"
  printf 'Verdict: pass\n' > "${WT_PATH}/.factory/runs/${RUN_ID}/reviews/review-behaviors.md"

  set +e
  OUTPUT="$("$BINARY" land "$RUN_ID" 2>&1)"
  EXIT_CODE=$?
  set -e

  RESULT=0
  if [ "$EXIT_CODE" -eq 0 ]; then
    printf '    FAIL: land should exit non-zero when rebase has conflicts\n'
    RESULT=1
  fi
  if ! echo "$OUTPUT" | grep -qi "conflict\|rebase"; then
    printf '    FAIL: output should mention conflict or rebase, got: %s\n' "$OUTPUT"
    RESULT=1
  fi

  # Abort any in-progress rebase before cleanup
  git rebase --abort 2>/dev/null || true

  cleanup_test_project
  return $RESULT
}

test_shell_land_rejects_non_complete_status() {
  FACTORY="${PROJECT_DIR}/scripts/factory"
  setup_test_project

  RUN_ID="run-shell-not-complete"
  mkdir -p ".factory/runs/${RUN_ID}/reviews"
  printf 'executing' > ".factory/runs/${RUN_ID}/status"
  printf 'Test brief' > ".factory/runs/${RUN_ID}/brief.md"
  printf 'Verdict: pass\n' > ".factory/runs/${RUN_ID}/reviews/review-behaviors.md"

  set +e
  OUTPUT="$("$FACTORY" land "$RUN_ID" 2>&1)"
  EXIT_CODE=$?
  set -e

  RESULT=0
  if [ "$EXIT_CODE" -eq 0 ]; then
    printf '    FAIL: shell land should exit non-zero for non-complete run, got exit 0\n'
    RESULT=1
  fi

  cleanup_test_project
  return $RESULT
}

test_shell_land_full_workflow() {
  FACTORY="${PROJECT_DIR}/scripts/factory"
  setup_test_project
  RUN_ID="run-shell-full"
  setup_run_with_worktree "$RUN_ID" pass

  WT_PATH="${TEST_DIR}/${RUN_ID}-wt"

  set +e
  OUTPUT="$("$FACTORY" land "$RUN_ID" 2>&1)"
  EXIT_CODE=$?
  set -e

  RESULT=0
  if [ "$EXIT_CODE" -ne 0 ]; then
    printf '    FAIL: shell land should succeed, exit code %d\n' "$EXIT_CODE"
    printf '    Output: %s\n' "$OUTPUT"
    RESULT=1
  fi

  # worktree removed
  if [ -d "$WT_PATH" ]; then
    printf '    FAIL: shell land should remove worktree\n'
    RESULT=1
  fi

  # branch deleted
  if git branch --list "$RUN_ID" | grep -q "$RUN_ID"; then
    printf '    FAIL: shell land should delete run branch\n'
    RESULT=1
  fi

  # artifacts copied
  if [ ! -f ".factory/runs/${RUN_ID}/sessions.log" ]; then
    printf '    FAIL: shell land should copy sessions.log back\n'
    RESULT=1
  fi

  # main updated
  LOG="$(git log --oneline)"
  if ! echo "$LOG" | grep -q "run commit for ${RUN_ID}"; then
    printf '    FAIL: shell land should merge run commits into main\n'
    RESULT=1
  fi

  cleanup_test_project
  return $RESULT
}

test_land_allows_no_reviews() {
  setup_test_project

  RUN_ID="run-no-reviews"

  # Create run branch with a commit
  git checkout -b "$RUN_ID" > /dev/null 2>&1
  echo "run change" >> README.md
  git add README.md
  git commit -m "run commit for ${RUN_ID}" > /dev/null 2>&1
  git checkout main > /dev/null 2>&1

  WT_PATH="${TEST_DIR}/${RUN_ID}-wt"
  git worktree add "$WT_PATH" "$RUN_ID" > /dev/null 2>&1

  # Source run directory — no reviews/ directory at all
  mkdir -p ".factory/runs/${RUN_ID}"
  printf 'complete' > ".factory/runs/${RUN_ID}/status"
  printf 'No reviews test' > ".factory/runs/${RUN_ID}/brief.md"
  printf 'main' > ".factory/runs/${RUN_ID}/source-branch"
  printf '%s' "$WT_PATH" > ".factory/runs/${RUN_ID}/worktree"
  printf '%s' "$RUN_ID" > ".factory/active-run"

  # Worktree artifacts — also no reviews
  mkdir -p "${WT_PATH}/.factory/runs/${RUN_ID}/sessions/session-1"
  printf 'complete' > "${WT_PATH}/.factory/runs/${RUN_ID}/status"
  printf 'Session log' > "${WT_PATH}/.factory/runs/${RUN_ID}/sessions.log"
  printf '{"event":"done"}' > "${WT_PATH}/.factory/runs/${RUN_ID}/sessions/session-1/transcript.jsonl"
  printf 'Report' > "${WT_PATH}/.factory/runs/${RUN_ID}/report.md"

  set +e
  OUTPUT="$("$BINARY" land "$RUN_ID" 2>&1)"
  EXIT_CODE=$?
  set -e

  RESULT=0
  if [ "$EXIT_CODE" -ne 0 ]; then
    printf '    FAIL: land should succeed when no reviews exist, exit code %d\n' "$EXIT_CODE"
    printf '    Output: %s\n' "$OUTPUT"
    RESULT=1
  fi

  # Verify it actually landed — main should contain the run commit
  LOG="$(git log --oneline)"
  if ! echo "$LOG" | grep -q "run commit for ${RUN_ID}"; then
    printf '    FAIL: main should contain run commit after landing with no reviews\n'
    RESULT=1
  fi

  cleanup_test_project
  return $RESULT
}

# -------------------------------------------------------------------------
# Run all tests
# -------------------------------------------------------------------------

printf 'test-land\n\n'

run_test "land rejects non-complete run" test_land_rejects_non_complete_status
run_test "land rejects fail review verdict" test_land_rejects_failed_review
run_test "land rejects uncertain review verdict" test_land_rejects_uncertain_review
run_test "land copies artifacts from worktree" test_land_copies_artifacts
run_test "land removes worktree" test_land_removes_worktree
run_test "land deletes run branch" test_land_deletes_branch
run_test "land merges run commits into main" test_land_merges_to_main
run_test "land fails on rebase conflict" test_land_fails_on_rebase_conflict
run_test "land allows run with no reviews" test_land_allows_no_reviews
run_test "shell: land rejects non-complete run (exit code)" test_shell_land_rejects_non_complete_status
run_test "shell: land full workflow" test_shell_land_full_workflow

printf '\n  %d passed, %d failed\n' "$PASS" "$FAIL"

if [ "$FAIL" -gt 0 ]; then
  printf '\n  Failures:%b\n' "$ERRORS"
  exit 1
fi
