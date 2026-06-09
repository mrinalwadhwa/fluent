#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"

failures=0

for reviewer in documentation architecture skills tests; do
  skill="$ROOT/skills/review-${reviewer}/SKILL.md"
  legacy_path=".factory/runs/[run-id]/reviews/review-${reviewer}.md"

  if ! grep -Fq "Write the review artifact to the exact path named in the prompt." "$skill"; then
    echo "review-${reviewer} does not tell Work reviewers to use the prompt path" >&2
    failures=$((failures + 1))
  fi

  if ! grep -Fq "legacy run reviews, that path is usually" "$skill"; then
    echo "review-${reviewer} does not frame the legacy path as a fallback" >&2
    failures=$((failures + 1))
  fi

  if ! grep -Fq "$legacy_path" "$skill"; then
    echo "review-${reviewer} no longer names its legacy artifact path" >&2
    failures=$((failures + 1))
  fi

  if ! grep -Fq "Do not create legacy run review artifacts during Work-model reviews." "$skill"; then
    echo "review-${reviewer} does not forbid legacy artifacts during Work reviews" >&2
    failures=$((failures + 1))
  fi
done

exit "$failures"
