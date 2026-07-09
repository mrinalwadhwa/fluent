#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
SKILL="$ROOT/skills/build-in-the-fluent/SKILL.md"
FLUENT_BIN="${FLUENT_BIN_OVERRIDE:-$ROOT/target/debug/fluent}"

if [ ! -x "$FLUENT_BIN" ]; then
  (cd "$ROOT" && cargo build --quiet)
fi

failures=0

if ! grep -Fq '## Fluent commands' "$SKILL"; then
  echo "build-in-the-fluent lacks ## Fluent commands section" >&2
  failures=$((failures + 1))
fi

if ! grep -Fq 'fluent --help' "$SKILL"; then
  echo "build-in-the-fluent does not reference fluent --help" >&2
  failures=$((failures + 1))
fi

required_commands=(
  "fluent status"
  "fluent cleanup"
)

for command in "${required_commands[@]}"; do
  if ! grep -Fq "$command" "$SKILL"; then
    echo "missing ${command} from build-in-the-fluent skill" >&2
    failures=$((failures + 1))
  fi
done

deleted_commands=(
  "fluent run "
  "fluent watch"
  "fluent summary"
  "fluent resume"
  "fluent pull"
  "fluent shell"
  "fluent work "
  "fluent observations "
)

for command in "${deleted_commands[@]}"; do
  if grep -Fq "$command" "$SKILL"; then
    echo "deleted command ${command} still present in build-in-the-fluent skill" >&2
    failures=$((failures + 1))
  fi
done

for phrase in \
  "fluent attempt create" \
  "fluent attempt run" \
  "fluent merge-candidate show" \
  "fluent merge-candidate land" \
  "fluent work-item create"
do
  if ! grep -Fq "$phrase" "$SKILL"; then
    echo "missing Work-model command: ${phrase}" >&2
    failures=$((failures + 1))
  fi
done

if ! "$FLUENT_BIN" --help | grep -Eq '^  dashboard[[:space:]]'; then
  echo "fluent --help did not expose dashboard command" >&2
  failures=$((failures + 1))
fi

if [ "$failures" -ne 0 ]; then
  exit 1
fi
