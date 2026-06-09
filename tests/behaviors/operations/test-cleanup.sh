#!/usr/bin/env bash
# test-cleanup — Verify cleanup behavior via public commands.
#
# Usage:
#   bash tests/behaviors/operations/test-cleanup.sh

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
  OUTPUT_FILE="$(mktemp -t factory-dashboard-output-XXXXXX)"

  (
    sleep 1
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
  OUTPUT="$("$FACTORY_BIN" status 2>&1)"

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
  OUTPUT="$(capture_dashboard_default "$TEST_DIR")"

  RESULT=0
  assert_output_contains "$OUTPUT" "Run: run-planned" || RESULT=1

  rm -rf "$TEST_DIR"
  return $RESULT
}

test_cleanup_work_items_dry_run_and_apply() {
  create_project
  "$FACTORY_BIN" work create work-1 --title "Cleanup work" >/dev/null
  "$FACTORY_BIN" work attempt work-1 attempt-1 >/dev/null
  "$FACTORY_BIN" work create work-active --title "Active work" >/dev/null
  "$FACTORY_BIN" work attempt work-active attempt-1 >/dev/null

  python3 - <<'PY'
import json
from pathlib import Path

path = Path(".factory/work/items/work-1.json")
data = json.loads(path.read_text())
task = data["attempts"][0]["tasks"][0]
data["attempts"][0]["status"] = "complete"
task["status"] = "complete"
task["artifact_area"] = {
    "path": ".factory/work/artifacts/attempt-1/attempt-1-write"
}
task["output"] = {
    "workspace_id": "candidate",
    "workspace_path": "../work-6-work-1-attempt-1",
    "source_branch": "main",
    "commit": "HEAD"
}
path.write_text(json.dumps(data, indent=2) + "\n")

path = Path(".factory/work/items/work-active.json")
data = json.loads(path.read_text())
task = data["attempts"][0]["tasks"][0]
data["attempts"][0]["status"] = "executing"
task["status"] = "executing"
task["artifact_area"] = {
    "path": ".factory/work/artifacts/attempt-1/attempt-1-active"
}
path.write_text(json.dumps(data, indent=2) + "\n")
PY

  ARTIFACT_DIR=".factory/work/artifacts/attempt-1/attempt-1-write"
  mkdir -p "$ARTIFACT_DIR"
  printf 'artifact' > "$ARTIFACT_DIR/result.md"
  ACTIVE_ARTIFACT_DIR=".factory/work/artifacts/attempt-1/attempt-1-active"
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
  rm -rf "$TEST_DIR" "$WORKTREE_DIR" "$ACTIVE_WORKTREE_DIR"
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

printf '\n  %d passed, %d failed\n' "$PASS" "$FAIL"

if [ "$FAIL" -gt 0 ]; then
  printf '\n  Failures:%b\n' "$ERRORS"
  exit 1
fi
