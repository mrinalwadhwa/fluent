#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
SKILL="$ROOT/skills/review-behaviors/SKILL.md"
failures=0

if grep -Eq 'may (inspect|read|open|load).*plan\.md|read.*plan\.md' "$SKILL"; then
  echo "review-behaviors positively tells reviewers to read plan.md" >&2
  failures=$((failures + 1))
fi

phase_one_reads="$(
  awk '
    /^### Phase 1 / { in_phase = 1; next }
    in_phase && /^### Phase 2 / { exit }
    in_phase { print }
  ' "$SKILL"
)"

disallowed_phase_reads="$(
  grep -E '^- .*`?(\.factory/runs/\[run-id\]/(approach|plan)\.md|Source code|Implementation files|Internal tests)' \
    <<<"$phase_one_reads" || true
)"

if [ -n "$disallowed_phase_reads" ]; then
  echo "review-behaviors Phase 1 read guidance includes files outside the visibility boundary:" >&2
  echo "$disallowed_phase_reads" >&2
  failures=$((failures + 1))
fi

if ! grep -Fq '.factory/runs/[run-id]/behaviors.diff.md' <<<"$phase_one_reads"; then
  echo "review-behaviors Phase 1 no longer tells reviewers to read behaviors.diff.md" >&2
  failures=$((failures + 1))
fi

if ! grep -Fq 'Work Item and Task context in the prompt for Work-model reviews' <<<"$phase_one_reads"; then
  echo "review-behaviors Phase 1 no longer tells Work reviewers to use prompt context" >&2
  failures=$((failures + 1))
fi

if ! grep -Fq 'or `.factory/runs/[run-id]/brief.md` for legacy run reviews' <<<"$phase_one_reads"; then
  echo "review-behaviors Phase 1 no longer limits brief.md to legacy reviews" >&2
  failures=$((failures + 1))
fi

if ! grep -Fq 'documentation/behaviors.md' <<<"$phase_one_reads"; then
  echo "review-behaviors Phase 1 no longer tells reviewers to read documentation/behaviors.md" >&2
  failures=$((failures + 1))
fi

exit "$failures"
