#!/usr/bin/env bash
# test-work-task-run - Verify write Task execution from the CLI.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$(dirname "$(dirname "$SCRIPT_DIR")")")"
FACTORY_BIN="${PROJECT_DIR}/target/debug/factory"

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
  TEST_DIR="$(mktemp -d -t factory-work-task-run-XXXXXX)"
  mkdir -p "$TEST_DIR/project" "$TEST_DIR/bin"
  cd "$TEST_DIR/project"
  git init -b main > /dev/null 2>&1
  git config commit.gpgsign false
  git config user.email "test@test"
  git config user.name "test"
  printf 'test\n' > README.md
  git add README.md && git commit -m "init" > /dev/null 2>&1
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
  "$FACTORY_BIN" work create work-1 --title "Task execution" > /dev/null
  "$FACTORY_BIN" work attempt work-1 attempt-1 > /dev/null
}

create_instructed_work_task() {
  "$FACTORY_BIN" work create work-1 \
    --title "Task execution" \
    --instructions "Implement durable task instructions." > /dev/null
  "$FACTORY_BIN" work attempt work-1 attempt-1 > /dev/null
}

json_value() {
  jq -r "$1" .factory/work/items/work-1.json
}

show_json_value() {
  "$FACTORY_BIN" work show work-1 | jq -r "$1"
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
    cat ../../../../../../work-6-work-1-attempt-1/README.md > candidate-read.txt
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
  status="$(json_value '.attempts[0].tasks[0].status // "missing"')"
  output_commit="$(json_value '.attempts[0].tasks[0].output.commit // "missing"')"
  if [ "$status" = "complete" ] || [ "$output_commit" != "missing" ]; then
    printf '    FAIL: Task completed unexpectedly\n'
    "$FACTORY_BIN" work show work-1
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
    "$FACTORY_BIN" work task run --no-sandbox --coder codex \
      work-1 attempt-1 attempt-1-write
}

run_review_task() {
  TASK_RUN_MOCK_MODE="$1" \
    PATH="${MOCK_BIN}:${PATH}" \
    CODER_CWD_LOG="${TEST_DIR}/coder-cwd.log" \
    CODER_ARGS_LOG="${TEST_DIR}/coder-args.log" \
    CODER_PROMPT_LOG="${TEST_DIR}/coder-prompt.log" \
    "$FACTORY_BIN" work task run --no-sandbox --coder codex \
      work-1 attempt-1 attempt-1-review-tests
}

run_review_command() {
  "$FACTORY_BIN" work review work-1 attempt-1
}

test_run_reuses_worktree_and_launches_coder_there() {
  setup_test_project
  create_work_task
  write_mock_codex

  RESULT=0
  git worktree add -b precreated-task-workspace "$(workspace_path)" HEAD > /dev/null 2>&1
  run_task commit > "$TEST_DIR/stdout" 2> "$TEST_DIR/stderr" || RESULT=1

  [ -d "$(workspace_path)/.git" ] || [ -f "$(workspace_path)/.git" ] || RESULT=1
  [ "$(cat "$TEST_DIR/coder-cwd.log")" = "$(physical_workspace_path)" ] || RESULT=1
  assert_contains "$(cat "$TEST_DIR/coder-args.log")" "exec" || RESULT=1

  cleanup_test_project
  return $RESULT
}

test_run_passes_task_context_to_coder_prompt() {
  setup_test_project
  "$FACTORY_BIN" work create work-1 --title "Prompt contract" > /dev/null
  "$FACTORY_BIN" work attempt work-1 attempt-1 > /dev/null
  write_mock_codex

  RESULT=0
  run_task commit > "$TEST_DIR/stdout" 2> "$TEST_DIR/stderr" || RESULT=1

  PROMPT="$(cat "$TEST_DIR/coder-prompt.log")"
  assert_contains "$PROMPT" "Work Item: work-1 - Prompt contract" || RESULT=1
  assert_contains "$PROMPT" "Attempt: attempt-1" || RESULT=1
  assert_contains "$PROMPT" "Task: attempt-1-write" || RESULT=1
  assert_contains "$PROMPT" "Role: author" || RESULT=1
  assert_contains "$PROMPT" "Completion contract:" || RESULT=1
  assert_contains "$PROMPT" "Commit all Task output" || RESULT=1
  assert_contains "$PROMPT" "Leave the writable workspace clean" || RESULT=1
  assert_contains "$PROMPT" "Current Task model:" || RESULT=1
  assert_contains "$PROMPT" '"id": "attempt-1-write"' || RESULT=1
  assert_contains "$PROMPT" '"kind": "write"' || RESULT=1

  cleanup_test_project
  return $RESULT
}

test_run_passes_task_instructions_to_coder_prompt() {
  setup_test_project
  create_instructed_work_task
  write_mock_codex

  RESULT=0
  run_task commit > "$TEST_DIR/stdout" 2> "$TEST_DIR/stderr" || RESULT=1

  PROMPT="$(cat "$TEST_DIR/coder-prompt.log")"
  assert_contains "$PROMPT" "Task instructions:" || RESULT=1
  assert_contains "$PROMPT" "Implement durable task instructions." || RESULT=1
  assert_contains "$PROMPT" '"instructions": "Implement durable task instructions."' || RESULT=1

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
  assert_contains "$(cat "$TEST_DIR/stdout")" "Completed Task attempt-1-write" || RESULT=1

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
  BEFORE="$(cat .factory/work/items/work-1.json)"
  assert_fails run_review_command || RESULT=1
  AFTER="$(cat .factory/work/items/work-1.json)"
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
  "$FACTORY_BIN" work review work-1 attempt-1 > "$TEST_DIR/stdout" 2> "$TEST_DIR/stderr" || RESULT=1

  [ "$(json_value '.attempts[0].status')" = "reviewing" ] || RESULT=1
  [ "$(json_value '.attempts[0].tasks[] | select(.id == "attempt-1-review-tests") | .kind')" = "review" ] || RESULT=1
  [ "$(json_value '.attempts[0].tasks[] | select(.id == "attempt-1-review-tests") | .workspace_access.writes | length')" = "0" ] || RESULT=1
  [ "$(json_value '.attempts[0].tasks[] | select(.id == "attempt-1-review-tests") | .workspace_access.reads | length')" -ge "1" ] || RESULT=1
  [ "$(json_value '.attempts[0].tasks[] | select(.id == "attempt-1-review-tests") | .workspace_access.reads[0].path')" = "$(workspace_path)" ] || RESULT=1
  [ "$(json_value '.attempts[0].tasks[] | select(.id == "attempt-1-review-tests") | .artifact_area.path')" = ".factory/work/artifacts/attempt-1/attempt-1-review-tests" ] || RESULT=1
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
  "$FACTORY_BIN" work review work-1 attempt-1 > "$TEST_DIR/stdout" 2> "$TEST_DIR/stderr"

  RESULT=0
  CANDIDATE_COMMIT="$(git -C "$(workspace_path)" rev-parse HEAD)"
  [ "$(json_value '.attempts[0].status')" = "reviewing" ] || RESULT=1
  [ "$(json_value '.attempts[0].tasks[] | select(.id == "attempt-1-review-tests") | .workspace_access.writes | length')" = "0" ] || RESULT=1
  [ "$(json_value '.attempts[0].tasks[] | select(.id == "attempt-1-review-tests") | .artifact_area.path')" = ".factory/work/artifacts/attempt-1/attempt-1-review-tests" ] || RESULT=1
  run_review_task review-fail > "$TEST_DIR/stdout" 2> "$TEST_DIR/stderr" || RESULT=1
  PROMPT="$(cat "$TEST_DIR/coder-prompt.log")"
  assert_contains "$PROMPT" "Execute this Factory review Task" || RESULT=1
  assert_contains "$PROMPT" "- candidate: $(physical_workspace_path)" || RESULT=1
  assert_contains "$PROMPT" ".factory/work/artifacts/attempt-1/attempt-1-review-tests" || RESULT=1
  REVIEW_CWD="$(tail -n 1 "$TEST_DIR/coder-cwd.log")"
  EXPECTED_ARTIFACT_CWD="$(cd .factory/work/artifacts/attempt-1/attempt-1-review-tests && pwd -P)"
  [ "$REVIEW_CWD" = "$EXPECTED_ARTIFACT_CWD" ] || RESULT=1
  [ "$(json_value '.attempts[0].tasks[] | select(.id == "attempt-1-review-tests") | .status')" = "complete" ] || RESULT=1
  [ "$(json_value '.attempts[0].artifacts[] | select(.path == ".factory/work/artifacts/attempt-1/attempt-1-review-tests/review.md") | .path')" = ".factory/work/artifacts/attempt-1/attempt-1-review-tests/review.md" ] || RESULT=1
  grep -q 'Verdict: fail' .factory/work/artifacts/attempt-1/attempt-1-review-tests/review.md || RESULT=1
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
  "$FACTORY_BIN" work review work-1 attempt-1 > "$TEST_DIR/stdout" 2> "$TEST_DIR/stderr"

  RESULT=0
  run_review_task review-read-candidate > "$TEST_DIR/stdout" 2> "$TEST_DIR/stderr" || RESULT=1
  [ "$(cat .factory/work/artifacts/attempt-1/attempt-1-review-tests/candidate-read.txt)" = "test" ] || RESULT=1
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
  "$FACTORY_BIN" work review work-1 attempt-1 > "$TEST_DIR/stdout" 2> "$TEST_DIR/stderr"

  RESULT=0
  run_review_task review-uncertain > "$TEST_DIR/stdout" 2> "$TEST_DIR/stderr" || RESULT=1
  [ "$(json_value '.attempts[0].tasks[] | select(.id == "attempt-1-review-tests") | .status')" = "complete" ] || RESULT=1
  grep -q 'Verdict: uncertain' .factory/work/artifacts/attempt-1/attempt-1-review-tests/review.md || RESULT=1

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
  "$FACTORY_BIN" work review work-1 attempt-1 > "$TEST_DIR/stdout" 2> "$TEST_DIR/stderr"

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
  "$FACTORY_BIN" work review work-1 attempt-1 > "$TEST_DIR/stdout" 2> "$TEST_DIR/stderr"

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
  "$FACTORY_BIN" work review work-1 attempt-1 > "$TEST_DIR/stdout" 2> "$TEST_DIR/stderr"

  RESULT=0
  REVIEW_FILTER='.attempts[0].tasks[] | select(.id == "attempt-1-review-tests")'
  BEFORE="$(cat .factory/work/items/work-1.json)"

  assert_fails "$FACTORY_BIN" work task run --no-sandbox --coder codex \
    missing-work attempt-1 attempt-1-review-tests || RESULT=1
  assert_fails "$FACTORY_BIN" work task run --no-sandbox --coder codex \
    work-1 missing-attempt attempt-1-review-tests || RESULT=1
  assert_fails "$FACTORY_BIN" work task run --no-sandbox --coder codex \
    work-1 attempt-1 missing-review-task || RESULT=1
  assert_fails "$FACTORY_BIN" work task run --no-sandbox --coder codex \
    work-1 attempt-2 attempt-1-review-tests || RESULT=1
  [ "$(cat .factory/work/items/work-1.json)" = "$BEFORE" ] || RESULT=1

  jq '(.attempts[0].tasks[] | select(.id == "attempt-1-review-tests") | .work_item_id) = "other-work"' \
    .factory/work/items/work-1.json > "$TEST_DIR/item.json"
  mv "$TEST_DIR/item.json" .factory/work/items/work-1.json
  AFTER_MUTATION="$(cat .factory/work/items/work-1.json)"
  assert_fails run_review_task review-fail || RESULT=1
  [ "$(cat .factory/work/items/work-1.json)" = "$AFTER_MUTATION" ] || RESULT=1

  printf '%s' "$BEFORE" > .factory/work/items/work-1.json
  jq '(.attempts[0].tasks[] | select(.id == "attempt-1-review-tests") | .attempt_id) = "other-attempt"' \
    .factory/work/items/work-1.json > "$TEST_DIR/item.json"
  mv "$TEST_DIR/item.json" .factory/work/items/work-1.json
  AFTER_MUTATION="$(cat .factory/work/items/work-1.json)"
  assert_fails run_review_task review-fail || RESULT=1
  [ "$(cat .factory/work/items/work-1.json)" = "$AFTER_MUTATION" ] || RESULT=1

  printf '%s' "$BEFORE" > .factory/work/items/work-1.json
  jq '(.attempts[0].tasks[] | select(.id == "attempt-1-review-tests") | .status) = "failed"' \
    .factory/work/items/work-1.json > "$TEST_DIR/item.json"
  mv "$TEST_DIR/item.json" .factory/work/items/work-1.json
  AFTER_MUTATION="$(cat .factory/work/items/work-1.json)"
  assert_fails run_review_task review-fail || RESULT=1
  [ "$(cat .factory/work/items/work-1.json)" = "$AFTER_MUTATION" ] || RESULT=1

  printf '%s' "$BEFORE" > .factory/work/items/work-1.json
  jq '(.attempts[0].tasks[] | select(.id == "attempt-1-review-tests") | .workspace_access.writes) = [{"id":"candidate","path":"../work-6-work-1-attempt-1"}]' \
    .factory/work/items/work-1.json > "$TEST_DIR/item.json"
  mv "$TEST_DIR/item.json" .factory/work/items/work-1.json
  AFTER_MUTATION="$(cat .factory/work/items/work-1.json)"
  assert_fails run_review_task review-fail || RESULT=1
  [ "$(cat .factory/work/items/work-1.json)" = "$AFTER_MUTATION" ] || RESULT=1

  printf '%s' "$BEFORE" > .factory/work/items/work-1.json
  jq 'del(.attempts[0].tasks[] | select(.id == "attempt-1-review-tests") | .artifact_area)' \
    .factory/work/items/work-1.json > "$TEST_DIR/item.json"
  mv "$TEST_DIR/item.json" .factory/work/items/work-1.json
  AFTER_MUTATION="$(cat .factory/work/items/work-1.json)"
  assert_fails run_review_task review-fail || RESULT=1
  [ "$(cat .factory/work/items/work-1.json)" = "$AFTER_MUTATION" ] || RESULT=1

  printf '%s' "$BEFORE" > .factory/work/items/work-1.json
  jq '(.attempts[0].tasks[] | select(.id == "attempt-1-review-tests") | .workspace_access.reads) = []' \
    .factory/work/items/work-1.json > "$TEST_DIR/item.json"
  mv "$TEST_DIR/item.json" .factory/work/items/work-1.json
  AFTER_MUTATION="$(cat .factory/work/items/work-1.json)"
  assert_fails run_review_task review-fail || RESULT=1
  [ "$(cat .factory/work/items/work-1.json)" = "$AFTER_MUTATION" ] || RESULT=1

  printf '%s' "$BEFORE" > .factory/work/items/work-1.json
  jq 'del(.attempts[0].tasks[] | select(.id == "attempt-1-review-tests") | .review_context)' \
    .factory/work/items/work-1.json > "$TEST_DIR/item.json"
  mv "$TEST_DIR/item.json" .factory/work/items/work-1.json
  AFTER_MUTATION="$(cat .factory/work/items/work-1.json)"
  assert_fails run_review_task review-fail || RESULT=1
  assert_contains "$(cat "$TEST_DIR/stderr")" "must declare review context" || RESULT=1
  [ "$(cat .factory/work/items/work-1.json)" = "$AFTER_MUTATION" ] || RESULT=1

  printf '%s' "$BEFORE" > .factory/work/items/work-1.json
  jq '(.attempts[0].tasks[] | select(.id == "attempt-1-review-tests") | .review_context.candidate_workspace_id) = "other-candidate"' \
    .factory/work/items/work-1.json > "$TEST_DIR/item.json"
  mv "$TEST_DIR/item.json" .factory/work/items/work-1.json
  AFTER_MUTATION="$(cat .factory/work/items/work-1.json)"
  assert_fails run_review_task review-fail || RESULT=1
  assert_contains "$(cat "$TEST_DIR/stderr")" "review context candidate must match" || RESULT=1
  [ "$(cat .factory/work/items/work-1.json)" = "$AFTER_MUTATION" ] || RESULT=1

  printf '%s' "$BEFORE" > .factory/work/items/work-1.json
  jq '
    (.attempts[0].tasks[] | select(.id == "attempt-1-review-tests") | .workspace_access.reads[0].path) = "../outside-review-read" |
    (.attempts[0].tasks[] | select(.id == "attempt-1-review-tests") | .review_context.candidate_workspace_path) = "../outside-review-read"
  ' \
    .factory/work/items/work-1.json > "$TEST_DIR/item.json"
  mv "$TEST_DIR/item.json" .factory/work/items/work-1.json
  AFTER_MUTATION="$(cat .factory/work/items/work-1.json)"
  assert_fails run_review_task review-fail || RESULT=1
  assert_contains "$(cat "$TEST_DIR/stderr")" "Task readable workspace path must" || RESULT=1
  [ "$(cat .factory/work/items/work-1.json)" = "$AFTER_MUTATION" ] || RESULT=1

  printf '%s' "$BEFORE" > .factory/work/items/work-1.json
  [ "$(json_value "$REVIEW_FILTER | .status // \"planned\"")" = "planned" ] || RESULT=1

  cleanup_test_project
  return $RESULT
}

test_invalid_task_requests_do_not_complete_or_mutate() {
  setup_test_project
  create_work_task
  "$FACTORY_BIN" work attempt work-1 attempt-2 > /dev/null
  write_mock_codex

  RESULT=0
  BEFORE="$(cat .factory/work/items/work-1.json)"
  assert_fails "$FACTORY_BIN" work task run --no-sandbox --coder codex \
    missing-work attempt-1 attempt-1-write || RESULT=1
  assert_fails "$FACTORY_BIN" work task run --no-sandbox --coder codex \
    work-1 missing-attempt attempt-1-write || RESULT=1
  assert_fails "$FACTORY_BIN" work task run --no-sandbox --coder codex \
    work-1 attempt-1 missing-task || RESULT=1
  assert_fails "$FACTORY_BIN" work task run --no-sandbox --coder codex \
    work-1 attempt-2 attempt-1-write || RESULT=1
  if [ "$(cat .factory/work/items/work-1.json)" != "$BEFORE" ]; then
    printf '    FAIL: missing id requests changed Work Item state\n'
    RESULT=1
  fi

  jq '.attempts[0].tasks[0].kind = "review"' \
    .factory/work/items/work-1.json > "$TEST_DIR/item.json"
  mv "$TEST_DIR/item.json" .factory/work/items/work-1.json
  assert_fails "$FACTORY_BIN" work task run --no-sandbox --coder codex \
    work-1 attempt-1 attempt-1-write || RESULT=1
  assert_not_complete || RESULT=1

  printf '%s' "$BEFORE" > .factory/work/items/work-1.json
  jq '.attempts[0].tasks[0].work_item_id = "other-work"' \
    .factory/work/items/work-1.json > "$TEST_DIR/item.json"
  mv "$TEST_DIR/item.json" .factory/work/items/work-1.json
  assert_fails "$FACTORY_BIN" work task run --no-sandbox --coder codex \
    work-1 attempt-1 attempt-1-write || RESULT=1
  assert_not_complete || RESULT=1

  printf '%s' "$BEFORE" > .factory/work/items/work-1.json
  jq '.attempts[0].tasks[0].status = "failed"' \
    .factory/work/items/work-1.json > "$TEST_DIR/item.json"
  mv "$TEST_DIR/item.json" .factory/work/items/work-1.json
  assert_fails "$FACTORY_BIN" work task run --no-sandbox --coder codex \
    work-1 attempt-1 attempt-1-write || RESULT=1
  assert_contains "$(cat "$TEST_DIR/stderr")" "expected planned" || RESULT=1
  assert_not_complete || RESULT=1

  printf '%s' "$BEFORE" > .factory/work/items/work-1.json
  jq '.attempts[0].tasks[0].workspace_access.writes = []' \
    .factory/work/items/work-1.json > "$TEST_DIR/item.json"
  mv "$TEST_DIR/item.json" .factory/work/items/work-1.json
  assert_fails "$FACTORY_BIN" work task run --no-sandbox --coder codex \
    work-1 attempt-1 attempt-1-write || RESULT=1
  assert_not_complete || RESULT=1

  printf '%s' "$BEFORE" > .factory/work/items/work-1.json
  jq '.attempts[0].tasks[0].workspace_access.writes += [{"id":"other","path":"../work-6-work-1-other"}]' \
    .factory/work/items/work-1.json > "$TEST_DIR/item.json"
  mv "$TEST_DIR/item.json" .factory/work/items/work-1.json
  assert_fails "$FACTORY_BIN" work task run --no-sandbox --coder codex \
    work-1 attempt-1 attempt-1-write || RESULT=1

  assert_not_complete || RESULT=1

  printf '%s' "$BEFORE" > .factory/work/items/work-1.json
  mkdir -p "$(workspace_path)"
  assert_fails "$FACTORY_BIN" work task run --no-sandbox --coder codex \
    work-1 attempt-1 attempt-1-write || RESULT=1
  assert_contains "$(cat "$TEST_DIR/stderr")" "not a registered git worktree" || RESULT=1
  rm -rf "$(workspace_path)"
  assert_not_complete || RESULT=1

  printf '%s' "$BEFORE" > .factory/work/items/work-1.json
  jq '.attempts[0].tasks[0].workspace_access.writes[0].path = "../outside-workspace"' \
    .factory/work/items/work-1.json > "$TEST_DIR/item.json"
  mv "$TEST_DIR/item.json" .factory/work/items/work-1.json
  assert_fails "$FACTORY_BIN" work task run --no-sandbox --coder codex \
    work-1 attempt-1 attempt-1-write || RESULT=1
  assert_contains "$(cat "$TEST_DIR/stderr")" "Task writable workspace path must" || RESULT=1
  if [ -e "../outside-workspace" ]; then
    printf '    FAIL: invalid workspace path created an external workspace\n'
    RESULT=1
  fi

  printf '%s' "$BEFORE" > .factory/work/items/work-1.json
  jq --arg path "${TEST_DIR}/outside-absolute" \
    '.attempts[0].tasks[0].workspace_access.writes[0].path = $path' \
    .factory/work/items/work-1.json > "$TEST_DIR/item.json"
  mv "$TEST_DIR/item.json" .factory/work/items/work-1.json
  assert_fails "$FACTORY_BIN" work task run --no-sandbox --coder codex \
    work-1 attempt-1 attempt-1-write || RESULT=1
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
run_test "run passes Task instructions to coder prompt" \
  test_run_passes_task_instructions_to_coder_prompt
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

printf '\n  %s passed, %s failed\n' "$PASS" "$FAIL"
if [ "$FAIL" -ne 0 ]; then
  printf 'Failed tests:%b\n' "$ERRORS"
  exit 1
fi
