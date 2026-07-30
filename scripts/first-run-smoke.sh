#!/usr/bin/env bash
#
# first-run-smoke.sh — operator harness for Fluent's clean-room first-run gate.
#
# Exercises Fluent's public first-run journey inside a fresh repository and an
# isolated home: install or select Fluent, initialize the repository, create a
# small deterministic Work Item, run its Attempt through the Learner, inspect
# the ready Merge Candidate, and land it only after an explicit second command.
#
# The harness runs in explicit, resumable phases so an operator can inspect
# evidence at every safety boundary:
#
#   first-run-smoke.sh prepare <root> [--installer <url|path>] [--binary <path>]
#   first-run-smoke.sh run     <root>
#   first-run-smoke.sh land    <root>
#
# All state — an isolated home, the Git repository, the selected binary, phase
# logs, and evidence — lives beneath <root>. The harness never deletes <root>,
# on success or failure, so every failure keeps the evidence needed to explain
# it.

set -euo pipefail

if [[ "${TRACE-0}" == "1" ]]; then set -o xtrace; fi

readonly SCHEMA_VERSION=1
readonly WORK_ITEM_ID="clean-room-fixture"
readonly ATTEMPT_ID="attempt-1"
readonly DEFAULT_INSTALLER="https://fluent.computer/install"

# Absolute path to this script, for the resume commands the harness prints.
SELF="$(cd "$(dirname "$0")" && pwd)/$(basename "$0")"
readonly SELF

# ---------------------------------------------------------------------------
# Diagnostics
# ---------------------------------------------------------------------------

# Report an error and exit without ever touching the smoke root.
die() {
  printf 'error: %s\n' "$1" >&2
  exit 1
}

info() {
  printf '%s\n' "$1"
}

# Shell-escape a value so a printed command stays executable even when a path
# contains spaces or other shell metacharacters.
shq() {
  printf '%q' "$1"
}

# Begin a log section without discarding evidence from a prior failed attempt.
# A resumed phase re-runs the same command against the same log path; appending
# a banner instead of truncating keeps the earlier failed command, its output,
# and its exit status preserved beneath the smoke root.
begin_log() {
  local logfile="$1" label="$2"
  printf '\n===== %s (%s) =====\n' "$label" "$(date -u '+%Y-%m-%dT%H:%M:%SZ')" \
    >> "$logfile"
}

# Report a phase failure with its log and the exact resume command, then exit
# non-zero. The smoke root is preserved untouched.
fail_phase() {
  local root="$1" phase="$2" log="$3" resume_phase="$4"
  printf '\n' >&2
  printf 'error: smoke phase "%s" failed\n' "$phase" >&2
  printf '  smoke root preserved: %s\n' "$root" >&2
  printf '  phase log: %s\n' "$log" >&2
  printf '  resume with: %s %s %s\n' "$(shq "$SELF")" "$resume_phase" "$(shq "$root")" >&2
  exit 1
}

# ---------------------------------------------------------------------------
# Manifest — a JSON record of durable phase state beneath the smoke root.
# Never sourced; read and written with jq.
# ---------------------------------------------------------------------------

manifest_path() {
  printf '%s/harness/manifest.json' "$1"
}

# Read one jq field from the manifest.
manifest_get() {
  local root="$1" filter="$2"
  jq -r "$filter" "$(manifest_path "$root")"
}

# Reject a manifest whose recorded smoke_root does not equal the normalized root
# supplied on this invocation. A prepared root that is copied or moved keeps the
# original absolute paths inside its manifest — the selected Fluent binary and the
# install boundary — so reusing it under a new root would run the binary from the
# old root and split one smoke's durable state across two roots. Binding every
# phase to its recorded root keeps the clean-room contained in the one selected
# external root.
verify_manifest_root() {
  local root="$1" recorded
  recorded="$(manifest_get "$root" '.smoke_root')"
  [ "$recorded" = "$root" ] \
    || die "manifest smoke_root ($recorded) does not match this root ($root); a copied or moved smoke root is not supported"
}

# Set one manifest field to a string value (read-modify-write via a temp file).
# The temporary copy is allocated beside the manifest under the smoke root so the
# rename is an atomic same-directory replacement — never a cross-filesystem copy —
# and no durable-state file ever escapes the one-root clean-room boundary. A jq
# write that fails leaves the prior manifest untouched and readable, discards the
# partial temp, and returns non-zero so the caller can route the interrupted
# checkpoint through the phase-failure contract instead of the rename overwriting
# the good manifest with a partial document.
manifest_set() {
  local root="$1" key="$2" value="$3"
  local path tmp
  path="$(manifest_path "$root")"
  tmp="$(mktemp "${path}.XXXXXX")"
  if ! jq --arg v "$value" ".${key} = \$v" "$path" > "$tmp"; then
    printf 'error: manifest checkpoint failed for key %s\n' "$key" >&2
    rm -f "$tmp"
    return 1
  fi
  mv "$tmp" "$path"
}

# Write a durable manifest checkpoint, routing an interrupted write through the
# phase-failure contract. A bare `manifest_set` under `set -e` would exit the
# shell the instant its jq write failed, denying the operator the failed phase,
# its log, and the exact resume command B5 requires. Capturing the failure here
# preserves that handoff and keeps the prior manifest as the safe checkpoint the
# resume continues from.
checkpoint() {
  local root="$1" phase="$2" log="$3" key="$4" value="$5"
  manifest_set "$root" "$key" "$value" >> "$log" 2>&1 \
    || fail_phase "$root" "$phase" "$log" "$phase"
}

# ---------------------------------------------------------------------------
# Shared setup
# ---------------------------------------------------------------------------

require_tools() {
  command -v git >/dev/null 2>&1 || die "git not found on PATH"
  command -v jq >/dev/null 2>&1 || die "jq not found on PATH"
}

# Resolve <root> to an absolute path without requiring it to exist yet.
absolute_path() {
  local path="$1"
  case "$path" in
    /*) printf '%s' "$path" ;;
    *)  printf '%s/%s' "$(pwd -P)" "$path" ;;
  esac
}

home_dir()    { printf '%s/home' "$1"; }
project_dir() { printf '%s/project/main' "$1"; }
bin_dir()     { printf '%s/bin' "$1"; }
evidence_dir(){ printf '%s/evidence' "$1"; }
log_dir()     { printf '%s/harness/logs' "$1"; }
workitem_dir(){ printf '%s/harness/workitem' "$1"; }

# ---------------------------------------------------------------------------
# prepare
# ---------------------------------------------------------------------------

# Seed the deterministic fixture repository. The initial commit deliberately
# fails the fixture test; the Work Item asks the Writer to make it pass.
seed_fixture_repo() {
  local project="$1"
  mkdir -p "$project"
  git -C "$project" init -q -b main
  git -C "$project" config user.email "smoke@fluent.local"
  git -C "$project" config user.name "Fluent Smoke"
  git -C "$project" config commit.gpgsign false

  printf 'TODO\n' > "$project/greeting.txt"

  cat > "$project/check.sh" <<'CHECK'
#!/usr/bin/env sh
# Fixture test: the greeting must say hello.
grep -q '^hello$' greeting.txt
CHECK
  chmod +x "$project/check.sh"

  # Fluent stores its Work state under .fluent/work; keep it out of the tree so
  # the target repository stays clean around a land.
  printf '/.fluent/work/\n' > "$project/.gitignore"

  mkdir -p "$project/.fluent"
  cat > "$project/.fluent/tester.yaml" <<'TESTER'
commands:
  - command: ./check.sh
    test_harness: shell-harness
TESTER

  git -C "$project" add -A
  git -C "$project" commit -q -m "Seed failing greeting fixture"
}

# Write the deterministic planning inputs the Work Item carries.
seed_planning_inputs() {
  local dir="$1"
  mkdir -p "$dir"

  cat > "$dir/brief.md" <<'BRIEF'
# Brief

Change the greeting so the fixture test passes.
BRIEF

  cat > "$dir/behaviors.md" <<'BEHAVIORS'
# Behaviors

WHEN the fixture test runs, THE SYSTEM SHALL find "hello" in greeting.txt.
BEHAVIORS

  cat > "$dir/approach.md" <<'APPROACH'
# Approach

Replace the placeholder line in greeting.txt with "hello".
APPROACH

  cat > "$dir/plan.md" <<'PLAN'
# Plan

1. Write "hello" to greeting.txt so ./check.sh exits zero.
PLAN

  cat > "$dir/instructions.md" <<'INSTRUCTIONS'
Set greeting.txt to a single line reading "hello" so ./check.sh passes.
INSTRUCTIONS
}

phase_prepare() {
  local root="" installer="$DEFAULT_INSTALLER" binary=""
  while [ $# -gt 0 ]; do
    case "$1" in
      --installer) installer="${2-}"; shift 2 ;;
      --binary)    binary="${2-}"; shift 2 ;;
      --*)         die "unknown prepare option: $1" ;;
      *)
        [ -z "$root" ] || die "prepare takes a single smoke root"
        root="$1"; shift ;;
    esac
  done
  [ -n "$root" ] || die "prepare requires a smoke root path"
  root="$(absolute_path "$root")"

  # Reject a nonempty root unless it already holds a compatible manifest or an
  # incomplete-prepare marker. A prior prepare that failed mid-seed leaves the
  # marker and no manifest, so the printed `prepare` resume can rebuild the
  # harness-owned partial state instead of tripping the nonempty guard.
  # Check the incomplete marker BEFORE the manifest: an interrupted initial
  # manifest write can leave both the marker and a partial or empty manifest
  # file. The marker takes precedence so the printed prepare resume always works.
  local resuming=0
  if [ -e "$root" ]; then
    if [ -f "$root/harness/.prepare-incomplete" ]; then
      resuming=1
    elif [ -f "$(manifest_path "$root")" ]; then
      local existing
      existing="$(manifest_get "$root" '.schema_version')"
      [ "$existing" = "$SCHEMA_VERSION" ] \
        || die "existing smoke root has incompatible schema $existing"
      verify_manifest_root "$root"
      info "Reusing existing smoke root: $root"
      return 0
    elif [ -n "$(ls -A -- "$root" 2>/dev/null)" ]; then
      die "smoke root $root is not empty and has no harness manifest"
    fi
  fi

  # Resolve a local installer or binary to an absolute path now so the boundary
  # stored in the manifest stays valid when `run` later executes from a
  # different working directory. A URL (never an existing file) is kept verbatim.
  local install_boundary
  if [ -n "$binary" ]; then
    install_boundary="binary:$(absolute_path "$binary")"
  elif [ -f "$installer" ]; then
    install_boundary="installer:$(absolute_path "$installer")"
  else
    install_boundary="installer:$installer"
  fi

  # Create the durable evidence home first so any later failure has somewhere to
  # record itself and a marker that makes the partial root resumable. This
  # minimal step is the irreducible precondition for the resumable contract: if
  # it cannot be created there is nowhere under the root to preserve evidence.
  mkdir -p "$(log_dir "$root")" \
    || die "cannot create harness evidence directory under $root"

  # Mark the prepare incomplete before any fallible setup so a failure — whether
  # in directory creation or seeding — leaves a resumable root: preserved,
  # logged, and accepted by a later prepare.
  local prepare_log
  prepare_log="$(log_dir "$root")/prepare.log"
  printf 'incomplete\n' > "$root/harness/.prepare-incomplete"
  begin_log "$prepare_log" "prepare"

  # The remaining smoke directories are fallible setup; route a permissions or
  # disk failure through the durable phase-failure contract instead of exiting
  # bare under set -e without a phase, log, or resume command.
  { mkdir -p \
      "$(home_dir "$root")" \
      "$(bin_dir "$root")" \
      "$(evidence_dir "$root")" \
      "$(workitem_dir "$root")"; } >> "$prepare_log" 2>&1 \
    || fail_phase "$root" "prepare" "$prepare_log" "prepare"

  # A resume re-seeds from scratch: discard the harness-owned partial repository,
  # planning inputs, and any partial manifest so the rebuild does not inherit
  # a broken half-state. The manifest is removed here so a later atomic write
  # cannot race against a file left by a prior interrupted attempt.
  if [ "$resuming" = "1" ]; then
    info "Resuming an incomplete prepare: $root"
    rm -rf "$(project_dir "$root")" "$(workitem_dir "$root")"
    rm -f "$(manifest_path "$root")"
    mkdir -p "$(workitem_dir "$root")"
  fi

  { seed_fixture_repo "$(project_dir "$root")"; } >> "$prepare_log" 2>&1 \
    || fail_phase "$root" "prepare" "$prepare_log" "prepare"
  { seed_planning_inputs "$(workitem_dir "$root")"; } >> "$prepare_log" 2>&1 \
    || fail_phase "$root" "prepare" "$prepare_log" "prepare"

  # Record the resolved binary path now for a prebuilt override; the installer
  # path selects the same location during run.
  local fluent_bin
  fluent_bin="$(bin_dir "$root")/fluent"

  # Build the manifest with jq so a root, boundary, or binary path containing
  # JSON-significant characters (quotes, backslashes) is encoded, not
  # interpolated. A raw heredoc would emit invalid JSON for such paths.
  # Write through a root-local temp and rename atomically: if jq crashes or
  # the disk is full, the manifest file is never created, the
  # .prepare-incomplete marker stays visible, and the printed prepare resume
  # can rebuild rather than failing to parse a partial or empty manifest.
  local manifest_tmp
  manifest_tmp="$(mktemp "$(manifest_path "$root").XXXXXX")"
  if ! jq -n \
      --argjson schema "$SCHEMA_VERSION" \
      --arg root "$root" \
      --arg boundary "$install_boundary" \
      --arg bin "$fluent_bin" \
      --arg wi "$WORK_ITEM_ID" \
      --arg attempt "$ATTEMPT_ID" \
      '{
        schema_version: $schema,
        smoke_root: $root,
        safe_phase: "prepared",
        run_stage: null,
        install_boundary: $boundary,
        fluent_bin: $bin,
        work_item_id: $wi,
        attempt_id: $attempt,
        merge_candidate_id: null,
        merged_commit: null
      }' > "$manifest_tmp" 2>>"$prepare_log"; then
    rm -f "$manifest_tmp"
    fail_phase "$root" "prepare" "$prepare_log" "prepare"
  fi
  if ! mv "$manifest_tmp" "$(manifest_path "$root")" >> "$prepare_log" 2>&1; then
    rm -f "$manifest_tmp"
    fail_phase "$root" "prepare" "$prepare_log" "prepare"
  fi

  # The manifest is the durable completion marker; drop the incomplete flag.
  rm -f "$root/harness/.prepare-incomplete" >> "$prepare_log" 2>&1 \
    || fail_phase "$root" "prepare" "$prepare_log" "prepare"

  info "Prepared clean-room smoke root: $root"
  info "  isolated home:  $(home_dir "$root")"
  info "  repository:     $(project_dir "$root")"
  info "  evidence:       $(evidence_dir "$root")"
  info "  install source: $install_boundary"
  info ""
  info "Next: $(shq "$SELF") run $(shq "$root")"
}

# ---------------------------------------------------------------------------
# run
# ---------------------------------------------------------------------------

# Globals set by phase_run and read by run_fluent.
RUN_PROJECT=""
RUN_HOME=""
RUN_BIN=""

# Rank the completed run-setup stages so a resumed run skips public commands it
# has already run. init, work-item create, and attempt create are not idempotent
# — replaying them against existing objects would fail — so each records a
# durable checkpoint and resume continues from the first unfinished stage.
run_stage_rank() {
  case "$1" in
    installed) printf '1' ;;
    init)      printf '2' ;;
    work-item) printf '3' ;;
    attempt)   printf '4' ;;
    *)         printf '0' ;;
  esac
}

# Invoke the selected Fluent binary against the fixture repository with the
# isolated home, appending the command and its exit status to a phase log.
# Returns the command's exit code.
run_fluent() {
  local logfile="$1"; shift
  local rc=0
  printf '$ fluent %s\n' "$*" >> "$logfile"
  ( cd "$RUN_PROJECT" && HOME="$RUN_HOME" "$RUN_BIN" "$@" ) >> "$logfile" 2>&1 || rc=$?
  printf 'exit: %s\n' "$rc" >> "$logfile"
  return "$rc"
}

# Capture one Fluent command's stdout while still logging it. Prints stdout on
# success; returns the command's exit code.
capture_fluent() {
  local logfile="$1"; shift
  local out rc=0
  out="$( cd "$RUN_PROJECT" && HOME="$RUN_HOME" "$RUN_BIN" "$@" 2>>"$logfile" )" || rc=$?
  printf '$ fluent %s\n%s\nexit: %s\n' "$*" "$out" "$rc" >> "$logfile"
  [ "$rc" -eq 0 ] && printf '%s' "$out"
  return "$rc"
}

# Install or select Fluent through the configured boundary. Every installer form
# runs with the isolated smoke HOME so a real installer never reads or writes the
# operator's home. Returns non-zero on any failure (logging the reason) so the
# caller can route it through the durable phase-failure contract.
select_fluent() {
  local boundary="$1" bin="$2" logfile="$3"
  local kind="${boundary%%:*}" rest="${boundary#*:}"
  printf '# install boundary: %s\n' "$boundary" >> "$logfile"
  case "$kind" in
    binary)
      if [ ! -f "$rest" ]; then
        printf 'error: prebuilt binary not found: %s\n' "$rest" >> "$logfile"
        return 1
      fi
      cp "$rest" "$bin" && chmod +x "$bin" || return 1
      ;;
    installer)
      if [ -f "$rest" ]; then
        HOME="$RUN_HOME" bash "$rest" \
          --install-path "$(dirname "$bin")" --no-modify-path \
          >> "$logfile" 2>&1 || return 1
      else
        if ! command -v curl >/dev/null 2>&1; then
          printf 'error: curl not found for installer %s\n' "$rest" >> "$logfile"
          return 1
        fi
        # Pipe under one subshell so both curl and sh see the isolated HOME.
        ( HOME="$RUN_HOME" \
          && curl -fsSL "$rest" \
             | HOME="$RUN_HOME" sh -s -- \
                 --install-path "$(dirname "$bin")" --no-modify-path \
        ) >> "$logfile" 2>&1 || return 1
      fi
      ;;
    *)
      printf 'error: unrecognized install boundary: %s\n' "$boundary" >> "$logfile"
      return 1
      ;;
  esac
  if [ ! -x "$bin" ]; then
    printf 'error: install did not produce an executable at %s\n' "$bin" >> "$logfile"
    return 1
  fi
}

phase_run() {
  local root="${1-}"
  [ -n "$root" ] || die "run requires a smoke root path"
  root="$(absolute_path "$root")"
  [ -f "$(manifest_path "$root")" ] || die "no harness manifest under $root"
  [ "$(manifest_get "$root" '.schema_version')" = "$SCHEMA_VERSION" ] \
    || die "smoke root has an incompatible manifest schema"
  verify_manifest_root "$root"

  local safe_phase wi cand
  safe_phase="$(manifest_get "$root" '.safe_phase')"
  wi="$(manifest_get "$root" '.work_item_id')"
  cand="${ATTEMPT_ID}-merge-candidate"

  RUN_PROJECT="$(project_dir "$root")"
  RUN_HOME="$(home_dir "$root")"
  RUN_BIN="$(manifest_get "$root" '.fluent_bin')"

  # Already at a ready candidate: reprint the handoff without repeating work.
  if [ "$safe_phase" = "ran" ]; then
    print_ready_handoff "$root" "$wi" "$cand"
    return 0
  fi
  [ "$safe_phase" = "prepared" ] \
    || die "run expects a prepared smoke root (safe_phase=$safe_phase)"

  # Resume from the first unfinished setup stage. A failed `run` prints its own
  # `run` command; replaying it must not re-run init or re-create the Work Item
  # and Attempt, which Fluent rejects for objects that already exist.
  local stage done_rank
  stage="$(manifest_get "$root" '.run_stage // "none"')"
  done_rank="$(run_stage_rank "$stage")"

  local logs install_log manifest_log
  logs="$(log_dir "$root")"
  manifest_log="$logs/manifest.log"

  if [ "$done_rank" -lt "$(run_stage_rank installed)" ]; then
    install_log="$logs/install.log"
    begin_log "$install_log" "install"
    select_fluent "$(manifest_get "$root" '.install_boundary')" "$RUN_BIN" "$install_log" \
      || fail_phase "$root" "run" "$install_log" "run"
    checkpoint "$root" "run" "$manifest_log" "run_stage" "installed"
  fi

  # init -> Work Item -> Attempt (public first-run commands), each checkpointed.
  if [ "$done_rank" -lt "$(run_stage_rank init)" ]; then
    run_fluent "$logs/init.log" init \
      || fail_phase "$root" "run" "$logs/init.log" "run"
    checkpoint "$root" "run" "$manifest_log" "run_stage" "init"
  fi
  if [ "$done_rank" -lt "$(run_stage_rank work-item)" ]; then
    run_fluent "$logs/work-item.log" work-item create "$wi" \
      --title "Clean-room fixture" \
      --brief-file "$(workitem_dir "$root")/brief.md" \
      --behaviors-file "$(workitem_dir "$root")/behaviors.md" \
      --approach-file "$(workitem_dir "$root")/approach.md" \
      --plan-file "$(workitem_dir "$root")/plan.md" \
      --instructions-file "$(workitem_dir "$root")/instructions.md" \
      || fail_phase "$root" "run" "$logs/work-item.log" "run"
    checkpoint "$root" "run" "$manifest_log" "run_stage" "work-item"
  fi
  if [ "$done_rank" -lt "$(run_stage_rank attempt)" ]; then
    run_fluent "$logs/attempt-create.log" attempt create "$wi" "$ATTEMPT_ID" \
      || fail_phase "$root" "run" "$logs/attempt-create.log" "run"
    checkpoint "$root" "run" "$manifest_log" "run_stage" "attempt"
  fi

  # Advance the Attempt until its Learner has succeeded. A Merge Candidate can
  # exist before the Learner runs, but landing requires a succeeded Learner, so
  # readiness is gated on the stored Attempt learning state — not merely on the
  # candidate record.
  local attempt_log="$logs/attempt-run.log"
  local attempt_show_log="$logs/attempt-show.log"
  begin_log "$attempt_log" "attempt run"
  begin_log "$attempt_show_log" "attempt show"
  local learning="" attempt_status=""
  for _ in 1 2 3 4 5 6 7 8; do
    run_fluent "$attempt_log" attempt run "$wi" "$ATTEMPT_ID" \
      || fail_phase "$root" "run" "$attempt_log" "run"
    capture_fluent "$attempt_show_log" attempt show "$wi" "$ATTEMPT_ID" \
      > "$logs/attempt.json" 2>/dev/null \
      || fail_phase "$root" "run" "$attempt_show_log" "run"
    learning="$(jq -r '.learning.status // "none"' "$logs/attempt.json" 2>>"$attempt_show_log")" \
      || fail_phase "$root" "run" "$attempt_show_log" "run"
    attempt_status="$(jq -r '.status // "none"' "$logs/attempt.json" 2>>"$attempt_show_log")" \
      || fail_phase "$root" "run" "$attempt_show_log" "run"
    case "$learning" in
      succeeded) break ;;
      failed) fail_phase "$root" "run" "$attempt_show_log" "run" ;;
    esac
    case "$attempt_status" in
      needs-user|failed) fail_phase "$root" "run" "$attempt_show_log" "run" ;;
    esac
  done

  # The Learner never reached a succeeded state within the retry budget: a
  # truthful nonterminal state that must not be handed off as ready.
  [ "$learning" = "succeeded" ] \
    || fail_phase "$root" "run" "$attempt_show_log" "run"

  # Confirm the succeeded Attempt exposes a pending Merge Candidate to inspect.
  local show_log="$logs/candidate-show.log"
  capture_fluent "$show_log" merge-candidate show "$wi" "$cand" \
    > "$logs/candidate.json" 2>/dev/null \
    || fail_phase "$root" "run" "$show_log" "run"
  local status
  status="$(jq -r '.merge_state.status' "$logs/candidate.json" 2>>"$show_log")" \
    || fail_phase "$root" "run" "$show_log" "run"
  case "$status" in
    pending)
      : # ready to inspect
      ;;
    *)
      fail_phase "$root" "run" "$show_log" "run"
      ;;
  esac

  checkpoint "$root" "run" "$manifest_log" "merge_candidate_id" "$cand"
  cp "$logs/candidate.json" "$(evidence_dir "$root")/merge-candidate.json" \
    >> "$manifest_log" 2>&1 \
    || fail_phase "$root" "run" "$manifest_log" "run"
  checkpoint "$root" "run" "$manifest_log" "safe_phase" "ran"

  print_ready_handoff "$root" "$wi" "$cand"
}

# Print the exact candidate-inspection command and the explicit land command.
print_ready_handoff() {
  local root="$1" wi="$2" cand="$3"
  info "Reached a ready Merge Candidate. The harness stops before landing."
  info ""
  info "Inspect the candidate:"
  info "  ( cd $(shq "$(project_dir "$root")") && HOME=$(shq "$(home_dir "$root")") \\"
  info "    $(shq "$(manifest_get "$root" '.fluent_bin')") merge-candidate show $(shq "$wi") $(shq "$cand") )"
  info ""
  info "Land it after human acceptance:"
  info "  $(shq "$SELF") land $(shq "$root")"
}

# ---------------------------------------------------------------------------
# land
# ---------------------------------------------------------------------------

phase_land() {
  local root="${1-}"
  [ -n "$root" ] || die "land requires a smoke root path"
  root="$(absolute_path "$root")"
  [ -f "$(manifest_path "$root")" ] || die "no harness manifest under $root"
  [ "$(manifest_get "$root" '.schema_version')" = "$SCHEMA_VERSION" ] \
    || die "smoke root has an incompatible manifest schema"
  verify_manifest_root "$root"

  local safe_phase wi cand
  safe_phase="$(manifest_get "$root" '.safe_phase')"
  wi="$(manifest_get "$root" '.work_item_id')"
  cand="$(manifest_get "$root" '.merge_candidate_id')"

  if [ "$safe_phase" = "landed" ]; then
    info "Already landed: $(manifest_get "$root" '.merged_commit')"
    return 0
  fi
  [ "$safe_phase" = "ran" ] \
    || die "land expects a ready smoke root (safe_phase=$safe_phase); run first"
  if [ "$cand" = "null" ] || [ -z "$cand" ]; then
    die "manifest has no ready Merge Candidate to land"
  fi

  RUN_PROJECT="$(project_dir "$root")"
  RUN_HOME="$(home_dir "$root")"
  RUN_BIN="$(manifest_get "$root" '.fluent_bin')"

  local logs land_log manifest_log
  logs="$(log_dir "$root")"
  land_log="$logs/land.log"
  manifest_log="$logs/manifest.log"
  begin_log "$land_log" "land"

  # A land resume must never replay a successful, non-idempotent land: the
  # candidate is already merged in Fluent, so a second `merge-candidate land`
  # cannot be relied on to succeed. Inspect the durable candidate state and skip
  # the land command when the merge has already happened, then re-run only the
  # idempotent verification the earlier attempt did not finish. A precheck that
  # cannot read the candidate leaves the merge state unknown — it may already be
  # merged — so treat it as a durable land failure rather than assuming the merge
  # still needs to happen and blindly re-landing.
  capture_fluent "$logs/land-precheck.log" merge-candidate show "$wi" "$cand" \
    > "$logs/precandidate.json" 2>/dev/null \
    || fail_phase "$root" "land" "$logs/land-precheck.log" "land"
  local merge_status
  merge_status="$(jq -r '.merge_state.status' "$logs/precandidate.json" 2>>"$logs/land-precheck.log")" \
    || fail_phase "$root" "land" "$logs/land-precheck.log" "land"

  if [ "$merge_status" != "merged" ]; then
    # Land only the accepted candidate through Fluent.
    run_fluent "$land_log" merge-candidate land "$wi" "$cand" \
      || fail_phase "$root" "land" "$land_log" "land"
  fi

  # The fixture's executable test must now pass on the target.
  local check_log="$logs/fixture-check.log"
  if ! ( cd "$RUN_PROJECT" && ./check.sh ) > "$check_log" 2>&1; then
    fail_phase "$root" "land" "$check_log" "land"
  fi

  # The target repository must be clean after the merge.
  if [ -n "$(git -C "$RUN_PROJECT" status --porcelain)" ]; then
    git -C "$RUN_PROJECT" status --porcelain > "$logs/target-dirty.log"
    fail_phase "$root" "land" "$logs/target-dirty.log" "land"
  fi

  # Record the merged commit from Fluent's stored candidate state.
  local merged
  capture_fluent "$logs/landed-show.log" merge-candidate show "$wi" "$cand" \
    > "$logs/landed-candidate.json" \
    || fail_phase "$root" "land" "$logs/landed-show.log" "land"
  merged="$(jq -r '.merge_state.merged_commit' "$logs/landed-candidate.json" 2>>"$logs/landed-show.log")" \
    || fail_phase "$root" "land" "$logs/landed-show.log" "land"
  if [ -z "$merged" ] || [ "$merged" = "null" ]; then
    fail_phase "$root" "land" "$logs/landed-show.log" "land"
  fi

  checkpoint "$root" "land" "$manifest_log" "merged_commit" "$merged"
  cp "$logs/landed-candidate.json" "$(evidence_dir "$root")/merged-candidate.json" \
    >> "$manifest_log" 2>&1 \
    || fail_phase "$root" "land" "$manifest_log" "land"
  checkpoint "$root" "land" "$manifest_log" "safe_phase" "landed"

  info "Landed Merge Candidate $cand"
  info "  merged commit: $merged"
  info "  fixture test:  passed"
  info "  target repo:   clean"
}

# ---------------------------------------------------------------------------
# Dispatch
# ---------------------------------------------------------------------------

usage() {
  cat >&2 <<USAGE
usage: first-run-smoke.sh <phase> <root> [options]

phases:
  prepare <root> [--installer <url|path>] [--binary <path>]
  run     <root>
  land    <root>
USAGE
  exit 2
}

main() {
  require_tools
  [ $# -ge 1 ] || usage
  local phase="$1"; shift
  case "$phase" in
    prepare) phase_prepare "$@" ;;
    run)     phase_run "$@" ;;
    land)    phase_land "$@" ;;
    -h|--help) usage ;;
    *) die "unknown phase: $phase" ;;
  esac
}

main "$@"
