#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
SKILL="$ROOT/skills/build-in-the-factory/SKILL.md"
FACTORY_BIN="${FACTORY_BIN_OVERRIDE:-$ROOT/target/debug/factory}"

if [ ! -x "$FACTORY_BIN" ]; then
  (cd "$ROOT" && cargo build --quiet)
fi

extract_reference() {
  awk '
    /^## Factory commands$/ { in_section = 1; next }
    in_section && /^```sh$/ { in_block = 1; next }
    in_block && /^```$/ { exit }
    in_block { print }
  ' "$SKILL"
}

reference="$(extract_reference)"
help_text="$("$FACTORY_BIN" --help)"
failures=0

while IFS= read -r line; do
  [ -n "$line" ] || continue

  command="$(awk '{ print $2 }' <<<"$line")"
  description="${line#*# }"

  if [ "$description" = "$line" ]; then
    echo "factory ${command} lacks an inline description" >&2
    failures=$((failures + 1))
    continue
  fi

  if [ "${#description}" -gt 64 ]; then
    echo "factory ${command} description is not concise: ${description}" >&2
    failures=$((failures + 1))
  fi

  if ! grep -Eq "^  ${command}[[:space:]]" <<<"$help_text"; then
    echo "factory ${command} is not present in factory --help" >&2
    failures=$((failures + 1))
  fi
done <<<"$reference"

for required_phrase in \
  "factory work create" \
  "factory work attempt" \
  "factory work task run" \
  "factory work merge-candidate" \
  "factory work merge"
do
  if ! grep -Fq "$required_phrase" <<<"$reference"; then
    echo "build-in-the-factory command reference lacks ${required_phrase}" >&2
    failures=$((failures + 1))
  fi
done

if [ "$failures" -ne 0 ]; then
  exit 1
fi
