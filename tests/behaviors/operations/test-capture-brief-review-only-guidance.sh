#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
SKILL="$ROOT/skills/fluent.full/references/capture-brief.md"

failures=0

if ! grep -Fq '## Review-only briefs' "$SKILL"; then
  echo "capture-brief lacks ## Review-only briefs section" >&2
  failures=$((failures + 1))
fi

if ! grep -Fq 'review request' "$SKILL"; then
  echo "capture-brief does not distinguish review requests" >&2
  failures=$((failures + 1))
fi

if ! grep -Fq 'fluent' "$SKILL"; then
  echo "capture-brief does not reference fluent skill for review flow" >&2
  failures=$((failures + 1))
fi

if [ "$failures" -ne 0 ]; then
  exit 1
fi
