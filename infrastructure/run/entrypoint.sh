#!/usr/bin/env bash
# entrypoint.sh — Single-container Fargate entrypoint for factory runs
# and Work Item Attempts.
#
# Two modes, selected by which env vars are set:
#
# Legacy run mode (FACTORY_RUN_ID set):
#   Pull workspace from S3, run the legacy Rust session loop, upload
#   completed workspace to S3.
#
# Work model mode (FACTORY_WORK_ITEM_ID + FACTORY_WORK_ATTEMPT_ID set):
#   Pull workspace + .factory/work state from S3, run
#   `factory work attempt run`, upload everything back to S3.
#
# Environment variables (passed as task overrides):
#   FACTORY_RUN_ID         — the legacy run identifier (mode A)
#   FACTORY_WORK_ITEM_ID   — the Work Item ID (mode B)
#   FACTORY_WORK_ATTEMPT_ID — the Attempt ID (mode B)
#   FACTORY_S3_BUCKET      — S3 bucket for workspace transfer
#   FACTORY_REGION         — AWS region
#   CLAUDE_CODE_OAUTH_TOKEN — Claude auth token

set -euo pipefail

WORKSPACE="${WORKSPACE:-/workspace}"

die() { printf 'factory-run: %s\n' "$1" >&2; exit 1; }

resolve_task_handle() {
  if [ -n "${FACTORY_TASK_ARN:-}" ]; then
    printf '%s' "$FACTORY_TASK_ARN"
    return 0
  fi

  if [ -n "${ECS_CONTAINER_METADATA_URI_V4:-}" ] &&
    command -v curl >/dev/null 2>&1 &&
    command -v jq >/dev/null 2>&1; then
    local task_json
    if task_json="$(curl -fsS "${ECS_CONTAINER_METADATA_URI_V4}/task" 2>/dev/null)"; then
      printf '%s' "$task_json" | jq -r '.TaskARN // empty'
    fi
    return 0
  fi

  printf ''
}

[ -n "${FACTORY_S3_BUCKET:-}" ] || die "FACTORY_S3_BUCKET not set"
[ -n "${CLAUDE_CODE_OAUTH_TOKEN:-}" ] || die "No Claude auth token"

MODE=""
if [ -n "${FACTORY_WORK_ITEM_ID:-}" ] && [ -n "${FACTORY_WORK_MERGE_CANDIDATE_ID:-}" ]; then
  MODE="work-merge"
elif [ -n "${FACTORY_WORK_ITEM_ID:-}" ] && [ -n "${FACTORY_WORK_ATTEMPT_ID:-}" ]; then
  MODE="work-attempt"
elif [ -n "${FACTORY_RUN_ID:-}" ]; then
  MODE="run"
else
  die "Set FACTORY_RUN_ID (legacy mode), FACTORY_WORK_ITEM_ID + FACTORY_WORK_ATTEMPT_ID (Work attempt mode), or FACTORY_WORK_ITEM_ID + FACTORY_WORK_MERGE_CANDIDATE_ID (Work merge mode)"
fi

S3_REGION="${FACTORY_REGION:-us-west-1}"

case "$MODE" in
  run)
    S3_IN_KEY="runs/${FACTORY_RUN_ID}/workspace-in.tar"
    S3_OUT_KEY="runs/${FACTORY_RUN_ID}/workspace.tar"
    ;;
  work-attempt)
    S3_IN_KEY="work/${FACTORY_WORK_ITEM_ID}/${FACTORY_WORK_ATTEMPT_ID}/workspace-in.tar"
    S3_OUT_KEY="work/${FACTORY_WORK_ITEM_ID}/${FACTORY_WORK_ATTEMPT_ID}/workspace-out.tar"
    ;;
  work-merge)
    S3_IN_KEY="work-merge/${FACTORY_WORK_ITEM_ID}/${FACTORY_WORK_MERGE_CANDIDATE_ID}/workspace-in.tar"
    S3_OUT_KEY="work-merge/${FACTORY_WORK_ITEM_ID}/${FACTORY_WORK_MERGE_CANDIDATE_ID}/workspace-out.tar"
    ;;
esac

# --------------------------------------------------------------------------
# Pull workspace from S3
# --------------------------------------------------------------------------

printf 'factory-run: pulling workspace from s3://%s/%s\n' "$FACTORY_S3_BUCKET" "$S3_IN_KEY"

WAIT=0
while true; do
  if aws s3 cp \
    --region "$S3_REGION" \
    "s3://${FACTORY_S3_BUCKET}/${S3_IN_KEY}" \
    - 2>/dev/null | tar xf - -C "$WORKSPACE"; then
    printf 'factory-run: workspace received\n'
    break
  fi
  sleep 5
  WAIT=$((WAIT + 1))
  if [ "$WAIT" -gt 60 ]; then
    die "Timed out waiting for workspace in S3 (5 minutes)"
  fi
done

cd "$WORKSPACE"

if [ -n "${FACTORY_BIN:-}" ]; then
  [ -x "$FACTORY_BIN" ] || die "FACTORY_BIN is not executable: $FACTORY_BIN"
elif [ -x "/usr/local/bin/factory" ]; then
  FACTORY_BIN="/usr/local/bin/factory"
elif [ -x "${WORKSPACE}/target/release/factory" ]; then
  FACTORY_BIN="${WORKSPACE}/target/release/factory"
elif command -v factory >/dev/null 2>&1; then
  FACTORY_BIN="$(command -v factory)"
else
  die "no factory binary available"
fi

case "$MODE" in
  run)
    RUN_DIR="${WORKSPACE}/.factory/runs/${FACTORY_RUN_ID}"
    [ -d "$RUN_DIR" ] || die "Run directory not found: $RUN_DIR"
    printf '%s' "$FACTORY_RUN_ID" > "${WORKSPACE}/.factory/active-run"
    printf 'fargate' > "${RUN_DIR}/runtime"
    TASK_HANDLE="$(resolve_task_handle)"
    if [ -n "$TASK_HANDLE" ]; then
      printf '%s' "$TASK_HANDLE" > "${RUN_DIR}/handle"
    fi

    "$FACTORY_BIN" run \
      --runtime local \
      --no-sandbox \
      --in-place \
      --preserve-run-metadata \
      --coder claude \
      --run-id "$FACTORY_RUN_ID"
    ;;
  work-attempt)
    printf 'factory-run: running factory work attempt run %s %s\n' \
      "$FACTORY_WORK_ITEM_ID" "$FACTORY_WORK_ATTEMPT_ID"

    "$FACTORY_BIN" work attempt run \
      --no-sandbox \
      --coder claude \
      "$FACTORY_WORK_ITEM_ID" \
      "$FACTORY_WORK_ATTEMPT_ID"
    ;;
  work-merge)
    printf 'factory-run: running factory work merge %s %s\n' \
      "$FACTORY_WORK_ITEM_ID" "$FACTORY_WORK_MERGE_CANDIDATE_ID"

    "$FACTORY_BIN" work merge \
      --no-sandbox \
      --coder claude \
      "$FACTORY_WORK_ITEM_ID" \
      "$FACTORY_WORK_MERGE_CANDIDATE_ID"
    ;;
esac

# --------------------------------------------------------------------------
# Upload workspace to S3
# --------------------------------------------------------------------------

printf 'factory-run: uploading workspace to s3://%s/%s\n' "$FACTORY_S3_BUCKET" "$S3_OUT_KEY"
tar cf - -C "$WORKSPACE" . | \
  aws s3 cp \
    --region "$S3_REGION" \
    - "s3://${FACTORY_S3_BUCKET}/${S3_OUT_KEY}"

printf 'factory-run: uploaded to s3://%s/%s\n' "$FACTORY_S3_BUCKET" "$S3_OUT_KEY"
printf 'factory-run: done\n'
