#!/usr/bin/env bash
# test-architecture-active-modules - Verify active module docs stay current.

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
ARCH="$ROOT/documentation/architecture.md"

section="$(
  awk '
    /^## Active module responsibilities$/ { found=1; next }
    /^## / && found { exit }
    found { print }
  ' "$ARCH"
)"

if [ -z "$section" ]; then
  echo "architecture documentation has no Active module responsibilities section" >&2
  exit 1
fi

for module in 'hooks.rs' 'merge.rs' 'cleanup.rs' 'coder.rs'; do
  if ! grep -Fq "$module" <<<"$section"; then
    echo "architecture active module section does not mention $module" >&2
    exit 1
  fi
done

for concept in \
  '.factory/hooks/<name>' \
  'check-pre-<phase>' \
  'fix-pre-<phase>' \
  'check-pre-merge' \
  'fix-pre-merge' \
  'git worktree remove --force'
do
  if ! grep -Fq "$concept" <<<"$section"; then
    echo "architecture active module section does not document $concept" >&2
    exit 1
  fi
done

for module in 'hooks.rs' 'fargate_bootstrap.rs' 'cleanup.rs' 'merge.rs'; do
  if ! grep -Fq "$module" "$ARCH"; then
    echo "architecture repository structure does not list $module" >&2
    exit 1
  fi
done
