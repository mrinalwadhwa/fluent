#!/usr/bin/env bash
# test-work-attempt-loop - Verify Attempt loop orchestration from the CLI.

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
  TEST_DIR="$(mktemp -d -t factory-work-attempt-loop-XXXXXX)"
  mkdir -p "$TEST_DIR/project" "$TEST_DIR/bin"
  cd "$TEST_DIR/project"
  git init -b main > /dev/null 2>&1
  git config commit.gpgsign false
  git config user.email "test@test"
  git config user.name "test"
  printf 'test\n' > README.md
  git add README.md && git commit -m "init" > /dev/null 2>&1
  "$FACTORY_BIN" work create work-1 --title "Attempt loop" > /dev/null
  "$FACTORY_BIN" work attempt work-1 attempt-1 > /dev/null
}

cleanup_test_project() {
  cd /
  if [ -d "${TEST_DIR}/project/.git" ]; then
    git -C "${TEST_DIR}/project" worktree list --porcelain 2>/dev/null | \
      awk '/^worktree / { print $2 }' | \
      grep -v "^${TEST_DIR}/project$" | while read -r wt; do
        git -C "${TEST_DIR}/project" worktree remove --force "$wt" 2>/dev/null || true
      done || true
  fi
  rm -rf "$TEST_DIR"
}

write_mock_claude() {
  local verdict="$1"
  cat > "${TEST_DIR}/bin/claude" <<MOCK_SCRIPT
#!/usr/bin/env bash
case "\$PWD" in
  */.factory/work/workspaces/*)
    printf 'loop output\n' > loop-output.txt
    git add loop-output.txt
    git commit -m "Add loop output" > /dev/null 2>&1
    ;;
  *)
    printf 'Verdict: ${verdict}\n\nLoop review.\n' > review.md
    ;;
esac
exit 0
MOCK_SCRIPT
  chmod +x "${TEST_DIR}/bin/claude"
}

json_value() {
  jq -r "$1" .factory/work/items/work-1.json
}

test_attempt_loop_passes_review_round() {
  setup_test_project
  trap cleanup_test_project RETURN
  write_mock_claude pass

  PATH="${TEST_DIR}/bin:$PATH" "$FACTORY_BIN" work attempt run work-1 attempt-1 --no-sandbox \
    > "$TEST_DIR/stdout"

  grep -q 'Attempt attempt-1 reviews passed' "$TEST_DIR/stdout" || return 1
  [ "$(json_value '.attempts[0].status')" = "complete" ] || return 1
  [ "$(json_value '.attempts[0].review_state')" = "passed" ] || return 1
}

test_attempt_loop_plans_followup() {
  setup_test_project
  trap cleanup_test_project RETURN
  write_mock_claude fail

  PATH="${TEST_DIR}/bin:$PATH" "$FACTORY_BIN" work attempt run work-1 attempt-1 --no-sandbox \
    > "$TEST_DIR/stdout"

  grep -q 'Planned follow-up write Task attempt-1-followup-1' "$TEST_DIR/stdout" || return 1
  [ "$(json_value '.attempts[0].status')" = "planned" ] || return 1
  [ "$(json_value '.attempts[0].review_state')" = "failed" ] || return 1
  [ "$(json_value '.attempts[0].tasks[-1].input_artifacts | length')" = "5" ] || return 1
}

run_test "attempt loop passes review round" test_attempt_loop_passes_review_round
run_test "attempt loop plans follow-up write" test_attempt_loop_plans_followup

printf '\nResults: %d passed, %d failed\n' "$PASS" "$FAIL"
if [ "$FAIL" -ne 0 ]; then
  printf 'Failed tests:%b\n' "$ERRORS"
  exit 1
fi
