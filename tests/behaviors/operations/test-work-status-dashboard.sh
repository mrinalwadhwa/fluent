#!/usr/bin/env bash
# test-work-status-dashboard - Verify Work state appears in status and dashboard.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$(dirname "$(dirname "$SCRIPT_DIR")")")"
FLUENT_BIN="${FLUENT_BIN_OVERRIDE:-${PROJECT_DIR}/target/debug/fluent}"

source "${PROJECT_DIR}/tests/lib/run_test.sh"
source "${PROJECT_DIR}/tests/lib/work_test_fixtures.sh"
LOG_DIR="${PROJECT_DIR}/tests/output/$(basename "$0" .sh)"

setup_test_project() {
  TEST_DIR="$(mktemp -d -t fluent-work-status-dashboard-XXXXXX)"
  mkdir -p "$TEST_DIR/project" "$TEST_DIR/bin"
  install_review_inspection_provider_helper "$TEST_DIR/bin"
  cd "$TEST_DIR/project"
  git init -b main > /dev/null 2>&1
  git config commit.gpgsign false
  git config user.email "test@test"
  git config user.name "test"
  printf 'test\n' > README.md
  seed_review_skill_stubs "."
  seed_tester_config "."
  git add . && git commit -m "init" > /dev/null 2>&1
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
  if [[ "$1" != *"$2"* ]]; then
    printf '    FAIL: output does not contain "%s"\n' "$2"
    printf '    Output:\n%s\n' "$1"
    return 1
  fi
}


# Exceed a pipe buffer so an early grep match cannot hide SIGPIPE.
test_assert_contains_large_literal_output() {
  local output
  output="$(printf 'literal [*].\n'; printf '%1048576s' ''; printf '\nend')"
  assert_contains "$output" 'literal [*].' || return 1
  assert_contains "$output" 'end' || return 1
  if assert_contains "$output" 'missing' >/dev/null; then
    return 1
  fi
  if assert_contains "$output" 'literal wildcard' >/dev/null; then
    return 1
  fi
  assert_not_contains "$output" 'missing' || return 1
  if assert_not_contains "$output" 'literal [*].' >/dev/null; then
    return 1
  fi
}

assert_not_contains() {
  if [[ "$1" == *"$2"* ]]; then
    printf '    FAIL: output unexpectedly contains "%s"\n' "$2"
    printf '    Output:\n%s\n' "$1"
    return 1
  fi
}

clean_dashboard_output() {
  # Reconstruct frames before removing spaces: later draws update changed cells.
  python3 -c '
import runpy
import sys
Screen = runpy.run_path(sys.argv[1])["Screen"]
screen = Screen(120, 30)
# Ratatui hides the cursor after each completed frame. Retain each screen so
# assertions can inspect both the Work list and a later errors overlay.
for frame in sys.stdin.buffer.read().split(b"\x1b[?25l"):
    screen.feed(frame)
    print(screen.text())
' "$PROJECT_DIR/tests/fixtures/dashboard-journey.py" |
    perl -CSDA -pe 's/[\s\x{2502}\x{2500}]//g'
}

clean_dashboard_output_tail() {
  clean_dashboard_output | perl -0777 -ne '$i = rindex($_, "Fluentwork"); print $i >= 0 ? substr($_, $i) : $_'
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
  local project_path="$1"
  local keys="${2:-}"
  HOME="$TEST_DIR" python3 "$PROJECT_DIR/tests/fixtures/dashboard-journey.py" \
    "$FLUENT_BIN" "$project_path" capture "$(printf '%b' "$keys")"
}

test_dashboard_capture_bounds_child_lifetime() {
  python3 "$PROJECT_DIR/tests/fixtures/dashboard-capture-tests.py"
}

capture_dashboard_after_poll_mutation() {
  PROJECT_PATH="$1"
  MUTATION="$2"
  INITIAL_NEEDLE="$3"
  POLLED_NEEDLE="$4"
  OUTPUT_FILE="$(mktemp -t fluent-work-dashboard-output-XXXXXX)"

  (
    if wait_for_dashboard_capture "$OUTPUT_FILE" "$INITIAL_NEEDLE" 200; then
      eval "$MUTATION"
      wait_for_dashboard_capture "$OUTPUT_FILE" "$POLLED_NEEDLE" 120 || true
    fi
    printf 'q'
  ) | HOME="$TEST_DIR" FLUENT_DASH_BIN="$FLUENT_BIN" \
      FLUENT_DASH_PROJECT="$PROJECT_PATH" \
      TERM=xterm \
      script -q "$OUTPUT_FILE" sh -c 'stty rows 30 cols 120; "$FLUENT_DASH_BIN" dashboard "$FLUENT_DASH_PROJECT"' >/dev/null 2>&1 || true

  cat "$OUTPUT_FILE"
  rm -f "$OUTPUT_FILE"
}

create_planned_work_item() {
  "$FLUENT_BIN" work-item create work-visible --title "Visible Work" > /dev/null
  "$FLUENT_BIN" attempt create work-visible attempt-visible > /dev/null
}

write_mock_claude() {
  cat > "${TEST_DIR}/bin/claude" <<'MOCK_SCRIPT'
#!/usr/bin/env bash
trap 'status=$?; fluent-test-review-inspection claude "$status" "$@" || exit $?; exit "$status"' EXIT
if [ "$1 $2 $3" = "auth status --json" ]; then
  printf '{"loggedIn":true}\n'
  exit 0
fi
if printf '%s' "$*" | grep -q 'follow-up draft as JSON'; then
  DRAFT_PATH="$(printf '%s' "$*" | grep -oE '/[^[:space:]]*follow-up-draft\.json' | head -1)"
  if [ -n "$DRAFT_PATH" ]; then
    mkdir -p "$(dirname "$DRAFT_PATH")"
    printf '{"learning_summary":"status dashboard learning","follow_ups":[]}\n' > "$DRAFT_PATH"
  fi
  exit 0
fi
case "$PWD" in
  */work-*-work-visible-attempt-visible|*/work-*-work-action-attempt-action)
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
trap 'status=$?; fluent-test-review-inspection claude "$status" "$@" || exit $?; exit "$status"' EXIT
if [ "$1 $2 $3" = "auth status --json" ]; then
  printf '{"loggedIn":true}\n'
  exit 0
fi
case "$PWD" in
  */work-*-work-needs-user-attempt-needs-user)
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
  "$FLUENT_BIN" work-item create work-action --title "Actionable Work" > /dev/null
  "$FLUENT_BIN" attempt create work-action attempt-action > /dev/null
  if ! PATH="${TEST_DIR}/bin:$PATH" \
    "$FLUENT_BIN" attempt run work-action attempt-action --no-sandbox \
      > "$TEST_DIR/attempt-run-stdout" 2> "$TEST_DIR/attempt-run-stderr"; then
    cat "$TEST_DIR/attempt-run-stderr" >&2
    return 1
  fi
}

create_needs_user_work_item() {
  write_uncertain_mock_claude
  "$FLUENT_BIN" work-item create work-needs-user --title "Needs User Work" > /dev/null
  "$FLUENT_BIN" attempt create work-needs-user attempt-needs-user > /dev/null
  PATH="${TEST_DIR}/bin:$PATH" \
    "$FLUENT_BIN" attempt run work-needs-user attempt-needs-user --no-sandbox \
      > "$TEST_DIR/needs-user-stdout" 2> "$TEST_DIR/needs-user-stderr"
}

test_status_prints_work_items() {
  setup_test_project
  trap cleanup_test_project RETURN
  create_planned_work_item

  RESULT=0
  OUTPUT="$("$FLUENT_BIN" status 2>&1)"
  assert_contains "$OUTPUT" "Work Items" || RESULT=1
  assert_contains "$OUTPUT" "work-visible" || RESULT=1
  assert_contains "$OUTPUT" "attempt-visible" || RESULT=1
  return $RESULT
}

test_status_summarizes_work_model_vocabulary() {
  setup_test_project
  trap cleanup_test_project RETURN
  create_merge_ready_work_item

  RESULT=0
  OUTPUT="$("$FLUENT_BIN" status 2>&1)"
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
  OUTPUT="$("$FLUENT_BIN" status 2>&1)"
  assert_contains "$OUTPUT" "work-needs-user" || RESULT=1
  assert_contains "$OUTPUT" "attempt-needs-user" || RESULT=1
  assert_contains "$OUTPUT" "needs-user" || RESULT=1
  return $RESULT
}

test_status_surfaces_abandoned_work_as_terminal() {
  setup_test_project
  trap cleanup_test_project RETURN
  "$FLUENT_BIN" work-item create work-abandoned --title "Abandoned Work" > /dev/null
  "$FLUENT_BIN" attempt create work-abandoned attempt-abandoned > /dev/null
  python3 - <<'PY'
import json
from pathlib import Path

attempt_path = Path(".fluent/work/attempts/work-abandoned/attempt-abandoned.json")
attempt = json.loads(attempt_path.read_text())
attempt["status"] = "needs-user"
attempt_path.write_text(json.dumps(attempt, indent=2) + "\n")

task_path = Path(".fluent/work/tasks/work-abandoned/attempt-abandoned/attempt-abandoned-write-1.json")
task = json.loads(task_path.read_text())
task["status"] = "needs-user"
task_path.write_text(json.dumps(task, indent=2) + "\n")
PY
  "$FLUENT_BIN" work-item abandon work-abandoned --reason "replacement landed" > /dev/null

  RESULT=0
  OUTPUT="$("$FLUENT_BIN" status 2>&1)"
  assert_contains "$OUTPUT" "work-abandoned" || RESULT=1
  assert_contains "$OUTPUT" "attempt-abandoned" || RESULT=1
  assert_contains "$OUTPUT" "abandoned" || RESULT=1
  return $RESULT
}

test_status_reports_invalid_work_without_hiding_valid_state() {
  setup_test_project
  trap cleanup_test_project RETURN
  mkdir -p .fluent/work/items
  create_planned_work_item
  printf '{ invalid json\n' > .fluent/work/items/broken-work.json

  RESULT=0
  OUTPUT="$("$FLUENT_BIN" status 2>&1 || true)"
  assert_contains "$OUTPUT" "work-visible" || RESULT=1
  assert_contains "$OUTPUT" "Work Item read errors" || RESULT=1
  assert_contains "$OUTPUT" ".fluent/work/items/broken-work.json" || RESULT=1
  return $RESULT
}

test_dashboard_lists_work_items() {
  setup_test_project
  trap cleanup_test_project RETURN
  create_planned_work_item

  RESULT=0
  OUTPUT="$(capture_dashboard_default "$TEST_DIR/project" "\x1b[C" | clean_dashboard_output)" || return 1
  assert_contains "$OUTPUT" "Current1" || RESULT=1
  assert_contains "$OUTPUT" "Ready1" || RESULT=1
  assert_contains "$OUTPUT" "work-visible" || RESULT=1
  assert_contains "$OUTPUT" "VisibleWork" || RESULT=1
  assert_contains "$OUTPUT" "Attempt:attempt-visible[planned]" || RESULT=1
  assert_contains "$OUTPUT" "RunAttempt:fluentattemptrunwork-visible" || RESULT=1
  return $RESULT
}

test_dashboard_refreshes_work_items_on_poll() {
  setup_test_project
  trap cleanup_test_project RETURN
  create_planned_work_item
  "$FLUENT_BIN" work-item create work-polled --title "Polled Work" > /dev/null

  RESULT=0
  OUTPUT="$(capture_dashboard_default "$TEST_DIR/project" "r" | clean_dashboard_output)" || return 1
  assert_contains "$OUTPUT" "Current2" || RESULT=1
  assert_contains "$OUTPUT" "Planned" || RESULT=1
  assert_contains "$OUTPUT" "PolledWork" || RESULT=1
  return $RESULT
}

test_dashboard_surfaces_actionable_work() {
  setup_test_project
  trap cleanup_test_project RETURN
  create_merge_ready_work_item || return 1

  RESULT=0
  OUTPUT="$(capture_dashboard_default "$TEST_DIR/project" "\x1b[Ca" | clean_dashboard_output)" || return 1
  assert_contains "$OUTPUT" "Ready1" || RESULT=1
  assert_contains "$OUTPUT" "work-action" || RESULT=1
  assert_contains "$OUTPUT" "ActionableWork" || RESULT=1
  assert_contains "$OUTPUT" "Readytoland" || RESULT=1
  assert_contains "$OUTPUT" "fluentmerge-candidateshowwork-action" || RESULT=1
  return $RESULT
}

test_dashboard_waits_for_initial_read() {
  setup_test_project
  trap cleanup_test_project RETURN
  create_planned_work_item

  # Hold the real first snapshot beyond the former one-second capture delay.
  local gate="$TEST_DIR/project/.fluent/work/items/read-gate.json"
  mkfifo "$gate"
  (
    exec 3>"$gate"
    sleep 1.5
    printf '{ invalid json\n' >&3
  ) &
  local gate_writer=$!
  trap '
    if [ -n "${gate_writer:-}" ]; then
      kill "$gate_writer" 2>/dev/null || true
      wait "$gate_writer" 2>/dev/null || true
    fi
    cleanup_test_project
  ' RETURN
  local capture_result=0
  OUTPUT="$(capture_dashboard_default "$TEST_DIR/project" | clean_dashboard_output)" || capture_result=$?
  [ "$capture_result" = 0 ] || return "$capture_result"
  wait "$gate_writer" || true
  gate_writer=""

  assert_contains "$OUTPUT" "Current1" &&
    assert_contains "$OUTPUT" "VisibleWork" &&
    assert_contains "$OUTPUT" "Readingsources"
}

test_dashboard_surfaces_needs_user_work() {
  setup_test_project
  trap cleanup_test_project RETURN
  create_needs_user_work_item

  RESULT=0
  OUTPUT="$(capture_dashboard_default "$TEST_DIR/project" "\x1b[C" | clean_dashboard_output)" || return 1
  assert_contains "$OUTPUT" "Needsyou1" || RESULT=1
  assert_contains "$OUTPUT" "work-needs-user" || RESULT=1
  assert_contains "$OUTPUT" "NeedsUserWork" || RESULT=1
  assert_contains "$OUTPUT" "Attempt:attempt-needs-user" || RESULT=1
  assert_contains "$OUTPUT" "State:Needsyou" || RESULT=1
  return $RESULT
}

test_dashboard_reports_work_read_errors() {
  setup_test_project
  trap cleanup_test_project RETURN
  create_planned_work_item
  printf '{ invalid json\n' > .fluent/work/items/broken-work.json

  RESULT=0
  OUTPUT="$(capture_dashboard_default "$TEST_DIR/project" "\x1b[Ce" | clean_dashboard_output)" || return 1
  assert_contains "$OUTPUT" "Current1" || RESULT=1
  assert_contains "$OUTPUT" "VisibleWork" || RESULT=1
  assert_contains "$OUTPUT" "failedtoparse" || RESULT=1
  return $RESULT
}

test_dashboard_shows_empty_work_view() {
  setup_test_project
  trap cleanup_test_project RETURN

  RESULT=0
  OUTPUT="$(capture_dashboard_default "$TEST_DIR/project" | clean_dashboard_output)" || return 1
  assert_contains "$OUTPUT" "Current0" || RESULT=1
  assert_contains "$OUTPUT" "NoWorkItemsinthisview" || RESULT=1
  return $RESULT
}

test_dashboard_help_journey() {
  setup_test_project
  trap cleanup_test_project RETURN
  create_planned_work_item
  python3 "$PROJECT_DIR/tests/fixtures/dashboard-journey.py" "$FLUENT_BIN" "$TEST_DIR/project" help
}

test_dashboard_navigates_during_local_refresh() {
  setup_test_project
  trap cleanup_test_project RETURN
  create_planned_work_item
  "$FLUENT_BIN" work-item create work-z-second --title "Second Work" > /dev/null
  "$FLUENT_BIN" attempt create work-z-second attempt-second > /dev/null
  python3 "$PROJECT_DIR/tests/fixtures/dashboard-journey.py" "$FLUENT_BIN" "$TEST_DIR/project" refresh
}

test_dashboard_overflow_error_journey() {
  setup_test_project
  trap cleanup_test_project RETURN
  create_planned_work_item
  for index in $(seq 0 29); do
    printf '{ invalid json\n' > "$(printf '.fluent/work/items/broken-%02d.json' "$index")"
  done
  python3 "$PROJECT_DIR/tests/fixtures/dashboard-journey.py" "$FLUENT_BIN" "$TEST_DIR/project" errors
}

# Select the capture regressions during focused verification.
if [ "${1:-}" = "--verification-layout" ]; then
  run_test "dashboard lists Work Items" test_dashboard_lists_work_items
  run_test "dashboard waits for initial read" test_dashboard_waits_for_initial_read
  run_test "dashboard reports Work read errors" test_dashboard_reports_work_read_errors
  run_test "dashboard shows empty Work view" test_dashboard_shows_empty_work_view
  run_test "dashboard navigates during local refresh" test_dashboard_navigates_during_local_refresh
  summarize_and_exit
  exit 0
fi

if [ "${1:-}" = "--verification-capture" ]; then
  run_test "dashboard capture bounds child lifetime" test_dashboard_capture_bounds_child_lifetime
  run_test "dashboard waits for initial read" test_dashboard_waits_for_initial_read
  run_test "dashboard surfaces actionable Work" test_dashboard_surfaces_actionable_work
  summarize_and_exit
  exit 0
fi

# Select only the two public interaction journeys during focused verification.
if [ "${1:-}" = "--verification-journeys" ]; then
  run_test "dashboard help journey" test_dashboard_help_journey
  run_test "dashboard overflow error journey" test_dashboard_overflow_error_journey
  summarize_and_exit
  exit 0
fi

printf 'test-work-status-dashboard\n\n'

run_test "assertions match large literal output" test_assert_contains_large_literal_output
run_test "dashboard capture bounds child lifetime" test_dashboard_capture_bounds_child_lifetime

run_test "status prints Work Items" test_status_prints_work_items
run_test "status summarizes Work model vocabulary" test_status_summarizes_work_model_vocabulary
run_test "status surfaces needs-user state" test_status_surfaces_needs_user_state
run_test "status surfaces abandoned Work as terminal" \
  test_status_surfaces_abandoned_work_as_terminal
run_test "status reports invalid Work without hiding valid state" test_status_reports_invalid_work_without_hiding_valid_state
run_test "dashboard lists Work Items" test_dashboard_lists_work_items
run_test "dashboard refreshes Work Items on poll" test_dashboard_refreshes_work_items_on_poll
run_test "dashboard surfaces actionable Work" test_dashboard_surfaces_actionable_work
run_test "dashboard waits for initial read" test_dashboard_waits_for_initial_read
run_test "dashboard surfaces needs-user Work" test_dashboard_surfaces_needs_user_work
run_test "dashboard reports Work read errors" test_dashboard_reports_work_read_errors
run_test "dashboard shows empty Work view" test_dashboard_shows_empty_work_view

run_test "dashboard help journey" test_dashboard_help_journey
run_test "dashboard overflow error journey" test_dashboard_overflow_error_journey
run_test "dashboard navigates during local refresh" test_dashboard_navigates_during_local_refresh

summarize_and_exit
