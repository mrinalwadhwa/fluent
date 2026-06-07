#!/usr/bin/env bash
# test-review-boundary-docs - Verify review/run boundary documentation.

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
ARCH="$ROOT/documentation/architecture.md"

section="$(
  awk '
    /^### Review phase$/ { found=1; next }
    /^### / && found { exit }
    /^## / && found { exit }
    found { print }
  ' "$ARCH"
)"

if [ -z "$section" ]; then
  echo "architecture documentation has no Review phase section" >&2
  exit 1
fi

flat_section="$(printf '%s\n' "$section" | tr '\n' ' ' | tr -s ' ')"

for phrase in \
  'The review subsystem owns verdict parsing and acceptance rules' \
  '`review.rs` reads `review-state.json`' \
  'falls back to current' \
  '`run.rs` does not interpret review verdicts directly' \
  'delegates review acceptance to `review.rs`' \
  'durable run status (`status`) separate from review outcome semantics'
do
  if ! grep -Fq "$phrase" <<<"$flat_section"; then
    echo "review phase documentation does not contain: $phrase" >&2
    exit 1
  fi
done
