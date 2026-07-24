#!/usr/bin/env bash
# test-fargate-entrypoint-post-merge-review — Verify the Fargate entrypoint
# translates only an affirmative post-merge review request into the inner land's
# --post-merge-review, and carries no post-merge-review flag when the request is
# absent.

set -euo pipefail

PROJECT_DIR="$(cd "$(dirname "$0")/../../.." && pwd)"
ENTRYPOINT="${PROJECT_DIR}/infrastructure/run/entrypoint.sh"
PASS=0
FAIL=0
ERRORS=""

run_test() {
  local name="$1"
  printf '  %s ... ' "$name"
  if ( eval "$2" ) 2>&1; then
    printf 'PASS\n'
    PASS=$((PASS + 1))
  else
    printf '\n'
    FAIL=$((FAIL + 1))
    ERRORS="${ERRORS}\n  - ${name}"
  fi
}

setup_entrypoint_test() {
  TEST_DIR="$(mktemp -d -t fluent-pmr-entrypoint-XXXXXX)"

  MOCK_BIN="${TEST_DIR}/bin"
  WORKTREES="${TEST_DIR}/worktrees"
  mkdir -p "$MOCK_BIN" "$WORKTREES"

  cat > "$MOCK_BIN/fluent" <<'FLUENT'
#!/usr/bin/env bash
set -euo pipefail
{
  printf 'fluent-bin=%s\n' "$0"
  printf '%s\n' "$@"
} > "$MOCK_FLUENT_ARGS"
FLUENT
  chmod +x "$MOCK_BIN/fluent"

  cat > "$MOCK_BIN/aws" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
if [ "${1:-}" = "s3" ] && [ "${2:-}" = "cp" ]; then
  shift 2
  while [ $# -gt 0 ]; do
    case "$1" in
      --region) shift 2 ;;
      --no-progress) shift ;;
      *) break ;;
    esac
  done
  src="${1:-}"
  dst="${2:-}"
  if [[ "$src" == s3://* ]] && [[ "$dst" != s3://* ]]; then
    cp "$MOCK_WORKSPACE_IN" "$dst"
  elif [[ "$dst" == s3://* ]] && [[ "$src" != s3://* ]]; then
    cp "$src" "$MOCK_WORKSPACE_OUT"
  fi
  exit 0
fi
exit 1
SH
  chmod +x "$MOCK_BIN/aws"

  local workspace="${TEST_DIR}/workspace-src/testproject"
  mkdir -p "$workspace"
  printf 'test\n' > "$workspace/README.md"
  MOCK_WORKSPACE_IN="${TEST_DIR}/workspace-in.tar"
  MOCK_WORKSPACE_OUT="${TEST_DIR}/workspace-out.tar"
  tar cf "$MOCK_WORKSPACE_IN" -C "${TEST_DIR}/workspace-src" testproject
  MOCK_FLUENT_ARGS="${TEST_DIR}/fluent-args"
}

cleanup_entrypoint_test() {
  rm -rf "$TEST_DIR"
}

# Run the entrypoint in Work Merge mode. Extra `env` assignments are appended.
run_merge_entrypoint() {
  HOME="${TEST_DIR}/fakehome" \
  PATH="${MOCK_BIN}:${PATH}" \
  FLUENT_WORKTREES_ROOT="$WORKTREES" \
  FLUENT_CODER="claude" \
  CLAUDE_CODE_OAUTH_TOKEN="test-token" \
  FLUENT_WORK_ITEM_ID="w1" \
  FLUENT_WORK_MERGE_CANDIDATE_ID="w1-attempt-1-merge-candidate" \
  FLUENT_PROJECT_NAME="testproject" \
  FLUENT_S3_BUCKET="bucket" \
  FLUENT_REGION="us-west-1" \
  FLUENT_BIN="$MOCK_BIN/fluent" \
  MOCK_WORKSPACE_IN="$MOCK_WORKSPACE_IN" \
  MOCK_WORKSPACE_OUT="$MOCK_WORKSPACE_OUT" \
  MOCK_FLUENT_ARGS="$MOCK_FLUENT_ARGS" \
  "$@" \
    bash "$ENTRYPOINT"
}

test_positive_request_reaches_inner_land() {
  setup_entrypoint_test
  mkdir -p "${TEST_DIR}/fakehome"

  run_merge_entrypoint env FLUENT_POST_MERGE_REVIEW="1"

  RESULT=0
  if ! grep -q -- '--post-merge-review' "$MOCK_FLUENT_ARGS"; then
    printf '    FAIL: FLUENT_POST_MERGE_REVIEW=1 did not pass --post-merge-review\n'
    RESULT=1
  fi
  if grep -q -- '--no-post-merge-review' "$MOCK_FLUENT_ARGS"; then
    printf '    FAIL: a positive request also passed the negative spelling\n'
    RESULT=1
  fi

  cleanup_entrypoint_test
  return $RESULT
}

test_absent_request_carries_no_post_merge_review_flag() {
  setup_entrypoint_test
  mkdir -p "${TEST_DIR}/fakehome"

  run_merge_entrypoint

  RESULT=0
  if grep -q -- '--post-merge-review' "$MOCK_FLUENT_ARGS"; then
    printf '    FAIL: an absent request still passed --post-merge-review\n'
    RESULT=1
  fi
  if grep -q -- '--no-post-merge-review' "$MOCK_FLUENT_ARGS"; then
    printf '    FAIL: an absent request passed --no-post-merge-review\n'
    RESULT=1
  fi

  cleanup_entrypoint_test
  return $RESULT
}

printf 'test-fargate-entrypoint-post-merge-review\n\n'

run_test "positive request reaches inner land" \
  test_positive_request_reaches_inner_land
run_test "absent request carries no post-merge-review flag" \
  test_absent_request_carries_no_post_merge_review_flag

printf '\n  %d passed, %d failed\n' "$PASS" "$FAIL"

if [ "$FAIL" -gt 0 ]; then
  printf '\n  Failures:%b\n' "$ERRORS"
  exit 1
fi
