#!/usr/bin/env bash
# test-work-attempt-intake-review - Verify Attempt intake from the CLI.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$(dirname "$(dirname "$SCRIPT_DIR")")")"
FACTORY_BIN="${FACTORY_BIN_OVERRIDE:-${PROJECT_DIR}/target/debug/factory}"

source "${PROJECT_DIR}/tests/lib/run_test.sh"
LOG_DIR="${PROJECT_DIR}/tests/output/$(basename "$0" .sh)"

setup_test_project() {
  TEST_DIR="$(mktemp -d -t factory-work-attempt-review-XXXXXX)"
  mkdir -p "$TEST_DIR/project"
  cd "$TEST_DIR/project"
  git init -b main > /dev/null 2>&1
  git config commit.gpgsign false
  git config user.email "test@test"
  git config user.name "test"
  printf 'test\n' > README.md
  git add . && git commit -m "init" > /dev/null 2>&1
}

cleanup_test_project() {
  cd /
  rm -rf "$TEST_DIR"
}

assert_fails() {
  if "$@" > "$TEST_DIR/stdout" 2> "$TEST_DIR/stderr"; then
    printf '    FAIL: command unexpectedly succeeded: %s\n' "$*"
    printf '    Stdout:\n%s\n' "$(cat "$TEST_DIR/stdout")"
    printf '    Stderr:\n%s\n' "$(cat "$TEST_DIR/stderr")"
    return 1
  fi
}

create_work_item() {
  "$FACTORY_BIN" work create work-1 --title "Attempt intake" > /dev/null
}

json_value() {
  jq -r "$1" .factory/work/items/work-1.json
}

show_json_value() {
  "$FACTORY_BIN" work show work-1 | jq -r "$1"
}

attempt_json_value() {
  ATTEMPT_ID="$1"
  QUERY="$2"
  jq -r "$QUERY" ".factory/work/attempts/work-1/${ATTEMPT_ID}.json"
}

task_json_value() {
  ATTEMPT_ID="$1"
  TASK_ID="$2"
  QUERY="$3"
  jq -r "$QUERY" ".factory/work/tasks/work-1/${ATTEMPT_ID}/${TASK_ID}.json"
}

test_attempt_adds_planned_attempt() {
  setup_test_project
  create_work_item

  RESULT=0
  "$FACTORY_BIN" work attempt work-1 attempt-1 > /dev/null
  [ "$(attempt_json_value attempt-1 '.id')" = "attempt-1" ] || RESULT=1
  [ "$(attempt_json_value attempt-1 '.work_item_id')" = "work-1" ] || RESULT=1
  [ "$(attempt_json_value attempt-1 '.status')" = "planned" ] || RESULT=1
  [ "$(show_json_value '.attempts | length')" = "1" ] || RESULT=1
  [ "$(show_json_value '.attempts[0].id')" = "attempt-1" ] || RESULT=1

  cleanup_test_project
  return $RESULT
}

test_attempt_appends_to_existing_attempts() {
  setup_test_project
  create_work_item

  RESULT=0
  "$FACTORY_BIN" work attempt work-1 attempt-1 > /dev/null
  "$FACTORY_BIN" work attempt work-1 attempt-2 > /dev/null
  [ "$(attempt_json_value attempt-1 '.order')" = "0" ] || RESULT=1
  [ "$(attempt_json_value attempt-2 '.order')" = "1" ] || RESULT=1
  [ "$(show_json_value '.attempts | length')" = "2" ] || RESULT=1
  [ "$(show_json_value '.attempts[0].id')" = "attempt-1" ] || RESULT=1
  [ "$(show_json_value '.attempts[1].id')" = "attempt-2" ] || RESULT=1
  [ "$(show_json_value '.attempts[1].tasks | length')" = "1" ] || RESULT=1
  [ "$(task_json_value attempt-2 attempt-2-write-1 '.id')" = "attempt-2-write-1" ] || RESULT=1
  [ "$(task_json_value attempt-2 attempt-2-write-1 '.attempt_id')" = "attempt-2" ] || RESULT=1
  [ "$(task_json_value attempt-2 attempt-2-write-1 '.workspace_access.writes[0].path')" = "../work-6-work-1-attempt-2" ] || RESULT=1

  cleanup_test_project
  return $RESULT
}

test_attempt_adds_one_initial_write_task() {
  setup_test_project
  create_work_item

  RESULT=0
  "$FACTORY_BIN" work attempt work-1 attempt-1 > /dev/null
  [ "$(show_json_value '.attempts[0].tasks | length')" = "1" ] || RESULT=1
  [ "$(task_json_value attempt-1 attempt-1-write-1 '.kind')" = "write" ] || RESULT=1

  cleanup_test_project
  return $RESULT
}

test_initial_write_task_has_ids_and_one_writable_workspace() {
  setup_test_project
  create_work_item

  RESULT=0
  "$FACTORY_BIN" work attempt work-1 attempt-1 > /dev/null
  [ "$(task_json_value attempt-1 attempt-1-write-1 '.work_item_id')" = "work-1" ] || RESULT=1
  [ "$(task_json_value attempt-1 attempt-1-write-1 '.attempt_id')" = "attempt-1" ] || RESULT=1
  [ "$(task_json_value attempt-1 attempt-1-write-1 '.workspace_access.reads | length')" = "0" ] || RESULT=1
  [ "$(task_json_value attempt-1 attempt-1-write-1 '.workspace_access.writes | length')" = "1" ] || RESULT=1

  cleanup_test_project
  return $RESULT
}

test_missing_work_item_does_not_create_state() {
  setup_test_project

  RESULT=0
  assert_fails "$FACTORY_BIN" work attempt missing-work attempt-1 || RESULT=1
  if [ -e .factory/work/items/missing-work.json ]; then
    printf '    FAIL: missing Work Item command created Work Item state\n'
    RESULT=1
  fi

  cleanup_test_project
  return $RESULT
}

test_duplicate_attempt_id_leaves_item_unchanged() {
  setup_test_project
  create_work_item
  "$FACTORY_BIN" work attempt work-1 attempt-1 > /dev/null

  RESULT=0
  BEFORE="$(cat .factory/work/items/work-1.json)"
  assert_fails "$FACTORY_BIN" work attempt work-1 attempt-1 || RESULT=1
  AFTER="$(cat .factory/work/items/work-1.json)"
  [ "$AFTER" = "$BEFORE" ] || RESULT=1

  cleanup_test_project
  return $RESULT
}

test_invalid_ids_leave_work_item_state_unchanged() {
  setup_test_project
  create_work_item

  RESULT=0
  BEFORE="$(cat .factory/work/items/work-1.json)"
  assert_fails "$FACTORY_BIN" work attempt ../escape attempt-1 || RESULT=1
  assert_fails "$FACTORY_BIN" work attempt work-1 ../escape || RESULT=1
  AFTER="$(cat .factory/work/items/work-1.json)"
  [ "$AFTER" = "$BEFORE" ] || RESULT=1

  cleanup_test_project
  return $RESULT
}

test_show_prints_attempt_and_task_as_pretty_json() {
  setup_test_project
  create_work_item
  "$FACTORY_BIN" work attempt work-1 attempt-1 > /dev/null

  RESULT=0
  "$FACTORY_BIN" work show work-1 > "$TEST_DIR/show.json"
  grep -q '^{' "$TEST_DIR/show.json" || RESULT=1
  grep -q '^  "id": "work-1"' "$TEST_DIR/show.json" || RESULT=1
  [ "$(jq -r '.id' "$TEST_DIR/show.json")" = "work-1" ] || RESULT=1
  [ "$(jq -r '.title' "$TEST_DIR/show.json")" = "Attempt intake" ] || RESULT=1
  [ "$(jq -r '.attempts[0].id' "$TEST_DIR/show.json")" = "attempt-1" ] || RESULT=1
  [ "$(jq -r '.attempts[0].status' "$TEST_DIR/show.json")" = "planned" ] || RESULT=1
  [ "$(jq -r '.attempts[0].tasks[0].id' "$TEST_DIR/show.json")" = "attempt-1-write-1" ] || RESULT=1
  [ "$(jq -r '.attempts[0].tasks[0].kind' "$TEST_DIR/show.json")" = "write" ] || RESULT=1
  [ "$(jq -r '.attempts[0].tasks[0].role' "$TEST_DIR/show.json")" = "author" ] || RESULT=1
  [ "$(jq -r '.attempts[0].tasks[0].workspace_access.writes[0].path' "$TEST_DIR/show.json")" = "../work-6-work-1-attempt-1" ] || RESULT=1
  [ "$(jq -r '.attempts[0].artifacts | length' "$TEST_DIR/show.json")" = "0" ] || RESULT=1

  cleanup_test_project
  return $RESULT
}

printf 'test-work-attempt-intake-review\n\n'
run_test "attempt adds planned Attempt" test_attempt_adds_planned_attempt
run_test "attempt appends to existing Attempts" test_attempt_appends_to_existing_attempts
run_test "attempt adds one initial write Task" test_attempt_adds_one_initial_write_task
run_test "initial write Task has ids and one writable workspace" test_initial_write_task_has_ids_and_one_writable_workspace
run_test "missing Work Item does not create state" test_missing_work_item_does_not_create_state
run_test "duplicate Attempt id leaves item unchanged" test_duplicate_attempt_id_leaves_item_unchanged
run_test "invalid ids leave Work Item state unchanged" test_invalid_ids_leave_work_item_state_unchanged
run_test "work show prints Attempt and Task as pretty JSON" test_show_prints_attempt_and_task_as_pretty_json

summarize_and_exit
