#!/usr/bin/env bash
# test-run-summary-updates — Verify updated factory summary reporting.
#
# Tests the new run summary update surface from a user's perspective by
# creating temporary Factory projects and checking CLI output from durable
# run artifacts only.
#
# Usage:
#   tests/behaviors/operations/test-run-summary-updates.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$(dirname "$(dirname "$SCRIPT_DIR")")")"
FACTORY_BIN="${FACTORY_BIN_OVERRIDE:-${PROJECT_DIR}/target/debug/factory}"

PASS=0
FAIL=0
ERRORS=""

setup_test_project() {
  TEST_DIR="$(mktemp -d -t factory-test-summary-updates-XXXXXX)"
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

test_summary_displays_current_phase_label() {
  setup_test_project
  mkdir -p .factory/runs/phase-run
  printf 'phase-run' > .factory/active-run
  printf 'needs-user' > .factory/runs/phase-run/status
  printf 'Phase label brief' > .factory/runs/phase-run/brief.md

  OUTPUT="$("$FACTORY_BIN" summary --run-id phase-run 2>&1)"

  RESULT=0
  assert_contains "$OUTPUT" "Status: needs-user" || RESULT=1
  assert_contains "$OUTPUT" "Phase: needs user" || RESULT=1

  cleanup_test_project
  return $RESULT
}

test_summary_displays_agent_and_child_activity() {
  setup_test_project
  mkdir -p .factory/runs/parent-run .factory/runs/parent-run-1-1 \
    .factory/runs/parent-run-1-2
  printf 'parent-run' > .factory/active-run
  printf 'executing' > .factory/runs/parent-run/status
  printf 'codex' > .factory/runs/parent-run/coder
  printf 'Parent summary brief' > .factory/runs/parent-run/brief.md
  printf 'parent-run-1-1\nparent-run-1-2\n' > .factory/runs/parent-run/children
  printf 'executing' > .factory/runs/parent-run-1-1/status
  printf 'Queue cleanup child' > .factory/runs/parent-run-1-1/brief.md
  printf 'complete' > .factory/runs/parent-run-1-2/status
  printf 'Reporting child' > .factory/runs/parent-run-1-2/brief.md

  OUTPUT="$("$FACTORY_BIN" summary --run-id parent-run 2>&1)"

  RESULT=0
  assert_contains "$OUTPUT" "Author: codex (active)" || RESULT=1
  assert_contains "$OUTPUT" "Child parent-run-1-1: executing - Queue cleanup child" || RESULT=1
  assert_contains "$OUTPUT" "Child parent-run-1-2: complete - Reporting child" || RESULT=1

  cleanup_test_project
  return $RESULT
}

test_summary_uses_durable_activity_without_transcripts() {
  setup_test_project
  mkdir -p .factory/runs/activity-run/sessions/session-1
  printf 'activity-run' > .factory/active-run
  printf 'executing' > .factory/runs/activity-run/status
  printf 'codex' > .factory/runs/activity-run/coder
  printf 'Durable activity brief' > .factory/runs/activity-run/brief.md
  {
    printf '2026-06-06T10:00:00Z session=1 exit=0 duration=4s status=executing\n'
    printf '2026-06-06T10:05:00Z session=2 exit=0 duration=7s status=needs-user\n'
  } > .factory/runs/activity-run/sessions.log
  printf 'TRANSCRIPT_SENTINEL_SHOULD_NOT_APPEAR\n' \
    > .factory/runs/activity-run/sessions/session-1/transcript.jsonl

  OUTPUT="$("$FACTORY_BIN" summary --run-id activity-run 2>&1)"

  RESULT=0
  assert_contains "$OUTPUT" "Recent sessions" || RESULT=1
  assert_contains "$OUTPUT" "session=1" || RESULT=1
  assert_contains "$OUTPUT" "session=2" || RESULT=1
  assert_not_contains "$OUTPUT" "TRANSCRIPT_SENTINEL_SHOULD_NOT_APPEAR" || RESULT=1

  cleanup_test_project
  return $RESULT
}

test_summary_groups_reviewer_verdicts() {
  setup_test_project
  mkdir -p .factory/runs/review-run/reviews
  printf 'review-run' > .factory/active-run
  printf 'reviewing' > .factory/runs/review-run/status
  printf 'Review verdict brief' > .factory/runs/review-run/brief.md
  {
    printf '# Behavior Review\n\n'
    printf 'Reviewer: review-behaviors\n'
    printf 'Verdict: pass\n'
  } > .factory/runs/review-run/reviews/review-behaviors.md
  {
    printf '# Documentation Review\n\n'
    printf 'Reviewer: review-documentation\n'
    printf 'Verdict: uncertain\n'
  } > .factory/runs/review-run/reviews/review-documentation.md

  OUTPUT="$("$FACTORY_BIN" summary --run-id review-run 2>&1)"

  RESULT=0
  assert_contains "$OUTPUT" "Reviewer verdicts" || RESULT=1
  assert_contains "$OUTPUT" "Reviewers: recent (2 verdicts)" || RESULT=1
  assert_contains "$OUTPUT" "behaviors: pass" || RESULT=1
  assert_contains "$OUTPUT" "documentation: uncertain" || RESULT=1

  cleanup_test_project
  return $RESULT
}

test_summary_reports_active_reviewers_without_verdicts() {
  setup_test_project
  mkdir -p .factory/runs/active-review-run
  printf 'active-review-run' > .factory/active-run
  printf 'reviewing' > .factory/runs/active-review-run/status
  printf 'Active review brief' > .factory/runs/active-review-run/brief.md

  OUTPUT="$("$FACTORY_BIN" summary --run-id active-review-run 2>&1)"

  RESULT=0
  assert_contains "$OUTPUT" "Phase: reviewing" || RESULT=1
  assert_contains "$OUTPUT" "Reviewers: active" || RESULT=1

  cleanup_test_project
  return $RESULT
}

test_summary_prefers_explicit_handoff_question() {
  setup_test_project
  mkdir -p .factory/runs/question-run
  printf 'question-run' > .factory/active-run
  printf 'needs-user' > .factory/runs/question-run/status
  printf 'Question brief' > .factory/runs/question-run/brief.md
  {
    printf '# Handoff\n\n'
    printf 'Context: the deployment target is not in the brief.\n'
    printf 'Question: Choose the deployment target\n'
  } > .factory/runs/question-run/handoff.md

  OUTPUT="$("$FACTORY_BIN" summary --run-id question-run 2>&1)"

  RESULT=0
  assert_contains "$OUTPUT" "Question: Choose the deployment target" || RESULT=1
  assert_contains "$OUTPUT" "read handoff.md and answer the open question." || RESULT=1
  assert_not_contains "$OUTPUT" "Context: the deployment target is not in the brief." || RESULT=1

  cleanup_test_project
  return $RESULT
}

printf 'test-run-summary-updates\n\n'

run_test "summary displays current phase label" \
  test_summary_displays_current_phase_label
run_test "summary displays agent and child activity" \
  test_summary_displays_agent_and_child_activity
run_test "summary uses durable activity without transcripts" \
  test_summary_uses_durable_activity_without_transcripts
run_test "summary groups reviewer verdicts" \
  test_summary_groups_reviewer_verdicts
run_test "summary reports active reviewers without verdicts" \
  test_summary_reports_active_reviewers_without_verdicts
run_test "summary prefers explicit handoff question" \
  test_summary_prefers_explicit_handoff_question

printf '\n  %d passed, %d failed\n' "$PASS" "$FAIL"

if [ "$FAIL" -gt 0 ]; then
  printf '\n  Failures:%b\n' "$ERRORS"
  exit 1
fi
