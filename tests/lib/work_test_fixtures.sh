#!/usr/bin/env bash
# Shared fixture helpers for work-model behavior tests.

# Seed minimal review-skill stubs for all five review roles.
# Usage: seed_review_skill_stubs <workspace_path>
seed_review_skill_stubs() {
  local workspace="$1"
  for role in documentation behaviors architecture skills tests; do
    mkdir -p "${workspace}/skills/review-${role}"
    printf 'stub\n' > "${workspace}/skills/review-${role}/SKILL.md"
  done
}

# Seed a minimal tester-results.json artifact so review tasks that
# depend on the tester can find their input artifact.
# Usage: seed_tester_results <project_root> <work_item_id> <attempt_id>
seed_tester_results() {
  local project_root="$1"
  local work_item_id="$2"
  local attempt_id="$3"
  local tester_id="${attempt_id}-tester"
  local dir="${project_root}/.factory/work/artifacts/${work_item_id}/${attempt_id}/${tester_id}"
  mkdir -p "$dir"
  printf '[]\n' > "${dir}/tester-results.json"
}
