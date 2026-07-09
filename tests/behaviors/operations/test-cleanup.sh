#!/usr/bin/env bash
# test-cleanup — Verify cleanup behavior via public commands.
#
# Usage:
#   bash tests/behaviors/operations/test-cleanup.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$(dirname "$(dirname "$SCRIPT_DIR")")")"
FACTORY_BIN="${FACTORY_BIN_OVERRIDE:-${PROJECT_DIR}/target/debug/factory}"

source "${PROJECT_DIR}/tests/lib/run_test.sh"
LOG_DIR="${PROJECT_DIR}/tests/output/$(basename "$0" .sh)"

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

test_cleanup_work_items_dry_run_and_apply() {
  create_project_with_unique_sibling_parent
  "$FACTORY_BIN" work-item create work-1 --title "Cleanup work" >/dev/null
  "$FACTORY_BIN" attempt create work-1 attempt-1 >/dev/null
  "$FACTORY_BIN" work-item create work-active --title "Active work" >/dev/null
  "$FACTORY_BIN" attempt create work-active attempt-1 >/dev/null

  python3 - <<'PY'
import json
from pathlib import Path

path = Path(".factory/work/attempts/work-1/attempt-1.json")
attempt = json.loads(path.read_text())
attempt["status"] = "complete"
path.write_text(json.dumps(attempt, indent=2) + "\n")

path = Path(".factory/work/tasks/work-1/attempt-1/attempt-1-write-1.json")
task = json.loads(path.read_text())
task["status"] = "complete"
task["artifact_area"] = {
    "path": ".factory/work/artifacts/work-1/attempt-1/attempt-1-write-1"
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

path = Path(".factory/work/tasks/work-active/attempt-1/attempt-1-write-1.json")
task = json.loads(path.read_text())
task["status"] = "executing"
task["artifact_area"] = {
    "path": ".factory/work/artifacts/work-active/attempt-1/attempt-1-active"
}
path.write_text(json.dumps(task, indent=2) + "\n")
PY

  ARTIFACT_DIR=".factory/work/artifacts/work-1/attempt-1/attempt-1-write-1"
  mkdir -p "$ARTIFACT_DIR"
  printf 'artifact' > "$ARTIFACT_DIR/result.md"
  ACTIVE_ARTIFACT_DIR=".factory/work/artifacts/work-active/attempt-1/attempt-1-active"
  mkdir -p "$ACTIVE_ARTIFACT_DIR"
  printf 'active artifact' > "$ACTIVE_ARTIFACT_DIR/result.md"

  WORKTREE_DIR="$(cd .. && pwd)/work-6-work-1-attempt-1"
  BRANCH_NAME="work/work-1/attempt-1/attempt-1-write-1"
  git worktree add -q -b "$BRANCH_NAME" "$WORKTREE_DIR" HEAD
  ACTIVE_WORKTREE_DIR="$(cd .. && pwd)/work-11-work-active-attempt-1"
  ACTIVE_BRANCH_NAME="work/work-active/attempt-1/attempt-1-write-1"
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
  "$FACTORY_BIN" work-item create work-active --title "Active work" >/dev/null

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
  "$FACTORY_BIN" work-item create work-1 --title "Cleanup work" >/dev/null
  "$FACTORY_BIN" attempt create work-1 attempt-1 >/dev/null

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

path = Path(".factory/work/tasks/work-1/attempt-1/attempt-1-write-1.json")
task = json.loads(path.read_text())
task["status"] = "complete"
task["artifact_area"] = {
    "path": ".factory/work/artifacts/work-1/attempt-1/attempt-1-write-1"
}
task["output"] = {
    "workspace_id": "candidate",
    "workspace_path": "../work-6-work-1-attempt-1",
    "source_branch": "main",
    "commit": "HEAD",
}
path.write_text(json.dumps(task, indent=2) + "\n")
PY

  ARTIFACT_DIR=".factory/work/artifacts/work-1/attempt-1/attempt-1-write-1"
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
  "$FACTORY_BIN" work-item create work-stale --title "Stale needs-user work" >/dev/null
  "$FACTORY_BIN" attempt create work-stale attempt-1 >/dev/null

  python3 - <<'PY'
import json
from pathlib import Path

attempt_path = Path(".factory/work/attempts/work-stale/attempt-1.json")
attempt = json.loads(attempt_path.read_text())
attempt["status"] = "needs-user"
attempt_path.write_text(json.dumps(attempt, indent=2) + "\n")

task_path = Path(".factory/work/tasks/work-stale/attempt-1/attempt-1-write-1.json")
task = json.loads(task_path.read_text())
task["status"] = "needs-user"
task_path.write_text(json.dumps(task, indent=2) + "\n")
PY

  "$FACTORY_BIN" work-item abandon work-stale --reason "replacement landed" >/dev/null

  WORKTREE_DIR="$(cd .. && pwd)/work-10-work-stale-attempt-1"
  BRANCH_NAME="work/work-stale/attempt-1/attempt-1-write-1"
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

test_cleanup_skips_abandoned_work_item_with_reviewing_attempt() {
  create_project
  "$FACTORY_BIN" work-item create work-active --title "Active review work" >/dev/null
  "$FACTORY_BIN" attempt create work-active attempt-1 >/dev/null
  "$FACTORY_BIN" work-item abandon work-active --reason "replacement landed" >/dev/null

  python3 - <<'PY'
import json
from pathlib import Path

attempt_path = Path(".factory/work/attempts/work-active/attempt-1.json")
attempt = json.loads(attempt_path.read_text())
attempt["status"] = "reviewing"
attempt_path.write_text(json.dumps(attempt, indent=2) + "\n")
PY

  OUTPUT="$("$FACTORY_BIN" cleanup 2>&1)"

  RESULT=0
  assert_output_not_contains "$OUTPUT" "work-active" || RESULT=1

  rm -rf "$TEST_DIR"
  return $RESULT
}

test_cleanup_skips_abandoned_work_item_with_active_merge_candidate() {
  create_project
  "$FACTORY_BIN" work-item create work-active --title "Active candidate work" >/dev/null
  "$FACTORY_BIN" attempt create work-active attempt-1 >/dev/null
  python3 - <<'PY'
import json
from pathlib import Path

attempt_path = Path(".factory/work/attempts/work-active/attempt-1.json")
attempt = json.loads(attempt_path.read_text())
attempt["status"] = "complete"
attempt["review_state"] = "passed"
attempt_path.write_text(json.dumps(attempt, indent=2) + "\n")

task_path = Path(".factory/work/tasks/work-active/attempt-1/attempt-1-write-1.json")
task = json.loads(task_path.read_text())
task["status"] = "complete"
task["output"] = {
    "workspace_id": "candidate",
    "workspace_path": "../work-6-work-active-attempt-1",
    "source_branch": "main",
    "commit": "abc123",
}
task_path.write_text(json.dumps(task, indent=2) + "\n")
PY
  "$FACTORY_BIN" work-item abandon work-active --reason "replacement landed" >/dev/null
  mkdir -p .factory/work/merge-candidates/work-active
  printf '%s\n' \
    '{' \
    '  "id": "candidate-1",' \
    '  "attempt_id": "attempt-1",' \
    '  "source_workspace": {' \
    '    "id": "candidate",' \
    '    "path": "../work-6-work-active-attempt-1"' \
    '  },' \
    '  "target_workspace": {' \
    '    "id": "target",' \
    '    "path": "."' \
    '  },' \
    '  "source_branch": "main",' \
    '  "target_branch": "main",' \
    '  "candidate_commit": "abc123",' \
    '  "review_state": "reviewing",' \
    '  "merge_state": {' \
    '    "status": "pending"' \
    '  }' \
    '}' > .factory/work/merge-candidates/work-active/candidate-1.json

  REVIEW_OUTPUT="$("$FACTORY_BIN" cleanup 2>&1)"

  RESULT=0
  assert_output_not_contains "$REVIEW_OUTPUT" "work-active" || RESULT=1

  python3 - <<'PY'
import json
from pathlib import Path

candidate_path = Path(".factory/work/merge-candidates/work-active/candidate-1.json")
candidate = json.loads(candidate_path.read_text())
candidate["review_state"] = "pending"
candidate["merge_state"]["status"] = "executing"
candidate_path.write_text(json.dumps(candidate, indent=2) + "\n")
PY

  MERGE_OUTPUT="$("$FACTORY_BIN" cleanup 2>&1)"
  assert_output_not_contains "$MERGE_OUTPUT" "work-active" || RESULT=1

  rm -rf "$TEST_DIR"
  return $RESULT
}

printf 'test-cleanup\n\n'

run_test "cleanup handles Work Items with dry run and apply" \
  test_cleanup_work_items_dry_run_and_apply
run_test "cleanup removes orphan Work artifact roots" \
  test_cleanup_work_items_remove_orphan_artifact_roots
run_test "cleanup ignores unmanaged Work artifact paths" \
  test_cleanup_work_items_ignore_unmanaged_artifacts
run_test "cleanup selects abandoned needs-user Work Items" \
  test_cleanup_selects_abandoned_needs_user_work_item
run_test "cleanup skips abandoned Work with reviewing Attempt" \
  test_cleanup_skips_abandoned_work_item_with_reviewing_attempt
run_test "cleanup skips abandoned Work with active Merge Candidate" \
  test_cleanup_skips_abandoned_work_item_with_active_merge_candidate

summarize_and_exit
