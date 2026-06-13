#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"

SKILLS=(
  "$ROOT/skills/capture-brief/SKILL.md"
  "$ROOT/skills/define-behaviors/SKILL.md"
  "$ROOT/skills/design-approach/SKILL.md"
  "$ROOT/skills/plan-execution/SKILL.md"
)

CANONICAL="Make questions easy to answer."
failures=0

# Check presence in each skill
for skill in "${SKILLS[@]}"; do
  label="$(basename "$(dirname "$skill")")"
  if ! grep -Fq "$CANONICAL" "$skill"; then
    echo "FAIL: ${label}/SKILL.md missing rule: ${CANONICAL}" >&2
    failures=$((failures + 1))
  fi
done

# Check wording consistency across all four skills
hashes=()
for skill in "${SKILLS[@]}"; do
  label="$(basename "$(dirname "$skill")")"
  h=$(grep -F "$CANONICAL" "$skill" | head -1 | shasum -a 256 | awk '{print $1}')
  hashes+=("$label:$h")
done

first_hash="${hashes[0]#*:}"
for entry in "${hashes[@]}"; do
  label="${entry%%:*}"
  h="${entry#*:}"
  if [ "$h" != "$first_hash" ]; then
    echo "FAIL: wording mismatch — ${label} differs from capture-brief" >&2
    failures=$((failures + 1))
  fi
done

if [ "$failures" -gt 0 ]; then
  echo "${failures} failure(s)" >&2
  exit 1
fi

echo "easy-to-answer-skill-rule: all checks passed"
