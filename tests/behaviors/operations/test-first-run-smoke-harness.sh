#!/usr/bin/env bash
# test-first-run-smoke-harness - Verify the clean-room first-run smoke harness.
#
# The harness orchestrates Fluent's public first-run journey. These cases drive
# it entirely with local stateful doubles: a fake installer and a fake Fluent
# binary that records commands, enforces order, and emits controlled state. No
# case starts a model, changes the operator's home, or reaches the network.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$(dirname "$(dirname "$SCRIPT_DIR")")")"
HARNESS="${PROJECT_DIR}/scripts/first-run-smoke.sh"

source "${PROJECT_DIR}/tests/lib/run_test.sh"
LOG_DIR="${PROJECT_DIR}/tests/output/$(basename "$0" .sh)"

# ---------------------------------------------------------------------------
# Local doubles
# ---------------------------------------------------------------------------

# Write a stateful fake Fluent binary into <bin>/fluent. It records every
# invocation to $FAKE_CMD_LOG, rejects out-of-order commands, and drives the
# fixture repository so that `attempt run` produces a ready Merge Candidate and
# `merge-candidate land` merges it into main.
write_fake_fluent() {
  local bin_path="$1"
  cat > "$bin_path" <<'FAKE'
#!/usr/bin/env bash
set -euo pipefail

# State lives beside the binary so order can be enforced across invocations.
STATE_DIR="$(cd "$(dirname "$0")" && pwd)/.fake-state"
mkdir -p "$STATE_DIR"
STAGE_FILE="$STATE_DIR/stage"
[ -f "$STAGE_FILE" ] || printf 'installed\n' > "$STAGE_FILE"
stage() { cat "$STAGE_FILE"; }
set_stage() { printf '%s\n' "$1" > "$STAGE_FILE"; }

record() {
  [ -n "${FAKE_CMD_LOG:-}" ] && printf '%s\n' "$*" >> "$FAKE_CMD_LOG"
  return 0
}

reject() {
  printf 'fake-fluent: %s\n' "$1" >&2
  exit 1
}

CANDIDATE_ID="attempt-1-merge-candidate"

case "${1:-}" in
  --version|version)
    printf 'fluent 0.0.0-fake\n'
    ;;
  init)
    record "init"
    [ "$(stage)" = "installed" ] || reject "init out of order (stage=$(stage))"
    mkdir -p .fluent/work
    set_stage "init"
    ;;
  work-item)
    record "work-item $2"
    [ "$(stage)" = "init" ] || reject "work-item out of order (stage=$(stage))"
    set_stage "work-item"
    ;;
  attempt)
    sub="$2"
    record "attempt $sub"
    case "$sub" in
      create)
        [ "$(stage)" = "work-item" ] || reject "attempt create out of order"
        set_stage "attempt"
        ;;
      run)
        [ "$(stage)" = "attempt" ] || reject "attempt run out of order"
        if [ "${FAKE_ATTEMPT_RUN_FAILS:-0}" = "1" ]; then
          reject "simulated attempt run failure"
        fi
        # Produce the fix on a candidate branch, leaving main untouched.
        git checkout -q -b smoke-candidate
        printf 'hello\n' > greeting.txt
        git add greeting.txt
        git commit -q -m "Make greeting say hello"
        CANDIDATE_COMMIT="$(git rev-parse HEAD)"
        git checkout -q main
        mkdir -p .fluent/work/merge-candidates
        cat > ".fluent/work/merge-candidates/${CANDIDATE_ID}.json" <<JSON
{
  "id": "${CANDIDATE_ID}",
  "candidate_commit": "${CANDIDATE_COMMIT}",
  "candidate_branch": "smoke-candidate",
  "merge_state": { "status": "pending", "merged_commit": null }
}
JSON
        set_stage "ran"
        ;;
      *) reject "unknown attempt subcommand: $sub" ;;
    esac
    ;;
  merge-candidate)
    sub="$2"
    record "merge-candidate $sub"
    rec=".fluent/work/merge-candidates/${CANDIDATE_ID}.json"
    case "$sub" in
      show)
        [ -f "$rec" ] || reject "no such merge candidate"
        cat "$rec"
        ;;
      land)
        [ "$(stage)" = "ran" ] || reject "land out of order (stage=$(stage))"
        git merge -q --ff-only smoke-candidate
        MERGED="$(git rev-parse HEAD)"
        tmp="$(mktemp)"
        jq --arg c "$MERGED" '.merge_state.status = "merged" | .merge_state.merged_commit = $c' \
          "$rec" > "$tmp"
        mv "$tmp" "$rec"
        set_stage "landed"
        printf 'Merged Merge Candidate %s\n' "$CANDIDATE_ID"
        ;;
      *) reject "unknown merge-candidate subcommand: $sub" ;;
    esac
    ;;
  *)
    reject "unknown command: ${1:-}"
    ;;
esac
FAKE
  chmod +x "$bin_path"
}

# Write a fake installer that places the fake Fluent binary at --install-path.
write_fake_installer() {
  local installer_path="$1"
  cat > "$installer_path" <<INSTALLER
#!/usr/bin/env bash
set -euo pipefail
install_path=""
while [ \$# -gt 0 ]; do
  case "\$1" in
    --install-path) install_path="\$2"; shift 2 ;;
    *) shift ;;
  esac
done
[ -n "\$install_path" ] || { printf 'installer: missing --install-path\n' >&2; exit 1; }
mkdir -p "\$install_path"
cp "$FAKE_FLUENT_SRC" "\$install_path/fluent"
chmod +x "\$install_path/fluent"
INSTALLER
  chmod +x "$installer_path"
}

# ---------------------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------------------

new_workspace() {
  WORK="$(mktemp -d -t fluent-first-run-smoke-XXXXXX)"
  ROOT="$WORK/smoke"
  # A sentinel operator home the harness must never touch.
  REAL_HOME="$WORK/operator-home"
  mkdir -p "$REAL_HOME"
  printf 'untouched\n' > "$REAL_HOME/sentinel"
  # A prebuilt fake binary the doubles copy from.
  FAKE_FLUENT_SRC="$WORK/fake-fluent"
  write_fake_fluent "$FAKE_FLUENT_SRC"
  FAKE_CMD_LOG="$WORK/cmd-log"
  : > "$FAKE_CMD_LOG"
}

cleanup_workspace() {
  cd /
  rm -rf "$WORK"
}

# Run the harness with an isolated HOME and the fake command log wired in.
run_harness() {
  HOME="$REAL_HOME" FAKE_CMD_LOG="$FAKE_CMD_LOG" bash "$HARNESS" "$@"
}

assert_contains() {
  if ! printf '%s' "$1" | grep -Fq -- "$2"; then
    printf '    FAIL: output missing "%s"\n' "$2"
    printf '    Output:\n%s\n' "$1"
    return 1
  fi
}

# ---------------------------------------------------------------------------
# Cases
# ---------------------------------------------------------------------------

test_prepare_is_isolated() {
  new_workspace
  trap cleanup_workspace RETURN

  run_harness prepare "$ROOT" --binary "$FAKE_FLUENT_SRC" \
    > "$WORK/prepare.out" 2>&1

  local rc=0
  # The smoke root holds an isolated home, repository, evidence, and manifest.
  [ -d "$ROOT/home" ] || { printf '    FAIL: no isolated home\n'; rc=1; }
  [ -d "$ROOT/project/main/.git" ] || { printf '    FAIL: no git repository\n'; rc=1; }
  [ -d "$ROOT/evidence" ] || { printf '    FAIL: no evidence directory\n'; rc=1; }
  [ -f "$ROOT/harness/manifest.json" ] || { printf '    FAIL: no manifest\n'; rc=1; }

  # The fixture test fails on the seeded commit.
  if ( cd "$ROOT/project/main" && ./check.sh ) 2>/dev/null; then
    printf '    FAIL: fixture test should fail before the fix\n'; rc=1
  fi

  # The manifest records the prepared safe phase.
  [ "$(jq -r '.safe_phase' "$ROOT/harness/manifest.json")" = "prepared" ] \
    || { printf '    FAIL: safe_phase not prepared\n'; rc=1; }

  # The operator's home is untouched.
  [ "$(cat "$REAL_HOME/sentinel")" = "untouched" ] \
    || { printf '    FAIL: operator home was modified\n'; rc=1; }
  [ "$(ls -A "$REAL_HOME")" = "sentinel" ] \
    || { printf '    FAIL: operator home gained entries\n'; rc=1; }

  return $rc
}

test_prepare_rejects_nonempty_root() {
  new_workspace
  trap cleanup_workspace RETURN

  mkdir -p "$ROOT"
  printf 'x\n' > "$ROOT/stray"

  local rc=0
  if run_harness prepare "$ROOT" --binary "$FAKE_FLUENT_SRC" \
    > "$WORK/out" 2>&1; then
    printf '    FAIL: prepare accepted a nonempty root\n'; rc=1
  fi
  assert_contains "$(cat "$WORK/out")" "not empty" || rc=1
  return $rc
}

printf 'test-first-run-smoke-harness\n\n'

run_test "prepare is isolated" test_prepare_is_isolated
run_test "prepare rejects nonempty root" test_prepare_rejects_nonempty_root

summarize_and_exit
