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

# Count `attempt run` invocations so the Learner can succeed only after a
# configurable number of runs, modelling a candidate that exists before the
# Learner has succeeded.
RUNS_FILE="$STATE_DIR/runs"
[ -f "$RUNS_FILE" ] || printf '0\n' > "$RUNS_FILE"
runs() { cat "$RUNS_FILE"; }
# The Learner reports "succeeded" once this many `attempt run` calls have
# happened; 0 means never (it stays in-progress forever).
LEARNER_SUCCEED_AT="${FAKE_LEARNER_SUCCEED_AT:-1}"
learning_status() {
  local n; n="$(runs)"
  if [ "$LEARNER_SUCCEED_AT" -ne 0 ] && [ "$n" -ge "$LEARNER_SUCCEED_AT" ]; then
    printf 'succeeded'
  else
    printf 'in-progress'
  fi
}

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
        case "$(stage)" in attempt|ran) ;; *) reject "attempt run out of order (stage=$(stage))" ;; esac
        if [ "${FAKE_ATTEMPT_RUN_FAILS:-0}" = "1" ]; then
          reject "simulated attempt run failure"
        fi
        printf '%s\n' "$(( $(runs) + 1 ))" > "$RUNS_FILE"
        # The first run produces the fix on a candidate branch, leaving main
        # untouched. The candidate exists before the Learner has succeeded.
        if [ "$(stage)" = "attempt" ]; then
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
        fi
        ;;
      show)
        # Expose the stored Attempt as JSON, including its Learner state.
        LEARN="$(learning_status)"
        ASTATUS="executing"
        [ "$LEARN" = "succeeded" ] && ASTATUS="complete"
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

test_prepare_encodes_json_significant_root() {
  new_workspace
  trap cleanup_workspace RETURN

  # A valid filesystem path containing a JSON-significant double quote. A raw
  # heredoc manifest would emit invalid JSON here and break the later jq reads.
  local quoted_root="$WORK/say \"hi\"/smoke"
  HOME="$REAL_HOME" FAKE_CMD_LOG="$FAKE_CMD_LOG" \
    bash "$HARNESS" prepare "$quoted_root" --binary "$FAKE_FLUENT_SRC" \
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
  # The recorded commands follow the public first-run sequence. `attempt show`
  # gates readiness on the stored Learner state before the candidate is shown.
  local expected
  expected="$(printf 'init\nwork-item create\nattempt create\nattempt run\nattempt show\nmerge-candidate show\n')"
  if [ "$(cat "$FAKE_CMD_LOG")" != "$expected" ]; then
    printf '    FAIL: unexpected command sequence:\n%s\n' "$(cat "$FAKE_CMD_LOG")"
    rc=1
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

test_run_waits_for_learner_success() {
  new_workspace
  trap cleanup_workspace RETURN

  run_harness prepare "$ROOT" --binary "$FAKE_FLUENT_SRC" > /dev/null 2>&1

  local rc=0
  # The candidate is produced on the first attempt run, but the Learner never
  # reaches "succeeded". The harness must not hand off a ready candidate.
  if HOME="$REAL_HOME" FAKE_CMD_LOG="$FAKE_CMD_LOG" FAKE_LEARNER_SUCCEED_AT=0 \
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

test_run_reaches_ready_after_delayed_learner() {
  new_workspace
  trap cleanup_workspace RETURN

  run_harness prepare "$ROOT" --binary "$FAKE_FLUENT_SRC" > /dev/null 2>&1

  local rc=0
  # The Learner succeeds only on the second run: the candidate exists first.
  HOME="$REAL_HOME" FAKE_CMD_LOG="$FAKE_CMD_LOG" FAKE_LEARNER_SUCCEED_AT=2 \
    bash "$HARNESS" run "$ROOT" > "$WORK/run.out" 2>&1 \
    || { printf '    FAIL: run did not reach a ready candidate\n'; rc=1; }
  # The harness advanced the Attempt more than once before handing off.
  if [ "$(grep -c '^attempt run$' "$FAKE_CMD_LOG")" -lt 2 ]; then
    printf '    FAIL: harness handed off on the first candidate\n'; rc=1
  fi
  # It then reached the ready state.
  [ "$(jq -r '.safe_phase' "$ROOT/harness/manifest.json")" = "ran" ] \
    || { printf '    FAIL: safe_phase not ran after delayed Learner\n'; rc=1; }
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
    bash "$HARNESS" prepare "$spaced_root" --binary "$FAKE_FLUENT_SRC" > /dev/null 2>&1
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

  run_harness prepare "$ROOT" --binary "$FAKE_FLUENT_SRC" > /dev/null 2>&1
  run_harness run "$ROOT" > /dev/null 2>&1
  run_harness land "$ROOT" > "$WORK/land.out" 2>&1

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
  # Force the Attempt run to fail mid-phase.
  if HOME="$REAL_HOME" FAKE_CMD_LOG="$FAKE_CMD_LOG" FAKE_ATTEMPT_RUN_FAILS=1 \
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
  # init, the Work Item, and the Attempt were created before the failure, so the
  # harness checkpointed them and must not replay them on resume.
  [ "$(jq -r '.run_stage' "$ROOT/harness/manifest.json")" = "attempt" ] \
    || { printf '    FAIL: run_stage not checkpointed at attempt\n'; rc=1; }

  # Clear the injected failure and execute the exact printed resume command. It
  # must reach a ready candidate by reusing the existing Work Item and Attempt,
  # not by re-running the non-idempotent setup commands.
  run_harness run "$ROOT" > "$WORK/resume.out" 2>&1 \
    || { printf '    FAIL: resume did not reach a ready candidate\n'; rc=1; }
  [ "$(grep -c '^init$' "$FAKE_CMD_LOG")" -eq 1 ] \
    || { printf '    FAIL: resume replayed init\n'; rc=1; }
  [ "$(grep -c '^work-item create$' "$FAKE_CMD_LOG")" -eq 1 ] \
    || { printf '    FAIL: resume replayed work-item create\n'; rc=1; }
  [ "$(grep -c '^attempt create$' "$FAKE_CMD_LOG")" -eq 1 ] \
    || { printf '    FAIL: resume replayed attempt create\n'; rc=1; }
  [ "$(jq -r '.safe_phase' "$ROOT/harness/manifest.json")" = "ran" ] \
    || { printf '    FAIL: resume did not reach the ready phase\n'; rc=1; }
  assert_contains "$(cat "$WORK/resume.out")" "ready Merge Candidate" || rc=1
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
    bash "$HARNESS" prepare "$ROOT" --binary "$FAKE_FLUENT_SRC" \
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
    bash "$HARNESS" prepare "$ROOT" --binary "$FAKE_FLUENT_SRC" \
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
    bash "$HARNESS" prepare "$ROOT" --binary "$FAKE_FLUENT_SRC" \
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

test_relative_installer_override_is_durable() {
  new_workspace
  trap cleanup_workspace RETURN

  # A local installer referenced by a path relative to the prepare CWD.
  mkdir -p "$WORK/tools"
  write_fake_installer "$WORK/tools/install.sh"

  local rc=0
  ( cd "$WORK" && HOME="$REAL_HOME" FAKE_CMD_LOG="$FAKE_CMD_LOG" \
      bash "$HARNESS" prepare "$ROOT" --installer ./tools/install.sh ) \
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
    bash "$HARNESS" prepare "$ROOT" --installer "$installer" > /dev/null 2>&1 || rc=1
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

run_test "prepare is isolated" test_prepare_is_isolated
run_test "prepare rejects nonempty root" test_prepare_rejects_nonempty_root
run_test "prepare encodes json significant root" \
  test_prepare_encodes_json_significant_root
run_test "run uses public sequence and stops before land" \
  test_run_uses_public_sequence_and_stops_before_land
run_test "run waits for learner success" test_run_waits_for_learner_success
run_test "run reaches ready after delayed learner" \
  test_run_reaches_ready_after_delayed_learner
run_test "ready handoff is actionable" test_ready_handoff_is_actionable
run_test "ready handoff quotes paths with spaces" \
  test_ready_handoff_quotes_paths_with_spaces
run_test "land verifies target" test_land_verifies_target
run_test "land failure preserves evidence and resume" \
  test_land_failure_preserves_evidence_and_resume
run_test "failure preserves evidence and resume" \
  test_failure_preserves_evidence_and_resume
run_test "installer failure preserves evidence and resume" \
  test_installer_failure_preserves_evidence_and_resume
run_test "prepare failure preserves evidence and resume" \
  test_prepare_failure_preserves_evidence_and_resume
run_test "prepare directory failure preserves evidence and resume" \
  test_prepare_directory_failure_preserves_evidence_and_resume
run_test "prepare initial manifest write failure is durable" \
  test_prepare_initial_manifest_write_failure_is_durable
run_test "interrupted manifest update keeps prior manifest" \
  test_interrupted_manifest_update_keeps_prior_manifest
run_test "land precheck failure does not replay land" \
  test_land_precheck_failure_does_not_replay_land
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
