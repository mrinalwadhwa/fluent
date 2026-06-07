#!/usr/bin/env bash
# test-fargate-launch — Verify Fargate launch behavior through the CLI.

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

assert_file_contains() {
  local file="$1"
  local expected="$2"
  if ! grep -Fq -- "$expected" "$file"; then
    printf '    FAIL: %s does not contain %s\n' "$file" "$expected"
    return 1
  fi
}

setup_test_project() {
  TEST_DIR="$(mktemp -d -t factory-fargate-launch-XXXXXX)"
  mkdir -p "${TEST_DIR}/project" "${TEST_DIR}/home/.config/factory" "${TEST_DIR}/bin"
  cd "${TEST_DIR}/project"
  git init -b main > /dev/null 2>&1
  git config commit.gpgsign false
  git config user.email "test@test"
  git config user.name "test"
  printf 'test\n' > README.md
  git add . && git commit -m "init" > /dev/null 2>&1

  mkdir -p .factory/runs/run-fg-launch
  printf 'Launch Fargate test\n' > .factory/runs/run-fg-launch/brief.md
  printf 'planned' > .factory/runs/run-fg-launch/status
  printf 'run-fg-launch' > .factory/active-run

  cat > "${TEST_DIR}/home/.config/factory/fargate.env" <<'EOF'
FACTORY_CLUSTER=cluster-arn
FACTORY_RUN_TASK=task-def
FACTORY_S3_BUCKET=bucket
FACTORY_SUBNETS=subnet-a,subnet-b
FACTORY_SECURITY_GROUP=sg-123
FACTORY_REGION=us-west-2
EOF

  cat > "${TEST_DIR}/bin/aws" <<'SH'
#!/usr/bin/env bash
set -euo pipefail

printf '%s\n' "$*" >> "$AWS_LOG"

case "$1 $2" in
  "configure export-credentials")
    printf 'AWS_ACCESS_KEY_ID=mock\n'
    printf 'AWS_SECRET_ACCESS_KEY=mock\n'
    printf 'AWS_SESSION_TOKEN=mock\n'
    ;;
  "configure get")
    printf 'us-west-2\n'
    ;;
  "s3 cp")
    if [ "$6" != "s3://bucket/runs/run-fg-launch/workspace-in.tar" ]; then
      printf 'unexpected upload target: %s\n' "$6" >&2
      exit 1
    fi
    cat > "$UPLOADED_WORKSPACE"
    ;;
  "ecs run-task")
    printf 'arn:aws:ecs:us-west-2:123:task/cluster/task-abc\n'
    ;;
  *)
    printf 'unexpected aws command: %s\n' "$*" >&2
    exit 1
    ;;
esac
SH
  chmod +x "${TEST_DIR}/bin/aws"
}

cleanup_test_project() {
  cd /
  if [ -d "${TEST_DIR}/project/.git" ]; then
    git -C "${TEST_DIR}/project" worktree list --porcelain 2>/dev/null | \
      grep '^worktree ' | awk '{print $2}' | \
      grep -v "${TEST_DIR}/project" | while read -r wt; do
      git -C "${TEST_DIR}/project" worktree remove --force "$wt" 2>/dev/null || true
    done || true
  fi
  rm -rf "$TEST_DIR"
}

test_fargate_launch_uploads_and_records_handle() {
  setup_test_project

  RESULT=0
  AWS_LOG="${TEST_DIR}/aws.log"
  UPLOADED_WORKSPACE="${TEST_DIR}/workspace-in.tar"
  export AWS_LOG UPLOADED_WORKSPACE

  PATH="${TEST_DIR}/bin:${PATH}" \
  HOME="${TEST_DIR}/home" \
  CLAUDE_CODE_OAUTH_TOKEN="mock-claude-token" \
    "$FACTORY_BIN" run --runtime fargate --run-id run-fg-launch \
      > "${TEST_DIR}/factory.out" 2>&1 || RESULT=1

  assert_file_contains "$AWS_LOG" "s3 cp --region us-west-2 - s3://bucket/runs/run-fg-launch/workspace-in.tar" || RESULT=1
  assert_file_contains "$AWS_LOG" "ecs run-task --region us-west-2" || RESULT=1
  assert_file_contains "$AWS_LOG" "--cluster cluster-arn" || RESULT=1
  assert_file_contains "$AWS_LOG" "--task-definition task-def" || RESULT=1
  assert_file_contains "$AWS_LOG" "FACTORY_RUN_ID" || RESULT=1
  assert_file_contains "$AWS_LOG" "run-fg-launch" || RESULT=1

  if [ ! -s "${TEST_DIR}/workspace-in.tar" ]; then
    printf '    FAIL: workspace upload tar was not written\n'
    RESULT=1
  fi

  if [ "$(cat .factory/runs/run-fg-launch/runtime)" != "fargate" ]; then
    printf '    FAIL: runtime file was not fargate\n'
    RESULT=1
  fi
  if [ "$(cat .factory/runs/run-fg-launch/handle)" != "arn:aws:ecs:us-west-2:123:task/cluster/task-abc" ]; then
    printf '    FAIL: handle file did not record ECS task handle\n'
    RESULT=1
  fi

  cleanup_test_project
  return $RESULT
}

test_fargate_launch_stops_when_archive_fails() {
  setup_test_project

  RESULT=0
  AWS_LOG="${TEST_DIR}/aws.log"
  UPLOADED_WORKSPACE="${TEST_DIR}/workspace-in.tar"
  export AWS_LOG UPLOADED_WORKSPACE

  cat > "${TEST_DIR}/bin/tar" <<'SH'
#!/usr/bin/env bash
set -euo pipefail

printf '%s\n' "$*" >> "$TAR_LOG"
exit 42
SH
  chmod +x "${TEST_DIR}/bin/tar"
  TAR_LOG="${TEST_DIR}/tar.log"
  export TAR_LOG

  PATH="${TEST_DIR}/bin:${PATH}" \
  HOME="${TEST_DIR}/home" \
  CLAUDE_CODE_OAUTH_TOKEN="mock-claude-token" \
    "$FACTORY_BIN" run --runtime fargate --run-id run-fg-launch \
      > "${TEST_DIR}/factory.out" 2>&1 && RESULT=1

  assert_file_contains "${TEST_DIR}/factory.out" "Failed to archive workspace for upload" || RESULT=1
  assert_file_contains "$AWS_LOG" "s3 cp --region us-west-2 - s3://bucket/runs/run-fg-launch/workspace-in.tar" || RESULT=1

  if grep -Fq "ecs run-task" "$AWS_LOG"; then
    printf '    FAIL: ECS task started after archive failure\n'
    RESULT=1
  fi
  if [ -e .factory/runs/run-fg-launch/runtime ]; then
    printf '    FAIL: runtime metadata was written after archive failure\n'
    RESULT=1
  fi
  if [ -e .factory/runs/run-fg-launch/handle ]; then
    printf '    FAIL: handle metadata was written after archive failure\n'
    RESULT=1
  fi

  cleanup_test_project
  return $RESULT
}

if [ ! -x "$FACTORY_BIN" ]; then
  printf 'ERROR: factory binary not found at %s\n' "$FACTORY_BIN"
  printf 'Run "cargo build" first.\n'
  exit 1
fi

printf 'test-fargate-launch\n\n'

run_test "fargate launch uploads workspace and records ECS handle" test_fargate_launch_uploads_and_records_handle
run_test "fargate launch stops when archive fails" test_fargate_launch_stops_when_archive_fails

printf '\n  %d passed, %d failed\n' "$PASS" "$FAIL"

if [ "$FAIL" -gt 0 ]; then
  printf '\n  Failures:%b\n' "$ERRORS"
  exit 1
fi
