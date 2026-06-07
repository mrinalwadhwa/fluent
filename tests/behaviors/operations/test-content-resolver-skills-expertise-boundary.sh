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

flat_section="$(tr '\n' ' ' <<<"$section")"

if ! grep -Fq 'Skills and expertise are outside this resolver boundary.' <<<"$section"; then
  echo "architecture documentation does not state that skills and expertise are outside the resolver boundary" >&2
  exit 1
fi

if ! grep -Eq 'Factory does not currently bundle or resolve[[:space:]]+skills and expertise through `ContentResolver`' <<<"$flat_section"; then
  echo "architecture documentation implies or fails to rule out ContentResolver resolution for skills and expertise" >&2
  exit 1
fi
