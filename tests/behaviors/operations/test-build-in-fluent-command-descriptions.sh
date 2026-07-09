#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
SKILL="$ROOT/skills/fluent/SKILL.md"
FLUENT_BIN="${FLUENT_BIN_OVERRIDE:-$ROOT/target/debug/fluent}"

if [ ! -x "$FLUENT_BIN" ]; then
  (cd "$ROOT" && cargo build --quiet)
fi

help_text="$("$FLUENT_BIN" --help)"
failures=0

while IFS= read -r subcommand; do
  [ -n "$subcommand" ] || continue
  if ! grep -Eq "^  ${subcommand}[[:space:]]" <<<"$help_text"; then
    echo "fluent ${subcommand} referenced in skill but not in fluent --help" >&2
    failures=$((failures + 1))
  fi
done < <(
  grep -oE '`fluent [a-z][a-z-]*' "$SKILL" \
    | sed 's/`fluent //' \
    | sort -u
)

if [ "$failures" -ne 0 ]; then
  exit 1
fi
