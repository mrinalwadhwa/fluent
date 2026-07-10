#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
# Check the full fluent skill body, not the shim.
SKILL="$ROOT/skills/fluent.full/fluent.md"
ARCHITECTURE="$ROOT/documentation/architecture.md"
AGENT_INSTRUCTIONS="$ROOT/CLAUDE.md"

failures=0

require_in_file() {
  local file="$1"
  local phrase="$2"
  local label="$3"

  if ! grep -Fq "$phrase" "$file"; then
    echo "${label} lacks required guidance: ${phrase}" >&2
    failures=$((failures + 1))
  fi
}

require_absent_from_file() {
  local file="$1"
  local phrase="$2"
  local label="$3"

  if grep -Fq "$phrase" "$file"; then
    echo "${label} still contains deleted legacy reference: ${phrase}" >&2
    failures=$((failures + 1))
  fi
}

require_in_file "$SKILL" \
  "The delegated build lifecycle is the Work model" \
  "fluent skill"
require_in_file "$SKILL" \
  "Work Item → Attempt → Task →" \
  "fluent skill"
require_in_file "$SKILL" \
  "approved planning files" \
  "fluent skill"
require_in_file "$SKILL" \
  'Create an Attempt' \
  "fluent skill"
require_in_file "$SKILL" \
  'Run the Attempt' \
  "fluent skill"
require_in_file "$SKILL" \
  'fluent merge-candidate show <work-item-id> <merge-candidate-id>' \
  "fluent skill"
require_in_file "$SKILL" \
  'Land through `fluent merge-candidate land <work-item-id>' \
  "fluent skill"
require_in_file "$SKILL" \
  "unrelated work that can proceed in parallel" \
  "fluent skill"
require_in_file "$SKILL" \
  "Workspaces are" \
  "fluent skill"
require_in_file "$ARCHITECTURE" \
  "Fluent's execution lifecycle uses these durable nouns" \
  "architecture documentation"

require_absent_from_file "$SKILL" \
  'Use legacy run artifacts only for explicit fallback' \
  "fluent skill"
require_absent_from_file "$SKILL" \
  'Fargate-only execution, coordinated child-run decomposition, or recovery' \
  "fluent skill"
require_absent_from_file "$SKILL" \
  "new Work-model Tasks as automatically creating legacy child runs" \
  "fluent skill"
require_absent_from_file "$ARCHITECTURE" \
  'commands remain supported as legacy compatibility' \
  "architecture documentation"
require_absent_from_file "$ARCHITECTURE" \
  '## Legacy run compatibility' \
  "architecture documentation"
require_absent_from_file "$AGENT_INSTRUCTIONS" \
  'Use legacy `fluent run` only for' \
  "agent instructions"
require_absent_from_file "$AGENT_INSTRUCTIONS" \
  'explicit fallback, Fargate-only execution, coordinated child-run' \
  "agent instructions"
require_absent_from_file "$AGENT_INSTRUCTIONS" \
  'decomposition, or recovery of existing run state' \
  "agent instructions"

if grep -Fq 'transitional fallback when the Work path cannot yet carry the work' "$AGENT_INSTRUCTIONS"; then
  echo "agent instructions still describe legacy run as a broad transitional fallback" >&2
  failures=$((failures + 1))
fi

if [ "$failures" -ne 0 ]; then
  exit 1
fi
