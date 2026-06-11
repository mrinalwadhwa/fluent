#!/usr/bin/env bash
# entrypoint.sh — Single-container Fargate entrypoint for Factory
# Work Item Attempts and Merges. The container layout uses a single
# worktrees root that contains the project root plus any sibling
# candidate / review worktrees Factory creates:
#
#   /worktrees/${FACTORY_PROJECT_NAME}/   project root
#   /worktrees/work-<...>/                candidate worktrees
#   /worktrees/review-<...>/              review worktrees
#
# Two modes, selected by env vars:
#
#   Work Attempt mode (FACTORY_WORK_ITEM_ID + FACTORY_WORK_ATTEMPT_ID):
#     Pull tar into /worktrees, run `factory work attempt run`, upload
#     /worktrees back to S3.
#
#   Work Merge mode (FACTORY_WORK_ITEM_ID +
#   FACTORY_WORK_MERGE_CANDIDATE_ID): same shape, runs
#   `factory work merge` instead.
#
# Environment variables (passed as task overrides):
#   FACTORY_WORK_ITEM_ID              Work Item ID
#   FACTORY_WORK_ATTEMPT_ID           Attempt ID  (Attempt mode)
#   FACTORY_WORK_MERGE_CANDIDATE_ID   Merge Candidate ID (Merge mode)
#   FACTORY_PROJECT_NAME              basename of the project root
#                                     (e.g. "main")
#   FACTORY_S3_BUCKET                 S3 bucket for workspace transfer
#   FACTORY_REGION                    AWS region
#   CLAUDE_CODE_OAUTH_TOKEN           Claude auth token

set -euo pipefail

WORKTREES_ROOT="/worktrees"

die() { printf 'factory-run: %s\n' "$1" >&2; exit 1; }

[ -n "${FACTORY_S3_BUCKET:-}" ] || die "FACTORY_S3_BUCKET not set"
[ -n "${CLAUDE_CODE_OAUTH_TOKEN:-}" ] || die "No Claude auth token"
[ -n "${FACTORY_WORK_ITEM_ID:-}" ] || die "FACTORY_WORK_ITEM_ID not set"
[ -n "${FACTORY_PROJECT_NAME:-}" ] || die "FACTORY_PROJECT_NAME not set"

MODE=""
if [ -n "${FACTORY_WORK_MERGE_CANDIDATE_ID:-}" ]; then
  MODE="work-merge"
elif [ -n "${FACTORY_WORK_ATTEMPT_ID:-}" ]; then
  MODE="work-attempt"
else
  die "Set FACTORY_WORK_ATTEMPT_ID (Work Attempt mode) or FACTORY_WORK_MERGE_CANDIDATE_ID (Work Merge mode)"
fi

S3_REGION="${FACTORY_REGION:-us-west-1}"
WORKSPACE="${WORKTREES_ROOT}/${FACTORY_PROJECT_NAME}"

case "$MODE" in
  work-attempt)
    S3_IN_KEY="work/${FACTORY_WORK_ITEM_ID}/${FACTORY_WORK_ATTEMPT_ID}/workspace-in.tar"
    S3_OUT_KEY="work/${FACTORY_WORK_ITEM_ID}/${FACTORY_WORK_ATTEMPT_ID}/workspace-out.tar"
    ;;
  work-merge)
    S3_IN_KEY="work-merge/${FACTORY_WORK_ITEM_ID}/${FACTORY_WORK_MERGE_CANDIDATE_ID}/workspace-in.tar"
    S3_OUT_KEY="work-merge/${FACTORY_WORK_ITEM_ID}/${FACTORY_WORK_MERGE_CANDIDATE_ID}/workspace-out.tar"
    ;;
esac

mkdir -p "$WORKTREES_ROOT"

# --------------------------------------------------------------------------
# Pull workspace from S3 into /worktrees
# --------------------------------------------------------------------------

printf 'factory-run: pulling workspace from s3://%s/%s\n' "$FACTORY_S3_BUCKET" "$S3_IN_KEY"

WAIT=0
while true; do
  if aws s3 cp \
    --region "$S3_REGION" \
    "s3://${FACTORY_S3_BUCKET}/${S3_IN_KEY}" \
    - 2>/dev/null | tar xf - -C "$WORKTREES_ROOT"; then
    printf 'factory-run: workspace received\n'
    break
  fi
  sleep 5
  WAIT=$((WAIT + 1))
  if [ "$WAIT" -gt 60 ]; then
    die "Timed out waiting for workspace in S3 (5 minutes)"
  fi
done

[ -d "$WORKSPACE" ] || die "Expected project root at $WORKSPACE after extracting tarball"

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
# Upload /worktrees back to S3
# --------------------------------------------------------------------------

printf 'factory-run: uploading worktrees to s3://%s/%s\n' "$FACTORY_S3_BUCKET" "$S3_OUT_KEY"

tar cf - -C "$WORKTREES_ROOT" . | \
  aws s3 cp \
    --region "$S3_REGION" \
    - "s3://${FACTORY_S3_BUCKET}/${S3_OUT_KEY}"

printf 'factory-run: uploaded to s3://%s/%s\n' "$FACTORY_S3_BUCKET" "$S3_OUT_KEY"
printf 'factory-run: done\n'
