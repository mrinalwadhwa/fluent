#!/usr/bin/env bash
# test-work-review-codebase - Verify Work review-only codebase execution.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$(dirname "$(dirname "$SCRIPT_DIR")")")"
FACTORY_BIN="${FACTORY_BIN_OVERRIDE:-${PROJECT_DIR}/target/debug/factory}"

source "${PROJECT_DIR}/tests/lib/run_test.sh"
LOG_DIR="${PROJECT_DIR}/tests/output/$(basename "$0" .sh)"

setup_test_project() {
  TEST_DIR="$(mktemp -d -t factory-work-review-codebase-XXXXXX)"
  mkdir -p "$TEST_DIR/project" "$TEST_DIR/bin"
  cd "$TEST_DIR/project"
  git init -b main > /dev/null 2>&1
  git config commit.gpgsign false
  git config user.email "test@test"
  git config user.name "test"
  printf 'test\n' > README.md
  git add README.md && git commit -m "init" > /dev/null 2>&1
  "$FACTORY_BIN" work create work-1 --title "Review codebase" > /dev/null
}

cleanup_test_project() {
  cd /
  rm -rf "$TEST_DIR"
}

write_mock_claude() {
  local verdict="$1"
  cat > "${TEST_DIR}/bin/claude" <<MOCK_SCRIPT
#!/usr/bin/env bash
printf 'Verdict: ${verdict}\n\nReview-only result.\n' > review.md
exit 0
MOCK_SCRIPT
  chmod +x "${TEST_DIR}/bin/claude"
}

write_dirty_mock_claude() {
  cat > "${TEST_DIR}/bin/claude" <<MOCK_SCRIPT
#!/usr/bin/env bash
printf 'reviewer edit\n' >> ../../../../../README.md
printf 'Verdict: pass\n\nReview-only result.\n' > review.md
exit 0
MOCK_SCRIPT
  chmod +x "${TEST_DIR}/bin/claude"
}

json_value() {
  "$FACTORY_BIN" work show work-1 | jq -r "$1"
}

assert_contains() {
  if ! printf '%s' "$1" | grep -Fq -- "$2"; then
    printf '    FAIL: output does not contain "%s"\n' "$2"
    printf '    Output:\n%s\n' "$1"
    return 1
  fi
}

assert_fails() {
  if "$@" > "$TEST_DIR/stdout" 2> "$TEST_DIR/stderr"; then
    printf '    FAIL: command unexpectedly succeeded: %s\n' "$*"
    printf '    Stdout:\n%s\n' "$(cat "$TEST_DIR/stdout")"
    printf '    Stderr:\n%s\n' "$(cat "$TEST_DIR/stderr")"
    return 1
  fi
}

run_review_codebase() {
  "$FACTORY_BIN" work review-codebase work-1 attempt-review --from-working-tree
}

run_attempt_loop() {
  PATH="${TEST_DIR}/bin:$PATH" "$FACTORY_BIN" work attempt run \
    work-1 attempt-review --no-sandbox
}

test_review_codebase_intake() {
  setup_test_project
  trap cleanup_test_project RETURN

  RESULT=0
  run_review_codebase > "$TEST_DIR/stdout" || RESULT=1
  assert_contains "$(cat "$TEST_DIR/stdout")" "Created review-only Attempt attempt-review" || RESULT=1
  [ "$(json_value '.attempts[0].kind')" = "review-only" ] || RESULT=1
  [ "$(json_value '.attempts[0].status')" = "reviewing" ] || RESULT=1
  [ "$(json_value '[.attempts[0].tasks[] | select(.kind == "review")] | length')" = "5" ] || RESULT=1
  [ "$(json_value '.attempts[0].tasks[] | select(.id == "attempt-review-review-tests") | .workspace_access.reads[0].id')" = "source" ] || RESULT=1
  [ "$(json_value '.attempts[0].tasks[] | select(.id == "attempt-review-review-tests") | .workspace_access.reads[0].path')" = "." ] || RESULT=1
  [ "$(json_value '.attempts[0].tasks[] | select(.id == "attempt-review-review-tests") | .artifact_area.path')" = ".factory/work/artifacts/work-1/attempt-review/attempt-review-review-tests" ] || RESULT=1

  return $RESULT
}

test_review_codebase_rejects_missing_and_duplicate() {
  setup_test_project
  trap cleanup_test_project RETURN

  RESULT=0
  run_review_codebase > /dev/null || RESULT=1
  BEFORE="$(find .factory/work -type f -print0 | sort -z | xargs -0 shasum)"
  assert_fails "$FACTORY_BIN" work review-codebase missing-work attempt-other || RESULT=1
  assert_fails "$FACTORY_BIN" work review-codebase work-1 attempt-review || RESULT=1
  AFTER="$(find .factory/work -type f -print0 | sort -z | xargs -0 shasum)"
  [ "$AFTER" = "$BEFORE" ] || RESULT=1

  return $RESULT
}

test_review_codebase_pass_completes_without_merge_candidate() {
  setup_test_project
  trap cleanup_test_project RETURN
  write_mock_claude pass
  run_review_codebase > /dev/null

  RESULT=0
  run_attempt_loop > "$TEST_DIR/stdout" || RESULT=1
  assert_contains "$(cat "$TEST_DIR/stdout")" "Review-only Attempt attempt-review passed" || RESULT=1
  [ "$(json_value '.attempts[0].status')" = "complete" ] || RESULT=1
  [ "$(json_value '.attempts[0].review_state')" = "passed" ] || RESULT=1
  [ "$(json_value '(.merge_candidates // []) | length')" = "0" ] || RESULT=1
  [ "$(json_value '[.attempts[0].tasks[] | select(.kind == "write")] | length')" = "0" ] || RESULT=1
  git status --porcelain | grep -v '^?? .factory/' > "$TEST_DIR/non-factory-status" || true
  [ ! -s "$TEST_DIR/non-factory-status" ] || RESULT=1

  return $RESULT
}

test_review_codebase_fail_stops_without_followup() {
  setup_test_project
  trap cleanup_test_project RETURN
  write_mock_claude fail
  run_review_codebase > /dev/null

  RESULT=0
  run_attempt_loop > "$TEST_DIR/stdout" || RESULT=1
  assert_contains "$(cat "$TEST_DIR/stdout")" "Review-only Attempt attempt-review failed" || RESULT=1
  [ "$(json_value '.attempts[0].status')" = "failed" ] || RESULT=1
  [ "$(json_value '.attempts[0].review_state')" = "failed" ] || RESULT=1
  [ "$(json_value '(.merge_candidates // []) | length')" = "0" ] || RESULT=1
  [ "$(json_value '[.attempts[0].tasks[] | select(.kind == "write")] | length')" = "0" ] || RESULT=1

  return $RESULT
}

test_review_codebase_rejects_source_changes() {
  setup_test_project
  trap cleanup_test_project RETURN
  write_dirty_mock_claude
  run_review_codebase > /dev/null

  RESULT=0
  assert_fails env PATH="${TEST_DIR}/bin:$PATH" "$FACTORY_BIN" work attempt run \
    work-1 attempt-review --no-sandbox || RESULT=1
  assert_contains "$(cat "$TEST_DIR/stderr")" "changed source checkout outside managed artifact area" || RESULT=1
  [ "$(json_value '(.merge_candidates // []) | length')" = "0" ] || RESULT=1
  [ "$(json_value '[.attempts[0].tasks[] | select(.kind == "write")] | length')" = "0" ] || RESULT=1
  [ "$(json_value '[.attempts[0].tasks[] | select(.kind == "review" and .status == "failed")] | length')" = "1" ] || RESULT=1
  [ "$(cat README.md)" = "test" ] || RESULT=1
  git status --porcelain --untracked-files=all -- . ':(exclude).factory' > "$TEST_DIR/non-factory-status"
  [ ! -s "$TEST_DIR/non-factory-status" ] || RESULT=1

  return $RESULT
}

test_review_codebase_uncertain_needs_user() {
  setup_test_project
  trap cleanup_test_project RETURN
  write_mock_claude uncertain
  run_review_codebase > /dev/null

  RESULT=0
  run_attempt_loop > "$TEST_DIR/stdout" || RESULT=1
  assert_contains "$(cat "$TEST_DIR/stdout")" "Attempt attempt-review needs user input" || RESULT=1
  [ "$(json_value '.attempts[0].status')" = "needs-user" ] || RESULT=1
  [ "$(json_value '.attempts[0].review_state')" = "uncertain" ] || RESULT=1
  [ -f .factory/work/artifacts/work-1/attempt-review/needs-user.md ] || RESULT=1
  assert_contains "$(cat .factory/work/artifacts/work-1/attempt-review/needs-user.md)" "attempt-review-review-tests/review.md" || RESULT=1
  [ "$(json_value '(.merge_candidates // []) | length')" = "0" ] || RESULT=1
  [ "$(json_value '[.attempts[0].tasks[] | select(.kind == "write")] | length')" = "0" ] || RESULT=1

  return $RESULT
}

printf 'test-work-review-codebase\n\n'
run_test "review-codebase creates review-only Attempt" test_review_codebase_intake
run_test "review-codebase rejects missing and duplicate" test_review_codebase_rejects_missing_and_duplicate
run_test "review-only pass completes without Merge Candidate" test_review_codebase_pass_completes_without_merge_candidate
run_test "review-only fail stops without follow-up" test_review_codebase_fail_stops_without_followup
run_test "review-only rejects source changes" test_review_codebase_rejects_source_changes
run_test "review-only uncertain needs user" test_review_codebase_uncertain_needs_user

summarize_and_exit
