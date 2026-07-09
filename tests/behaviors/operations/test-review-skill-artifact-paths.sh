#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"

failures=0

for reviewer in documentation architecture skills tests; do
  skill="$ROOT/skills/review-${reviewer}/SKILL.md"

  if grep -Fq ".fluent/runs/" "$skill"; then
    echo "review-${reviewer} still contains legacy .fluent/runs/ path" >&2
    failures=$((failures + 1))
  fi

  if ! grep -Fq "## Purpose" "$skill"; then
    echo "review-${reviewer} lacks ## Purpose section" >&2
    failures=$((failures + 1))
  fi

  if ! grep -Fq "## Scope" "$skill"; then
    echo "review-${reviewer} lacks ## Scope section" >&2
    failures=$((failures + 1))
  fi

  if ! grep -Fq "## Method" "$skill"; then
    echo "review-${reviewer} lacks ## Method section" >&2
    failures=$((failures + 1))
  fi

  if ! grep -Fq "invoking layer" "$skill"; then
    echo "review-${reviewer} does not delegate scope to the invoking layer" >&2
    failures=$((failures + 1))
  fi
done

exit "$failures"
