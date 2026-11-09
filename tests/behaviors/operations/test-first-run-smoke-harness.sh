#!/usr/bin/env bash
# test-first-run-smoke-harness - Verify the clean-room first-run smoke harness.
#
# The harness models Fluent's first-run journey from public-shaped bootstrap
# output onward. These cases drive it entirely with local stateful doubles: a
# fake installer and a fake Fluent binary that records commands, enforces order,
# and emits controlled state. No case starts a model, changes the operator's
# home, or reaches the network.

set -euo pipefail

export FLUENT_SMOKE_QUEUE_POLL_SECONDS=0.01

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
# fixture repository so the scheduler produces a ready Merge Candidate and
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

# Record whether the fake scheduler produced a candidate. The Attempt response
# can still model an inconsistent candidate-ready dispatch whose Learner did
# not succeed, so the harness's independent readiness check is exercised.
RUNS_FILE="$STATE_DIR/runs"
[ -f "$RUNS_FILE" ] || printf '0\n' > "$RUNS_FILE"
runs() { cat "$RUNS_FILE"; }
# Set to 0 to leave the Learner in progress after scheduler completion.
LEARNER_SUCCEEDS="${FAKE_LEARNER_SUCCEEDS:-1}"
learning_status() {
  if [ "$LEARNER_SUCCEEDS" = "1" ]; then
    printf 'succeeded'
  else
    printf 'in-progress'
  fi
}

attempt_status() {
  if [ -n "${FAKE_ATTEMPT_STATUS:-}" ]; then
    printf '%s' "$FAKE_ATTEMPT_STATUS"
  elif [ "$(learning_status)" = "succeeded" ]; then
    printf 'complete'
  else
    printf 'executing'
  fi
}

QUEUE_STATUS_FILE="$STATE_DIR/queue-status"
queue_status() {
  [ -f "$QUEUE_STATUS_FILE" ] && cat "$QUEUE_STATUS_FILE" || printf 'none'
}
set_queue_status() {
  local temporary
  temporary="$(mktemp "${QUEUE_STATUS_FILE}.XXXXXX")"
  printf '%s\n' "$1" > "$temporary"
  mv "$temporary" "$QUEUE_STATUS_FILE"
}
QUEUE_ATTEMPT_FILE="$STATE_DIR/queue-attempt"
queue_attempt() {
  [ -f "$QUEUE_ATTEMPT_FILE" ] && cat "$QUEUE_ATTEMPT_FILE" || printf 'none'
}
set_queue_attempt() {
  local temporary
  temporary="$(mktemp "${QUEUE_ATTEMPT_FILE}.XXXXXX")"
  printf '%s\n' "$1" > "$temporary"
  mv "$temporary" "$QUEUE_ATTEMPT_FILE"
}

produce_candidate() {
  if [ "$(stage)" != "ran" ]; then
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
    printf '1\n' > "$RUNS_FILE"
    set_stage "ran"
  fi
}

# When set, emit malformed JSON for these commands (exit 0) so the caller's
# jq parse step fails rather than the command itself.
# FAKE_QUEUE_SHOW_MALFORMED=1          — queue show returns invalid JSON
# FAKE_ATTEMPT_SHOW_MALFORMED=1        — attempt show returns invalid JSON
# FAKE_CANDIDATE_SHOW_MALFORMED=1      — merge-candidate show returns invalid JSON
# FAKE_LANDED_SHOW_MALFORMED=1         — merge-candidate show returns invalid JSON
#                                        only when stage=landed (post-land show)
# FAKE_CANDIDATE_SHOW_UNKNOWN_STATUS=1 — merge-candidate show returns valid JSON
#                                        but with an unrecognised merge_state.status
#                                        (not "pending" or "merged"), only at stage=ran

record() {
  [ -n "${FAKE_CMD_LOG:-}" ] && printf '%s\n' "$*" >> "$FAKE_CMD_LOG"
  [ -n "${FAKE_CODEX_HOME_LOG:-}" ] && printf '%s\n' "${CODEX_HOME:-}" >> "$FAKE_CODEX_HOME_LOG"
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
  skills)
    record "skills $2"
    [ "$2" = "add" ] || reject "unknown skills subcommand: $2"
    case "$(stage)" in installed|skills) ;; *) reject "skills add out of order (stage=$(stage))" ;; esac
    if [ "${FAKE_SKILLS_ADD_FAILS:-0}" = "1" ]; then
      reject "simulated skill handoff failure"
    fi
    skill_dir="$PWD/.agents/skills/fluent"
    [ -f "$skill_dir/SKILL.md" ] || reject "missing bootstrap shim"
    if ! grep -Fq 'fluent-shim: true' "$skill_dir/SKILL.md" \
      && [ ! -f "$skill_dir/.fluent-managed.json" ]; then
      reject "missing shim marker or ownership sidecar"
    fi
    cat > "$skill_dir/SKILL.md" <<'FULL_SKILL'
---
name: fluent
description: Full managed Fluent skill.
---

# Fluent
FULL_SKILL
    cat > "$skill_dir/.fluent-managed.json" <<'SIDECAR'
{"schema_version":1,"agent":"universal","scope":"project","skill":"fluent"}
SIDECAR
    if [ "${FAKE_SKILLS_BAD_SIDECAR:-0}" = "1" ]; then
      jq '.agent = "wrong"' "$skill_dir/.fluent-managed.json" \
        > "$skill_dir/.fluent-managed.json.tmp"
      mv "$skill_dir/.fluent-managed.json.tmp" "$skill_dir/.fluent-managed.json"
    fi
    lock_tmp="$(mktemp)"
    jq 'del(.skills.fluent)' skills-lock.json > "$lock_tmp"
    mv "$lock_tmp" skills-lock.json
    set_stage "skills"
    ;;
  init)
    record "init $*"
    [ "$#" = 5 ] \
      && [ "$2" = "--coder-profile" ] \
      && [ "$4" = "--follow-up-mode" ] && [ "$5" = "propose" ] \
      || reject "init must configure a coder profile and follow-up mode"
    case "$3" in
      codex-balanced|codex-stronger|claude-balanced|opencode-default|codex-write-claude-review|claude-write-codex-review) ;;
      *) reject "unsupported coder profile: $3" ;;
    esac
    case "$(stage)" in skills|init) ;; *) reject "init out of order (stage=$(stage))" ;; esac
    mkdir -p .fluent/work
    printf '# Fluent instructions\n' > AGENTS.md
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
      show)
        if [ "${FAKE_ATTEMPT_SHOW_MALFORMED:-0}" = "1" ]; then
          printf '{bad json\n'
          exit 0
        fi
        # Expose the stored Attempt as JSON, including its Learner state.
        LEARN="$(learning_status)"
        ASTATUS="$(attempt_status)"
        cat <<JSON
{
  "id": "attempt-1",
  "status": "${ASTATUS}",
  "learning": { "status": "${LEARN}", "runs": $(runs) }
}
JSON
        ;;
      *) reject "unknown attempt subcommand: $sub" ;;
    esac
    ;;
  queue)
    sub="$2"
    record "queue $sub"
    case "$sub" in
      add)
        [ "$(stage)" = "attempt" ] || reject "queue add out of order (stage=$(stage))"
        set_queue_status "queued"
        set_queue_attempt "none"
        set_stage "queued"
        ;;
      list)
        case "$(queue_status)" in
          queued|claimed|running)
            printf '0 2026-08-07T00:00:00Z %s clean-room-fixture\n' "$(queue_status)"
            ;;
          *)
            printf 'Queue is empty\n'
            ;;
        esac
        ;;
      show)
        [ "$3" = "clean-room-fixture" ] || reject "queue show must name the Work Item"
        if [ "${FAKE_QUEUE_SHOW_MALFORMED:-0}" = "1" ]; then
          printf '{bad json\n'
          exit 0
        fi
        visible_status="$(queue_status)"
        [ "$visible_status" != "none" ] || reject "no queue dispatch"
        if [ "${FAKE_QUEUE_SHOW_STALE_ONCE_AFTER_TERMINAL:-0}" = "1" ] \
          && [ "$visible_status" = "candidate-ready" ] \
          && [ ! -f "$STATE_DIR/stale-queue-used" ]; then
          : > "$STATE_DIR/stale-queue-used"
          visible_status="queued"
        fi
        if [ "$(queue_attempt)" = "none" ]; then
          jq -n --arg status "$visible_status" '{
            dispatch_id: "dispatch-1",
            work_item_id: "clean-room-fixture",
            queued_at: "2026-08-07T00:00:00Z",
            priority: 0,
            status: $status,
            generation: 0
          }'
        else
          jq -n --arg status "$visible_status" --arg attempt "$(queue_attempt)" '{
            dispatch_id: "dispatch-1",
            work_item_id: "clean-room-fixture",
            queued_at: "2026-08-07T00:00:00Z",
            priority: 0,
            status: $status,
            generation: 0,
            bound_attempt_id: $attempt
          }'
        fi
        ;;
      resume)
        [ "$(queue_status)" = "needs-user" ] || reject "queue resume without pause"
        [ "$3" = "clean-room-fixture" ] \
          && [ "$4" = "--attempt" ] && [ "$5" = "attempt-1" ] \
          || reject "queue resume must bind attempt-1"
        set_queue_status "queued"
        set_queue_attempt "attempt-1"
        set_stage "queued"
        ;;
      *) reject "unknown queue subcommand: $sub" ;;
    esac
    ;;
  scheduler)
    record "scheduler $2"
    [ "$2" = "run" ] || reject "unknown scheduler subcommand: $2"
    [ "$(queue_status)" = "queued" ] || reject "scheduler requires queued work"
    set_queue_attempt "attempt-1"
    if [ "${FAKE_SCHEDULER_HANGS:-0}" = "1" ]; then
      set_queue_status "running"
      [ -z "${FAKE_SCHEDULER_PID_FILE:-}" ] \
        || printf '%s\n' "$$" > "$FAKE_SCHEDULER_PID_FILE"
      trap 'exit 0' INT TERM
      while :; do sleep 0.05; done
    fi
    if [ "${FAKE_SCHEDULER_NEEDS_USER_ONCE:-0}" = "1" ] \
      && [ ! -f "$STATE_DIR/provider-pause-used" ]; then
      : > "$STATE_DIR/provider-pause-used"
      set_queue_status "needs-user"
      set_stage "needs-user"
      exit 0
    fi
    if [ "${FAKE_SCHEDULER_FAILS:-0}" = "1" ]; then
      set_queue_status "failed"
      exit 0
    fi
    produce_candidate
    set_queue_status "candidate-ready"
    ;;
  merge-candidate)
    sub="$2"
    record "merge-candidate $sub"
    rec=".fluent/work/merge-candidates/${CANDIDATE_ID}.json"
    case "$sub" in
      show)
        [ -f "$rec" ] || reject "no such merge candidate"
        # Model a post-land verification failure: the merge already happened
        # (stage=landed) but the harness's candidate-show step trips once. A
        # resume with this flag cleared must complete without re-landing.
        if [ "${FAKE_POST_LAND_SHOW_FAILS:-0}" = "1" ] && [ "$(stage)" = "landed" ]; then
          reject "simulated post-land show failure"
        fi
        # Model a malformed-JSON response: the command exits 0 but the content
        # is not valid JSON, so any jq parse on it will fail.
        if [ "${FAKE_CANDIDATE_SHOW_MALFORMED:-0}" = "1" ]; then
          printf '{bad json\n'
          exit 0
        fi
        # Same but only for the post-land show (stage=landed), leaving the
        # precheck (still stage=ran) unaffected.
        if [ "${FAKE_LANDED_SHOW_MALFORMED:-0}" = "1" ] && [ "$(stage)" = "landed" ]; then
          printf '{bad json\n'
          exit 0
        fi
        # Return syntactically valid JSON with an unrecognised merge_state.status
        # so the harness receives a well-formed document but must reject the
        # unknown state rather than treating it as permission to land.
        if [ "${FAKE_CANDIDATE_SHOW_UNKNOWN_STATUS:-0}" = "1" ] && [ "$(stage)" = "ran" ]; then
          jq '.merge_state.status = "unknown_state"' "$rec"
          exit 0
        fi
        cat "$rec"
        ;;
      land)
        [ "$(stage)" = "ran" ] || reject "land out of order (stage=$(stage))"
        if [ "${FAKE_LAND_FAILS:-0}" = "1" ]; then
          reject "simulated land failure"
        fi
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
# It records the HOME it ran with to $INSTALLER_HOME_LOG so a test can prove the
# harness runs the installer under the isolated smoke home, never the operator's.
write_fake_installer() {
  local installer_path="$1"
  cat > "$installer_path" <<INSTALLER
#!/usr/bin/env bash
set -euo pipefail
[ -n "\${INSTALLER_HOME_LOG:-}" ] && printf '%s\n' "\$HOME" > "\$INSTALLER_HOME_LOG"
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

# Write a fake `git` that fails on any invocation, into <dir>/git. Placed first
# on PATH, it makes the prepare phase's repository seeding fail so the mid-seed
# failure-and-resume contract can be exercised deterministically.
write_failing_git() {
  local dir="$1"
  mkdir -p "$dir"
  cat > "$dir/git" <<'GIT'
#!/usr/bin/env bash
printf 'git: simulated seeding failure\n' >&2
exit 1
GIT
  chmod +x "$dir/git"
}

# Write a fake `mkdir` that fails when asked to create a specific target, into
# <dir>/mkdir. Placed first on PATH with $FAILING_MKDIR_MATCH set, it makes one
# prepare-phase directory-setup call fail so the durable failure-and-resume
# contract can be exercised without depending on real permission errors.
write_failing_mkdir() {
  local dir="$1"
  mkdir -p "$dir"
  local real_mkdir
  real_mkdir="$(command -v mkdir)"
  cat > "$dir/mkdir" <<MKDIR
#!/usr/bin/env bash
for a in "\$@"; do
  if [ "\$a" = "\${FAILING_MKDIR_MATCH:-}" ]; then
    printf 'mkdir: simulated directory-setup failure: %s\n' "\$a" >&2
    exit 1
  fi
done
exec "$real_mkdir" "\$@"
MKDIR
  chmod +x "$dir/mkdir"
}

# Write a fake `cp` that fails when the destination matches $FAILING_CP_DEST,
# into <dir>/cp. Placed first on PATH, it makes one evidence-copy call fail
# so the durable failure-and-resume contract can be exercised.
write_failing_cp() {
  local dir="$1"
  mkdir -p "$dir"
  local real_cp
  real_cp="$(command -v cp)"
  cat > "$dir/cp" <<CP
#!/usr/bin/env bash
last=''
for a in "\$@"; do last="\$a"; done
if [ "\${FAILING_CP_DEST:-}" != "" ] && [ "\$last" = "\$FAILING_CP_DEST" ]; then
  printf 'cp: simulated copy failure to: %s\n' "\$last" >&2
  exit 1
fi
exec "$real_cp" "\$@"
CP
  chmod +x "$dir/cp"
}

# Write a fake `mv` that fails when the destination arg matches $FAILING_MV_DEST,
# into <dir>/mv. Placed first on PATH, it makes one atomic manifest rename fail
# so the manifest-finalization failure-and-resume contract can be exercised.
write_failing_mv() {
  local dir="$1"
  mkdir -p "$dir"
  local real_mv
  real_mv="$(command -v mv)"
  cat > "$dir/mv" <<MV
#!/usr/bin/env bash
last=''
for a in "\$@"; do last="\$a"; done
if [ "\${FAILING_MV_DEST:-}" != "" ] && [ "\$last" = "\${FAILING_MV_DEST}" ]; then
  printf 'mv: simulated rename failure to: %s\n' "\$last" >&2
  exit 1
fi
exec "$real_mv" "\$@"
MV
  chmod +x "$dir/mv"
}

# Write a fake `rm` that fails when any non-flag argument matches $FAILING_RM_TARGET,
# into <dir>/rm. Placed first on PATH, it makes one marker-removal call fail so
# the prepare-marker-cleanup failure-and-resume contract can be exercised.
write_failing_rm() {
  local dir="$1"
  mkdir -p "$dir"
  local real_rm
  real_rm="$(command -v rm)"
  cat > "$dir/rm" <<RM
#!/usr/bin/env bash
for a in "\$@"; do
  case "\$a" in
    -*) continue ;;
  esac
  if [ "\${FAILING_RM_TARGET:-}" != "" ] && [ "\$a" = "\${FAILING_RM_TARGET}" ]; then
    printf 'rm: simulated removal failure: %s\n' "\$a" >&2
    exit 1
  fi
done
exec "$real_rm" "\$@"
RM
  chmod +x "$dir/rm"
}

# Write a fake `jq` that crashes mid-write for a manifest update, into <dir>/jq.
# manifest_set invokes `jq --arg v <value> ...`; the fake emits a partial JSON
# document to stdout and exits non-zero for exactly that form, modelling a crash
# during a durable checkpoint. All other jq calls (reads, `jq -n`) pass through.
write_crashing_jq() {
  local dir="$1"
  mkdir -p "$dir"
  local real_jq
  real_jq="$(command -v jq)"
  cat > "$dir/jq" <<JQ
#!/usr/bin/env bash
if [ "\${1:-}" = "--arg" ] && [ "\${2:-}" = "v" ]; then
  printf '{ "schema_version": 1, "smoke_'
  exit 1
fi
exec "$real_jq" "\$@"
JQ
  chmod +x "$dir/jq"
}

# Fail only the init-stage checkpoint after configured init has generated and
# committed its files. The resumed run must repeat that idempotent stage without
# creating a second initialization commit, Work Item, or Attempt.
write_init_checkpoint_crashing_jq() {
  local dir="$1"
  mkdir -p "$dir"
  local real_jq
  real_jq="$(command -v jq)"
  cat > "$dir/jq" <<JQ
#!/usr/bin/env bash
if [ "\${1:-}" = "--arg" ] && [ "\${2:-}" = "v" ] && [ "\${3:-}" = "init" ]; then
  printf '{ "schema_version": 1, "run_stage": '
  exit 1
fi
exec "$real_jq" "\$@"
JQ
  chmod +x "$dir/jq"
}

# Write a fake installer that always fails without producing a binary.
write_failing_installer() {
  local installer_path="$1"
  cat > "$installer_path" <<'INSTALLER'
#!/usr/bin/env bash
printf 'installer: simulated download failure\n' >&2
exit 1
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
  if [ "${1-}" = "prepare" ]; then
    local arg has_profile=0
    for arg in "$@"; do
      [ "$arg" = "--coder-profile" ] && has_profile=1
    done
    if [ "$has_profile" -eq 0 ]; then
      set -- "$@" --coder-profile codex-balanced
    fi
  fi
  HOME="$REAL_HOME" FAKE_CMD_LOG="$FAKE_CMD_LOG" bash "$HARNESS" "$@"
}

# Install tripwire executables that record any attempt to reach the network or a
# model. Each appends to $TRIPWIRE_LOG and fails so accidental use is loud.
write_tripwires() {
  local dir="$1"
  mkdir -p "$dir"
  local name
  for name in curl wget claude codex; do
    cat > "$dir/$name" <<TRIP
#!/usr/bin/env bash
printf '%s\n' "$name" >> "$TRIPWIRE_LOG"
exit 97
TRIP
    chmod +x "$dir/$name"
  done
}

assert_contains() {
  if [[ "$1" != *"$2"* ]]; then
    printf '    FAIL: output missing "%s"\n' "$2"
    printf '    Output:\n%s\n' "$1"
    return 1
  fi
}


# Exceed a pipe buffer so an early grep match cannot hide SIGPIPE.
test_assert_contains_large_literal_output() {
  local output
  output="$(printf 'literal [*].\n'; printf '%1048576s' ''; printf '\nend')"
  assert_contains "$output" 'literal [*].' || return 1
  assert_contains "$output" 'end' || return 1
  if assert_contains "$output" 'missing' >/dev/null; then
    return 1
  fi
  if assert_contains "$output" 'literal wildcard' >/dev/null; then
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
  grep -Fxq '# Test identity: first-run-smoke/greeting' \
    "$ROOT/project/main/check.sh" \
    || { printf '    FAIL: fixture test does not declare its structured identity\n'; rc=1; }
  grep -Fxq 'Test: check.sh (first-run-smoke/greeting)' \
    "$ROOT/harness/workitem/behaviors.md" \
    || { printf '    FAIL: fixture behavior does not reference its structured test\n'; rc=1; }

  # The manifest records the prepared safe phase.
  [ "$(jq -r '.safe_phase' "$ROOT/harness/manifest.json")" = "prepared" ] \
    || { printf '    FAIL: safe_phase not prepared\n'; rc=1; }
  [ "$(jq -r '.schema_version' "$ROOT/harness/manifest.json")" = "2" ] \
    || { printf '    FAIL: manifest schema is not version 2\n'; rc=1; }
  [ "$(jq -r '.coder_profile' "$ROOT/harness/manifest.json")" = "codex-balanced" ] \
    || { printf '    FAIL: manifest did not record the coder profile\n'; rc=1; }
  [ -f "$ROOT/project/main/.agents/skills/fluent/SKILL.md" ] \
    || { printf '    FAIL: prepare did not seed the public-style bootstrap\n'; rc=1; }
  jq -e '.skills.fluent == {
      "source": "mrinalwadhwa/fluent",
      "sourceType": "github",
      "skillPath": "skills/fluent/SKILL.md",
      "computedHash": "clean-room-bootstrap"
    } and .metadata == {"fixture": "keep"} and
    .skills.other == {
      "source": "owner/other",
      "skillPath": "skills/other/SKILL.md"
    }' \
    "$ROOT/project/main/skills-lock.json" > /dev/null \
    || { printf '    FAIL: prepare did not seed the exact external lock\n'; rc=1; }

  # The operator's home is untouched.
  [ "$(cat "$REAL_HOME/sentinel")" = "untouched" ] \
    || { printf '    FAIL: operator home was modified\n'; rc=1; }
  [ "$(ls -A "$REAL_HOME")" = "sentinel" ] \
    || { printf '    FAIL: operator home gained entries\n'; rc=1; }

  return $rc
}

test_prepare_leaves_fluent_uninitialized_and_records_codex_home() {
  new_workspace
  trap cleanup_workspace RETURN

  local codex_home="$WORK/codex-home"
  mkdir -p "$codex_home"
  printf 'secret credential\n' > "$codex_home/auth.json"

  run_harness prepare "$ROOT" --binary "$FAKE_FLUENT_SRC" \
    --codex-home "$codex_home" > "$WORK/prepare.out" 2>&1

  local rc=0
  local recorded_codex_home
  recorded_codex_home="$(cd "$codex_home" && pwd -P)"
  [ ! -e "$ROOT/project/main/.fluent" ] \
    || { printf '    FAIL: prepare initialized .fluent before run\n'; rc=1; }
  [ "$(jq -r '.codex_home' "$ROOT/harness/manifest.json")" = "$recorded_codex_home" ] \
    || { printf '    FAIL: manifest did not record the Codex home\n'; rc=1; }
  if grep -R -Fq -- 'secret credential' "$ROOT"; then
    printf '    FAIL: prepare copied Codex credentials into the smoke root\n'; rc=1
  fi
  return $rc
}

test_prepare_requires_a_supported_profile() {
  new_workspace
  trap cleanup_workspace RETURN

  local rc=0
  if HOME="$REAL_HOME" FAKE_CMD_LOG="$FAKE_CMD_LOG" \
    bash "$HARNESS" prepare "$ROOT" --binary "$FAKE_FLUENT_SRC" \
    > "$WORK/missing.out" 2>&1; then
    printf '    FAIL: prepare accepted a missing coder profile\n'; rc=1
  fi
  assert_contains "$(cat "$WORK/missing.out")" 'prepare requires --coder-profile' || rc=1
  [ ! -e "$ROOT" ] \
    || { printf '    FAIL: missing profile mutated the smoke root\n'; rc=1; }

  if HOME="$REAL_HOME" FAKE_CMD_LOG="$FAKE_CMD_LOG" \
    bash "$HARNESS" prepare "$ROOT" --coder-profile custom \
      --binary "$FAKE_FLUENT_SRC" > "$WORK/invalid.out" 2>&1; then
    printf '    FAIL: prepare accepted an unsupported coder profile\n'; rc=1
  fi
  assert_contains "$(cat "$WORK/invalid.out")" 'unsupported coder profile: custom' || rc=1
  [ ! -e "$ROOT" ] \
    || { printf '    FAIL: invalid profile mutated the smoke root\n'; rc=1; }
  return $rc
}

test_prepare_records_every_curated_profile() {
  new_workspace
  trap cleanup_workspace RETURN

  local rc=0 profile profile_root
  for profile in codex-balanced codex-stronger claude-balanced \
    opencode-default codex-write-claude-review claude-write-codex-review; do
    profile_root="$WORK/smoke-$profile"
    HOME="$REAL_HOME" FAKE_CMD_LOG="$FAKE_CMD_LOG" \
      bash "$HARNESS" prepare "$profile_root" --coder-profile "$profile" \
        --binary "$FAKE_FLUENT_SRC" > "$WORK/prepare-$profile.out" 2>&1 \
      || { printf '    FAIL: prepare rejected %s\n' "$profile"; rc=1; continue; }
    [ "$(jq -r '.coder_profile' "$profile_root/harness/manifest.json")" = "$profile" ] \
      || { printf '    FAIL: manifest did not record %s\n' "$profile"; rc=1; }
  done

  profile_root="$WORK/smoke-claude-balanced"
  if HOME="$REAL_HOME" FAKE_CMD_LOG="$FAKE_CMD_LOG" \
    bash "$HARNESS" prepare "$profile_root" --coder-profile codex-balanced \
      --binary "$FAKE_FLUENT_SRC" > "$WORK/profile-change.out" 2>&1; then
    printf '    FAIL: prepare changed an existing root profile\n'; rc=1
  fi
  assert_contains "$(cat "$WORK/profile-change.out")" \
    'uses coder profile claude-balanced, not codex-balanced' || rc=1
  [ "$(jq -r '.coder_profile' "$profile_root/harness/manifest.json")" = "claude-balanced" ] \
    || { printf '    FAIL: rejected profile change mutated the manifest\n'; rc=1; }

  local tampered_root="$WORK/smoke-codex-stronger" tampered_manifest tmp
  tampered_manifest="$tampered_root/harness/manifest.json"
  tmp="$(mktemp "$tampered_manifest.XXXXXX")"
  jq '.coder_profile = "custom"' "$tampered_manifest" > "$tmp"
  mv "$tmp" "$tampered_manifest"
  if HOME="$REAL_HOME" FAKE_CMD_LOG="$FAKE_CMD_LOG" \
    bash "$HARNESS" run "$tampered_root" > "$WORK/tampered-profile.out" 2>&1; then
    printf '    FAIL: run accepted a tampered coder profile\n'; rc=1
  fi
  assert_contains "$(cat "$WORK/tampered-profile.out")" \
    'unsupported coder profile: custom' || rc=1
  [ ! -s "$FAKE_CMD_LOG" ] \
    || { printf '    FAIL: tampered profile invoked Fluent\n'; rc=1; }

  HOME="$REAL_HOME" FAKE_CMD_LOG="$FAKE_CMD_LOG" \
    bash "$HARNESS" run "$profile_root" > "$WORK/claude-run.out" 2>&1 \
    || { printf '    FAIL: recorded Claude profile did not run\n'; rc=1; }
  grep -Fxq 'init init --coder-profile claude-balanced --follow-up-mode propose' \
    "$FAKE_CMD_LOG" \
    || { printf '    FAIL: run did not use the recorded Claude profile\n'; rc=1; }
  return $rc
}

test_run_configures_init_and_commits_before_attempt() {
  new_workspace
  trap cleanup_workspace RETURN

  local codex_home="$WORK/codex-home"
  mkdir -p "$codex_home"
  printf 'secret credential\n' > "$codex_home/auth.json"
  run_harness prepare "$ROOT" --binary "$FAKE_FLUENT_SRC" \
    --codex-home "$codex_home" > /dev/null 2>&1
  # Keep older history larger than a pipe buffer so checking the initialization
  # commit also verifies that the assertion lets git finish writing its log.
  printf 'Seed older history %1048576s\n' '' > "$WORK/older-commit-message"
  git -C "$ROOT/project/main" commit --allow-empty --cleanup=verbatim \
    -F "$WORK/older-commit-message" > /dev/null 2>&1
  local codex_home_log="$WORK/codex-home-log"
  FAKE_CODEX_HOME_LOG="$codex_home_log" run_harness run "$ROOT" > "$WORK/run.out" 2>&1

  local rc=0
  grep -Fxq 'init init --coder-profile codex-balanced --follow-up-mode propose' "$FAKE_CMD_LOG" \
    || { printf '    FAIL: run did not configure fluent init\n'; rc=1; }
  [ -f "$ROOT/project/main/.fluent/tester.yaml" ] \
    || { printf '    FAIL: run did not install tester configuration\n'; rc=1; }
  [ -x "$ROOT/project/main/.fluent/extract-tester-results" ] \
    || { printf '    FAIL: run did not install an executable tester extractor\n'; rc=1; }
  git -C "$ROOT/project/main" ls-files --error-unmatch \
    .fluent/extract-tester-results > /dev/null 2>&1 \
    || { printf '    FAIL: run did not commit the tester extractor\n'; rc=1; }
  [ -f "$ROOT/project/main/AGENTS.md" ] \
    || { printf '    FAIL: configured init did not generate instructions\n'; rc=1; }
  [ -z "$(git -C "$ROOT/project/main" status --porcelain)" ] \
    || { printf '    FAIL: attempt started from a dirty checkout\n'; rc=1; }
  # Consume all history so an early match cannot fail git with SIGPIPE.
  git -C "$ROOT/project/main" log --format=%s | grep -Fx 'Initialize Fluent smoke fixture' > /dev/null \
    || { printf '    FAIL: initialization changes were not committed\n'; rc=1; }
  [ -f "$ROOT/project/main/.agents/skills/fluent/.fluent-managed.json" ] \
    || { printf '    FAIL: run did not adopt the bootstrap skill\n'; rc=1; }
  jq -e '.schema_version == 1 and .agent == "universal" and
    .scope == "project" and .skill == "fluent"' \
    "$ROOT/project/main/.agents/skills/fluent/.fluent-managed.json" > /dev/null \
    || { printf '    FAIL: run recorded incorrect skill ownership\n'; rc=1; }
  if grep -Fq 'fluent-shim: true' \
    "$ROOT/project/main/.agents/skills/fluent/SKILL.md"; then
    printf '    FAIL: run left the bootstrap marker installed\n'; rc=1
  fi
  [ ! -e "$ROOT/project/main/.claude" ] \
    || { printf '    FAIL: bare handoff created a project Claude root\n'; rc=1; }
  [ ! -e "$ROOT/project/main/.codex" ] \
    || { printf '    FAIL: bare handoff created a project Codex root\n'; rc=1; }
  jq -e '.skills.fluent == null and .metadata.fixture == "keep" and
    .skills.other.source == "owner/other"' "$ROOT/project/main/skills-lock.json" \
    > /dev/null || { printf '    FAIL: run did not preserve the external lock\n'; rc=1; }
  grep -Fxq 'skills add' "$FAKE_CMD_LOG" \
    || { printf '    FAIL: run did not invoke the skill handoff\n'; rc=1; }
  local recorded_codex_home
  recorded_codex_home="$(cd "$codex_home" && pwd -P)"
  [ -s "$codex_home_log" ] \
    || { printf '    FAIL: run did not invoke Fluent with CODEX_HOME\n'; rc=1; }
  if ! awk -v expected="$recorded_codex_home" '$0 != expected { exit 1 }' \
    "$codex_home_log"; then
    printf '    FAIL: run did not pass the recorded Codex home to every Fluent invocation\n'
    rc=1
  fi
  if grep -R -Fq -- 'secret credential' "$ROOT"; then
    printf '    FAIL: run copied Codex credentials into the smoke root\n'; rc=1
  fi
  return $rc
}

test_tester_extractor_normalizes_fixture_contract() {
  new_workspace
  trap cleanup_workspace RETURN

  run_harness prepare "$ROOT" --binary "$FAKE_FLUENT_SRC" > /dev/null 2>&1
  run_harness run "$ROOT" > /dev/null 2>&1

  local rc=0 artifact="$WORK/tester-artifact"
  local extractor="$ROOT/project/main/.fluent/extract-tester-results"
  mkdir -p "$artifact"

  jq -n '[{
    command: "./check.sh",
    test_harness: "shell-harness",
    exit_code: 1,
    duration_ms: 12,
    stdout_log: "commands/0-stdout.log",
    stderr_log: "commands/0-stderr.log"
  }]' > "$artifact/commands.json"
  "$extractor" "$artifact" > "$artifact/baseline.json" \
    || { printf '    FAIL: extractor rejected the baseline failure\n'; rc=1; }
  jq -e '. == [{
    "id": "first-run-smoke/greeting",
    "test_harness": "shell-harness",
    "status": "fail",
    "duration_ms": 12,
    "failure_excerpt": "greeting.txt has no line exactly equal to hello"
  }]' "$artifact/baseline.json" > /dev/null \
    || { printf '    FAIL: extractor misreported the baseline failure\n'; rc=1; }

  jq '.[0].exit_code = 0 | .[0].duration_ms = 7' \
    "$artifact/commands.json" > "$artifact/final-commands.json"
  mv "$artifact/final-commands.json" "$artifact/commands.json"
  "$extractor" "$artifact" > "$artifact/final.json" \
    || { printf '    FAIL: extractor rejected the final pass\n'; rc=1; }
  jq -e '.[0].id == "first-run-smoke/greeting" and
    .[0].status == "pass" and .[0].duration_ms == 7 and
    .[0].failure_excerpt == null' "$artifact/final.json" > /dev/null \
    || { printf '    FAIL: extractor misreported the final pass\n'; rc=1; }

  printf '[]\n' > "$artifact/commands.json"
  "$extractor" "$artifact" > "$artifact/empty.json" \
    || { printf '    FAIL: extractor rejected the empty structural probe\n'; rc=1; }
  jq -e '. == []' "$artifact/empty.json" > /dev/null \
    || { printf '    FAIL: extractor invented a structural-probe result\n'; rc=1; }

  printf '{}\n' > "$artifact/commands.json"
  if "$extractor" "$artifact" > "$artifact/wrong-type.out" 2>&1; then
    printf '    FAIL: extractor accepted non-array command evidence\n'; rc=1
  fi

  printf '{bad json\n' > "$artifact/commands.json"
  if "$extractor" "$artifact" > "$artifact/malformed.out" 2>&1; then
    printf '    FAIL: extractor accepted malformed command evidence\n'; rc=1
  fi

  jq -n '[{
    command: "./other-check.sh",
    test_harness: "shell-harness",
    exit_code: 0,
    duration_ms: 1
  }]' > "$artifact/commands.json"
  if "$extractor" "$artifact" > "$artifact/unexpected.out" 2>&1; then
    printf '    FAIL: extractor accepted an unexpected command shape\n'; rc=1
  fi
  return $rc
}

test_skill_handoff_failure_preserves_evidence_and_resumes() {
  new_workspace
  trap cleanup_workspace RETURN

  run_harness prepare "$ROOT" --binary "$FAKE_FLUENT_SRC" > /dev/null 2>&1
  local rc=0
  if HOME="$REAL_HOME" FAKE_CMD_LOG="$FAKE_CMD_LOG" FAKE_SKILLS_ADD_FAILS=1 \
    bash "$HARNESS" run "$ROOT" > "$WORK/run.out" 2>&1; then
    printf '    FAIL: run accepted a failed skill handoff\n'; rc=1
  fi
  assert_contains "$(cat "$WORK/run.out")" "$ROOT/harness/logs/skills.log" || rc=1
  [ "$(jq -r '.run_stage' "$ROOT/harness/manifest.json")" = "installed" ] \
    || { printf '    FAIL: failed handoff advanced the run checkpoint\n'; rc=1; }
  grep -Fq 'fluent-shim: true' "$ROOT/project/main/.agents/skills/fluent/SKILL.md" \
    || { printf '    FAIL: failed handoff changed the bootstrap\n'; rc=1; }
  jq -e '.skills.fluent.source == "mrinalwadhwa/fluent"' \
    "$ROOT/project/main/skills-lock.json" > /dev/null \
    || { printf '    FAIL: failed handoff changed the lock\n'; rc=1; }

  run_harness run "$ROOT" > "$WORK/resume.out" 2>&1 \
    || { printf '    FAIL: skill handoff resume did not reach ready\n'; rc=1; }
  [ "$(grep -c '^skills add$' "$FAKE_CMD_LOG")" -eq 2 ] \
    || { printf '    FAIL: resume did not retry only the skill handoff\n'; rc=1; }
  [ "$(grep -c '^init init ' "$FAKE_CMD_LOG")" -eq 1 ] \
    || { printf '    FAIL: skill retry duplicated initialization\n'; rc=1; }
  return $rc
}

test_skill_handoff_rejects_wrong_ownership_and_resumes() {
  new_workspace
  trap cleanup_workspace RETURN

  run_harness prepare "$ROOT" --binary "$FAKE_FLUENT_SRC" > /dev/null 2>&1
  local rc=0
  if HOME="$REAL_HOME" FAKE_CMD_LOG="$FAKE_CMD_LOG" FAKE_SKILLS_BAD_SIDECAR=1 \
    bash "$HARNESS" run "$ROOT" > "$WORK/run.out" 2>&1; then
    printf '    FAIL: run accepted the wrong skill ownership sidecar\n'; rc=1
  fi
  [ "$(jq -r '.run_stage' "$ROOT/harness/manifest.json")" = "installed" ] \
    || { printf '    FAIL: invalid ownership advanced the checkpoint\n'; rc=1; }
  assert_contains "$(cat "$WORK/run.out")" "$ROOT/harness/logs/skills.log" || rc=1

  run_harness run "$ROOT" > "$WORK/resume.out" 2>&1 \
    || { printf '    FAIL: corrected ownership handoff did not resume\n'; rc=1; }
  jq -e '.agent == "universal" and .scope == "project" and .skill == "fluent"' \
    "$ROOT/project/main/.agents/skills/fluent/.fluent-managed.json" > /dev/null \
    || { printf '    FAIL: resume did not repair skill ownership\n'; rc=1; }
  [ "$(grep -c '^skills add$' "$FAKE_CMD_LOG")" -eq 2 ] \
    || { printf '    FAIL: ownership recovery did not retry the handoff once\n'; rc=1; }
  return $rc
}

test_schema_one_manifest_is_rejected_without_migration() {
  new_workspace
  trap cleanup_workspace RETURN

  run_harness prepare "$ROOT" --binary "$FAKE_FLUENT_SRC" > /dev/null 2>&1
  local manifest="$ROOT/harness/manifest.json" tmp rc=0
  tmp="$(mktemp "$manifest.XXXXXX")"
  jq '.schema_version = 1' "$manifest" > "$tmp"
  mv "$tmp" "$manifest"

  if run_harness run "$ROOT" > "$WORK/run.out" 2>&1; then
    printf '    FAIL: run migrated an old smoke manifest\n'; rc=1
  fi
  assert_contains "$(cat "$WORK/run.out")" 'incompatible manifest schema' || rc=1
  [ ! -s "$FAKE_CMD_LOG" ] \
    || { printf '    FAIL: old manifest invoked Fluent before rejection\n'; rc=1; }
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

test_prepare_encodes_json_significant_root() {
  new_workspace
  trap cleanup_workspace RETURN

  # A valid filesystem path containing a JSON-significant double quote. A raw
  # heredoc manifest would emit invalid JSON here and break the later jq reads.
  local quoted_root="$WORK/say \"hi\"/smoke"
  HOME="$REAL_HOME" FAKE_CMD_LOG="$FAKE_CMD_LOG" \
    bash "$HARNESS" prepare "$quoted_root" --coder-profile codex-balanced \
      --binary "$FAKE_FLUENT_SRC" \
    > /dev/null 2>&1

  local rc=0 mpath="$quoted_root/harness/manifest.json"
  # The manifest is valid JSON and decodes to the exact root path.
  if ! jq -e . "$mpath" > /dev/null 2>&1; then
    printf '    FAIL: manifest is not valid JSON for a quoted root\n'; rc=1
  fi
  [ "$(jq -r '.smoke_root' "$mpath")" = "$quoted_root" ] \
    || { printf '    FAIL: manifest smoke_root not JSON-encoded\n'; rc=1; }
  # run continues to a ready candidate despite the quote in the path.
  HOME="$REAL_HOME" FAKE_CMD_LOG="$FAKE_CMD_LOG" \
    bash "$HARNESS" run "$quoted_root" > "$WORK/run.out" 2>&1 \
    || { printf '    FAIL: run failed for a quoted root\n'; rc=1; }
  [ "$(jq -r '.safe_phase' "$mpath")" = "ran" ] \
    || { printf '    FAIL: quoted root did not reach the ready phase\n'; rc=1; }
  return $rc
}

test_run_uses_public_sequence_and_stops_before_land() {
  new_workspace
  trap cleanup_workspace RETURN

  run_harness prepare "$ROOT" --binary "$FAKE_FLUENT_SRC" > /dev/null 2>&1
  run_harness run "$ROOT" > "$WORK/run.out" 2>&1

  local rc=0
  # Setup stays ordered even though queue polling and the scheduler run
  # concurrently. The scheduler, not the harness, owns Attempt execution.
  local skills_line init_line work_line attempt_line queue_line
  skills_line="$(grep -n '^skills add$' "$FAKE_CMD_LOG" | cut -d: -f1)"
  init_line="$(grep -n '^init init --coder-profile codex-balanced --follow-up-mode propose$' "$FAKE_CMD_LOG" | cut -d: -f1)"
  work_line="$(grep -n '^work-item create$' "$FAKE_CMD_LOG" | cut -d: -f1)"
  attempt_line="$(grep -n '^attempt create$' "$FAKE_CMD_LOG" | cut -d: -f1)"
  queue_line="$(grep -n '^queue add$' "$FAKE_CMD_LOG" | cut -d: -f1)"
  if [ -z "$skills_line" ] || [ -z "$init_line" ] || [ -z "$work_line" ] \
    || [ -z "$attempt_line" ] || [ -z "$queue_line" ] \
    || [ "$skills_line" -ge "$init_line" ] || [ "$init_line" -ge "$work_line" ] \
    || [ "$work_line" -ge "$attempt_line" ] || [ "$attempt_line" -ge "$queue_line" ]; then
    printf '    FAIL: setup commands are missing or out of order:\n%s\n' "$(cat "$FAKE_CMD_LOG")"
    rc=1
  fi
  grep -Fxq 'scheduler run' "$FAKE_CMD_LOG" \
    || { printf '    FAIL: scheduler was not started\n'; rc=1; }
  grep -Fxq 'queue show' "$FAKE_CMD_LOG" \
    || { printf '    FAIL: queue status was not observed\n'; rc=1; }
  if grep -Fxq 'queue list' "$FAKE_CMD_LOG"; then
    printf '    FAIL: harness parsed the active-only queue list\n'; rc=1
  fi
  if grep -q '^attempt run$' "$FAKE_CMD_LOG"; then
    printf '    FAIL: harness ran the Attempt outside the scheduler\n'; rc=1
  fi
  # run never lands.
  if grep -q 'merge-candidate land' "$FAKE_CMD_LOG"; then
    printf '    FAIL: run invoked land\n'; rc=1
  fi
  # The manifest advances to the ran safe phase and records the candidate.
  [ "$(jq -r '.safe_phase' "$ROOT/harness/manifest.json")" = "ran" ] \
    || { printf '    FAIL: safe_phase not ran\n'; rc=1; }
  [ "$(jq -r '.merge_candidate_id' "$ROOT/harness/manifest.json")" = "attempt-1-merge-candidate" ] \
    || { printf '    FAIL: candidate id not recorded\n'; rc=1; }
  # main is untouched — the fix lives only on the candidate branch.
  if ( cd "$ROOT/project/main" && ./check.sh ) 2>/dev/null; then
    printf '    FAIL: main already carries the fix before landing\n'; rc=1
  fi
  if [ -n "$(git -C "$ROOT/project/main" status --porcelain)" ]; then
    printf '    FAIL: target repository is dirty after run\n'; rc=1
  fi
  return $rc
}

test_scheduler_exit_rereads_terminal_queue_state() {
  new_workspace
  trap cleanup_workspace RETURN

  run_harness prepare "$ROOT" --binary "$FAKE_FLUENT_SRC" > /dev/null 2>&1
  local rc=0
  HOME="$REAL_HOME" FAKE_CMD_LOG="$FAKE_CMD_LOG" \
    FAKE_QUEUE_SHOW_STALE_ONCE_AFTER_TERMINAL=1 \
    bash "$HARNESS" run "$ROOT" > "$WORK/run.out" 2>&1 \
    || {
      printf '    FAIL: one stale queue read hid the terminal disposition\n'
      printf '    harness output:\n'
      sed 's/^/      /' "$WORK/run.out"
      printf '    scheduler log:\n'
      sed 's/^/      /' "$ROOT/harness/logs/scheduler.log"
      printf '    queue log:\n'
      sed 's/^/      /' "$ROOT/harness/logs/queue-status.log"
      printf '    fake command log:\n'
      sed 's/^/      /' "$FAKE_CMD_LOG"
      printf '    fake stage and queue state:\n'
      sed 's/^/      /' "$ROOT/bin/.fake-state/stage" \
        "$ROOT/bin/.fake-state/queue-status"
      rc=1
    }
  [ "$(jq -r '.safe_phase' "$ROOT/harness/manifest.json")" = "ran" ] \
    || { printf '    FAIL: terminal queue reread did not reach ready\n'; rc=1; }
  [ "$(grep -c '^queue show$' "$FAKE_CMD_LOG")" -ge 3 ] \
    || { printf '    FAIL: harness did not reread queue state\n'; rc=1; }
  local listed
  listed="$(cd "$ROOT/project/main" && HOME="$ROOT/home" \
    FAKE_CMD_LOG="$FAKE_CMD_LOG" "$ROOT/bin/fluent" queue list)"
  [ "$listed" = "Queue is empty" ] \
    || { printf '    FAIL: fake queue list exposed terminal history\n'; rc=1; }
  return $rc
}

test_scheduler_failed_terminal_preserves_evidence() {
  new_workspace
  trap cleanup_workspace RETURN

  run_harness prepare "$ROOT" --binary "$FAKE_FLUENT_SRC" > /dev/null 2>&1
  local rc=0
  if HOME="$REAL_HOME" FAKE_CMD_LOG="$FAKE_CMD_LOG" FAKE_SCHEDULER_FAILS=1 \
    bash "$HARNESS" run "$ROOT" > "$WORK/run.out" 2>&1; then
    printf '    FAIL: failed scheduler dispatch reached ready\n'; rc=1
  fi
  [ "$(jq -r '.run_stage' "$ROOT/harness/manifest.json")" = "queued" ] \
    || { printf '    FAIL: failed dispatch lost the queue checkpoint\n'; rc=1; }
  [ "$(jq -r '.safe_phase' "$ROOT/harness/manifest.json")" = "prepared" ] \
    || { printf '    FAIL: failed dispatch advanced the safe phase\n'; rc=1; }
  grep -Fq 'queue reached terminal status: failed' \
    "$ROOT/harness/logs/queue-status.log" \
    || { printf '    FAIL: failed terminal status was not retained\n'; rc=1; }
  [ ! -e "$ROOT/project/main/.fluent/work/merge-candidates/attempt-1-merge-candidate.json" ] \
    || { printf '    FAIL: failed dispatch produced a candidate\n'; rc=1; }

  local before after
  before="$(grep -c '^scheduler run$' "$FAKE_CMD_LOG" || true)"
  if run_harness run "$ROOT" > "$WORK/resume.out" 2>&1; then
    printf '    FAIL: rerunning a failed terminal dispatch reached ready\n'; rc=1
  fi
  after="$(grep -c '^scheduler run$' "$FAKE_CMD_LOG" || true)"
  [ "$after" -eq "$before" ] \
    || { printf '    FAIL: terminal failure restarted the scheduler\n'; rc=1; }
  return $rc
}

test_interruption_reaps_scheduler_child() {
  new_workspace
  trap cleanup_workspace RETURN

  run_harness prepare "$ROOT" --binary "$FAKE_FLUENT_SRC" > /dev/null 2>&1
  local pid_file="$WORK/scheduler.pid" harness_pid child_pid="" rc=0
  HOME="$REAL_HOME" FAKE_CMD_LOG="$FAKE_CMD_LOG" FAKE_SCHEDULER_HANGS=1 \
    FAKE_SCHEDULER_PID_FILE="$pid_file" \
    bash "$HARNESS" run "$ROOT" > "$WORK/run.out" 2>&1 &
  harness_pid="$!"

  local _
  for _ in $(seq 1 200); do
    [ -s "$pid_file" ] && break
    sleep 0.01
  done
  if [ ! -s "$pid_file" ]; then
    printf '    FAIL: hanging scheduler did not publish its pid\n'; rc=1
    kill -TERM "$harness_pid" 2>/dev/null || true
    wait "$harness_pid" 2>/dev/null || true
    return $rc
  fi
  child_pid="$(cat "$pid_file")"

  kill -TERM "$harness_pid"
  if wait "$harness_pid"; then
    printf '    FAIL: interrupted harness exited successfully\n'; rc=1
  fi
  for _ in $(seq 1 100); do
    kill -0 "$child_pid" 2>/dev/null || break
    sleep 0.01
  done
  if kill -0 "$child_pid" 2>/dev/null; then
    printf '    FAIL: scheduler child survived harness interruption\n'; rc=1
    kill -TERM "$child_pid" 2>/dev/null || true
  fi
  grep -Fq 'scheduler exit:' "$ROOT/harness/logs/scheduler.log" \
    || { printf '    FAIL: interruption did not record scheduler cleanup\n'; rc=1; }
  return $rc
}

test_run_waits_for_learner_success() {
  new_workspace
  trap cleanup_workspace RETURN

  run_harness prepare "$ROOT" --binary "$FAKE_FLUENT_SRC" > /dev/null 2>&1

  local rc=0
  # The scheduler reports candidate-ready, but the stored Learner never reaches
  # "succeeded". The harness must not hand off the candidate.
  if HOME="$REAL_HOME" FAKE_CMD_LOG="$FAKE_CMD_LOG" FAKE_LEARNER_SUCCEEDS=0 \
    bash "$HARNESS" run "$ROOT" > "$WORK/run.out" 2>&1; then
    printf '    FAIL: run handed off before the Learner succeeded\n'; rc=1
  fi
  local out; out="$(cat "$WORK/run.out")"
  # The candidate record exists, proving the gate is on the Learner, not merely
  # on the candidate's presence.
  [ -f "$ROOT/project/main/.fluent/work/merge-candidates/attempt-1-merge-candidate.json" ] \
    || { printf '    FAIL: candidate was never created\n'; rc=1; }
  # No ready handoff was printed.
  if printf '%s' "$out" | grep -q 'ready Merge Candidate'; then
    printf '    FAIL: printed a ready handoff without a succeeded Learner\n'; rc=1
  fi
  # The failure is truthful: it names the run phase and preserves the root.
  assert_contains "$out" 'phase "run" failed' || rc=1
  # The safe phase stays prepared so resume repeats run, not land.
  [ "$(jq -r '.safe_phase' "$ROOT/harness/manifest.json")" = "prepared" ] \
    || { printf '    FAIL: safe_phase advanced without a succeeded Learner\n'; rc=1; }
  return $rc
}

test_run_waits_for_complete_attempt() {
  new_workspace
  trap cleanup_workspace RETURN

  run_harness prepare "$ROOT" --binary "$FAKE_FLUENT_SRC" > /dev/null 2>&1
  local rc=0
  if HOME="$REAL_HOME" FAKE_CMD_LOG="$FAKE_CMD_LOG" FAKE_ATTEMPT_STATUS=executing \
    bash "$HARNESS" run "$ROOT" > "$WORK/run.out" 2>&1; then
    printf '    FAIL: run handed off an incomplete Attempt\n'; rc=1
  fi
  [ "$(jq -r '.learning.status' "$ROOT/harness/logs/attempt.json")" = "succeeded" ] \
    || { printf '    FAIL: fixture did not isolate Attempt completion\n'; rc=1; }
  [ "$(jq -r '.status' "$ROOT/harness/logs/attempt.json")" = "executing" ] \
    || { printf '    FAIL: fixture did not record an incomplete Attempt\n'; rc=1; }
  if grep -q 'ready Merge Candidate' "$WORK/run.out"; then
    printf '    FAIL: incomplete Attempt received a ready handoff\n'; rc=1
  fi
  [ "$(jq -r '.safe_phase' "$ROOT/harness/manifest.json")" = "prepared" ] \
    || { printf '    FAIL: incomplete Attempt advanced the safe phase\n'; rc=1; }
  return $rc
}

test_run_resumes_same_attempt_after_scheduler_pause() {
  new_workspace
  trap cleanup_workspace RETURN

  run_harness prepare "$ROOT" --binary "$FAKE_FLUENT_SRC" > /dev/null 2>&1

  local rc=0
  # The first scheduler run pauses at needs-user and preserves the exact
  # planned Attempt for an explicit queue resume.
  if HOME="$REAL_HOME" FAKE_CMD_LOG="$FAKE_CMD_LOG" FAKE_SCHEDULER_NEEDS_USER_ONCE=1 \
    bash "$HARNESS" run "$ROOT" > "$WORK/run.out" 2>&1; then
    printf '    FAIL: paused scheduler run should require a resume\n'; rc=1
  fi
  [ "$(jq -r '.run_stage' "$ROOT/harness/manifest.json")" = "queued" ] \
    || { printf '    FAIL: queue checkpoint was not preserved\n'; rc=1; }
  assert_contains "$(cat "$ROOT/harness/logs/queue-status.log")" \
    'fluent queue resume clean-room-fixture --attempt attempt-1' || rc=1

  HOME="$REAL_HOME" FAKE_CMD_LOG="$FAKE_CMD_LOG" FAKE_SCHEDULER_NEEDS_USER_ONCE=1 \
    bash "$HARNESS" run "$ROOT" > "$WORK/resume.out" 2>&1 \
    || { printf '    FAIL: resumed scheduler run did not reach ready\n'; rc=1; }
  [ "$(grep -c '^queue resume$' "$FAKE_CMD_LOG")" -eq 1 ] \
    || { printf '    FAIL: exact queue resume was not issued once\n'; rc=1; }
  [ "$(grep -c '^scheduler run$' "$FAKE_CMD_LOG")" -eq 2 ] \
    || { printf '    FAIL: scheduler did not run once before and after pause\n'; rc=1; }
  [ "$(jq -r '.safe_phase' "$ROOT/harness/manifest.json")" = "ran" ] \
    || { printf '    FAIL: scheduler resume did not reach the ready phase\n'; rc=1; }
  return $rc
}

test_ready_handoff_is_actionable() {
  new_workspace
  trap cleanup_workspace RETURN

  run_harness prepare "$ROOT" --binary "$FAKE_FLUENT_SRC" > /dev/null 2>&1
  run_harness run "$ROOT" > "$WORK/run.out" 2>&1

  local rc=0 out
  out="$(cat "$WORK/run.out")"
  # The exact inspection command names the stored candidate.
  assert_contains "$out" "merge-candidate show clean-room-fixture attempt-1-merge-candidate" || rc=1
  # A separate explicit land command targets this same root.
  assert_contains "$out" "land $ROOT" || rc=1
  return $rc
}

test_ready_handoff_quotes_paths_with_spaces() {
  new_workspace
  trap cleanup_workspace RETURN

  # A valid smoke root whose path contains a space.
  local spaced_root="$WORK/first run/smoke"
  HOME="$REAL_HOME" FAKE_CMD_LOG="$FAKE_CMD_LOG" \
    bash "$HARNESS" prepare "$spaced_root" --coder-profile codex-balanced \
      --binary "$FAKE_FLUENT_SRC" > /dev/null 2>&1
  HOME="$REAL_HOME" FAKE_CMD_LOG="$FAKE_CMD_LOG" \
    bash "$HARNESS" run "$spaced_root" > "$WORK/run.out" 2>&1

  local rc=0
  # The printed inspection command must execute despite the space and show the
  # candidate — proving every inserted path is shell-escaped.
  local insp
  insp="$(grep -A1 -- '( cd ' "$WORK/run.out")"
  if ! FAKE_CMD_LOG="$FAKE_CMD_LOG" eval "$insp" > "$WORK/insp.out" 2>&1; then
    printf '    FAIL: printed inspection command did not execute:\n%s\n' "$insp"; rc=1
  fi
  assert_contains "$(cat "$WORK/insp.out")" "attempt-1-merge-candidate" || rc=1

  # The printed land command (the last handoff line) must also execute.
  local land_cmd
  land_cmd="$(tail -1 "$WORK/run.out")"
  if ! HOME="$REAL_HOME" FAKE_CMD_LOG="$FAKE_CMD_LOG" eval "$land_cmd" > "$WORK/land.out" 2>&1; then
    printf '    FAIL: printed land command did not execute:\n%s\n' "$land_cmd"; rc=1
  fi
  [ "$(jq -r '.safe_phase' "$spaced_root/harness/manifest.json")" = "landed" ] \
    || { printf '    FAIL: printed land command did not land the candidate\n'; rc=1; }
  return $rc
}

test_land_verifies_target() {
  new_workspace
  trap cleanup_workspace RETURN

  local codex_home="$WORK/codex-home"
  mkdir -p "$codex_home"
  printf 'secret credential\n' > "$codex_home/auth.json"
  local codex_home_log="$WORK/codex-home-log"
  run_harness prepare "$ROOT" --binary "$FAKE_FLUENT_SRC" \
    --codex-home "$codex_home" > /dev/null 2>&1
  FAKE_CODEX_HOME_LOG="$codex_home_log" run_harness run "$ROOT" > /dev/null 2>&1
  : > "$codex_home_log"
  FAKE_CODEX_HOME_LOG="$codex_home_log" run_harness land "$ROOT" > "$WORK/land.out" 2>&1

  local rc=0
  # The candidate lands and the fixture test now passes on main.
  if ! ( cd "$ROOT/project/main" && ./check.sh ) 2>/dev/null; then
    printf '    FAIL: fixture test does not pass after land\n'; rc=1
  fi
  # The target repository is clean.
  if [ -n "$(git -C "$ROOT/project/main" status --porcelain)" ]; then
    printf '    FAIL: target repository is dirty after land\n'; rc=1
  fi
  # The manifest records the merged commit and the landed safe phase.
  [ "$(jq -r '.safe_phase' "$ROOT/harness/manifest.json")" = "landed" ] \
    || { printf '    FAIL: safe_phase not landed\n'; rc=1; }
  local merged
  merged="$(jq -r '.merged_commit' "$ROOT/harness/manifest.json")"
  [ "$merged" = "$(git -C "$ROOT/project/main" rev-parse HEAD)" ] \
    || { printf '    FAIL: merged_commit does not match main HEAD\n'; rc=1; }
  # The land command is the only path that reached land.
  grep -q 'merge-candidate land' "$FAKE_CMD_LOG" \
    || { printf '    FAIL: land was never invoked\n'; rc=1; }
  local recorded_codex_home
  recorded_codex_home="$(cd "$codex_home" && pwd -P)"
  [ -s "$codex_home_log" ] \
    || { printf '    FAIL: land did not invoke Fluent with CODEX_HOME\n'; rc=1; }
  if ! awk -v expected="$recorded_codex_home" '$0 != expected { exit 1 }' \
    "$codex_home_log"; then
    printf '    FAIL: land did not pass the recorded Codex home to every Fluent invocation\n'
    rc=1
  fi
  if grep -R -Fq -- 'secret credential' "$ROOT"; then
    printf '    FAIL: land copied Codex credentials into the smoke root\n'; rc=1
  fi
  return $rc
}

test_land_failure_preserves_evidence_and_resume() {
  new_workspace
  trap cleanup_workspace RETURN

  run_harness prepare "$ROOT" --binary "$FAKE_FLUENT_SRC" > /dev/null 2>&1
  run_harness run "$ROOT" > /dev/null 2>&1

  local rc=0
  # Force the land itself to fail so the land phase exercises the recovery
  # contract, not only its success path.
  if HOME="$REAL_HOME" FAKE_CMD_LOG="$FAKE_CMD_LOG" FAKE_LAND_FAILS=1 \
    bash "$HARNESS" land "$ROOT" > "$WORK/land.out" 2>&1; then
    printf '    FAIL: land should exit non-zero when the merge fails\n'; rc=1
  fi
  local out; out="$(cat "$WORK/land.out")"
  # The smoke root is preserved with the failed phase, its log, and a resume.
  [ -d "$ROOT/project/main/.git" ] || { printf '    FAIL: smoke root not preserved\n'; rc=1; }
  assert_contains "$out" 'phase "land" failed' || rc=1
  assert_contains "$out" "$ROOT/harness/logs/land.log" || rc=1
  assert_contains "$out" "land $ROOT" || rc=1
  # The safe phase stays at ran so the resume repeats land, not run.
  [ "$(jq -r '.safe_phase' "$ROOT/harness/manifest.json")" = "ran" ] \
    || { printf '    FAIL: safe_phase advanced past the land failure\n'; rc=1; }
  # main never received the merge.
  if ( cd "$ROOT/project/main" && ./check.sh ) 2>/dev/null; then
    printf '    FAIL: main carries the fix despite a failed land\n'; rc=1
  fi

  # Clear the injected failure and execute the printed land resume; it lands.
  run_harness land "$ROOT" > "$WORK/land-resume.out" 2>&1 \
    || { printf '    FAIL: land resume did not land the candidate\n'; rc=1; }
  [ "$(jq -r '.safe_phase' "$ROOT/harness/manifest.json")" = "landed" ] \
    || { printf '    FAIL: land resume did not reach the landed phase\n'; rc=1; }
  if ! ( cd "$ROOT/project/main" && ./check.sh ) 2>/dev/null; then
    printf '    FAIL: fixture test does not pass after the land resume\n'; rc=1
  fi
  return $rc
}

test_failure_preserves_evidence_and_resume() {
  new_workspace
  trap cleanup_workspace RETURN

  run_harness prepare "$ROOT" --binary "$FAKE_FLUENT_SRC" > /dev/null 2>&1

  local rc=0
  # Pause the scheduler-owned Attempt at a recoverable provider boundary.
  if HOME="$REAL_HOME" FAKE_CMD_LOG="$FAKE_CMD_LOG" FAKE_SCHEDULER_NEEDS_USER_ONCE=1 \
    bash "$HARNESS" run "$ROOT" > "$WORK/run.out" 2>&1; then
    printf '    FAIL: run should exit non-zero when a phase fails\n'; rc=1
  fi
  local out
  out="$(cat "$WORK/run.out")"
  # The smoke root is preserved.
  [ -d "$ROOT/project/main/.git" ] || { printf '    FAIL: smoke root not preserved\n'; rc=1; }
  # The failure names the phase, a log, and the exact resume command.
  assert_contains "$out" 'phase "run" failed' || rc=1
  assert_contains "$out" "$ROOT/harness/logs/" || rc=1
  assert_contains "$out" "run $ROOT" || rc=1
  # The safe phase stays at prepared so resume repeats run, not land.
  [ "$(jq -r '.safe_phase' "$ROOT/harness/manifest.json")" = "prepared" ] \
    || { printf '    FAIL: safe_phase advanced past the failure\n'; rc=1; }
  # Setup and queue submission completed before the pause, so resume must not
  # replay them.
  [ "$(jq -r '.run_stage' "$ROOT/harness/manifest.json")" = "queued" ] \
    || { printf '    FAIL: run_stage not checkpointed at queued\n'; rc=1; }

  # Clear the injected failure and execute the exact printed resume command. It
  # must reach a ready candidate by reusing the existing Work Item and Attempt,
  # not by re-running the non-idempotent setup commands.
  run_harness run "$ROOT" > "$WORK/resume.out" 2>&1 \
    || { printf '    FAIL: resume did not reach a ready candidate\n'; rc=1; }
  [ "$(grep -c '^init init --coder-profile codex-balanced --follow-up-mode propose$' "$FAKE_CMD_LOG")" -eq 1 ] \
    || { printf '    FAIL: resume replayed init\n'; rc=1; }
  [ "$(grep -c '^work-item create$' "$FAKE_CMD_LOG")" -eq 1 ] \
    || { printf '    FAIL: resume replayed work-item create\n'; rc=1; }
  [ "$(grep -c '^attempt create$' "$FAKE_CMD_LOG")" -eq 1 ] \
    || { printf '    FAIL: resume replayed attempt create\n'; rc=1; }
  [ "$(grep -c '^queue add$' "$FAKE_CMD_LOG")" -eq 1 ] \
    || { printf '    FAIL: resume replayed queue add\n'; rc=1; }
  [ "$(grep -c '^queue resume$' "$FAKE_CMD_LOG")" -eq 1 ] \
    || { printf '    FAIL: resume did not authorize the exact Attempt\n'; rc=1; }
  [ "$(jq -r '.safe_phase' "$ROOT/harness/manifest.json")" = "ran" ] \
    || { printf '    FAIL: resume did not reach the ready phase\n'; rc=1; }
  assert_contains "$(cat "$WORK/resume.out")" "ready Merge Candidate" || rc=1
  return $rc
}

test_run_resume_after_init_commit_does_not_duplicate_work_state() {
  new_workspace
  trap cleanup_workspace RETURN

  run_harness prepare "$ROOT" --binary "$FAKE_FLUENT_SRC" > /dev/null 2>&1
  local crash_bin="$WORK/crashing-init-checkpoint-jq-bin"
  write_init_checkpoint_crashing_jq "$crash_bin"

  local rc=0
  if HOME="$REAL_HOME" FAKE_CMD_LOG="$FAKE_CMD_LOG" PATH="$crash_bin:$PATH" \
    bash "$HARNESS" run "$ROOT" > "$WORK/run.out" 2>&1; then
    printf '    FAIL: run should fail at the init checkpoint\n'; rc=1
  fi
  [ "$(jq -r '.run_stage' "$ROOT/harness/manifest.json")" = "skills" ] \
    || { printf '    FAIL: init checkpoint advanced despite its write failure\n'; rc=1; }
  [ "$(git -C "$ROOT/project/main" log --format=%s | grep -Fc 'Initialize Fluent smoke fixture')" -eq 1 ] \
    || { printf '    FAIL: init did not commit exactly once before the checkpoint failure\n'; rc=1; }

  run_harness run "$ROOT" > "$WORK/resume.out" 2>&1 \
    || { printf '    FAIL: run resume did not reach a ready candidate\n'; rc=1; }
  [ "$(grep -c '^work-item create$' "$FAKE_CMD_LOG")" -eq 1 ] \
    || { printf '    FAIL: resume duplicated the Work Item\n'; rc=1; }
  [ "$(grep -c '^attempt create$' "$FAKE_CMD_LOG")" -eq 1 ] \
    || { printf '    FAIL: resume duplicated the Attempt\n'; rc=1; }
  [ "$(git -C "$ROOT/project/main" log --format=%s | grep -Fc 'Initialize Fluent smoke fixture')" -eq 1 ] \
    || { printf '    FAIL: resume duplicated the initialization commit\n'; rc=1; }
  [ "$(jq -r '.safe_phase' "$ROOT/harness/manifest.json")" = "ran" ] \
    || { printf '    FAIL: resume did not reach the ready phase\n'; rc=1; }
  return $rc
}

test_installer_failure_preserves_evidence_and_resume() {
  new_workspace
  trap cleanup_workspace RETURN

  # An installer that fails without producing a binary.
  local installer="$WORK/failing-installer"
  write_failing_installer "$installer"

  run_harness prepare "$ROOT" --installer "$installer" > /dev/null 2>&1

  local rc=0
  if run_harness run "$ROOT" > "$WORK/run.out" 2>&1; then
    printf '    FAIL: run should exit non-zero when the installer fails\n'; rc=1
  fi
  local out; out="$(cat "$WORK/run.out")"
  # The smoke root is preserved.
  [ -d "$ROOT/project/main/.git" ] || { printf '    FAIL: smoke root not preserved\n'; rc=1; }
  # The failure names the run phase, its install log, and the exact resume.
  assert_contains "$out" 'phase "run" failed' || rc=1
  assert_contains "$out" "$ROOT/harness/logs/install.log" || rc=1
  assert_contains "$out" "run $ROOT" || rc=1
  # The install log retains the installer's own failure diagnostic.
  assert_contains "$(cat "$ROOT/harness/logs/install.log")" "simulated download failure" || rc=1
  # The safe phase stays prepared so resume repeats run, not land.
  [ "$(jq -r '.safe_phase' "$ROOT/harness/manifest.json")" = "prepared" ] \
    || { printf '    FAIL: safe_phase advanced past the install failure\n'; rc=1; }
  # No Fluent command ran: the boundary failed before init.
  [ -s "$FAKE_CMD_LOG" ] \
    && { printf '    FAIL: a Fluent command ran despite a failed install\n'; rc=1; }
  return $rc
}

test_prepare_failure_preserves_evidence_and_resume() {
  new_workspace
  trap cleanup_workspace RETURN

  # A failing `git` first on PATH makes the fixture-repo seeding fail mid-prepare.
  local trap_bin="$WORK/failing-git-bin"
  write_failing_git "$trap_bin"

  local rc=0
  if HOME="$REAL_HOME" FAKE_CMD_LOG="$FAKE_CMD_LOG" PATH="$trap_bin:$PATH" \
    bash "$HARNESS" prepare "$ROOT" --coder-profile codex-balanced \
      --binary "$FAKE_FLUENT_SRC" \
    > "$WORK/prepare.out" 2>&1; then
    printf '    FAIL: prepare should exit non-zero when seeding fails\n'; rc=1
  fi
  local out; out="$(cat "$WORK/prepare.out")"
  # The partial root is preserved with a phase log and an exact prepare resume.
  [ -f "$ROOT/harness/.prepare-incomplete" ] \
    || { printf '    FAIL: no incomplete-prepare marker retained\n'; rc=1; }
  [ -f "$ROOT/harness/manifest.json" ] \
    && { printf '    FAIL: a manifest was written despite the failure\n'; rc=1; }
  assert_contains "$out" 'phase "prepare" failed' || rc=1
  assert_contains "$out" "$ROOT/harness/logs/prepare.log" || rc=1
  assert_contains "$out" "prepare $ROOT" || rc=1
  assert_contains "$(cat "$ROOT/harness/logs/prepare.log")" "simulated seeding failure" || rc=1

  # Execute the exact printed resume with a working git. The partial root is
  # accepted (not rejected as nonempty) and prepares to completion.
  run_harness prepare "$ROOT" --binary "$FAKE_FLUENT_SRC" \
    > "$WORK/prepare-resume.out" 2>&1 \
    || { printf '    FAIL: prepare resume did not complete\n'; rc=1; }
  [ -f "$ROOT/harness/manifest.json" ] \
    || { printf '    FAIL: resume did not write a manifest\n'; rc=1; }
  [ -f "$ROOT/harness/.prepare-incomplete" ] \
    && { printf '    FAIL: incomplete marker left after a completed prepare\n'; rc=1; }
  [ "$(jq -r '.safe_phase' "$ROOT/harness/manifest.json")" = "prepared" ] \
    || { printf '    FAIL: resumed prepare did not reach the prepared phase\n'; rc=1; }
  # The rebuilt fixture is sound: its test fails before the fix, and a full run
  # still reaches a ready candidate.
  if ( cd "$ROOT/project/main" && ./check.sh ) 2>/dev/null; then
    printf '    FAIL: rebuilt fixture test should fail before the fix\n'; rc=1
  fi
  run_harness run "$ROOT" > /dev/null 2>&1 \
    || { printf '    FAIL: run after a resumed prepare did not reach ready\n'; rc=1; }
  return $rc
}

test_prepare_directory_failure_preserves_evidence_and_resume() {
  new_workspace
  trap cleanup_workspace RETURN

  # A failing `mkdir` first on PATH makes the isolated-home directory setup fail
  # after the durable evidence home exists, modelling a permissions or disk error
  # during prepare's directory creation.
  local trap_bin="$WORK/failing-mkdir-bin"
  write_failing_mkdir "$trap_bin"

  local rc=0
  if HOME="$REAL_HOME" FAKE_CMD_LOG="$FAKE_CMD_LOG" PATH="$trap_bin:$PATH" \
    FAILING_MKDIR_MATCH="$ROOT/home" \
    bash "$HARNESS" prepare "$ROOT" --coder-profile codex-balanced \
      --binary "$FAKE_FLUENT_SRC" \
    > "$WORK/prepare.out" 2>&1; then
    printf '    FAIL: prepare should exit non-zero when directory setup fails\n'; rc=1
  fi
  local out; out="$(cat "$WORK/prepare.out")"
  # Durable evidence exists even though the failure struck during directory setup,
  # before any seeding: a marker, a log, and no premature manifest.
  [ -f "$ROOT/harness/.prepare-incomplete" ] \
    || { printf '    FAIL: no incomplete-prepare marker retained\n'; rc=1; }
  [ -f "$ROOT/harness/logs/prepare.log" ] \
    || { printf '    FAIL: no prepare log retained\n'; rc=1; }
  [ -f "$ROOT/harness/manifest.json" ] \
    && { printf '    FAIL: a manifest was written despite the failure\n'; rc=1; }
  # The failure names the phase, its log, and the exact prepare resume command.
  assert_contains "$out" 'phase "prepare" failed' || rc=1
  assert_contains "$out" "$ROOT/harness/logs/prepare.log" || rc=1
  assert_contains "$out" "prepare $ROOT" || rc=1
  assert_contains "$(cat "$ROOT/harness/logs/prepare.log")" "simulated directory-setup failure" || rc=1

  # Execute the printed resume with working directory creation. The marked partial
  # root is accepted and prepares to completion.
  run_harness prepare "$ROOT" --binary "$FAKE_FLUENT_SRC" \
    > "$WORK/prepare-resume.out" 2>&1 \
    || { printf '    FAIL: prepare resume did not complete\n'; rc=1; }
  [ -f "$ROOT/harness/manifest.json" ] \
    || { printf '    FAIL: resume did not write a manifest\n'; rc=1; }
  [ -f "$ROOT/harness/.prepare-incomplete" ] \
    && { printf '    FAIL: incomplete marker left after a completed prepare\n'; rc=1; }
  [ "$(jq -r '.safe_phase' "$ROOT/harness/manifest.json")" = "prepared" ] \
    || { printf '    FAIL: resumed prepare did not reach the prepared phase\n'; rc=1; }
  run_harness run "$ROOT" > /dev/null 2>&1 \
    || { printf '    FAIL: run after a resumed prepare did not reach ready\n'; rc=1; }
  return $rc
}

test_prepare_initial_manifest_write_failure_is_durable() {
  new_workspace
  trap cleanup_workspace RETURN

  # A fake jq that fails for `jq -n` — the initial manifest build form — while
  # passing all other jq calls through. This models an interrupted manifest
  # write (jq crash or full disk) after seeding has fully succeeded.
  local crash_bin="$WORK/crashing-initial-jq-bin"
  mkdir -p "$crash_bin"
  local real_jq
  real_jq="$(command -v jq)"
  cat > "$crash_bin/jq" <<JQ
#!/usr/bin/env bash
if [ "\${1:-}" = "-n" ]; then
  printf '{ "schema_version": 1, "smoke_'
  exit 1
fi
exec "$real_jq" "\$@"
JQ
  chmod +x "$crash_bin/jq"

  local rc=0
  if HOME="$REAL_HOME" FAKE_CMD_LOG="$FAKE_CMD_LOG" PATH="$crash_bin:$PATH" \
    bash "$HARNESS" prepare "$ROOT" --coder-profile codex-balanced \
      --binary "$FAKE_FLUENT_SRC" \
    > "$WORK/prepare.out" 2>&1; then
    printf '    FAIL: prepare should exit non-zero when initial manifest write fails\n'; rc=1
  fi
  local out; out="$(cat "$WORK/prepare.out")"
  # The partial root keeps the .prepare-incomplete marker and the log.
  # No manifest must exist: the atomic temp was discarded before renaming.
  [ -f "$ROOT/harness/.prepare-incomplete" ] \
    || { printf '    FAIL: no incomplete-prepare marker retained\n'; rc=1; }
  [ -f "$ROOT/harness/manifest.json" ] \
    && { printf '    FAIL: a partial manifest was written despite the failure\n'; rc=1; }
  [ -f "$ROOT/harness/logs/prepare.log" ] \
    || { printf '    FAIL: no prepare log retained\n'; rc=1; }
  # The failure names the prepare phase, its log, and the exact resume command.
  assert_contains "$out" 'phase "prepare" failed' || rc=1
  assert_contains "$out" "$ROOT/harness/logs/prepare.log" || rc=1
  assert_contains "$out" "prepare $ROOT" || rc=1

  # Execute the exact printed resume with a working jq. The marked partial root
  # (with no manifest) is accepted and prepares to completion.
  run_harness prepare "$ROOT" --binary "$FAKE_FLUENT_SRC" \
    > "$WORK/prepare-resume.out" 2>&1 \
    || { printf '    FAIL: prepare resume did not complete\n'; rc=1; }
  [ -f "$ROOT/harness/manifest.json" ] \
    || { printf '    FAIL: resume did not write a manifest\n'; rc=1; }
  [ -f "$ROOT/harness/.prepare-incomplete" ] \
    && { printf '    FAIL: incomplete marker left after a completed prepare\n'; rc=1; }
  [ "$(jq -r '.safe_phase' "$ROOT/harness/manifest.json")" = "prepared" ] \
    || { printf '    FAIL: resumed prepare did not reach the prepared phase\n'; rc=1; }
  # The rebuilt fixture is sound: run reaches a ready candidate.
  run_harness run "$ROOT" > /dev/null 2>&1 \
    || { printf '    FAIL: run after a resumed prepare did not reach ready\n'; rc=1; }
  return $rc
}

test_prepare_manifest_mv_failure_is_durable() {
  new_workspace
  trap cleanup_workspace RETURN

  # A fake `mv` that fails when the destination is the manifest path, modelling
  # an I/O error during the atomic temp→manifest rename at the end of prepare.
  local trap_bin="$WORK/failing-mv-bin"
  write_failing_mv "$trap_bin"

  local rc=0
  if HOME="$REAL_HOME" FAKE_CMD_LOG="$FAKE_CMD_LOG" PATH="$trap_bin:$PATH" \
    FAILING_MV_DEST="$ROOT/harness/manifest.json" \
    bash "$HARNESS" prepare "$ROOT" --coder-profile codex-balanced \
      --binary "$FAKE_FLUENT_SRC" \
    > "$WORK/prepare.out" 2>&1; then
    printf '    FAIL: prepare should exit non-zero when manifest rename fails\n'; rc=1
  fi
  local out; out="$(cat "$WORK/prepare.out")"
  # The partial root keeps the .prepare-incomplete marker and no valid manifest.
  [ -f "$ROOT/harness/.prepare-incomplete" ] \
    || { printf '    FAIL: no incomplete-prepare marker retained\n'; rc=1; }
  [ -f "$ROOT/harness/manifest.json" ] \
    && { printf '    FAIL: a partial manifest was written despite the failed rename\n'; rc=1; }
  [ -f "$ROOT/harness/logs/prepare.log" ] \
    || { printf '    FAIL: no prepare log retained\n'; rc=1; }
  # The failure names the prepare phase, its log, and the exact resume command.
  assert_contains "$out" 'phase "prepare" failed' || rc=1
  assert_contains "$out" "$ROOT/harness/logs/prepare.log" || rc=1
  assert_contains "$out" "prepare $ROOT" || rc=1

  # Execute the printed resume with a working mv. The marked partial root is
  # accepted and prepares to completion.
  run_harness prepare "$ROOT" --binary "$FAKE_FLUENT_SRC" \
    > "$WORK/prepare-resume.out" 2>&1 \
    || { printf '    FAIL: prepare resume did not complete\n'; rc=1; }
  [ -f "$ROOT/harness/manifest.json" ] \
    || { printf '    FAIL: resume did not write a manifest\n'; rc=1; }
  [ -f "$ROOT/harness/.prepare-incomplete" ] \
    && { printf '    FAIL: incomplete marker left after a completed prepare\n'; rc=1; }
  [ "$(jq -r '.safe_phase' "$ROOT/harness/manifest.json")" = "prepared" ] \
    || { printf '    FAIL: resumed prepare did not reach the prepared phase\n'; rc=1; }
  # The rebuilt fixture is sound: run reaches a ready candidate.
  run_harness run "$ROOT" > /dev/null 2>&1 \
    || { printf '    FAIL: run after a resumed prepare did not reach ready\n'; rc=1; }
  return $rc
}

test_prepare_marker_rm_failure_is_durable() {
  new_workspace
  trap cleanup_workspace RETURN

  # A fake `rm` that fails when asked to remove the .prepare-incomplete marker,
  # modelling a permissions error during the marker-cleanup step that follows a
  # successful manifest write.
  local trap_bin="$WORK/failing-rm-bin"
  write_failing_rm "$trap_bin"

  local rc=0
  if HOME="$REAL_HOME" FAKE_CMD_LOG="$FAKE_CMD_LOG" PATH="$trap_bin:$PATH" \
    FAILING_RM_TARGET="$ROOT/harness/.prepare-incomplete" \
    bash "$HARNESS" prepare "$ROOT" --coder-profile codex-balanced \
      --binary "$FAKE_FLUENT_SRC" \
    > "$WORK/prepare.out" 2>&1; then
    printf '    FAIL: prepare should exit non-zero when marker removal fails\n'; rc=1
  fi
  local out; out="$(cat "$WORK/prepare.out")"
  # The manifest was written but the .prepare-incomplete marker persists, so
  # the root is resumable: prepare reuse sees the marker and rebuilds from scratch.
  [ -f "$ROOT/harness/.prepare-incomplete" ] \
    || { printf '    FAIL: no incomplete-prepare marker retained\n'; rc=1; }
  [ -f "$ROOT/harness/logs/prepare.log" ] \
    || { printf '    FAIL: no prepare log retained\n'; rc=1; }
  # The failure names the prepare phase, its log, and the exact resume command.
  assert_contains "$out" 'phase "prepare" failed' || rc=1
  assert_contains "$out" "$ROOT/harness/logs/prepare.log" || rc=1
  assert_contains "$out" "prepare $ROOT" || rc=1

  # Execute the printed resume with a working rm. The marker takes precedence
  # over any existing manifest and prepare rebuilds to completion.
  run_harness prepare "$ROOT" --binary "$FAKE_FLUENT_SRC" \
    > "$WORK/prepare-resume.out" 2>&1 \
    || { printf '    FAIL: prepare resume did not complete\n'; rc=1; }
  [ -f "$ROOT/harness/manifest.json" ] \
    || { printf '    FAIL: resume did not write a manifest\n'; rc=1; }
  [ -f "$ROOT/harness/.prepare-incomplete" ] \
    && { printf '    FAIL: incomplete marker left after a completed prepare\n'; rc=1; }
  [ "$(jq -r '.safe_phase' "$ROOT/harness/manifest.json")" = "prepared" ] \
    || { printf '    FAIL: resumed prepare did not reach the prepared phase\n'; rc=1; }
  run_harness run "$ROOT" > /dev/null 2>&1 \
    || { printf '    FAIL: run after a resumed prepare did not reach ready\n'; rc=1; }
  return $rc
}

test_malformed_attempt_show_routes_through_b5() {
  new_workspace
  trap cleanup_workspace RETURN

  run_harness prepare "$ROOT" --binary "$FAKE_FLUENT_SRC" > /dev/null 2>&1

  local rc=0
  # The fake returns malformed JSON for attempt show (exit 0). The harness
  # must route the jq parse failure through fail_phase, not exit bare under set -e.
  if HOME="$REAL_HOME" FAKE_CMD_LOG="$FAKE_CMD_LOG" FAKE_ATTEMPT_SHOW_MALFORMED=1 \
    bash "$HARNESS" run "$ROOT" > "$WORK/run.out" 2>&1; then
    printf '    FAIL: run should exit non-zero when attempt show returns malformed JSON\n'; rc=1
  fi
  local out; out="$(cat "$WORK/run.out")"
  # The smoke root is preserved with the failed phase, its log, and a resume.
  [ -d "$ROOT/project/main/.git" ] \
    || { printf '    FAIL: smoke root not preserved\n'; rc=1; }
  assert_contains "$out" 'phase "run" failed' || rc=1
  assert_contains "$out" "$ROOT/harness/logs/attempt-show.log" || rc=1
  assert_contains "$out" "run $ROOT" || rc=1
  # The safe phase stays at prepared.
  [ "$(jq -r '.safe_phase' "$ROOT/harness/manifest.json")" = "prepared" ] \
    || { printf '    FAIL: safe_phase advanced past the malformed JSON failure\n'; rc=1; }

  # Clear the injected failure and resume. It must reach a ready candidate.
  run_harness run "$ROOT" > "$WORK/resume.out" 2>&1 \
    || { printf '    FAIL: resume did not reach a ready candidate\n'; rc=1; }
  [ "$(jq -r '.safe_phase' "$ROOT/harness/manifest.json")" = "ran" ] \
    || { printf '    FAIL: resume did not reach the ready phase\n'; rc=1; }
  assert_contains "$(cat "$WORK/resume.out")" "ready Merge Candidate" || rc=1
  return $rc
}

test_malformed_queue_show_preserves_evidence_and_resumes() {
  new_workspace
  trap cleanup_workspace RETURN

  run_harness prepare "$ROOT" --binary "$FAKE_FLUENT_SRC" > /dev/null 2>&1

  local rc=0
  if HOME="$REAL_HOME" FAKE_CMD_LOG="$FAKE_CMD_LOG" FAKE_QUEUE_SHOW_MALFORMED=1 \
    bash "$HARNESS" run "$ROOT" > "$WORK/run.out" 2>&1; then
    printf '    FAIL: run should exit non-zero when queue show returns malformed JSON\n'; rc=1
  fi
  local out; out="$(cat "$WORK/run.out")"
  [ -d "$ROOT/project/main/.git" ] \
    || { printf '    FAIL: smoke root not preserved\n'; rc=1; }
  assert_contains "$out" 'phase "run" failed' || rc=1
  assert_contains "$out" "$ROOT/harness/logs/queue-status.log" || rc=1
  assert_contains "$out" "run $ROOT" || rc=1
  [ "$(jq -r '.safe_phase' "$ROOT/harness/manifest.json")" = "prepared" ] \
    || { printf '    FAIL: malformed queue state advanced the safe phase\n'; rc=1; }

  run_harness run "$ROOT" > "$WORK/resume.out" 2>&1 \
    || { printf '    FAIL: resume did not reach a ready candidate\n'; rc=1; }
  [ "$(jq -r '.safe_phase' "$ROOT/harness/manifest.json")" = "ran" ] \
    || { printf '    FAIL: resume did not reach the ready phase\n'; rc=1; }
  return $rc
}

test_malformed_candidate_show_routes_through_b5() {
  new_workspace
  trap cleanup_workspace RETURN

  run_harness prepare "$ROOT" --binary "$FAKE_FLUENT_SRC" > /dev/null 2>&1

  local rc=0
  # The fake returns malformed JSON for merge-candidate show (exit 0) while the
  # Learner succeeds normally. The jq parse on the candidate status must route
  # through fail_phase rather than exiting bare under set -e.
  if HOME="$REAL_HOME" FAKE_CMD_LOG="$FAKE_CMD_LOG" FAKE_CANDIDATE_SHOW_MALFORMED=1 \
    bash "$HARNESS" run "$ROOT" > "$WORK/run.out" 2>&1; then
    printf '    FAIL: run should exit non-zero when candidate show returns malformed JSON\n'; rc=1
  fi
  local out; out="$(cat "$WORK/run.out")"
  # The smoke root is preserved with the failed phase, its log, and a resume.
  [ -d "$ROOT/project/main/.git" ] \
    || { printf '    FAIL: smoke root not preserved\n'; rc=1; }
  assert_contains "$out" 'phase "run" failed' || rc=1
  assert_contains "$out" "$ROOT/harness/logs/candidate-show.log" || rc=1
  assert_contains "$out" "run $ROOT" || rc=1
  [ "$(jq -r '.safe_phase' "$ROOT/harness/manifest.json")" = "prepared" ] \
    || { printf '    FAIL: safe_phase advanced past the malformed JSON failure\n'; rc=1; }

  # Clear the injected failure and resume to a ready candidate.
  run_harness run "$ROOT" > "$WORK/resume.out" 2>&1 \
    || { printf '    FAIL: resume did not reach a ready candidate\n'; rc=1; }
  [ "$(jq -r '.safe_phase' "$ROOT/harness/manifest.json")" = "ran" ] \
    || { printf '    FAIL: resume did not reach the ready phase\n'; rc=1; }
  return $rc
}

test_malformed_land_precheck_routes_through_b5() {
  new_workspace
  trap cleanup_workspace RETURN

  run_harness prepare "$ROOT" --binary "$FAKE_FLUENT_SRC" > /dev/null 2>&1
  run_harness run "$ROOT" > /dev/null 2>&1

  local rc=0
  # The fake returns malformed JSON for merge-candidate show during the land
  # precheck (exit 0). The jq parse on the merge status must route through
  # fail_phase rather than exiting bare under set -e.
  if HOME="$REAL_HOME" FAKE_CMD_LOG="$FAKE_CMD_LOG" FAKE_CANDIDATE_SHOW_MALFORMED=1 \
    bash "$HARNESS" land "$ROOT" > "$WORK/land.out" 2>&1; then
    printf '    FAIL: land should exit non-zero when precheck returns malformed JSON\n'; rc=1
  fi
  local out; out="$(cat "$WORK/land.out")"
  [ -d "$ROOT/project/main/.git" ] \
    || { printf '    FAIL: smoke root not preserved\n'; rc=1; }
  assert_contains "$out" 'phase "land" failed' || rc=1
  assert_contains "$out" "$ROOT/harness/logs/land-precheck.log" || rc=1
  assert_contains "$out" "land $ROOT" || rc=1
  [ "$(jq -r '.safe_phase' "$ROOT/harness/manifest.json")" = "ran" ] \
    || { printf '    FAIL: safe_phase advanced past the malformed precheck failure\n'; rc=1; }
  # No merge happened: main does not carry the fix.
  if ( cd "$ROOT/project/main" && ./check.sh ) 2>/dev/null; then
    printf '    FAIL: main carries the fix despite a failed precheck\n'; rc=1
  fi

  # Clear the injected failure and resume to a landed candidate.
  run_harness land "$ROOT" > /dev/null 2>&1 \
    || { printf '    FAIL: land resume did not complete\n'; rc=1; }
  [ "$(jq -r '.safe_phase' "$ROOT/harness/manifest.json")" = "landed" ] \
    || { printf '    FAIL: land resume did not reach the landed phase\n'; rc=1; }
  return $rc
}

test_malformed_landed_show_routes_through_b5() {
  new_workspace
  trap cleanup_workspace RETURN

  run_harness prepare "$ROOT" --binary "$FAKE_FLUENT_SRC" > /dev/null 2>&1
  run_harness run "$ROOT" > /dev/null 2>&1

  local rc=0
  # The fake returns malformed JSON for the post-land merge-candidate show
  # (exit 0, only when stage=landed). The merge itself succeeds first; the
  # jq parse on the merged commit must route through fail_phase.
  if HOME="$REAL_HOME" FAKE_CMD_LOG="$FAKE_CMD_LOG" FAKE_LANDED_SHOW_MALFORMED=1 \
    bash "$HARNESS" land "$ROOT" > "$WORK/land.out" 2>&1; then
    printf '    FAIL: land should exit non-zero when post-land show returns malformed JSON\n'; rc=1
  fi
  local out; out="$(cat "$WORK/land.out")"
  [ -d "$ROOT/project/main/.git" ] \
    || { printf '    FAIL: smoke root not preserved\n'; rc=1; }
  assert_contains "$out" 'phase "land" failed' || rc=1
  assert_contains "$out" "$ROOT/harness/logs/landed-show.log" || rc=1
  assert_contains "$out" "land $ROOT" || rc=1
  # safe_phase stays ran even though the merge happened.
  [ "$(jq -r '.safe_phase' "$ROOT/harness/manifest.json")" = "ran" ] \
    || { printf '    FAIL: safe_phase advanced past the malformed post-land failure\n'; rc=1; }
  # The merge did happen: the fixture passes on main.
  ( cd "$ROOT/project/main" && ./check.sh ) 2>/dev/null \
    || { printf '    FAIL: candidate was not actually merged\n'; rc=1; }

  # Clear the injected failure and resume. The precheck sees the already-merged
  # candidate, skips re-landing, and completes verification.
  run_harness land "$ROOT" > /dev/null 2>&1 \
    || { printf '    FAIL: land resume did not complete\n'; rc=1; }
  [ "$(jq -r '.safe_phase' "$ROOT/harness/manifest.json")" = "landed" ] \
    || { printf '    FAIL: land resume did not reach the landed phase\n'; rc=1; }
  return $rc
}

test_interrupted_manifest_update_keeps_prior_manifest() {
  new_workspace
  trap cleanup_workspace RETURN

  run_harness prepare "$ROOT" --binary "$FAKE_FLUENT_SRC" > /dev/null 2>&1

  # Snapshot the prepared manifest, then crash the very next manifest update.
  local before; before="$(cat "$ROOT/harness/manifest.json")"
  local crash_bin="$WORK/crashing-jq-bin"
  write_crashing_jq "$crash_bin"

  local rc=0
  if HOME="$REAL_HOME" FAKE_CMD_LOG="$FAKE_CMD_LOG" PATH="$crash_bin:$PATH" \
    bash "$HARNESS" run "$ROOT" > "$WORK/run.out" 2>&1; then
    printf '    FAIL: run should fail when a manifest update is interrupted\n'; rc=1
  fi
  local out; out="$(cat "$WORK/run.out")"
  # The interrupted checkpoint is routed through the phase-failure contract, not a
  # bare set -e exit: the operator gets the failed phase, its log, and the exact
  # resume command required by B5.
  assert_contains "$out" 'phase "run" failed' || rc=1
  assert_contains "$out" "$ROOT/harness/logs/manifest.log" || rc=1
  assert_contains "$out" "run $ROOT" || rc=1
  # The prior manifest survives intact and readable: the interrupted write went to
  # a root-local temp and never replaced the manifest, so the rename is atomic.
  if ! jq -e . "$ROOT/harness/manifest.json" > /dev/null 2>&1; then
    printf '    FAIL: manifest is not valid JSON after an interrupted update\n'; rc=1
  fi
  [ "$(cat "$ROOT/harness/manifest.json")" = "$before" ] \
    || { printf '    FAIL: interrupted update corrupted or changed the manifest\n'; rc=1; }
  # The safe checkpoint is unchanged, so the resume repeats run from the prior
  # stage rather than skipping ahead.
  [ "$(jq -r '.safe_phase' "$ROOT/harness/manifest.json")" = "prepared" ] \
    || { printf '    FAIL: safe_phase advanced past the interrupted checkpoint\n'; rc=1; }
  [ "$(jq -r '.run_stage' "$ROOT/harness/manifest.json")" = "null" ] \
    || { printf '    FAIL: run_stage advanced past the interrupted checkpoint\n'; rc=1; }
  # Any residual temp file stayed beside the manifest under the smoke root, not in
  # the system temp directory.
  if ls "${TMPDIR:-/tmp}"/manifest.json.* >/dev/null 2>&1; then
    printf '    FAIL: a manifest temp file escaped to the system temp dir\n'; rc=1
  fi

  # Clear the injected crash and execute the exact printed resume. It repeats run
  # from the prior safe checkpoint and reaches a ready candidate.
  run_harness run "$ROOT" > "$WORK/resume.out" 2>&1 \
    || { printf '    FAIL: run resume did not reach a ready candidate\n'; rc=1; }
  [ "$(jq -r '.safe_phase' "$ROOT/harness/manifest.json")" = "ran" ] \
    || { printf '    FAIL: resume did not reach the ready phase\n'; rc=1; }
  return $rc
}

test_land_precheck_failure_does_not_replay_land() {
  new_workspace
  trap cleanup_workspace RETURN

  run_harness prepare "$ROOT" --binary "$FAKE_FLUENT_SRC" > /dev/null 2>&1
  run_harness run "$ROOT" > /dev/null 2>&1

  local rc=0
  # Land merges the candidate, then the post-land candidate-show trips, leaving
  # safe_phase at ran with the merge already done.
  if HOME="$REAL_HOME" FAKE_CMD_LOG="$FAKE_CMD_LOG" FAKE_POST_LAND_SHOW_FAILS=1 \
    bash "$HARNESS" land "$ROOT" > "$WORK/land.out" 2>&1; then
    printf '    FAIL: land should exit non-zero when post-land verification fails\n'; rc=1
  fi
  ( cd "$ROOT/project/main" && ./check.sh ) 2>/dev/null \
    || { printf '    FAIL: candidate was not actually merged\n'; rc=1; }
  [ "$(jq -r '.safe_phase' "$ROOT/harness/manifest.json")" = "ran" ] \
    || { printf '    FAIL: safe_phase advanced past a failed land verification\n'; rc=1; }

  local before
  before="$(grep -c '^merge-candidate land$' "$FAKE_CMD_LOG" || true)"

  # Resume while the candidate-show still trips: the precheck cannot read the
  # merge state. The harness must treat this as a durable land failure and must
  # NOT replay the non-idempotent land against an already-merged candidate.
  if HOME="$REAL_HOME" FAKE_CMD_LOG="$FAKE_CMD_LOG" FAKE_POST_LAND_SHOW_FAILS=1 \
    bash "$HARNESS" land "$ROOT" > "$WORK/land-resume.out" 2>&1; then
    printf '    FAIL: land resume should fail when the precheck cannot read the candidate\n'; rc=1
  fi
  local out; out="$(cat "$WORK/land-resume.out")"
  assert_contains "$out" 'phase "land" failed' || rc=1
  assert_contains "$out" "$ROOT/harness/logs/land-precheck.log" || rc=1
  assert_contains "$out" "land $ROOT" || rc=1
  local after
  after="$(grep -c '^merge-candidate land$' "$FAKE_CMD_LOG" || true)"
  [ "$after" -eq "$before" ] \
    || { printf '    FAIL: resume replayed merge-candidate land (%s -> %s)\n' "$before" "$after"; rc=1; }
  # The merge state is untouched and safe_phase still ran, so a later clean resume
  # can finish the verification.
  [ "$(jq -r '.safe_phase' "$ROOT/harness/manifest.json")" = "ran" ] \
    || { printf '    FAIL: safe_phase advanced past a failed precheck\n'; rc=1; }
  run_harness land "$ROOT" > /dev/null 2>&1 \
    || { printf '    FAIL: clean land resume did not complete\n'; rc=1; }
  [ "$(jq -r '.safe_phase' "$ROOT/harness/manifest.json")" = "landed" ] \
    || { printf '    FAIL: clean resume did not reach the landed phase\n'; rc=1; }
  return $rc
}

test_land_precheck_unknown_state_does_not_replay_land() {
  new_workspace
  trap cleanup_workspace RETURN

  run_harness prepare "$ROOT" --binary "$FAKE_FLUENT_SRC" > /dev/null 2>&1
  run_harness run "$ROOT" > /dev/null 2>&1

  local rc=0
  # The precheck returns valid JSON with an unrecognised merge_state.status.
  # The harness must reject it through fail_phase rather than treating any
  # non-"merged" value as permission to call merge-candidate land.
  local before
  before="$(grep -c '^merge-candidate land$' "$FAKE_CMD_LOG" || true)"

  if HOME="$REAL_HOME" FAKE_CMD_LOG="$FAKE_CMD_LOG" FAKE_CANDIDATE_SHOW_UNKNOWN_STATUS=1 \
    bash "$HARNESS" land "$ROOT" > "$WORK/land.out" 2>&1; then
    printf '    FAIL: land should exit non-zero when precheck returns unknown status\n'; rc=1
  fi
  local out; out="$(cat "$WORK/land.out")"
  [ -d "$ROOT/project/main/.git" ] \
    || { printf '    FAIL: smoke root not preserved\n'; rc=1; }
  assert_contains "$out" 'phase "land" failed' || rc=1
  assert_contains "$out" "$ROOT/harness/logs/land-precheck.log" || rc=1
  assert_contains "$out" "land $ROOT" || rc=1

  local after
  after="$(grep -c '^merge-candidate land$' "$FAKE_CMD_LOG" || true)"
  [ "$after" -eq "$before" ] \
    || { printf '    FAIL: unknown precheck status triggered merge-candidate land (%s -> %s)\n' "$before" "$after"; rc=1; }
  [ "$(jq -r '.safe_phase' "$ROOT/harness/manifest.json")" = "ran" ] \
    || { printf '    FAIL: safe_phase advanced past an unknown precheck status\n'; rc=1; }
  # No merge happened: the fixture still fails on main.
  if ( cd "$ROOT/project/main" && ./check.sh ) 2>/dev/null; then
    printf '    FAIL: main carries the fix despite an unknown precheck status\n'; rc=1
  fi

  # Clear the injected failure; the resume sees pending and lands normally.
  run_harness land "$ROOT" > /dev/null 2>&1 \
    || { printf '    FAIL: clean land resume did not complete\n'; rc=1; }
  [ "$(jq -r '.safe_phase' "$ROOT/harness/manifest.json")" = "landed" ] \
    || { printf '    FAIL: clean resume did not reach the landed phase\n'; rc=1; }
  return $rc
}

test_relative_installer_override_is_durable() {
  new_workspace
  trap cleanup_workspace RETURN

  # A local installer referenced by a path relative to the prepare CWD.
  mkdir -p "$WORK/tools"
  write_fake_installer "$WORK/tools/install.sh"

  local rc=0
  ( cd "$WORK" && HOME="$REAL_HOME" FAKE_CMD_LOG="$FAKE_CMD_LOG" \
      bash "$HARNESS" prepare "$ROOT" --coder-profile codex-balanced \
        --installer ./tools/install.sh ) \
    > /dev/null 2>&1 || { printf '    FAIL: prepare failed\n'; rc=1; }
  # prepare resolved the relative override to an absolute boundary in the manifest.
  local boundary
  boundary="$(jq -r '.install_boundary' "$ROOT/harness/manifest.json")"
  case "$boundary" in
    installer:/*) : ;;
    *) printf '    FAIL: installer boundary not absolute: %s\n' "$boundary"; rc=1 ;;
  esac
  # run from a different working directory still resolves the local installer.
  ( cd / && HOME="$REAL_HOME" FAKE_CMD_LOG="$FAKE_CMD_LOG" \
      bash "$HARNESS" run "$ROOT" ) > "$WORK/run.out" 2>&1 \
    || { printf '    FAIL: run could not resolve the relative installer from another dir\n'; rc=1; }
  [ "$(jq -r '.safe_phase' "$ROOT/harness/manifest.json")" = "ran" ] \
    || { printf '    FAIL: run did not reach ready with a resolved installer\n'; rc=1; }
  return $rc
}

test_land_replay_safe_after_post_land_failure() {
  new_workspace
  trap cleanup_workspace RETURN

  run_harness prepare "$ROOT" --binary "$FAKE_FLUENT_SRC" > /dev/null 2>&1
  run_harness run "$ROOT" > /dev/null 2>&1

  local rc=0
  # Land merges the candidate, then the post-land candidate-show trips. The merge
  # already happened, so the phase must fail without advancing safe_phase.
  if HOME="$REAL_HOME" FAKE_CMD_LOG="$FAKE_CMD_LOG" FAKE_POST_LAND_SHOW_FAILS=1 \
    bash "$HARNESS" land "$ROOT" > "$WORK/land.out" 2>&1; then
    printf '    FAIL: land should exit non-zero when post-land verification fails\n'; rc=1
  fi
  assert_contains "$(cat "$WORK/land.out")" 'phase "land" failed' || rc=1
  # The candidate really did merge: the fixture now passes on main.
  ( cd "$ROOT/project/main" && ./check.sh ) 2>/dev/null \
    || { printf '    FAIL: candidate was not actually merged\n'; rc=1; }
  # safe_phase stays ran so the printed resume repeats land.
  [ "$(jq -r '.safe_phase' "$ROOT/harness/manifest.json")" = "ran" ] \
    || { printf '    FAIL: safe_phase advanced past a failed land verification\n'; rc=1; }

  local before
  before="$(grep -c '^merge-candidate land$' "$FAKE_CMD_LOG" || true)"

  # Resume with the failure cleared. It must finish the verification WITHOUT a
  # second land — the fake rejects a second land, proving the resume relies on
  # the durable candidate state rather than replaying the merge.
  run_harness land "$ROOT" > "$WORK/land-resume.out" 2>&1 \
    || { printf '    FAIL: land resume did not complete\n'; rc=1; }
  local after
  after="$(grep -c '^merge-candidate land$' "$FAKE_CMD_LOG" || true)"
  [ "$after" -eq "$before" ] \
    || { printf '    FAIL: resume replayed merge-candidate land (%s -> %s)\n' "$before" "$after"; rc=1; }
  [ "$(jq -r '.safe_phase' "$ROOT/harness/manifest.json")" = "landed" ] \
    || { printf '    FAIL: land resume did not reach the landed phase\n'; rc=1; }
  return $rc
}

test_moved_smoke_root_is_rejected() {
  new_workspace
  trap cleanup_workspace RETURN

  run_harness prepare "$ROOT" --binary "$FAKE_FLUENT_SRC" > /dev/null 2>&1

  local rc=0
  # Copy the prepared root elsewhere. Its manifest still records the original
  # smoke_root and the original absolute binary path under $ROOT.
  local moved="$WORK/moved-smoke"
  cp -R "$ROOT" "$moved"

  # run against the copied root must refuse: the manifest is bound to $ROOT, so
  # advancing it here would run the binary from the old root and split state.
  if run_harness run "$moved" > "$WORK/run.out" 2>&1; then
    printf '    FAIL: run accepted a copied smoke root\n'; rc=1
  fi
  assert_contains "$(cat "$WORK/run.out")" "does not match this root" || rc=1
  # No Fluent command ran against the copied root.
  [ -s "$FAKE_CMD_LOG" ] \
    && { printf '    FAIL: a Fluent command ran against the copied root\n'; rc=1; }
  # The copied manifest is left untouched — never advanced from prepared.
  [ "$(jq -r '.safe_phase' "$moved/harness/manifest.json")" = "prepared" ] \
    || { printf '    FAIL: copied manifest was advanced\n'; rc=1; }

  # prepare reuse of the copied root is likewise refused.
  if run_harness prepare "$moved" --binary "$FAKE_FLUENT_SRC" \
    > "$WORK/prep.out" 2>&1; then
    printf '    FAIL: prepare reused a copied smoke root\n'; rc=1
  fi
  assert_contains "$(cat "$WORK/prep.out")" "does not match this root" || rc=1

  # land against the copied root is refused too.
  if run_harness land "$moved" > "$WORK/land.out" 2>&1; then
    printf '    FAIL: land accepted a copied smoke root\n'; rc=1
  fi
  assert_contains "$(cat "$WORK/land.out")" "does not match this root" || rc=1

  # The original root still works normally.
  run_harness run "$ROOT" > /dev/null 2>&1 \
    || { printf '    FAIL: original root did not reach ready\n'; rc=1; }
  [ "$(jq -r '.safe_phase' "$ROOT/harness/manifest.json")" = "ran" ] \
    || { printf '    FAIL: original root did not reach the ready phase\n'; rc=1; }
  return $rc
}

test_automated_test_uses_only_local_doubles() {
  new_workspace
  trap cleanup_workspace RETURN

  # A fake installer selects Fluent through the boundary without the network.
  local installer="$WORK/fake-installer"
  write_fake_installer "$installer"
  INSTALLER_HOME_LOG="$WORK/installer-home"
  : > "$INSTALLER_HOME_LOG"

  # Tripwires catch any model launch or network fetch.
  TRIPWIRE_LOG="$WORK/tripwire-log"
  : > "$TRIPWIRE_LOG"
  local tripwire_bin="$WORK/tripwire-bin"
  write_tripwires "$tripwire_bin"

  local rc=0
  # Drive the full journey with the local doubles on PATH.
  HOME="$REAL_HOME" FAKE_CMD_LOG="$FAKE_CMD_LOG" PATH="$tripwire_bin:$PATH" \
    bash "$HARNESS" prepare "$ROOT" --coder-profile codex-balanced \
      --installer "$installer" > /dev/null 2>&1 || rc=1
  HOME="$REAL_HOME" FAKE_CMD_LOG="$FAKE_CMD_LOG" PATH="$tripwire_bin:$PATH" \
    INSTALLER_HOME_LOG="$INSTALLER_HOME_LOG" \
    bash "$HARNESS" run "$ROOT" > /dev/null 2>&1 || rc=1
  HOME="$REAL_HOME" FAKE_CMD_LOG="$FAKE_CMD_LOG" PATH="$tripwire_bin:$PATH" \
    bash "$HARNESS" land "$ROOT" > /dev/null 2>&1 || rc=1

  # No model or network double was ever invoked.
  if [ -s "$TRIPWIRE_LOG" ]; then
    printf '    FAIL: harness reached the network or a model: %s\n' \
      "$(cat "$TRIPWIRE_LOG")"
    rc=1
  fi
  # The installer ran under the isolated smoke home, not the operator's.
  [ "$(cat "$INSTALLER_HOME_LOG")" = "$ROOT/home" ] \
    || { printf '    FAIL: installer ran with HOME=%s, want %s\n' \
           "$(cat "$INSTALLER_HOME_LOG")" "$ROOT/home"; rc=1; }
  # The operator's real home is untouched.
  [ "$(ls -A "$REAL_HOME")" = "sentinel" ] \
    || { printf '    FAIL: operator home changed\n'; rc=1; }
  [ "$(cat "$REAL_HOME/sentinel")" = "untouched" ] \
    || { printf '    FAIL: operator home content changed\n'; rc=1; }
  # The journey still completed against the local doubles.
  [ "$(jq -r '.safe_phase' "$ROOT/harness/manifest.json")" = "landed" ] \
    || { printf '    FAIL: journey did not complete\n'; rc=1; }
  return $rc
}

test_run_evidence_copy_fails_preserves_evidence_and_resume() {
  new_workspace
  trap cleanup_workspace RETURN

  run_harness prepare "$ROOT" --binary "$FAKE_FLUENT_SRC" > /dev/null 2>&1

  # Inject a cp failure for the run-phase evidence copy that occurs before
  # safe_phase is advanced to "ran".
  local trap_bin="$WORK/failing-cp-bin"
  write_failing_cp "$trap_bin"

  local rc=0
  if HOME="$REAL_HOME" FAKE_CMD_LOG="$FAKE_CMD_LOG" PATH="$trap_bin:$PATH" \
    FAILING_CP_DEST="$ROOT/evidence/merge-candidate.json" \
    bash "$HARNESS" run "$ROOT" > "$WORK/run.out" 2>&1; then
    printf '    FAIL: run should exit non-zero when the evidence copy fails\n'; rc=1
  fi
  local out; out="$(cat "$WORK/run.out")"
  # The smoke root is preserved with the failed phase, its log, and a resume.
  [ -d "$ROOT/project/main/.git" ] \
    || { printf '    FAIL: smoke root not preserved\n'; rc=1; }
  assert_contains "$out" 'phase "run" failed' || rc=1
  assert_contains "$out" "$ROOT/harness/logs/manifest.log" || rc=1
  assert_contains "$out" "run $ROOT" || rc=1
  # The safe phase must not advance to "ran" when the evidence copy fails.
  [ "$(jq -r '.safe_phase' "$ROOT/harness/manifest.json")" = "prepared" ] \
    || { printf '    FAIL: safe_phase advanced past the evidence copy failure\n'; rc=1; }

  # Clear the injected failure and resume. It must reach a ready candidate.
  run_harness run "$ROOT" > "$WORK/resume.out" 2>&1 \
    || { printf '    FAIL: run resume did not reach a ready candidate\n'; rc=1; }
  [ "$(jq -r '.safe_phase' "$ROOT/harness/manifest.json")" = "ran" ] \
    || { printf '    FAIL: resume did not reach the ready phase\n'; rc=1; }
  assert_contains "$(cat "$WORK/resume.out")" "ready Merge Candidate" || rc=1
  return $rc
}

test_land_evidence_copy_fails_preserves_evidence_and_resume() {
  new_workspace
  trap cleanup_workspace RETURN

  run_harness prepare "$ROOT" --binary "$FAKE_FLUENT_SRC" > /dev/null 2>&1
  run_harness run "$ROOT" > /dev/null 2>&1

  # Inject a cp failure for the land-phase evidence copy that occurs before
  # safe_phase is advanced to "landed".
  local trap_bin="$WORK/failing-cp-bin"
  write_failing_cp "$trap_bin"

  local rc=0
  if HOME="$REAL_HOME" FAKE_CMD_LOG="$FAKE_CMD_LOG" PATH="$trap_bin:$PATH" \
    FAILING_CP_DEST="$ROOT/evidence/merged-candidate.json" \
    bash "$HARNESS" land "$ROOT" > "$WORK/land.out" 2>&1; then
    printf '    FAIL: land should exit non-zero when the evidence copy fails\n'; rc=1
  fi
  local out; out="$(cat "$WORK/land.out")"
  # The smoke root is preserved with the failed phase, its log, and a resume.
  [ -d "$ROOT/project/main/.git" ] \
    || { printf '    FAIL: smoke root not preserved\n'; rc=1; }
  assert_contains "$out" 'phase "land" failed' || rc=1
  assert_contains "$out" "$ROOT/harness/logs/manifest.log" || rc=1
  assert_contains "$out" "land $ROOT" || rc=1
  # The safe phase must not advance to "landed" when the evidence copy fails.
  [ "$(jq -r '.safe_phase' "$ROOT/harness/manifest.json")" = "ran" ] \
    || { printf '    FAIL: safe_phase advanced past the evidence copy failure\n'; rc=1; }
  # The merge itself completed; the fixture test passes on main.
  ( cd "$ROOT/project/main" && ./check.sh ) 2>/dev/null \
    || { printf '    FAIL: candidate was not actually merged before the copy failure\n'; rc=1; }

  # Clear the injected failure and resume. The precheck sees the already-merged
  # candidate, skips re-landing, and completes verification.
  run_harness land "$ROOT" > "$WORK/land-resume.out" 2>&1 \
    || { printf '    FAIL: land resume did not complete\n'; rc=1; }
  [ "$(jq -r '.safe_phase' "$ROOT/harness/manifest.json")" = "landed" ] \
    || { printf '    FAIL: resume did not reach the landed phase\n'; rc=1; }
  return $rc
}

printf 'test-first-run-smoke-harness\n\n'

run_test "assertions match large literal output" test_assert_contains_large_literal_output

run_test "prepare is isolated" test_prepare_is_isolated
run_test "prepare leaves Fluent uninitialized and records Codex home" \
  test_prepare_leaves_fluent_uninitialized_and_records_codex_home
run_test "prepare requires a supported profile" \
  test_prepare_requires_a_supported_profile
run_test "prepare records every curated profile" \
  test_prepare_records_every_curated_profile
run_test "run configures init and commits before attempt" \
  test_run_configures_init_and_commits_before_attempt
run_test "tester extractor normalizes fixture contract" \
  test_tester_extractor_normalizes_fixture_contract
run_test "skill handoff failure preserves evidence and resumes" \
  test_skill_handoff_failure_preserves_evidence_and_resumes
run_test "skill handoff rejects wrong ownership and resumes" \
  test_skill_handoff_rejects_wrong_ownership_and_resumes
run_test "schema one manifest is rejected without migration" \
  test_schema_one_manifest_is_rejected_without_migration
run_test "prepare rejects nonempty root" test_prepare_rejects_nonempty_root
run_test "prepare encodes json significant root" \
  test_prepare_encodes_json_significant_root
run_test "run uses public sequence and stops before land" \
  test_run_uses_public_sequence_and_stops_before_land
run_test "scheduler exit rereads terminal queue state" \
  test_scheduler_exit_rereads_terminal_queue_state
run_test "scheduler failed terminal preserves evidence" \
  test_scheduler_failed_terminal_preserves_evidence
run_test "interruption reaps scheduler child" \
  test_interruption_reaps_scheduler_child
run_test "run waits for learner success" test_run_waits_for_learner_success
run_test "run waits for complete attempt" test_run_waits_for_complete_attempt
run_test "run resumes same attempt after scheduler pause" \
  test_run_resumes_same_attempt_after_scheduler_pause
run_test "ready handoff is actionable" test_ready_handoff_is_actionable
run_test "ready handoff quotes paths with spaces" \
  test_ready_handoff_quotes_paths_with_spaces
run_test "land verifies target" test_land_verifies_target
run_test "land failure preserves evidence and resume" \
  test_land_failure_preserves_evidence_and_resume
run_test "failure preserves evidence and resume" \
  test_failure_preserves_evidence_and_resume
run_test "run resume after init commit does not duplicate work state" \
  test_run_resume_after_init_commit_does_not_duplicate_work_state
run_test "installer failure preserves evidence and resume" \
  test_installer_failure_preserves_evidence_and_resume
run_test "prepare failure preserves evidence and resume" \
  test_prepare_failure_preserves_evidence_and_resume
run_test "prepare directory failure preserves evidence and resume" \
  test_prepare_directory_failure_preserves_evidence_and_resume
run_test "prepare initial manifest write failure is durable" \
  test_prepare_initial_manifest_write_failure_is_durable
run_test "prepare manifest mv failure is durable" \
  test_prepare_manifest_mv_failure_is_durable
run_test "prepare marker rm failure is durable" \
  test_prepare_marker_rm_failure_is_durable
run_test "malformed queue show preserves evidence and resumes" \
  test_malformed_queue_show_preserves_evidence_and_resumes
run_test "malformed attempt show routes through b5" \
  test_malformed_attempt_show_routes_through_b5
run_test "malformed candidate show routes through b5" \
  test_malformed_candidate_show_routes_through_b5
run_test "malformed land precheck routes through b5" \
  test_malformed_land_precheck_routes_through_b5
run_test "malformed landed show routes through b5" \
  test_malformed_landed_show_routes_through_b5
run_test "interrupted manifest update keeps prior manifest" \
  test_interrupted_manifest_update_keeps_prior_manifest
run_test "land precheck failure does not replay land" \
  test_land_precheck_failure_does_not_replay_land
run_test "land precheck unknown state does not replay land" \
  test_land_precheck_unknown_state_does_not_replay_land
run_test "relative installer override is durable" \
  test_relative_installer_override_is_durable
run_test "land replay safe after post-land failure" \
  test_land_replay_safe_after_post_land_failure
run_test "moved smoke root is rejected" test_moved_smoke_root_is_rejected
run_test "automated test uses only local doubles" \
  test_automated_test_uses_only_local_doubles
run_test "run evidence copy fails preserves evidence and resume" \
  test_run_evidence_copy_fails_preserves_evidence_and_resume
run_test "land evidence copy fails preserves evidence and resume" \
  test_land_evidence_copy_fails_preserves_evidence_and_resume

summarize_and_exit
