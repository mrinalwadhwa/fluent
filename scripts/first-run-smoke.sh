#!/usr/bin/env bash
#
# first-run-smoke.sh — operator harness for Fluent's clean-room first-run gate.
#
# Exercises a candidate Fluent binary inside a fresh repository and isolated
# home: install or select Fluent, adopt a public-shaped skill bootstrap,
# initialize the repository, queue a small deterministic Work Item, run it
# through the scheduler and Learner, inspect the ready Merge Candidate, and land
# it only after an explicit second command.
#
# The harness runs in explicit, resumable phases so an operator can inspect
# evidence at every safety boundary:
#
#   first-run-smoke.sh prepare <root> --coder-profile <profile>
#                              [--installer <url|path>] [--binary <path>]
#                              [--codex-home <path>]
#   first-run-smoke.sh run     <root>
#   first-run-smoke.sh land    <root>
#
# All state — an isolated home, the Git repository, the selected binary, phase
# logs, and evidence — lives beneath <root>. The harness never deletes <root>,
# on success or failure, so every failure keeps the evidence needed to explain
# it.

set -euo pipefail

if [[ "${TRACE-0}" == "1" ]]; then set -o xtrace; fi

readonly SCHEMA_VERSION=2
readonly WORK_ITEM_ID="clean-room-fixture"
readonly ATTEMPT_ID="attempt-1"
readonly DEFAULT_INSTALLER="https://fluent.computer/install"
readonly SCHEDULER_TIMEOUT_SECONDS=7200
readonly SCHEDULER_EXIT_GRACE_SECONDS=5
readonly QUEUE_POLL_SECONDS="${FLUENT_SMOKE_QUEUE_POLL_SECONDS:-1}"

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
# Test identity: first-run-smoke/greeting
grep -q '^hello$' greeting.txt
CHECK
  chmod +x "$project/check.sh"

  # Fluent stores its Work state under .fluent/work; keep it out of the tree so
  # the target repository stays clean around a land.
  printf '/.fluent/work/\n' > "$project/.gitignore"

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
Test: check.sh (first-run-smoke/greeting)
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

# Model the project files produced by the public `npx skills add` command. The
# separate live release matrix runs that networked command through real coding
# agents; this deterministic fixture lets the local harness prove that the
# selected Fluent binary adopts the marked bootstrap and releases only Fluent's
# external lock entry.
seed_skill_bootstrap() {
  local project="$1" skill_dir="$1/.agents/skills/fluent"
  mkdir -p "$skill_dir"
  cat > "$skill_dir/SKILL.md" <<'SKILL'
---
name: fluent
description: Install and continue with Fluent.
fluent-shim: true
---

# Fluent bootstrap

Run `fluent skills add`, then continue from the full managed skill.
SKILL
  cat > "$project/skills-lock.json" <<'LOCK'
{
  "version": 1,
  "metadata": {"fixture": "keep"},
  "skills": {
    "fluent": {
      "source": "mrinalwadhwa/fluent",
      "sourceType": "github",
      "skillPath": "skills/fluent/SKILL.md",
      "computedHash": "clean-room-bootstrap"
    },
    "other": {
      "source": "owner/other",
      "skillPath": "skills/other/SKILL.md"
    }
  }
}
LOCK
}

validate_coder_profile() {
  case "$1" in
    codex-balanced|codex-stronger|claude-balanced|opencode-default|codex-write-claude-review|claude-write-codex-review)
      return 0
      ;;
    *)
      die "unsupported coder profile: $1"
      ;;
  esac
}

phase_prepare() {
  local root="" installer="$DEFAULT_INSTALLER" binary="" codex_home=""
  local coder_profile=""
  while [ $# -gt 0 ]; do
    case "$1" in
      --installer) installer="${2-}"; shift 2 ;;
      --binary)    binary="${2-}"; shift 2 ;;
      --codex-home) codex_home="${2-}"; shift 2 ;;
      --coder-profile) coder_profile="${2-}"; shift 2 ;;
      --*)         die "unknown prepare option: $1" ;;
      *)
        [ -z "$root" ] || die "prepare takes a single smoke root"
        root="$1"; shift ;;
    esac
  done
  [ -n "$root" ] || die "prepare requires a smoke root path"
  [ -n "$coder_profile" ] || die "prepare requires --coder-profile"
  validate_coder_profile "$coder_profile"
  root="$(absolute_path "$root")"

  if [ -n "$codex_home" ]; then
    [ -d "$codex_home" ] || die "Codex authentication home is not a directory: $codex_home"
    codex_home="$(cd "$codex_home" && pwd -P)"
  fi

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
      local existing_profile
      existing_profile="$(manifest_get "$root" '.coder_profile')"
      [ "$existing_profile" = "$coder_profile" ] \
        || die "existing smoke root uses coder profile $existing_profile, not $coder_profile"
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
  { seed_skill_bootstrap "$(project_dir "$root")"; } >> "$prepare_log" 2>&1 \
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
      --arg codex_home "$codex_home" \
      --arg coder_profile "$coder_profile" \
      --arg wi "$WORK_ITEM_ID" \
      --arg attempt "$ATTEMPT_ID" \
      '{
        schema_version: $schema,
        smoke_root: $root,
        safe_phase: "prepared",
        run_stage: null,
        install_boundary: $boundary,
        fluent_bin: $bin,
        codex_home: $codex_home,
        coder_profile: $coder_profile,
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
  info "  coder profile:  $coder_profile"
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
RUN_CODEX_HOME=""
SCHEDULER_TERMINAL_STATUS=""
SCHEDULER_PID=""
SCHEDULER_LOG=""

cleanup_scheduler() {
  local pid="$SCHEDULER_PID" rc=0
  [ -n "$pid" ] || return 0
  if kill -0 "$pid" 2>/dev/null; then
    kill -TERM "$pid" 2>> "${SCHEDULER_LOG:-/dev/null}" || true
  fi
  wait "$pid" 2>> "${SCHEDULER_LOG:-/dev/null}" || rc=$?
  if [ -n "$SCHEDULER_LOG" ]; then
    printf 'scheduler exit: %s\n' "$rc" >> "$SCHEDULER_LOG"
  fi
  SCHEDULER_PID=""
  SCHEDULER_LOG=""
  [ "$rc" -eq 0 ] || [ "$rc" -eq 143 ]
}

handle_scheduler_signal() {
  local signal="$1" status=1
  case "$signal" in
    INT) status=130 ;;
    TERM) status=143 ;;
  esac
  cleanup_scheduler || true
  exit "$status"
}

trap 'handle_scheduler_signal INT' INT
trap 'handle_scheduler_signal TERM' TERM

# Rank the completed run-setup stages so a resumed run skips public commands it
# has already run. init, work-item create, and attempt create are not idempotent
# — replaying them against existing objects would fail — so each records a
# durable checkpoint and resume continues from the first unfinished stage.
run_stage_rank() {
  case "$1" in
    installed) printf '1' ;;
    skills)    printf '2' ;;
    init)      printf '3' ;;
    work-item) printf '4' ;;
    attempt)   printf '5' ;;
    queued)    printf '6' ;;
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
  if [ -n "$RUN_CODEX_HOME" ]; then
    ( cd "$RUN_PROJECT" && HOME="$RUN_HOME" CODEX_HOME="$RUN_CODEX_HOME" "$RUN_BIN" "$@" ) \
      >> "$logfile" 2>&1 || rc=$?
  else
    ( cd "$RUN_PROJECT" && HOME="$RUN_HOME" "$RUN_BIN" "$@" ) >> "$logfile" 2>&1 || rc=$?
  fi
  printf 'exit: %s\n' "$rc" >> "$logfile"
  return "$rc"
}

# Capture one Fluent command's stdout while still logging it. Prints stdout on
# success; returns the command's exit code.
capture_fluent() {
  local logfile="$1"; shift
  local out rc=0
  if [ -n "$RUN_CODEX_HOME" ]; then
    out="$( cd "$RUN_PROJECT" && HOME="$RUN_HOME" CODEX_HOME="$RUN_CODEX_HOME" "$RUN_BIN" "$@" 2>>"$logfile" )" || rc=$?
  else
    out="$( cd "$RUN_PROJECT" && HOME="$RUN_HOME" "$RUN_BIN" "$@" 2>>"$logfile" )" || rc=$?
  fi
  printf '$ fluent %s\n%s\nexit: %s\n' "$*" "$out" "$rc" >> "$logfile"
  [ "$rc" -eq 0 ] && printf '%s' "$out"
  return "$rc"
}

start_scheduler() {
  local logfile="$1"
  printf '$ fluent scheduler run --poll-seconds 1\n' >> "$logfile"
  if [ -n "$RUN_CODEX_HOME" ]; then
    ( cd "$RUN_PROJECT" && exec env HOME="$RUN_HOME" CODEX_HOME="$RUN_CODEX_HOME" \
      "$RUN_BIN" scheduler run --poll-seconds 1 ) >> "$logfile" 2>&1 &
  else
    ( cd "$RUN_PROJECT" && exec env HOME="$RUN_HOME" \
      "$RUN_BIN" scheduler run --poll-seconds 1 ) >> "$logfile" 2>&1 &
  fi
  SCHEDULER_PID="$!"
  SCHEDULER_LOG="$logfile"
}

stop_scheduler() {
  local pid="$1" logfile="$2"
  [ "$pid" = "$SCHEDULER_PID" ] || {
    printf 'error: scheduler pid changed before cleanup\n' >> "$logfile"
    return 1
  }
  cleanup_scheduler
}

read_queue_status() {
  local work_item_id="$1" logfile="$2" output
  output="$(capture_fluent "$logfile" queue show "$work_item_id")" || return 1
  printf '%s\n' "$output" \
    | jq -er --arg id "$work_item_id" \
      'select(.work_item_id == $id) | .status | select(type == "string" and length > 0)'
}

run_scheduler_until_terminal() {
  local work_item_id="$1" scheduler_log="$2" queue_log="$3"
  local pid status="" started now rc=0
  local scheduler_stopped=0 scheduler_stopped_at=0
  started="$(date +%s)"
  start_scheduler "$scheduler_log"
  pid="$SCHEDULER_PID"

  while :; do
    status="$(read_queue_status "$work_item_id" "$queue_log")" || {
      rc=1
      break
    }
    case "$status" in
      candidate-ready|needs-user|failed|blocked|canceled)
        break
        ;;
    esac
    if [ "$scheduler_stopped" -eq 0 ] && ! kill -0 "$pid" 2>/dev/null; then
      # Reap first so the scheduler can no longer publish another update. Queue
      # reads may still briefly expose the prior value, so allow a short,
      # bounded convergence window before classifying the exit as premature.
      stop_scheduler "$pid" "$scheduler_log" || {
        rc=1
        break
      }
      scheduler_stopped=1
      scheduler_stopped_at="$(date +%s)"
    fi
    now="$(date +%s)"
    if [ "$scheduler_stopped" -eq 1 ] \
      && [ "$((now - scheduler_stopped_at))" -ge "$SCHEDULER_EXIT_GRACE_SECONDS" ]; then
      printf 'error: scheduler exited before queue terminal state (%s)\n' "$status" \
        >> "$scheduler_log"
      rc=1
      break
    fi
    if [ "$((now - started))" -ge "$SCHEDULER_TIMEOUT_SECONDS" ]; then
      printf 'error: scheduler timed out with queue status %s\n' "$status" \
        >> "$scheduler_log"
      rc=1
      break
    fi
    sleep "$QUEUE_POLL_SECONDS" || {
      rc=1
      break
    }
  done

  if [ "$scheduler_stopped" -eq 0 ]; then
    stop_scheduler "$pid" "$scheduler_log" || rc=1
  fi
  SCHEDULER_TERMINAL_STATUS="$status"
  return "$rc"
}

install_tester_contract() {
  local project="$1"
  mkdir -p "$project/.fluent"
  cat > "$project/.fluent/tester.yaml" <<'TESTER'
commands:
  - command: ./check.sh
    test_harness: shell-harness
TESTER
  cat > "$project/.fluent/extract-tester-results" <<'EXTRACTOR'
#!/usr/bin/env sh
set -eu

artifact_dir="${1:?usage: extract-tester-results <artifact-dir>}"
jq '
  if type != "array" then
    error("expected command evidence array")
  elif length == 0 then
    []
  elif length == 1
    and .[0].command == "./check.sh"
    and .[0].test_harness == "shell-harness"
    and (.[0].exit_code | type) == "number"
    and (.[0].duration_ms | type) == "number"
  then
    [.[0] | {
      id: "first-run-smoke/greeting",
      test_harness,
      status: (if .exit_code == 0 then "pass" else "fail" end),
      duration_ms,
      failure_excerpt: (
        if .exit_code == 0
        then null
        else "greeting.txt has no line exactly equal to hello"
        end
      )
    }]
  else
    error("expected one ./check.sh shell-harness command")
  end
' "$artifact_dir/commands.json"
EXTRACTOR
  chmod +x "$project/.fluent/extract-tester-results"
}

commit_initialization() {
  local project="$1"
  if [ -n "$(git -C "$project" status --porcelain)" ]; then
    git -C "$project" add -A
    git -C "$project" commit -m "Initialize Fluent smoke fixture"
  fi
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

verify_skill_handoff() {
  local project="$1" logfile="$2"
  local skill_dir="$project/.agents/skills/fluent"
  local lock="$project/skills-lock.json"
  [ -f "$skill_dir/.fluent-managed.json" ] || {
    printf 'error: Fluent did not record ownership at %s\n' "$skill_dir" >> "$logfile"
    return 1
  }
  jq -e '.schema_version == 1 and .agent == "universal" and
    .scope == "project" and .skill == "fluent"' \
    "$skill_dir/.fluent-managed.json" > /dev/null 2>> "$logfile" || return 1
  if grep -Fq 'fluent-shim: true' "$skill_dir/SKILL.md"; then
    printf 'error: Fluent left the bootstrap marker at %s\n' "$skill_dir" >> "$logfile"
    return 1
  fi
  jq -e '.skills.fluent == null' "$lock" > /dev/null 2>> "$logfile" || return 1
  jq -e '.metadata.fixture == "keep" and .skills.other.source == "owner/other"' \
    "$lock" > /dev/null 2>> "$logfile" || return 1
  if [ -e "$project/.claude" ] || [ -e "$project/.codex" ]; then
    printf 'error: bare skill handoff created an unselected agent root\n' >> "$logfile"
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

  local safe_phase wi cand coder_profile
  safe_phase="$(manifest_get "$root" '.safe_phase')"
  wi="$(manifest_get "$root" '.work_item_id')"
  cand="${ATTEMPT_ID}-merge-candidate"
  coder_profile="$(manifest_get "$root" '.coder_profile')"
  validate_coder_profile "$coder_profile"

  RUN_PROJECT="$(project_dir "$root")"
  RUN_HOME="$(home_dir "$root")"
  RUN_BIN="$(manifest_get "$root" '.fluent_bin')"
  RUN_CODEX_HOME="$(manifest_get "$root" '.codex_home // ""')"

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

  if [ "$done_rank" -lt "$(run_stage_rank skills)" ]; then
    begin_log "$logs/skills.log" "skill bootstrap"
    run_fluent "$logs/skills.log" skills add \
      || fail_phase "$root" "run" "$logs/skills.log" "run"
    verify_skill_handoff "$RUN_PROJECT" "$logs/skills.log" \
      || fail_phase "$root" "run" "$logs/skills.log" "run"
    checkpoint "$root" "run" "$manifest_log" "run_stage" "skills"
  fi

  # init -> Work Item -> planned Attempt -> queue (public first-run commands),
  # each checkpointed before the scheduler owns execution.
  if [ "$done_rank" -lt "$(run_stage_rank init)" ]; then
    begin_log "$logs/init.log" "init"
    run_fluent "$logs/init.log" init \
      --coder-profile "$coder_profile" \
      --follow-up-mode propose \
      || fail_phase "$root" "run" "$logs/init.log" "run"
    install_tester_contract "$RUN_PROJECT" >> "$logs/init.log" 2>&1 \
      || fail_phase "$root" "run" "$logs/init.log" "run"
    commit_initialization "$RUN_PROJECT" >> "$logs/init.log" 2>&1 \
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

  if [ "$done_rank" -lt "$(run_stage_rank queued)" ]; then
    run_fluent "$logs/queue-add.log" queue add "$wi" \
      || fail_phase "$root" "run" "$logs/queue-add.log" "run"
    checkpoint "$root" "run" "$manifest_log" "run_stage" "queued"
  fi

  local queue_log="$logs/queue-status.log"
  local queue_status=""
  begin_log "$queue_log" "queue status"
  queue_status="$(read_queue_status "$wi" "$queue_log")" \
    || fail_phase "$root" "run" "$queue_log" "run"
  if [ "$queue_status" = "needs-user" ]; then
    run_fluent "$logs/queue-resume.log" queue resume "$wi" --attempt "$ATTEMPT_ID" \
      || fail_phase "$root" "run" "$logs/queue-resume.log" "run"
    queue_status="queued"
  fi

  case "$queue_status" in
    candidate-ready)
      SCHEDULER_TERMINAL_STATUS="$queue_status"
      ;;
    queued|claimed|running)
      begin_log "$logs/scheduler.log" "scheduler"
      run_scheduler_until_terminal "$wi" "$logs/scheduler.log" "$queue_log" \
        || fail_phase "$root" "run" "$logs/scheduler.log" "run"
      ;;
    failed|blocked|canceled)
      printf 'queue reached terminal status: %s\n' "$queue_status" >> "$queue_log"
      fail_phase "$root" "run" "$queue_log" "run"
      ;;
    *)
      printf 'unexpected queue status: %s\n' "$queue_status" >> "$queue_log"
      fail_phase "$root" "run" "$queue_log" "run"
      ;;
  esac

  case "$SCHEDULER_TERMINAL_STATUS" in
    candidate-ready)
      ;;
    needs-user)
      printf 'Resolve the provider or host condition, then rerun the harness.\n' \
        >> "$queue_log"
      printf 'The next run will queue: fluent queue resume %s --attempt %s\n' \
        "$wi" "$ATTEMPT_ID" >> "$queue_log"
      fail_phase "$root" "run" "$queue_log" "run"
      ;;
    *)
      printf 'queue reached terminal status: %s\n' "$SCHEDULER_TERMINAL_STATUS" \
        >> "$queue_log"
      fail_phase "$root" "run" "$queue_log" "run"
      ;;
  esac

  # Queue readiness is necessary but not sufficient evidence for landing. Read
  # the stored Attempt and require the Learner handoff before showing the
  # candidate.
  local attempt_show_log="$logs/attempt-show.log"
  begin_log "$attempt_show_log" "attempt show"
  capture_fluent "$attempt_show_log" attempt show "$wi" "$ATTEMPT_ID" \
    > "$logs/attempt.json" 2>/dev/null \
    || fail_phase "$root" "run" "$attempt_show_log" "run"
  local learning attempt_status
  learning="$(jq -r '.learning.status // "none"' "$logs/attempt.json" 2>>"$attempt_show_log")" \
    || fail_phase "$root" "run" "$attempt_show_log" "run"
  attempt_status="$(jq -r '.status // "none"' "$logs/attempt.json" 2>>"$attempt_show_log")" \
    || fail_phase "$root" "run" "$attempt_show_log" "run"
  if [ "$learning" != "succeeded" ] || [ "$attempt_status" != "complete" ]; then
    fail_phase "$root" "run" "$attempt_show_log" "run"
  fi

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
  RUN_CODEX_HOME="$(manifest_get "$root" '.codex_home // ""')"

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

  if [ "$merge_status" = "pending" ]; then
    # Land only the accepted candidate through Fluent.
    run_fluent "$land_log" merge-candidate land "$wi" "$cand" \
      || fail_phase "$root" "land" "$land_log" "land"
  elif [ "$merge_status" != "merged" ]; then
    # Any status other than the expected pending or already-merged is unknown;
    # replaying a non-idempotent land against an unknown state is unsafe.
    printf 'unexpected merge candidate status: %s\n' "$merge_status" \
      >> "$logs/land-precheck.log"
    fail_phase "$root" "land" "$logs/land-precheck.log" "land"
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
  prepare <root> --coder-profile <profile>
                 [--installer <url|path>] [--binary <path>]
                 [--codex-home <path>]
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
