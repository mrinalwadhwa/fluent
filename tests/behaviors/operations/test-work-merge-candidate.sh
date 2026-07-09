#!/usr/bin/env bash
# test-work-merge-candidate - Verify Merge Candidate execution from the CLI.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$(dirname "$(dirname "$SCRIPT_DIR")")")"
FACTORY_BIN="${FACTORY_BIN_OVERRIDE:-${PROJECT_DIR}/target/debug/factory}"

source "${PROJECT_DIR}/tests/lib/run_test.sh"
source "${PROJECT_DIR}/tests/lib/work_test_fixtures.sh"
LOG_DIR="${PROJECT_DIR}/tests/output/$(basename "$0" .sh)"

setup_test_project() {
  TEST_DIR="$(mktemp -d -t factory-work-merge-candidate-XXXXXX)"
  mkdir -p "$TEST_DIR/project" "$TEST_DIR/bin"
  cd "$TEST_DIR/project"
  TEST_PROJECT_PWD="$(pwd -P)"
  git init -b main > /dev/null 2>&1
  git config commit.gpgsign false
  git config user.email "test@test"
  git config user.name "test"
  printf 'test\n' > README.md
  seed_review_skill_stubs "."
  git add . && git commit -m "init" > /dev/null 2>&1
}

cleanup_test_project() {
  cd /
  if [ -d "${TEST_DIR}/project/.git" ]; then
    git -C "${TEST_DIR}/project" worktree list --porcelain 2>/dev/null | \
      awk '/^worktree / { print $2 }' | \
      grep -v "^${TEST_PROJECT_PWD}$" | while read -r wt; do
        git -C "${TEST_DIR}/project" worktree remove --force "$wt" 2>/dev/null || true
      done || true
  fi
  rm -rf "$TEST_DIR"
}

write_mock_claude() {
  cat > "${TEST_DIR}/bin/claude" <<'MOCK_SCRIPT'
#!/usr/bin/env bash
# Skip side effects for --version probes from capture_coder_info()
if [ "$1" = "--version" ]; then
  exit 0
fi
# Detect rebase agent invocations via -p flag
PROMPT=""
for arg in "$@"; do
  if [ "$PREV_WAS_P" = "1" ]; then
    PROMPT="$arg"
    break
  fi
  if [ "$arg" = "-p" ]; then
    PREV_WAS_P=1
  else
    PREV_WAS_P=0
  fi
done

if printf '%s' "$PROMPT" | grep -q "Rebase the candidate branch onto"; then
  TARGET=$(printf '%s' "$PROMPT" | grep -o 'onto `[^`]*`' | sed 's/onto `//;s/`//')
  git rebase "$TARGET" 2>/dev/null
  exit $?
fi

case "$PWD" in
  */work-6-work-1-attempt-1)
    if printf '%s' "$*" | grep -q "Address the following merge-time review findings"; then
      # Acting as merge follow-up writer. Commit a uniquely-named
      # file so the merge loop can iterate (the executor requires
      # a new commit before retrying).
      MARKER="followup-$$-$RANDOM.txt"
      printf 'followup %s\n' "$MARKER" > "$MARKER"
      git add "$MARKER"
      git commit -m "Followup commit $MARKER" > /dev/null 2>&1
    else
      printf 'merge output\n' > merge-output.txt
      git add merge-output.txt
      git commit -m "Add merge output" > /dev/null 2>&1
    fi
    ;;
  */merge/reviews/*)
    REVIEWER="$(basename "$PWD")"
    REVIEW_WORKSPACE="$(printf '%s\n' "$*" | awk -F': ' '/Candidate workspace:/ { print $2; exit }')"
    if [ -n "${MERGE_REVIEW_LOG:-}" ]; then
      printf '%s\n' "$REVIEWER" >> "$MERGE_REVIEW_LOG"
    fi
    if [ -n "${MERGE_REVIEW_ENV_LOG:-}" ]; then
      printf 'CARGO_TARGET_DIR=%s\n' "${CARGO_TARGET_DIR:-}" >> "$MERGE_REVIEW_ENV_LOG"
    fi
    if [ -n "${MERGE_REVIEW_TIMING_LOG:-}" ]; then
      printf 'start %s\n' "$REVIEWER" >> "$MERGE_REVIEW_TIMING_LOG"
      sleep 1
      printf 'end %s\n' "$REVIEWER" >> "$MERGE_REVIEW_TIMING_LOG"
    fi
    if [ "${MERGE_MOCK_MODE:-pass}" = "dirty-merge-review-out-of-order" ]; then
      if [ "$REVIEWER" = "documentation" ]; then
        sleep 1
      elif [ "$REVIEWER" = "behaviors" ]; then
        sleep 0.1
      fi
    fi
    if [ -n "${MERGE_REVIEW_ARGS_LOG:-}" ]; then
      printf '%s\n' "$*" >> "$MERGE_REVIEW_ARGS_LOG"
    fi
    if [ "$REVIEWER" = "behaviors" ] && [ "${MERGE_MOCK_MODE:-pass}" = "fail-merge-review" ]; then
      printf 'Verdict: fail\n\nMerge behavior review failed.\n' > review.md
    elif [ "$REVIEWER" = "behaviors" ] && [ "${MERGE_MOCK_MODE:-pass}" = "missing-merge-review" ]; then
      :
    elif { [ "$REVIEWER" = "documentation" ] || [ "$REVIEWER" = "behaviors" ]; } \
      && [ "${MERGE_MOCK_MODE:-pass}" = "dirty-merge-review-out-of-order" ]; then
      printf 'Verdict: pass\n\nMerge review dirtied the candidate.\n' > review.md
      printf 'dirty merge review\n' > "${REVIEW_WORKSPACE}/dirty-${REVIEWER}.txt"
    elif [ "$REVIEWER" = "behaviors" ] && [ "${MERGE_MOCK_MODE:-pass}" = "dirty-merge-review" ]; then
      printf 'Verdict: pass\n\nMerge behavior review dirtied the candidate.\n' > review.md
      printf 'dirty merge review\n' > "${REVIEW_WORKSPACE}/dirty-merge-review.txt"
    elif [ "$REVIEWER" = "behaviors" ] && [ "${MERGE_MOCK_MODE:-pass}" = "dirty-merge-review-factory" ]; then
      printf 'Verdict: pass\n\nMerge behavior review dirtied Factory state.\n' > review.md
      mkdir -p "${REVIEW_WORKSPACE}/.factory/review-scratch"
      printf 'dirty merge review\n' \
        > "${REVIEW_WORKSPACE}/.factory/review-scratch/dirty.txt"
    elif [ "$REVIEWER" = "behaviors" ]; then
      printf 'Verdict: pass\n\nMerge behavior review passed.\n' > review.md
    else
      printf 'Verdict: pass\n\nMerge review passed.\n' > review.md
    fi
    ;;
  *)
    printf 'Verdict: pass\n\nAttempt review passed.\n' > review.md
    ;;
esac

printf '{"type":"result","subtype":"success","result":"done","session_id":"mock"}\n'
MOCK_SCRIPT
  chmod +x "${TEST_DIR}/bin/claude"
}

json_value() {
  "$FACTORY_BIN" work-item show work-1 | jq -r "$1"
}

attempt_record_path() {
  printf '.factory/work/attempts/work-1/attempt-1.json'
}

task_record_path() {
  printf '.factory/work/tasks/work-1/attempt-1/attempt-1-write-1.json'
}

merge_candidate_record_path() {
  printf '.factory/work/merge-candidates/work-1/attempt-1-merge-candidate.json'
}

merge_candidate_record_value() {
  jq -r "$1" "$(merge_candidate_record_path)"
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

create_passed_merge_candidate() {
  "$FACTORY_BIN" work-item create work-1 --title "Merge candidate" > /dev/null
  "$FACTORY_BIN" attempt create work-1 attempt-1 > /dev/null
  PATH="${TEST_DIR}/bin:$PATH" \
    "$FACTORY_BIN" attempt run work-1 attempt-1 --no-sandbox \
      > "$TEST_DIR/attempt-stdout" 2> "$TEST_DIR/attempt-stderr"
}

run_merge() {
  MERGE_MOCK_MODE="${1:-pass}" \
    MERGE_REVIEW_LOG="${TEST_DIR}/merge-review-log" \
    MERGE_REVIEW_ARGS_LOG="${TEST_DIR}/merge-review-args-log" \
    MERGE_REVIEW_ENV_LOG="${TEST_DIR}/merge-review-env-log" \
    MERGE_REVIEW_TIMING_LOG="${MERGE_REVIEW_TIMING_LOG:-}" \
    CANDIDATE_WORKSPACE="${TEST_DIR}/work-6-work-1-attempt-1" \
    PATH="${TEST_DIR}/bin:$PATH" \
    "$FACTORY_BIN" merge-candidate land --no-sandbox work-1 attempt-1-merge-candidate
}

assert_no_merge_reviewer_worktrees() {
  if git -C "$TEST_PROJECT_PWD" worktree list --porcelain | grep -Eq "/review-[0-9]+-"; then
    printf '    FAIL: reviewer worktree remains registered\n'
    return 1
  fi
  SIBLING_DIR="$(dirname "$TEST_PROJECT_PWD")"
  for reviewer in documentation behaviors architecture skills tests; do
    if [ -d "$SIBLING_DIR/review-6-work-1-attempt-1-${reviewer}" ]; then
      printf '    FAIL: reviewer worktree directory remains: %s\n' "review-6-work-1-attempt-1-${reviewer}"
      return 1
    fi
  done
}

assert_merge_review_artifacts_in_reviewer_order() {
  EXPECTED="$TEST_DIR/expected-review-artifacts"
  ACTUAL="$TEST_DIR/actual-review-artifacts"
  cat > "$EXPECTED" <<'EOF'
merge-review-documentation:.factory/work/artifacts/work-1/attempt-1/attempt-1-merge-candidate/merge/reviews/documentation/review.md
merge-review-behaviors:.factory/work/artifacts/work-1/attempt-1/attempt-1-merge-candidate/merge/reviews/behaviors/review.md
merge-review-architecture:.factory/work/artifacts/work-1/attempt-1/attempt-1-merge-candidate/merge/reviews/architecture/review.md
merge-review-skills:.factory/work/artifacts/work-1/attempt-1/attempt-1-merge-candidate/merge/reviews/skills/review.md
merge-review-tests:.factory/work/artifacts/work-1/attempt-1/attempt-1-merge-candidate/merge/reviews/tests/review.md
merge-review-state:.factory/work/artifacts/work-1/attempt-1/attempt-1-merge-candidate/merge/review-state.json
EOF
  json_value '.merge_candidates[0].merge_state.review_artifacts[] | "\(.producer_id):\(.path)"' \
    > "$ACTUAL"
  if ! diff -u "$EXPECTED" "$ACTUAL"; then
    printf '    FAIL: merge review artifacts are not in reviewer order\n'
    return 1
  fi
}

test_work_merge_lands_after_update_and_checks() {
  setup_test_project
  trap cleanup_test_project RETURN
  write_mock_claude
  create_passed_merge_candidate

  OLD_CANDIDATE="$(json_value '.merge_candidates[0].candidate_commit')"
  CANDIDATE_PWD="$(cd ../work-6-work-1-attempt-1 && pwd -P)"
  printf 'target update\n' > target.txt
  git add target.txt && git commit -m "Add target update" > /dev/null 2>&1
  TARGET_BEFORE="$(git rev-parse main)"

  mkdir -p .factory/hooks
  cat > .factory/hooks/check-pre-merge <<EOF
#!/usr/bin/env bash
test -f merge-output.txt && test -f target.txt && printf '%s' "\$PWD" > '${TEST_DIR}/check-pwd'
EOF
  chmod +x .factory/hooks/check-pre-merge

  RESULT=0
  # Use a long debounce so the detached post-merge child does not race
  # the test cleanup.
  FACTORY_POST_MERGE_DEBOUNCE_SECONDS=3600 \
    run_merge pass > "$TEST_DIR/stdout" 2> "$TEST_DIR/stderr" || RESULT=1
  LANDED_COMMIT="$(json_value '.merge_candidates[0].merge_state.merged_commit')"

  [ "$(git rev-parse main)" = "$LANDED_COMMIT" ] || RESULT=1
  [ "$(git rev-parse main)" != "$OLD_CANDIDATE" ] || RESULT=1
  git merge-base --is-ancestor "$TARGET_BEFORE" main || RESULT=1
  [ "$(cat merge-output.txt)" = "merge output" ] || RESULT=1
  [ "$(cat target.txt)" = "target update" ] || RESULT=1
  [ "$(cat "$TEST_DIR/check-pwd")" = "$CANDIDATE_PWD" ] || RESULT=1
  [ "$(json_value '.merge_candidates[0].merge_state.status')" = "merged" ] || RESULT=1
  [ "$(json_value '.merge_candidates[0].merge_state.check_artifacts | length')" = "1" ] || RESULT=1
  # Merge-time reviewers are gone in slice 3 — no review_artifacts on the
  # candidate, the post-merge review fires asynchronously instead.
  [ "$(json_value '.merge_candidates[0].merge_state.review_artifacts | length')" = "0" ] || RESULT=1
  assert_contains "$(cat "$TEST_DIR/stdout")" "Merged Merge Candidate attempt-1-merge-candidate" || RESULT=1
  "$FACTORY_BIN" merge-candidate show work-1 attempt-1-merge-candidate \
    > "$TEST_DIR/landed-candidate" 2> "$TEST_DIR/stderr" || RESULT=1
  [ "$(jq -r '.merge_state.status' "$TEST_DIR/landed-candidate")" = "merged" ] || RESULT=1
  [ "$(jq -r '.merge_state.merged_commit' "$TEST_DIR/landed-candidate")" = "$LANDED_COMMIT" ] || RESULT=1
  if git worktree list --porcelain | grep -Fq "../work-6-work-1-attempt-1"; then
    printf '    FAIL: managed candidate workspace remains registered after merge\n'
    RESULT=1
  fi
  # Verify a post-merge review queue entry was appended.
  [ -f .factory/work/post-merge-review-queue.json ] || RESULT=1
  return $RESULT
}

test_work_merge_rejects_missing_work_item() {
  setup_test_project
  trap cleanup_test_project RETURN
  write_mock_claude
  create_passed_merge_candidate

  MAIN_BEFORE="$(git rev-parse main)"

  RESULT=0
  if "$FACTORY_BIN" merge-candidate land --no-sandbox missing-work attempt-1-merge-candidate \
    > "$TEST_DIR/stdout" 2> "$TEST_DIR/stderr"; then
    printf '    FAIL: command unexpectedly succeeded for missing Work Item\n'
    RESULT=1
  fi
  [ "$(git rev-parse main)" = "$MAIN_BEFORE" ] || RESULT=1
  assert_contains "$(cat "$TEST_DIR/stderr")" "missing-work" || RESULT=1
  return $RESULT
}

test_work_merge_rejects_missing_merge_candidate() {
  setup_test_project
  trap cleanup_test_project RETURN
  write_mock_claude
  create_passed_merge_candidate

  MAIN_BEFORE="$(git rev-parse main)"
  STATE_BEFORE="$("$FACTORY_BIN" work-item show work-1)"

  RESULT=0
  if "$FACTORY_BIN" merge-candidate land --no-sandbox work-1 missing-candidate \
    > "$TEST_DIR/stdout" 2> "$TEST_DIR/stderr"; then
    printf '    FAIL: command unexpectedly succeeded for missing Merge Candidate\n'
    RESULT=1
  fi
  [ "$(git rev-parse main)" = "$MAIN_BEFORE" ] || RESULT=1
  [ "$("$FACTORY_BIN" work-item show work-1)" = "$STATE_BEFORE" ] || RESULT=1
  assert_contains "$(cat "$TEST_DIR/stderr")" "missing-candidate" || RESULT=1
  return $RESULT
}

test_work_merge_rejects_candidate_without_passed_attempt() {
  setup_test_project
  trap cleanup_test_project RETURN
  write_mock_claude
  create_passed_merge_candidate

  MAIN_BEFORE="$(git rev-parse main)"
  jq '.review_state = "failed"' "$(attempt_record_path)" > "$TEST_DIR/attempt.json"
  mv "$TEST_DIR/attempt.json" "$(attempt_record_path)"

  RESULT=0
  assert_fails run_merge pass || RESULT=1
  [ "$(git rev-parse main)" = "$MAIN_BEFORE" ] || RESULT=1
  [ "$(json_value '.merge_candidates[0].merge_state.status')" = "failed" ] || RESULT=1
  assert_contains "$(json_value '.merge_candidates[0].merge_state.failure_reason')" "before reviews passed" || RESULT=1
  assert_contains "$(cat "$TEST_DIR/stderr")" "before reviews passed" || RESULT=1
  return $RESULT
}

test_work_merge_rejects_stored_source_branch_mismatch() {
  setup_test_project
  trap cleanup_test_project RETURN
  write_mock_claude
  create_passed_merge_candidate
  MAIN_BEFORE="$(git rev-parse main)"

  jq '.source_branch = "unexpected-source"' \
    "$(merge_candidate_record_path)" > "$TEST_DIR/candidate.json"
  mv "$TEST_DIR/candidate.json" "$(merge_candidate_record_path)"

  RESULT=0
  assert_fails run_merge pass || RESULT=1
  [ "$(git rev-parse main)" = "$MAIN_BEFORE" ] || RESULT=1
  [ "$(merge_candidate_record_value '.merge_state.status')" = "pending" ] || RESULT=1
  assert_contains "$(cat "$TEST_DIR/stderr")" "source_branch" || RESULT=1
  return $RESULT
}

test_work_merge_rejects_source_workspace_mismatch() {
  setup_test_project
  trap cleanup_test_project RETURN
  write_mock_claude
  create_passed_merge_candidate
  MAIN_BEFORE="$(git rev-parse main)"

  jq '.source_workspace.path = "/tmp/not-factory-source"' \
    "$(merge_candidate_record_path)" > "$TEST_DIR/candidate.json"
  mv "$TEST_DIR/candidate.json" "$(merge_candidate_record_path)"

  RESULT=0
  assert_fails run_merge pass || RESULT=1
  [ "$(git rev-parse main)" = "$MAIN_BEFORE" ] || RESULT=1
  [ "$(merge_candidate_record_value '.merge_state.status')" = "pending" ] || RESULT=1
  assert_contains "$(cat "$TEST_DIR/stderr")" "source_workspace.path" || RESULT=1
  return $RESULT
}

test_work_merge_rejects_wrong_managed_source_workspace() {
  setup_test_project
  trap cleanup_test_project RETURN
  write_mock_claude
  create_passed_merge_candidate
  MAIN_BEFORE="$(git rev-parse main)"

  jq '.source_workspace.path = "../work-6-work-1-other-attempt"' \
    "$(merge_candidate_record_path)" > "$TEST_DIR/candidate.json"
  mv "$TEST_DIR/candidate.json" "$(merge_candidate_record_path)"
  jq '.output.workspace_path = "../work-6-work-1-other-attempt"' \
    "$(task_record_path)" > "$TEST_DIR/task.json"
  mv "$TEST_DIR/task.json" "$(task_record_path)"

  RESULT=0
  assert_fails run_merge pass || RESULT=1
  [ "$(git rev-parse main)" = "$MAIN_BEFORE" ] || RESULT=1
  [ "$(merge_candidate_record_value '.merge_state.status')" = "pending" ] || RESULT=1
  assert_contains "$(cat "$TEST_DIR/stderr")" "source workspace path" || RESULT=1
  assert_contains "$(cat "$TEST_DIR/stderr")" "../work-6-work-1-attempt-1" || RESULT=1
  assert_contains "$(cat "$TEST_DIR/stderr")" "../work-6-work-1-other-attempt" || RESULT=1
  return $RESULT
}

test_work_merge_rejects_target_workspace_mismatch() {
  setup_test_project
  trap cleanup_test_project RETURN
  write_mock_claude
  create_passed_merge_candidate
  MAIN_BEFORE="$(git rev-parse main)"

  jq '.target_workspace.path = "/tmp/not-factory-target"' \
    "$(merge_candidate_record_path)" > "$TEST_DIR/candidate.json"
  mv "$TEST_DIR/candidate.json" "$(merge_candidate_record_path)"

  RESULT=0
  assert_fails run_merge pass || RESULT=1
  [ "$(git rev-parse main)" = "$MAIN_BEFORE" ] || RESULT=1
  [ "$(merge_candidate_record_value '.merge_state.status')" = "pending" ] || RESULT=1
  assert_contains "$(cat "$TEST_DIR/stderr")" "target_workspace.path" || RESULT=1
  return $RESULT
}

test_work_merge_failed_check_leaves_target_unchanged() {
  setup_test_project
  trap cleanup_test_project RETURN
  write_mock_claude
  create_passed_merge_candidate
  MAIN_BEFORE="$(git rev-parse main)"

  mkdir -p .factory/hooks
  cat > .factory/hooks/check-pre-merge <<EOF
#!/usr/bin/env bash
printf check-ran > '${TEST_DIR}/check-marker'
printf 'failing-check-output\n' >&2
exit 3
EOF
  chmod +x .factory/hooks/check-pre-merge

  RESULT=0
  assert_fails run_merge pass || RESULT=1
  [ "$(git rev-parse main)" = "$MAIN_BEFORE" ] || RESULT=1
  [ "$(cat "$TEST_DIR/check-marker")" = "check-ran" ] || RESULT=1
  [ "$(json_value '.merge_candidates[0].merge_state.status')" = "failed" ] || RESULT=1
  [ "$(json_value '.merge_candidates[0].review_state')" = "pending" ] || RESULT=1
  assert_contains "$(json_value '.merge_candidates[0].merge_state.failure_reason')" "check-pre-merge failed" || RESULT=1
  [ "$(json_value '.merge_candidates[0].merge_state.check_artifacts | length')" = "1" ] || RESULT=1
  HOOKS_DIR="$(json_value '.merge_candidates[0].merge_state.check_artifacts[0].path')"
  [ -n "$HOOKS_DIR" ] && [ -d "$HOOKS_DIR" ] || RESULT=1
  grep -q "failing-check-output" "$HOOKS_DIR/check-pre-merge.log" || RESULT=1
  return $RESULT
}

test_work_merge_rebase_failure_leaves_target_unchanged() {
  setup_test_project
  trap cleanup_test_project RETURN
  write_mock_claude
  create_passed_merge_candidate

  printf 'target version\n' > merge-output.txt
  git add merge-output.txt
  git commit -m "Add conflicting target output" > /dev/null 2>&1
  MAIN_BEFORE="$(git rev-parse main)"

  RESULT=0
  assert_fails run_merge pass || RESULT=1
  [ "$(git rev-parse main)" = "$MAIN_BEFORE" ] || RESULT=1
  [ "$(cat merge-output.txt)" = "target version" ] || RESULT=1
  [ "$(json_value '.merge_candidates[0].merge_state.status')" = "failed" ] || RESULT=1
  [ "$(json_value '.merge_candidates[0].review_state')" = "pending" ] || RESULT=1
  assert_contains "$(json_value '.merge_candidates[0].merge_state.failure_reason')" "Rebase agent failed" || RESULT=1
  assert_contains "$(cat "$TEST_DIR/stderr")" "Rebase agent failed" || RESULT=1
  if git -C ../work-6-work-1-attempt-1 status 2>&1 | grep -qi "rebase in progress"; then
    printf '    FAIL: rebase remains in progress after merge failure\n'
    RESULT=1
  fi
  return $RESULT
}

test_work_merge_candidate_inspection_read_only() {
  setup_test_project
  trap cleanup_test_project RETURN
  write_mock_claude
  create_passed_merge_candidate
  MAIN_BEFORE="$(git rev-parse main)"
  STATE_BEFORE="$("$FACTORY_BIN" work-item show work-1)"

  RESULT=0
  "$FACTORY_BIN" merge-candidate show work-1 attempt-1-merge-candidate \
    > "$TEST_DIR/candidate" 2> "$TEST_DIR/stderr" || RESULT=1
  jq -e '.id == "attempt-1-merge-candidate"' "$TEST_DIR/candidate" > /dev/null || RESULT=1
  [ "$(git rev-parse main)" = "$MAIN_BEFORE" ] || RESULT=1
  [ "$("$FACTORY_BIN" work-item show work-1)" = "$STATE_BEFORE" ] || RESULT=1
  [ "$(json_value '.merge_candidates[0].merge_state.status')" = "pending" ] || RESULT=1
  return $RESULT
}

printf 'test-work-merge-candidate\n\n'

run_test "work merge lands after update and checks" \
  test_work_merge_lands_after_update_and_checks
run_test "work merge rejects missing Work Item" \
  test_work_merge_rejects_missing_work_item
run_test "work merge rejects missing Merge Candidate" \
  test_work_merge_rejects_missing_merge_candidate
run_test "work merge rejects candidate without passed Attempt" \
  test_work_merge_rejects_candidate_without_passed_attempt
run_test "work merge rejects source branch mismatch" \
  test_work_merge_rejects_stored_source_branch_mismatch
run_test "work merge rejects source workspace mismatch" \
  test_work_merge_rejects_source_workspace_mismatch
run_test "work merge rejects wrong managed source workspace" \
  test_work_merge_rejects_wrong_managed_source_workspace
run_test "work merge rejects target workspace mismatch" \
  test_work_merge_rejects_target_workspace_mismatch
run_test "work merge failed check leaves target unchanged" \
  test_work_merge_failed_check_leaves_target_unchanged
run_test "work merge rebase failure leaves target unchanged" \
  test_work_merge_rebase_failure_leaves_target_unchanged
run_test "work merge-candidate inspection is read-only" \
  test_work_merge_candidate_inspection_read_only

summarize_and_exit
