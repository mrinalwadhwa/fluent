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
#   FACTORY_CODER                     coder to use: "claude" (default)
#                                     or "codex"
#   CLAUDE_CODE_OAUTH_TOKEN           Claude auth token (claude coder)
#   CODEX_AUTH_JSON                   Codex auth.json content (codex coder)

set -euo pipefail

WORKTREES_ROOT="${FACTORY_WORKTREES_ROOT:-/worktrees}"

die() { printf 'factory-run: %s\n' "$1" >&2; exit 1; }

[ -n "${FACTORY_S3_BUCKET:-}" ] || die "FACTORY_S3_BUCKET not set"
[ -n "${FACTORY_WORK_ITEM_ID:-}" ] || die "FACTORY_WORK_ITEM_ID not set"
[ -n "${FACTORY_PROJECT_NAME:-}" ] || die "FACTORY_PROJECT_NAME not set"

CODER="${FACTORY_CODER:-claude}"

case "$CODER" in
  claude)
    [ -n "${CLAUDE_CODE_OAUTH_TOKEN:-}" ] || die "FACTORY_CODER=claude but CLAUDE_CODE_OAUTH_TOKEN is not set"
    ;;
  codex)
    [ -n "${CODEX_AUTH_JSON:-}" ] || die "FACTORY_CODER=codex but CODEX_AUTH_JSON is not set"
    command -v codex >/dev/null 2>&1 || die "codex binary not found on PATH"

    auth_mode=$(printf '%s' "$CODEX_AUTH_JSON" | jq -r '.auth_mode // empty')
    [ "$auth_mode" = "chatgpt" ] || die "Fargate Codex requires auth_mode=chatgpt (subscription billing), got: '$auth_mode'"

    config_toml="${HOME}/.codex/config.toml"
    if [ -f "$config_toml" ] && grep -qE '^[[:space:]]*preferred_auth_method[[:space:]]*=[[:space:]]*"apikey"' "$config_toml"; then
      die "Fargate Codex refuses preferred_auth_method=apikey in ${config_toml} (would force per-token billing)"
    fi

    unset OPENAI_API_KEY

    mkdir -p "${HOME}/.codex"
    chmod 0700 "${HOME}/.codex"
    printf '%s' "$CODEX_AUTH_JSON" > "${HOME}/.codex/auth.json"
    chmod 0600 "${HOME}/.codex/auth.json"
    ;;
  *)
    die "Unsupported FACTORY_CODER: '$CODER'. Expected 'claude' or 'codex'."
    ;;
esac

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

# Download to a local file first, then extract. Streaming `aws s3 cp -`
# directly into tar has produced "tar: short read" on the chainguard
# aws-cli, so a two-step approach is more robust.
INPUT_TAR="/tmp/workspace-in-$$.tar"
WAIT=0
while true; do
  if aws s3 cp \
    --region "$S3_REGION" \
    --no-progress \
    "s3://${FACTORY_S3_BUCKET}/${S3_IN_KEY}" \
    "$INPUT_TAR" 2>/dev/null; then
    break
  fi
  sleep 5
  WAIT=$((WAIT + 1))
  if [ "$WAIT" -gt 60 ]; then
    die "Timed out waiting for workspace in S3 (5 minutes)"
  fi
done

tar xf "$INPUT_TAR" -C "$WORKTREES_ROOT" || die "Failed to extract input tarball"
rm -f "$INPUT_TAR"
printf 'factory-run: workspace received\n'

[ -d "$WORKSPACE" ] || die "Expected project root at $WORKSPACE after extracting tarball"

cd "$WORKSPACE"

# The uploaded tarball embeds the local machine's absolute paths in
# any sibling worktrees' .git files and the main .git/worktrees/*
# gitdir entries. Re-link them to this container's layout.
if [ -d .git ] || [ -f .git ]; then
  for sib in "$WORKTREES_ROOT"/work-* "$WORKTREES_ROOT"/review-*; do
    if [ -d "$sib" ]; then
      git worktree repair "$sib" 2>/dev/null || true
    fi
  done
fi

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
    printf 'factory-run: running factory work attempt run %s %s (coder=%s)\n' \
      "$FACTORY_WORK_ITEM_ID" "$FACTORY_WORK_ATTEMPT_ID" "$CODER"

    "$FACTORY_BIN" work attempt run \
      --no-sandbox \
      --coder "$CODER" \
      "$FACTORY_WORK_ITEM_ID" \
      "$FACTORY_WORK_ATTEMPT_ID"
    ;;
  work-merge)
    printf 'factory-run: running factory work merge %s %s (coder=%s)\n' \
      "$FACTORY_WORK_ITEM_ID" "$FACTORY_WORK_MERGE_CANDIDATE_ID" "$CODER"

    "$FACTORY_BIN" work merge \
      --no-sandbox \
      --coder "$CODER" \
      "$FACTORY_WORK_ITEM_ID" \
      "$FACTORY_WORK_MERGE_CANDIDATE_ID"
    ;;
esac

# --------------------------------------------------------------------------
# Upload /worktrees back to S3
# --------------------------------------------------------------------------

printf 'factory-run: uploading worktrees to s3://%s/%s\n' "$FACTORY_S3_BUCKET" "$S3_OUT_KEY"

# Tar to a file first, then upload, for the same robustness reason
# as the input download.
OUTPUT_TAR="/tmp/workspace-out-$$.tar"
tar cf "$OUTPUT_TAR" -C "$WORKTREES_ROOT" . || die "Failed to archive worktrees for upload"
aws s3 cp \
    --region "$S3_REGION" \
    --no-progress \
    "$OUTPUT_TAR" \
    "s3://${FACTORY_S3_BUCKET}/${S3_OUT_KEY}" || die "Failed to upload worktrees to S3"
rm -f "$OUTPUT_TAR"

printf 'factory-run: uploaded to s3://%s/%s\n' "$FACTORY_S3_BUCKET" "$S3_OUT_KEY"
printf 'factory-run: done\n'
