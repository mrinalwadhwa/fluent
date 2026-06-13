#!/usr/bin/env bash
# test-run-curation - Verify repository-state curation behaviors.
#
# These checks exercise the user-visible Factory state: observation
# queues, run artifacts, and status output. They do not inspect
# implementation code.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$(dirname "$(dirname "$SCRIPT_DIR")")")"
FACTORY_BIN="${FACTORY_BIN_OVERRIDE:-${PROJECT_DIR}/target/debug/factory}"

source "${PROJECT_DIR}/tests/lib/run_test.sh"
LOG_DIR="${PROJECT_DIR}/tests/output/$(basename "$0" .sh)"


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
  assert_not_contains ".factory/observations.md" "Dashboard \"reviewing\" status shows no spinner" || RESULT=1
  assert_not_contains ".factory/observations.md" "Currently only the header phase label animates" || RESULT=1
  assert_not_contains ".factory/observations.md" "active agents in the agent tabs (spinner next to" || RESULT=1
  assert_not_contains ".factory/observations.md" "and the \"reviewing\" status" || RESULT=1
  assert_not_contains ".factory/observations.md" "Factory review detection is commit-based" || RESULT=1
  assert_not_contains ".factory/observations.md" "\`factory resume\` should support non-interactive automation" || RESULT=1

  assert_contains ".factory/observations-resolved.md" "Add a \`factory version\` command" || RESULT=1
  assert_contains ".factory/observations-resolved.md" "Resolved: fc81453, 1a696f5" || RESULT=1
  assert_contains ".factory/observations-resolved.md" "The dashboard never removes runs that were deleted" || RESULT=1
  assert_contains ".factory/observations-resolved.md" "Resolved: 1fc4b8c" || RESULT=1
  assert_contains ".factory/observations-resolved.md" "Codex sandbox support needs a focused verification run" || RESULT=1
  assert_contains ".factory/observations-resolved.md" "Resolved: 77aeddd, 11d0313, d50b2c3" || RESULT=1
  assert_contains ".factory/observations-resolved.md" "Dashboard \"reviewing\" status shows no spinner" || RESULT=1
  assert_contains ".factory/observations-resolved.md" "Resolved: 04b083a, 307c112, a6b8f8a, bae62ca, 5a46c92" || RESULT=1
  assert_contains ".factory/observations-resolved.md" "Factory review detection is commit-based" || RESULT=1
  assert_contains ".factory/observations-resolved.md" "Resolved: cfba7c3" || RESULT=1
  assert_contains ".factory/observations-resolved.md" "\`factory resume\` should support non-interactive automation" || RESULT=1
  assert_contains ".factory/observations-resolved.md" "Resolved: bd82a58, a2f8d84, e057ae7, c757421, 53077d6" || RESULT=1

  return $RESULT
}

test_smoke_runs_are_marked_non_actionable() {
  cd "$PROJECT_DIR"

  OUTPUT="$("$FACTORY_BIN" status --runs 2>&1)"

  RESULT=0
  for RUN_ID in \
    20260605-codex-installed-smoke \
    20260606-codex-installed-ca-smoke \
    20260606-codex-installed-seatbelt-smoke
  do
    RUN_DIR=".factory/runs/${RUN_ID}"
    if [ ! -d "$RUN_DIR" ]; then
      printf '    FAIL: .factory/runs/%s is missing superseded artifacts\n' "$RUN_ID"
      RESULT=1
    fi

    assert_contains "${RUN_DIR}/status" "merged" || RESULT=1
    assert_contains "${RUN_DIR}/handoff.md" "non-actionable" || RESULT=1
    assert_contains "${RUN_DIR}/handoff.md" "superseded" || RESULT=1
    assert_contains "${RUN_DIR}/report.md" "later landed verification" || RESULT=1

    if printf '%s' "$OUTPUT" \
      | grep -F "$RUN_ID" \
      | grep -Eq 'planned|executing|reviewing|needs-user|failed'
    then
      printf '    FAIL: status output still shows %s as pending work\n' "$RUN_ID"
      RESULT=1
    fi
  done

  return $RESULT
}

test_cleanup_policy_direction_is_captured() {
  cd "$PROJECT_DIR"

  RESULT=0
  assert_not_contains ".factory/observations.md" "Stale run artifacts need a first-class cleanup policy" || RESULT=1
  assert_contains ".factory/observations-resolved.md" "Stale run artifacts need a first-class cleanup policy" || RESULT=1
  assert_contains ".factory/observations-resolved.md" "Cleanup should happen where the Factory state" || RESULT=1
  assert_contains ".factory/observations-resolved.md" "resides: the source worktree's \`.factory/runs\` registry" || RESULT=1
  assert_contains ".factory/observations-resolved.md" "source worktree's \`.factory/runs\` registry" || RESULT=1
  assert_contains ".factory/observations-resolved.md" "should not be modeled as ordinary author" || RESULT=1
  assert_contains ".factory/observations-resolved.md" "work inside an isolated run worktree" || RESULT=1
  assert_contains ".factory/observations-resolved.md" "Landed and reported runs should remain" || RESULT=1
  assert_contains ".factory/observations-resolved.md" "queryable but should not dominate" || RESULT=1
  assert_contains ".factory/observations-resolved.md" "Complete and landed stale runs need a \`factory cleanup\` command" || RESULT=1
  assert_contains ".factory/observations-resolved.md" "preserves the cleanup reason in the source Factory state" || RESULT=1
  assert_contains ".factory/observations-resolved.md" "registered git worktrees safely" || RESULT=1
  assert_contains ".factory/observations-resolved.md" "Superseded planned runs, failed smoke" || RESULT=1
  assert_contains ".factory/observations-resolved.md" "outside the current" || RESULT=1
  assert_contains ".factory/observations-resolved.md" "cleanup command scope" || RESULT=1
  assert_contains ".factory/observations-resolved.md" "writes" || RESULT=1
  assert_contains ".factory/observations-resolved.md" "\`cleaned.md\`" || RESULT=1

  return $RESULT
}

printf 'test-run-curation\n\n'

run_test "resolved observations are curated" test_resolved_observations_are_curated
run_test "smoke runs are marked non-actionable" test_smoke_runs_are_marked_non_actionable
run_test "cleanup policy direction is captured" test_cleanup_policy_direction_is_captured

summarize_and_exit
