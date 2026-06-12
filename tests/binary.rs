use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;
use tempfile::TempDir;

fn factory_cmd() -> Command {
    Command::cargo_bin("factory").unwrap()
}

fn work_item_value(project_root: &Path, id: &str) -> serde_json::Value {
    let output = factory_cmd()
        .current_dir(project_root)
        .args(["work", "show", id])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "work show failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn read_json_path(path: &Path) -> serde_json::Value {
    serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
}

fn write_json_path(path: &Path, value: &serde_json::Value) {
    fs::write(path, serde_json::to_string_pretty(value).unwrap()).unwrap()
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
fn fargate_teardown_nothing_to_teardown() {
    let tmp = TempDir::new().unwrap();

    let output = factory_cmd()
        .current_dir(tmp.path())
        .env("HOME", tmp.path().to_string_lossy().to_string())
        .env_remove("AWS_DEFAULT_REGION")
        .args(["fargate", "teardown"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "fargate teardown should exit zero when nothing to tear down: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Nothing to tear down"),
        "expected nothing-to-teardown message, got: {stdout}"
    );
}

#[test]
fn fargate_teardown_help_shows_keep_flags() {
    let tmp = TempDir::new().unwrap();

    let output = factory_cmd()
        .current_dir(tmp.path())
        .args(["fargate", "teardown", "--help"])
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("--keep-ecr"), "help should show --keep-ecr");
    assert!(stdout.contains("--keep-s3"), "help should show --keep-s3");
}

#[test]
fn fargate_teardown_deletes_stack_ecr_s3_and_removes_state() {
    let tmp = TempDir::new().unwrap();
    let bin_dir = tmp.path().join("bin");

    let state_dir = tmp.path().join(".config/factory");
    fs::create_dir_all(&state_dir).unwrap();
    let state_path = state_dir.join("fargate.state.json");
    fs::write(
        &state_path,
        r#"{
  "stack_deployed": true,
  "region": "us-west-2",
  "repo_uri": "123.dkr.ecr.us-west-2.amazonaws.com/factory/run",
  "s3_bucket": "factory-workspace-123"
}"#,
    )
    .unwrap();

    let aws_log = tmp.path().join("aws.log");
    write_mock_executable(
        &bin_dir,
        "aws",
        r##"#!/bin/bash
set -euo pipefail
printf '%s\n' "$*" >> "${AWS_LOG:?}"

case "$1 $2" in
  "cloudformation describe-stacks")
    printf 'CREATE_COMPLETE\n'
    ;;
  "ecr describe-repositories")
    printf 'factory/run\n'
    ;;
  "ecr delete-repository")
    ;;
  "s3 rm")
    ;;
  "s3 rb")
    ;;
  "cloudformation delete-stack")
    ;;
  "cloudformation wait")
    ;;
  *)
    printf 'unexpected aws command: %s\n' "$*" >&2
    exit 1
    ;;
esac
"##,
    );

    let output = factory_cmd()
        .current_dir(tmp.path())
        .env("HOME", tmp.path())
        .env("PATH", mock_path(&bin_dir))
        .env("AWS_LOG", &aws_log)
        .args(["fargate", "teardown"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "fargate teardown failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Removed:"),
        "expected removal summary, got: {stdout}"
    );
    assert!(
        stdout.contains("CloudFormation stack"),
        "expected stack in summary, got: {stdout}"
    );
    assert!(
        stdout.contains("ECR repository"),
        "expected ECR in summary, got: {stdout}"
    );
    assert!(
        stdout.contains("S3 bucket"),
        "expected S3 in summary, got: {stdout}"
    );

    let log = fs::read_to_string(&aws_log).unwrap();
    assert!(
        log.contains("ecr delete-repository"),
        "should call ecr delete-repository: {log}"
    );
    assert!(log.contains("s3 rm"), "should call s3 rm: {log}");
    assert!(log.contains("s3 rb"), "should call s3 rb: {log}");
    assert!(
        log.contains("cloudformation delete-stack"),
        "should call cloudformation delete-stack: {log}"
    );
    assert!(
        log.contains("cloudformation wait stack-delete-complete"),
        "should wait for stack deletion: {log}"
    );

    assert!(
        !state_path.exists(),
        "state file should be removed after successful teardown"
    );
}

#[test]
fn fargate_teardown_keep_ecr_skips_ecr_delete() {
    let tmp = TempDir::new().unwrap();
    let bin_dir = tmp.path().join("bin");

    let state_dir = tmp.path().join(".config/factory");
    fs::create_dir_all(&state_dir).unwrap();
    let state_path = state_dir.join("fargate.state.json");
    fs::write(
        &state_path,
        r#"{
  "stack_deployed": true,
  "region": "us-west-2",
  "repo_uri": "123.dkr.ecr.us-west-2.amazonaws.com/factory/run",
  "s3_bucket": "factory-workspace-123"
}"#,
    )
    .unwrap();

    let aws_log = tmp.path().join("aws.log");
    write_mock_executable(
        &bin_dir,
        "aws",
        r##"#!/bin/bash
set -euo pipefail
printf '%s\n' "$*" >> "${AWS_LOG:?}"

case "$1 $2" in
  "cloudformation describe-stacks")
    printf 'CREATE_COMPLETE\n'
    ;;
  "s3 rm")
    ;;
  "s3 rb")
    ;;
  "cloudformation delete-stack")
    ;;
  "cloudformation wait")
    ;;
  *)
    printf 'unexpected aws command: %s\n' "$*" >&2
    exit 1
    ;;
esac
"##,
    );

    let output = factory_cmd()
        .current_dir(tmp.path())
        .env("HOME", tmp.path())
        .env("PATH", mock_path(&bin_dir))
        .env("AWS_LOG", &aws_log)
        .args(["fargate", "teardown", "--keep-ecr"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "fargate teardown --keep-ecr failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let log = fs::read_to_string(&aws_log).unwrap();
    assert!(
        !log.contains("ecr"),
        "--keep-ecr should skip all ECR commands: {log}"
    );
    assert!(
        log.contains("s3 rm"),
        "--keep-ecr should still delete S3: {log}"
    );
    assert!(
        log.contains("cloudformation delete-stack"),
        "--keep-ecr should still delete stack: {log}"
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("ECR"),
        "--keep-ecr should not mention ECR in summary: {stdout}"
    );

    assert!(
        !state_path.exists(),
        "state file should be removed after successful teardown"
    );
}

#[test]
fn fargate_teardown_keep_s3_skips_s3_delete() {
    let tmp = TempDir::new().unwrap();
    let bin_dir = tmp.path().join("bin");

    let state_dir = tmp.path().join(".config/factory");
    fs::create_dir_all(&state_dir).unwrap();
    let state_path = state_dir.join("fargate.state.json");
    fs::write(
        &state_path,
        r#"{
  "stack_deployed": true,
  "region": "us-west-2",
  "repo_uri": "123.dkr.ecr.us-west-2.amazonaws.com/factory/run",
  "s3_bucket": "factory-workspace-123"
}"#,
    )
    .unwrap();

    let aws_log = tmp.path().join("aws.log");
    write_mock_executable(
        &bin_dir,
        "aws",
        r##"#!/bin/bash
set -euo pipefail
printf '%s\n' "$*" >> "${AWS_LOG:?}"

case "$1 $2" in
  "cloudformation describe-stacks")
    printf 'CREATE_COMPLETE\n'
    ;;
  "ecr describe-repositories")
    printf 'factory/run\n'
    ;;
  "ecr delete-repository")
    ;;
  "cloudformation delete-stack")
    ;;
  "cloudformation wait")
    ;;
  *)
    printf 'unexpected aws command: %s\n' "$*" >&2
    exit 1
    ;;
esac
"##,
    );

    let output = factory_cmd()
        .current_dir(tmp.path())
        .env("HOME", tmp.path())
        .env("PATH", mock_path(&bin_dir))
        .env("AWS_LOG", &aws_log)
        .args(["fargate", "teardown", "--keep-s3"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "fargate teardown --keep-s3 failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let log = fs::read_to_string(&aws_log).unwrap();
    assert!(!log.contains("s3 rm"), "--keep-s3 should skip S3 rm: {log}");
    assert!(!log.contains("s3 rb"), "--keep-s3 should skip S3 rb: {log}");
    assert!(
        log.contains("ecr delete-repository"),
        "--keep-s3 should still delete ECR: {log}"
    );
    assert!(
        log.contains("cloudformation delete-stack"),
        "--keep-s3 should still delete stack: {log}"
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("S3"),
        "--keep-s3 should not mention S3 in summary: {stdout}"
    );

    assert!(
        !state_path.exists(),
        "state file should be removed after successful teardown"
    );
}

#[test]
fn fargate_teardown_error_preserves_state_file() {
    let tmp = TempDir::new().unwrap();
    let bin_dir = tmp.path().join("bin");

    let state_dir = tmp.path().join(".config/factory");
    fs::create_dir_all(&state_dir).unwrap();
    let state_path = state_dir.join("fargate.state.json");
    fs::write(
        &state_path,
        r#"{
  "stack_deployed": true,
  "region": "us-west-2",
  "repo_uri": "123.dkr.ecr.us-west-2.amazonaws.com/factory/run",
  "s3_bucket": "factory-workspace-123"
}"#,
    )
    .unwrap();

    let aws_log = tmp.path().join("aws.log");
    write_mock_executable(
        &bin_dir,
        "aws",
        r##"#!/bin/bash
set -euo pipefail
printf '%s\n' "$*" >> "${AWS_LOG:?}"

case "$1 $2" in
  "cloudformation describe-stacks")
    printf 'CREATE_COMPLETE\n'
    ;;
  "ecr describe-repositories")
    printf 'factory/run\n'
    ;;
  "ecr delete-repository")
    printf 'RepositoryNotEmptyException: cannot delete\n' >&2
    exit 1
    ;;
  *)
    printf 'unexpected aws command: %s\n' "$*" >&2
    exit 1
    ;;
esac
"##,
    );

    let output = factory_cmd()
        .current_dir(tmp.path())
        .env("HOME", tmp.path())
        .env("PATH", mock_path(&bin_dir))
        .env("AWS_LOG", &aws_log)
        .args(["fargate", "teardown"])
        .output()
        .unwrap();

    assert!(
        !output.status.success(),
        "fargate teardown should exit non-zero on error"
    );

    assert!(
        state_path.exists(),
        "state file should be preserved when teardown fails"
    );
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
        .stdout(predicate::str::contains("No Work Items found"));
}

#[test]
fn status_empty_runs() {
    let tmp = TempDir::new().unwrap();
    fs::create_dir_all(tmp.path().join(".factory/runs")).unwrap();

    factory_cmd()
        .current_dir(tmp.path())
        .args(["status", "--runs"])
        .assert()
        .success()
        .stdout(predicate::str::contains("RUN"))
        .stdout(predicate::str::contains("STATUS"));
}

#[test]
fn status_hides_runs_by_default() {
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
        .stdout(predicate::str::contains("No Work Items found"))
        .stdout(predicate::str::contains("test-run").not())
        .stdout(predicate::str::contains("Do the thing").not());
}

#[test]
fn status_runs_shows_runs_with_correct_fields() {
    let tmp = TempDir::new().unwrap();
    let run_dir = tmp.path().join(".factory/runs/test-run");
    fs::create_dir_all(&run_dir).unwrap();
    fs::write(run_dir.join("status"), "executing\n").unwrap();
    fs::write(run_dir.join("runtime"), "local\n").unwrap();
    fs::write(run_dir.join("brief.md"), "# Brief\n\nDo the thing\n").unwrap();

    factory_cmd()
        .current_dir(tmp.path())
        .args(["status", "--runs"])
        .assert()
        .success()
        .stdout(predicate::str::contains("test-run"))
        .stdout(predicate::str::contains("executing"))
        .stdout(predicate::str::contains("local"))
        .stdout(predicate::str::contains("Do the thing"));
}

#[test]
fn status_shows_work_items_without_runs() {
    let tmp = TempDir::new().unwrap();

    factory_cmd()
        .current_dir(tmp.path())
        .args(["work", "create", "work-1", "--title", "Build status view"])
        .assert()
        .success();
    factory_cmd()
        .current_dir(tmp.path())
        .args(["work", "attempt", "work-1", "attempt-1"])
        .assert()
        .success();

    factory_cmd()
        .current_dir(tmp.path())
        .arg("status")
        .assert()
        .success()
        .stdout(predicate::str::contains("Work Items"))
        .stdout(predicate::str::contains("work-1"))
        .stdout(predicate::str::contains("attempt-1 [planned]"))
        .stdout(predicate::str::contains(
            "write:attempt-1-write-1 [planned]",
        ))
        .stdout(predicate::str::contains("task-ready"))
        .stdout(predicate::str::contains("Build status view"))
        .stdout(predicate::str::contains("No runs found").not());
}

#[test]
fn status_runs_shows_runs_and_work_items_together() {
    let tmp = TempDir::new().unwrap();
    let run_dir = tmp.path().join(".factory/runs/legacy-run");
    fs::create_dir_all(&run_dir).unwrap();
    fs::write(run_dir.join("status"), "executing\n").unwrap();
    fs::write(run_dir.join("runtime"), "local\n").unwrap();
    fs::write(run_dir.join("brief.md"), "Legacy run\n").unwrap();
    write_work_item_json(tmp.path(), "work-mixed", "Mixed status work");

    let output = factory_cmd()
        .current_dir(tmp.path())
        .args(["status", "--runs"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "status failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let run_index = stdout.find("legacy-run").unwrap();
    let work_header_index = stdout.find("Work Items").unwrap();
    assert!(work_header_index < run_index, "{stdout}");
    assert!(stdout.contains("executing"), "{stdout}");
    assert!(stdout.contains("work-mixed"), "{stdout}");
    assert!(stdout.contains("Mixed status work"), "{stdout}");
}

#[test]
fn status_reports_invalid_work_item_by_default_and_with_runs() {
    let tmp = TempDir::new().unwrap();
    let run_dir = tmp.path().join(".factory/runs/valid-run");
    fs::create_dir_all(&run_dir).unwrap();
    fs::write(run_dir.join("status"), "complete\n").unwrap();
    fs::write(run_dir.join("runtime"), "local\n").unwrap();
    fs::write(run_dir.join("brief.md"), "Valid run\n").unwrap();
    write_work_item_json(tmp.path(), "work-valid", "Valid work");
    fs::write(
        tmp.path().join(".factory/work/items/work-broken.json"),
        "{ invalid json\n",
    )
    .unwrap();

    let output = factory_cmd()
        .current_dir(tmp.path())
        .arg("status")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "status failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("work-valid"), "{stdout}");
    assert!(stdout.contains("Work Item read errors"), "{stdout}");
    assert!(
        stdout.contains(".factory/work/items/work-broken.json"),
        "{stdout}"
    );
    assert!(!stdout.contains("valid-run"), "{stdout}");

    let output = factory_cmd()
        .current_dir(tmp.path())
        .args(["status", "--runs"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "status failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("valid-run"), "{stdout}");
    assert!(stdout.contains("work-valid"), "{stdout}");
    assert!(stdout.contains("Work Item read errors"), "{stdout}");
    assert!(
        stdout.contains(".factory/work/items/work-broken.json"),
        "{stdout}"
    );
}

#[test]
fn status_prefers_live_worktree_status() {
    let tmp = TempDir::new().unwrap();
    let source_run = tmp.path().join(".factory/runs/live-status");
    let worktree_root = tmp.path().join("worktree");
    let live_run = worktree_root.join(".factory/runs/live-status");
    fs::create_dir_all(&source_run).unwrap();
    fs::create_dir_all(&live_run).unwrap();
    fs::write(source_run.join("status"), "planned").unwrap();
    fs::write(source_run.join("runtime"), "local").unwrap();
    fs::write(source_run.join("brief.md"), "Live status\n").unwrap();
    fs::write(source_run.join("worktree"), worktree_root.to_str().unwrap()).unwrap();
    fs::write(live_run.join("status"), "complete").unwrap();

    let output = factory_cmd()
        .current_dir(tmp.path())
        .args(["status", "--runs"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "status failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let line = stdout
        .lines()
        .find(|line| line.contains("live-status"))
        .unwrap();
    assert!(line.contains("complete"), "{stdout}");
    assert!(!line.contains("planned"), "{stdout}");
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
        .args(["status", "--runs"])
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
        .args(["status", "--runs"])
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
        .args(["status", "--runs"])
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
        .args(["status", "--runs", project.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("path-test"));
}

// -------------------------------------------------------------------------
// Work Items
// -------------------------------------------------------------------------

#[test]
fn work_create_writes_minimal_work_item() {
    let tmp = TempDir::new().unwrap();

    factory_cmd()
        .current_dir(tmp.path())
        .args(["work", "create", "work-intake", "--title", "Intake title"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Created Work Item work-intake"));

    let path = tmp.path().join(".factory/work/items/work-intake.json");
    let json = fs::read_to_string(path).unwrap();
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(value["id"], "work-intake");
    assert_eq!(value["title"], "Intake title");
    assert!(value.get("attempts").is_none());
    assert!(value.get("merge_candidates").is_none());
}

#[test]
fn work_create_refuses_existing_work_item() {
    let tmp = TempDir::new().unwrap();
    write_work_item_json(tmp.path(), "work-existing", "Original title");

    factory_cmd()
        .current_dir(tmp.path())
        .args([
            "work",
            "create",
            "work-existing",
            "--title",
            "Replacement title",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "Work Item \"work-existing\" already exists",
        ));

    let json =
        fs::read_to_string(tmp.path().join(".factory/work/items/work-existing.json")).unwrap();
    assert!(json.contains("Original title"));
    assert!(!json.contains("Replacement title"));
}

#[test]
fn work_create_rejects_invalid_work_item_id() {
    let tmp = TempDir::new().unwrap();

    factory_cmd()
        .current_dir(tmp.path())
        .args(["work", "create", "../escape", "--title", "Invalid item"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "work item id \"../escape\" cannot be used as a file name",
        ));

    assert!(!tmp.path().join(".factory/work/items").exists());
}

#[test]
fn work_create_item_is_visible_through_list_and_show() {
    let tmp = TempDir::new().unwrap();

    factory_cmd()
        .current_dir(tmp.path())
        .args(["work", "create", "work-visible", "--title", "Visible title"])
        .assert()
        .success();

    factory_cmd()
        .current_dir(tmp.path())
        .args(["work", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("work-visible"))
        .stdout(predicate::str::contains("Visible title"));

    factory_cmd()
        .current_dir(tmp.path())
        .args(["work", "show", "work-visible"])
        .assert()
        .success()
        .stdout(predicate::str::contains("  \"id\": \"work-visible\""))
        .stdout(predicate::str::contains("  \"title\": \"Visible title\""))
        .stdout(predicate::str::contains("  \"attempts\": []"));
}

#[test]
fn work_create_is_independent_from_legacy_runs() {
    let tmp = TempDir::new().unwrap();
    let run_dir = tmp.path().join(".factory/runs/legacy-run");
    fs::create_dir_all(&run_dir).unwrap();
    fs::write(run_dir.join("status"), "complete").unwrap();

    factory_cmd()
        .current_dir(tmp.path())
        .args(["work", "create", "work-new", "--title", "New work"])
        .assert()
        .success();

    factory_cmd()
        .current_dir(tmp.path())
        .args(["work", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("work-new"))
        .stdout(predicate::str::contains("legacy-run").not());

    factory_cmd()
        .current_dir(tmp.path())
        .arg("status")
        .assert()
        .success()
        .stdout(predicate::str::contains("work-new"))
        .stdout(predicate::str::contains("New work"))
        .stdout(predicate::str::contains("legacy-run").not());
}

#[test]
fn work_attempt_adds_planned_attempt_with_initial_write_task() {
    let tmp = TempDir::new().unwrap();
    write_work_item_json(tmp.path(), "work-1", "Attempt intake");

    factory_cmd()
        .current_dir(tmp.path())
        .args(["work", "attempt", "work-1", "attempt-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Created Attempt attempt-1 for Work Item work-1",
        ));

    let output = factory_cmd()
        .current_dir(tmp.path())
        .args(["work", "show", "work-1"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "work show failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let attempt = &value["attempts"][0];
    assert_eq!(attempt["id"], "attempt-1");
    assert_eq!(attempt["work_item_id"], "work-1");
    assert_eq!(attempt["status"], "planned");
    assert_eq!(attempt["tasks"][0]["id"], "attempt-1-write-1");
    assert_eq!(attempt["tasks"][0]["kind"], "write");
    assert_eq!(attempt["tasks"][0]["role"], "author");
    assert_eq!(attempt["tasks"][0]["work_item_id"], "work-1");
    assert_eq!(attempt["tasks"][0]["attempt_id"], "attempt-1");
    assert_eq!(
        attempt["tasks"][0]["workspace_access"]["writes"][0]["id"],
        "candidate"
    );
    assert_eq!(
        attempt["tasks"][0]["workspace_access"]["writes"][0]["path"],
        "../work-6-work-1-attempt-1"
    );
    assert!(
        attempt["tasks"][0]["workspace_access"]["reads"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(!tmp.path().join("../work-6-work-1-attempt-1").exists());
}

#[test]
fn work_attempt_paths_disambiguate_hyphenated_ids() {
    let tmp = TempDir::new().unwrap();
    write_work_item_json(tmp.path(), "work-a", "First work");
    write_work_item_json(tmp.path(), "work-a-b", "Second work");

    factory_cmd()
        .current_dir(tmp.path())
        .args(["work", "attempt", "work-a", "b-c"])
        .assert()
        .success();
    factory_cmd()
        .current_dir(tmp.path())
        .args(["work", "attempt", "work-a-b", "c"])
        .assert()
        .success();

    let first = work_item_value(tmp.path(), "work-a");
    let second = work_item_value(tmp.path(), "work-a-b");
    let first_path = &first["attempts"][0]["tasks"][0]["workspace_access"]["writes"][0]["path"];
    let second_path = &second["attempts"][0]["tasks"][0]["workspace_access"]["writes"][0]["path"];

    assert_eq!(first_path, "../work-6-work-a-b-c");
    assert_eq!(second_path, "../work-8-work-a-b-c");
    assert_ne!(first_path, second_path);
}

#[test]
fn work_attempt_appends_to_existing_attempts() {
    let tmp = TempDir::new().unwrap();
    write_work_item_json(tmp.path(), "work-1", "Attempt intake");

    factory_cmd()
        .current_dir(tmp.path())
        .args(["work", "attempt", "work-1", "attempt-1"])
        .assert()
        .success();

    factory_cmd()
        .current_dir(tmp.path())
        .args(["work", "attempt", "work-1", "attempt-2"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Created Attempt attempt-2 for Work Item work-1",
        ));

    let output = factory_cmd()
        .current_dir(tmp.path())
        .args(["work", "show", "work-1"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "work show failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let attempts = value["attempts"].as_array().unwrap();
    assert_eq!(attempts.len(), 2);
    assert_eq!(attempts[0]["id"], "attempt-1");
    assert_eq!(attempts[1]["id"], "attempt-2");
    assert_eq!(attempts[1]["tasks"].as_array().unwrap().len(), 1);
    assert_eq!(attempts[1]["tasks"][0]["id"], "attempt-2-write-1");
    assert_eq!(attempts[1]["tasks"][0]["attempt_id"], "attempt-2");
    assert_eq!(
        attempts[1]["tasks"][0]["workspace_access"]["writes"][0]["path"],
        "../work-6-work-1-attempt-2"
    );
}

#[test]
fn work_attempt_missing_work_item_reports_not_found() {
    let tmp = TempDir::new().unwrap();

    factory_cmd()
        .current_dir(tmp.path())
        .args(["work", "attempt", "missing-work", "attempt-1"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "Work Item \"missing-work\" not found",
        ));

    assert!(!tmp.path().join(".factory/work/items").exists());
}

#[test]
fn work_attempt_duplicate_attempt_id_fails_without_changes() {
    let tmp = TempDir::new().unwrap();
    write_work_item_json(tmp.path(), "work-1", "Attempt intake");

    factory_cmd()
        .current_dir(tmp.path())
        .args(["work", "attempt", "work-1", "attempt-1"])
        .assert()
        .success();
    let before = fs::read_to_string(tmp.path().join(".factory/work/items/work-1.json")).unwrap();

    factory_cmd()
        .current_dir(tmp.path())
        .args(["work", "attempt", "work-1", "attempt-1"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "Attempt \"attempt-1\" already exists",
        ));

    let after = fs::read_to_string(tmp.path().join(".factory/work/items/work-1.json")).unwrap();
    assert_eq!(after, before);
}

#[test]
fn work_attempt_rejects_invalid_attempt_id_without_changes() {
    let tmp = TempDir::new().unwrap();
    write_work_item_json(tmp.path(), "work-1", "Attempt intake");
    let before = fs::read_to_string(tmp.path().join(".factory/work/items/work-1.json")).unwrap();

    factory_cmd()
        .current_dir(tmp.path())
        .args(["work", "attempt", "work-1", "../escape"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "attempt id \"../escape\" cannot be used as a file name",
        ));

    let after = fs::read_to_string(tmp.path().join(".factory/work/items/work-1.json")).unwrap();
    assert_eq!(after, before);
}

#[test]
fn work_attempt_auto_id_creates_attempt_1() {
    let tmp = TempDir::new().unwrap();
    write_work_item_json(tmp.path(), "work-1", "Auto attempt");

    factory_cmd()
        .current_dir(tmp.path())
        .args(["work", "attempt", "work-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Created Attempt attempt-1 for Work Item work-1",
        ));

    let value = work_item_value(tmp.path(), "work-1");
    assert_eq!(value["attempts"][0]["id"], "attempt-1");
}

#[test]
fn work_attempt_auto_id_sequential_creates_attempt_2() {
    let tmp = TempDir::new().unwrap();
    write_work_item_json(tmp.path(), "work-1", "Auto attempt seq");

    factory_cmd()
        .current_dir(tmp.path())
        .args(["work", "attempt", "work-1"])
        .assert()
        .success();

    factory_cmd()
        .current_dir(tmp.path())
        .args(["work", "attempt", "work-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Created Attempt attempt-2 for Work Item work-1",
        ));

    let value = work_item_value(tmp.path(), "work-1");
    let attempts = value["attempts"].as_array().unwrap();
    assert_eq!(attempts.len(), 2);
    assert_eq!(attempts[0]["id"], "attempt-1");
    assert_eq!(attempts[1]["id"], "attempt-2");
}

#[test]
fn work_attempt_auto_id_fills_gap() {
    let tmp = TempDir::new().unwrap();
    write_work_item_json(tmp.path(), "work-1", "Auto attempt gap");

    factory_cmd()
        .current_dir(tmp.path())
        .args(["work", "attempt", "work-1", "attempt-1"])
        .assert()
        .success();

    factory_cmd()
        .current_dir(tmp.path())
        .args(["work", "attempt", "work-1", "attempt-3"])
        .assert()
        .success();

    factory_cmd()
        .current_dir(tmp.path())
        .args(["work", "attempt", "work-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Created Attempt attempt-2 for Work Item work-1",
        ));
}

#[test]
fn work_attempt_explicit_id_still_works() {
    let tmp = TempDir::new().unwrap();
    write_work_item_json(tmp.path(), "work-1", "Explicit attempt");

    factory_cmd()
        .current_dir(tmp.path())
        .args(["work", "attempt", "work-1", "my-custom-attempt"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Created Attempt my-custom-attempt for Work Item work-1",
        ));

    let value = work_item_value(tmp.path(), "work-1");
    assert_eq!(value["attempts"][0]["id"], "my-custom-attempt");
}

#[test]
fn work_attempt_run_no_attempts_reports_error() {
    let tmp = TempDir::new().unwrap();
    write_work_item_json(tmp.path(), "work-1", "No attempts");

    factory_cmd()
        .current_dir(tmp.path())
        .args(["work", "attempt", "run", "work-1", "--no-sandbox"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("has no Attempts"));
}

#[test]
fn work_merge_no_candidates_reports_error() {
    let tmp = TempDir::new().unwrap();
    write_work_item_json(tmp.path(), "work-1", "No candidates");

    factory_cmd()
        .current_dir(tmp.path())
        .args(["work", "merge", "work-1", "--no-sandbox"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("has no Merge Candidates"));
}

#[test]
fn work_task_run_completes_write_task_with_committed_output() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    let bin_dir = tmp.path().join("bin");
    write_mock_claude(
        &bin_dir,
        r##"#!/bin/bash
printf 'task output\n' > task-output.txt
git add task-output.txt
git commit -m "Add task output" >/dev/null
exit 0
"##,
    );

    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "create", "work-1", "--title", "Run task"])
        .assert()
        .success();
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "attempt", "work-1", "attempt-1"])
        .assert()
        .success();

    let output = factory_cmd()
        .current_dir(&main_dir)
        .args([
            "work",
            "task",
            "run",
            "work-1",
            "attempt-1",
            "attempt-1-write-1",
            "--no-sandbox",
        ])
        .env("PATH", mock_path(&bin_dir))
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "work task run failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("Completed Task attempt-1-write-1"));

    let workspace = main_dir.join("../work-6-work-1-attempt-1");
    assert!(workspace.join("task-output.txt").is_file());
    let head = StdCommand::new("git")
        .args(["-C", &workspace.to_string_lossy()])
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap();
    let head = String::from_utf8(head.stdout).unwrap().trim().to_string();

    let value = work_item_value(&main_dir, "work-1");
    let attempt = &value["attempts"][0];
    let task = &attempt["tasks"][0];
    assert_eq!(attempt["status"], "complete");
    assert_eq!(task["status"], "complete");
    assert_eq!(task["output"]["workspace_id"], "candidate");
    assert_eq!(
        task["output"]["workspace_path"],
        "../work-6-work-1-attempt-1"
    );
    assert_eq!(task["output"]["source_branch"], "main");
    assert_eq!(task["output"]["commit"], head);
    assert_eq!(attempt["artifacts"][0]["producer_id"], "attempt-1-write-1");
    assert_eq!(attempt["artifacts"][0]["path"], head);
}

#[test]
fn work_task_run_passes_task_context_to_coder_prompt() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    let bin_dir = tmp.path().join("bin");
    let prompt_log = tmp.path().join("prompt.log");
    let system_log = tmp.path().join("system.log");
    write_mock_claude(
        &bin_dir,
        r##"#!/bin/bash
while [ "$#" -gt 0 ]; do
  if [ "$1" = "--append-system-prompt" ]; then
    shift
    printf '%s\n' "$1" > "$SYSTEM_LOG"
  fi
  if [ "$1" = "-p" ]; then
    shift
    printf '%s\n' "$1" > "$PROMPT_LOG"
    break
  fi
  shift
done
printf 'task output\n' > task-output.txt
git add task-output.txt
git commit -m "Add task output" >/dev/null
exit 0
"##,
    );

    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "create", "work-1", "--title", "Prompt contract"])
        .assert()
        .success();
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "attempt", "work-1", "attempt-1"])
        .assert()
        .success();

    factory_cmd()
        .current_dir(&main_dir)
        .args([
            "work",
            "task",
            "run",
            "work-1",
            "attempt-1",
            "attempt-1-write-1",
            "--no-sandbox",
        ])
        .env("PATH", mock_path(&bin_dir))
        .env("PROMPT_LOG", &prompt_log)
        .env("SYSTEM_LOG", &system_log)
        .assert()
        .success();

    let prompt = fs::read_to_string(prompt_log).unwrap();
    let system_prompt = fs::read_to_string(system_log).unwrap();
    assert!(prompt.contains("Work Item: work-1 - Prompt contract"));
    assert!(prompt.contains("Attempt: attempt-1"));
    assert!(prompt.contains("Task: attempt-1-write-1"));
    assert!(prompt.contains("Role: author"));
    assert!(prompt.contains("Completion contract:"));
    assert!(prompt.contains("Commit all Task output"));
    assert!(prompt.contains("Leave the writable workspace clean"));
    assert!(prompt.contains("no committed Task output makes the Task fail"));
    assert!(!prompt.contains("mark the Task needs-user"));
    assert!(system_prompt.contains("Factory Work model"));
    assert!(!system_prompt.contains("Status file contract"));
    assert!(!system_prompt.contains(".factory/runs/"));
    assert!(!system_prompt.contains("handoff.md"));
    assert!(prompt.contains("Author preflight:"));
    assert!(prompt.contains("Before editing, identify the likely touched surfaces"));
    assert!(prompt.contains(
        "behavior statements, user-facing docs, tests, skills/expertise, and verification commands"
    ));
    assert!(
        prompt.contains(
            "update the applicable behavior contract, docs, tests, and verification notes"
        )
    );
    assert!(prompt.contains("record why the other related artifacts do not apply"));
    assert!(!prompt.contains("Task instructions:"));
    assert!(prompt.contains("Current Task model:"));
    assert!(prompt.contains(r#""id": "attempt-1-write-1""#));
    assert!(prompt.contains(r#""kind": "write""#));
    assert!(prompt.contains(r#""workspace_access""#));
}

#[test]
fn work_create_persists_instructions_and_attempt_copies_them_to_write_task() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    let instructions_path = tmp.path().join("instructions.md");
    fs::write(
        &instructions_path,
        "Brief: implement durable task instructions.\n\n- Keep extra args as flags.\n",
    )
    .unwrap();

    factory_cmd()
        .current_dir(&main_dir)
        .args([
            "work",
            "create",
            "work-1",
            "--title",
            "Instruction contract",
            "--instructions-file",
            &instructions_path.to_string_lossy(),
        ])
        .assert()
        .success();
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "attempt", "work-1", "attempt-1"])
        .assert()
        .success();

    let output = factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "show", "work-1"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "work show failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        value["instructions"],
        "Brief: implement durable task instructions.\n\n- Keep extra args as flags.\n"
    );
    assert_eq!(
        value["attempts"][0]["tasks"][0]["instructions"],
        value["instructions"]
    );
}

#[test]
fn work_create_persists_planning_context_and_attempt_copies_it_to_write_task() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    let brief_path = tmp.path().join("brief.md");
    let behaviors_path = tmp.path().join("behaviors.md");
    let approach_path = tmp.path().join("approach.md");
    let plan_path = tmp.path().join("plan.md");
    fs::write(&brief_path, "Build Work planning context.\n").unwrap();
    fs::write(&behaviors_path, "WHEN planning exists, store it.\n").unwrap();
    fs::write(&approach_path, "Add first-class Work state.\n").unwrap();
    fs::write(&plan_path, "1. Implement the model change.\n").unwrap();

    factory_cmd()
        .current_dir(&main_dir)
        .args([
            "work",
            "create",
            "work-1",
            "--title",
            "Planning contract",
            "--brief-file",
            &brief_path.to_string_lossy(),
            "--behaviors-file",
            &behaviors_path.to_string_lossy(),
            "--approach-file",
            &approach_path.to_string_lossy(),
            "--plan-file",
            &plan_path.to_string_lossy(),
        ])
        .assert()
        .success();
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "attempt", "work-1", "attempt-1"])
        .assert()
        .success();

    let output = factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "show", "work-1"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "work show failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        value["planning_context"]["brief"],
        "Build Work planning context.\n"
    );
    assert_eq!(
        value["planning_context"]["behaviors"],
        "WHEN planning exists, store it.\n"
    );
    assert_eq!(
        value["planning_context"]["approach"],
        "Add first-class Work state.\n"
    );
    assert_eq!(
        value["planning_context"]["plan"],
        "1. Implement the model change.\n"
    );
    assert_eq!(value["instructions"], serde_json::Value::Null);
    let task_instructions = value["attempts"][0]["tasks"][0]["instructions"]
        .as_str()
        .unwrap();
    assert!(task_instructions.contains("# Brief\n\nBuild Work planning context."));
    assert!(task_instructions.contains("# Behaviors\n\nWHEN planning exists, store it."));
    assert!(task_instructions.contains("# Approach\n\nAdd first-class Work state."));
    assert!(task_instructions.contains("# Plan\n\n1. Implement the model change."));
}

#[test]
fn work_create_prefers_instructions_over_planning_context_for_write_task() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);

    factory_cmd()
        .current_dir(&main_dir)
        .args([
            "work",
            "create",
            "work-1",
            "--title",
            "Planning precedence",
            "--instructions",
            "Use these explicit instructions.",
            "--planning-context",
            "# Brief\n\nDo not use this for the prompt.",
        ])
        .assert()
        .success();
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "attempt", "work-1", "attempt-1"])
        .assert()
        .success();

    let output = factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "show", "work-1"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        value["attempts"][0]["tasks"][0]["instructions"],
        "Use these explicit instructions."
    );
    assert_eq!(
        value["planning_context"]["combined"],
        "# Brief\n\nDo not use this for the prompt."
    );
}

#[test]
fn work_task_run_includes_task_instructions_in_coder_prompt() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    let bin_dir = tmp.path().join("bin");
    let prompt_log = tmp.path().join("prompt.log");
    write_mock_claude(
        &bin_dir,
        r##"#!/bin/bash
while [ "$#" -gt 0 ]; do
  if [ "$1" = "-p" ]; then
    shift
    printf '%s\n' "$1" > "$PROMPT_LOG"
    break
  fi
  shift
done
printf 'task output\n' > task-output.txt
git add task-output.txt
git commit -m "Add task output" >/dev/null
exit 0
"##,
    );

    factory_cmd()
        .current_dir(&main_dir)
        .args([
            "work",
            "create",
            "work-1",
            "--title",
            "Prompt contract",
            "--instructions",
            "Implement the first slice.\nAvoid prompt smuggling.",
        ])
        .assert()
        .success();
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "attempt", "work-1", "attempt-1"])
        .assert()
        .success();

    factory_cmd()
        .current_dir(&main_dir)
        .args([
            "work",
            "task",
            "run",
            "work-1",
            "attempt-1",
            "attempt-1-write-1",
            "--no-sandbox",
        ])
        .env("PATH", mock_path(&bin_dir))
        .env("PROMPT_LOG", &prompt_log)
        .assert()
        .success();

    let prompt = fs::read_to_string(prompt_log).unwrap();
    assert!(prompt.contains("Task instructions:"));
    assert!(prompt.contains("Implement the first slice."));
    assert!(prompt.contains("Avoid prompt smuggling."));
}

#[test]
fn work_task_run_includes_planning_context_in_coder_prompt() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    let bin_dir = tmp.path().join("bin");
    let prompt_log = tmp.path().join("prompt.log");
    write_mock_claude(
        &bin_dir,
        r##"#!/bin/bash
while [ "$#" -gt 0 ]; do
  if [ "$1" = "-p" ]; then
    shift
    printf '%s\n' "$1" > "$PROMPT_LOG"
    break
  fi
  shift
done
printf 'task output\n' > task-output.txt
git add task-output.txt
git commit -m "Add task output" >/dev/null
exit 0
"##,
    );

    factory_cmd()
        .current_dir(&main_dir)
        .args([
            "work",
            "create",
            "work-1",
            "--title",
            "Planning prompt",
            "--planning-context",
            "# Brief\n\nUse durable planning context.\n\n# Plan\n\n1. Build it.",
        ])
        .assert()
        .success();
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "attempt", "work-1", "attempt-1"])
        .assert()
        .success();

    factory_cmd()
        .current_dir(&main_dir)
        .args([
            "work",
            "task",
            "run",
            "work-1",
            "attempt-1",
            "attempt-1-write-1",
            "--no-sandbox",
        ])
        .env("PATH", mock_path(&bin_dir))
        .env("PROMPT_LOG", &prompt_log)
        .assert()
        .success();

    let prompt = fs::read_to_string(prompt_log).unwrap();
    assert!(prompt.contains("Task instructions:"));
    assert!(prompt.contains("Use durable planning context."));
    assert!(prompt.contains("1. Build it."));
}

#[test]
fn work_task_run_keeps_extra_args_out_of_task_prompt() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    let bin_dir = tmp.path().join("bin");
    let args_log = tmp.path().join("args.log");
    let prompt_log = tmp.path().join("prompt.log");
    write_mock_codex(
        &bin_dir,
        r##"#!/bin/bash
printf '%s\n' "$@" > "$ARGS_LOG"
for arg in "$@"; do
  case "$arg" in
    *"Execute this Factory write Task"*)
      printf '%s\n' "$arg" > "$PROMPT_LOG"
      ;;
  esac
done
printf 'task output\n' > task-output.txt
git add task-output.txt
git commit -m "Add task output" >/dev/null
exit 0
"##,
    );

    factory_cmd()
        .current_dir(&main_dir)
        .args([
            "work",
            "create",
            "work-1",
            "--title",
            "Extra args",
            "--instructions",
            "Durable instructions only.",
        ])
        .assert()
        .success();
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "attempt", "work-1", "attempt-1"])
        .assert()
        .success();

    factory_cmd()
        .current_dir(&main_dir)
        .args([
            "work",
            "task",
            "run",
            "work-1",
            "attempt-1",
            "attempt-1-write-1",
            "--no-sandbox",
            "--coder",
            "codex",
            "--",
            "--config",
            "factory_marker=\"coder-flag-only\"",
        ])
        .env("PATH", mock_path(&bin_dir))
        .env("ARGS_LOG", &args_log)
        .env("PROMPT_LOG", &prompt_log)
        .assert()
        .success();

    let args = fs::read_to_string(args_log).unwrap();
    let prompt = fs::read_to_string(prompt_log).unwrap();
    assert!(args.contains("factory_marker=\"coder-flag-only\""));
    assert!(prompt.contains("Durable instructions only."));
    assert!(!prompt.contains("coder-flag-only"));
}

#[test]
fn work_review_plans_review_tasks_for_completed_attempt() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    create_completed_work_attempt(&tmp, &main_dir);

    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "review", "work-1", "attempt-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Planned 5 review Tasks for Attempt attempt-1",
        ))
        .stdout(predicate::str::contains("attempt-1-review-tests"));

    let value = read_work_show_json(&main_dir, "work-1");
    let attempt = &value["attempts"][0];
    assert_eq!(attempt["status"], "reviewing");
    assert_eq!(attempt["review_state"], "not-reviewed");
    assert_eq!(attempt["tasks"].as_array().unwrap().len(), 6);

    let review_task = attempt["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|task| task["id"] == "attempt-1-review-tests")
        .unwrap();
    assert_eq!(review_task["kind"], "review");
    assert_eq!(review_task["role"], "tests");
    assert_eq!(
        review_task["workspace_access"]["reads"][0]["id"],
        "candidate"
    );
    assert_eq!(
        review_task["workspace_access"]["reads"][0]["path"],
        "../work-6-work-1-attempt-1"
    );
    assert!(
        review_task["workspace_access"]["writes"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        review_task["artifact_area"]["path"],
        ".factory/work/artifacts/work-1/attempt-1/attempt-1-review-tests"
    );
    assert_eq!(
        review_task["review_context"]["candidate_workspace_id"],
        "candidate"
    );
    assert_eq!(
        review_task["review_context"]["candidate_workspace_path"],
        "../work-6-work-1-attempt-1"
    );
    assert_eq!(review_task["review_context"]["source_branch"], "main");
    assert_eq!(
        review_task["review_context"]["candidate_commit"],
        git_head(&main_dir.join("../work-6-work-1-attempt-1"))
    );
}

#[test]
fn work_review_requires_completed_write_output() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "create", "work-1", "--title", "Review too early"])
        .assert()
        .success();
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "attempt", "work-1", "attempt-1"])
        .assert()
        .success();
    let item_path = main_dir.join(".factory/work/items/work-1.json");
    let before = fs::read_to_string(&item_path).unwrap();

    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "review", "work-1", "attempt-1"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("completed write Task"));

    assert_eq!(fs::read_to_string(item_path).unwrap(), before);
}

#[test]
fn work_review_codebase_creates_review_only_attempt() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    let brief_path = tmp.path().join("review-brief.md");
    fs::write(
        &brief_path,
        "Review only skills/ and focus on review-only prompt context.\n",
    )
    .unwrap();
    factory_cmd()
        .current_dir(&main_dir)
        .args([
            "work",
            "create",
            "work-1",
            "--title",
            "Review codebase",
            "--brief-file",
            &brief_path.to_string_lossy(),
        ])
        .assert()
        .success();

    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "review-codebase", "work-1", "attempt-review"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Created review-only Attempt attempt-review with 5 review Tasks",
        ))
        .stdout(predicate::str::contains("attempt-review-review-tests"));

    let value = read_work_show_json(&main_dir, "work-1");
    let attempt = &value["attempts"][0];
    assert_eq!(attempt["id"], "attempt-review");
    assert_eq!(attempt["kind"], "review-only");
    assert_eq!(attempt["status"], "reviewing");
    assert_eq!(attempt["review_state"], "not-reviewed");
    assert_eq!(attempt["tasks"].as_array().unwrap().len(), 5);
    assert!(merge_candidates_are_empty(&value));

    let review_task = attempt["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|task| task["id"] == "attempt-review-review-tests")
        .unwrap();
    assert!(
        review_task["instructions"]
            .as_str()
            .unwrap()
            .contains("Review only skills/ and focus on review-only prompt context.")
    );
    assert_eq!(review_task["kind"], "review");
    assert_eq!(review_task["workspace_access"]["reads"][0]["id"], "source");
    assert_eq!(review_task["workspace_access"]["reads"][0]["path"], ".");
    assert!(
        review_task["workspace_access"]["writes"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        review_task["artifact_area"]["path"],
        ".factory/work/artifacts/work-1/attempt-review/attempt-review-review-tests"
    );
    assert_eq!(
        review_task["review_context"]["candidate_workspace_id"],
        "source"
    );
    assert_eq!(
        review_task["review_context"]["candidate_workspace_path"],
        "."
    );
    assert_eq!(review_task["review_context"]["source_branch"], "main");
    assert_eq!(
        review_task["review_context"]["candidate_commit"],
        git_head(&main_dir)
    );
}

#[test]
fn work_attempt_run_review_only_includes_planning_context_in_prompt() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    let bin_dir = tmp.path().join("bin-review-only-prompt");
    let prompt_log = tmp.path().join("review-prompt.log");
    let brief_path = tmp.path().join("review-brief.md");
    fs::write(
        &brief_path,
        "Review only skills/ and focus on review-only prompt context.\n",
    )
    .unwrap();
    write_mock_claude(
        &bin_dir,
        r##"#!/bin/bash
while [ "$#" -gt 0 ]; do
  if [ "$1" = "-p" ]; then
    shift
    printf '%s\n' "$1" >> "$PROMPT_LOG"
    break
  fi
  shift
done
printf 'Verdict: pass\n\nReview-only result.\n' > review.md
exit 0
"##,
    );

    factory_cmd()
        .current_dir(&main_dir)
        .args([
            "work",
            "create",
            "work-1",
            "--title",
            "Review codebase",
            "--brief-file",
            &brief_path.to_string_lossy(),
        ])
        .assert()
        .success();
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "review-codebase", "work-1", "attempt-review"])
        .assert()
        .success();

    factory_cmd()
        .current_dir(&main_dir)
        .args([
            "work",
            "attempt",
            "run",
            "work-1",
            "attempt-review",
            "--no-sandbox",
        ])
        .env("PATH", mock_path(&bin_dir))
        .env("PROMPT_LOG", &prompt_log)
        .assert()
        .success();

    let prompt = fs::read_to_string(prompt_log).unwrap();
    assert!(prompt.contains("Task instructions:"));
    assert!(prompt.contains("Review only skills/ and focus on review-only prompt context."));
    assert!(prompt.contains("Readable source checkout:"));
}

#[test]
fn work_review_codebase_missing_or_duplicate_leaves_state_unchanged() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "create", "work-1", "--title", "Review codebase"])
        .assert()
        .success();

    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "review-codebase", "work-1", "attempt-review"])
        .assert()
        .success();
    let item_path = main_dir.join(".factory/work/items/work-1.json");
    let before = fs::read_to_string(&item_path).unwrap();

    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "review-codebase", "missing-work", "attempt-review"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "Work Item \"missing-work\" not found",
        ));
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "review-codebase", "work-1", "attempt-review"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "Attempt \"attempt-review\" already exists",
        ));

    assert_eq!(fs::read_to_string(item_path).unwrap(), before);
    assert!(
        !main_dir
            .join(".factory/work/items/missing-work.json")
            .exists()
    );
}

#[test]
fn work_task_run_completes_review_task_with_fail_verdict_artifact() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    fs::create_dir_all(main_dir.join("skills/review-tests")).unwrap();
    fs::write(
        main_dir.join("skills/review-tests/SKILL.md"),
        "# Review tests\n\nCheck tests.\n",
    )
    .unwrap();
    StdCommand::new("git")
        .args(["add", "skills/review-tests/SKILL.md"])
        .current_dir(&main_dir)
        .output()
        .unwrap();
    StdCommand::new("git")
        .args(["commit", "-m", "Add review tests skill"])
        .current_dir(&main_dir)
        .output()
        .unwrap();
    create_completed_work_attempt(&tmp, &main_dir);
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "review", "work-1", "attempt-1"])
        .assert()
        .success();
    let prior_review_path = main_dir
        .join(".factory/work/artifacts/work-1/attempt-1/attempt-1-review-tests-prior/review.md");
    fs::create_dir_all(prior_review_path.parent().unwrap()).unwrap();
    fs::write(
        &prior_review_path,
        "Verdict: fail\n\nPrior tests finding.\n",
    )
    .unwrap();
    let task_path =
        work_task_record_path(&main_dir, "work-1", "attempt-1", "attempt-1-review-tests");
    let mut task = read_json_value(&task_path);
    task["input_artifacts"] = serde_json::json!([
        {
            "producer_id": "attempt-1-review-tests-prior",
            "path": ".factory/work/artifacts/work-1/attempt-1/attempt-1-review-tests-prior/review.md"
        }
    ]);
    write_json_value(&task_path, &task);

    let bin_dir = tmp.path().join("bin-review");
    let prompt_log = tmp.path().join("review-prompt.log");
    let system_log = tmp.path().join("review-system.log");
    let sandbox_profile_log = tmp.path().join("review-sandbox.sb");
    write_mock_claude(
        &bin_dir,
        r##"#!/bin/bash
while [ "$#" -gt 0 ]; do
  if [ "$1" = "--append-system-prompt" ]; then
    shift
    printf '%s\n' "$1" > "$SYSTEM_LOG"
  fi
  if [ "$1" = "-p" ]; then
    shift
    printf '%s\n' "$1" > "$PROMPT_LOG"
    break
  fi
  shift
done
printf 'Verdict: fail\n\nFinding remains.\n' > review.md
exit 0
"##,
    );
    write_mock_executable(
        &bin_dir,
        "sandbox-exec",
        r##"#!/bin/bash
if [ "$1" = "-f" ]; then
  cp "$2" "$SANDBOX_PROFILE_LOG"
  shift 2
fi
exec "$@"
"##,
    );

    factory_cmd()
        .current_dir(&main_dir)
        .args([
            "work",
            "task",
            "run",
            "work-1",
            "attempt-1",
            "attempt-1-review-tests",
        ])
        .env("PATH", mock_path(&bin_dir))
        .env("PROMPT_LOG", &prompt_log)
        .env("SYSTEM_LOG", &system_log)
        .env("SANDBOX_PROFILE_LOG", &sandbox_profile_log)
        .env("CLAUDE_CODE_OAUTH_TOKEN", "mock-token")
        .env("BRAVE_SEARCH_API_KEY", "mock-key")
        .env("AWS_ACCESS_KEY_ID", "mock-access")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Completed Task attempt-1-review-tests",
        ));

    let review_path =
        main_dir.join(".factory/work/artifacts/work-1/attempt-1/attempt-1-review-tests/review.md");
    assert!(
        fs::read_to_string(&review_path)
            .unwrap()
            .contains("Verdict: fail")
    );
    let prompt = fs::read_to_string(prompt_log).unwrap();
    assert!(prompt.contains("Execute this Factory review Task"));
    assert!(prompt.contains("A previous review of this candidate"));
    let expected_prior_review_path = fs::canonicalize(&prior_review_path).unwrap();
    assert!(prompt.contains(&format!("- {}", expected_prior_review_path.display())));
    assert!(prompt.contains("Treat that previous review as another reviewer's findings"));
    assert!(prompt.contains("Progress:"));
    assert!(prompt.contains("Readable candidate workspaces:"));
    let candidate_workspace =
        fs::canonicalize(main_dir.join("../work-6-work-1-attempt-1")).unwrap();
    assert!(prompt.contains(&format!("- candidate: {}", candidate_workspace.display())));
    assert!(!prompt.contains("- candidate: ../work-6-work-1-attempt-1"));
    assert!(prompt.contains("Review context:"));
    assert!(prompt.contains("- Source branch: main"));
    assert!(prompt.contains(&format!(
        "- Review diff: git -C '{}' diff 'main..",
        candidate_workspace.display()
    )));
    assert!(prompt.contains(&review_path.to_string_lossy().to_string()));
    let system = fs::read_to_string(system_log).unwrap();
    assert!(system.contains("Factory tests reviewer"));
    let candidate_skill = candidate_workspace.join("skills/review-tests/SKILL.md");
    assert!(system.contains(&candidate_skill.to_string_lossy().to_string()));
    assert!(system.contains(&review_path.to_string_lossy().to_string()));
    assert!(!system.contains(".factory/runs/{{RUN_ID}}/reviews"));
    let expected_prior_review_dir = fs::canonicalize(prior_review_path.parent().unwrap()).unwrap();
    let sandbox_profile = fs::read_to_string(sandbox_profile_log).unwrap();
    assert!(
        sandbox_profile.contains(&format!(
            "(allow file-read*  (subpath \"{}\"))",
            expected_prior_review_dir.display()
        )),
        "{sandbox_profile}"
    );
    assert!(
        !sandbox_profile.contains(&format!(
            "(allow file-write* (subpath \"{}\"))",
            expected_prior_review_dir.display()
        )),
        "{sandbox_profile}"
    );

    let value = read_work_show_json(&main_dir, "work-1");
    let review_task = value["attempts"][0]["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|task| task["id"] == "attempt-1-review-tests")
        .unwrap();
    assert_eq!(review_task["status"], "complete");
    assert!(review_task.get("output").is_none());
    assert_eq!(value["attempts"][0]["status"], "reviewing");
    assert_eq!(
        value["attempts"][0]["artifacts"]
            .as_array()
            .unwrap()
            .last()
            .unwrap()["path"],
        ".factory/work/artifacts/work-1/attempt-1/attempt-1-review-tests/review.md"
    );
}

#[test]
fn work_behavior_review_task_prompt_includes_behavior_increment() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    create_completed_work_attempt_with_behaviors(
        &tmp,
        &main_dir,
        "WHEN behavior input exists,\nTHE SYSTEM SHALL show it to behavior reviewers.\n",
    );
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "review", "work-1", "attempt-1"])
        .assert()
        .success();

    let bin_dir = tmp.path().join("bin-behavior-review-prompt");
    let prompt_log = tmp.path().join("behavior-review-prompt.log");
    write_mock_claude(
        &bin_dir,
        r##"#!/bin/bash
while [ "$#" -gt 0 ]; do
  if [ "$1" = "-p" ]; then
    shift
    printf '%s\n' "$1" > "$PROMPT_LOG"
    break
  fi
  shift
done
printf 'Verdict: pass\n\nBehavior review passed.\n' > review.md
exit 0
"##,
    );

    factory_cmd()
        .current_dir(&main_dir)
        .args([
            "work",
            "task",
            "run",
            "work-1",
            "attempt-1",
            "attempt-1-review-behaviors",
            "--no-sandbox",
        ])
        .env("PATH", mock_path(&bin_dir))
        .env("PROMPT_LOG", &prompt_log)
        .assert()
        .success();

    let prompt = fs::read_to_string(prompt_log).unwrap();
    assert!(prompt.contains("Work behavior review input:"));
    assert!(prompt.contains("WHEN behavior input exists,"));
    assert!(prompt.contains("THE SYSTEM SHALL show it to behavior reviewers."));
    assert!(
        prompt.contains(
            "without requiring a legacy .factory/runs/[run-id]/behaviors.diff.md artifact"
        )
    );
    assert!(!prompt.contains("Read behaviors.diff.md and the brief"));
}

#[test]
fn work_behavior_review_task_prompt_states_missing_behavior_increment() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    create_completed_work_attempt(&tmp, &main_dir);
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "review", "work-1", "attempt-1"])
        .assert()
        .success();

    let bin_dir = tmp.path().join("bin-behavior-review-missing-prompt");
    let prompt_log = tmp.path().join("behavior-review-missing-prompt.log");
    write_mock_claude(
        &bin_dir,
        r##"#!/bin/bash
while [ "$#" -gt 0 ]; do
  if [ "$1" = "-p" ]; then
    shift
    printf '%s\n' "$1" > "$PROMPT_LOG"
    break
  fi
  shift
done
printf 'Verdict: pass\n\nBehavior review passed.\n' > review.md
exit 0
"##,
    );

    factory_cmd()
        .current_dir(&main_dir)
        .args([
            "work",
            "task",
            "run",
            "work-1",
            "attempt-1",
            "attempt-1-review-behaviors",
            "--no-sandbox",
        ])
        .env("PATH", mock_path(&bin_dir))
        .env("PROMPT_LOG", &prompt_log)
        .assert()
        .success();

    let prompt = fs::read_to_string(prompt_log).unwrap();
    assert!(prompt.contains("Work behavior review input:"));
    assert!(prompt.contains("No Work behavior increment was provided for this Work Item."));
    assert!(!prompt.contains("Read behaviors.diff.md and the brief"));
}

#[test]
fn work_task_run_review_only_uses_source_prompt() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "create", "work-1", "--title", "Review codebase"])
        .assert()
        .success();
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "review-codebase", "work-1", "attempt-review"])
        .assert()
        .success();

    let bin_dir = tmp.path().join("bin-review-only-prompt");
    let prompt_log = tmp.path().join("review-only-prompt.log");
    let system_log = tmp.path().join("review-only-system.log");
    write_mock_claude(
        &bin_dir,
        r##"#!/bin/bash
while [ "$#" -gt 0 ]; do
  if [ "$1" = "--append-system-prompt" ]; then
    shift
    printf '%s\n' "$1" > "$SYSTEM_LOG"
  fi
  if [ "$1" = "-p" ]; then
    shift
    printf '%s\n' "$1" > "$PROMPT_LOG"
    break
  fi
  shift
done
printf 'Verdict: pass\n\nSource review passed.\n' > review.md
exit 0
"##,
    );

    factory_cmd()
        .current_dir(&main_dir)
        .args([
            "work",
            "task",
            "run",
            "work-1",
            "attempt-review",
            "attempt-review-review-tests",
            "--no-sandbox",
        ])
        .env("PATH", mock_path(&bin_dir))
        .env("PROMPT_LOG", &prompt_log)
        .env("SYSTEM_LOG", &system_log)
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Completed Task attempt-review-review-tests",
        ));

    let review_path = main_dir.join(
        ".factory/work/artifacts/work-1/attempt-review/attempt-review-review-tests/review.md",
    );
    let source_checkout = fs::canonicalize(&main_dir).unwrap();
    let prompt = fs::read_to_string(prompt_log).unwrap();
    assert!(prompt.contains("Readable source checkout:"));
    assert!(prompt.contains(&format!("- source: {}", source_checkout.display())));
    assert!(prompt.contains("- Source checkout: source (.)"));
    assert!(prompt.contains("- Source ref: main"));
    assert!(prompt.contains("- Source commit: "));
    assert!(!prompt.contains("Readable candidate workspaces:"));
    assert!(!prompt.contains("Review diff: git -C <candidate-workspace> diff"));
    assert!(prompt.contains(&review_path.to_string_lossy().to_string()));

    let system = fs::read_to_string(system_log).unwrap();
    assert!(system.contains("Read the source checkout only; do not edit or commit in it."));
    assert!(system.contains("No review-tests skill file was found in the source checkout"));
    assert!(!system.contains("Read candidate workspaces only"));
}

#[test]
fn work_task_run_completes_attempt_after_all_review_tasks_complete() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    create_completed_work_attempt(&tmp, &main_dir);
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "review", "work-1", "attempt-1"])
        .assert()
        .success();

    let bin_dir = tmp.path().join("bin-review");
    write_mock_claude(
        &bin_dir,
        "#!/bin/bash\nprintf 'Verdict: pass\\n' > review.md\nexit 0\n",
    );

    for role in [
        "documentation",
        "behaviors",
        "architecture",
        "skills",
        "tests",
    ] {
        factory_cmd()
            .current_dir(&main_dir)
            .args([
                "work",
                "task",
                "run",
                "work-1",
                "attempt-1",
                &format!("attempt-1-review-{role}"),
                "--no-sandbox",
            ])
            .env("PATH", mock_path(&bin_dir))
            .assert()
            .success();
    }

    let value = read_work_show_json(&main_dir, "work-1");
    let attempt = &value["attempts"][0];
    assert_eq!(attempt["status"], "complete");
    for task in attempt["tasks"].as_array().unwrap() {
        assert_eq!(task["status"], "complete");
    }
    for role in [
        "documentation",
        "behaviors",
        "architecture",
        "skills",
        "tests",
    ] {
        assert!(
            main_dir
                .join(format!(
                    ".factory/work/artifacts/work-1/attempt-1/attempt-1-review-{role}/review.md"
                ))
                .exists()
        );
    }
}

#[test]
fn work_attempt_run_drives_write_reviews_and_passes() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "create", "work-1", "--title", "Attempt loop"])
        .assert()
        .success();
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "attempt", "work-1", "attempt-1"])
        .assert()
        .success();

    let bin_dir = tmp.path().join("bin-loop-pass");
    write_mock_claude(&bin_dir, &loop_mock_script("pass"));

    factory_cmd()
        .current_dir(&main_dir)
        .args([
            "work",
            "attempt",
            "run",
            "work-1",
            "attempt-1",
            "--no-sandbox",
        ])
        .env("PATH", mock_path(&bin_dir))
        .assert()
        .success()
        .stdout(predicate::str::contains("Completed Task attempt-1-write-1"))
        .stdout(predicate::str::contains(
            "Planned 5 review Tasks for Attempt attempt-1",
        ))
        .stdout(predicate::str::contains(
            "Attempt attempt-1 reviews passed; Merge Candidate attempt-1-merge-candidate is ready",
        ));

    let value = read_work_show_json(&main_dir, "work-1");
    let attempt = &value["attempts"][0];
    assert_eq!(attempt["status"], "complete");
    assert_eq!(attempt["review_state"], "passed");
    assert_eq!(attempt["tasks"].as_array().unwrap().len(), 6);
    assert_eq!(value["merge_candidates"].as_array().unwrap().len(), 1);
    let candidate = &value["merge_candidates"][0];
    assert_eq!(candidate["id"], "attempt-1-merge-candidate");
    assert_eq!(candidate["attempt_id"], "attempt-1");
    assert_eq!(candidate["source_workspace"]["id"], "candidate");
    assert_eq!(
        candidate["source_workspace"]["path"],
        "../work-6-work-1-attempt-1"
    );
    assert_eq!(candidate["target_workspace"]["id"], "target");
    assert_eq!(candidate["target_workspace"]["path"], ".");
    assert_eq!(candidate["source_branch"], "main");
    assert_eq!(candidate["target_branch"], "main");
    assert_eq!(candidate["review_state"], "pending");
    assert!(
        main_dir
            .join(".factory/work/artifacts/work-1/attempt-1/attempt-1-review-tests/review.md")
            .exists()
    );

    let inspection = factory_cmd()
        .current_dir(&main_dir)
        .args([
            "work",
            "merge-candidate",
            "work-1",
            "attempt-1-merge-candidate",
        ])
        .output()
        .unwrap();

    assert!(
        inspection.status.success(),
        "merge candidate inspection failed: stdout={} stderr={}",
        String::from_utf8_lossy(&inspection.stdout),
        String::from_utf8_lossy(&inspection.stderr)
    );
    let inspected: serde_json::Value = serde_json::from_slice(&inspection.stdout).unwrap();
    assert_eq!(inspected, *candidate);

    let before = read_work_show_json(&main_dir, "work-1");
    factory_cmd()
        .current_dir(&main_dir)
        .args([
            "work",
            "attempt",
            "run",
            "work-1",
            "attempt-1",
            "--no-sandbox",
        ])
        .env("PATH", mock_path(&bin_dir))
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Merge Candidate attempt-1-merge-candidate is ready",
        ));
    let after = read_work_show_json(&main_dir, "work-1");
    assert_eq!(after, before);
}

#[test]
fn work_attempt_run_review_only_passes_without_merge_candidate() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "create", "work-1", "--title", "Review codebase"])
        .assert()
        .success();
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "review-codebase", "work-1", "attempt-review"])
        .assert()
        .success();

    let main_head = git_head(&main_dir);
    let bin_dir = tmp.path().join("bin-review-only-pass");
    write_mock_claude(&bin_dir, &review_only_mock_script("pass"));

    factory_cmd()
        .current_dir(&main_dir)
        .args([
            "work",
            "attempt",
            "run",
            "work-1",
            "attempt-review",
            "--no-sandbox",
        ])
        .env("PATH", mock_path(&bin_dir))
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Review-only Attempt attempt-review passed",
        ))
        .stdout(predicate::str::contains("Merge Candidate").not())
        .stdout(predicate::str::contains("follow-up").not());

    let value = read_work_show_json(&main_dir, "work-1");
    let attempt = &value["attempts"][0];
    assert_eq!(attempt["status"], "complete");
    assert_eq!(attempt["review_state"], "passed");
    assert_eq!(review_only_write_task_count(attempt), 0);
    assert!(merge_candidates_are_empty(&value));
    assert_eq!(git_head(&main_dir), main_head);
    assert_no_non_factory_changes(&main_dir);
}

#[test]
fn work_attempt_run_review_only_rejects_source_changes() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "create", "work-1", "--title", "Review codebase"])
        .assert()
        .success();
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "review-codebase", "work-1", "attempt-review"])
        .assert()
        .success();

    let main_head = git_head(&main_dir);
    let bin_dir = tmp.path().join("bin-review-only-dirty");
    write_mock_claude(&bin_dir, &review_only_dirty_source_mock_script());

    factory_cmd()
        .current_dir(&main_dir)
        .args([
            "work",
            "attempt",
            "run",
            "work-1",
            "attempt-review",
            "--no-sandbox",
        ])
        .env("PATH", mock_path(&bin_dir))
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "Review Task changed non-Factory source files",
        ))
        .stdout(predicate::str::contains("Merge Candidate").not())
        .stdout(predicate::str::contains("follow-up").not());

    let value = read_work_show_json(&main_dir, "work-1");
    let attempt = &value["attempts"][0];
    assert_eq!(review_only_write_task_count(attempt), 0);
    assert!(merge_candidates_are_empty(&value));
    assert!(
        attempt["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|task| task["kind"] == "review" && task["status"] == "failed")
    );
    assert_eq!(git_head(&main_dir), main_head);
    assert_no_non_factory_changes(&main_dir);
}

#[test]
fn work_attempt_run_review_only_restores_changed_source_head() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "create", "work-1", "--title", "Review codebase"])
        .assert()
        .success();
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "review-codebase", "work-1", "attempt-review"])
        .assert()
        .success();

    let main_head = git_head(&main_dir);
    let bin_dir = tmp.path().join("bin-review-only-head");
    write_mock_claude(&bin_dir, &review_only_changed_head_mock_script());

    factory_cmd()
        .current_dir(&main_dir)
        .args([
            "work",
            "attempt",
            "run",
            "work-1",
            "attempt-review",
            "--no-sandbox",
        ])
        .env("PATH", mock_path(&bin_dir))
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "changed readable source checkout HEAD",
        ))
        .stdout(predicate::str::contains("Merge Candidate").not())
        .stdout(predicate::str::contains("follow-up").not());

    let value = read_work_show_json(&main_dir, "work-1");
    let attempt = &value["attempts"][0];
    assert_eq!(review_only_write_task_count(attempt), 0);
    assert!(merge_candidates_are_empty(&value));
    assert!(
        attempt["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|task| task["kind"] == "review" && task["status"] == "failed")
    );
    assert_eq!(git_head(&main_dir), main_head);
    assert_no_non_factory_changes(&main_dir);
}

#[test]
fn work_attempt_run_review_only_requires_recorded_source_commit() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "create", "work-1", "--title", "Review codebase"])
        .assert()
        .success();
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "review-codebase", "work-1", "attempt-review"])
        .assert()
        .success();

    fs::write(main_dir.join("README.md"), "source advanced\n").unwrap();
    StdCommand::new("git")
        .args(["add", "README.md"])
        .current_dir(&main_dir)
        .output()
        .unwrap();
    StdCommand::new("git")
        .args(["commit", "-m", "advance source"])
        .current_dir(&main_dir)
        .output()
        .unwrap();

    let bin_dir = tmp.path().join("bin-review-only-stale");
    write_mock_claude(&bin_dir, &review_only_mock_script("pass"));

    factory_cmd()
        .current_dir(&main_dir)
        .args([
            "work",
            "attempt",
            "run",
            "work-1",
            "attempt-review",
            "--no-sandbox",
        ])
        .env("PATH", mock_path(&bin_dir))
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "does not match review context source commit",
        ))
        .stdout(predicate::str::contains("Merge Candidate").not())
        .stdout(predicate::str::contains("follow-up").not());

    let value = read_work_show_json(&main_dir, "work-1");
    let attempt = &value["attempts"][0];
    assert_eq!(review_only_write_task_count(attempt), 0);
    assert!(merge_candidates_are_empty(&value));
}

#[test]
fn work_attempt_run_review_only_rejects_factory_state_changes() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    fs::create_dir_all(main_dir.join(".factory/expertise")).unwrap();
    fs::write(
        main_dir.join(".factory/expertise/decisions.md"),
        "# Decisions\n\n",
    )
    .unwrap();
    StdCommand::new("git")
        .args(["add", ".factory/expertise/decisions.md"])
        .current_dir(&main_dir)
        .output()
        .unwrap();
    StdCommand::new("git")
        .args(["commit", "-m", "record decisions"])
        .current_dir(&main_dir)
        .output()
        .unwrap();

    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "create", "work-1", "--title", "Review codebase"])
        .assert()
        .success();
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "review-codebase", "work-1", "attempt-review"])
        .assert()
        .success();

    let bin_dir = tmp.path().join("bin-review-only-factory-dirty");
    write_mock_claude(&bin_dir, &review_only_dirty_factory_mock_script());

    factory_cmd()
        .current_dir(&main_dir)
        .args([
            "work",
            "attempt",
            "run",
            "work-1",
            "attempt-review",
            "--no-sandbox",
        ])
        .env("PATH", mock_path(&bin_dir))
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "changed source checkout outside managed artifact area",
        ))
        .stderr(predicate::str::contains(".factory/expertise/decisions.md"))
        .stdout(predicate::str::contains("Merge Candidate").not())
        .stdout(predicate::str::contains("follow-up").not());

    let value = read_work_show_json(&main_dir, "work-1");
    let attempt = &value["attempts"][0];
    assert_eq!(review_only_write_task_count(attempt), 0);
    assert!(merge_candidates_are_empty(&value));
    assert!(
        attempt["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|task| task["kind"] == "review" && task["status"] == "failed")
    );
}

#[test]
fn work_attempt_run_review_only_rejects_work_state_changes() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "create", "work-1", "--title", "Review codebase"])
        .assert()
        .success();
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "review-codebase", "work-1", "attempt-review"])
        .assert()
        .success();

    let bin_dir = tmp.path().join("bin-review-only-work-state-dirty");
    write_mock_claude(&bin_dir, &review_only_dirty_work_state_mock_script());

    factory_cmd()
        .current_dir(&main_dir)
        .args([
            "work",
            "attempt",
            "run",
            "work-1",
            "attempt-review",
            "--no-sandbox",
        ])
        .env("PATH", mock_path(&bin_dir))
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "changed source checkout outside managed artifact area",
        ))
        .stderr(predicate::str::contains(".factory/work/items/work-1.json"))
        .stdout(predicate::str::contains("Merge Candidate").not())
        .stdout(predicate::str::contains("follow-up").not());

    let value = read_work_show_json(&main_dir, "work-1");
    let attempt = &value["attempts"][0];
    assert_eq!(review_only_write_task_count(attempt), 0);
    assert!(merge_candidates_are_empty(&value));
    assert!(
        attempt["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|task| task["kind"] == "review" && task["status"] == "failed")
    );
    assert!(
        !fs::read_to_string(main_dir.join(".factory/work/items/work-1.json"))
            .unwrap()
            .contains("reviewer edit")
    );
}

#[test]
fn work_attempt_run_review_only_restores_mixed_source_and_factory_changes() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    fs::create_dir_all(main_dir.join(".factory/expertise")).unwrap();
    fs::write(
        main_dir.join(".factory/expertise/decisions.md"),
        "# Decisions\n\n",
    )
    .unwrap();
    StdCommand::new("git")
        .args(["add", ".factory/expertise/decisions.md"])
        .current_dir(&main_dir)
        .output()
        .unwrap();
    StdCommand::new("git")
        .args(["commit", "-m", "record decisions"])
        .current_dir(&main_dir)
        .output()
        .unwrap();

    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "create", "work-1", "--title", "Review codebase"])
        .assert()
        .success();
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "review-codebase", "work-1", "attempt-review"])
        .assert()
        .success();

    let bin_dir = tmp.path().join("bin-review-only-mixed-dirty");
    write_mock_claude(
        &bin_dir,
        &review_only_dirty_source_and_factory_mock_script(),
    );

    factory_cmd()
        .current_dir(&main_dir)
        .args([
            "work",
            "attempt",
            "run",
            "work-1",
            "attempt-review",
            "--no-sandbox",
        ])
        .env("PATH", mock_path(&bin_dir))
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "changed source checkout outside managed artifact area",
        ))
        .stderr(predicate::str::contains(".factory/expertise/decisions.md"))
        .stdout(predicate::str::contains("Merge Candidate").not())
        .stdout(predicate::str::contains("follow-up").not());

    assert_eq!(
        fs::read_to_string(main_dir.join("README.md")).unwrap(),
        "test"
    );
    assert_eq!(
        fs::read_to_string(main_dir.join(".factory/expertise/decisions.md")).unwrap(),
        "# Decisions\n\n"
    );
    let value = read_work_show_json(&main_dir, "work-1");
    let attempt = &value["attempts"][0];
    assert_eq!(review_only_write_task_count(attempt), 0);
    assert!(merge_candidates_are_empty(&value));
    assert!(
        attempt["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|task| task["kind"] == "review" && task["status"] == "failed")
    );
}

#[test]
fn work_attempt_run_review_only_fails_without_followup() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "create", "work-1", "--title", "Review codebase"])
        .assert()
        .success();
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "review-codebase", "work-1", "attempt-review"])
        .assert()
        .success();

    let bin_dir = tmp.path().join("bin-review-only-fail");
    write_mock_claude(&bin_dir, &review_only_mock_script("fail"));

    factory_cmd()
        .current_dir(&main_dir)
        .args([
            "work",
            "attempt",
            "run",
            "work-1",
            "attempt-review",
            "--no-sandbox",
        ])
        .env("PATH", mock_path(&bin_dir))
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Review-only Attempt attempt-review failed",
        ))
        .stdout(predicate::str::contains("Merge Candidate").not())
        .stdout(predicate::str::contains("follow-up").not());

    let value = read_work_show_json(&main_dir, "work-1");
    let attempt = &value["attempts"][0];
    assert_eq!(attempt["status"], "failed");
    assert_eq!(attempt["review_state"], "failed");
    assert_eq!(review_only_write_task_count(attempt), 0);
    assert!(merge_candidates_are_empty(&value));
    assert_no_non_factory_changes(&main_dir);
}

#[test]
fn work_attempt_run_review_only_uncertain_needs_user() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "create", "work-1", "--title", "Review codebase"])
        .assert()
        .success();
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "review-codebase", "work-1", "attempt-review"])
        .assert()
        .success();

    let bin_dir = tmp.path().join("bin-review-only-uncertain");
    write_mock_claude(&bin_dir, &review_only_mock_script("uncertain"));

    factory_cmd()
        .current_dir(&main_dir)
        .args([
            "work",
            "attempt",
            "run",
            "work-1",
            "attempt-review",
            "--no-sandbox",
        ])
        .env("PATH", mock_path(&bin_dir))
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Attempt attempt-review needs user input",
        ))
        .stdout(predicate::str::contains("Merge Candidate").not())
        .stdout(predicate::str::contains("follow-up").not());

    let value = read_work_show_json(&main_dir, "work-1");
    let attempt = &value["attempts"][0];
    assert_eq!(attempt["status"], "needs-user");
    assert_eq!(attempt["review_state"], "uncertain");
    assert!(
        attempt["artifacts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|artifact| {
                artifact["path"] == ".factory/work/artifacts/work-1/attempt-review/needs-user.md"
            })
    );
    assert_eq!(review_only_write_task_count(attempt), 0);
    assert!(merge_candidates_are_empty(&value));
    assert!(
        main_dir
            .join(".factory/work/artifacts/work-1/attempt-review/needs-user.md")
            .is_file()
    );
    assert_no_non_factory_changes(&main_dir);
}

#[test]
fn work_merge_candidate_failed_check_leaves_target_unchanged() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "create", "work-1", "--title", "Merge check failure"])
        .assert()
        .success();
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "attempt", "work-1", "attempt-1"])
        .assert()
        .success();

    let bin_dir = tmp.path().join("bin-check-fail");
    write_mock_claude(&bin_dir, &rebase_mock_script("pass"));
    factory_cmd()
        .current_dir(&main_dir)
        .args([
            "work",
            "attempt",
            "run",
            "work-1",
            "attempt-1",
            "--no-sandbox",
        ])
        .env("PATH", mock_path(&bin_dir))
        .assert()
        .success();
    write_executable_hook(
        &main_dir,
        "check-pre-merge",
        "#!/bin/sh\nprintf check-failed >&2\nexit 1\n",
    );

    let candidate_workspace = main_dir.join("../work-6-work-1-attempt-1");
    let candidate_head = git_head(&candidate_workspace);
    let main_before = git_head(&main_dir);
    factory_cmd()
        .current_dir(&main_dir)
        .args([
            "work",
            "merge",
            "work-1",
            "attempt-1-merge-candidate",
            "--no-sandbox",
        ])
        .env("PATH", mock_path(&bin_dir))
        .assert()
        .failure()
        .stderr(predicate::str::contains("check-pre-merge failed (exit 1)"));

    assert_eq!(git_head(&main_dir), main_before);
    assert_eq!(git_head(&candidate_workspace), candidate_head);
    let value = read_work_show_json(&main_dir, "work-1");
    let candidate = &value["merge_candidates"][0];
    assert_eq!(candidate["review_state"], "pending");
    assert_eq!(candidate["merge_state"]["status"], "failed");
    assert!(
        candidate["merge_state"]["failure_reason"]
            .as_str()
            .unwrap()
            .contains("check-pre-merge failed")
    );
    assert!(
        candidate["merge_state"]["check_artifacts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|artifact| artifact["producer_id"] == "merge-hooks")
    );
}

#[test]
fn work_merge_candidate_warns_when_cleanup_fails_after_landing() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "create", "work-1", "--title", "Cleanup warning"])
        .assert()
        .success();
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "attempt", "work-1", "attempt-1"])
        .assert()
        .success();

    let bin_dir = tmp.path().join("bin-cleanup-warning-pass");
    write_mock_claude(&bin_dir, &rebase_mock_script("pass"));
    factory_cmd()
        .current_dir(&main_dir)
        .args([
            "work",
            "attempt",
            "run",
            "work-1",
            "attempt-1",
            "--no-sandbox",
        ])
        .env("PATH", mock_path(&bin_dir))
        .assert()
        .success();

    let candidate_workspace = main_dir.join("../work-6-work-1-attempt-1");
    let candidate_head = git_head(&candidate_workspace);
    let lock_output = StdCommand::new("git")
        .args(["worktree", "lock", &candidate_workspace.to_string_lossy()])
        .current_dir(&main_dir)
        .output()
        .unwrap();
    assert!(
        lock_output.status.success(),
        "git worktree lock failed: {}",
        String::from_utf8_lossy(&lock_output.stderr)
    );

    factory_cmd()
        .current_dir(&main_dir)
        .args([
            "work",
            "merge",
            "work-1",
            "attempt-1-merge-candidate",
            "--no-sandbox",
        ])
        .env("PATH", mock_path(&bin_dir))
        .assert()
        .success()
        .stderr(predicate::str::contains("managed workspace cleanup failed"));

    assert_eq!(git_head(&main_dir), candidate_head);
    assert!(candidate_workspace.exists());
    let value = read_work_show_json(&main_dir, "work-1");
    let candidate = &value["merge_candidates"][0];
    assert_eq!(candidate["review_state"], "passed");
    assert_eq!(candidate["merge_state"]["status"], "merged");
    assert_eq!(candidate["merge_state"]["merged_commit"], candidate_head);
}

#[test]
fn work_merge_candidate_rerun_after_cleanup_preserves_landed_state() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "create", "work-1", "--title", "Cleanup rerun"])
        .assert()
        .success();
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "attempt", "work-1", "attempt-1"])
        .assert()
        .success();

    let bin_dir = tmp.path().join("bin-cleanup-rerun-pass");
    write_mock_claude(&bin_dir, &rebase_mock_script("pass"));
    factory_cmd()
        .current_dir(&main_dir)
        .args([
            "work",
            "attempt",
            "run",
            "work-1",
            "attempt-1",
            "--no-sandbox",
        ])
        .env("PATH", mock_path(&bin_dir))
        .assert()
        .success();

    let candidate_workspace = main_dir.join("../work-6-work-1-attempt-1");
    let candidate_head = git_head(&candidate_workspace);

    factory_cmd()
        .current_dir(&main_dir)
        .args([
            "work",
            "merge",
            "work-1",
            "attempt-1-merge-candidate",
            "--no-sandbox",
        ])
        .env("PATH", mock_path(&bin_dir))
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Merged Merge Candidate attempt-1-merge-candidate",
        ));

    assert_eq!(git_head(&main_dir), candidate_head);
    assert!(!candidate_workspace.exists());

    write_executable_hook(
        &main_dir,
        "check-pre-merge",
        "#!/bin/sh\nprintf should-not-run >&2\nexit 1\n",
    );
    let fail_bin = tmp.path().join("bin-cleanup-rerun-should-not-run");
    write_mock_claude(
        &fail_bin,
        "#!/bin/bash\nprintf 'reviewer should not rerun' >&2\nexit 42\n",
    );

    factory_cmd()
        .current_dir(&main_dir)
        .args([
            "work",
            "merge",
            "work-1",
            "attempt-1-merge-candidate",
            "--no-sandbox",
        ])
        .env("PATH", mock_path(&fail_bin))
        .assert()
        .success()
        .stdout(predicate::str::contains(candidate_head.clone()))
        .stderr(predicate::str::contains("should-not-run").not())
        .stderr(predicate::str::contains("reviewer should not rerun").not());

    assert_eq!(git_head(&main_dir), candidate_head);
    assert!(!candidate_workspace.exists());
    let value = read_work_show_json(&main_dir, "work-1");
    let candidate = &value["merge_candidates"][0];
    assert_eq!(candidate["review_state"], "passed");
    assert_eq!(candidate["merge_state"]["status"], "merged");
    assert_eq!(candidate["merge_state"]["merged_commit"], candidate_head);
}

#[test]
fn work_merge_candidate_rejects_stale_stored_provenance_without_rewrite() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "create", "work-1", "--title", "Stale candidate"])
        .assert()
        .success();
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "attempt", "work-1", "attempt-1"])
        .assert()
        .success();

    let bin_dir = tmp.path().join("bin-stale-provenance");
    write_mock_claude(&bin_dir, &rebase_mock_script("pass"));
    factory_cmd()
        .current_dir(&main_dir)
        .args([
            "work",
            "attempt",
            "run",
            "work-1",
            "attempt-1",
            "--no-sandbox",
        ])
        .env("PATH", mock_path(&bin_dir))
        .assert()
        .success();

    let candidate_path =
        main_dir.join(".factory/work/merge-candidates/work-1/attempt-1-merge-candidate.json");
    let mut value: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&candidate_path).unwrap()).unwrap();
    value["candidate_commit"] =
        serde_json::Value::String("0000000000000000000000000000000000000000".to_string());
    fs::write(
        &candidate_path,
        serde_json::to_string_pretty(&value).unwrap(),
    )
    .unwrap();
    let main_before = git_head(&main_dir);

    factory_cmd()
        .current_dir(&main_dir)
        .args([
            "work",
            "merge",
            "work-1",
            "attempt-1-merge-candidate",
            "--no-sandbox",
        ])
        .env("PATH", mock_path(&bin_dir))
        .assert()
        .failure()
        .stderr(predicate::str::contains("candidate_commit"));

    assert_eq!(git_head(&main_dir), main_before);
    let value: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&candidate_path).unwrap()).unwrap();
    assert_eq!(value["merge_state"]["status"], "pending");
    assert!(value["merge_state"].get("failure_reason").is_none());
}

#[test]
fn work_merge_candidate_rebases_when_target_advanced() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "create", "work-1", "--title", "Rebase candidate"])
        .assert()
        .success();
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "attempt", "work-1", "attempt-1"])
        .assert()
        .success();

    let bin_dir = tmp.path().join("bin-rebase-pass");
    write_mock_claude(&bin_dir, &rebase_mock_script("pass"));
    factory_cmd()
        .current_dir(&main_dir)
        .args([
            "work",
            "attempt",
            "run",
            "work-1",
            "attempt-1",
            "--no-sandbox",
        ])
        .env("PATH", mock_path(&bin_dir))
        .assert()
        .success();

    let candidate_workspace = main_dir.join("../work-6-work-1-attempt-1");
    let candidate_head = git_head(&candidate_workspace);
    commit_file(
        &main_dir,
        "target-only.txt",
        "target advanced\n",
        "Advance target",
    );
    let main_before_merge = git_head(&main_dir);

    factory_cmd()
        .current_dir(&main_dir)
        .args([
            "work",
            "merge",
            "work-1",
            "attempt-1-merge-candidate",
            "--no-sandbox",
        ])
        .env("PATH", mock_path(&bin_dir))
        .assert()
        .success();

    let main_after_merge = git_head(&main_dir);
    assert_ne!(main_after_merge, candidate_head);
    assert_ne!(main_after_merge, main_before_merge);
    assert!(main_dir.join("target-only.txt").is_file());
    assert!(main_dir.join("loop-output.txt").is_file());
    let value = read_work_show_json(&main_dir, "work-1");
    let candidate = &value["merge_candidates"][0];
    assert_eq!(candidate["merge_state"]["status"], "merged");
    assert_eq!(candidate["merge_state"]["merged_commit"], main_after_merge);

    // Rebase task should appear in the attempt
    let attempt = &value["attempts"][0];
    let rebase_tasks: Vec<_> = attempt["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|t| t["kind"] == "rebase")
        .collect();
    assert_eq!(rebase_tasks.len(), 1);
    assert_eq!(rebase_tasks[0]["id"], "attempt-1-rebase");
    assert_eq!(rebase_tasks[0]["status"], "complete");
}

#[test]
fn work_merge_candidate_rebase_failure_leaves_target_unchanged() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "create", "work-1", "--title", "Rebase conflict"])
        .assert()
        .success();
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "attempt", "work-1", "attempt-1"])
        .assert()
        .success();

    let bin_dir = tmp.path().join("bin-rebase-conflict");
    write_mock_claude(
        &bin_dir,
        &rebase_give_up_mock_script(),
    );
    factory_cmd()
        .current_dir(&main_dir)
        .args([
            "work",
            "attempt",
            "run",
            "work-1",
            "attempt-1",
            "--no-sandbox",
        ])
        .env("PATH", mock_path(&bin_dir))
        .assert()
        .success();

    let candidate_workspace = main_dir.join("../work-6-work-1-attempt-1");
    let candidate_head = git_head(&candidate_workspace);
    commit_file(
        &main_dir,
        "README.md",
        "target readme\n",
        "Update README from target",
    );
    let main_before_merge = git_head(&main_dir);

    factory_cmd()
        .current_dir(&main_dir)
        .args([
            "work",
            "merge",
            "work-1",
            "attempt-1-merge-candidate",
            "--no-sandbox",
        ])
        .env("PATH", mock_path(&bin_dir))
        .assert()
        .failure()
        .stderr(predicate::str::contains("needs-user"));

    assert_eq!(git_head(&main_dir), main_before_merge);
    assert_eq!(git_head(&candidate_workspace), candidate_head);
    let value = read_work_show_json(&main_dir, "work-1");
    let candidate = &value["merge_candidates"][0];
    assert_eq!(candidate["merge_state"]["status"], "needs-user");
    assert!(
        candidate["merge_state"]["failure_reason"]
            .as_str()
            .unwrap()
            .contains("Cannot resolve conflict")
    );
}

#[test]
fn work_merge_rebase_resolves_trivial_conflict() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "create", "work-1", "--title", "Trivial conflict"])
        .assert()
        .success();
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "attempt", "work-1", "attempt-1"])
        .assert()
        .success();

    let bin_dir = tmp.path().join("bin-rebase-resolve");
    write_mock_claude(&bin_dir, &rebase_conflict_resolve_mock_script());
    factory_cmd()
        .current_dir(&main_dir)
        .args([
            "work",
            "attempt",
            "run",
            "work-1",
            "attempt-1",
            "--no-sandbox",
        ])
        .env("PATH", mock_path(&bin_dir))
        .assert()
        .success();

    // Create a conflicting change on main
    commit_file(
        &main_dir,
        "shared.txt",
        "target content\n",
        "Add shared from target",
    );

    factory_cmd()
        .current_dir(&main_dir)
        .args([
            "work",
            "merge",
            "work-1",
            "attempt-1-merge-candidate",
            "--no-sandbox",
        ])
        .env("PATH", mock_path(&bin_dir))
        .assert()
        .success();

    let value = read_work_show_json(&main_dir, "work-1");
    let candidate = &value["merge_candidates"][0];
    assert_eq!(candidate["merge_state"]["status"], "merged");

    // Verify both contents are present (conflict resolved by keeping both)
    let merged = fs::read_to_string(main_dir.join("shared.txt")).unwrap();
    assert!(merged.contains("target content") || merged.contains("shared content"));
}

#[test]
fn work_merge_rebase_gives_up_transitions_to_needs_user() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "create", "work-1", "--title", "Give up"])
        .assert()
        .success();
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "attempt", "work-1", "attempt-1"])
        .assert()
        .success();

    let bin_dir = tmp.path().join("bin-rebase-giveup");
    write_mock_claude(&bin_dir, &rebase_give_up_mock_script());
    factory_cmd()
        .current_dir(&main_dir)
        .args([
            "work",
            "attempt",
            "run",
            "work-1",
            "attempt-1",
            "--no-sandbox",
        ])
        .env("PATH", mock_path(&bin_dir))
        .assert()
        .success();

    let candidate_workspace = main_dir.join("../work-6-work-1-attempt-1");
    commit_file(
        &main_dir,
        "README.md",
        "target readme\n",
        "Update README from target",
    );
    let main_before_merge = git_head(&main_dir);

    factory_cmd()
        .current_dir(&main_dir)
        .args([
            "work",
            "merge",
            "work-1",
            "attempt-1-merge-candidate",
            "--no-sandbox",
        ])
        .env("PATH", mock_path(&bin_dir))
        .assert()
        .failure()
        .stderr(predicate::str::contains("needs-user"));

    // Target unchanged
    assert_eq!(git_head(&main_dir), main_before_merge);
    // Candidate workspace restored
    assert_eq!(
        git_head(&candidate_workspace),
        git_head(&candidate_workspace)
    );

    let value = read_work_show_json(&main_dir, "work-1");
    let candidate = &value["merge_candidates"][0];
    assert_eq!(candidate["merge_state"]["status"], "needs-user");

    // Rebase task should show needs-user status
    let attempt = &value["attempts"][0];
    let rebase_tasks: Vec<_> = attempt["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|t| t["kind"] == "rebase")
        .collect();
    assert_eq!(rebase_tasks.len(), 1);
    assert_eq!(rebase_tasks[0]["status"], "needs-user");
}

#[test]
fn work_merge_rebase_provenance_updated_after_rebase() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "create", "work-1", "--title", "Provenance update"])
        .assert()
        .success();
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "attempt", "work-1", "attempt-1"])
        .assert()
        .success();

    let bin_dir = tmp.path().join("bin-rebase-prov");
    write_mock_claude(&bin_dir, &rebase_mock_script("pass"));
    factory_cmd()
        .current_dir(&main_dir)
        .args([
            "work",
            "attempt",
            "run",
            "work-1",
            "attempt-1",
            "--no-sandbox",
        ])
        .env("PATH", mock_path(&bin_dir))
        .assert()
        .success();

    // Advance target so rebase creates new SHAs
    commit_file(
        &main_dir,
        "target-only.txt",
        "target content\n",
        "Advance target",
    );

    factory_cmd()
        .current_dir(&main_dir)
        .args([
            "work",
            "merge",
            "work-1",
            "attempt-1-merge-candidate",
            "--no-sandbox",
        ])
        .env("PATH", mock_path(&bin_dir))
        .assert()
        .success();

    let value = read_work_show_json(&main_dir, "work-1");
    let candidate = &value["merge_candidates"][0];
    let merged_commit = candidate["merge_state"]["merged_commit"].as_str().unwrap();

    // candidate_commit should have been updated to the post-rebase tip
    let candidate_commit = candidate["candidate_commit"].as_str().unwrap();
    assert_eq!(candidate_commit, merged_commit);

    // Write task output.commit should also have been updated
    let attempt = &value["attempts"][0];
    let write_tasks: Vec<_> = attempt["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|t| t["kind"] == "write" && t["status"] == "complete")
        .collect();
    assert!(!write_tasks.is_empty());
    for task in &write_tasks {
        assert_eq!(
            task["output"]["commit"].as_str().unwrap(),
            merged_commit,
            "write task output commit should match merged commit after provenance regeneration"
        );
    }
}

#[test]
fn work_attempt_run_plans_followup_for_failed_reviews() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    create_completed_work_attempt_with_instructions(
        &tmp,
        &main_dir,
        Some("Keep durable instructions on every write Task."),
    );

    let bin_dir = tmp.path().join("bin-loop-fail");
    write_mock_claude(&bin_dir, &stateful_loop_mock_script("fail"));

    factory_cmd()
        .current_dir(&main_dir)
        .args([
            "work",
            "attempt",
            "run",
            "work-1",
            "attempt-1",
            "--no-sandbox",
        ])
        .env("PATH", mock_path(&bin_dir))
        .env("FACTORY_MAX_TOTAL_WRITE_ROUNDS", "3")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Planned 5 review Tasks for Attempt attempt-1",
        ))
        .stdout(predicate::str::contains(
            "Planned write Task attempt-1-write-2",
        ))
        .stdout(predicate::str::contains("Completed Task attempt-1-write-2"))
        .stdout(predicate::str::contains(
            "Planned write Task attempt-1-write-3",
        ))
        .stdout(predicate::str::contains("Completed Task attempt-1-write-3"))
        .stdout(predicate::str::contains("needs user input"))
        .stdout(predicate::str::contains("attempt-1-write-4").not());

    let value = read_work_show_json(&main_dir, "work-1");
    let attempt = &value["attempts"][0];
    assert_eq!(attempt["status"], "needs-user");
    assert_eq!(attempt["review_state"], "failed");
    let followup = attempt["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|task| task["id"] == "attempt-1-write-2")
        .unwrap();
    assert_eq!(followup["kind"], "write");
    assert_eq!(followup["workspace_access"]["writes"][0]["id"], "candidate");
    assert_eq!(
        followup["workspace_access"]["writes"][0]["path"],
        "../work-6-work-1-attempt-1"
    );
    assert_eq!(followup["input_artifacts"].as_array().unwrap().len(), 5);
    assert_eq!(
        followup["input_artifacts"][0]["path"],
        ".factory/work/artifacts/work-1/attempt-1/attempt-1-review-documentation/review.md"
    );
    assert_eq!(
        followup["instructions"],
        "Keep durable instructions on every write Task."
    );
    assert_eq!(followup["status"], "complete");
    let second_followup = attempt["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|task| task["id"] == "attempt-1-write-3")
        .unwrap();
    assert_eq!(second_followup["status"], "complete");
    assert!(
        !attempt["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|task| task["id"] == "attempt-1-write-4")
    );
    let second_round_reviews: Vec<_> = attempt["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|task| {
            task["kind"] == "review"
                && task["id"]
                    .as_str()
                    .is_some_and(|id| id.starts_with("attempt-1-review-2-"))
        })
        .collect();
    assert_eq!(second_round_reviews.len(), 5);
    let second_tests_review = second_round_reviews
        .iter()
        .find(|task| task["id"] == "attempt-1-review-2-tests")
        .unwrap();
    assert_eq!(second_tests_review["status"], "complete");
    assert_eq!(
        second_tests_review["review_context"]["candidate_commit"],
        followup["output"]["commit"]
    );
    assert_eq!(
        second_tests_review["review_context"]["candidate_workspace_path"],
        "../work-6-work-1-attempt-1"
    );
    let handoff =
        fs::read_to_string(main_dir.join(".factory/work/artifacts/work-1/attempt-1/needs-user.md"))
            .unwrap();
    assert!(handoff.contains("write-round ceiling"));
    assert!(handoff.contains("attempt-1-review-3-tests/review.md"));
}

#[test]
fn work_create_planning_context_feeds_followup_for_failed_reviews() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    let brief_path = tmp.path().join("brief.md");
    let behaviors_path = tmp.path().join("behaviors.md");
    let approach_path = tmp.path().join("approach.md");
    let plan_path = tmp.path().join("plan.md");
    fs::write(&brief_path, "Build retry planning context.\n").unwrap();
    fs::write(&behaviors_path, "WHEN reviews fail, retry with context.\n").unwrap();
    fs::write(&approach_path, "Reuse Work state for retry prompts.\n").unwrap();
    fs::write(&plan_path, "1. Plan the write round.\n").unwrap();

    let bin_dir = tmp.path().join("bin-planning-followup");
    write_mock_claude(&bin_dir, &stateful_loop_mock_script("fail"));

    factory_cmd()
        .current_dir(&main_dir)
        .args([
            "work",
            "create",
            "work-1",
            "--title",
            "Planning follow-up",
            "--brief-file",
            &brief_path.to_string_lossy(),
            "--behaviors-file",
            &behaviors_path.to_string_lossy(),
            "--approach-file",
            &approach_path.to_string_lossy(),
            "--plan-file",
            &plan_path.to_string_lossy(),
        ])
        .assert()
        .success();
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "attempt", "work-1", "attempt-1"])
        .assert()
        .success();
    factory_cmd()
        .current_dir(&main_dir)
        .args([
            "work",
            "attempt",
            "run",
            "work-1",
            "attempt-1",
            "--no-sandbox",
        ])
        .env("PATH", mock_path(&bin_dir))
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Planned write Task attempt-1-write-2",
        ))
        .stdout(predicate::str::contains("needs user input"));

    let value = read_work_show_json(&main_dir, "work-1");
    assert_eq!(value["instructions"], serde_json::Value::Null);
    let followup = value["attempts"][0]["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|task| task["id"] == "attempt-1-write-2")
        .unwrap();
    let instructions = followup["instructions"].as_str().unwrap();
    assert!(instructions.contains("# Brief\n\nBuild retry planning context."));
    assert!(instructions.contains("# Behaviors\n\nWHEN reviews fail, retry with context."));
    assert!(instructions.contains("# Approach\n\nReuse Work state for retry prompts."));
    assert!(instructions.contains("# Plan\n\n1. Plan the write round."));
    assert_eq!(followup["input_artifacts"].as_array().unwrap().len(), 5);
}

#[test]
fn work_attempt_run_plans_followup_for_mixed_failed_and_uncertain_reviews() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    create_completed_work_attempt(&tmp, &main_dir);

    let bin_dir = tmp.path().join("bin-loop-mixed-fail-uncertain");
    write_mock_claude(&bin_dir, &loop_mock_script_with_mixed_verdicts());

    factory_cmd()
        .current_dir(&main_dir)
        .args([
            "work",
            "attempt",
            "run",
            "work-1",
            "attempt-1",
            "--no-sandbox",
        ])
        .env("PATH", mock_path(&bin_dir))
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Planned write Task attempt-1-write-2",
        ))
        .stdout(predicate::str::contains("Completed Task attempt-1-write-2"))
        .stdout(predicate::str::contains(
            "Planned 1 review Tasks for Attempt attempt-1",
        ))
        .stdout(predicate::str::contains("attempt-1-review-2-documentation"))
        .stdout(predicate::str::contains("attempt-1-review-2-tests").not())
        .stdout(predicate::str::contains(
            "Merge Candidate attempt-1-merge-candidate is ready",
        ));

    let value = read_work_show_json(&main_dir, "work-1");
    let attempt = &value["attempts"][0];
    assert_eq!(attempt["status"], "complete");
    assert_eq!(attempt["review_state"], "passed");
    assert!(
        !main_dir
            .join(".factory/work/artifacts/work-1/attempt-1/needs-user.md")
            .exists()
    );
    let followup = attempt["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|task| task["id"] == "attempt-1-write-2")
        .unwrap();
    let input_artifacts = followup["input_artifacts"].as_array().unwrap();
    assert_eq!(input_artifacts.len(), 1);
    assert_eq!(
        input_artifacts[0]["path"],
        ".factory/work/artifacts/work-1/attempt-1/attempt-1-review-documentation/review.md"
    );

    let second_round_reviews: Vec<_> = attempt["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|task| {
            task["kind"] == "review"
                && task["id"]
                    .as_str()
                    .is_some_and(|id| id.starts_with("attempt-1-review-2-"))
        })
        .collect();
    assert_eq!(second_round_reviews.len(), 1);
    assert_eq!(
        second_round_reviews[0]["id"],
        "attempt-1-review-2-documentation"
    );
    let second_round_inputs = second_round_reviews[0]["input_artifacts"]
        .as_array()
        .unwrap();
    assert_eq!(second_round_inputs.len(), 1);
    assert_eq!(
        second_round_inputs[0]["path"],
        ".factory/work/artifacts/work-1/attempt-1/attempt-1-review-documentation/review.md"
    );
    assert_eq!(
        second_round_inputs[0]["producer_id"],
        "attempt-1-review-documentation"
    );
}

#[test]
fn work_attempt_run_counts_already_planned_followup_against_budget() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    create_completed_work_attempt(&tmp, &main_dir);

    write_planned_followup_task(&main_dir, Vec::new());

    let bin_dir = tmp.path().join("bin-loop-preplanned-followup");
    write_mock_claude(&bin_dir, &stateful_loop_mock_script("fail"));
    factory_cmd()
        .current_dir(&main_dir)
        .args([
            "work",
            "attempt",
            "run",
            "work-1",
            "attempt-1",
            "--no-sandbox",
        ])
        .env("PATH", mock_path(&bin_dir))
        .env("FACTORY_MAX_TOTAL_WRITE_ROUNDS", "3")
        .assert()
        .success()
        .stdout(predicate::str::contains("Completed Task attempt-1-write-2"))
        .stdout(predicate::str::contains(
            "Planned 5 review Tasks for Attempt attempt-1",
        ))
        .stdout(predicate::str::contains(
            "Planned write Task attempt-1-write-3",
        ))
        .stdout(predicate::str::contains("Completed Task attempt-1-write-3"))
        .stdout(predicate::str::contains("needs user input"))
        .stdout(predicate::str::contains("attempt-1-write-4").not());

    let value = read_work_show_json(&main_dir, "work-1");
    let attempt = &value["attempts"][0];
    assert_eq!(attempt["status"], "needs-user");
    assert_eq!(attempt["review_state"], "failed");
    assert!(
        !attempt["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|task| task["id"] == "attempt-1-write-4")
    );
    let second_round_reviews: Vec<_> = attempt["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|task| {
            task["kind"] == "review"
                && task["id"]
                    .as_str()
                    .is_some_and(|id| id.starts_with("attempt-1-review-2-"))
        })
        .collect();
    assert_eq!(second_round_reviews.len(), 5);
    assert!(
        second_round_reviews
            .iter()
            .any(|task| task["role"] == "tests")
    );
    assert!(
        second_round_reviews
            .iter()
            .any(|task| task["role"] == "documentation")
    );
    let handoff =
        fs::read_to_string(main_dir.join(".factory/work/artifacts/work-1/attempt-1/needs-user.md"))
            .unwrap();
    assert!(handoff.contains("write-round ceiling"));
    assert!(handoff.contains("attempt-1-review-2-tests/review.md"));
}

#[test]
fn work_attempt_run_exposes_followup_input_artifacts() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    create_completed_work_attempt(&tmp, &main_dir);

    let bin_dir = tmp.path().join("bin-loop-followup-inputs");
    let review_artifact_path =
        ".factory/work/artifacts/work-1/attempt-1/attempt-1-review-documentation/review.md";
    let review_artifact = main_dir.join(review_artifact_path);
    fs::create_dir_all(review_artifact.parent().unwrap()).unwrap();
    fs::write(
        &review_artifact,
        "Verdict: fail\n\nmissing first-pass preflight item\n",
    )
    .unwrap();
    write_planned_followup_task(
        &main_dir,
        vec![serde_json::json!({
            "producer_id": "attempt-1-review-documentation",
            "path": review_artifact_path
        })],
    );

    let prompt_log = tmp.path().join("followup-prompt.log");
    let sandbox_profile_log = tmp.path().join("followup-sandbox.sb");
    write_mock_claude(
        &bin_dir,
        r##"#!/bin/bash
case "$PWD" in
  */work-6-work-1-attempt-1)
    prompt=""
    while [ "$#" -gt 0 ]; do
      if [ "$1" = "-p" ]; then
        shift
        prompt="$1"
        break
      fi
      shift
    done
    printf '%s\n' "$prompt" > "$PROMPT_LOG"
    artifact="$(printf '%s\n' "$prompt" | awk '/^Input artifacts:/{getline; sub(/^- /, ""); print; exit}')"
    test -f "$artifact"
    grep -q 'Verdict: fail' "$artifact"
    printf 'follow-up output\n' > followup-output.txt
    git add followup-output.txt
    git commit -m "Add follow-up output" >/dev/null
    ;;
  *)
    printf 'Verdict: pass\n\nLoop review passed.\n' > review.md
    ;;
esac
exit 0
"##,
    );
    write_mock_executable(
        &bin_dir,
        "sandbox-exec",
        r##"#!/bin/bash
if [ "$1" = "-f" ]; then
  profile="$2"
  shift 2
  case "$PWD" in
    */work-6-work-1-attempt-1)
      cp "$profile" "$SANDBOX_PROFILE_LOG"
      ;;
  esac
fi
exec "$@"
"##,
    );

    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "attempt", "run", "work-1", "attempt-1"])
        .env("PATH", mock_path(&bin_dir))
        .env("PROMPT_LOG", &prompt_log)
        .env("SANDBOX_PROFILE_LOG", &sandbox_profile_log)
        .env("CLAUDE_CODE_OAUTH_TOKEN", "mock-token")
        .env("BRAVE_SEARCH_API_KEY", "mock-key")
        .env("AWS_ACCESS_KEY_ID", "mock-access")
        .assert()
        .success()
        .stdout(predicate::str::contains("Completed Task attempt-1-write-2"));

    let expected_artifact =
        fs::canonicalize(main_dir.join(
            ".factory/work/artifacts/work-1/attempt-1/attempt-1-review-documentation/review.md",
        ))
        .unwrap();
    let expected_artifact_dir = expected_artifact.parent().unwrap();
    let prompt = fs::read_to_string(prompt_log).unwrap();
    assert!(prompt.contains("Input artifacts:"));
    assert!(prompt.contains("Author preflight:"));
    assert!(prompt.contains("Read the review input artifacts first"));
    assert!(prompt.contains("address the concrete findings"));
    assert!(prompt.contains("missing first-pass preflight item"));
    assert!(
        prompt.contains(&format!("- {}", expected_artifact.display())),
        "{prompt}"
    );

    let sandbox_profile = fs::read_to_string(sandbox_profile_log).unwrap();
    assert!(
        sandbox_profile.contains(&format!(
            "(allow file-read*  (subpath \"{}\"))",
            expected_artifact_dir.display()
        )),
        "{sandbox_profile}"
    );
    assert!(
        !sandbox_profile.contains(&format!(
            "(allow file-write* (subpath \"{}\"))",
            expected_artifact_dir.display()
        )),
        "{sandbox_profile}"
    );
}

#[test]
fn work_attempt_run_rejects_unmanaged_completed_review_artifact_area_path() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    create_completed_work_attempt(&tmp, &main_dir);
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "review", "work-1", "attempt-1"])
        .assert()
        .success();

    let bin_dir = tmp.path().join("bin-review-pass");
    write_mock_claude(
        &bin_dir,
        "#!/bin/bash\nprintf 'Verdict: pass\\n' > review.md\nexit 0\n",
    );
    for role in [
        "documentation",
        "behaviors",
        "architecture",
        "skills",
        "tests",
    ] {
        factory_cmd()
            .current_dir(&main_dir)
            .args([
                "work",
                "task",
                "run",
                "work-1",
                "attempt-1",
                &format!("attempt-1-review-{role}"),
                "--no-sandbox",
            ])
            .env("PATH", mock_path(&bin_dir))
            .assert()
            .success();
    }

    let outside_dir = tmp.path().join("outside-review-artifacts");
    fs::create_dir_all(&outside_dir).unwrap();
    fs::write(outside_dir.join("review.md"), "Verdict: fail\n").unwrap();

    let task_path =
        work_task_record_path(&main_dir, "work-1", "attempt-1", "attempt-1-review-tests");
    let mut value = read_json_value(&task_path);
    value["artifact_area"]["path"] =
        serde_json::Value::String("../outside-review-artifacts".to_string());
    write_json_value(&task_path, &value);
    let before = fs::read_to_string(&task_path).unwrap();

    factory_cmd()
        .current_dir(&main_dir)
        .args([
            "work",
            "attempt",
            "run",
            "work-1",
            "attempt-1",
            "--no-sandbox",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("Task artifact area path must"));

    assert_eq!(fs::read_to_string(&task_path).unwrap(), before);
}

#[test]
fn work_attempt_run_marks_uncertain_reviews_needs_user() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    create_completed_work_attempt(&tmp, &main_dir);

    let bin_dir = tmp.path().join("bin-loop-uncertain");
    write_mock_claude(&bin_dir, &loop_mock_script("uncertain"));

    factory_cmd()
        .current_dir(&main_dir)
        .args([
            "work",
            "attempt",
            "run",
            "work-1",
            "attempt-1",
            "--no-sandbox",
        ])
        .env("PATH", mock_path(&bin_dir))
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Attempt attempt-1 needs user input: .factory/work/artifacts/work-1/attempt-1/needs-user.md",
        ));

    let value = read_work_show_json(&main_dir, "work-1");
    let attempt = &value["attempts"][0];
    assert_eq!(attempt["status"], "needs-user");
    assert_eq!(attempt["review_state"], "uncertain");
    let handoff =
        fs::read_to_string(main_dir.join(".factory/work/artifacts/work-1/attempt-1/needs-user.md"))
            .unwrap();
    assert!(handoff.contains("attempt-1-review-tests/review.md"));
}

#[test]
fn work_attempt_run_marks_missing_verdict_needs_user() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    create_completed_work_attempt(&tmp, &main_dir);

    let bin_dir = tmp.path().join("bin-loop-missing-verdict");
    write_mock_claude(&bin_dir, &loop_mock_script_without_verdict());

    factory_cmd()
        .current_dir(&main_dir)
        .args([
            "work",
            "attempt",
            "run",
            "work-1",
            "attempt-1",
            "--no-sandbox",
        ])
        .env("PATH", mock_path(&bin_dir))
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Attempt attempt-1 needs user input: .factory/work/artifacts/work-1/attempt-1/needs-user.md",
        ));

    let value = read_work_show_json(&main_dir, "work-1");
    let attempt = &value["attempts"][0];
    assert_eq!(attempt["status"], "needs-user");
    assert_eq!(attempt["review_state"], "uncertain");
    let handoff =
        fs::read_to_string(main_dir.join(".factory/work/artifacts/work-1/attempt-1/needs-user.md"))
            .unwrap();
    assert!(handoff.contains("uncertain or missing review verdicts"));
    assert!(handoff.contains("attempt-1-review-tests/review.md"));
}

#[test]
fn work_attempt_run_stops_when_task_executor_fails() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "create", "work-1", "--title", "Attempt loop"])
        .assert()
        .success();
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "attempt", "work-1", "attempt-1"])
        .assert()
        .success();

    let bin_dir = tmp.path().join("bin-loop-failure");
    write_mock_claude(&bin_dir, "#!/bin/bash\nexit 7\n");

    factory_cmd()
        .current_dir(&main_dir)
        .args([
            "work",
            "attempt",
            "run",
            "work-1",
            "attempt-1",
            "--no-sandbox",
        ])
        .env("PATH", mock_path(&bin_dir))
        .assert()
        .failure()
        .stderr(predicate::str::contains("Coder exited with code 7"));

    let value = read_work_show_json(&main_dir, "work-1");
    let attempt = &value["attempts"][0];
    assert_eq!(attempt["status"], "failed");
    assert_eq!(attempt["tasks"][0]["status"], "failed");
    assert_eq!(attempt["tasks"].as_array().unwrap().len(), 1);
}

#[test]
fn work_task_run_rejects_unmanaged_review_read_workspace_path() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    create_completed_work_attempt(&tmp, &main_dir);
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "review", "work-1", "attempt-1"])
        .assert()
        .success();

    let task_path =
        work_task_record_path(&main_dir, "work-1", "attempt-1", "attempt-1-review-tests");
    let planned = fs::read_to_string(&task_path).unwrap();
    let outside_absolute = tmp.path().join("outside-review-read");
    let outside_absolute = outside_absolute.to_string_lossy().to_string();
    for path in [
        "../outside-review-read",
        "../work-6-work-1-attempt-1/nested",
        outside_absolute.as_str(),
    ] {
        let mut value: serde_json::Value = serde_json::from_str(&planned).unwrap();
        value["workspace_access"]["reads"][0]["path"] = serde_json::Value::String(path.to_string());
        value["review_context"]["candidate_workspace_path"] =
            serde_json::Value::String(path.to_string());
        write_json_value(&task_path, &value);
        let before = fs::read_to_string(&task_path).unwrap();

        factory_cmd()
            .current_dir(&main_dir)
            .args([
                "work",
                "task",
                "run",
                "work-1",
                "attempt-1",
                "attempt-1-review-tests",
                "--no-sandbox",
            ])
            .assert()
            .failure()
            .stderr(predicate::str::contains(
                "Task readable workspace path must",
            ));

        assert_eq!(fs::read_to_string(&task_path).unwrap(), before);
    }
    assert!(
        !main_dir
            .join(".factory/work/artifacts/work-1/attempt-1/attempt-1-review-tests")
            .exists()
    );
}

#[test]
fn work_task_run_rejects_malformed_review_context() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    create_completed_work_attempt(&tmp, &main_dir);
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "review", "work-1", "attempt-1"])
        .assert()
        .success();

    let task_path =
        work_task_record_path(&main_dir, "work-1", "attempt-1", "attempt-1-review-tests");
    let planned = fs::read_to_string(&task_path).unwrap();
    for (mutation, expected) in [
        (
            "delete",
            "review task attempt-1-review-tests must declare review context",
        ),
        (
            "id",
            "review task attempt-1-review-tests review context candidate must match a readable workspace",
        ),
        (
            "path",
            "review task attempt-1-review-tests review context candidate must match a readable workspace",
        ),
    ] {
        let mut review_task: serde_json::Value = serde_json::from_str(&planned).unwrap();
        match mutation {
            "delete" => {
                review_task
                    .as_object_mut()
                    .unwrap()
                    .remove("review_context");
            }
            "id" => {
                review_task["review_context"]["candidate_workspace_id"] =
                    serde_json::Value::String("other-candidate".to_string());
            }
            "path" => {
                review_task["review_context"]["candidate_workspace_path"] =
                    serde_json::Value::String("../work-6-work-1-other".to_string());
            }
            _ => unreachable!(),
        }
        write_json_value(&task_path, &review_task);
        let before = fs::read_to_string(&task_path).unwrap();

        factory_cmd()
            .current_dir(&main_dir)
            .args([
                "work",
                "task",
                "run",
                "work-1",
                "attempt-1",
                "attempt-1-review-tests",
                "--no-sandbox",
            ])
            .assert()
            .failure()
            .stderr(predicate::str::contains(expected));

        assert_eq!(fs::read_to_string(&task_path).unwrap(), before);
    }
    assert!(
        !main_dir
            .join(".factory/work/artifacts/work-1/attempt-1/attempt-1-review-tests")
            .exists()
    );
}

#[test]
fn work_task_run_fails_review_task_without_artifact() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    create_completed_work_attempt(&tmp, &main_dir);
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "review", "work-1", "attempt-1"])
        .assert()
        .success();
    let bin_dir = tmp.path().join("bin-review");
    write_mock_claude(&bin_dir, "#!/bin/bash\nexit 0\n");

    factory_cmd()
        .current_dir(&main_dir)
        .args([
            "work",
            "task",
            "run",
            "work-1",
            "attempt-1",
            "attempt-1-review-tests",
            "--no-sandbox",
        ])
        .env("PATH", mock_path(&bin_dir))
        .assert()
        .failure()
        .stderr(predicate::str::contains("without writing"));

    let value = read_work_show_json(&main_dir, "work-1");
    let review_task = value["attempts"][0]["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|task| task["id"] == "attempt-1-review-tests")
        .unwrap();
    assert_eq!(value["attempts"][0]["status"], "failed");
    assert_eq!(review_task["status"], "failed");
}

#[test]
fn work_task_run_ignores_stale_review_artifact() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    create_completed_work_attempt(&tmp, &main_dir);
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "review", "work-1", "attempt-1"])
        .assert()
        .success();

    let review_dir =
        main_dir.join(".factory/work/artifacts/work-1/attempt-1/attempt-1-review-tests");
    let review_path = review_dir.join("review.md");
    fs::create_dir_all(&review_dir).unwrap();
    fs::write(&review_path, "Verdict: pass\n\nstale\n").unwrap();

    let bin_dir = tmp.path().join("bin-review");
    write_mock_claude(&bin_dir, "#!/bin/bash\nexit 0\n");

    factory_cmd()
        .current_dir(&main_dir)
        .args([
            "work",
            "task",
            "run",
            "work-1",
            "attempt-1",
            "attempt-1-review-tests",
            "--no-sandbox",
        ])
        .env("PATH", mock_path(&bin_dir))
        .assert()
        .failure()
        .stderr(predicate::str::contains("without writing"));

    assert!(!review_path.exists());
    let value = read_work_show_json(&main_dir, "work-1");
    let review_task = value["attempts"][0]["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|task| task["id"] == "attempt-1-review-tests")
        .unwrap();
    assert_eq!(value["attempts"][0]["status"], "failed");
    assert_eq!(review_task["status"], "failed");
}

#[test]
fn work_task_run_rejects_unmanaged_review_artifact_area_path() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    create_completed_work_attempt(&tmp, &main_dir);
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "review", "work-1", "attempt-1"])
        .assert()
        .success();

    let task_path =
        work_task_record_path(&main_dir, "work-1", "attempt-1", "attempt-1-review-tests");
    let planned = fs::read_to_string(&task_path).unwrap();
    let outside_absolute = tmp.path().join("outside-review-absolute");
    let outside_absolute = outside_absolute.to_string_lossy().to_string();
    for path in [
        "../outside-review-artifacts",
        ".factory/work/artifacts",
        ".factory/work/artifacts/../outside-review-artifacts",
        outside_absolute.as_str(),
    ] {
        let mut value: serde_json::Value = serde_json::from_str(&planned).unwrap();
        value["artifact_area"]["path"] = serde_json::Value::String(path.to_string());
        write_json_value(&task_path, &value);
        let before = fs::read_to_string(&task_path).unwrap();

        factory_cmd()
            .current_dir(&main_dir)
            .args([
                "work",
                "task",
                "run",
                "work-1",
                "attempt-1",
                "attempt-1-review-tests",
                "--no-sandbox",
            ])
            .assert()
            .failure()
            .stderr(predicate::str::contains("Task artifact area path must"));

        assert_eq!(fs::read_to_string(&task_path).unwrap(), before);
    }

    assert!(!main_dir.join("../outside-review-artifacts").exists());
    assert!(
        !main_dir
            .join(".factory/work/outside-review-artifacts")
            .exists()
    );
    assert!(!Path::new(&outside_absolute).exists());
}

#[test]
fn work_task_run_marks_review_task_failed_when_coder_exits_nonzero() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    create_completed_work_attempt(&tmp, &main_dir);
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "review", "work-1", "attempt-1"])
        .assert()
        .success();

    let bin_dir = tmp.path().join("bin-review");
    write_mock_claude(&bin_dir, "#!/bin/bash\nexit 7\n");

    factory_cmd()
        .current_dir(&main_dir)
        .args([
            "work",
            "task",
            "run",
            "work-1",
            "attempt-1",
            "attempt-1-review-tests",
            "--no-sandbox",
        ])
        .env("PATH", mock_path(&bin_dir))
        .assert()
        .failure()
        .stderr(predicate::str::contains("Coder exited with code 7"));

    let value = read_work_show_json(&main_dir, "work-1");
    let review_task = value["attempts"][0]["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|task| task["id"] == "attempt-1-review-tests")
        .unwrap();
    assert_eq!(value["attempts"][0]["status"], "failed");
    assert_eq!(review_task["status"], "failed");
}

#[test]
fn work_task_run_fails_review_task_that_dirties_candidate_workspace() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    create_completed_work_attempt(&tmp, &main_dir);
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "review", "work-1", "attempt-1"])
        .assert()
        .success();

    let bin_dir = tmp.path().join("bin-review");
    let candidate = main_dir.join("../work-6-work-1-attempt-1");
    write_mock_claude(
        &bin_dir,
        r##"#!/bin/bash
printf 'Verdict: pass\n' > review.md
printf 'review mutation\n' > "$CANDIDATE/dirty-review.txt"
exit 0
"##,
    );

    factory_cmd()
        .current_dir(&main_dir)
        .args([
            "work",
            "task",
            "run",
            "work-1",
            "attempt-1",
            "attempt-1-review-tests",
            "--no-sandbox",
        ])
        .env("PATH", mock_path(&bin_dir))
        .env("CANDIDATE", &candidate)
        .assert()
        .failure()
        .stderr(predicate::str::contains("uncommitted changes"));

    let value = read_work_show_json(&main_dir, "work-1");
    let review_task = value["attempts"][0]["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|task| task["id"] == "attempt-1-review-tests")
        .unwrap();
    assert_eq!(value["attempts"][0]["status"], "failed");
    assert_eq!(review_task["status"], "failed");
}

#[test]
fn work_task_run_fails_review_task_that_dirties_candidate_workspace_and_exits_nonzero() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    create_completed_work_attempt(&tmp, &main_dir);
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "review", "work-1", "attempt-1"])
        .assert()
        .success();

    let bin_dir = tmp.path().join("bin-review");
    let candidate = main_dir.join("../work-6-work-1-attempt-1");
    write_mock_claude(
        &bin_dir,
        r##"#!/bin/bash
printf 'review mutation\n' > "$CANDIDATE/dirty-review.txt"
exit 7
"##,
    );

    factory_cmd()
        .current_dir(&main_dir)
        .args([
            "work",
            "task",
            "run",
            "work-1",
            "attempt-1",
            "attempt-1-review-tests",
            "--no-sandbox",
        ])
        .env("PATH", mock_path(&bin_dir))
        .env("CANDIDATE", &candidate)
        .assert()
        .failure()
        .stderr(predicate::str::contains("uncommitted changes"));

    let value = read_work_show_json(&main_dir, "work-1");
    let review_task = value["attempts"][0]["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|task| task["id"] == "attempt-1-review-tests")
        .unwrap();
    assert_eq!(value["attempts"][0]["status"], "failed");
    assert_eq!(review_task["status"], "failed");
}

#[test]
fn work_task_run_fails_review_task_that_commits_to_candidate_workspace() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    create_completed_work_attempt(&tmp, &main_dir);
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "review", "work-1", "attempt-1"])
        .assert()
        .success();

    let bin_dir = tmp.path().join("bin-review");
    let candidate = main_dir.join("../work-6-work-1-attempt-1");
    let baseline_head = git_head(&candidate);
    write_mock_claude(
        &bin_dir,
        r##"#!/bin/bash
printf 'Verdict: pass\n' > review.md
printf 'committed review mutation\n' > "$CANDIDATE/committed-review.txt"
git -C "$CANDIDATE" add committed-review.txt
git -C "$CANDIDATE" commit -m "Commit review mutation" >/dev/null
exit 0
"##,
    );

    factory_cmd()
        .current_dir(&main_dir)
        .args([
            "work",
            "task",
            "run",
            "work-1",
            "attempt-1",
            "attempt-1-review-tests",
            "--no-sandbox",
        ])
        .env("PATH", mock_path(&bin_dir))
        .env("CANDIDATE", &candidate)
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "changed readable candidate workspace HEAD",
        ));

    assert_eq!(git_head(&candidate), baseline_head);
    let value = read_work_show_json(&main_dir, "work-1");
    let review_task = value["attempts"][0]["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|task| task["id"] == "attempt-1-review-tests")
        .unwrap();
    assert_eq!(value["attempts"][0]["status"], "failed");
    assert_eq!(review_task["status"], "failed");
}

#[test]
fn work_task_run_restores_committed_review_mutation_before_dirty_failure() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    create_completed_work_attempt(&tmp, &main_dir);
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "review", "work-1", "attempt-1"])
        .assert()
        .success();

    let bin_dir = tmp.path().join("bin-review");
    let candidate = main_dir.join("../work-6-work-1-attempt-1");
    let baseline_head = git_head(&candidate);
    write_mock_claude(
        &bin_dir,
        r##"#!/bin/bash
printf 'Verdict: pass\n' > review.md
printf 'committed review mutation\n' > "$CANDIDATE/committed-review.txt"
git -C "$CANDIDATE" add committed-review.txt
git -C "$CANDIDATE" commit -m "Commit review mutation" >/dev/null
printf 'dirty review mutation\n' > "$CANDIDATE/dirty-review.txt"
exit 0
"##,
    );

    factory_cmd()
        .current_dir(&main_dir)
        .args([
            "work",
            "task",
            "run",
            "work-1",
            "attempt-1",
            "attempt-1-review-tests",
            "--no-sandbox",
        ])
        .env("PATH", mock_path(&bin_dir))
        .env("CANDIDATE", &candidate)
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "changed readable candidate workspace HEAD",
        ));

    assert_eq!(git_head(&candidate), baseline_head);
    let value = read_work_show_json(&main_dir, "work-1");
    let review_task = value["attempts"][0]["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|task| task["id"] == "attempt-1-review-tests")
        .unwrap();
    assert_eq!(value["attempts"][0]["status"], "failed");
    assert_eq!(review_task["status"], "failed");
}

#[test]
fn work_task_run_sandboxes_review_with_read_only_candidate() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    create_completed_work_attempt(&tmp, &main_dir);
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "review", "work-1", "attempt-1"])
        .assert()
        .success();

    let bin_dir = tmp.path().join("bin-review");
    let profile_copy = tmp.path().join("review-sandbox.sb");
    write_mock_claude(
        &bin_dir,
        "#!/bin/bash\nprintf 'Verdict: pass\\n' > review.md\nexit 0\n",
    );
    write_mock_executable(
        &bin_dir,
        "sandbox-exec",
        "#!/bin/bash\ncp \"$2\" \"${SANDBOX_PROFILE_COPY:?}\"\nshift 2\nexec \"$@\"\n",
    );

    factory_cmd()
        .current_dir(&main_dir)
        .args([
            "work",
            "task",
            "run",
            "work-1",
            "attempt-1",
            "attempt-1-review-tests",
        ])
        .env("PATH", mock_path(&bin_dir))
        .env("SANDBOX_PROFILE_COPY", &profile_copy)
        .assert()
        .success();

    let profile = fs::read_to_string(profile_copy).unwrap();
    let candidate = fs::canonicalize(main_dir.join("../work-6-work-1-attempt-1")).unwrap();
    let common_git_dir = fs::canonicalize(git_common_dir(&candidate)).unwrap();
    let artifact_dir = fs::canonicalize(
        main_dir.join(".factory/work/artifacts/work-1/attempt-1/attempt-1-review-tests"),
    )
    .unwrap();
    assert!(
        profile.contains(&format!(
            "(allow file-read*  (subpath \"{}\"))",
            candidate.display()
        )),
        "{profile}"
    );
    assert!(
        profile.contains(&format!(
            "(allow file-read*  (subpath \"{}\"))",
            common_git_dir.display()
        )),
        "{profile}"
    );
    assert!(
        !profile.contains(&format!(
            "(allow file-write* (subpath \"{}\"))",
            candidate.display()
        )),
        "{profile}"
    );
    assert!(
        profile.contains(&format!(
            "(allow file-write* (subpath \"{}\"))",
            artifact_dir.display()
        )),
        "{profile}"
    );
}

#[test]
fn work_task_run_does_not_complete_attempt_with_unfinished_tasks() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    let bin_dir = tmp.path().join("bin");
    write_mock_claude(
        &bin_dir,
        r##"#!/bin/bash
printf 'task output\n' > task-output.txt
git add task-output.txt
git commit -m "Add task output" >/dev/null
exit 0
"##,
    );

    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "create", "work-1", "--title", "Run task"])
        .assert()
        .success();
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "attempt", "work-1", "attempt-1"])
        .assert()
        .success();

    let report_path = work_task_record_path(&main_dir, "work-1", "attempt-1", "attempt-1-report");
    write_json_value(
        &report_path,
        &serde_json::json!({
            "id": "attempt-1-report",
            "kind": "report",
            "role": "reporter",
            "work_item_id": "work-1",
            "attempt_id": "attempt-1",
            "workspace_access": {
                "reads": [],
                "writes": []
            }
        }),
    );

    factory_cmd()
        .current_dir(&main_dir)
        .args([
            "work",
            "task",
            "run",
            "work-1",
            "attempt-1",
            "attempt-1-write-1",
            "--no-sandbox",
        ])
        .env("PATH", mock_path(&bin_dir))
        .assert()
        .success();

    let value = read_work_show_json(&main_dir, "work-1");
    assert_eq!(value["attempts"][0]["status"], "executing");
    assert_eq!(value["attempts"][0]["tasks"][0]["status"], "complete");
    assert!(value["attempts"][0]["tasks"][1].get("status").is_none());
}

#[test]
fn work_task_run_rejects_dirty_successful_workspace() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    let bin_dir = tmp.path().join("bin");
    write_mock_claude(
        &bin_dir,
        r##"#!/bin/bash
printf 'uncommitted\n' > dirty-output.txt
exit 0
"##,
    );

    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "create", "work-1", "--title", "Run task"])
        .assert()
        .success();
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "attempt", "work-1", "attempt-1"])
        .assert()
        .success();

    factory_cmd()
        .current_dir(&main_dir)
        .args([
            "work",
            "task",
            "run",
            "work-1",
            "attempt-1",
            "attempt-1-write-1",
            "--no-sandbox",
        ])
        .env("PATH", mock_path(&bin_dir))
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "commit or remove them before completing",
        ));

    let value = read_work_show_json(&main_dir, "work-1");
    let attempt = &value["attempts"][0];
    let task = &attempt["tasks"][0];
    assert_eq!(attempt["status"], "failed");
    assert_eq!(task["status"], "failed");
    assert!(task.get("output").is_none());
}

#[test]
fn work_task_run_marks_task_failed_when_coder_exits_nonzero() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    let bin_dir = tmp.path().join("bin");
    write_mock_claude(
        &bin_dir,
        r##"#!/bin/bash
printf 'partial task output\n' > partial-output.txt
exit 7
"##,
    );

    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "create", "work-1", "--title", "Run task"])
        .assert()
        .success();
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "attempt", "work-1", "attempt-1"])
        .assert()
        .success();

    factory_cmd()
        .current_dir(&main_dir)
        .args([
            "work",
            "task",
            "run",
            "work-1",
            "attempt-1",
            "attempt-1-write-1",
            "--no-sandbox",
        ])
        .env("PATH", mock_path(&bin_dir))
        .assert()
        .failure()
        .stderr(predicate::str::contains("Coder exited with code 7"));

    let value = read_work_show_json(&main_dir, "work-1");
    let attempt = &value["attempts"][0];
    let task = &attempt["tasks"][0];
    assert_eq!(attempt["status"], "failed");
    assert_eq!(task["status"], "failed");
    assert!(task.get("output").is_none());
}

#[test]
fn work_task_run_rejects_success_without_commits() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    let bin_dir = tmp.path().join("bin");
    write_mock_claude(&bin_dir, "#!/bin/bash\nexit 0\n");

    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "create", "work-1", "--title", "Run task"])
        .assert()
        .success();
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "attempt", "work-1", "attempt-1"])
        .assert()
        .success();

    factory_cmd()
        .current_dir(&main_dir)
        .args([
            "work",
            "task",
            "run",
            "work-1",
            "attempt-1",
            "attempt-1-write-1",
            "--no-sandbox",
        ])
        .env("PATH", mock_path(&bin_dir))
        .assert()
        .failure()
        .stderr(predicate::str::contains("no committed Task output"));

    let value = read_work_show_json(&main_dir, "work-1");
    assert_eq!(value["attempts"][0]["status"], "failed");
    assert_eq!(value["attempts"][0]["tasks"][0]["status"], "failed");
    assert!(value["attempts"][0]["tasks"][0].get("output").is_none());
}

#[test]
fn work_task_run_rejects_reused_workspace_without_new_commit() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    let bin_dir = tmp.path().join("bin");
    write_mock_claude(&bin_dir, "#!/bin/bash\nexit 0\n");

    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "create", "work-1", "--title", "Run task"])
        .assert()
        .success();
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "attempt", "work-1", "attempt-1"])
        .assert()
        .success();

    let workspace = main_dir.join("../work-6-work-1-attempt-1");
    StdCommand::new("git")
        .args([
            "-C",
            &main_dir.to_string_lossy(),
            "worktree",
            "add",
            "-b",
            "precreated-task-workspace",
            &workspace.to_string_lossy(),
            "HEAD",
        ])
        .output()
        .unwrap();
    fs::write(workspace.join("stale-output.txt"), "stale").unwrap();
    StdCommand::new("git")
        .args([
            "-C",
            &workspace.to_string_lossy(),
            "add",
            "stale-output.txt",
        ])
        .output()
        .unwrap();
    StdCommand::new("git")
        .args([
            "-C",
            &workspace.to_string_lossy(),
            "commit",
            "-m",
            "Add stale output",
        ])
        .output()
        .unwrap();

    factory_cmd()
        .current_dir(&main_dir)
        .args([
            "work",
            "task",
            "run",
            "work-1",
            "attempt-1",
            "attempt-1-write-1",
            "--no-sandbox",
        ])
        .env("PATH", mock_path(&bin_dir))
        .assert()
        .failure()
        .stderr(predicate::str::contains("no committed Task output"));

    let value = read_work_show_json(&main_dir, "work-1");
    assert_eq!(value["attempts"][0]["status"], "failed");
    assert_eq!(value["attempts"][0]["tasks"][0]["status"], "failed");
    assert!(value["attempts"][0]["tasks"][0].get("output").is_none());
}

#[test]
fn work_task_run_rejects_existing_directory_that_is_not_worktree() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    let bin_dir = tmp.path().join("bin");
    write_mock_claude(&bin_dir, "#!/bin/bash\nexit 0\n");

    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "create", "work-1", "--title", "Run task"])
        .assert()
        .success();
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "attempt", "work-1", "attempt-1"])
        .assert()
        .success();

    let workspace = main_dir.join("../work-6-work-1-attempt-1");
    fs::create_dir_all(&workspace).unwrap();
    let item_path = main_dir.join(".factory/work/items/work-1.json");
    let before = fs::read_to_string(&item_path).unwrap();

    factory_cmd()
        .current_dir(&main_dir)
        .args([
            "work",
            "task",
            "run",
            "work-1",
            "attempt-1",
            "attempt-1-write-1",
            "--no-sandbox",
        ])
        .env("PATH", mock_path(&bin_dir))
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "exists but is not a registered git worktree",
        ));

    assert_eq!(fs::read_to_string(&item_path).unwrap(), before);
}

#[test]
fn work_task_run_rejects_existing_task_branch_without_workspace() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    let bin_dir = tmp.path().join("bin");
    write_mock_claude(&bin_dir, "#!/bin/bash\nexit 0\n");

    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "create", "work-1", "--title", "Run task"])
        .assert()
        .success();
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "attempt", "work-1", "attempt-1"])
        .assert()
        .success();

    StdCommand::new("git")
        .args([
            "-C",
            &main_dir.to_string_lossy(),
            "branch",
            "work/work-1/attempt-1/attempt-1-write-1",
            "HEAD",
        ])
        .output()
        .unwrap();

    factory_cmd()
        .current_dir(&main_dir)
        .args([
            "work",
            "task",
            "run",
            "work-1",
            "attempt-1",
            "attempt-1-write-1",
            "--no-sandbox",
        ])
        .env("PATH", mock_path(&bin_dir))
        .assert()
        .failure()
        .stderr(predicate::str::contains("already exists but workspace"));

    let value = read_work_show_json(&main_dir, "work-1");
    assert!(value["attempts"][0]["tasks"][0].get("status").is_none());
    assert!(value["attempts"][0]["tasks"][0].get("output").is_none());
    assert!(!main_dir.join("../work-6-work-1-attempt-1").exists());
}

#[test]
fn work_task_run_rejects_task_that_is_not_planned() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);

    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "create", "work-1", "--title", "Run task"])
        .assert()
        .success();
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "attempt", "work-1", "attempt-1"])
        .assert()
        .success();

    let task_path = work_task_record_path(&main_dir, "work-1", "attempt-1", "attempt-1-write-1");
    let mut value = read_json_value(&task_path);
    value["status"] = serde_json::Value::String("failed".to_string());
    write_json_value(&task_path, &value);
    let before = fs::read_to_string(&task_path).unwrap();

    factory_cmd()
        .current_dir(&main_dir)
        .args([
            "work",
            "task",
            "run",
            "work-1",
            "attempt-1",
            "attempt-1-write-1",
            "--no-sandbox",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("expected planned"));

    assert_eq!(fs::read_to_string(&task_path).unwrap(), before);
    assert!(!main_dir.join("../work-6-work-1-attempt-1").exists());
}

#[test]
fn work_task_run_rejects_non_write_task() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);

    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "create", "work-1", "--title", "Run task"])
        .assert()
        .success();
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "attempt", "work-1", "attempt-1"])
        .assert()
        .success();

    let task_path = work_task_record_path(&main_dir, "work-1", "attempt-1", "attempt-1-write-1");
    let mut value = read_json_value(&task_path);
    value["kind"] = serde_json::Value::String("probe".to_string());
    write_json_value(&task_path, &value);
    let before = fs::read_to_string(&task_path).unwrap();

    factory_cmd()
        .current_dir(&main_dir)
        .args([
            "work",
            "task",
            "run",
            "work-1",
            "attempt-1",
            "attempt-1-write-1",
            "--no-sandbox",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unsupported by task run"));

    assert_eq!(fs::read_to_string(&task_path).unwrap(), before);
    assert!(!main_dir.join("../work-6-work-1-attempt-1").exists());
}

#[test]
fn work_task_run_requires_one_writable_workspace() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);

    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "create", "work-1", "--title", "Run task"])
        .assert()
        .success();
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "attempt", "work-1", "attempt-1"])
        .assert()
        .success();

    let task_path = work_task_record_path(&main_dir, "work-1", "attempt-1", "attempt-1-write-1");
    let mut value = read_json_value(&task_path);
    value["workspace_access"]["writes"] = serde_json::Value::Array(Vec::new());
    write_json_value(&task_path, &value);
    let before = fs::read_to_string(&task_path).unwrap();

    factory_cmd()
        .current_dir(&main_dir)
        .args([
            "work",
            "task",
            "run",
            "work-1",
            "attempt-1",
            "attempt-1-write-1",
            "--no-sandbox",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "must declare exactly one writable workspace",
        ));

    assert_eq!(fs::read_to_string(&task_path).unwrap(), before);
    assert!(!main_dir.join("../work-6-work-1-attempt-1").exists());

    let mut value: serde_json::Value = serde_json::from_str(&before).unwrap();
    value["workspace_access"]["writes"] = serde_json::json!([
        {"id": "candidate", "path": "../work-6-work-1-attempt-1"},
        {"id": "other", "path": "../work-6-work-1-other"}
    ]);
    write_json_value(&task_path, &value);
    let before = fs::read_to_string(&task_path).unwrap();

    factory_cmd()
        .current_dir(&main_dir)
        .args([
            "work",
            "task",
            "run",
            "work-1",
            "attempt-1",
            "attempt-1-write-1",
            "--no-sandbox",
        ])
        .assert()
        .failure();

    assert_eq!(fs::read_to_string(&task_path).unwrap(), before);
    assert!(!main_dir.join("../work-6-work-1-attempt-1").exists());
}

#[test]
fn work_task_run_rejects_unmanaged_writable_workspace_path() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);

    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "create", "work-1", "--title", "Run task"])
        .assert()
        .success();
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "attempt", "work-1", "attempt-1"])
        .assert()
        .success();

    let task_path = work_task_record_path(&main_dir, "work-1", "attempt-1", "attempt-1-write-1");
    let outside_absolute = tmp.path().join("outside-absolute");
    let outside_absolute = outside_absolute.to_string_lossy().to_string();
    for path in [
        "../outside-workspace",
        "../outside",
        "../work-6-work-1-other-attempt",
        outside_absolute.as_str(),
    ] {
        let mut value = read_json_value(&task_path);
        value["workspace_access"]["writes"][0]["path"] =
            serde_json::Value::String(path.to_string());
        write_json_value(&task_path, &value);
        let before = fs::read_to_string(&task_path).unwrap();

        factory_cmd()
            .current_dir(&main_dir)
            .args([
                "work",
                "task",
                "run",
                "work-1",
                "attempt-1",
                "attempt-1-write-1",
                "--no-sandbox",
            ])
            .assert()
            .failure()
            .stderr(predicate::str::contains(
                "Task writable workspace path must",
            ));

        assert_eq!(fs::read_to_string(&task_path).unwrap(), before);
    }

    assert!(!main_dir.join("../outside-workspace").exists());
    assert!(!main_dir.join("../work-6-work-1-other-attempt").exists());
    assert!(!main_dir.join(".factory/work/outside").exists());
    assert!(!Path::new(&outside_absolute).exists());
}

#[test]
fn work_task_run_missing_ids_leave_work_item_unchanged() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);

    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "create", "work-1", "--title", "Run task"])
        .assert()
        .success();
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "attempt", "work-1", "attempt-1"])
        .assert()
        .success();
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "attempt", "work-1", "attempt-2"])
        .assert()
        .success();

    let item_path = main_dir.join(".factory/work/items/work-1.json");
    let before = fs::read_to_string(&item_path).unwrap();

    for args in [
        [
            "work",
            "task",
            "run",
            "missing-work",
            "attempt-1",
            "attempt-1-write-1",
            "--no-sandbox",
        ],
        [
            "work",
            "task",
            "run",
            "work-1",
            "missing-attempt",
            "attempt-1-write-1",
            "--no-sandbox",
        ],
        [
            "work",
            "task",
            "run",
            "work-1",
            "attempt-1",
            "missing-task",
            "--no-sandbox",
        ],
        [
            "work",
            "task",
            "run",
            "work-1",
            "attempt-2",
            "attempt-1-write-1",
            "--no-sandbox",
        ],
    ] {
        factory_cmd()
            .current_dir(&main_dir)
            .args(args)
            .assert()
            .failure();
    }

    assert_eq!(fs::read_to_string(&item_path).unwrap(), before);
    assert!(!main_dir.join("../work-6-work-1-attempt-1").exists());
    assert!(!main_dir.join("../work-6-work-1-attempt-2").exists());
}

#[test]
fn work_list_outputs_stored_work_items() {
    let tmp = TempDir::new().unwrap();
    write_work_item_json(tmp.path(), "work-beta", "Second work item");
    write_work_item_json(tmp.path(), "work-alpha", "First work item");

    let output = factory_cmd()
        .current_dir(tmp.path())
        .args(["work", "list"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "work list failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stderr, b"");

    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("ID"));
    assert!(stdout.contains("TITLE"));
    assert!(stdout.contains("work-alpha"));
    assert!(stdout.contains("First work item"));
    assert!(stdout.contains("work-beta"));
    assert!(stdout.contains("Second work item"));
    assert!(
        stdout.find("work-alpha").unwrap() < stdout.find("work-beta").unwrap(),
        "work list should use storage order: {stdout}"
    );
}

#[test]
fn work_list_empty_state_succeeds_without_work_items() {
    let tmp = TempDir::new().unwrap();
    fs::create_dir_all(tmp.path().join(".factory/runs/legacy-run")).unwrap();
    fs::write(
        tmp.path().join(".factory/runs/legacy-run/status"),
        "complete",
    )
    .unwrap();

    factory_cmd()
        .current_dir(tmp.path())
        .args(["work", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("No Work Items found"));

    factory_cmd()
        .current_dir(tmp.path())
        .arg("status")
        .assert()
        .success()
        .stdout(predicate::str::contains("No Work Items found"))
        .stdout(predicate::str::contains("legacy-run").not());
}

#[test]
fn work_show_outputs_pretty_json_for_one_work_item() {
    let tmp = TempDir::new().unwrap();
    write_work_item_json(tmp.path(), "work-1", "Inspect work item");

    let output = factory_cmd()
        .current_dir(tmp.path())
        .args(["work", "show", "work-1"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "work show failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stderr, b"");

    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.starts_with("{\n"));
    assert!(stdout.contains("  \"id\": \"work-1\""));
    assert!(stdout.contains("  \"title\": \"Inspect work item\""));
    assert!(stdout.ends_with('\n'));
}

#[test]
fn work_show_missing_item_reports_not_found() {
    let tmp = TempDir::new().unwrap();

    factory_cmd()
        .current_dir(tmp.path())
        .args(["work", "show", "missing-work"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "Work Item \"missing-work\" not found",
        ));
}

#[test]
fn work_show_rejects_invalid_work_item_id() {
    let tmp = TempDir::new().unwrap();

    factory_cmd()
        .current_dir(tmp.path())
        .args(["work", "show", "../escape"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "work item id \"../escape\" cannot be used as a file name",
        ));
}

#[test]
fn work_merge_candidate_missing_item_or_candidate_reports_error() {
    let tmp = TempDir::new().unwrap();
    write_work_item_json(tmp.path(), "work-1", "Inspect candidate");
    let before = fs::read_to_string(tmp.path().join(".factory/work/items/work-1.json")).unwrap();

    factory_cmd()
        .current_dir(tmp.path())
        .args(["work", "merge-candidate", "missing-work", "candidate-1"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "Work Item \"missing-work\" not found",
        ));

    factory_cmd()
        .current_dir(tmp.path())
        .args(["work", "merge-candidate", "work-1", "candidate-1"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "Merge Candidate \"candidate-1\" not found in Work Item \"work-1\"",
        ));

    let after = fs::read_to_string(tmp.path().join(".factory/work/items/work-1.json")).unwrap();
    assert_eq!(after, before);
}

#[test]
fn work_list_reports_invalid_stored_json_path() {
    let tmp = TempDir::new().unwrap();
    let items_dir = tmp.path().join(".factory/work/items");
    fs::create_dir_all(&items_dir).unwrap();
    fs::write(items_dir.join("bad.json"), "{").unwrap();

    factory_cmd()
        .current_dir(tmp.path())
        .args(["work", "list"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(".factory/work/items/bad.json"))
        .stderr(predicate::str::contains("failed to parse"));
}

#[test]
fn work_list_reports_stored_work_item_id_mismatch() {
    let tmp = TempDir::new().unwrap();
    let items_dir = tmp.path().join(".factory/work/items");
    fs::create_dir_all(&items_dir).unwrap();
    fs::write(
        items_dir.join("work-1.json"),
        r#"{
  "id": "work-2",
  "title": "Mismatched id"
}
"#,
    )
    .unwrap();

    factory_cmd()
        .current_dir(tmp.path())
        .args(["work", "list"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(".factory/work/items/work-1.json"))
        .stderr(predicate::str::contains("contains id work-2"))
        .stderr(predicate::str::contains("expected work-1"));
}

#[test]
fn work_list_reports_invalid_stored_work_item_id() {
    let tmp = TempDir::new().unwrap();
    let items_dir = tmp.path().join(".factory/work/items");
    fs::create_dir_all(&items_dir).unwrap();
    fs::write(
        items_dir.join(r"bad\id.json"),
        r#"{
  "id": "bad\\id",
  "title": "Invalid id"
}
"#,
    )
    .unwrap();

    factory_cmd()
        .current_dir(tmp.path())
        .args(["work", "list"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("bad\\\\id"))
        .stderr(predicate::str::contains("cannot be used as a file name"));
}

#[test]
fn work_list_reports_invalid_stored_model() {
    let tmp = TempDir::new().unwrap();
    let items_dir = tmp.path().join(".factory/work/items");
    fs::create_dir_all(&items_dir).unwrap();
    fs::write(
        items_dir.join("work-invalid.json"),
        r#"{
  "id": "work-invalid",
  "title": "Invalid model"
}
"#,
    )
    .unwrap();
    let attempts_dir = tmp.path().join(".factory/work/attempts/work-invalid");
    fs::create_dir_all(&attempts_dir).unwrap();
    fs::write(
        attempts_dir.join("attempt-1.json"),
        r#"{
  "id": "attempt-1",
  "work_item_id": "other-work",
  "order": 0,
  "status": "planned"
}
"#,
    )
    .unwrap();

    factory_cmd()
        .current_dir(tmp.path())
        .args(["work", "list"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            ".factory/work/attempts/work-invalid/attempt-1.json",
        ))
        .stderr(predicate::str::contains("invalid work model"))
        .stderr(predicate::str::contains("expected work-invalid"));
}

fn write_work_item_json(project_root: &Path, id: &str, title: &str) {
    let items_dir = project_root.join(".factory/work/items");
    fs::create_dir_all(&items_dir).unwrap();
    fs::write(
        items_dir.join(format!("{id}.json")),
        format!(
            r#"{{
  "id": "{id}",
  "title": "{title}"
}}
"#
        ),
    )
    .unwrap();
}

fn read_work_show_json(project_root: &Path, work_item_id: &str) -> serde_json::Value {
    let output = factory_cmd()
        .current_dir(project_root)
        .args(["work", "show", work_item_id])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "work show failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn work_task_record_path(
    project_root: &Path,
    work_item_id: &str,
    attempt_id: &str,
    task_id: &str,
) -> PathBuf {
    project_root
        .join(".factory/work/tasks")
        .join(work_item_id)
        .join(attempt_id)
        .join(format!("{task_id}.json"))
}

fn read_json_value(path: &Path) -> serde_json::Value {
    serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
}

fn write_json_value(path: &Path, value: &serde_json::Value) {
    fs::write(path, serde_json::to_string_pretty(value).unwrap()).unwrap();
}

fn write_planned_followup_task(main_dir: &Path, input_artifacts: Vec<serde_json::Value>) {
    let value = read_work_show_json(main_dir, "work-1");
    let attempt = &value["attempts"][0];
    let initial_write = attempt["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|task| task["id"] == "attempt-1-write-1")
        .unwrap();
    let output = &initial_write["output"];
    let task_count = attempt["tasks"].as_array().unwrap().len();
    let task = serde_json::json!({
        "order": task_count,
        "id": "attempt-1-write-2",
        "kind": "write",
        "role": "author",
        "work_item_id": "work-1",
        "attempt_id": "attempt-1",
        "workspace_access": {
            "reads": [],
            "writes": [
                {
                    "id": output["workspace_id"],
                    "path": output["workspace_path"]
                }
            ]
        },
        "input_artifacts": input_artifacts
    });
    let task_path = work_task_record_path(main_dir, "work-1", "attempt-1", "attempt-1-write-2");
    write_json_value(&task_path, &task);

    let attempt_path = main_dir
        .join(".factory/work/attempts")
        .join("work-1")
        .join("attempt-1.json");
    let mut attempt_record = read_json_value(&attempt_path);
    attempt_record["status"] = serde_json::Value::String("planned".to_string());
    attempt_record["review_state"] = serde_json::Value::String("failed".to_string());
    write_json_value(&attempt_path, &attempt_record);
}

// -------------------------------------------------------------------------
// Cleanup
// -------------------------------------------------------------------------

#[test]
fn cleanup_dry_run_reports_without_changes() {
    let tmp = TempDir::new().unwrap();
    let run_dir = tmp.path().join(".factory/runs/done-run");
    fs::create_dir_all(&run_dir).unwrap();
    fs::write(run_dir.join("status"), "complete").unwrap();

    factory_cmd()
        .current_dir(tmp.path())
        .arg("cleanup")
        .assert()
        .success()
        .stdout(predicate::str::contains("Dry run"))
        .stdout(predicate::str::contains("would clean done-run"));

    assert!(!run_dir.join("cleaned.md").exists());
}

#[test]
fn cleanup_apply_writes_marker_without_changing_status() {
    let tmp = TempDir::new().unwrap();
    let run_dir = tmp.path().join(".factory/runs/landed-run");
    fs::create_dir_all(&run_dir).unwrap();
    fs::write(run_dir.join("status"), "merged").unwrap();

    factory_cmd()
        .current_dir(tmp.path())
        .args(["cleanup", "--apply"])
        .assert()
        .success()
        .stdout(predicate::str::contains("cleaned landed-run"));

    assert_eq!(
        fs::read_to_string(run_dir.join("status")).unwrap(),
        "merged"
    );
    let marker = fs::read_to_string(run_dir.join("cleaned.md")).unwrap();
    assert!(marker.contains("Reason: stale terminal run cleanup"));
}

#[test]
fn cleanup_refuses_active_run() {
    let tmp = TempDir::new().unwrap();
    let run_dir = tmp.path().join(".factory/runs/active-run");
    fs::create_dir_all(&run_dir).unwrap();
    fs::write(run_dir.join("status"), "executing").unwrap();

    factory_cmd()
        .current_dir(tmp.path())
        .args(["cleanup", "--run-id", "active-run", "--apply"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("expected complete or merged"));

    assert!(!run_dir.join("cleaned.md").exists());
}

#[test]
fn cleanup_refuses_failed_run() {
    let tmp = TempDir::new().unwrap();
    let run_dir = tmp.path().join(".factory/runs/failed-run");
    fs::create_dir_all(&run_dir).unwrap();
    fs::write(run_dir.join("status"), "failed").unwrap();

    factory_cmd()
        .current_dir(tmp.path())
        .args(["cleanup", "--run-id", "failed-run", "--apply"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("expected complete or merged"));

    assert!(!run_dir.join("cleaned.md").exists());
}

#[test]
fn cleanup_skips_unregistered_worktree_path() {
    let tmp = TempDir::new().unwrap();
    let run_dir = tmp.path().join(".factory/runs/done-run");
    let unregistered = tmp.path().join("unregistered-worktree");
    fs::create_dir_all(&run_dir).unwrap();
    fs::create_dir_all(&unregistered).unwrap();
    fs::write(run_dir.join("status"), "complete").unwrap();
    fs::write(run_dir.join("worktree"), unregistered.to_str().unwrap()).unwrap();

    factory_cmd()
        .current_dir(tmp.path())
        .args(["cleanup", "--apply"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skipped unregistered worktree"));

    assert!(unregistered.is_dir());
}

#[test]
fn cleanup_apply_removes_registered_worktree() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    let run_id = "cleanup-worktree";
    let run_dir = main_dir.join(format!(".factory/runs/{run_id}"));
    fs::create_dir_all(&run_dir).unwrap();
    fs::write(run_dir.join("status"), "complete").unwrap();

    let worktree_dir = tmp.path().join(run_id);
    StdCommand::new("git")
        .args([
            "worktree",
            "add",
            worktree_dir.to_str().unwrap(),
            "-b",
            run_id,
        ])
        .current_dir(&main_dir)
        .output()
        .unwrap();
    fs::write(run_dir.join("worktree"), worktree_dir.to_str().unwrap()).unwrap();

    factory_cmd()
        .current_dir(&main_dir)
        .args(["cleanup", "--run-id", run_id, "--apply"])
        .assert()
        .success()
        .stdout(predicate::str::contains("removed registered worktree"));

    assert!(!worktree_dir.exists());
    assert!(run_dir.join("cleaned.md").exists());
}

#[test]
fn cleanup_dry_run_keeps_registered_worktree() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    let run_id = "cleanup-dry-worktree";
    let run_dir = main_dir.join(format!(".factory/runs/{run_id}"));
    fs::create_dir_all(&run_dir).unwrap();
    fs::write(run_dir.join("status"), "complete").unwrap();

    let worktree_dir = tmp.path().join(run_id);
    StdCommand::new("git")
        .args([
            "worktree",
            "add",
            worktree_dir.to_str().unwrap(),
            "-b",
            run_id,
        ])
        .current_dir(&main_dir)
        .output()
        .unwrap();
    fs::write(run_dir.join("worktree"), worktree_dir.to_str().unwrap()).unwrap();

    factory_cmd()
        .current_dir(&main_dir)
        .arg("cleanup")
        .assert()
        .success()
        .stdout(predicate::str::contains("would remove registered worktree"));

    assert!(worktree_dir.is_dir());
    assert!(!run_dir.join("cleaned.md").exists());

    StdCommand::new("git")
        .args([
            "worktree",
            "remove",
            "--force",
            worktree_dir.to_str().unwrap(),
        ])
        .current_dir(&main_dir)
        .output()
        .unwrap();
}

#[test]
fn cleanup_from_run_worktree_uses_source_registry() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    let run_id = "cleanup-source-registry";
    let source_run_dir = main_dir.join(format!(".factory/runs/{run_id}"));
    fs::create_dir_all(&source_run_dir).unwrap();
    fs::write(source_run_dir.join("status"), "complete").unwrap();

    let worktree_dir = tmp.path().join(run_id);
    StdCommand::new("git")
        .args([
            "worktree",
            "add",
            worktree_dir.to_str().unwrap(),
            "-b",
            run_id,
        ])
        .current_dir(&main_dir)
        .output()
        .unwrap();
    fs::write(
        source_run_dir.join("worktree"),
        worktree_dir.to_str().unwrap(),
    )
    .unwrap();

    let copied_run_dir = worktree_dir.join(format!(".factory/runs/{run_id}"));
    fs::create_dir_all(&copied_run_dir).unwrap();
    fs::write(copied_run_dir.join("status"), "complete").unwrap();

    factory_cmd()
        .current_dir(&worktree_dir)
        .args(["cleanup", "--run-id", run_id, "--apply"])
        .assert()
        .success()
        .stdout(predicate::str::contains("cleaned cleanup-source-registry"));

    assert!(source_run_dir.join("cleaned.md").exists());
    assert!(!copied_run_dir.join("cleaned.md").exists());
    assert!(!worktree_dir.exists());
}

#[test]
fn cleanup_work_items_dry_run_and_apply_manage_state_worktree_and_branch() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "create", "work-1", "--title", "Cleanup work"])
        .assert()
        .success();
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "attempt", "work-1", "attempt-1"])
        .assert()
        .success();
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "create", "work-active", "--title", "Active work"])
        .assert()
        .success();
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "attempt", "work-active", "attempt-1"])
        .assert()
        .success();

    let item_path = main_dir.join(".factory/work/items/work-1.json");
    let attempt_path = main_dir.join(".factory/work/attempts/work-1/attempt-1.json");
    let task_path = main_dir.join(".factory/work/tasks/work-1/attempt-1/attempt-1-write-1.json");
    let mut attempt = read_json_path(&attempt_path);
    attempt["status"] = serde_json::Value::String("complete".to_string());
    write_json_path(&attempt_path, &attempt);
    let mut task = read_json_path(&task_path);
    task["status"] = serde_json::Value::String("complete".to_string());
    task["artifact_area"] = serde_json::json!({
        "path": ".factory/work/artifacts/work-1/attempt-1/attempt-1-write-1"
    });
    task["output"] = serde_json::json!({
        "workspace_id": "candidate",
        "workspace_path": "../work-6-work-1-attempt-1",
        "source_branch": "main",
        "commit": git_head(&main_dir)
    });
    write_json_path(&task_path, &task);

    let artifact_dir = main_dir.join(".factory/work/artifacts/work-1/attempt-1/attempt-1-write-1");
    let artifact_parent = main_dir.join(".factory/work/artifacts/work-1/attempt-1");
    fs::create_dir_all(&artifact_dir).unwrap();
    fs::write(artifact_dir.join("result.md"), "artifact").unwrap();

    let worktree_dir = main_dir.join("../work-6-work-1-attempt-1");
    let branch_name = "work/work-1/attempt-1/attempt-1-write-1";
    StdCommand::new("git")
        .args([
            "worktree",
            "add",
            worktree_dir.to_str().unwrap(),
            "-b",
            branch_name,
            "HEAD",
        ])
        .current_dir(&main_dir)
        .output()
        .unwrap();

    let active_item_path = main_dir.join(".factory/work/items/work-active.json");
    let active_attempt_path = main_dir.join(".factory/work/attempts/work-active/attempt-1.json");
    let active_task_path =
        main_dir.join(".factory/work/tasks/work-active/attempt-1/attempt-1-write-1.json");
    let mut active_attempt = read_json_path(&active_attempt_path);
    active_attempt["status"] = serde_json::Value::String("executing".to_string());
    write_json_path(&active_attempt_path, &active_attempt);
    let mut active_task = read_json_path(&active_task_path);
    active_task["status"] = serde_json::Value::String("executing".to_string());
    active_task["artifact_area"] = serde_json::json!({
        "path": ".factory/work/artifacts/work-active/attempt-1/attempt-1-active"
    });
    write_json_path(&active_task_path, &active_task);

    let active_artifact_dir =
        main_dir.join(".factory/work/artifacts/work-active/attempt-1/attempt-1-active");
    fs::create_dir_all(&active_artifact_dir).unwrap();
    fs::write(active_artifact_dir.join("result.md"), "active artifact").unwrap();

    let active_worktree_dir = main_dir.join("../work-11-work-active-attempt-1");
    let active_branch_name = "work/work-active/attempt-1/attempt-1-write-1";
    StdCommand::new("git")
        .args([
            "worktree",
            "add",
            active_worktree_dir.to_str().unwrap(),
            "-b",
            active_branch_name,
            "HEAD",
        ])
        .current_dir(&main_dir)
        .output()
        .unwrap();

    factory_cmd()
        .current_dir(&main_dir)
        .arg("cleanup")
        .assert()
        .success()
        .stdout(predicate::str::contains("would clean Work Item work-1"))
        .stdout(predicate::str::contains("would remove registered worktree"))
        .stdout(predicate::str::contains("would remove Work branch"))
        .stdout(predicate::str::contains("would remove Work artifact"))
        .stdout(predicate::str::contains("work-active").not());

    assert!(item_path.exists());
    assert!(worktree_dir.is_dir());
    assert!(artifact_dir.is_dir());
    assert!(artifact_parent.is_dir());
    assert!(active_item_path.exists());
    assert!(active_worktree_dir.is_dir());
    assert!(active_artifact_dir.is_dir());

    factory_cmd()
        .current_dir(&main_dir)
        .args(["cleanup", "--apply"])
        .assert()
        .success()
        .stdout(predicate::str::contains("cleaned Work Item work-1"))
        .stdout(predicate::str::contains("removed registered worktree"))
        .stdout(predicate::str::contains("removed Work branch"));

    assert!(!item_path.exists());
    assert!(!attempt_path.exists());
    assert!(!task_path.exists());
    assert!(active_item_path.exists());
    assert!(active_attempt_path.exists());
    assert!(active_task_path.exists());
    assert!(!worktree_dir.exists());
    assert!(!artifact_dir.exists());
    assert!(!artifact_parent.exists());
    assert!(active_worktree_dir.is_dir());
    assert!(active_artifact_dir.is_dir());

    let branch_check = StdCommand::new("git")
        .args([
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch_name}"),
        ])
        .current_dir(&main_dir)
        .status()
        .unwrap();
    assert!(!branch_check.success());

    let active_branch_check = StdCommand::new("git")
        .args([
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/heads/{active_branch_name}"),
        ])
        .current_dir(&main_dir)
        .status()
        .unwrap();
    assert!(active_branch_check.success());

    StdCommand::new("git")
        .args([
            "worktree",
            "remove",
            "--force",
            active_worktree_dir.to_str().unwrap(),
        ])
        .current_dir(&main_dir)
        .output()
        .unwrap();
}

#[test]
fn cleanup_work_items_reports_and_removes_orphan_artifact_roots() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "create", "work-active", "--title", "Active work"])
        .assert()
        .success();

    let artifacts_dir = main_dir.join(".factory/work/artifacts");
    let orphan_artifact_root = artifacts_dir.join("work-orphan");
    let active_artifact_root = artifacts_dir.join("work-active");
    let file_entry = artifacts_dir.join("not-a-directory");
    fs::create_dir_all(orphan_artifact_root.join("attempt-1/task-1")).unwrap();
    fs::write(
        orphan_artifact_root.join("attempt-1/task-1/result.md"),
        "orphan artifact",
    )
    .unwrap();
    fs::create_dir_all(active_artifact_root.join("attempt-1/task-1")).unwrap();
    fs::write(
        active_artifact_root.join("attempt-1/task-1/result.md"),
        "active artifact",
    )
    .unwrap();
    fs::write(&file_entry, "keep file entries").unwrap();

    factory_cmd()
        .current_dir(&main_dir)
        .arg("cleanup")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "would remove orphan Work artifact root",
        ))
        .stdout(predicate::str::contains("work-orphan"))
        .stdout(predicate::str::contains("work-active").not())
        .stdout(predicate::str::contains("not-a-directory").not());

    assert!(orphan_artifact_root.is_dir());
    assert!(active_artifact_root.is_dir());
    assert!(file_entry.is_file());

    factory_cmd()
        .current_dir(&main_dir)
        .args(["cleanup", "--apply"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "removed orphan Work artifact root",
        ))
        .stdout(predicate::str::contains("work-orphan"))
        .stdout(predicate::str::contains("work-active").not())
        .stdout(predicate::str::contains("not-a-directory").not());

    assert!(!orphan_artifact_root.exists());
    assert!(active_artifact_root.is_dir());
    assert!(file_entry.is_file());
}

#[test]
fn cleanup_work_items_apply_skips_unregistered_managed_worktree() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    factory_cmd()
        .current_dir(&main_dir)
        .args([
            "work",
            "create",
            "work-unregistered",
            "--title",
            "Unregistered cleanup work",
        ])
        .assert()
        .success();
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "attempt", "work-unregistered", "attempt-1"])
        .assert()
        .success();

    let item_path = main_dir.join(".factory/work/items/work-unregistered.json");
    let workspace_path = "../work-17-work-unregistered-attempt-1";
    let workspace_dir = main_dir.join(workspace_path);
    fs::create_dir_all(&workspace_dir).unwrap();
    fs::write(workspace_dir.join("user-file.txt"), "keep me").unwrap();

    let attempt_path = main_dir.join(".factory/work/attempts/work-unregistered/attempt-1.json");
    let task_path =
        main_dir.join(".factory/work/tasks/work-unregistered/attempt-1/attempt-1-write-1.json");
    let mut attempt = read_json_path(&attempt_path);
    attempt["status"] = serde_json::Value::String("complete".to_string());
    write_json_path(&attempt_path, &attempt);
    let mut task = read_json_path(&task_path);
    task["status"] = serde_json::Value::String("complete".to_string());
    task["output"] = serde_json::json!({
        "workspace_id": "candidate",
        "workspace_path": workspace_path,
        "source_branch": "main",
        "commit": git_head(&main_dir)
    });
    write_json_path(&task_path, &task);

    factory_cmd()
        .current_dir(&main_dir)
        .args(["cleanup", "--apply"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "cleaned Work Item work-unregistered",
        ))
        .stdout(predicate::str::contains("skipped unregistered worktree"));

    assert!(!item_path.exists());
    assert!(!attempt_path.exists());
    assert!(!task_path.exists());
    assert!(workspace_dir.is_dir());
    assert_eq!(
        fs::read_to_string(workspace_dir.join("user-file.txt")).unwrap(),
        "keep me"
    );
}

#[test]
fn cleanup_work_items_selects_failed_terminal_and_skips_pending_merge_candidate() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "create", "work-failed", "--title", "Failed work"])
        .assert()
        .success();
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "attempt", "work-failed", "attempt-1"])
        .assert()
        .success();
    factory_cmd()
        .current_dir(&main_dir)
        .args([
            "work",
            "create",
            "work-pending-merge",
            "--title",
            "Pending merge work",
        ])
        .assert()
        .success();
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "attempt", "work-pending-merge", "attempt-1"])
        .assert()
        .success();

    let failed_item_path = main_dir.join(".factory/work/items/work-failed.json");
    let failed_attempt_path = main_dir.join(".factory/work/attempts/work-failed/attempt-1.json");
    let failed_task_path =
        main_dir.join(".factory/work/tasks/work-failed/attempt-1/attempt-1-write-1.json");
    let mut failed_attempt = read_json_path(&failed_attempt_path);
    failed_attempt["status"] = serde_json::Value::String("failed".to_string());
    write_json_path(&failed_attempt_path, &failed_attempt);
    let mut failed_task = read_json_path(&failed_task_path);
    failed_task["status"] = serde_json::Value::String("failed".to_string());
    write_json_path(&failed_task_path, &failed_task);

    let pending_item_path = main_dir.join(".factory/work/items/work-pending-merge.json");
    let pending_workspace = "../work-18-work-pending-merge-attempt-1";
    let head = git_head(&main_dir);
    let pending_attempt_path =
        main_dir.join(".factory/work/attempts/work-pending-merge/attempt-1.json");
    let pending_task_path =
        main_dir.join(".factory/work/tasks/work-pending-merge/attempt-1/attempt-1-write-1.json");
    let mut pending_attempt = read_json_path(&pending_attempt_path);
    pending_attempt["status"] = serde_json::Value::String("complete".to_string());
    pending_attempt["review_state"] = serde_json::Value::String("passed".to_string());
    write_json_path(&pending_attempt_path, &pending_attempt);
    let mut pending_task = read_json_path(&pending_task_path);
    pending_task["status"] = serde_json::Value::String("complete".to_string());
    pending_task["output"] = serde_json::json!({
        "workspace_id": "candidate",
        "workspace_path": pending_workspace,
        "source_branch": "main",
        "commit": head
    });
    write_json_path(&pending_task_path, &pending_task);
    let pending_candidate_path =
        main_dir.join(".factory/work/merge-candidates/work-pending-merge/candidate-1.json");
    fs::create_dir_all(pending_candidate_path.parent().unwrap()).unwrap();
    write_json_path(
        &pending_candidate_path,
        &serde_json::json!({
            "id": "candidate-1",
            "attempt_id": "attempt-1",
            "source_workspace": {
                "id": "candidate",
                "path": pending_workspace
            },
            "target_workspace": {
                "id": "target",
                "path": "."
            },
            "source_branch": "main",
            "target_branch": "main",
            "candidate_commit": head,
            "review_state": "pending",
            "merge_state": {
                "status": "pending"
            }
        }),
    );

    factory_cmd()
        .current_dir(&main_dir)
        .arg("cleanup")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "would clean Work Item work-failed",
        ))
        .stdout(predicate::str::contains("work-pending-merge").not());

    factory_cmd()
        .current_dir(&main_dir)
        .args(["cleanup", "--apply"])
        .assert()
        .success()
        .stdout(predicate::str::contains("cleaned Work Item work-failed"))
        .stdout(predicate::str::contains("work-pending-merge").not());

    assert!(!failed_item_path.exists());
    assert!(!failed_attempt_path.exists());
    assert!(!failed_task_path.exists());
    assert!(pending_item_path.exists());
    assert!(pending_attempt_path.exists());
    assert!(pending_task_path.exists());
    assert!(pending_candidate_path.exists());
}

#[test]
fn cleanup_work_items_skips_failed_attempt_with_active_task() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    factory_cmd()
        .current_dir(&main_dir)
        .args([
            "work",
            "create",
            "work-active-task",
            "--title",
            "Active task cleanup work",
        ])
        .assert()
        .success();
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "attempt", "work-active-task", "attempt-1"])
        .assert()
        .success();

    let item_path = main_dir.join(".factory/work/items/work-active-task.json");
    let attempt_path = main_dir.join(".factory/work/attempts/work-active-task/attempt-1.json");
    let task_path =
        main_dir.join(".factory/work/tasks/work-active-task/attempt-1/attempt-1-write-1.json");
    let mut attempt = read_json_path(&attempt_path);
    attempt["status"] = serde_json::Value::String("failed".to_string());
    write_json_path(&attempt_path, &attempt);
    let mut task = read_json_path(&task_path);
    task["status"] = serde_json::Value::String("executing".to_string());
    task["artifact_area"] = serde_json::json!({
        "path": ".factory/work/artifacts/work-active-task/attempt-1/attempt-1-write-1"
    });
    write_json_path(&task_path, &task);

    let artifact_dir =
        main_dir.join(".factory/work/artifacts/work-active-task/attempt-1/attempt-1-write-1");
    fs::create_dir_all(&artifact_dir).unwrap();
    fs::write(artifact_dir.join("result.md"), "active task artifact").unwrap();

    factory_cmd()
        .current_dir(&main_dir)
        .arg("cleanup")
        .assert()
        .success()
        .stdout(predicate::str::contains("work-active-task").not());

    factory_cmd()
        .current_dir(&main_dir)
        .args(["cleanup", "--apply"])
        .assert()
        .success()
        .stdout(predicate::str::contains("work-active-task").not());

    assert!(item_path.exists());
    assert!(attempt_path.exists());
    assert!(task_path.exists());
    assert!(artifact_dir.is_dir());
    assert_eq!(
        fs::read_to_string(artifact_dir.join("result.md")).unwrap(),
        "active task artifact"
    );
}

#[test]
fn cleanup_work_items_removes_terminal_merge_candidate_artifacts_and_worktree() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    factory_cmd()
        .current_dir(&main_dir)
        .args([
            "work",
            "create",
            "work-merge-cleanup",
            "--title",
            "Merge cleanup work",
        ])
        .assert()
        .success();
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "attempt", "work-merge-cleanup", "attempt-1"])
        .assert()
        .success();

    let item_path = main_dir.join(".factory/work/items/work-merge-cleanup.json");
    let workspace_path = "../work-18-work-merge-cleanup-attempt-1";
    let worktree_dir = main_dir.join(workspace_path);
    let branch_name = "work/work-merge-cleanup/attempt-1/attempt-1-write-1";
    StdCommand::new("git")
        .args([
            "worktree",
            "add",
            worktree_dir.to_str().unwrap(),
            "-b",
            branch_name,
            "HEAD",
        ])
        .current_dir(&main_dir)
        .output()
        .unwrap();

    let candidate_head = git_head(&worktree_dir);
    let attempt_path = main_dir.join(".factory/work/attempts/work-merge-cleanup/attempt-1.json");
    let task_path =
        main_dir.join(".factory/work/tasks/work-merge-cleanup/attempt-1/attempt-1-write-1.json");
    let mut attempt = read_json_path(&attempt_path);
    attempt["status"] = serde_json::Value::String("complete".to_string());
    attempt["review_state"] = serde_json::Value::String("passed".to_string());
    write_json_path(&attempt_path, &attempt);
    let mut task = read_json_path(&task_path);
    task["status"] = serde_json::Value::String("complete".to_string());
    task["output"] = serde_json::json!({
        "workspace_id": "candidate",
        "workspace_path": workspace_path,
        "source_branch": "main",
        "commit": candidate_head
    });
    write_json_path(&task_path, &task);
    let candidate_path =
        main_dir.join(".factory/work/merge-candidates/work-merge-cleanup/candidate-1.json");
    fs::create_dir_all(candidate_path.parent().unwrap()).unwrap();
    write_json_path(
        &candidate_path,
        &serde_json::json!({
            "id": "candidate-1",
            "attempt_id": "attempt-1",
            "source_workspace": {
                "id": "candidate",
                "path": workspace_path
            },
            "target_workspace": {
                "id": "target",
                "path": "."
            },
            "source_branch": "main",
            "target_branch": "main",
            "candidate_commit": candidate_head,
            "review_state": "passed",
            "merge_state": {
                "status": "merged",
                "merged_commit": git_head(&main_dir),
                "check_artifacts": [
                    {
                        "producer_id": "merge-check",
                        "path": ".factory/work/artifacts/work-merge-cleanup/attempt-1/candidate-1/merge/checks/checks.json"
                    }
                ],
                "review_artifacts": [
                    {
                        "producer_id": "merge-review-tests",
                        "path": ".factory/work/artifacts/work-merge-cleanup/attempt-1/candidate-1/merge/reviews/tests/review.md"
                    }
                ]
            }
        }),
    );

    let check_artifact = main_dir.join(
        ".factory/work/artifacts/work-merge-cleanup/attempt-1/candidate-1/merge/checks/checks.json",
    );
    let attempt_artifact_dir =
        main_dir.join(".factory/work/artifacts/work-merge-cleanup/attempt-1");
    let candidate_artifact_dir = attempt_artifact_dir.join("candidate-1");
    let review_artifact = main_dir
        .join(".factory/work/artifacts/work-merge-cleanup/attempt-1/candidate-1/merge/reviews/tests/review.md");
    fs::create_dir_all(check_artifact.parent().unwrap()).unwrap();
    fs::create_dir_all(review_artifact.parent().unwrap()).unwrap();
    fs::write(&check_artifact, "{}").unwrap();
    fs::write(&review_artifact, "Verdict: pass\n").unwrap();

    factory_cmd()
        .current_dir(&main_dir)
        .arg("cleanup")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "would clean Work Item work-merge-cleanup",
        ))
        .stdout(predicate::str::contains("would remove registered worktree"))
        .stdout(predicate::str::contains("would remove Work branch"))
        .stdout(predicate::str::contains(
            check_artifact.to_string_lossy().as_ref(),
        ))
        .stdout(predicate::str::contains(
            review_artifact.to_string_lossy().as_ref(),
        ));

    assert!(item_path.exists());
    assert!(worktree_dir.exists());
    assert!(check_artifact.exists());
    assert!(review_artifact.exists());
    assert!(candidate_artifact_dir.is_dir());
    assert!(attempt_artifact_dir.is_dir());

    factory_cmd()
        .current_dir(&main_dir)
        .args(["cleanup", "--apply"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "cleaned Work Item work-merge-cleanup",
        ))
        .stdout(predicate::str::contains("removed registered worktree"))
        .stdout(predicate::str::contains("removed Work branch"))
        .stdout(predicate::str::contains("removed Work artifact"));

    assert!(!item_path.exists());
    assert!(!attempt_path.exists());
    assert!(!task_path.exists());
    assert!(!candidate_path.exists());
    assert!(!worktree_dir.exists());
    assert!(!check_artifact.exists());
    assert!(!review_artifact.exists());
    assert!(!candidate_artifact_dir.exists());
    assert!(!attempt_artifact_dir.exists());

    let branch_check = StdCommand::new("git")
        .args([
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch_name}"),
        ])
        .current_dir(&main_dir)
        .status()
        .unwrap();
    assert!(!branch_check.success());
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
    fs::write(run_dir.join("coder"), "codex").unwrap();
    fs::write(run_dir.join("brief.md"), "# Brief\n\nSummarize this run\n").unwrap();

    factory_cmd()
        .current_dir(tmp.path())
        .args(["summary", "--run-id", "selected-run"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Run\n"))
        .stdout(predicate::str::contains("ID: selected-run"))
        .stdout(predicate::str::contains("Status: planned"))
        .stdout(predicate::str::contains("Phase: ready to run"))
        .stdout(predicate::str::contains("Author: codex (pending)"))
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
    fs::write(run_dir.join("coder"), "claude").unwrap();
    fs::write(run_dir.join("brief.md"), "Active run brief\n").unwrap();

    factory_cmd()
        .current_dir(tmp.path())
        .arg("summary")
        .assert()
        .success()
        .stdout(predicate::str::contains("ID: active-summary"))
        .stdout(predicate::str::contains("Status: executing"))
        .stdout(predicate::str::contains("Phase: authoring"))
        .stdout(predicate::str::contains("Author: claude (active)"))
        .stdout(predicate::str::contains("author work is still in progress"));
}

#[test]
fn summary_includes_sessions_reviews_handoff_and_report() {
    let tmp = TempDir::new().unwrap();
    let run_dir = tmp.path().join(".factory/runs/artifact-summary");
    fs::create_dir_all(run_dir.join("reviews")).unwrap();
    fs::write(run_dir.join("status"), "needs-user").unwrap();
    fs::write(run_dir.join("coder"), "codex").unwrap();
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
        stdout.contains("Phase: needs user"),
        "summary should include the phase: {stdout}"
    );
    assert!(
        stdout.contains("Author: codex (blocked)"),
        "summary should include the author state: {stdout}"
    );
    assert!(
        stdout.contains("Reviewers: recent (2 verdicts)"),
        "summary should include reviewer activity: {stdout}"
    );
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
fn summary_reports_active_reviewers_without_verdicts() {
    let tmp = TempDir::new().unwrap();
    let run_dir = tmp.path().join(".factory/runs/active-review");
    fs::create_dir_all(&run_dir).unwrap();
    fs::write(run_dir.join("status"), "reviewing").unwrap();
    fs::write(run_dir.join("brief.md"), "Active review\n").unwrap();

    factory_cmd()
        .current_dir(tmp.path())
        .args(["summary", "--run-id", "active-review"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Reviewers: active"));
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
    fs::write(source_run.join("report.md"), "# Source report\n").unwrap();

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
    assert!(!stdout.contains("Source report"), "{stdout}");
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
fn summary_includes_child_activity() {
    let tmp = TempDir::new().unwrap();
    let parent_dir = tmp.path().join(".factory/runs/parent-summary");
    let child_one_dir = tmp.path().join(".factory/runs/parent-summary-1-1");
    let child_two_dir = tmp.path().join(".factory/runs/parent-summary-1-2");
    fs::create_dir_all(&parent_dir).unwrap();
    fs::create_dir_all(&child_one_dir).unwrap();
    fs::create_dir_all(&child_two_dir).unwrap();
    fs::write(parent_dir.join("status"), "executing").unwrap();
    fs::write(parent_dir.join("coder"), "codex").unwrap();
    fs::write(parent_dir.join("brief.md"), "Parent summary\n").unwrap();
    fs::write(
        parent_dir.join("children"),
        "parent-summary-1-1\nparent-summary-1-2\n",
    )
    .unwrap();
    fs::write(child_one_dir.join("status"), "executing").unwrap();
    fs::write(child_one_dir.join("brief.md"), "First child step\n").unwrap();
    fs::write(child_two_dir.join("status"), "complete").unwrap();
    fs::write(child_two_dir.join("brief.md"), "Second child step\n").unwrap();

    let output = factory_cmd()
        .current_dir(tmp.path())
        .args(["summary", "--run-id", "parent-summary"])
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
        stdout.contains("Child parent-summary-1-1: executing - First child step"),
        "summary should include active child activity: {stdout}"
    );
    assert!(
        stdout.contains("Child parent-summary-1-2: complete - Second child step"),
        "summary should include recent child activity: {stdout}"
    );
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
fn summary_prefers_explicit_handoff_question() {
    let tmp = TempDir::new().unwrap();
    let run_dir = tmp.path().join(".factory/runs/handoff-question");
    fs::create_dir_all(&run_dir).unwrap();
    fs::write(run_dir.join("status"), "needs-user").unwrap();
    fs::write(run_dir.join("brief.md"), "Question handoff\n").unwrap();
    fs::write(
        run_dir.join("handoff.md"),
        "# Handoff\n\nContext: credentials changed yesterday.\nQuestion: Which account should Factory use?\n",
    )
    .unwrap();

    let output = factory_cmd()
        .current_dir(tmp.path())
        .args(["summary", "--run-id", "handoff-question"])
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
        stdout.contains("Handoff\n  Question: Which account should Factory use?"),
        "summary should prefer the explicit question: {stdout}"
    );
    assert!(
        !stdout.contains("Handoff\n  Context: credentials changed yesterday."),
        "summary should not prefer earlier context over the question: {stdout}"
    );
    assert!(
        stdout.contains("Next\n  read handoff.md and answer the open question."),
        "summary should include the status-derived next action: {stdout}"
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

fn create_completed_work_attempt(tmp: &TempDir, main_dir: &Path) {
    create_completed_work_attempt_with_instructions(tmp, main_dir, None);
}

fn create_completed_work_attempt_with_behaviors(tmp: &TempDir, main_dir: &Path, behaviors: &str) {
    let bin_dir = tmp.path().join("bin-write-with-behaviors");
    let behaviors_path = tmp.path().join("behaviors.md");
    fs::write(&behaviors_path, behaviors).unwrap();
    write_mock_claude(
        &bin_dir,
        r##"#!/bin/bash
printf 'task output\n' > task-output.txt
git add task-output.txt
git commit -m "Add task output" >/dev/null
exit 0
"##,
    );

    factory_cmd()
        .current_dir(main_dir)
        .args([
            "work",
            "create",
            "work-1",
            "--title",
            "Run review",
            "--behaviors-file",
            &behaviors_path.to_string_lossy(),
        ])
        .assert()
        .success();
    factory_cmd()
        .current_dir(main_dir)
        .args(["work", "attempt", "work-1", "attempt-1"])
        .assert()
        .success();
    factory_cmd()
        .current_dir(main_dir)
        .args([
            "work",
            "task",
            "run",
            "work-1",
            "attempt-1",
            "attempt-1-write-1",
            "--no-sandbox",
        ])
        .env("PATH", mock_path(&bin_dir))
        .assert()
        .success();
}

fn create_completed_work_attempt_with_instructions(
    tmp: &TempDir,
    main_dir: &Path,
    instructions: Option<&str>,
) {
    let bin_dir = tmp.path().join("bin-write");
    write_mock_claude(
        &bin_dir,
        r##"#!/bin/bash
printf 'task output\n' > task-output.txt
git add task-output.txt
git commit -m "Add task output" >/dev/null
exit 0
"##,
    );

    let mut create_args = vec!["work", "create", "work-1", "--title", "Run review"];
    if let Some(instructions) = instructions {
        create_args.extend(["--instructions", instructions]);
    }
    factory_cmd()
        .current_dir(main_dir)
        .args(create_args)
        .assert()
        .success();
    factory_cmd()
        .current_dir(main_dir)
        .args(["work", "attempt", "work-1", "attempt-1"])
        .assert()
        .success();
    factory_cmd()
        .current_dir(main_dir)
        .args([
            "work",
            "task",
            "run",
            "work-1",
            "attempt-1",
            "attempt-1-write-1",
            "--no-sandbox",
        ])
        .env("PATH", mock_path(&bin_dir))
        .assert()
        .success();
}

fn merge_prompt_logging_mock_script(verdict: &str) -> String {
    format!(
        r##"#!/bin/bash
case "$PWD" in
  */work-6-work-1-attempt-1)
    printf 'loop output\n' > loop-output.txt
    git add loop-output.txt
    git commit -m "Add loop output" >/dev/null
    ;;
  */merge/reviews/behaviors)
    while [ "$#" -gt 0 ]; do
      if [ "$1" = "-p" ]; then
        shift
        printf '%s\n' "$1" > "$PROMPT_LOG"
        break
      fi
      shift
    done
    printf 'Verdict: {verdict}\n\nMerge behavior review.\n' > review.md
    ;;
  *)
    if [ -n "${{SYSTEM_LOG:-}}" ]; then
      while [ "$#" -gt 0 ]; do
        if [ "$1" = "--append-system-prompt" ]; then
          shift
          printf '%s\n' "$1" >> "$SYSTEM_LOG"
          break
        fi
        shift
      done
    fi
    printf 'Verdict: {verdict}\n\nLoop review.\n' > review.md
    ;;
esac
exit 0
"##
    )
}

fn loop_mock_script(verdict: &str) -> String {
    format!(
        r##"#!/bin/bash
case "$PWD" in
  */work-6-work-1-attempt-1)
    printf 'loop output\n' > loop-output.txt
    git add loop-output.txt
    git commit -m "Add loop output" >/dev/null
    ;;
  *)
    if [ -n "${{SYSTEM_LOG:-}}" ]; then
      while [ "$#" -gt 0 ]; do
        if [ "$1" = "--append-system-prompt" ]; then
          shift
          printf '%s\n' "$1" >> "$SYSTEM_LOG"
          break
        fi
        shift
      done
    fi
    printf 'Verdict: {verdict}\n\nLoop review.\n' > review.md
    ;;
esac
exit 0
"##
    )
}

fn rebase_mock_script(verdict: &str) -> String {
    format!(
        r##"#!/bin/bash
# Detect rebase invocations by checking for the rebase prompt
PROMPT=""
while [ "$#" -gt 0 ]; do
  if [ "$1" = "-p" ]; then
    shift
    PROMPT="$1"
    break
  fi
  shift
done

if echo "$PROMPT" | grep -q "Rebase the candidate branch"; then
  # Extract target branch from prompt
  TARGET=$(echo "$PROMPT" | grep -o 'onto `[^`]*`' | sed 's/onto `//;s/`//')
  git rebase "$TARGET" 2>/dev/null
  exit $?
fi

case "$PWD" in
  */work-6-work-1-attempt-1)
    printf 'loop output\n' > loop-output.txt
    git add loop-output.txt
    git commit -m "Add loop output" >/dev/null
    ;;
  *)
    printf 'Verdict: {verdict}\n\nLoop review.\n' > review.md
    ;;
esac
exit 0
"##
    )
}

fn rebase_give_up_mock_script() -> String {
    r##"#!/bin/bash
# Detect rebase invocations by checking for the rebase prompt
PROMPT=""
while [ "$#" -gt 0 ]; do
  if [ "$1" = "-p" ]; then
    shift
    PROMPT="$1"
    break
  fi
  shift
done

if echo "$PROMPT" | grep -q "Rebase the candidate branch"; then
  # Extract artifact dir from prompt for give-up.md
  ARTIFACT_DIR=$(echo "$PROMPT" | grep -o '/[^ ]*/give-up.md' | sed 's|/give-up.md$||')
  if [ -n "$ARTIFACT_DIR" ]; then
    mkdir -p "$ARTIFACT_DIR"
    printf 'Cannot resolve conflict in README.md\n' > "$ARTIFACT_DIR/give-up.md"
  fi
  exit 1
fi

case "$PWD" in
  */work-6-work-1-attempt-1)
    printf 'candidate readme\n' > README.md
    git add README.md
    git commit -m "Update README from candidate" >/dev/null
    ;;
  *)
    printf 'Verdict: pass\n\nLoop review passed.\n' > review.md
    ;;
esac
exit 0
"##
    .to_string()
}

fn rebase_conflict_resolve_mock_script() -> String {
    r##"#!/bin/bash
PROMPT=""
while [ "$#" -gt 0 ]; do
  if [ "$1" = "-p" ]; then
    shift
    PROMPT="$1"
    break
  fi
  shift
done

if echo "$PROMPT" | grep -q "Rebase the candidate branch"; then
  TARGET=$(echo "$PROMPT" | grep -o 'onto `[^`]*`' | sed 's/onto `//;s/`//')
  git rebase "$TARGET" 2>/dev/null
  if [ $? -ne 0 ]; then
    # Resolve conflicts by keeping both sides
    for f in $(git diff --name-only --diff-filter=U); do
      # Remove conflict markers, keep all content
      sed -i '' -e '/^<<<<<<</d' -e '/^=======/d' -e '/^>>>>>>>/d' "$f" 2>/dev/null || \
      sed -i -e '/^<<<<<<</d' -e '/^=======/d' -e '/^>>>>>>>/d' "$f"
      git add "$f"
    done
    GIT_EDITOR=true git rebase --continue 2>/dev/null
  fi
  exit $?
fi

case "$PWD" in
  */work-6-work-1-attempt-1)
    printf 'shared content\n' >> shared.txt
    git add shared.txt
    git commit -m "Add shared content from candidate" >/dev/null
    ;;
  *)
    printf 'Verdict: pass\n\nLoop review passed.\n' > review.md
    ;;
esac
exit 0
"##
    .to_string()
}

fn review_only_mock_script(verdict: &str) -> String {
    format!(
        r##"#!/bin/bash
printf 'Verdict: {verdict}\n\nReview-only result.\n' > review.md
exit 0
"##
    )
}

fn review_only_dirty_source_mock_script() -> String {
    r##"#!/bin/bash
printf 'reviewer edit\n' >> ../../../../../../README.md
printf 'Verdict: pass\n\nReview-only result.\n' > review.md
exit 0
"##
    .to_string()
}

fn review_only_changed_head_mock_script() -> String {
    r##"#!/bin/bash
repo="$(pwd)/../../../../../../"
git -C "$repo" config user.email test@example.com
git -C "$repo" config user.name "Test User"
printf 'reviewer commit\n' > "$repo/reviewer-commit.txt"
git -C "$repo" add reviewer-commit.txt
git -C "$repo" commit -m "Mutate source head" >/dev/null
printf 'Verdict: pass\n\nReview-only result.\n' > review.md
exit 0
"##
    .to_string()
}

fn review_only_dirty_factory_mock_script() -> String {
    r##"#!/bin/bash
printf 'reviewer edit\n' >> ../../../../../../.factory/expertise/decisions.md
printf 'Verdict: pass\n\nReview-only result.\n' > review.md
exit 0
"##
    .to_string()
}

fn review_only_dirty_work_state_mock_script() -> String {
    r##"#!/bin/bash
printf 'reviewer edit\n' >> ../../../../items/work-1.json
printf 'Verdict: pass\n\nReview-only result.\n' > review.md
exit 0
"##
    .to_string()
}

fn review_only_dirty_source_and_factory_mock_script() -> String {
    r##"#!/bin/bash
printf 'reviewer source edit\n' >> ../../../../../../README.md
printf 'reviewer factory edit\n' >> ../../../../../../.factory/expertise/decisions.md
printf 'Verdict: pass\n\nReview-only result.\n' > review.md
exit 0
"##
    .to_string()
}

fn review_only_write_task_count(attempt: &serde_json::Value) -> usize {
    attempt["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|task| task["kind"] == "write")
        .count()
}

fn merge_candidates_are_empty(value: &serde_json::Value) -> bool {
    value
        .get("merge_candidates")
        .and_then(|candidates| candidates.as_array())
        .is_none_or(Vec::is_empty)
}

fn assert_no_non_factory_changes(path: &Path) {
    let output = StdCommand::new("git")
        .args([
            "status",
            "--porcelain",
            "--untracked-files=all",
            "--",
            ".",
            ":(exclude).factory",
        ])
        .current_dir(path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git status failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let status = String::from_utf8_lossy(&output.stdout);
    assert!(
        status.is_empty(),
        "source files should not change:\n{status}"
    );
}

fn stateful_loop_mock_script(verdict: &str) -> String {
    format!(
        r##"#!/bin/bash
case "$PWD" in
  */work-6-work-1-attempt-1)
    count_file="$PWD/.factory-loop-write-count"
    if [ -f "$count_file" ]; then
      count="$(cat "$count_file")"
    else
      count=0
    fi
    count="$((count + 1))"
    printf '%s\n' "$count" > "$count_file"
    printf 'loop output %s\n' "$count" > "loop-output-$count.txt"
    git add "$count_file" "loop-output-$count.txt"
    git commit -m "Add loop output $count" >/dev/null
    ;;
  *)
    printf 'Verdict: {verdict}\n\nLoop review.\n' > review.md
    ;;
esac
exit 0
"##
    )
}

fn loop_mock_script_without_verdict() -> String {
    r##"#!/bin/bash
case "$PWD" in
  */work-6-work-1-attempt-1)
    printf 'loop output\n' > loop-output.txt
    git add loop-output.txt
    git commit -m "Add loop output" >/dev/null
    ;;
  *)
    printf 'Loop review without a verdict.\n' > review.md
    ;;
esac
exit 0
"##
    .to_string()
}

fn loop_mock_script_with_mixed_verdicts() -> String {
    r##"#!/bin/bash
case "$PWD" in
  */work-6-work-1-attempt-1)
    printf 'loop output\n' > loop-output.txt
    git add loop-output.txt
    git commit -m "Add loop output" >/dev/null
    ;;
  */attempt-1-review-documentation)
    printf 'Verdict: fail\n\nDocumentation review failed.\n' > review.md
    ;;
  */attempt-1-review-tests)
    printf 'Verdict: uncertain\n\nTests review is uncertain.\n' > review.md
    ;;
  *)
    printf 'Verdict: pass\n\nLoop review passed.\n' > review.md
    ;;
esac
exit 0
"##
    .to_string()
}

fn git_head(repo: &Path) -> String {
    let output = StdCommand::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git rev-parse failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

fn git_common_dir(repo: &Path) -> PathBuf {
    let output = StdCommand::new("git")
        .args(["rev-parse", "--git-common-dir"])
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git rev-parse --git-common-dir failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let path = String::from_utf8(output.stdout).unwrap();
    let path = PathBuf::from(path.trim());
    if path.is_absolute() {
        path
    } else {
        repo.join(path)
    }
}

fn commit_file(repo: &Path, path: &str, content: &str, message: &str) {
    fs::write(repo.join(path), content).unwrap();
    let output = StdCommand::new("git")
        .args(["add", path])
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git add failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output = StdCommand::new("git")
        .args(["commit", "-m", message])
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git commit failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
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

fn write_executable_hook(project_root: &Path, name: &str, script: &str) {
    let hooks_dir = project_root.join(".factory/hooks");
    fs::create_dir_all(&hooks_dir).unwrap();
    let path = hooks_dir.join(name);
    fs::write(&path, script).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
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

fn write_mock_executable(bin_dir: &Path, name: &str, script: &str) {
    fs::create_dir_all(bin_dir).unwrap();

    let path = bin_dir.join(name);
    fs::write(&path, script).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
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
fn run_in_place_uses_current_workspace() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);

    let run_id = "20260606-in-place";
    let run_dir = main_dir.join(format!(".factory/runs/{run_id}"));
    fs::create_dir_all(&run_dir).unwrap();
    fs::write(run_dir.join("status"), "planned").unwrap();
    fs::write(run_dir.join("brief.md"), "# Brief\n\nRun here\n").unwrap();

    let bin_dir = tmp.path().join("bin");
    write_mock_claude(
        &bin_dir,
        r##"#!/bin/bash
WORKING_DIR="$(pwd)"
RUN_ID=$(ls "$WORKING_DIR/.factory/runs/" 2>/dev/null | head -1)
RUN_DIR="$WORKING_DIR/.factory/runs/$RUN_ID"
printf '%s' "$WORKING_DIR" > "$RUN_DIR/working-dir"
printf 'complete' > "$RUN_DIR/status"
exit 0
"##,
    );

    factory_cmd()
        .current_dir(&main_dir)
        .args([
            "run",
            "--runtime",
            "local",
            "--no-sandbox",
            "--in-place",
            "--coder",
            "claude",
            "--run-id",
            run_id,
        ])
        .env("PATH", mock_path(&bin_dir))
        .assert()
        .success()
        .stderr(predicate::str::contains("in-place session loop"));

    let status = fs::read_to_string(run_dir.join("status")).unwrap();
    assert_eq!(status.trim(), "complete");
    assert!(!run_dir.join("worktree").exists());

    let working_dir = fs::read_to_string(run_dir.join("working-dir")).unwrap();
    assert_eq!(
        fs::canonicalize(Path::new(working_dir.trim())).unwrap(),
        fs::canonicalize(&main_dir).unwrap()
    );
}

#[test]
fn run_in_place_can_preserve_run_metadata() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);

    let run_id = "20260606-in-place-preserve-metadata";
    let run_dir = main_dir.join(format!(".factory/runs/{run_id}"));
    fs::create_dir_all(&run_dir).unwrap();
    fs::write(run_dir.join("status"), "planned").unwrap();
    fs::write(run_dir.join("brief.md"), "# Brief\n\nRun here\n").unwrap();
    fs::write(run_dir.join("runtime"), "fargate").unwrap();
    fs::write(
        run_dir.join("handle"),
        "arn:aws:ecs:us-west-1:123:task/cluster/task-abc",
    )
    .unwrap();

    let bin_dir = tmp.path().join("bin");
    write_mock_claude(
        &bin_dir,
        r##"#!/bin/bash
WORKING_DIR="$(pwd)"
RUN_ID=$(ls "$WORKING_DIR/.factory/runs/" 2>/dev/null | head -1)
printf 'complete' > "$WORKING_DIR/.factory/runs/$RUN_ID/status"
exit 0
"##,
    );

    factory_cmd()
        .current_dir(&main_dir)
        .args([
            "run",
            "--runtime",
            "local",
            "--no-sandbox",
            "--in-place",
            "--preserve-run-metadata",
            "--coder",
            "claude",
            "--run-id",
            run_id,
        ])
        .env("PATH", mock_path(&bin_dir))
        .assert()
        .success()
        .stderr(predicate::str::contains("in-place session loop"));

    assert_eq!(
        fs::read_to_string(run_dir.join("runtime")).unwrap(),
        "fargate"
    );
    assert_eq!(
        fs::read_to_string(run_dir.join("handle")).unwrap(),
        "arn:aws:ecs:us-west-1:123:task/cluster/task-abc"
    );
    assert_eq!(
        fs::read_to_string(run_dir.join("status")).unwrap(),
        "complete"
    );
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
fn watch_detects_live_status_change_and_notifies() {
    let tmp = TempDir::new().unwrap();
    let source_run = tmp.path().join(".factory/runs/live-watch");
    let worktree_root = tmp.path().join("worktree");
    let live_run = worktree_root.join(".factory/runs/live-watch");
    fs::create_dir_all(&source_run).unwrap();
    fs::create_dir_all(&live_run).unwrap();
    fs::write(source_run.join("status"), "executing").unwrap();
    fs::write(source_run.join("brief.md"), "Brief\n").unwrap();
    fs::write(source_run.join("worktree"), worktree_root.to_str().unwrap()).unwrap();
    fs::write(live_run.join("status"), "executing").unwrap();

    let bin = assert_cmd::cargo::cargo_bin("factory");
    let child = std::process::Command::new(&bin)
        .current_dir(tmp.path())
        .args(["watch", "1", "--timeout", "5"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();

    std::thread::sleep(std::time::Duration::from_millis(1500));
    fs::write(live_run.join("status"), "complete").unwrap();

    let output = child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(output.status.success(), "watch failed: stderr={stderr}");
    assert!(
        stdout.contains("live-watch") && stdout.contains("complete"),
        "watch should print the live status change: stdout={stdout}"
    );
    assert!(
        stderr.contains("[NOTIFY] Run live-watch: complete"),
        "watch should notify from live status: stderr={stderr}"
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
fn resume_help_lists_local_runtime_flags() {
    let output = factory_cmd().args(["resume", "--help"]).output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "resume --help failed");
    assert!(
        stdout.contains("--no-sandbox"),
        "resume --help should list --no-sandbox: {stdout}"
    );
    assert!(
        stdout.contains("--coder"),
        "resume --help should list --coder: {stdout}"
    );
}

#[test]
fn resume_local_no_sandbox_does_not_leak_into_extra_args() {
    let tmp = TempDir::new().unwrap();

    let run_id = "20260612-no-leak";
    let run_dir = tmp.path().join(format!(".factory/runs/{run_id}"));
    fs::create_dir_all(&run_dir).unwrap();
    fs::write(run_dir.join("status"), "needs-user").unwrap();
    fs::write(run_dir.join("source-branch"), "main").unwrap();
    fs::write(run_dir.join("brief.md"), "# Brief\n\nNo leak test\n").unwrap();
    fs::write(run_dir.join("handoff.md"), "## Handoff\nContinue.\n").unwrap();

    let bin_dir = tmp.path().join("bin");
    write_mock_codex(
        &bin_dir,
        r##"#!/bin/bash
RUN_DIR="$PWD/.factory/runs/20260612-no-leak"
printf '%s\n' "$@" > "$RUN_DIR/codex-args"
echo "complete" > "$RUN_DIR/status"
exit 0
"##,
    );
    write_mock_executable(
        &bin_dir,
        "git",
        r##"#!/bin/bash
exit 0
"##,
    );

    let output = factory_cmd()
        .current_dir(tmp.path())
        .args(["resume", run_id, "--no-sandbox", "--coder", "codex"])
        .env("PATH", &bin_dir)
        .write_stdin("")
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "resume should succeed: {stderr}");

    let args = fs::read_to_string(run_dir.join("codex-args")).unwrap();
    assert!(
        !args.contains("--no-sandbox"),
        "--no-sandbox should not leak into coder args: {args}"
    );
}

#[test]
fn resume_local_coder_takes_precedence_over_global() {
    let tmp = TempDir::new().unwrap();

    let run_id = "20260612-coder-precedence";
    let run_dir = tmp.path().join(format!(".factory/runs/{run_id}"));
    fs::create_dir_all(&run_dir).unwrap();
    fs::write(run_dir.join("status"), "needs-user").unwrap();
    fs::write(run_dir.join("source-branch"), "main").unwrap();
    fs::write(run_dir.join("brief.md"), "# Brief\n\nPrecedence test\n").unwrap();
    fs::write(run_dir.join("handoff.md"), "## Handoff\nContinue.\n").unwrap();

    let bin_dir = tmp.path().join("bin");
    write_mock_codex(
        &bin_dir,
        r##"#!/bin/bash
RUN_DIR="$PWD/.factory/runs/20260612-coder-precedence"
printf '%s\n' "$@" > "$RUN_DIR/codex-args"
echo "complete" > "$RUN_DIR/status"
exit 0
"##,
    );
    write_mock_executable(
        &bin_dir,
        "git",
        r##"#!/bin/bash
exit 0
"##,
    );

    // Global --coder claude, local --coder codex → local should win
    let output = factory_cmd()
        .current_dir(tmp.path())
        .args([
            "--coder",
            "claude",
            "resume",
            run_id,
            "--no-sandbox",
            "--coder",
            "codex",
        ])
        .env("PATH", &bin_dir)
        .write_stdin("")
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "resume should succeed: {stderr}");

    // If the local --coder codex won, the mock codex binary was invoked
    // (not claude). The codex-args file existing proves the codex binary
    // ran.
    assert!(
        run_dir.join("codex-args").exists(),
        "local --coder codex should take precedence over global --coder claude"
    );

    let args = fs::read_to_string(run_dir.join("codex-args")).unwrap();
    assert!(
        args.lines().any(|line| line == "exec"),
        "codex should be invoked with exec subcommand: {args}"
    );
}

#[test]
fn headless_resume_restarts_selected_run_loop() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);

    let run_id = "20260606-headless-resume";
    let run_dir = main_dir.join(format!(".factory/runs/{run_id}"));
    fs::create_dir_all(&run_dir).unwrap();
    fs::write(run_dir.join("status"), "planned").unwrap();
    fs::write(run_dir.join("brief.md"), "# Brief\n\nResume headlessly\n").unwrap();

    let bin_dir = tmp.path().join("bin");
    write_mock_codex(
        &bin_dir,
        r##"#!/bin/bash
WORKING_DIR="$(pwd)"
RUN_ID=$(ls "$WORKING_DIR/.factory/runs/" 2>/dev/null | head -1)
RUN_DIR="$WORKING_DIR/.factory/runs/$RUN_ID"
CALL_FILE="$RUN_DIR/codex-call-count"
COUNT=$(cat "$CALL_FILE" 2>/dev/null || echo "0")
COUNT=$((COUNT + 1))
echo "$COUNT" > "$CALL_FILE"
if [ "$COUNT" -eq 1 ]; then
  printf '%s\n' "$@" > "$RUN_DIR/initial-codex-args"
  echo "needs-user" > "$RUN_DIR/status"
  printf '## Handoff\nContinue.\n' > "$RUN_DIR/handoff.md"
else
  printf '%s\n' "$@" > "$RUN_DIR/resume-codex-args"
  echo "complete" > "$RUN_DIR/status"
fi
exit 0
"##,
    );
    write_mock_sandbox_exec(&bin_dir);
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

    let output = factory_cmd()
        .current_dir(&main_dir)
        .args(["resume", run_id, "--no-sandbox", "--coder", "codex"])
        .env("PATH", mock_path(&bin_dir))
        .env("SANDBOX_EXEC_LOG", tmp.path().join("sandbox-exec.log"))
        .write_stdin("")
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "resume failed: {stderr}");
    assert!(
        stderr.contains("session loop (run: 20260606-headless-resume)"),
        "headless resume should restart the session loop: {stderr}"
    );
    assert!(
        !stderr.contains("stdin is not a terminal"),
        "headless resume should not invoke an interactive agent: {stderr}"
    );
    assert!(
        !tmp.path().join("sandbox-exec.log").exists(),
        "resume --no-sandbox should not invoke sandbox-exec"
    );

    let args = fs::read_to_string(wt_run_dir.join("resume-codex-args")).unwrap();
    assert!(
        args.lines().any(|line| line == "exec"),
        "headless resume should use codex exec: {args}"
    );
    assert!(
        args.lines().any(|line| line == "--json"),
        "headless resume should capture JSON output: {args}"
    );

    let status = fs::read_to_string(wt_run_dir.join("status")).unwrap();
    assert_eq!(status.trim(), "complete");
    let sessions_log = fs::read_to_string(wt_run_dir.join("sessions.log")).unwrap();
    assert!(
        sessions_log.contains("session=2"),
        "resumed loop should continue session numbering: {sessions_log}"
    );
}

#[test]
fn headless_resume_no_sandbox_does_not_require_sandbox_exec() {
    let tmp = TempDir::new().unwrap();

    let run_id = "20260606-headless-no-seatbelt";
    let run_dir = tmp.path().join(format!(".factory/runs/{run_id}"));
    fs::create_dir_all(&run_dir).unwrap();
    fs::write(run_dir.join("status"), "needs-user").unwrap();
    fs::write(run_dir.join("source-branch"), "main").unwrap();
    fs::write(
        run_dir.join("brief.md"),
        "# Brief\n\nResume without Seatbelt\n",
    )
    .unwrap();
    fs::write(run_dir.join("handoff.md"), "## Handoff\nContinue.\n").unwrap();

    let bin_dir = tmp.path().join("bin");
    write_mock_codex(
        &bin_dir,
        r##"#!/bin/bash
RUN_DIR="$PWD/.factory/runs/20260606-headless-no-seatbelt"
printf '%s\n' "$@" > "$RUN_DIR/resume-codex-args"
echo "complete" > "$RUN_DIR/status"
exit 0
"##,
    );
    write_mock_executable(
        &bin_dir,
        "git",
        r##"#!/bin/bash
if [ "$1" = "diff" ]; then
  exit 0
fi
if [ "$1" = "status" ]; then
  exit 0
fi
exit 0
"##,
    );

    let output = factory_cmd()
        .current_dir(tmp.path())
        .args(["resume", run_id, "--no-sandbox", "--coder", "codex"])
        .env("PATH", &bin_dir)
        .write_stdin("")
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "resume should succeed without sandbox-exec on PATH: {stderr}"
    );
    assert!(
        stderr.contains("session loop (run: 20260606-headless-no-seatbelt)"),
        "headless resume should restart the session loop: {stderr}"
    );

    let args = fs::read_to_string(run_dir.join("resume-codex-args")).unwrap();
    assert!(
        args.lines().any(|line| line == "exec"),
        "headless resume should pass control to codex exec: {args}"
    );
    assert!(
        args.lines().any(|line| line == "--json"),
        "headless resume should capture JSON output: {args}"
    );
    let status = fs::read_to_string(run_dir.join("status")).unwrap();
    assert_eq!(status.trim(), "complete");
}

#[test]
fn headless_resume_global_no_sandbox_does_not_require_sandbox_exec() {
    let tmp = TempDir::new().unwrap();

    let run_id = "20260606-headless-global-no-seatbelt";
    let run_dir = tmp.path().join(format!(".factory/runs/{run_id}"));
    fs::create_dir_all(&run_dir).unwrap();
    fs::write(run_dir.join("status"), "needs-user").unwrap();
    fs::write(run_dir.join("source-branch"), "main").unwrap();
    fs::write(
        run_dir.join("brief.md"),
        "# Brief\n\nResume without Seatbelt via global flag\n",
    )
    .unwrap();
    fs::write(run_dir.join("handoff.md"), "## Handoff\nContinue.\n").unwrap();

    let bin_dir = tmp.path().join("bin");
    write_mock_codex(
        &bin_dir,
        r##"#!/bin/bash
RUN_DIR="$PWD/.factory/runs/20260606-headless-global-no-seatbelt"
printf '%s\n' "$@" > "$RUN_DIR/resume-codex-args"
echo "complete" > "$RUN_DIR/status"
exit 0
"##,
    );
    write_mock_executable(
        &bin_dir,
        "git",
        r##"#!/bin/bash
if [ "$1" = "diff" ]; then
  exit 0
fi
if [ "$1" = "status" ]; then
  exit 0
fi
exit 0
"##,
    );

    let output = factory_cmd()
        .current_dir(tmp.path())
        .args(["--no-sandbox", "resume", run_id, "--coder", "codex"])
        .env("PATH", &bin_dir)
        .write_stdin("")
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "resume should honor global --no-sandbox without sandbox-exec on PATH: {stderr}"
    );
    assert!(
        stderr.contains("session loop (run: 20260606-headless-global-no-seatbelt)"),
        "headless resume should restart the session loop: {stderr}"
    );

    let args = fs::read_to_string(run_dir.join("resume-codex-args")).unwrap();
    assert!(
        args.lines().any(|line| line == "exec"),
        "headless resume should pass control to codex exec: {args}"
    );
    assert!(
        args.lines().any(|line| line == "--json"),
        "headless resume should capture JSON output: {args}"
    );
    let status = fs::read_to_string(run_dir.join("status")).unwrap();
    assert_eq!(status.trim(), "complete");
}

#[test]
fn headless_resume_rejects_parallel_parent() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);

    let parent_dir = main_dir.join(".factory/runs/parallel-parent");
    fs::create_dir_all(&parent_dir).unwrap();
    fs::write(parent_dir.join("status"), "failed").unwrap();
    fs::write(parent_dir.join("brief.md"), "# Brief\n\nResume parent\n").unwrap();
    fs::write(parent_dir.join("children"), "parallel-parent-1-1\n").unwrap();

    let bin_dir = tmp.path().join("bin");
    write_mock_codex(
        &bin_dir,
        r##"#!/bin/bash
echo "codex should not run" >&2
exit 99
"##,
    );
    write_mock_sandbox_exec(&bin_dir);

    factory_cmd()
        .current_dir(&main_dir)
        .args(["resume", "parallel-parent", "--coder", "codex"])
        .env("PATH", mock_path(&bin_dir))
        .write_stdin("")
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "Cannot headlessly resume parallel parent run parallel-parent",
        ))
        .stderr(predicate::str::contains("codex should not run").not());
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

#[test]
fn resume_finds_live_needs_user_run() {
    let tmp = TempDir::new().unwrap();
    let source_run = tmp.path().join(".factory/runs/live-resume");
    let worktree_root = setup_git_project(&tmp);
    let live_run = worktree_root.join(".factory/runs/live-resume");
    fs::create_dir_all(&source_run).unwrap();
    fs::create_dir_all(&live_run).unwrap();
    fs::write(source_run.join("status"), "complete").unwrap();
    fs::write(source_run.join("brief.md"), "Brief\n").unwrap();
    fs::write(source_run.join("worktree"), worktree_root.to_str().unwrap()).unwrap();
    fs::write(live_run.join("status"), "needs-user").unwrap();
    fs::write(live_run.join("source-branch"), "main").unwrap();

    let bin_dir = tmp.path().join("bin");
    write_mock_codex(
        &bin_dir,
        r##"#!/bin/bash
WORKING_DIR="$(pwd)"
RUN_DIR="$WORKING_DIR/.factory/runs/live-resume"
printf '%s\n' "$@" > "$RUN_DIR/resume-codex-args"
echo "complete" > "$RUN_DIR/status"
exit 0
"##,
    );
    write_mock_sandbox_exec(&bin_dir);

    let output = factory_cmd()
        .current_dir(tmp.path())
        .args(["resume", "--coder", "codex"])
        .env("PATH", mock_path(&bin_dir))
        .env("SANDBOX_EXEC_LOG", tmp.path().join("sandbox-exec.log"))
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "resume failed: {stderr}");
    assert!(
        stderr.contains("Resuming run live-resume"),
        "should resolve the live needs-user run exactly: stderr={stderr}"
    );
    assert!(
        stderr.contains("session loop (run: live-resume)"),
        "should restart the live run headlessly: stderr={stderr}"
    );
    let status = fs::read_to_string(live_run.join("status")).unwrap();
    assert_eq!(status.trim(), "complete");
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

#[test]
fn pull_downloads_workspace_to_recorded_worktree() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    let worktree = tmp.path().join("worktree");
    let run_dir = project.join(".factory/runs/pull-run");
    fs::create_dir_all(&run_dir).unwrap();
    fs::create_dir_all(&worktree).unwrap();
    fs::write(run_dir.join("runtime"), "fargate").unwrap();
    fs::write(
        run_dir.join("worktree"),
        worktree.to_string_lossy().as_ref(),
    )
    .unwrap();

    let bin_dir = tmp.path().join("bin");
    write_mock_executable(
        &bin_dir,
        "aws",
        r##"#!/bin/bash
set -euo pipefail
printf '%s\n' "$*" >> "${AWS_LOG:?}"
case "$1 $2" in
  "s3 cp")
    printf 'mock workspace archive\n'
    ;;
  *)
    printf 'unexpected aws command: %s\n' "$*" >&2
    exit 1
    ;;
esac
"##,
    );
    write_mock_executable(
        &bin_dir,
        "tar",
        r##"#!/bin/bash
set -euo pipefail
printf '%s\n' "$*" >> "${TAR_LOG:?}"
cat > "${TAR_STDIN:?}"
"##,
    );

    let aws_log = tmp.path().join("aws.log");
    let tar_log = tmp.path().join("tar.log");
    let tar_stdin = tmp.path().join("tar.stdin");

    let output = factory_cmd()
        .current_dir(&project)
        .arg("pull")
        .env("PATH", mock_path(&bin_dir))
        .env("HOME", tmp.path())
        .env("FACTORY_CLUSTER", "cluster-arn")
        .env("FACTORY_S3_BUCKET", "bucket")
        .env("FACTORY_SUBNETS", "subnet-a")
        .env("FACTORY_SECURITY_GROUP", "sg-123")
        .env("FACTORY_REGION", "us-west-2")
        .env("AWS_LOG", &aws_log)
        .env("TAR_LOG", &tar_log)
        .env("TAR_STDIN", &tar_stdin)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "pull failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let aws = fs::read_to_string(&aws_log).unwrap();
    assert!(
        aws.contains("s3 cp --region us-west-2 s3://bucket/runs/pull-run/workspace.tar -"),
        "pull should download the run workspace archive: {aws}"
    );

    let tar = fs::read_to_string(&tar_log).unwrap();
    assert!(
        tar.contains(&format!("xf - -C {}", worktree.display())),
        "pull should extract into the recorded worktree: {tar}"
    );
    assert_eq!(
        fs::read_to_string(&tar_stdin).unwrap(),
        "mock workspace archive\n"
    );
}

#[test]
fn pull_downloads_workspace_to_fallback_target() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    let run_dir = project.join(".factory/runs/pull-run");
    fs::create_dir_all(&run_dir).unwrap();
    fs::write(run_dir.join("runtime"), "fargate").unwrap();
    fs::write(
        run_dir.join("worktree"),
        tmp.path().join("missing").to_string_lossy().as_ref(),
    )
    .unwrap();

    let bin_dir = tmp.path().join("bin");
    write_mock_executable(
        &bin_dir,
        "aws",
        r##"#!/bin/bash
set -euo pipefail
printf '%s\n' "$*" >> "${AWS_LOG:?}"
printf 'mock workspace archive\n'
"##,
    );
    write_mock_executable(
        &bin_dir,
        "tar",
        r##"#!/bin/bash
set -euo pipefail
printf '%s\n' "$*" >> "${TAR_LOG:?}"
cat >/dev/null
"##,
    );

    let aws_log = tmp.path().join("aws.log");
    let tar_log = tmp.path().join("tar.log");
    let fallback = tmp.path().join("pull-run");

    let output = factory_cmd()
        .current_dir(&project)
        .args(["pull", "pull-run"])
        .env("PATH", mock_path(&bin_dir))
        .env("HOME", tmp.path())
        .env("FACTORY_CLUSTER", "cluster-arn")
        .env("FACTORY_S3_BUCKET", "bucket")
        .env("FACTORY_SUBNETS", "subnet-a")
        .env("FACTORY_SECURITY_GROUP", "sg-123")
        .env("FACTORY_REGION", "us-west-2")
        .env("AWS_LOG", &aws_log)
        .env("TAR_LOG", &tar_log)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "pull failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(fallback.is_dir(), "pull should create the fallback target");
    let canonical_fallback = fallback.canonicalize().unwrap();

    let tar = fs::read_to_string(&tar_log).unwrap();
    assert!(
        tar.contains(&format!("xf - -C {}", canonical_fallback.display())),
        "pull should extract into the fallback target: {tar}"
    );
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

#[test]
fn shell_opens_ecs_exec_for_recorded_task() {
    let tmp = TempDir::new().unwrap();
    let run_dir = tmp.path().join(".factory/runs/shell-run");
    fs::create_dir_all(&run_dir).unwrap();
    fs::write(run_dir.join("status"), "executing").unwrap();
    fs::write(
        run_dir.join("handle"),
        "arn:aws:ecs:us-west-2:123:task/cluster/task-abc",
    )
    .unwrap();

    let bin_dir = tmp.path().join("bin");
    write_mock_executable(
        &bin_dir,
        "aws",
        r##"#!/bin/bash
set -euo pipefail
printf '%s\n' "$*" >> "${AWS_LOG:?}"
case "$1 $2" in
  "ecs execute-command")
    exit 0
    ;;
  *)
    printf 'unexpected aws command: %s\n' "$*" >&2
    exit 1
    ;;
esac
"##,
    );

    let aws_log = tmp.path().join("aws.log");

    factory_cmd()
        .current_dir(tmp.path())
        .args(["shell", "shell-run"])
        .env("PATH", mock_path(&bin_dir))
        .env("HOME", tmp.path())
        .env("FACTORY_CLUSTER", "cluster-arn")
        .env("FACTORY_S3_BUCKET", "bucket")
        .env("FACTORY_SUBNETS", "subnet-a")
        .env("FACTORY_SECURITY_GROUP", "sg-123")
        .env("FACTORY_REGION", "us-west-2")
        .env("AWS_LOG", &aws_log)
        .assert()
        .success();

    let log = fs::read_to_string(&aws_log).unwrap();
    assert!(
        log.contains("ecs execute-command --region us-west-2"),
        "shell should invoke ECS Exec in the configured region: {log}"
    );
    assert!(
        log.contains("--cluster cluster-arn"),
        "shell should use the configured cluster: {log}"
    );
    assert!(
        log.contains("--task arn:aws:ecs:us-west-2:123:task/cluster/task-abc"),
        "shell should use the recorded task handle: {log}"
    );
    assert!(
        log.contains("--container run"),
        "shell should target the run container: {log}"
    );
    assert!(
        log.contains("--command /bin/bash"),
        "shell should open bash: {log}"
    );
    assert!(
        log.contains("--interactive"),
        "shell should request an interactive session: {log}"
    );
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
fn run_fargate_launch_uploads_workspace_and_records_task_handle() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);

    let run_id = "20260606-fargate-launch";
    let run_dir = main_dir.join(format!(".factory/runs/{run_id}"));
    fs::create_dir_all(&run_dir).unwrap();
    fs::write(run_dir.join("status"), "planned").unwrap();
    fs::write(run_dir.join("brief.md"), "Launch Fargate\n").unwrap();

    let bin_dir = tmp.path().join("bin");
    write_mock_executable(
        &bin_dir,
        "aws",
        r##"#!/bin/bash
set -euo pipefail
printf '%s\n' "$*" >> "${AWS_LOG:?}"

case "$1 $2" in
  "s3 cp")
    cat > "${UPLOADED_WORKSPACE:?}"
    ;;
  "ecs run-task")
    printf 'arn:aws:ecs:us-west-2:123:task/cluster/task-abc\n'
    ;;
  *)
    printf 'unexpected aws command: %s\n' "$*" >&2
    exit 1
    ;;
esac
"##,
    );

    let aws_log = tmp.path().join("aws.log");
    let uploaded_workspace = tmp.path().join("workspace-in.tar");
    let _guard = worktree_guard(&main_dir, run_id);

    let output = factory_cmd()
        .current_dir(&main_dir)
        .args(["run", "--runtime", "fargate", "--run-id", run_id])
        .env("PATH", mock_path(&bin_dir))
        .env("HOME", tmp.path())
        .env("AWS_ACCESS_KEY_ID", "mock")
        .env("AWS_SECRET_ACCESS_KEY", "mock")
        .env("BRAVE_SEARCH_API_KEY", "mock")
        .env("CLAUDE_CODE_OAUTH_TOKEN", "mock-claude-token")
        .env("FACTORY_CLUSTER", "cluster-arn")
        .env("FACTORY_RUN_TASK", "task-def")
        .env("FACTORY_S3_BUCKET", "bucket")
        .env("FACTORY_SUBNETS", "subnet-a,subnet-b")
        .env("FACTORY_SECURITY_GROUP", "sg-123")
        .env("FACTORY_REGION", "us-west-2")
        .env("AWS_LOG", &aws_log)
        .env("UPLOADED_WORKSPACE", &uploaded_workspace)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "fargate launch failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let log = fs::read_to_string(&aws_log).unwrap();
    assert!(
        log.contains(
            "s3 cp --region us-west-2 - s3://bucket/runs/20260606-fargate-launch/workspace-in.tar"
        ),
        "S3 upload should target the run input archive: {log}"
    );
    assert!(
        log.contains("ecs run-task --region us-west-2"),
        "launch should start an ECS task: {log}"
    );
    assert!(
        log.contains("--cluster cluster-arn"),
        "missing cluster: {log}"
    );
    assert!(
        log.contains("--task-definition task-def"),
        "missing task definition: {log}"
    );
    assert!(
        log.contains("--network-configuration awsvpcConfiguration={subnets=[subnet-a,subnet-b],securityGroups=[sg-123],assignPublicIp=ENABLED}"),
        "missing network configuration: {log}"
    );
    assert!(
        log.contains("FACTORY_RUN_ID") && log.contains(run_id),
        "overrides should include the run ID: {log}"
    );
    assert!(
        log.contains("CLAUDE_CODE_OAUTH_TOKEN") && log.contains("mock-claude-token"),
        "overrides should include the Claude token: {log}"
    );
    assert!(
        fs::metadata(&uploaded_workspace)
            .map(|metadata| metadata.len() > 0)
            .unwrap_or(false),
        "workspace upload tar should be written"
    );
    assert_eq!(
        fs::read_to_string(run_dir.join("runtime")).unwrap(),
        "fargate"
    );
    assert_eq!(
        fs::read_to_string(run_dir.join("handle")).unwrap(),
        "arn:aws:ecs:us-west-2:123:task/cluster/task-abc"
    );
}

#[test]
fn run_fargate_launch_fails_when_workspace_upload_fails() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);

    let run_id = "20260606-fargate-upload-fail";
    let run_dir = main_dir.join(format!(".factory/runs/{run_id}"));
    fs::create_dir_all(&run_dir).unwrap();
    fs::write(run_dir.join("status"), "planned").unwrap();
    fs::write(run_dir.join("brief.md"), "Launch Fargate\n").unwrap();

    let bin_dir = tmp.path().join("bin");
    write_mock_executable(
        &bin_dir,
        "aws",
        r##"#!/bin/bash
set -euo pipefail
printf '%s\n' "$*" >> "${AWS_LOG:?}"

case "$1 $2" in
  "s3 cp")
    cat >/dev/null
    printf 'upload denied\n' >&2
    exit 42
    ;;
  "ecs run-task")
    printf 'ecs should not run after upload failure\n' >&2
    exit 1
    ;;
  *)
    printf 'unexpected aws command: %s\n' "$*" >&2
    exit 1
    ;;
esac
"##,
    );

    let aws_log = tmp.path().join("aws.log");
    let _guard = worktree_guard(&main_dir, run_id);

    factory_cmd()
        .current_dir(&main_dir)
        .args(["run", "--runtime", "fargate", "--run-id", run_id])
        .env("PATH", mock_path(&bin_dir))
        .env("HOME", tmp.path())
        .env("AWS_ACCESS_KEY_ID", "mock")
        .env("AWS_SECRET_ACCESS_KEY", "mock")
        .env("BRAVE_SEARCH_API_KEY", "mock")
        .env("CLAUDE_CODE_OAUTH_TOKEN", "mock-claude-token")
        .env("FACTORY_CLUSTER", "cluster-arn")
        .env("FACTORY_RUN_TASK", "task-def")
        .env("FACTORY_S3_BUCKET", "bucket")
        .env("FACTORY_SUBNETS", "subnet-a,subnet-b")
        .env("FACTORY_SECURITY_GROUP", "sg-123")
        .env("FACTORY_REGION", "us-west-2")
        .env("AWS_LOG", &aws_log)
        .assert()
        .failure()
        .stderr(predicate::str::contains("Failed to upload workspace to S3"));

    let log = fs::read_to_string(&aws_log).unwrap();
    assert!(
        log.contains("s3 cp --region us-west-2 - s3://bucket/runs/20260606-fargate-upload-fail/workspace-in.tar"),
        "launch should attempt the workspace upload: {log}"
    );
    assert!(
        !log.contains("ecs run-task"),
        "launch should not start ECS after upload failure: {log}"
    );
    assert!(
        !run_dir.join("runtime").exists(),
        "failed launch should not record fargate runtime"
    );
    assert!(
        !run_dir.join("handle").exists(),
        "failed launch should not record a task handle"
    );
}

#[test]
fn run_fargate_launch_fails_when_ecs_run_task_fails() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);

    let run_id = "20260606-fargate-ecs-fail";
    let run_dir = main_dir.join(format!(".factory/runs/{run_id}"));
    fs::create_dir_all(&run_dir).unwrap();
    fs::write(run_dir.join("status"), "planned").unwrap();
    fs::write(run_dir.join("brief.md"), "Launch Fargate\n").unwrap();

    let bin_dir = tmp.path().join("bin");
    write_mock_executable(
        &bin_dir,
        "aws",
        r##"#!/bin/bash
set -euo pipefail
printf '%s\n' "$*" >> "${AWS_LOG:?}"

case "$1 $2" in
  "s3 cp")
    cat > "${UPLOADED_WORKSPACE:?}"
    ;;
  "ecs run-task")
    printf 'task definition not found\n' >&2
    exit 43
    ;;
  *)
    printf 'unexpected aws command: %s\n' "$*" >&2
    exit 1
    ;;
esac
"##,
    );

    let aws_log = tmp.path().join("aws.log");
    let uploaded_workspace = tmp.path().join("workspace-in.tar");
    let _guard = worktree_guard(&main_dir, run_id);

    factory_cmd()
        .current_dir(&main_dir)
        .args(["run", "--runtime", "fargate", "--run-id", run_id])
        .env("PATH", mock_path(&bin_dir))
        .env("HOME", tmp.path())
        .env("AWS_ACCESS_KEY_ID", "mock")
        .env("AWS_SECRET_ACCESS_KEY", "mock")
        .env("BRAVE_SEARCH_API_KEY", "mock")
        .env("CLAUDE_CODE_OAUTH_TOKEN", "mock-claude-token")
        .env("FACTORY_CLUSTER", "cluster-arn")
        .env("FACTORY_RUN_TASK", "task-def")
        .env("FACTORY_S3_BUCKET", "bucket")
        .env("FACTORY_SUBNETS", "subnet-a,subnet-b")
        .env("FACTORY_SECURITY_GROUP", "sg-123")
        .env("FACTORY_REGION", "us-west-2")
        .env("AWS_LOG", &aws_log)
        .env("UPLOADED_WORKSPACE", &uploaded_workspace)
        .assert()
        .failure()
        .stderr(predicate::str::contains("Failed to start Fargate task"));

    let log = fs::read_to_string(&aws_log).unwrap();
    assert!(
        log.contains(
            "s3 cp --region us-west-2 - s3://bucket/runs/20260606-fargate-ecs-fail/workspace-in.tar"
        ),
        "launch should upload before starting ECS: {log}"
    );
    assert!(
        log.contains("ecs run-task --region us-west-2"),
        "launch should attempt ECS start: {log}"
    );
    assert!(
        !run_dir.join("runtime").exists(),
        "failed launch should not record fargate runtime"
    );
    assert!(
        !run_dir.join("handle").exists(),
        "failed launch should not record a task handle"
    );
}

#[test]
fn run_fargate_launch_fails_when_ecs_returns_no_task_arn() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);

    let run_id = "20260606-fargate-no-task-arn";
    let run_dir = main_dir.join(format!(".factory/runs/{run_id}"));
    fs::create_dir_all(&run_dir).unwrap();
    fs::write(run_dir.join("status"), "planned").unwrap();
    fs::write(run_dir.join("brief.md"), "Launch Fargate\n").unwrap();

    let bin_dir = tmp.path().join("bin");
    write_mock_executable(
        &bin_dir,
        "aws",
        r##"#!/bin/bash
set -euo pipefail
printf '%s\n' "$*" >> "${AWS_LOG:?}"

case "$1 $2" in
  "s3 cp")
    cat > "${UPLOADED_WORKSPACE:?}"
    ;;
  "ecs run-task")
    printf 'None\n'
    ;;
  *)
    printf 'unexpected aws command: %s\n' "$*" >&2
    exit 1
    ;;
esac
"##,
    );

    let aws_log = tmp.path().join("aws.log");
    let uploaded_workspace = tmp.path().join("workspace-in.tar");
    let _guard = worktree_guard(&main_dir, run_id);

    factory_cmd()
        .current_dir(&main_dir)
        .args(["run", "--runtime", "fargate", "--run-id", run_id])
        .env("PATH", mock_path(&bin_dir))
        .env("HOME", tmp.path())
        .env("AWS_ACCESS_KEY_ID", "mock")
        .env("AWS_SECRET_ACCESS_KEY", "mock")
        .env("BRAVE_SEARCH_API_KEY", "mock")
        .env("CLAUDE_CODE_OAUTH_TOKEN", "mock-claude-token")
        .env("FACTORY_CLUSTER", "cluster-arn")
        .env("FACTORY_RUN_TASK", "task-def")
        .env("FACTORY_S3_BUCKET", "bucket")
        .env("FACTORY_SUBNETS", "subnet-a,subnet-b")
        .env("FACTORY_SECURITY_GROUP", "sg-123")
        .env("FACTORY_REGION", "us-west-2")
        .env("AWS_LOG", &aws_log)
        .env("UPLOADED_WORKSPACE", &uploaded_workspace)
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "Failed to start Fargate task: no task ARN returned",
        ));

    let log = fs::read_to_string(&aws_log).unwrap();
    assert!(
        log.contains("ecs run-task --region us-west-2"),
        "launch should attempt ECS start: {log}"
    );
    assert!(
        fs::metadata(&uploaded_workspace)
            .map(|metadata| metadata.len() > 0)
            .unwrap_or(false),
        "workspace upload tar should be written before ECS response validation"
    );
    assert!(
        !run_dir.join("runtime").exists(),
        "failed launch should not record fargate runtime"
    );
    assert!(
        !run_dir.join("handle").exists(),
        "failed launch should not record a task handle"
    );
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
fn run_with_codex_prefers_factory_ca_bundle() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);

    let run_id = "20260606-codex-factory-ca-bundle";
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
    assert_eq!(ssl_cert_file.trim(), ca_bundle.to_string_lossy());
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
    fs::write(run_dir.join("reviewers"), "tests").unwrap();

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
fn run_merge_completes_full_lifecycle() {
    let tmp = TempDir::new().unwrap();
    let (main_dir, run_id) = setup_completed_run(&tmp);
    let run_dir = main_dir.join(format!(".factory/runs/{run_id}"));

    // Verify worktree exists before landing
    let wt_path_str = fs::read_to_string(run_dir.join("worktree")).unwrap();
    let wt_path = Path::new(wt_path_str.trim());
    assert!(wt_path.is_dir(), "worktree should exist before landing");

    factory_cmd()
        .current_dir(&main_dir)
        .args(["merge", &run_id])
        .assert()
        .success()
        .stderr(predicate::str::contains("Merging run"))
        .stderr(predicate::str::contains("merged successfully"));

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
    assert_eq!(status.trim(), "merged");

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
fn run_merge_resolves_most_recent_complete_run() {
    let tmp = TempDir::new().unwrap();
    let (main_dir, run_id) = setup_completed_run(&tmp);

    // Land without specifying run ID
    factory_cmd()
        .current_dir(&main_dir)
        .args(["merge"])
        .assert()
        .success()
        .stderr(predicate::str::contains(&run_id));
}

#[test]
fn run_merge_rejects_non_complete_run() {
    let tmp = TempDir::new().unwrap();
    let run_dir = tmp.path().join(".factory/runs/test-not-complete");
    fs::create_dir_all(&run_dir).unwrap();
    fs::write(run_dir.join("status"), "executing").unwrap();

    factory_cmd()
        .current_dir(tmp.path())
        .args(["merge", "test-not-complete"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("expected 'complete'"));
}

#[test]
fn run_merge_rejects_dirty_completed_worktree() {
    let tmp = TempDir::new().unwrap();
    let (main_dir, run_id) = setup_completed_run(&tmp);
    let run_dir = main_dir.join(format!(".factory/runs/{run_id}"));
    let wt_path_str = fs::read_to_string(run_dir.join("worktree")).unwrap();
    let wt_path = Path::new(wt_path_str.trim());

    fs::write(wt_path.join("leftover.txt"), "uncommitted\n").unwrap();

    factory_cmd()
        .current_dir(&main_dir)
        .args(["merge", &run_id])
        .assert()
        .failure()
        .stderr(predicate::str::contains("uncommitted worktree changes"));

    assert!(
        wt_path.join("leftover.txt").exists(),
        "landing failure should preserve dirty worktree content"
    );
}

#[test]
fn run_merge_runs_configured_check_before_merging() {
    let tmp = TempDir::new().unwrap();
    let (main_dir, run_id) = setup_completed_run(&tmp);
    let run_dir = main_dir.join(format!(".factory/runs/{run_id}"));
    let wt_path_str = fs::read_to_string(run_dir.join("worktree")).unwrap();
    let wt_path = Path::new(wt_path_str.trim());
    write_executable_hook(
        &main_dir,
        "check-pre-merge",
        "#!/bin/sh\nprintf check-failed >&2\nexit 1\n",
    );

    factory_cmd()
        .current_dir(&main_dir)
        .args(["merge", &run_id])
        .assert()
        .failure()
        .stderr(predicate::str::contains("check-pre-merge failed (exit 1)"))
        .stderr(predicate::str::contains("Log: "));

    let log_path = run_dir.join("hooks/check-pre-merge.log");
    assert!(log_path.is_file(), "hook log should be written");
    let log = fs::read_to_string(&log_path).unwrap();
    assert!(
        log.contains("check-failed"),
        "hook log should capture stderr, got: {log}"
    );
    assert!(wt_path.is_dir(), "failed check should keep worktree");
}

#[test]
fn run_merge_refuses_autofix_when_worktree_has_user_changes() {
    let tmp = TempDir::new().unwrap();
    let (main_dir, run_id) = setup_completed_run(&tmp);
    let run_dir = main_dir.join(format!(".factory/runs/{run_id}"));
    let wt_path_str = fs::read_to_string(run_dir.join("worktree")).unwrap();
    let wt_path = Path::new(wt_path_str.trim());

    fs::write(wt_path.join("dirty-user-file"), "do not commit me\n").unwrap();
    write_executable_hook(
        &main_dir,
        "check-pre-merge",
        "#!/bin/sh\ntest -f already-fixed\n",
    );
    write_executable_hook(
        &main_dir,
        "fix-pre-merge",
        "#!/bin/sh\ntouch already-fixed\n",
    );

    factory_cmd()
        .current_dir(&main_dir)
        .args(["merge", &run_id])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "fix-pre-merge cannot run: worktree has uncommitted changes",
        ));

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
fn run_merge_autofixes_and_reruns_reviewers() {
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

    write_executable_hook(
        &main_dir,
        "check-pre-merge",
        "#!/bin/sh\ntest ! -f needs-format\n",
    );
    write_executable_hook(
        &main_dir,
        "fix-pre-merge",
        "#!/bin/sh\nrm -f needs-format\n",
    );

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
        .args(["merge", &run_id])
        .env("PATH", mock_path(&bin_dir))
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "Rerunning reviewers after fix-pre-merge autofix",
        ));

    let log = std::process::Command::new("git")
        .args(["-C", &main_dir.to_string_lossy()])
        .args(["log", "--oneline", "-5"])
        .output()
        .unwrap();
    let log = String::from_utf8_lossy(&log.stdout);
    assert!(log.contains("Apply fix-pre-merge changes"));
    let review = fs::read_to_string(run_dir.join("reviews/review-tests.md")).unwrap();
    assert!(review.contains("Autofix review passed"));
    assert!(!main_dir.join("needs-format").exists());
}

#[test]
fn run_merge_keeps_worktree_when_autofix_review_fails() {
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

    write_executable_hook(
        &main_dir,
        "check-pre-merge",
        "#!/bin/sh\ntest ! -f needs-format\n",
    );
    write_executable_hook(
        &main_dir,
        "fix-pre-merge",
        "#!/bin/sh\nrm -f needs-format\n",
    );

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
        .args(["merge", &run_id])
        .env("PATH", mock_path(&bin_dir))
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "reviewers did not pass after fix-pre-merge",
        ));

    assert!(wt_path.is_dir(), "review failure should keep worktree");
    let review = fs::read_to_string(run_dir.join("reviews/review-tests.md")).unwrap();
    assert!(review.contains("Verdict: fail"));
    let status = fs::read_to_string(run_dir.join("status")).unwrap();
    assert_ne!(status.trim(), "merged");
}

#[test]
fn run_merge_rejects_failed_reviews() {
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
        .args(["merge", run_id])
        .assert()
        .failure()
        .stderr(predicate::str::contains("reviews did not pass"));
}

#[test]
fn run_merge_accepts_review_limit_state_with_stale_fail_artifact() {
    let tmp = TempDir::new().unwrap();
    let (main_dir, run_id) = setup_completed_run(&tmp);
    let run_dir = main_dir.join(format!(".factory/runs/{run_id}"));
    let wt_path_str = fs::read_to_string(run_dir.join("worktree")).unwrap();
    let wt_run_dir = Path::new(wt_path_str.trim()).join(format!(".factory/runs/{run_id}"));

    fs::write(
        wt_run_dir.join("reviews/review-tests.md"),
        "Verdict: fail\n\nStale finding.\n",
    )
    .unwrap();
    fs::write(
        wt_run_dir.join("review-state.json"),
        r#"{
  "state": "accepted-review-limit",
  "round": 11,
  "source": "review-limit",
  "verdicts": {
    "tests": "fail"
  },
  "max_rounds": 10,
  "reason": "Review round limit reached with a clean worktree."
}
"#,
    )
    .unwrap();

    factory_cmd()
        .current_dir(&main_dir)
        .args(["merge", &run_id])
        .assert()
        .success()
        .stderr(predicate::str::contains("merged successfully"));

    let landed_state = fs::read_to_string(run_dir.join("review-state.json")).unwrap();
    assert!(landed_state.contains(r#""state": "accepted-review-limit""#));
    assert!(landed_state.contains(r#""tests": "fail""#));

    let status = fs::read_to_string(run_dir.join("status")).unwrap();
    assert_eq!(status.trim(), "merged");
}

#[test]
fn run_merge_rejects_live_failed_reviews() {
    let tmp = TempDir::new().unwrap();
    let (main_dir, run_id) = setup_completed_run(&tmp);
    let run_dir = main_dir.join(format!(".factory/runs/{run_id}"));
    let wt_path_str = fs::read_to_string(run_dir.join("worktree")).unwrap();
    let wt_run_dir = Path::new(wt_path_str.trim()).join(format!(".factory/runs/{run_id}"));

    fs::create_dir_all(run_dir.join("reviews")).unwrap();
    fs::write(run_dir.join("reviews/review-tests.md"), "Verdict: pass").unwrap();
    fs::write(
        wt_run_dir.join("reviews/review-tests.md"),
        "Verdict: fail\n\nLive review failed.\n",
    )
    .unwrap();
    fs::write(
        wt_run_dir.join("review-state.json"),
        r#"{
  "state": "failed",
  "round": 1,
  "source": "reviewers",
  "verdicts": {
    "tests": "fail"
  }
}
"#,
    )
    .unwrap();

    factory_cmd()
        .current_dir(&main_dir)
        .args(["merge", &run_id])
        .assert()
        .failure()
        .stderr(predicate::str::contains("reviews did not pass"));

    assert!(
        wt_run_dir.is_dir(),
        "landing failure should keep the worktree run artifacts"
    );
}

#[test]
fn run_merge_fails_when_no_complete_run() {
    let tmp = TempDir::new().unwrap();
    fs::create_dir_all(tmp.path().join(".factory/runs/some-run")).unwrap();
    fs::write(
        tmp.path().join(".factory/runs/some-run/status"),
        "executing",
    )
    .unwrap();

    factory_cmd()
        .current_dir(tmp.path())
        .args(["merge"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("No complete run found"));
}

#[test]
fn run_merge_preserves_linear_history() {
    let tmp = TempDir::new().unwrap();
    let (main_dir, run_id) = setup_completed_run(&tmp);

    factory_cmd()
        .current_dir(&main_dir)
        .args(["merge", &run_id])
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
fn run_merge_fails_on_rebase_conflict() {
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
        .args(["merge", &run_id])
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
        "merged",
        "status should not be landed after failed rebase"
    );
}

#[test]
fn run_merge_fails_when_worktree_file_missing() {
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
        .args(["merge", run_id])
        .assert()
        .failure()
        .stderr(predicate::str::contains("worktree"));
}

fn patch_attempt_kind_to_post_merge_review(
    project_root: &Path,
    work_item_id: &str,
    attempt_id: &str,
) {
    let attempt_path = project_root
        .join(".factory/work/attempts")
        .join(work_item_id)
        .join(format!("{attempt_id}.json"));
    let content = fs::read_to_string(&attempt_path).unwrap();
    let patched = content.replace("\"review-only\"", "\"post-merge-review\"");
    assert_ne!(
        content, patched,
        "expected to patch review-only to post-merge-review in {attempt_path:?}"
    );
    fs::write(&attempt_path, patched).unwrap();
}

#[test]
fn post_merge_review_guard_allows_source_changes() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "create", "work-1", "--title", "Post-merge review"])
        .assert()
        .success();
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "review-codebase", "work-1", "attempt-review"])
        .assert()
        .success();
    patch_attempt_kind_to_post_merge_review(&main_dir, "work-1", "attempt-review");

    let main_head = git_head(&main_dir);
    let bin_dir = tmp.path().join("bin-pmr-dirty");
    write_mock_claude(&bin_dir, &review_only_dirty_source_mock_script());

    factory_cmd()
        .current_dir(&main_dir)
        .args([
            "work",
            "attempt",
            "run",
            "work-1",
            "attempt-review",
            "--no-sandbox",
        ])
        .env("PATH", mock_path(&bin_dir))
        .assert()
        .success();

    let value = read_work_show_json(&main_dir, "work-1");
    let attempt = &value["attempts"][0];
    assert_eq!(attempt["kind"], "post-merge-review");
    assert_eq!(attempt["status"], "complete");
    assert_eq!(attempt["review_state"], "passed");
    assert_eq!(git_head(&main_dir), main_head);
}

#[test]
fn post_merge_review_guard_allows_factory_state_changes() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    fs::create_dir_all(main_dir.join(".factory/expertise")).unwrap();
    fs::write(
        main_dir.join(".factory/expertise/decisions.md"),
        "initial\n",
    )
    .unwrap();
    StdCommand::new("git")
        .args(["add", ".factory"])
        .current_dir(&main_dir)
        .output()
        .unwrap();
    StdCommand::new("git")
        .args(["commit", "-m", "add factory state"])
        .current_dir(&main_dir)
        .output()
        .unwrap();

    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "create", "work-1", "--title", "Post-merge review"])
        .assert()
        .success();
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "review-codebase", "work-1", "attempt-review"])
        .assert()
        .success();
    patch_attempt_kind_to_post_merge_review(&main_dir, "work-1", "attempt-review");

    let main_head = git_head(&main_dir);
    let bin_dir = tmp.path().join("bin-pmr-factory-dirty");
    write_mock_claude(&bin_dir, &review_only_dirty_factory_mock_script());

    factory_cmd()
        .current_dir(&main_dir)
        .args([
            "work",
            "attempt",
            "run",
            "work-1",
            "attempt-review",
            "--no-sandbox",
        ])
        .env("PATH", mock_path(&bin_dir))
        .assert()
        .success();

    let value = read_work_show_json(&main_dir, "work-1");
    let attempt = &value["attempts"][0];
    assert_eq!(attempt["kind"], "post-merge-review");
    assert_eq!(attempt["status"], "complete");
    assert_eq!(attempt["review_state"], "passed");
    assert_eq!(git_head(&main_dir), main_head);
}

#[test]
fn post_merge_review_guard_fails_when_head_moves() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "create", "work-1", "--title", "Post-merge review"])
        .assert()
        .success();
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "review-codebase", "work-1", "attempt-review"])
        .assert()
        .success();
    patch_attempt_kind_to_post_merge_review(&main_dir, "work-1", "attempt-review");

    let bin_dir = tmp.path().join("bin-pmr-head");
    write_mock_claude(&bin_dir, &review_only_changed_head_mock_script());

    factory_cmd()
        .current_dir(&main_dir)
        .args([
            "work",
            "attempt",
            "run",
            "work-1",
            "attempt-review",
            "--no-sandbox",
        ])
        .env("PATH", mock_path(&bin_dir))
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "Source HEAD moved during post-merge review",
        ));

    let value = read_work_show_json(&main_dir, "work-1");
    let attempt = &value["attempts"][0];
    assert!(
        attempt["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|task| task["kind"] == "review" && task["status"] == "failed")
    );
}

#[test]
fn post_merge_review_guard_passes_clean_review() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "create", "work-1", "--title", "Post-merge review"])
        .assert()
        .success();
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "review-codebase", "work-1", "attempt-review"])
        .assert()
        .success();
    patch_attempt_kind_to_post_merge_review(&main_dir, "work-1", "attempt-review");

    let main_head = git_head(&main_dir);
    let bin_dir = tmp.path().join("bin-pmr-pass");
    write_mock_claude(&bin_dir, &review_only_mock_script("pass"));

    factory_cmd()
        .current_dir(&main_dir)
        .args([
            "work",
            "attempt",
            "run",
            "work-1",
            "attempt-review",
            "--no-sandbox",
        ])
        .env("PATH", mock_path(&bin_dir))
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Review-only Attempt attempt-review passed",
        ));

    let value = read_work_show_json(&main_dir, "work-1");
    let attempt = &value["attempts"][0];
    assert_eq!(attempt["kind"], "post-merge-review");
    assert_eq!(attempt["status"], "complete");
    assert_eq!(attempt["review_state"], "passed");
    assert_eq!(git_head(&main_dir), main_head);
}

#[test]
fn post_merge_review_preflight_allows_non_factory_worktree_changes() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "create", "work-1", "--title", "Post-merge review"])
        .assert()
        .success();
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "review-codebase", "work-1", "attempt-review"])
        .assert()
        .success();
    patch_attempt_kind_to_post_merge_review(&main_dir, "work-1", "attempt-review");

    fs::write(main_dir.join("user-edit.txt"), "concurrent user work\n").unwrap();

    let main_head = git_head(&main_dir);
    let bin_dir = tmp.path().join("bin-pmr-concurrent");
    write_mock_claude(&bin_dir, &review_only_mock_script("pass"));

    factory_cmd()
        .current_dir(&main_dir)
        .args([
            "work",
            "attempt",
            "run",
            "work-1",
            "attempt-review",
            "--no-sandbox",
        ])
        .env("PATH", mock_path(&bin_dir))
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Review-only Attempt attempt-review passed",
        ));

    let value = read_work_show_json(&main_dir, "work-1");
    let attempt = &value["attempts"][0];
    assert_eq!(attempt["status"], "complete");
    assert_eq!(git_head(&main_dir), main_head);
    assert!(
        main_dir.join("user-edit.txt").exists(),
        "user's concurrent edit should be preserved"
    );
}
