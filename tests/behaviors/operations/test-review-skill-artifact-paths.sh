#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"

failures=0

for reviewer in documentation architecture skills tests; do
  skill="$ROOT/skills/review-${reviewer}/SKILL.md"

  if ! grep -Fq "Write the review artifact to the exact path named in the prompt." "$skill"; then
    echo "review-${reviewer} does not tell Work reviewers to use the prompt path" >&2
    failures=$((failures + 1))
  fi

  if grep -Fq "legacy run reviews, that path is usually" "$skill"; then
    echo "review-${reviewer} still references legacy run review path framing" >&2
    failures=$((failures + 1))
  fi

  if grep -Fq ".factory/runs/" "$skill"; then
    echo "review-${reviewer} still contains legacy .factory/runs/ path" >&2
    failures=$((failures + 1))
  fi

  if grep -Fq "Do not create legacy run review artifacts during Work-model reviews." "$skill"; then
    echo "review-${reviewer} still contains legacy artifact prohibition (no longer needed)" >&2
    failures=$((failures + 1))
  fi
done

exit "$failures"
