#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
SKILL="$ROOT/skills/build-in-the-factory/SKILL.md"
FACTORY_BIN="${FACTORY_BIN_OVERRIDE:-$ROOT/target/debug/factory}"

if [ ! -x "$FACTORY_BIN" ]; then
  (cd "$ROOT" && cargo build --quiet)
fi

failures=0

if ! grep -Fq '## Factory commands' "$SKILL"; then
  echo "build-in-the-factory lacks ## Factory commands section" >&2
  failures=$((failures + 1))
fi

if ! grep -Fq 'factory --help' "$SKILL"; then
  echo "build-in-the-factory does not reference factory --help" >&2
  failures=$((failures + 1))
fi

required_commands=(
  "factory status"
  "factory work"
  "factory cleanup"
)

for command in "${required_commands[@]}"; do
  if ! grep -Fq "$command" "$SKILL"; then
    echo "missing ${command} from build-in-the-factory skill" >&2
    failures=$((failures + 1))
  fi
done

deleted_commands=(
  "factory run "
  "factory watch"
  "factory summary"
  "factory resume"
  "factory pull"
  "factory shell"
)

for command in "${deleted_commands[@]}"; do
  if grep -Fq "$command" "$SKILL"; then
    echo "deleted command ${command} still present in build-in-the-factory skill" >&2
    failures=$((failures + 1))
  fi
done

for phrase in \
  "factory work attempt" \
  "factory work merge-candidate" \
  "factory work merge" \
  "factory work create"
do
  if ! grep -Fq "$phrase" "$SKILL"; then
    echo "missing Work-model command: ${phrase}" >&2
    failures=$((failures + 1))
  fi
done

if ! "$FACTORY_BIN" --help | grep -Eq '^  dashboard[[:space:]]'; then
  echo "factory --help did not expose dashboard command" >&2
  failures=$((failures + 1))
fi

if [ "$failures" -ne 0 ]; then
  exit 1
fi
