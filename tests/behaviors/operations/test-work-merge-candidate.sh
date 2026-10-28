#!/usr/bin/env bash
# test-work-merge-candidate - Verify Merge Candidate execution from the CLI.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$(dirname "$(dirname "$SCRIPT_DIR")")")"
FLUENT_BIN="${FLUENT_BIN_OVERRIDE:-${PROJECT_DIR}/target/debug/fluent}"

source "${PROJECT_DIR}/tests/lib/run_test.sh"
source "${PROJECT_DIR}/tests/lib/work_test_fixtures.sh"
LOG_DIR="${PROJECT_DIR}/tests/output/$(basename "$0" .sh)"

setup_test_project() {
  TEST_DIR="$(mktemp -d -t fluent-work-merge-candidate-XXXXXX)"
  mkdir -p "$TEST_DIR/project" "$TEST_DIR/bin"
  install_review_inspection_provider_helper "$TEST_DIR/bin"
  cd "$TEST_DIR/project"
  TEST_PROJECT_PWD="$(pwd -P)"
  git init -b main > /dev/null 2>&1
  git config commit.gpgsign false
  git config user.email "test@test"
  git config user.name "test"
  printf 'test\n' > README.md
  seed_review_skill_stubs "."
  seed_tester_config "."
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
trap 'status=$?; fluent-test-review-inspection claude "$status" "$@" || exit $?; exit "$status"' EXIT
if [ "$1 $2 $3" = "auth status --json" ]; then
  printf '{"loggedIn":true}\n'
  exit 0
fi
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
    elif [ "$REVIEWER" = "behaviors" ] && [ "${MERGE_MOCK_MODE:-pass}" = "dirty-merge-review-fluent" ]; then
      printf 'Verdict: pass\n\nMerge behavior review dirtied Fluent state.\n' > review.md
      mkdir -p "${REVIEW_WORKSPACE}/.fluent/review-scratch"
      printf 'dirty merge review\n' \
        > "${REVIEW_WORKSPACE}/.fluent/review-scratch/dirty.txt"
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
  "$FLUENT_BIN" work-item show work-1 | jq -r "$1"
}

attempt_record_path() {
  printf '.fluent/work/attempts/work-1/attempt-1.json'
}

task_record_path() {
  printf '.fluent/work/tasks/work-1/attempt-1/attempt-1-write-1.json'
}

merge_candidate_record_path() {
  printf '.fluent/work/merge-candidates/work-1/attempt-1-merge-candidate.json'
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
  "$FLUENT_BIN" work-item create work-1 --title "Merge candidate" > /dev/null
  "$FLUENT_BIN" attempt create work-1 attempt-1 > /dev/null
  PATH="${TEST_DIR}/bin:$PATH" \
    "$FLUENT_BIN" attempt run work-1 attempt-1 --no-sandbox \
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
    "$FLUENT_BIN" merge-candidate land --no-sandbox work-1 attempt-1-merge-candidate
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
merge-review-documentation:.fluent/work/artifacts/work-1/attempt-1/attempt-1-merge-candidate/merge/reviews/documentation/review.md
merge-review-behaviors:.fluent/work/artifacts/work-1/attempt-1/attempt-1-merge-candidate/merge/reviews/behaviors/review.md
merge-review-architecture:.fluent/work/artifacts/work-1/attempt-1/attempt-1-merge-candidate/merge/reviews/architecture/review.md
merge-review-skills:.fluent/work/artifacts/work-1/attempt-1/attempt-1-merge-candidate/merge/reviews/skills/review.md
merge-review-tests:.fluent/work/artifacts/work-1/attempt-1/attempt-1-merge-candidate/merge/reviews/tests/review.md
merge-review-state:.fluent/work/artifacts/work-1/attempt-1/attempt-1-merge-candidate/merge/review-state.json
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

  mkdir -p .fluent/hooks
  cat > .fluent/hooks/check-pre-merge <<EOF
#!/usr/bin/env bash
test -f merge-output.txt && test -f target.txt && printf '%s' "\$PWD" > '${TEST_DIR}/check-pwd'
EOF
  chmod +x .fluent/hooks/check-pre-merge

  RESULT=0
  run_merge pass > "$TEST_DIR/stdout" 2> "$TEST_DIR/stderr" || RESULT=1
  LANDED_COMMIT="$(json_value '.merge_candidates[0].merge_state.merged_commit')"

  [ "$(git rev-parse main)" = "$LANDED_COMMIT" ] || RESULT=1
  [ "$(git rev-parse main)" != "$OLD_CANDIDATE" ] || RESULT=1
  git merge-base --is-ancestor "$TARGET_BEFORE" main || RESULT=1
  [ "$(cat merge-output.txt)" = "merge output" ] || RESULT=1
  [ "$(cat target.txt)" = "target update" ] || RESULT=1
  [ "$(cat "$TEST_DIR/check-pwd")" = "$CANDIDATE_PWD" ] || RESULT=1
  [ "$(json_value '.merge_candidates[0].merge_state.status')" = "merged" ] || RESULT=1
  [ "$(json_value '.merge_candidates[0].merge_state.check_artifacts | map(.producer_id) | sort | join(",")')" = \
    "merge-hook-check-pre-merge,merge-target-baseline,merge-tester,merge-tester-manifest" ] || RESULT=1
  TESTER_ARTIFACT="$(json_value '.merge_candidates[0].merge_state.check_artifacts[] | select(.producer_id == "merge-tester") | .path')"
  [ "$(jq -r '.candidate_commit' "$TESTER_ARTIFACT")" = "$LANDED_COMMIT" ] || RESULT=1
  MANIFEST_ARTIFACT="$(json_value '.merge_candidates[0].merge_state.check_artifacts[] | select(.producer_id == "merge-tester-manifest") | .path')"
  [ "$MANIFEST_ARTIFACT" = "$(dirname "$TESTER_ARTIFACT")/canonical-run.json" ] || RESULT=1
  [ "$(jq -r '.full_run' "$MANIFEST_ARTIFACT")" = "true" ] || RESULT=1
  # Attempt reviews are complete; landing runs deterministic checks only.
  [ "$(json_value '.merge_candidates[0].merge_state.review_artifacts | length')" = "0" ] || RESULT=1
  assert_contains "$(cat "$TEST_DIR/stdout")" "Merged Merge Candidate attempt-1-merge-candidate" || RESULT=1
  "$FLUENT_BIN" merge-candidate show work-1 attempt-1-merge-candidate \
    > "$TEST_DIR/landed-candidate" 2> "$TEST_DIR/stderr" || RESULT=1
  [ "$(jq -r '.merge_state.status' "$TEST_DIR/landed-candidate")" = "merged" ] || RESULT=1
  [ "$(jq -r '.merge_state.merged_commit' "$TEST_DIR/landed-candidate")" = "$LANDED_COMMIT" ] || RESULT=1
  if git worktree list --porcelain | grep -Fq "../work-6-work-1-attempt-1"; then
    printf '    FAIL: managed candidate workspace remains registered after merge\n'
    RESULT=1
  fi
  # An ordinary land stops after Learner handoff processing: it queues, logs,
  # and spawns no post-merge review.
  if [ -f .fluent/work/post-merge-review-queue.json ] && \
     [ "$(jq -r '.entries | length' .fluent/work/post-merge-review-queue.json)" != "0" ]; then
    printf '    FAIL: ordinary land appended a post-merge review queue entry\n'
    RESULT=1
  fi
  if [ -f .fluent/work/post-merge-review.log ]; then
    printf '    FAIL: ordinary land spawned a post-merge review runner\n'
    RESULT=1
  fi
  return $RESULT
}

test_work_merge_explicit_post_merge_review_enqueues() {
  setup_test_project
  trap cleanup_test_project RETURN
  write_mock_claude
  create_passed_merge_candidate

  RESULT=0
  # The explicit positive option enqueues the review. Cap the corrective fix
  # depth at 0 so the detached runner is deferred rather than spawned: the entry
  # is observable and no child outlives the test.
  MERGE_MOCK_MODE=pass \
  CANDIDATE_WORKSPACE="${TEST_DIR}/work-6-work-1-attempt-1" \
  FLUENT_MAX_POST_MERGE_REVIEW_FIX_DEPTH=0 \
  PATH="${TEST_DIR}/bin:$PATH" \
    "$FLUENT_BIN" merge-candidate land --no-sandbox --post-merge-review \
      work-1 attempt-1-merge-candidate \
      > "$TEST_DIR/stdout" 2> "$TEST_DIR/stderr" || RESULT=1

  [ "$(json_value '.merge_candidates[0].merge_state.status')" = "merged" ] || RESULT=1
  if [ ! -f .fluent/work/post-merge-review-queue.json ]; then
    printf '    FAIL: explicit --post-merge-review appended no queue entry\n'
    RESULT=1
  elif [ "$(jq -r '.entries | length' .fluent/work/post-merge-review-queue.json)" != "1" ]; then
    printf '    FAIL: explicit --post-merge-review did not append exactly one entry\n'
    RESULT=1
  fi
  # The fix-depth cap deferred the spawn, so no runner log exists.
  if [ -f .fluent/work/post-merge-review.log ]; then
    printf '    FAIL: capped spawn still started a post-merge review runner\n'
    RESULT=1
  fi
  assert_contains "$(cat "$TEST_DIR/stderr")" "Skipping post-merge review spawn" || RESULT=1
  return $RESULT
}

test_work_merge_rejects_conflicting_post_merge_review_options() {
  setup_test_project
  trap cleanup_test_project RETURN
  write_mock_claude
  create_passed_merge_candidate

  MAIN_BEFORE="$(git rev-parse main)"

  RESULT=0
  # Passing both spellings is contradictory and is rejected before any mutation.
  if PATH="${TEST_DIR}/bin:$PATH" \
    "$FLUENT_BIN" merge-candidate land --no-sandbox \
      --post-merge-review --no-post-merge-review \
      work-1 attempt-1-merge-candidate \
      > "$TEST_DIR/stdout" 2> "$TEST_DIR/stderr"; then
    printf '    FAIL: land accepted conflicting post-merge-review options\n'
    RESULT=1
  fi
  assert_contains "$(cat "$TEST_DIR/stderr")" "conflict" || RESULT=1
  [ "$(git rev-parse main)" = "$MAIN_BEFORE" ] || RESULT=1
  [ "$(json_value '.merge_candidates[0].merge_state.status')" = "pending" ] || RESULT=1
  return $RESULT
}

test_work_merge_rejects_missing_work_item() {
  setup_test_project
  trap cleanup_test_project RETURN
  write_mock_claude
  create_passed_merge_candidate

  MAIN_BEFORE="$(git rev-parse main)"

  RESULT=0
  if "$FLUENT_BIN" merge-candidate land --no-sandbox missing-work attempt-1-merge-candidate \
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
  STATE_BEFORE="$("$FLUENT_BIN" work-item show work-1)"

  RESULT=0
  if "$FLUENT_BIN" merge-candidate land --no-sandbox work-1 missing-candidate \
    > "$TEST_DIR/stdout" 2> "$TEST_DIR/stderr"; then
    printf '    FAIL: command unexpectedly succeeded for missing Merge Candidate\n'
    RESULT=1
  fi
  [ "$(git rev-parse main)" = "$MAIN_BEFORE" ] || RESULT=1
  [ "$("$FLUENT_BIN" work-item show work-1)" = "$STATE_BEFORE" ] || RESULT=1
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

  jq '.source_workspace.path = "/tmp/not-fluent-source"' \
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

  jq '.target_workspace.path = "/tmp/not-fluent-target"' \
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

  mkdir -p .fluent/hooks
  cat > .fluent/hooks/check-pre-merge <<EOF
#!/usr/bin/env bash
printf check-ran > '${TEST_DIR}/check-marker'
printf 'failing-check-output\n' >&2
exit 3
EOF
  chmod +x .fluent/hooks/check-pre-merge

  RESULT=0
  assert_fails run_merge pass || RESULT=1
  [ "$(git rev-parse main)" = "$MAIN_BEFORE" ] || RESULT=1
  [ "$(cat "$TEST_DIR/check-marker")" = "check-ran" ] || RESULT=1
  [ "$(json_value '.merge_candidates[0].merge_state.status')" = "failed" ] || RESULT=1
  [ "$(json_value '.merge_candidates[0].merge_review_state')" = "pending" ] || RESULT=1
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
  [ "$(json_value '.merge_candidates[0].merge_review_state')" = "pending" ] || RESULT=1
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
  STATE_BEFORE="$("$FLUENT_BIN" work-item show work-1)"

  RESULT=0
  "$FLUENT_BIN" merge-candidate show work-1 attempt-1-merge-candidate \
    > "$TEST_DIR/candidate" 2> "$TEST_DIR/stderr" || RESULT=1
  jq -e '.id == "attempt-1-merge-candidate"' "$TEST_DIR/candidate" > /dev/null || RESULT=1
  [ "$(git rev-parse main)" = "$MAIN_BEFORE" ] || RESULT=1
  [ "$("$FLUENT_BIN" work-item show work-1)" = "$STATE_BEFORE" ] || RESULT=1
  [ "$(json_value '.merge_candidates[0].merge_state.status')" = "pending" ] || RESULT=1
  return $RESULT
}

legacy_fixture_cli() {
  env -u FLUENT_TASK_KIND \
    HOME="${TEST_DIR}/home" XDG_CONFIG_HOME="${TEST_DIR}/config" \
    XDG_DATA_HOME="${TEST_DIR}/data" CLAUDE_CONFIG_DIR="${TEST_DIR}/claude" \
    FLUENT_NO_UPDATE_CHECK=1 "$@"
}

write_legacy_mock_claude() {
  cat > "${TEST_DIR}/bin/claude" <<'MOCK'
#!/usr/bin/env bash
set -eu
trap 'status=$?; fluent-test-review-inspection claude "$status" "$@" || exit $?; exit "$status"' EXIT
if [ "${1:-} ${2:-} ${3:-}" = "auth status --json" ]; then
  printf '{"loggedIn":true}\n'
  exit 0
fi
if [ "${1:-}" = "--version" ]; then printf '2.1.211\n'; exit 0; fi
PROMPT=""
previous=""
for arg in "$@"; do
  if [ "$previous" = "-p" ]; then PROMPT="$arg"; break; fi
  previous="$arg"
done
# Credential refresh is a host probe, even when it runs in a Writer directory.
# Handle it before inferring task roles from the current directory.
if [ "$#" -eq 3 ] && [ "$1" = --safe-mode ] && [ "$2" = -p ] && [ "$PROMPT" = ok ]; then
  printf '{"type":"result","subtype":"success","result":"ok"}\n'
  exit 0
fi
# Writer launches can point at a saved assignment instead of inlining its text.
case "$PWD:$PROMPT" in
  */work-6-work-1-attempt-1:*'You are the Learner'*) ;;
  */work-6-work-1-attempt-1:*'Rebase the candidate branch onto'*) ;;
  */work-6-work-1-attempt-1:*) PROMPT='You are a Fluent Writer' ;;
esac
case "$PROMPT" in
  *'Rebase the candidate branch onto'*)
    target=$(printf '%s' "$PROMPT" | sed -n 's/.*onto `\([^`]*\)`.*/\1/p' | head -1)
    git rebase "$target"
    ;;
  *'You are the Learner'*)
    mkdir -p .fluent/expertise
    printf 'Preserve the original review identity.\n' > .fluent/expertise/landing.md
    draft=$(printf '%s\n' "$PROMPT" | grep -o '/[^ ]*follow-up-draft.json' | head -1)
    printf '{"learning_summary":"Preserve review identity","follow_ups":[]}\n' > "$draft"
    ;;
  *'You are a Fluent Writer'*)
    if [ ! -f candidate.txt ]; then
      printf 'first candidate\n' > candidate.txt
      git add candidate.txt
      git commit -m 'Add candidate' -m '- Create the initial candidate'
    else
      if [ "$LANDING_FIXTURE_DELTA" = carried ]; then
        printf 'Document the candidate.\n' >> README.md
        git add README.md
      else
        printf 'corrected candidate\n' >> candidate.txt
        git add candidate.txt
      fi
      git commit -m 'Update candidate' -m '- Apply the requested correction'
    fi
    ;;
  *)
    case "$PWD" in
      *attempt-1-review-documentation*) printf 'Verdict: fail\n\nCorrect the candidate.\n' > review.md ;;
      *) printf 'Verdict: pass\n\nCandidate inspected.\n' > review.md ;;
    esac
    ;;
esac
printf '{"type":"result","subtype":"success","result":"done","session_id":"mock"}\n'
MOCK
  chmod +x "${TEST_DIR}/bin/claude"
}

test_legacy_mock_probes_leave_source_unchanged() {
  setup_test_project
  trap cleanup_test_project RETURN
  write_legacy_mock_claude
  local before
  before=$(git status --porcelain)
  # A fresh token needs no refresh; an expired token runs this exact probe.
  # Exercise it in both the source root and a managed Writer-shaped directory.
  local directory
  for directory in "$PWD" "$TEST_DIR/work-6-work-1-attempt-1"; do
    mkdir -p "$directory"
    (
      cd "$directory"
      export PATH="$TEST_DIR/bin:$PATH"
      "$TEST_DIR/bin/claude" auth status --json | grep -q '"loggedIn":true' || exit 1
      "$TEST_DIR/bin/claude" --version | grep -q '2.1.211' || exit 1
      "$TEST_DIR/bin/claude" --safe-mode -p ok || exit 1
      [ ! -e review.md ] && [ ! -e candidate.txt ]
    ) || return 1
  done
  [ "$(git status --porcelain)" = "$before" ]
}

# Build the history with the published executable. No Work records are synthesized
# for the positive cases: the old Writer and reviewers own every saved identity.
create_legacy_branch_switch_candidate() {
  local delta="$1"
  mkdir -p "${TEST_DIR}/home"
  OLD_FLUENT_BIN="$(bash "${PROJECT_DIR}/tests/fixtures/prepare-released-060.sh")" || return 1
  write_legacy_mock_claude
  export LANDING_FIXTURE_DELTA="$delta"
  legacy_fixture_cli "$OLD_FLUENT_BIN" work-item create work-1 --title 'Preserve review identity' > /dev/null || return 1
  legacy_fixture_cli "$OLD_FLUENT_BIN" attempt create work-1 attempt-1 --write-coder claude --review-coder claude > /dev/null || return 1
  PATH="${TEST_DIR}/bin:$PATH" legacy_fixture_cli "$OLD_FLUENT_BIN" task run work-1 attempt-1 attempt-1-write-1 --no-sandbox || return 1
  git checkout -b integration > /dev/null 2>&1 || return 1
  PATH="${TEST_DIR}/bin:$PATH" legacy_fixture_cli "$OLD_FLUENT_BIN" attempt run work-1 attempt-1 --no-sandbox || return 1
  legacy_fixture_cli "$OLD_FLUENT_BIN" work-item show work-1 > "$TEST_DIR/legacy.json" || return 1
  jq -e '
    .attempts[0] as $a |
    [$a.tasks[] | select(.kind == "write") | .output.source_branch] == ["main", "integration"] and
    $a.review_state == "passed" and $a.learning.status == "succeeded" and
    .merge_candidates[0].target_branch == "integration"
  ' "$TEST_DIR/legacy.json" > /dev/null || return 1
}

snapshot_ordinary_reviews() {
  local destination="$1"
  mkdir -p "$destination"
  for record in .fluent/work/tasks/work-1/attempt-1/*review*.json; do
    cp "$record" "$destination/$(basename "$record")" || return 1
    local area
    area=$(jq -r '.artifact_area.path' "$record")
    cp "$area/review.md" "$destination/$(basename "$record").md" || return 1
  done
}

test_work_merge_legacy_branch_switch() {
  local delta="$1"
  setup_test_project
  trap cleanup_test_project RETURN
  create_legacy_branch_switch_candidate "$delta" || return 1
  snapshot_ordinary_reviews "$TEST_DIR/reviews-before" || return 1
  local main_before
  main_before=$(git rev-parse main)
  PATH="${TEST_DIR}/bin:$PATH" legacy_fixture_cli "$FLUENT_BIN" merge-candidate land --no-sandbox work-1 attempt-1-merge-candidate || return 1
  [ "$(git rev-parse main)" = "$main_before" ] || return 1
  [ "$(merge_candidate_record_value '.target_branch')" = integration ] || return 1
  [ "$(merge_candidate_record_value '.merge_state.status')" = merged ] || return 1
  snapshot_ordinary_reviews "$TEST_DIR/reviews-after" || return 1
  diff -ru "$TEST_DIR/reviews-before" "$TEST_DIR/reviews-after"
}

test_work_merge_rejects_changed_effective_coverage() {
  setup_test_project
  trap cleanup_test_project RETURN
  create_legacy_branch_switch_candidate carried || return 1
  cp -R .fluent/work "$TEST_DIR/original-work" || return 1
  local target_before corruption
  target_before=$(git rev-parse integration)
  for corruption in branch commit workspace access missing changed-domain; do
    # Deliberate negative controls start from the same real completed history.
    rm -rf .fluent/work
    cp -R "$TEST_DIR/original-work" .fluent/work || return 1
    python3 - "$corruption" <<'PYCONTROL' || return 1
import json, sys
from pathlib import Path
kind = sys.argv[1]
root = Path('.fluent/work')
name = 'attempt-1-review-architecture'
if kind == 'changed-domain':
    name = 'attempt-1-review-2-documentation'
path = root / 'tasks/work-1/attempt-1' / (name + '.json')
if kind in ('missing', 'changed-domain'):
    path.unlink()
else:
    value = json.loads(path.read_text())
    if kind == 'branch': value['review_context']['source_branch'] = 'integration'
    if kind == 'commit': value['review_context']['candidate_commit'] = '0' * 40
    if kind == 'workspace':
        value['review_context']['candidate_workspace_path'] = '../foreign'
        value['workspace_access']['reads'] = [{'id': 'candidate', 'path': '../foreign'}]
    if kind == 'access': value['workspace_access']['reads'] = []
    path.write_text(json.dumps(value))
PYCONTROL
    if PATH="${TEST_DIR}/bin:$PATH" legacy_fixture_cli "$FLUENT_BIN" merge-candidate land --no-sandbox work-1 attempt-1-merge-candidate > "$TEST_DIR/refusal" 2>&1; then
      printf 'Unexpected landing success for %s\n' "$corruption"
      return 1
    fi
    cat "$TEST_DIR/refusal"
    if [ "$corruption" = access ]; then
      assert_contains "$(cat "$TEST_DIR/refusal")" 'readable workspace' || return 1
    else
      assert_contains "$(cat "$TEST_DIR/refusal")" 'review binding changed before landing' || return 1
    fi
    [ "$(git rev-parse integration)" = "$target_before" ] || return 1
    [ "$(merge_candidate_record_value '.merge_state.landing_intent // empty')" = "" ] || return 1
    if [ "$corruption" = branch ]; then
      if PATH="${TEST_DIR}/bin:$PATH" legacy_fixture_cli "$FLUENT_BIN" attempt run work-1 attempt-1 --no-sandbox > "$TEST_DIR/readiness" 2>&1; then
        cat "$TEST_DIR/readiness"
        return 1
      fi
      assert_contains "$(cat "$TEST_DIR/readiness")" 'review binding changed before landing' || return 1
      assert_contains "$(cat "$TEST_DIR/readiness")" 'fluent merge-candidate show work-1' || return 1
    fi
  done
}

test_work_merge_recovers_released_landing_pending() {
  setup_test_project
  trap cleanup_test_project RETURN
  create_legacy_branch_switch_candidate carried || return 1
  snapshot_ordinary_reviews "$TEST_DIR/reviews-before" || return 1
  local target_before
  target_before=$(git rev-parse integration)
  if PATH="${TEST_DIR}/bin:$PATH" legacy_fixture_cli "$OLD_FLUENT_BIN" merge-candidate land --no-sandbox work-1 attempt-1-merge-candidate > "$TEST_DIR/old-land" 2>&1; then
    printf 'Published 0.6 unexpectedly landed the branch-switch candidate\n'
    return 1
  fi
  cat "$TEST_DIR/old-land"
  assert_contains "$(cat "$TEST_DIR/old-land")" 'ordinary documentation review binding changed before landing' || return 1
  [ "$(merge_candidate_record_value '.merge_state.status')" = landing-pending ] || return 1
  [ "$(git rev-parse integration)" = "$target_before" ] || return 1
  merge_candidate_record_value '.merge_state.landing_intent' > "$TEST_DIR/original-intent.json"
  cp -R .fluent/work "$TEST_DIR/pending-work" || return 1

  local corruption expected target_after
  for corruption in evidence model authorization cancellation target repository; do
    rm -rf .fluent/work
    cp -R "$TEST_DIR/pending-work" .fluent/work || return 1
    expected='landing intent no longer matches'
    case "$corruption" in
      evidence)
        local evidence_path
        evidence_path=$(jq -r '.check_artifacts[0].path' "$TEST_DIR/original-intent.json")
        printf '\nchanged evidence\n' >> "$evidence_path"
        ;;
      model)
        local record='.fluent/work/tasks/work-1/attempt-1/attempt-1-review-architecture.json'
        jq '.review_context.source_branch = "altered"' "$record" > "$TEST_DIR/changed.json" || return 1
        cp "$TEST_DIR/changed.json" "$record"
        ;;
      authorization)
        local record='.fluent/work/items/work-1.json'
        jq '.authorization = {state:"execution-ready", authority:"automatic"}' "$record" > "$TEST_DIR/changed.json" || return 1
        cp "$TEST_DIR/changed.json" "$record"
        expected='landing authorization changed'
        ;;
      cancellation)
        legacy_fixture_cli "$FLUENT_BIN" attempt cancel work-1 attempt-1 || return 1
        expected='cancel'
        ;;
      target)
        git commit --allow-empty -m 'Move fixture target' > /dev/null || return 1
        expected='target moved'
        ;;
      repository)
        local record
        record=$(merge_candidate_record_path)
        jq '.merge_state.landing_intent.repository = "/different/repository"' "$record" > "$TEST_DIR/changed.json" || return 1
        cp "$TEST_DIR/changed.json" "$record"
        ;;
    esac
    target_after=$(git rev-parse integration)
    if PATH="${TEST_DIR}/bin:$PATH" legacy_fixture_cli "$FLUENT_BIN" merge-candidate land --no-sandbox work-1 attempt-1-merge-candidate > "$TEST_DIR/refusal" 2>&1; then
      printf 'Unexpected recovery success after changing %s\n' "$corruption"
      return 1
    fi
    cat "$TEST_DIR/refusal"
    assert_contains "$(cat "$TEST_DIR/refusal")" "$expected" || return 1
    [ "$(git rev-parse integration)" = "$target_after" ] || return 1
    if [ "$corruption" = target ]; then
      git reset --hard "$target_before" > /dev/null || return 1
    fi
  done

  rm -rf .fluent/work
  cp -R "$TEST_DIR/pending-work" .fluent/work || return 1
  # Retry without rewriting any historical record or starting another Writer.
  PATH="${TEST_DIR}/bin:$PATH" legacy_fixture_cli "$FLUENT_BIN" attempt run work-1 attempt-1 --no-sandbox || return 1
  PATH="${TEST_DIR}/bin:$PATH" legacy_fixture_cli "$FLUENT_BIN" merge-candidate land --no-sandbox work-1 attempt-1-merge-candidate || return 1
  [ "$(merge_candidate_record_value '.merge_state.status')" = merged ] || return 1
  [ "$(git rev-parse integration)" = "$(jq -r '.verified_commit' "$TEST_DIR/original-intent.json")" ] || return 1
  merge_candidate_record_value '.merge_state.landing_intent' > "$TEST_DIR/after-intent.json"
  diff -u "$TEST_DIR/original-intent.json" "$TEST_DIR/after-intent.json" || return 1
  snapshot_ordinary_reviews "$TEST_DIR/reviews-after" || return 1
  diff -ru "$TEST_DIR/reviews-before" "$TEST_DIR/reviews-after" || return 1
  local reflog_before
  reflog_before=$(git reflog show --format='%H %gs' integration)
  PATH="${TEST_DIR}/bin:$PATH" legacy_fixture_cli "$FLUENT_BIN" merge-candidate land --no-sandbox work-1 attempt-1-merge-candidate || return 1
  [ "$(git reflog show --format='%H %gs' integration)" = "$reflog_before" ] || return 1
}

test_work_merge_reconciles_applied_intent() {
  setup_test_project
  trap cleanup_test_project RETURN
  create_legacy_branch_switch_candidate fresh || return 1
  snapshot_ordinary_reviews "$TEST_DIR/reviews-before" || return 1
  if FLUENT_TEST_INTERRUPT_AFTER_LANDING_GIT=1 PATH="${TEST_DIR}/bin:$PATH" legacy_fixture_cli "$FLUENT_BIN" merge-candidate land --no-sandbox work-1 attempt-1-merge-candidate > "$TEST_DIR/interrupted" 2>&1; then
    printf 'Landing did not stop at the injected publication boundary\n'
    return 1
  fi
  cat "$TEST_DIR/interrupted"
  assert_contains "$(cat "$TEST_DIR/interrupted")" 'injected interruption after landing Git effect' || return 1
  [ "$(merge_candidate_record_value '.merge_state.status')" = landing-pending ] || return 1
  [ "$(git rev-parse integration)" = "$(merge_candidate_record_value '.merge_state.landing_intent.verified_commit')" ] || return 1
  local reflog_before
  reflog_before=$(git reflog show --format='%H %gs' integration)
  PATH="${TEST_DIR}/bin:$PATH" legacy_fixture_cli "$FLUENT_BIN" merge-candidate land --no-sandbox work-1 attempt-1-merge-candidate || return 1
  [ "$(merge_candidate_record_value '.merge_state.status')" = merged ] || return 1
  [ "$(git reflog show --format='%H %gs' integration)" = "$reflog_before" ] || return 1
  snapshot_ordinary_reviews "$TEST_DIR/reviews-after" || return 1
  diff -ru "$TEST_DIR/reviews-before" "$TEST_DIR/reviews-after"
}

printf 'test-work-merge-candidate\n\n'

run_test "work merge lands after update and checks" \
  test_work_merge_lands_after_update_and_checks
run_test "work merge explicit post-merge review enqueues" \
  test_work_merge_explicit_post_merge_review_enqueues
run_test "work merge rejects conflicting post-merge-review options" \
  test_work_merge_rejects_conflicting_post_merge_review_options
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

run_test "legacy mock credential probes leave source unchanged" \
  test_legacy_mock_probes_leave_source_unchanged

run_test "legacy branch switch lands with fresh ordinary reviews" \
  'test_work_merge_legacy_branch_switch fresh'
run_test "legacy branch switch lands with carried ordinary reviews" \
  'test_work_merge_legacy_branch_switch carried'

run_test "landing rejects changed or missing effective coverage" \
  test_work_merge_rejects_changed_effective_coverage

run_test "public land recovers released 0.6 landing-pending history" \
  test_work_merge_recovers_released_landing_pending
run_test "public land reconciles an already-applied intent" \
  test_work_merge_reconciles_applied_intent

summarize_and_exit
