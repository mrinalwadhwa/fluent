#!/usr/bin/env bash
# test-dashboard — Verify dashboard edge cases.
#
# Tests:
#   - Dashboard exits gracefully with no runs
#   - Dashboard handles invalid run-id
#   - Dashboard does not modify run state
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
FACTORY_BIN="${PROJECT_DIR}/target/debug/factory"

PASS=0
FAIL=0
ERRORS=""

# -------------------------------------------------------------------------
# Helpers
# -------------------------------------------------------------------------

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
run_test "completed run with report shows report by default" test_completed_run_with_report_shows_report_by_default
run_test "completed run without report shows transcript" test_completed_run_without_report_shows_transcript
run_test "active run with report shows transcript" test_active_run_with_report_shows_transcript
run_test "completed run keeps transcript tabs accessible" test_completed_run_keeps_transcript_tabs_accessible

printf '\n  %d passed, %d failed\n' "$PASS" "$FAIL"

if [ "$FAIL" -gt 0 ]; then
  printf '\n  Failures:%b\n' "$ERRORS"
  exit 1
fi
