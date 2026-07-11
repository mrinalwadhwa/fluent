#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
REVIEW_USER="$ROOT/prompts/review-user.md"
REVIEW_ONLY="$ROOT/prompts/review-only-user.md"

failures=0
total=0

check() {
  total=$((total + 1))
  local file="$1" label="$2" pattern="$3"
  if ! grep -qiF "$pattern" "$file"; then
    printf 'FAIL: %s does not contain "%s"\n' "$label" "$pattern" >&2
    failures=$((failures + 1))
  fi
}

# B1 — removal claims must be grounded in the diff
check "$REVIEW_USER" "review-user.md" "removal claim"
check "$REVIEW_USER" "review-user.md" "diff does not support"
check "$REVIEW_ONLY" "review-only-user.md" "removal claim"
check "$REVIEW_ONLY" "review-only-user.md" "diff does not support"

# B2 — design decisions route to uncertain, not fail
check "$REVIEW_USER" "review-user.md" "design decision"
check "$REVIEW_USER" "review-user.md" "uncertain"
check "$REVIEW_ONLY" "review-only-user.md" "design decision"
check "$REVIEW_ONLY" "review-only-user.md" "uncertain"

# Both prompts list uncertain as a valid verdict
check "$REVIEW_USER" "review-user.md" "uncertain"
check "$REVIEW_ONLY" "review-only-user.md" "uncertain"

printf 'review-verdict-rules: %d/%d checks passed\n' "$((total - failures))" "$total"
exit "$failures"
