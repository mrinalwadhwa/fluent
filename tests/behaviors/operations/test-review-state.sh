#!/usr/bin/env bash
# test-review-state — Verify effective review state behavior.
#
# Drives the public factory CLI against temporary Git projects. Fake
# reviewer and author commands exercise review phases and completed run
# landing without importing Factory internals.

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
  TEST_DIR="$(mktemp -d -t factory-test-review-state-XXXXXX)"
  SOURCE_DIR="${TEST_DIR}/repo"
  BIN_DIR="${TEST_DIR}/bin"
  mkdir -p "$SOURCE_DIR" "$BIN_DIR"
  cd "$SOURCE_DIR"
  git init -q -b main
  git config user.email test@example.com
  git config user.name Test
  git config commit.gpgsign false
  printf 'base\n' > README.md
  git add README.md
  git commit -qm init
}

cleanup_project() {
  cd "$PROJECT_DIR"
  if [ -n "${TEST_DIR:-}" ] && [ -d "${SOURCE_DIR:-}/.git" ]; then
    git -C "$SOURCE_DIR" worktree list --porcelain 2>/dev/null |
      awk '/^worktree / {print $2}' |
      grep -v "^${SOURCE_DIR}$" |
      while read -r worktree; do
        git -C "$SOURCE_DIR" worktree remove --force "$worktree" 2>/dev/null || true
      done
  fi
  rm -rf "${TEST_DIR:-}"
}

create_run() {
  RUN_ID="$1"
  mkdir -p ".factory/runs/${RUN_ID}"
  printf 'Exercise review-state behavior.\n' > ".factory/runs/${RUN_ID}/brief.md"
  printf 'planned' > ".factory/runs/${RUN_ID}/status"
  printf '%s' "$RUN_ID" > .factory/active-run
}

write_fake_claude() {
  cat > "${BIN_DIR}/claude" <<'SH'
#!/usr/bin/env bash
set -euo pipefail

run_id="${FACTORY_TEST_RUN_ID}"
run_dir="${PWD}/.factory/runs/${run_id}"
args="$*"

if printf '%s' "$args" | grep -q 'Write your review to'; then
  mkdir -p "${run_dir}/reviews"
  for reviewer in architecture behaviors documentation skills tests; do
    printf 'Verdict: %s\n\nReview-state fixture.\n' \
      "${FACTORY_TEST_REVIEW_VERDICT}" \
      > "${run_dir}/reviews/review-${reviewer}.md"
  done
else
  if [ ! -f "${run_dir}/authored-once" ]; then
    printf 'change\n' > review-state-change.txt
    git add review-state-change.txt
    git commit -qm "Add review state fixture"
    printf 'done' > "${run_dir}/authored-once"
  fi
  printf 'complete' > "${run_dir}/status"
fi

printf '{"type":"result","subtype":"success","result":"done","session_id":"fixture"}\n'
SH
  chmod +x "${BIN_DIR}/claude"
}

write_dirty_limit_claude() {
  cat > "${BIN_DIR}/claude" <<'SH'
#!/usr/bin/env bash
set -euo pipefail

run_id="${FACTORY_TEST_RUN_ID}"
run_dir="${PWD}/.factory/runs/${run_id}"
args="$*"

if printf '%s' "$args" | grep -q 'Write your review to'; then
  mkdir -p "${run_dir}/reviews"
  for reviewer in architecture behaviors documentation skills tests; do
    printf 'Verdict: fail\n\nReview-state fixture.\n' \
      > "${run_dir}/reviews/review-${reviewer}.md"
  done
else
  count=0
  if [ -f "${run_dir}/author-count" ]; then
    count="$(cat "${run_dir}/author-count")"
  fi
  count=$((count + 1))
  printf '%s' "$count" > "${run_dir}/author-count"

  if [ ! -f "${run_dir}/authored-once" ]; then
    printf 'change\n' > review-state-change.txt
    git add review-state-change.txt
    git commit -qm "Add review state fixture"
    printf 'done' > "${run_dir}/authored-once"
  fi

  if [ "$count" -ge 11 ]; then
    printf 'needs-user' > "${run_dir}/status"
  else
    printf 'dirty\n' > dirty-review-limit.txt
    printf 'complete' > "${run_dir}/status"
  fi
fi

printf '{"type":"result","subtype":"success","result":"done","session_id":"fixture"}\n'
SH
  chmod +x "${BIN_DIR}/claude"
}

setup_complete_run_with_worktree() {
  RUN_ID="$1"
  local state="$2"
  local artifact_verdict="$3"

  git checkout -q -b "$RUN_ID"
  printf 'land change\n' > land-review-state.txt
  git add land-review-state.txt
  git commit -qm "Add land review state fixture"
  git checkout -q main

  WORKTREE="${TEST_DIR}/${RUN_ID}-wt"
  git worktree add -q "$WORKTREE" "$RUN_ID"

  mkdir -p ".factory/runs/${RUN_ID}/reviews"
  printf 'complete' > ".factory/runs/${RUN_ID}/status"
  printf 'Review-state land fixture.\n' > ".factory/runs/${RUN_ID}/brief.md"
  printf 'main' > ".factory/runs/${RUN_ID}/source-branch"
  printf '%s' "$WORKTREE" > ".factory/runs/${RUN_ID}/worktree"
  printf 'Verdict: %s\n' "$artifact_verdict" \
    > ".factory/runs/${RUN_ID}/reviews/review-behaviors.md"

  mkdir -p "${WORKTREE}/.factory/runs/${RUN_ID}/reviews"
  printf 'complete' > "${WORKTREE}/.factory/runs/${RUN_ID}/status"
  printf 'Verdict: %s\n' "$artifact_verdict" \
    > "${WORKTREE}/.factory/runs/${RUN_ID}/reviews/review-behaviors.md"
  cat > "${WORKTREE}/.factory/runs/${RUN_ID}/review-state.json" <<JSON
{
  "state": "${state}",
  "round": 3,
  "source": "review-limit",
  "max_rounds": 10,
  "reason": "Review round limit reached with clean worktree.",
  "verdicts": {
    "behaviors": "${artifact_verdict}"
  }
}
JSON
}

assert_json_value() {
  local file="$1"
  local filter="$2"
  local expected="$3"

  local actual
  actual="$(jq -r "$filter" "$file")"
  if [ "$actual" != "$expected" ]; then
    printf '    FAIL: expected %s to be %s, got %s\n' \
      "$filter" "$expected" "$actual"
    return 1
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

capture_dashboard() {
  local project_path="$1"
  local run_id="$2"
  local output_file
  output_file="$(mktemp -t factory-review-state-dashboard-XXXXXX)"

  (
    sleep 1
    printf 'q'
  ) | FACTORY_DASH_BIN="$FACTORY_BIN" \
      FACTORY_DASH_PROJECT="$project_path" \
      FACTORY_DASH_RUN="$run_id" \
      TERM=xterm \
      script -q "$output_file" sh -c 'stty rows 30 cols 120; "$FACTORY_DASH_BIN" dashboard --run-id "$FACTORY_DASH_RUN" "$FACTORY_DASH_PROJECT"' >/dev/null 2>&1 || true

  perl -pe 's/\e\[[0-9;?]*[ -\/]*[@-~]//g; s/\r/\n/g' "$output_file"
  rm -f "$output_file"
}


test_review_phase_writes_passed_state() {
  setup_project
  create_run review-state-pass
  write_fake_claude

  PATH="${BIN_DIR}:$PATH" \
    FACTORY_TEST_RUN_ID="$RUN_ID" \
    FACTORY_TEST_REVIEW_VERDICT=pass \
    "$FACTORY_BIN" run --no-sandbox --run-id "$RUN_ID" > /dev/null 2>&1

  WORKTREE="$(cat ".factory/runs/${RUN_ID}/worktree")"
  STATE_FILE="${WORKTREE}/.factory/runs/${RUN_ID}/review-state.json"

  RESULT=0
  test -f "$STATE_FILE" || {
    printf '    FAIL: review-state.json was not written\n'
    RESULT=1
  }
  assert_json_value "$STATE_FILE" '.state' 'passed' || RESULT=1
  assert_json_value "$STATE_FILE" '.source' 'reviewers' || RESULT=1
  jq -e '.round | type == "number"' "$STATE_FILE" > /dev/null || {
    printf '    FAIL: round is missing or not numeric\n'
    RESULT=1
  }
  jq -e '.verdicts.behaviors == "pass"' "$STATE_FILE" > /dev/null || {
    printf '    FAIL: per-reviewer verdicts do not include behaviors=pass\n'
    RESULT=1
  }

  cleanup_project
  return $RESULT
}

test_review_phase_writes_uncertain_state() {
  setup_project
  create_run review-state-uncertain
  printf 'review' > ".factory/runs/${RUN_ID}/mode"
  write_fake_claude

  PATH="${BIN_DIR}:$PATH" \
    FACTORY_TEST_RUN_ID="$RUN_ID" \
    FACTORY_TEST_REVIEW_VERDICT=uncertain \
    "$FACTORY_BIN" run --no-sandbox --run-id "$RUN_ID" > /dev/null 2>&1

  WORKTREE="$(cat ".factory/runs/${RUN_ID}/worktree")"
  STATE_FILE="${WORKTREE}/.factory/runs/${RUN_ID}/review-state.json"

  RESULT=0
  assert_json_value "$STATE_FILE" '.state' 'uncertain' || RESULT=1
  assert_json_value "$STATE_FILE" '.source' 'reviewers' || RESULT=1
  jq -e '.verdicts.behaviors == "uncertain"' "$STATE_FILE" > /dev/null || {
    printf '    FAIL: per-reviewer verdicts do not include behaviors=uncertain\n'
    RESULT=1
  }

  cleanup_project
  return $RESULT
}

test_review_phase_writes_failed_state() {
  setup_project
  create_run review-state-failed
  printf 'review' > ".factory/runs/${RUN_ID}/mode"
  write_fake_claude

  PATH="${BIN_DIR}:$PATH" \
    FACTORY_TEST_RUN_ID="$RUN_ID" \
    FACTORY_TEST_REVIEW_VERDICT=fail \
    "$FACTORY_BIN" run --no-sandbox --run-id "$RUN_ID" > /dev/null 2>&1

  WORKTREE="$(cat ".factory/runs/${RUN_ID}/worktree")"
  STATE_FILE="${WORKTREE}/.factory/runs/${RUN_ID}/review-state.json"

  RESULT=0
  assert_json_value "$STATE_FILE" '.state' 'failed' || RESULT=1
  assert_json_value "$STATE_FILE" '.source' 'reviewers' || RESULT=1
  jq -e '.verdicts.behaviors == "fail"' "$STATE_FILE" > /dev/null || {
    printf '    FAIL: per-reviewer verdicts do not include behaviors=fail\n'
    RESULT=1
  }

  cleanup_project
  return $RESULT
}

test_review_limit_writes_acceptance_state() {
  setup_project
  create_run review-state-limit
  write_fake_claude

  PATH="${BIN_DIR}:$PATH" \
    FACTORY_TEST_RUN_ID="$RUN_ID" \
    FACTORY_TEST_REVIEW_VERDICT=fail \
    "$FACTORY_BIN" run --no-sandbox --run-id "$RUN_ID" > /dev/null 2>&1

  WORKTREE="$(cat ".factory/runs/${RUN_ID}/worktree")"
  STATE_FILE="${WORKTREE}/.factory/runs/${RUN_ID}/review-state.json"

  RESULT=0
  assert_json_value "$STATE_FILE" '.state' 'accepted-review-limit' || RESULT=1
  assert_json_value "$STATE_FILE" '.source' 'review-limit' || RESULT=1
  assert_json_value "$STATE_FILE" '.max_rounds' '10' || RESULT=1
  jq -e '.reason | type == "string" and length > 0' "$STATE_FILE" > /dev/null || {
    printf '    FAIL: accepted-review-limit state does not include a reason\n'
    RESULT=1
  }
  assert_contains \
    "$(cat "${WORKTREE}/.factory/runs/${RUN_ID}/report.md")" \
    "accepted-review-limit" || RESULT=1

  cleanup_project
  return $RESULT
}

test_dirty_review_limit_does_not_write_acceptance() {
  setup_project
  create_run review-state-limit-dirty
  write_dirty_limit_claude

  PATH="${BIN_DIR}:$PATH" \
    FACTORY_TEST_RUN_ID="$RUN_ID" \
    "$FACTORY_BIN" run --no-sandbox --run-id "$RUN_ID" > /dev/null 2>&1

  WORKTREE="$(cat ".factory/runs/${RUN_ID}/worktree")"
  STATE_FILE="${WORKTREE}/.factory/runs/${RUN_ID}/review-state.json"

  RESULT=0
  if [ -f "$STATE_FILE" ] &&
    [ "$(jq -r '.state' "$STATE_FILE")" = "accepted-review-limit" ]; then
    printf '    FAIL: dirty review-limit run wrote accepted-review-limit\n'
    RESULT=1
  fi
  if [ "$(cat "${WORKTREE}/.factory/runs/${RUN_ID}/status")" != "needs-user" ]; then
    printf '    FAIL: dirty review-limit run did not require user cleanup\n'
    RESULT=1
  fi
  test -f "${WORKTREE}/dirty-review-limit.txt" || {
    printf '    FAIL: dirty worktree marker was not left for cleanup\n'
    RESULT=1
  }

  cleanup_project
  return $RESULT
}

test_land_accepts_passed_review_state_over_stale_artifact() {
  setup_project
  setup_complete_run_with_worktree review-state-land-passed passed fail

  set +e
  OUTPUT="$("$FACTORY_BIN" merge "$RUN_ID" 2>&1)"
  EXIT_CODE=$?
  set -e

  RESULT=0
  if [ "$EXIT_CODE" -ne 0 ]; then
    printf '    FAIL: land should accept passed review-state, got %d\n' \
      "$EXIT_CODE"
    printf '    Output:\n%s\n' "$OUTPUT"
    RESULT=1
  fi
  test -f land-review-state.txt || {
    printf '    FAIL: landed commit was not merged into main\n'
    RESULT=1
  }
  assert_json_value \
    ".factory/runs/${RUN_ID}/review-state.json" \
    '.state' \
    'passed' || RESULT=1
  jq -e '.verdicts.behaviors == "fail"' \
    ".factory/runs/${RUN_ID}/review-state.json" > /dev/null || {
    printf '    FAIL: landed run did not preserve review-state verdicts\n'
    RESULT=1
  }

  cleanup_project
  return $RESULT
}

test_land_accepts_review_state_over_stale_artifact() {
  setup_project
  setup_complete_run_with_worktree review-state-land accepted-review-limit fail

  set +e
  OUTPUT="$("$FACTORY_BIN" merge "$RUN_ID" 2>&1)"
  EXIT_CODE=$?
  set -e

  RESULT=0
  if [ "$EXIT_CODE" -ne 0 ]; then
    printf '    FAIL: land should accept accepted-review-limit review-state, got %d\n' \
      "$EXIT_CODE"
    printf '    Output:\n%s\n' "$OUTPUT"
    RESULT=1
  fi
  test -f land-review-state.txt || {
    printf '    FAIL: landed commit was not merged into main\n'
    RESULT=1
  }
  assert_json_value \
    ".factory/runs/${RUN_ID}/review-state.json" \
    '.state' \
    'accepted-review-limit' || RESULT=1
  jq -e '.verdicts.behaviors == "fail"' \
    ".factory/runs/${RUN_ID}/review-state.json" > /dev/null || {
    printf '    FAIL: landed run did not preserve review-state verdicts\n'
    RESULT=1
  }

  cleanup_project
  return $RESULT
}

test_land_rejects_failed_review_state_over_pass_artifact() {
  setup_project
  setup_complete_run_with_worktree review-state-land-fail failed pass

  set +e
  OUTPUT="$("$FACTORY_BIN" merge "$RUN_ID" 2>&1)"
  EXIT_CODE=$?
  set -e

  RESULT=0
  if [ "$EXIT_CODE" -eq 0 ]; then
    printf '    FAIL: land should reject failed review-state\n'
    RESULT=1
  fi

  cleanup_project
  return $RESULT
}

test_land_rejects_uncertain_review_state_over_pass_artifact() {
  setup_project
  setup_complete_run_with_worktree review-state-land-uncertain uncertain pass

  set +e
  OUTPUT="$("$FACTORY_BIN" merge "$RUN_ID" 2>&1)"
  EXIT_CODE=$?
  set -e

  RESULT=0
  if [ "$EXIT_CODE" -eq 0 ]; then
    printf '    FAIL: land should reject uncertain review-state\n'
    RESULT=1
  fi

  cleanup_project
  return $RESULT
}

test_summary_uses_recorded_review_state() {
  setup_project
  mkdir -p .factory/runs/review-state-summary/reviews
  printf 'review-state-summary' > .factory/active-run
  printf 'complete' > .factory/runs/review-state-summary/status
  printf 'Review state summary fixture.\n' \
    > .factory/runs/review-state-summary/brief.md
  printf 'Verdict: pass\n' \
    > .factory/runs/review-state-summary/reviews/review-behaviors.md
  cat > .factory/runs/review-state-summary/review-state.json <<'JSON'
{
  "state": "failed",
  "round": 2,
  "source": "reviewers",
  "verdicts": {
    "behaviors": "fail"
  }
}
JSON

  OUTPUT="$("$FACTORY_BIN" summary --run-id review-state-summary 2>&1)"

  RESULT=0
  assert_contains "$OUTPUT" "Reviewers: failed" || RESULT=1
  assert_contains "$OUTPUT" "behaviors: fail" || RESULT=1
  assert_not_contains "$OUTPUT" "behaviors: pass" || RESULT=1

  cleanup_project
  return $RESULT
}

test_dashboard_uses_recorded_review_state() {
  setup_project
  mkdir -p .factory/runs/review-state-dashboard/reviews
  printf 'complete' > .factory/runs/review-state-dashboard/status
  printf 'Review state dashboard fixture.\n' \
    > .factory/runs/review-state-dashboard/brief.md
  printf 'Verdict: fail\n' \
    > .factory/runs/review-state-dashboard/reviews/review-behaviors.md
  cat > .factory/runs/review-state-dashboard/review-state.json <<'JSON'
{
  "state": "accepted-review-limit",
  "round": 11,
  "source": "review-limit",
  "verdicts": {
    "behaviors": "fail"
  },
  "max_rounds": 10,
  "reason": "Review round limit reached with a clean worktree."
}
JSON

  OUTPUT="$(capture_dashboard "$SOURCE_DIR" review-state-dashboard)"

  RESULT=0
  assert_contains "$OUTPUT" "accepted-review-limit" || RESULT=1
  assert_contains "$OUTPUT" "review-limit" || RESULT=1

  cleanup_project
  return $RESULT
}

printf 'test-review-state\n\n'

run_test "review phase writes passed review state" \
  test_review_phase_writes_passed_state
run_test "review phase writes uncertain review state" \
  test_review_phase_writes_uncertain_state
run_test "review phase writes failed review state" \
  test_review_phase_writes_failed_state
run_test "review-limit clean run writes acceptance state" \
  test_review_limit_writes_acceptance_state
run_test "dirty review-limit run does not write acceptance" \
  test_dirty_review_limit_does_not_write_acceptance
run_test "land accepts passed review state over stale artifact" \
  test_land_accepts_passed_review_state_over_stale_artifact
run_test "land accepts review state over stale artifact" \
  test_land_accepts_review_state_over_stale_artifact
run_test "land rejects failed review state over pass artifact" \
  test_land_rejects_failed_review_state_over_pass_artifact
run_test "land rejects uncertain review state over pass artifact" \
  test_land_rejects_uncertain_review_state_over_pass_artifact
run_test "summary uses recorded review state" \
  test_summary_uses_recorded_review_state
run_test "dashboard uses recorded review state" \
  test_dashboard_uses_recorded_review_state

summarize_and_exit
