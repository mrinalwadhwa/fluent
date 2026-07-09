#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"

failures=0

# Every review role must exist as a skill directory with SKILL.md so
# build.rs bundles it and the reviewer materializes it from the binary
# without depending on the project-local skills/ directory.
ROLES=(architecture behaviors documentation skills tests)

for role in "${ROLES[@]}"; do
  skill="$ROOT/skills/review-${role}/SKILL.md"
  if [ ! -f "$skill" ]; then
    echo "review-${role} skill missing: ${skill}" >&2
    failures=$((failures + 1))
  fi
done

# build.rs must list skills/ in its rerun-if-changed so changes trigger
# a rebuild that re-bundles the review skills.
if ! grep -Fq 'cargo:rerun-if-changed=skills' "$ROOT/build.rs"; then
  echo "build.rs missing rerun-if-changed=skills" >&2
  failures=$((failures + 1))
fi

# review_skill_path resolves via project_root (materialized to
# .fluent/work/skills/), not by searching readable workspaces.
if grep -n 'fn review_skill_path' "$ROOT/src/work_task_executor.rs" | grep -q 'readable_workspaces'; then
  echo "review_skill_path still accepts readable_workspaces" >&2
  failures=$((failures + 1))
fi

# The sandbox readable_roots include the materialized skills directory.
if ! grep -Fq 'review_skills_dir(project_root)' "$ROOT/src/work_task_executor.rs"; then
  echo "sandbox readable_roots missing review_skills_dir" >&2
  failures=$((failures + 1))
fi

if [ "$failures" -gt 0 ]; then
  echo "${failures} failure(s)" >&2
  exit 1
fi

echo "review-skill-materialized: all checks passed"
