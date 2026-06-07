#!/usr/bin/env bash
# entrypoint.sh — Single-container Fargate entrypoint for factory runs.
#
# Pull workspace from S3, run the Rust session loop, upload completed
# workspace to S3.
#
# Environment variables (passed as task overrides):
#   FACTORY_RUN_ID         — the run identifier
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

[ -n "${FACTORY_RUN_ID:-}" ] || die "FACTORY_RUN_ID not set"
[ -n "${FACTORY_S3_BUCKET:-}" ] || die "FACTORY_S3_BUCKET not set"
[ -n "${CLAUDE_CODE_OAUTH_TOKEN:-}" ] || die "No Claude auth token"

# --------------------------------------------------------------------------
# Pull workspace from S3
# --------------------------------------------------------------------------

printf 'factory-run: pulling workspace from S3...\n'

WAIT=0
while true; do
  if aws s3 cp \
    --region "${FACTORY_REGION:-us-west-1}" \
    "s3://${FACTORY_S3_BUCKET}/runs/${FACTORY_RUN_ID}/workspace-in.tar" \
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

RUN_DIR="${WORKSPACE}/.factory/runs/${FACTORY_RUN_ID}"
[ -d "$RUN_DIR" ] || die "Run directory not found: $RUN_DIR"

# Write active-run pointer
printf '%s' "$FACTORY_RUN_ID" > "${WORKSPACE}/.factory/active-run"

# Preserve Fargate as the durable runtime identity while the container reuses
# the local Rust session loop internally.
printf 'fargate' > "${RUN_DIR}/runtime"
TASK_HANDLE="$(resolve_task_handle)"
if [ -n "$TASK_HANDLE" ]; then
  printf '%s' "$TASK_HANDLE" > "${RUN_DIR}/handle"
else
  printf 'factory-run: warning: task handle unavailable from ECS metadata\n' >&2
fi

# --------------------------------------------------------------------------
# Run the Rust session loop
# --------------------------------------------------------------------------

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

"$FACTORY_BIN" run \
  --runtime local \
  --no-sandbox \
  --in-place \
  --preserve-run-metadata \
  --coder claude \
  --run-id "$FACTORY_RUN_ID"

# --------------------------------------------------------------------------
# Upload workspace to S3
# --------------------------------------------------------------------------

printf 'factory-run: uploading workspace to S3...\n'
tar cf - -C "$WORKSPACE" . | \
  aws s3 cp \
    --region "${FACTORY_REGION:-us-west-1}" \
    - "s3://${FACTORY_S3_BUCKET}/runs/${FACTORY_RUN_ID}/workspace.tar"

printf 'factory-run: uploaded to s3://%s/runs/%s/workspace.tar\n' \
  "$FACTORY_S3_BUCKET" "$FACTORY_RUN_ID"
printf 'factory-run: done\n'
