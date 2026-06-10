#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"

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

require_not_in_file() {
  local file="$1"
  local phrase="$2"
  local label="$3"

  if grep -Fq "$phrase" "$file"; then
    echo "${label} still contains legacy-default guidance: ${phrase}" >&2
    failures=$((failures + 1))
  fi
}

CAPTURE="$ROOT/skills/capture-brief/SKILL.md"
DEFINE="$ROOT/skills/define-behaviors/SKILL.md"
APPROACH="$ROOT/skills/design-approach/SKILL.md"
PLAN="$ROOT/skills/plan-execution/SKILL.md"
BUILD="$ROOT/skills/build-in-the-factory/SKILL.md"
ARCH="$ROOT/documentation/architecture.md"
BEHAVIORS="$ROOT/documentation/behaviors.md"

require_in_file "$CAPTURE" \
  'planning context is set at `factory work create` time' \
  "capture-brief skill"
require_in_file "$CAPTURE" \
  'Do not create `.factory/runs/[run-id]/brief.md`,' \
  "capture-brief skill"
require_in_file "$CAPTURE" \
  'for ordinary Work-model planning.' \
  "capture-brief skill"
require_in_file "$CAPTURE" \
  'when an explicit legacy fallback or recovery path needs them' \
  "capture-brief skill"

for skill in "$DEFINE" "$APPROACH" "$PLAN"; do
  require_in_file "$skill" \
    "normal source of intent" \
    "$skill"
  require_in_file "$skill" \
    "stores Work Item planning context" \
    "$skill"
  require_in_file "$skill" \
    'Work Item planning context from `factory work show <work-item-id>` only' \
    "$skill"
  require_in_file "$skill" \
    "only in a legacy fallback or" \
    "$skill"
done

require_in_file "$DEFINE" \
  "The approved brief from the active planning conversation or draft" \
  "define-behaviors skill"
require_in_file "$APPROACH" \
  "The approved brief and behavior diff from the active planning" \
  "design-approach skill"
require_in_file "$PLAN" \
  "The approved brief, behavior diff, and approach from the active" \
  "plan-execution skill"

require_in_file "$PLAN" \
  "This is the normal path for" \
  "plan-execution skill"
require_in_file "$PLAN" \
  'Do not write' \
  "plan-execution skill"
require_in_file "$PLAN" \
  '`.factory/runs/[run-id]/brief.md`, `status`, or `.factory/active-run`' \
  "plan-execution skill"
require_in_file "$PLAN" \
  'when `factory work create` can express' \
  "plan-execution skill"

require_in_file "$BUILD" \
  "Write a brief that will become" \
  "build-in-the-factory skill"
require_in_file "$BUILD" \
  "These files are not the normal planning handoff for Work-model" \
  "build-in-the-factory skill"
require_in_file "$ARCH" \
  "skills treat this Work Item planning context as the normal handoff" \
  "architecture documentation"
require_in_file "$BEHAVIORS" \
  'describe Work Item planning context through' \
  "behavior documentation"
require_in_file "$BEHAVIORS" \
  'planning files to legacy fallback or recovery' \
  "behavior documentation"
require_in_file "$BEHAVIORS" \
  "create the Work Item with approved planning context" \
  "behavior documentation"
require_in_file "$BEHAVIORS" \
  "keep the approved brief available for later planning" \
  "behavior documentation"

require_not_in_file "$CAPTURE" \
  "Write bridge planning artifacts when later skills" \
  "capture-brief skill"
require_not_in_file "$PLAN" \
  'Assemble a legacy run `execution-instructions.md` file only when a compatibility' \
  "plan-execution skill"

if [ "$failures" -ne 0 ]; then
  exit 1
fi
