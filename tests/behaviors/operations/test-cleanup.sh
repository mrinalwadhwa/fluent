#!/usr/bin/env bash
# test-cleanup — Verify cleanup behavior via public commands.
#
# Usage:
#   bash tests/behaviors/operations/test-cleanup.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$(dirname "$(dirname "$SCRIPT_DIR")")")"
FACTORY_BIN="${FACTORY_BIN_OVERRIDE:-${PROJECT_DIR}/target/debug/factory}"

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

assert_output_contains() {
  if ! echo "$1" | grep -q "$2"; then
    printf '    FAIL: output does not contain "%s"\n' "$2"
    return 1
  fi
}

assert_output_not_contains() {
  if echo "$1" | grep -q "$2"; then
    printf '    FAIL: output unexpectedly contains "%s"\n' "$2"
    return 1
  fi
}

create_project() {
  TEST_DIR="$(mktemp -d -t factory-test-cleanup-XXXXXX)"
  cd "$TEST_DIR"
  git init -q
  git config user.email "factory@example.com"
  git config user.name "Factory Test"
  git config commit.gpgsign false
  touch README.md
  git add README.md
  git -c commit.gpgsign=false commit -qm "init"
}

create_project_with_unique_sibling_parent() {
  TEST_PARENT_DIR="$(mktemp -d -t factory-test-cleanup-parent-XXXXXX)"
  TEST_DIR="${TEST_PARENT_DIR}/source"
  mkdir -p "$TEST_DIR"
  cd "$TEST_DIR"
  git init -q
  git config user.email "factory@example.com"
  git config user.name "Factory Test"
  git config commit.gpgsign false
  touch README.md
  git add README.md
  git -c commit.gpgsign=false commit -qm "init"
}

create_run() {
  RUN_ID="$1"
  STATUS="$2"
  BRIEF="$3"
  mkdir -p ".factory/runs/${RUN_ID}"
  printf '%s' "$STATUS" > ".factory/runs/${RUN_ID}/status"
  printf '%s' "$BRIEF" > ".factory/runs/${RUN_ID}/brief.md"
  printf 'local' > ".factory/runs/${RUN_ID}/runtime"
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

  perl -pe 's/\e\[[0-9;?]*[ -\/]*[@-~]//g; s/\r/\n/g' "$OUTPUT_FILE"
  rm -f "$OUTPUT_FILE"
}

test_cleanup_selects_stale_complete_and_landed_runs_by_default() {
  create_project
  create_run "run-complete" "complete" "Complete brief"
  create_run "run-landed" "landed" "Landed brief"
  create_run "run-failed" "failed" "Failed brief"
  create_run "run-executing" "executing" "Executing brief"

  OUTPUT="$("$FACTORY_BIN" cleanup 2>&1)"

  RESULT=0
  assert_output_contains "$OUTPUT" "would clean run-complete (complete)" || RESULT=1
  assert_output_contains "$OUTPUT" "would clean run-landed (landed)" || RESULT=1
  assert_output_not_contains "$OUTPUT" "run-failed" || RESULT=1
  assert_output_not_contains "$OUTPUT" "run-executing" || RESULT=1

  rm -rf "$TEST_DIR"
  return $RESULT
}

test_cleanup_preserves_run_directory_and_writes_context() {
  create_project
  create_run "run-complete" "complete" "Complete brief"

  "$FACTORY_BIN" cleanup --apply >/dev/null

  RESULT=0
  test -d ".factory/runs/run-complete" || {
    printf '    FAIL: run directory was removed\n'
    RESULT=1
  }
  test "$(cat .factory/runs/run-complete/status)" = "complete" || {
    printf '    FAIL: status changed after cleanup\n'
    RESULT=1
  }
  assert_output_contains "$(cat .factory/runs/run-complete/cleaned.md)" "Reason: stale terminal run cleanup" || RESULT=1

  rm -rf "$TEST_DIR"
  return $RESULT
}

test_cleanup_removes_registered_worktree() {
  create_project
  WORKTREE_DIR="$(mktemp -d -t factory-test-cleanup-wt-XXXXXX)"
  rmdir "$WORKTREE_DIR"
  git worktree add -q -b cleanup-run "$WORKTREE_DIR" HEAD
  create_run "run-worktree" "complete" "Worktree brief"
  printf '%s' "$WORKTREE_DIR" > ".factory/runs/run-worktree/worktree"

  OUTPUT="$("$FACTORY_BIN" cleanup --apply 2>&1)"

  RESULT=0
  assert_output_contains "$OUTPUT" "removed registered worktree" || RESULT=1
  if [ -d "$WORKTREE_DIR" ]; then
    printf '    FAIL: registered worktree path still exists\n'
    RESULT=1
  fi
  if git worktree list --porcelain | grep -q "$WORKTREE_DIR"; then
    printf '    FAIL: git still registers removed worktree\n'
    RESULT=1
  fi

  rm -rf "$TEST_DIR" "$WORKTREE_DIR"
  return $RESULT
}

test_cleanup_skips_unregistered_worktree_path() {
  create_project
  UNREGISTERED_DIR="$(mktemp -d -t factory-test-cleanup-unregistered-XXXXXX)"
  create_run "run-unregistered" "complete" "Unregistered brief"
  printf '%s' "$UNREGISTERED_DIR" > ".factory/runs/run-unregistered/worktree"

  OUTPUT="$("$FACTORY_BIN" cleanup --apply 2>&1)"

  RESULT=0
  assert_output_contains "$OUTPUT" "skipped unregistered worktree" || RESULT=1
  if [ ! -d "$UNREGISTERED_DIR" ]; then
    printf '    FAIL: unregistered worktree path was removed\n'
    RESULT=1
  fi

  rm -rf "$TEST_DIR" "$UNREGISTERED_DIR"
  return $RESULT
}

test_cleanup_dry_run_does_not_change_artifacts_or_worktrees() {
  create_project
  WORKTREE_DIR="$(mktemp -d -t factory-test-cleanup-dry-wt-XXXXXX)"
  rmdir "$WORKTREE_DIR"
  git worktree add -q -b cleanup-dry-run "$WORKTREE_DIR" HEAD
  create_run "run-complete" "complete" "Complete brief"
  printf '%s' "$WORKTREE_DIR" > ".factory/runs/run-complete/worktree"

  OUTPUT="$("$FACTORY_BIN" cleanup 2>&1)"

  RESULT=0
  assert_output_contains "$OUTPUT" "would remove registered worktree" || RESULT=1
  if [ -f ".factory/runs/run-complete/cleaned.md" ]; then
    printf '    FAIL: dry run wrote cleanup context\n'
    RESULT=1
  fi
  if [ ! -d "$WORKTREE_DIR" ]; then
    printf '    FAIL: dry run changed worktree path\n'
    RESULT=1
  fi
  test "$(cat .factory/runs/run-complete/status)" = "complete" || {
    printf '    FAIL: dry run changed status\n'
    RESULT=1
  }

  git worktree remove --force "$WORKTREE_DIR" >/dev/null 2>&1 || true
  rm -rf "$TEST_DIR" "$WORKTREE_DIR"
  return $RESULT
}

test_status_lists_cleaned_runs_with_original_status() {
  create_project
  create_run "run-complete" "complete" "Complete brief"

  "$FACTORY_BIN" cleanup --apply >/dev/null
  OUTPUT="$("$FACTORY_BIN" status --runs 2>&1)"

  RESULT=0
  assert_output_contains "$OUTPUT" "run-complete" || RESULT=1
  assert_output_contains "$OUTPUT" "complete" || RESULT=1
  assert_output_not_contains "$OUTPUT" "cleaned" || RESULT=1

  rm -rf "$TEST_DIR"
  return $RESULT
}

test_dashboard_prefers_actionable_run_over_cleaned_run() {
  create_project
  create_run "run-cleaned" "complete" "Cleaned brief"
  create_run "run-planned" "planned" "Planned brief"

  "$FACTORY_BIN" cleanup --apply >/dev/null
  OUTPUT="$(capture_dashboard_default "$TEST_DIR" "r")"

  RESULT=0
  assert_output_contains "$OUTPUT" "Run: run-planned" || RESULT=1

  rm -rf "$TEST_DIR"
  return $RESULT
}

test_cleanup_work_items_dry_run_and_apply() {
  create_project_with_unique_sibling_parent
  "$FACTORY_BIN" work create work-1 --title "Cleanup work" >/dev/null
  "$FACTORY_BIN" work attempt work-1 attempt-1 >/dev/null
  "$FACTORY_BIN" work create work-active --title "Active work" >/dev/null
  "$FACTORY_BIN" work attempt work-active attempt-1 >/dev/null

  python3 - <<'PY'
import json
from pathlib import Path

path = Path(".factory/work/attempts/work-1/attempt-1.json")
attempt = json.loads(path.read_text())
attempt["status"] = "complete"
path.write_text(json.dumps(attempt, indent=2) + "\n")

path = Path(".factory/work/tasks/work-1/attempt-1/attempt-1-write.json")
task = json.loads(path.read_text())
task["status"] = "complete"
task["artifact_area"] = {
    "path": ".factory/work/artifacts/work-1/attempt-1/attempt-1-write"
}
task["output"] = {
    "workspace_id": "candidate",
    "workspace_path": "../work-6-work-1-attempt-1",
    "source_branch": "main",
    "commit": "HEAD"
}
path.write_text(json.dumps(task, indent=2) + "\n")

path = Path(".factory/work/attempts/work-active/attempt-1.json")
attempt = json.loads(path.read_text())
attempt["status"] = "executing"
path.write_text(json.dumps(attempt, indent=2) + "\n")

path = Path(".factory/work/tasks/work-active/attempt-1/attempt-1-write.json")
task = json.loads(path.read_text())
task["status"] = "executing"
task["artifact_area"] = {
    "path": ".factory/work/artifacts/work-active/attempt-1/attempt-1-active"
}
path.write_text(json.dumps(task, indent=2) + "\n")
PY

  ARTIFACT_DIR=".factory/work/artifacts/work-1/attempt-1/attempt-1-write"
  mkdir -p "$ARTIFACT_DIR"
  printf 'artifact' > "$ARTIFACT_DIR/result.md"
  ACTIVE_ARTIFACT_DIR=".factory/work/artifacts/work-active/attempt-1/attempt-1-active"
  mkdir -p "$ACTIVE_ARTIFACT_DIR"
  printf 'active artifact' > "$ACTIVE_ARTIFACT_DIR/result.md"

  WORKTREE_DIR="$(cd .. && pwd)/work-6-work-1-attempt-1"
  BRANCH_NAME="work/work-1/attempt-1/attempt-1-write"
  git worktree add -q -b "$BRANCH_NAME" "$WORKTREE_DIR" HEAD
  ACTIVE_WORKTREE_DIR="$(cd .. && pwd)/work-11-work-active-attempt-1"
  ACTIVE_BRANCH_NAME="work/work-active/attempt-1/attempt-1-write"
  git worktree add -q -b "$ACTIVE_BRANCH_NAME" "$ACTIVE_WORKTREE_DIR" HEAD

  OUTPUT="$("$FACTORY_BIN" cleanup 2>&1)"

  RESULT=0
  assert_output_contains "$OUTPUT" "would clean Work Item work-1" || RESULT=1
  assert_output_contains "$OUTPUT" "would remove registered worktree" || RESULT=1
  assert_output_contains "$OUTPUT" "would remove Work branch" || RESULT=1
  assert_output_contains "$OUTPUT" "would remove Work artifact" || RESULT=1
  assert_output_not_contains "$OUTPUT" "work-active" || RESULT=1
  if [ ! -f ".factory/work/items/work-1.json" ]; then
    printf '    FAIL: dry run removed Work Item state\n'
    RESULT=1
  fi
  if [ ! -d "$WORKTREE_DIR" ]; then
    printf '    FAIL: dry run removed Work worktree\n'
    RESULT=1
  fi
  if [ ! -d "$ARTIFACT_DIR" ]; then
    printf '    FAIL: dry run removed Work artifact\n'
    RESULT=1
  fi
  if [ ! -f ".factory/work/items/work-active.json" ]; then
    printf '    FAIL: dry run removed active Work Item state\n'
    RESULT=1
  fi
  if [ ! -d "$ACTIVE_WORKTREE_DIR" ]; then
    printf '    FAIL: dry run removed active Work worktree\n'
    RESULT=1
  fi
  if [ ! -d "$ACTIVE_ARTIFACT_DIR" ]; then
    printf '    FAIL: dry run removed active Work artifact\n'
    RESULT=1
  fi

  APPLY_OUTPUT="$("$FACTORY_BIN" cleanup --apply 2>&1)"
  assert_output_contains "$APPLY_OUTPUT" "cleaned Work Item work-1" || RESULT=1
  assert_output_contains "$APPLY_OUTPUT" "removed registered worktree" || RESULT=1
  assert_output_contains "$APPLY_OUTPUT" "removed Work branch" || RESULT=1
  if [ -f ".factory/work/items/work-1.json" ]; then
    printf '    FAIL: apply kept Work Item state\n'
    RESULT=1
  fi
  if [ ! -f ".factory/work/items/work-active.json" ]; then
    printf '    FAIL: apply removed active Work Item state\n'
    RESULT=1
  fi
  if [ -d "$WORKTREE_DIR" ]; then
    printf '    FAIL: apply kept Work worktree\n'
    RESULT=1
  fi
  if [ -d "$ARTIFACT_DIR" ]; then
    printf '    FAIL: apply kept Work artifact\n'
    RESULT=1
  fi
  if [ ! -d "$ACTIVE_WORKTREE_DIR" ]; then
    printf '    FAIL: apply removed active Work worktree\n'
    RESULT=1
  fi
  if [ ! -d "$ACTIVE_ARTIFACT_DIR" ]; then
    printf '    FAIL: apply removed active Work artifact\n'
    RESULT=1
  fi
  if git show-ref --verify --quiet "refs/heads/${BRANCH_NAME}"; then
    printf '    FAIL: apply kept Work branch\n'
    RESULT=1
  fi
  if ! git show-ref --verify --quiet "refs/heads/${ACTIVE_BRANCH_NAME}"; then
    printf '    FAIL: apply removed active Work branch\n'
    RESULT=1
  fi

  git worktree remove --force "$WORKTREE_DIR" >/dev/null 2>&1 || true
  git worktree remove --force "$ACTIVE_WORKTREE_DIR" >/dev/null 2>&1 || true
  rm -rf "$TEST_PARENT_DIR"
  rm -rf "$WORKTREE_DIR" "$ACTIVE_WORKTREE_DIR"
  return $RESULT
}

test_cleanup_work_items_remove_orphan_artifact_roots() {
  create_project
  "$FACTORY_BIN" work create work-active --title "Active work" >/dev/null

  ORPHAN_ROOT=".factory/work/artifacts/work-orphan"
  ACTIVE_ROOT=".factory/work/artifacts/work-active"
  FILE_ENTRY=".factory/work/artifacts/not-a-directory"
  mkdir -p "$ORPHAN_ROOT/attempt-1/task-1"
  printf 'orphan artifact' > "$ORPHAN_ROOT/attempt-1/task-1/result.md"
  mkdir -p "$ACTIVE_ROOT/attempt-1/task-1"
  printf 'active artifact' > "$ACTIVE_ROOT/attempt-1/task-1/result.md"
  printf 'keep file entries' > "$FILE_ENTRY"

  OUTPUT="$("$FACTORY_BIN" cleanup 2>&1)"

  RESULT=0
  assert_output_contains "$OUTPUT" "would remove orphan Work artifact root" || RESULT=1
  assert_output_contains "$OUTPUT" "work-orphan" || RESULT=1
  assert_output_not_contains "$OUTPUT" "work-active" || RESULT=1
  assert_output_not_contains "$OUTPUT" "not-a-directory" || RESULT=1
  if [ ! -d "$ORPHAN_ROOT" ]; then
    printf '    FAIL: dry run removed orphan Work artifact root\n'
    RESULT=1
  fi
  if [ ! -d "$ACTIVE_ROOT" ]; then
    printf '    FAIL: dry run removed active Work artifact root\n'
    RESULT=1
  fi
  if [ ! -f "$FILE_ENTRY" ]; then
    printf '    FAIL: dry run removed Work artifact file entry\n'
    RESULT=1
  fi

  APPLY_OUTPUT="$("$FACTORY_BIN" cleanup --apply 2>&1)"
  assert_output_contains "$APPLY_OUTPUT" "removed orphan Work artifact root" || RESULT=1
  assert_output_contains "$APPLY_OUTPUT" "work-orphan" || RESULT=1
  assert_output_not_contains "$APPLY_OUTPUT" "work-active" || RESULT=1
  assert_output_not_contains "$APPLY_OUTPUT" "not-a-directory" || RESULT=1
  if [ -d "$ORPHAN_ROOT" ]; then
    printf '    FAIL: apply kept orphan Work artifact root\n'
    RESULT=1
  fi
  if [ ! -d "$ACTIVE_ROOT" ]; then
    printf '    FAIL: apply removed active Work artifact root\n'
    RESULT=1
  fi
  if [ ! -f "$FILE_ENTRY" ]; then
    printf '    FAIL: apply removed Work artifact file entry\n'
    RESULT=1
  fi

  rm -rf "$TEST_DIR"
  return $RESULT
}

test_cleanup_work_items_ignore_unmanaged_artifacts() {
  create_project
  "$FACTORY_BIN" work create work-1 --title "Cleanup work" >/dev/null
  "$FACTORY_BIN" work attempt work-1 attempt-1 >/dev/null

  OUTSIDE_DIR="$(mktemp -d -t factory-test-cleanup-outside-XXXXXX)"
  OUTSIDE_FILE="${OUTSIDE_DIR}/outside.md"
  PARENT_ESCAPE_FILE="../outside-artifact.md"

  python3 - "$OUTSIDE_FILE" "$PARENT_ESCAPE_FILE" <<'PY'
import json
import sys
from pathlib import Path

absolute_path = sys.argv[1]
parent_escape_path = sys.argv[2]

path = Path(".factory/work/attempts/work-1/attempt-1.json")
attempt = json.loads(path.read_text())
attempt["status"] = "complete"
attempt["artifacts"] = [
    {"producer_id": "outside-absolute", "path": absolute_path},
    {"producer_id": "outside-parent", "path": parent_escape_path},
    {
        "producer_id": "managed",
        "path": ".factory/work/artifacts/work-1/attempt-1/attempt-1-review/review.md",
    },
]
path.write_text(json.dumps(attempt, indent=2) + "\n")

path = Path(".factory/work/tasks/work-1/attempt-1/attempt-1-write.json")
task = json.loads(path.read_text())
task["status"] = "complete"
task["artifact_area"] = {
    "path": ".factory/work/artifacts/work-1/attempt-1/attempt-1-write"
}
task["output"] = {
    "workspace_id": "candidate",
    "workspace_path": "../work-6-work-1-attempt-1",
    "source_branch": "main",
    "commit": "HEAD",
}
path.write_text(json.dumps(task, indent=2) + "\n")
PY

  ARTIFACT_DIR=".factory/work/artifacts/work-1/attempt-1/attempt-1-write"
  MANAGED_REVIEW_ARTIFACT=".factory/work/artifacts/work-1/attempt-1/attempt-1-review/review.md"
  mkdir -p "$ARTIFACT_DIR" "$(dirname "$MANAGED_REVIEW_ARTIFACT")"
  printf 'artifact' > "$ARTIFACT_DIR/result.md"
  printf 'review' > "$MANAGED_REVIEW_ARTIFACT"
  printf 'outside' > "$OUTSIDE_FILE"
  printf 'parent escape' > "$PARENT_ESCAPE_FILE"

  OUTPUT="$("$FACTORY_BIN" cleanup --apply 2>&1)"

  RESULT=0
  assert_output_contains "$OUTPUT" "cleaned Work Item work-1" || RESULT=1
  assert_output_contains "$OUTPUT" "removed Work artifact" || RESULT=1
  assert_output_not_contains "$OUTPUT" "$OUTSIDE_FILE" || RESULT=1
  assert_output_not_contains "$OUTPUT" "$PARENT_ESCAPE_FILE" || RESULT=1
  if [ ! -f "$OUTSIDE_FILE" ]; then
    printf '    FAIL: apply removed absolute unmanaged artifact\n'
    RESULT=1
  fi
  if [ ! -f "$PARENT_ESCAPE_FILE" ]; then
    printf '    FAIL: apply removed parent-directory unmanaged artifact\n'
    RESULT=1
  fi
  if [ -d "$ARTIFACT_DIR" ]; then
    printf '    FAIL: apply kept managed task artifact directory\n'
    RESULT=1
  fi
  if [ -f "$MANAGED_REVIEW_ARTIFACT" ]; then
    printf '    FAIL: apply kept managed attempt artifact\n'
    RESULT=1
  fi

  rm -f "$PARENT_ESCAPE_FILE"
  rm -rf "$TEST_DIR" "$OUTSIDE_DIR"
  return $RESULT
}

test_cleanup_selects_abandoned_needs_user_work_item() {
  create_project_with_unique_sibling_parent
  "$FACTORY_BIN" work create work-stale --title "Stale needs-user work" >/dev/null
  "$FACTORY_BIN" work attempt work-stale attempt-1 >/dev/null

  python3 - <<'PY'
import json
from pathlib import Path

attempt_path = Path(".factory/work/attempts/work-stale/attempt-1.json")
attempt = json.loads(attempt_path.read_text())
attempt["status"] = "needs-user"
attempt_path.write_text(json.dumps(attempt, indent=2) + "\n")

task_path = Path(".factory/work/tasks/work-stale/attempt-1/attempt-1-write.json")
task = json.loads(task_path.read_text())
task["status"] = "needs-user"
task_path.write_text(json.dumps(task, indent=2) + "\n")
PY

  "$FACTORY_BIN" work abandon work-stale --reason "replacement landed" >/dev/null

  WORKTREE_DIR="$(cd .. && pwd)/work-10-work-stale-attempt-1"
  BRANCH_NAME="work/work-stale/attempt-1/attempt-1-write"
  git worktree add -q -b "$BRANCH_NAME" "$WORKTREE_DIR" HEAD

  OUTPUT="$("$FACTORY_BIN" cleanup 2>&1)"

  RESULT=0
  assert_output_contains "$OUTPUT" "would clean Work Item work-stale" || RESULT=1
  assert_output_contains "$OUTPUT" "would remove registered worktree" || RESULT=1
  assert_output_contains "$OUTPUT" "would remove Work branch" || RESULT=1
  if [ ! -f ".factory/work/items/work-stale.json" ]; then
    printf '    FAIL: dry run removed abandoned Work Item state\n'
    RESULT=1
  fi

  APPLY_OUTPUT="$("$FACTORY_BIN" cleanup --apply 2>&1)"
  assert_output_contains "$APPLY_OUTPUT" "cleaned Work Item work-stale" || RESULT=1
  assert_output_contains "$APPLY_OUTPUT" "removed registered worktree" || RESULT=1
  if [ -f ".factory/work/items/work-stale.json" ]; then
    printf '    FAIL: apply kept abandoned Work Item state\n'
    RESULT=1
  fi
  if [ -d "$WORKTREE_DIR" ]; then
    printf '    FAIL: apply kept abandoned Work worktree\n'
    RESULT=1
  fi
  if git show-ref --verify --quiet "refs/heads/${BRANCH_NAME}"; then
    printf '    FAIL: apply kept abandoned Work branch\n'
    RESULT=1
  fi

  git worktree remove --force "$WORKTREE_DIR" >/dev/null 2>&1 || true
  rm -rf "$TEST_PARENT_DIR"
  rm -rf "$WORKTREE_DIR"
  return $RESULT
}

printf 'test-cleanup\n\n'

run_test "cleanup selects stale complete and landed runs by default" \
  test_cleanup_selects_stale_complete_and_landed_runs_by_default
run_test "cleanup preserves run directory and writes context" \
  test_cleanup_preserves_run_directory_and_writes_context
run_test "cleanup removes registered worktree" \
  test_cleanup_removes_registered_worktree
run_test "cleanup skips unregistered worktree path" \
  test_cleanup_skips_unregistered_worktree_path
run_test "cleanup dry run does not change artifacts or worktrees" \
  test_cleanup_dry_run_does_not_change_artifacts_or_worktrees
run_test "status lists cleaned runs with original status" \
  test_status_lists_cleaned_runs_with_original_status
run_test "dashboard prefers actionable run over cleaned run" \
  test_dashboard_prefers_actionable_run_over_cleaned_run
run_test "cleanup handles Work Items with dry run and apply" \
  test_cleanup_work_items_dry_run_and_apply
run_test "cleanup removes orphan Work artifact roots" \
  test_cleanup_work_items_remove_orphan_artifact_roots
run_test "cleanup ignores unmanaged Work artifact paths" \
  test_cleanup_work_items_ignore_unmanaged_artifacts
run_test "cleanup selects abandoned needs-user Work Items" \
  test_cleanup_selects_abandoned_needs_user_work_item

printf '\n  %d passed, %d failed\n' "$PASS" "$FAIL"

if [ "$FAIL" -gt 0 ]; then
  printf '\n  Failures:%b\n' "$ERRORS"
  exit 1
fi
