#!/usr/bin/env bash
# test-live-run-state - Verify Factory prefers live worktree run state.
#
# These tests exercise the public CLI only. They create temporary Factory
# projects where source run artifacts are stale and the recorded worktree
# contains newer run artifacts.
#
# Usage:
#   bash tests/behaviors/operations/test-live-run-state.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$(dirname "$(dirname "$SCRIPT_DIR")")")"
FACTORY_BIN="${FACTORY_BIN_OVERRIDE:-${PROJECT_DIR}/target/debug/factory}"

PASS=0
FAIL=0
ERRORS=""

setup_plain_project() {
  TEST_DIR="$(mktemp -d -t factory-test-live-state-XXXXXX)"
  mkdir -p "${TEST_DIR}/project"
  cd "${TEST_DIR}/project"
}

setup_git_project() {
  TEST_DIR="$(mktemp -d -t factory-test-live-state-XXXXXX)"
  mkdir -p "${TEST_DIR}/main"
  cd "${TEST_DIR}/main"
  git init -b main > /dev/null 2>&1
  git config commit.gpgsign false
  git config user.email "test@test"
  git config user.name "test"
  echo "test" > README.md
  git add README.md
  git commit -m "init" > /dev/null 2>&1
}

cleanup_test_project() {
  cd /
  if [ -n "${TEST_DIR:-}" ] && [ -d "${TEST_DIR}/main/.git" ]; then
    git -C "${TEST_DIR}/main" worktree list --porcelain 2>/dev/null | \
      grep '^worktree ' | awk '{print $2}' | \
      grep -v "${TEST_DIR}/main" | while read -r wt; do
      git -C "${TEST_DIR}/main" worktree remove --force "$wt" \
        2>/dev/null || true
    done || true
  fi
  rm -rf "${TEST_DIR:-}"
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

assert_contains() {
  if ! printf '%s' "$1" | grep -q "$2"; then
    printf '    FAIL: output does not contain "%s"\n' "$2"
    printf '    Output:\n%s\n' "$1"
    return 1
  fi
}

assert_not_contains() {
  if printf '%s' "$1" | grep -q "$2"; then
    printf '    FAIL: output should not contain "%s"\n' "$2"
    printf '    Output:\n%s\n' "$1"
    return 1
  fi
}

create_source_and_live_run() {
  RUN_ID="$1"
  SOURCE_STATUS="$2"
  LIVE_STATUS="$3"
  SOURCE_BRIEF="$4"
  LIVE_BRIEF="$5"
  LIVE_WT="${TEST_DIR}/${RUN_ID}-wt"

  mkdir -p ".factory/runs/${RUN_ID}" \
    "${LIVE_WT}/.factory/runs/${RUN_ID}"
  printf '%s' "$SOURCE_STATUS" > ".factory/runs/${RUN_ID}/status"
  printf '%s' "$SOURCE_BRIEF" > ".factory/runs/${RUN_ID}/brief.md"
  printf 'local' > ".factory/runs/${RUN_ID}/runtime"
  printf 'codex' > ".factory/runs/${RUN_ID}/coder"
  printf '%s' "$LIVE_WT" > ".factory/runs/${RUN_ID}/worktree"

  printf '%s' "$LIVE_STATUS" > \
    "${LIVE_WT}/.factory/runs/${RUN_ID}/status"
  printf '%s' "$LIVE_BRIEF" > \
    "${LIVE_WT}/.factory/runs/${RUN_ID}/brief.md"
  printf 'local' > "${LIVE_WT}/.factory/runs/${RUN_ID}/runtime"
  printf 'codex' > "${LIVE_WT}/.factory/runs/${RUN_ID}/coder"
}

test_current_run_status_prefers_live_worktree() {
  setup_plain_project
  create_source_and_live_run \
    "run-live-current" "planned" "needs-user" \
    "SOURCE_CURRENT_BRIEF" "LIVE_CURRENT_BRIEF"

  OUTPUT="$("$FACTORY_BIN" summary --run-id run-live-current 2>&1)"

  RESULT=0
  assert_contains "$OUTPUT" "ID: run-live-current" || RESULT=1
  assert_contains "$OUTPUT" "Status: needs-user" || RESULT=1
  assert_contains "$OUTPUT" "${LIVE_WT}/.factory/runs/run-live-current" || RESULT=1
  assert_not_contains "$OUTPUT" "Status: planned" || RESULT=1

  cleanup_test_project
  return $RESULT
}

test_status_lists_live_status() {
  setup_plain_project
  create_source_and_live_run \
    "run-live-status" "planned" "complete" \
    "SOURCE_STATUS_BRIEF" "LIVE_STATUS_BRIEF"

  OUTPUT="$("$FACTORY_BIN" status --runs 2>&1)"

  RESULT=0
  assert_contains "$OUTPUT" "run-live-status" || RESULT=1
  assert_contains "$OUTPUT" "complete" || RESULT=1
  assert_not_contains "$OUTPUT" "planned" || RESULT=1

  cleanup_test_project
  return $RESULT
}

test_summary_reads_live_artifacts() {
  setup_plain_project
  create_source_and_live_run \
    "run-live-summary" "planned" "needs-user" \
    "SOURCE_SUMMARY_BRIEF" "LIVE_SUMMARY_BRIEF"

  printf 'source-session status=planned\n' > \
    ".factory/runs/run-live-summary/sessions.log"
  printf 'live-session status=needs-user\n' > \
    "${LIVE_WT}/.factory/runs/run-live-summary/sessions.log"
  {
    printf '# Handoff\n\n'
    printf 'Question: LIVE_HANDOFF_QUESTION\n'
  } > "${LIVE_WT}/.factory/runs/run-live-summary/handoff.md"

  OUTPUT="$("$FACTORY_BIN" summary --run-id run-live-summary 2>&1)"

  RESULT=0
  assert_contains "$OUTPUT" "live-session status=needs-user" || RESULT=1
  assert_contains "$OUTPUT" "Question: LIVE_HANDOFF_QUESTION" || RESULT=1
  assert_not_contains "$OUTPUT" "source-session status=planned" || RESULT=1

  cleanup_test_project
  return $RESULT
}

write_mock_agents() {
  MOCK_BIN="${TEST_DIR}/bin"
  mkdir -p "$MOCK_BIN"
  cat > "${MOCK_BIN}/codex" <<'MOCK_CODEX'
#!/usr/bin/env bash
RUN_ID="${EXPECTED_RUN_ID:?}"
RUN_DIR=".factory/runs/${RUN_ID}"
COUNT_FILE="${RUN_DIR}/agent-count"
COUNT=0
if [ -f "$COUNT_FILE" ]; then
  COUNT="$(cat "$COUNT_FILE")"
fi
COUNT=$((COUNT + 1))
printf '%s' "$COUNT" > "$COUNT_FILE"
printf '%s\n' "$*" > "${RUN_DIR}/agent-args-${COUNT}"
printf '{"type":"result","subtype":"success","result":"done","session_id":"mock"}\n'
printf 'needs-user' > "${RUN_DIR}/status"
MOCK_CODEX
  chmod +x "${MOCK_BIN}/codex"
  cp "${MOCK_BIN}/codex" "${MOCK_BIN}/claude"

  cat > "${MOCK_BIN}/sandbox-exec" <<'MOCK_SANDBOX'
#!/usr/bin/env bash
if [ "$1" = "-f" ]; then
  shift 2
fi
exec "$@"
MOCK_SANDBOX
  chmod +x "${MOCK_BIN}/sandbox-exec"
}

test_resume_uses_live_status_rule() {
  setup_git_project
  RUN_ID="run-live-resume"
  git checkout -b "$RUN_ID" > /dev/null 2>&1
  git checkout main > /dev/null 2>&1
  LIVE_WT="${TEST_DIR}/${RUN_ID}-wt"
  git worktree add "$LIVE_WT" "$RUN_ID" > /dev/null 2>&1

  mkdir -p ".factory/runs/${RUN_ID}" \
    "${LIVE_WT}/.factory/runs/${RUN_ID}"
  printf 'planned' > ".factory/runs/${RUN_ID}/status"
  printf 'Resume source brief' > ".factory/runs/${RUN_ID}/brief.md"
  printf 'local' > ".factory/runs/${RUN_ID}/runtime"
  printf 'codex' > ".factory/runs/${RUN_ID}/coder"
  printf '%s' "$LIVE_WT" > ".factory/runs/${RUN_ID}/worktree"
  printf 'failed' > "${LIVE_WT}/.factory/runs/${RUN_ID}/status"
  printf 'Resume live brief' > \
    "${LIVE_WT}/.factory/runs/${RUN_ID}/brief.md"
  printf 'local' > "${LIVE_WT}/.factory/runs/${RUN_ID}/runtime"
  printf 'codex' > "${LIVE_WT}/.factory/runs/${RUN_ID}/coder"
  write_mock_agents

  set +e
  OUTPUT="$(PATH="${MOCK_BIN}:${PATH}" EXPECTED_RUN_ID="$RUN_ID" \
    timeout 20 "$FACTORY_BIN" --no-sandbox resume --coder codex \
    < /dev/null 2>&1)"
  EXIT_CODE=$?
  set -e

  RESULT=0
  if [ "$EXIT_CODE" -ne 0 ]; then
    printf '    FAIL: resume should complete with the mocked agent\n'
    printf '    Output: %s\n' "$OUTPUT"
    RESULT=1
  fi
  assert_contains "$OUTPUT" "Resuming run ${RUN_ID}" || RESULT=1
  if [ "$(cat "${LIVE_WT}/.factory/runs/${RUN_ID}/agent-count" \
    2>/dev/null || echo 0)" != "1" ]; then
    printf '    FAIL: resume did not restart the live worktree run\n'
    RESULT=1
  fi
  if [ -f ".factory/runs/${RUN_ID}/agent-count" ]; then
    printf '    FAIL: resume wrote agent state to the stale source run\n'
    RESULT=1
  fi

  cleanup_test_project
  return $RESULT
}

test_land_uses_live_status_and_reviews() {
  setup_git_project
  RUN_ID="run-live-land"
  git checkout -b "$RUN_ID" > /dev/null 2>&1
  echo "run change" >> README.md
  git add README.md
  git commit -m "run commit for live land" > /dev/null 2>&1
  git checkout main > /dev/null 2>&1

  LIVE_WT="${TEST_DIR}/${RUN_ID}-wt"
  git worktree add "$LIVE_WT" "$RUN_ID" > /dev/null 2>&1

  mkdir -p ".factory/runs/${RUN_ID}/reviews" \
    "${LIVE_WT}/.factory/runs/${RUN_ID}/reviews" \
    "${LIVE_WT}/.factory/runs/${RUN_ID}/sessions/session-1"
  printf 'planned' > ".factory/runs/${RUN_ID}/status"
  printf 'Land source brief' > ".factory/runs/${RUN_ID}/brief.md"
  printf 'main' > ".factory/runs/${RUN_ID}/source-branch"
  printf '%s' "$LIVE_WT" > ".factory/runs/${RUN_ID}/worktree"
  printf 'Verdict: fail\n' > \
    ".factory/runs/${RUN_ID}/reviews/review-behaviors.md"

  printf 'complete' > "${LIVE_WT}/.factory/runs/${RUN_ID}/status"
  printf 'Land live brief' > \
    "${LIVE_WT}/.factory/runs/${RUN_ID}/brief.md"
  printf 'live session log' > \
    "${LIVE_WT}/.factory/runs/${RUN_ID}/sessions.log"
  printf '{"event":"done"}' > \
    "${LIVE_WT}/.factory/runs/${RUN_ID}/sessions/session-1/transcript.jsonl"
  printf 'Verdict: pass\n' > \
    "${LIVE_WT}/.factory/runs/${RUN_ID}/reviews/review-behaviors.md"
  printf 'live report' > "${LIVE_WT}/.factory/runs/${RUN_ID}/report.md"

  set +e
  OUTPUT="$("$FACTORY_BIN" merge "$RUN_ID" 2>&1)"
  EXIT_CODE=$?
  set -e

  RESULT=0
  if [ "$EXIT_CODE" -ne 0 ]; then
    printf '    FAIL: land should use live complete status and pass review\n'
    printf '    Output: %s\n' "$OUTPUT"
    RESULT=1
  fi
  if [ "$(cat ".factory/runs/${RUN_ID}/status" 2>/dev/null)" != "merged" ]; then
    printf '    FAIL: land did not mark the run landed\n'
    RESULT=1
  fi
  if grep -q "Verdict: fail" \
    ".factory/runs/${RUN_ID}/reviews/review-behaviors.md" 2>/dev/null; then
    printf '    FAIL: land validated or kept stale source review artifacts\n'
    RESULT=1
  fi

  cleanup_test_project
  return $RESULT
}

test_invalid_worktree_falls_back_to_source() {
  setup_plain_project
  mkdir -p .factory/runs

  mkdir -p ".factory/runs/run-missing-worktree"
  printf 'planned' > ".factory/runs/run-missing-worktree/status"
  printf 'SOURCE_MISSING_WORKTREE' > \
    ".factory/runs/run-missing-worktree/brief.md"

  mkdir -p ".factory/runs/run-empty-worktree"
  printf 'executing' > ".factory/runs/run-empty-worktree/status"
  printf 'SOURCE_EMPTY_WORKTREE' > \
    ".factory/runs/run-empty-worktree/brief.md"
  : > ".factory/runs/run-empty-worktree/worktree"

  mkdir -p ".factory/runs/run-invalid-worktree"
  printf 'needs-user' > ".factory/runs/run-invalid-worktree/status"
  printf 'SOURCE_INVALID_WORKTREE' > \
    ".factory/runs/run-invalid-worktree/brief.md"
  printf '%s' "${TEST_DIR}/does-not-exist" > \
    ".factory/runs/run-invalid-worktree/worktree"

  mkdir -p ".factory/runs/run-no-live-run"
  printf 'failed' > ".factory/runs/run-no-live-run/status"
  printf 'SOURCE_NO_LIVE_RUN' > ".factory/runs/run-no-live-run/brief.md"
  LIVE_OTHER="${TEST_DIR}/other-live"
  mkdir -p "${LIVE_OTHER}/.factory/runs/other-run"
  printf 'complete' > "${LIVE_OTHER}/.factory/runs/other-run/status"
  printf 'LIVE_SENTINEL_SHOULD_NOT_APPEAR' > \
    "${LIVE_OTHER}/.factory/runs/other-run/brief.md"
  printf '%s' "$LIVE_OTHER" > ".factory/runs/run-no-live-run/worktree"

  OUTPUT="$("$FACTORY_BIN" status --runs 2>&1)"

  RESULT=0
  assert_contains "$OUTPUT" "run-missing-worktree" || RESULT=1
  assert_contains "$OUTPUT" "planned" || RESULT=1
  assert_contains "$OUTPUT" "SOURCE_MISSING_WORKTREE" || RESULT=1
  assert_contains "$OUTPUT" "run-empty-worktree" || RESULT=1
  assert_contains "$OUTPUT" "executing" || RESULT=1
  assert_contains "$OUTPUT" "SOURCE_EMPTY_WORKTREE" || RESULT=1
  assert_contains "$OUTPUT" "run-invalid-worktree" || RESULT=1
  assert_contains "$OUTPUT" "needs-user" || RESULT=1
  assert_contains "$OUTPUT" "SOURCE_INVALID_WORKTREE" || RESULT=1
  assert_contains "$OUTPUT" "run-no-live-run" || RESULT=1
  assert_contains "$OUTPUT" "failed" || RESULT=1
  assert_contains "$OUTPUT" "SOURCE_NO_LIVE_RUN" || RESULT=1
  assert_not_contains "$OUTPUT" "LIVE_SENTINEL_SHOULD_NOT_APPEAR" || RESULT=1

  cleanup_test_project
  return $RESULT
}

printf 'test-live-run-state\n\n'

run_test "current run status prefers live worktree" \
  test_current_run_status_prefers_live_worktree
run_test "status lists live status" test_status_lists_live_status
run_test "summary reads live artifacts" test_summary_reads_live_artifacts
run_test "resume uses live status rule" test_resume_uses_live_status_rule
run_test "land uses live status and reviews" test_land_uses_live_status_and_reviews
run_test "invalid worktree falls back to source" \
  test_invalid_worktree_falls_back_to_source

printf '\n  %d passed, %d failed\n' "$PASS" "$FAIL"

if [ "$FAIL" -gt 0 ]; then
  printf '\n  Failures:%b\n' "$ERRORS"
  exit 1
fi
