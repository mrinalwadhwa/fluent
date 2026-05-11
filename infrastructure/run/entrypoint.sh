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

# --------------------------------------------------------------------------
# Run report
# --------------------------------------------------------------------------

generate_report() {
  REPORT_FILE="${RUN_DIR}/report.md"
  BRIEF_SUMMARY=""
  if [ -f "${RUN_DIR}/brief.md" ]; then
    BRIEF_SUMMARY="$(grep -v '^#' "${RUN_DIR}/brief.md" | grep -v '^$' | head -3)"
  fi
  {
    printf '# Run Report\n\nRun: %s\nStatus: %s\nMode: %s\nSessions: %d\n\n' \
      "$FACTORY_RUN_ID" \
      "$(cat "${RUN_DIR}/status" 2>/dev/null || echo "unknown")" \
      "$(cat "${RUN_DIR}/mode" 2>/dev/null || echo "build")" \
      "${SESSION:-0}"
    printf '## Brief\n\n%s\n\n' "${BRIEF_SUMMARY:-(no brief)}"
    printf '## Reviewer verdicts\n\n'
    if [ -d "${RUN_DIR}/reviews" ]; then
      for review in "${RUN_DIR}/reviews"/review-*.md; do
        [ -f "$review" ] || continue
        rname="$(basename "$review" .md | sed 's/^review-//')"
        rverdict="$(grep -i '^Verdict:' "$review" | head -1 | sed 's/.*: *//')"
        printf '- **%s**: %s\n' "$rname" "${rverdict:-no verdict}"
      done
    else
      printf '(no reviews)\n'
    fi
    printf '\n'
  } > "$REPORT_FILE"
  printf 'factory-run: report written\n'
}

# --------------------------------------------------------------------------
# Review functions
# --------------------------------------------------------------------------

run_single_reviewer() {
  local reviewer_name="$1" reviewer_system="$2" reviewer_prompt="$3"
  local reviewer_run_dir="$4" reviewer_result_file="$5"

  printf '  [%s] starting...\n' "$reviewer_name"

  set +e
  claude \
    --dangerously-skip-permissions \
    --append-system-prompt "$reviewer_system" \
    -p "$reviewer_prompt"
  local reviewer_exit=$?
  set -e

  if [ "$reviewer_exit" -ne 0 ]; then
    printf '  [%s] session failed (exit %d), skipping\n' "$reviewer_name" "$reviewer_exit"
    printf 'pass' > "$reviewer_result_file"
    return
  fi

  local review_file="${reviewer_run_dir}/reviews/review-${reviewer_name}.md"
  if [ ! -f "$review_file" ]; then
    printf '  [%s] no review artifact produced, skipping\n' "$reviewer_name"
    printf 'pass' > "$reviewer_result_file"
    return
  fi

  local verdict
  verdict="$(grep -i '^Verdict:' "$review_file" | head -1 | sed 's/.*: *//' | tr '[:upper:]' '[:lower:]' | tr -d '[:space:]')"
  printf '  [%s] verdict: %s\n' "$reviewer_name" "$verdict"
  printf '%s' "$verdict" > "$reviewer_result_file"
}

# Run reviewers in parallel. Returns 0 if all pass, 1 if any fail.
# Args: run-dir run-id [reviewer-filter] [mode]
run_reviews() {
  local review_run_dir="$1" review_run_id="$2"
  local reviewer_filter="${3:-}" review_mode="${4:-run-scoped}"

  mkdir -p "${review_run_dir}/reviews"

  local scope_detail scope_instruction=""
  scope_detail="$(cat "${review_run_dir}/scope" 2>/dev/null || true)"
  if [ -n "$scope_detail" ]; then
    scope_instruction=" Focus your review on: ${scope_detail}. Read surrounding context as needed, but concentrate your findings on these areas."
  fi

  printf '\nfactory-run: === review phase (run: %s, mode: %s) ===\n\n' "$review_run_id" "$review_mode"

  local review_tmp pids=""
  review_tmp="$(mktemp -d -t factory-review-XXXXXX)"

  # --- Documentation reviewer ---
  if [ -z "$reviewer_filter" ] || echo "$reviewer_filter" | grep -q "documentation"; then
    local doc_system doc_prompt
    doc_system='You are a documentation reviewer operating inside the Factory.
Follow the review-documentation skill at skills/review-documentation/SKILL.md.
Read the code and documentation, check accuracy, writing quality, and
completeness, and produce a review artifact.
Write your review to .factory/runs/'"${review_run_id}"'/reviews/review-documentation.md
with a verdict (pass, fail, or uncertain) and findings.'

    if [ "$review_mode" = "full-codebase" ]; then
      doc_prompt="Perform a full-codebase documentation review. Review all documentation files against the source code. Check accuracy, writing quality, and completeness. The review output goes to .factory/runs/${review_run_id}/reviews/review-documentation.md."
    else
      doc_prompt="Review the documentation for run ${review_run_id}. The run artifacts are in .factory/runs/${review_run_id}/. Read the brief and behaviors.diff.md to understand the run's intent, then review all documentation affected by the run."
    fi

    doc_prompt="${doc_prompt}${scope_instruction}"
    run_single_reviewer "documentation" "$doc_system" "$doc_prompt" "$review_run_dir" "${review_tmp}/documentation" &
    pids="$pids $!"
  fi

  # --- Behavior reviewer ---
  if [ -z "$reviewer_filter" ] || echo "$reviewer_filter" | grep -q "behaviors"; then
    local beh_system beh_prompt
    beh_system='You are a behavior reviewer operating inside the Factory.
Follow the review-behaviors skill at skills/review-behaviors/SKILL.md.
Read behaviors and user-facing documentation. Write tests that verify
behavior from the user perspective, run them, and check for regressions.
Do NOT read source code or implementation files.
Write your review to .factory/runs/'"${review_run_id}"'/reviews/review-behaviors.md
with a verdict (pass, fail, or uncertain) and findings.'

    if [ "$review_mode" = "full-codebase" ]; then
      beh_prompt="Perform a full-codebase behavior review. Read documentation/behaviors.md and run all existing behavior tests. Report any failures as regressions. Report any behaviors without test references as gaps. Write tests for untested behaviors where possible. The review output goes to .factory/runs/${review_run_id}/reviews/review-behaviors.md."
    else
      beh_prompt="Review the behaviors for run ${review_run_id}. The run artifacts are in .factory/runs/${review_run_id}/. Read behaviors.diff.md and the brief, then write and run tests to verify each behavior from the user's perspective."
    fi

    beh_prompt="${beh_prompt}${scope_instruction}"
    run_single_reviewer "behaviors" "$beh_system" "$beh_prompt" "$review_run_dir" "${review_tmp}/behaviors" &
    pids="$pids $!"
  fi

  # --- Architecture reviewer ---
  if [ -z "$reviewer_filter" ] || echo "$reviewer_filter" | grep -q "architecture"; then
    local arch_system arch_prompt
    arch_system='You are an architecture reviewer operating inside the Factory.
Follow the review-architecture skill at skills/review-architecture/SKILL.md.
Read the code and architectural expertise. Evaluate structural decisions
against the principles. Check at whatever scale is relevant.
Write your review to .factory/runs/'"${review_run_id}"'/reviews/review-architecture.md
with a verdict (pass, fail, or uncertain) and findings.'

    if [ "$review_mode" = "full-codebase" ]; then
      arch_prompt="Perform a full-codebase architecture review. Read expertise/architecture/principles.md and documentation/architecture.md. Evaluate the overall system structure against the architectural principles. Check all viewpoints. The review output goes to .factory/runs/${review_run_id}/reviews/review-architecture.md."
    else
      arch_prompt="Review the architecture for run ${review_run_id}. The run artifacts are in .factory/runs/${review_run_id}/. Read the brief and approach.md to understand the run's intent. Read expertise/architecture/principles.md. Evaluate the code changes against the architectural principles."
    fi

    arch_prompt="${arch_prompt}${scope_instruction}"
    run_single_reviewer "architecture" "$arch_system" "$arch_prompt" "$review_run_dir" "${review_tmp}/architecture" &
    pids="$pids $!"
  fi

  # Wait for all
  local pid
  for pid in $pids; do
    wait "$pid" 2>/dev/null || true
  done

  # Check results
  local review_failed=0 result_file result
  for result_file in "${review_tmp}"/*; do
    [ -f "$result_file" ] || continue
    result="$(cat "$result_file")"
    case "$result" in
      pass) ;;
      fail|uncertain) review_failed=1 ;;
    esac
  done

  rm -rf "$review_tmp"
  return "$review_failed"
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

[ -d "$RUN_DIR" ] || die "Run directory not found: $RUN_DIR"

# Write active-run pointer
printf '%s' "$FACTORY_RUN_ID" > "${WORKSPACE}/.factory/active-run"

# --------------------------------------------------------------------------
# Detect run mode
# --------------------------------------------------------------------------

RUN_MODE="$(cat "${RUN_DIR}/mode" 2>/dev/null || echo "build")"
REVIEWER_FILTER="$(cat "${RUN_DIR}/reviewers" 2>/dev/null || true)"

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

# For review runs, start by running reviewers to get initial findings
if [ "$RUN_MODE" = "review" ]; then
  printf 'factory-run: mode: review (reviewers run first)\n'
  REVIEW_SCOPE="full-codebase"
  if ! run_reviews "$RUN_DIR" "$FACTORY_RUN_ID" "$REVIEWER_FILTER" "$REVIEW_SCOPE"; then
    INITIAL_PROMPT="This is a review run. Reviewers have produced findings. Read the review artifacts at .factory/runs/${FACTORY_RUN_ID}/reviews/ and address the findings. When done, write status 'complete'."
  else
    printf '\nfactory-run: all reviewers passed — nothing to fix\n'
    printf 'complete' > "${RUN_DIR}/status"
    generate_report
    printf 'factory-run: run %s completed\n' "$FACTORY_RUN_ID"
    # Skip session loop — jump to S3 upload
    SESSION=0
    SKIP_SESSION_LOOP=1
  fi
fi

# --------------------------------------------------------------------------
# Session loop
# --------------------------------------------------------------------------

SKIP_SESSION_LOOP=${SKIP_SESSION_LOOP:-0}

if [ "$SKIP_SESSION_LOOP" -eq 0 ]; then

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
    complete)
      # Run review phase before accepting completion
      REVIEW_SCOPE="run-scoped"
      [ "$RUN_MODE" = "review" ] && REVIEW_SCOPE="full-codebase"
      if run_reviews "$RUN_DIR" "$FACTORY_RUN_ID" "$REVIEWER_FILTER" "$REVIEW_SCOPE"; then
        generate_report
        printf 'factory-run: run %s completed\n' "$FACTORY_RUN_ID"
        break
      else
        printf 'factory-run: review returned findings — restarting author\n'
        printf 'executing' > "${RUN_DIR}/status"
        PROMPT="Reviewers found issues. Read the review artifacts at .factory/runs/${FACTORY_RUN_ID}/reviews/ and address the findings."
        sleep 2
      fi
      ;;
    executing)
      printf 'factory-run: restarting session...\n'
      sleep 5
      ;;
    rate-limited)
      printf 'factory-run: rate limited — waiting 5 minutes\n'
      sleep 300
      ;;
    needs-user|failed)
      printf 'factory-run: terminal status (%s)\n' "$STATUS"
      break
      ;;
    *)
      printf 'factory-run: unexpected status "%s" — stopping\n' "$STATUS"
      break
      ;;
  esac
done

fi  # SKIP_SESSION_LOOP

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
printf 'factory-run: done (sessions: %d)\n' "${SESSION:-0}"
