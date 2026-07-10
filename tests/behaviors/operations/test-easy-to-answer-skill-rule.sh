#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"

SKILLS=(
  "$ROOT/skills/fluent.full/references/capture-brief.md"
  "$ROOT/skills/fluent.full/references/define-behaviors.md"
  "$ROOT/skills/fluent.full/references/design-approach.md"
  "$ROOT/skills/fluent.full/references/plan-execution.md"
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
  label="$(basename "$skill" .md)"
  for pattern in "${OPEN_ENDED_PATTERNS[@]}"; do
    if grep -Fqi "$pattern" "$skill"; then
      echo "FAIL: ${label} contains open-ended confirm phrasing: '${pattern}'" >&2
      failures=$((failures + 1))
    fi
  done
done

# --- Positive checks: each stage procedure's confirm prompt uses easy-to-answer form ---
# Anchor on stable markers: a "yes" reply and at least one labeled "(a)" option
# in the confirm-step blockquote sections.

for skill in "${SKILLS[@]}"; do
  label="$(basename "$skill" .md)"
  if ! grep -q '\*\*yes\*\*' "$skill"; then
    echo "FAIL: ${label} confirm prompt missing **yes** reply option" >&2
    failures=$((failures + 1))
  fi
  if ! grep -q '(a)' "$skill"; then
    echo "FAIL: ${label} confirm prompt missing labeled option (a)" >&2
    failures=$((failures + 1))
  fi
done

# --- Rule wording: each stage procedure states the easy-to-answer rule ---
RULE_TEXT="Label options as (a), (b), (c), or ask a yes/no with an obvious default."

for skill in "${SKILLS[@]}"; do
  label="$(basename "$skill" .md)"
  if ! grep -Fq "$RULE_TEXT" "$skill"; then
    echo "FAIL: ${label} missing rule: ${RULE_TEXT}" >&2
    failures=$((failures + 1))
  fi
done

if [ "$failures" -gt 0 ]; then
  echo "${failures} failure(s)" >&2
  exit 1
fi

echo "easy-to-answer-skill-rule: all checks passed"
