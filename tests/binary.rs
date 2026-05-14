use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use std::path::Path;
use tempfile::TempDir;

fn factory_cmd() -> Command {
    Command::cargo_bin("factory").unwrap()
}

// -------------------------------------------------------------------------
// Init
// -------------------------------------------------------------------------

#[test]
fn init_creates_factory_structure() {
    let tmp = TempDir::new().unwrap();

    factory_cmd()
        .current_dir(tmp.path())
        .arg("init")
        .assert()
        .success()
        .stderr(predicate::str::contains("Initialized .factory/"));

    assert!(tmp.path().join(".factory/runs").is_dir());
    assert!(tmp.path().join(".factory/expertise").is_dir());
}

#[test]
fn init_is_idempotent() {
    let tmp = TempDir::new().unwrap();

    factory_cmd()
        .current_dir(tmp.path())
        .arg("init")
        .assert()
        .success();

    factory_cmd()
        .current_dir(tmp.path())
        .arg("init")
        .assert()
        .success()
        .stderr(predicate::str::contains("Already initialized"));
}

// -------------------------------------------------------------------------
// Status
// -------------------------------------------------------------------------

#[test]
fn status_no_factory_dir() {
    let tmp = TempDir::new().unwrap();

    factory_cmd()
        .current_dir(tmp.path())
        .arg("status")
        .assert()
        .success()
        .stdout(predicate::str::contains("No runs found"));
}

#[test]
fn status_empty_runs() {
    let tmp = TempDir::new().unwrap();
    fs::create_dir_all(tmp.path().join(".factory/runs")).unwrap();

    factory_cmd()
        .current_dir(tmp.path())
        .arg("status")
        .assert()
        .success()
        .stdout(predicate::str::contains("RUN"))
        .stdout(predicate::str::contains("STATUS"));
}

#[test]
fn status_shows_runs_with_correct_fields() {
    let tmp = TempDir::new().unwrap();
    let run_dir = tmp.path().join(".factory/runs/test-run");
    fs::create_dir_all(&run_dir).unwrap();
    fs::write(run_dir.join("status"), "executing\n").unwrap();
    fs::write(run_dir.join("runtime"), "local\n").unwrap();
    fs::write(run_dir.join("brief.md"), "# Brief\n\nDo the thing\n").unwrap();

    factory_cmd()
        .current_dir(tmp.path())
        .arg("status")
        .assert()
        .success()
        .stdout(predicate::str::contains("test-run"))
        .stdout(predicate::str::contains("executing"))
        .stdout(predicate::str::contains("local"))
        .stdout(predicate::str::contains("Do the thing"));
}

#[test]
fn status_trims_runtime_with_trailing_newline() {
    let tmp = TempDir::new().unwrap();
    let run_dir = tmp.path().join(".factory/runs/trim-test");
    fs::create_dir_all(&run_dir).unwrap();
    fs::write(run_dir.join("status"), "planned\n").unwrap();
    fs::write(run_dir.join("runtime"), "fargate\n").unwrap();
    fs::write(run_dir.join("brief.md"), "Brief\n").unwrap();

    let output = factory_cmd()
        .current_dir(tmp.path())
        .arg("status")
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let data_line = stdout.lines().find(|l| l.contains("trim-test")).unwrap();
    assert!(
        data_line.contains("fargate"),
        "runtime should be on same line: {data_line}"
    );
}

#[test]
fn status_truncates_long_brief() {
    let tmp = TempDir::new().unwrap();
    let run_dir = tmp.path().join(".factory/runs/long-brief");
    fs::create_dir_all(&run_dir).unwrap();
    fs::write(run_dir.join("status"), "planned").unwrap();
    fs::write(
        run_dir.join("brief.md"),
        "This is a very long brief that exceeds fifty characters and should be truncated\n",
    )
    .unwrap();

    factory_cmd()
        .current_dir(tmp.path())
        .arg("status")
        .assert()
        .success()
        .stdout(predicate::str::contains("..."));
}

#[test]
fn status_outputs_to_stdout_not_stderr() {
    let tmp = TempDir::new().unwrap();
    fs::create_dir_all(tmp.path().join(".factory/runs/check-stream")).unwrap();
    fs::write(
        tmp.path().join(".factory/runs/check-stream/status"),
        "planned",
    )
    .unwrap();

    let output = factory_cmd()
        .current_dir(tmp.path())
        .arg("status")
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(stdout.contains("check-stream"), "table should be on stdout");
    assert!(
        !stderr.contains("check-stream"),
        "table should not be on stderr"
    );
}

#[test]
fn status_accepts_path_argument() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("myproject");
    let run_dir = project.join(".factory/runs/path-test");
    fs::create_dir_all(&run_dir).unwrap();
    fs::write(run_dir.join("status"), "complete").unwrap();

    factory_cmd()
        .current_dir(tmp.path())
        .args(["status", project.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("path-test"));
}

// -------------------------------------------------------------------------
// Resume resolution
// -------------------------------------------------------------------------

#[test]
fn resume_no_resumable_run() {
    let tmp = TempDir::new().unwrap();
    let run_dir = tmp.path().join(".factory/runs/done-run");
    fs::create_dir_all(&run_dir).unwrap();
    fs::write(run_dir.join("status"), "complete").unwrap();

    factory_cmd()
        .current_dir(tmp.path())
        .arg("resume")
        .assert()
        .failure()
        .stderr(predicate::str::contains("No run found needing resume"));
}

// -------------------------------------------------------------------------
// Run resolution
// -------------------------------------------------------------------------

#[test]
fn run_no_active_run() {
    let tmp = TempDir::new().unwrap();
    fs::create_dir_all(tmp.path().join(".factory/runs")).unwrap();

    factory_cmd()
        .current_dir(tmp.path())
        .args(["run", "--no-sandbox"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("No active run found"));
}

#[test]
fn run_missing_run_id() {
    let tmp = TempDir::new().unwrap();
    fs::create_dir_all(tmp.path().join(".factory/runs")).unwrap();

    factory_cmd()
        .current_dir(tmp.path())
        .args(["run", "--no-sandbox", "--run-id", "nonexistent"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("Run directory not found"));
}

// -------------------------------------------------------------------------
// Session loop with mock claude
// -------------------------------------------------------------------------

fn setup_git_project(tmp: &TempDir) -> std::path::PathBuf {
    let main_dir = tmp.path().join("main");
    fs::create_dir_all(&main_dir).unwrap();

    std::process::Command::new("git")
        .args(["init", "-b", "main"])
        .current_dir(&main_dir)
        .output()
        .unwrap();
    for args in [
        vec!["config", "commit.gpgsign", "false"],
        vec!["config", "user.email", "test@test"],
        vec!["config", "user.name", "test"],
    ] {
        std::process::Command::new("git")
            .args(&args)
            .current_dir(&main_dir)
            .output()
            .unwrap();
    }
    fs::write(main_dir.join("README.md"), "test").unwrap();
    std::process::Command::new("git")
        .args(["add", "."])
        .current_dir(&main_dir)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["commit", "-m", "init"])
        .current_dir(&main_dir)
        .output()
        .unwrap();

    main_dir
}

fn write_mock_claude(bin_dir: &Path, script: &str) {
    fs::create_dir_all(bin_dir).unwrap();

    let claude_path = bin_dir.join("claude");
    fs::write(&claude_path, script).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&claude_path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    let sandbox_path = bin_dir.join("sandbox-exec");
    fs::write(&sandbox_path, "#!/bin/bash\nexit 1\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&sandbox_path, fs::Permissions::from_mode(0o755)).unwrap();
    }
}

struct WorktreeGuard {
    main_dir: std::path::PathBuf,
    run_id: String,
}

impl Drop for WorktreeGuard {
    fn drop(&mut self) {
        let wt_file = self
            .main_dir
            .join(format!(".factory/runs/{}/worktree", self.run_id));
        if let Ok(wt) = fs::read_to_string(&wt_file) {
            let wt = wt.trim();
            if Path::new(wt).is_dir() {
                std::process::Command::new("git")
                    .args(["-C", &self.main_dir.to_string_lossy()])
                    .args(["worktree", "remove", "--force", wt])
                    .output()
                    .ok();
            }
        }
    }
}

fn worktree_guard(main_dir: &Path, run_id: &str) -> WorktreeGuard {
    WorktreeGuard {
        main_dir: main_dir.to_path_buf(),
        run_id: run_id.to_string(),
    }
}

fn mock_path(bin_dir: &Path) -> String {
    format!("{}:{}", bin_dir.display(), std::env::var("PATH").unwrap())
}

#[test]
fn run_session_loop_stops_on_complete() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);

    let run_id = "20260513-complete-test";
    let run_dir = main_dir.join(format!(".factory/runs/{run_id}"));
    fs::create_dir_all(&run_dir).unwrap();
    fs::write(run_dir.join("status"), "planned").unwrap();
    fs::write(run_dir.join("brief.md"), "# Brief\n\nTest\n").unwrap();

    let bin_dir = tmp.path().join("bin");
    write_mock_claude(
        &bin_dir,
        r##"#!/bin/bash
WORKING_DIR="$(pwd)"
RUN_ID=$(ls "$WORKING_DIR/.factory/runs/" 2>/dev/null | head -1)
echo "complete" > "$WORKING_DIR/.factory/runs/$RUN_ID/status"
exit 0
"##,
    );

    let _guard = worktree_guard(&main_dir, run_id);

    factory_cmd()
        .current_dir(&main_dir)
        .args(["run", "--no-sandbox", "--run-id", run_id])
        .env("PATH", mock_path(&bin_dir))
        .assert()
        .success()
        .stderr(predicate::str::contains("Session 1"))
        .stderr(predicate::str::contains("Run status: complete"));
}

#[test]
fn run_session_loop_stops_on_needs_user() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);

    let run_id = "20260513-needs-user-test";
    let run_dir = main_dir.join(format!(".factory/runs/{run_id}"));
    fs::create_dir_all(&run_dir).unwrap();
    fs::write(run_dir.join("status"), "planned").unwrap();
    fs::write(run_dir.join("brief.md"), "# Brief\n\nTest\n").unwrap();

    let bin_dir = tmp.path().join("bin");
    write_mock_claude(
        &bin_dir,
        r##"#!/bin/bash
WORKING_DIR="$(pwd)"
RUN_ID=$(ls "$WORKING_DIR/.factory/runs/" 2>/dev/null | head -1)
RUN_DIR="$WORKING_DIR/.factory/runs/$RUN_ID"
echo "needs-user" > "$RUN_DIR/status"
printf '## Handoff\nBlocked.\n' > "$RUN_DIR/handoff.md"
exit 0
"##,
    );

    let _guard = worktree_guard(&main_dir, run_id);

    factory_cmd()
        .current_dir(&main_dir)
        .args(["run", "--no-sandbox", "--run-id", run_id])
        .env("PATH", mock_path(&bin_dir))
        .assert()
        .success()
        .stderr(predicate::str::contains("needs your input"))
        .stderr(predicate::str::contains("handoff.md"));
}

#[test]
fn run_session_loop_restarts_on_executing() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);

    let run_id = "20260513-restart-test";
    let run_dir = main_dir.join(format!(".factory/runs/{run_id}"));
    fs::create_dir_all(&run_dir).unwrap();
    fs::write(run_dir.join("status"), "planned").unwrap();
    fs::write(run_dir.join("brief.md"), "# Brief\n\nTest\n").unwrap();

    let bin_dir = tmp.path().join("bin");
    write_mock_claude(
        &bin_dir,
        r##"#!/bin/bash
WORKING_DIR="$(pwd)"
RUN_ID=$(ls "$WORKING_DIR/.factory/runs/" 2>/dev/null | head -1)
RUN_DIR="$WORKING_DIR/.factory/runs/$RUN_ID"
CALL_FILE="$RUN_DIR/call-count"
COUNT=$(cat "$CALL_FILE" 2>/dev/null || echo "0")
COUNT=$((COUNT + 1))
echo "$COUNT" > "$CALL_FILE"
if [ "$COUNT" -le 2 ]; then
  echo "executing" > "$RUN_DIR/status"
  printf '## Handoff\nContinuing.\n' > "$RUN_DIR/handoff.md"
else
  echo "needs-user" > "$RUN_DIR/status"
fi
exit 0
"##,
    );

    let _guard = worktree_guard(&main_dir, run_id);

    factory_cmd()
        .current_dir(&main_dir)
        .args(["run", "--no-sandbox", "--run-id", run_id])
        .env("PATH", mock_path(&bin_dir))
        .assert()
        .success()
        .stderr(predicate::str::contains("Session 1"))
        .stderr(predicate::str::contains("Session 2"))
        .stderr(predicate::str::contains("Session 3"))
        .stderr(predicate::str::contains("Restarting session"));
}

#[test]
fn run_session_loop_consecutive_failures() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);

    let run_id = "20260513-fail-test";
    let run_dir = main_dir.join(format!(".factory/runs/{run_id}"));
    fs::create_dir_all(&run_dir).unwrap();
    fs::write(run_dir.join("status"), "planned").unwrap();
    fs::write(run_dir.join("brief.md"), "# Brief\n\nTest\n").unwrap();

    let bin_dir = tmp.path().join("bin");
    write_mock_claude(
        &bin_dir,
        r##"#!/bin/bash
WORKING_DIR="$(pwd)"
RUN_ID=$(ls "$WORKING_DIR/.factory/runs/" 2>/dev/null | head -1)
echo "executing" > "$WORKING_DIR/.factory/runs/$RUN_ID/status"
exit 1
"##,
    );

    let _guard = worktree_guard(&main_dir, run_id);

    factory_cmd()
        .current_dir(&main_dir)
        .args(["run", "--no-sandbox", "--run-id", run_id])
        .env("PATH", mock_path(&bin_dir))
        .assert()
        .success()
        .stderr(predicate::str::contains("3 consecutive failures"));

    // Verify worktree status is "failed"
    let worktree_path = fs::read_to_string(run_dir.join("worktree")).unwrap();
    let wt_status = fs::read_to_string(
        Path::new(worktree_path.trim())
            .join(format!(".factory/runs/{run_id}/status")),
    )
    .unwrap();
    assert_eq!(wt_status.trim(), "failed");
}

#[test]
fn run_uses_handoff_prompt_when_handoff_exists() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);

    let run_id = "20260513-handoff-prompt";
    let run_dir = main_dir.join(format!(".factory/runs/{run_id}"));
    fs::create_dir_all(&run_dir).unwrap();
    fs::write(run_dir.join("status"), "executing").unwrap();
    fs::write(run_dir.join("brief.md"), "# Brief\n\nTest\n").unwrap();
    fs::write(run_dir.join("handoff.md"), "Previous context\n").unwrap();

    let bin_dir = tmp.path().join("bin");
    write_mock_claude(
        &bin_dir,
        r##"#!/bin/bash
WORKING_DIR="$(pwd)"
PROMPT=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    -p) PROMPT="$2"; shift 2 ;;
    *) shift ;;
  esac
done
RUN_ID=$(ls "$WORKING_DIR/.factory/runs/" 2>/dev/null | head -1)
RUN_DIR="$WORKING_DIR/.factory/runs/$RUN_ID"
echo "$PROMPT" > "$RUN_DIR/captured-prompt"
echo "needs-user" > "$RUN_DIR/status"
exit 0
"##,
    );

    let _guard = worktree_guard(&main_dir, run_id);

    factory_cmd()
        .current_dir(&main_dir)
        .args(["run", "--no-sandbox", "--run-id", run_id])
        .env("PATH", mock_path(&bin_dir))
        .assert()
        .success();

    let worktree_path = fs::read_to_string(run_dir.join("worktree")).unwrap();
    let prompt = fs::read_to_string(
        Path::new(worktree_path.trim())
            .join(format!(".factory/runs/{run_id}/captured-prompt")),
    )
    .unwrap();
    assert!(
        prompt.contains("handoff"),
        "prompt should reference handoff: {prompt}"
    );
}

#[test]
fn run_writes_runtime_and_handle() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);

    let run_id = "20260513-runtime-write";
    let run_dir = main_dir.join(format!(".factory/runs/{run_id}"));
    fs::create_dir_all(&run_dir).unwrap();
    fs::write(run_dir.join("status"), "planned").unwrap();
    fs::write(run_dir.join("brief.md"), "Brief\n").unwrap();

    let bin_dir = tmp.path().join("bin");
    write_mock_claude(
        &bin_dir,
        r##"#!/bin/bash
WORKING_DIR="$(pwd)"
RUN_ID=$(ls "$WORKING_DIR/.factory/runs/" 2>/dev/null | head -1)
echo "needs-user" > "$WORKING_DIR/.factory/runs/$RUN_ID/status"
exit 0
"##,
    );

    let _guard = worktree_guard(&main_dir, run_id);

    factory_cmd()
        .current_dir(&main_dir)
        .args(["run", "--no-sandbox", "--run-id", run_id])
        .env("PATH", mock_path(&bin_dir))
        .assert()
        .success();

    let runtime = fs::read_to_string(run_dir.join("runtime")).unwrap();
    assert_eq!(runtime.trim(), "local");
    assert!(run_dir.join("handle").exists());
}

// -------------------------------------------------------------------------
// Worktree isolation
// -------------------------------------------------------------------------

#[test]
fn worktree_copies_run_state() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);

    let run_id = "20260513-wt-state";
    let run_dir = main_dir.join(format!(".factory/runs/{run_id}"));
    fs::create_dir_all(&run_dir).unwrap();
    fs::write(run_dir.join("status"), "planned").unwrap();
    fs::write(run_dir.join("brief.md"), "# Brief\n\nTest\n").unwrap();
    fs::write(run_dir.join("plan.md"), "## Plan\n1. Step one\n").unwrap();

    let bin_dir = tmp.path().join("bin");
    write_mock_claude(
        &bin_dir,
        r##"#!/bin/bash
WORKING_DIR="$(pwd)"
RUN_ID=$(ls "$WORKING_DIR/.factory/runs/" 2>/dev/null | head -1)
echo "needs-user" > "$WORKING_DIR/.factory/runs/$RUN_ID/status"
exit 0
"##,
    );

    let _guard = worktree_guard(&main_dir, run_id);

    factory_cmd()
        .current_dir(&main_dir)
        .args(["run", "--no-sandbox", "--run-id", run_id])
        .env("PATH", mock_path(&bin_dir))
        .assert()
        .success();

    // Verify worktree was created and state was copied
    let wt_path_str = fs::read_to_string(run_dir.join("worktree")).unwrap();
    let wt_path = Path::new(wt_path_str.trim());
    let wt_run = wt_path.join(format!(".factory/runs/{run_id}"));

    assert!(wt_run.join("brief.md").exists(), "brief.md should be copied");
    assert!(wt_run.join("plan.md").exists(), "plan.md should be copied");
    assert!(wt_run.join("status").exists(), "status should be copied");

    // active-run pointer should exist in worktree
    let active_run = fs::read_to_string(wt_path.join(".factory/active-run")).unwrap();
    assert_eq!(active_run.trim(), run_id);

    // source-branch should be recorded
    assert!(run_dir.join("source-branch").exists(), "source-branch should be written");
}

#[test]
fn worktree_branches_from_current_branch() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);

    // Create and switch to a feature branch
    std::process::Command::new("git")
        .args(["checkout", "-b", "feature-test"])
        .current_dir(&main_dir)
        .output()
        .unwrap();
    fs::write(main_dir.join("feature.txt"), "feature content").unwrap();
    std::process::Command::new("git")
        .args(["add", "."])
        .current_dir(&main_dir)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["commit", "-m", "add feature"])
        .current_dir(&main_dir)
        .output()
        .unwrap();

    let run_id = "20260513-wt-branch";
    let run_dir = main_dir.join(format!(".factory/runs/{run_id}"));
    fs::create_dir_all(&run_dir).unwrap();
    fs::write(run_dir.join("status"), "planned").unwrap();
    fs::write(run_dir.join("brief.md"), "Brief\n").unwrap();

    let bin_dir = tmp.path().join("bin");
    write_mock_claude(
        &bin_dir,
        r##"#!/bin/bash
WORKING_DIR="$(pwd)"
RUN_ID=$(ls "$WORKING_DIR/.factory/runs/" 2>/dev/null | head -1)
echo "needs-user" > "$WORKING_DIR/.factory/runs/$RUN_ID/status"
exit 0
"##,
    );

    let _guard = worktree_guard(&main_dir, run_id);

    factory_cmd()
        .current_dir(&main_dir)
        .args(["run", "--no-sandbox", "--run-id", run_id])
        .env("PATH", mock_path(&bin_dir))
        .assert()
        .success();

    // source-branch should record the feature branch
    let source_branch = fs::read_to_string(run_dir.join("source-branch")).unwrap();
    assert_eq!(source_branch.trim(), "feature-test");

    // Worktree should contain the feature branch file
    let wt_path_str = fs::read_to_string(run_dir.join("worktree")).unwrap();
    let wt_path = Path::new(wt_path_str.trim());
    assert!(
        wt_path.join("feature.txt").exists(),
        "worktree should contain feature branch file"
    );
}

// -------------------------------------------------------------------------
// Run-id resolution via active-run pointer
// -------------------------------------------------------------------------

#[test]
fn run_resolves_via_active_run_pointer() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);

    // Create two runs, point active-run at one
    let run_id = "20260513-active-ptr";
    let run_dir = main_dir.join(format!(".factory/runs/{run_id}"));
    fs::create_dir_all(&run_dir).unwrap();
    fs::write(run_dir.join("status"), "planned").unwrap();
    fs::write(run_dir.join("brief.md"), "Brief\n").unwrap();

    let other_dir = main_dir.join(".factory/runs/20260513-other-run");
    fs::create_dir_all(&other_dir).unwrap();
    fs::write(other_dir.join("status"), "planned").unwrap();
    fs::write(other_dir.join("brief.md"), "Other\n").unwrap();

    fs::write(main_dir.join(".factory/active-run"), run_id).unwrap();

    let bin_dir = tmp.path().join("bin");
    write_mock_claude(
        &bin_dir,
        r##"#!/bin/bash
WORKING_DIR="$(pwd)"
RUN_ID=$(ls "$WORKING_DIR/.factory/runs/" 2>/dev/null | head -1)
echo "needs-user" > "$WORKING_DIR/.factory/runs/$RUN_ID/status"
exit 0
"##,
    );

    let _guard = worktree_guard(&main_dir, run_id);

    factory_cmd()
        .current_dir(&main_dir)
        .args(["run", "--no-sandbox"])
        .env("PATH", mock_path(&bin_dir))
        .assert()
        .success();

    // The active-run pointer should have been used — worktree file for our run should exist
    assert!(
        run_dir.join("worktree").exists(),
        "should resolve via active-run pointer"
    );
}

// -------------------------------------------------------------------------
// Watch
// -------------------------------------------------------------------------

#[test]
fn watch_outputs_status_table() {
    let tmp = TempDir::new().unwrap();
    let run_dir = tmp.path().join(".factory/runs/watch-test");
    fs::create_dir_all(&run_dir).unwrap();
    fs::write(run_dir.join("status"), "executing").unwrap();
    fs::write(run_dir.join("runtime"), "local").unwrap();
    fs::write(run_dir.join("brief.md"), "# Brief\n\nWatch me\n").unwrap();

    // Watch runs an infinite loop, so we use a short timeout and expect it to
    // produce at least one status table before we kill it.
    let output = factory_cmd()
        .current_dir(tmp.path())
        .args(["watch", "1"])
        .timeout(std::time::Duration::from_secs(3))
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("RUN"), "should print header");
    assert!(stdout.contains("watch-test"), "should list the run");
    assert!(stdout.contains("executing"), "should show status");
}

#[test]
fn watch_detects_status_change_and_notifies() {
    let tmp = TempDir::new().unwrap();
    let run_dir = tmp.path().join(".factory/runs/notify-test");
    fs::create_dir_all(&run_dir).unwrap();
    fs::write(run_dir.join("status"), "executing").unwrap();
    fs::write(run_dir.join("brief.md"), "Brief\n").unwrap();

    // Start watch in background, then change status after a moment
    let run_dir_clone = run_dir.clone();
    let handle = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(1500));
        fs::write(run_dir_clone.join("status"), "complete").unwrap();
    });

    let output = factory_cmd()
        .current_dir(tmp.path())
        .args(["watch", "1"])
        .timeout(std::time::Duration::from_secs(5))
        .output()
        .unwrap();

    handle.join().unwrap();

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("[NOTIFY]") || stderr.contains("complete"),
        "should notify on status change: stderr={stderr}"
    );
}

// -------------------------------------------------------------------------
// Resume
// -------------------------------------------------------------------------

#[test]
fn resume_finds_needs_user_run() {
    let tmp = TempDir::new().unwrap();
    let run_dir = tmp.path().join(".factory/runs/resume-target");
    fs::create_dir_all(&run_dir).unwrap();
    fs::write(run_dir.join("status"), "needs-user").unwrap();
    fs::write(run_dir.join("brief.md"), "Brief\n").unwrap();

    // Resume requires sandbox-exec and claude — it will fail on
    // prerequisites, but the error message tells us it resolved the run.
    let output = factory_cmd()
        .current_dir(tmp.path())
        .arg("resume")
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Resuming run resume-target")
            || stderr.contains("resume-target"),
        "should resolve the needs-user run: stderr={stderr}"
    );
}

#[test]
fn resume_finds_failed_run() {
    let tmp = TempDir::new().unwrap();
    let run_dir = tmp.path().join(".factory/runs/failed-run");
    fs::create_dir_all(&run_dir).unwrap();
    fs::write(run_dir.join("status"), "failed").unwrap();
    fs::write(run_dir.join("brief.md"), "Brief\n").unwrap();

    let output = factory_cmd()
        .current_dir(tmp.path())
        .arg("resume")
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Resuming run failed-run"),
        "should resolve the failed run: stderr={stderr}"
    );
}

#[test]
fn resume_with_explicit_run_id() {
    let tmp = TempDir::new().unwrap();
    let run_dir = tmp.path().join(".factory/runs/specific-resume");
    fs::create_dir_all(&run_dir).unwrap();
    fs::write(run_dir.join("status"), "needs-user").unwrap();
    fs::write(run_dir.join("brief.md"), "Brief\n").unwrap();

    let output = factory_cmd()
        .current_dir(tmp.path())
        .args(["resume", "specific-resume"])
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Resuming run specific-resume"),
        "should resume the specified run: stderr={stderr}"
    );
}

#[test]
fn resume_skips_executing_run() {
    let tmp = TempDir::new().unwrap();
    let run_dir = tmp.path().join(".factory/runs/active-run");
    fs::create_dir_all(&run_dir).unwrap();
    fs::write(run_dir.join("status"), "executing").unwrap();

    factory_cmd()
        .current_dir(tmp.path())
        .arg("resume")
        .assert()
        .failure()
        .stderr(predicate::str::contains("No run found needing resume"));
}

// -------------------------------------------------------------------------
// Pull (Fargate)
// -------------------------------------------------------------------------

#[test]
fn pull_fails_without_fargate_config() {
    let tmp = TempDir::new().unwrap();
    fs::create_dir_all(tmp.path().join(".factory/runs/pull-run")).unwrap();
    fs::write(
        tmp.path().join(".factory/runs/pull-run/runtime"),
        "fargate",
    )
    .unwrap();

    factory_cmd()
        .current_dir(tmp.path())
        .arg("pull")
        .env("HOME", tmp.path().to_str().unwrap())
        .env_remove("FACTORY_CLUSTER")
        .env_remove("FACTORY_S3_BUCKET")
        .env_remove("FACTORY_SUBNETS")
        .env_remove("FACTORY_SECURITY_GROUP")
        .assert()
        .failure()
        .stderr(predicate::str::contains("FACTORY_CLUSTER not set"));
}

#[test]
fn pull_no_fargate_run() {
    let tmp = TempDir::new().unwrap();
    fs::create_dir_all(tmp.path().join(".factory/runs/local-run")).unwrap();
    fs::write(
        tmp.path().join(".factory/runs/local-run/runtime"),
        "local",
    )
    .unwrap();

    factory_cmd()
        .current_dir(tmp.path())
        .arg("pull")
        .env("HOME", tmp.path().to_str().unwrap())
        .env_remove("FACTORY_CLUSTER")
        .env_remove("FACTORY_S3_BUCKET")
        .env_remove("FACTORY_SUBNETS")
        .env_remove("FACTORY_SECURITY_GROUP")
        .assert()
        .failure()
        .stderr(predicate::str::contains("No fargate run found"));
}

// -------------------------------------------------------------------------
// Shell (Fargate)
// -------------------------------------------------------------------------

#[test]
fn shell_fails_without_fargate_config() {
    let tmp = TempDir::new().unwrap();
    let run_dir = tmp.path().join(".factory/runs/shell-run");
    fs::create_dir_all(&run_dir).unwrap();
    fs::write(run_dir.join("status"), "executing").unwrap();
    fs::write(run_dir.join("handle"), "arn:aws:ecs:us-west-1:123:task/abc").unwrap();

    factory_cmd()
        .current_dir(tmp.path())
        .args(["shell", "shell-run"])
        .env("HOME", tmp.path().to_str().unwrap())
        .env_remove("FACTORY_CLUSTER")
        .env_remove("FACTORY_S3_BUCKET")
        .env_remove("FACTORY_SUBNETS")
        .env_remove("FACTORY_SECURITY_GROUP")
        .assert()
        .failure()
        .stderr(predicate::str::contains("FACTORY_CLUSTER not set"));
}

#[test]
fn shell_fails_without_handle() {
    let tmp = TempDir::new().unwrap();
    let run_dir = tmp.path().join(".factory/runs/no-handle-run");
    fs::create_dir_all(&run_dir).unwrap();
    fs::write(run_dir.join("status"), "executing").unwrap();
    // No handle file

    factory_cmd()
        .current_dir(tmp.path())
        .args(["shell", "no-handle-run"])
        .env("HOME", tmp.path().to_str().unwrap())
        .env_remove("FACTORY_CLUSTER")
        .env_remove("FACTORY_S3_BUCKET")
        .env_remove("FACTORY_SUBNETS")
        .env_remove("FACTORY_SECURITY_GROUP")
        .assert()
        .failure()
        .stderr(predicate::str::contains("No task handle found"));
}

// -------------------------------------------------------------------------
// Run: Fargate backend
// -------------------------------------------------------------------------

#[test]
fn run_fargate_fails_without_config() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);

    let run_id = "20260513-fargate-noconfig";
    let run_dir = main_dir.join(format!(".factory/runs/{run_id}"));
    fs::create_dir_all(&run_dir).unwrap();
    fs::write(run_dir.join("status"), "planned").unwrap();
    fs::write(run_dir.join("brief.md"), "Brief\n").unwrap();

    factory_cmd()
        .current_dir(&main_dir)
        .args(["run", "--runtime", "fargate", "--run-id", run_id])
        .env("HOME", tmp.path().to_str().unwrap())
        .env_remove("FACTORY_CLUSTER")
        .env_remove("FACTORY_S3_BUCKET")
        .env_remove("FACTORY_SUBNETS")
        .env_remove("FACTORY_SECURITY_GROUP")
        .assert()
        .failure()
        .stderr(predicate::str::contains("FACTORY_CLUSTER not set"));
}

#[test]
fn run_unknown_runtime_fails() {
    let tmp = TempDir::new().unwrap();

    factory_cmd()
        .current_dir(tmp.path())
        .args(["run", "--runtime", "kubernetes"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("Unknown runtime"));
}

// -------------------------------------------------------------------------
// Observability: sessions.log
// -------------------------------------------------------------------------

#[test]
fn run_writes_sessions_log() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);

    let run_id = "20260513-sesslog-test";
    let run_dir = main_dir.join(format!(".factory/runs/{run_id}"));
    fs::create_dir_all(&run_dir).unwrap();
    fs::write(run_dir.join("status"), "planned").unwrap();
    fs::write(run_dir.join("brief.md"), "# Brief\n\nTest sessions.log\n").unwrap();

    let bin_dir = tmp.path().join("bin");
    write_mock_claude(
        &bin_dir,
        r##"#!/bin/bash
WORKING_DIR="$(pwd)"
RUN_ID=$(ls "$WORKING_DIR/.factory/runs/" 2>/dev/null | head -1)
echo "needs-user" > "$WORKING_DIR/.factory/runs/$RUN_ID/status"
exit 0
"##,
    );

    let _guard = worktree_guard(&main_dir, run_id);

    factory_cmd()
        .current_dir(&main_dir)
        .args(["run", "--no-sandbox", "--run-id", run_id])
        .env("PATH", mock_path(&bin_dir))
        .assert()
        .success();

    // sessions.log should exist in the worktree's run dir
    let wt_path_str = fs::read_to_string(run_dir.join("worktree")).unwrap();
    let wt_run_dir = Path::new(wt_path_str.trim())
        .join(format!(".factory/runs/{run_id}"));
    let log_path = wt_run_dir.join("sessions.log");
    assert!(log_path.exists(), "sessions.log should exist");

    let log = fs::read_to_string(&log_path).unwrap();
    let lines: Vec<&str> = log.lines().collect();
    assert_eq!(lines.len(), 1, "should have one session entry");
    assert!(
        lines[0].starts_with("session=1 exit=0 duration="),
        "wrong format: {}",
        lines[0]
    );
    assert!(
        lines[0].contains("status=needs-user"),
        "should record status: {}",
        lines[0]
    );
}

// -------------------------------------------------------------------------
// Observability: transcript.jsonl from stream-json stdout
// -------------------------------------------------------------------------

#[test]
fn run_captures_stream_json_transcript() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);

    let run_id = "20260513-transcript-test";
    let run_dir = main_dir.join(format!(".factory/runs/{run_id}"));
    fs::create_dir_all(&run_dir).unwrap();
    fs::write(run_dir.join("status"), "planned").unwrap();
    fs::write(run_dir.join("brief.md"), "# Brief\n\nTest transcript\n").unwrap();

    // Mock claude that outputs stream-json to stdout
    let bin_dir = tmp.path().join("bin");
    write_mock_claude(
        &bin_dir,
        r##"#!/bin/bash
WORKING_DIR="$(pwd)"
RUN_ID=$(ls "$WORKING_DIR/.factory/runs/" 2>/dev/null | head -1)
# Output stream-json format to stdout (this should be captured as transcript)
echo '{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Working on it"}]}}'
echo '{"type":"result","result":"done","duration_ms":1234,"cost_usd":0.01}'
echo "needs-user" > "$WORKING_DIR/.factory/runs/$RUN_ID/status"
exit 0
"##,
    );

    let _guard = worktree_guard(&main_dir, run_id);

    factory_cmd()
        .current_dir(&main_dir)
        .args(["run", "--no-sandbox", "--run-id", run_id])
        .env("PATH", mock_path(&bin_dir))
        .assert()
        .success();

    // transcript.jsonl should contain stream-json from claude's stdout
    let wt_path_str = fs::read_to_string(run_dir.join("worktree")).unwrap();
    let wt_run_dir = Path::new(wt_path_str.trim())
        .join(format!(".factory/runs/{run_id}"));
    let transcript = wt_run_dir.join("sessions/session-1/transcript.jsonl");
    assert!(transcript.exists(), "transcript.jsonl should exist");

    let content = fs::read_to_string(&transcript).unwrap();
    assert!(
        content.contains(r#""type":"result""#),
        "transcript should contain stream-json result marker, got: {}",
        content
    );
    assert!(
        content.contains(r#""type":"assistant""#),
        "transcript should contain stream-json assistant marker, got: {}",
        content
    );
}

#[test]
fn run_transcript_not_from_history() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);

    let run_id = "20260513-no-history";
    let run_dir = main_dir.join(format!(".factory/runs/{run_id}"));
    fs::create_dir_all(&run_dir).unwrap();
    fs::write(run_dir.join("status"), "planned").unwrap();
    fs::write(run_dir.join("brief.md"), "# Brief\n\nTest\n").unwrap();

    // Create a fake ~/.claude/history.jsonl with a unique marker
    let fake_home = tmp.path().join("fakehome");
    let claude_dir = fake_home.join(".claude");
    fs::create_dir_all(&claude_dir).unwrap();
    fs::write(
        claude_dir.join("history.jsonl"),
        r#"{"MARKER_OLD_HISTORY":"this is the old history format"}"#,
    )
    .unwrap();

    let bin_dir = tmp.path().join("bin");
    write_mock_claude(
        &bin_dir,
        r##"#!/bin/bash
WORKING_DIR="$(pwd)"
RUN_ID=$(ls "$WORKING_DIR/.factory/runs/" 2>/dev/null | head -1)
echo '{"type":"result","stream":"json"}'
echo "needs-user" > "$WORKING_DIR/.factory/runs/$RUN_ID/status"
exit 0
"##,
    );

    let _guard = worktree_guard(&main_dir, run_id);

    factory_cmd()
        .current_dir(&main_dir)
        .args(["run", "--no-sandbox", "--run-id", run_id])
        .env("PATH", mock_path(&bin_dir))
        .env("HOME", fake_home.to_str().unwrap())
        .assert()
        .success();

    let wt_path_str = fs::read_to_string(run_dir.join("worktree")).unwrap();
    let wt_run_dir = Path::new(wt_path_str.trim())
        .join(format!(".factory/runs/{run_id}"));
    let transcript = wt_run_dir.join("sessions/session-1/transcript.jsonl");
    assert!(transcript.exists(), "transcript.jsonl should exist");

    let content = fs::read_to_string(&transcript).unwrap();
    assert!(
        !content.contains("MARKER_OLD_HISTORY"),
        "transcript should NOT contain old history.jsonl content, got: {}",
        content
    );
    assert!(
        content.contains(r#""type":"result""#),
        "transcript should contain stream-json, got: {}",
        content
    );
}

// -------------------------------------------------------------------------
// Observability: no unrelated global state capture
// -------------------------------------------------------------------------

#[test]
fn run_does_not_capture_global_claude_state() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);

    let run_id = "20260513-no-global";
    let run_dir = main_dir.join(format!(".factory/runs/{run_id}"));
    fs::create_dir_all(&run_dir).unwrap();
    fs::write(run_dir.join("status"), "planned").unwrap();
    fs::write(run_dir.join("brief.md"), "# Brief\n\nTest\n").unwrap();

    // Create fake global ~/.claude state
    let fake_home = tmp.path().join("fakehome");
    let claude_dir = fake_home.join(".claude");
    fs::create_dir_all(claude_dir.join("todos")).unwrap();
    fs::write(claude_dir.join("todos/todo.json"), "{}").unwrap();
    fs::create_dir_all(claude_dir.join("plans")).unwrap();
    fs::write(claude_dir.join("plans/plan.json"), "{}").unwrap();
    fs::create_dir_all(claude_dir.join("projects/test/memory")).unwrap();
    fs::write(claude_dir.join("projects/test/memory/MEMORY.md"), "test").unwrap();
    fs::write(
        claude_dir.join("history.jsonl"),
        r#"{"old":"history"}"#,
    )
    .unwrap();

    let bin_dir = tmp.path().join("bin");
    write_mock_claude(
        &bin_dir,
        r##"#!/bin/bash
WORKING_DIR="$(pwd)"
RUN_ID=$(ls "$WORKING_DIR/.factory/runs/" 2>/dev/null | head -1)
echo '{"type":"result"}'
echo "needs-user" > "$WORKING_DIR/.factory/runs/$RUN_ID/status"
exit 0
"##,
    );

    let _guard = worktree_guard(&main_dir, run_id);

    factory_cmd()
        .current_dir(&main_dir)
        .args(["run", "--no-sandbox", "--run-id", run_id])
        .env("PATH", mock_path(&bin_dir))
        .env("HOME", fake_home.to_str().unwrap())
        .assert()
        .success();

    let wt_path_str = fs::read_to_string(run_dir.join("worktree")).unwrap();
    let session_dir = Path::new(wt_path_str.trim())
        .join(format!(".factory/runs/{run_id}/sessions/session-1"));

    // Should NOT have global state dirs
    assert!(
        !session_dir.join("todos").exists(),
        "should not capture global todos"
    );
    assert!(
        !session_dir.join("plans").exists(),
        "should not capture global plans"
    );
    assert!(
        !session_dir.join("memory").exists(),
        "should not capture global memory"
    );
    assert!(
        !session_dir.join("history.jsonl").exists(),
        "should not capture global history.jsonl"
    );
}

// -------------------------------------------------------------------------
// Observability: review round archives
// -------------------------------------------------------------------------

#[test]
fn run_archives_review_rounds() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);

    let run_id = "20260513-review-archive";
    let run_dir = main_dir.join(format!(".factory/runs/{run_id}"));
    fs::create_dir_all(&run_dir).unwrap();
    fs::write(run_dir.join("status"), "planned").unwrap();
    fs::write(run_dir.join("brief.md"), "# Brief\n\nTest review archiving\n").unwrap();

    // Mock claude that distinguishes author vs reviewer by system prompt.
    // The reviewer gets "--append-system-prompt" containing "test reviewer"
    // from the review prompt file's [system] section.
    let bin_dir = tmp.path().join("bin");
    write_mock_claude(
        &bin_dir,
        r##"#!/bin/bash
WORKING_DIR="$(pwd)"
RUN_ID=$(ls "$WORKING_DIR/.factory/runs/" 2>/dev/null | head -1)
RUN_DIR="$WORKING_DIR/.factory/runs/$RUN_ID"

# Detect reviewer vs author by scanning all args for "test reviewer"
IS_REVIEWER=0
for arg in "$@"; do
  case "$arg" in
    *"test reviewer"*) IS_REVIEWER=1 ;;
  esac
done

if [ "$IS_REVIEWER" = 1 ]; then
  # Reviewer call
  REVIEWER_ROUND=$(cat "$RUN_DIR/reviewer-round" 2>/dev/null || echo "0")
  mkdir -p "$RUN_DIR/reviews"
  if [ "$REVIEWER_ROUND" = "0" ]; then
    echo "1" > "$RUN_DIR/reviewer-round"
    printf 'Verdict: fail\n\n1. Missing tests.\n' > "$RUN_DIR/reviews/review-tests.md"
    echo '{"type":"result"}' > "$RUN_DIR/reviews/transcript-tests.jsonl"
  else
    printf 'Verdict: pass\n\nAll good.\n' > "$RUN_DIR/reviews/review-tests.md"
  fi
  echo '{"type":"result"}'
  exit 0
fi

# Author call
echo '{"type":"result"}'
echo "complete" > "$RUN_DIR/status"
exit 0
"##,
    );

    // Only run the "tests" reviewer
    fs::write(run_dir.join("reviewers"), "tests").unwrap();

    // Create the review prompt file
    let prompts_dir = main_dir.join("prompts");
    fs::create_dir_all(&prompts_dir).unwrap();
    fs::write(
        prompts_dir.join("review-tests.md"),
        "[system]\nYou are a test reviewer.\n[run-scoped]\nReview the changes.\n[full-codebase]\nReview everything.\n",
    )
    .unwrap();

    let _guard = worktree_guard(&main_dir, run_id);

    factory_cmd()
        .current_dir(&main_dir)
        .args(["run", "--no-sandbox", "--run-id", run_id])
        .env("PATH", mock_path(&bin_dir))
        .timeout(std::time::Duration::from_secs(30))
        .assert()
        .success();

    // Check that round-1 archive exists
    let wt_path_str = fs::read_to_string(run_dir.join("worktree")).unwrap();
    let wt_run_dir = Path::new(wt_path_str.trim())
        .join(format!(".factory/runs/{run_id}"));
    let round1_dir = wt_run_dir.join("reviews/round-1");
    assert!(
        round1_dir.exists(),
        "reviews/round-1/ archive should exist"
    );
    assert!(
        round1_dir.join("review-tests.md").exists(),
        "round-1 should contain review-tests.md"
    );
}
