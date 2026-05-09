#!/usr/bin/env bash
# entrypoint.sh — Single-container Fargate entrypoint for factory runs.
#
# Pull workspace from S3, run Claude session loop in tmux,
# capture session snapshots, upload completed workspace to S3.
#
# Environment variables (passed as task overrides):
#   FACTORY_RUN_ID         — the run identifier
#   FACTORY_S3_BUCKET      — S3 bucket for workspace transfer
#   FACTORY_REGION         — AWS region
#   CLAUDE_CODE_OAUTH_TOKEN — Claude auth token

set -euo pipefail

WORKSPACE=/workspace
RUN_DIR="${WORKSPACE}/.factory/runs/${FACTORY_RUN_ID}"
MAX_SESSIONS=50

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

[ -d "$RUN_DIR" ] || die "Run directory not found: $RUN_DIR"

# Write active-run pointer
printf '%s' "$FACTORY_RUN_ID" > "${WORKSPACE}/.factory/active-run"

# --------------------------------------------------------------------------
# Build initial prompt
# --------------------------------------------------------------------------

INITIAL_PROMPT="Read the brief at .factory/runs/${FACTORY_RUN_ID}/brief.md and begin working."
if [ -f "${RUN_DIR}/handoff.md" ]; then
  INITIAL_PROMPT="Read the handoff at .factory/runs/${FACTORY_RUN_ID}/handoff.md and continue working."
fi

# --------------------------------------------------------------------------
# System prompt
# --------------------------------------------------------------------------

FACTORY_SYSTEM_PROMPT='You are operating inside the Factory — a system for extended autonomous coding work.

## Status file contract

Before exiting a session, you MUST write a status file at .factory/runs/[run-id]/status containing exactly one of:
- executing    — context running low, handoff written, session loop will restart you
- rate-limited — API rate limit hit, session loop will wait and restart you
- needs-user   — blocked on a question only the user can answer
- complete     — work is done
- failed       — unrecoverable error

## Handoff file

When writing status "executing" or "needs-user", also write .factory/runs/[run-id]/handoff.md:

## Run [run-id]
Brief: [one-line summary]
Status: [current stage]

### Completed
- [what is done]

### In progress
- [what was happening]

### Open questions
- [anything blocking or unclear]

### Next steps
- [what the next session should do first]

## Session start

On session start, check .factory/runs/ for active runs. If a handoff.md exists, read it and continue from where the previous session left off. Do not re-read the full history — the handoff is your starting context.'

# --------------------------------------------------------------------------
# Set status to executing if starting fresh
# --------------------------------------------------------------------------

CURRENT_STATUS="$(cat "${RUN_DIR}/status" 2>/dev/null || true)"
if [ "$CURRENT_STATUS" = "planned" ]; then
  printf 'executing' > "${RUN_DIR}/status"
fi

# --------------------------------------------------------------------------
# Session loop
# --------------------------------------------------------------------------

SESSION=0
CONSECUTIVE_FAILURES=0

while true; do
  SESSION=$((SESSION + 1))
  printf '\nfactory-run: === session %d ===\n' "$SESSION"

  if [ "$SESSION" -gt "$MAX_SESSIONS" ]; then
    printf 'factory-run: max sessions (%d) reached\n' "$MAX_SESSIONS"
    printf 'failed' > "${RUN_DIR}/status"
    break
  fi

  # Build prompt
  if [ "$SESSION" -eq 1 ]; then
    PROMPT="$INITIAL_PROMPT"
  else
    PROMPT="Continue from the handoff at .factory/runs/${FACTORY_RUN_ID}/handoff.md"
  fi

  # Run Claude
  set +e
  claude \
    --dangerously-skip-permissions \
    --append-system-prompt "$FACTORY_SYSTEM_PROMPT" \
    -p "$PROMPT"
  AGENT_EXIT=$?
  set -e

  printf 'factory-run: agent exited (code: %d)\n' "$AGENT_EXIT"

  # Track consecutive failures
  if [ "$AGENT_EXIT" -ne 0 ]; then
    CONSECUTIVE_FAILURES=$((CONSECUTIVE_FAILURES + 1))
    if [ "$CONSECUTIVE_FAILURES" -ge 3 ]; then
      printf 'factory-run: %d consecutive failures — stopping\n' "$CONSECUTIVE_FAILURES"
      printf 'failed' > "${RUN_DIR}/status"
      break
    fi
  else
    CONSECUTIVE_FAILURES=0
  fi

  # Capture session snapshot
  SESSION_DIR="${RUN_DIR}/sessions/session-${SESSION}"
  mkdir -p "$SESSION_DIR"
  CLAUDE_DIR="${HOME}/.claude"
  if [ -d "$CLAUDE_DIR" ]; then
    [ -f "${CLAUDE_DIR}/history.jsonl" ] && \
      cp "${CLAUDE_DIR}/history.jsonl" "${SESSION_DIR}/transcript.jsonl" 2>/dev/null || true
    PROJ_MEMORY="$(find "${CLAUDE_DIR}" -path "*/memory" -type d 2>/dev/null | head -1 || true)"
    if [ -n "$PROJ_MEMORY" ] && [ -d "$PROJ_MEMORY" ]; then
      cp -r "$PROJ_MEMORY" "${SESSION_DIR}/memory" 2>/dev/null || true
    fi
    [ -d "${CLAUDE_DIR}/todos" ] && \
      cp -r "${CLAUDE_DIR}/todos" "${SESSION_DIR}/todos" 2>/dev/null || true
    printf 'factory-run: session %d snapshot captured\n' "$SESSION"
  fi

  # Read status and decide
  STATUS="$(cat "${RUN_DIR}/status" 2>/dev/null || echo "unknown")"
  printf 'factory-run: status: %s\n' "$STATUS"

  case "$STATUS" in
    executing)
      printf 'factory-run: restarting session...\n'
      sleep 5
      ;;
    rate-limited)
      printf 'factory-run: rate limited — waiting 5 minutes\n'
      sleep 300
      ;;
    complete|needs-user|failed)
      printf 'factory-run: terminal status (%s)\n' "$STATUS"
      break
      ;;
    *)
      printf 'factory-run: unexpected status "%s" — stopping\n' "$STATUS"
      break
      ;;
  esac
done

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
printf 'factory-run: done (sessions: %d)\n' "$SESSION"
