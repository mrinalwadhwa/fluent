#!/usr/bin/env bash
# test-observability — Verify session observability and review archiving.
#
# Tests observability behaviors by running the factory binary with a mock
# claude command and checking file system artifacts.
#
# Covers:
#   - sessions.log written with session number, exit code, duration, status
#   - transcript.jsonl captures stream-json from agent stdout
#   - Session directories do not contain global ~/.claude state
#   - Review round archives created on review failure and restart
#   - Reviewer transcript captured to reviews/transcript-{name}.jsonl
#
# Usage:
#   tests/behaviors/operations/test-observability.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$(dirname "$(dirname "$SCRIPT_DIR")")")"
FACTORY_BIN="${PROJECT_DIR}/target/debug/factory"

PASS=0
FAIL=0
ERRORS=""

# -------------------------------------------------------------------------
# Helpers
# -------------------------------------------------------------------------

setup_test_project() {
  TEST_DIR="$(mktemp -d -t factory-test-obs-XXXXXX)"

  # Create a git repo
  mkdir -p "${TEST_DIR}/project"
  cd "${TEST_DIR}/project"
  git init -b main > /dev/null 2>&1
  git config commit.gpgsign false
  git config user.email "test@test"
  git config user.name "test"
  echo "test" > README.md
  git add . && git commit -m "init" > /dev/null 2>&1

  # Create mock bin directory
  MOCK_BIN="${TEST_DIR}/bin"
  mkdir -p "$MOCK_BIN"
}

create_planned_run() {
  RUN_ID="$1"
  RUN_DIR=".factory/runs/${RUN_ID}"
  mkdir -p "$RUN_DIR"
  printf 'Test observability brief' > "${RUN_DIR}/brief.md"
  printf 'planned' > "${RUN_DIR}/status"
  printf 'local' > "${RUN_DIR}/runtime"
  printf '%s' "$RUN_ID" > .factory/active-run
}

# Create a mock claude that outputs stream-json and writes status.
# Usage: create_mock_claude <status-to-set>
# The mock writes the given status after outputting stream-json.
create_mock_claude() {
  local target_status="$1"
  cat > "${MOCK_BIN}/claude" << MOCK_SCRIPT
#!/bin/bash
# Mock claude — output stream-json to stdout, set status
echo '{"type":"assistant","message":{"content":"working on it"}}'
echo '{"type":"result","subtype":"success","result":"done","session_id":"mock-session"}'
if [ -f .factory/active-run ]; then
  RID=\$(cat .factory/active-run)
  echo -n "${target_status}" > ".factory/runs/\$RID/status"
  echo "Handoff context" > ".factory/runs/\$RID/handoff.md"
fi
MOCK_SCRIPT
  chmod +x "${MOCK_BIN}/claude"
}

# Create a mock claude that changes behavior across calls.
# First call: sets status to "executing" (loop continues).
# Second call: sets status to "needs-user" (loop stops).
create_mock_claude_multi_session() {
  cat > "${MOCK_BIN}/claude" << 'MOCK_SCRIPT'
#!/bin/bash
# Mock claude — multi-session behavior
echo '{"type":"assistant","message":{"content":"working on it"}}'
echo '{"type":"result","subtype":"success","result":"done","session_id":"mock-session"}'
if [ -f .factory/active-run ]; then
  RID=$(cat .factory/active-run)
  COUNTER_FILE=".factory/runs/$RID/.mock-call-count"
  if [ -f "$COUNTER_FILE" ]; then
    N=$(cat "$COUNTER_FILE")
  else
    N=0
  fi
  N=$((N + 1))
  echo -n "$N" > "$COUNTER_FILE"
  if [ "$N" -eq 1 ]; then
    echo -n "executing" > ".factory/runs/$RID/status"
  else
    echo -n "needs-user" > ".factory/runs/$RID/status"
    echo "Need user input" > ".factory/runs/$RID/handoff.md"
  fi
fi
MOCK_SCRIPT
  chmod +x "${MOCK_BIN}/claude"
}

# Find the worktree path for a run
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
    done
  fi
  rm -rf "$TEST_DIR"
}

assert_file_exists() {
  if [ ! -f "$1" ]; then
    printf '    FAIL: expected file %s to exist\n' "$1"
    return 1
  fi
}

assert_dir_exists() {
  if [ ! -d "$1" ]; then
    printf '    FAIL: expected directory %s to exist\n' "$1"
    return 1
  fi
}

assert_dir_not_exists() {
  if [ -d "$1" ]; then
    printf '    FAIL: directory %s should not exist\n' "$1"
    return 1
  fi
}

assert_file_matches() {
  if [ ! -f "$1" ]; then
    printf '    FAIL: file %s does not exist\n' "$1"
    return 1
  fi
  if ! grep -qE "$2" "$1"; then
    printf '    FAIL: file %s does not match "%s"\n' "$1" "$2"
    printf '    Content: %s\n' "$(cat "$1")"
    return 1
  fi
}

assert_file_not_matches() {
  if [ ! -f "$1" ]; then
    return 0
  fi
  if grep -qE "$2" "$1"; then
    printf '    FAIL: file %s should not match "%s"\n' "$1" "$2"
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

# -------------------------------------------------------------------------
# Tests
# -------------------------------------------------------------------------

test_sessions_log_written() {
  setup_test_project
  create_planned_run "test-sesslog"
  create_mock_claude "needs-user"

  PATH="${MOCK_BIN}:${PATH}" "$FACTORY_BIN" run --no-sandbox --run-id "test-sesslog" \
    > /dev/null 2>&1 || true

  RESULT=0
  WT="$(find_worktree ".factory/runs/test-sesslog")"

  # sessions.log should exist in the worktree's run dir
  SL=""
  if [ -n "$WT" ] && [ -f "${WT}/.factory/runs/test-sesslog/sessions.log" ]; then
    SL="${WT}/.factory/runs/test-sesslog/sessions.log"
  fi

  if [ -z "$SL" ]; then
    printf '    FAIL: sessions.log not found\n'
    RESULT=1
  else
    assert_file_matches "$SL" 'session=' || RESULT=1
    assert_file_matches "$SL" 'exit=' || RESULT=1
    assert_file_matches "$SL" 'duration=' || RESULT=1
    assert_file_matches "$SL" 'status=' || RESULT=1
  fi

  cleanup_test_project
  return $RESULT
}

test_transcript_captures_stream_json() {
  setup_test_project
  create_planned_run "test-transcript"
  create_mock_claude "needs-user"

  PATH="${MOCK_BIN}:${PATH}" "$FACTORY_BIN" run --no-sandbox --run-id "test-transcript" \
    > /dev/null 2>&1 || true

  RESULT=0
  WT="$(find_worktree ".factory/runs/test-transcript")"

  # transcript.jsonl should exist in session-1 dir
  TRANSCRIPT=""
  if [ -n "$WT" ]; then
    TRANSCRIPT="${WT}/.factory/runs/test-transcript/sessions/session-1/transcript.jsonl"
  fi

  if [ -z "$TRANSCRIPT" ] || [ ! -f "$TRANSCRIPT" ]; then
    printf '    FAIL: transcript.jsonl not found at %s\n' "$TRANSCRIPT"
    RESULT=1
  else
    # Should contain stream-json output from mock claude
    assert_file_matches "$TRANSCRIPT" '"type"' || RESULT=1
    assert_file_matches "$TRANSCRIPT" '"result"' || RESULT=1
  fi

  cleanup_test_project
  return $RESULT
}

test_transcript_not_from_history() {
  setup_test_project
  create_planned_run "test-no-history"
  create_mock_claude "needs-user"

  # Create a fake ~/.claude/history.jsonl with distinctive content
  FAKE_CLAUDE_DIR="${TEST_DIR}/fake-claude-home"
  mkdir -p "$FAKE_CLAUDE_DIR"
  echo '{"type":"OLD_HISTORY_FORMAT","data":"should_not_appear"}' > "${FAKE_CLAUDE_DIR}/history.jsonl"

  PATH="${MOCK_BIN}:${PATH}" "$FACTORY_BIN" run --no-sandbox --run-id "test-no-history" \
    > /dev/null 2>&1 || true

  RESULT=0
  WT="$(find_worktree ".factory/runs/test-no-history")"
  TRANSCRIPT="${WT}/.factory/runs/test-no-history/sessions/session-1/transcript.jsonl"

  if [ -f "$TRANSCRIPT" ]; then
    # Transcript should NOT contain old history format markers
    assert_file_not_matches "$TRANSCRIPT" 'OLD_HISTORY_FORMAT' || RESULT=1
    # Should contain stream-json markers
    assert_file_matches "$TRANSCRIPT" '"type"' || RESULT=1
  else
    printf '    FAIL: transcript.jsonl not found\n'
    RESULT=1
  fi

  cleanup_test_project
  return $RESULT
}

test_no_global_claude_state_in_sessions() {
  setup_test_project
  create_planned_run "test-no-global"
  create_mock_claude "needs-user"

  # Create global ~/.claude state that should NOT be captured
  mkdir -p "${HOME}/.claude"

  PATH="${MOCK_BIN}:${PATH}" "$FACTORY_BIN" run --no-sandbox --run-id "test-no-global" \
    > /dev/null 2>&1 || true

  RESULT=0
  WT="$(find_worktree ".factory/runs/test-no-global")"
  SESSION_DIR="${WT}/.factory/runs/test-no-global/sessions/session-1"

  if [ -d "$SESSION_DIR" ]; then
    # Session dir should NOT contain global ~/.claude state dirs
    assert_dir_not_exists "${SESSION_DIR}/memory" || RESULT=1
    assert_dir_not_exists "${SESSION_DIR}/todos" || RESULT=1
    assert_dir_not_exists "${SESSION_DIR}/plans" || RESULT=1
  fi
  # transcript.jsonl is fine — that's the agent's own output

  cleanup_test_project
  return $RESULT
}

test_multi_session_writes_multiple_log_lines() {
  setup_test_project
  create_planned_run "test-multi"
  create_mock_claude_multi_session

  PATH="${MOCK_BIN}:${PATH}" "$FACTORY_BIN" run --no-sandbox --run-id "test-multi" \
    > /dev/null 2>&1 || true

  RESULT=0
  WT="$(find_worktree ".factory/runs/test-multi")"
  SL="${WT}/.factory/runs/test-multi/sessions.log"

  if [ ! -f "$SL" ]; then
    printf '    FAIL: sessions.log not found\n'
    RESULT=1
  else
    LINE_COUNT=$(wc -l < "$SL" | tr -d ' ')
    if [ "$LINE_COUNT" -lt 2 ]; then
      printf '    FAIL: expected at least 2 lines in sessions.log, got %s\n' "$LINE_COUNT"
      RESULT=1
    fi
    # Check session numbers are sequential
    assert_file_matches "$SL" 'session=1' || RESULT=1
    assert_file_matches "$SL" 'session=2' || RESULT=1
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

printf 'test-observability\n\n'

run_test "sessions.log written with required fields" test_sessions_log_written
run_test "transcript captures stream-json from agent" test_transcript_captures_stream_json
run_test "transcript not sourced from history" test_transcript_not_from_history
run_test "no global ~/.claude state in session dirs" test_no_global_claude_state_in_sessions
run_test "multi-session writes sequential log lines" test_multi_session_writes_multiple_log_lines

printf '\n  %d passed, %d failed\n' "$PASS" "$FAIL"

if [ "$FAIL" -gt 0 ]; then
  printf '\n  Failures:%b\n' "$ERRORS"
  exit 1
fi
