#!/usr/bin/env bash
# test-parallel-runs — Verify plan-time parallel child run behaviors.
#
# Tests that a structured plan.md triggers child run creation,
# parallel execution, failure handling, and sequential gating.
#
# Uses the factory binary directly (not library mode) because these
# behaviors are implemented in Rust, not in the shell script.
#
# Uses a fake claude that writes "complete" (or "failed") to the
# active run's status file immediately, with no code changes.
# The empty diff causes the review phase to be skipped automatically.
#
# Child run ID format: {parent-id}-{group-idx}-{step-idx} (1-indexed).
# Discovered empirically: e.g., a plan with Group 1 / step 1 becomes
# run ID "{parent-id}-1-1".
#
# Covers:
#   - Parallel plan creates child runs for each step
#   - Single-step plan uses serial session loop (no child runs created)
#   - Run with no plan uses serial session loop
#   - Child run failure marks parent as failed
#   - Child run failure leaves sibling worktrees intact
#   - Sequential groups: group 2 runs only after group 1 completes
#   - Dashboard shows child runs without crashing
#
# Usage:
#   tests/behaviors/operations/test-parallel-runs.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$(dirname "$(dirname "$SCRIPT_DIR")")")"
FACTORY="${PROJECT_DIR}/target/debug/factory"

PASS=0
FAIL=0
ERRORS=""

# -------------------------------------------------------------------------
# Helpers
# -------------------------------------------------------------------------

setup_fake_claude() {
  local fake_bin
  fake_bin="$(mktemp -d -t factory-fake-claude-XXXXXX)"

  # Fake claude: writes "complete" for all runs except specific failing child IDs.
  # The parallel orchestrator names children {parent-id}-{group-idx}-{step-idx}.
  # Tests that need a failing step use parent IDs "test-fail" and "test-siblings";
  # in both cases the second step of group 1 ({parent}-1-2) is made to fail.
  cat > "${fake_bin}/claude" << 'EOF'
#!/usr/bin/env bash
# Fake claude — ignores all arguments.
# Reads .factory/active-run to find the status file and writes a verdict.
RUN_ID="$(cat .factory/active-run 2>/dev/null || echo "")"
if [ -n "$RUN_ID" ] && [ -d ".factory/runs/${RUN_ID}" ]; then
  case "$RUN_ID" in
    test-fail-1-2|test-siblings-1-2)
      printf 'failed' > ".factory/runs/${RUN_ID}/status"
      ;;
    *)
      printf 'complete' > ".factory/runs/${RUN_ID}/status"
      ;;
  esac
fi
# Emit minimal stream-json so the transcript capture sees EOF cleanly.
printf '{"type":"message_stop"}\n'
exit 0
EOF
  chmod +x "${fake_bin}/claude"

  # Prepend to PATH so the factory binary finds our fake claude first.
  export PATH="${fake_bin}:${PATH}"
}

setup_test_project() {
  TEST_DIR="$(mktemp -d -t factory-test-parallel-XXXXXX)"
  mkdir -p "${TEST_DIR}/main"
  cd "${TEST_DIR}/main"
  git init -b main > /dev/null 2>&1
  git config commit.gpgsign false
  git config user.email "test@test"
  git config user.name "test"
  echo "test" > README.md
  git add . && git commit -m "init" > /dev/null 2>&1
}

cleanup_test_project() {
  cd /
  if [ -d "${TEST_DIR}/main/.git" ]; then
    git -C "${TEST_DIR}/main" worktree list --porcelain 2>/dev/null \
      | grep '^worktree ' | awk '{print $2}' \
      | grep -v "${TEST_DIR}/main" | while read -r wt; do
        git -C "${TEST_DIR}/main" worktree remove --force "$wt" 2>/dev/null || true
      done
  fi
  rm -rf "$TEST_DIR"
}

create_parent_run() {
  local run_id="$1"
  local plan_content="${2:-}"

  mkdir -p ".factory/runs/${run_id}"
  printf '%s' "$run_id" > ".factory/active-run"
  printf 'Test brief for parallel run' > ".factory/runs/${run_id}/brief.md"
  printf 'planned' > ".factory/runs/${run_id}/status"
  if [ -n "$plan_content" ]; then
    printf '%s' "$plan_content" > ".factory/runs/${run_id}/plan.md"
  fi
}

count_child_runs() {
  # Count run directories whose name starts with "$1-"
  local parent="$1"
  find ".factory/runs" -mindepth 1 -maxdepth 1 -type d -name "${parent}-*" 2>/dev/null \
    | wc -l | tr -d ' '
}

worktree_count() {
  git worktree list --porcelain 2>/dev/null | grep -c '^worktree ' || true
}

assert_eq() {
  if [ "$1" != "$2" ]; then
    printf '    FAIL: got "%s", expected "%s"\n' "$1" "$2"
    return 1
  fi
}

assert_ge() {
  if [ "$1" -lt "$2" ]; then
    printf '    FAIL: got %s, expected >= %s\n' "$1" "$2"
    return 1
  fi
}

run_test() {
  local test_name="$1"
  local test_func="$2"
  printf '  %s ... ' "$test_name"
  if ( "$test_func" ) 2>&1; then
    printf 'PASS\n'
    PASS=$((PASS + 1))
  else
    printf '\n'
    FAIL=$((FAIL + 1))
    ERRORS="${ERRORS}\n  - ${test_name}"
  fi
}

# -------------------------------------------------------------------------
# Tests
# -------------------------------------------------------------------------

test_parallel_plan_creates_child_runs() {
  setup_test_project

  local plan='## Group 1 (parallel)

### alpha
Build the alpha component.

### beta
Build the beta component.
'
  create_parent_run "test-prll" "$plan"

  "${FACTORY}" run --no-sandbox > /dev/null 2>&1 || true

  local result=0

  # Two child runs should exist: test-prll-1-1 and test-prll-1-2
  local child_count
  child_count="$(count_child_runs test-prll)"
  if [ "$child_count" -lt 2 ]; then
    printf '    FAIL: expected >=2 child run dirs, got %s\n' "$child_count"
    result=1
  fi

  # Each child run should have a brief.md (written from the plan step body)
  local child_briefs=0
  for dir in .factory/runs/test-prll-*/; do
    [ -f "${dir}brief.md" ] && child_briefs=$((child_briefs + 1))
  done
  if [ "$child_briefs" -lt 2 ]; then
    printf '    FAIL: expected >=2 child brief.md files, got %s\n' "$child_briefs"
    result=1
  fi

  cleanup_test_project
  return $result
}

test_single_step_uses_serial_loop() {
  setup_test_project

  # One group, one step: the factory uses the serial session loop, not the
  # parallel orchestrator. No child run directories are created.
  local plan='## Group 1

### only-step
The single step in this run.
'
  create_parent_run "test-serial" "$plan"

  "${FACTORY}" run --no-sandbox > /dev/null 2>&1 || true

  local result=0

  # No child runs should exist
  local child_count
  child_count="$(count_child_runs test-serial)"
  if [ "$child_count" -ne 0 ]; then
    printf '    FAIL: expected 0 child runs for single-step plan, got %s\n' "$child_count"
    result=1
  fi

  # Sessions directory should exist in the run's worktree (serial loop ran).
  # The worktree path is recorded in the run's "worktree" file.
  local worktree
  worktree="$(cat ".factory/runs/test-serial/worktree" 2>/dev/null || echo "")"
  if [ -z "$worktree" ]; then
    printf '    FAIL: worktree file not written (serial loop did not set up worktree)\n'
    result=1
  elif [ ! -d "${worktree}/.factory/runs/test-serial/sessions" ]; then
    printf '    FAIL: sessions/ not found in worktree at %s\n' "$worktree"
    result=1
  fi

  cleanup_test_project
  return $result
}

test_no_plan_uses_serial_loop() {
  setup_test_project

  # No plan.md: the factory uses the serial session loop, not the orchestrator.
  create_parent_run "test-noplan"

  "${FACTORY}" run --no-sandbox > /dev/null 2>&1 || true

  local result=0

  # No child runs should exist
  local child_count
  child_count="$(count_child_runs test-noplan)"
  if [ "$child_count" -ne 0 ]; then
    printf '    FAIL: expected 0 child runs for run with no plan, got %s\n' "$child_count"
    result=1
  fi

  # Sessions directory in the run's worktree
  local worktree
  worktree="$(cat ".factory/runs/test-noplan/worktree" 2>/dev/null || echo "")"
  if [ -z "$worktree" ]; then
    printf '    FAIL: worktree file not written (serial loop did not set up worktree)\n'
    result=1
  elif [ ! -d "${worktree}/.factory/runs/test-noplan/sessions" ]; then
    printf '    FAIL: sessions/ not found in worktree at %s\n' "$worktree"
    result=1
  fi

  cleanup_test_project
  return $result
}

test_child_failure_marks_parent_failed() {
  setup_test_project

  # Plan with two parallel steps. The fake claude writes "failed" for
  # test-fail-1-2 (second step of group 1) and "complete" for test-fail-1-1.
  local plan='## Group 1 (parallel)

### goodstep
The step that completes normally.

### failstep
The step that fails.
'
  create_parent_run "test-fail" "$plan"

  "${FACTORY}" run --no-sandbox > /dev/null 2>&1 || true

  local result=0

  local parent_status
  parent_status="$(cat ".factory/runs/test-fail/status" 2>/dev/null || echo "missing")"
  if [ "$parent_status" != "failed" ]; then
    printf '    FAIL: parent status is "%s", expected "failed"\n' "$parent_status"
    result=1
  fi

  cleanup_test_project
  return $result
}

test_child_failure_preserves_sibling_worktrees() {
  setup_test_project

  # Same failure setup, different parent ID.
  # The behavior: sibling runs' worktrees must remain intact for inspection.
  local plan='## Group 1 (parallel)

### goodstep
The step that completes normally.

### failstep
The step that fails.
'
  create_parent_run "test-siblings" "$plan"

  "${FACTORY}" run --no-sandbox > /dev/null 2>&1 || true

  local result=0

  # After failure, both sibling git worktrees should still be registered.
  # (main + goodstep-worktree + failstep-worktree = at least 3)
  local wt_count
  wt_count="$(worktree_count)"
  if [ "$wt_count" -lt 3 ]; then
    printf '    FAIL: expected >=3 worktrees (main + 2 siblings), got %s\n' "$wt_count"
    result=1
  fi

  # Both child run directories should exist in .factory/runs/
  local child_count
  child_count="$(count_child_runs test-siblings)"
  if [ "$child_count" -lt 2 ]; then
    printf '    FAIL: expected >=2 child run dirs, got %s\n' "$child_count"
    result=1
  fi

  cleanup_test_project
  return $result
}

test_sequential_groups_run_in_order() {
  setup_test_project

  # Plan with two groups. Group 1 runs in parallel (2 steps), group 2 sequential
  # (1 step). Verify group 1 children AND group 2 child all exist after the run,
  # and that group 1's changes were merged before group 2 ran.
  local plan='## Group 1 (parallel)

### first
First group step.

### second
First group other step.

## Group 2

### third
Second group step — must run after group 1.
'
  create_parent_run "test-seqgroup" "$plan"

  "${FACTORY}" run --no-sandbox > /dev/null 2>&1 || true

  local result=0

  # Group 1 children: test-seqgroup-1-1 and test-seqgroup-1-2
  local g1_count=0
  for dir in .factory/runs/test-seqgroup-1-*/; do
    [ -d "$dir" ] && g1_count=$((g1_count + 1))
  done

  # Group 2 child: test-seqgroup-2-1
  local g2_count=0
  for dir in .factory/runs/test-seqgroup-2-*/; do
    [ -d "$dir" ] && g2_count=$((g2_count + 1))
  done

  if [ "$g1_count" -lt 2 ]; then
    printf '    FAIL: expected 2 group-1 child dirs, got %s\n' "$g1_count"
    result=1
  fi

  if [ "$g2_count" -lt 1 ]; then
    printf '    FAIL: expected 1 group-2 child dir, got %s (group 2 never ran)\n' \
      "$g2_count"
    result=1
  fi

  # Verify ordering: group 1's landed status confirms it merged before group 2 ran.
  # The child status is set to "landed" after merge; if group 1 children are
  # landed but group 2 exists, sequential gating worked.
  local g1_status
  g1_status="$(cat ".factory/runs/test-seqgroup-1-1/status" 2>/dev/null || echo "missing")"
  if [ "$g1_status" != "landed" ]; then
    printf '    FAIL: group-1 child status is "%s", expected "landed" (merge before group 2)\n' "$g1_status"
    result=1
  fi

  cleanup_test_project
  return $result
}

test_child_runs_shown_in_dashboard() {
  setup_test_project

  # Create child run directories manually, simulating an in-progress parallel run.
  # The dashboard discovers runs by scanning .factory/runs/ and should display
  # the children without crashing.
  local parent_id="test-dash-parent"
  mkdir -p ".factory/runs/${parent_id}"
  printf '%s' "$parent_id" > ".factory/active-run"
  printf 'Parent run brief' > ".factory/runs/${parent_id}/brief.md"
  printf 'executing' > ".factory/runs/${parent_id}/status"

  for step in alpha beta; do
    local child_id="${parent_id}-1-${step}"
    mkdir -p ".factory/runs/${child_id}"
    printf 'Child %s brief' "$step" > ".factory/runs/${child_id}/brief.md"
    printf 'executing' > ".factory/runs/${child_id}/status"
  done

  local result=0

  # Run the dashboard briefly. Capture its exit code to detect crashes.
  # The dashboard is a TUI that runs until killed, so we launch it in the
  # background, let it initialize, then kill it. If it crashed before the
  # kill, the wait will capture the crash exit code.
  "${FACTORY}" dashboard --run-id "${parent_id}" > /dev/null 2>&1 &
  local dash_pid=$!

  sleep 1

  # Check if the process is still running (didn't crash during startup)
  if ! kill -0 "$dash_pid" 2>/dev/null; then
    # Process already exited — get its exit code
    wait "$dash_pid"
    local exit_code=$?
    if [ "$exit_code" -ne 0 ]; then
      printf '    FAIL: dashboard crashed on startup (exit %d)\n' "$exit_code"
      result=1
    fi
  else
    # Process is still running — send SIGTERM and check it exits cleanly
    kill "$dash_pid" 2>/dev/null
    wait "$dash_pid" 2>/dev/null
  fi

  cleanup_test_project
  return $result
}

# -------------------------------------------------------------------------
# Run all tests
# -------------------------------------------------------------------------

setup_fake_claude

printf 'test-parallel-runs\n\n'

run_test "parallel plan creates child runs" \
  test_parallel_plan_creates_child_runs

run_test "single-step plan uses serial loop" \
  test_single_step_uses_serial_loop

run_test "no plan uses serial loop" \
  test_no_plan_uses_serial_loop

run_test "child failure marks parent failed" \
  test_child_failure_marks_parent_failed

run_test "child failure preserves sibling worktrees" \
  test_child_failure_preserves_sibling_worktrees

run_test "sequential groups run in order" \
  test_sequential_groups_run_in_order

run_test "child runs shown in dashboard without crash" \
  test_child_runs_shown_in_dashboard

printf '\n  %d passed, %d failed\n' "$PASS" "$FAIL"

if [ "$FAIL" -gt 0 ]; then
  printf '\n  Failures:%b\n' "$ERRORS"
  exit 1
fi
