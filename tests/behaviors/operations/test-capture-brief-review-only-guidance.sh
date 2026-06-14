#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
SKILL="$ROOT/skills/capture-brief/SKILL.md"

failures=0

require_guidance() {
  local phrase="$1"

  if ! grep -Fq "$phrase" "$SKILL"; then
    echo "capture-brief lacks required review-only guidance: $phrase" >&2
    failures=$((failures + 1))
  fi
}

require_guidance "The review-only path currently runs the default reviewer set."
require_guidance "Confirm that the user accepts the default reviewer set"
require_guidance "review-only Attempt"

if [ "$failures" -ne 0 ]; then
  exit 1
fi
