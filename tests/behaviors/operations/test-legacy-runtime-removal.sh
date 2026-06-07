#!/usr/bin/env bash
# test-legacy-runtime-removal — Verify shipped behavior no longer names scripts/factory.

set -euo pipefail

PROJECT_DIR="$(cd "$(dirname "$0")/../../.." && pwd)"
RESULT=0

if [ -e "${PROJECT_DIR}/scripts/factory" ]; then
  printf 'FAIL: scripts/factory still exists\n'
  RESULT=1
fi

if grep -R -F "scripts/factory" \
    --exclude "test-legacy-runtime-removal.sh" \
    "${PROJECT_DIR}/documentation" \
    "${PROJECT_DIR}/skills" \
    "${PROJECT_DIR}/tests/behaviors" \
    "${PROJECT_DIR}/tests/test-run" >/tmp/factory-legacy-runtime-grep.$$ 2>/dev/null; then
  printf 'FAIL: user-facing docs or behavior tests still reference scripts/factory\n'
  cat /tmp/factory-legacy-runtime-grep.$$
  RESULT=1
fi
rm -f /tmp/factory-legacy-runtime-grep.$$

if [ "$RESULT" -eq 0 ]; then
  printf 'PASS: legacy scripts/factory runtime is absent from docs and behavior coverage\n'
fi

exit "$RESULT"
