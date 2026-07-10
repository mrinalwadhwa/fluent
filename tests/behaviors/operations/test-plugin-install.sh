#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"

readonly EXPECTED_SKILLS=(
  fluent
  review-architecture
  review-behaviors
  review-documentation
  review-skills
  review-tests
)

failures=0

fail() {
  printf 'FAIL: %s\n' "$1" >&2
  failures=$((failures + 1))
}

# --- Manifest files parse as valid JSON ---

marketplace="$ROOT/.claude-plugin/marketplace.json"
plugin="$ROOT/.claude-plugin/plugin.json"

if [ ! -f "$marketplace" ]; then
  fail ".claude-plugin/marketplace.json does not exist"
else
  if ! python3 -c "import json, sys; json.load(open(sys.argv[1]))" "$marketplace" 2>/dev/null; then
    fail ".claude-plugin/marketplace.json is not valid JSON"
  fi
fi

if [ ! -f "$plugin" ]; then
  fail ".claude-plugin/plugin.json does not exist"
else
  if ! python3 -c "import json, sys; json.load(open(sys.argv[1]))" "$plugin" 2>/dev/null; then
    fail ".claude-plugin/plugin.json is not valid JSON"
  fi
fi

# --- Conventional skills/ discovery yields exactly the six skills ---

discovered=()
for dir in "$ROOT"/skills/*/; do
  [ -f "${dir}SKILL.md" ] || continue
  name="$(basename "$dir")"
  discovered+=("$name")
done

IFS=$'\n' sorted_discovered=($(printf '%s\n' "${discovered[@]}" | sort))
IFS=$'\n' sorted_expected=($(printf '%s\n' "${EXPECTED_SKILLS[@]}" | sort))

if [ "${#sorted_discovered[@]}" -ne "${#sorted_expected[@]}" ]; then
  fail "expected ${#sorted_expected[@]} skills, found ${#sorted_discovered[@]}: ${sorted_discovered[*]}"
else
  for i in "${!sorted_expected[@]}"; do
    if [ "${sorted_discovered[$i]}" != "${sorted_expected[$i]}" ]; then
      fail "skill mismatch at position $i: expected '${sorted_expected[$i]}', found '${sorted_discovered[$i]}'"
    fi
  done
fi

if [ "$failures" -ne 0 ]; then
  exit 1
fi
