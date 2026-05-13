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
