#!/usr/bin/env bash
# test-core-work-model-review-command - Verify factory review prepares durable run state.

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
  TEST_DIR="$(mktemp -d -t factory-core-review-command-XXXXXX)"
  mkdir -p "${TEST_DIR}/project"
  cd "${TEST_DIR}/project"
  git init -b main > /dev/null 2>&1
  git config commit.gpgsign false
  git config user.email "test@test"
  git config user.name "test"
  printf 'test\n' > README.md
  mkdir -p prompts
  cat > prompts/review-tests.md << 'PROMPT'
[system]
Test reviewer.
[changes]
Review changes.
[full]
Review all code.
PROMPT
  git add . && git commit -m "init" > /dev/null 2>&1

  MOCK_BIN="${TEST_DIR}/bin"
  mkdir -p "$MOCK_BIN"
}

cleanup_test_project() {
  cd /
  if [ -d "${TEST_DIR}/project/.git" ]; then
    git -C "${TEST_DIR}/project" worktree list --porcelain 2>/dev/null | \
      grep '^worktree ' | awk '{print $2}' | \
      grep -v "${TEST_DIR}/project" | while read -r wt; do
      git -C "${TEST_DIR}/project" worktree remove --force "$wt" 2>/dev/null || true
    done || true
  fi
  rm -rf "$TEST_DIR"
}

write_mock_codex_reviewer() {
  cat > "${MOCK_BIN}/codex" << 'MOCK_SCRIPT'
#!/usr/bin/env bash
RID="$(cat .factory/active-run)"
RUN_DIR=".factory/runs/${RID}"
mkdir -p "${RUN_DIR}/reviews"
printf '%s\n' "$*" > "${RUN_DIR}/reviews/codex-args"
printf 'Verdict: pass\n\nCommand-level review passed.\n' > "${RUN_DIR}/reviews/review-tests.md"
echo '{"type":"result","subtype":"success","result":"done","session_id":"mock"}'
MOCK_SCRIPT
  chmod +x "${MOCK_BIN}/codex"
}

assert_file_contains() {
  FILE="$1"
  TEXT="$2"
  if ! grep -q "$TEXT" "$FILE"; then
    printf '    FAIL: %s does not contain "%s"\n' "$FILE" "$TEXT"
    if [ -f "$FILE" ]; then
      printf '    File contents:\n'
      cat "$FILE"
      printf '\n'
    fi
    return 1
  fi
}

assert_file_equals() {
  FILE="$1"
  TEXT="$2"
  if [ ! -f "$FILE" ] || [ "$(cat "$FILE")" != "$TEXT" ]; then
    printf '    FAIL: %s does not equal "%s"\n' "$FILE" "$TEXT"
    if [ -f "$FILE" ]; then
      printf '    Actual: %s\n' "$(cat "$FILE")"
    fi
    return 1
  fi
}

test_review_command_prepares_and_runs_review_run() {
  setup_test_project
  write_mock_codex_reviewer

  RESULT=0
  PATH="${MOCK_BIN}:${PATH}" "$FACTORY_BIN" review --no-sandbox --coder codex \
    --run-id review-command --reviewers tests \
    --brief "Review command brief" > "${TEST_DIR}/factory.out" 2>&1 || RESULT=1

  SOURCE_RUN=".factory/runs/review-command"
  assert_file_equals ".factory/active-run" "review-command" || RESULT=1
  assert_file_equals "${SOURCE_RUN}/mode" "review" || RESULT=1
  assert_file_equals "${SOURCE_RUN}/reviewers" "tests" || RESULT=1
  assert_file_equals "${SOURCE_RUN}/brief.md" "Review command brief" || RESULT=1
  assert_file_equals "${SOURCE_RUN}/runtime" "local" || RESULT=1
  assert_file_equals "${SOURCE_RUN}/coder" "codex" || RESULT=1

  if [ ! -f "${SOURCE_RUN}/worktree" ]; then
    printf '    FAIL: review command did not record a worktree\n'
    RESULT=1
  else
    WT="$(cat "${SOURCE_RUN}/worktree")"
    WT_RUN="${WT}/.factory/runs/review-command"
    assert_file_equals "${WT}/.factory/active-run" "review-command" || RESULT=1
    assert_file_equals "${WT_RUN}/mode" "review" || RESULT=1
    assert_file_equals "${WT_RUN}/status" "complete" || RESULT=1
    assert_file_contains "${WT_RUN}/reviews/review-tests.md" "Verdict: pass" || RESULT=1
    assert_file_contains "${WT_RUN}/review-state.json" '"state": "passed"' || RESULT=1
    assert_file_contains "${WT_RUN}/reviews/codex-args" "exec" || RESULT=1
    assert_file_contains "${WT_RUN}/reviews/codex-args" "full-codebase test review" || RESULT=1
  fi

  cleanup_test_project
  return $RESULT
}

printf 'test-core-work-model-review-command\n\n'

run_test "review command prepares and runs review state" test_review_command_prepares_and_runs_review_run

printf '\n  %d passed, %d failed\n' "$PASS" "$FAIL"

if [ "$FAIL" -gt 0 ]; then
  printf '\n  Failures:%b\n' "$ERRORS"
  exit 1
fi
