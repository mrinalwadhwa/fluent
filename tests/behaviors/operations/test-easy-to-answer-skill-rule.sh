#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"

SKILLS=(
  "$ROOT/skills/fluent.full/references/capture-brief.md"
  "$ROOT/skills/fluent.full/references/define-behaviors.md"
  "$ROOT/skills/fluent.full/references/design-approach.md"
  "$ROOT/skills/fluent.full/references/plan-execution.md"
)

# The always-loaded craft section (seeded by `fluent init`) is a rule surface too.
CRAFT_SURFACE="$ROOT/src/main.rs"

failures=0

fail() {
  echo "FAIL: $1" >&2
  failures=$((failures + 1))
}

# --- Negative checks: open-ended confirm phrasings must be absent ---
OPEN_ENDED_PATTERNS=(
  "hold together"
  "feel off"
  "need to move"
  "ordering feel right"
  "capture the intent, or is something missing"
)

for skill in "${SKILLS[@]}"; do
  label="$(basename "$skill" .md)"
  for pattern in "${OPEN_ENDED_PATTERNS[@]}"; do
    if grep -Fqi "$pattern" "$skill"; then
      fail "${label} contains open-ended confirm phrasing: '${pattern}'"
    fi
  done
done

# --- The stale loose two-pattern rule wording must be gone ---
STALE_RULE="Label options as (a), (b), (c), or ask a yes/no with an obvious default."
for skill in "${SKILLS[@]}"; do
  label="$(basename "$skill" .md)"
  if grep -Fq "$STALE_RULE" "$skill"; then
    fail "${label} still carries the old loose two-pattern rule"
  fi
done

# --- Each surface names both archetypes and their conventions ---
# Structural, not exact-string: assert the two archetype names plus the
# yes (y) convention and the marked recommendation, so the rule survives
# rewrites while a broken rule still fails. The archetype names are matched
# in their bold `**Decision**` / `**Confirm gate**` form so a bare "Decision"
# heading in the approach-format template cannot satisfy the check.
assert_expresses_rule() {
  surface="$1"
  label="$2"
  grep -Fq "**Decision**" "$surface" || fail "${label} does not name the Decision archetype"
  grep -Fq "**Confirm gate**" "$surface" ||
    fail "${label} does not name the Confirm gate archetype"
  grep -Fq "yes (y)" "$surface" || fail "${label} does not state the yes (y) confirm convention"
  grep -Fq "(recommended" "$surface" || fail "${label} does not mark the recommended option"
}

for skill in "${SKILLS[@]}"; do
  label="$(basename "$skill" .md)"
  assert_expresses_rule "$skill" "$label"
  # A labeled option and the named anti-pattern must be present in each reference.
  grep -Fq "(a)" "$skill" || fail "${label} confirm prompt missing labeled option (a)"
  grep -Fqi "re-describe" "$skill" ||
    fail "${label} does not name the unlabeled-option anti-pattern"
done

# --- Cross-surface: the seeded craft section expresses the same rule ---
assert_expresses_rule "$CRAFT_SURFACE" "craft-section"

if [ "$failures" -gt 0 ]; then
  echo "${failures} failure(s)" >&2
  exit 1
fi

echo "easy-to-answer-skill-rule: all checks passed"
