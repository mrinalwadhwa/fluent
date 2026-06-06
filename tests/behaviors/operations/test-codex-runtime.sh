#!/usr/bin/env bash
# test-codex-runtime — Verify Codex local and Fargate launch behaviors.
#
# Tests Codex runtime behavior through the factory CLI external
# interface. Mocks codex and, where needed, sandbox-exec on PATH to
# record whether factory launches the right command without reading
# implementation.
#
# Usage:
#   tests/behaviors/operations/test-codex-runtime.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$(dirname "$(dirname "$SCRIPT_DIR")")")"
FACTORY_BIN="${PROJECT_DIR}/target/debug/factory"

PASS=0
FAIL=0
ERRORS=""

setup_test_project() {
  TEST_DIR="$(mktemp -d -t factory-test-codex-XXXXXX)"
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
  printf 'Test Codex brief' > "${RUN_DIR}/brief.md"
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
echo "CODEX_LAUNCHED=1" > .codex-launched
echo '{"type":"result","subtype":"success","result":"done","session_id":"mock"}'
if [ -f .factory/active-run ]; then
  RID="$(cat .factory/active-run)"
  printf 'needs-user' > ".factory/runs/${RID}/status"
fi
MOCK_SCRIPT
  chmod +x "${MOCK_BIN}/codex"
}

write_mock_sandbox_exec() {
  cat > "${MOCK_BIN}/sandbox-exec" << 'MOCK_SCRIPT'
#!/usr/bin/env bash
echo "SANDBOX_EXEC_USED=1" > "${SANDBOX_EXEC_LOG}"
exit 97
MOCK_SCRIPT
  chmod +x "${MOCK_BIN}/sandbox-exec"
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

write_mock_claude_refresh_probe() {
  cat > "${MOCK_BIN}/claude" << 'MOCK_SCRIPT'
#!/usr/bin/env bash
if [ -n "${CLAUDE_REFRESH_PROBE_FILE:-}" ]; then
  printf 'CLAUDE_REFRESH_CALLED=1\n' > "$CLAUDE_REFRESH_PROBE_FILE" 2>/dev/null || true
fi
echo '{"type":"result","subtype":"success","result":"done","session_id":"mock"}'
if [ -f .factory/active-run ]; then
  RID="$(cat .factory/active-run)"
  printf 'needs-user' > ".factory/runs/${RID}/status"
fi
MOCK_SCRIPT
  chmod +x "${MOCK_BIN}/claude"
}

write_mock_codex_complete() {
  cat > "${MOCK_BIN}/codex" << 'MOCK_SCRIPT'
#!/usr/bin/env bash
if [ -n "${CODEX_ARGS_LOG:-}" ]; then
  {
    printf '%s\n' '---'
    printf '%s\n' "$*"
  } >> "$CODEX_ARGS_LOG"
fi
echo '{"type":"result","subtype":"success","result":"done","session_id":"mock"}'
if [ -f .factory/active-run ]; then
  RID="$(cat .factory/active-run)"
  printf '%s\n' "$*" > ".factory/runs/${RID}/codex-args"
  echo "CODEX_LAUNCHED=1" > ".factory/runs/${RID}/codex-launched"
  printf 'complete' > ".factory/runs/${RID}/status"
fi
MOCK_SCRIPT
  chmod +x "${MOCK_BIN}/codex"
}

assert_contains() {
  if ! grep -q -- "$2" <<< "$1"; then
    printf '    FAIL: output does not contain "%s"\n' "$2"
    return 1
  fi
}

assert_not_contains() {
  if grep -q -- "$2" <<< "$1"; then
    printf '    FAIL: output should not contain "%s"\n' "$2"
    return 1
  fi
}

assert_before() {
  local haystack="$1" first="$2" second="$3"
  local pos_first pos_second
  pos_first="$(grep -m1 -bo -- "$first" <<< "$haystack" | cut -d: -f1)"
  pos_second="$(grep -m1 -bo -- "$second" <<< "$haystack" | cut -d: -f1)"
  if [ -z "$pos_first" ] || [ -z "$pos_second" ]; then
    printf '    FAIL: could not locate "%s" or "%s"\n' "$first" "$second"
    return 1
  fi
  if [ "$pos_first" -ge "$pos_second" ]; then
    printf '    FAIL: "%s" (pos %s) must appear before "%s" (pos %s)\n' "$first" "$pos_first" "$second" "$pos_second"
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

test_sandboxed_codex_uses_workspace_write() {
  setup_test_project
  create_planned_run "test-codex-sandboxed"
  write_mock_codex
  write_mock_only_path_tools

  SANDBOX_EXEC_LOG="${TEST_DIR}/sandbox-exec.log"
  export SANDBOX_EXEC_LOG

  PATH="$MOCK_ONLY_PATH" "$FACTORY_BIN" run --coder codex \
    --run-id "test-codex-sandboxed" > "${TEST_DIR}/factory.out" 2>&1 || true

  RESULT=0
  WT="$(find_worktree ".factory/runs/test-codex-sandboxed")"
  if [ -z "$WT" ] || [ ! -f "${WT}/.codex-args" ]; then
    printf '    FAIL: mock codex did not run\n'
    RESULT=1
  else
    ARGS="$(cat "${WT}/.codex-args")"
    EXPECTED_ROOT="$(git -C "${TEST_DIR}/project" rev-parse --path-format=absolute --git-common-dir)"
    assert_contains "$ARGS" "exec" || RESULT=1
    assert_contains "$ARGS" "--sandbox workspace-write" || RESULT=1
    assert_contains "$ARGS" "--add-dir $EXPECTED_ROOT" || RESULT=1
    assert_contains "$ARGS" "--ask-for-approval never" || RESULT=1
    assert_before "$ARGS" "--ask-for-approval" "exec" || RESULT=1
    assert_not_contains "$ARGS" "--dangerously-bypass-approvals-and-sandbox" || RESULT=1
  fi

  if [ -f "$SANDBOX_EXEC_LOG" ]; then
    printf '    FAIL: sandbox-exec was used for sandboxed Codex\n'
    RESULT=1
  fi

  cleanup_test_project
  return $RESULT
}

test_no_sandbox_codex_bypasses_approvals_and_sandbox() {
  setup_test_project
  create_planned_run "test-codex-no-sandbox"
  write_mock_codex
  write_mock_sandbox_exec

  SANDBOX_EXEC_LOG="${TEST_DIR}/sandbox-exec.log"
  export SANDBOX_EXEC_LOG

  PATH="${MOCK_BIN}:${PATH}" "$FACTORY_BIN" run --coder codex --no-sandbox \
    --run-id "test-codex-no-sandbox" > "${TEST_DIR}/factory.out" 2>&1 || true

  RESULT=0
  WT="$(find_worktree ".factory/runs/test-codex-no-sandbox")"
  if [ -z "$WT" ] || [ ! -f "${WT}/.codex-args" ]; then
    printf '    FAIL: mock codex did not run\n'
    RESULT=1
  else
    ARGS="$(cat "${WT}/.codex-args")"
    assert_contains "$ARGS" "exec" || RESULT=1
    assert_contains "$ARGS" "--dangerously-bypass-approvals-and-sandbox" || RESULT=1
  fi

  if [ -f "$SANDBOX_EXEC_LOG" ]; then
    printf '    FAIL: sandbox-exec was used for --no-sandbox Codex\n'
    RESULT=1
  fi

  cleanup_test_project
  return $RESULT
}

test_codex_does_not_run_claude_refresh_hook() {
  setup_test_project
  create_planned_run "test-codex-no-claude-hook"
  write_mock_codex
  write_mock_claude_refresh_probe

  CLAUDE_REFRESH_PROBE_FILE="${TEST_DIR}/claude-refresh.log"
  export CLAUDE_REFRESH_PROBE_FILE

  PATH="${MOCK_BIN}:${PATH}" "$FACTORY_BIN" run --coder codex \
    --run-id "test-codex-no-claude-hook" > "${TEST_DIR}/factory.out" 2>&1 || true

  RESULT=0
  WT="$(find_worktree ".factory/runs/test-codex-no-claude-hook")"
  if [ -z "$WT" ] || [ ! -f "${WT}/.codex-args" ]; then
    printf '    FAIL: mock codex did not run\n'
    RESULT=1
  fi

  if [ -f "$CLAUDE_REFRESH_PROBE_FILE" ]; then
    printf '    FAIL: Claude refresh hook ran during Codex session\n'
    RESULT=1
  fi

  cleanup_test_project
  return $RESULT
}

test_parallel_codex_does_not_run_claude_refresh_hook() {
  setup_test_project
  create_planned_run "test-codex-parallel-hook"
  cat > .factory/runs/test-codex-parallel-hook/plan.md << 'PLAN'
## Group 1 (parallel)

### alpha
Run alpha.

### beta
Run beta.
PLAN
  write_mock_codex_complete
  write_mock_claude_refresh_probe
  write_mock_sandbox_exec

  CLAUDE_REFRESH_PROBE_FILE="${TEST_DIR}/claude-refresh.log"
  SANDBOX_EXEC_LOG="${TEST_DIR}/sandbox-exec.log"
  CODEX_ARGS_LOG="${TEST_DIR}/codex-args.log"
  export CLAUDE_REFRESH_PROBE_FILE
  export SANDBOX_EXEC_LOG
  export CODEX_ARGS_LOG

  PATH="${MOCK_BIN}:${PATH}" "$FACTORY_BIN" run --coder codex \
    --run-id "test-codex-parallel-hook" > "${TEST_DIR}/factory.out" 2>&1 || true

  RESULT=0
  CHILD_ARGS="$(cat "$CODEX_ARGS_LOG" 2>/dev/null || true)"
  if [ -z "$CHILD_ARGS" ]; then
    printf '    FAIL: mock codex did not run for parallel children\n'
    RESULT=1
  else
    EXPECTED_ROOT="$(git -C "${TEST_DIR}/project" rev-parse --path-format=absolute --git-common-dir)"
    assert_contains "$CHILD_ARGS" "exec" || RESULT=1
    assert_contains "$CHILD_ARGS" "--sandbox workspace-write" || RESULT=1
    assert_contains "$CHILD_ARGS" "--add-dir $EXPECTED_ROOT" || RESULT=1
    assert_contains "$CHILD_ARGS" "--ask-for-approval never" || RESULT=1
    assert_before "$CHILD_ARGS" "--ask-for-approval" "exec" || RESULT=1
    assert_not_contains "$CHILD_ARGS" "--dangerously-bypass-approvals-and-sandbox" || RESULT=1
  fi

  if [ -f "$CLAUDE_REFRESH_PROBE_FILE" ]; then
    printf '    FAIL: Claude refresh hook ran during parallel Codex session\n'
    RESULT=1
  fi

  if [ -f "$SANDBOX_EXEC_LOG" ]; then
    printf '    FAIL: sandbox-exec was used for parallel sandboxed Codex\n'
    RESULT=1
  fi

  cleanup_test_project
  return $RESULT
}

test_fargate_codex_fails_before_launch() {
  setup_test_project
  create_planned_run "test-codex-fargate"
  write_mock_codex

  set +e
  OUTPUT="$(PATH="${MOCK_BIN}:${PATH}" "$FACTORY_BIN" run --runtime fargate \
    --coder codex --run-id "test-codex-fargate" 2>&1)"
  STATUS=$?
  set -e

  RESULT=0
  if [ "$STATUS" -eq 0 ]; then
    printf '    FAIL: fargate Codex command succeeded\n'
    RESULT=1
  fi

  assert_contains "$OUTPUT" "Fargate" || RESULT=1
  assert_contains "$OUTPUT" "supports only the claude coder" || RESULT=1

  LAUNCHED_FILES="$(find "${TEST_DIR}/project" -name .codex-launched -print)"
  if [ -n "$LAUNCHED_FILES" ]; then
    printf '    FAIL: mock codex was launched for fargate runtime\n'
    RESULT=1
  fi

  cleanup_test_project
  return $RESULT
}

if [ ! -x "$FACTORY_BIN" ]; then
  printf 'ERROR: factory binary not found at %s\n' "$FACTORY_BIN"
  printf 'Run "cargo build" first.\n'
  exit 1
fi

printf 'test-codex-runtime\n\n'

run_test "sandboxed codex uses workspace-write" test_sandboxed_codex_uses_workspace_write
run_test "no-sandbox codex bypasses approvals and sandbox" test_no_sandbox_codex_bypasses_approvals_and_sandbox
run_test "codex does not run claude refresh hook" test_codex_does_not_run_claude_refresh_hook
run_test "parallel codex does not run claude refresh hook" test_parallel_codex_does_not_run_claude_refresh_hook
run_test "fargate codex fails before launch" test_fargate_codex_fails_before_launch

printf '\n  %d passed, %d failed\n' "$PASS" "$FAIL"

if [ "$FAIL" -gt 0 ]; then
  printf '\n  Failures:%b\n' "$ERRORS"
  exit 1
fi
