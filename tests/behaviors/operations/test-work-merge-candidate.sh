#!/usr/bin/env bash
# test-work-merge-candidate - Verify Merge Candidate execution from the CLI.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$(dirname "$(dirname "$SCRIPT_DIR")")")"
FACTORY_BIN="${FACTORY_BIN_OVERRIDE:-${PROJECT_DIR}/target/debug/factory}"

PASS=0
FAIL=0
ERRORS=""

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

setup_test_project() {
  TEST_DIR="$(mktemp -d -t factory-work-merge-candidate-XXXXXX)"
  mkdir -p "$TEST_DIR/project" "$TEST_DIR/bin"
  cd "$TEST_DIR/project"
  TEST_PROJECT_PWD="$(pwd -P)"
  git init -b main > /dev/null 2>&1
  git config commit.gpgsign false
  git config user.email "test@test"
  git config user.name "test"
  printf 'test\n' > README.md
  git add README.md && git commit -m "init" > /dev/null 2>&1
}

cleanup_test_project() {
  cd /
  if [ -d "${TEST_DIR}/project/.git" ]; then
    git -C "${TEST_DIR}/project" worktree list --porcelain 2>/dev/null | \
      awk '/^worktree / { print $2 }' | \
      grep -v "^${TEST_PROJECT_PWD}$" | while read -r wt; do
        git -C "${TEST_DIR}/project" worktree remove --force "$wt" 2>/dev/null || true
      done || true
  fi
  rm -rf "$TEST_DIR"
}

write_mock_claude() {
  cat > "${TEST_DIR}/bin/claude" <<'MOCK_SCRIPT'
#!/usr/bin/env bash
case "$PWD" in
  */work-6-work-1-attempt-1)
    printf 'merge output\n' > merge-output.txt
    git add merge-output.txt
    git commit -m "Add merge output" > /dev/null 2>&1
    ;;
  */merge/reviews/*)
    REVIEWER="$(basename "$PWD")"
    REVIEW_WORKSPACE="$(printf '%s\n' "$*" | awk -F': ' '/Candidate workspace:/ { print $2; exit }')"
    if [ -n "${MERGE_REVIEW_LOG:-}" ]; then
      printf '%s\n' "$REVIEWER" >> "$MERGE_REVIEW_LOG"
    fi
    if [ -n "${MERGE_REVIEW_TIMING_LOG:-}" ]; then
      printf 'start %s\n' "$REVIEWER" >> "$MERGE_REVIEW_TIMING_LOG"
      sleep 1
      printf 'end %s\n' "$REVIEWER" >> "$MERGE_REVIEW_TIMING_LOG"
    fi
    if [ -n "${MERGE_REVIEW_ARGS_LOG:-}" ]; then
      printf '%s\n' "$*" >> "$MERGE_REVIEW_ARGS_LOG"
    fi
    if [ "$REVIEWER" = "behaviors" ] && [ "${MERGE_MOCK_MODE:-pass}" = "fail-merge-review" ]; then
      printf 'Verdict: fail\n\nMerge behavior review failed.\n' > review.md
    elif [ "$REVIEWER" = "behaviors" ] && [ "${MERGE_MOCK_MODE:-pass}" = "missing-merge-review" ]; then
      :
    elif [ "$REVIEWER" = "behaviors" ] && [ "${MERGE_MOCK_MODE:-pass}" = "dirty-merge-review" ]; then
      printf 'Verdict: pass\n\nMerge behavior review dirtied the candidate.\n' > review.md
      printf 'dirty merge review\n' > "${REVIEW_WORKSPACE}/dirty-merge-review.txt"
    elif [ "$REVIEWER" = "behaviors" ] && [ "${MERGE_MOCK_MODE:-pass}" = "dirty-merge-review-factory" ]; then
      printf 'Verdict: pass\n\nMerge behavior review dirtied Factory state.\n' > review.md
      mkdir -p "${REVIEW_WORKSPACE}/.factory/review-scratch"
      printf 'dirty merge review\n' \
        > "${REVIEW_WORKSPACE}/.factory/review-scratch/dirty.txt"
    elif [ "$REVIEWER" = "behaviors" ]; then
      printf 'Verdict: pass\n\nMerge behavior review passed.\n' > review.md
    else
      printf 'Verdict: pass\n\nMerge review passed.\n' > review.md
    fi
    ;;
  *)
    printf 'Verdict: pass\n\nAttempt review passed.\n' > review.md
    ;;
esac

printf '{"type":"result","subtype":"success","result":"done","session_id":"mock"}\n'
MOCK_SCRIPT
  chmod +x "${TEST_DIR}/bin/claude"
}

json_value() {
  "$FACTORY_BIN" work show work-1 | jq -r "$1"
}

attempt_record_path() {
  printf '.factory/work/attempts/work-1/attempt-1.json'
}

task_record_path() {
  printf '.factory/work/tasks/work-1/attempt-1/attempt-1-write.json'
}

merge_candidate_record_path() {
  printf '.factory/work/merge-candidates/work-1/attempt-1-merge-candidate.json'
}

merge_candidate_record_value() {
  jq -r "$1" "$(merge_candidate_record_path)"
}

assert_contains() {
  if ! printf '%s' "$1" | grep -Fq -- "$2"; then
    printf '    FAIL: output does not contain "%s"\n' "$2"
    printf '    Output:\n%s\n' "$1"
    return 1
  fi
}

assert_fails() {
  if "$@" > "$TEST_DIR/stdout" 2> "$TEST_DIR/stderr"; then
    printf '    FAIL: command unexpectedly succeeded: %s\n' "$*"
    printf '    Stdout:\n%s\n' "$(cat "$TEST_DIR/stdout")"
    printf '    Stderr:\n%s\n' "$(cat "$TEST_DIR/stderr")"
    return 1
  fi
}

create_passed_merge_candidate() {
  "$FACTORY_BIN" work create work-1 --title "Merge candidate" > /dev/null
  "$FACTORY_BIN" work attempt work-1 attempt-1 > /dev/null
  PATH="${TEST_DIR}/bin:$PATH" \
    "$FACTORY_BIN" work attempt run work-1 attempt-1 --no-sandbox \
      > "$TEST_DIR/attempt-stdout" 2> "$TEST_DIR/attempt-stderr"
}

run_merge() {
  MERGE_MOCK_MODE="${1:-pass}" \
    MERGE_REVIEW_LOG="${TEST_DIR}/merge-review-log" \
    MERGE_REVIEW_ARGS_LOG="${TEST_DIR}/merge-review-args-log" \
    MERGE_REVIEW_TIMING_LOG="${MERGE_REVIEW_TIMING_LOG:-}" \
    CANDIDATE_WORKSPACE="${TEST_DIR}/work-6-work-1-attempt-1" \
    PATH="${TEST_DIR}/bin:$PATH" \
    "$FACTORY_BIN" work merge --no-sandbox work-1 attempt-1-merge-candidate
}

assert_no_merge_reviewer_worktrees() {
  if git -C "$TEST_PROJECT_PWD" worktree list --porcelain | grep -Fq "/review-worktrees/"; then
    printf '    FAIL: reviewer worktree remains registered\n'
    return 1
  fi
  if [ -d "$TEST_PROJECT_PWD/.factory/work/artifacts/work-1/attempt-1/attempt-1-merge-candidate/merge/review-worktrees" ]; then
    printf '    FAIL: reviewer worktree directory remains\n'
    return 1
  fi
}

test_work_merge_lands_after_update_checks_and_reviewers() {
  setup_test_project
  trap cleanup_test_project RETURN
  write_mock_claude
  create_passed_merge_candidate

  OLD_CANDIDATE="$(json_value '.merge_candidates[0].candidate_commit')"
  CANDIDATE_PWD="$(cd ../work-6-work-1-attempt-1 && pwd -P)"
  printf 'target update\n' > target.txt
  git add target.txt && git commit -m "Add target update" > /dev/null 2>&1
  TARGET_BEFORE="$(git rev-parse main)"

  mkdir -p .factory
  cat > .factory/config.toml <<EOF
[checks.probe]
command = "test -f merge-output.txt && test -f target.txt && printf '%s' \"\$PWD\" > '${TEST_DIR}/check-pwd'"
run_before_land = true
EOF

  RESULT=0
  run_merge pass > "$TEST_DIR/stdout" 2> "$TEST_DIR/stderr" || RESULT=1
  LANDED_COMMIT="$(json_value '.merge_candidates[0].merge_state.landed_commit')"

  [ "$(git rev-parse main)" = "$LANDED_COMMIT" ] || RESULT=1
  [ "$(git rev-parse main)" != "$OLD_CANDIDATE" ] || RESULT=1
  git merge-base --is-ancestor "$TARGET_BEFORE" main || RESULT=1
  [ "$(cat merge-output.txt)" = "merge output" ] || RESULT=1
  [ "$(cat target.txt)" = "target update" ] || RESULT=1
  [ "$(cat "$TEST_DIR/check-pwd")" = "$CANDIDATE_PWD" ] || RESULT=1
  [ "$(json_value '.merge_candidates[0].merge_state.status')" = "landed" ] || RESULT=1
  [ "$(json_value '.merge_candidates[0].review_state')" = "passed" ] || RESULT=1
  [ "$(json_value '.merge_candidates[0].merge_state.check_artifacts | length')" = "1" ] || RESULT=1
  [ "$(json_value '.merge_candidates[0].merge_state.review_artifacts | length')" = "6" ] || RESULT=1
  [ "$(wc -l < "$TEST_DIR/merge-review-log" | tr -d ' ')" = "5" ] || RESULT=1
  assert_contains "$(cat "$TEST_DIR/merge-review-args-log")" "work-1" || RESULT=1
  assert_contains "$(cat "$TEST_DIR/merge-review-args-log")" "attempt-1" || RESULT=1
  assert_contains "$(cat "$TEST_DIR/merge-review-args-log")" "attempt-1-merge-candidate" || RESULT=1
  assert_contains "$(cat "$TEST_DIR/merge-review-args-log")" "Target branch: main" || RESULT=1
  assert_contains "$(cat "$TEST_DIR/merge-review-args-log")" "review-worktrees/behaviors" || RESULT=1
  assert_contains "$(cat "$TEST_DIR/merge-review-args-log")" "Review diff: git -C '$TEST_PROJECT_PWD/.factory/work/artifacts/work-1/attempt-1/attempt-1-merge-candidate/merge/review-worktrees/behaviors' diff '$TARGET_BEFORE..$LANDED_COMMIT'" || RESULT=1
  assert_contains "$(cat "$TEST_DIR/merge-review-args-log")" ".factory/work/artifacts/work-1/attempt-1/attempt-1-merge-candidate/merge/reviews/behaviors/review.md" || RESULT=1
  assert_contains "$(cat "$TEST_DIR/merge-review-args-log")" "$TEST_PROJECT_PWD/.factory/work/artifacts/work-1/attempt-1/attempt-1-merge-candidate/merge/reviews/behaviors/review.md" || RESULT=1
  assert_contains "$(cat "$TEST_DIR/merge-review-args-log")" "candidate workspace as read-only" || RESULT=1
  if grep -Fq ".factory/runs/" "$TEST_DIR/merge-review-args-log"; then
    printf '    FAIL: merge reviewer prompt contains legacy run review path\n'
    RESULT=1
  fi
  assert_contains "$(cat "$TEST_DIR/merge-review-args-log")" "Attempt history:" || RESULT=1
  assert_contains "$(cat "$TEST_DIR/merge-review-args-log")" "Attempt attempt-1 review_state: passed" || RESULT=1
  assert_contains "$(cat "$TEST_DIR/merge-review-args-log")" "Task attempt-1-write: kind=write" || RESULT=1
  assert_contains "$(cat "$TEST_DIR/merge-review-args-log")" "Rebase/update state:" || RESULT=1
  assert_contains "$(cat "$TEST_DIR/merge-review-args-log")" "Rebased candidate workspace onto target branch main" || RESULT=1
  assert_contains "$(cat "$TEST_DIR/merge-review-args-log")" "$TARGET_BEFORE" || RESULT=1
  assert_contains "$(cat "$TEST_DIR/merge-review-args-log")" "source_workspace" || RESULT=1
  assert_contains "$(cat "$TEST_DIR/merge-review-args-log")" "candidate_commit" || RESULT=1
  assert_contains "$(cat "$TEST_DIR/merge-review-args-log")" "Merge check status:" || RESULT=1
  assert_contains "$(cat "$TEST_DIR/merge-review-args-log")" "Merge checks ran before reviewers" || RESULT=1
  if grep -Fq "Check artifacts:" "$TEST_DIR/merge-review-args-log"; then
    printf '    FAIL: merge reviewer prompt contains check artifact path list\n'
    RESULT=1
  fi
  assert_contains "$(cat "$TEST_DIR/stdout")" "Merged Merge Candidate attempt-1-merge-candidate" || RESULT=1
  "$FACTORY_BIN" work merge-candidate work-1 attempt-1-merge-candidate \
    > "$TEST_DIR/landed-candidate" 2> "$TEST_DIR/stderr" || RESULT=1
  [ "$(jq -r '.merge_state.status' "$TEST_DIR/landed-candidate")" = "landed" ] || RESULT=1
  [ "$(jq -r '.merge_state.landed_commit' "$TEST_DIR/landed-candidate")" = "$LANDED_COMMIT" ] || RESULT=1
  if git worktree list --porcelain | grep -Fq "../work-6-work-1-attempt-1"; then
    printf '    FAIL: managed candidate workspace remains registered after merge\n'
    RESULT=1
  fi
  assert_no_merge_reviewer_worktrees || RESULT=1
  return $RESULT
}

test_work_merge_reviewers_run_in_parallel() {
  setup_test_project
  trap cleanup_test_project RETURN
  write_mock_claude
  create_passed_merge_candidate
  export MERGE_REVIEW_TIMING_LOG="${TEST_DIR}/merge-review-timing-log"

  RESULT=0
  run_merge pass > "$TEST_DIR/stdout" 2> "$TEST_DIR/stderr" || RESULT=1
  STARTS_BEFORE_FIRST_END="$(awk '
    /^end / { print starts; exit }
    /^start / { starts++ }
  ' "$MERGE_REVIEW_TIMING_LOG")"
  [ "${STARTS_BEFORE_FIRST_END:-0}" -gt 1 ] || RESULT=1
  assert_no_merge_reviewer_worktrees || RESULT=1
  unset MERGE_REVIEW_TIMING_LOG
  return $RESULT
}

test_work_merge_rejects_missing_work_item() {
  setup_test_project
  trap cleanup_test_project RETURN
  write_mock_claude
  create_passed_merge_candidate

  MAIN_BEFORE="$(git rev-parse main)"

  RESULT=0
  if "$FACTORY_BIN" work merge --no-sandbox missing-work attempt-1-merge-candidate \
    > "$TEST_DIR/stdout" 2> "$TEST_DIR/stderr"; then
    printf '    FAIL: command unexpectedly succeeded for missing Work Item\n'
    RESULT=1
  fi
  [ "$(git rev-parse main)" = "$MAIN_BEFORE" ] || RESULT=1
  assert_contains "$(cat "$TEST_DIR/stderr")" "missing-work" || RESULT=1
  return $RESULT
}

test_work_merge_rejects_missing_merge_candidate() {
  setup_test_project
  trap cleanup_test_project RETURN
  write_mock_claude
  create_passed_merge_candidate

  MAIN_BEFORE="$(git rev-parse main)"
  STATE_BEFORE="$("$FACTORY_BIN" work show work-1)"

  RESULT=0
  if "$FACTORY_BIN" work merge --no-sandbox work-1 missing-candidate \
    > "$TEST_DIR/stdout" 2> "$TEST_DIR/stderr"; then
    printf '    FAIL: command unexpectedly succeeded for missing Merge Candidate\n'
    RESULT=1
  fi
  [ "$(git rev-parse main)" = "$MAIN_BEFORE" ] || RESULT=1
  [ "$("$FACTORY_BIN" work show work-1)" = "$STATE_BEFORE" ] || RESULT=1
  assert_contains "$(cat "$TEST_DIR/stderr")" "missing-candidate" || RESULT=1
  return $RESULT
}

test_work_merge_rejects_candidate_without_passed_attempt() {
  setup_test_project
  trap cleanup_test_project RETURN
  write_mock_claude
  create_passed_merge_candidate

  MAIN_BEFORE="$(git rev-parse main)"
  jq '.review_state = "failed"' "$(attempt_record_path)" > "$TEST_DIR/attempt.json"
  mv "$TEST_DIR/attempt.json" "$(attempt_record_path)"

  RESULT=0
  assert_fails run_merge pass || RESULT=1
  [ "$(git rev-parse main)" = "$MAIN_BEFORE" ] || RESULT=1
  [ "$(json_value '.merge_candidates[0].merge_state.status')" = "failed" ] || RESULT=1
  assert_contains "$(json_value '.merge_candidates[0].merge_state.failure_reason')" "before reviews passed" || RESULT=1
  assert_contains "$(cat "$TEST_DIR/stderr")" "before reviews passed" || RESULT=1
  return $RESULT
}

test_work_merge_rejects_stored_source_branch_mismatch() {
  setup_test_project
  trap cleanup_test_project RETURN
  write_mock_claude
  create_passed_merge_candidate
  MAIN_BEFORE="$(git rev-parse main)"

  jq '.source_branch = "unexpected-source"' \
    "$(merge_candidate_record_path)" > "$TEST_DIR/candidate.json"
  mv "$TEST_DIR/candidate.json" "$(merge_candidate_record_path)"

  RESULT=0
  assert_fails run_merge pass || RESULT=1
  [ "$(git rev-parse main)" = "$MAIN_BEFORE" ] || RESULT=1
  [ "$(merge_candidate_record_value '.merge_state.status')" = "pending" ] || RESULT=1
  assert_contains "$(cat "$TEST_DIR/stderr")" "source_branch" || RESULT=1
  return $RESULT
}

test_work_merge_rejects_source_workspace_mismatch() {
  setup_test_project
  trap cleanup_test_project RETURN
  write_mock_claude
  create_passed_merge_candidate
  MAIN_BEFORE="$(git rev-parse main)"

  jq '.source_workspace.path = "/tmp/not-factory-source"' \
    "$(merge_candidate_record_path)" > "$TEST_DIR/candidate.json"
  mv "$TEST_DIR/candidate.json" "$(merge_candidate_record_path)"

  RESULT=0
  assert_fails run_merge pass || RESULT=1
  [ "$(git rev-parse main)" = "$MAIN_BEFORE" ] || RESULT=1
  [ "$(merge_candidate_record_value '.merge_state.status')" = "pending" ] || RESULT=1
  assert_contains "$(cat "$TEST_DIR/stderr")" "source_workspace.path" || RESULT=1
  return $RESULT
}

test_work_merge_rejects_wrong_managed_source_workspace() {
  setup_test_project
  trap cleanup_test_project RETURN
  write_mock_claude
  create_passed_merge_candidate
  MAIN_BEFORE="$(git rev-parse main)"

  jq '.source_workspace.path = "../work-6-work-1-other-attempt"' \
    "$(merge_candidate_record_path)" > "$TEST_DIR/candidate.json"
  mv "$TEST_DIR/candidate.json" "$(merge_candidate_record_path)"
  jq '.output.workspace_path = "../work-6-work-1-other-attempt"' \
    "$(task_record_path)" > "$TEST_DIR/task.json"
  mv "$TEST_DIR/task.json" "$(task_record_path)"

  RESULT=0
  assert_fails run_merge pass || RESULT=1
  [ "$(git rev-parse main)" = "$MAIN_BEFORE" ] || RESULT=1
  [ "$(merge_candidate_record_value '.merge_state.status')" = "pending" ] || RESULT=1
  assert_contains "$(cat "$TEST_DIR/stderr")" "source workspace path" || RESULT=1
  assert_contains "$(cat "$TEST_DIR/stderr")" "../work-6-work-1-attempt-1" || RESULT=1
  assert_contains "$(cat "$TEST_DIR/stderr")" "../work-6-work-1-other-attempt" || RESULT=1
  return $RESULT
}

test_work_merge_rejects_target_workspace_mismatch() {
  setup_test_project
  trap cleanup_test_project RETURN
  write_mock_claude
  create_passed_merge_candidate
  MAIN_BEFORE="$(git rev-parse main)"

  jq '.target_workspace.path = "/tmp/not-factory-target"' \
    "$(merge_candidate_record_path)" > "$TEST_DIR/candidate.json"
  mv "$TEST_DIR/candidate.json" "$(merge_candidate_record_path)"

  RESULT=0
  assert_fails run_merge pass || RESULT=1
  [ "$(git rev-parse main)" = "$MAIN_BEFORE" ] || RESULT=1
  [ "$(merge_candidate_record_value '.merge_state.status')" = "pending" ] || RESULT=1
  assert_contains "$(cat "$TEST_DIR/stderr")" "target_workspace.path" || RESULT=1
  return $RESULT
}

test_work_merge_failed_check_leaves_target_unchanged() {
  setup_test_project
  trap cleanup_test_project RETURN
  write_mock_claude
  create_passed_merge_candidate
  MAIN_BEFORE="$(git rev-parse main)"

  mkdir -p .factory
  cat > .factory/config.toml <<EOF
[checks.quality]
command = "printf check-ran > '${TEST_DIR}/check-marker'; printf failing-check-output; exit 3"
run_before_land = true
EOF

  RESULT=0
  assert_fails run_merge pass || RESULT=1
  [ "$(git rev-parse main)" = "$MAIN_BEFORE" ] || RESULT=1
  [ "$(cat "$TEST_DIR/check-marker")" = "check-ran" ] || RESULT=1
  [ "$(json_value '.merge_candidates[0].merge_state.status')" = "failed" ] || RESULT=1
  [ "$(json_value '.merge_candidates[0].review_state')" = "pending" ] || RESULT=1
  assert_contains "$(json_value '.merge_candidates[0].merge_state.failure_reason')" "Pre-land check 'quality' failed" || RESULT=1
  [ "$(json_value '.merge_candidates[0].merge_state.check_artifacts | length')" = "1" ] || RESULT=1
  assert_contains "$(cat "$TEST_DIR/stderr")" "failing-check-output" || RESULT=1
  return $RESULT
}

test_work_merge_failed_reviewer_leaves_target_unchanged() {
  setup_test_project
  trap cleanup_test_project RETURN
  write_mock_claude
  create_passed_merge_candidate
  MAIN_BEFORE="$(git rev-parse main)"

  RESULT=0
  assert_fails run_merge fail-merge-review || RESULT=1
  [ "$(git rev-parse main)" = "$MAIN_BEFORE" ] || RESULT=1
  [ "$(json_value '.merge_candidates[0].merge_state.status')" = "failed" ] || RESULT=1
  [ "$(json_value '.merge_candidates[0].review_state')" = "failed" ] || RESULT=1
  assert_contains "$(json_value '.merge_candidates[0].merge_state.failure_reason')" "Merge-time reviewers returned failed" || RESULT=1
  [ "$(json_value '.merge_candidates[0].merge_state.review_artifacts | length')" = "6" ] || RESULT=1
  assert_contains "$(cat "$TEST_DIR/stderr")" "Merge-time reviewers did not pass" || RESULT=1
  assert_no_merge_reviewer_worktrees || RESULT=1
  return $RESULT
}

test_work_merge_missing_reviewer_artifact_leaves_target_unchanged() {
  setup_test_project
  trap cleanup_test_project RETURN
  write_mock_claude
  create_passed_merge_candidate
  MAIN_BEFORE="$(git rev-parse main)"

  RESULT=0
  assert_fails run_merge missing-merge-review || RESULT=1
  [ "$(git rev-parse main)" = "$MAIN_BEFORE" ] || RESULT=1
  [ "$(json_value '.merge_candidates[0].merge_state.status')" = "failed" ] || RESULT=1
  [ "$(json_value '.merge_candidates[0].review_state')" = "failed" ] || RESULT=1
  assert_contains "$(cat "$TEST_DIR/stderr")" "Merge-time reviewers did not pass" || RESULT=1
  grep -q "completed without writing" \
    ".factory/work/artifacts/work-1/attempt-1/attempt-1-merge-candidate/merge/reviews/behaviors/review.md" || RESULT=1
  assert_no_merge_reviewer_worktrees || RESULT=1
  return $RESULT
}

test_work_merge_dirty_reviewer_leaves_target_unchanged() {
  setup_test_project
  trap cleanup_test_project RETURN
  write_mock_claude
  create_passed_merge_candidate
  MAIN_BEFORE="$(git rev-parse main)"
  CANDIDATE_WORKSPACE="${TEST_DIR}/work-6-work-1-attempt-1"

  RESULT=0
  assert_fails run_merge dirty-merge-review || RESULT=1
  [ "$(git rev-parse main)" = "$MAIN_BEFORE" ] || RESULT=1
  [ "$(json_value '.merge_candidates[0].merge_state.status')" = "failed" ] || RESULT=1
  [ "$(json_value '.merge_candidates[0].review_state')" = "failed" ] || RESULT=1
  assert_contains "$(json_value '.merge_candidates[0].merge_state.failure_reason')" "Merge-time reviewer behaviors dirtied candidate workspace" || RESULT=1
  assert_contains "$(json_value '.merge_candidates[0].merge_state.failure_reason')" "review-worktrees/behaviors" || RESULT=1
  [ "$(json_value '.merge_candidates[0].merge_state.review_artifacts | length')" = "6" ] || RESULT=1
  assert_contains "$(cat "$TEST_DIR/stderr")" "Merge-time reviewer behaviors dirtied candidate workspace" || RESULT=1
  test ! -f "$CANDIDATE_WORKSPACE/dirty-merge-review.txt" || RESULT=1
  assert_no_merge_reviewer_worktrees || RESULT=1
  return $RESULT
}

test_work_merge_dirty_factory_state_reviewer_leaves_target_unchanged() {
  setup_test_project
  trap cleanup_test_project RETURN
  write_mock_claude
  create_passed_merge_candidate
  MAIN_BEFORE="$(git rev-parse main)"
  CANDIDATE_WORKSPACE="${TEST_DIR}/work-6-work-1-attempt-1"

  RESULT=0
  assert_fails run_merge dirty-merge-review-factory || RESULT=1
  [ "$(git rev-parse main)" = "$MAIN_BEFORE" ] || RESULT=1
  [ "$(json_value '.merge_candidates[0].merge_state.status')" = "failed" ] || RESULT=1
  [ "$(json_value '.merge_candidates[0].review_state')" = "failed" ] || RESULT=1
  assert_contains "$(json_value '.merge_candidates[0].merge_state.failure_reason')" "Merge-time reviewer behaviors dirtied candidate workspace" || RESULT=1
  assert_contains "$(json_value '.merge_candidates[0].merge_state.failure_reason')" ".factory/review-scratch/dirty.txt" || RESULT=1
  assert_contains "$(cat "$TEST_DIR/stderr")" "Dirty ignored or Factory files" || RESULT=1
  test ! -f "$CANDIDATE_WORKSPACE/.factory/review-scratch/dirty.txt" || RESULT=1
  assert_no_merge_reviewer_worktrees || RESULT=1
  return $RESULT
}

test_work_merge_rebase_failure_leaves_target_unchanged() {
  setup_test_project
  trap cleanup_test_project RETURN
  write_mock_claude
  create_passed_merge_candidate

  printf 'target version\n' > merge-output.txt
  git add merge-output.txt
  git commit -m "Add conflicting target output" > /dev/null 2>&1
  MAIN_BEFORE="$(git rev-parse main)"

  RESULT=0
  assert_fails run_merge pass || RESULT=1
  [ "$(git rev-parse main)" = "$MAIN_BEFORE" ] || RESULT=1
  [ "$(cat merge-output.txt)" = "target version" ] || RESULT=1
  [ "$(json_value '.merge_candidates[0].merge_state.status')" = "failed" ] || RESULT=1
  [ "$(json_value '.merge_candidates[0].review_state')" = "pending" ] || RESULT=1
  assert_contains "$(json_value '.merge_candidates[0].merge_state.failure_reason')" "rebase" || RESULT=1
  assert_contains "$(cat "$TEST_DIR/stderr")" "rebase" || RESULT=1
  if git -C ../work-6-work-1-attempt-1 status 2>&1 | grep -qi "rebase in progress"; then
    printf '    FAIL: rebase remains in progress after merge failure\n'
    RESULT=1
  fi
  return $RESULT
}

test_work_merge_candidate_inspection_read_only() {
  setup_test_project
  trap cleanup_test_project RETURN
  write_mock_claude
  create_passed_merge_candidate
  MAIN_BEFORE="$(git rev-parse main)"
  STATE_BEFORE="$("$FACTORY_BIN" work show work-1)"

  RESULT=0
  "$FACTORY_BIN" work merge-candidate work-1 attempt-1-merge-candidate \
    > "$TEST_DIR/candidate" 2> "$TEST_DIR/stderr" || RESULT=1
  jq -e '.id == "attempt-1-merge-candidate"' "$TEST_DIR/candidate" > /dev/null || RESULT=1
  [ "$(git rev-parse main)" = "$MAIN_BEFORE" ] || RESULT=1
  [ "$("$FACTORY_BIN" work show work-1)" = "$STATE_BEFORE" ] || RESULT=1
  [ "$(json_value '.merge_candidates[0].merge_state.status')" = "pending" ] || RESULT=1
  return $RESULT
}

printf 'test-work-merge-candidate\n\n'

run_test "work merge lands after update, checks, and reviewers" \
  test_work_merge_lands_after_update_checks_and_reviewers
run_test "work merge reviewers run in parallel" \
  test_work_merge_reviewers_run_in_parallel
run_test "work merge rejects missing Work Item" \
  test_work_merge_rejects_missing_work_item
run_test "work merge rejects missing Merge Candidate" \
  test_work_merge_rejects_missing_merge_candidate
run_test "work merge rejects candidate without passed Attempt" \
  test_work_merge_rejects_candidate_without_passed_attempt
run_test "work merge rejects source branch mismatch" \
  test_work_merge_rejects_stored_source_branch_mismatch
run_test "work merge rejects source workspace mismatch" \
  test_work_merge_rejects_source_workspace_mismatch
run_test "work merge rejects wrong managed source workspace" \
  test_work_merge_rejects_wrong_managed_source_workspace
run_test "work merge rejects target workspace mismatch" \
  test_work_merge_rejects_target_workspace_mismatch
run_test "work merge failed check leaves target unchanged" \
  test_work_merge_failed_check_leaves_target_unchanged
run_test "work merge failed reviewer leaves target unchanged" \
  test_work_merge_failed_reviewer_leaves_target_unchanged
run_test "work merge missing reviewer artifact leaves target unchanged" \
  test_work_merge_missing_reviewer_artifact_leaves_target_unchanged
run_test "work merge dirty reviewer leaves target unchanged" \
  test_work_merge_dirty_reviewer_leaves_target_unchanged
run_test "work merge dirty Factory state reviewer leaves target unchanged" \
  test_work_merge_dirty_factory_state_reviewer_leaves_target_unchanged
run_test "work merge rebase failure leaves target unchanged" \
  test_work_merge_rebase_failure_leaves_target_unchanged
run_test "work merge-candidate inspection is read-only" \
  test_work_merge_candidate_inspection_read_only

printf '\nResults: %d passed, %d failed\n' "$PASS" "$FAIL"
if [ "$FAIL" -ne 0 ]; then
  printf 'Failed tests:%b\n' "$ERRORS"
  exit 1
fi
