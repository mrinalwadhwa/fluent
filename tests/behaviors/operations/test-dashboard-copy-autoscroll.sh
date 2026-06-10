#!/usr/bin/env bash
# test-dashboard-copy-autoscroll — Verify copy mode and auto-scroll behaviors.
#
# Tests:
#   - Dashboard does not crash with transcript content (copy mode scenario)
#   - Dashboard does not crash with many feed lines (auto-scroll scenario)
#
# Note: Visual/behavioral assertions (help bar indicator, auto-scroll toggle)
# are verified by Rust unit tests:
#   cargo test --lib dashboard -- \
#     test_help_bar_shows_copy_key \
#     test_help_bar_shows_copy_mode_indicator \
#     test_scroll_down_reenables_auto_scroll_at_bottom \
#     test_scroll_to_bottom_enables_auto_scroll
#
# Usage:
#   tests/behaviors/operations/test-dashboard-copy-autoscroll.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$(dirname "$(dirname "$SCRIPT_DIR")")")"
FACTORY_BIN="${FACTORY_BIN_OVERRIDE:-${PROJECT_DIR}/target/debug/factory}"

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

no_panic() {
  local output="$1"
  if echo "$output" | grep -qi "panic"; then
    printf '    FAIL: dashboard panicked\n'
    return 1
  fi
  return 0
}

not_crashed() {
  local exit_code="$1"
  if [ "$exit_code" -gt 128 ]; then
    printf '    FAIL: dashboard crashed with signal %d\n' $((exit_code - 128))
    return 1
  fi
  return 0
}

# -------------------------------------------------------------------------
# Tests
# -------------------------------------------------------------------------

# Copy mode behavior: WHEN the user activates copy mode,
# THE SYSTEM SHALL allow text selection using the terminal's native selection.
# Shell test: dashboard does not crash when it has transcript content
# (the scenario where a user would want to use copy mode).
# Visual/behavioral assertions covered by:
#   dashboard::tests::test_help_bar_shows_copy_key
#   dashboard::tests::test_help_bar_shows_copy_mode_indicator
test_dashboard_no_crash_with_copyable_content() {
  TEST_DIR="$(mktemp -d -t factory-test-dash-copy-XXXXXX)"
  mkdir -p "${TEST_DIR}/.factory/runs/copy-run/sessions/session-1"
  printf 'executing' > "${TEST_DIR}/.factory/runs/copy-run/status"
  printf 'Test run with copyable content' > "${TEST_DIR}/.factory/runs/copy-run/brief.md"

  # Create transcript with content a user would want to copy
  cat > "${TEST_DIR}/.factory/runs/copy-run/sessions/session-1/transcript.jsonl" << 'EOF'
{"type":"assistant","message":{"content":[{"type":"text","text":"Analyzing the codebase..."}]}}
{"type":"tool_use","name":"Read","input":{"file_path":"/some/file.rs"}}
{"type":"tool_result","content":[{"type":"text","text":"fn main() { println!(\"hello\"); }"}]}
{"type":"assistant","message":{"content":[{"type":"text","text":"Found the main function."}]}}
EOF

  set +e
  OUTPUT="$(cd "$TEST_DIR" && timeout 1 "$FACTORY_BIN" dashboard 2>&1)"
  EXIT_CODE=$?
  set -e

  local RESULT=0
  no_panic "$OUTPUT" || RESULT=1
  not_crashed "$EXIT_CODE" || RESULT=1

  rm -rf "$TEST_DIR"
  return $RESULT
}

# Copy mode visible indicator: WHEN copy mode is active,
# THE SYSTEM SHALL indicate it visibly.
# Shell test: dashboard does not crash when initialized in a state
# where copy mode toggle would be available.
# Visual assertions covered by:
#   dashboard::tests::test_help_bar_shows_copy_mode_indicator
test_dashboard_no_crash_copy_mode_indicator_scenario() {
  TEST_DIR="$(mktemp -d -t factory-test-dash-copy-XXXXXX)"
  mkdir -p "${TEST_DIR}/.factory/runs/indicator-run"
  printf 'complete' > "${TEST_DIR}/.factory/runs/indicator-run/status"
  printf 'Run to verify copy mode indicator scenario' > "${TEST_DIR}/.factory/runs/indicator-run/brief.md"

  set +e
  OUTPUT="$(cd "$TEST_DIR" && timeout 1 "$FACTORY_BIN" dashboard 2>&1)"
  EXIT_CODE=$?
  set -e

  local RESULT=0
  no_panic "$OUTPUT" || RESULT=1
  not_crashed "$EXIT_CODE" || RESULT=1

  rm -rf "$TEST_DIR"
  return $RESULT
}

# Auto-scroll re-enable: WHEN the user scrolls to the bottom of the activity
# feed, THE SYSTEM SHALL re-enable auto-scroll.
# Shell test: dashboard does not crash with many feed lines (a scenario
# where scrolling to the bottom would trigger auto-scroll re-enable).
# Behavioral assertions covered by:
#   dashboard::tests::test_scroll_down_reenables_auto_scroll_at_bottom
#   dashboard::tests::test_scroll_to_bottom_enables_auto_scroll
test_dashboard_no_crash_with_many_feed_lines() {
  TEST_DIR="$(mktemp -d -t factory-test-dash-copy-XXXXXX)"
  mkdir -p "${TEST_DIR}/.factory/runs/scroll-run/sessions/session-1"
  printf 'executing' > "${TEST_DIR}/.factory/runs/scroll-run/status"
  printf 'Run with many feed lines to test auto-scroll' > "${TEST_DIR}/.factory/runs/scroll-run/brief.md"

  # Generate enough lines to fill multiple screens, triggering scroll behavior
  TRANSCRIPT_FILE="${TEST_DIR}/.factory/runs/scroll-run/sessions/session-1/transcript.jsonl"
  for i in $(seq 1 100); do
    printf '{"type":"assistant","message":{"content":[{"type":"text","text":"Line %d: processing step %d of the analysis pipeline"}]}}\n' "$i" "$i"
  done > "$TRANSCRIPT_FILE"

  set +e
  OUTPUT="$(cd "$TEST_DIR" && timeout 1 "$FACTORY_BIN" dashboard 2>&1)"
  EXIT_CODE=$?
  set -e

  local RESULT=0
  no_panic "$OUTPUT" || RESULT=1
  not_crashed "$EXIT_CODE" || RESULT=1

  rm -rf "$TEST_DIR"
  return $RESULT
}

# -------------------------------------------------------------------------
# Run all tests
# -------------------------------------------------------------------------

printf 'test-dashboard-copy-autoscroll\n\n'

run_test "no crash with copyable transcript content" test_dashboard_no_crash_with_copyable_content
run_test "no crash in copy mode indicator scenario" test_dashboard_no_crash_copy_mode_indicator_scenario
run_test "no crash with many feed lines (auto-scroll scenario)" test_dashboard_no_crash_with_many_feed_lines

printf '\n  %d passed, %d failed\n' "$PASS" "$FAIL"

if [ "$FAIL" -gt 0 ]; then
  printf '\n  Failures:%b\n' "$ERRORS"
  exit 1
fi
