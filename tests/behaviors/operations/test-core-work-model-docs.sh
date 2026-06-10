#!/usr/bin/env bash
# test-core-work-model-docs - Verify core work model documentation.

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
ARCH="$ROOT/documentation/architecture.md"
BEHAVIORS="$ROOT/documentation/behaviors.md"

section="$(
  awk '
    /^## Core work model$/ { found=1; next }
    /^## / && found { exit }
    found { print }
  ' "$ARCH"
)"

if [ -z "$section" ]; then
  echo "architecture documentation has no Core work model section" >&2
  exit 1
fi

flat_section="$(printf '%s\n' "$section" | tr '\n' ' ' | tr -s ' ')"

for phrase in \
  'Work Item, Attempt, Task, Workspace, and Merge Candidate' \
  'commands remain supported as legacy compatibility' \
  'does not migrate run directories' \
  'Task kinds stay generic: `write`, `review`, `merge`, `report`, `learn`, and `probe`' \
  'A task may read many workspaces and write at most one' \
  'Review tasks are read-only with respect to candidate workspaces' \
  'Merge Candidate' \
  'review state is separate from attempt review state' \
  'Project-local `.factory/observations.md` and `.factory/expertise/*` are durable Factory memory' \
  'Legacy runtime compatibility state remains under `.factory/runs`'
do
  if ! grep -Fq "$phrase" <<<"$flat_section"; then
    echo "core work model documentation does not contain: $phrase" >&2
    exit 1
  fi
done

if ! grep -Fq 'work_model.rs' "$ARCH"; then
  echo "architecture repository structure does not list work_model.rs" >&2
  exit 1
fi

if ! grep -Fq '## Legacy sandbox compatibility (local)' "$BEHAVIORS"; then
  echo "behavior documentation does not label sandbox run contracts as legacy compatibility" >&2
  exit 1
fi

for phrase in \
  'WHEN `factory run --coder codex` is invoked with the sandboxed local runtime,' \
  'WHEN `factory run --coder codex --no-sandbox` is invoked,'
do
  if grep -Fq "$phrase" "$BEHAVIORS"; then
    echo "behavior documentation has an unlabeled legacy run contract: $phrase" >&2
    exit 1
  fi
done

for phrase in \
  'WHEN legacy `factory run --coder codex` is invoked with the sandboxed' \
  'WHEN legacy `factory run --coder codex --no-sandbox` is invoked,'
do
  if ! grep -Fq "$phrase" "$BEHAVIORS"; then
    echo "behavior documentation lacks legacy-labeled run contract: $phrase" >&2
    exit 1
  fi
done
