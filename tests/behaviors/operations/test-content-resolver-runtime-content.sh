#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
DOC="$ROOT/documentation/architecture.md"

section="$(
  awk '
    /^## Content resolution$/ { in_section = 1; next }
    in_section && /^## / { exit }
    in_section { print }
  ' "$DOC"
)"

if [ -z "$section" ]; then
  echo "architecture documentation has no Content resolution section" >&2
  exit 1
fi

if ! grep -Eq '`ContentResolver` resolves runtime content that the Factory binary reads' <<<"$section"; then
  echo "architecture documentation does not identify ContentResolver as resolving runtime content read by the Factory binary" >&2
  exit 1
fi
