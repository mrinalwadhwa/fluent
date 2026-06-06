#!/usr/bin/env bash
# test-codex-approval-flag — Verify approval-policy flag placement.
#
# The Codex CLI accepts --ask-for-approval as a top-level option, not as
# an option after the exec subcommand. This test verifies that factory
# places --ask-for-approval BEFORE exec in the command line, so the
# installed Codex CLI accepts it.
#
# Usage:
#   tests/behaviors/operations/test-codex-approval-flag.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$(dirname "$(dirname "$SCRIPT_DIR")")")"
FACTORY_BIN="${PROJECT_DIR}/target/debug/factory"

PASS=0
FAIL=0
ERRORS=""

setup_test_project() {
  TEST_DIR="$(mktemp -d -t factory-test-approval-XXXXXX)"
  mkdir -p "${TEST_DIR}/project"
  cd "${TEST_DIR}/project"
  git init -b main > /dev/null 2>&1
  git config commit.gpgsign false
  git config user.email "test@test"
  git config user.name "test"
  echo "test" > README.md
  git add . && git commit -m "init" > /dev/null 2>&1

  MOCK_BIN="${TEST_DIR}/bin"
  mkdir -p "$MOCK_BIN"
}

create_planned_run() {
  RUN_ID="$1"
  RUN_DIR=".factory/runs/${RUN_ID}"
  mkdir -p "$RUN_DIR"
  printf 'Test approval flag brief' > "${RUN_DIR}/brief.md"
  printf 'planned' > "${RUN_DIR}/status"
  printf 'local' > "${RUN_DIR}/runtime"
  printf '%s' "$RUN_ID" > .factory/active-run
}

find_worktree() {
  local run_dir="$1"
  if [ -f "${run_dir}/worktree" ]; then
    cat "${run_dir}/worktree"
  else
    echo ""
  fi
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

write_mock_codex() {
  cat > "${MOCK_BIN}/codex" << 'MOCK_SCRIPT'
#!/usr/bin/env bash
printf '%s\n' "$*" > .codex-args
echo '{"type":"result","subtype":"success","result":"done","session_id":"mock"}'
if [ -f .factory/active-run ]; then
  RID="$(cat .factory/active-run)"
  printf 'needs-user' > ".factory/runs/${RID}/status"
fi
MOCK_SCRIPT
  chmod +x "${MOCK_BIN}/codex"
}

write_mock_only_path_tools() {
  TOOL_BIN="${TEST_DIR}/tools"
  mkdir -p "$TOOL_BIN"
  for tool in bash cat git which; do
    tool_path="$(command -v "$tool")"
    ln -s "$tool_path" "${TOOL_BIN}/${tool}"
  done
  MOCK_ONLY_PATH="${MOCK_BIN}:${TOOL_BIN}"
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

# -------------------------------------------------------------------------
# Test: approval-policy flag appears before exec subcommand
# -------------------------------------------------------------------------
# The Codex CLI rejects --ask-for-approval when placed after exec.
# Verify that factory places it as a top-level option (before exec).

test_approval_flag_before_exec() {
  setup_test_project
  create_planned_run "test-approval-order"
  write_mock_codex
  write_mock_only_path_tools

  PATH="$MOCK_ONLY_PATH" "$FACTORY_BIN" run --coder codex \
    --run-id "test-approval-order" > "${TEST_DIR}/factory.out" 2>&1 || true

  RESULT=0
  WT="$(find_worktree ".factory/runs/test-approval-order")"
  if [ -z "$WT" ] || [ ! -f "${WT}/.codex-args" ]; then
    printf '    FAIL: mock codex did not run\n'
    RESULT=1
  else
    ARGS="$(cat "${WT}/.codex-args")"

    # --ask-for-approval must be present
    if ! printf '%s' "$ARGS" | grep -q -- '--ask-for-approval never'; then
      printf '    FAIL: --ask-for-approval never not found in args\n'
      RESULT=1
    fi

    # exec must be present
    if ! printf '%s' "$ARGS" | grep -q 'exec'; then
      printf '    FAIL: exec not found in args\n'
      RESULT=1
    fi

    # --ask-for-approval must appear BEFORE exec
    # Extract the position of each in the args string
    APPROVAL_POS="$(printf '%s' "$ARGS" | grep -bo -- '--ask-for-approval' | head -1 | cut -d: -f1)"
    EXEC_POS="$(printf '%s' "$ARGS" | grep -bo 'exec' | head -1 | cut -d: -f1)"

    if [ -n "$APPROVAL_POS" ] && [ -n "$EXEC_POS" ]; then
      if [ "$APPROVAL_POS" -ge "$EXEC_POS" ]; then
        printf '    FAIL: --ask-for-approval (pos %s) appears after exec (pos %s)\n' "$APPROVAL_POS" "$EXEC_POS"
        printf '    Args: %s\n' "$ARGS"
        RESULT=1
      fi
    else
      printf '    FAIL: could not determine positions of flags\n'
      printf '    Args: %s\n' "$ARGS"
      RESULT=1
    fi
  fi

  cleanup_test_project
  return $RESULT
}

# -------------------------------------------------------------------------
# Run all tests
# -------------------------------------------------------------------------

if [ ! -x "$FACTORY_BIN" ]; then
  printf 'ERROR: factory binary not found at %s\n' "$FACTORY_BIN"
  printf 'Run "cargo build" first.\n'
  exit 1
fi

printf 'test-codex-approval-flag\n\n'

run_test "approval-policy flag appears before exec" test_approval_flag_before_exec

printf '\n  %d passed, %d failed\n' "$PASS" "$FAIL"

if [ "$FAIL" -gt 0 ]; then
  printf '\n  Failures:%b\n' "$ERRORS"
  exit 1
fi
