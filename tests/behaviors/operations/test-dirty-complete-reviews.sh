#!/usr/bin/env bash
# test-dirty-complete-reviews — Verify completed dirty runs enter review.
#
# Drives the real factory CLI against a temporary Git project. A fake
# claude executable acts as the user-facing agent: the author marks the
# run complete and optionally leaves worktree changes; reviewers write
# passing review artifacts.
#
# Covers:
#   - Complete with unstaged tracked changes runs reviews before final completion
#   - Complete with staged changes runs reviews before final completion
#   - Complete with untracked non-ignored files runs reviews before final completion
#   - Complete with no code changes skips run-scoped reviews

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
  TEST_DIR="$(mktemp -d -t factory-test-dirty-complete-XXXXXX)"
  SOURCE_DIR="${TEST_DIR}/repo"
  BIN_DIR="${TEST_DIR}/bin"
  RUN_ID="dirty-complete"

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

  printf 'Exercise dirty complete handling.\n' > ".factory/runs/${RUN_ID}/brief.md"
  printf 'planned' > ".factory/runs/${RUN_ID}/status"

  cat > "${BIN_DIR}/claude" <<'SH'
#!/usr/bin/env bash
set -euo pipefail

run_id="dirty-complete"
run_dir="${PWD}/.factory/runs/${run_id}"
args="$*"

if printf '%s' "$args" | grep -q 'Write your review to'; then
  mkdir -p "${run_dir}/reviews"
  for reviewer in architecture behaviors documentation skills tests; do
    printf 'Verdict: pass\n\nLooks good.\n' > "${run_dir}/reviews/review-${reviewer}.md"
  done
elif [ -f "${run_dir}/status" ]; then
  if [ -f "${run_dir}/handoff.md" ]; then
    git add -A
    git commit -qm "Commit dirty work"
  else
    case "${FACTORY_DIRTY_COMPLETE_SCENARIO}" in
      tracked)
        printf 'changed\n' >> tracked.txt
        ;;
      staged)
        printf 'changed\n' >> tracked.txt
        git add tracked.txt
        ;;
      untracked)
        printf 'new\n' > new-file.txt
        ;;
      clean)
        ;;
      *)
        printf 'unknown scenario: %s\n' "${FACTORY_DIRTY_COMPLETE_SCENARIO}" >&2
        exit 1
        ;;
    esac
  fi
  printf 'complete' > "${run_dir}/status"
else
  printf 'missing run status\n' >&2
  exit 1
fi

printf '{"type":"result"}\n'
SH
  chmod +x "${BIN_DIR}/claude"
}

cleanup_project() {
  cd "$PROJECT_DIR"
  rm -rf "$TEST_DIR"
}

assert_contains() {
  if ! printf '%s' "$1" | grep -q "$2"; then
    printf '    FAIL: output does not contain "%s"\n' "$2"
    return 1
  fi
}

assert_not_contains() {
  if printf '%s' "$1" | grep -q "$2"; then
    printf '    FAIL: output unexpectedly contains "%s"\n' "$2"
    return 1
  fi
}

run_factory_scenario() {
  scenario="$1"
  setup_project
  OUTPUT="$(
    PATH="${BIN_DIR}:$PATH" \
    FACTORY_DIRTY_COMPLETE_SCENARIO="$scenario" \
      "$FACTORY_BIN" run --no-sandbox --run-id "$RUN_ID" 2>&1
  )"
  WORKTREE="$(cat "${SOURCE_DIR}/.factory/runs/${RUN_ID}/worktree")"
}

test_tracked_dirty_complete_runs_reviews() {
  run_factory_scenario tracked

  RESULT=0
  assert_contains "$OUTPUT" "=== Review phase" || RESULT=1
  assert_contains "$OUTPUT" "uncommitted changes remain" || RESULT=1
  test -z "$(git -C "$WORKTREE" status --porcelain --untracked-files=normal)" || {
    printf '    FAIL: worktree was not clean after completion\n'
    RESULT=1
  }
  test -f "${WORKTREE}/.factory/runs/${RUN_ID}/reviews/review-behaviors.md" || {
    printf '    FAIL: review artifact was not written\n'
    RESULT=1
  }

  cleanup_project
  return $RESULT
}

test_staged_dirty_complete_runs_reviews() {
  run_factory_scenario staged

  RESULT=0
  assert_contains "$OUTPUT" "=== Review phase" || RESULT=1
  assert_contains "$OUTPUT" "uncommitted changes remain" || RESULT=1
  test -z "$(git -C "$WORKTREE" status --porcelain --untracked-files=normal)" || {
    printf '    FAIL: worktree was not clean after completion\n'
    RESULT=1
  }
  test -f "${WORKTREE}/.factory/runs/${RUN_ID}/reviews/review-tests.md" || {
    printf '    FAIL: review artifact was not written\n'
    RESULT=1
  }

  cleanup_project
  return $RESULT
}

test_untracked_dirty_complete_runs_reviews() {
  run_factory_scenario untracked

  RESULT=0
  assert_contains "$OUTPUT" "=== Review phase" || RESULT=1
  assert_contains "$OUTPUT" "uncommitted changes remain" || RESULT=1
  test -z "$(git -C "$WORKTREE" status --porcelain --untracked-files=normal)" || {
    printf '    FAIL: worktree was not clean after completion\n'
    RESULT=1
  }
  test -f "${WORKTREE}/.factory/runs/${RUN_ID}/reviews/review-documentation.md" || {
    printf '    FAIL: review artifact was not written\n'
    RESULT=1
  }

  cleanup_project
  return $RESULT
}

test_clean_complete_skips_reviews() {
  run_factory_scenario clean

  RESULT=0
  assert_not_contains "$OUTPUT" "=== Review phase" || RESULT=1
  test ! -d "${WORKTREE}/.factory/runs/${RUN_ID}/reviews" || {
    printf '    FAIL: reviews directory exists for clean run\n'
    RESULT=1
  }

  cleanup_project
  return $RESULT
}

printf 'test-dirty-complete-reviews\n\n'

run_test "tracked dirty complete runs reviews" test_tracked_dirty_complete_runs_reviews
run_test "staged dirty complete runs reviews" test_staged_dirty_complete_runs_reviews
run_test "untracked dirty complete runs reviews" test_untracked_dirty_complete_runs_reviews
run_test "clean complete skips reviews" test_clean_complete_skips_reviews

summarize_and_exit
