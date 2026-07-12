#!/usr/bin/env bash
# entrypoint.sh — Single-container Fargate entrypoint for Fluent.
# Supports two dispatch modes:
#
#   Work Attempt mode (FLUENT_WORK_ITEM_ID + FLUENT_WORK_ATTEMPT_ID):
#     Pull tar into /worktrees, run `fluent attempt run`, upload
#     /worktrees back to S3.
#
#   Work Merge mode (FLUENT_WORK_ITEM_ID +
#   FLUENT_WORK_MERGE_CANDIDATE_ID): same shape, runs
#   `fluent merge-candidate land` instead.
#
# Environment variables (passed as task overrides):
#   FLUENT_S3_BUCKET                 S3 bucket for workspace transfer
#   FLUENT_REGION                    AWS region
#   FLUENT_CODER                     coder to use: "claude" (default)
#                                     or "codex"
#   CLAUDE_CODE_OAUTH_TOKEN           Claude auth token (claude coder)
#   CODEX_AUTH_JSON                   Codex auth.json content (codex coder)
#   FLUENT_WORK_ITEM_ID              Work Item ID
#   FLUENT_WORK_ATTEMPT_ID           Attempt ID  (Attempt mode)
#   FLUENT_WORK_MERGE_CANDIDATE_ID   Merge Candidate ID (Merge mode)
#   FLUENT_PROJECT_NAME              basename of the project root
#                                     (e.g. "main")
#   FLUENT_NO_POST_MERGE_REVIEW      if "1", pass --no-post-merge-review
#                                     to merge-candidate land

set -euo pipefail

die() { printf 'fluent-run: %s\n' "$1" >&2; exit 1; }

[ -n "${FLUENT_S3_BUCKET:-}" ] || die "FLUENT_S3_BUCKET not set"

CODER="${FLUENT_CODER:-claude}"

case "$CODER" in
  claude)
    [ -n "${CLAUDE_CODE_OAUTH_TOKEN:-}" ] || die "FLUENT_CODER=claude but CLAUDE_CODE_OAUTH_TOKEN is not set"
    ;;
  codex)
    [ -n "${CODEX_AUTH_JSON:-}" ] || die "FLUENT_CODER=codex but CODEX_AUTH_JSON is not set"
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
    die "Unsupported FLUENT_CODER: '$CODER'. Expected 'claude' or 'codex'."
    ;;
esac

# --------------------------------------------------------------------------
# Dispatch: Work Attempt or Work Merge
# --------------------------------------------------------------------------

WORKTREES_ROOT="${FLUENT_WORKTREES_ROOT:-/worktrees}"

[ -n "${FLUENT_WORK_ITEM_ID:-}" ] || die "FLUENT_WORK_ITEM_ID not set"
[ -n "${FLUENT_PROJECT_NAME:-}" ] || die "FLUENT_PROJECT_NAME not set"

MODE=""
if [ -n "${FLUENT_WORK_MERGE_CANDIDATE_ID:-}" ]; then
  MODE="work-merge"
elif [ -n "${FLUENT_WORK_ATTEMPT_ID:-}" ]; then
  MODE="work-attempt"
else
  die "Set FLUENT_WORK_ATTEMPT_ID (Work Attempt mode) or FLUENT_WORK_MERGE_CANDIDATE_ID (Work Merge mode)"
fi

S3_REGION="${FLUENT_REGION:-us-west-1}"
WORKSPACE="${WORKTREES_ROOT}/${FLUENT_PROJECT_NAME}"

case "$MODE" in
  work-attempt)
    S3_IN_KEY="work/${FLUENT_WORK_ITEM_ID}/${FLUENT_WORK_ATTEMPT_ID}/workspace-in.tar"
    S3_OUT_KEY="work/${FLUENT_WORK_ITEM_ID}/${FLUENT_WORK_ATTEMPT_ID}/workspace-out.tar"
    ;;
  work-merge)
    S3_IN_KEY="work-merge/${FLUENT_WORK_ITEM_ID}/${FLUENT_WORK_MERGE_CANDIDATE_ID}/workspace-in.tar"
    S3_OUT_KEY="work-merge/${FLUENT_WORK_ITEM_ID}/${FLUENT_WORK_MERGE_CANDIDATE_ID}/workspace-out.tar"
    ;;
esac

mkdir -p "$WORKTREES_ROOT"

# --------------------------------------------------------------------------
# Pull workspace from S3 into /worktrees
# --------------------------------------------------------------------------

printf 'fluent-run: pulling workspace from s3://%s/%s\n' "$FLUENT_S3_BUCKET" "$S3_IN_KEY"

# Download to a local file first, then extract. Streaming `aws s3 cp -`
# directly into tar has produced "tar: short read" on the chainguard
# aws-cli, so a two-step approach is more robust.
INPUT_TAR="/tmp/workspace-in-$$.tar"
WAIT=0
while true; do
  if aws s3 cp \
    --region "$S3_REGION" \
    --no-progress \
    "s3://${FLUENT_S3_BUCKET}/${S3_IN_KEY}" \
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
printf 'fluent-run: workspace received\n'

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

if [ -n "${FLUENT_BIN:-}" ]; then
  [ -x "$FLUENT_BIN" ] || die "FLUENT_BIN is not executable: $FLUENT_BIN"
elif [ -x "/usr/local/bin/fluent" ]; then
  FLUENT_BIN="/usr/local/bin/fluent"
elif [ -x "${WORKSPACE}/target/release/fluent" ]; then
  FLUENT_BIN="${WORKSPACE}/target/release/fluent"
elif command -v fluent >/dev/null 2>&1; then
  FLUENT_BIN="$(command -v fluent)"
else
  die "no fluent binary available"
fi

case "$MODE" in
  work-attempt)
    printf 'fluent-run: running fluent attempt run %s %s (coder=%s)\n' \
      "$FLUENT_WORK_ITEM_ID" "$FLUENT_WORK_ATTEMPT_ID" "$CODER"

    "$FLUENT_BIN" attempt run \
      --no-sandbox \
      --coder "$CODER" \
      "$FLUENT_WORK_ITEM_ID" \
      "$FLUENT_WORK_ATTEMPT_ID"
    ;;
  work-merge)
    printf 'fluent-run: running fluent merge-candidate land %s %s (coder=%s)\n' \
      "$FLUENT_WORK_ITEM_ID" "$FLUENT_WORK_MERGE_CANDIDATE_ID" "$CODER"

    merge_extra_args=()
    if [ "${FLUENT_NO_POST_MERGE_REVIEW:-}" = "1" ]; then
      merge_extra_args+=(--no-post-merge-review)
    fi

    "$FLUENT_BIN" merge-candidate land \
      --no-sandbox \
      --coder "$CODER" \
      "${merge_extra_args[@]+"${merge_extra_args[@]}"}" \
      "$FLUENT_WORK_ITEM_ID" \
      "$FLUENT_WORK_MERGE_CANDIDATE_ID"
    ;;
esac

# --------------------------------------------------------------------------
# Upload /worktrees back to S3
# --------------------------------------------------------------------------

printf 'fluent-run: uploading worktrees to s3://%s/%s\n' "$FLUENT_S3_BUCKET" "$S3_OUT_KEY"

# Tar to a file first, then upload, for the same robustness reason
# as the input download.
OUTPUT_TAR="/tmp/workspace-out-$$.tar"
tar cf "$OUTPUT_TAR" -C "$WORKTREES_ROOT" . || die "Failed to archive worktrees for upload"
aws s3 cp \
    --region "$S3_REGION" \
    --no-progress \
    "$OUTPUT_TAR" \
    "s3://${FLUENT_S3_BUCKET}/${S3_OUT_KEY}" || die "Failed to upload worktrees to S3"
rm -f "$OUTPUT_TAR"

printf 'fluent-run: uploaded to s3://%s/%s\n' "$FLUENT_S3_BUCKET" "$S3_OUT_KEY"
printf 'fluent-run: done\n'
