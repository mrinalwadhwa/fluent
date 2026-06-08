#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
SKILL="$ROOT/skills/build-in-the-factory/SKILL.md"
ARCHITECTURE="$ROOT/documentation/architecture.md"

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

require_in_file "$SKILL" \
  "The target lifecycle is the Work model" \
  "build-in-the-factory skill"
require_in_file "$SKILL" \
  "Work Item → Attempt → Task →" \
  "build-in-the-factory skill"
require_in_file "$SKILL" \
  'Use legacy `factory run` only as a transitional fallback' \
  "build-in-the-factory skill"
require_in_file "$SKILL" \
  ".factory/work/items/<work-item-id>.json" \
  "build-in-the-factory skill"
require_in_file "$ARCHITECTURE" \
  "Factory's target execution lifecycle uses these durable nouns" \
  "architecture documentation"
require_in_file "$ARCHITECTURE" \
  'commands remain supported as a transitional fallback' \
  "architecture documentation"

if grep -Fq 'The current `.factory/runs` lifecycle remains the execution implementation' "$ARCHITECTURE"; then
  echo "architecture still names legacy runs as the execution implementation" >&2
  failures=$((failures + 1))
fi

if [ "$failures" -ne 0 ]; then
  exit 1
fi
