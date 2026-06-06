#!/usr/bin/env bash
# test-headless-resume - Verify non-interactive resume behavior.
#
# Covers:
#   - Explicit headless resume restarts the selected run's session loop
#   - Implicit headless resume restarts a resumable run's session loop
#   - Headless resume does not fail with "stdin is not a terminal"
#   - Explicit resume leaves other resumable runs untouched
#   - Headless resume rejects parallel parent runs
#
# Usage:
#   tests/behaviors/operations/test-headless-resume.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$(dirname "$(dirname "$SCRIPT_DIR")")")"
FACTORY_BIN="${PROJECT_DIR}/target/debug/factory"

PASS=0
FAIL=0
ERRORS=""

setup_test_project() {
  TEST_DIR="$(mktemp -d -t factory-test-headless-resume-XXXXXX)"
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

create_planned_run() {
  RUN_ID="$1"
  RUN_DIR=".factory/runs/${RUN_ID}"
  mkdir -p "$RUN_DIR"
  printf 'Headless resume brief' > "${RUN_DIR}/brief.md"
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

write_mock_agents() {
  cat > "${MOCK_BIN}/mock-agent" << 'MOCK_SCRIPT'
#!/usr/bin/env bash
RID="$(cat .factory/active-run)"
RUN_DIR=".factory/runs/${RID}"
COUNT_FILE="${RUN_DIR}/agent-count"
COUNT=0
if [ -f "$COUNT_FILE" ]; then
  COUNT="$(cat "$COUNT_FILE")"
fi
COUNT=$((COUNT + 1))
printf '%s' "$COUNT" > "$COUNT_FILE"
printf '%s\n' "$*" > "${RUN_DIR}/agent-args-${COUNT}"
echo '{"type":"result","subtype":"success","result":"done","session_id":"mock"}'
if [ "$COUNT" -eq 1 ]; then
  printf 'failed' > "${RUN_DIR}/status"
else
  printf 'needs-user' > "${RUN_DIR}/status"
fi
MOCK_SCRIPT
  chmod +x "${MOCK_BIN}/mock-agent"
  ln -s "${MOCK_BIN}/mock-agent" "${MOCK_BIN}/codex"
  ln -s "${MOCK_BIN}/mock-agent" "${MOCK_BIN}/claude"
  cat > "${MOCK_BIN}/sandbox-exec" << 'MOCK_SANDBOX'
#!/usr/bin/env bash
if [ "$1" = "-f" ]; then
  shift 2
fi
exec "$@"
MOCK_SANDBOX
  chmod +x "${MOCK_BIN}/sandbox-exec"
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

test_explicit_headless_resume_restarts_selected_run_loop() {
  setup_test_project
  write_mock_agents
  create_planned_run "run-selected"

  PATH="${MOCK_BIN}:${PATH}" "$FACTORY_BIN" run --coder codex --no-sandbox \
    --run-id "run-selected" > "${TEST_DIR}/initial.out" 2>&1 || true

  SELECTED_WT="$(find_worktree ".factory/runs/run-selected")"
  if [ -z "$SELECTED_WT" ]; then
    printf '    FAIL: initial run did not create selected worktree\n'
    cleanup_test_project
    return 1
  fi

  mkdir -p ".factory/runs/run-other"
  printf 'Other brief' > ".factory/runs/run-other/brief.md"
  printf 'failed' > ".factory/runs/run-other/status"
  printf 'local' > ".factory/runs/run-other/runtime"

  PATH="${MOCK_BIN}:${PATH}" "$FACTORY_BIN" resume "run-selected" \
    < /dev/null > "${TEST_DIR}/resume.out" 2>&1

  RESULT=0
  RESUME_OUTPUT="$(cat "${TEST_DIR}/resume.out")"
  if echo "$RESUME_OUTPUT" | grep -q "stdin is not a terminal"; then
    printf '    FAIL: headless resume reported stdin is not a terminal\n'
    RESULT=1
  fi
  if [ "$(cat "${SELECTED_WT}/.factory/runs/run-selected/agent-count" 2>/dev/null || echo 0)" != "2" ]; then
    printf '    FAIL: selected run session loop did not restart\n'
    RESULT=1
  fi
  if [ -f ".factory/runs/run-other/agent-count" ]; then
    printf '    FAIL: explicit resume touched another resumable run\n'
    RESULT=1
  fi

  cleanup_test_project
  return $RESULT
}

test_implicit_headless_resume_restarts_resumable_run_loop() {
  setup_test_project
  write_mock_agents
  mkdir -p ".factory/runs/run-implicit"
  printf 'Implicit brief' > ".factory/runs/run-implicit/brief.md"
  printf 'failed' > ".factory/runs/run-implicit/status"
  printf 'local' > ".factory/runs/run-implicit/runtime"
  printf 'run-implicit' > ".factory/active-run"

  PATH="${MOCK_BIN}:${PATH}" "$FACTORY_BIN" resume \
    < /dev/null > "${TEST_DIR}/resume-implicit.out" 2>&1

  RESULT=0
  RESUME_OUTPUT="$(cat "${TEST_DIR}/resume-implicit.out")"
  if echo "$RESUME_OUTPUT" | grep -q "stdin is not a terminal"; then
    printf '    FAIL: implicit headless resume reported stdin is not a terminal\n'
    RESULT=1
  fi
  if ! echo "$RESUME_OUTPUT" | grep -q "Resuming run run-implicit"; then
    printf '    FAIL: implicit headless resume did not select the resumable run\n'
    RESULT=1
  fi
  AGENT_COUNT="$(cat ".factory/runs/run-implicit/agent-count" 2>/dev/null || echo 0)"
  if [ "$AGENT_COUNT" -lt 1 ]; then
    printf '    FAIL: implicit run session loop did not restart\n'
    RESULT=1
  fi

  cleanup_test_project
  return $RESULT
}

test_headless_resume_rejects_parallel_parent() {
  setup_test_project
  write_mock_agents
  mkdir -p ".factory/runs/parallel-parent"
  printf 'Parent brief' > ".factory/runs/parallel-parent/brief.md"
  printf 'failed' > ".factory/runs/parallel-parent/status"
  printf 'local' > ".factory/runs/parallel-parent/runtime"
  printf 'parallel-parent-1-1\n' > ".factory/runs/parallel-parent/children"

  if PATH="${MOCK_BIN}:${PATH}" "$FACTORY_BIN" resume "parallel-parent" \
    < /dev/null > "${TEST_DIR}/resume-parent.out" 2>&1; then
    printf '    FAIL: headless resume should reject a parallel parent run\n'
    cleanup_test_project
    return 1
  fi

  RESULT=0
  RESUME_OUTPUT="$(cat "${TEST_DIR}/resume-parent.out")"
  if ! echo "$RESUME_OUTPUT" | grep -q "Cannot headlessly resume parallel parent run parallel-parent"; then
    printf '    FAIL: headless resume did not explain parallel parent rejection\n'
    RESULT=1
  fi
  if [ -f ".factory/runs/parallel-parent/agent-count" ]; then
    printf '    FAIL: headless resume launched an agent for the parallel parent\n'
    RESULT=1
  fi

  cleanup_test_project
  return $RESULT
}

printf 'test-headless-resume\n\n'

run_test "explicit headless resume restarts selected run loop" \
  test_explicit_headless_resume_restarts_selected_run_loop

run_test "implicit headless resume restarts resumable run loop" \
  test_implicit_headless_resume_restarts_resumable_run_loop

run_test "headless resume rejects parallel parent runs" \
  test_headless_resume_rejects_parallel_parent

printf '\n  %d passed, %d failed\n' "$PASS" "$FAIL"

if [ "$FAIL" -gt 0 ]; then
  printf '\n  Failures:%b\n' "$ERRORS"
  exit 1
fi
