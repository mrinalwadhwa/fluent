#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"

failures=0

stale_patterns=(
  "fluent work list"
  "fluent work show"
  "fluent work create"
  "fluent work attempt"
  "fluent work merge"
  "fluent work review"
  "fluent work task"
  "fluent work tester"
  "fluent work queue"
  "fluent work scheduler"
  "fluent work auto-merge"
  "fluent work post-merge-review"
  "fluent work status"
  "fluent work cleanup"
  "fluent work abandon"
  "fluent work review-codebase"
  "fluent work review-only-worktree"
  "fluent observations add"
  "fluent observations resolve"
  "fluent observations list"
  "fluent observations show"
  "fluent observations migrate"
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
        | grep -v 'test-build-in-fluent-command-reference\.sh' || true)
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
