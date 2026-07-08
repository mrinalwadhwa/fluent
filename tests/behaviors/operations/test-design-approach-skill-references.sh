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

if ! grep -Fq "references/architecture.md" "$SKILL"; then
  echo "design-approach does not mention references/architecture.md" >&2
  failures=$((failures + 1))
fi

if [ ! -e "$SKILL_DIR/references/architecture.md" ]; then
  echo "design-approach does not ship references/architecture.md" >&2
  failures=$((failures + 1))
fi

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
