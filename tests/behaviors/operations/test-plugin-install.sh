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

# --- The fluent skill carries the shim marker and has no references ---

fluent_skill="$ROOT/skills/fluent/SKILL.md"
if [ ! -f "$fluent_skill" ]; then
  fail "skills/fluent/SKILL.md does not exist"
elif ! grep -q 'fluent-shim: true' "$fluent_skill"; then
  fail "skills/fluent/SKILL.md does not carry the fluent-shim marker"
fi

if [ -d "$ROOT/skills/fluent/references" ]; then
  fail "skills/fluent/ should not have a references/ directory (shim must be minimal)"
fi

# --- The full skill lives in fluent.full/ and is not a skill directory ---

if [ ! -f "$ROOT/skills/fluent.full/fluent.md" ]; then
  fail "skills/fluent.full/fluent.md (full skill body) does not exist"
fi
if [ ! -d "$ROOT/skills/fluent.full/references" ]; then
  fail "skills/fluent.full/references/ (full skill references) does not exist"
fi
if [ -f "$ROOT/skills/fluent.full/SKILL.md" ]; then
  fail "skills/fluent.full/ must not have a SKILL.md (would make it a discoverable skill)"
fi

if [ "$failures" -ne 0 ]; then
  exit 1
fi
