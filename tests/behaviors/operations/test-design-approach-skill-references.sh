#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
SKILL_DIR="$ROOT/skills/design-approach"
SKILL="$SKILL_DIR/SKILL.md"
failures=0

if grep -Eq '(^|[^[:alnum:]_./-])expertise/[[:alnum:]_.-]+\.md' "$SKILL"; then
  echo "design-approach references repo-level expertise paths directly" >&2
  failures=$((failures + 1))
fi

for reference in references/INDEX.md references/architecture.md; do
  if ! grep -Fq "$reference" "$SKILL"; then
    echo "design-approach does not mention $reference" >&2
    failures=$((failures + 1))
  fi

  if [ ! -e "$SKILL_DIR/$reference" ]; then
    echo "design-approach does not ship $reference" >&2
    failures=$((failures + 1))
  fi
done

while IFS= read -r reference; do
  if [ ! -e "$SKILL_DIR/references/$reference" ]; then
    echo "design-approach index advertises missing reference: $reference" >&2
    failures=$((failures + 1))
  fi
done < <(
  awk -F'|' '
    NR > 2 {
      gsub(/^[[:space:]]+|[[:space:]]+$/, "", $2)
      if ($2 ~ /^[[:alnum:]_.-]+\.md$/) {
        print $2
      }
    }
  ' "$SKILL_DIR/references/INDEX.md"
)

direct_references="$(
  grep -Eo '`[^`]+\.md`' "$SKILL" \
    | tr -d '`' \
    | grep -E '^expertise/' || true
)"

if [ -n "$direct_references" ]; then
  echo "design-approach output or guidance contains direct expertise references:" >&2
  echo "$direct_references" >&2
  failures=$((failures + 1))
fi

exit "$failures"
