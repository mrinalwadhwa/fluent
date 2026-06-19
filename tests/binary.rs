#[path = "lib/log.rs"]
mod log;

use factory::git;
use log::LoggedCommand;
use predicates::prelude::*;
use serial_test::serial;
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

fn factory_cmd() -> LoggedCommand {
    let mut cmd = LoggedCommand::cargo_bin("factory");
    cmd.env_remove("FACTORY_TASK_KIND");
    cmd
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
fn factory_help_lists_tester_subcommand() {
    let tmp = TempDir::new().unwrap();
    let output = factory_cmd()
        .current_dir(tmp.path())
        .args(["work", "--help"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("tester"),
        "factory work --help should list the tester subcommand; got:\n{stdout}"
    );
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
    let cwd = std::env::current_dir().ok()?;
    git::run_stdout(&cwd, &["rev-parse", "--short", "HEAD"], "read build commit")
        .ok()
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
fn fargate_ensure_setup_creates_dockerfile_stub_when_missing() {
    let tmp = TempDir::new().unwrap();
    let bin_dir = tmp.path().join("bin");
    let project_dir = tmp.path().join("my-project");
    let factory_src = tmp.path().join("factory-src");
    fs::create_dir_all(project_dir.join(".factory")).unwrap();
    fs::create_dir_all(factory_src.join("infrastructure/run")).unwrap();
    fs::write(
        factory_src.join("Cargo.toml"),
        "[package]\nname = \"factory\"\n",
    )
    .unwrap();
    fs::write(
        factory_src.join("infrastructure/run/Dockerfile"),
        "FROM node:latest\n",
    )
    .unwrap();
    fs::write(
        factory_src.join("infrastructure/run/entrypoint.sh"),
        "#!/bin/sh\n",
    )
    .unwrap();

    let state_dir = tmp.path().join(".config/factory");
    fs::create_dir_all(&state_dir).unwrap();
    fs::write(
        state_dir.join("fargate.state.json"),
        r#"{
  "stack_deployed": true,
  "region": "us-west-2",
  "cluster_arn": "arn:aws:ecs:us-west-2:123:cluster/factory",
  "task_def_arn": "arn:aws:ecs:us-west-2:123:task-definition/factory-run:1",
  "repo_uri": "123456789012.dkr.ecr.us-west-2.amazonaws.com/factory/run",
  "s3_bucket": "factory-workspace-123",
  "subnets": "subnet-a,subnet-b",
  "security_group_id": "sg-abc"
}"#,
    )
    .unwrap();

    let aws_log = tmp.path().join("aws.log");
    write_mock_executable(
        &bin_dir,
        "aws",
        r##"#!/bin/bash
printf '%s\n' "$*" >> "${AWS_LOG:?}"
case "$1 $2" in
  "sts get-caller-identity")
    printf '123456789012\n'
    ;;
  "ecr describe-images")
    printf 'None\n'
    ;;
  "ecs describe-task-definition")
    if echo "$*" | grep -q 'containerDefinitions\[0\].image'; then
      printf 'placeholder\n'
    else
      printf '{"family":"factory-run","containerDefinitions":[{"name":"run","image":"placeholder","essential":true}],"requiresCompatibilities":["FARGATE"],"networkMode":"awsvpc","cpu":"1024","memory":"2048"}\n'
    fi
    ;;
  "ecs register-task-definition")
    printf 'arn:aws:ecs:us-west-2:123:task-definition/factory-run:2\n'
    ;;
  *)
    ;;
esac
"##,
    );

    write_mock_executable(
        &bin_dir,
        "docker",
        r##"#!/bin/bash
printf '%s\n' "$*" >> "${AWS_LOG:?}.docker"
exit 0
"##,
    );

    let output = factory_cmd()
        .current_dir(&project_dir)
        .env("HOME", tmp.path())
        .env("PATH", mock_path(&bin_dir))
        .env("AWS_LOG", &aws_log)
        .env("FACTORY_SOURCE_ROOT", &factory_src)
        .args(["fargate", "ensure-setup"])
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "ensure-setup should succeed: stdout={} stderr={stderr}",
        String::from_utf8_lossy(&output.stdout)
    );

    let dockerfile = project_dir.join(".factory/Dockerfile");
    assert!(dockerfile.exists(), "Dockerfile stub should be created");
    let content = fs::read_to_string(&dockerfile).unwrap();
    assert!(
        content.contains("ARG FACTORY_BASE_URI"),
        "stub should contain ARG: {content}"
    );
    assert!(
        content.contains("FROM ${FACTORY_BASE_URI}"),
        "stub should contain FROM: {content}"
    );
    assert!(
        stderr.contains("Created .factory/Dockerfile stub"),
        "should report stub creation: {stderr}"
    );
}

#[test]
fn fargate_ensure_setup_skips_base_build_when_ecr_tag_exists() {
    let tmp = TempDir::new().unwrap();
    let bin_dir = tmp.path().join("bin");
    let project_dir = tmp.path().join("my-project");
    let factory_src = tmp.path().join("factory-src");
    fs::create_dir_all(project_dir.join(".factory")).unwrap();
    fs::create_dir_all(factory_src.join("infrastructure/run")).unwrap();
    fs::write(
        factory_src.join("Cargo.toml"),
        "[package]\nname = \"factory\"\n",
    )
    .unwrap();
    fs::write(
        factory_src.join("infrastructure/run/Dockerfile"),
        "FROM node:latest\n",
    )
    .unwrap();
    fs::write(
        factory_src.join("infrastructure/run/entrypoint.sh"),
        "#!/bin/sh\n",
    )
    .unwrap();

    let state_dir = tmp.path().join(".config/factory");
    fs::create_dir_all(&state_dir).unwrap();
    fs::write(
        state_dir.join("fargate.state.json"),
        r#"{
  "stack_deployed": true,
  "region": "us-west-2",
  "cluster_arn": "arn:aws:ecs:us-west-2:123:cluster/factory",
  "task_def_arn": "arn:aws:ecs:us-west-2:123:task-definition/factory-run:1",
  "repo_uri": "123456789012.dkr.ecr.us-west-2.amazonaws.com/factory/run",
  "s3_bucket": "factory-workspace-123",
  "subnets": "subnet-a,subnet-b",
  "security_group_id": "sg-abc"
}"#,
    )
    .unwrap();

    let aws_log = tmp.path().join("aws.log");
    write_mock_executable(
        &bin_dir,
        "aws",
        r##"#!/bin/bash
printf '%s\n' "$*" >> "${AWS_LOG:?}"
case "$1 $2" in
  "sts get-caller-identity")
    printf '123456789012\n'
    ;;
  "ecr describe-images")
    printf 'sha256:abc123\n'
    ;;
  "ecs describe-task-definition")
    if echo "$*" | grep -q 'containerDefinitions\[0\].image'; then
      printf 'placeholder\n'
    else
      printf '{"family":"factory-run","containerDefinitions":[{"name":"run","image":"placeholder","essential":true}],"requiresCompatibilities":["FARGATE"],"networkMode":"awsvpc","cpu":"1024","memory":"2048"}\n'
    fi
    ;;
  "ecs register-task-definition")
    printf 'arn:aws:ecs:us-west-2:123:task-definition/factory-run:2\n'
    ;;
  *)
    ;;
esac
"##,
    );

    write_mock_executable(
        &bin_dir,
        "docker",
        r##"#!/bin/bash
printf 'docker: %s\n' "$*" >> "${AWS_LOG:?}.docker"
exit 0
"##,
    );

    let output = factory_cmd()
        .current_dir(&project_dir)
        .env("HOME", tmp.path())
        .env("PATH", mock_path(&bin_dir))
        .env("AWS_LOG", &aws_log)
        .env("FACTORY_SOURCE_ROOT", &factory_src)
        .args(["fargate", "ensure-setup"])
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "ensure-setup should succeed: stdout={} stderr={stderr}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        stderr.contains("already in ECR, skipping build"),
        "should skip base build when ECR has the tag: {stderr}"
    );

    let docker_log_path = format!("{}.docker", aws_log.display());
    let docker_log = fs::read_to_string(&docker_log_path).unwrap_or_default();
    assert!(
        !docker_log.contains("build"),
        "should not invoke docker build when base image exists in ECR: {docker_log}"
    );
}

#[test]
fn fargate_ensure_setup_skips_project_build_when_ecr_tag_exists() {
    let tmp = TempDir::new().unwrap();
    let bin_dir = tmp.path().join("bin");
    let project_dir = tmp.path().join("my-project");
    let factory_src = tmp.path().join("factory-src");
    fs::create_dir_all(project_dir.join(".factory")).unwrap();
    fs::create_dir_all(factory_src.join("infrastructure/run")).unwrap();
    fs::write(
        factory_src.join("Cargo.toml"),
        "[package]\nname = \"factory\"\n",
    )
    .unwrap();
    fs::write(
        factory_src.join("infrastructure/run/Dockerfile"),
        "FROM node:latest\n",
    )
    .unwrap();
    fs::write(
        factory_src.join("infrastructure/run/entrypoint.sh"),
        "#!/bin/sh\n",
    )
    .unwrap();
    fs::write(
        project_dir.join(".factory/Dockerfile"),
        "ARG FACTORY_BASE_URI\nFROM ${FACTORY_BASE_URI}\nRUN echo hello\n",
    )
    .unwrap();

    let state_dir = tmp.path().join(".config/factory");
    fs::create_dir_all(&state_dir).unwrap();
    fs::write(
        state_dir.join("fargate.state.json"),
        r#"{
  "stack_deployed": true,
  "region": "us-west-2",
  "cluster_arn": "arn:aws:ecs:us-west-2:123:cluster/factory",
  "task_def_arn": "arn:aws:ecs:us-west-2:123:task-definition/factory-run:1",
  "repo_uri": "123456789012.dkr.ecr.us-west-2.amazonaws.com/factory/run",
  "s3_bucket": "factory-workspace-123",
  "subnets": "subnet-a,subnet-b",
  "security_group_id": "sg-abc"
}"#,
    )
    .unwrap();

    let aws_log = tmp.path().join("aws.log");
    write_mock_executable(
        &bin_dir,
        "aws",
        r##"#!/bin/bash
printf '%s\n' "$*" >> "${AWS_LOG:?}"
case "$1 $2" in
  "sts get-caller-identity")
    printf '123456789012\n'
    ;;
  "ecr describe-images")
    printf 'sha256:abc123\n'
    ;;
  "ecs describe-task-definition")
    if echo "$*" | grep -q 'containerDefinitions\[0\].image'; then
      printf '123456789012.dkr.ecr.us-west-2.amazonaws.com/factory/run:project-existing\n'
    else
      printf '{"family":"factory-run","containerDefinitions":[{"name":"run","image":"123456789012.dkr.ecr.us-west-2.amazonaws.com/factory/run:project-existing","essential":true}],"requiresCompatibilities":["FARGATE"],"networkMode":"awsvpc","cpu":"1024","memory":"2048"}\n'
    fi
    ;;
  "ecs register-task-definition")
    printf 'arn:aws:ecs:us-west-2:123:task-definition/factory-run:2\n'
    ;;
  *)
    ;;
esac
"##,
    );

    write_mock_executable(
        &bin_dir,
        "docker",
        r##"#!/bin/bash
printf 'docker: %s\n' "$*" >> "${AWS_LOG:?}.docker"
exit 0
"##,
    );

    let output = factory_cmd()
        .current_dir(&project_dir)
        .env("HOME", tmp.path())
        .env("PATH", mock_path(&bin_dir))
        .env("AWS_LOG", &aws_log)
        .env("FACTORY_SOURCE_ROOT", &factory_src)
        .args(["fargate", "ensure-setup"])
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "ensure-setup should succeed: stdout={} stderr={stderr}",
        String::from_utf8_lossy(&output.stdout)
    );

    let docker_log_path = format!("{}.docker", aws_log.display());
    let docker_log = fs::read_to_string(&docker_log_path).unwrap_or_default();
    let build_count = docker_log.matches("build").count();
    assert!(
        build_count == 0,
        "should not invoke docker build when both images exist in ECR: {docker_log}"
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
        .stdout(predicate::str::contains("Build status view"));
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
    let head = git::run_stdout(&workspace, &["rev-parse", "HEAD"], "read workspace HEAD").unwrap();

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
fn write_task_transcript_persists_after_successful_attempt() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    let bin_dir = tmp.path().join("bin");
    write_mock_claude(
        &bin_dir,
        r##"#!/bin/bash
printf '{"type":"transcript","line":"hello"}\n'
printf 'task output\n' > task-output.txt
git add task-output.txt
git commit -m "Add task output" >/dev/null
exit 0
"##,
    );

    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "create", "work-1", "--title", "Transcript test"])
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
        .success();

    let transcript = main_dir
        .join(".factory/work/artifacts/work-1/attempt-1/attempt-1-write-1/transcript.jsonl");
    assert!(
        transcript.is_file(),
        "transcript.jsonl should exist at {}",
        transcript.display()
    );
    let content = fs::read_to_string(&transcript).unwrap();
    assert!(
        content.contains("transcript"),
        "transcript should contain mock coder output"
    );

    let value = work_item_value(&main_dir, "work-1");
    let task = &value["attempts"][0]["tasks"][0];
    assert_eq!(
        task["artifact_area"]["path"],
        ".factory/work/artifacts/work-1/attempt-1/attempt-1-write-1"
    );
}

#[test]
fn write_task_transcript_persists_after_failed_attempt() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    let bin_dir = tmp.path().join("bin");
    write_mock_claude(
        &bin_dir,
        r##"#!/bin/bash
printf '{"type":"partial","data":"before-failure"}\n'
exit 1
"##,
    );

    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "create", "work-1", "--title", "Fail transcript"])
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
        .output()
        .unwrap();

    let transcript = main_dir
        .join(".factory/work/artifacts/work-1/attempt-1/attempt-1-write-1/transcript.jsonl");
    assert!(
        transcript.is_file(),
        "transcript.jsonl should persist even on failure at {}",
        transcript.display()
    );
    let content = fs::read_to_string(&transcript).unwrap();
    assert!(
        content.contains("before-failure"),
        "partial transcript should contain content written before failure"
    );
}

#[test]
fn write_task_sandbox_grants_artifact_dir_write_access() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    let bin_dir = tmp.path().join("bin-write-sandbox");
    let sandbox_profile_log = tmp.path().join("write-sandbox.sb");
    write_mock_claude(
        &bin_dir,
        r##"#!/bin/bash
printf '{"type":"transcript"}\n'
printf 'task output\n' > task-output.txt
git add task-output.txt
git commit -m "Add task output" >/dev/null
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
        .args(["work", "create", "work-1", "--title", "Sandbox test"])
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
        ])
        .env("PATH", mock_path(&bin_dir))
        .env("SANDBOX_PROFILE_LOG", &sandbox_profile_log)
        .env("CLAUDE_CODE_OAUTH_TOKEN", "mock-token")
        .assert()
        .success();

    let artifact_dir = fs::canonicalize(
        main_dir.join(".factory/work/artifacts/work-1/attempt-1/attempt-1-write-1"),
    )
    .unwrap();
    let sandbox_profile = fs::read_to_string(&sandbox_profile_log).unwrap();
    assert!(
        sandbox_profile.contains(&format!(
            "(allow file-write* (subpath \"{}\"))",
            artifact_dir.display()
        )),
        "sandbox should grant write access to artifact dir: {sandbox_profile}"
    );
}

#[test]
fn reviewer_sandbox_does_not_include_writer_artifact_dir() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    create_completed_work_attempt(&tmp, &main_dir);
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "review", "work-1", "attempt-1"])
        .assert()
        .success();

    let writer_artifact_dir =
        main_dir.join(".factory/work/artifacts/work-1/attempt-1/attempt-1-write-1");
    fs::create_dir_all(&writer_artifact_dir).unwrap();
    fs::write(
        writer_artifact_dir.join("transcript.jsonl"),
        r#"{"type":"transcript","line":"writer content"}"#,
    )
    .unwrap();

    let bin_dir = tmp.path().join("bin-review-check");
    let sandbox_profile_log = tmp.path().join("reviewer-sandbox.sb");
    write_mock_claude(
        &bin_dir,
        "#!/bin/bash\nprintf 'Verdict: pass\\n' > review.md\nexit 0\n",
    );
    write_mock_executable(
        &bin_dir,
        "sandbox-exec",
        "#!/bin/bash\ncp \"$2\" \"${SANDBOX_PROFILE_LOG:?}\"\nshift 2\nexec \"$@\"\n",
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
        .env("SANDBOX_PROFILE_LOG", &sandbox_profile_log)
        .env("CLAUDE_CODE_OAUTH_TOKEN", "mock-token")
        .env("BRAVE_SEARCH_API_KEY", "mock-key")
        .env("AWS_ACCESS_KEY_ID", "mock-access")
        .assert()
        .success();

    let writer_artifact_canonical = fs::canonicalize(&writer_artifact_dir).unwrap();
    let sandbox_profile = fs::read_to_string(&sandbox_profile_log).unwrap();
    assert!(
        !sandbox_profile.contains(&format!(
            "(allow file-read*  (subpath \"{}\"))",
            writer_artifact_canonical.display()
        )),
        "reviewer sandbox should NOT include writer artifact dir for reading: {sandbox_profile}"
    );
    assert!(
        !sandbox_profile.contains(&format!(
            "(allow file-write* (subpath \"{}\"))",
            writer_artifact_canonical.display()
        )),
        "reviewer sandbox should NOT include writer artifact dir for writing: {sandbox_profile}"
    );
}

#[test]
fn review_task_transcript_persists_after_completion() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    create_completed_work_attempt(&tmp, &main_dir);
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "review", "work-1", "attempt-1"])
        .assert()
        .success();

    let bin_dir = tmp.path().join("bin-review-transcript");
    write_mock_claude(
        &bin_dir,
        r##"#!/bin/bash
printf '{"type":"transcript","line":"review-session-data"}\n'
printf 'Verdict: pass\n\nAll tests present.\n' > review.md
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
        .assert()
        .success();

    let transcript = main_dir
        .join(".factory/work/artifacts/work-1/attempt-1/attempt-1-review-tests/transcript.jsonl");
    assert!(
        transcript.is_file(),
        "transcript.jsonl should exist at {}",
        transcript.display()
    );
    let content = fs::read_to_string(&transcript).unwrap();
    assert!(
        content.contains("review-session-data"),
        "transcript should contain mock reviewer output"
    );

    let review =
        main_dir.join(".factory/work/artifacts/work-1/attempt-1/attempt-1-review-tests/review.md");
    assert!(
        review.is_file(),
        "review.md should exist alongside transcript"
    );
}

#[test]
fn review_task_transcript_persists_on_failure() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    create_completed_work_attempt(&tmp, &main_dir);
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "review", "work-1", "attempt-1"])
        .assert()
        .success();

    let bin_dir = tmp.path().join("bin-review-fail-transcript");
    write_mock_claude(
        &bin_dir,
        r##"#!/bin/bash
printf '{"type":"partial","data":"reviewer-before-crash"}\n'
exit 1
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
        .output()
        .unwrap();

    let transcript = main_dir
        .join(".factory/work/artifacts/work-1/attempt-1/attempt-1-review-tests/transcript.jsonl");
    assert!(
        transcript.is_file(),
        "transcript.jsonl should persist even on reviewer failure at {}",
        transcript.display()
    );
    let content = fs::read_to_string(&transcript).unwrap();
    assert!(
        content.contains("reviewer-before-crash"),
        "partial transcript should contain content written before failure"
    );
}

#[test]
fn behavior_tests_task_transcript_persists_alongside_results_json() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    create_completed_work_attempt(&tmp, &main_dir);
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "review", "work-1", "attempt-1"])
        .assert()
        .success();

    let bin_dir = tmp.path().join("bin-bt-transcript");
    write_mock_executable(
        &bin_dir,
        "claude",
        r##"#!/bin/bash
printf '{"type":"transcript","line":"behavior-tests-session"}\n'

args_blob=""
for arg in "$@"; do
    args_blob="$args_blob $arg"
done
results_path=$(printf '%s' "$args_blob" \
    | grep -oE '/[^ ]*/behavior-tests-results\.json' \
    | head -n 1)

if [ -z "$results_path" ]; then
    echo "could not extract behavior-tests-results.json path" >&2
    exit 1
fi

mkdir -p "$(dirname "$results_path")" 2>/dev/null
cat > "$results_path" <<JSON
{
  "ran_at": "1970-01-01T00:00:00Z",
  "candidate_commit": "0000000",
  "commands_run": [],
  "summary": {
    "behaviors_total": 0,
    "tested_passing": 0,
    "tested_failing": 0,
    "untestable": 0,
    "missing_test_ref": 0
  },
  "behaviors": []
}
JSON
exit 0
"##,
    );
    write_mock_executable(&bin_dir, "sandbox-exec", "#!/bin/bash\nexit 1\n");

    factory_cmd()
        .current_dir(&main_dir)
        .args([
            "work",
            "task",
            "run",
            "work-1",
            "attempt-1",
            "attempt-1-behavior-tests",
            "--no-sandbox",
        ])
        .env("PATH", mock_path(&bin_dir))
        .assert()
        .success();

    let transcript = main_dir
        .join(".factory/work/artifacts/work-1/attempt-1/attempt-1-behavior-tests/transcript.jsonl");
    assert!(
        transcript.is_file(),
        "transcript.jsonl should exist at {}",
        transcript.display()
    );
    let content = fs::read_to_string(&transcript).unwrap();
    assert!(
        content.contains("behavior-tests-session"),
        "transcript should contain mock behavior-tests output"
    );

    let results = main_dir.join(
        ".factory/work/artifacts/work-1/attempt-1/attempt-1-behavior-tests/behavior-tests-results.json",
    );
    assert!(
        results.is_file(),
        "behavior-tests-results.json should exist alongside transcript"
    );
}

#[test]
fn reviewer_sandbox_does_not_include_other_reviewer_artifact_dirs() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    create_completed_work_attempt(&tmp, &main_dir);
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "review", "work-1", "attempt-1"])
        .assert()
        .success();

    // Complete two review tasks so their artifact dirs exist with transcripts
    let review_tests_artifact =
        main_dir.join(".factory/work/artifacts/work-1/attempt-1/attempt-1-review-tests");
    let review_documentation_artifact =
        main_dir.join(".factory/work/artifacts/work-1/attempt-1/attempt-1-review-documentation");
    fs::create_dir_all(&review_tests_artifact).unwrap();
    fs::create_dir_all(&review_documentation_artifact).unwrap();
    fs::write(
        review_tests_artifact.join("transcript.jsonl"),
        r#"{"type":"transcript","line":"tests-reviewer"}"#,
    )
    .unwrap();
    fs::write(review_tests_artifact.join("review.md"), "Verdict: pass\n").unwrap();
    fs::write(
        review_documentation_artifact.join("transcript.jsonl"),
        r#"{"type":"transcript","line":"docs-reviewer"}"#,
    )
    .unwrap();
    fs::write(
        review_documentation_artifact.join("review.md"),
        "Verdict: pass\n",
    )
    .unwrap();

    // Mark those two tasks as complete in the store
    for task_id in &["attempt-1-review-tests", "attempt-1-review-documentation"] {
        let task_path = work_task_record_path(&main_dir, "work-1", "attempt-1", task_id);
        let mut task = read_json_value(&task_path);
        task["status"] = serde_json::json!("complete");
        write_json_value(&task_path, &task);
    }

    // Run a third reviewer (architecture) with sandbox profiling
    let bin_dir = tmp.path().join("bin-reviewer-isolation");
    let sandbox_profile_log = tmp.path().join("reviewer-isolation.sb");
    write_mock_claude(
        &bin_dir,
        "#!/bin/bash\nprintf 'Verdict: pass\\n' > review.md\nexit 0\n",
    );
    write_mock_executable(
        &bin_dir,
        "sandbox-exec",
        "#!/bin/bash\ncp \"$2\" \"${SANDBOX_PROFILE_LOG:?}\"\nshift 2\nexec \"$@\"\n",
    );

    factory_cmd()
        .current_dir(&main_dir)
        .args([
            "work",
            "task",
            "run",
            "work-1",
            "attempt-1",
            "attempt-1-review-architecture",
        ])
        .env("PATH", mock_path(&bin_dir))
        .env("SANDBOX_PROFILE_LOG", &sandbox_profile_log)
        .env("CLAUDE_CODE_OAUTH_TOKEN", "mock-token")
        .env("BRAVE_SEARCH_API_KEY", "mock-key")
        .env("AWS_ACCESS_KEY_ID", "mock-access")
        .assert()
        .success();

    let sandbox_profile = fs::read_to_string(&sandbox_profile_log).unwrap();
    let tests_canonical = fs::canonicalize(&review_tests_artifact).unwrap();
    let docs_canonical = fs::canonicalize(&review_documentation_artifact).unwrap();

    assert!(
        !sandbox_profile.contains(&format!(
            "(allow file-read*  (subpath \"{}\"))",
            tests_canonical.display()
        )),
        "reviewer sandbox should NOT include other reviewer (tests) artifact dir: {sandbox_profile}"
    );
    assert!(
        !sandbox_profile.contains(&format!(
            "(allow file-read*  (subpath \"{}\"))",
            docs_canonical.display()
        )),
        "reviewer sandbox should NOT include other reviewer (documentation) artifact dir: {sandbox_profile}"
    );
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
            "Planned 6 review Tasks for Attempt attempt-1",
        ))
        .stdout(predicate::str::contains("attempt-1-review-tests"));

    let value = read_work_show_json(&main_dir, "work-1");
    let attempt = &value["attempts"][0];
    assert_eq!(attempt["status"], "reviewing");
    assert_eq!(attempt["review_state"], "not-reviewed");
    assert_eq!(attempt["tasks"].as_array().unwrap().len(), 7);

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
    git::run(
        &main_dir,
        &["add", "skills/review-tests/SKILL.md"],
        "stage skill",
    )
    .unwrap();
    git::run(
        &main_dir,
        &["commit", "-m", "Add review tests skill"],
        "commit skill",
    )
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
            "attempt-1-behavior-tests",
            "--no-sandbox",
        ])
        .env("PATH", mock_path(&bin_dir))
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
            "attempt-1-behavior-tests",
            "--no-sandbox",
        ])
        .env("PATH", mock_path(&bin_dir))
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

    factory_cmd()
        .current_dir(&main_dir)
        .args([
            "work",
            "task",
            "run",
            "work-1",
            "attempt-1",
            "attempt-1-behavior-tests",
            "--no-sandbox",
        ])
        .env("PATH", mock_path(&bin_dir))
        .assert()
        .success();

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
            "Planned 6 review Tasks for Attempt attempt-1",
        ))
        .stdout(predicate::str::contains(
            "Attempt attempt-1 reviews passed; Merge Candidate attempt-1-merge-candidate is ready",
        ));

    let value = read_work_show_json(&main_dir, "work-1");
    let attempt = &value["attempts"][0];
    assert_eq!(attempt["status"], "complete");
    assert_eq!(attempt["review_state"], "passed");
    assert_eq!(attempt["tasks"].as_array().unwrap().len(), 7);
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
    git::run(&main_dir, &["add", "README.md"], "stage README").unwrap();
    git::run(
        &main_dir,
        &["commit", "-m", "advance source"],
        "commit advance",
    )
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
    git::run(
        &main_dir,
        &["add", ".factory/expertise/decisions.md"],
        "stage decisions",
    )
    .unwrap();
    git::run(
        &main_dir,
        &["commit", "-m", "record decisions"],
        "commit decisions",
    )
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
    git::run(
        &main_dir,
        &["add", ".factory/expertise/decisions.md"],
        "stage decisions",
    )
    .unwrap();
    git::run(
        &main_dir,
        &["commit", "-m", "record decisions"],
        "commit decisions",
    )
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
    git::run(
        &main_dir,
        &["worktree", "lock", &candidate_workspace.to_string_lossy()],
        "lock worktree",
    )
    .unwrap();

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
    assert!(merged.contains("target content") && merged.contains("shared content"));
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
    // Candidate workspace restored to pre-merge state
    let candidate_head = git_head(&candidate_workspace);
    assert_ne!(candidate_head, main_before_merge);

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
fn work_merge_rebase_agent_crash_without_give_up_fails() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "create", "work-1", "--title", "Agent crash"])
        .assert()
        .success();
    factory_cmd()
        .current_dir(&main_dir)
        .args(["work", "attempt", "work-1", "attempt-1"])
        .assert()
        .success();

    let bin_dir = tmp.path().join("bin-rebase-crash");
    write_mock_claude(&bin_dir, &rebase_crash_mock_script());
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
        .stderr(predicate::str::contains("Rebase agent failed"));

    // Target unchanged
    assert_eq!(git_head(&main_dir), main_before_merge);

    let value = read_work_show_json(&main_dir, "work-1");
    let candidate = &value["merge_candidates"][0];
    assert_eq!(candidate["merge_state"]["status"], "failed");

    // Rebase task should show failed status (not needs-user)
    let attempt = &value["attempts"][0];
    let rebase_tasks: Vec<_> = attempt["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|t| t["kind"] == "rebase")
        .collect();
    assert_eq!(rebase_tasks.len(), 1);
    assert_eq!(rebase_tasks[0]["status"], "failed");
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
            "Planned 6 review Tasks for Attempt attempt-1",
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
    assert_eq!(second_round_inputs.len(), 2);
    assert_eq!(
        second_round_inputs[0]["path"],
        ".factory/work/artifacts/work-1/attempt-1/attempt-1-review-documentation/review.md"
    );
    assert_eq!(
        second_round_inputs[0]["producer_id"],
        "attempt-1-review-documentation"
    );
    assert_eq!(
        second_round_inputs[1]["path"],
        ".factory/work/artifacts/work-1/attempt-1/progress.md"
    );
    assert_eq!(second_round_inputs[1]["producer_id"], "writer");
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
            "Planned 6 review Tasks for Attempt attempt-1",
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
    factory_cmd()
        .current_dir(&main_dir)
        .args([
            "work",
            "task",
            "run",
            "work-1",
            "attempt-1",
            "attempt-1-behavior-tests",
            "--no-sandbox",
        ])
        .env("PATH", mock_path(&bin_dir))
        .assert()
        .success();
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
    git::run(
        &main_dir,
        &[
            "worktree",
            "add",
            "-b",
            "precreated-task-workspace",
            &workspace.to_string_lossy(),
            "HEAD",
        ],
        "create worktree",
    )
    .unwrap();
    fs::write(workspace.join("stale-output.txt"), "stale").unwrap();
    git::run(
        &workspace,
        &["add", "stale-output.txt"],
        "stage stale output",
    )
    .unwrap();
    git::run(
        &workspace,
        &["commit", "-m", "Add stale output"],
        "commit stale output",
    )
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

    git::run(
        &main_dir,
        &["branch", "work/work-1/attempt-1/attempt-1-write-1", "HEAD"],
        "create task branch",
    )
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
    git::run(
        &main_dir,
        &[
            "worktree",
            "add",
            worktree_dir.to_str().unwrap(),
            "-b",
            branch_name,
            "HEAD",
        ],
        "create worktree",
    )
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
    git::run(
        &main_dir,
        &[
            "worktree",
            "add",
            active_worktree_dir.to_str().unwrap(),
            "-b",
            active_branch_name,
            "HEAD",
        ],
        "create active worktree",
    )
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

    let branch_check = git::run_raw(
        &main_dir,
        &[
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch_name}"),
        ],
    )
    .unwrap();
    assert!(!branch_check.status.success());

    let active_branch_check = git::run_raw(
        &main_dir,
        &[
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/heads/{active_branch_name}"),
        ],
    )
    .unwrap();
    assert!(active_branch_check.status.success());

    git::run(
        &main_dir,
        &[
            "worktree",
            "remove",
            "--force",
            active_worktree_dir.to_str().unwrap(),
        ],
        "remove active worktree",
    )
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
    git::run(
        &main_dir,
        &[
            "worktree",
            "add",
            worktree_dir.to_str().unwrap(),
            "-b",
            branch_name,
            "HEAD",
        ],
        "create worktree",
    )
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

    let branch_check = git::run_raw(
        &main_dir,
        &[
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch_name}"),
        ],
    )
    .unwrap();
    assert!(!branch_check.status.success());
}

// -------------------------------------------------------------------------
// Shared test helpers
// -------------------------------------------------------------------------

fn setup_git_project(tmp: &TempDir) -> std::path::PathBuf {
    let main_dir = tmp.path().join("main");
    fs::create_dir_all(&main_dir).unwrap();

    git::run(&main_dir, &["init", "-b", "main"], "init repo").unwrap();
    // Persistent config needed because external coder processes (spawned
    // by factory work task run) make commits outside our wrapper.
    git::run(
        &main_dir,
        &["config", "commit.gpgsign", "false"],
        "disable signing",
    )
    .unwrap();
    git::run(
        &main_dir,
        &["config", "user.email", "test@test"],
        "set user.email",
    )
    .unwrap();
    git::run(&main_dir, &["config", "user.name", "test"], "set user.name").unwrap();
    fs::write(main_dir.join("README.md"), "test").unwrap();
    git::run(&main_dir, &["add", "."], "stage files").unwrap();
    git::run(&main_dir, &["commit", "-m", "init"], "initial commit").unwrap();

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

fn rebase_crash_mock_script() -> String {
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
  # Simulate an agent crash: exit non-zero without writing give-up.md
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
    let status = git::run_stdout(
        path,
        &[
            "status",
            "--porcelain",
            "--untracked-files=all",
            "--",
            ".",
            ":(exclude).factory",
        ],
        "check for non-factory changes",
    )
    .unwrap();
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
    git::run_stdout(repo, &["rev-parse", "HEAD"], "get HEAD").unwrap()
}

fn git_common_dir(repo: &Path) -> PathBuf {
    let path_str = git::run_stdout(
        repo,
        &["rev-parse", "--git-common-dir"],
        "get git common dir",
    )
    .unwrap();
    let path = PathBuf::from(&path_str);
    if path.is_absolute() {
        path
    } else {
        repo.join(path)
    }
}

fn commit_file(repo: &Path, path: &str, content: &str, message: &str) {
    fs::write(repo.join(path), content).unwrap();
    git::run(repo, &["add", path], "stage file").unwrap();
    git::run(repo, &["commit", "-m", message], "commit").unwrap();
}

const BEHAVIOR_TESTS_MOCK_PRELUDE: &str = r##"#!/bin/bash

if [ "${FACTORY_TASK_KIND:-}" = "behavior-tests" ]; then
    args_blob=""
    for arg in "$@"; do
        args_blob="$args_blob $arg"
    done
    results_path=$(printf '%s' "$args_blob" \
        | grep -oE '/[^ ]*/behavior-tests-results\.json' \
        | head -n 1)

    if [ -z "$results_path" ]; then
        echo "mock-prelude: could not extract behavior-tests-results.json path from prompt" >&2
        exit 1
    fi

    ran_at=$(date -u +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || echo '1970-01-01T00:00:00Z')
    candidate_commit=$(git rev-parse HEAD 2>/dev/null || echo '0000000000000000000000000000000000000000')

    mkdir -p "$(dirname "$results_path")" 2>/dev/null
    cat > "$results_path" <<JSON
{
  "ran_at": "$ran_at",
  "candidate_commit": "$candidate_commit",
  "commands_run": [],
  "summary": {
    "behaviors_total": 0,
    "tested_passing": 0,
    "tested_failing": 0,
    "untestable": 0,
    "missing_test_ref": 0
  },
  "behaviors": []
}
JSON
    exit 0
fi

"##;

fn write_mock_claude(bin_dir: &Path, script: &str) {
    fs::create_dir_all(bin_dir).unwrap();

    let script_body = script.strip_prefix("#!/bin/bash\n").unwrap_or(script);
    let combined = format!("{}{}", BEHAVIOR_TESTS_MOCK_PRELUDE, script_body);

    let claude_path = bin_dir.join("claude");
    fs::write(&claude_path, &combined).unwrap();
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

fn mock_path(bin_dir: &Path) -> String {
    format!("{}:{}", bin_dir.display(), std::env::var("PATH").unwrap())
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
    git::run(&main_dir, &["add", ".factory"], "stage factory state").unwrap();
    git::run(
        &main_dir,
        &["commit", "-m", "add factory state"],
        "commit factory state",
    )
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

// --- Observations management ---

#[test]
fn observations_add_with_inline_content() {
    let tmp = TempDir::new().unwrap();

    let output = factory_cmd()
        .current_dir(tmp.path())
        .args(["observations", "add", "Test observation content"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "add should succeed: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let id = stdout.trim();
    assert!(!id.is_empty(), "should print the generated ID");
    assert!(
        id.contains("-test-observation-content"),
        "ID should contain slugified title: {id}"
    );

    let obs_dir = tmp.path().join(".factory/observations");
    let file = obs_dir.join(format!("{id}.md"));
    assert!(file.exists(), "observation file should exist");
    let content = fs::read_to_string(&file).unwrap();
    assert!(content.contains("Test observation content"));
}

#[test]
fn observations_add_from_stdin() {
    let tmp = TempDir::new().unwrap();

    let output = factory_cmd()
        .current_dir(tmp.path())
        .args(["observations", "add"])
        .write_stdin("Observation from stdin")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "add from stdin should succeed: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let id = stdout.trim();
    assert!(!id.is_empty());

    let file = tmp.path().join(format!(".factory/observations/{id}.md"));
    assert!(file.exists(), "observation file should exist");
    let content = fs::read_to_string(&file).unwrap();
    assert!(content.contains("Observation from stdin"));
}

#[test]
fn observations_add_empty_stdin_errors() {
    let tmp = TempDir::new().unwrap();

    let output = factory_cmd()
        .current_dir(tmp.path())
        .args(["observations", "add"])
        .write_stdin("")
        .output()
        .unwrap();

    assert!(!output.status.success(), "add with empty stdin should fail");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("No content provided on stdin"),
        "should show error message: {stderr}"
    );
}

#[test]
fn observations_resolve_inline() {
    let tmp = TempDir::new().unwrap();
    let obs_dir = tmp.path().join(".factory/observations");
    fs::create_dir_all(&obs_dir).unwrap();
    fs::write(
        obs_dir.join("20260612-000000-test-obs.md"),
        "Test obs body\n",
    )
    .unwrap();

    let output = factory_cmd()
        .current_dir(tmp.path())
        .args([
            "observations",
            "resolve",
            "20260612-000000-test-obs",
            "Fixed it",
        ])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "resolve should succeed: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );

    assert!(
        !obs_dir.join("20260612-000000-test-obs.md").exists(),
        "open file should be removed"
    );

    let resolved = obs_dir.join("resolved/20260612-000000-test-obs.md");
    assert!(resolved.exists(), "resolved file should exist");
    let content = fs::read_to_string(&resolved).unwrap();
    assert!(content.contains("Test obs body"));
    assert!(content.contains("Resolved: Fixed it"));
}

#[test]
fn observations_resolve_unknown_id_errors() {
    let tmp = TempDir::new().unwrap();

    let output = factory_cmd()
        .current_dir(tmp.path())
        .args(["observations", "resolve", "nonexistent-id", "whatever"])
        .output()
        .unwrap();

    assert!(!output.status.success(), "resolve unknown id should fail");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("No open observation matching"),
        "should name the missing id: {stderr}"
    );
}

#[test]
fn observations_resolve_prefix_unique_match() {
    let tmp = TempDir::new().unwrap();
    let obs_dir = tmp.path().join(".factory/observations");
    fs::create_dir_all(&obs_dir).unwrap();
    fs::write(
        obs_dir.join("20260612-143000-unique-entry.md"),
        "Unique observation\n",
    )
    .unwrap();

    let output = factory_cmd()
        .current_dir(tmp.path())
        .args(["observations", "resolve", "20260612-143", "Done"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "prefix resolve should succeed: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout.trim(), "20260612-143000-unique-entry");

    assert!(!obs_dir.join("20260612-143000-unique-entry.md").exists());
    assert!(
        obs_dir
            .join("resolved/20260612-143000-unique-entry.md")
            .exists()
    );
}

#[test]
fn observations_resolve_prefix_ambiguous_errors() {
    let tmp = TempDir::new().unwrap();
    let obs_dir = tmp.path().join(".factory/observations");
    fs::create_dir_all(&obs_dir).unwrap();
    fs::write(obs_dir.join("20260612-000000-alpha.md"), "a\n").unwrap();
    fs::write(obs_dir.join("20260612-000000-bravo.md"), "b\n").unwrap();

    let output = factory_cmd()
        .current_dir(tmp.path())
        .args(["observations", "resolve", "20260612", "Done"])
        .output()
        .unwrap();

    assert!(!output.status.success(), "ambiguous prefix should fail");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Ambiguous prefix"),
        "should mention ambiguous prefix: {stderr}"
    );
    assert!(
        stderr.contains("20260612-000000-alpha"),
        "should list matching ids: {stderr}"
    );
}

#[test]
fn observations_list_orders_chronologically() {
    let tmp = TempDir::new().unwrap();
    let obs_dir = tmp.path().join(".factory/observations");
    fs::create_dir_all(&obs_dir).unwrap();
    fs::write(obs_dir.join("20260612-120000-second.md"), "Second entry\n").unwrap();
    fs::write(obs_dir.join("20260611-100000-first.md"), "First entry\n").unwrap();
    fs::write(obs_dir.join("20260613-080000-third.md"), "Third entry\n").unwrap();

    let output = factory_cmd()
        .current_dir(tmp.path())
        .args(["observations", "list"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "list should succeed: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 3);
    assert!(lines[0].starts_with("20260611-100000-first"));
    assert!(lines[1].starts_with("20260612-120000-second"));
    assert!(lines[2].starts_with("20260613-080000-third"));
    assert!(lines[0].contains("First entry"));
}

#[test]
fn observations_show_open_and_resolved() {
    let tmp = TempDir::new().unwrap();
    let obs_dir = tmp.path().join(".factory/observations");
    let resolved_dir = obs_dir.join("resolved");
    fs::create_dir_all(&resolved_dir).unwrap();
    fs::write(obs_dir.join("20260612-open.md"), "Open observation body\n").unwrap();
    fs::write(
        resolved_dir.join("20260611-resolved.md"),
        "Resolved observation body\n",
    )
    .unwrap();

    // Show open observation
    let output = factory_cmd()
        .current_dir(tmp.path())
        .args(["observations", "show", "20260612-open"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Open observation body"));

    // Show resolved observation (falls back to resolved dir)
    let output = factory_cmd()
        .current_dir(tmp.path())
        .args(["observations", "show", "20260611-resolved"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Resolved observation body"));

    // Show unknown observation
    let output = factory_cmd()
        .current_dir(tmp.path())
        .args(["observations", "show", "nonexistent"])
        .output()
        .unwrap();
    assert!(!output.status.success());
}

#[test]
fn observations_migrate_splits_monolithic_files() {
    let tmp = TempDir::new().unwrap();
    let factory = tmp.path().join(".factory");
    fs::create_dir_all(&factory).unwrap();

    fs::write(
        factory.join("observations.md"),
        "# Observations\n\nOpen queue.\n\n---\n\n\
         2026-06-12 \u{2014} First open observation\nDetails here.\n\n\
         2026-06-12 \u{2014} Second open observation\nMore details.\n",
    )
    .unwrap();

    fs::write(
        factory.join("observations-resolved.md"),
        "# Resolved Observations\n\nResolved queue.\n\n---\n\n\
         2026-06-11 \u{2014} Old resolved observation\n\u{2192} Resolved: fixed.\n",
    )
    .unwrap();

    let output = factory_cmd()
        .current_dir(tmp.path())
        .args(["observations", "migrate"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "migrate should succeed: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Monolithic files removed
    assert!(
        !factory.join("observations.md").exists(),
        "observations.md should be removed"
    );
    assert!(
        !factory.join("observations-resolved.md").exists(),
        "observations-resolved.md should be removed"
    );

    // Per-file layout exists
    let obs_dir = factory.join("observations");
    assert!(obs_dir.is_dir());
    assert!(obs_dir.join("resolved").is_dir());

    // Open observations split correctly
    let open_files: Vec<String> = fs::read_dir(&obs_dir)
        .unwrap()
        .filter_map(|e| {
            let e = e.ok()?;
            if e.file_type().ok()?.is_file() {
                Some(e.file_name().to_string_lossy().to_string())
            } else {
                None
            }
        })
        .collect();
    assert_eq!(
        open_files.len(),
        2,
        "should have two open observation files"
    );

    // Resolved observations split correctly
    let resolved_files: Vec<String> = fs::read_dir(obs_dir.join("resolved"))
        .unwrap()
        .filter_map(|e| {
            let e = e.ok()?;
            Some(e.file_name().to_string_lossy().to_string())
        })
        .collect();
    assert_eq!(
        resolved_files.len(),
        1,
        "should have one resolved observation file"
    );

    // Content preserved verbatim
    let resolved_file = obs_dir.join("resolved").join(&resolved_files[0]);
    let content = fs::read_to_string(&resolved_file).unwrap();
    assert!(
        content.contains("Old resolved observation"),
        "resolved content should be preserved"
    );
    assert!(
        content.contains("\u{2192} Resolved: fixed."),
        "resolution context should be preserved"
    );

    // Idempotent: second run is a no-op
    let output2 = factory_cmd()
        .current_dir(tmp.path())
        .args(["observations", "migrate"])
        .output()
        .unwrap();
    assert!(output2.status.success());
    let stdout2 = String::from_utf8_lossy(&output2.stdout);
    assert!(stdout2.contains("Nothing to migrate"));
}

#[test]
fn no_direct_git_command_in_src() {
    let src_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut violations = Vec::new();
    collect_git_command_violations(&src_dir, &src_dir, &mut violations);
    assert!(
        violations.is_empty(),
        "Direct Command::new(\"git\") found outside src/git.rs — use the git wrapper instead:\n{}",
        violations.join("\n")
    );
}

fn collect_git_command_violations(dir: &Path, src_root: &Path, violations: &mut Vec<String>) {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };
    for entry in entries {
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        if path.is_dir() {
            collect_git_command_violations(&path, src_root, violations);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            let relative = path.strip_prefix(src_root).unwrap_or(&path);
            if relative == Path::new("git.rs") {
                continue;
            }
            let Ok(contents) = fs::read_to_string(&path) else {
                continue;
            };
            for (line_number, line) in contents.lines().enumerate() {
                if line.contains("Command::new(\"git\")") {
                    violations.push(format!(
                        "  {}:{}: {}",
                        relative.display(),
                        line_number + 1,
                        line.trim()
                    ));
                }
            }
        }
    }
}

#[test]
fn no_osascript_in_notify() {
    let notify_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/notify.rs");
    let contents = fs::read_to_string(&notify_path).expect("src/notify.rs should exist");
    for (line_number, line) in contents.lines().enumerate() {
        assert!(
            !line.contains("osascript"),
            "src/notify.rs:{}: osascript reference found — the macOS notification path should be removed: {}",
            line_number + 1,
            line.trim()
        );
    }
}

#[test]
#[serial(env_skip_log)]
fn log_command_writes_log_file_on_success() {
    let log_dir = log::test_log_dir_path();
    let _ = fs::create_dir_all(&log_dir);

    let test_name = log::test_current_test_name();
    let log_path = log_dir.join(format!("{test_name}.log"));

    let output = LoggedCommand::cargo_bin("factory")
        .arg("version")
        .output()
        .unwrap();

    assert!(output.status.success());
    assert!(
        log_path.exists(),
        "log file should be created at {}",
        log_path.display()
    );

    let content = fs::read_to_string(&log_path).unwrap();
    assert!(content.contains("=== "), "log should contain header");
    assert!(
        content.contains("command: factory version"),
        "log should contain command line"
    );
    assert!(content.contains("exit: 0"), "log should contain exit code");
    assert!(
        content.contains("---stdout---"),
        "log should contain stdout marker"
    );
    assert!(
        content.contains("---stderr---"),
        "log should contain stderr marker"
    );
}

#[test]
#[serial(env_skip_log)]
fn log_command_skips_on_factory_tests_skip_log() {
    let log_dir = log::test_log_dir_path();
    let test_name = log::test_current_test_name();
    let log_path = log_dir.join(format!("{test_name}.log"));

    let _ = fs::remove_file(&log_path);

    // SAFETY: this test runs a single LoggedCommand synchronously and
    // restores the variable immediately; no other thread reads
    // FACTORY_TESTS_SKIP_LOG in this window.
    unsafe { std::env::set_var("FACTORY_TESTS_SKIP_LOG", "1") };
    let output = LoggedCommand::cargo_bin("factory")
        .arg("version")
        .output()
        .unwrap();
    unsafe { std::env::remove_var("FACTORY_TESTS_SKIP_LOG") };

    assert!(output.status.success());
    assert!(
        !log_path.exists(),
        "log file should NOT be created when skip is set"
    );
}

#[test]
fn log_command_failed_command_appends_to_failed_sentinel() {
    let log_dir = log::test_log_dir_path();
    let _ = fs::create_dir_all(&log_dir);

    let failed_path = log_dir.join(".failed");
    let _ = fs::remove_file(&failed_path);

    let tmp = TempDir::new().unwrap();
    let output = LoggedCommand::cargo_bin("factory")
        .current_dir(tmp.path())
        .args(["work", "show", "nonexistent-work-item-for-test"])
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(
        failed_path.exists(),
        ".failed sentinel should exist after a failed command"
    );

    let content = fs::read_to_string(&failed_path).unwrap();
    assert!(
        !content.trim().is_empty(),
        ".failed sentinel should contain a log path"
    );
}

// --- auto-merge CLI tests ---

#[test]
fn auto_merge_with_both_flags_set_errors() {
    let tmp = TempDir::new().unwrap();
    let output = factory_cmd()
        .current_dir(tmp.path())
        .args(["work", "auto-merge", "some-work-item", "--all"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("mutually exclusive"),
        "expected mutually exclusive error, got: {stderr}"
    );
}

#[test]
fn auto_merge_with_neither_flag_set_errors() {
    let tmp = TempDir::new().unwrap();
    let output = factory_cmd()
        .current_dir(tmp.path())
        .args(["work", "auto-merge"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Work Item ID") || stderr.contains("--all"),
        "expected usage guidance, got: {stderr}"
    );
}

#[test]
fn auto_merge_single_mode_rejects_unknown_work_item_id() {
    let tmp = TempDir::new().unwrap();
    let output = factory_cmd()
        .current_dir(tmp.path())
        .args(["work", "auto-merge", "nonexistent-work-item"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("not found"),
        "expected not-found error, got: {stderr}"
    );
}

#[test]
fn auto_merge_skips_candidate_already_marked_skipped() {
    let tmp = TempDir::new().unwrap();
    git::run(tmp.path(), &["init", "-b", "main"], "init repo").unwrap();
    git::run(
        tmp.path(),
        &["commit", "--allow-empty", "-m", "init"],
        "initial commit",
    )
    .unwrap();
    factory_cmd()
        .current_dir(tmp.path())
        .args(["work", "create", "wi-skip", "--title", "Test skip"])
        .output()
        .unwrap();

    // Write a completed attempt with review_state passed
    let attempt_dir = tmp.path().join(".factory/work/attempts/wi-skip");
    fs::create_dir_all(&attempt_dir).unwrap();
    let attempt_json = serde_json::json!({
        "id": "attempt-1",
        "work_item_id": "wi-skip",
        "status": "complete",
        "review_state": "passed"
    });
    fs::write(
        attempt_dir.join("attempt-1.json"),
        serde_json::to_string_pretty(&attempt_json).unwrap(),
    )
    .unwrap();

    // Write a merge candidate with auto_merge_skipped set
    let mc_dir = tmp.path().join(".factory/work/merge-candidates/wi-skip");
    fs::create_dir_all(&mc_dir).unwrap();
    let candidate_json = serde_json::json!({
        "id": "attempt-1-merge-candidate",
        "attempt_id": "attempt-1",
        "source_workspace": { "id": "candidate", "path": "." },
        "target_workspace": { "id": "target", "path": "." },
        "source_branch": "main",
        "target_branch": "main",
        "candidate_commit": "abc123",
        "review_state": "passed",
        "merge_state": {
            "status": "pending",
            "auto_merge_skipped": true
        }
    });
    fs::write(
        mc_dir.join("attempt-1-merge-candidate.json"),
        serde_json::to_string_pretty(&candidate_json).unwrap(),
    )
    .unwrap();

    let mut child = std::process::Command::new(assert_cmd::cargo::cargo_bin("factory"))
        .current_dir(tmp.path())
        .args(["work", "auto-merge", "wi-skip", "--poll-seconds", "1"])
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();

    std::thread::sleep(std::time::Duration::from_secs(2));

    send_signal(child.id(), "INT");
    let output = child.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("[auto-merge] merged"),
        "should not have merged a skipped candidate: {stderr}"
    );
}

#[test]
fn auto_merge_exits_clean_on_sigterm() {
    let tmp = TempDir::new().unwrap();
    git::run(tmp.path(), &["init", "-b", "main"], "init repo").unwrap();
    git::run(
        tmp.path(),
        &["commit", "--allow-empty", "-m", "init"],
        "initial commit",
    )
    .unwrap();

    factory_cmd()
        .current_dir(tmp.path())
        .args(["work", "create", "wi-sig", "--title", "Test signal"])
        .output()
        .unwrap();

    let mut child = std::process::Command::new(assert_cmd::cargo::cargo_bin("factory"))
        .current_dir(tmp.path())
        .args(["work", "auto-merge", "wi-sig", "--poll-seconds", "1"])
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();

    std::thread::sleep(std::time::Duration::from_secs(2));

    send_signal(child.id(), "TERM");
    let status = child.wait().unwrap();
    assert!(
        status.success(),
        "auto-merge should exit cleanly on SIGTERM"
    );
}

fn send_signal(pid: u32, signal: &str) {
    std::process::Command::new("kill")
        .args([&format!("-{signal}"), &pid.to_string()])
        .status()
        .expect("send signal");
}

// --- Git wrapper lock-retry integration tests ---

fn init_git_repo(dir: &Path) {
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(dir)
        .output()
        .expect("git init");
    std::process::Command::new("git")
        .args(["commit", "--allow-empty", "-m", "init"])
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "test")
        .env("GIT_AUTHOR_EMAIL", "test@test.com")
        .env("GIT_COMMITTER_NAME", "test")
        .env("GIT_COMMITTER_EMAIL", "test@test.com")
        .output()
        .expect("initial commit");
}

#[test]
fn git_wrapper_succeeds_on_first_attempt_when_no_lock_error() {
    let tmp = TempDir::new().unwrap();
    init_git_repo(tmp.path());

    let result = git::run(tmp.path(), &["status"], "check status");
    assert!(result.is_ok(), "git status should succeed: {result:?}");
}

#[test]
fn git_wrapper_succeeds_after_config_lock_clears_within_budget() {
    let tmp = TempDir::new().unwrap();
    init_git_repo(tmp.path());

    let lock_path = tmp.path().join(".git/config.lock");
    fs::write(&lock_path, "lock").expect("create lock file");

    let lp = lock_path.clone();
    let handle = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(50));
        let _ = fs::remove_file(&lp);
    });

    let result = git::run(
        tmp.path(),
        &["config", "user.name", "test-user"],
        "set user name",
    );
    handle.join().unwrap();
    assert!(
        result.is_ok(),
        "git config should succeed after lock clears: {result:?}"
    );
}

#[test]
fn git_wrapper_succeeds_after_index_lock_clears_within_budget() {
    let tmp = TempDir::new().unwrap();
    init_git_repo(tmp.path());

    let lock_path = tmp.path().join(".git/index.lock");
    fs::write(&lock_path, "lock").expect("create lock file");

    let lp = lock_path.clone();
    let handle = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(50));
        let _ = fs::remove_file(&lp);
    });

    let result = git::run(tmp.path(), &["add", "."], "stage files");
    handle.join().unwrap();
    assert!(
        result.is_ok(),
        "git add should succeed after index lock clears: {result:?}"
    );
}

#[test]
fn git_wrapper_bails_when_lock_persists_past_budget() {
    let tmp = TempDir::new().unwrap();
    init_git_repo(tmp.path());

    let lock_path = tmp.path().join(".git/config.lock");
    fs::write(&lock_path, "lock").expect("create lock file");

    let start = std::time::Instant::now();
    let result = git::run(
        tmp.path(),
        &["config", "user.name", "test-user"],
        "set user name",
    );
    let elapsed = start.elapsed();

    assert!(result.is_err(), "should fail when lock persists");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("set user name"),
        "error should contain action: {err}"
    );
    assert!(
        elapsed.as_millis() > 100,
        "should have retried with backoff, elapsed: {elapsed:?}"
    );

    let _ = fs::remove_file(&lock_path);
}

#[test]
fn git_wrapper_does_not_retry_on_non_lock_error() {
    let tmp = TempDir::new().unwrap();
    init_git_repo(tmp.path());

    let start = std::time::Instant::now();
    let result = git::run(
        tmp.path(),
        &["checkout", "nonexistent-branch-xyz"],
        "switch branch",
    );
    let elapsed = start.elapsed();

    assert!(result.is_err(), "should fail for bad branch");
    assert!(
        elapsed.as_millis() < 500,
        "should not have slept for retries, elapsed: {elapsed:?}"
    );
}

#[test]
#[ignore]
fn git_wrapper_parallel_config_writes_both_succeed() {
    let tmp = TempDir::new().unwrap();
    init_git_repo(tmp.path());

    let p1 = tmp.path().to_path_buf();
    let p2 = tmp.path().to_path_buf();

    let h1 =
        std::thread::spawn(move || git::run(&p1, &["config", "user.name", "alice"], "set alice"));
    let h2 = std::thread::spawn(move || {
        git::run(&p2, &["config", "user.email", "bob@test.com"], "set bob")
    });

    let r1 = h1.join().unwrap();
    let r2 = h2.join().unwrap();
    assert!(
        r1.is_ok(),
        "first parallel config write should succeed: {r1:?}"
    );
    assert!(
        r2.is_ok(),
        "second parallel config write should succeed: {r2:?}"
    );
}

// =========================================================================
// Lint-style absence tests — regression guard against legacy re-introduction
// =========================================================================

fn grep_recursive_for(dir: &Path, forbidden: &[&str], skip_self: bool) -> Vec<String> {
    let mut offenders = Vec::new();
    grep_recursive_walk(dir, forbidden, skip_self, &mut offenders);
    offenders
}

fn grep_recursive_walk(
    dir: &Path,
    forbidden: &[&str],
    skip_self: bool,
    offenders: &mut Vec<String>,
) {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if name == "target" || name == ".git" {
            continue;
        }
        if path.is_dir() {
            grep_recursive_walk(&path, forbidden, skip_self, offenders);
            continue;
        }
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if ext != "rs" && ext != "md" && ext != "sh" && ext != "toml" {
            continue;
        }
        let content = match fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        for &needle in forbidden {
            for (line_num, line) in content.lines().enumerate() {
                if line.contains(needle) {
                    let trimmed = line.trim();
                    if skip_self && path.ends_with("binary.rs") {
                        if trimmed.starts_with('"') || trimmed.starts_with("//") {
                            continue;
                        }
                    }
                    // Skip negative assertions that verify absence
                    if trimmed.contains("!content.contains(") || trimmed.contains("!prompts.") {
                        continue;
                    }
                    offenders.push(format!(
                        "{}:{}: {}",
                        path.display(),
                        line_num + 1,
                        line.trim()
                    ));
                }
            }
        }
    }
}

#[test]
fn no_legacy_run_strings_in_src() {
    let project_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let src = project_root.join("src");
    let forbidden = &[
        ".factory/runs",
        "sessions.log",
        "active-run",
        "mod run;",
        "mod session;",
        "mod parallel;",
        "mod merge;",
    ];
    let offenders = grep_recursive_for(&src, forbidden, false);
    assert!(
        offenders.is_empty(),
        "Legacy run strings still present in src/:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn no_legacy_run_strings_in_documentation() {
    let project_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let forbidden = &[
        ".factory/runs",
        "factory run ",
        "factory resume",
        "factory watch",
        "sessions.log",
        "active-run",
        "legacy fallback",
        "transitional bridge",
    ];
    let mut offenders = Vec::new();
    for dir in &["documentation", "skills"] {
        let mut dir_offenders = grep_recursive_for(&project_root.join(dir), forbidden, false);
        // Exclude the DeleteLegacyRunModel EARS section in behaviors.md —
        // it names forbidden strings as specification (SHALL NOT assertions)
        dir_offenders.retain(|offender| {
            if !offender.contains("behaviors.md:") {
                return true;
            }
            // Extract line number and check if it's in the EARS assertion section
            let parts: Vec<&str> = offender.splitn(3, ':').collect();
            if parts.len() < 2 {
                return true;
            }
            let line_num: usize = parts[1].parse().unwrap_or(0);
            // The DeleteLegacyRunModel section starts near line 2320
            line_num < 2319
        });
        offenders.extend(dir_offenders);
    }
    let claude_md = project_root.join("CLAUDE.md");
    if claude_md.exists() {
        let content = fs::read_to_string(&claude_md).unwrap();
        for &needle in forbidden {
            for (line_num, line) in content.lines().enumerate() {
                if line.contains(needle) {
                    offenders.push(format!("CLAUDE.md:{}: {}", line_num + 1, line.trim()));
                }
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "Legacy run strings still present in documentation/skills/CLAUDE.md:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn no_legacy_prompt_files_in_prompts_dir() {
    let project_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let prompts_dir = project_root.join("prompts");

    assert!(
        !prompts_dir.join("author.md").exists(),
        "Legacy prompts/author.md should not exist"
    );

    let allowed_prefixes = ["work-", "review-"];
    for entry in fs::read_dir(&prompts_dir).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.ends_with(".md") {
            continue;
        }
        assert!(
            allowed_prefixes
                .iter()
                .any(|prefix| name.starts_with(prefix)),
            "Unexpected prompt file: {name}. Only work-* and review-* prompts should exist."
        );
    }
}

// =========================================================================
// CLI verification tests — deleted subcommands absent from help
// =========================================================================

#[test]
fn deleted_subcommands_absent_from_help() {
    let output = factory_cmd().args(["--help"]).output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let in_commands = stdout
        .lines()
        .skip_while(|line| !line.contains("Commands:"))
        .take_while(|line| !line.is_empty() || line.contains("Commands:"))
        .collect::<Vec<_>>()
        .join("\n");
    for cmd in [
        "run", "resume", "watch", "summary", "pull", "shell", "merge", "review",
    ] {
        assert!(
            !in_commands.lines().any(|line| line.trim().starts_with(cmd)),
            "Deleted subcommand {cmd:?} should not appear in Commands section:\n{in_commands}"
        );
    }
    assert!(
        in_commands.contains("work"),
        "Work subcommand should appear"
    );
    assert!(
        in_commands.contains("status"),
        "Status subcommand should appear"
    );
}

// =========================================================================
// Queue CLI tests
// =========================================================================

#[test]
fn work_queue_add_and_list_round_trip() {
    let tmp = TempDir::new().unwrap();
    write_work_item_json(tmp.path(), "wi-q1", "Queue test");

    factory_cmd()
        .current_dir(tmp.path())
        .args(["work", "queue", "add", "wi-q1", "--priority", "5"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Queued Work Item wi-q1"));

    factory_cmd()
        .current_dir(tmp.path())
        .args(["work", "queue", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("5"))
        .stdout(predicate::str::contains("queued"))
        .stdout(predicate::str::contains("wi-q1"));
}

#[test]
fn work_queue_add_unknown_work_item_errors() {
    let tmp = TempDir::new().unwrap();
    fs::create_dir_all(tmp.path().join(".factory/work/items")).unwrap();

    factory_cmd()
        .current_dir(tmp.path())
        .args(["work", "queue", "add", "nonexistent"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("not found"));
}

#[test]
fn work_queue_add_existing_with_priority_updates_only_priority() {
    let tmp = TempDir::new().unwrap();
    write_work_item_json(tmp.path(), "wi-q2", "Priority update");

    factory_cmd()
        .current_dir(tmp.path())
        .args(["work", "queue", "add", "wi-q2", "--priority", "3"])
        .assert()
        .success();

    factory_cmd()
        .current_dir(tmp.path())
        .args(["work", "queue", "add", "wi-q2", "--priority", "10"])
        .assert()
        .success();

    let output = factory_cmd()
        .current_dir(tmp.path())
        .args(["work", "queue", "list"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.lines().any(|l| l.starts_with("10 ")),
        "a line should start with priority 10: {stdout}"
    );
    assert!(stdout.contains("wi-q2"));
}

#[test]
fn work_queue_list_format_includes_priority_queued_at_status_id() {
    let tmp = TempDir::new().unwrap();
    write_work_item_json(tmp.path(), "wi-fmt", "Format test");

    factory_cmd()
        .current_dir(tmp.path())
        .args(["work", "queue", "add", "wi-fmt"])
        .assert()
        .success();

    let output = factory_cmd()
        .current_dir(tmp.path())
        .args(["work", "queue", "list"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let line = stdout.lines().next().unwrap();
    assert!(
        line.starts_with("0 "),
        "line should start with default priority 0: {line}"
    );
    assert!(line.contains("queued"), "should contain status");
    assert!(line.contains("wi-fmt"), "should contain work item id");
}

#[test]
fn work_queue_remove_after_add_removes_entry() {
    let tmp = TempDir::new().unwrap();
    write_work_item_json(tmp.path(), "wi-rm", "Remove test");

    factory_cmd()
        .current_dir(tmp.path())
        .args(["work", "queue", "add", "wi-rm"])
        .assert()
        .success();

    factory_cmd()
        .current_dir(tmp.path())
        .args(["work", "queue", "remove", "wi-rm"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Removed wi-rm"));

    factory_cmd()
        .current_dir(tmp.path())
        .args(["work", "queue", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("empty"));
}

#[test]
fn work_queue_remove_unknown_errors() {
    let tmp = TempDir::new().unwrap();
    fs::create_dir_all(tmp.path().join(".factory/work/items")).unwrap();

    factory_cmd()
        .current_dir(tmp.path())
        .args(["work", "queue", "remove", "nonexistent"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("not queued"));
}

// =========================================================================
// Scheduler CLI tests
// =========================================================================

#[test]
fn work_scheduler_run_exits_clean_on_sigterm_when_idle() {
    let tmp = TempDir::new().unwrap();
    fs::create_dir_all(tmp.path().join(".factory/work/items")).unwrap();

    let mut child = std::process::Command::new(assert_cmd::cargo::cargo_bin("factory"))
        .current_dir(tmp.path())
        .args(["work", "scheduler", "run", "--poll-seconds", "1"])
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();

    std::thread::sleep(std::time::Duration::from_secs(2));
    send_signal(child.id(), "TERM");
    let status = child.wait().unwrap();
    assert!(
        status.success(),
        "scheduler should exit cleanly on SIGTERM when idle"
    );
}

#[test]
fn work_scheduler_run_processes_queued_work_item_end_to_end() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path();

    let bin_dir = project.join("mock-bin");
    write_mock_claude(
        &bin_dir,
        r#"#!/bin/bash
# Mock writer: create a commit in the workspace
git add -A 2>/dev/null
git commit --allow-empty -m "mock write" 2>/dev/null
exit 0
"#,
    );

    write_work_item_json(project, "wi-sched", "Scheduler test");

    factory_cmd()
        .current_dir(project)
        .args(["work", "queue", "add", "wi-sched", "--priority", "1"])
        .assert()
        .success();

    let queue_entry_path = project.join(".factory/work/queue/wi-sched.json");
    assert!(queue_entry_path.exists());
    let before: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&queue_entry_path).unwrap()).unwrap();
    assert_eq!(before["status"], "queued");

    let child = std::process::Command::new(assert_cmd::cargo::cargo_bin("factory"))
        .current_dir(project)
        .env(
            "PATH",
            format!(
                "{}:{}",
                bin_dir.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .args(["work", "scheduler", "run", "--poll-seconds", "1"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        std::thread::sleep(std::time::Duration::from_millis(500));
        let entry: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&queue_entry_path).unwrap()).unwrap();
        let s = entry["status"].as_str().unwrap_or("");
        if s == "done" || s == "failed" {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "scheduler did not reach terminal state within 30s, got: {s}"
        );
    }
    send_signal(child.id(), "TERM");
    let output = child.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        stderr.contains("[scheduler] starting wi-sched"),
        "scheduler should log start: {stderr}"
    );

    let after: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&queue_entry_path).unwrap()).unwrap();
    let status = after["status"].as_str().unwrap_or("");
    assert!(
        status == "done" || status == "failed",
        "queue entry should be terminal after scheduler runs, got: {status}"
    );
}
