#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"

BUILD="$ROOT/skills/fluent.full/fluent.md"
CAPTURE="$ROOT/skills/fluent.full/references/capture-brief.md"
DEFINE="$ROOT/skills/fluent.full/references/define-behaviors.md"
APPROACH="$ROOT/skills/fluent.full/references/design-approach.md"
PLAN="$ROOT/skills/fluent.full/references/plan-execution.md"

failures=0

require_in_file() {
  local file="$1"
  local phrase="$2"
  local label="$3"
  local normalized

  normalized="$(tr '\n\t' '  ' < "$file" | tr -s ' ')"

  if [[ "$normalized" != *"$phrase"* ]]; then
    echo "${label} lacks planning-delegation guidance: ${phrase}" >&2
    failures=$((failures + 1))
  fi
}

# Behavior selectors used by documentation/behaviors.md:
# planning delegation is offered early
# explicit delegation spans the remaining planning stages
# revoking delegation restores ordered confirmations
# delegated planning keeps mandatory interruptions
# delegated planning uses one final planning-set confirmation
# delegated planning does not authorize execution

require_in_file "$BUILD" \
  '## Planning-wide delegation' \
  "fluent skill"
require_in_file "$BUILD" \
  'use your judgment through the rest of planning' \
  "fluent skill"
require_in_file "$BUILD" \
  '"Keep going" does not activate delegation' \
  "fluent skill"
require_in_file "$BUILD" \
  'brief, behaviors, approach, and plan' \
  "fluent skill"
require_in_file "$BUILD" \
  'one confirmation after the last applicable planning artifact' \
  "fluent skill"
require_in_file "$BUILD" \
  'Do not repeat the offer after the user has chosen a collaboration style.' \
  "fluent skill"
require_in_file "$BUILD" \
  'The user may activate it at any point, revoke it at any point' \
  "fluent skill"
require_in_file "$BUILD" \
  'return to the earliest provisional artifact' \
  "fluent skill"
require_in_file "$BUILD" \
  'Research facts that can be discovered from the project or authoritative sources without asking the user.' \
  "fluent skill"
require_in_file "$BUILD" \
  'Delegation expires after the final planning confirmation.' \
  "fluent skill"
require_in_file "$BUILD" \
  'Delegation does not authorize Work Item creation, Attempt creation, execution, or landing.' \
  "fluent skill"

for stage in "$CAPTURE" "$DEFINE" "$APPROACH" "$PLAN"; do
  require_in_file "$stage" \
    'When planning-wide delegation is active' \
    "$(basename "$stage")"
  require_in_file "$stage" \
    'mandatory interruption' \
    "$(basename "$stage")"
  require_in_file "$stage" \
    'Planning-wide delegation is the only exception' \
    "$(basename "$stage")"
done

require_in_file "$CAPTURE" \
  'write the brief as provisional' \
  "capture-brief"
require_in_file "$CAPTURE" \
  'continue directly to `define-behaviors`' \
  "capture-brief"
require_in_file "$CAPTURE" \
  'Confirm the brief and move to behaviors?' \
  "capture-brief normal path"

require_in_file "$DEFINE" \
  'read a provisional brief' \
  "define-behaviors"
require_in_file "$DEFINE" \
  'write `behaviors.diff.md` as provisional' \
  "define-behaviors"
require_in_file "$DEFINE" \
  'continue directly to `design-approach`' \
  "define-behaviors"
require_in_file "$DEFINE" \
  'Confirm the behaviors diff and move to approach?' \
  "define-behaviors normal path"

require_in_file "$APPROACH" \
  'read provisional planning inputs' \
  "design-approach"
require_in_file "$APPROACH" \
  'write `approach.md` as provisional' \
  "design-approach"
require_in_file "$APPROACH" \
  'continue directly to `plan-execution`' \
  "design-approach"
require_in_file "$APPROACH" \
  'Confirm the approach and move to planning?' \
  "design-approach normal path"

require_in_file "$PLAN" \
  '## Final delegated-planning confirmation' \
  "plan-execution"
require_in_file "$PLAN" \
  'show the complete planning set' \
  "plan-execution"
require_in_file "$PLAN" \
  'Brief, Behaviors, Approach, and Plan' \
  "plan-execution"
require_in_file "$PLAN" \
  'Learner mode' \
  "plan-execution"
require_in_file "$PLAN" \
  'release criteria' \
  "plan-execution"
require_in_file "$PLAN" \
  'Do not create any Work Item before this confirmation.' \
  "plan-execution"
require_in_file "$PLAN" \
  'update every affected downstream artifact' \
  "plan-execution"
require_in_file "$PLAN" \
  'Confirm the plan and move to Work Item creation?' \
  "plan-execution normal path"

if [ "$failures" -ne 0 ]; then
  exit 1
fi

echo "planning-wide-delegation: all checks passed"
