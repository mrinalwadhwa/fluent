#!/usr/bin/env bash
# test-work-status-dashboard - Verify Work state appears in status and dashboard.

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
  TEST_DIR="$(mktemp -d -t factory-work-status-dashboard-XXXXXX)"
  mkdir -p "$TEST_DIR/project" "$TEST_DIR/bin"
  cd "$TEST_DIR/project"
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
      grep -v "^${TEST_DIR}/project$" | while read -r wt; do
        git -C "${TEST_DIR}/project" worktree remove --force "$wt" 2>/dev/null || true
      done || true
  fi
  rm -rf "$TEST_DIR"
}

assert_contains() {
  if ! printf '%s' "$1" | grep -Fq -- "$2"; then
    printf '    FAIL: output does not contain "%s"\n' "$2"
    printf '    Output:\n%s\n' "$1"
    return 1
  fi
}

assert_not_contains() {
  if printf '%s' "$1" | grep -Fq -- "$2"; then
    printf '    FAIL: output unexpectedly contains "%s"\n' "$2"
    printf '    Output:\n%s\n' "$1"
    return 1
  fi
}

clean_dashboard_output() {
  perl -pe 's/\e\[[0-9;?]*[ -\/]*[@-~]//g; s/\r/\n/g'
}

clean_dashboard_output_tail() {
  clean_dashboard_output | perl -0777 -ne '$i = rindex($_, "FactoryDashboard"); print $i >= 0 ? substr($_, $i) : $_'
}

wait_for_dashboard_capture() {
  OUTPUT_FILE="$1"
  NEEDLE="$2"
  ATTEMPTS="${3:-100}"

  while [ "$ATTEMPTS" -gt 0 ]; do
    if grep -Fq -- "$NEEDLE" "$OUTPUT_FILE" 2>/dev/null; then
      return 0
    fi
    ATTEMPTS=$((ATTEMPTS - 1))
    sleep 0.1
  done

  return 1
}

capture_dashboard_default() {
  PROJECT_PATH="$1"
  KEYS="${2:-}"
  OUTPUT_FILE="$(mktemp -t factory-work-dashboard-output-XXXXXX)"

  (
    sleep 1
    if [ -n "$KEYS" ]; then
      printf '%b' "$KEYS"
      sleep 1
    fi
    printf 'q'
  ) | FACTORY_DASH_BIN="$FACTORY_BIN" \
      FACTORY_DASH_PROJECT="$PROJECT_PATH" \
      TERM=xterm \
      script -q "$OUTPUT_FILE" sh -c 'stty rows 30 cols 120; "$FACTORY_DASH_BIN" dashboard "$FACTORY_DASH_PROJECT"' >/dev/null 2>&1 || true

  cat "$OUTPUT_FILE"
  rm -f "$OUTPUT_FILE"
}

capture_dashboard_after_poll_mutation() {
  PROJECT_PATH="$1"
  MUTATION="$2"
  INITIAL_NEEDLE="$3"
  POLLED_NEEDLE="$4"
  OUTPUT_FILE="$(mktemp -t factory-work-dashboard-output-XXXXXX)"

  (
    if wait_for_dashboard_capture "$OUTPUT_FILE" "$INITIAL_NEEDLE" 200; then
      eval "$MUTATION"
      wait_for_dashboard_capture "$OUTPUT_FILE" "$POLLED_NEEDLE" 120 || true
    fi
    printf 'q'
  ) | FACTORY_DASH_BIN="$FACTORY_BIN" \
      FACTORY_DASH_PROJECT="$PROJECT_PATH" \
      TERM=xterm \
      script -q "$OUTPUT_FILE" sh -c 'stty rows 30 cols 120; "$FACTORY_DASH_BIN" dashboard "$FACTORY_DASH_PROJECT"' >/dev/null 2>&1 || true

  cat "$OUTPUT_FILE"
  rm -f "$OUTPUT_FILE"
}

create_planned_work_item() {
  "$FACTORY_BIN" work create work-visible --title "Visible Work" > /dev/null
  "$FACTORY_BIN" work attempt work-visible attempt-visible > /dev/null
}

write_mock_claude() {
  cat > "${TEST_DIR}/bin/claude" <<'MOCK_SCRIPT'
#!/usr/bin/env bash
case "$PWD" in
  */work-12-work-visible-attempt-visible|*/work-11-work-action-attempt-action)
    printf 'status dashboard output\n' > status-dashboard-output.txt
    git add status-dashboard-output.txt
    git commit -m "Add status dashboard output" > /dev/null 2>&1
    ;;
  *)
    printf 'Verdict: pass\n\nStatus dashboard review passed.\n' > review.md
    ;;
esac
exit 0
MOCK_SCRIPT
  chmod +x "${TEST_DIR}/bin/claude"
}

write_uncertain_mock_claude() {
  cat > "${TEST_DIR}/bin/claude" <<'MOCK_SCRIPT'
#!/usr/bin/env bash
case "$PWD" in
  */work-15-work-needs-user-attempt-needs-user)
    printf 'status dashboard uncertain output\n' > status-dashboard-uncertain.txt
    git add status-dashboard-uncertain.txt
    git commit -m "Add status dashboard uncertain output" > /dev/null 2>&1
    ;;
  *)
    printf 'Verdict: uncertain\n\nStatus dashboard review needs user input.\n' > review.md
    ;;
esac
exit 0
MOCK_SCRIPT
  chmod +x "${TEST_DIR}/bin/claude"
}

create_merge_ready_work_item() {
  write_mock_claude
  "$FACTORY_BIN" work create work-action --title "Actionable Work" > /dev/null
  "$FACTORY_BIN" work attempt work-action attempt-action > /dev/null
  PATH="${TEST_DIR}/bin:$PATH" \
    "$FACTORY_BIN" work attempt run work-action attempt-action --no-sandbox \
      > "$TEST_DIR/attempt-run-stdout" 2> "$TEST_DIR/attempt-run-stderr"
}

create_needs_user_work_item() {
  write_uncertain_mock_claude
  "$FACTORY_BIN" work create work-needs-user --title "Needs User Work" > /dev/null
  "$FACTORY_BIN" work attempt work-needs-user attempt-needs-user > /dev/null
  PATH="${TEST_DIR}/bin:$PATH" \
    "$FACTORY_BIN" work attempt run work-needs-user attempt-needs-user --no-sandbox \
      > "$TEST_DIR/needs-user-stdout" 2> "$TEST_DIR/needs-user-stderr"
}

test_status_hides_runs_by_default_and_prints_work_summary() {
  setup_test_project
  trap cleanup_test_project RETURN
  mkdir -p .factory/runs/run-legacy
  printf 'executing' > .factory/runs/run-legacy/status
  printf 'local' > .factory/runs/run-legacy/runtime
  printf 'Legacy run' > .factory/runs/run-legacy/brief.md
  create_planned_work_item

  RESULT=0
  OUTPUT="$("$FACTORY_BIN" status 2>&1)"
  assert_contains "$OUTPUT" "Work Items" || RESULT=1
  assert_contains "$OUTPUT" "work-visible" || RESULT=1
  assert_contains "$OUTPUT" "attempt-visible" || RESULT=1
  assert_not_contains "$OUTPUT" "run-legacy" || RESULT=1
  return $RESULT
}

test_status_runs_prints_legacy_runs_and_work_summary() {
  setup_test_project
  trap cleanup_test_project RETURN
  mkdir -p .factory/runs/run-legacy
  printf 'executing' > .factory/runs/run-legacy/status
  printf 'local' > .factory/runs/run-legacy/runtime
  printf 'Legacy run' > .factory/runs/run-legacy/brief.md
  create_planned_work_item

  RESULT=0
  OUTPUT="$("$FACTORY_BIN" status --runs 2>&1)"
  assert_contains "$OUTPUT" "run-legacy" || RESULT=1
  assert_contains "$OUTPUT" "executing" || RESULT=1
  assert_contains "$OUTPUT" "Work Items" || RESULT=1
  assert_contains "$OUTPUT" "work-visible" || RESULT=1
  assert_contains "$OUTPUT" "attempt-visible" || RESULT=1
  return $RESULT
}

test_status_prints_work_without_legacy_runs() {
  setup_test_project
  trap cleanup_test_project RETURN
  create_planned_work_item

  RESULT=0
  OUTPUT="$("$FACTORY_BIN" status 2>&1)"
  assert_contains "$OUTPUT" "Work Items" || RESULT=1
  assert_contains "$OUTPUT" "work-visible" || RESULT=1
  assert_not_contains "$OUTPUT" "No runs found" || RESULT=1
  return $RESULT
}

test_status_summarizes_work_model_vocabulary() {
  setup_test_project
  trap cleanup_test_project RETURN
  create_merge_ready_work_item

  RESULT=0
  OUTPUT="$("$FACTORY_BIN" status 2>&1)"
  assert_contains "$OUTPUT" "WORK" || RESULT=1
  assert_contains "$OUTPUT" "ATTEMPT" || RESULT=1
  assert_contains "$OUTPUT" "TASK" || RESULT=1
  assert_contains "$OUTPUT" "MERGE CANDIDATE" || RESULT=1
  assert_contains "$OUTPUT" "MERGE" || RESULT=1
  assert_contains "$OUTPUT" "attempt-action-merge-candidate" || RESULT=1
  assert_contains "$OUTPUT" "pending" || RESULT=1
  return $RESULT
}

test_status_surfaces_needs_user_state() {
  setup_test_project
  trap cleanup_test_project RETURN
  create_needs_user_work_item

  RESULT=0
  OUTPUT="$("$FACTORY_BIN" status 2>&1)"
  assert_contains "$OUTPUT" "work-needs-user" || RESULT=1
  assert_contains "$OUTPUT" "attempt-needs-user" || RESULT=1
  assert_contains "$OUTPUT" "needs-user" || RESULT=1
  return $RESULT
}

test_status_surfaces_abandoned_work_as_terminal() {
  setup_test_project
  trap cleanup_test_project RETURN
  "$FACTORY_BIN" work create work-abandoned --title "Abandoned Work" > /dev/null
  "$FACTORY_BIN" work attempt work-abandoned attempt-abandoned > /dev/null
  python3 - <<'PY'
import json
from pathlib import Path

attempt_path = Path(".factory/work/attempts/work-abandoned/attempt-abandoned.json")
attempt = json.loads(attempt_path.read_text())
attempt["status"] = "needs-user"
attempt_path.write_text(json.dumps(attempt, indent=2) + "\n")

task_path = Path(".factory/work/tasks/work-abandoned/attempt-abandoned/attempt-abandoned-write.json")
task = json.loads(task_path.read_text())
task["status"] = "needs-user"
task_path.write_text(json.dumps(task, indent=2) + "\n")
PY
  "$FACTORY_BIN" work abandon work-abandoned --reason "replacement landed" > /dev/null

  RESULT=0
  OUTPUT="$("$FACTORY_BIN" status 2>&1)"
  assert_contains "$OUTPUT" "work-abandoned" || RESULT=1
  assert_contains "$OUTPUT" "attempt-abandoned" || RESULT=1
  assert_contains "$OUTPUT" "abandoned" || RESULT=1
  return $RESULT
}

test_status_reports_invalid_work_without_hiding_valid_state() {
  setup_test_project
  trap cleanup_test_project RETURN
  mkdir -p .factory/runs/run-valid .factory/work/items
  printf 'complete' > .factory/runs/run-valid/status
  printf 'local' > .factory/runs/run-valid/runtime
  printf 'Valid legacy run' > .factory/runs/run-valid/brief.md
  create_planned_work_item
  printf '{ invalid json\n' > .factory/work/items/broken-work.json

  RESULT=0
  OUTPUT="$("$FACTORY_BIN" status 2>&1 || true)"
  assert_contains "$OUTPUT" "work-visible" || RESULT=1
  assert_contains "$OUTPUT" "Work Item read errors" || RESULT=1
  assert_contains "$OUTPUT" ".factory/work/items/broken-work.json" || RESULT=1
  assert_not_contains "$OUTPUT" "run-valid" || RESULT=1

  RUN_OUTPUT="$("$FACTORY_BIN" status --runs 2>&1 || true)"
  assert_contains "$RUN_OUTPUT" "run-valid" || RESULT=1
  assert_contains "$RUN_OUTPUT" "complete" || RESULT=1
  assert_contains "$RUN_OUTPUT" "work-visible" || RESULT=1
  assert_contains "$RUN_OUTPUT" ".factory/work/items/broken-work.json" || RESULT=1
  return $RESULT
}

test_dashboard_lists_work_items() {
  setup_test_project
  trap cleanup_test_project RETURN
  create_planned_work_item

  RESULT=0
  OUTPUT="$(capture_dashboard_default "$TEST_DIR/project" | clean_dashboard_output)"
  assert_contains "$OUTPUT" "Work Items (1)" || RESULT=1
  assert_contains "$OUTPUT" "work-visible - Visible Work" || RESULT=1
  assert_contains "$OUTPUT" "Attempt: attempt-visible [planned]" || RESULT=1
  assert_contains "$OUTPUT" "Task: write:attempt-visible-write-1 [planned]" || RESULT=1
  assert_contains "$OUTPUT" "Review: -" || RESULT=1
  assert_contains "$OUTPUT" "Merge Candidate: -" || RESULT=1
  assert_contains "$OUTPUT" "Merge: -" || RESULT=1
  return $RESULT
}

test_dashboard_shows_work_items_alongside_legacy_runs() {
  setup_test_project
  trap cleanup_test_project RETURN
  mkdir -p .factory/runs/run-legacy
  printf 'executing' > .factory/runs/run-legacy/status
  printf 'local' > .factory/runs/run-legacy/runtime
  printf 'Legacy run' > .factory/runs/run-legacy/brief.md
  create_planned_work_item

  RESULT=0
  WORK_OUTPUT="$(capture_dashboard_default "$TEST_DIR/project" | clean_dashboard_output)"
  assert_contains "$WORK_OUTPUT" "Runs (1)" || RESULT=1
  assert_contains "$WORK_OUTPUT" "Work Items (1)" || RESULT=1
  assert_contains "$WORK_OUTPUT" "work-visible - Visible Work" || RESULT=1
  assert_contains "$WORK_OUTPUT" "Attempt: attempt-visible [planned]" || RESULT=1

  RUN_OUTPUT="$(capture_dashboard_default "$TEST_DIR/project" "r" | clean_dashboard_output)"
  assert_contains "$RUN_OUTPUT" "Runs (1)" || RESULT=1
  assert_contains "$RUN_OUTPUT" "run-legacy" || RESULT=1
  assert_contains "$RUN_OUTPUT" "Work Items (1)" || RESULT=1
  return $RESULT
}

test_dashboard_refreshes_work_items_on_poll() {
  setup_test_project
  trap cleanup_test_project RETURN
  create_planned_work_item

  RESULT=0
  OUTPUT="$(capture_dashboard_after_poll_mutation \
    "$TEST_DIR/project" \
    "'$FACTORY_BIN' work create work-polled --title 'Polled Work' > /dev/null" \
    "work-visible - Visible Work" \
    "work-polled - Polled Work")"
  FINAL_OUTPUT="$(printf '%s' "$OUTPUT" | clean_dashboard_output_tail)"
  assert_contains "$FINAL_OUTPUT" "Work Items (2)" || RESULT=1
  assert_contains "$FINAL_OUTPUT" "work-polled - Polled Work" || RESULT=1
  return $RESULT
}

test_dashboard_surfaces_actionable_work() {
  setup_test_project
  trap cleanup_test_project RETURN
  create_merge_ready_work_item

  RESULT=0
  OUTPUT="$(capture_dashboard_default "$TEST_DIR/project" | clean_dashboard_output)"
  assert_contains "$OUTPUT" "Actionable" || RESULT=1
  assert_contains "$OUTPUT" "work-action - Actionable Work" || RESULT=1
  assert_contains "$OUTPUT" "merge-ready" || RESULT=1
  assert_contains "$OUTPUT" "Merge Candidate: attempt-action-merge-candidate" || RESULT=1
  assert_contains "$OUTPUT" "Merge: pending review:pending" || RESULT=1
  return $RESULT
}

test_dashboard_surfaces_needs_user_work() {
  setup_test_project
  trap cleanup_test_project RETURN
  create_needs_user_work_item

  RESULT=0
  OUTPUT="$(capture_dashboard_default "$TEST_DIR/project" | clean_dashboard_output)"
  assert_contains "$OUTPUT" "Actionable" || RESULT=1
  assert_contains "$OUTPUT" "work-needs-user - Needs User Work" || RESULT=1
  assert_contains "$OUTPUT" "needs-user" || RESULT=1
  assert_contains "$OUTPUT" "Attempt: attempt-needs-user [needs-user]" || RESULT=1
  return $RESULT
}

test_dashboard_reports_work_read_errors() {
  setup_test_project
  trap cleanup_test_project RETURN
  create_planned_work_item
  printf '{ invalid json\n' > .factory/work/items/broken-work.json

  RESULT=0
  OUTPUT="$(capture_dashboard_default "$TEST_DIR/project" | clean_dashboard_output)"
  assert_contains "$OUTPUT" "Work Items: 1" || RESULT=1
  assert_contains "$OUTPUT" "Errors: 1" || RESULT=1
  assert_contains "$OUTPUT" "Work Item read errors" || RESULT=1
  assert_contains "$OUTPUT" "failed to parse" || RESULT=1
  assert_contains "$OUTPUT" "work-visible - Visible Work" || RESULT=1
  return $RESULT
}

test_dashboard_shows_empty_work_view() {
  setup_test_project
  trap cleanup_test_project RETURN

  RESULT=0
  OUTPUT="$(capture_dashboard_default "$TEST_DIR/project" | clean_dashboard_output)"
  assert_contains "$OUTPUT" "Work Items (0)" || RESULT=1
  assert_contains "$OUTPUT" "No Work Items found" || RESULT=1
  return $RESULT
}

printf 'test-work-status-dashboard\n\n'

run_test "status hides runs by default and prints Work summary" \
  test_status_hides_runs_by_default_and_prints_work_summary
run_test "status --runs prints legacy runs and Work summary" \
  test_status_runs_prints_legacy_runs_and_work_summary
run_test "status prints Work summary without legacy runs" test_status_prints_work_without_legacy_runs
run_test "status summarizes Work model vocabulary" test_status_summarizes_work_model_vocabulary
run_test "status surfaces needs-user state" test_status_surfaces_needs_user_state
run_test "status surfaces abandoned Work as terminal" \
  test_status_surfaces_abandoned_work_as_terminal
run_test "status reports invalid Work without hiding valid state" test_status_reports_invalid_work_without_hiding_valid_state
run_test "dashboard lists Work Items" test_dashboard_lists_work_items
run_test "dashboard shows Work Items alongside legacy runs" test_dashboard_shows_work_items_alongside_legacy_runs
run_test "dashboard refreshes Work Items on poll" test_dashboard_refreshes_work_items_on_poll
run_test "dashboard surfaces actionable Work" test_dashboard_surfaces_actionable_work
run_test "dashboard surfaces needs-user Work" test_dashboard_surfaces_needs_user_work
run_test "dashboard reports Work read errors" test_dashboard_reports_work_read_errors
run_test "dashboard shows empty Work view" test_dashboard_shows_empty_work_view

printf '\n  %d passed, %d failed\n' "$PASS" "$FAIL"
if [ "$FAIL" -gt 0 ]; then
  printf '\n  Failures:%b\n' "$ERRORS"
  exit 1
fi
