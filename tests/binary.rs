use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use std::path::Path;
use std::process::Command as StdCommand;
use tempfile::TempDir;

fn factory_cmd() -> Command {
    Command::cargo_bin("factory").unwrap()
}

#[test]
fn version_prints_package_version_and_commit() {
    let tmp = TempDir::new().unwrap();

    let output = factory_cmd()
        .current_dir(tmp.path())
        .arg("version")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "version failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stderr, b"");

    let stdout = String::from_utf8(output.stdout).unwrap();
    let fields = stdout.trim_end().split(' ').collect::<Vec<_>>();
    assert_eq!(fields.len(), 3, "version output should have three fields");
    assert_eq!(fields[0], "factory");
    assert_eq!(fields[1], env!("CARGO_PKG_VERSION"));
    match expected_build_commit() {
        Some(commit) => assert_eq!(fields[2], commit, "commit should match the build HEAD"),
        None => assert_eq!(fields[2], "unknown", "commit should use the fallback"),
    }
}

fn expected_build_commit() -> Option<String> {
    StdCommand::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|stdout| stdout.trim().to_string())
        .filter(|commit| !commit.is_empty())
}

#[test]
fn dry_run_with_codex_uses_codex_profile_layer() {
    let tmp = TempDir::new().unwrap();
    let bin_dir = tmp.path().join("bin");
    write_mock_codex(&bin_dir, "#!/bin/bash\nexit 0\n");
    write_mock_sandbox_exec(&bin_dir);

    let output = factory_cmd()
        .current_dir(tmp.path())
        .args(["--dry-run", "--coder", "codex"])
        .env("PATH", mock_path(&bin_dir))
        .env("SANDBOX_EXEC_LOG", tmp.path().join("sandbox-exec.log"))
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "dry-run failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Codex CLI -- profile-specific Seatbelt rules"),
        "dry-run should include Codex profile layer: {stdout}"
    );
    assert!(
        !stdout.contains("Claude Code CLI -- profile-specific Seatbelt rules"),
        "dry-run should not include Claude profile layer for Codex: {stdout}"
    );
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
// Summary
// -------------------------------------------------------------------------

#[test]
fn summary_uses_explicit_run_id() {
    let tmp = TempDir::new().unwrap();
    let run_dir = tmp.path().join(".factory/runs/selected-run");
    fs::create_dir_all(&run_dir).unwrap();
    fs::write(run_dir.join("status"), "planned").unwrap();
    fs::write(run_dir.join("runtime"), "local").unwrap();
    fs::write(run_dir.join("brief.md"), "# Brief\n\nSummarize this run\n").unwrap();

    factory_cmd()
        .current_dir(tmp.path())
        .args(["summary", "--run-id", "selected-run"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Run\n"))
        .stdout(predicate::str::contains("ID: selected-run"))
        .stdout(predicate::str::contains("Status: planned"))
        .stdout(predicate::str::contains("Summarize this run"))
        .stdout(predicate::str::contains("start or resume the run"));
}

#[test]
fn summary_resolves_active_run() {
    let tmp = TempDir::new().unwrap();
    let run_dir = tmp.path().join(".factory/runs/active-summary");
    fs::create_dir_all(&run_dir).unwrap();
    fs::write(tmp.path().join(".factory/active-run"), "active-summary").unwrap();
    fs::write(run_dir.join("status"), "executing").unwrap();
    fs::write(run_dir.join("brief.md"), "Active run brief\n").unwrap();

    factory_cmd()
        .current_dir(tmp.path())
        .arg("summary")
        .assert()
        .success()
        .stdout(predicate::str::contains("ID: active-summary"))
        .stdout(predicate::str::contains("Status: executing"))
        .stdout(predicate::str::contains("author work is still in progress"));
}

#[test]
fn summary_includes_sessions_reviews_handoff_and_report() {
    let tmp = TempDir::new().unwrap();
    let run_dir = tmp.path().join(".factory/runs/artifact-summary");
    fs::create_dir_all(run_dir.join("reviews")).unwrap();
    fs::write(run_dir.join("status"), "needs-user").unwrap();
    fs::write(run_dir.join("brief.md"), "# Brief\n\nArtifact summary\n").unwrap();
    fs::write(
        run_dir.join("sessions.log"),
        "session=1 exit=0 duration=5s status=executing\nreview=1 duration=2s verdict=fail\n",
    )
    .unwrap();
    fs::write(run_dir.join("reviews/review-tests.md"), "Verdict: fail").unwrap();
    fs::write(
        run_dir.join("reviews/review-architecture.md"),
        "Verdict: pass",
    )
    .unwrap();
    fs::write(
        run_dir.join("handoff.md"),
        "## Open questions\n- Should Factory retry after the failed review?\n",
    )
    .unwrap();
    fs::write(
        run_dir.join("report.md"),
        "# Run Report\n\nREPORT_BODY_SENTINEL should not print\n",
    )
    .unwrap();

    let output = factory_cmd()
        .current_dir(tmp.path())
        .args(["summary", "--run-id", "artifact-summary"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "summary failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("session=1 exit=0 duration=5s status=executing"),
        "summary should include session entries: {stdout}"
    );
    assert!(
        stdout.contains("review=1 duration=2s verdict=fail"),
        "summary should include review session entries: {stdout}"
    );
    assert!(
        stdout.contains("architecture: pass"),
        "summary should include architecture verdict: {stdout}"
    );
    assert!(
        stdout.contains("tests: fail"),
        "summary should include test verdict: {stdout}"
    );
    assert!(
        stdout.contains("Should Factory retry after the failed review?"),
        "summary should include the open question: {stdout}"
    );
    assert!(
        stdout.contains("Available: report.md"),
        "summary should show report presence: {stdout}"
    );
    assert!(
        !stdout.contains("REPORT_BODY_SENTINEL"),
        "summary should not dump report contents: {stdout}"
    );
}

#[test]
fn summary_prefers_live_worktree_artifacts() {
    let tmp = TempDir::new().unwrap();
    let source_run = tmp.path().join(".factory/runs/worktree-summary");
    let worktree_root = tmp.path().join("worktree");
    let live_run = worktree_root.join(".factory/runs/worktree-summary");
    fs::create_dir_all(source_run.join("reviews")).unwrap();
    fs::create_dir_all(live_run.join("reviews")).unwrap();
    fs::write(source_run.join("status"), "planned").unwrap();
    fs::write(source_run.join("brief.md"), "# Brief\n\nWorktree summary\n").unwrap();
    fs::write(source_run.join("worktree"), worktree_root.to_str().unwrap()).unwrap();
    fs::write(
        source_run.join("sessions.log"),
        "source session should not print\n",
    )
    .unwrap();
    fs::write(source_run.join("reviews/review-tests.md"), "Verdict: fail").unwrap();
    fs::write(
        source_run.join("handoff.md"),
        "source handoff should not print\n",
    )
    .unwrap();

    fs::write(live_run.join("status"), "complete").unwrap();
    fs::write(live_run.join("sessions.log"), "live session should print\n").unwrap();
    fs::write(live_run.join("reviews/review-tests.md"), "Verdict: pass").unwrap();
    fs::write(live_run.join("handoff.md"), "live handoff should print\n").unwrap();
    fs::write(live_run.join("report.md"), "# Live report\n").unwrap();

    let output = factory_cmd()
        .current_dir(tmp.path())
        .args(["summary", "--run-id", "worktree-summary"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "summary failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Status: complete"), "{stdout}");
    assert!(stdout.contains("Artifacts:"), "{stdout}");
    assert!(stdout.contains("live session should print"), "{stdout}");
    assert!(stdout.contains("tests: pass"), "{stdout}");
    assert!(stdout.contains("live handoff should print"), "{stdout}");
    assert!(stdout.contains("Available: report.md"), "{stdout}");
    assert!(
        !stdout.contains("source session should not print"),
        "{stdout}"
    );
    assert!(!stdout.contains("tests: fail"), "{stdout}");
    assert!(
        !stdout.contains("source handoff should not print"),
        "{stdout}"
    );
}

#[test]
fn summary_limits_sessions_to_latest_entries() {
    let tmp = TempDir::new().unwrap();
    let run_dir = tmp.path().join(".factory/runs/session-limit");
    fs::create_dir_all(&run_dir).unwrap();
    fs::write(run_dir.join("status"), "executing").unwrap();
    fs::write(run_dir.join("brief.md"), "Session limit\n").unwrap();
    fs::write(
        run_dir.join("sessions.log"),
        [
            "session=1 old entry",
            "session=2 retained entry",
            "session=3 retained entry",
            "session=4 retained entry",
            "session=5 retained entry",
            "session=6 newest entry",
        ]
        .join("\n"),
    )
    .unwrap();

    let output = factory_cmd()
        .current_dir(tmp.path())
        .args(["summary", "--run-id", "session-limit"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "summary failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!stdout.contains("session=1 old entry"), "{stdout}");
    assert!(stdout.contains("session=2 retained entry"), "{stdout}");
    assert!(stdout.contains("session=6 newest entry"), "{stdout}");
}

#[test]
fn summary_uses_handoff_fallback_without_open_questions() {
    let tmp = TempDir::new().unwrap();
    let run_dir = tmp.path().join(".factory/runs/handoff-fallback");
    fs::create_dir_all(&run_dir).unwrap();
    fs::write(run_dir.join("status"), "needs-user").unwrap();
    fs::write(run_dir.join("brief.md"), "Fallback handoff\n").unwrap();
    fs::write(
        run_dir.join("handoff.md"),
        "# Handoff\n\nBrief: Ignore boilerplate\nStatus: needs-user\n- Retry after updating credentials\n",
    )
    .unwrap();

    let output = factory_cmd()
        .current_dir(tmp.path())
        .args(["summary", "--run-id", "handoff-fallback"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "summary failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Handoff\n  Retry after updating credentials"),
        "summary should use the first actionable fallback line: {stdout}"
    );
    assert!(
        !stdout.contains("Brief: Ignore boilerplate"),
        "summary should skip handoff boilerplate: {stdout}"
    );
}

#[test]
fn summary_fails_without_resolved_run() {
    let tmp = TempDir::new().unwrap();
    fs::create_dir_all(tmp.path().join(".factory/runs")).unwrap();

    factory_cmd()
        .current_dir(tmp.path())
        .arg("summary")
        .assert()
        .failure()
        .stderr(predicate::str::contains("No active run found"));
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

fn write_mock_codex(bin_dir: &Path, script: &str) {
    fs::create_dir_all(bin_dir).unwrap();

    let codex_path = bin_dir.join("codex");
    fs::write(&codex_path, script).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&codex_path, fs::Permissions::from_mode(0o755)).unwrap();
    }
}

const CODEX_SSL_CERT_FILE_RECORDER: &str = r##"#!/bin/bash
WORKING_DIR="$(pwd)"
RUN_ID=$(ls "$WORKING_DIR/.factory/runs/" 2>/dev/null | head -1)
RUN_DIR="$WORKING_DIR/.factory/runs/$RUN_ID"
printf '%s\n' "$@" > "$RUN_DIR/codex-args"
printf '%s\n' "${SSL_CERT_FILE:-}" > "$RUN_DIR/codex-ssl-cert-file"
echo '{"type":"assistant","message":"codex sandboxed"}'
echo "needs-user" > "$RUN_DIR/status"
exit 0
"##;

fn write_mock_sandbox_exec(bin_dir: &Path) {
    fs::create_dir_all(bin_dir).unwrap();

    let sandbox_path = bin_dir.join("sandbox-exec");
    fs::write(
        &sandbox_path,
        "#!/bin/bash\nprintf 'used' > \"${SANDBOX_EXEC_LOG:?}\"\nif [ \"$1\" = \"-f\" ]; then shift 2; fi\nexec \"$@\"\n",
    )
    .unwrap();
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
        Path::new(worktree_path.trim()).join(format!(".factory/runs/{run_id}/status")),
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
        Path::new(worktree_path.trim()).join(format!(".factory/runs/{run_id}/captured-prompt")),
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

    assert!(
        wt_run.join("brief.md").exists(),
        "brief.md should be copied"
    );
    assert!(wt_run.join("plan.md").exists(), "plan.md should be copied");
    assert!(wt_run.join("status").exists(), "status should be copied");

    // active-run pointer should exist in worktree
    let active_run = fs::read_to_string(wt_path.join(".factory/active-run")).unwrap();
    assert_eq!(active_run.trim(), run_id);

    // source-branch should be recorded
    assert!(
        run_dir.join("source-branch").exists(),
        "source-branch should be written"
    );
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

    factory_cmd()
        .current_dir(tmp.path())
        .args(["watch", "1", "--timeout", "2"])
        .timeout(std::time::Duration::from_secs(10))
        .assert()
        .success()
        .stdout(predicate::str::contains("RUN"))
        .stdout(predicate::str::contains("watch-test"))
        .stdout(predicate::str::contains("executing"));
}

#[test]
fn watch_detects_status_change_and_notifies() {
    let tmp = TempDir::new().unwrap();
    let run_dir = tmp.path().join(".factory/runs/notify-test");
    fs::create_dir_all(&run_dir).unwrap();
    fs::write(run_dir.join("status"), "executing").unwrap();
    fs::write(run_dir.join("brief.md"), "Brief\n").unwrap();

    let bin = assert_cmd::cargo::cargo_bin("factory");
    let child = std::process::Command::new(&bin)
        .current_dir(tmp.path())
        .args(["watch", "1", "--timeout", "5"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();

    // Change status after watch has polled at least once
    std::thread::sleep(std::time::Duration::from_millis(1500));
    fs::write(run_dir.join("status"), "complete").unwrap();

    let output = child.wait_with_output().unwrap();

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("[NOTIFY]") || stderr.contains("complete"),
        "should notify on status change: stderr={stderr}"
    );
}

#[test]
fn watch_exits_on_timeout() {
    let tmp = TempDir::new().unwrap();
    fs::create_dir_all(tmp.path().join(".factory/runs")).unwrap();

    let start = std::time::Instant::now();
    factory_cmd()
        .current_dir(tmp.path())
        .args(["watch", "1", "--timeout", "2"])
        .timeout(std::time::Duration::from_secs(10))
        .assert()
        .success()
        .stderr(predicate::str::contains("Timeout reached"));
    let elapsed = start.elapsed().as_secs();
    assert!(
        elapsed < 5,
        "should exit promptly after timeout, took {elapsed}s"
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
        stderr.contains("Resuming run resume-target") || stderr.contains("resume-target"),
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
    fs::write(tmp.path().join(".factory/runs/pull-run/runtime"), "fargate").unwrap();

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
    fs::write(tmp.path().join(".factory/runs/local-run/runtime"), "local").unwrap();

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
fn run_fargate_with_codex_fails_before_config() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);

    let run_id = "20260605-fargate-codex";
    let run_dir = main_dir.join(format!(".factory/runs/{run_id}"));
    fs::create_dir_all(&run_dir).unwrap();
    fs::write(run_dir.join("status"), "planned").unwrap();
    fs::write(run_dir.join("brief.md"), "Brief\n").unwrap();

    factory_cmd()
        .current_dir(&main_dir)
        .args([
            "run",
            "--runtime",
            "fargate",
            "--coder",
            "codex",
            "--run-id",
            run_id,
        ])
        .env("HOME", tmp.path().to_str().unwrap())
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "Fargate runtime currently supports only the claude coder",
        ));
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
    let wt_run_dir = Path::new(wt_path_str.trim()).join(format!(".factory/runs/{run_id}"));
    let log_path = wt_run_dir.join("sessions.log");
    assert!(log_path.exists(), "sessions.log should exist");

    let log = fs::read_to_string(&log_path).unwrap();
    let lines: Vec<&str> = log.lines().collect();
    assert_eq!(lines.len(), 1, "should have one session entry");
    assert!(
        lines[0].contains("session=1 exit=0 duration="),
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
    let wt_run_dir = Path::new(wt_path_str.trim()).join(format!(".factory/runs/{run_id}"));
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
fn run_with_codex_uses_exec_json_and_status_contract() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);

    let run_id = "20260605-codex-run";
    let run_dir = main_dir.join(format!(".factory/runs/{run_id}"));
    fs::create_dir_all(&run_dir).unwrap();
    fs::write(run_dir.join("status"), "planned").unwrap();
    fs::write(run_dir.join("brief.md"), "# Brief\n\nRun with Codex\n").unwrap();

    let bin_dir = tmp.path().join("bin");
    write_mock_codex(
        &bin_dir,
        r##"#!/bin/bash
WORKING_DIR="$(pwd)"
RUN_ID=$(ls "$WORKING_DIR/.factory/runs/" 2>/dev/null | head -1)
RUN_DIR="$WORKING_DIR/.factory/runs/$RUN_ID"
printf '%s\n' "$@" > "$RUN_DIR/codex-args"
printf '%s\n' "${@: -1}" > "$RUN_DIR/codex-prompt"
echo '{"type":"assistant","message":"codex running"}'
echo "needs-user" > "$RUN_DIR/status"
exit 0
"##,
    );
    let _guard = worktree_guard(&main_dir, run_id);

    factory_cmd()
        .current_dir(&main_dir)
        .args([
            "run",
            "--no-sandbox",
            "--coder",
            "codex",
            "--run-id",
            run_id,
        ])
        .env("PATH", mock_path(&bin_dir))
        .assert()
        .success();

    let wt_path_str = fs::read_to_string(run_dir.join("worktree")).unwrap();
    let wt_run_dir = Path::new(wt_path_str.trim()).join(format!(".factory/runs/{run_id}"));
    let coder = fs::read_to_string(run_dir.join("coder")).unwrap();
    assert_eq!(coder.trim(), "codex");

    let args = fs::read_to_string(wt_run_dir.join("codex-args")).unwrap();
    assert!(
        args.lines().any(|line| line == "exec"),
        "expected codex exec: {args}"
    );
    assert!(
        args.lines().any(|line| line == "--json"),
        "expected --json: {args}"
    );
    assert!(
        args.lines()
            .any(|line| line == "--dangerously-bypass-approvals-and-sandbox"),
        "expected non-interactive bypass flag: {args}"
    );

    let prompt = fs::read_to_string(wt_run_dir.join("codex-prompt")).unwrap();
    assert!(
        prompt.contains("Status file contract"),
        "prompt should include factory system prompt: {prompt}"
    );
    assert!(
        prompt.contains("brief"),
        "prompt should include run prompt: {prompt}"
    );

    let transcript =
        fs::read_to_string(wt_run_dir.join("sessions/session-1/transcript.jsonl")).unwrap();
    assert!(
        transcript.contains("codex running"),
        "transcript should capture Codex JSON output: {transcript}"
    );
}

#[test]
fn run_with_codex_uses_factory_seatbelt() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);

    let run_id = "20260605-codex-sandbox-run";
    let run_dir = main_dir.join(format!(".factory/runs/{run_id}"));
    fs::create_dir_all(&run_dir).unwrap();
    fs::write(run_dir.join("status"), "planned").unwrap();
    fs::write(run_dir.join("brief.md"), "# Brief\n\nRun Codex sandboxed\n").unwrap();

    let bin_dir = tmp.path().join("bin");
    write_mock_codex(
        &bin_dir,
        r##"#!/bin/bash
WORKING_DIR="$(pwd)"
RUN_ID=$(ls "$WORKING_DIR/.factory/runs/" 2>/dev/null | head -1)
RUN_DIR="$WORKING_DIR/.factory/runs/$RUN_ID"
printf '%s\n' "$@" > "$RUN_DIR/codex-args"
printf '%s\n' "${SSL_CERT_FILE:-}" > "$RUN_DIR/codex-ssl-cert-file"
echo "codex sandbox commit" > codex-sandbox-commit.txt
git add codex-sandbox-commit.txt > "$RUN_DIR/codex-commit-output" 2>&1
git commit -m "Codex sandbox commit" >> "$RUN_DIR/codex-commit-output" 2>&1
echo '{"type":"assistant","message":"codex sandboxed"}'
echo "needs-user" > "$RUN_DIR/status"
exit 0
"##,
    );
    write_mock_sandbox_exec(&bin_dir);
    let sandbox_exec_log = tmp.path().join("sandbox-exec.log");
    let ca_bundle = tmp.path().join("ca-bundle.pem");
    fs::write(&ca_bundle, "test ca bundle").unwrap();

    let _guard = worktree_guard(&main_dir, run_id);

    factory_cmd()
        .current_dir(&main_dir)
        .args(["run", "--coder", "codex", "--run-id", run_id])
        .env("PATH", mock_path(&bin_dir))
        .env("SANDBOX_EXEC_LOG", &sandbox_exec_log)
        .env("FACTORY_CODEX_CA_BUNDLE", &ca_bundle)
        .env_remove("SSL_CERT_FILE")
        .assert()
        .success();

    let wt_path_str = fs::read_to_string(run_dir.join("worktree")).unwrap();
    let wt_run_dir = Path::new(wt_path_str.trim()).join(format!(".factory/runs/{run_id}"));
    let args = fs::read_to_string(wt_run_dir.join("codex-args")).unwrap();
    let ssl_cert_file = fs::read_to_string(wt_run_dir.join("codex-ssl-cert-file")).unwrap();
    assert_eq!(ssl_cert_file.trim(), ca_bundle.to_string_lossy());

    assert!(
        args.lines().any(|line| line == "exec"),
        "expected codex exec: {args}"
    );
    assert!(
        args.lines().any(|line| line == "--json"),
        "expected --json: {args}"
    );
    assert!(
        sandbox_exec_log.exists(),
        "sandboxed Codex should be launched through sandbox-exec"
    );
    assert!(
        args.lines()
            .any(|line| line == "--dangerously-bypass-approvals-and-sandbox"),
        "expected Codex to bypass its own sandbox under Factory Seatbelt: {args}"
    );
    assert!(
        !args.lines().any(|line| line == "--sandbox")
            && !args.lines().any(|line| line == "workspace-write")
            && !args.lines().any(|line| line == "--add-dir"),
        "sandboxed Codex should not use Codex workspace-write sandbox: {args}"
    );
    assert!(
        args.lines().any(|line| line == "--ask-for-approval")
            && args.lines().any(|line| line == "never"),
        "expected Codex approval policy never: {args}"
    );

    // --ask-for-approval is a top-level option and must precede the exec subcommand
    let approval_pos = args
        .lines()
        .position(|line| line == "--ask-for-approval")
        .expect("--ask-for-approval not found");
    let exec_pos = args
        .lines()
        .position(|line| line == "exec")
        .expect("exec not found");
    assert!(
        approval_pos < exec_pos,
        "--ask-for-approval (pos {approval_pos}) must precede exec (pos {exec_pos}): {args}"
    );

    assert!(
        args.lines()
            .position(|line| line == "--dangerously-bypass-approvals-and-sandbox")
            .expect("bypass flag not found")
            > exec_pos,
        "bypass flag should be an exec option after exec: {args}"
    );

    let log = std::process::Command::new("git")
        .args(["log", "-1", "--format=%s"])
        .current_dir(Path::new(wt_path_str.trim()))
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&log.stdout).trim(),
        "Codex sandbox commit"
    );

    let transcript =
        fs::read_to_string(wt_run_dir.join("sessions/session-1/transcript.jsonl")).unwrap();
    assert!(
        transcript.contains("codex sandboxed"),
        "transcript should capture Codex JSON output: {transcript}"
    );
}

#[test]
fn run_with_codex_preserves_caller_ssl_cert_file() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);

    let run_id = "20260606-codex-preserve-ssl-cert";
    let run_dir = main_dir.join(format!(".factory/runs/{run_id}"));
    fs::create_dir_all(&run_dir).unwrap();
    fs::write(run_dir.join("status"), "planned").unwrap();
    fs::write(run_dir.join("brief.md"), "# Brief\n\nRun Codex sandboxed\n").unwrap();

    let bin_dir = tmp.path().join("bin");
    write_mock_codex(&bin_dir, CODEX_SSL_CERT_FILE_RECORDER);
    write_mock_sandbox_exec(&bin_dir);
    let sandbox_exec_log = tmp.path().join("sandbox-exec.log");
    let ca_bundle = tmp.path().join("ca-bundle.pem");
    let caller_ca_bundle = tmp.path().join("caller-ca-bundle.pem");
    fs::write(&ca_bundle, "factory ca bundle").unwrap();
    fs::write(&caller_ca_bundle, "caller ca bundle").unwrap();

    let _guard = worktree_guard(&main_dir, run_id);

    factory_cmd()
        .current_dir(&main_dir)
        .args(["run", "--coder", "codex", "--run-id", run_id])
        .env("PATH", mock_path(&bin_dir))
        .env("SANDBOX_EXEC_LOG", &sandbox_exec_log)
        .env("FACTORY_CODEX_CA_BUNDLE", &ca_bundle)
        .env("SSL_CERT_FILE", &caller_ca_bundle)
        .assert()
        .success();

    let wt_path_str = fs::read_to_string(run_dir.join("worktree")).unwrap();
    let wt_run_dir = Path::new(wt_path_str.trim()).join(format!(".factory/runs/{run_id}"));
    let ssl_cert_file = fs::read_to_string(wt_run_dir.join("codex-ssl-cert-file")).unwrap();
    assert_eq!(ssl_cert_file.trim(), caller_ca_bundle.to_string_lossy());
    assert!(
        sandbox_exec_log.exists(),
        "sandboxed Codex should be launched through sandbox-exec"
    );
}

#[test]
fn run_unknown_coder_fails() {
    let tmp = TempDir::new().unwrap();
    fs::create_dir_all(tmp.path().join(".factory/runs")).unwrap();

    factory_cmd()
        .current_dir(tmp.path())
        .args(["run", "--no-sandbox", "--coder", "unknown"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("Unknown coder"));
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
    let wt_run_dir = Path::new(wt_path_str.trim()).join(format!(".factory/runs/{run_id}"));
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
    fs::write(claude_dir.join("history.jsonl"), r#"{"old":"history"}"#).unwrap();

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
    let session_dir =
        Path::new(wt_path_str.trim()).join(format!(".factory/runs/{run_id}/sessions/session-1"));

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
    fs::write(
        run_dir.join("brief.md"),
        "# Brief\n\nTest review archiving\n",
    )
    .unwrap();

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

# Author call — make a code change so reviews aren't skipped
echo "new code" > "$WORKING_DIR/feature.txt"
git -C "$WORKING_DIR" add feature.txt
git -C "$WORKING_DIR" commit -m "Add feature"
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
        "[system]\nYou are a test reviewer.\n[changes]\nReview the changes.\n[full]\nReview everything.\n",
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
    let wt_run_dir = Path::new(wt_path_str.trim()).join(format!(".factory/runs/{run_id}"));
    let round1_dir = wt_run_dir.join("reviews/round-1");
    assert!(round1_dir.exists(), "reviews/round-1/ archive should exist");
    assert!(
        round1_dir.join("review-tests.md").exists(),
        "round-1 should contain review-tests.md"
    );
}

#[test]
fn run_skips_reviews_when_no_code_changed() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);

    fs::write(main_dir.join(".gitignore"), ".factory/*\n").unwrap();
    let prompts_dir = main_dir.join("prompts");
    fs::create_dir_all(&prompts_dir).unwrap();
    fs::write(
        prompts_dir.join("review-tests.md"),
        "[system]\nReviewer.\n[changes]\nReview.\n[full]\nReview all.\n",
    )
    .unwrap();
    std::process::Command::new("git")
        .args(["add", "."])
        .current_dir(&main_dir)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["commit", "-m", "add test fixtures"])
        .current_dir(&main_dir)
        .output()
        .unwrap();

    let run_id = "20260515-skip-review";
    let run_dir = main_dir.join(format!(".factory/runs/{run_id}"));
    fs::create_dir_all(&run_dir).unwrap();
    fs::write(run_dir.join("status"), "planned").unwrap();
    fs::write(run_dir.join("brief.md"), "# Brief\n\nTrivial run\n").unwrap();

    // Mock claude that writes complete without making any commits
    let bin_dir = tmp.path().join("bin");
    write_mock_claude(
        &bin_dir,
        r##"#!/bin/bash
WORKING_DIR="$(pwd)"
RUN_ID=$(ls "$WORKING_DIR/.factory/runs/" 2>/dev/null | head -1)
echo '{"type":"result"}'
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
        .stderr(predicate::str::contains("No code changes"))
        .stderr(predicate::str::contains("skipping reviews"));

    // Reviews directory should not have any review artifacts
    let wt_path_str = fs::read_to_string(run_dir.join("worktree")).unwrap();
    let wt_run_dir = Path::new(wt_path_str.trim()).join(format!(".factory/runs/{run_id}"));
    assert!(
        !wt_run_dir.join("reviews/review-tests.md").exists(),
        "reviewer should not have run"
    );
}

#[test]
fn run_reviews_when_complete_worktree_is_dirty() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);

    fs::write(main_dir.join(".gitignore"), ".factory/*\n").unwrap();
    let prompts_dir = main_dir.join("prompts");
    fs::create_dir_all(&prompts_dir).unwrap();
    fs::write(
        prompts_dir.join("review-tests.md"),
        "[system]\nReviewer.\n[changes]\nReview.\n[full]\nReview all.\n",
    )
    .unwrap();
    std::process::Command::new("git")
        .args(["add", "."])
        .current_dir(&main_dir)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["commit", "-m", "add test fixtures"])
        .current_dir(&main_dir)
        .output()
        .unwrap();

    let run_id = "20260515-dirty-review";
    let run_dir = main_dir.join(format!(".factory/runs/{run_id}"));
    fs::create_dir_all(&run_dir).unwrap();
    fs::write(run_dir.join("status"), "planned").unwrap();
    fs::write(run_dir.join("brief.md"), "# Brief\n\nDirty run\n").unwrap();

    let bin_dir = tmp.path().join("bin");
    write_mock_claude(
        &bin_dir,
        r##"#!/bin/bash
WORKING_DIR="$(pwd)"
RUN_ID=$(ls "$WORKING_DIR/.factory/runs/" 2>/dev/null | head -1)
echo '{"type":"result"}'
if [ ! -f "$WORKING_DIR/.factory/runs/$RUN_ID/authored" ]; then
  echo "dirty work" > "$WORKING_DIR/dirty.txt"
  touch "$WORKING_DIR/.factory/runs/$RUN_ID/authored"
elif [ -f "$WORKING_DIR/.factory/runs/$RUN_ID/handoff.md" ]; then
  git -C "$WORKING_DIR" add dirty.txt
  git -C "$WORKING_DIR" commit -m "Add dirty work"
fi
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
        .stderr(predicate::str::contains("Review phase"))
        .stderr(predicate::str::contains("No code changes").not());

    let wt_path_str = fs::read_to_string(run_dir.join("worktree")).unwrap();
    let wt_path = Path::new(wt_path_str.trim());
    assert!(
        wt_path.join("dirty.txt").exists(),
        "dirty author work should remain in the worktree after it is committed"
    );
    let status = std::process::Command::new("git")
        .args(["status", "--porcelain", "--untracked-files=normal"])
        .current_dir(wt_path)
        .output()
        .unwrap();
    assert!(
        status.stdout.is_empty(),
        "worktree should be clean after completion: {}",
        String::from_utf8_lossy(&status.stdout)
    );
    let log = std::process::Command::new("git")
        .args(["log", "--oneline", "-3"])
        .current_dir(wt_path)
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&log.stdout).contains("Add dirty work"),
        "dirty author work should be committed before completion"
    );
}

// -------------------------------------------------------------------------
// Land
// -------------------------------------------------------------------------

/// Set up a git project with a completed run in a worktree, ready to land.
fn setup_completed_run(tmp: &TempDir) -> (std::path::PathBuf, String) {
    let main_dir = setup_git_project(tmp);
    let run_id = "20260515-land-test";
    let run_dir = main_dir.join(format!(".factory/runs/{run_id}"));
    fs::create_dir_all(&run_dir).unwrap();
    fs::write(run_dir.join("status"), "planned").unwrap();
    fs::write(run_dir.join("brief.md"), "# Brief\n\nTest landing\n").unwrap();

    let bin_dir = tmp.path().join("bin");
    write_mock_claude(
        &bin_dir,
        r##"#!/bin/bash
WORKING_DIR="$(pwd)"
RUN_ID=$(ls "$WORKING_DIR/.factory/runs/" 2>/dev/null | head -1)
RUN_DIR="$WORKING_DIR/.factory/runs/$RUN_ID"
# Write a commit in the worktree
echo "new content" > "$WORKING_DIR/feature.txt"
git -C "$WORKING_DIR" add feature.txt
git -C "$WORKING_DIR" commit -m "Add feature"
# Create review and session artifacts
mkdir -p "$RUN_DIR/reviews"
printf 'Verdict: pass\n\nAll good.\n' > "$RUN_DIR/reviews/review-tests.md"
mkdir -p "$RUN_DIR/sessions/session-1"
echo '{"type":"result"}' > "$RUN_DIR/sessions/session-1/transcript.jsonl"
printf 'session=1 exit=0 duration=5s status=complete\n' > "$RUN_DIR/sessions.log"
printf '# Report\nDone.\n' > "$RUN_DIR/report.md"
echo "complete" > "$RUN_DIR/status"
exit 0
"##,
    );

    factory_cmd()
        .current_dir(&main_dir)
        .args(["run", "--no-sandbox", "--run-id", run_id])
        .env("PATH", mock_path(&bin_dir))
        .assert()
        .success();

    (main_dir, run_id.to_string())
}

#[test]
fn land_completes_full_lifecycle() {
    let tmp = TempDir::new().unwrap();
    let (main_dir, run_id) = setup_completed_run(&tmp);
    let run_dir = main_dir.join(format!(".factory/runs/{run_id}"));

    // Verify worktree exists before landing
    let wt_path_str = fs::read_to_string(run_dir.join("worktree")).unwrap();
    let wt_path = Path::new(wt_path_str.trim());
    assert!(wt_path.is_dir(), "worktree should exist before landing");

    factory_cmd()
        .current_dir(&main_dir)
        .args(["land", &run_id])
        .assert()
        .success()
        .stderr(predicate::str::contains("Landing run"))
        .stderr(predicate::str::contains("landed successfully"));

    // Verify artifacts were copied back
    assert!(
        run_dir.join("sessions/session-1/transcript.jsonl").exists(),
        "sessions should be copied back"
    );
    assert!(
        run_dir.join("sessions.log").exists(),
        "sessions.log should be copied back"
    );
    assert!(
        run_dir.join("reviews/review-tests.md").exists(),
        "reviews should be copied back"
    );
    assert!(
        run_dir.join("report.md").exists(),
        "report.md should be copied back"
    );

    // Verify status is landed
    let status = fs::read_to_string(run_dir.join("status")).unwrap();
    assert_eq!(status.trim(), "landed");

    // Verify worktree was removed
    assert!(
        !wt_path.is_dir(),
        "worktree should be removed after landing"
    );

    // Verify branch was deleted
    let branches = std::process::Command::new("git")
        .args(["-C", &main_dir.to_string_lossy()])
        .args(["branch", "--list", &run_id])
        .output()
        .unwrap();
    let branch_list = String::from_utf8_lossy(&branches.stdout);
    assert!(
        branch_list.trim().is_empty(),
        "branch should be deleted after landing"
    );

    // Verify commit is on main
    let log = std::process::Command::new("git")
        .args(["-C", &main_dir.to_string_lossy()])
        .args(["log", "--oneline", "-5"])
        .output()
        .unwrap();
    let log_str = String::from_utf8_lossy(&log.stdout);
    assert!(
        log_str.contains("Add feature"),
        "feature commit should be on main: {log_str}"
    );
}

#[test]
fn land_resolves_most_recent_complete_run() {
    let tmp = TempDir::new().unwrap();
    let (main_dir, run_id) = setup_completed_run(&tmp);

    // Land without specifying run ID
    factory_cmd()
        .current_dir(&main_dir)
        .args(["land"])
        .assert()
        .success()
        .stderr(predicate::str::contains(&run_id));
}

#[test]
fn land_rejects_non_complete_run() {
    let tmp = TempDir::new().unwrap();
    let run_dir = tmp.path().join(".factory/runs/test-not-complete");
    fs::create_dir_all(&run_dir).unwrap();
    fs::write(run_dir.join("status"), "executing").unwrap();

    factory_cmd()
        .current_dir(tmp.path())
        .args(["land", "test-not-complete"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("expected 'complete'"));
}

#[test]
fn land_rejects_dirty_completed_worktree() {
    let tmp = TempDir::new().unwrap();
    let (main_dir, run_id) = setup_completed_run(&tmp);
    let run_dir = main_dir.join(format!(".factory/runs/{run_id}"));
    let wt_path_str = fs::read_to_string(run_dir.join("worktree")).unwrap();
    let wt_path = Path::new(wt_path_str.trim());

    fs::write(wt_path.join("leftover.txt"), "uncommitted\n").unwrap();

    factory_cmd()
        .current_dir(&main_dir)
        .args(["land", &run_id])
        .assert()
        .failure()
        .stderr(predicate::str::contains("uncommitted worktree changes"));

    assert!(
        wt_path.join("leftover.txt").exists(),
        "landing failure should preserve dirty worktree content"
    );
}

#[test]
fn land_runs_configured_check_before_landing() {
    let tmp = TempDir::new().unwrap();
    let (main_dir, run_id) = setup_completed_run(&tmp);
    let run_dir = main_dir.join(format!(".factory/runs/{run_id}"));
    let wt_path_str = fs::read_to_string(run_dir.join("worktree")).unwrap();
    let wt_path = Path::new(wt_path_str.trim());
    fs::create_dir_all(main_dir.join(".factory")).unwrap();
    fs::write(
        main_dir.join(".factory/config.toml"),
        r#"
[checks.format]
command = "printf check-failed >&2; exit 1"
fix_command = "cargo fmt --all"
run_before_land = true
"#,
    )
    .unwrap();

    factory_cmd()
        .current_dir(&main_dir)
        .args(["land", &run_id])
        .assert()
        .failure()
        .stderr(predicate::str::contains("Pre-land check 'format' failed"))
        .stderr(predicate::str::contains(
            "Configured fix command: cargo fmt --all",
        ))
        .stderr(predicate::str::contains("check-failed"));

    assert!(wt_path.is_dir(), "failed check should keep worktree");
}

#[test]
fn land_refuses_autofix_when_worktree_has_user_changes() {
    let tmp = TempDir::new().unwrap();
    let (main_dir, run_id) = setup_completed_run(&tmp);
    let run_dir = main_dir.join(format!(".factory/runs/{run_id}"));
    let wt_path_str = fs::read_to_string(run_dir.join("worktree")).unwrap();
    let wt_path = Path::new(wt_path_str.trim());

    fs::write(wt_path.join("dirty-user-file"), "do not commit me\n").unwrap();
    fs::write(
        main_dir.join(".factory/config.toml"),
        r#"
[checks.format]
command = "test -f already-fixed"
fix_command = "touch already-fixed"
autofix = true
run_before_land = true
"#,
    )
    .unwrap();

    factory_cmd()
        .current_dir(&main_dir)
        .args(["land", &run_id])
        .assert()
        .failure()
        .stderr(predicate::str::contains("Cannot autofix check 'format'"))
        .stderr(predicate::str::contains("uncommitted changes"));

    assert!(
        !wt_path.join("already-fixed").exists(),
        "fix command should not run when user-visible files are dirty"
    );
    assert!(
        wt_path.join("dirty-user-file").exists(),
        "dirty user work should remain in the worktree"
    );
}

#[test]
fn land_autofixes_and_reruns_reviewers() {
    let tmp = TempDir::new().unwrap();
    let (main_dir, run_id) = setup_completed_run(&tmp);
    let run_dir = main_dir.join(format!(".factory/runs/{run_id}"));
    let wt_path_str = fs::read_to_string(run_dir.join("worktree")).unwrap();
    let wt_path = Path::new(wt_path_str.trim());
    let wt_run_dir = wt_path.join(format!(".factory/runs/{run_id}"));

    fs::write(run_dir.join("reviewers"), "tests").unwrap();
    fs::write(wt_run_dir.join("reviewers"), "tests").unwrap();
    fs::write(wt_path.join("needs-format"), "bad\n").unwrap();
    std::process::Command::new("git")
        .args(["-C", &wt_path.to_string_lossy()])
        .args(["add", "needs-format"])
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["-C", &wt_path.to_string_lossy()])
        .args(["commit", "-m", "Add unformatted file"])
        .output()
        .unwrap();

    fs::write(
        main_dir.join(".factory/config.toml"),
        r#"
[checks.format]
command = "test ! -f needs-format"
fix_command = "rm needs-format"
autofix = true
run_before_land = true
"#,
    )
    .unwrap();

    let bin_dir = tmp.path().join("land-bin");
    write_mock_claude(
        &bin_dir,
        r##"#!/bin/bash
WORKING_DIR="$(pwd)"
RUN_ID=$(ls "$WORKING_DIR/.factory/runs/" 2>/dev/null | head -1)
RUN_DIR="$WORKING_DIR/.factory/runs/$RUN_ID"
mkdir -p "$RUN_DIR/reviews"
printf 'Verdict: pass\n\nAutofix review passed.\n' > "$RUN_DIR/reviews/review-tests.md"
printf 'reviewed\n' > "$RUN_DIR/review-rerun-marker"
exit 0
"##,
    );

    factory_cmd()
        .current_dir(&main_dir)
        .args(["land", &run_id])
        .env("PATH", mock_path(&bin_dir))
        .assert()
        .success()
        .stderr(predicate::str::contains("Autofix changes committed"))
        .stderr(predicate::str::contains("Rerunning reviewers"));

    let log = std::process::Command::new("git")
        .args(["-C", &main_dir.to_string_lossy()])
        .args(["log", "--oneline", "-5"])
        .output()
        .unwrap();
    let log = String::from_utf8_lossy(&log.stdout);
    assert!(log.contains("Apply project check autofix"));
    let review = fs::read_to_string(run_dir.join("reviews/review-tests.md")).unwrap();
    assert!(review.contains("Autofix review passed"));
    assert!(!main_dir.join("needs-format").exists());
}

#[test]
fn land_keeps_worktree_when_autofix_review_fails() {
    let tmp = TempDir::new().unwrap();
    let (main_dir, run_id) = setup_completed_run(&tmp);
    let run_dir = main_dir.join(format!(".factory/runs/{run_id}"));
    let wt_path_str = fs::read_to_string(run_dir.join("worktree")).unwrap();
    let wt_path = Path::new(wt_path_str.trim());
    let wt_run_dir = wt_path.join(format!(".factory/runs/{run_id}"));

    fs::write(run_dir.join("reviewers"), "tests").unwrap();
    fs::write(wt_run_dir.join("reviewers"), "tests").unwrap();
    fs::write(wt_path.join("needs-format"), "bad\n").unwrap();
    std::process::Command::new("git")
        .args(["-C", &wt_path.to_string_lossy()])
        .args(["add", "needs-format"])
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["-C", &wt_path.to_string_lossy()])
        .args(["commit", "-m", "Add unformatted file"])
        .output()
        .unwrap();

    fs::write(
        main_dir.join(".factory/config.toml"),
        r#"
[checks.format]
command = "test ! -f needs-format"
fix_command = "rm needs-format"
autofix = true
run_before_land = true
"#,
    )
    .unwrap();

    let bin_dir = tmp.path().join("land-bin-fail");
    write_mock_claude(
        &bin_dir,
        r##"#!/bin/bash
WORKING_DIR="$(pwd)"
RUN_ID=$(ls "$WORKING_DIR/.factory/runs/" 2>/dev/null | head -1)
RUN_DIR="$WORKING_DIR/.factory/runs/$RUN_ID"
mkdir -p "$RUN_DIR/reviews"
printf 'Verdict: fail\n\nAutofix needs more work.\n' > "$RUN_DIR/reviews/review-tests.md"
exit 0
"##,
    );

    factory_cmd()
        .current_dir(&main_dir)
        .args(["land", &run_id])
        .env("PATH", mock_path(&bin_dir))
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "reviewers did not pass after autofix",
        ));

    assert!(wt_path.is_dir(), "review failure should keep worktree");
    let review = fs::read_to_string(run_dir.join("reviews/review-tests.md")).unwrap();
    assert!(review.contains("Verdict: fail"));
    let status = fs::read_to_string(run_dir.join("status")).unwrap();
    assert_ne!(status.trim(), "landed");
}

#[test]
fn land_rejects_failed_reviews() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);

    let run_id = "20260515-land-fail-review";
    let run_dir = main_dir.join(format!(".factory/runs/{run_id}"));
    fs::create_dir_all(&run_dir).unwrap();
    fs::write(run_dir.join("status"), "complete").unwrap();
    fs::write(
        run_dir.join("worktree"),
        main_dir.to_string_lossy().as_ref(),
    )
    .unwrap();
    fs::write(run_dir.join("source-branch"), "main").unwrap();

    // Create a failing review
    fs::create_dir_all(run_dir.join("reviews")).unwrap();
    fs::write(
        run_dir.join("reviews/review-tests.md"),
        "Verdict: fail\n\nTests broken.\n",
    )
    .unwrap();

    factory_cmd()
        .current_dir(&main_dir)
        .args(["land", run_id])
        .assert()
        .failure()
        .stderr(predicate::str::contains("reviews did not pass"));
}

#[test]
fn land_fails_when_no_complete_run() {
    let tmp = TempDir::new().unwrap();
    fs::create_dir_all(tmp.path().join(".factory/runs/some-run")).unwrap();
    fs::write(
        tmp.path().join(".factory/runs/some-run/status"),
        "executing",
    )
    .unwrap();

    factory_cmd()
        .current_dir(tmp.path())
        .args(["land"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("No complete run found"));
}

#[test]
fn land_preserves_linear_history() {
    let tmp = TempDir::new().unwrap();
    let (main_dir, run_id) = setup_completed_run(&tmp);

    factory_cmd()
        .current_dir(&main_dir)
        .args(["land", &run_id])
        .assert()
        .success();

    // Verify no merge commits exist (linear history)
    let log = std::process::Command::new("git")
        .args(["-C", &main_dir.to_string_lossy()])
        .args(["log", "--oneline", "--merges"])
        .output()
        .unwrap();
    let merge_log = String::from_utf8_lossy(&log.stdout);
    assert!(
        merge_log.trim().is_empty(),
        "should have no merge commits (linear history): {merge_log}"
    );
}

#[test]
fn land_fails_on_rebase_conflict() {
    let tmp = TempDir::new().unwrap();
    let (main_dir, run_id) = setup_completed_run(&tmp);
    let run_dir = main_dir.join(format!(".factory/runs/{run_id}"));

    // Create a conflicting commit on main after the run branched
    fs::write(main_dir.join("feature.txt"), "conflicting content").unwrap();
    std::process::Command::new("git")
        .args(["-C", &main_dir.to_string_lossy()])
        .args(["add", "feature.txt"])
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["-C", &main_dir.to_string_lossy()])
        .args(["commit", "-m", "conflicting commit on main"])
        .output()
        .unwrap();

    factory_cmd()
        .current_dir(&main_dir)
        .args(["land", &run_id])
        .assert()
        .failure()
        .stderr(predicate::str::contains("Rebase failed"));

    // Verify repo is not left in a rebase state
    let rebase_dir = main_dir.join(".git/rebase-merge");
    assert!(
        !rebase_dir.exists(),
        "rebase should have been aborted on failure"
    );

    // Verify status was NOT changed to landed
    let run_status = fs::read_to_string(run_dir.join("status")).unwrap();
    assert_ne!(
        run_status.trim(),
        "landed",
        "status should not be landed after failed rebase"
    );
}

#[test]
fn land_fails_when_worktree_file_missing() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);

    let run_id = "20260515-land-no-wt";
    let run_dir = main_dir.join(format!(".factory/runs/{run_id}"));
    fs::create_dir_all(&run_dir).unwrap();
    fs::write(run_dir.join("status"), "complete").unwrap();
    fs::write(run_dir.join("source-branch"), "main").unwrap();
    // Deliberately omit the worktree file

    factory_cmd()
        .current_dir(&main_dir)
        .args(["land", run_id])
        .assert()
        .failure()
        .stderr(predicate::str::contains("worktree"));
}
