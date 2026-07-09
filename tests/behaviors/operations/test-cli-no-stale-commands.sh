#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"

failures=0

stale_patterns=(
  "factory work list"
  "factory work show"
  "factory work create"
  "factory work attempt"
  "factory work merge"
  "factory work review"
  "factory work task"
  "factory work tester"
  "factory work queue"
  "factory work scheduler"
  "factory work auto-merge"
  "factory work post-merge-review"
  "factory work status"
  "factory work cleanup"
  "factory work abandon"
  "factory work review-codebase"
  "factory work review-only-worktree"
  "factory observations add"
  "factory observations resolve"
  "factory observations list"
  "factory observations show"
  "factory observations migrate"
)

scan_dirs=(
  "$ROOT/skills"
  "$ROOT/documentation"
  "$ROOT/infrastructure"
  "$ROOT/tests/behaviors"
)

for pattern in "${stale_patterns[@]}"; do
  for dir in "${scan_dirs[@]}"; do
    if [ -d "$dir" ]; then
      matches=$(grep -rn --include='*.md' --include='*.sh' --include='*.yaml' \
        -F "$pattern" "$dir" 2>/dev/null \
        | grep -v 'test-cli-no-stale-commands\.sh' \
        | grep -v 'test-build-in-factory-command-reference\.sh' || true)
      if [ -n "$matches" ]; then
        echo "stale command pattern '${pattern}' found:" >&2
        echo "$matches" | head -5 >&2
        failures=$((failures + 1))
      fi
    fi
  done
done

if [ "$failures" -ne 0 ]; then
  echo "${failures} stale command pattern(s) remain" >&2
  exit 1
fi
