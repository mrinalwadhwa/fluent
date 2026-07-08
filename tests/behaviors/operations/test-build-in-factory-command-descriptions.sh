#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
SKILL="$ROOT/skills/build-in-the-factory/SKILL.md"
FACTORY_BIN="${FACTORY_BIN_OVERRIDE:-$ROOT/target/debug/factory}"

if [ ! -x "$FACTORY_BIN" ]; then
  (cd "$ROOT" && cargo build --quiet)
fi

help_text="$("$FACTORY_BIN" --help)"
failures=0

while IFS= read -r subcommand; do
  [ -n "$subcommand" ] || continue
  if ! grep -Eq "^  ${subcommand}[[:space:]]" <<<"$help_text"; then
    echo "factory ${subcommand} referenced in skill but not in factory --help" >&2
    failures=$((failures + 1))
  fi
done < <(
  grep -oE '`factory [a-z][a-z-]*' "$SKILL" \
    | sed 's/`factory //' \
    | sort -u
)

if [ "$failures" -ne 0 ]; then
  exit 1
fi
