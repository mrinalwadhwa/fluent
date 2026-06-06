#!/usr/bin/env bash
# test-run-curation - Verify repository-state curation behaviors.
#
# These checks exercise the user-visible Factory state: observation
# queues and status output. They do not inspect implementation code.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$(dirname "$(dirname "$SCRIPT_DIR")")")"

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

assert_contains() {
  FILE="$1"
  PATTERN="$2"
  if ! grep -Fq "$PATTERN" "$FILE"; then
    printf '    FAIL: %s does not contain %s\n' "$FILE" "$PATTERN"
    return 1
  fi
}

assert_not_contains() {
  FILE="$1"
  PATTERN="$2"
  if grep -Fq "$PATTERN" "$FILE"; then
    printf '    FAIL: %s still contains %s\n' "$FILE" "$PATTERN"
    return 1
  fi
}

test_resolved_observations_are_curated() {
  cd "$PROJECT_DIR"

  RESULT=0
  assert_not_contains ".factory/observations.md" "Add a \`factory version\` command" || RESULT=1
  assert_not_contains ".factory/observations.md" "The dashboard never removes runs that were deleted" || RESULT=1
  assert_not_contains ".factory/observations.md" "Codex sandbox support needs a focused verification run" || RESULT=1

  assert_contains ".factory/observations-resolved.md" "Add a \`factory version\` command" || RESULT=1
  assert_contains ".factory/observations-resolved.md" "Resolved: fc81453, 1a696f5" || RESULT=1
  assert_contains ".factory/observations-resolved.md" "The dashboard never removes runs that were deleted" || RESULT=1
  assert_contains ".factory/observations-resolved.md" "Resolved: 1fc4b8c" || RESULT=1
  assert_contains ".factory/observations-resolved.md" "Codex sandbox support needs a focused verification run" || RESULT=1
  assert_contains ".factory/observations-resolved.md" "Resolved: 77aeddd, 11d0313, d50b2c3" || RESULT=1

  return $RESULT
}

test_smoke_runs_do_not_appear_pending() {
  cd "$PROJECT_DIR"

  OUTPUT="$(cargo run --quiet -- status 2>&1)"

  RESULT=0
  for RUN_ID in \
    20260605-codex-installed-smoke \
    20260606-codex-installed-ca-smoke \
    20260606-codex-installed-seatbelt-smoke
  do
    if printf '%s' "$OUTPUT" | grep -Fq "$RUN_ID"; then
      printf '    FAIL: status output still shows %s\n' "$RUN_ID"
      RESULT=1
    fi
    if [ -d ".factory/runs/${RUN_ID}" ]; then
      printf '    FAIL: .factory/runs/%s still exists in the active run registry\n' "$RUN_ID"
      RESULT=1
    fi
  done

  return $RESULT
}

test_cleanup_policy_direction_is_captured() {
  cd "$PROJECT_DIR"

  RESULT=0
  assert_contains ".factory/observations.md" "Stale run artifacts need a first-class cleanup policy" || RESULT=1
  assert_contains ".factory/observations.md" "Cleanup should happen where the Factory state" || RESULT=1
  assert_contains ".factory/observations.md" "resides: the source worktree's \`.factory/runs\` registry" || RESULT=1
  assert_contains ".factory/observations.md" "source worktree's \`.factory/runs\` registry" || RESULT=1
  assert_contains ".factory/observations.md" "should not be modeled as ordinary author" || RESULT=1
  assert_contains ".factory/observations.md" "work inside an isolated run worktree" || RESULT=1
  assert_contains ".factory/observations.md" "Landed and reported runs should remain" || RESULT=1
  assert_contains ".factory/observations.md" "queryable but should not dominate" || RESULT=1
  assert_contains ".factory/observations.md" "Superseded planned," || RESULT=1
  assert_contains ".factory/observations.md" "or \`factory cleanup\`" || RESULT=1
  assert_contains ".factory/observations.md" "preserves the reason in the source Factory state" || RESULT=1
  assert_contains ".factory/observations.md" "removes registered git worktrees safely" || RESULT=1

  return $RESULT
}

test_resume_gap_is_captured() {
  cd "$PROJECT_DIR"

  RESULT=0
  assert_contains ".factory/observations.md" "\`factory resume\` should support non-interactive automation" || RESULT=1
  assert_contains ".factory/observations.md" "stdin is not a terminal" || RESULT=1
  assert_contains ".factory/observations.md" "separate headless resume path" || RESULT=1

  return $RESULT
}

printf 'test-run-curation\n\n'

run_test "resolved observations are curated" test_resolved_observations_are_curated
run_test "smoke runs do not appear pending" test_smoke_runs_do_not_appear_pending
run_test "cleanup policy direction is captured" test_cleanup_policy_direction_is_captured
run_test "resume automation gap is captured" test_resume_gap_is_captured

printf '\n  %d passed, %d failed\n' "$PASS" "$FAIL"

if [ "$FAIL" -ne 0 ]; then
  printf '\nFailed tests:%b\n' "$ERRORS"
  exit 1
fi
