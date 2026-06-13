#!/usr/bin/env bash
# test-resume-resolve — Verify resume run-id resolution behaviors.
#
# Tests that `factory resume` finds runs with status `needs-user` or
# `failed`, ignores runs with other statuses, and launches interactive
# resume when stdin is a terminal.
#
# Covers:
#   - Resume finds a needs-user run
#   - Resume finds a failed run
#   - Resume skips complete and executing runs
#   - Resume finds either resumable status when both exist
#   - Terminal resume launches an interactive agent for an implicit run
#   - Terminal resume launches an interactive agent for an explicit run
#
# Usage:
#   tests/behaviors/operations/test-resume-resolve.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$(dirname "$(dirname "$SCRIPT_DIR")")")"
FACTORY="${FACTORY_BIN_OVERRIDE:-${PROJECT_DIR}/target/debug/factory}"

source "${PROJECT_DIR}/tests/lib/run_test.sh"
LOG_DIR="${PROJECT_DIR}/tests/output/$(basename "$0" .sh)"

# -------------------------------------------------------------------------
# Helpers
# -------------------------------------------------------------------------

setup_test_project() {
  TEST_DIR="$(mktemp -d -t factory-test-resume-XXXXXX)"
  mkdir -p "${TEST_DIR}/main"
  cd "${TEST_DIR}/main"
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
  rm -rf "$TEST_DIR"
}

write_mock_resume_agents() {
  cat > "${MOCK_BIN}/claude" << 'MOCK_CLAUDE'
#!/usr/bin/env bash
RUN_ID="${EXPECTED_RUN_ID:?}"
RUN_DIR=".factory/runs/${RUN_ID}"
printf '%s\n' "$@" > "${RUN_DIR}/interactive-agent-args"
printf 'called' > "${RUN_DIR}/interactive-agent-called"
MOCK_CLAUDE
  chmod +x "${MOCK_BIN}/claude"

  cat > "${MOCK_BIN}/sandbox-exec" << 'MOCK_SANDBOX'
#!/usr/bin/env bash
if [ "$1" = "-f" ]; then
  shift 2
fi
exec "$@"
MOCK_SANDBOX
  chmod +x "${MOCK_BIN}/sandbox-exec"
}

run_factory_with_terminal() {
  script -q /dev/null env \
    PATH="${MOCK_BIN}:${PATH}" \
    EXPECTED_RUN_ID="$EXPECTED_RUN_ID" \
    "$FACTORY" --no-sandbox "$@"
}

# -------------------------------------------------------------------------
# Tests
# -------------------------------------------------------------------------

test_resume_finds_needs_user() {
  setup_test_project

  mkdir -p ".factory/runs/run-paused"
  printf 'needs-user' > ".factory/runs/run-paused/status"
  printf 'Paused run' > ".factory/runs/run-paused/brief.md"

  # Run factory resume and capture just the first line of output
  OUTPUT="$("$FACTORY" resume 2>&1 | head -1 || true)"

  RESULT=0
  if ! echo "$OUTPUT" | grep -q "run-paused"; then
    printf '    FAIL: resume did not find run-paused, got: %s\n' "$OUTPUT"
    RESULT=1
  fi

  cleanup_test_project
  return $RESULT
}

test_resume_finds_failed() {
  setup_test_project

  mkdir -p ".factory/runs/run-broken"
  printf 'failed' > ".factory/runs/run-broken/status"
  printf 'Broken run' > ".factory/runs/run-broken/brief.md"

  OUTPUT="$("$FACTORY" resume 2>&1 | head -1 || true)"

  RESULT=0
  if ! echo "$OUTPUT" | grep -q "run-broken"; then
    printf '    FAIL: resume did not find run-broken, got: %s\n' "$OUTPUT"
    RESULT=1
  fi

  cleanup_test_project
  return $RESULT
}

test_resume_skips_complete_and_executing() {
  setup_test_project

  mkdir -p ".factory/runs/run-done" ".factory/runs/run-active"
  printf 'complete' > ".factory/runs/run-done/status"
  printf 'Done' > ".factory/runs/run-done/brief.md"
  printf 'executing' > ".factory/runs/run-active/status"
  printf 'Active' > ".factory/runs/run-active/brief.md"

  # With only complete and executing runs, resume should not find a target
  OUTPUT="$("$FACTORY" resume 2>&1 | head -3 || true)"

  RESULT=0
  # Should not say "Resuming run run-done" or "Resuming run run-active"
  if echo "$OUTPUT" | grep -q "Resuming run run-done"; then
    printf '    FAIL: resume should not target complete run\n'
    RESULT=1
  fi
  if echo "$OUTPUT" | grep -q "Resuming run run-active"; then
    printf '    FAIL: resume should not target executing run\n'
    RESULT=1
  fi

  cleanup_test_project
  return $RESULT
}

test_resume_finds_either_resumable_status() {
  setup_test_project

  mkdir -p ".factory/runs/run-failed" ".factory/runs/run-paused"
  printf 'failed' > ".factory/runs/run-failed/status"
  printf 'Failed' > ".factory/runs/run-failed/brief.md"
  printf 'needs-user' > ".factory/runs/run-paused/status"
  printf 'Paused' > ".factory/runs/run-paused/brief.md"

  OUTPUT="$("$FACTORY" resume 2>&1 | head -1 || true)"

  RESULT=0
  if ! echo "$OUTPUT" | grep -qE "run-failed|run-paused"; then
    printf '    FAIL: resume should find a resumable run, got: %s\n' "$OUTPUT"
    RESULT=1
  fi

  cleanup_test_project
  return $RESULT
}

test_terminal_resume_launches_interactive_agent_for_implicit_run() {
  setup_test_project
  write_mock_resume_agents

  mkdir -p ".factory/runs/run-paused"
  printf 'needs-user' > ".factory/runs/run-paused/status"
  printf 'Paused run' > ".factory/runs/run-paused/brief.md"

  EXPECTED_RUN_ID="run-paused" run_factory_with_terminal resume \
    > "${TEST_DIR}/resume-terminal.out" 2>&1

  RESULT=0
  if [ ! -f ".factory/runs/run-paused/interactive-agent-called" ]; then
    printf '    FAIL: terminal resume did not launch interactive agent\n'
    RESULT=1
  fi
  if grep -Fxq -- "-p" ".factory/runs/run-paused/interactive-agent-args" 2>/dev/null; then
    printf '    FAIL: terminal resume used non-interactive prompt mode\n'
    RESULT=1
  fi

  cleanup_test_project
  return $RESULT
}

test_terminal_resume_launches_interactive_agent_for_explicit_run() {
  setup_test_project
  write_mock_resume_agents

  mkdir -p ".factory/runs/run-explicit" ".factory/runs/run-other"
  printf 'needs-user' > ".factory/runs/run-explicit/status"
  printf 'Explicit run' > ".factory/runs/run-explicit/brief.md"
  printf 'failed' > ".factory/runs/run-other/status"
  printf 'Other run' > ".factory/runs/run-other/brief.md"

  EXPECTED_RUN_ID="run-explicit" run_factory_with_terminal resume run-explicit \
    > "${TEST_DIR}/resume-explicit-terminal.out" 2>&1

  RESULT=0
  if [ ! -f ".factory/runs/run-explicit/interactive-agent-called" ]; then
    printf '    FAIL: explicit terminal resume did not launch interactive agent\n'
    RESULT=1
  fi
  if [ -f ".factory/runs/run-other/interactive-agent-called" ]; then
    printf '    FAIL: explicit terminal resume touched another run\n'
    RESULT=1
  fi
  if grep -Fxq -- "-p" ".factory/runs/run-explicit/interactive-agent-args" 2>/dev/null; then
    printf '    FAIL: explicit terminal resume used non-interactive prompt mode\n'
    RESULT=1
  fi

  cleanup_test_project
  return $RESULT
}

# -------------------------------------------------------------------------
# Run all tests
# -------------------------------------------------------------------------

printf 'test-resume-resolve\n\n'

run_test "resume finds needs-user run" test_resume_finds_needs_user
run_test "resume finds failed run" test_resume_finds_failed
run_test "resume skips complete and executing runs" test_resume_skips_complete_and_executing
run_test "resume finds either resumable status" test_resume_finds_either_resumable_status
run_test "terminal resume launches interactive agent for implicit run" \
  test_terminal_resume_launches_interactive_agent_for_implicit_run
run_test "terminal resume launches interactive agent for explicit run" \
  test_terminal_resume_launches_interactive_agent_for_explicit_run

summarize_and_exit
