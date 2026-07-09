#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
SKILL_DIR="$ROOT/skills/fluent"
SKILL="$SKILL_DIR/SKILL.md"

failures=0

# SKILL.md exists and has the right frontmatter name
if [ ! -f "$SKILL" ]; then
  echo "fluent/SKILL.md does not exist" >&2
  exit 1
fi

if ! grep -q '^name: fluent$' "$SKILL"; then
  echo "fluent skill frontmatter name is not 'fluent'" >&2
  failures=$((failures + 1))
fi

# The four stage procedure references exist as real files
for stage in capture-brief define-behaviors design-approach plan-execution; do
  ref="$SKILL_DIR/references/${stage}.md"
  if [ ! -f "$ref" ]; then
    echo "missing stage reference: references/${stage}.md" >&2
    failures=$((failures + 1))
  fi
  if [ -L "$ref" ]; then
    echo "stage reference is a symlink, should be a real file: references/${stage}.md" >&2
    failures=$((failures + 1))
  fi
done

# SKILL.md points to each stage reference
for stage in capture-brief define-behaviors design-approach plan-execution; do
  if ! grep -Fq "references/${stage}.md" "$SKILL"; then
    echo "SKILL.md does not reference references/${stage}.md" >&2
    failures=$((failures + 1))
  fi
done

# The old standalone skill directories no longer exist
for old in build-in-the-fluent capture-brief define-behaviors design-approach plan-execution; do
  if [ -d "$ROOT/skills/${old}" ]; then
    echo "old standalone skill directory still exists: skills/${old}" >&2
    failures=$((failures + 1))
  fi
done

exit "$failures"
