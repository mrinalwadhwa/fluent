#!/usr/bin/env bash
# test-core-work-model-task-definitions - Verify documented task definitions.

set -euo pipefail

root="$(cd "$(dirname "$0")/../../.." && pwd)"
arch="$root/documentation/architecture.md"

section="$(
  awk '
    /^## Core work model$/ { found=1; next }
    /^## / && found { exit }
    found { print }
  ' "$arch"
)"

if [[ -z "$section" ]]; then
  echo "architecture documentation has no Core work model section" >&2
  exit 1
fi

flat_section="$(printf '%s\n' "$section" | tr '\n' ' ' | tr -s ' ')"

for phrase in \
  'use the serialized `Task` shape from `factory::work_model` and call `Task::validate` after parsing' \
  'The `kind` field accepts `write`, `review`, `merge`, `report`, `learn`, `probe`, or `behavior-tests`' \
  '`workspace_access.reads` may list any number of workspaces' \
  '`workspace_access.writes` may be empty or contain one workspace' \
  'A `review` task must keep `writes` empty'
do
  if ! grep -Fq "$phrase" <<<"$flat_section"; then
    echo "core work model documentation does not contain: $phrase" >&2
    exit 1
  fi
done

cd "$root"
cargo test --test work_model_external
