#!/usr/bin/env bash
# test-dashboard — Verify dashboard edge cases.
#
# Tests:
#   - Dashboard exits gracefully with no runs
#   - Dashboard handles invalid run-id
#   - Dashboard does not modify run state
#   - Run tabs show status from the live run directory
#   - Initial run selection prefers live active status
#   - Polling removes deleted source run directories
#   - Polling selects an existing run when the selected run is removed
#   - Polling renders the empty state when all runs are removed
#   - Completed runs show report.md by default
#   - Completed runs without report.md show transcript activity
#   - Active runs show transcript activity even when report.md exists
#   - Completed runs keep transcript tabs accessible
#
# Usage:
#   tests/behaviors/operations/test-dashboard.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$(dirname "$(dirname "$SCRIPT_DIR")")")"
FACTORY_BIN="${FACTORY_BIN_OVERRIDE:-${PROJECT_DIR}/target/debug/factory}"

source "${PROJECT_DIR}/tests/lib/run_test.sh"
LOG_DIR="${PROJECT_DIR}/tests/output/$(basename "$0" .sh)"

# -------------------------------------------------------------------------
# Helpers
# -------------------------------------------------------------------------

capture_dashboard() {
  PROJECT_PATH="$1"
  RUN_ID="$2"
  KEYS="${3:-}"
  OUTPUT_FILE="$(mktemp -t factory-dashboard-output-XXXXXX)"

  if [ -n "$KEYS" ]; then
    (
      sleep 1
      printf '%b' "$KEYS"
      sleep 1
      printf 'q'
    ) | FACTORY_DASH_BIN="$FACTORY_BIN" \
        FACTORY_DASH_PROJECT="$PROJECT_PATH" \
        FACTORY_DASH_RUN="$RUN_ID" \
        TERM=xterm \
        script -q "$OUTPUT_FILE" sh -c 'stty rows 30 cols 120; "$FACTORY_DASH_BIN" dashboard --run-id "$FACTORY_DASH_RUN" "$FACTORY_DASH_PROJECT"' >/dev/null 2>&1 || true
  else
    (
      sleep 1
      printf 'q'
    ) | FACTORY_DASH_BIN="$FACTORY_BIN" \
        FACTORY_DASH_PROJECT="$PROJECT_PATH" \
        FACTORY_DASH_RUN="$RUN_ID" \
        TERM=xterm \
        script -q "$OUTPUT_FILE" sh -c 'stty rows 30 cols 120; "$FACTORY_DASH_BIN" dashboard --run-id "$FACTORY_DASH_RUN" "$FACTORY_DASH_PROJECT"' >/dev/null 2>&1 || true
  fi

  cat "$OUTPUT_FILE"
  rm -f "$OUTPUT_FILE"
}

capture_dashboard_default() {
  PROJECT_PATH="$1"
  KEYS="${2:-}"
  OUTPUT_FILE="$(mktemp -t factory-dashboard-output-XXXXXX)"

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
  RUN_ID="$2"
  MUTATION="$3"
  OUTPUT_FILE="$(mktemp -t factory-dashboard-output-XXXXXX)"

  (
    sleep 1
    eval "$MUTATION"
    sleep 4
    printf 'q'
  ) | FACTORY_DASH_BIN="$FACTORY_BIN" \
      FACTORY_DASH_PROJECT="$PROJECT_PATH" \
      FACTORY_DASH_RUN="$RUN_ID" \
      TERM=xterm \
      script -q "$OUTPUT_FILE" sh -c 'stty rows 30 cols 120; "$FACTORY_DASH_BIN" dashboard --run-id "$FACTORY_DASH_RUN" "$FACTORY_DASH_PROJECT"' >/dev/null 2>&1 || true

  cat "$OUTPUT_FILE"
  rm -f "$OUTPUT_FILE"
}

clean_dashboard_output_tail() {
  clean_dashboard_output | perl -0777 -ne '$i = rindex($_, "FactoryDashboard"); $i = rindex($_, "Factory Dashboard") if $i < 0; print $i >= 0 ? substr($_, $i) : $_'
}

clean_dashboard_output() {
  perl -pe 's/\e\[[0-9;?]*[ -\/]*[@-~]//g; s/\r/\n/g'
}

write_transcript() {
  TRANSCRIPT_PATH="$1"
  MESSAGE="$2"
  mkdir -p "$(dirname "$TRANSCRIPT_PATH")"
  printf '{"type":"assistant","message":{"content":[{"type":"text","text":"%s"}]}}\n' "$MESSAGE" > "$TRANSCRIPT_PATH"
}

# -------------------------------------------------------------------------
# Tests
# -------------------------------------------------------------------------

test_dashboard_exits_gracefully_with_no_runs() {
  TEST_DIR="$(mktemp -d -t factory-test-dash-XXXXXX)"
  mkdir -p "${TEST_DIR}/.factory/runs"

  # Dashboard should exit non-zero (no runs) but not crash/segfault
  OUTPUT="$(cd "$TEST_DIR" && "$FACTORY_BIN" dashboard 2>&1 || true)"

  RESULT=0
  # Should mention no runs or exit cleanly — must not panic
  if echo "$OUTPUT" | grep -qi "panic"; then
    printf '    FAIL: dashboard panicked with no runs\n'
    RESULT=1
  fi

  rm -rf "$TEST_DIR"
  return $RESULT
}

test_dashboard_handles_invalid_run_id() {
  TEST_DIR="$(mktemp -d -t factory-test-dash-XXXXXX)"
  mkdir -p "${TEST_DIR}/.factory/runs/valid-run"
  printf 'planned' > "${TEST_DIR}/.factory/runs/valid-run/status"
  printf 'Test brief' > "${TEST_DIR}/.factory/runs/valid-run/brief.md"

  # Request a non-existent run-id — should exit with an error message
  set +e
  OUTPUT="$(cd "$TEST_DIR" && timeout 2 "$FACTORY_BIN" dashboard --run-id nonexistent 2>&1)"
  EXIT_CODE=$?
  set -e

  RESULT=0
  if echo "$OUTPUT" | grep -qi "panic"; then
    printf '    FAIL: dashboard panicked with invalid run-id\n'
    RESULT=1
  fi
  # Signal-killed process (128+) indicates a crash
  if [ "$EXIT_CODE" -gt 128 ]; then
    printf '    FAIL: dashboard crashed with signal %d\n' $((EXIT_CODE - 128))
    RESULT=1
  fi
  # Should report that the run was not found
  if ! echo "$OUTPUT" | grep -qi "not found"; then
    printf '    FAIL: expected "not found" error message, got: %s\n' "$OUTPUT"
    RESULT=1
  fi

  rm -rf "$TEST_DIR"
  return $RESULT
}

test_dashboard_does_not_modify_run_state() {
  TEST_DIR="$(mktemp -d -t factory-test-dash-XXXXXX)"
  mkdir -p "${TEST_DIR}/.factory/runs/state-check"
  printf 'executing' > "${TEST_DIR}/.factory/runs/state-check/status"
  printf 'Test brief' > "${TEST_DIR}/.factory/runs/state-check/brief.md"

  # Record file state before (use shasum for cross-platform portability)
  BEFORE="$(find "${TEST_DIR}/.factory" -type f -exec shasum {} \; | sort)"

  # Run dashboard briefly (it will fail without a terminal, but should not
  # modify state regardless)
  cd "$TEST_DIR" && timeout 1 "$FACTORY_BIN" dashboard 2>/dev/null || true

  # Record file state after
  AFTER="$(find "${TEST_DIR}/.factory" -type f -exec shasum {} \; | sort)"

  RESULT=0
  if [ "$BEFORE" != "$AFTER" ]; then
    printf '    FAIL: dashboard modified run state files\n'
    RESULT=1
  fi

  rm -rf "$TEST_DIR"
  return $RESULT
}

test_run_tabs_show_live_status() {
  TEST_DIR="$(mktemp -d -t factory-test-dash-XXXXXX)"
  WORKTREE_DIR="$(mktemp -d -t factory-test-dash-wt-XXXXXX)"
  mkdir -p "${TEST_DIR}/.factory/runs/live-run"
  mkdir -p "${WORKTREE_DIR}/.factory/runs/live-run"
  printf 'planned' > "${TEST_DIR}/.factory/runs/live-run/status"
  printf 'Live brief' > "${TEST_DIR}/.factory/runs/live-run/brief.md"
  printf 'executing' > "${WORKTREE_DIR}/.factory/runs/live-run/status"
  printf 'Live brief' > "${WORKTREE_DIR}/.factory/runs/live-run/brief.md"
  printf '%s' "$WORKTREE_DIR" > "${TEST_DIR}/.factory/runs/live-run/worktree"

  OUTPUT="$(capture_dashboard "$TEST_DIR" live-run)"

  RESULT=0
  if ! echo "$OUTPUT" | grep -q "live-run \\[[^]]*executing\\]"; then
    printf '    FAIL: expected run tab to show live status [executing]\n'
    RESULT=1
  fi
  if echo "$OUTPUT" | grep -q "live-run \\[planned\\]"; then
    printf '    FAIL: run tab showed stale source status [planned]\n'
    RESULT=1
  fi

  rm -rf "$TEST_DIR" "$WORKTREE_DIR"
  return $RESULT
}

test_initial_run_prefers_live_active_status() {
  TEST_DIR="$(mktemp -d -t factory-test-dash-XXXXXX)"
  WORKTREE_DIR="$(mktemp -d -t factory-test-dash-wt-XXXXXX)"
  mkdir -p "${TEST_DIR}/.factory/runs/live-active"
  mkdir -p "${TEST_DIR}/.factory/runs/source-complete"
  mkdir -p "${WORKTREE_DIR}/.factory/runs/live-active"
  printf 'complete' > "${TEST_DIR}/.factory/runs/live-active/status"
  printf 'Live active brief' > "${TEST_DIR}/.factory/runs/live-active/brief.md"
  printf 'executing' > "${WORKTREE_DIR}/.factory/runs/live-active/status"
  printf 'Live active brief' > "${WORKTREE_DIR}/.factory/runs/live-active/brief.md"
  printf '%s' "$WORKTREE_DIR" > "${TEST_DIR}/.factory/runs/live-active/worktree"
  printf 'complete' > "${TEST_DIR}/.factory/runs/source-complete/status"
  printf 'Complete brief' > "${TEST_DIR}/.factory/runs/source-complete/brief.md"

  OUTPUT="$(capture_dashboard_default "$TEST_DIR" "r")"
  CLEAN_OUTPUT="$(printf '%s' "$OUTPUT" | clean_dashboard_output)"

  RESULT=0
  if ! echo "$CLEAN_OUTPUT" | grep -q "Run: live-active"; then
    printf '    FAIL: expected initial selection to prefer live-active\n'
    RESULT=1
  fi
  if ! echo "$CLEAN_OUTPUT" | grep -q "live-active \\[[^]]*executing\\]"; then
    printf '    FAIL: expected live-active tab to show [executing]\n'
    RESULT=1
  fi

  rm -rf "$TEST_DIR" "$WORKTREE_DIR"
  return $RESULT
}

test_poll_removes_deleted_source_run() {
  TEST_DIR="$(mktemp -d -t factory-test-dash-XXXXXX)"
  mkdir -p "${TEST_DIR}/.factory/runs/delete-me"
  mkdir -p "${TEST_DIR}/.factory/runs/keep-me"
  printf 'complete' > "${TEST_DIR}/.factory/runs/delete-me/status"
  printf 'Delete brief' > "${TEST_DIR}/.factory/runs/delete-me/brief.md"
  printf 'planned' > "${TEST_DIR}/.factory/runs/keep-me/status"
  printf 'Keep brief' > "${TEST_DIR}/.factory/runs/keep-me/brief.md"

  OUTPUT="$(capture_dashboard_after_poll_mutation "$TEST_DIR" keep-me "rm -rf '${TEST_DIR}/.factory/runs/delete-me'")"
  FINAL_OUTPUT="$(printf '%s' "$OUTPUT" | clean_dashboard_output_tail)"

  RESULT=0
  if echo "$OUTPUT" | grep -qi "panic"; then
    printf '    FAIL: dashboard panicked after deleting a source run\n'
    RESULT=1
  fi
  if [[ "$FINAL_OUTPUT" == *delete-me* ]]; then
    printf '    FAIL: deleted source run remained in the polled dashboard state\n'
    RESULT=1
  fi
  if [[ "$FINAL_OUTPUT" != *keep-me* || "$FINAL_OUTPUT" != *planned* ]]; then
    printf '    FAIL: expected remaining run to stay visible after poll\n'
    RESULT=1
  fi

  rm -rf "$TEST_DIR"
  return $RESULT
}

test_poll_selects_existing_run_when_selected_removed() {
  TEST_DIR="$(mktemp -d -t factory-test-dash-XXXXXX)"
  mkdir -p "${TEST_DIR}/.factory/runs/remove-me"
  mkdir -p "${TEST_DIR}/.factory/runs/keep-me"
  printf 'executing' > "${TEST_DIR}/.factory/runs/remove-me/status"
  printf 'Remove brief' > "${TEST_DIR}/.factory/runs/remove-me/brief.md"
  printf 'planned' > "${TEST_DIR}/.factory/runs/keep-me/status"
  printf 'Keep brief' > "${TEST_DIR}/.factory/runs/keep-me/brief.md"

  OUTPUT="$(capture_dashboard_after_poll_mutation "$TEST_DIR" remove-me "rm -rf '${TEST_DIR}/.factory/runs/remove-me'")"
  FINAL_OUTPUT="$(printf '%s' "$OUTPUT" | clean_dashboard_output_tail)"

  RESULT=0
  if echo "$OUTPUT" | grep -qi "panic"; then
    printf '    FAIL: dashboard panicked after selected run was removed\n'
    RESULT=1
  fi
  if ! echo "$FINAL_OUTPUT" | grep -q "keep-me"; then
    printf '    FAIL: expected dashboard to select an existing run\n'
    RESULT=1
  fi

  rm -rf "$TEST_DIR"
  return $RESULT
}

test_poll_renders_empty_state_when_all_runs_removed() {
  TEST_DIR="$(mktemp -d -t factory-test-dash-XXXXXX)"
  mkdir -p "${TEST_DIR}/.factory/runs/remove-me"
  printf 'executing' > "${TEST_DIR}/.factory/runs/remove-me/status"
  printf 'Remove brief' > "${TEST_DIR}/.factory/runs/remove-me/brief.md"

  OUTPUT="$(capture_dashboard_after_poll_mutation "$TEST_DIR" remove-me "rm -rf '${TEST_DIR}/.factory/runs/remove-me'")"
  FINAL_OUTPUT="$(printf '%s' "$OUTPUT" | clean_dashboard_output_tail)"

  RESULT=0
  if echo "$OUTPUT" | grep -qi "panic"; then
    printf '    FAIL: dashboard panicked after all runs were removed\n'
    RESULT=1
  fi
  if ! echo "$FINAL_OUTPUT" | grep -q "Work Items (0)"; then
    printf '    FAIL: expected Work view after all runs were removed\n'
    RESULT=1
  fi
  if ! echo "$FINAL_OUTPUT" | grep -q "No Work Items found"; then
    printf '    FAIL: expected Work empty-state body after all runs were removed\n'
    RESULT=1
  fi

  rm -rf "$TEST_DIR"
  return $RESULT
}

test_completed_run_with_report_shows_report_by_default() {
  TEST_DIR="$(mktemp -d -t factory-test-dash-XXXXXX)"
  mkdir -p "${TEST_DIR}/.factory/runs/finished"
  printf 'complete' > "${TEST_DIR}/.factory/runs/finished/status"
  printf 'Test brief' > "${TEST_DIR}/.factory/runs/finished/brief.md"
  printf '# Report Marker\nReport body marker\n' > "${TEST_DIR}/.factory/runs/finished/report.md"
  write_transcript "${TEST_DIR}/.factory/runs/finished/sessions/session-1/transcript.jsonl" "Transcript marker Session complete"

  OUTPUT="$(capture_dashboard "$TEST_DIR" finished)"

  RESULT=0
  if ! echo "$OUTPUT" | grep -q "Report Marker"; then
    printf '    FAIL: expected report content in default dashboard view\n'
    RESULT=1
  fi
  if echo "$OUTPUT" | grep -q "Transcript marker"; then
    printf '    FAIL: expected report view instead of transcript content\n'
    RESULT=1
  fi

  rm -rf "$TEST_DIR"
  return $RESULT
}

test_completed_run_without_report_shows_transcript() {
  TEST_DIR="$(mktemp -d -t factory-test-dash-XXXXXX)"
  mkdir -p "${TEST_DIR}/.factory/runs/no-report"
  printf 'complete' > "${TEST_DIR}/.factory/runs/no-report/status"
  printf 'Test brief' > "${TEST_DIR}/.factory/runs/no-report/brief.md"
  write_transcript "${TEST_DIR}/.factory/runs/no-report/sessions/session-1/transcript.jsonl" "Transcript fallback marker"

  OUTPUT="$(capture_dashboard "$TEST_DIR" no-report)"

  RESULT=0
  if ! echo "$OUTPUT" | grep -q "Transcript fallback marker"; then
    printf '    FAIL: expected transcript content when report.md is missing\n'
    RESULT=1
  fi

  rm -rf "$TEST_DIR"
  return $RESULT
}

test_active_run_with_report_shows_transcript() {
  TEST_DIR="$(mktemp -d -t factory-test-dash-XXXXXX)"
  mkdir -p "${TEST_DIR}/.factory/runs/active-run"
  printf 'executing' > "${TEST_DIR}/.factory/runs/active-run/status"
  printf 'Test brief' > "${TEST_DIR}/.factory/runs/active-run/brief.md"
  printf '# Active Report Marker\n' > "${TEST_DIR}/.factory/runs/active-run/report.md"
  write_transcript "${TEST_DIR}/.factory/runs/active-run/sessions/session-1/transcript.jsonl" "Live transcript marker"

  OUTPUT="$(capture_dashboard "$TEST_DIR" active-run)"

  RESULT=0
  if ! echo "$OUTPUT" | grep -q "Live transcript marker"; then
    printf '    FAIL: expected live transcript content for active run\n'
    RESULT=1
  fi
  if echo "$OUTPUT" | grep -q "Active Report Marker"; then
    printf '    FAIL: expected transcript view instead of report for active run\n'
    RESULT=1
  fi

  rm -rf "$TEST_DIR"
  return $RESULT
}

test_completed_run_keeps_transcript_tabs_accessible() {
  TEST_DIR="$(mktemp -d -t factory-test-dash-XXXXXX)"
  mkdir -p "${TEST_DIR}/.factory/runs/finished"
  printf 'complete' > "${TEST_DIR}/.factory/runs/finished/status"
  printf 'Test brief' > "${TEST_DIR}/.factory/runs/finished/brief.md"
  printf '# Report Marker\nReport body marker\n' > "${TEST_DIR}/.factory/runs/finished/report.md"
  write_transcript "${TEST_DIR}/.factory/runs/finished/sessions/session-1/transcript.jsonl" "Accessible transcript marker"

  OUTPUT="$(capture_dashboard "$TEST_DIR" finished "\\t")"

  RESULT=0
  if ! echo "$OUTPUT" | grep -q "Report Marker"; then
    printf '    FAIL: expected report content before switching tabs\n'
    RESULT=1
  fi
  if ! echo "$OUTPUT" | grep -q "ssible transcript marker"; then
    printf '    FAIL: expected transcript content after pressing Tab\n'
    RESULT=1
  fi

  rm -rf "$TEST_DIR"
  return $RESULT
}

# -------------------------------------------------------------------------
# Run all tests
# -------------------------------------------------------------------------

printf 'test-dashboard\n\n'

run_test "dashboard exits gracefully with no runs" test_dashboard_exits_gracefully_with_no_runs
run_test "dashboard handles invalid run-id" test_dashboard_handles_invalid_run_id
run_test "dashboard does not modify run state" test_dashboard_does_not_modify_run_state
run_test "run tabs show live status" test_run_tabs_show_live_status
run_test "initial run prefers live active status" test_initial_run_prefers_live_active_status
run_test "poll removes deleted source run" test_poll_removes_deleted_source_run
run_test "poll selects existing run when selected removed" test_poll_selects_existing_run_when_selected_removed
run_test "poll renders empty state when all runs removed" test_poll_renders_empty_state_when_all_runs_removed
run_test "completed run with report shows report by default" test_completed_run_with_report_shows_report_by_default
run_test "completed run without report shows transcript" test_completed_run_without_report_shows_transcript
run_test "active run with report shows transcript" test_active_run_with_report_shows_transcript
run_test "completed run keeps transcript tabs accessible" test_completed_run_keeps_transcript_tabs_accessible

summarize_and_exit
