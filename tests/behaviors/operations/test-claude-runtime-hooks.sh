#!/usr/bin/env bash
# test-claude-runtime-hooks — Verify Claude local runtime hook behavior.
#
# Exercises the factory CLI through a temporary git project. Mocks claude
# and sandbox-exec on PATH to observe public command launches without
# reading implementation internals.
#
# Usage:
#   tests/behaviors/operations/test-claude-runtime-hooks.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$(dirname "$(dirname "$SCRIPT_DIR")")")"
FACTORY_BIN="${FACTORY_BIN_OVERRIDE:-${PROJECT_DIR}/target/debug/factory}"

source "${PROJECT_DIR}/tests/lib/run_test.sh"
LOG_DIR="${PROJECT_DIR}/tests/output/$(basename "$0" .sh)"

setup_test_project() {
  TEST_DIR="$(mktemp -d -t factory-test-claude-hooks-XXXXXX)"
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
  printf 'Test Claude hook brief' > "${RUN_DIR}/brief.md"
  printf 'planned' > "${RUN_DIR}/status"
  printf 'local' > "${RUN_DIR}/runtime"
  printf '%s' "$RUN_ID" > .factory/active-run
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

write_mock_claude() {
  cat > "${MOCK_BIN}/claude" << 'MOCK_SCRIPT'
#!/usr/bin/env bash
{
  printf '%s\n' '---'
  printf 'cwd=%s\n' "$PWD"
  printf 'args=%s\n' "$*"
} >> "$CLAUDE_CALL_LOG"
echo '{"type":"result","subtype":"success","result":"done","session_id":"mock"}'
if [ -f .factory/active-run ]; then
  RID="$(cat .factory/active-run)"
  printf 'needs-user' > ".factory/runs/${RID}/status"
fi
MOCK_SCRIPT
  chmod +x "${MOCK_BIN}/claude"
}

write_mock_sandbox_exec() {
  cat > "${MOCK_BIN}/sandbox-exec" << 'MOCK_SCRIPT'
#!/usr/bin/env bash
printf 'sandbox-exec %s\n' "$*" >> "$SANDBOX_EXEC_LOG"
if [ "${1:-}" = "-f" ]; then
  shift 2
fi
exec "$@"
MOCK_SCRIPT
  chmod +x "${MOCK_BIN}/sandbox-exec"
}

assert_eq() {
  if [ "$1" != "$2" ]; then
    printf '    FAIL: got "%s", expected "%s"\n' "$1" "$2"
    return 1
  fi
}

test_sandboxed_claude_runs_refresh_hook() {
  setup_test_project
  create_planned_run "test-claude-refresh-hook"
  write_mock_claude
  write_mock_sandbox_exec

  CLAUDE_CALL_LOG="${TEST_DIR}/claude-calls.log"
  SANDBOX_EXEC_LOG="${TEST_DIR}/sandbox-exec.log"
  export CLAUDE_CALL_LOG
  export SANDBOX_EXEC_LOG

  PATH="${MOCK_BIN}:${PATH}" "$FACTORY_BIN" run \
    --run-id "test-claude-refresh-hook" > "${TEST_DIR}/factory.out" 2>&1

  RESULT=0
  CLAUDE_CALLS="$(grep -c '^---$' "$CLAUDE_CALL_LOG" 2>/dev/null || true)"
  SANDBOX_CALLS="$(grep -c '^sandbox-exec ' "$SANDBOX_EXEC_LOG" 2>/dev/null || true)"

  assert_eq "$CLAUDE_CALLS" "2" || RESULT=1
  assert_eq "$SANDBOX_CALLS" "1" || RESULT=1

  cleanup_test_project
  return $RESULT
}

if [ ! -x "$FACTORY_BIN" ]; then
  printf 'ERROR: factory binary not found at %s\n' "$FACTORY_BIN"
  printf 'Run "cargo build" first.\n'
  exit 1
fi

printf 'test-claude-runtime-hooks\n\n'

run_test "sandboxed claude runs refresh hook" test_sandboxed_claude_runs_refresh_hook

summarize_and_exit
