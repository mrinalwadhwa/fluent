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

if ! grep -Fq "check the Work Item and Task prompt" "$SKILL"; then
  echo "review-behaviors no longer uses Work prompt context for test fallback" >&2
  failures=$((failures + 1))
fi

skill_text="$(tr '\n' ' ' < "$SKILL")"

if ! grep -Fq "or the run's brief for legacy run reviews" <<<"$skill_text"; then
  echo "review-behaviors no longer limits fallback brief use to legacy reviews" >&2
  failures=$((failures + 1))
fi

if ! grep -Fq 'documentation/behaviors.md' <<<"$phase_one_reads"; then
  echo "review-behaviors Phase 1 no longer tells reviewers to read documentation/behaviors.md" >&2
  failures=$((failures + 1))
fi

if ! grep -Fq 'lacks credentials, network access, or another' "$SKILL" ||
    ! grep -Fq 'external prerequisite' "$SKILL"; then
  echo "review-behaviors no longer records environment-limited test execution" >&2
  failures=$((failures + 1))
fi

if ! grep -Fq 'Do not treat that execution error as a behavior failure.' "$SKILL"; then
  echo "review-behaviors no longer distinguishes execution errors from behavior failures" >&2
  failures=$((failures + 1))
fi

if ! grep -Fq 'passing deterministic behavior check covers the same behavior' "$SKILL"; then
  echo "review-behaviors no longer allows pass when deterministic checks cover an environment-limited test" >&2
  failures=$((failures + 1))
fi

if ! grep -Fq 'When no deterministic check covers the behavior and the only' "$SKILL" ||
    ! grep -Fq 'available test cannot run because of an environment limitation, return' "$SKILL" ||
    ! grep -Fq '`uncertain`' "$SKILL"; then
  echo "review-behaviors no longer keeps uncertain for uncovered environment-limited behavior" >&2
  failures=$((failures + 1))
fi

if ! grep -Fq 'return `fail`' "$SKILL"; then
  echo "review-behaviors no longer keeps fail for behavior mismatches" >&2
  failures=$((failures + 1))
fi

exit "$failures"
