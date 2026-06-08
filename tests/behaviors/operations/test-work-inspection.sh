#!/usr/bin/env bash
# test-work-inspection - Verify read-only Work Item inspection commands.

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
  TEST_DIR="$(mktemp -d -t factory-work-inspection-XXXXXX)"
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

write_work_item() {
  ITEM_ID="$1"
  TITLE="$2"
  mkdir -p .factory/work/items
  printf '%s\n' \
    '{' \
    "  \"id\": \"${ITEM_ID}\"," \
    "  \"title\": \"${TITLE}\"," \
    '  "attempts": []' \
    '}' > ".factory/work/items/${ITEM_ID}.json"
}

assert_contains() {
  if ! printf '%s' "$1" | grep -Fq "$2"; then
    printf '    FAIL: output does not contain "%s"\n' "$2"
    printf '    Output:\n%s\n' "$1"
    return 1
  fi
}

assert_not_contains() {
  if printf '%s' "$1" | grep -Fq "$2"; then
    printf '    FAIL: output unexpectedly contains "%s"\n' "$2"
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

test_work_list_outputs_stored_items() {
  setup_test_project
  write_work_item "work-alpha" "Alpha title"
  write_work_item "work-beta" "Beta title"

  OUTPUT="$("$FACTORY_BIN" work list 2>&1)"
  RESULT=0
  assert_contains "$OUTPUT" "work-alpha" || RESULT=1
  assert_contains "$OUTPUT" "Alpha title" || RESULT=1
  assert_contains "$OUTPUT" "work-beta" || RESULT=1
  assert_contains "$OUTPUT" "Beta title" || RESULT=1

  cleanup_test_project
  return $RESULT
}

test_work_list_empty_state_succeeds() {
  setup_test_project

  OUTPUT="$("$FACTORY_BIN" work list 2>&1)"
  RESULT=0
  assert_contains "$OUTPUT" "No Work Items found" || RESULT=1

  cleanup_test_project
  return $RESULT
}

test_work_show_outputs_pretty_json() {
  setup_test_project
  write_work_item "work-alpha" "Alpha title"

  OUTPUT="$("$FACTORY_BIN" work show work-alpha 2>&1)"
  RESULT=0
  assert_contains "$OUTPUT" '{' || RESULT=1
  assert_contains "$OUTPUT" '  "id": "work-alpha",' || RESULT=1
  assert_contains "$OUTPUT" '  "title": "Alpha title",' || RESULT=1
  assert_contains "$OUTPUT" '  "attempts": []' || RESULT=1
  assert_contains "$OUTPUT" '}' || RESULT=1

  cleanup_test_project
  return $RESULT
}

test_work_show_missing_item_fails() {
  setup_test_project

  RESULT=0
  assert_fails "$FACTORY_BIN" work show missing-work || RESULT=1
  ERROR_OUTPUT="$(cat "$TEST_DIR/stderr")"
  assert_contains "$ERROR_OUTPUT" "missing-work" || RESULT=1
  assert_contains "$ERROR_OUTPUT" "not found" || RESULT=1

  cleanup_test_project
  return $RESULT
}

test_work_list_invalid_state_fails() {
  setup_test_project
  mkdir -p .factory/work/items
  printf '{ invalid json\n' > .factory/work/items/broken-json.json

  RESULT=0
  assert_fails "$FACTORY_BIN" work list || RESULT=1
  ERROR_OUTPUT="$(cat "$TEST_DIR/stderr")"
  assert_contains "$ERROR_OUTPUT" ".factory/work/items/broken-json.json" || RESULT=1

  printf '%s\n' \
    '{' \
    '  "id": "bad/id",' \
    '  "title": "Invalid id",' \
    '  "attempts": []' \
    '}' > .factory/work/items/bad-id.json
  rm .factory/work/items/broken-json.json

  assert_fails "$FACTORY_BIN" work list || RESULT=1
  ERROR_OUTPUT="$(cat "$TEST_DIR/stderr")"
  assert_contains "$ERROR_OUTPUT" ".factory/work/items/bad-id.json" || RESULT=1

  printf '%s\n' \
    '{' \
    '  "id": "work-invalid",' \
    '  "title": "Invalid model",' \
    '  "attempts": [' \
    '    {' \
    '      "id": "attempt-1",' \
    '      "work_item_id": "other-work",' \
    '      "status": "planned",' \
    '      "tasks": []' \
    '    }' \
    '  ]' \
    '}' > .factory/work/items/work-invalid.json
  rm .factory/work/items/bad-id.json

  assert_fails "$FACTORY_BIN" work list || RESULT=1
  ERROR_OUTPUT="$(cat "$TEST_DIR/stderr")"
  assert_contains "$ERROR_OUTPUT" ".factory/work/items/work-invalid.json" || RESULT=1

  cleanup_test_project
  return $RESULT
}

test_work_list_id_mismatch_fails() {
  setup_test_project
  mkdir -p .factory/work/items
  printf '%s\n' \
    '{' \
    '  "id": "work-object",' \
    '  "title": "Mismatched id",' \
    '  "attempts": []' \
    '}' > .factory/work/items/work-file.json

  RESULT=0
  assert_fails "$FACTORY_BIN" work list || RESULT=1
  ERROR_OUTPUT="$(cat "$TEST_DIR/stderr")"
  assert_contains "$ERROR_OUTPUT" ".factory/work/items/work-file.json" || RESULT=1

  cleanup_test_project
  return $RESULT
}

test_runs_and_work_items_are_independent() {
  setup_test_project
  mkdir -p .factory/runs/run-legacy
  printf 'run-legacy' > .factory/active-run
  printf 'complete' > .factory/runs/run-legacy/status
  printf 'local' > .factory/runs/run-legacy/runtime
  printf 'claude' > .factory/runs/run-legacy/coder
  printf 'Legacy run without Work Item state' > .factory/runs/run-legacy/brief.md

  RESULT=0
  STATUS_OUTPUT="$("$FACTORY_BIN" status 2>&1)"
  assert_contains "$STATUS_OUTPUT" "run-legacy" || RESULT=1
  assert_contains "$STATUS_OUTPUT" "complete" || RESULT=1

  WORK_OUTPUT="$("$FACTORY_BIN" work list 2>&1)"
  assert_contains "$WORK_OUTPUT" "No Work Items found" || RESULT=1
  assert_not_contains "$WORK_OUTPUT" "run-legacy" || RESULT=1

  cleanup_test_project
  return $RESULT
}

printf 'test-work-inspection\n\n'

run_test "work list prints stored Work Items" test_work_list_outputs_stored_items
run_test "work list prints empty state" test_work_list_empty_state_succeeds
run_test "work show prints pretty JSON" test_work_show_outputs_pretty_json
run_test "work show missing item fails" test_work_show_missing_item_fails
run_test "work list reports invalid stored state" test_work_list_invalid_state_fails
run_test "work list reports id mismatch" test_work_list_id_mismatch_fails
run_test "legacy runs and work inspection are independent" test_runs_and_work_items_are_independent

printf '\n  %d passed, %d failed\n' "$PASS" "$FAIL"

if [ "$FAIL" -gt 0 ]; then
  printf '\n  Failures:%b\n' "$ERRORS"
  exit 1
fi
