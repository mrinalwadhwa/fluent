#!/usr/bin/env bash
# entrypoint.sh — Single-container Fargate entrypoint for factory runs.
#
# Pull workspace from S3, source the factory script for shared functions,
# run the session loop, upload completed workspace to S3.
#
# Environment variables (passed as task overrides):
#   FACTORY_RUN_ID         — the run identifier
#   FACTORY_S3_BUCKET      — S3 bucket for workspace transfer
#   FACTORY_REGION         — AWS region
#   CLAUDE_CODE_OAUTH_TOKEN — Claude auth token

set -euo pipefail

WORKSPACE=/workspace

die() { printf 'factory-run: %s\n' "$1" >&2; exit 1; }

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

# --------------------------------------------------------------------------
# Source factory script for shared functions
# --------------------------------------------------------------------------

FACTORY_SCRIPT="${WORKSPACE}/scripts/factory"
[ -f "$FACTORY_SCRIPT" ] || die "Factory script not found: $FACTORY_SCRIPT"

PROMPTS_DIR="${WORKSPACE}/prompts"
FACTORY_LIB=1 . "$FACTORY_SCRIPT"

# Set variables the shared functions expect
RUN_ID="$FACTORY_RUN_ID"

# --------------------------------------------------------------------------
# Define how this backend launches the author
# --------------------------------------------------------------------------

launch_author() {
  LAUNCH_PROMPT="$1"; shift
  claude \
    --dangerously-skip-permissions \
    --append-system-prompt "$FACTORY_SYSTEM_PROMPT" \
    -p "$LAUNCH_PROMPT" \
    "$@"
}

# No per-session hook on Fargate
PRE_SESSION_HOOK=""

# --------------------------------------------------------------------------
# Run the session loop (from factory script)
# --------------------------------------------------------------------------

cd "$WORKSPACE"
run_session_loop

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
