#!/usr/bin/env bash
# test-architecture-model-env - Verify model-selection env vars are documented.

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
ARCH="$ROOT/documentation/architecture.md"

section="$(
  awk '
    /^### Model selection environment$/ { found=1; next }
    /^### / && found { exit }
    /^## / && found { exit }
    found { print }
  ' "$ARCH"
)"

if [ -z "$section" ]; then
  echo "architecture documentation has no Model selection environment section" >&2
  exit 1
fi

for env_var in \
  'FACTORY_CLAUDE_MODEL' \
  'FACTORY_MODEL' \
  'FACTORY_CODEX_MODEL' \
  'FACTORY_CODER' \
  'FACTORY_CODEX_CA_BUNDLE'
do
  if ! grep -Fq "$env_var" <<<"$section"; then
    echo "model environment section does not mention $env_var" >&2
    exit 1
  fi
done

if ! grep -Fq 'claude-opus-4-6' <<<"$section"; then
  echo "model environment section does not document Claude default model" >&2
  exit 1
fi

if ! grep -Fq 'Codex CLI default' <<<"$section"; then
  echo "model environment section does not document Codex default delegation" >&2
  exit 1
fi
