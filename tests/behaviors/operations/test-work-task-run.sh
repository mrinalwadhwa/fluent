#!/usr/bin/env bash
# test-work-task-run - Verify write Task execution from the CLI.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$(dirname "$(dirname "$SCRIPT_DIR")")")"
FLUENT_BIN="${FLUENT_BIN_OVERRIDE:-${PROJECT_DIR}/target/debug/fluent}"

source "${PROJECT_DIR}/tests/lib/run_test.sh"
source "${PROJECT_DIR}/tests/lib/work_test_fixtures.sh"
LOG_DIR="${PROJECT_DIR}/tests/output/$(basename "$0" .sh)"

setup_test_project() {
  TEST_DIR="$(mktemp -d -t fluent-work-task-run-XXXXXX)"
  mkdir -p "$TEST_DIR/project" "$TEST_DIR/bin"
  cd "$TEST_DIR/project"
  git init -b main > /dev/null 2>&1
  git config commit.gpgsign false
  git config user.email "test@test"
  git config user.name "test"
  printf 'test\n' > README.md
  seed_review_skill_stubs "."
  git add . && git commit -m "init" > /dev/null 2>&1
  MOCK_BIN="$TEST_DIR/bin"
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

create_work_task() {
  "$FLUENT_BIN" work-item create work-1 --title "Task execution" > /dev/null
  "$FLUENT_BIN" attempt create work-1 attempt-1 > /dev/null
}

json_value() {
  "$FLUENT_BIN" work-item show work-1 | jq -r "$1"
}

show_json_value() {
  "$FLUENT_BIN" work-item show work-1 | jq -r "$1"
}

workspace_path() {
  printf '%s\n' "../work-6-work-1-attempt-1"
}

physical_workspace_path() {
  cd "$(workspace_path)" && pwd -P
}

write_mock_codex() {
  cat > "${MOCK_BIN}/codex" <<'MOCK_SCRIPT'
#!/usr/bin/env bash
printf '%s\n' "$PWD" >> "$CODER_CWD_LOG"
printf '%s\n' "$*" >> "$CODER_ARGS_LOG"
last_arg=""
for arg in "$@"; do
  last_arg="$arg"
done
printf '%s\n' "$last_arg" >> "$CODER_PROMPT_LOG"

case "${TASK_RUN_MOCK_MODE:-commit}" in
  commit)
    printf 'task output\n' > task-output.txt
    git add task-output.txt
    git commit -m "Add task output" > /dev/null 2>&1
    ;;
  dirty)
    printf 'uncommitted task output\n' > dirty-output.txt
    ;;
  no-commit)
    ;;
  review-fail)
    printf 'Verdict: fail\n\nReview finding.\n' > review.md
    ;;
  review-uncertain)
    printf 'Verdict: uncertain\n\nCould not verify.\n' > review.md
    ;;
  review-read-candidate)
    cat ../../../../../../../work-6-work-1-attempt-1/README.md > candidate-read.txt
    printf 'Verdict: pass\n\nRead candidate workspace.\n' > review.md
    ;;
  review-missing)
    ;;
  fail)
    printf 'partial task output\n' > partial-output.txt
    exit 7
    ;;
  *)
    printf 'unknown TASK_RUN_MOCK_MODE: %s\n' "$TASK_RUN_MOCK_MODE" >&2
    exit 2
    ;;
esac

printf '%s\n' '{"type":"result","subtype":"success","result":"done","session_id":"mock"}'
MOCK_SCRIPT
  chmod +x "${MOCK_BIN}/codex"
}

assert_contains() {
  if ! printf '%s' "$1" | grep -Fq -- "$2"; then
    printf '    FAIL: output does not contain "%s"\n' "$2"
    printf '    Output:\n%s\n' "$1"
    return 1
  fi
}

assert_not_complete() {
  local status output_commit
  local task_path=".fluent/work/tasks/work-1/attempt-1/attempt-1-write-1.json"
  if [ -f "$task_path" ]; then
    status="$(jq -r '.status // "missing"' "$task_path")"
    output_commit="$(jq -r '.output.commit // "missing"' "$task_path")"
  else
    status="$(json_value '.attempts[0].tasks[0].status // "missing"')"
    output_commit="$(json_value '.attempts[0].tasks[0].output.commit // "missing"')"
  fi
  if [ "$status" = "complete" ] || [ "$output_commit" != "missing" ]; then
    printf '    FAIL: Task completed unexpectedly\n'
    "$FLUENT_BIN" work-item show work-1 || true
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

run_task() {
  TASK_RUN_MOCK_MODE="$1" \
    PATH="${MOCK_BIN}:${PATH}" \
    CODER_CWD_LOG="${TEST_DIR}/coder-cwd.log" \
    CODER_ARGS_LOG="${TEST_DIR}/coder-args.log" \
    CODER_PROMPT_LOG="${TEST_DIR}/coder-prompt.log" \
    "$FLUENT_BIN" task run --no-sandbox --coder codex \
      work-1 attempt-1 attempt-1-write-1
}

run_review_task() {
  TASK_RUN_MOCK_MODE="$1" \
    PATH="${MOCK_BIN}:${PATH}" \
    CODER_CWD_LOG="${TEST_DIR}/coder-cwd.log" \
    CODER_ARGS_LOG="${TEST_DIR}/coder-args.log" \
    CODER_PROMPT_LOG="${TEST_DIR}/coder-prompt.log" \
    "$FLUENT_BIN" task run --no-sandbox --coder codex \
      work-1 attempt-1 attempt-1-review-tests
}

run_review_command() {
  "$FLUENT_BIN" review work-1 attempt-1
}

seed_review_fixtures() {
  seed_review_skill_stubs "$(physical_workspace_path)"
  seed_tester_results "$TEST_DIR/project" work-1 attempt-1
}

test_run_reuses_worktree_and_launches_coder_there() {
  setup_test_project
  create_work_task
  write_mock_codex

  RESULT=0
  git worktree add -b precreated-task-workspace "$(workspace_path)" HEAD > /dev/null 2>&1
  run_task commit > "$TEST_DIR/stdout" 2> "$TEST_DIR/stderr" || RESULT=1

  [ -d "$(workspace_path)/.git" ] || [ -f "$(workspace_path)/.git" ] || RESULT=1
  [ "$(tail -n 1 "$TEST_DIR/coder-cwd.log")" = "$(physical_workspace_path)" ] || RESULT=1
  assert_contains "$(cat "$TEST_DIR/coder-args.log")" "exec" || RESULT=1

  cleanup_test_project
  return $RESULT
}

test_run_passes_task_context_to_coder_prompt() {
  setup_test_project
  "$FLUENT_BIN" work-item create work-1 --title "Prompt contract" > /dev/null
  "$FLUENT_BIN" attempt create work-1 attempt-1 > /dev/null
  write_mock_codex

  RESULT=0
  run_task commit > "$TEST_DIR/stdout" 2> "$TEST_DIR/stderr" || RESULT=1

  PROMPT="$(cat "$TEST_DIR/coder-prompt.log")"
  assert_contains "$PROMPT" "Work Item: work-1 - Prompt contract" || RESULT=1
  assert_contains "$PROMPT" "Fluent Writer" || RESULT=1
  assert_contains "$PROMPT" "Phase 1" || RESULT=1
  assert_contains "$PROMPT" "Read Brief at" || RESULT=1
  assert_contains "$PROMPT" "Phase 2" || RESULT=1
  assert_contains "$PROMPT" "progress.md" || RESULT=1
  assert_contains "$PROMPT" "Phase 3" || RESULT=1
  assert_contains "$PROMPT" "Implement test-first" || RESULT=1
  assert_contains "$PROMPT" "Phase 4" || RESULT=1
  assert_contains "$PROMPT" ".fluent/tester.yaml" || RESULT=1
  assert_contains "$PROMPT" "no new commits fails automatically" || RESULT=1
  if printf '%s' "$PROMPT" | grep -Fq "Status file contract"; then
    printf '    FAIL: Work prompt should not include legacy status file contract\n'
    RESULT=1
  fi
  if printf '%s' "$PROMPT" | grep -Fq ".fluent/runs/"; then
    printf '    FAIL: Work prompt should not include legacy run state paths\n'
    RESULT=1
  fi
  if printf '%s' "$PROMPT" | grep -Fq "handoff.md"; then
    printf '    FAIL: Work prompt should not include legacy handoff instructions\n'
    RESULT=1
  fi
  if printf '%s' "$PROMPT" | grep -Fq "mark the Task needs-user"; then
    printf '    FAIL: prompt should not tell write Task authors to mark needs-user\n'
    RESULT=1
  fi

  cleanup_test_project
  return $RESULT
}

test_clean_committed_task_completes() {
  setup_test_project
  create_work_task
  write_mock_codex

  RESULT=0
  run_task commit > "$TEST_DIR/stdout" 2> "$TEST_DIR/stderr" || RESULT=1

  COMMIT="$(show_json_value '.attempts[0].tasks[0].output.commit')"
  [ "$(show_json_value '.attempts[0].tasks[0].status')" = "complete" ] || RESULT=1
  [ "$(show_json_value '.attempts[0].status')" = "complete" ] || RESULT=1
  [ "$(show_json_value '.attempts[0].tasks[0].output.workspace_path')" = "$(workspace_path)" ] || RESULT=1
  [ "$COMMIT" != "null" ] && git -C "$(workspace_path)" cat-file -e "${COMMIT}^{commit}" || RESULT=1
  [ -z "$(git -C "$(workspace_path)" status --porcelain)" ] || RESULT=1
  assert_contains "$(cat "$TEST_DIR/stdout")" "Completed Task attempt-1-write-1" || RESULT=1

  cleanup_test_project
  return $RESULT
}

test_dirty_successful_task_fails_with_guidance() {
  setup_test_project
  create_work_task
  write_mock_codex

  RESULT=0
  assert_fails run_task dirty || RESULT=1
  assert_contains "$(cat "$TEST_DIR/stderr")" "commit or remove them before completing" || RESULT=1
  assert_not_complete || RESULT=1

  cleanup_test_project
  return $RESULT
}

test_coder_failure_marks_task_failed() {
  setup_test_project
  create_work_task
  write_mock_codex

  RESULT=0
  assert_fails run_task fail || RESULT=1
  assert_contains "$(cat "$TEST_DIR/stderr")" "Coder exited with code 7" || RESULT=1
  [ "$(show_json_value '.attempts[0].status')" = "failed" ] || RESULT=1
  [ "$(show_json_value '.attempts[0].tasks[0].status')" = "failed" ] || RESULT=1
  assert_not_complete || RESULT=1

  cleanup_test_project
  return $RESULT
}

test_success_without_new_commit_fails_with_guidance() {
  setup_test_project
  create_work_task
  write_mock_codex

  RESULT=0
  assert_fails run_task no-commit || RESULT=1
  assert_contains "$(cat "$TEST_DIR/stderr")" "no committed Task output" || RESULT=1
  assert_not_complete || RESULT=1

  cleanup_test_project
  return $RESULT
}

test_review_planning_requires_completed_write_output() {
  setup_test_project
  create_work_task

  RESULT=0
  BEFORE="$(cat .fluent/work/items/work-1.json)"
  assert_fails run_review_command || RESULT=1
  AFTER="$(cat .fluent/work/items/work-1.json)"
  [ "$AFTER" = "$BEFORE" ] || RESULT=1
  assert_contains "$(cat "$TEST_DIR/stderr")" "completed write" || RESULT=1

  cleanup_test_project
  return $RESULT
}

test_review_planning_adds_read_only_task_without_changing_candidate() {
  setup_test_project
  create_work_task
  write_mock_codex
  run_task commit > "$TEST_DIR/stdout" 2> "$TEST_DIR/stderr" || {
    cleanup_test_project
    return 1
  }

  RESULT=0
  CANDIDATE_COMMIT="$(git -C "$(workspace_path)" rev-parse HEAD)"
  CANDIDATE_STATUS="$(git -C "$(workspace_path)" status --porcelain)"
  "$FLUENT_BIN" review work-1 attempt-1 > "$TEST_DIR/stdout" 2> "$TEST_DIR/stderr" || RESULT=1

  [ "$(json_value '.attempts[0].status')" = "reviewing" ] || RESULT=1
  [ "$(json_value '.attempts[0].tasks[] | select(.id == "attempt-1-review-tests") | .kind')" = "review" ] || RESULT=1
  [ "$(json_value '.attempts[0].tasks[] | select(.id == "attempt-1-review-tests") | .workspace_access.writes | length')" = "0" ] || RESULT=1
  [ "$(json_value '.attempts[0].tasks[] | select(.id == "attempt-1-review-tests") | .workspace_access.reads | length')" -ge "1" ] || RESULT=1
  [ "$(json_value '.attempts[0].tasks[] | select(.id == "attempt-1-review-tests") | .workspace_access.reads[0].path')" = "$(workspace_path)" ] || RESULT=1
  [ "$(json_value '.attempts[0].tasks[] | select(.id == "attempt-1-review-tests") | .artifact_area.path')" = ".fluent/work/artifacts/work-1/attempt-1/attempt-1-review-tests" ] || RESULT=1
  [ "$(git -C "$(workspace_path)" rev-parse HEAD)" = "$CANDIDATE_COMMIT" ] || RESULT=1
  [ "$(git -C "$(workspace_path)" status --porcelain)" = "$CANDIDATE_STATUS" ] || RESULT=1

  cleanup_test_project
  return $RESULT
}

test_review_task_with_fail_verdict_completes() {
  setup_test_project
  create_work_task
  write_mock_codex
  run_task commit > "$TEST_DIR/stdout" 2> "$TEST_DIR/stderr" || {
    cleanup_test_project
    return 1
  }
  "$FLUENT_BIN" review work-1 attempt-1 > "$TEST_DIR/stdout" 2> "$TEST_DIR/stderr"
  seed_review_fixtures

  RESULT=0
  CANDIDATE_COMMIT="$(git -C "$(workspace_path)" rev-parse HEAD)"
  [ "$(json_value '.attempts[0].status')" = "reviewing" ] || RESULT=1
  [ "$(json_value '.attempts[0].tasks[] | select(.id == "attempt-1-review-tests") | .workspace_access.writes | length')" = "0" ] || RESULT=1
  [ "$(json_value '.attempts[0].tasks[] | select(.id == "attempt-1-review-tests") | .artifact_area.path')" = ".fluent/work/artifacts/work-1/attempt-1/attempt-1-review-tests" ] || RESULT=1
  run_review_task review-fail > "$TEST_DIR/stdout" 2> "$TEST_DIR/stderr" || RESULT=1
  PROMPT="$(cat "$TEST_DIR/coder-prompt.log")"
  assert_contains "$PROMPT" "Review changes for this Work Item" || RESULT=1
  assert_contains "$PROMPT" "Workspace: $(workspace_path)" || RESULT=1
  assert_contains "$PROMPT" ".fluent/work/artifacts/work-1/attempt-1/attempt-1-review-tests" || RESULT=1
  REVIEW_CWD="$(tail -n 1 "$TEST_DIR/coder-cwd.log")"
  EXPECTED_ARTIFACT_CWD="$(cd .fluent/work/artifacts/work-1/attempt-1/attempt-1-review-tests && pwd -P)"
  [ "$REVIEW_CWD" = "$EXPECTED_ARTIFACT_CWD" ] || RESULT=1
  [ "$(json_value '.attempts[0].tasks[] | select(.id == "attempt-1-review-tests") | .status')" = "complete" ] || RESULT=1
  [ "$(json_value '.attempts[0].artifacts[] | select(.path == ".fluent/work/artifacts/work-1/attempt-1/attempt-1-review-tests/review.md") | .path')" = ".fluent/work/artifacts/work-1/attempt-1/attempt-1-review-tests/review.md" ] || RESULT=1
  grep -q 'Verdict: fail' .fluent/work/artifacts/work-1/attempt-1/attempt-1-review-tests/review.md || RESULT=1
  [ "$(git -C "$(workspace_path)" rev-parse HEAD)" = "$CANDIDATE_COMMIT" ] || RESULT=1
  [ -z "$(git -C "$(workspace_path)" status --porcelain)" ] || RESULT=1

  cleanup_test_project
  return $RESULT
}

test_review_task_can_read_candidate_workspace() {
  setup_test_project
  create_work_task
  write_mock_codex
  run_task commit > "$TEST_DIR/stdout" 2> "$TEST_DIR/stderr" || {
    cleanup_test_project
    return 1
  }
  "$FLUENT_BIN" review work-1 attempt-1 > "$TEST_DIR/stdout" 2> "$TEST_DIR/stderr"
  seed_review_fixtures

  RESULT=0
  run_review_task review-read-candidate > "$TEST_DIR/stdout" 2> "$TEST_DIR/stderr" || RESULT=1
  [ "$(cat .fluent/work/artifacts/work-1/attempt-1/attempt-1-review-tests/candidate-read.txt)" = "test" ] || RESULT=1
  [ "$(json_value '.attempts[0].tasks[] | select(.id == "attempt-1-review-tests") | .status')" = "complete" ] || RESULT=1

  cleanup_test_project
  return $RESULT
}

test_review_task_with_uncertain_verdict_completes() {
  setup_test_project
  create_work_task
  write_mock_codex
  run_task commit > "$TEST_DIR/stdout" 2> "$TEST_DIR/stderr" || {
    cleanup_test_project
    return 1
  }
  "$FLUENT_BIN" review work-1 attempt-1 > "$TEST_DIR/stdout" 2> "$TEST_DIR/stderr"
  seed_review_fixtures

  RESULT=0
  run_review_task review-uncertain > "$TEST_DIR/stdout" 2> "$TEST_DIR/stderr" || RESULT=1
  [ "$(json_value '.attempts[0].tasks[] | select(.id == "attempt-1-review-tests") | .status')" = "complete" ] || RESULT=1
  grep -q 'Verdict: uncertain' .fluent/work/artifacts/work-1/attempt-1/attempt-1-review-tests/review.md || RESULT=1

  cleanup_test_project
  return $RESULT
}

test_review_task_missing_artifact_fails() {
  setup_test_project
  create_work_task
  write_mock_codex
  run_task commit > "$TEST_DIR/stdout" 2> "$TEST_DIR/stderr" || {
    cleanup_test_project
    return 1
  }
  "$FLUENT_BIN" review work-1 attempt-1 > "$TEST_DIR/stdout" 2> "$TEST_DIR/stderr"
  seed_review_fixtures

  RESULT=0
  assert_fails run_review_task review-missing || RESULT=1
  assert_contains "$(cat "$TEST_DIR/stderr")" "without writing" || RESULT=1
  [ "$(json_value '.attempts[0].tasks[] | select(.id == "attempt-1-review-tests") | .status')" = "failed" ] || RESULT=1

  cleanup_test_project
  return $RESULT
}

test_review_coder_failure_marks_task_failed() {
  setup_test_project
  create_work_task
  write_mock_codex
  run_task commit > "$TEST_DIR/stdout" 2> "$TEST_DIR/stderr" || {
    cleanup_test_project
    return 1
  }
  "$FLUENT_BIN" review work-1 attempt-1 > "$TEST_DIR/stdout" 2> "$TEST_DIR/stderr"
  seed_review_fixtures

  RESULT=0
  assert_fails run_review_task fail || RESULT=1
  assert_contains "$(cat "$TEST_DIR/stderr")" "Coder exited with code 7" || RESULT=1
  [ "$(json_value '.attempts[0].tasks[] | select(.id == "attempt-1-review-tests") | .status')" = "failed" ] || RESULT=1

  cleanup_test_project
  return $RESULT
}

test_invalid_review_task_requests_do_not_complete_or_mutate() {
  setup_test_project
  create_work_task
  write_mock_codex
  run_task commit > "$TEST_DIR/stdout" 2> "$TEST_DIR/stderr" || {
    cleanup_test_project
    return 1
  }
  "$FLUENT_BIN" review work-1 attempt-1 > "$TEST_DIR/stdout" 2> "$TEST_DIR/stderr"
  seed_review_fixtures

  RESULT=0
  REVIEW_FILTER='.attempts[0].tasks[] | select(.id == "attempt-1-review-tests")'
  REVIEW_TASK_PATH=".fluent/work/tasks/work-1/attempt-1/attempt-1-review-tests.json"
  BEFORE="$(jq -S . "$REVIEW_TASK_PATH")"

  assert_fails "$FLUENT_BIN" task run --no-sandbox --coder codex \
    missing-work attempt-1 attempt-1-review-tests || RESULT=1
  assert_fails "$FLUENT_BIN" task run --no-sandbox --coder codex \
    work-1 missing-attempt attempt-1-review-tests || RESULT=1
  assert_fails "$FLUENT_BIN" task run --no-sandbox --coder codex \
    work-1 attempt-1 missing-review-task || RESULT=1
  assert_fails "$FLUENT_BIN" task run --no-sandbox --coder codex \
    work-1 attempt-2 attempt-1-review-tests || RESULT=1
  [ "$(jq -S . "$REVIEW_TASK_PATH")" = "$BEFORE" ] || RESULT=1

  jq '.work_item_id = "other-work"' "$REVIEW_TASK_PATH" > "$TEST_DIR/task.json"
  mv "$TEST_DIR/task.json" "$REVIEW_TASK_PATH"
  AFTER_MUTATION="$(jq -S . "$REVIEW_TASK_PATH")"
  assert_fails run_review_task review-fail || RESULT=1
  [ "$(jq -S . "$REVIEW_TASK_PATH")" = "$AFTER_MUTATION" ] || RESULT=1

  jq -S . <<< "$BEFORE" > "$REVIEW_TASK_PATH"
  jq '.attempt_id = "other-attempt"' "$REVIEW_TASK_PATH" > "$TEST_DIR/task.json"
  mv "$TEST_DIR/task.json" "$REVIEW_TASK_PATH"
  AFTER_MUTATION="$(jq -S . "$REVIEW_TASK_PATH")"
  assert_fails run_review_task review-fail || RESULT=1
  [ "$(jq -S . "$REVIEW_TASK_PATH")" = "$AFTER_MUTATION" ] || RESULT=1

  jq -S . <<< "$BEFORE" > "$REVIEW_TASK_PATH"
  jq '.status = "failed"' "$REVIEW_TASK_PATH" > "$TEST_DIR/task.json"
  mv "$TEST_DIR/task.json" "$REVIEW_TASK_PATH"
  AFTER_MUTATION="$(jq -S . "$REVIEW_TASK_PATH")"
  assert_fails run_review_task review-fail || RESULT=1
  [ "$(jq -S . "$REVIEW_TASK_PATH")" = "$AFTER_MUTATION" ] || RESULT=1

  jq -S . <<< "$BEFORE" > "$REVIEW_TASK_PATH"
  jq '.workspace_access.writes = [{"id":"candidate","path":"../work-6-work-1-attempt-1"}]' \
    "$REVIEW_TASK_PATH" > "$TEST_DIR/task.json"
  mv "$TEST_DIR/task.json" "$REVIEW_TASK_PATH"
  AFTER_MUTATION="$(jq -S . "$REVIEW_TASK_PATH")"
  assert_fails run_review_task review-fail || RESULT=1
  [ "$(jq -S . "$REVIEW_TASK_PATH")" = "$AFTER_MUTATION" ] || RESULT=1

  jq -S . <<< "$BEFORE" > "$REVIEW_TASK_PATH"
  jq 'del(.artifact_area)' "$REVIEW_TASK_PATH" > "$TEST_DIR/task.json"
  mv "$TEST_DIR/task.json" "$REVIEW_TASK_PATH"
  AFTER_MUTATION="$(jq -S . "$REVIEW_TASK_PATH")"
  assert_fails run_review_task review-fail || RESULT=1
  [ "$(jq -S . "$REVIEW_TASK_PATH")" = "$AFTER_MUTATION" ] || RESULT=1

  jq -S . <<< "$BEFORE" > "$REVIEW_TASK_PATH"
  jq '.workspace_access.reads = []' "$REVIEW_TASK_PATH" > "$TEST_DIR/task.json"
  mv "$TEST_DIR/task.json" "$REVIEW_TASK_PATH"
  AFTER_MUTATION="$(jq -S . "$REVIEW_TASK_PATH")"
  assert_fails run_review_task review-fail || RESULT=1
  [ "$(jq -S . "$REVIEW_TASK_PATH")" = "$AFTER_MUTATION" ] || RESULT=1

  jq -S . <<< "$BEFORE" > "$REVIEW_TASK_PATH"
  jq 'del(.review_context)' "$REVIEW_TASK_PATH" > "$TEST_DIR/task.json"
  mv "$TEST_DIR/task.json" "$REVIEW_TASK_PATH"
  AFTER_MUTATION="$(jq -S . "$REVIEW_TASK_PATH")"
  assert_fails run_review_task review-fail || RESULT=1
  assert_contains "$(cat "$TEST_DIR/stderr")" "must declare review context" || RESULT=1
  [ "$(jq -S . "$REVIEW_TASK_PATH")" = "$AFTER_MUTATION" ] || RESULT=1

  jq -S . <<< "$BEFORE" > "$REVIEW_TASK_PATH"
  jq '.review_context.candidate_workspace_id = "other-candidate"' \
    "$REVIEW_TASK_PATH" > "$TEST_DIR/task.json"
  mv "$TEST_DIR/task.json" "$REVIEW_TASK_PATH"
  AFTER_MUTATION="$(jq -S . "$REVIEW_TASK_PATH")"
  assert_fails run_review_task review-fail || RESULT=1
  assert_contains "$(cat "$TEST_DIR/stderr")" "review context candidate must match" || RESULT=1
  [ "$(jq -S . "$REVIEW_TASK_PATH")" = "$AFTER_MUTATION" ] || RESULT=1

  jq -S . <<< "$BEFORE" > "$REVIEW_TASK_PATH"
  jq '
    .workspace_access.reads[0].path = "../outside-review-read" |
    .review_context.candidate_workspace_path = "../outside-review-read"
  ' "$REVIEW_TASK_PATH" > "$TEST_DIR/task.json"
  mv "$TEST_DIR/task.json" "$REVIEW_TASK_PATH"
  AFTER_MUTATION="$(jq -S . "$REVIEW_TASK_PATH")"
  assert_fails run_review_task review-fail || RESULT=1
  assert_contains "$(cat "$TEST_DIR/stderr")" "Task readable workspace path must" || RESULT=1
  [ "$(jq -S . "$REVIEW_TASK_PATH")" = "$AFTER_MUTATION" ] || RESULT=1

  printf '%s' "$BEFORE" > "$REVIEW_TASK_PATH"
  [ "$(json_value "$REVIEW_FILTER | .status // \"planned\"")" = "planned" ] || RESULT=1

  cleanup_test_project
  return $RESULT
}

test_invalid_task_requests_do_not_complete_or_mutate() {
  setup_test_project
  create_work_task
  "$FLUENT_BIN" attempt create work-1 attempt-2 > /dev/null
  write_mock_codex

  RESULT=0
  TASK_PATH=".fluent/work/tasks/work-1/attempt-1/attempt-1-write-1.json"
  BEFORE="$(cat "$TASK_PATH")"
  assert_fails "$FLUENT_BIN" task run --no-sandbox --coder codex \
    missing-work attempt-1 attempt-1-write-1 || RESULT=1
  assert_fails "$FLUENT_BIN" task run --no-sandbox --coder codex \
    work-1 missing-attempt attempt-1-write-1 || RESULT=1
  assert_fails "$FLUENT_BIN" task run --no-sandbox --coder codex \
    work-1 attempt-1 missing-task || RESULT=1
  assert_fails "$FLUENT_BIN" task run --no-sandbox --coder codex \
    work-1 attempt-2 attempt-1-write-1 || RESULT=1
  if [ "$(cat "$TASK_PATH")" != "$BEFORE" ]; then
    printf '    FAIL: missing id requests changed Work Item state\n'
    RESULT=1
  fi

  jq '.kind = "review"' "$TASK_PATH" > "$TEST_DIR/task.json"
  mv "$TEST_DIR/task.json" "$TASK_PATH"
  assert_fails "$FLUENT_BIN" task run --no-sandbox --coder codex \
    work-1 attempt-1 attempt-1-write-1 || RESULT=1
  assert_not_complete || RESULT=1

  printf '%s' "$BEFORE" > "$TASK_PATH"
  jq '.work_item_id = "other-work"' "$TASK_PATH" > "$TEST_DIR/task.json"
  mv "$TEST_DIR/task.json" "$TASK_PATH"
  assert_fails "$FLUENT_BIN" task run --no-sandbox --coder codex \
    work-1 attempt-1 attempt-1-write-1 || RESULT=1
  assert_not_complete || RESULT=1

  printf '%s' "$BEFORE" > "$TASK_PATH"
  jq '.status = "failed"' "$TASK_PATH" > "$TEST_DIR/task.json"
  mv "$TEST_DIR/task.json" "$TASK_PATH"
  assert_fails "$FLUENT_BIN" task run --no-sandbox --coder codex \
    work-1 attempt-1 attempt-1-write-1 || RESULT=1
  assert_contains "$(cat "$TEST_DIR/stderr")" "expected planned" || RESULT=1
  assert_not_complete || RESULT=1

  printf '%s' "$BEFORE" > "$TASK_PATH"
  jq '.workspace_access.writes = []' "$TASK_PATH" > "$TEST_DIR/task.json"
  mv "$TEST_DIR/task.json" "$TASK_PATH"
  assert_fails "$FLUENT_BIN" task run --no-sandbox --coder codex \
    work-1 attempt-1 attempt-1-write-1 || RESULT=1
  assert_not_complete || RESULT=1

  printf '%s' "$BEFORE" > "$TASK_PATH"
  jq '.workspace_access.writes += [{"id":"other","path":"../work-6-work-1-other"}]' \
    "$TASK_PATH" > "$TEST_DIR/task.json"
  mv "$TEST_DIR/task.json" "$TASK_PATH"
  assert_fails "$FLUENT_BIN" task run --no-sandbox --coder codex \
    work-1 attempt-1 attempt-1-write-1 || RESULT=1

  assert_not_complete || RESULT=1

  printf '%s' "$BEFORE" > "$TASK_PATH"
  mkdir -p "$(workspace_path)"
  assert_fails "$FLUENT_BIN" task run --no-sandbox --coder codex \
    work-1 attempt-1 attempt-1-write-1 || RESULT=1
  assert_contains "$(cat "$TEST_DIR/stderr")" "not a registered git worktree" || RESULT=1
  rm -rf "$(workspace_path)"
  assert_not_complete || RESULT=1

  printf '%s' "$BEFORE" > "$TASK_PATH"
  jq '.workspace_access.writes[0].path = "../outside-workspace"' \
    "$TASK_PATH" > "$TEST_DIR/task.json"
  mv "$TEST_DIR/task.json" "$TASK_PATH"
  assert_fails "$FLUENT_BIN" task run --no-sandbox --coder codex \
    work-1 attempt-1 attempt-1-write-1 || RESULT=1
  assert_contains "$(cat "$TEST_DIR/stderr")" "Task writable workspace path must" || RESULT=1
  if [ -e "../outside-workspace" ]; then
    printf '    FAIL: invalid workspace path created an external workspace\n'
    RESULT=1
  fi

  printf '%s' "$BEFORE" > "$TASK_PATH"
  jq --arg path "${TEST_DIR}/outside-absolute" \
    '.workspace_access.writes[0].path = $path' \
    "$TASK_PATH" > "$TEST_DIR/task.json"
  mv "$TEST_DIR/task.json" "$TASK_PATH"
  assert_fails "$FLUENT_BIN" task run --no-sandbox --coder codex \
    work-1 attempt-1 attempt-1-write-1 || RESULT=1
  assert_contains "$(cat "$TEST_DIR/stderr")" "Task writable workspace path must" || RESULT=1
  if [ -e "${TEST_DIR}/outside-absolute" ]; then
    printf '    FAIL: invalid absolute workspace path created a workspace\n'
    RESULT=1
  fi

  if [ -e "$(workspace_path)" ]; then
    printf '    FAIL: invalid task requests created a workspace\n'
    RESULT=1
  fi

  cleanup_test_project
  return $RESULT
}

printf 'test-work-task-run\n\n'
run_test "run reuses worktree and launches coder there" \
  test_run_reuses_worktree_and_launches_coder_there
run_test "run passes Task context to coder prompt" \
  test_run_passes_task_context_to_coder_prompt
run_test "clean committed Task completes" test_clean_committed_task_completes
run_test "dirty successful Task fails with guidance" \
  test_dirty_successful_task_fails_with_guidance
run_test "coder failure marks Task failed" test_coder_failure_marks_task_failed
run_test "success without new commit fails with guidance" \
  test_success_without_new_commit_fails_with_guidance
run_test "review planning requires completed write output" \
  test_review_planning_requires_completed_write_output
run_test "review planning adds read-only Task without changing candidate" \
  test_review_planning_adds_read_only_task_without_changing_candidate
run_test "review Task with fail verdict completes" \
  test_review_task_with_fail_verdict_completes
run_test "review Task can read candidate workspace" \
  test_review_task_can_read_candidate_workspace
run_test "review Task with uncertain verdict completes" \
  test_review_task_with_uncertain_verdict_completes
run_test "review Task missing artifact fails" \
  test_review_task_missing_artifact_fails
run_test "review coder failure marks Task failed" \
  test_review_coder_failure_marks_task_failed
run_test "invalid review Task requests do not complete or mutate" \
  test_invalid_review_task_requests_do_not_complete_or_mutate
run_test "invalid task requests do not complete or mutate" \
  test_invalid_task_requests_do_not_complete_or_mutate

summarize_and_exit
