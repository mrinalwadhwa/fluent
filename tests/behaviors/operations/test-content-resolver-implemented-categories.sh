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

if ! grep -Eq 'implemented runtime content categories are[[:space:]]+prompts under `prompts/` and sandbox profiles under `sandbox/`' <<<"$flat_section"; then
  echo "architecture documentation does not limit implemented ContentResolver categories to prompts and sandbox profiles" >&2
  exit 1
fi

if grep -Eq 'implemented runtime content categories are[^.]*\b(skills|expertise)\b' <<<"$flat_section"; then
  echo "architecture documentation lists skills or expertise as implemented ContentResolver runtime content categories" >&2
  exit 1
fi
