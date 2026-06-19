#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
SKILL="$ROOT/skills/review-behaviors/SKILL.md"
failures=0

if grep -Eq 'may (inspect|read|open|load).*plan\.md|read.*plan\.md' "$SKILL"; then
  echo "review-behaviors positively tells reviewers to read plan.md" >&2
  failures=$((failures + 1))
fi

# The rewritten review-behaviors skill verifies behavior completeness
# from behavior-tests-results.json. It should not reference legacy run
# paths or source code.

if grep -Fq '.factory/runs/' "$SKILL"; then
  echo "review-behaviors still references legacy .factory/runs/ paths" >&2
  failures=$((failures + 1))
fi

if grep -Fq 'brief.md' "$SKILL"; then
  echo "review-behaviors still references brief.md" >&2
  failures=$((failures + 1))
fi

if ! grep -Fq 'tester-results.json' "$SKILL"; then
  echo "review-behaviors no longer references tester-results.json" >&2
  failures=$((failures + 1))
fi

if ! grep -Fq 'documentation/behaviors.md' "$SKILL"; then
  echo "review-behaviors no longer references documentation/behaviors.md" >&2
  failures=$((failures + 1))
fi

if ! grep -Fq 'Do not write or run tests' "$SKILL"; then
  echo "review-behaviors no longer forbids writing or running tests" >&2
  failures=$((failures + 1))
fi

if ! grep -Fq 'Do not read source code' "$SKILL"; then
  echo "review-behaviors no longer forbids reading source code" >&2
  failures=$((failures + 1))
fi

if ! grep -Fq 'Verdict: fail' "$SKILL"; then
  echo "review-behaviors no longer keeps fail verdict for behavior mismatches" >&2
  failures=$((failures + 1))
fi

exit "$failures"
