#!/usr/bin/env bash
# test-reviewer-launch-failures — Verify reviewer launch failure behavior.
#
# Drives the real factory CLI against temporary Git projects. Fake claude
# commands act as author and reviewer processes so the checks observe the
# same CLI output and run artifacts a user would see.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$(dirname "$(dirname "$SCRIPT_DIR")")")"
FACTORY_BIN="${FACTORY_BIN_OVERRIDE:-${PROJECT_DIR}/target/debug/factory}"

source "${PROJECT_DIR}/tests/lib/run_test.sh"
LOG_DIR="${PROJECT_DIR}/tests/output/$(basename "$0" .sh)"

if [ ! -x "$FACTORY_BIN" ]; then
  (cd "$PROJECT_DIR" && cargo build --quiet)
fi

setup_project() {
  TEST_DIR="$(mktemp -d -t factory-test-reviewer-failures-XXXXXX)"
  SOURCE_DIR="${TEST_DIR}/repo"
  BIN_DIR="${TEST_DIR}/bin"
  RUN_ID="reviewer-failures"

  mkdir -p "${SOURCE_DIR}/.factory/runs/${RUN_ID}" "$BIN_DIR"
  cd "$SOURCE_DIR"
  git init -q -b main
  git config user.email test@example.com
  git config user.name Test
  git config commit.gpgsign false
  printf 'base\n' > tracked.txt
  {
    printf 'ignored.txt\n'
    printf '.factory/*\n'
  } > .gitignore
  git add tracked.txt .gitignore
  git commit -qm init

  printf 'Exercise reviewer failure handling.\n' > ".factory/runs/${RUN_ID}/brief.md"
  printf 'planned' > ".factory/runs/${RUN_ID}/status"
}

cleanup_project() {
  cd "$PROJECT_DIR"
  rm -rf "$TEST_DIR"
}

write_mock_claude() {
  local mode="$1"
  cat > "${BIN_DIR}/claude" <<SH
#!/usr/bin/env bash
set -euo pipefail

run_id="${RUN_ID}"
run_dir="\${PWD}/.factory/runs/\${run_id}"
args="\$*"

if printf '%s' "\$args" | grep -q 'Write your review to'; then
  mkdir -p "\${run_dir}/reviews"
  case "${mode}" in
    launch-failure)
      printf 'reviewer command should have been removed before launch\n' >&2
      exit 1
      ;;
    nonzero-exit)
      printf 'reviewer failed intentionally\n' >&2
      exit 42
      ;;
    missing-artifact)
      printf '{"type":"result","subtype":"success"}\n'
      exit 0
      ;;
    pass)
      for reviewer in architecture behaviors documentation skills tests; do
        printf 'Verdict: pass\n\nLooks good.\n' > "\${run_dir}/reviews/review-\${reviewer}.md"
      done
      printf '{"type":"result","subtype":"success"}\n'
      exit 0
      ;;
    *)
      printf 'unknown reviewer mode: %s\n' "${mode}" >&2
      exit 1
      ;;
  esac
fi

if grep -q 'Verdict: fail' "\${run_dir}"/reviews/review-*.md 2>/dev/null; then
  printf 'needs-user' > "\${run_dir}/status"
  printf '{"type":"result","subtype":"success"}\n'
  exit 0
fi

if [ -f "\${run_dir}/handoff.md" ]; then
  git add -A
  git commit -qm "Commit reviewed work"
else
  printf 'changed\n' >> tracked.txt
  if [ "${mode}" = "launch-failure" ]; then
    rm -f "${BIN_DIR}/claude"
  fi
fi
printf 'complete' > "\${run_dir}/status"
printf '{"type":"result","subtype":"success"}\n'
SH
  chmod +x "${BIN_DIR}/claude"
}

run_factory_with_path() {
  local path_prefix="$1"
  local output_file="${TEST_DIR}/factory-output.txt"
  local test_path="${path_prefix}:/usr/bin:/bin:/usr/sbin:/sbin"
  set +e
  PATH="$test_path" "$FACTORY_BIN" run --no-sandbox --run-id "$RUN_ID" > "$output_file" 2>&1 &
  local pid=$!
  local waited=0
  while kill -0 "$pid" 2>/dev/null && [ "$waited" -lt 20 ]; do
    sleep 1
    waited=$((waited + 1))
  done
  if kill -0 "$pid" 2>/dev/null; then
    kill "$pid" 2>/dev/null || true
    sleep 1
    kill -9 "$pid" 2>/dev/null || true
    EXIT_CODE=124
  else
    wait "$pid"
    EXIT_CODE=$?
  fi
  OUTPUT="$(cat "$output_file")"
  set -e
  WORKTREE="$(cat "${SOURCE_DIR}/.factory/runs/${RUN_ID}/worktree")"
  RUN_DIR="${WORKTREE}/.factory/runs/${RUN_ID}"
}

assert_contains() {
  if ! printf '%s' "$1" | grep -qiE "$2"; then
    printf '    FAIL: output does not match /%s/\n' "$2"
    printf '    Output:\n%s\n' "$1"
    return 1
  fi
}

assert_file_contains() {
  if [ ! -f "$1" ]; then
    printf '    FAIL: expected file %s to exist\n' "$1"
    return 1
  fi
  if ! grep -qiE "$2" "$1"; then
    printf '    FAIL: file %s does not match /%s/\n' "$1" "$2"
    printf '    Content:\n%s\n' "$(cat "$1")"
    return 1
  fi
}

assert_not_status() {
  local unexpected="$1"
  if [ -f "${RUN_DIR}/status" ] && [ "$(cat "${RUN_DIR}/status")" = "$unexpected" ]; then
    printf '    FAIL: status should not remain %s\n' "$unexpected"
    return 1
  fi
}

test_reviewer_launch_failure_blocks_review() {
  setup_project
  write_mock_claude launch-failure
  run_factory_with_path "$BIN_DIR"

  RESULT=0
  [ "$EXIT_CODE" -ne 0 ] || {
    printf '    FAIL: factory exited successfully after reviewer launch failure\n'
    RESULT=1
  }
  [ "$EXIT_CODE" -ne 124 ] || {
    printf '    FAIL: factory did not terminate after reviewer launch failure\n'
    RESULT=1
  }
  assert_contains "$OUTPUT" 'claude|launch|spawn|not found|review' || RESULT=1
  assert_file_contains "${RUN_DIR}/reviews/review-tests.md" 'Verdict: fail' || RESULT=1
  assert_file_contains "${RUN_DIR}/reviews/review-tests.md" 'failed to launch|not found|No such file' || RESULT=1
  assert_not_status complete || RESULT=1

  cleanup_project
  return $RESULT
}

test_reviewer_nonzero_exit_blocks_review() {
  setup_project
  write_mock_claude nonzero-exit
  run_factory_with_path "$BIN_DIR"

  RESULT=0
  [ "$EXIT_CODE" -ne 124 ] || {
    printf '    FAIL: factory did not terminate after reviewer non-zero exit\n'
    RESULT=1
  }
  assert_contains "$OUTPUT" 'exited with code 42|session failed|reviewer failed' || RESULT=1
  assert_file_contains "${RUN_DIR}/reviews/review-tests.md" 'Verdict: fail' || RESULT=1
  assert_file_contains "${RUN_DIR}/reviews/review-tests.md" 'exited with code 42' || RESULT=1
  assert_not_status complete || RESULT=1

  cleanup_project
  return $RESULT
}

test_missing_review_artifact_blocks_review() {
  setup_project
  write_mock_claude missing-artifact
  run_factory_with_path "$BIN_DIR"

  RESULT=0
  [ "$EXIT_CODE" -ne 124 ] || {
    printf '    FAIL: factory did not terminate after missing review artifact\n'
    RESULT=1
  }
  assert_contains "$OUTPUT" 'no review artifact|without writing|review' || RESULT=1
  assert_file_contains "${RUN_DIR}/reviews/review-tests.md" 'Verdict: fail' || RESULT=1
  assert_file_contains "${RUN_DIR}/reviews/review-tests.md" 'without writing' || RESULT=1
  assert_not_status complete || RESULT=1

  cleanup_project
  return $RESULT
}

test_passing_review_artifacts_keep_review_passing() {
  setup_project
  write_mock_claude pass
  run_factory_with_path "$BIN_DIR"

  RESULT=0
  [ "$EXIT_CODE" -eq 0 ] || {
    printf '    FAIL: factory failed when all reviewers wrote pass artifacts\n'
    printf '    Output:\n%s\n' "$OUTPUT"
    RESULT=1
  }
  assert_file_contains "${RUN_DIR}/reviews/review-behaviors.md" 'Verdict: pass' || RESULT=1
  assert_file_contains "${RUN_DIR}/reviews/review-tests.md" 'Verdict: pass' || RESULT=1
  if [ -f "${RUN_DIR}/status" ] && [ "$(cat "${RUN_DIR}/status")" != "complete" ]; then
    printf '    FAIL: status should be complete after passing reviews, got %s\n' "$(cat "${RUN_DIR}/status")"
    RESULT=1
  fi

  cleanup_project
  return $RESULT
}

printf 'test-reviewer-launch-failures\n\n'

run_test "reviewer launch failure blocks review" test_reviewer_launch_failure_blocks_review
run_test "reviewer non-zero exit blocks review" test_reviewer_nonzero_exit_blocks_review
run_test "missing review artifact blocks review" test_missing_review_artifact_blocks_review
run_test "passing review artifacts keep review passing" test_passing_review_artifacts_keep_review_passing

summarize_and_exit
