#!/usr/bin/env bash
# test-dashboard-review-rounds - Verify dashboard review-round rendering.
#
# Tests:
#   - Reviewing status shows active reviewer work before transcripts exist
#   - Archived review artifacts do not drive current reviewer verdict tabs
#   - Archived reviewer transcripts do not create current reviewer tabs
#   - Stale reviewer tabs disappear after top-level transcripts are archived
#
# Usage:
#   tests/behaviors/operations/test-dashboard-review-rounds.sh

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

capture_dashboard() {
  PROJECT_PATH="$1"
  RUN_ID="$2"
  OUTPUT_FILE="$(mktemp -t factory-dashboard-output-XXXXXX)"

  (
    sleep 1
    printf 'q'
  ) | FACTORY_DASH_BIN="$FACTORY_BIN" \
      FACTORY_DASH_PROJECT="$PROJECT_PATH" \
      FACTORY_DASH_RUN="$RUN_ID" \
      TERM=xterm \
      script -q "$OUTPUT_FILE" sh -c 'stty rows 30 cols 120; "$FACTORY_DASH_BIN" dashboard --run-id "$FACTORY_DASH_RUN" "$FACTORY_DASH_PROJECT"' >/dev/null 2>&1 || true

  cat "$OUTPUT_FILE"
  rm -f "$OUTPUT_FILE"
}

clean_dashboard_output() {
  perl -pe 's/\e\[[0-9;?]*[ -\/]*[@-~]//g; s/\r/\n/g'
}

write_review_artifacts() {
  REVIEWS_DIR="$1"
  REVIEWER="$2"
  VERDICT="$3"
  mkdir -p "$REVIEWS_DIR"
  printf '{}\n' > "${REVIEWS_DIR}/transcript-${REVIEWER}.jsonl"
  printf 'Verdict: %s\n' "$VERDICT" > "${REVIEWS_DIR}/review-${REVIEWER}.md"
}

test_reviewing_status_shows_active_work_before_transcripts() {
  TEST_DIR="$(mktemp -d -t factory-test-dash-rounds-XXXXXX)"
  mkdir -p "${TEST_DIR}/.factory/runs/reviewing-run/reviews"
  printf 'reviewing' > "${TEST_DIR}/.factory/runs/reviewing-run/status"
  printf 'Reviewing brief' > "${TEST_DIR}/.factory/runs/reviewing-run/brief.md"

  OUTPUT="$(capture_dashboard "$TEST_DIR" reviewing-run)"
  CLEAN_OUTPUT="$(printf '%s' "$OUTPUT" | clean_dashboard_output)"

  RESULT=0
  if echo "$CLEAN_OUTPUT" | grep -qi "panic"; then
    printf '    FAIL: dashboard panicked for reviewing status\n'
    RESULT=1
  fi
  if ! echo "$CLEAN_OUTPUT" | grep -q "Reviewing"; then
    printf '    FAIL: expected header to show reviewing as active work\n'
    RESULT=1
  fi
  if ! echo "$CLEAN_OUTPUT" | grep -Eq '⠋|⠙|⠹|⠸|⠼|⠴|⠦|⠧|⠇'; then
    printf '    FAIL: expected reviewing header to show spinner frames\n'
    RESULT=1
  fi

  rm -rf "$TEST_DIR"
  return $RESULT
}

test_archived_reviews_do_not_drive_current_verdict() {
  TEST_DIR="$(mktemp -d -t factory-test-dash-rounds-XXXXXX)"
  RUN_DIR="${TEST_DIR}/.factory/runs/round-run"
  mkdir -p "${RUN_DIR}/reviews/round-1"
  printf 'reviewing' > "${RUN_DIR}/status"
  printf 'Round brief' > "${RUN_DIR}/brief.md"
  write_review_artifacts "${RUN_DIR}/reviews/round-1" behaviors fail
  write_review_artifacts "${RUN_DIR}/reviews" behaviors pass

  OUTPUT="$(capture_dashboard "$TEST_DIR" round-run)"
  CLEAN_OUTPUT="$(printf '%s' "$OUTPUT" | clean_dashboard_output)"

  RESULT=0
  if ! echo "$CLEAN_OUTPUT" | grep -q "✓ behaviors"; then
    printf '    FAIL: expected current top-level pass verdict tab\n'
    RESULT=1
  fi
  if echo "$CLEAN_OUTPUT" | grep -q "✗ behaviors"; then
    printf '    FAIL: archived fail verdict appeared as current\n'
    RESULT=1
  fi

  rm -rf "$TEST_DIR"
  return $RESULT
}

test_archived_transcripts_do_not_create_current_tabs() {
  TEST_DIR="$(mktemp -d -t factory-test-dash-rounds-XXXXXX)"
  RUN_DIR="${TEST_DIR}/.factory/runs/archived-run"
  mkdir -p "${RUN_DIR}/reviews/round-1"
  printf 'reviewing' > "${RUN_DIR}/status"
  printf 'Archived brief' > "${RUN_DIR}/brief.md"
  write_review_artifacts "${RUN_DIR}/reviews/round-1" behaviors fail

  OUTPUT="$(capture_dashboard "$TEST_DIR" archived-run)"
  CLEAN_OUTPUT="$(printf '%s' "$OUTPUT" | clean_dashboard_output)"

  RESULT=0
  if echo "$CLEAN_OUTPUT" | grep -q "behaviors"; then
    printf '    FAIL: archived reviewer appeared as a current tab\n'
    RESULT=1
  fi

  rm -rf "$TEST_DIR"
  return $RESULT
}

test_stale_reviewer_tabs_disappear_when_top_level_transcripts_are_archived() {
  cargo test --lib dashboard::tests::test_discover_agents_resets_archived_review_round_verdicts \
    >/dev/null 2>&1
}

printf 'test-dashboard-review-rounds\n\n'

run_test "reviewing status shows active work before transcripts" test_reviewing_status_shows_active_work_before_transcripts
run_test "archived reviews do not drive current verdict" test_archived_reviews_do_not_drive_current_verdict
run_test "archived transcripts do not create current tabs" test_archived_transcripts_do_not_create_current_tabs
run_test "stale reviewer tabs disappear when top-level transcripts are archived" test_stale_reviewer_tabs_disappear_when_top_level_transcripts_are_archived

printf '\n  %d passed, %d failed\n' "$PASS" "$FAIL"

if [ "$FAIL" -gt 0 ]; then
  printf '\n  Failures:%b\n' "$ERRORS"
  exit 1
fi
