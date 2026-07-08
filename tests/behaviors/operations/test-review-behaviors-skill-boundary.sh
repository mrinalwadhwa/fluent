#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
SKILL="$ROOT/skills/review-behaviors/SKILL.md"
failures=0

if grep -Eq 'may (inspect|read|open|load).*plan\.md|read.*plan\.md' "$SKILL"; then
  echo "review-behaviors positively tells reviewers to read plan.md" >&2
  failures=$((failures + 1))
fi

if grep -Fq '.factory/runs/' "$SKILL"; then
  echo "review-behaviors still references legacy .factory/runs/ paths" >&2
  failures=$((failures + 1))
fi

if grep -Fq 'brief.md' "$SKILL"; then
  echo "review-behaviors still references brief.md" >&2
  failures=$((failures + 1))
fi

if ! grep -Fq 'references/behaviors.md' "$SKILL"; then
  echo "review-behaviors does not reference references/behaviors.md for standards" >&2
  failures=$((failures + 1))
fi

if ! grep -Fqi 'EARS' "$SKILL"; then
  echo "review-behaviors does not mention EARS patterns" >&2
  failures=$((failures + 1))
fi

if ! grep -Fq 'behavior statement' "$SKILL"; then
  echo "review-behaviors does not mention behavior statements" >&2
  failures=$((failures + 1))
fi

exit "$failures"
