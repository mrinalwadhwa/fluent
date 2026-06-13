#!/usr/bin/env bash
# test-run-merge — Verify factory merge behaviors for legacy Run model.
#
# Tests that `factory merge <run-id>` completes the run lifecycle:
# rebases the run branch onto main, fast-forward merges, copies
# artifacts from the worktree back to the source run directory,
# removes the worktree, and deletes the branch.
#
# Covers:
#   - merge refuses a run with status other than 'complete'
#   - merge refuses when any review verdict is not 'pass'
#   - merge allows runs with no reviews
#   - merge copies sessions/, sessions.log, reviews/, report.md, status back
#   - merge removes the worktree
#   - merge deletes the run's branch
#   - merge fast-forward merges run commits into main
#
# Usage:
#   tests/behaviors/operations/test-run-merge.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$(dirname "$(dirname "$SCRIPT_DIR")")")"
BINARY="${FACTORY_BIN_OVERRIDE:-${PROJECT_DIR}/target/debug/factory}"

source "${PROJECT_DIR}/tests/lib/run_test.sh"
LOG_DIR="${PROJECT_DIR}/tests/output/$(basename "$0" .sh)"

# -------------------------------------------------------------------------
# Helpers
# -------------------------------------------------------------------------

setup_test_project() {
  TEST_DIR="$(mktemp -d -t factory-test-run-merge-XXXXXX)"
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

write_check_hook() {
  mkdir -p .factory/hooks
  cat > .factory/hooks/check-pre-merge
  chmod +x .factory/hooks/check-pre-merge
}

write_fix_hook() {
  mkdir -p .factory/hooks
  cat > .factory/hooks/fix-pre-merge
  chmod +x .factory/hooks/fix-pre-merge
}

write_mock_reviewer() {
  local verdict="${1:-pass}"
  local failing_reviewer="${2:-}"

  MOCK_BIN="${TEST_DIR}/bin"
  mkdir -p "$MOCK_BIN"
  cat > "${MOCK_BIN}/claude" <<'EOF'
#!/usr/bin/env bash
args="$(printf '%s' "$*" | tr '\n' ' ')"
review_path="$(
  printf '%s' "$args" |
    sed -n 's#.*Write your review to \([^ ]*reviews/review-[a-z]*\.md\).*#\1#p'
)"
reviewer="unknown"

if [ -n "${review_path}" ]; then
  reviewer="$(basename "$review_path" .md)"
  reviewer="${reviewer#review-}"
  mkdir -p "$(dirname "$review_path")"
  if [ "${reviewer}" = "${MOCK_FAILING_REVIEWER:-}" ]; then
    printf 'Verdict: %s\n\nMock reviewer finding.\n' \
      "${MOCK_REVIEW_VERDICT:-fail}" \
      > "$review_path"
  else
    printf 'Verdict: pass\n\nMock reviewer passed.\n' \
      > "$review_path"
  fi
fi

if [ -n "${MOCK_REVIEW_LOG:-}" ]; then
  printf '%s\n' "${reviewer}" >> "${MOCK_REVIEW_LOG}"
fi

printf '{"type":"result","subtype":"success","result":"done","session_id":"mock"}\n'
EOF
  chmod +x "${MOCK_BIN}/claude"

  MOCK_REVIEW_VERDICT="$verdict"
  MOCK_FAILING_REVIEWER="$failing_reviewer"
  MOCK_REVIEW_LOG="${TEST_DIR}/review-calls.log"
  export MOCK_REVIEW_VERDICT MOCK_FAILING_REVIEWER MOCK_REVIEW_LOG
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

# -------------------------------------------------------------------------
# Tests
# -------------------------------------------------------------------------

test_merge_rejects_non_complete_status() {
  setup_test_project

  RUN_ID="run-not-complete"
  mkdir -p ".factory/runs/${RUN_ID}/reviews"
  printf 'executing' > ".factory/runs/${RUN_ID}/status"
  printf 'Test brief' > ".factory/runs/${RUN_ID}/brief.md"
  printf 'Verdict: pass\n' > ".factory/runs/${RUN_ID}/reviews/review-behaviors.md"

  set +e
  OUTPUT="$("$BINARY" merge "$RUN_ID" 2>&1)"
  EXIT_CODE=$?
  set -e

  RESULT=0
  if [ "$EXIT_CODE" -eq 0 ]; then
    printf '    FAIL: merge should exit non-zero for non-complete run, got exit 0\n'
    RESULT=1
  fi
  if ! echo "$OUTPUT" | grep -qi "executing"; then
    printf '    FAIL: output should mention the status, got: %s\n' "$OUTPUT"
    RESULT=1
  fi

  cleanup_test_project
  return $RESULT
}

test_merge_rejects_failed_review() {
  setup_test_project

  RUN_ID="run-failed-review"
  mkdir -p ".factory/runs/${RUN_ID}/reviews"
  printf 'complete' > ".factory/runs/${RUN_ID}/status"
  printf 'Test brief' > ".factory/runs/${RUN_ID}/brief.md"
  printf 'Verdict: pass\n' > ".factory/runs/${RUN_ID}/reviews/review-a.md"
  printf 'Verdict: fail\n' > ".factory/runs/${RUN_ID}/reviews/review-b.md"

  set +e
  OUTPUT="$("$BINARY" merge "$RUN_ID" 2>&1)"
  EXIT_CODE=$?
  set -e

  RESULT=0
  if [ "$EXIT_CODE" -eq 0 ]; then
    printf '    FAIL: merge should exit non-zero when review has fail verdict\n'
    RESULT=1
  fi

  cleanup_test_project
  return $RESULT
}

test_merge_rejects_uncertain_review() {
  setup_test_project

  RUN_ID="run-uncertain-review"
  mkdir -p ".factory/runs/${RUN_ID}/reviews"
  printf 'complete' > ".factory/runs/${RUN_ID}/status"
  printf 'Test brief' > ".factory/runs/${RUN_ID}/brief.md"
  printf 'Verdict: uncertain\n' > ".factory/runs/${RUN_ID}/reviews/review-a.md"

  set +e
  OUTPUT="$("$BINARY" merge "$RUN_ID" 2>&1)"
  EXIT_CODE=$?
  set -e

  RESULT=0
  if [ "$EXIT_CODE" -eq 0 ]; then
    printf '    FAIL: merge should exit non-zero when review has uncertain verdict\n'
    RESULT=1
  fi

  cleanup_test_project
  return $RESULT
}

test_merge_copies_artifacts() {
  setup_test_project
  RUN_ID="run-copy-artifacts"
  setup_run_with_worktree "$RUN_ID" pass

  set +e
  "$BINARY" merge "$RUN_ID" > /dev/null 2>&1
  EXIT_CODE=$?
  set -e

  RESULT=0
  if [ "$EXIT_CODE" -ne 0 ]; then
    printf '    FAIL: merge command should succeed, exit code %d\n' "$EXIT_CODE"
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

test_merge_removes_worktree() {
  setup_test_project
  RUN_ID="run-remove-wt"
  setup_run_with_worktree "$RUN_ID" pass

  WT_PATH="${TEST_DIR}/${RUN_ID}-wt"

  set +e
  "$BINARY" merge "$RUN_ID" > /dev/null 2>&1
  set -e

  RESULT=0
  if [ -d "$WT_PATH" ]; then
    printf '    FAIL: worktree directory should have been removed\n'
    RESULT=1
  fi

  cleanup_test_project
  return $RESULT
}

test_merge_deletes_branch() {
  setup_test_project
  RUN_ID="run-del-branch"
  setup_run_with_worktree "$RUN_ID" pass

  set +e
  "$BINARY" merge "$RUN_ID" > /dev/null 2>&1
  set -e

  RESULT=0
  if git branch --list "$RUN_ID" | grep -q "$RUN_ID"; then
    printf '    FAIL: run branch should have been deleted after merging\n'
    RESULT=1
  fi

  cleanup_test_project
  return $RESULT
}

test_merge_merges_to_main() {
  setup_test_project
  RUN_ID="run-merge-main"
  setup_run_with_worktree "$RUN_ID" pass

  set +e
  "$BINARY" merge "$RUN_ID" > /dev/null 2>&1
  set -e

  RESULT=0
  # main should now contain the run's commit
  LOG="$(git log --oneline)"
  if ! echo "$LOG" | grep -q "run commit for ${RUN_ID}"; then
    printf '    FAIL: main should contain run commit after merging\n'
    RESULT=1
  fi

  cleanup_test_project
  return $RESULT
}

test_merge_fails_on_rebase_conflict() {
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
  OUTPUT="$("$BINARY" merge "$RUN_ID" 2>&1)"
  EXIT_CODE=$?
  set -e

  RESULT=0
  if [ "$EXIT_CODE" -eq 0 ]; then
    printf '    FAIL: merge should exit non-zero when rebase has conflicts\n'
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

test_shell_merge_rejects_non_complete_status() {
  FACTORY="$BINARY"
  setup_test_project

  RUN_ID="run-shell-not-complete"
  mkdir -p ".factory/runs/${RUN_ID}/reviews"
  printf 'executing' > ".factory/runs/${RUN_ID}/status"
  printf 'Test brief' > ".factory/runs/${RUN_ID}/brief.md"
  printf 'Verdict: pass\n' > ".factory/runs/${RUN_ID}/reviews/review-behaviors.md"

  set +e
  OUTPUT="$("$FACTORY" merge "$RUN_ID" 2>&1)"
  EXIT_CODE=$?
  set -e

  RESULT=0
  if [ "$EXIT_CODE" -eq 0 ]; then
    printf '    FAIL: merge should exit non-zero for non-complete run, got exit 0\n'
    RESULT=1
  fi

  cleanup_test_project
  return $RESULT
}

test_shell_merge_full_workflow() {
  FACTORY="$BINARY"
  setup_test_project
  RUN_ID="run-shell-full"
  setup_run_with_worktree "$RUN_ID" pass

  WT_PATH="${TEST_DIR}/${RUN_ID}-wt"

  set +e
  OUTPUT="$("$FACTORY" merge "$RUN_ID" 2>&1)"
  EXIT_CODE=$?
  set -e

  RESULT=0
  if [ "$EXIT_CODE" -ne 0 ]; then
    printf '    FAIL: merge should succeed, exit code %d\n' "$EXIT_CODE"
    printf '    Output: %s\n' "$OUTPUT"
    RESULT=1
  fi

  # worktree removed
  if [ -d "$WT_PATH" ]; then
    printf '    FAIL: shell merge should remove worktree\n'
    RESULT=1
  fi

  # branch deleted
  if git branch --list "$RUN_ID" | grep -q "$RUN_ID"; then
    printf '    FAIL: shell merge should delete run branch\n'
    RESULT=1
  fi

  # artifacts copied
  if [ ! -f ".factory/runs/${RUN_ID}/sessions.log" ]; then
    printf '    FAIL: shell merge should copy sessions.log back\n'
    RESULT=1
  fi

  # main updated
  LOG="$(git log --oneline)"
  if ! echo "$LOG" | grep -q "run commit for ${RUN_ID}"; then
    printf '    FAIL: shell merge should merge run commits into main\n'
    RESULT=1
  fi

  cleanup_test_project
  return $RESULT
}

test_merge_allows_no_reviews() {
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
  OUTPUT="$("$BINARY" merge "$RUN_ID" 2>&1)"
  EXIT_CODE=$?
  set -e

  RESULT=0
  if [ "$EXIT_CODE" -ne 0 ]; then
    printf '    FAIL: merge should succeed when no reviews exist, exit code %d\n' "$EXIT_CODE"
    printf '    Output: %s\n' "$OUTPUT"
    RESULT=1
  fi

  # Verify it actually landed — main should contain the run commit
  LOG="$(git log --oneline)"
  if ! echo "$LOG" | grep -q "run commit for ${RUN_ID}"; then
    printf '    FAIL: main should contain run commit after merging with no reviews\n'
    RESULT=1
  fi

  cleanup_test_project
  return $RESULT
}

test_merge_without_check_config_does_not_require_formatter() {
  setup_test_project
  RUN_ID="run-no-check-config"
  setup_run_with_worktree "$RUN_ID" pass

  MOCK_BIN="${TEST_DIR}/bin"
  mkdir -p "$MOCK_BIN"
  cat > "${MOCK_BIN}/cargo" <<'EOF'
#!/usr/bin/env bash
printf 'cargo should not run without factory check config\n' >&2
exit 99
EOF
  chmod +x "${MOCK_BIN}/cargo"

  set +e
  PATH="${MOCK_BIN}:$PATH" "$BINARY" merge "$RUN_ID" > "${TEST_DIR}/land.out" 2>&1
  EXIT_CODE=$?
  set -e

  RESULT=0
  if [ "$EXIT_CODE" -ne 0 ]; then
    printf '    FAIL: merge should succeed without check config, exit code %d\n' "$EXIT_CODE"
    printf '    Output: %s\n' "$(cat "${TEST_DIR}/land.out")"
    RESULT=1
  fi
  if [ -d "${TEST_DIR}/${RUN_ID}-wt" ]; then
    printf '    FAIL: merge should still remove the worktree\n'
    RESULT=1
  fi

  cleanup_test_project
  return $RESULT
}

test_merge_runs_blocking_check_in_worktree() {
  setup_test_project
  RUN_ID="run-check-pwd"
  setup_run_with_worktree "$RUN_ID" pass
  WT_PATH="${TEST_DIR}/${RUN_ID}-wt"

  write_check_hook <<EOF
#!/usr/bin/env bash
printf '%s' "\$PWD" > '${TEST_DIR}/check-pwd'
EOF

  set +e
  OUTPUT="$("$BINARY" merge "$RUN_ID" 2>&1)"
  EXIT_CODE=$?
  set -e

  RESULT=0
  if [ "$EXIT_CODE" -ne 0 ]; then
    printf '    FAIL: merge should succeed when the configured check passes\n'
    printf '    Output: %s\n' "$OUTPUT"
    RESULT=1
  fi
  if [ ! -f "${TEST_DIR}/check-pwd" ]; then
    printf '    FAIL: check did not write its marker\n'
    RESULT=1
  else
    CHECK_PWD="$(cat "${TEST_DIR}/check-pwd")"
    if [ "${CHECK_PWD#/private}" != "${WT_PATH#/private}" ]; then
      printf '    FAIL: check ran in %s, expected %s\n' "$CHECK_PWD" "$WT_PATH"
      RESULT=1
    fi
  fi

  cleanup_test_project
  return $RESULT
}

test_merge_failed_check_keeps_worktree_and_reports_details() {
  setup_test_project
  RUN_ID="run-check-fails"
  setup_run_with_worktree "$RUN_ID" pass
  WT_PATH="${TEST_DIR}/${RUN_ID}-wt"

  write_check_hook <<'EOF'
#!/usr/bin/env bash
printf 'quality output\n' >&2
exit 42
EOF

  set +e
  OUTPUT="$("$BINARY" merge "$RUN_ID" 2>&1)"
  EXIT_CODE=$?
  set -e

  RESULT=0
  if [ "$EXIT_CODE" -eq 0 ]; then
    printf '    FAIL: merge should exit non-zero when a blocking check fails\n'
    RESULT=1
  fi
  if [ ! -d "$WT_PATH" ]; then
    printf '    FAIL: worktree should remain after check failure\n'
    RESULT=1
  fi
  if ! git branch --list "$RUN_ID" | grep -q "$RUN_ID"; then
    printf '    FAIL: run branch should remain unlanded after check failure\n'
    RESULT=1
  fi
  if ! printf '%s' "$OUTPUT" | grep -q "check-pre-merge failed"; then
    printf '    FAIL: output should report hook failure, got: %s\n' "$OUTPUT"
    RESULT=1
  fi
  if ! printf '%s' "$OUTPUT" | grep -q "exit 42"; then
    printf '    FAIL: output should include hook exit code, got: %s\n' "$OUTPUT"
    RESULT=1
  fi
  LOG_PATH="$(printf '%s' "$OUTPUT" | sed -n 's#.*Log: \([^[:space:]]*\).*#\1#p' | head -1)"
  if [ -z "$LOG_PATH" ] || [ ! -f "$LOG_PATH" ]; then
    printf '    FAIL: output should point at hook log file, got: %s\n' "$OUTPUT"
    RESULT=1
  elif ! grep -q "quality output" "$LOG_PATH"; then
    printf '    FAIL: hook log should capture hook output, got: %s\n' "$(cat "$LOG_PATH" 2>/dev/null)"
    RESULT=1
  fi

  cleanup_test_project
  return $RESULT
}

test_merge_autofix_commits_reruns_checks_and_reviewers() {
  setup_test_project
  RUN_ID="run-autofix-pass"
  setup_run_with_worktree "$RUN_ID" pass
  WT_PATH="${TEST_DIR}/${RUN_ID}-wt"

  printf 'needs fix\n' > "${WT_PATH}/format.txt"
  git -C "$WT_PATH" add format.txt
  git -C "$WT_PATH" commit -m "add unformatted file" > /dev/null 2>&1

  write_mock_reviewer pass
  write_check_hook <<EOF
#!/usr/bin/env bash
printf check >> '${TEST_DIR}/check-count'
grep -q fixed format.txt
EOF
  write_fix_hook <<EOF
#!/usr/bin/env bash
printf fixed > format.txt
EOF

  set +e
  OUTPUT="$(PATH="${MOCK_BIN}:$PATH" "$BINARY" merge "$RUN_ID" 2>&1)"
  EXIT_CODE=$?
  set -e

  RESULT=0
  if [ "$EXIT_CODE" -ne 0 ]; then
    printf '    FAIL: merge should succeed after autofix and passing reviews, exit code %d\n' "$EXIT_CODE"
    printf '    Output: %s\n' "$OUTPUT"
    RESULT=1
  fi
  if [ -d "$WT_PATH" ]; then
    printf '    FAIL: worktree should be removed after successful autofix land\n'
    RESULT=1
  fi
  if ! grep -q "fixed" format.txt 2>/dev/null; then
    printf '    FAIL: autofix change should be merged into main\n'
    RESULT=1
  fi
  if [ ! -f "${TEST_DIR}/check-count" ] || [ "$(wc -c < "${TEST_DIR}/check-count")" -lt 10 ]; then
    printf '    FAIL: pre-merge check should run before and after autofix\n'
    RESULT=1
  fi
  if [ ! -f "${TEST_DIR}/review-calls.log" ] || [ "$(wc -l < "${TEST_DIR}/review-calls.log")" -lt 5 ]; then
    printf '    FAIL: reviewers should rerun after autofix\n'
    RESULT=1
  fi
  if ! printf '%s' "$OUTPUT" | grep -q "Rerunning reviewers after fix-pre-merge autofix"; then
    printf '    FAIL: output should report reviewer rerun after autofix, got: %s\n' "$OUTPUT"
    RESULT=1
  fi
  if ! grep -q "Mock reviewer passed" ".factory/runs/${RUN_ID}/reviews/review-tests.md" 2>/dev/null; then
    printf '    FAIL: rerun review artifacts should be copied back to the source run\n'
    RESULT=1
  fi

  cleanup_test_project
  return $RESULT
}

test_merge_autofix_requires_clean_worktree() {
  setup_test_project
  RUN_ID="run-autofix-dirty"
  setup_run_with_worktree "$RUN_ID" pass
  WT_PATH="${TEST_DIR}/${RUN_ID}-wt"

  printf 'dirty user change\n' > "${WT_PATH}/dirty.txt"

  write_check_hook <<'EOF'
#!/usr/bin/env bash
printf 'check-output\n' >&2
exit 1
EOF
  write_fix_hook <<EOF
#!/usr/bin/env bash
printf fixed > '${TEST_DIR}/fix-ran'
EOF

  set +e
  OUTPUT="$("$BINARY" merge "$RUN_ID" 2>&1)"
  EXIT_CODE=$?
  set -e

  RESULT=0
  if [ "$EXIT_CODE" -eq 0 ]; then
    printf '    FAIL: merge should exit non-zero when autofix needs a dirty worktree\n'
    RESULT=1
  fi
  if [ -f "${TEST_DIR}/fix-ran" ]; then
    printf '    FAIL: autofix command should not run with a dirty worktree\n'
    RESULT=1
  fi
  if [ ! -d "$WT_PATH" ]; then
    printf '    FAIL: worktree should remain when autofix refuses a dirty worktree\n'
    RESULT=1
  fi
  if [ "$(cat ".factory/runs/${RUN_ID}/status")" = "merged" ]; then
    printf '    FAIL: run should not be marked landed when autofix refuses a dirty worktree\n'
    RESULT=1
  fi
  if ! printf '%s' "$OUTPUT" | grep -qi "uncommitted changes"; then
    printf '    FAIL: output should explain that autofix requires no uncommitted changes, got: %s\n' "$OUTPUT"
    RESULT=1
  fi

  cleanup_test_project
  return $RESULT
}

test_merge_autofix_command_failure_keeps_worktree() {
  setup_test_project
  RUN_ID="run-autofix-command-fails"
  setup_run_with_worktree "$RUN_ID" pass
  WT_PATH="${TEST_DIR}/${RUN_ID}-wt"

  write_check_hook <<'EOF'
#!/usr/bin/env bash
printf 'check-output\n' >&2
exit 1
EOF
  write_fix_hook <<'EOF'
#!/usr/bin/env bash
printf 'fix-output\n' >&2
exit 2
EOF

  set +e
  OUTPUT="$("$BINARY" merge "$RUN_ID" 2>&1)"
  EXIT_CODE=$?
  set -e

  RESULT=0
  if [ "$EXIT_CODE" -eq 0 ]; then
    printf '    FAIL: merge should exit non-zero when autofix command fails\n'
    RESULT=1
  fi
  if [ ! -d "$WT_PATH" ]; then
    printf '    FAIL: worktree should remain after autofix command failure\n'
    RESULT=1
  fi
  if ! git branch --list "$RUN_ID" | grep -q "$RUN_ID"; then
    printf '    FAIL: run branch should remain after autofix command failure\n'
    RESULT=1
  fi
  if [ "$(cat ".factory/runs/${RUN_ID}/status")" = "merged" ]; then
    printf '    FAIL: run should not be marked landed after autofix command failure\n'
    RESULT=1
  fi
  if ! printf '%s' "$OUTPUT" | grep -q "fix-pre-merge failed"; then
    printf '    FAIL: output should report fix-pre-merge failure, got: %s\n' "$OUTPUT"
    RESULT=1
  fi
  if ! printf '%s' "$OUTPUT" | grep -q "exit 2"; then
    printf '    FAIL: output should include fix hook exit code, got: %s\n' "$OUTPUT"
    RESULT=1
  fi
  LOG_PATH="$(printf '%s' "$OUTPUT" | sed -n 's#.*Log: \([^[:space:]]*\).*#\1#p' | head -1)"
  if [ -z "$LOG_PATH" ] || [ ! -f "$LOG_PATH" ] || ! grep -q "fix-output" "$LOG_PATH"; then
    printf '    FAIL: fix hook log should capture hook stderr, got log %s, output %s\n' "$LOG_PATH" "$OUTPUT"
    RESULT=1
  fi

  cleanup_test_project
  return $RESULT
}

test_merge_autofix_rerun_failure_keeps_worktree() {
  setup_test_project
  RUN_ID="run-autofix-rerun-fails"
  setup_run_with_worktree "$RUN_ID" pass
  WT_PATH="${TEST_DIR}/${RUN_ID}-wt"

  printf 'needs fix\n' > "${WT_PATH}/format.txt"
  git -C "$WT_PATH" add format.txt
  git -C "$WT_PATH" commit -m "add unformatted file" > /dev/null 2>&1

  write_mock_reviewer pass
  write_check_hook <<EOF
#!/usr/bin/env bash
printf check >> '${TEST_DIR}/check-count'
grep -q fixed format.txt && grep -q approved format.txt
EOF
  write_fix_hook <<EOF
#!/usr/bin/env bash
printf fixed > format.txt
EOF

  set +e
  OUTPUT="$(PATH="${MOCK_BIN}:$PATH" "$BINARY" merge "$RUN_ID" 2>&1)"
  EXIT_CODE=$?
  set -e

  RESULT=0
  if [ "$EXIT_CODE" -eq 0 ]; then
    printf '    FAIL: merge should exit non-zero when checks fail after autofix\n'
    RESULT=1
  fi
  if [ ! -d "$WT_PATH" ]; then
    printf '    FAIL: worktree should remain after post-autofix check failure\n'
    RESULT=1
  fi
  if ! git branch --list "$RUN_ID" | grep -q "$RUN_ID"; then
    printf '    FAIL: run branch should remain after post-autofix check failure\n'
    RESULT=1
  fi
  if [ "$(cat ".factory/runs/${RUN_ID}/status")" = "merged" ]; then
    printf '    FAIL: run should not be marked landed after post-autofix check failure\n'
    RESULT=1
  fi
  if [ ! -f "${TEST_DIR}/check-count" ] || [ "$(wc -c < "${TEST_DIR}/check-count")" -lt 10 ]; then
    printf '    FAIL: check should run before and after autofix failure\n'
    RESULT=1
  fi
  if ! grep -q "fixed" "${WT_PATH}/format.txt"; then
    printf '    FAIL: autofix change should remain in the worktree for diagnosis\n'
    RESULT=1
  fi
  if [ -n "$(git -C "$WT_PATH" status --porcelain -- format.txt)" ]; then
    printf '    FAIL: autofix change should be committed before the rerun failure\n'
    RESULT=1
  fi
  if ! printf '%s' "$OUTPUT" | grep -q "check-pre-merge failed after fix-pre-merge"; then
    printf '    FAIL: output should report rerun check failure, got: %s\n' "$OUTPUT"
    RESULT=1
  fi

  cleanup_test_project
  return $RESULT
}

test_merge_autofix_review_failure_keeps_worktree() {
  setup_test_project
  RUN_ID="run-autofix-review-fails"
  setup_run_with_worktree "$RUN_ID" pass
  WT_PATH="${TEST_DIR}/${RUN_ID}-wt"

  printf 'needs fix\n' > "${WT_PATH}/format.txt"
  git -C "$WT_PATH" add format.txt
  git -C "$WT_PATH" commit -m "add unformatted file" > /dev/null 2>&1

  write_mock_reviewer fail tests
  write_check_hook <<EOF
#!/usr/bin/env bash
grep -q fixed format.txt
EOF
  write_fix_hook <<EOF
#!/usr/bin/env bash
printf fixed > format.txt
EOF

  set +e
  OUTPUT="$(PATH="${MOCK_BIN}:$PATH" "$BINARY" merge "$RUN_ID" 2>&1)"
  EXIT_CODE=$?
  set -e

  RESULT=0
  if [ "$EXIT_CODE" -eq 0 ]; then
    printf '    FAIL: merge should exit non-zero when reviewers fail after autofix\n'
    RESULT=1
  fi
  if [ ! -d "$WT_PATH" ]; then
    printf '    FAIL: worktree should remain after autofix reviewer failure\n'
    RESULT=1
  fi
  if ! git branch --list "$RUN_ID" | grep -q "$RUN_ID"; then
    printf '    FAIL: run branch should remain unlanded after autofix reviewer failure\n'
    RESULT=1
  fi
  if [ "$(cat ".factory/runs/${RUN_ID}/status")" = "merged" ]; then
    printf '    FAIL: run should not be marked landed after autofix reviewer failure\n'
    RESULT=1
  fi
  if ! grep -q "Verdict: fail" ".factory/runs/${RUN_ID}/reviews/review-tests.md" 2>/dev/null; then
    printf '    FAIL: failing rerun review artifact should be copied back to the source run\n'
    RESULT=1
  fi
  if ! printf '%s' "$OUTPUT" | grep -qi "review"; then
    printf '    FAIL: output should mention reviewer failure, got: %s\n' "$OUTPUT"
    RESULT=1
  fi

  cleanup_test_project
  return $RESULT
}

test_factory_config_defines_format_check() {
  RESULT=0
  CHECK_HOOK="${PROJECT_DIR}/.factory/hooks/check-pre-merge"
  FIX_HOOK="${PROJECT_DIR}/.factory/hooks/fix-pre-merge"

  if [ ! -x "$CHECK_HOOK" ]; then
    printf '    FAIL: %s should exist and be executable\n' "$CHECK_HOOK"
    return 1
  fi
  if [ ! -x "$FIX_HOOK" ]; then
    printf '    FAIL: %s should exist and be executable\n' "$FIX_HOOK"
    return 1
  fi
  if ! grep -q 'cargo fmt --all -- --check' "$CHECK_HOOK"; then
    printf '    FAIL: check-pre-merge hook should run cargo fmt --all -- --check\n'
    RESULT=1
  fi
  if ! grep -q 'cargo fmt --all' "$FIX_HOOK"; then
    printf '    FAIL: fix-pre-merge hook should run cargo fmt --all\n'
    RESULT=1
  fi

  return $RESULT
}

# -------------------------------------------------------------------------
# Run all tests
# -------------------------------------------------------------------------

printf 'test-run-merge\n\n'

run_test "merge rejects non-complete run" test_merge_rejects_non_complete_status
run_test "merge rejects fail review verdict" test_merge_rejects_failed_review
run_test "merge rejects uncertain review verdict" test_merge_rejects_uncertain_review
run_test "merge copies artifacts from worktree" test_merge_copies_artifacts
run_test "merge removes worktree" test_merge_removes_worktree
run_test "merge deletes run branch" test_merge_deletes_branch
run_test "merge merges run commits into main" test_merge_merges_to_main
run_test "merge fails on rebase conflict" test_merge_fails_on_rebase_conflict
run_test "merge allows run with no reviews" test_merge_allows_no_reviews
run_test "merge without check config does not require formatter" test_merge_without_check_config_does_not_require_formatter
run_test "merge runs blocking check in worktree" test_merge_runs_blocking_check_in_worktree
run_test "merge failed check keeps worktree and reports details" test_merge_failed_check_keeps_worktree_and_reports_details
run_test "merge autofix commits, reruns checks, and reruns reviewers" test_merge_autofix_commits_reruns_checks_and_reviewers
run_test "merge autofix requires clean worktree" test_merge_autofix_requires_clean_worktree
run_test "merge autofix command failure keeps worktree" test_merge_autofix_command_failure_keeps_worktree
run_test "merge autofix rerun failure keeps worktree" test_merge_autofix_rerun_failure_keeps_worktree
run_test "merge autofix reviewer failure keeps worktree" test_merge_autofix_review_failure_keeps_worktree
run_test "this repo defines a pre-merge format check" test_factory_config_defines_format_check
run_test "shell: merge rejects non-complete run (exit code)" test_shell_merge_rejects_non_complete_status
run_test "shell: merge full workflow" test_shell_merge_full_workflow

summarize_and_exit
