#!/usr/bin/env bash
# test-work-attempt-loop - Verify Attempt loop orchestration from the CLI.

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
  TEST_DIR="$(mktemp -d -t factory-work-attempt-loop-XXXXXX)"
  mkdir -p "$TEST_DIR/project" "$TEST_DIR/bin"
  cd "$TEST_DIR/project"
  git init -b main > /dev/null 2>&1
  git config commit.gpgsign false
  git config user.email "test@test"
  git config user.name "test"
  printf 'test\n' > README.md
  git add README.md && git commit -m "init" > /dev/null 2>&1
  "$FACTORY_BIN" work create work-1 --title "Attempt loop" > /dev/null
  "$FACTORY_BIN" work attempt work-1 attempt-1 > /dev/null
}

cleanup_test_project() {
  cd /
  if [ -d "${TEST_DIR}/project/.git" ]; then
    git -C "${TEST_DIR}/project" worktree list --porcelain 2>/dev/null | \
      awk '/^worktree / { print $2 }' | \
      grep -v "^${TEST_DIR}/project$" | while read -r wt; do
        git -C "${TEST_DIR}/project" worktree remove --force "$wt" 2>/dev/null || true
      done || true
  fi
  rm -rf "$TEST_DIR"
}

write_mock_claude() {
  local verdict="$1"
  local write_mode="${2:-commit}"
  local write_count_file="${TEST_DIR}/write-count"
  printf '0\n' > "$write_count_file"
  cat > "${TEST_DIR}/bin/claude" <<MOCK_SCRIPT
#!/usr/bin/env bash
case "\$PWD" in
  */work-6-work-1-attempt-1)
    if [ "${write_mode}" = "fail" ]; then
      printf 'partial loop output\n' > partial-loop-output.txt
      exit 9
    fi
    count="\$(cat "${write_count_file}")"
    count="\$((count + 1))"
    printf '%s\n' "\$count" > "${write_count_file}"
    printf 'loop output %s\n' "\$count" > "loop-output-\$count.txt"
    git add "loop-output-\$count.txt"
    git commit -m "Add loop output \$count" > /dev/null 2>&1
    ;;
  *)
    if [ "${verdict}" = "mixed-missing" ]; then
      case "\$PWD" in
        */attempt-1-review-documentation)
          printf 'Verdict: fail\n\nDocumentation review failed.\n' > review.md
          ;;
        */attempt-1-review-tests)
          printf 'Loop review without a verdict.\n' > review.md
          ;;
        *)
          printf 'Verdict: pass\n\nLoop review passed.\n' > review.md
          ;;
      esac
    elif [ "${verdict}" = "missing" ]; then
      printf 'Loop review without a verdict.\n' > review.md
    else
      printf 'Verdict: ${verdict}\n\nLoop review.\n' > review.md
    fi
    ;;
esac
exit 0
MOCK_SCRIPT
  chmod +x "${TEST_DIR}/bin/claude"
}

json_value() {
  "$FACTORY_BIN" work show work-1 | jq -r "$1"
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

run_attempt_loop() {
  PATH="${TEST_DIR}/bin:$PATH" "$FACTORY_BIN" work attempt run work-1 attempt-1 --no-sandbox
}

run_write_task() {
  PATH="${TEST_DIR}/bin:$PATH" "$FACTORY_BIN" work task run \
    work-1 attempt-1 attempt-1-write --no-sandbox
}

test_attempt_loop_passes_review_round() {
  setup_test_project
  trap cleanup_test_project RETURN
  write_mock_claude pass
  MAIN_BEFORE="$(git rev-parse main)"

  run_attempt_loop > "$TEST_DIR/stdout"

  grep -q 'Merge Candidate attempt-1-merge-candidate is ready' "$TEST_DIR/stdout" || return 1
  [ "$(json_value '.attempts[0].status')" = "complete" ] || return 1
  [ "$(json_value '.attempts[0].review_state')" = "passed" ] || return 1
  [ "$(json_value '.merge_candidates | length')" = "1" ] || return 1
  [ "$(json_value '.merge_candidates[0].id')" = "attempt-1-merge-candidate" ] || return 1
  [ "$(json_value '.merge_candidates[0].source_workspace.path')" = "../work-6-work-1-attempt-1" ] || return 1
  [ "$(json_value '.merge_candidates[0].target_workspace.id')" = "target" ] || return 1
  [ "$(json_value '.merge_candidates[0].target_workspace.path')" = "." ] || return 1
  [ "$(json_value '.merge_candidates[0].source_branch')" = "main" ] || return 1
  [ "$(json_value '.merge_candidates[0].target_branch')" = "main" ] || return 1
  [ "$(json_value '.merge_candidates[0].review_state')" = "pending" ] || return 1
  [ "$(json_value '.merge_candidates[0].candidate_commit')" = "$(git -C ../work-6-work-1-attempt-1 rev-parse HEAD)" ] || return 1
  "$FACTORY_BIN" work merge-candidate work-1 attempt-1-merge-candidate > "$TEST_DIR/candidate"
  [ "$(jq -r '.candidate_commit' "$TEST_DIR/candidate")" = "$(json_value '.merge_candidates[0].candidate_commit')" ] || return 1
  [ "$(jq -r '.target_workspace.path' "$TEST_DIR/candidate")" = "." ] || return 1
  [ "$(jq -r '.source_branch' "$TEST_DIR/candidate")" = "main" ] || return 1
  [ "$(jq -r '.target_branch' "$TEST_DIR/candidate")" = "main" ] || return 1
  [ "$(git rev-parse main)" = "$MAIN_BEFORE" ] || return 1
}

test_work_show_includes_merge_candidate() {
  setup_test_project
  trap cleanup_test_project RETURN
  write_mock_claude pass

  run_attempt_loop > "$TEST_DIR/stdout"
  "$FACTORY_BIN" work show work-1 > "$TEST_DIR/show"

  [ "$(jq -r '.merge_candidates | length' "$TEST_DIR/show")" = "1" ] || return 1
  [ "$(jq -r '.merge_candidates[0].id' "$TEST_DIR/show")" = "attempt-1-merge-candidate" ] || return 1
  [ "$(jq -r '.merge_candidates[0].candidate_commit' "$TEST_DIR/show")" = "$(json_value '.merge_candidates[0].candidate_commit')" ] || return 1
}

test_work_merge_candidate_prints_pretty_json() {
  setup_test_project
  trap cleanup_test_project RETURN
  write_mock_claude pass

  run_attempt_loop > "$TEST_DIR/stdout"
  "$FACTORY_BIN" work merge-candidate work-1 attempt-1-merge-candidate > "$TEST_DIR/candidate"

  jq -e '.id == "attempt-1-merge-candidate"' "$TEST_DIR/candidate" > /dev/null || return 1
  grep -q '^{' "$TEST_DIR/candidate" || return 1
  grep -q '^  "id": "attempt-1-merge-candidate"' "$TEST_DIR/candidate" || return 1
}

test_work_merge_candidate_missing_requests_leave_state_unchanged() {
  setup_test_project
  trap cleanup_test_project RETURN
  write_mock_claude pass

  run_attempt_loop > "$TEST_DIR/stdout"
  BEFORE="$(cat .factory/work/items/work-1.json)"

  RESULT=0
  assert_fails "$FACTORY_BIN" work merge-candidate missing-work attempt-1-merge-candidate || RESULT=1
  assert_fails "$FACTORY_BIN" work merge-candidate work-1 missing-candidate || RESULT=1
  [ "$(cat .factory/work/items/work-1.json)" = "$BEFORE" ] || RESULT=1
  return $RESULT
}

test_attempt_loop_runs_planned_review_tasks() {
  setup_test_project
  trap cleanup_test_project RETURN
  write_mock_claude pass

  run_write_task > "$TEST_DIR/write-stdout" 2> "$TEST_DIR/write-stderr"
  "$FACTORY_BIN" work review work-1 attempt-1 > "$TEST_DIR/review-stdout"

  RESULT=0
  [ "$(json_value '[.attempts[0].tasks[] | select(.kind == "review" and (.status // "planned") == "planned")] | length')" = "5" ] || RESULT=1
  run_attempt_loop > "$TEST_DIR/stdout" || RESULT=1
  assert_contains "$(cat "$TEST_DIR/stdout")" "Completed Task attempt-1-review-tests" || RESULT=1
  [ "$(json_value '[.attempts[0].tasks[] | select(.kind == "review" and .status == "complete")] | length')" = "5" ] || RESULT=1
  [ "$(json_value '.attempts[0].review_state')" = "passed" ] || RESULT=1
  [ "$(json_value '.merge_candidates[0].id')" = "attempt-1-merge-candidate" ] || RESULT=1
  return $RESULT
}

test_attempt_loop_plans_followup() {
  setup_test_project
  trap cleanup_test_project RETURN
  write_mock_claude fail

  RESULT=0
  run_attempt_loop > "$TEST_DIR/stdout"

  grep -q 'Planned follow-up write Task attempt-1-followup-1' "$TEST_DIR/stdout" || RESULT=1
  [ "$(json_value '.attempts[0].status')" = "planned" ] || RESULT=1
  [ "$(json_value '.attempts[0].review_state')" = "failed" ] || RESULT=1
  [ "$(json_value '.attempts[0].tasks[-1].input_artifacts | length')" = "5" ] || RESULT=1

  FOLLOWUP_COMMIT_BEFORE="$(git -C ../work-6-work-1-attempt-1 rev-parse HEAD)"
  run_attempt_loop > "$TEST_DIR/followup-stdout" || RESULT=1
  FOLLOWUP_COMMIT_AFTER="$(git -C ../work-6-work-1-attempt-1 rev-parse HEAD)"
  [ "$FOLLOWUP_COMMIT_AFTER" != "$FOLLOWUP_COMMIT_BEFORE" ] || RESULT=1
  assert_contains "$(cat "$TEST_DIR/followup-stdout")" "Completed Task attempt-1-followup-1" || RESULT=1
  assert_contains "$(cat "$TEST_DIR/followup-stdout")" "Planned 5 review Tasks for Attempt attempt-1" || RESULT=1
  [ "$(json_value '[.attempts[0].tasks[] | select(.kind == "review" and (.id | startswith("attempt-1-review-2-")))] | length')" = "5" ] || RESULT=1
  [ "$(json_value '.attempts[0].tasks[] | select(.id == "attempt-1-review-2-tests") | .review_context.candidate_commit')" = "$FOLLOWUP_COMMIT_AFTER" ] || RESULT=1
  [ "$(json_value '.attempts[0].tasks[] | select(.id == "attempt-1-review-2-tests") | .review_context.candidate_workspace_path')" = "../work-6-work-1-attempt-1" ] || RESULT=1
  return $RESULT
}

test_attempt_loop_plans_followup_with_mixed_failed_and_missing_reviews() {
  setup_test_project
  trap cleanup_test_project RETURN
  write_mock_claude mixed-missing

  RESULT=0
  run_attempt_loop > "$TEST_DIR/stdout" || RESULT=1
  assert_contains "$(cat "$TEST_DIR/stdout")" "Planned follow-up write Task attempt-1-followup-1" || RESULT=1
  [ "$(json_value '.attempts[0].status')" = "planned" ] || RESULT=1
  [ "$(json_value '.attempts[0].review_state')" = "failed" ] || RESULT=1
  [ "$(json_value '.attempts[0].tasks[-1].input_artifacts | length')" = "1" ] || RESULT=1
  [ "$(json_value '.attempts[0].tasks[-1].input_artifacts[0].path')" = ".factory/work/artifacts/work-1/attempt-1/attempt-1-review-documentation/review.md" ] || RESULT=1
  [ ! -f .factory/work/artifacts/work-1/attempt-1/needs-user.md ] || RESULT=1

  run_attempt_loop > "$TEST_DIR/followup-stdout" || RESULT=1
  assert_contains "$(cat "$TEST_DIR/followup-stdout")" "Completed Task attempt-1-followup-1" || RESULT=1
  assert_contains "$(cat "$TEST_DIR/followup-stdout")" "Planned 1 review Tasks for Attempt attempt-1" || RESULT=1
  assert_contains "$(cat "$TEST_DIR/followup-stdout")" "attempt-1-review-2-documentation" || RESULT=1
  [ "$(json_value '[.attempts[0].tasks[] | select(.kind == "review" and (.id | startswith("attempt-1-review-2-")))] | length')" = "1" ] || RESULT=1
  [ "$(json_value '.attempts[0].tasks[] | select(.id == "attempt-1-review-2-documentation") | .role')" = "documentation" ] || RESULT=1
  return $RESULT
}

test_attempt_loop_falls_back_when_followup_inputs_are_missing() {
  setup_test_project
  trap cleanup_test_project RETURN
  write_mock_claude mixed-missing

  RESULT=0
  run_attempt_loop > "$TEST_DIR/stdout" || RESULT=1
  assert_contains "$(cat "$TEST_DIR/stdout")" "Planned follow-up write Task attempt-1-followup-1" || RESULT=1

  jq '.input_artifacts = []' \
    .factory/work/tasks/work-1/attempt-1/attempt-1-followup-1.json \
    > "$TEST_DIR/followup-task.json" || RESULT=1
  mv "$TEST_DIR/followup-task.json" \
    .factory/work/tasks/work-1/attempt-1/attempt-1-followup-1.json || RESULT=1
  [ "$(json_value '.attempts[0].tasks[-1].input_artifacts | length')" = "0" ] || RESULT=1

  run_attempt_loop > "$TEST_DIR/followup-stdout" || RESULT=1
  assert_contains "$(cat "$TEST_DIR/followup-stdout")" "Completed Task attempt-1-followup-1" || RESULT=1
  assert_contains "$(cat "$TEST_DIR/followup-stdout")" "Planned 5 review Tasks for Attempt attempt-1" || RESULT=1
  [ "$(json_value '[.attempts[0].tasks[] | select(.kind == "review" and (.id | startswith("attempt-1-review-2-")))] | length')" = "5" ] || RESULT=1
  [ "$(json_value '.attempts[0].tasks[] | select(.id == "attempt-1-review-2-documentation") | .role')" = "documentation" ] || RESULT=1
  [ "$(json_value '.attempts[0].tasks[] | select(.id == "attempt-1-review-2-tests") | .role')" = "tests" ] || RESULT=1
  return $RESULT
}

test_attempt_loop_marks_uncertain_reviews_needs_user() {
  setup_test_project
  trap cleanup_test_project RETURN
  write_mock_claude uncertain

  RESULT=0
  run_attempt_loop > "$TEST_DIR/stdout" || RESULT=1
  assert_contains "$(cat "$TEST_DIR/stdout")" "Attempt attempt-1 needs user input" || RESULT=1
  [ "$(json_value '.attempts[0].status')" = "needs-user" ] || RESULT=1
  [ "$(json_value '.attempts[0].review_state')" = "uncertain" ] || RESULT=1
  [ -f .factory/work/artifacts/work-1/attempt-1/needs-user.md ] || RESULT=1
  assert_contains "$(cat .factory/work/artifacts/work-1/attempt-1/needs-user.md)" "attempt-1-review-tests/review.md" || RESULT=1
  return $RESULT
}

test_attempt_loop_marks_missing_verdict_needs_user() {
  setup_test_project
  trap cleanup_test_project RETURN
  write_mock_claude missing

  RESULT=0
  run_attempt_loop > "$TEST_DIR/stdout" || RESULT=1
  [ "$(json_value '.attempts[0].status')" = "needs-user" ] || RESULT=1
  [ "$(json_value '.attempts[0].review_state')" = "uncertain" ] || RESULT=1
  [ -f .factory/work/artifacts/work-1/attempt-1/needs-user.md ] || RESULT=1
  assert_contains "$(cat .factory/work/artifacts/work-1/attempt-1/needs-user.md)" "uncertain or missing review verdicts" || RESULT=1
  return $RESULT
}

test_attempt_loop_stops_after_task_executor_failure() {
  setup_test_project
  trap cleanup_test_project RETURN
  write_mock_claude pass fail

  RESULT=0
  assert_fails run_attempt_loop || RESULT=1
  assert_contains "$(cat "$TEST_DIR/stderr")" "Coder exited with code 9" || RESULT=1
  [ "$(json_value '.attempts[0].status')" = "failed" ] || RESULT=1
  [ "$(json_value '.attempts[0].tasks[0].status')" = "failed" ] || RESULT=1
  [ "$(json_value '[.attempts[0].tasks[] | select(.kind == "review")] | length')" = "0" ] || RESULT=1
  return $RESULT
}

test_attempt_loop_invalid_or_terminal_request_leaves_state_unchanged() {
  setup_test_project
  trap cleanup_test_project RETURN
  write_mock_claude pass

  RESULT=0
  BEFORE="$(cat .factory/work/items/work-1.json)"
  assert_fails "$FACTORY_BIN" work attempt run missing-work attempt-1 --no-sandbox || RESULT=1
  assert_fails "$FACTORY_BIN" work attempt run ../escape attempt-1 --no-sandbox || RESULT=1
  assert_fails "$FACTORY_BIN" work attempt run work-1 missing-attempt --no-sandbox || RESULT=1
  assert_fails "$FACTORY_BIN" work attempt run work-1 ../escape --no-sandbox || RESULT=1
  [ "$(cat .factory/work/items/work-1.json)" = "$BEFORE" ] || RESULT=1

  run_attempt_loop > "$TEST_DIR/stdout" || RESULT=1
  TERMINAL="$(cat .factory/work/items/work-1.json)"
  run_attempt_loop > "$TEST_DIR/rerun-stdout" || RESULT=1
  assert_contains "$(cat "$TEST_DIR/rerun-stdout")" "Merge Candidate attempt-1-merge-candidate is ready" || RESULT=1
  [ "$(cat .factory/work/items/work-1.json)" = "$TERMINAL" ] || RESULT=1
  return $RESULT
}

run_test "attempt loop passes review round" test_attempt_loop_passes_review_round
run_test "work show includes Merge Candidate" test_work_show_includes_merge_candidate
run_test "work merge-candidate prints pretty JSON" test_work_merge_candidate_prints_pretty_json
run_test "work merge-candidate missing requests leave state unchanged" test_work_merge_candidate_missing_requests_leave_state_unchanged
run_test "attempt loop runs planned review Tasks" test_attempt_loop_runs_planned_review_tasks
run_test "attempt loop plans follow-up write" test_attempt_loop_plans_followup
run_test "attempt loop plans follow-up with mixed missing review" test_attempt_loop_plans_followup_with_mixed_failed_and_missing_reviews
run_test "attempt loop falls back when follow-up inputs are missing" test_attempt_loop_falls_back_when_followup_inputs_are_missing
run_test "attempt loop marks uncertain reviews needs-user" test_attempt_loop_marks_uncertain_reviews_needs_user
run_test "attempt loop marks missing verdict needs-user" test_attempt_loop_marks_missing_verdict_needs_user
run_test "attempt loop stops after Task executor failure" test_attempt_loop_stops_after_task_executor_failure
run_test "attempt loop invalid or terminal request leaves state unchanged" test_attempt_loop_invalid_or_terminal_request_leaves_state_unchanged

printf '\nResults: %d passed, %d failed\n' "$PASS" "$FAIL"
if [ "$FAIL" -ne 0 ]; then
  printf 'Failed tests:%b\n' "$ERRORS"
  exit 1
fi
