#!/usr/bin/env bash
# test-work-sibling-workspaces - Verify Work candidate workspaces live beside the checkout.

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
  TEST_DIR="$(mktemp -d -t factory-work-sibling-workspaces-XXXXXX)"
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

write_mock_codex() {
  cat > "${TEST_DIR}/bin/codex" <<'MOCK_SCRIPT'
#!/usr/bin/env bash
printf '%s\n' "$PWD" > "$CODER_CWD_LOG"
case "$PWD" in
  */.factory/work/artifacts/*)
    printf 'Verdict: pass\n\nReview passed.\n' > review.md
    printf '%s\n' '{"type":"result","subtype":"success","result":"done","session_id":"mock"}'
    exit 0
    ;;
esac
case "${TASK_RUN_MOCK_MODE:-commit}" in
  commit)
    printf 'task output\n' > task-output.txt
    git add task-output.txt
    git commit -m "Add task output" > /dev/null 2>&1
    ;;
  *)
    printf 'unknown TASK_RUN_MOCK_MODE: %s\n' "$TASK_RUN_MOCK_MODE" >&2
    exit 2
    ;;
esac
printf '%s\n' '{"type":"result","subtype":"success","result":"done","session_id":"mock"}'
MOCK_SCRIPT
  chmod +x "${TEST_DIR}/bin/codex"
}

assert_fails() {
  if "$@" > "$TEST_DIR/stdout" 2> "$TEST_DIR/stderr"; then
    printf '    FAIL: command unexpectedly succeeded: %s\n' "$*"
    printf '    Stdout:\n%s\n' "$(cat "$TEST_DIR/stdout")"
    printf '    Stderr:\n%s\n' "$(cat "$TEST_DIR/stderr")"
    return 1
  fi
}

work_json_value() {
  "$FACTORY_BIN" work show "$1" | jq -r "$2"
}

run_write_task() {
  TASK_RUN_MOCK_MODE=commit \
    PATH="${TEST_DIR}/bin:$PATH" \
    CODER_CWD_LOG="${TEST_DIR}/coder-cwd.log" \
    "$FACTORY_BIN" work task run --no-sandbox --coder codex \
      "$1" "$2" "$2-write"
}

test_attempt_records_sibling_candidate_path() {
  setup_test_project
  trap cleanup_test_project RETURN

  RESULT=0
  "$FACTORY_BIN" work create work-alpha --title "Sibling path" > /dev/null
  "$FACTORY_BIN" work attempt work-alpha attempt-one > /dev/null

  PATH_VALUE="$(work_json_value work-alpha '.attempts[0].tasks[0].workspace_access.writes[0].path')"
  [ "$PATH_VALUE" = "../work-10-work-alpha-attempt-one" ] || RESULT=1
  case "$(cd "$PATH_VALUE/.." 2>/dev/null || cd ..; pwd -P)/$(basename "$PATH_VALUE")" in
    "$TEST_PROJECT_PWD"/*) RESULT=1 ;;
  esac
  [ ! -e .factory/work/workspaces/attempt-one ] || RESULT=1

  return $RESULT
}

test_attempt_paths_include_work_item_to_avoid_collisions() {
  setup_test_project
  trap cleanup_test_project RETURN

  RESULT=0
  "$FACTORY_BIN" work create work-alpha --title "First item" > /dev/null
  "$FACTORY_BIN" work create work-beta --title "Second item" > /dev/null
  "$FACTORY_BIN" work attempt work-alpha shared-attempt > /dev/null
  "$FACTORY_BIN" work attempt work-beta shared-attempt > /dev/null

  ALPHA_PATH="$(work_json_value work-alpha '.attempts[0].tasks[0].workspace_access.writes[0].path')"
  BETA_PATH="$(work_json_value work-beta '.attempts[0].tasks[0].workspace_access.writes[0].path')"
  [ "$ALPHA_PATH" = "../work-10-work-alpha-shared-attempt" ] || RESULT=1
  [ "$BETA_PATH" = "../work-9-work-beta-shared-attempt" ] || RESULT=1
  [ "$ALPHA_PATH" != "$BETA_PATH" ] || RESULT=1

  "$FACTORY_BIN" work create work-a --title "Hyphen first" > /dev/null
  "$FACTORY_BIN" work create work-a-b --title "Hyphen second" > /dev/null
  "$FACTORY_BIN" work attempt work-a b-c > /dev/null
  "$FACTORY_BIN" work attempt work-a-b c > /dev/null

  HYPHEN_FIRST_PATH="$(work_json_value work-a '.attempts[0].tasks[0].workspace_access.writes[0].path')"
  HYPHEN_SECOND_PATH="$(work_json_value work-a-b '.attempts[0].tasks[0].workspace_access.writes[0].path')"
  [ "$HYPHEN_FIRST_PATH" = "../work-6-work-a-b-c" ] || RESULT=1
  [ "$HYPHEN_SECOND_PATH" = "../work-8-work-a-b-c" ] || RESULT=1
  [ "$HYPHEN_FIRST_PATH" != "$HYPHEN_SECOND_PATH" ] || RESULT=1

  return $RESULT
}

test_task_run_creates_registered_sibling_worktree() {
  setup_test_project
  trap cleanup_test_project RETURN
  write_mock_codex

  RESULT=0
  "$FACTORY_BIN" work create work-alpha --title "Run task" > /dev/null
  "$FACTORY_BIN" work attempt work-alpha attempt-one > /dev/null
  run_write_task work-alpha attempt-one > "$TEST_DIR/stdout" 2> "$TEST_DIR/stderr" || RESULT=1

  WORKSPACE_PATH="../work-10-work-alpha-attempt-one"
  WORKSPACE_PWD="$(cd "$WORKSPACE_PATH" && pwd -P)"
  [ -f "$WORKSPACE_PATH/.git" ] || [ -d "$WORKSPACE_PATH/.git" ] || RESULT=1
  [ "$(cat "$TEST_DIR/coder-cwd.log")" = "$WORKSPACE_PWD" ] || RESULT=1
  git worktree list --porcelain | grep -F "worktree $WORKSPACE_PWD" > /dev/null || RESULT=1
  [ ! -e .factory/work/workspaces/attempt-one ] || RESULT=1

  return $RESULT
}

test_task_run_rejects_unmanaged_workspace_paths() {
  setup_test_project
  trap cleanup_test_project RETURN
  write_mock_codex

  RESULT=0
  "$FACTORY_BIN" work create work-alpha --title "Reject paths" > /dev/null
  "$FACTORY_BIN" work attempt work-alpha attempt-one > /dev/null

  TASK_RECORD=.factory/work/tasks/work-alpha/attempt-one/attempt-one-write.json

  jq '.workspace_access.writes[0].path = ".factory/work/workspaces/attempt-one"' \
    "$TASK_RECORD" > "$TEST_DIR/task.json"
  mv "$TEST_DIR/task.json" "$TASK_RECORD"
  assert_fails run_write_task work-alpha attempt-one || RESULT=1

  jq --arg path "$TEST_DIR/absolute-workspace" \
    '.workspace_access.writes[0].path = $path' \
    "$TASK_RECORD" > "$TEST_DIR/task.json"
  mv "$TEST_DIR/task.json" "$TASK_RECORD"
  assert_fails run_write_task work-alpha attempt-one || RESULT=1

  jq '.workspace_access.writes[0].path = "../work-10-work-alpha-other-attempt"' \
    "$TASK_RECORD" > "$TEST_DIR/task.json"
  mv "$TEST_DIR/task.json" "$TASK_RECORD"
  assert_fails run_write_task work-alpha attempt-one || RESULT=1

  [ ! -e .factory/work/workspaces/attempt-one ] || RESULT=1
  [ ! -e "$TEST_DIR/absolute-workspace" ] || RESULT=1
  [ ! -e ../work-10-work-alpha-other-attempt ] || RESULT=1

  return $RESULT
}

test_attempt_run_keeps_state_and_artifacts_in_source_checkout() {
  setup_test_project
  trap cleanup_test_project RETURN
  write_mock_codex

  RESULT=0
  "$FACTORY_BIN" work create work-alpha --title "Attempt run" > /dev/null
  "$FACTORY_BIN" work attempt work-alpha attempt-one > /dev/null
  TASK_RUN_MOCK_MODE=commit \
    PATH="${TEST_DIR}/bin:$PATH" \
    CODER_CWD_LOG="${TEST_DIR}/coder-cwd.log" \
    "$FACTORY_BIN" work attempt run --no-sandbox --coder codex \
      work-alpha attempt-one > "$TEST_DIR/stdout" 2> "$TEST_DIR/stderr" || RESULT=1

  [ -f .factory/work/items/work-alpha.json ] || RESULT=1
  [ -d .factory/work/artifacts/work-alpha/attempt-one ] || RESULT=1
  [ "$(work_json_value work-alpha '.merge_candidates[0].source_workspace.path')" = "../work-10-work-alpha-attempt-one" ] || RESULT=1
  [ -d ../work-10-work-alpha-attempt-one ] || RESULT=1
  [ ! -d ../work-10-work-alpha-attempt-one/.factory/work/artifacts ] || RESULT=1

  return $RESULT
}

test_documentation_describes_sibling_workspace_layout() {
  RESULT=0

  rg -n '\.factory/work/workspaces/<attempt-id>|\.factory/work/workspaces/' \
    "$PROJECT_DIR/documentation" \
    "$PROJECT_DIR/skills/build-in-the-factory/SKILL.md" \
    "$PROJECT_DIR/tests/behaviors/README.md" && RESULT=1

  rg -n '\.\./work-<work-item-id-byte-len>-<work-item-id>-<attempt-id>|\.\./work-6-work-1-attempt-1' \
    "$PROJECT_DIR/documentation" \
    "$PROJECT_DIR/skills/build-in-the-factory/SKILL.md" \
    "$PROJECT_DIR/tests/behaviors/README.md" > /dev/null || RESULT=1

  return $RESULT
}

printf 'test-work-sibling-workspaces\n\n'
run_test "attempt records sibling candidate path" test_attempt_records_sibling_candidate_path
run_test "attempt paths include Work Item to avoid collisions" test_attempt_paths_include_work_item_to_avoid_collisions
run_test "task run creates registered sibling worktree" test_task_run_creates_registered_sibling_worktree
run_test "task run rejects unmanaged workspace paths" test_task_run_rejects_unmanaged_workspace_paths
run_test "attempt run keeps state and artifacts in source checkout" test_attempt_run_keeps_state_and_artifacts_in_source_checkout
run_test "documentation describes sibling workspace layout" test_documentation_describes_sibling_workspace_layout

printf '\n  %s passed, %s failed\n' "$PASS" "$FAIL"
if [ "$FAIL" -ne 0 ]; then
  printf 'Failed tests:%b\n' "$ERRORS"
  exit 1
fi
