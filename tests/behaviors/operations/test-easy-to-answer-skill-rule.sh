#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"

SKILLS=(
  "$ROOT/skills/capture-brief/SKILL.md"
  "$ROOT/skills/define-behaviors/SKILL.md"
  "$ROOT/skills/design-approach/SKILL.md"
  "$ROOT/skills/plan-execution/SKILL.md"
)

failures=0

# --- Negative checks: open-ended confirm phrasings must be absent ---
OPEN_ENDED_PATTERNS=(
  "hold together"
  "feel off"
  "need to move"
  "ordering feel right"
  "capture the intent, or is something missing"
)

for skill in "${SKILLS[@]}"; do
  label="$(basename "$(dirname "$skill")")"
  for pattern in "${OPEN_ENDED_PATTERNS[@]}"; do
    if grep -Fqi "$pattern" "$skill"; then
      echo "FAIL: ${label}/SKILL.md contains open-ended confirm phrasing: '${pattern}'" >&2
      failures=$((failures + 1))
    fi
  done
done

# --- Positive checks: each skill's confirm prompt uses easy-to-answer form ---
# Anchor on stable markers: a "yes" reply and at least one labeled "(a)" option
# in the confirm-step blockquote sections.

for skill in "${SKILLS[@]}"; do
  label="$(basename "$(dirname "$skill")")"
  if ! grep -q '\*\*yes\*\*' "$skill"; then
    echo "FAIL: ${label}/SKILL.md confirm prompt missing **yes** reply option" >&2
    failures=$((failures + 1))
  fi
  if ! grep -q '(a)' "$skill"; then
    echo "FAIL: ${label}/SKILL.md confirm prompt missing labeled option (a)" >&2
    failures=$((failures + 1))
  fi
done

# --- Rule wording: each skill states the easy-to-answer rule ---
RULE_TEXT="Label options as (a), (b), (c), or ask a yes/no with an obvious default."

for skill in "${SKILLS[@]}"; do
  label="$(basename "$(dirname "$skill")")"
  if ! grep -Fq "$RULE_TEXT" "$skill"; then
    echo "FAIL: ${label}/SKILL.md missing rule: ${RULE_TEXT}" >&2
    failures=$((failures + 1))
  fi
done

if [ "$failures" -gt 0 ]; then
  echo "${failures} failure(s)" >&2
  exit 1
fi

echo "easy-to-answer-skill-rule: all checks passed"
