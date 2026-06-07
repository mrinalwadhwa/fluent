#!/usr/bin/env bash
# test-skill-harness-artifacts - Verify printed harness artifacts exist.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$(dirname "$(dirname "$SCRIPT_DIR")")")"

PASS=0
FAIL=0
ERRORS=""

run_test() {
  TEST_NAME="$1"
  printf '  %s ... ' "$TEST_NAME"
  if ( eval "$2" ) 2>&1; then
    printf 'PASS\n'
    PASS=$((PASS + 1))
  else
    printf '\n'
    FAIL=$((FAIL + 1))
    ERRORS="${ERRORS}\n  - ${TEST_NAME}"
  fi
}

write_fake_claude() {
  FAKE_BIN="$(mktemp -d -t factory-test-skill-bin-XXXXXX)"

  cat > "${FAKE_BIN}/claude" <<'FAKE_CLAUDE'
#!/usr/bin/env bash
set -euo pipefail

args="$*"

if printf '%s' "$args" | grep -q 'You are a judge evaluating'; then
  printf 'Overall verdict: PASS\n'
elif [ "${FACTORY_FAKE_CLAUDE_ARTIFACT:-1}" = 0 ]; then
  printf 'The skill is complete.\n'
else
  cat <<'RESPONSE'
---ARTIFACT START---
# Brief

Add the timeout flag.
---ARTIFACT END---

The skill is complete.
RESPONSE
fi
FAKE_CLAUDE
  chmod +x "${FAKE_BIN}/claude"
}

run_skill_harness() {
  artifact_mode="$1"
  skill_path="$2"
  shift
  shift

  write_fake_claude
  OUTPUT="$(
    FACTORY_FAKE_CLAUDE_ARTIFACT="$artifact_mode" \
      PATH="${FAKE_BIN}:$PATH" \
      tests/test-skill tests/behaviors/skills/timeout-flag.md \
        "$skill_path" "$@" 2>&1
  )"
  RESULT_DIR="$(printf '%s\n' "$OUTPUT" | awk '
    /^Done[.] Results in / { print substr($0, 18); exit }
    /^Output: / { output_dir = substr($0, 9) }
    END { if (output_dir != "") print output_dir }
  ' | tail -n 1)"
  PRINTED_ARTIFACTS="$(printf '%s\n' "$OUTPUT" | awk '/^  [^ ]+[.]md[[:space:]]/ { print $1 }')"
}

assert_result_dir_exists() {
  if [ -z "$RESULT_DIR" ] || [ ! -d "$RESULT_DIR" ]; then
    printf '    FAIL: harness result directory was not printed or does not exist\n'
    return 1
  fi
}

assert_printed_artifact() {
  artifact="$1"
  description="$2"

  if ! printf '%s\n' "$PRINTED_ARTIFACTS" | grep -qx "$artifact"; then
    printf '    FAIL: harness did not print %s: %s\n' "$description" "$artifact"
    return 1
  fi
}

assert_not_printed_artifact() {
  artifact="$1"
  description="$2"

  if printf '%s\n' "$PRINTED_ARTIFACTS" | grep -qx "$artifact"; then
    printf '    FAIL: harness printed absent %s: %s\n' "$description" "$artifact"
    return 1
  fi
}

assert_printed_artifacts_exist() {
  RESULT=0

  if [ -z "$PRINTED_ARTIFACTS" ]; then
    printf '    FAIL: harness did not print any artifact paths\n'
    RESULT=1
  fi

  while IFS= read -r ARTIFACT; do
    [ -n "$ARTIFACT" ] || continue
    if [ ! -f "${RESULT_DIR}/${ARTIFACT}" ]; then
      printf '    FAIL: printed artifact does not exist: %s/%s\n' "$RESULT_DIR" "$ARTIFACT"
      RESULT=1
    fi
  done <<EOF_ARTIFACTS
$PRINTED_ARTIFACTS
EOF_ARTIFACTS

  return $RESULT
}

test_skill_harness_prints_existing_artifacts() {
  cd "$PROJECT_DIR"

  run_skill_harness 1 skills/capture-brief/SKILL.md --judge

  RESULT=0
  assert_result_dir_exists || return 1
  assert_printed_artifact "transcript.md" "transcript artifact" || RESULT=1
  assert_printed_artifact "brief.md" "brief artifact" || RESULT=1
  assert_printed_artifact "verdict.md" "judge artifact" || RESULT=1
  assert_printed_artifacts_exist || RESULT=1

  return $RESULT
}

test_skill_harness_omits_brief_without_artifact() {
  cd "$PROJECT_DIR"

  run_skill_harness 0 skills/capture-brief/SKILL.md --judge

  RESULT=0
  assert_result_dir_exists || return 1
  assert_printed_artifact "transcript.md" "transcript artifact" || RESULT=1
  assert_not_printed_artifact "brief.md" "brief artifact" || RESULT=1
  assert_printed_artifact "verdict.md" "judge artifact" || RESULT=1
  assert_printed_artifacts_exist || RESULT=1

  return $RESULT
}

test_skill_harness_omits_verdict_without_judge() {
  cd "$PROJECT_DIR"

  run_skill_harness 1 skills/capture-brief/SKILL.md

  RESULT=0
  assert_result_dir_exists || return 1
  assert_printed_artifact "transcript.md" "transcript artifact" || RESULT=1
  assert_printed_artifact "brief.md" "brief artifact" || RESULT=1
  assert_not_printed_artifact "verdict.md" "judge artifact" || RESULT=1
  assert_printed_artifacts_exist || RESULT=1

  return $RESULT
}

test_skill_harness_names_planning_artifacts() {
  cd "$PROJECT_DIR"

  RESULT=0

  run_skill_harness 1 skills/define-behaviors/SKILL.md --judge
  assert_result_dir_exists || return 1
  assert_printed_artifact "behaviors.diff.md" "define-behaviors artifact" || RESULT=1
  assert_not_printed_artifact "brief.md" "capture-brief artifact" || RESULT=1
  assert_printed_artifacts_exist || RESULT=1

  run_skill_harness 1 skills/design-approach/SKILL.md --judge
  assert_result_dir_exists || return 1
  assert_printed_artifact "approach.md" "design-approach artifact" || RESULT=1
  assert_not_printed_artifact "brief.md" "capture-brief artifact" || RESULT=1
  assert_printed_artifacts_exist || RESULT=1

  run_skill_harness 1 skills/plan-execution/SKILL.md --judge
  assert_result_dir_exists || return 1
  assert_printed_artifact "plan.md" "plan-execution artifact" || RESULT=1
  assert_not_printed_artifact "brief.md" "capture-brief artifact" || RESULT=1
  assert_printed_artifacts_exist || RESULT=1

  return $RESULT
}

printf 'test-skill-harness-artifacts\n\n'

run_test "skill harness prints existing artifacts" test_skill_harness_prints_existing_artifacts
run_test "skill harness omits brief without artifact" test_skill_harness_omits_brief_without_artifact
run_test "skill harness omits verdict without judge" test_skill_harness_omits_verdict_without_judge
run_test "skill harness names planning artifacts" test_skill_harness_names_planning_artifacts

printf '\n  %d passed, %d failed\n' "$PASS" "$FAIL"

if [ "$FAIL" -ne 0 ]; then
  printf '\nFailed tests:%b\n' "$ERRORS"
  exit 1
fi
