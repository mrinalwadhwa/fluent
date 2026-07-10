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

# The full fluent skill and its references live in fluent.full/.
CAPTURE="$ROOT/skills/fluent.full/references/capture-brief.md"
DEFINE="$ROOT/skills/fluent.full/references/define-behaviors.md"
APPROACH="$ROOT/skills/fluent.full/references/design-approach.md"
PLAN="$ROOT/skills/fluent.full/references/plan-execution.md"
BUILD="$ROOT/skills/fluent.full/fluent.md"
ARCH="$ROOT/documentation/architecture.md"
BEHAVIORS="$ROOT/documentation/behaviors.md"

require_in_file "$CAPTURE" \
  '.fluent/drafts/' \
  "capture-brief skill"
require_in_file "$CAPTURE" \
  'plan-execution' \
  "capture-brief skill"

for skill in "$DEFINE" "$APPROACH" "$PLAN"; do
  require_in_file "$skill" \
    '.fluent/drafts/' \
    "$skill"
done

require_in_file "$DEFINE" \
  'brief.md' \
  "define-behaviors skill"

require_in_file "$APPROACH" \
  'brief.md' \
  "design-approach skill"
require_in_file "$APPROACH" \
  'behaviors.diff.md' \
  "design-approach skill"

require_in_file "$PLAN" \
  'brief.md' \
  "plan-execution skill"
require_in_file "$PLAN" \
  'behaviors.diff.md' \
  "plan-execution skill"
require_in_file "$PLAN" \
  'approach.md' \
  "plan-execution skill"

require_in_file "$PLAN" \
  '## Decide one Work Item or several' \
  "plan-execution skill"
require_in_file "$PLAN" \
  'fluent work-item create' \
  "plan-execution skill"
require_in_file "$PLAN" \
  '## Plan format' \
  "plan-execution skill"
require_in_file "$PLAN" \
  'State reached' \
  "plan-execution skill"
require_in_file "$PLAN" \
  '## Sync points' \
  "plan-execution skill"
require_in_file "$PLAN" \
  'items/<slug>/plan.md' \
  "plan-execution skill"

require_in_file "$BUILD" \
  'capture-brief' \
  "fluent skill"

require_in_file "$ARCH" \
  "skills treat this Work Item planning context as the normal handoff" \
  "architecture documentation"
require_in_file "$BEHAVIORS" \
  'describe Work Item planning context through' \
  "behavior documentation"
require_in_file "$BEHAVIORS" \
  "create the Work Item with approved planning context" \
  "behavior documentation"
require_in_file "$BEHAVIORS" \
  "keep the approved brief available for later planning" \
  "behavior documentation"
require_in_file "$BEHAVIORS" \
  'Test: tests/behaviors/skills/parallel-work-items-plan.md (test-skill)' \
  "behavior documentation"

require_in_file "$ROOT/tests/behaviors/README.md" \
  'Prefer peer Work Items for independent parallel work' \
  "behavior mapping"
require_in_file "$ROOT/tests/behaviors/README.md" \
  'Define sync points without default Task dependencies or child-run groups' \
  "behavior mapping"

require_in_file "$ROOT/tests/behaviors/skills/parallel-work-items-plan.md" \
  'peer Work Items rather than one Work Item with parallel Tasks' \
  "parallel Work Items skill scenario"
require_in_file "$ROOT/tests/behaviors/skills/parallel-work-items-plan.md" \
  'sync point around the shared user identity contract' \
  "parallel Work Items skill scenario"
require_in_file "$ROOT/tests/behaviors/skills/parallel-work-items-plan.md" \
  'avoid using legacy child-run groups as the default plan shape' \
  "parallel Work Items skill scenario"

require_not_in_file "$CAPTURE" \
  "Write bridge planning artifacts when later skills" \
  "capture-brief skill"
require_not_in_file "$PLAN" \
  '## Shared Attempt/Task notes' \
  "plan-execution skill"
require_not_in_file "$PLAN" \
  'Assemble a legacy run `execution-instructions.md` file only when a compatibility' \
  "plan-execution skill"
require_not_in_file "$PLAN" \
  'Determine whether the work can be executed as a single run (leaf)' \
  "plan-execution skill"
require_not_in_file "$PLAN" \
  '## Output format (leaf run)' \
  "plan-execution skill"
require_not_in_file "$PLAN" \
  'When the plan has parallel child runs, identify where they must converge' \
  "plan-execution skill"
require_not_in_file "$PLAN" \
  'When the work decomposes into independent child runs, use the group/step' \
  "plan-execution skill"
require_not_in_file "$PLAN" \
  'Work-model Tasks with explicit dependencies' \
  "plan-execution skill"
require_not_in_file "$PLAN" \
  'parallel Work-model Tasks' \
  "plan-execution skill"
require_not_in_file "$BEHAVIORS" \
  'Work-model task/dependency structure' \
  "behavior documentation"

if [ "$failures" -ne 0 ]; then
  exit 1
fi
