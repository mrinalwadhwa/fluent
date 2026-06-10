#!/usr/bin/env bash
# test-work-inspection - Verify Work Item intake and inspection commands.

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
    "  \"title\": \"${TITLE}\"" \
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

test_work_create_writes_minimal_item() {
  setup_test_project

  RESULT=0
  OUTPUT="$("$FACTORY_BIN" work create work-intake --title "Intake title" 2>&1)"
  assert_contains "$OUTPUT" "Created Work Item work-intake" || RESULT=1
  assert_contains "$(cat .factory/work/items/work-intake.json)" '"id": "work-intake"' || RESULT=1
  assert_contains "$(cat .factory/work/items/work-intake.json)" '"title": "Intake title"' || RESULT=1
  assert_not_contains "$(cat .factory/work/items/work-intake.json)" '"attempts"' || RESULT=1
  assert_not_contains "$(cat .factory/work/items/work-intake.json)" '"merge_candidates"' || RESULT=1
  SHOW_OUTPUT="$("$FACTORY_BIN" work show work-intake 2>&1)"
  assert_contains "$SHOW_OUTPUT" '"attempts": []' || RESULT=1

  cleanup_test_project
  return $RESULT
}

test_work_create_existing_item_fails() {
  setup_test_project
  write_work_item "work-existing" "Original title"

  RESULT=0
  assert_fails "$FACTORY_BIN" work create work-existing --title "Replacement title" || RESULT=1
  ERROR_OUTPUT="$(cat "$TEST_DIR/stderr")"
  assert_contains "$ERROR_OUTPUT" "already exists" || RESULT=1
  assert_contains "$(cat .factory/work/items/work-existing.json)" "Original title" || RESULT=1
  assert_not_contains "$(cat .factory/work/items/work-existing.json)" "Replacement title" || RESULT=1

  cleanup_test_project
  return $RESULT
}

test_work_create_invalid_id_fails() {
  setup_test_project

  RESULT=0
  assert_fails "$FACTORY_BIN" work create ../escape --title "Invalid title" || RESULT=1
  ERROR_OUTPUT="$(cat "$TEST_DIR/stderr")"
  assert_contains "$ERROR_OUTPUT" "cannot be used as a file name" || RESULT=1
  if [ -e .factory/work/items ]; then
    printf '    FAIL: invalid id created Work Item storage\n'
    RESULT=1
  fi

  cleanup_test_project
  return $RESULT
}

test_work_create_item_is_visible() {
  setup_test_project

  RESULT=0
  "$FACTORY_BIN" work create work-visible --title "Visible title" > /dev/null
  LIST_OUTPUT="$("$FACTORY_BIN" work list 2>&1)"
  assert_contains "$LIST_OUTPUT" "work-visible" || RESULT=1
  assert_contains "$LIST_OUTPUT" "Visible title" || RESULT=1
  SHOW_OUTPUT="$("$FACTORY_BIN" work show work-visible 2>&1)"
  assert_contains "$SHOW_OUTPUT" '"id": "work-visible"' || RESULT=1
  assert_contains "$SHOW_OUTPUT" '"title": "Visible title"' || RESULT=1

  cleanup_test_project
  return $RESULT
}

test_work_create_persists_instructions() {
  setup_test_project
  printf 'Brief: build the slice.\n\n- Preserve coder flags.\n' > "$TEST_DIR/instructions.md"

  RESULT=0
  "$FACTORY_BIN" work create work-guided \
    --title "Guided work" \
    --instructions-file "$TEST_DIR/instructions.md" > /dev/null
  SHOW_OUTPUT="$("$FACTORY_BIN" work show work-guided 2>&1)"
  assert_contains "$SHOW_OUTPUT" '"instructions": "Brief: build the slice.\n\n- Preserve coder flags.\n"' || RESULT=1

  cleanup_test_project
  return $RESULT
}

test_work_attempt_adds_initial_write_task() {
  setup_test_project
  write_work_item "work-1" "Attempt intake"

  RESULT=0
  OUTPUT="$("$FACTORY_BIN" work attempt work-1 attempt-1 2>&1)"
  assert_contains "$OUTPUT" "Created Attempt attempt-1 for Work Item work-1" || RESULT=1
  SHOW_OUTPUT="$("$FACTORY_BIN" work show work-1 2>&1)"
  assert_contains "$SHOW_OUTPUT" '"id": "attempt-1"' || RESULT=1
  assert_contains "$SHOW_OUTPUT" '"work_item_id": "work-1"' || RESULT=1
  assert_contains "$SHOW_OUTPUT" '"status": "planned"' || RESULT=1
  assert_contains "$SHOW_OUTPUT" '"id": "attempt-1-write"' || RESULT=1
  assert_contains "$SHOW_OUTPUT" '"kind": "write"' || RESULT=1
  assert_contains "$SHOW_OUTPUT" '"role": "author"' || RESULT=1
  assert_contains "$SHOW_OUTPUT" '"id": "candidate"' || RESULT=1
  assert_contains "$SHOW_OUTPUT" '"path": "../work-6-work-1-attempt-1"' || RESULT=1

  cleanup_test_project
  return $RESULT
}

test_work_attempt_failure_modes_leave_item_unchanged() {
  setup_test_project
  write_work_item "work-1" "Attempt intake"

  RESULT=0
  BEFORE="$(cat .factory/work/items/work-1.json)"
  assert_fails "$FACTORY_BIN" work attempt missing-work attempt-1 || RESULT=1
  ERROR_OUTPUT="$(cat "$TEST_DIR/stderr")"
  assert_contains "$ERROR_OUTPUT" "missing-work" || RESULT=1
  assert_contains "$ERROR_OUTPUT" "not found" || RESULT=1
  assert_fails "$FACTORY_BIN" work attempt work-1 ../escape || RESULT=1
  ERROR_OUTPUT="$(cat "$TEST_DIR/stderr")"
  assert_contains "$ERROR_OUTPUT" "attempt id" || RESULT=1
  assert_contains "$ERROR_OUTPUT" "cannot be used as a file name" || RESULT=1
  AFTER="$(cat .factory/work/items/work-1.json)"
  if [ "$AFTER" != "$BEFORE" ]; then
    printf '    FAIL: failing attempt command changed Work Item\n'
    RESULT=1
  fi

  "$FACTORY_BIN" work attempt work-1 attempt-1 > /dev/null
  BEFORE="$(cat .factory/work/items/work-1.json)"
  assert_fails "$FACTORY_BIN" work attempt work-1 attempt-1 || RESULT=1
  ERROR_OUTPUT="$(cat "$TEST_DIR/stderr")"
  assert_contains "$ERROR_OUTPUT" "already exists" || RESULT=1
  AFTER="$(cat .factory/work/items/work-1.json)"
  if [ "$AFTER" != "$BEFORE" ]; then
    printf '    FAIL: duplicate attempt changed Work Item\n'
    RESULT=1
  fi

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

test_work_abandon_persists_reason() {
  setup_test_project
  "$FACTORY_BIN" work create work-stale --title "Stale work" > /dev/null
  "$FACTORY_BIN" work attempt work-stale attempt-1 > /dev/null

  OUTPUT="$("$FACTORY_BIN" work abandon work-stale --reason "replacement landed" 2>&1)"
  SHOW_OUTPUT="$("$FACTORY_BIN" work show work-stale 2>&1)"

  RESULT=0
  assert_contains "$OUTPUT" "Abandoned Work Item work-stale" || RESULT=1
  assert_contains "$SHOW_OUTPUT" '"abandonment": {' || RESULT=1
  assert_contains "$SHOW_OUTPUT" '"reason": "replacement landed"' || RESULT=1

  cleanup_test_project
  return $RESULT
}

test_work_abandon_missing_item_fails() {
  setup_test_project

  RESULT=0
  assert_fails "$FACTORY_BIN" work abandon missing-work --reason "obsolete" || RESULT=1
  ERROR_OUTPUT="$(cat "$TEST_DIR/stderr")"
  assert_contains "$ERROR_OUTPUT" "missing-work" || RESULT=1
  assert_contains "$ERROR_OUTPUT" "not found" || RESULT=1

  cleanup_test_project
  return $RESULT
}

test_work_abandon_active_item_fails_without_state_change() {
  setup_test_project
  "$FACTORY_BIN" work create work-active --title "Active work" > /dev/null
  "$FACTORY_BIN" work attempt work-active attempt-1 > /dev/null
  python3 - <<'PY'
import json
from pathlib import Path

attempt_path = Path(".factory/work/attempts/work-active/attempt-1.json")
attempt = json.loads(attempt_path.read_text())
attempt["status"] = "executing"
attempt_path.write_text(json.dumps(attempt, indent=2) + "\n")

task_path = Path(".factory/work/tasks/work-active/attempt-1/attempt-1-write.json")
task = json.loads(task_path.read_text())
task["status"] = "executing"
task_path.write_text(json.dumps(task, indent=2) + "\n")
PY

  RESULT=0
  assert_fails "$FACTORY_BIN" work abandon work-active --reason "obsolete" || RESULT=1
  ERROR_OUTPUT="$(cat "$TEST_DIR/stderr")"
  assert_contains "$ERROR_OUTPUT" "cannot be abandoned" || RESULT=1
  assert_not_contains "$(cat .factory/work/items/work-active.json)" "abandonment" || RESULT=1

  cleanup_test_project
  return $RESULT
}

test_work_abandon_reviewing_attempt_fails_without_state_change() {
  setup_test_project
  "$FACTORY_BIN" work create work-active --title "Active review" > /dev/null
  "$FACTORY_BIN" work attempt work-active attempt-1 > /dev/null
  python3 - <<'PY'
import json
from pathlib import Path

attempt_path = Path(".factory/work/attempts/work-active/attempt-1.json")
attempt = json.loads(attempt_path.read_text())
attempt["status"] = "reviewing"
attempt_path.write_text(json.dumps(attempt, indent=2) + "\n")
PY

  RESULT=0
  assert_fails "$FACTORY_BIN" work abandon work-active --reason "obsolete" || RESULT=1
  ERROR_OUTPUT="$(cat "$TEST_DIR/stderr")"
  assert_contains "$ERROR_OUTPUT" "cannot be abandoned" || RESULT=1
  assert_not_contains "$(cat .factory/work/items/work-active.json)" "abandonment" || RESULT=1

  cleanup_test_project
  return $RESULT
}

test_work_abandon_executing_task_fails_without_state_change() {
  setup_test_project
  "$FACTORY_BIN" work create work-active --title "Active task" > /dev/null
  "$FACTORY_BIN" work attempt work-active attempt-1 > /dev/null
  python3 - <<'PY'
import json
from pathlib import Path

attempt_path = Path(".factory/work/attempts/work-active/attempt-1.json")
attempt = json.loads(attempt_path.read_text())
attempt["status"] = "failed"
attempt_path.write_text(json.dumps(attempt, indent=2) + "\n")

task_path = Path(".factory/work/tasks/work-active/attempt-1/attempt-1-write.json")
task = json.loads(task_path.read_text())
task["status"] = "executing"
task_path.write_text(json.dumps(task, indent=2) + "\n")
PY

  RESULT=0
  assert_fails "$FACTORY_BIN" work abandon work-active --reason "obsolete" || RESULT=1
  ERROR_OUTPUT="$(cat "$TEST_DIR/stderr")"
  assert_contains "$ERROR_OUTPUT" "cannot be abandoned" || RESULT=1
  assert_not_contains "$(cat .factory/work/items/work-active.json)" "abandonment" || RESULT=1

  cleanup_test_project
  return $RESULT
}

test_work_abandon_active_merge_candidate_fails_without_state_change() {
  setup_test_project
  "$FACTORY_BIN" work create work-active --title "Active merge candidate" > /dev/null
  "$FACTORY_BIN" work attempt work-active attempt-1 > /dev/null
  python3 - <<'PY'
import json
from pathlib import Path

attempt_path = Path(".factory/work/attempts/work-active/attempt-1.json")
attempt = json.loads(attempt_path.read_text())
attempt["status"] = "complete"
attempt["review_state"] = "passed"
attempt_path.write_text(json.dumps(attempt, indent=2) + "\n")

task_path = Path(".factory/work/tasks/work-active/attempt-1/attempt-1-write.json")
task = json.loads(task_path.read_text())
task["status"] = "complete"
task["output"] = {
    "workspace_id": "candidate",
    "workspace_path": "../work-6-work-active-attempt-1",
    "source_branch": "main",
    "commit": "abc123",
}
task_path.write_text(json.dumps(task, indent=2) + "\n")
PY
  mkdir -p .factory/work/merge-candidates/work-active
  printf '%s\n' \
    '{' \
    '  "id": "candidate-1",' \
    '  "attempt_id": "attempt-1",' \
    '  "source_workspace": {' \
    '    "id": "candidate",' \
    '    "path": "../work-6-work-active-attempt-1"' \
    '  },' \
    '  "target_workspace": {' \
    '    "id": "target",' \
    '    "path": "."' \
    '  },' \
    '  "source_branch": "main",' \
    '  "target_branch": "main",' \
    '  "candidate_commit": "abc123",' \
    '  "review_state": "reviewing",' \
    '  "merge_state": {' \
    '    "status": "pending"' \
    '  }' \
    '}' > .factory/work/merge-candidates/work-active/candidate-1.json

  RESULT=0
  assert_fails "$FACTORY_BIN" work abandon work-active --reason "obsolete" || RESULT=1
  ERROR_OUTPUT="$(cat "$TEST_DIR/stderr")"
  assert_contains "$ERROR_OUTPUT" "cannot be abandoned" || RESULT=1
  assert_not_contains "$(cat .factory/work/items/work-active.json)" "abandonment" || RESULT=1

  python3 - <<'PY'
import json
from pathlib import Path

candidate_path = Path(".factory/work/merge-candidates/work-active/candidate-1.json")
candidate = json.loads(candidate_path.read_text())
candidate["review_state"] = "pending"
candidate["merge_state"]["status"] = "executing"
candidate_path.write_text(json.dumps(candidate, indent=2) + "\n")
PY

  assert_fails "$FACTORY_BIN" work abandon work-active --reason "obsolete" || RESULT=1
  ERROR_OUTPUT="$(cat "$TEST_DIR/stderr")"
  assert_contains "$ERROR_OUTPUT" "cannot be abandoned" || RESULT=1
  assert_not_contains "$(cat .factory/work/items/work-active.json)" "abandonment" || RESULT=1

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
    '  "title": "Invalid id"' \
    '}' > .factory/work/items/bad-id.json
  rm .factory/work/items/broken-json.json

  assert_fails "$FACTORY_BIN" work list || RESULT=1
  ERROR_OUTPUT="$(cat "$TEST_DIR/stderr")"
  assert_contains "$ERROR_OUTPUT" ".factory/work/items/bad-id.json" || RESULT=1

  printf '%s\n' \
    '{' \
    '  "id": "work-invalid",' \
    '  "title": "Invalid model"' \
    '}' > .factory/work/items/work-invalid.json
  mkdir -p .factory/work/attempts/work-invalid
  printf '%s\n' \
    '{' \
    '  "id": "attempt-1",' \
    '  "work_item_id": "other-work",' \
    '  "order": 0,' \
    '  "status": "planned"' \
    '}' > .factory/work/attempts/work-invalid/attempt-1.json
  rm .factory/work/items/bad-id.json

  assert_fails "$FACTORY_BIN" work list || RESULT=1
  ERROR_OUTPUT="$(cat "$TEST_DIR/stderr")"
  assert_contains "$ERROR_OUTPUT" ".factory/work/attempts/work-invalid/attempt-1.json" || RESULT=1

  cleanup_test_project
  return $RESULT
}

test_work_list_id_mismatch_fails() {
  setup_test_project
  mkdir -p .factory/work/items
  printf '%s\n' \
    '{' \
    '  "id": "work-object",' \
    '  "title": "Mismatched id"' \
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
  assert_contains "$STATUS_OUTPUT" "No Work Items found" || RESULT=1
  assert_not_contains "$STATUS_OUTPUT" "run-legacy" || RESULT=1

  STATUS_OUTPUT="$("$FACTORY_BIN" status --runs 2>&1)"
  assert_contains "$STATUS_OUTPUT" "run-legacy" || RESULT=1
  assert_contains "$STATUS_OUTPUT" "complete" || RESULT=1

  WORK_OUTPUT="$("$FACTORY_BIN" work list 2>&1)"
  assert_contains "$WORK_OUTPUT" "No Work Items found" || RESULT=1
  assert_not_contains "$WORK_OUTPUT" "run-legacy" || RESULT=1

  "$FACTORY_BIN" work create work-from-run-project --title "Work from planning" > /dev/null
  WORK_OUTPUT="$("$FACTORY_BIN" work list 2>&1)"
  assert_contains "$WORK_OUTPUT" "work-from-run-project" || RESULT=1
  assert_not_contains "$WORK_OUTPUT" "run-legacy" || RESULT=1

  cleanup_test_project
  return $RESULT
}

printf 'test-work-inspection\n\n'

run_test "work create writes minimal Work Item" test_work_create_writes_minimal_item
run_test "work create existing item fails" test_work_create_existing_item_fails
run_test "work create invalid id fails" test_work_create_invalid_id_fails
run_test "work create item is visible" test_work_create_item_is_visible
run_test "work create persists instructions" test_work_create_persists_instructions
run_test "work attempt adds initial write Task" test_work_attempt_adds_initial_write_task
run_test "work attempt failures leave item unchanged" test_work_attempt_failure_modes_leave_item_unchanged
run_test "work list prints stored Work Items" test_work_list_outputs_stored_items
run_test "work list prints empty state" test_work_list_empty_state_succeeds
run_test "work show prints pretty JSON" test_work_show_outputs_pretty_json
run_test "work show missing item fails" test_work_show_missing_item_fails
run_test "work abandon persists reason" test_work_abandon_persists_reason
run_test "work abandon missing item fails" test_work_abandon_missing_item_fails
run_test "work abandon active item fails without state change" \
  test_work_abandon_active_item_fails_without_state_change
run_test "work abandon reviewing attempt fails without state change" \
  test_work_abandon_reviewing_attempt_fails_without_state_change
run_test "work abandon executing task fails without state change" \
  test_work_abandon_executing_task_fails_without_state_change
run_test "work abandon active merge candidate fails without state change" \
  test_work_abandon_active_merge_candidate_fails_without_state_change
run_test "work list reports invalid stored state" test_work_list_invalid_state_fails
run_test "work list reports id mismatch" test_work_list_id_mismatch_fails
run_test "legacy runs and work inspection are independent" test_runs_and_work_items_are_independent

printf '\n  %d passed, %d failed\n' "$PASS" "$FAIL"

if [ "$FAIL" -gt 0 ]; then
  printf '\n  Failures:%b\n' "$ERRORS"
  exit 1
fi
