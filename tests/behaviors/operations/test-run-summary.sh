#!/usr/bin/env bash
# test-run-summary — Verify factory summary behavior from the CLI.
#
# Tests the run summary command from a user's perspective by creating
# temporary Factory projects and checking stdout/stderr.
#
# Covers:
#   - factory summary resolves the active run and prints a summary
#   - --run-id selects a specific run
#   - phase and agent activity appear from durable artifacts
#   - sessions.log entries appear in the summary
#   - review verdicts appear grouped by reviewer name
#   - handoff context appears without boilerplate
#   - report.md presence appears without dumping the report
#   - unresolved runs fail with a clear error
#
# Usage:
#   tests/behaviors/operations/test-run-summary.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$(dirname "$(dirname "$SCRIPT_DIR")")")"
FACTORY_BIN="${PROJECT_DIR}/target/debug/factory"

PASS=0
FAIL=0
ERRORS=""

setup_test_project() {
  TEST_DIR="$(mktemp -d -t factory-test-summary-XXXXXX)"
  mkdir -p "${TEST_DIR}/project"
  cd "${TEST_DIR}/project"
  git init -b main > /dev/null 2>&1
  git config commit.gpgsign false
  git config user.email "test@test"
  git config user.name "test"
  echo "test" > README.md
  git add . && git commit -m "init" > /dev/null 2>&1
}

cleanup_test_project() {
  cd /
  rm -rf "$TEST_DIR"
}

create_summary_fixture() {
  mkdir -p .factory/runs/run-alpha/reviews .factory/runs/run-beta/reviews
  printf 'run-alpha' > .factory/active-run

  printf 'executing' > .factory/runs/run-alpha/status
  printf 'local' > .factory/runs/run-alpha/runtime
  printf 'codex' > .factory/runs/run-alpha/coder
  printf 'Summarize alpha run behavior' > .factory/runs/run-alpha/brief.md
  {
    printf '2026-06-06T09:00:00Z session=1 exit=0 duration=12s status=executing\n'
    printf '2026-06-06T09:10:00Z session=2 exit=0 duration=8s status=needs-user\n'
  } > .factory/runs/run-alpha/sessions.log
  {
    printf '# Behavior Review\n\n'
    printf 'Reviewer: review-behaviors\n'
    printf 'Verdict: pass\n'
  } > .factory/runs/run-alpha/reviews/review-behaviors.md
  {
    printf '# Test Review\n\n'
    printf 'Reviewer: review-tests\n'
    printf 'Verdict: fail\n'
  } > .factory/runs/run-alpha/reviews/review-tests.md
  {
    printf '# Handoff\n\n'
    printf 'Question: Choose the deployment target\n'
    printf 'Extra context should not be the first actionable line\n'
  } > .factory/runs/run-alpha/handoff.md
  {
    printf '# Report\n\n'
    printf 'REPORT_BODY_SENTINEL that should not be dumped by summary\n'
  } > .factory/runs/run-alpha/report.md

  printf 'planned' > .factory/runs/run-beta/status
  printf 'local' > .factory/runs/run-beta/runtime
  printf 'claude' > .factory/runs/run-beta/coder
  printf 'Summarize beta run behavior' > .factory/runs/run-beta/brief.md
}

assert_contains() {
  if ! printf '%s' "$1" | grep -q "$2"; then
    printf '    FAIL: output does not contain "%s"\n' "$2"
    printf '    Output:\n%s\n' "$1"
    return 1
  fi
}

assert_not_contains() {
  if printf '%s' "$1" | grep -q "$2"; then
    printf '    FAIL: output should not contain "%s"\n' "$2"
    printf '    Output:\n%s\n' "$1"
    return 1
  fi
}

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

test_summary_resolves_active_run() {
  setup_test_project
  create_summary_fixture

  OUTPUT="$("$FACTORY_BIN" summary 2>&1)"

  RESULT=0
  assert_contains "$OUTPUT" "ID: run-alpha" || RESULT=1
  assert_contains "$OUTPUT" "Status: executing" || RESULT=1
  assert_contains "$OUTPUT" "Phase: authoring" || RESULT=1
  assert_contains "$OUTPUT" "Author: codex (active)" || RESULT=1
  assert_contains "$OUTPUT" "Summarize alpha run behavior" || RESULT=1
  assert_not_contains "$OUTPUT" "run-beta" || RESULT=1

  cleanup_test_project
  return $RESULT
}

test_summary_uses_explicit_run_id() {
  setup_test_project
  create_summary_fixture

  OUTPUT="$("$FACTORY_BIN" summary --run-id run-beta 2>&1)"

  RESULT=0
  assert_contains "$OUTPUT" "ID: run-beta" || RESULT=1
  assert_contains "$OUTPUT" "Status: planned" || RESULT=1
  assert_contains "$OUTPUT" "Phase: ready to run" || RESULT=1
  assert_contains "$OUTPUT" "Author: claude (pending)" || RESULT=1
  assert_contains "$OUTPUT" "Summarize beta run behavior" || RESULT=1
  assert_not_contains "$OUTPUT" "run-alpha" || RESULT=1

  cleanup_test_project
  return $RESULT
}

test_summary_includes_session_history() {
  setup_test_project
  create_summary_fixture

  OUTPUT="$("$FACTORY_BIN" summary --run-id run-alpha 2>&1)"

  RESULT=0
  assert_contains "$OUTPUT" "Recent sessions" || RESULT=1
  assert_contains "$OUTPUT" "session=1" || RESULT=1
  assert_contains "$OUTPUT" "session=2" || RESULT=1
  assert_contains "$OUTPUT" "status=needs-user" || RESULT=1

  cleanup_test_project
  return $RESULT
}

test_summary_includes_review_verdicts() {
  setup_test_project
  create_summary_fixture

  OUTPUT="$("$FACTORY_BIN" summary --run-id run-alpha 2>&1)"

  RESULT=0
  assert_contains "$OUTPUT" "Reviewer verdicts" || RESULT=1
  assert_contains "$OUTPUT" "Reviewers: recent (2 verdicts)" || RESULT=1
  assert_contains "$OUTPUT" "behaviors: pass" || RESULT=1
  assert_contains "$OUTPUT" "tests: fail" || RESULT=1

  cleanup_test_project
  return $RESULT
}

test_summary_includes_handoff_context() {
  setup_test_project
  create_summary_fixture

  OUTPUT="$("$FACTORY_BIN" summary --run-id run-alpha 2>&1)"

  RESULT=0
  assert_contains "$OUTPUT" "Handoff" || RESULT=1
  assert_contains "$OUTPUT" "Question: Choose the deployment target" || RESULT=1

  cleanup_test_project
  return $RESULT
}

test_summary_reports_report_presence_without_dumping() {
  setup_test_project
  create_summary_fixture

  OUTPUT="$("$FACTORY_BIN" summary --run-id run-alpha 2>&1)"

  RESULT=0
  assert_contains "$OUTPUT" "Report" || RESULT=1
  assert_contains "$OUTPUT" "Available: report.md" || RESULT=1
  assert_not_contains "$OUTPUT" "REPORT_BODY_SENTINEL" || RESULT=1

  cleanup_test_project
  return $RESULT
}

test_summary_fails_when_no_run_resolves() {
  setup_test_project
  mkdir -p .factory/runs/run-complete
  printf 'complete' > .factory/runs/run-complete/status
  printf 'Completed run' > .factory/runs/run-complete/brief.md

  set +e
  OUTPUT="$("$FACTORY_BIN" summary 2>&1)"
  EXIT_STATUS=$?
  set -e

  RESULT=0
  if [ "$EXIT_STATUS" -eq 0 ]; then
    printf '    FAIL: expected non-zero exit status\n'
    RESULT=1
  fi
  assert_contains "$OUTPUT" "No active run found" || RESULT=1
  assert_not_contains "$OUTPUT" "Run"$'\n'"  ID:" || RESULT=1

  cleanup_test_project
  return $RESULT
}

printf 'test-run-summary\n\n'

run_test "summary resolves active run" test_summary_resolves_active_run
run_test "summary uses explicit run-id" test_summary_uses_explicit_run_id
run_test "summary includes session history" test_summary_includes_session_history
run_test "summary includes review verdicts" test_summary_includes_review_verdicts
run_test "summary includes handoff context" test_summary_includes_handoff_context
run_test "summary reports report presence without dumping" test_summary_reports_report_presence_without_dumping
run_test "summary fails when no run resolves" test_summary_fails_when_no_run_resolves

printf '\n  %d passed, %d failed\n' "$PASS" "$FAIL"

if [ "$FAIL" -gt 0 ]; then
  printf '\n  Failures:%b\n' "$ERRORS"
  exit 1
fi
