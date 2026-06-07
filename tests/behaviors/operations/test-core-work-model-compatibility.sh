#!/usr/bin/env bash
# test-core-work-model-compatibility - Verify legacy runs work without Work Item state.

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
  TEST_DIR="$(mktemp -d -t factory-core-work-model-compat-XXXXXX)"
  mkdir -p "$TEST_DIR/project"
  cd "$TEST_DIR/project"
  git init -b main > /dev/null 2>&1
  git config commit.gpgsign false
  git config user.email "test@test"
  git config user.name "test"
  printf 'test\n' > README.md
  git add . && git commit -m "init" > /dev/null 2>&1

  mkdir -p .factory/runs/run-legacy
  printf 'run-legacy' > .factory/active-run
  printf 'complete' > .factory/runs/run-legacy/status
  printf 'local' > .factory/runs/run-legacy/runtime
  printf 'claude' > .factory/runs/run-legacy/coder
  printf 'Legacy run without Work Item state' > .factory/runs/run-legacy/brief.md
}

cleanup_test_project() {
  cd /
  rm -rf "$TEST_DIR"
}

assert_contains() {
  if ! printf '%s' "$1" | grep -q "$2"; then
    printf '    FAIL: output does not contain "%s"\n' "$2"
    printf '    Output:\n%s\n' "$1"
    return 1
  fi
}

capture_dashboard() {
  PROJECT_PATH="$1"
  RUN_ID="$2"
  OUTPUT_FILE="$(mktemp -t factory-core-work-model-dashboard-XXXXXX)"

  (
    sleep 1
    printf 'q'
  ) | FACTORY_DASH_BIN="$FACTORY_BIN" \
      FACTORY_DASH_PROJECT="$PROJECT_PATH" \
      FACTORY_DASH_RUN="$RUN_ID" \
      TERM=xterm \
      script -q "$OUTPUT_FILE" sh -c 'stty rows 30 cols 120; "$FACTORY_DASH_BIN" dashboard --run-id "$FACTORY_DASH_RUN" "$FACTORY_DASH_PROJECT"' >/dev/null 2>&1 || true

  perl -pe 's/\e\[[0-9;?]*[ -\/]*[@-~]//g; s/\r/\n/g' "$OUTPUT_FILE"
  rm -f "$OUTPUT_FILE"
}

assert_absent_work_item_state() {
  if find .factory -maxdepth 2 \( -name 'work-items' -o -name 'work-items.json' -o -name 'work_item*' \) | grep -q .; then
    printf '    FAIL: fixture unexpectedly contains Work Item state\n'
    return 1
  fi
}

test_status_summary_dashboard_and_cleanup_without_work_item_state() {
  setup_test_project

  RESULT=0
  assert_absent_work_item_state || RESULT=1

  STATUS_OUTPUT="$("$FACTORY_BIN" status 2>&1)"
  assert_contains "$STATUS_OUTPUT" "run-legacy" || RESULT=1
  assert_contains "$STATUS_OUTPUT" "complete" || RESULT=1
  assert_contains "$STATUS_OUTPUT" "Legacy run without Work Item state" || RESULT=1

  SUMMARY_OUTPUT="$("$FACTORY_BIN" summary --run-id run-legacy 2>&1)"
  assert_contains "$SUMMARY_OUTPUT" "ID: run-legacy" || RESULT=1
  assert_contains "$SUMMARY_OUTPUT" "Status: complete" || RESULT=1
  assert_contains "$SUMMARY_OUTPUT" "Legacy run without Work Item state" || RESULT=1

  HELP_OUTPUT="$("$FACTORY_BIN" --help 2>&1)"
  assert_contains "$HELP_OUTPUT" "review" || RESULT=1

  REVIEW_HELP_OUTPUT="$("$FACTORY_BIN" review --help 2>&1)"
  assert_contains "$REVIEW_HELP_OUTPUT" "Run reviewers against the current codebase" || RESULT=1

  DASHBOARD_OUTPUT="$(capture_dashboard "$PWD" run-legacy)"
  assert_contains "$DASHBOARD_OUTPUT" "run-legacy" || RESULT=1
  assert_contains "$DASHBOARD_OUTPUT" "complete" || RESULT=1

  CLEANUP_OUTPUT="$("$FACTORY_BIN" cleanup 2>&1)"
  assert_contains "$CLEANUP_OUTPUT" "run-legacy" || RESULT=1

  cleanup_test_project
  return $RESULT
}

printf 'test-core-work-model-compatibility\n\n'

run_test "legacy run commands work without Work Item state" test_status_summary_dashboard_and_cleanup_without_work_item_state

printf '\n  %d passed, %d failed\n' "$PASS" "$FAIL"

if [ "$FAIL" -gt 0 ]; then
  printf '\n  Failures:%b\n' "$ERRORS"
  exit 1
fi
