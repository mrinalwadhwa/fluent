#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
SHIM_DIR="$ROOT/skills/fluent"
SHIM="$SHIM_DIR/SKILL.md"
FULL_DIR="$ROOT/skills/fluent.full"
FULL="$FULL_DIR/fluent.md"

failures=0

# Shim SKILL.md exists and has the right frontmatter name
if [ ! -f "$SHIM" ]; then
  echo "fluent/SKILL.md (shim) does not exist" >&2
  exit 1
fi

if ! grep -q '^name: fluent$' "$SHIM"; then
  echo "fluent shim frontmatter name is not 'fluent'" >&2
  failures=$((failures + 1))
fi

# Shim carries the shim marker
if ! grep -q '^fluent-shim: true$' "$SHIM"; then
  echo "fluent shim missing fluent-shim: true marker" >&2
  failures=$((failures + 1))
fi

# Shim has no references directory
if [ -d "$SHIM_DIR/references" ]; then
  echo "fluent shim should not have a references/ directory" >&2
  failures=$((failures + 1))
fi

# Full skill body exists
if [ ! -f "$FULL" ]; then
  echo "fluent.full/fluent.md does not exist" >&2
  exit 1
fi

# The four stage procedure references exist as real files in fluent.full
for stage in capture-brief define-behaviors design-approach plan-execution; do
  ref="$FULL_DIR/references/${stage}.md"
  if [ ! -f "$ref" ]; then
    echo "missing stage reference: fluent.full/references/${stage}.md" >&2
    failures=$((failures + 1))
  fi
  if [ -L "$ref" ]; then
    echo "stage reference is a symlink, should be a real file: fluent.full/references/${stage}.md" >&2
    failures=$((failures + 1))
  fi
done

# Full skill body points to each stage reference
for stage in capture-brief define-behaviors design-approach plan-execution; do
  if ! grep -Fq "references/${stage}.md" "$FULL"; then
    echo "fluent.full/fluent.md does not reference references/${stage}.md" >&2
    failures=$((failures + 1))
  fi
done

# fluent.full has no SKILL.md (so the skills CLI won't discover it)
if [ -f "$FULL_DIR/SKILL.md" ]; then
  echo "fluent.full/ must not have a SKILL.md (would be discovered by skills CLI)" >&2
  failures=$((failures + 1))
fi

# The old standalone skill directories no longer exist
for old in build-in-the-fluent capture-brief define-behaviors design-approach plan-execution; do
  if [ -d "$ROOT/skills/${old}" ]; then
    echo "old standalone skill directory still exists: skills/${old}" >&2
    failures=$((failures + 1))
  fi
done

exit "$failures"
