#[path = "lib/log.rs"]
mod log;

use fluent::git;
use fluent::review;
use log::LoggedCommand;
use predicates::prelude::*;
use serial_test::serial;
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

fn fluent_cmd() -> LoggedCommand {
    let mut cmd = LoggedCommand::cargo_bin("fluent");
    cmd.env_remove("FLUENT_TASK_KIND");
    cmd.env("FLUENT_NO_UPDATE_CHECK", "1");
    cmd
}

fn work_item_value(project_root: &Path, id: &str) -> serde_json::Value {
    let output = fluent_cmd()
        .current_dir(project_root)
        .args(["work-item", "show", id])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "show failed: stdout={} stderr={}",
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
fn fluent_help_lists_tester_subcommand() {
    let tmp = TempDir::new().unwrap();
    let output = fluent_cmd()
        .current_dir(tmp.path())
        .args(["--help"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("tester"),
        "fluent --help should list the tester command; got:\n{stdout}"
    );
}

#[test]
fn version_prints_package_version_and_commit() {
    let tmp = TempDir::new().unwrap();

    let output = fluent_cmd()
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
    assert_eq!(fields[0], "fluent");
    assert_eq!(fields[1], env!("CARGO_PKG_VERSION"));
    let commit = fields[2];
    assert!(
        commit == "unknown" || (commit.len() >= 7 && commit.chars().all(|c| c.is_ascii_hexdigit())),
        "commit field should be 'unknown' or a short hex hash, got: {commit}"
    );
}

#[test]
fn version_flag_matches_version_subcommand() {
    let tmp = TempDir::new().unwrap();

    let subcommand = fluent_cmd()
        .current_dir(tmp.path())
        .arg("version")
        .output()
        .unwrap();
    assert!(subcommand.status.success());

    let flag = fluent_cmd()
        .current_dir(tmp.path())
        .arg("--version")
        .output()
        .unwrap();
    assert!(
        flag.status.success(),
        "--version failed: stderr={}",
        String::from_utf8_lossy(&flag.stderr)
    );

    let subcommand_out = String::from_utf8(subcommand.stdout).unwrap();
    let flag_out = String::from_utf8(flag.stdout).unwrap();
    assert_eq!(
        subcommand_out.trim(),
        flag_out.trim(),
        "--version output must match `version` subcommand output"
    );
}

#[test]
fn fluent_skills_install_writes_all_public_skills() {
    let tmp = TempDir::new().unwrap();

    let output = fluent_cmd()
        .current_dir(tmp.path())
        .env("HOME", tmp.path().to_string_lossy().to_string())
        .arg("skills")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "skills failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);

    let expected_skills = [
        "fluent",
        "review-architecture",
        "review-behaviors",
        "review-documentation",
        "review-skills",
        "review-tests",
    ];

    for skill_name in &expected_skills {
        let skill_md = tmp
            .path()
            .join(format!(".claude/skills/{skill_name}/SKILL.md"));
        assert!(
            skill_md.is_file(),
            "SKILL.md should exist for {skill_name} at {}",
            skill_md.display()
        );
    }

    assert!(
        stderr.contains("Installed 6 skills"),
        "should report installing all skills: {stderr}"
    );

    let refs_dir = tmp.path().join(".claude/skills/fluent/references");
    assert!(
        refs_dir.is_dir(),
        "references/ should exist at {}",
        refs_dir.display()
    );

    let capture = refs_dir.join("capture-brief.md");
    assert!(capture.is_file(), "capture-brief.md reference should exist");
}

#[test]
fn fargate_teardown_nothing_to_teardown() {
    let tmp = TempDir::new().unwrap();

    let output = fluent_cmd()
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

    let output = fluent_cmd()
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

    let state_dir = tmp.path().join(".config/fluent");
    fs::create_dir_all(&state_dir).unwrap();
    let state_path = state_dir.join("fargate.state.json");
    fs::write(
        &state_path,
        r#"{
  "stack_deployed": true,
  "region": "us-west-2",
  "repo_uri": "123.dkr.ecr.us-west-2.amazonaws.com/fluent/run",
  "s3_bucket": "fluent-workspace-123"
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
    printf 'fluent/run\n'
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

    let output = fluent_cmd()
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

    let state_dir = tmp.path().join(".config/fluent");
    fs::create_dir_all(&state_dir).unwrap();
    let state_path = state_dir.join("fargate.state.json");
    fs::write(
        &state_path,
        r#"{
  "stack_deployed": true,
  "region": "us-west-2",
  "repo_uri": "123.dkr.ecr.us-west-2.amazonaws.com/fluent/run",
  "s3_bucket": "fluent-workspace-123"
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

    let output = fluent_cmd()
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

    let state_dir = tmp.path().join(".config/fluent");
    fs::create_dir_all(&state_dir).unwrap();
    let state_path = state_dir.join("fargate.state.json");
    fs::write(
        &state_path,
        r#"{
  "stack_deployed": true,
  "region": "us-west-2",
  "repo_uri": "123.dkr.ecr.us-west-2.amazonaws.com/fluent/run",
  "s3_bucket": "fluent-workspace-123"
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
    printf 'fluent/run\n'
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

    let output = fluent_cmd()
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

    let state_dir = tmp.path().join(".config/fluent");
    fs::create_dir_all(&state_dir).unwrap();
    let state_path = state_dir.join("fargate.state.json");
    fs::write(
        &state_path,
        r#"{
  "stack_deployed": true,
  "region": "us-west-2",
  "repo_uri": "123.dkr.ecr.us-west-2.amazonaws.com/fluent/run",
  "s3_bucket": "fluent-workspace-123"
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
    printf 'fluent/run\n'
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

    let output = fluent_cmd()
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
    let fluent_src = tmp.path().join("fluent-src");
    fs::create_dir_all(project_dir.join(".fluent")).unwrap();
    fs::create_dir_all(fluent_src.join("infrastructure/run")).unwrap();
    fs::write(
        fluent_src.join("Cargo.toml"),
        "[package]\nname = \"fluent\"\n",
    )
    .unwrap();
    fs::write(
        fluent_src.join("infrastructure/run/Dockerfile"),
        "FROM node:latest\n",
    )
    .unwrap();
    fs::write(
        fluent_src.join("infrastructure/run/entrypoint.sh"),
        "#!/bin/sh\n",
    )
    .unwrap();

    let state_dir = tmp.path().join(".config/fluent");
    fs::create_dir_all(&state_dir).unwrap();
    fs::write(
        state_dir.join("fargate.state.json"),
        r#"{
  "stack_deployed": true,
  "region": "us-west-2",
  "cluster_arn": "arn:aws:ecs:us-west-2:123:cluster/fluent",
  "task_def_arn": "arn:aws:ecs:us-west-2:123:task-definition/fluent-run:1",
  "repo_uri": "123456789012.dkr.ecr.us-west-2.amazonaws.com/fluent/run",
  "s3_bucket": "fluent-workspace-123",
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
      printf '{"family":"fluent-run","containerDefinitions":[{"name":"run","image":"placeholder","essential":true}],"requiresCompatibilities":["FARGATE"],"networkMode":"awsvpc","cpu":"1024","memory":"2048"}\n'
    fi
    ;;
  "ecs register-task-definition")
    printf 'arn:aws:ecs:us-west-2:123:task-definition/fluent-run:2\n'
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

    let output = fluent_cmd()
        .current_dir(&project_dir)
        .env("HOME", tmp.path())
        .env("PATH", mock_path(&bin_dir))
        .env("AWS_LOG", &aws_log)
        .env("FLUENT_SOURCE_ROOT", &fluent_src)
        .args(["fargate", "ensure-setup"])
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "ensure-setup should succeed: stdout={} stderr={stderr}",
        String::from_utf8_lossy(&output.stdout)
    );

    let dockerfile = project_dir.join(".fluent/Dockerfile");
    assert!(dockerfile.exists(), "Dockerfile stub should be created");
    let content = fs::read_to_string(&dockerfile).unwrap();
    assert!(
        content.contains("ARG FLUENT_BASE_URI"),
        "stub should contain ARG: {content}"
    );
    assert!(
        content.contains("FROM ${FLUENT_BASE_URI}"),
        "stub should contain FROM: {content}"
    );
    assert!(
        stderr.contains("Created .fluent/Dockerfile stub"),
        "should report stub creation: {stderr}"
    );
}

#[test]
fn fargate_ensure_setup_skips_base_build_when_ecr_tag_exists() {
    let tmp = TempDir::new().unwrap();
    let bin_dir = tmp.path().join("bin");
    let project_dir = tmp.path().join("my-project");
    let fluent_src = tmp.path().join("fluent-src");
    fs::create_dir_all(project_dir.join(".fluent")).unwrap();
    fs::create_dir_all(fluent_src.join("infrastructure/run")).unwrap();
    fs::write(
        fluent_src.join("Cargo.toml"),
        "[package]\nname = \"fluent\"\n",
    )
    .unwrap();
    fs::write(
        fluent_src.join("infrastructure/run/Dockerfile"),
        "FROM node:latest\n",
    )
    .unwrap();
    fs::write(
        fluent_src.join("infrastructure/run/entrypoint.sh"),
        "#!/bin/sh\n",
    )
    .unwrap();

    let state_dir = tmp.path().join(".config/fluent");
    fs::create_dir_all(&state_dir).unwrap();
    fs::write(
        state_dir.join("fargate.state.json"),
        r#"{
  "stack_deployed": true,
  "region": "us-west-2",
  "cluster_arn": "arn:aws:ecs:us-west-2:123:cluster/fluent",
  "task_def_arn": "arn:aws:ecs:us-west-2:123:task-definition/fluent-run:1",
  "repo_uri": "123456789012.dkr.ecr.us-west-2.amazonaws.com/fluent/run",
  "s3_bucket": "fluent-workspace-123",
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
      printf '{"family":"fluent-run","containerDefinitions":[{"name":"run","image":"placeholder","essential":true}],"requiresCompatibilities":["FARGATE"],"networkMode":"awsvpc","cpu":"1024","memory":"2048"}\n'
    fi
    ;;
  "ecs register-task-definition")
    printf 'arn:aws:ecs:us-west-2:123:task-definition/fluent-run:2\n'
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

    let output = fluent_cmd()
        .current_dir(&project_dir)
        .env("HOME", tmp.path())
        .env("PATH", mock_path(&bin_dir))
        .env("AWS_LOG", &aws_log)
        .env("FLUENT_SOURCE_ROOT", &fluent_src)
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
    let fluent_src = tmp.path().join("fluent-src");
    fs::create_dir_all(project_dir.join(".fluent")).unwrap();
    fs::create_dir_all(fluent_src.join("infrastructure/run")).unwrap();
    fs::write(
        fluent_src.join("Cargo.toml"),
        "[package]\nname = \"fluent\"\n",
    )
    .unwrap();
    fs::write(
        fluent_src.join("infrastructure/run/Dockerfile"),
        "FROM node:latest\n",
    )
    .unwrap();
    fs::write(
        fluent_src.join("infrastructure/run/entrypoint.sh"),
        "#!/bin/sh\n",
    )
    .unwrap();
    fs::write(
        project_dir.join(".fluent/Dockerfile"),
        "ARG FLUENT_BASE_URI\nFROM ${FLUENT_BASE_URI}\nRUN echo hello\n",
    )
    .unwrap();

    let state_dir = tmp.path().join(".config/fluent");
    fs::create_dir_all(&state_dir).unwrap();
    fs::write(
        state_dir.join("fargate.state.json"),
        r#"{
  "stack_deployed": true,
  "region": "us-west-2",
  "cluster_arn": "arn:aws:ecs:us-west-2:123:cluster/fluent",
  "task_def_arn": "arn:aws:ecs:us-west-2:123:task-definition/fluent-run:1",
  "repo_uri": "123456789012.dkr.ecr.us-west-2.amazonaws.com/fluent/run",
  "s3_bucket": "fluent-workspace-123",
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
      printf '123456789012.dkr.ecr.us-west-2.amazonaws.com/fluent/run:project-existing\n'
    else
      printf '{"family":"fluent-run","containerDefinitions":[{"name":"run","image":"123456789012.dkr.ecr.us-west-2.amazonaws.com/fluent/run:project-existing","essential":true}],"requiresCompatibilities":["FARGATE"],"networkMode":"awsvpc","cpu":"1024","memory":"2048"}\n'
    fi
    ;;
  "ecs register-task-definition")
    printf 'arn:aws:ecs:us-west-2:123:task-definition/fluent-run:2\n'
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

    let output = fluent_cmd()
        .current_dir(&project_dir)
        .env("HOME", tmp.path())
        .env("PATH", mock_path(&bin_dir))
        .env("AWS_LOG", &aws_log)
        .env("FLUENT_SOURCE_ROOT", &fluent_src)
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

    let output = fluent_cmd()
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
fn init_creates_fluent_structure() {
    let tmp = TempDir::new().unwrap();

    fluent_cmd()
        .current_dir(tmp.path())
        .arg("init")
        .assert()
        .success()
        .stderr(predicate::str::contains("Initialized .fluent/"));

    assert!(tmp.path().join(".fluent/expertise").is_dir());
}

#[test]
fn init_is_idempotent() {
    let tmp = TempDir::new().unwrap();

    fluent_cmd()
        .current_dir(tmp.path())
        .arg("init")
        .assert()
        .success();

    fluent_cmd()
        .current_dir(tmp.path())
        .arg("init")
        .assert()
        .success()
        .stderr(predicate::str::contains("Already initialized"));
}

#[test]
fn init_writes_gitignore_when_absent() {
    let tmp = TempDir::new().unwrap();

    fluent_cmd()
        .current_dir(tmp.path())
        .arg("init")
        .assert()
        .success();

    let gitignore = tmp.path().join(".fluent/.gitignore");
    assert!(
        gitignore.is_file(),
        ".fluent/.gitignore should exist after init"
    );
}

#[test]
fn init_gitignore_excludes_working_state_and_tracks_durable() {
    let tmp = TempDir::new().unwrap();
    init_git_repo(tmp.path());

    fluent_cmd()
        .current_dir(tmp.path())
        .arg("init")
        .assert()
        .success();

    // Create directories so git can distinguish files from dirs
    let fluent = tmp.path().join(".fluent");
    for dir in &["work", "drafts", "expertise", "observations", "hooks"] {
        fs::create_dir_all(fluent.join(dir)).unwrap();
    }

    // Ephemeral paths must be ignored
    for path in &["work", "drafts", "observations"] {
        let full = format!(".fluent/{}", path);
        let output = std::process::Command::new("git")
            .args(["check-ignore", &full])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            ".fluent/{path} should be ignored by git"
        );
    }

    // Durable paths must NOT be ignored
    for path in &[
        ".gitignore",
        "expertise",
        "hooks",
        "Dockerfile",
        "tester.yaml",
        "extract-tester-results",
    ] {
        let full = format!(".fluent/{}", path);
        let output = std::process::Command::new("git")
            .args(["check-ignore", &full])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        assert!(
            !output.status.success(),
            ".fluent/{path} should NOT be ignored by git"
        );
    }
}

#[test]
fn init_preserves_existing_gitignore() {
    let tmp = TempDir::new().unwrap();
    let fluent_dir = tmp.path().join(".fluent");
    fs::create_dir_all(&fluent_dir).unwrap();
    let gitignore = fluent_dir.join(".gitignore");
    fs::write(&gitignore, "# custom content\n").unwrap();

    fluent_cmd()
        .current_dir(tmp.path())
        .arg("init")
        .assert()
        .success();

    let content = fs::read_to_string(&gitignore).unwrap();
    assert_eq!(
        content, "# custom content\n",
        "existing .gitignore should be preserved"
    );
}

#[test]
fn init_backfills_gitignore_on_existing_fluent() {
    let tmp = TempDir::new().unwrap();

    // First init creates .fluent/
    fluent_cmd()
        .current_dir(tmp.path())
        .arg("init")
        .assert()
        .success();

    // Remove the .gitignore to simulate a pre-existing .fluent/ without one
    let gitignore = tmp.path().join(".fluent/.gitignore");
    fs::remove_file(&gitignore).unwrap();
    assert!(!gitignore.exists());

    // Second init should backfill the .gitignore
    fluent_cmd()
        .current_dir(tmp.path())
        .arg("init")
        .assert()
        .success();

    assert!(
        gitignore.is_file(),
        ".gitignore should be backfilled on existing .fluent/"
    );
}

#[test]
fn init_gitignore_does_not_allowlist_observations() {
    let tmp = TempDir::new().unwrap();

    fluent_cmd()
        .current_dir(tmp.path())
        .arg("init")
        .assert()
        .success();

    let content = fs::read_to_string(tmp.path().join(".fluent/.gitignore")).unwrap();
    assert!(
        !content.contains("observations"),
        ".fluent/.gitignore must not re-include observations; got:\n{content}"
    );
}

#[test]
fn init_output_notes_fluent_tracks_its_state() {
    let tmp = TempDir::new().unwrap();

    // Fresh init
    fluent_cmd()
        .current_dir(tmp.path())
        .arg("init")
        .assert()
        .success()
        .stderr(predicate::str::contains(".fluent/").and(predicate::str::contains("commits")));

    // Already initialized
    fluent_cmd()
        .current_dir(tmp.path())
        .arg("init")
        .assert()
        .success()
        .stderr(predicate::str::contains(".fluent/").and(predicate::str::contains("commits")));
}

#[test]
fn init_prints_layout_tip_when_dir_not_named_main() {
    let tmp = TempDir::new().unwrap();

    fluent_cmd()
        .current_dir(tmp.path())
        .arg("init")
        .assert()
        .success()
        .stderr(predicate::str::contains("Tip:"));
}

#[test]
fn init_no_layout_tip_when_dir_named_main() {
    let tmp = TempDir::new().unwrap();
    let main_dir = tmp.path().join("main");
    fs::create_dir(&main_dir).unwrap();

    fluent_cmd()
        .current_dir(&main_dir)
        .arg("init")
        .assert()
        .success()
        .stderr(predicate::str::contains("Tip:").not());
}

// -------------------------------------------------------------------------
// Init — craft section seeding
// -------------------------------------------------------------------------

#[test]
fn init_appends_craft_section_to_existing_agents_md() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let project = tmp.path().join("project");
    fs::create_dir_all(&project).unwrap();

    fs::write(project.join("AGENTS.md"), "# My Project\n").unwrap();

    fluent_cmd()
        .env("HOME", home.to_str().unwrap())
        .current_dir(&project)
        .arg("init")
        .assert()
        .success();

    let content = fs::read_to_string(project.join("AGENTS.md")).unwrap();
    assert!(
        content.starts_with("# My Project\n"),
        "existing content must be preserved"
    );
    assert!(
        content.contains("<!-- BEGIN fluent -->"),
        "craft section begin marker must be present"
    );
    assert!(
        content.contains("<!-- END fluent -->"),
        "craft section end marker must be present"
    );
}

#[test]
fn init_creates_agents_md_with_craft_section_when_none() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let project = tmp.path().join("project");
    fs::create_dir_all(&project).unwrap();

    assert!(!project.join("AGENTS.md").exists());
    assert!(!project.join("CLAUDE.md").exists());

    fluent_cmd()
        .env("HOME", home.to_str().unwrap())
        .current_dir(&project)
        .arg("init")
        .assert()
        .success();

    assert!(
        project.join("AGENTS.md").exists(),
        "AGENTS.md must be created when neither AGENTS.md nor CLAUDE.md exist"
    );
    let content = fs::read_to_string(project.join("AGENTS.md")).unwrap();
    assert!(content.contains("<!-- BEGIN fluent -->"));
    assert!(content.contains("<!-- END fluent -->"));
}

#[test]
fn init_updates_craft_section_in_place_idempotently() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let project = tmp.path().join("project");
    fs::create_dir_all(&project).unwrap();

    let pre = "# My Project\n\nSome content.\n";
    let post = "\n## Footer\n";
    let old_block = "<!-- BEGIN fluent -->\nold craft content\n<!-- END fluent -->";
    fs::write(project.join("AGENTS.md"), format!("{pre}{old_block}{post}")).unwrap();

    fluent_cmd()
        .env("HOME", home.to_str().unwrap())
        .current_dir(&project)
        .arg("init")
        .assert()
        .success();

    let content = fs::read_to_string(project.join("AGENTS.md")).unwrap();
    assert!(
        content.starts_with(pre),
        "content before the markers must be preserved"
    );
    assert!(
        content.ends_with(post),
        "content after the markers must be preserved"
    );
    assert!(
        !content.contains("old craft content"),
        "old craft content must be replaced"
    );
    assert!(
        content.contains("<!-- BEGIN fluent -->"),
        "begin marker must be present"
    );
    assert!(
        content.contains("<!-- END fluent -->"),
        "end marker must be present"
    );
    assert_eq!(
        content.matches("<!-- BEGIN fluent -->").count(),
        1,
        "begin marker must appear exactly once"
    );
}

#[test]
fn craft_section_names_skill_and_lifecycle_stages() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let project = tmp.path().join("project");
    fs::create_dir_all(&project).unwrap();

    fluent_cmd()
        .env("HOME", home.to_str().unwrap())
        .current_dir(&project)
        .arg("init")
        .assert()
        .success();

    let content = fs::read_to_string(project.join("AGENTS.md")).unwrap();
    assert!(
        content.contains("fluent skill"),
        "craft section must name the fluent skill"
    );
    for stage in &[
        "brief",
        "behaviors",
        "approach",
        "plan",
        "execute",
        "review",
        "land",
    ] {
        assert!(
            content.contains(stage),
            "craft section must mention lifecycle stage: {stage}"
        );
    }
}

fn craft_section_content(tmp: &TempDir) -> String {
    let home = tmp.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let project = tmp.path().join("project");
    fs::create_dir_all(&project).unwrap();

    fluent_cmd()
        .env("HOME", home.to_str().unwrap())
        .current_dir(&project)
        .arg("init")
        .assert()
        .success();

    fs::read_to_string(project.join("AGENTS.md")).unwrap()
}

#[test]
fn craft_section_is_self_sufficient_for_skilless_operator() {
    let tmp = TempDir::new().unwrap();
    let content = craft_section_content(&tmp);

    assert!(
        content.contains("user conversation"),
        "craft section must state the first stages are a user conversation"
    );
    assert!(
        content.contains("→ Next:") || content.to_lowercase().contains("next-action"),
        "craft section must tell the operator to follow the next-action line"
    );
    assert!(
        content.to_lowercase().contains("question"),
        "craft section must teach how to ask the user questions"
    );
}

#[test]
fn craft_section_states_scaffolding_is_committed() {
    let tmp = TempDir::new().unwrap();
    let content = craft_section_content(&tmp);

    assert!(
        content.contains(".fluent/"),
        "craft section must mention the .fluent/ scaffolding"
    );
    assert!(
        content.to_lowercase().contains("commit"),
        "craft section must state the scaffolding is committed"
    );
}

#[test]
fn craft_section_presents_question_format() {
    let tmp = TempDir::new().unwrap();
    let content = craft_section_content(&tmp);

    assert!(
        content.contains("Decision"),
        "craft section must name the Decision archetype"
    );
    assert!(
        content.contains("Confirm gate"),
        "craft section must name the Confirm gate archetype"
    );
    assert!(
        content.contains("yes (y)"),
        "craft section must state the yes (y) confirm convention"
    );
    assert!(
        content.contains("(recommended"),
        "craft section must mark the recommended option"
    );
    assert!(
        content.to_lowercase().contains("one question at a time"),
        "craft section must state one question at a time"
    );
}

#[test]
fn craft_section_stays_summary_names_skill() {
    let tmp = TempDir::new().unwrap();
    let content = craft_section_content(&tmp);

    assert!(
        content.contains("fluent skill"),
        "craft section must name the fluent skill as the deep reference"
    );
    assert!(
        content.to_lowercase().contains("summary") || content.to_lowercase().contains("reference"),
        "craft section must present itself as a summary that defers to the skill"
    );

    let begin = content.find("<!-- BEGIN fluent -->").unwrap();
    let end = content.find("<!-- END fluent -->").unwrap();
    let block_lines = content[begin..end].lines().count();
    assert!(
        block_lines < 60,
        "craft section should stay a concise summary, not inline the stage procedures; got {block_lines} lines"
    );
}

#[test]
fn init_succeeds_when_craft_section_write_fails() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let project = tmp.path().join("project");
    fs::create_dir_all(&project).unwrap();

    // Create a read-only AGENTS.md so the craft section write fails.
    let agents = project.join("AGENTS.md");
    fs::write(&agents, "# Existing\n").unwrap();
    let mut perms = fs::metadata(&agents).unwrap().permissions();
    perms.set_readonly(true);
    fs::set_permissions(&agents, perms).unwrap();

    let output = fluent_cmd()
        .env("HOME", home.to_str().unwrap())
        .current_dir(&project)
        .args(["init"])
        .output()
        .unwrap();

    // Restore write permission for cleanup.
    let mut perms = fs::metadata(&agents).unwrap().permissions();
    perms.set_readonly(false);
    fs::set_permissions(&agents, perms).unwrap();

    assert!(
        output.status.success(),
        "init must succeed even when craft section write fails"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("warning: could not seed agent instructions"),
        "init must print a warning when craft section write fails: {stderr}"
    );
}

// -------------------------------------------------------------------------
// Status
// -------------------------------------------------------------------------

#[test]
fn status_no_fluent_dir() {
    let tmp = TempDir::new().unwrap();

    fluent_cmd()
        .current_dir(tmp.path())
        .arg("status")
        .assert()
        .success()
        .stdout(predicate::str::contains("No Work Items found"));
}

#[test]
fn status_shows_work_items_without_runs() {
    let tmp = TempDir::new().unwrap();

    fluent_cmd()
        .current_dir(tmp.path())
        .args([
            "work-item",
            "create",
            "work-1",
            "--title",
            "Build status view",
        ])
        .assert()
        .success();
    fluent_cmd()
        .current_dir(tmp.path())
        .args(["attempt", "create", "work-1", "attempt-1"])
        .assert()
        .success();

    fluent_cmd()
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

    fluent_cmd()
        .current_dir(tmp.path())
        .args([
            "work-item",
            "create",
            "work-intake",
            "--title",
            "Intake title",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Created Work Item work-intake"));

    let path = tmp.path().join(".fluent/work/items/work-intake.json");
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

    fluent_cmd()
        .current_dir(tmp.path())
        .args([
            "work-item",
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
        fs::read_to_string(tmp.path().join(".fluent/work/items/work-existing.json")).unwrap();
    assert!(json.contains("Original title"));
    assert!(!json.contains("Replacement title"));
}

#[test]
fn work_create_rejects_invalid_work_item_id() {
    let tmp = TempDir::new().unwrap();

    fluent_cmd()
        .current_dir(tmp.path())
        .args([
            "work-item",
            "create",
            "../escape",
            "--title",
            "Invalid item",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "work item id \"../escape\" cannot be used as a file name",
        ));

    assert!(!tmp.path().join(".fluent/work/items").exists());
}

#[test]
fn work_create_item_is_visible_through_list_and_show() {
    let tmp = TempDir::new().unwrap();

    fluent_cmd()
        .current_dir(tmp.path())
        .args([
            "work-item",
            "create",
            "work-visible",
            "--title",
            "Visible title",
        ])
        .assert()
        .success();

    fluent_cmd()
        .current_dir(tmp.path())
        .args(["work-item", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("work-visible"))
        .stdout(predicate::str::contains("Visible title"));

    fluent_cmd()
        .current_dir(tmp.path())
        .args(["work-item", "show", "work-visible"])
        .assert()
        .success()
        .stdout(predicate::str::contains("  \"id\": \"work-visible\""))
        .stdout(predicate::str::contains("  \"title\": \"Visible title\""))
        .stdout(predicate::str::contains("  \"attempts\": []"));
}

// -------------------------------------------------------------------------
// Follow-up contracts: Work authorization and provenance
// -------------------------------------------------------------------------

fn write_raw_work_item(project_root: &Path, id: &str, value: &serde_json::Value) {
    let items_dir = project_root.join(".fluent/work/items");
    fs::create_dir_all(&items_dir).unwrap();
    write_json_path(&items_dir.join(format!("{id}.json")), value);
}

fn proposed_corrective_work_item(id: &str) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "title": "Restore the retry guard",
        "origin": {
            "kind": "derived",
            "observation_id": "obs-1",
            "work_item_id": "root-1"
        },
        "authorization": { "state": "proposed" },
        "lineage": { "root_id": "root-1" },
        "corrective_context": {
            "objective": "Restore the retry guard",
            "requirement": "Retries stop after the configured cap",
            "evidence": "Merged commit abc123 removed the cap check",
            "included_scope": "src/retry.rs",
            "excluded_scope": "unrelated backoff tuning",
            "verification": "cargo test retry::cap_is_enforced"
        }
    })
}

#[test]
fn proposed_work_rejects_attempt_create() {
    let tmp = TempDir::new().unwrap();
    write_raw_work_item(
        tmp.path(),
        "work-fix-1",
        &proposed_corrective_work_item("work-fix-1"),
    );

    fluent_cmd()
        .current_dir(tmp.path())
        .args(["attempt", "create", "work-fix-1", "attempt-1"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("human authorization is required"));

    let value = work_item_value(tmp.path(), "work-fix-1");
    assert!(value["attempts"].as_array().unwrap().is_empty());
}

#[test]
fn proposed_work_rejects_attempt_run() {
    let tmp = TempDir::new().unwrap();
    write_raw_work_item(
        tmp.path(),
        "work-fix-1",
        &proposed_corrective_work_item("work-fix-1"),
    );

    fluent_cmd()
        .current_dir(tmp.path())
        .args(["attempt", "run", "work-fix-1"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("human authorization is required"));
}

#[test]
fn derived_work_show_exposes_authorization_and_provenance() {
    let tmp = TempDir::new().unwrap();
    let value = serde_json::json!({
        "id": "work-fix-1",
        "title": "Restore the retry guard",
        "origin": {
            "kind": "derived",
            "observation_id": "obs-1",
            "work_item_id": "root-1",
            "attempt_id": "attempt-2",
            "merge_candidate_id": "candidate-1",
            "merged_commit": "abc123"
        },
        "authorization": { "state": "execution-ready", "authority": "automatic" },
        "lineage": { "root_id": "root-1", "charged": true, "descendant_limit": 10 }
    });
    write_raw_work_item(tmp.path(), "work-fix-1", &value);

    let shown = work_item_value(tmp.path(), "work-fix-1");
    assert_eq!(shown["authorization"]["state"], "execution-ready");
    assert_eq!(shown["authorization"]["authority"], "automatic");
    assert_eq!(shown["origin"]["kind"], "derived");
    assert_eq!(shown["origin"]["observation_id"], "obs-1");
    assert_eq!(shown["origin"]["work_item_id"], "root-1");
    assert_eq!(shown["origin"]["attempt_id"], "attempt-2");
    assert_eq!(shown["origin"]["merge_candidate_id"], "candidate-1");
    assert_eq!(shown["origin"]["merged_commit"], "abc123");
    assert_eq!(shown["lineage"]["root_id"], "root-1");
    assert_eq!(shown["lineage"]["charged"], true);
}

#[test]
fn planned_work_is_execution_ready_lineage_root() {
    let tmp = TempDir::new().unwrap();
    fluent_cmd()
        .current_dir(tmp.path())
        .args(["work-item", "create", "work-1", "--title", "Planned work"])
        .assert()
        .success();

    let shown = work_item_value(tmp.path(), "work-1");
    assert_eq!(shown["authorization"]["state"], "execution-ready");
    // A lineage root carries no stored lineage and no corrective context, and it
    // does not require an originating Observation.
    assert!(shown.get("lineage").is_none());
    assert!(shown.get("corrective_context").is_none());
    assert!(shown.get("origin").is_none());
}

#[test]
fn attempt_create_accepts_execution_ready_work() {
    let tmp = TempDir::new().unwrap();
    fluent_cmd()
        .current_dir(tmp.path())
        .args(["work-item", "create", "work-1", "--title", "Planned work"])
        .assert()
        .success();

    fluent_cmd()
        .current_dir(tmp.path())
        .args(["attempt", "create", "work-1", "attempt-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Created Attempt attempt-1 for Work Item work-1",
        ));

    let shown = work_item_value(tmp.path(), "work-1");
    let attempt = &shown["attempts"][0];
    assert_eq!(attempt["id"], "attempt-1");
    assert_eq!(attempt["status"], "planned");
    assert_eq!(attempt["tasks"][0]["id"], "attempt-1-write-1");
    assert_eq!(attempt["tasks"][0]["kind"], "write");
}

#[test]
fn work_attempt_adds_planned_attempt_with_initial_write_task() {
    let tmp = TempDir::new().unwrap();
    write_work_item_json(tmp.path(), "work-1", "Attempt intake");

    fluent_cmd()
        .current_dir(tmp.path())
        .args(["attempt", "create", "work-1", "attempt-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Created Attempt attempt-1 for Work Item work-1",
        ));

    let output = fluent_cmd()
        .current_dir(tmp.path())
        .args(["work-item", "show", "work-1"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "work-item show failed: stdout={} stderr={}",
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

    fluent_cmd()
        .current_dir(tmp.path())
        .args(["attempt", "create", "work-a", "b-c"])
        .assert()
        .success();
    fluent_cmd()
        .current_dir(tmp.path())
        .args(["attempt", "create", "work-a-b", "c"])
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

    fluent_cmd()
        .current_dir(tmp.path())
        .args(["attempt", "create", "work-1", "attempt-1"])
        .assert()
        .success();

    fluent_cmd()
        .current_dir(tmp.path())
        .args(["attempt", "create", "work-1", "attempt-2"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Created Attempt attempt-2 for Work Item work-1",
        ));

    let output = fluent_cmd()
        .current_dir(tmp.path())
        .args(["work-item", "show", "work-1"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "work-item show failed: stdout={} stderr={}",
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

    fluent_cmd()
        .current_dir(tmp.path())
        .args(["attempt", "create", "missing-work", "attempt-1"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "Work Item \"missing-work\" not found",
        ));

    assert!(!tmp.path().join(".fluent/work/items").exists());
}

#[test]
fn work_attempt_duplicate_attempt_id_fails_without_changes() {
    let tmp = TempDir::new().unwrap();
    write_work_item_json(tmp.path(), "work-1", "Attempt intake");

    fluent_cmd()
        .current_dir(tmp.path())
        .args(["attempt", "create", "work-1", "attempt-1"])
        .assert()
        .success();
    let before = fs::read_to_string(tmp.path().join(".fluent/work/items/work-1.json")).unwrap();

    fluent_cmd()
        .current_dir(tmp.path())
        .args(["attempt", "create", "work-1", "attempt-1"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "Attempt \"attempt-1\" already exists",
        ));

    let after = fs::read_to_string(tmp.path().join(".fluent/work/items/work-1.json")).unwrap();
    assert_eq!(after, before);
}

#[test]
fn work_attempt_rejects_invalid_attempt_id_without_changes() {
    let tmp = TempDir::new().unwrap();
    write_work_item_json(tmp.path(), "work-1", "Attempt intake");
    let before = fs::read_to_string(tmp.path().join(".fluent/work/items/work-1.json")).unwrap();

    fluent_cmd()
        .current_dir(tmp.path())
        .args(["attempt", "create", "work-1", "../escape"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "attempt id \"../escape\" cannot be used as a file name",
        ));

    let after = fs::read_to_string(tmp.path().join(".fluent/work/items/work-1.json")).unwrap();
    assert_eq!(after, before);
}

#[test]
fn work_attempt_auto_id_creates_attempt_1() {
    let tmp = TempDir::new().unwrap();
    write_work_item_json(tmp.path(), "work-1", "Auto attempt");

    fluent_cmd()
        .current_dir(tmp.path())
        .args(["attempt", "create", "work-1"])
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

    fluent_cmd()
        .current_dir(tmp.path())
        .args(["attempt", "create", "work-1"])
        .assert()
        .success();

    fluent_cmd()
        .current_dir(tmp.path())
        .args(["attempt", "create", "work-1"])
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

    fluent_cmd()
        .current_dir(tmp.path())
        .args(["attempt", "create", "work-1", "attempt-1"])
        .assert()
        .success();

    fluent_cmd()
        .current_dir(tmp.path())
        .args(["attempt", "create", "work-1", "attempt-3"])
        .assert()
        .success();

    fluent_cmd()
        .current_dir(tmp.path())
        .args(["attempt", "create", "work-1"])
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

    fluent_cmd()
        .current_dir(tmp.path())
        .args(["attempt", "create", "work-1", "my-custom-attempt"])
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

    fluent_cmd()
        .current_dir(tmp.path())
        .args(["attempt", "run", "work-1", "--no-sandbox"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("has no Attempts"));
}

#[test]
fn work_merge_no_candidates_reports_error() {
    let tmp = TempDir::new().unwrap();
    write_work_item_json(tmp.path(), "work-1", "No candidates");

    fluent_cmd()
        .current_dir(tmp.path())
        .args(["merge-candidate", "land", "work-1", "--no-sandbox"])
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

    fluent_cmd()
        .current_dir(&main_dir)
        .args(["work-item", "create", "work-1", "--title", "Run task"])
        .assert()
        .success();
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "create", "work-1", "attempt-1"])
        .assert()
        .success();

    let output = fluent_cmd()
        .current_dir(&main_dir)
        .args([
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

    fluent_cmd()
        .current_dir(&main_dir)
        .args([
            "work-item",
            "create",
            "work-1",
            "--title",
            "Transcript test",
        ])
        .assert()
        .success();
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "create", "work-1", "attempt-1"])
        .assert()
        .success();
    fluent_cmd()
        .current_dir(&main_dir)
        .args([
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

    let transcript =
        main_dir.join(".fluent/work/artifacts/work-1/attempt-1/attempt-1-write-1/transcript.jsonl");
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
        ".fluent/work/artifacts/work-1/attempt-1/attempt-1-write-1"
    );
}

#[test]
fn seed_failure_does_not_abort_attempt() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    let bin_dir = tmp.path().join("bin-seed-fail");
    let counter_path = tmp.path().join("call-counter");

    write_mock_claude(
        &bin_dir,
        &format!(
            r##"#!/bin/bash
COUNTER="{}"
if [ ! -f "$COUNTER" ]; then
    echo 1 > "$COUNTER"
    exit 1
fi
printf 'task output\n' > task-output.txt
git add task-output.txt
git commit -m "Add task output" >/dev/null
exit 0
"##,
            counter_path.display()
        ),
    );

    fluent_cmd()
        .current_dir(&main_dir)
        .args(["work-item", "create", "work-1", "--title", "Seed fail test"])
        .assert()
        .success();
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "create", "work-1", "attempt-1"])
        .assert()
        .success();

    let output = fluent_cmd()
        .current_dir(&main_dir)
        .args([
            "task",
            "run",
            "work-1",
            "attempt-1",
            "attempt-1-write-1",
            "--no-sandbox",
        ])
        .env("PATH", mock_path(&bin_dir))
        .env("FLUENT_MAX_TASK_RETRIES", "0")
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("seed project model failed"),
        "stderr should warn about seed failure; got:\n{stderr}"
    );
    assert!(
        output.status.success(),
        "write task should succeed despite seed failure: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        stderr
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("Completed Task attempt-1-write-1"));
}

/// A mock coder that drives write and review rounds like `loop_mock_script`, but
/// makes the Learner invocation fail by exiting non-zero.
fn learner_failing_mock_script() -> String {
    r##"#!/bin/bash
PROMPT=""
NEXT_IS_PROMPT=0
for arg in "$@"; do
  if [ "$NEXT_IS_PROMPT" = 1 ]; then PROMPT="$arg"; break; fi
  if [ "$arg" = "-p" ]; then NEXT_IS_PROMPT=1; fi
done
if [ -z "$PROMPT" ]; then exit 0; fi
if printf '%s' "$PROMPT" | grep -q "You are the Learner"; then
  exit 1
fi
case "$PWD" in
  */work-6-work-1-attempt-1)
    printf 'loop output\n' > loop-output.txt
    git add loop-output.txt
    git commit -m "Add loop output" >/dev/null 2>&1
    ;;
  *)
    printf 'Verdict: pass\n\nLoop review.\n' > review.md
    ;;
esac
exit 0
"##
    .to_string()
}

#[test]
fn learner_failure_is_retryable_and_does_not_block_candidate() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    let bin_dir = tmp.path().join("bin-learner-fail");
    write_mock_claude(&bin_dir, &learner_failing_mock_script());

    fluent_cmd()
        .current_dir(&main_dir)
        .args(["work-item", "create", "work-1", "--title", "Learner"])
        .assert()
        .success();
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "create", "work-1", "attempt-1"])
        .assert()
        .success();

    let output = fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "run", "work-1", "attempt-1", "--no-sandbox"])
        .env("PATH", mock_path(&bin_dir))
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "run should succeed despite learner failure: stdout={stdout} stderr={stderr}"
    );
    assert!(
        stdout.contains("Merge Candidate attempt-1-merge-candidate is ready"),
        "candidate is produced despite the learner failure; got:\n{stdout}"
    );
    assert!(
        stderr.contains("learner failed"),
        "operator is warned about the learner failure; got:\n{stderr}"
    );

    let value = read_work_show_json(&main_dir, "work-1");
    assert_eq!(value["attempts"][0]["learning"]["status"], "failed");
    assert_eq!(value["merge_candidates"].as_array().unwrap().len(), 1);
    assert!(
        !main_dir
            .join(".fluent/work/artifacts/work-1/attempt-1/learner/handoff.json")
            .exists(),
        "a failed learner run persists no handoff"
    );
}

#[test]
fn learner_retry_completes_existing_record_idempotently() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    let bin_dir = tmp.path().join("bin-learner-retry");
    write_mock_claude(&bin_dir, &learner_failing_mock_script());

    fluent_cmd()
        .current_dir(&main_dir)
        .args(["work-item", "create", "work-1", "--title", "Learner"])
        .assert()
        .success();
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "create", "work-1", "attempt-1"])
        .assert()
        .success();
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "run", "work-1", "attempt-1", "--no-sandbox"])
        .env("PATH", mock_path(&bin_dir))
        .assert()
        .success();

    let failed = read_work_show_json(&main_dir, "work-1");
    assert_eq!(failed["attempts"][0]["learning"]["status"], "failed");

    // A successful retry completes the same record with one handoff.
    write_mock_claude(
        &bin_dir,
        &learner_mock_script(
            r#"{"learning_summary":"learned","follow_ups":[{"id":"fu-1","summary":"Follow up","corrective":false}]}"#,
        ),
    );
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "run", "work-1", "attempt-1", "--no-sandbox"])
        .env("PATH", mock_path(&bin_dir))
        .assert()
        .success();

    let value = read_work_show_json(&main_dir, "work-1");
    assert_eq!(value["attempts"][0]["learning"]["status"], "succeeded");
    assert_eq!(value["attempts"][0]["learning"]["runs"], 2);

    let handoff = read_json_path(
        &main_dir.join(".fluent/work/artifacts/work-1/attempt-1/learner/handoff.json"),
    );
    let follow_ups = handoff["follow_ups"].as_array().unwrap();
    assert_eq!(
        follow_ups.len(),
        1,
        "one accepted follow-up, not duplicated"
    );
    assert_eq!(follow_ups[0]["id"], "fu-1");
}

#[test]
fn attempt_run_retries_only_failed_learner() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    let bin_dir = tmp.path().join("bin-learner-only");
    write_mock_claude(&bin_dir, &learner_failing_mock_script());

    fluent_cmd()
        .current_dir(&main_dir)
        .args(["work-item", "create", "work-1", "--title", "Learner"])
        .assert()
        .success();
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "create", "work-1", "attempt-1"])
        .assert()
        .success();
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "run", "work-1", "attempt-1", "--no-sandbox"])
        .env("PATH", mock_path(&bin_dir))
        .assert()
        .success();

    let before = read_work_show_json(&main_dir, "work-1");
    let tasks_before = before["attempts"][0]["tasks"].as_array().unwrap().len();
    let commit_before = before["merge_candidates"][0]["candidate_commit"].clone();

    write_mock_claude(
        &bin_dir,
        &learner_mock_script(r#"{"learning_summary":"x","follow_ups":[]}"#),
    );
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "run", "work-1", "attempt-1", "--no-sandbox"])
        .env("PATH", mock_path(&bin_dir))
        .assert()
        .success();

    let after = read_work_show_json(&main_dir, "work-1");
    assert_eq!(
        after["attempts"][0]["tasks"].as_array().unwrap().len(),
        tasks_before,
        "retry adds no Writer, Tester, or reviewer Tasks"
    );
    assert_eq!(
        after["merge_candidates"][0]["candidate_commit"], commit_before,
        "retry does not rerun the Writer"
    );
    assert_eq!(after["attempts"][0]["learning"]["status"], "succeeded");
}

/// A mock coder that drives write and review rounds like `loop_mock_script`, and
/// additionally writes a Learner follow-up draft (verbatim `draft_json`) to the
/// path named in the Learner prompt.
fn learner_mock_script(draft_json: &str) -> String {
    format!(
        r##"#!/bin/bash
PROMPT=""
NEXT_IS_PROMPT=0
for arg in "$@"; do
  if [ "$NEXT_IS_PROMPT" = 1 ]; then PROMPT="$arg"; break; fi
  if [ "$arg" = "-p" ]; then NEXT_IS_PROMPT=1; fi
done
if [ -z "$PROMPT" ]; then exit 0; fi
if printf '%s' "$PROMPT" | grep -q "You are the Learner"; then
  DRAFT=$(printf '%s' "$PROMPT" | grep -o '/[^ ]*follow-up-draft.json' | head -1)
  if [ -n "$DRAFT" ]; then
    mkdir -p "$(dirname "$DRAFT")"
    cat > "$DRAFT" <<'DRAFTJSON'
{draft_json}
DRAFTJSON
  fi
  exit 0
fi
case "$PWD" in
  */work-6-work-1-attempt-1)
    printf 'loop output\n' > loop-output.txt
    git add loop-output.txt
    git commit -m "Add loop output" >/dev/null 2>&1
    ;;
  *)
    printf 'Verdict: pass\n\nLoop review.\n' > review.md
    ;;
esac
exit 0
"##
    )
}

fn create_and_run_learner_attempt(main_dir: &Path, bin_dir: &Path) {
    fluent_cmd()
        .current_dir(main_dir)
        .args(["work-item", "create", "work-1", "--title", "Learner"])
        .assert()
        .success();
    fluent_cmd()
        .current_dir(main_dir)
        .args(["attempt", "create", "work-1", "attempt-1"])
        .assert()
        .success();
    fluent_cmd()
        .current_dir(main_dir)
        .args(["attempt", "run", "work-1", "attempt-1", "--no-sandbox"])
        .env("PATH", mock_path(bin_dir))
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Merge Candidate attempt-1-merge-candidate is ready",
        ));
}

#[test]
fn learner_persists_empty_handoff() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    let bin_dir = tmp.path().join("bin-learner-empty");
    write_mock_claude(
        &bin_dir,
        &learner_mock_script(r#"{"learning_summary":"nothing durable","follow_ups":[]}"#),
    );

    create_and_run_learner_attempt(&main_dir, &bin_dir);

    let handoff_path =
        main_dir.join(".fluent/work/artifacts/work-1/attempt-1/learner/handoff.json");
    assert!(handoff_path.exists(), "one handoff must be persisted");
    let handoff = read_json_path(&handoff_path);
    assert_eq!(handoff["source_work_item_id"], "work-1");
    assert_eq!(handoff["source_attempt_id"], "attempt-1");
    assert_eq!(
        handoff["source_merge_candidate_id"],
        "attempt-1-merge-candidate"
    );
    assert!(
        handoff["follow_ups"]
            .as_array()
            .map(|a| a.is_empty())
            .unwrap_or(true),
        "an empty handoff carries no follow-ups"
    );

    let value = read_work_show_json(&main_dir, "work-1");
    assert_eq!(value["attempts"][0]["learning"]["status"], "succeeded");
    assert_eq!(value["merge_candidates"].as_array().unwrap().len(), 1);
}

#[test]
fn learner_persists_followup_handoff_with_origin_provenance() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    let bin_dir = tmp.path().join("bin-learner-followup");
    write_mock_claude(
        &bin_dir,
        &learner_mock_script(
            r#"{"learning_summary":"learned","follow_ups":[{"id":"fu-1","summary":"Consolidate retry handling","corrective":false}]}"#,
        ),
    );

    create_and_run_learner_attempt(&main_dir, &bin_dir);

    let handoff_path =
        main_dir.join(".fluent/work/artifacts/work-1/attempt-1/learner/handoff.json");
    let handoff = read_json_path(&handoff_path);
    let follow_ups = handoff["follow_ups"].as_array().unwrap();
    assert_eq!(follow_ups.len(), 1);
    assert_eq!(follow_ups[0]["id"], "fu-1");
    // Origin provenance is stamped by the host, not the untrusted draft.
    assert_eq!(handoff["source_work_item_id"], "work-1");
    assert_eq!(handoff["source_attempt_id"], "attempt-1");
    assert_eq!(
        handoff["source_merge_candidate_id"],
        "attempt-1-merge-candidate"
    );
}

#[test]
fn learner_handoff_does_not_materialize_before_land() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    let bin_dir = tmp.path().join("bin-learner-inert");
    write_mock_claude(
        &bin_dir,
        &learner_mock_script(
            r#"{"learning_summary":"learned","follow_ups":[{"id":"fu-1","summary":"Follow up later","corrective":false}]}"#,
        ),
    );

    create_and_run_learner_attempt(&main_dir, &bin_dir);

    // The handoff exists but is inert: no Observation, no derived Work Item.
    let handoff_path =
        main_dir.join(".fluent/work/artifacts/work-1/attempt-1/learner/handoff.json");
    assert!(handoff_path.exists());

    let items_dir = main_dir.join(".fluent/work/items");
    let item_count = fs::read_dir(&items_dir)
        .map(|entries| {
            entries
                .filter_map(|entry| entry.ok())
                .filter(|entry| {
                    entry
                        .path()
                        .extension()
                        .map(|e| e == "json")
                        .unwrap_or(false)
                })
                .count()
        })
        .unwrap_or(0);
    assert_eq!(item_count, 1, "the learner must not derive a Work Item");

    let observations_dir = main_dir.join(".fluent/observations");
    let observation_count = fs::read_dir(&observations_dir)
        .map(|entries| {
            entries
                .filter_map(|entry| entry.ok())
                .filter(|entry| entry.path().extension().map(|e| e == "md").unwrap_or(false))
                .count()
        })
        .unwrap_or(0);
    assert_eq!(
        observation_count, 0,
        "the learner must not create an Observation before land"
    );
}

// --- Land-gated learner handoff materialization (Step 1) ---

/// A mock coder that drives write and review rounds, writes a Learner follow-up
/// draft (verbatim `draft_json`), and performs a real `git rebase` when handed
/// the rebase prompt during a land.
fn learner_land_mock_script(draft_json: &str) -> String {
    format!(
        r##"#!/bin/bash
PROMPT=""
NEXT_IS_PROMPT=0
for arg in "$@"; do
  if [ "$NEXT_IS_PROMPT" = 1 ]; then PROMPT="$arg"; break; fi
  if [ "$arg" = "-p" ]; then NEXT_IS_PROMPT=1; fi
done
if [ -z "$PROMPT" ]; then exit 0; fi
if printf '%s' "$PROMPT" | grep -q "Rebase the candidate branch"; then
  TARGET=$(printf '%s' "$PROMPT" | grep -o 'onto `[^`]*`' | sed 's/onto `//;s/`//')
  git rebase "$TARGET" 2>/dev/null
  exit $?
fi
if printf '%s' "$PROMPT" | grep -q "You are the Learner"; then
  DRAFT=$(printf '%s' "$PROMPT" | grep -o '/[^ ]*follow-up-draft.json' | head -1)
  if [ -n "$DRAFT" ]; then
    mkdir -p "$(dirname "$DRAFT")"
    cat > "$DRAFT" <<'DRAFTJSON'
{draft_json}
DRAFTJSON
  fi
  exit 0
fi
case "$PWD" in
  */work-6-work-1-attempt-1)
    printf 'loop output\n' > loop-output.txt
    git add loop-output.txt
    git commit -m "Add loop output" >/dev/null 2>&1
    ;;
  *)
    printf 'Verdict: pass\n\nLoop review.\n' > review.md
    ;;
esac
exit 0
"##
    )
}

/// Land the sole Merge Candidate of `work-1`, driving the rebase with `bin_dir`.
fn land_work_1(main_dir: &Path, bin_dir: &Path, no_post_merge_review: bool) {
    let mut args = vec![
        "merge-candidate",
        "land",
        "work-1",
        "attempt-1-merge-candidate",
        "--no-sandbox",
    ];
    if no_post_merge_review {
        args.push("--no-post-merge-review");
    }
    fluent_cmd()
        .current_dir(main_dir)
        .args(&args)
        .env("PATH", mock_path(bin_dir))
        .assert()
        .success();
}

/// A learner mock that fails its first run and succeeds on retry, keyed by a
/// counter file so a post-land retry (which runs after land) drives the
/// handoff-only path. On the successful run it writes `draft_json` and, when
/// `commit_expertise` is set, commits an expertise change that a handoff-only
/// run must discard.
fn post_land_learner_mock_script(counter: &Path, draft_json: &str, commit_expertise: bool) -> String {
    let counter = counter.display().to_string();
    let prompt_log = format!("{counter}.prompt");
    let expertise = if commit_expertise {
        "  mkdir -p .fluent/expertise\n  printf 'late knowledge\\n' > .fluent/expertise/late.md\n  git add .fluent/expertise/late.md\n  git commit -m \"Update expertise\" >/dev/null 2>&1\n"
    } else {
        ""
    };
    format!(
        r##"#!/bin/bash
PROMPT=""
NEXT_IS_PROMPT=0
for arg in "$@"; do
  if [ "$NEXT_IS_PROMPT" = 1 ]; then PROMPT="$arg"; break; fi
  if [ "$arg" = "-p" ]; then NEXT_IS_PROMPT=1; fi
done
if [ -z "$PROMPT" ]; then exit 0; fi
if printf '%s' "$PROMPT" | grep -q "Rebase the candidate branch"; then
  TARGET=$(printf '%s' "$PROMPT" | grep -o 'onto `[^`]*`' | sed 's/onto `//;s/`//')
  git rebase "$TARGET" 2>/dev/null
  exit $?
fi
if printf '%s' "$PROMPT" | grep -q "You are the Learner"; then
  printf 'run\n' >> "{counter}.invocations"
  printf '%s' "$PROMPT" > "{prompt_log}"
  if [ ! -f "{counter}" ]; then
    touch "{counter}"
    exit 1
  fi
  git rev-parse HEAD > "{counter}.head"
  DRAFT=$(printf '%s' "$PROMPT" | grep -o '/[^ ]*follow-up-draft.json' | head -1)
  if [ -n "$DRAFT" ]; then
    mkdir -p "$(dirname "$DRAFT")"
    cat > "$DRAFT" <<'DRAFTJSON'
{draft_json}
DRAFTJSON
  fi
{expertise}  exit 0
fi
case "$PWD" in
  */work-6-work-1-attempt-1)
    printf 'loop output\n' > loop-output.txt
    git add loop-output.txt
    git commit -m "Add loop output" >/dev/null 2>&1
    ;;
  *)
    printf 'Verdict: pass\n\nLoop review.\n' > review.md
    ;;
esac
exit 0
"##
    )
}

/// A learner mock whose retry dirties the candidate, announces that state, and
/// waits for the test to release it before committing. This makes a land/retry
/// overlap deterministic instead of relying on scheduler timing.
fn contended_learner_mock_script(
    counter: &Path,
    retry_started: &Path,
    retry_release: &Path,
) -> String {
    format!(
        r##"#!/bin/bash
PROMPT=""
NEXT_IS_PROMPT=0
for arg in "$@"; do
  if [ "$NEXT_IS_PROMPT" = 1 ]; then PROMPT="$arg"; break; fi
  if [ "$arg" = "-p" ]; then NEXT_IS_PROMPT=1; fi
done
if [ -z "$PROMPT" ]; then exit 0; fi
if printf '%s' "$PROMPT" | grep -q "Rebase the candidate branch"; then
  TARGET=$(printf '%s' "$PROMPT" | grep -o 'onto `[^`]*`' | sed 's/onto `//;s/`//')
  git rebase "$TARGET" 2>/dev/null
  exit $?
fi
if printf '%s' "$PROMPT" | grep -q "You are the Learner"; then
  if [ ! -f "{}" ]; then
    touch "{}"
    exit 1
  fi
  printf 'transient learner write\n' > transient-learner.txt
  git add transient-learner.txt
  touch "{}"
  while [ ! -f "{}" ]; do sleep 0.02; done
  git commit -m "Update expertise" >/dev/null 2>&1
  DRAFT=$(printf '%s' "$PROMPT" | grep -o '/[^ ]*follow-up-draft.json' | head -1)
  mkdir -p "$(dirname "$DRAFT")"
  printf '%s\n' '{{"learning_summary":"won before land","follow_ups":[]}}' > "$DRAFT"
  exit 0
fi
case "$PWD" in
  */work-6-work-1-attempt-1)
    printf 'loop output\n' > loop-output.txt
    git add loop-output.txt
    git commit -m "Add loop output" >/dev/null 2>&1
    ;;
  *)
    printf 'Verdict: pass\n\nLoop review.\n' > review.md
    ;;
esac
exit 0
"##,
        counter.display(),
        counter.display(),
        retry_started.display(),
        retry_release.display(),
    )
}

/// A retrying Learner that attempts to mutate target Git state, protected refs,
/// and the candidate index before returning a handoff.
fn hostile_post_land_learner_mock_script(counter: &Path) -> String {
    format!(
        r##"#!/bin/bash
PROMPT=""
NEXT_IS_PROMPT=0
for arg in "$@"; do
  if [ "$NEXT_IS_PROMPT" = 1 ]; then PROMPT="$arg"; break; fi
  if [ "$arg" = "-p" ]; then NEXT_IS_PROMPT=1; fi
done
if [ -z "$PROMPT" ]; then exit 0; fi
if printf '%s' "$PROMPT" | grep -q "Rebase the candidate branch"; then
  TARGET=$(printf '%s' "$PROMPT" | grep -o 'onto `[^`]*`' | sed 's/onto `//;s/`//')
  git rebase "$TARGET" 2>/dev/null
  exit $?
fi
if printf '%s' "$PROMPT" | grep -q "You are the Learner"; then
  if [ ! -f "{}" ]; then touch "{}"; exit 1; fi
  DRAFT=$(printf '%s' "$PROMPT" | grep -o '/[^ ]*follow-up-draft.json' | head -1)
  PROJECT=${{DRAFT%%/.fluent/*}}
  printf 'hostile target write\n' > "$PROJECT/target-mutated.txt"
  git -C "$PROJECT" add target-mutated.txt
  git -C "$PROJECT" commit -m "Move target" >/dev/null 2>&1
  git update-ref refs/heads/unauthorized HEAD
  printf 'hostile candidate index\n' > candidate-index.txt
  git add candidate-index.txt
  mkdir -p "$(dirname "$DRAFT")"
  printf '%s\n' '{{"learning_summary":"hostile","follow_ups":[]}}' > "$DRAFT"
  exit 0
fi
case "$PWD" in
  */work-6-work-1-attempt-1)
    printf 'loop output\n' > loop-output.txt
    git add loop-output.txt
    git commit -m "Add loop output" >/dev/null 2>&1
    ;;
  *) printf 'Verdict: pass\n\nLoop review.\n' > review.md ;;
esac
exit 0
"##,
        counter.display(),
        counter.display(),
    )
}

fn failing_dirty_post_land_learner_mock_script(counter: &Path) -> String {
    format!(
        r##"#!/bin/bash
PROMPT=""
NEXT_IS_PROMPT=0
for arg in "$@"; do
  if [ "$NEXT_IS_PROMPT" = 1 ]; then PROMPT="$arg"; break; fi
  if [ "$arg" = "-p" ]; then NEXT_IS_PROMPT=1; fi
done
if [ -z "$PROMPT" ]; then exit 0; fi
if printf '%s' "$PROMPT" | grep -q "Rebase the candidate branch"; then
  TARGET=$(printf '%s' "$PROMPT" | grep -o 'onto `[^`]*`' | sed 's/onto `//;s/`//')
  git rebase "$TARGET" 2>/dev/null
  exit $?
fi
if printf '%s' "$PROMPT" | grep -q "You are the Learner"; then
  if [ ! -f "{}" ]; then touch "{}"; exit 1; fi
  printf 'dirty failed retry\n' > failed-retry.txt
  git add failed-retry.txt
  INDEX_PATH=$(git rev-parse --path-format=absolute --git-path index)
  HEAD_PATH=$(git rev-parse --path-format=absolute --git-path HEAD)
  printf 'corrupt index\n' > "$INDEX_PATH"
  printf 'ref: refs/heads/malformed-candidate\n' > "$HEAD_PATH"
  exit 1
fi
case "$PWD" in
  */work-6-work-1-attempt-1)
    printf 'loop output\n' > loop-output.txt
    git add loop-output.txt
    git commit -m "Add loop output" >/dev/null 2>&1
    ;;
  *) printf 'Verdict: pass\n\nLoop review.\n' > review.md ;;
esac
exit 0
"##,
        counter.display(),
        counter.display(),
    )
}

/// A mock that fails any rebase request, so a code path that must not rebase is
/// proven by its success.
fn rebase_failing_mock_script() -> String {
    r##"#!/bin/bash
PROMPT=""
NEXT_IS_PROMPT=0
for arg in "$@"; do
  if [ "$NEXT_IS_PROMPT" = 1 ]; then PROMPT="$arg"; break; fi
  if [ "$arg" = "-p" ]; then NEXT_IS_PROMPT=1; fi
done
if printf '%s' "$PROMPT" | grep -q "Rebase the candidate branch"; then exit 1; fi
exit 0
"##
    .to_string()
}

/// Re-run the completed Attempt to retry a failed Learner.
fn rerun_learner_attempt(main_dir: &Path, bin_dir: &Path) {
    fluent_cmd()
        .current_dir(main_dir)
        .args(["attempt", "run", "work-1", "attempt-1", "--no-sandbox"])
        .env("PATH", mock_path(bin_dir))
        .assert()
        .success();
}

fn rerun_learner_attempt_sandboxed(main_dir: &Path, bin_dir: &Path) {
    fluent_cmd()
        .current_dir(main_dir)
        .args(["attempt", "run", "work-1", "attempt-1"])
        .env("PATH", mock_path(bin_dir))
        .env("CLAUDE_CODE_OAUTH_TOKEN", "mock-token")
        .assert()
        .success();
}

fn merged_commit_of(main_dir: &Path) -> String {
    work_item_value(main_dir, "work-1")["merge_candidates"][0]["merge_state"]["merged_commit"]
        .as_str()
        .unwrap()
        .to_string()
}

/// The open Observation files in a project, sorted by id.
fn open_observation_files(main_dir: &Path) -> Vec<String> {
    let dir = main_dir.join(".fluent/observations");
    let mut files: Vec<String> = fs::read_dir(&dir)
        .map(|entries| {
            entries
                .filter_map(|e| e.ok())
                .filter(|e| e.path().extension().map(|x| x == "md").unwrap_or(false))
                .map(|e| e.file_name().to_string_lossy().to_string())
                .collect()
        })
        .unwrap_or_default();
    files.sort();
    files
}

fn work_item_json_count(main_dir: &Path) -> usize {
    let items_dir = main_dir.join(".fluent/work/items");
    fs::read_dir(&items_dir)
        .map(|entries| {
            entries
                .filter_map(|e| e.ok())
                .filter(|e| e.path().extension().map(|x| x == "json").unwrap_or(false))
                .count()
        })
        .unwrap_or(0)
}

#[test]
fn ready_candidate_does_not_materialize_learner_handoff() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    let bin_dir = tmp.path().join("bin-ready");
    write_mock_claude(
        &bin_dir,
        &learner_land_mock_script(
            r#"{"learning_summary":"learned","follow_ups":[{"id":"fu-1","summary":"Later","corrective":false}]}"#,
        ),
    );

    create_and_run_learner_attempt(&main_dir, &bin_dir);

    // The candidate is ready but has not landed: nothing is materialized.
    assert!(
        open_observation_files(&main_dir).is_empty(),
        "a ready, unlanded candidate materializes no Observation"
    );
    assert_eq!(
        work_item_json_count(&main_dir),
        1,
        "a ready, unlanded candidate derives no Work Item"
    );
}

#[test]
fn abandoned_candidate_never_materializes_learner_handoff() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    let bin_dir = tmp.path().join("bin-abandoned");
    write_mock_claude(
        &bin_dir,
        &learner_land_mock_script(
            r#"{"learning_summary":"learned","follow_ups":[{"id":"fu-1","summary":"Later","corrective":false}]}"#,
        ),
    );

    create_and_run_learner_attempt(&main_dir, &bin_dir);

    fluent_cmd()
        .current_dir(&main_dir)
        .args(["work-item", "abandon", "work-1", "--reason", "superseded"])
        .assert()
        .success();

    // An abandoned Work Item's candidate can never land, so its handoff never
    // materializes.
    fluent_cmd()
        .current_dir(&main_dir)
        .args([
            "merge-candidate",
            "land",
            "work-1",
            "attempt-1-merge-candidate",
            "--no-sandbox",
        ])
        .env("PATH", mock_path(&bin_dir))
        .assert()
        .failure();

    assert!(
        open_observation_files(&main_dir).is_empty(),
        "an abandoned candidate materializes no Observation"
    );
}

#[test]
fn land_processes_handoff_after_recording_merge() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    let bin_dir = tmp.path().join("bin-land-process");
    write_mock_claude(
        &bin_dir,
        &learner_land_mock_script(
            r#"{"learning_summary":"learned","follow_ups":[{"id":"fu-1","summary":"Consolidate retry handling","corrective":false}]}"#,
        ),
    );

    create_and_run_learner_attempt(&main_dir, &bin_dir);
    land_work_1(&main_dir, &bin_dir, false);

    let value = read_work_show_json(&main_dir, "work-1");
    assert_eq!(value["merge_candidates"][0]["merge_state"]["status"], "merged");

    let observations = open_observation_files(&main_dir);
    assert_eq!(
        observations.len(),
        1,
        "landing a handoff with one follow-up materializes one Observation; got {observations:?}"
    );

    let operation_dir = main_dir.join(format!(".fluent/work/follow-ups/{OPERATION_ID}"));
    let journal = read_json_path(&operation_dir.join("journal.json"));
    assert_eq!(journal["completed"], true, "the journal is marked complete");
    assert_eq!(journal["follow_ups"].as_array().unwrap().len(), 1);
}

#[test]
fn no_post_merge_review_does_not_skip_learner_handoff() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    let bin_dir = tmp.path().join("bin-land-quiet");
    write_mock_claude(
        &bin_dir,
        &learner_land_mock_script(
            r#"{"learning_summary":"learned","follow_ups":[{"id":"fu-1","summary":"Follow up","corrective":false}]}"#,
        ),
    );

    create_and_run_learner_attempt(&main_dir, &bin_dir);
    land_work_1(&main_dir, &bin_dir, true);

    assert_eq!(
        open_observation_files(&main_dir).len(),
        1,
        "--no-post-merge-review still processes the landed learner handoff"
    );
}

#[test]
fn land_creates_provenance_linked_observation() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    let bin_dir = tmp.path().join("bin-provenance");
    write_mock_claude(
        &bin_dir,
        &learner_land_mock_script(
            r#"{"learning_summary":"learned","follow_ups":[{"id":"fu-1","summary":"Restore the retry cap","corrective":false}]}"#,
        ),
    );

    create_and_run_learner_attempt(&main_dir, &bin_dir);
    land_work_1(&main_dir, &bin_dir, true);

    let value = read_work_show_json(&main_dir, "work-1");
    let merged_commit = value["merge_candidates"][0]["merge_state"]["merged_commit"]
        .as_str()
        .unwrap()
        .to_string();

    let observations = open_observation_files(&main_dir);
    assert_eq!(observations.len(), 1);
    let content =
        fs::read_to_string(main_dir.join(".fluent/observations").join(&observations[0])).unwrap();

    // The Observation identifies the follow-up and its full origin.
    assert!(content.contains("follow-up-id: fu-1"), "content:\n{content}");
    assert!(content.contains("work-item-id: work-1"));
    assert!(content.contains("attempt-id: attempt-1"));
    assert!(content.contains("merge-candidate-id: attempt-1-merge-candidate"));
    assert!(
        content.contains(&format!("merged-commit: {merged_commit}")),
        "Observation must name the merged commit {merged_commit}; content:\n{content}"
    );
}

#[test]
fn observation_show_exposes_learning_provenance() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    let bin_dir = tmp.path().join("bin-show-prov");
    write_mock_claude(
        &bin_dir,
        &learner_land_mock_script(
            r#"{"learning_summary":"learned","follow_ups":[{"id":"fu-1","summary":"Restore the retry cap","corrective":false}]}"#,
        ),
    );

    create_and_run_learner_attempt(&main_dir, &bin_dir);
    land_work_1(&main_dir, &bin_dir, true);

    let output = fluent_cmd()
        .current_dir(&main_dir)
        .args(["observation", "show", "followup-work-1"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("work-item-id: work-1"), "show output:\n{stdout}");
    assert!(stdout.contains("merge-candidate-id: attempt-1-merge-candidate"));
    assert!(stdout.contains("Restore the retry cap"));
}

#[test]
fn land_consumes_empty_handoff_without_placeholders() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    let bin_dir = tmp.path().join("bin-empty-land");
    write_mock_claude(
        &bin_dir,
        &learner_land_mock_script(r#"{"learning_summary":"nothing durable","follow_ups":[]}"#),
    );

    create_and_run_learner_attempt(&main_dir, &bin_dir);
    land_work_1(&main_dir, &bin_dir, true);

    assert!(
        open_observation_files(&main_dir).is_empty(),
        "an empty handoff creates no placeholder Observation"
    );
    assert_eq!(
        work_item_json_count(&main_dir),
        1,
        "an empty handoff derives no Work Item"
    );

    let journal = read_json_path(
        &main_dir.join(format!(".fluent/work/follow-ups/{OPERATION_ID}/journal.json")),
    );
    assert_eq!(journal["completed"], true, "an empty handoff records as processed");
}

#[test]
fn learner_followup_processing_is_idempotent() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    let bin_dir = tmp.path().join("bin-idempotent");
    write_mock_claude(
        &bin_dir,
        &learner_land_mock_script(
            r#"{"learning_summary":"learned","follow_ups":[{"id":"fu-1","summary":"Consolidate retry handling","corrective":false}]}"#,
        ),
    );

    create_and_run_learner_attempt(&main_dir, &bin_dir);
    land_work_1(&main_dir, &bin_dir, true);

    let first = open_observation_files(&main_dir);
    assert_eq!(first.len(), 1);

    // Re-landing an already-merged candidate resumes processing idempotently.
    land_work_1(&main_dir, &bin_dir, true);

    let second = open_observation_files(&main_dir);
    assert_eq!(
        second, first,
        "re-processing a landed handoff reuses the same Observation"
    );
    assert_eq!(
        work_item_json_count(&main_dir),
        1,
        "re-processing derives no additional Work Item"
    );
}

// --- Corrective classification and Work authorization (Step 2) ---

const AUTHORITY_ANCHOR: &str = "Cap enforcement belongs in retry rs";
const AUTHORITY_PATH: &str = ".fluent/expertise/retry.md";
const OPERATION_ID: &str =
    "land-a1478b19201ae32c3d73895587323e1200206c0803f6469558d8b376c53c3a43";
const DERIVED_FU1: &str = "derived-land-a1478b19201ae32c3d73895587323e1200206c0803f6469558d8b376c53c3a43-a0eebf1952dae493547552e76655314a612b44205eec110b5745ccbbf378b4eb";
const DERIVED_FU2: &str = "derived-land-a1478b19201ae32c3d73895587323e1200206c0803f6469558d8b376c53c3a43-bfb56c767d9eb049aff6c02810b2a650d9fb0390a4fa87d5f894e1125713a85c";

/// Commit the trusted authority a corrective follow-up cites so the host gate
/// resolves it fresh at land time and the worktree stays clean.
fn commit_authority(main_dir: &Path) {
    let path = main_dir.join(AUTHORITY_PATH);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, format!("# Retry expertise\n\n{AUTHORITY_ANCHOR}\n")).unwrap();
    git::run(main_dir, &["add", AUTHORITY_PATH], "stage authority").unwrap();
    git::run(main_dir, &["commit", "-m", "Add retry expertise"], "commit authority").unwrap();
}

/// A corrective follow-up JSON object citing the committed expertise authority,
/// with a digest computed over the anchor so the gate accepts it.
fn corrective_follow_up_json(id: &str) -> String {
    let digest = fluent::follow_up::content_digest(AUTHORITY_ANCHOR.as_bytes());
    format!(
        r#"{{"id":"{id}","summary":"Restore the retry cap ({id})","corrective":true,"expected_result":"The retry cap is enforced again","target_paths":["src/retry.rs"],"corrective_context":{{"objective":"Restore the retry guard","requirement":"{AUTHORITY_ANCHOR}","evidence":"Merged commit removed the cap check","included_scope":"src/retry.rs","excluded_scope":"unrelated backoff tuning","verification":"cargo test retry"}},"authority":{{"kind":"expertise-entry","path":"{AUTHORITY_PATH}","anchor":"{AUTHORITY_ANCHOR}","digest":"{digest}"}}}}"#
    )
}

/// A corrective follow-up that cites no authority, so the gate keeps it
/// Observation-only.
fn untrusted_corrective_follow_up_json(id: &str) -> String {
    format!(
        r#"{{"id":"{id}","summary":"Untrusted change ({id})","corrective":true,"expected_result":"Something changes","target_paths":["src/lib.rs"],"corrective_context":{{"objective":"Do a thing","requirement":"The thing is done","evidence":"a hunch","included_scope":"src","excluded_scope":"tests","verification":"cargo test"}}}}"#
    )
}

fn learner_draft(follow_ups: &[String]) -> String {
    format!(
        r#"{{"learning_summary":"learned","follow_ups":[{}]}}"#,
        follow_ups.join(",")
    )
}

/// Write a project follow-up policy. `.fluent/config.yaml` is gitignored, so it
/// does not need committing to keep the worktree clean.
fn write_follow_up_policy(main_dir: &Path, yaml: &str) {
    fs::write(main_dir.join(".fluent/config.yaml"), yaml).unwrap();
}

fn queue_ledger_path(main_dir: &Path, work_item_id: &str) -> PathBuf {
    main_dir
        .join(".fluent/work/queue")
        .join(format!("{work_item_id}.json"))
}

#[test]
fn corrective_host_gate_requires_complete_fresh_trusted_context() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    commit_authority(&main_dir);
    let bin_dir = tmp.path().join("bin-gate");
    // fu-1 cites complete, fresh, trusted authority; fu-2 is corrective but
    // cites no authority, so only fu-1 may promote to Work.
    write_mock_claude(
        &bin_dir,
        &learner_land_mock_script(&learner_draft(&[
            corrective_follow_up_json("fu-1"),
            untrusted_corrective_follow_up_json("fu-2"),
        ])),
    );

    create_and_run_learner_attempt(&main_dir, &bin_dir);
    land_work_1(&main_dir, &bin_dir, true);

    // Both follow-ups materialize an Observation.
    assert_eq!(open_observation_files(&main_dir).len(), 2);
    // Only the trusted follow-up derives Work.
    assert!(main_dir.join(".fluent/work/items").join(format!("{DERIVED_FU1}.json")).exists());
    assert!(
        !main_dir.join(".fluent/work/items").join(format!("{DERIVED_FU2}.json")).exists(),
        "an untrusted corrective context stays Observation-only"
    );
}

#[test]
fn non_corrective_learning_remains_observation_only() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    let bin_dir = tmp.path().join("bin-noncorrective");
    write_mock_claude(
        &bin_dir,
        &learner_land_mock_script(
            r#"{"learning_summary":"learned","follow_ups":[{"id":"fu-1","summary":"Consider consolidating retries","corrective":false}]}"#,
        ),
    );

    create_and_run_learner_attempt(&main_dir, &bin_dir);
    land_work_1(&main_dir, &bin_dir, true);

    assert_eq!(open_observation_files(&main_dir).len(), 1);
    assert_eq!(
        work_item_json_count(&main_dir),
        1,
        "a non-corrective follow-up derives no Work Item"
    );
}

#[test]
fn propose_mode_creates_linked_proposed_work_item() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    commit_authority(&main_dir);
    let bin_dir = tmp.path().join("bin-propose");
    // Propose is the built-in default; no config needed.
    write_mock_claude(
        &bin_dir,
        &learner_land_mock_script(&learner_draft(&[corrective_follow_up_json("fu-1")])),
    );

    create_and_run_learner_attempt(&main_dir, &bin_dir);
    land_work_1(&main_dir, &bin_dir, true);

    let derived = work_item_value(&main_dir, DERIVED_FU1);
    assert_eq!(derived["authorization"]["state"], "proposed");
    assert_eq!(
        derived["origin"]["observation_id"], "followup-land-a1478b19201ae32c3d73895587323e1200206c0803f6469558d8b376c53c3a43-a0eebf1952dae493547552e76655314a612b44205eec110b5745ccbbf378b4eb",
        "the proposed Work Item links back to its Observation"
    );
    assert!(
        !queue_ledger_path(&main_dir, DERIVED_FU1).exists(),
        "propose mode creates no queue entry"
    );
}

#[test]
fn execute_mode_creates_ready_queued_corrective_work() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    commit_authority(&main_dir);
    write_follow_up_policy(&main_dir, "follow-up:\n  mode: execute\n");
    let bin_dir = tmp.path().join("bin-execute");
    write_mock_claude(
        &bin_dir,
        &learner_land_mock_script(&learner_draft(&[corrective_follow_up_json("fu-1")])),
    );

    create_and_run_learner_attempt(&main_dir, &bin_dir);
    land_work_1(&main_dir, &bin_dir, true);

    let derived = work_item_value(&main_dir, DERIVED_FU1);
    assert_eq!(derived["authorization"]["state"], "execution-ready");
    assert_eq!(derived["lineage"]["charged"], true);
    assert_eq!(
        latest_dispatch_status(&main_dir, DERIVED_FU1),
        "queued",
        "execute mode enqueues the corrective Work exactly once"
    );
    assert_eq!(dispatch_count(&main_dir, DERIVED_FU1), 1);
}

#[test]
fn execute_mode_at_lineage_limit_retains_proposed_work() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    commit_authority(&main_dir);
    // Execute mode with room for a single descendant in the lineage.
    write_follow_up_policy(&main_dir, "follow-up:\n  mode: execute\n  descendant-limit: 1\n");
    let bin_dir = tmp.path().join("bin-limit");
    // Two corrective follow-ups compete for one lineage slot.
    write_mock_claude(
        &bin_dir,
        &learner_land_mock_script(&learner_draft(&[
            corrective_follow_up_json("fu-1"),
            corrective_follow_up_json("fu-2"),
        ])),
    );

    create_and_run_learner_attempt(&main_dir, &bin_dir);
    land_work_1(&main_dir, &bin_dir, true);

    // The first descendant is authorized and queued; the second is retained
    // proposed because the lineage budget is exhausted.
    let first = work_item_value(&main_dir, DERIVED_FU1);
    assert_eq!(first["authorization"]["state"], "execution-ready");
    assert_eq!(latest_dispatch_status(&main_dir, DERIVED_FU1), "queued");

    let second = work_item_value(&main_dir, DERIVED_FU2);
    assert_eq!(second["authorization"]["state"], "proposed");
    assert!(
        !queue_ledger_path(&main_dir, DERIVED_FU2).exists(),
        "an exhausted lineage budget does not enqueue the descendant"
    );
}

#[test]
fn followup_retry_does_not_reapply_changed_policy() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    commit_authority(&main_dir);
    write_follow_up_policy(&main_dir, "follow-up:\n  mode: execute\n");
    let bin_dir = tmp.path().join("bin-policy-drift");
    write_mock_claude(
        &bin_dir,
        &learner_land_mock_script(&learner_draft(&[corrective_follow_up_json("fu-1")])),
    );

    create_and_run_learner_attempt(&main_dir, &bin_dir);
    land_work_1(&main_dir, &bin_dir, true);
    assert_eq!(
        work_item_value(&main_dir, DERIVED_FU1)["authorization"]["state"],
        "execution-ready"
    );

    // The operator changes the policy to propose, then re-lands the already
    // merged candidate. The frozen decision is reused, not re-applied.
    write_follow_up_policy(&main_dir, "follow-up:\n  mode: propose\n");
    land_work_1(&main_dir, &bin_dir, true);

    let derived = work_item_value(&main_dir, DERIVED_FU1);
    assert_eq!(
        derived["authorization"]["state"], "execution-ready",
        "a changed policy does not re-decide an already-promoted follow-up"
    );
    assert_eq!(
        dispatch_count(&main_dir, DERIVED_FU1),
        1,
        "the retry adds no duplicate dispatch"
    );
}

fn authorize_work_item(main_dir: &Path, id: &str) {
    fluent_cmd()
        .current_dir(main_dir)
        .args(["work-item", "authorize", id])
        .assert()
        .success();
}

/// Land a propose-mode corrective follow-up, leaving one proposed derived Work
/// Item at `DERIVED_FU1`.
fn land_one_proposed_corrective(main_dir: &Path, tmp: &TempDir, bin_name: &str) {
    commit_authority(main_dir);
    let bin_dir = tmp.path().join(bin_name);
    write_mock_claude(
        &bin_dir,
        &learner_land_mock_script(&learner_draft(&[corrective_follow_up_json("fu-1")])),
    );
    create_and_run_learner_attempt(main_dir, &bin_dir);
    land_work_1(main_dir, &bin_dir, true);
}

#[test]
fn human_authorization_transitions_same_work_item() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    land_one_proposed_corrective(&main_dir, &tmp, "bin-authorize");
    assert_eq!(
        work_item_value(&main_dir, DERIVED_FU1)["authorization"]["state"],
        "proposed"
    );

    authorize_work_item(&main_dir, DERIVED_FU1);

    let derived = work_item_value(&main_dir, DERIVED_FU1);
    assert_eq!(derived["id"], DERIVED_FU1, "the same Work Item transitions in place");
    assert_eq!(derived["authorization"]["state"], "execution-ready");
    assert_eq!(
        derived["authorization"]["authority"], "human",
        "the human authority is recorded"
    );
}

#[test]
fn human_authorization_immediately_enqueues_work() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    land_one_proposed_corrective(&main_dir, &tmp, "bin-authorize-enqueue");
    assert!(!queue_ledger_path(&main_dir, DERIVED_FU1).exists());

    authorize_work_item(&main_dir, DERIVED_FU1);

    assert_eq!(latest_dispatch_status(&main_dir, DERIVED_FU1), "queued");
    assert_eq!(dispatch_count(&main_dir, DERIVED_FU1), 1);
}

#[test]
fn authorization_crash_before_queue_write_is_recoverable() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    land_one_proposed_corrective(&main_dir, &tmp, "bin-authorize-crash");

    // Simulate a crash after the locked Work mutation persisted the
    // authorization and enqueue intent but before the queue dispatch was
    // written: the Work Item is execution-ready with its intent intact and no
    // queue ledger.
    let item_path = main_dir
        .join(".fluent/work/items")
        .join(format!("{DERIVED_FU1}.json"));
    let mut value = read_json_path(&item_path);
    value["authorization"] = serde_json::json!({"state": "execution-ready", "authority": "human"});
    value["lineage"]["charged"] = serde_json::json!(true);
    fs::write(&item_path, serde_json::to_string_pretty(&value).unwrap()).unwrap();
    assert!(
        value["pending_enqueue"]["origin_operation_id"].is_string(),
        "the durable enqueue intent survives the crash"
    );
    assert!(!queue_ledger_path(&main_dir, DERIVED_FU1).exists());

    // Re-invoking authorize reconciles the dispatch from the durable intent.
    authorize_work_item(&main_dir, DERIVED_FU1);
    assert_eq!(latest_dispatch_status(&main_dir, DERIVED_FU1), "queued");
    assert_eq!(dispatch_count(&main_dir, DERIVED_FU1), 1);
}

#[test]
fn human_authorization_can_override_exhausted_automatic_budget() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    commit_authority(&main_dir);
    write_follow_up_policy(&main_dir, "follow-up:\n  mode: execute\n  descendant-limit: 1\n");
    let bin_dir = tmp.path().join("bin-override");
    write_mock_claude(
        &bin_dir,
        &learner_land_mock_script(&learner_draft(&[
            corrective_follow_up_json("fu-1"),
            corrective_follow_up_json("fu-2"),
        ])),
    );
    create_and_run_learner_attempt(&main_dir, &bin_dir);
    land_work_1(&main_dir, &bin_dir, true);

    // Automatic promotion stopped at the single lineage slot: fu-1 authorized,
    // fu-2 retained proposed.
    assert_eq!(
        work_item_value(&main_dir, DERIVED_FU1)["authorization"]["state"],
        "execution-ready"
    );
    assert_eq!(
        work_item_value(&main_dir, DERIVED_FU2)["authorization"]["state"],
        "proposed"
    );

    // A human explicitly authorizes the proposed descendant beyond the exhausted
    // automatic budget.
    authorize_work_item(&main_dir, DERIVED_FU2);
    let overridden = work_item_value(&main_dir, DERIVED_FU2);
    assert_eq!(overridden["authorization"]["state"], "execution-ready");
    assert_eq!(overridden["authorization"]["authority"], "human");
    assert_eq!(overridden["lineage"]["charged"], true);
    assert_eq!(latest_dispatch_status(&main_dir, DERIVED_FU2), "queued");

    // The lineage now holds two charges against a limit of one — the human
    // override exceeded the cap while the automatic path had correctly stopped.
    assert_eq!(work_item_value(&main_dir, DERIVED_FU1)["lineage"]["charged"], true);
}

#[test]
fn repeated_work_authorization_is_idempotent() {
    use fluent::queue::{self, DispatchStatus};

    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    land_one_proposed_corrective(&main_dir, &tmp, "bin-reauthorize");

    authorize_work_item(&main_dir, DERIVED_FU1);
    assert_eq!(latest_dispatch_status(&main_dir, DERIVED_FU1), "queued");

    // Re-authorizing an already execution-ready Work Item preserves its
    // authorization and lineage charge without duplicating the dispatch.
    authorize_work_item(&main_dir, DERIVED_FU1);
    let derived = work_item_value(&main_dir, DERIVED_FU1);
    assert_eq!(derived["authorization"]["state"], "execution-ready");
    assert_eq!(derived["lineage"]["charged"], true);
    assert_eq!(dispatch_count(&main_dir, DERIVED_FU1), 1);

    // Drive the dispatch to a terminal disposition; re-authorization must not
    // revive it.
    let token = queue::claim(&main_dir, DERIVED_FU1, "attempt-x").unwrap().unwrap();
    queue::reconcile(&main_dir, &token, DispatchStatus::Failed).unwrap();
    authorize_work_item(&main_dir, DERIVED_FU1);
    assert_eq!(
        latest_dispatch_status(&main_dir, DERIVED_FU1),
        "failed",
        "re-authorization never reactivates a terminal dispatch"
    );
    assert_eq!(dispatch_count(&main_dir, DERIVED_FU1), 1);
}

#[test]
fn repeated_authorization_repairs_missing_dispatch() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    land_one_proposed_corrective(&main_dir, &tmp, "bin-repair");

    authorize_work_item(&main_dir, DERIVED_FU1);
    assert_eq!(latest_dispatch_status(&main_dir, DERIVED_FU1), "queued");

    // The queue ledger is lost; a repeated authorization repairs the missing
    // dispatch required by the durable enqueue intent.
    fs::remove_file(queue_ledger_path(&main_dir, DERIVED_FU1)).unwrap();
    assert!(!queue_ledger_path(&main_dir, DERIVED_FU1).exists());

    authorize_work_item(&main_dir, DERIVED_FU1);
    assert_eq!(latest_dispatch_status(&main_dir, DERIVED_FU1), "queued");
    assert_eq!(dispatch_count(&main_dir, DERIVED_FU1), 1);
}

// --- Post-land Learner retry, handoff-only (Step 4) ---

#[test]
fn post_land_learner_retry_materializes_recovered_handoff() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    let counter = tmp.path().join("learner-counter");
    let bin_dir = tmp.path().join("bin-recovered");
    write_mock_claude(
        &bin_dir,
        &post_land_learner_mock_script(
            &counter,
            r#"{"learning_summary":"learned late","follow_ups":[{"id":"fu-1","summary":"Recovered follow-up","corrective":false}]}"#,
            false,
        ),
    );

    // The first Learner run fails, so land materializes nothing.
    create_and_run_learner_attempt(&main_dir, &bin_dir);
    land_work_1(&main_dir, &bin_dir, true);
    let candidate_workspace = main_dir.join("../work-6-work-1-attempt-1");
    assert!(
        candidate_workspace.is_dir(),
        "land retains the candidate workspace while Learning is retryable"
    );
    assert!(
        open_observation_files(&main_dir).is_empty(),
        "a failed learner leaves nothing to materialize at land"
    );

    // Applying cleanup before recovery must preserve the complete origin,
    // including the workspace needed by the handoff-only retry.
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["cleanup", "--apply"])
        .assert()
        .success();
    assert!(main_dir.join(".fluent/work/items/work-1.json").exists());
    assert!(candidate_workspace.is_dir());

    // Retrying the Learner after land recovers the handoff and materializes it
    // immediately under the land-gated rules.
    rerun_learner_attempt(&main_dir, &bin_dir);
    let observations = open_observation_files(&main_dir);
    assert_eq!(observations.len(), 1, "the recovered handoff materializes one Observation");
    let observation = fs::read_to_string(
        main_dir.join(".fluent/observations").join(&observations[0]),
    )
    .unwrap();
    assert!(observation.contains("follow-up-id: fu-1"));
    assert!(
        !candidate_workspace.exists(),
        "successful post-land recovery removes the retained candidate workspace"
    );

    fluent_cmd()
        .current_dir(&main_dir)
        .args(["cleanup", "--apply"])
        .assert()
        .success();
    assert!(!main_dir.join(".fluent/work/items/work-1.json").exists());
    assert_eq!(
        open_observation_files(&main_dir).len(),
        1,
        "cleanup after recovery preserves the materialized descendant"
    );
}

#[test]
fn missing_legacy_learning_record_retries_after_land() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    let counter = tmp.path().join("learner-counter-missing-record");
    let bin_dir = tmp.path().join("bin-missing-record");
    write_mock_claude(
        &bin_dir,
        &post_land_learner_mock_script(
            &counter,
            r#"{"learning_summary":"legacy retry","follow_ups":[{"id":"fu-1","summary":"Recovered legacy follow-up","corrective":false}]}"#,
            false,
        ),
    );

    create_and_run_learner_attempt(&main_dir, &bin_dir);
    land_work_1(&main_dir, &bin_dir, true);
    let attempt_path = main_dir.join(".fluent/work/attempts/work-1/attempt-1.json");
    let mut attempt = read_json_value(&attempt_path);
    attempt.as_object_mut().unwrap().remove("learning");
    write_json_value(&attempt_path, &attempt);

    fluent_cmd()
        .current_dir(&main_dir)
        .args(["cleanup", "--apply"])
        .assert()
        .success();
    assert!(main_dir.join(".fluent/work/items/work-1.json").exists());
    assert!(main_dir.join("../work-6-work-1-attempt-1").is_dir());

    rerun_learner_attempt(&main_dir, &bin_dir);

    assert_eq!(open_observation_files(&main_dir).len(), 1);
    assert_eq!(
        fs::read_to_string(format!("{}.invocations", counter.display()))
            .unwrap()
            .lines()
            .count(),
        2,
        "a missing legacy record triggers one post-land Learner retry"
    );
    assert!(!main_dir.join("../work-6-work-1-attempt-1").exists());
}

#[test]
fn successful_learning_resumes_failed_materialization_without_rerunning_coder() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    let counter = tmp.path().join("learner-counter-materialization");
    let bin_dir = tmp.path().join("bin-materialization-resume");
    write_mock_claude(
        &bin_dir,
        &post_land_learner_mock_script(
            &counter,
            r#"{"learning_summary":"learned late","follow_ups":[{"id":"fu-1","summary":"Recovered follow-up","corrective":false}]}"#,
            false,
        ),
    );

    create_and_run_learner_attempt(&main_dir, &bin_dir);
    land_work_1(&main_dir, &bin_dir, true);
    fs::write(main_dir.join(".fluent/observations"), "block observation directory\n").unwrap();

    rerun_learner_attempt(&main_dir, &bin_dir);
    let failed = work_item_value(&main_dir, "work-1");
    assert_eq!(
        failed["merge_candidates"][0]["merge_state"]["follow_up_failure"]["stage"],
        "observation"
    );
    assert_eq!(
        fs::read_to_string(format!("{}.invocations", counter.display()))
            .unwrap()
            .lines()
            .count(),
        2,
        "one initial failure and one successful Learner retry"
    );

    fs::remove_file(main_dir.join(".fluent/observations")).unwrap();
    rerun_learner_attempt(&main_dir, &bin_dir);

    assert_eq!(open_observation_files(&main_dir).len(), 1);
    let recovered = work_item_value(&main_dir, "work-1");
    assert!(
        recovered["merge_candidates"][0]["merge_state"]["follow_up_failure"].is_null(),
        "successful resume clears the durable failure"
    );
    assert_eq!(
        fs::read_to_string(format!("{}.invocations", counter.display()))
            .unwrap()
            .lines()
            .count(),
        2,
        "materialization recovery must not rerun a successful Learner"
    );
}

#[test]
fn post_land_learner_retry_preserves_merged_commit() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    let counter = tmp.path().join("learner-counter");
    let bin_dir = tmp.path().join("bin-preserve");
    // The retry attempts an expertise commit, which a handoff-only run discards.
    write_mock_claude(
        &bin_dir,
        &post_land_learner_mock_script(
            &counter,
            r#"{"learning_summary":"learned late","follow_ups":[]}"#,
            true,
        ),
    );

    create_and_run_learner_attempt(&main_dir, &bin_dir);
    let candidate = main_dir.join("../work-6-work-1-attempt-1");
    fs::write(candidate.join("merge-fix.txt"), "accepted merge fix\n").unwrap();
    git::run(&candidate, &["add", "merge-fix.txt"], "stage accepted merge fix").unwrap();
    git::run(
        &candidate,
        &["commit", "-m", "Apply accepted merge fix"],
        "commit accepted merge fix",
    )
    .unwrap();
    fs::write(main_dir.join("target-only.txt"), "unrelated target change\n").unwrap();
    git::run(&main_dir, &["add", "target-only.txt"], "stage target-only change").unwrap();
    git::run(
        &main_dir,
        &["commit", "-m", "Add target-only change"],
        "commit target-only change",
    )
    .unwrap();
    let accepted_base = git_head(&main_dir);
    land_work_1(&main_dir, &bin_dir, true);
    let merged_before = merged_commit_of(&main_dir);

    // Simulate an already-merged legacy TaskOutput written before base_commit
    // existed. Recovery must derive an immutable range from merge/Git state,
    // not fall back to the now-moving `main` source branch.
    let task_path = work_task_record_path(&main_dir, "work-1", "attempt-1", "attempt-1-write-1");
    let mut task = read_json_value(&task_path);
    task["output"].as_object_mut().unwrap().remove("base_commit");
    write_json_value(&task_path, &task);

    let candidate_reflog = git::run_stdout(
        &candidate,
        &["reflog", "show", "--format=%H%x09%gs", "HEAD"],
        "inspect candidate reflog",
    )
    .unwrap();

    rerun_learner_attempt(&main_dir, &bin_dir);

    let retry_prompt = fs::read_to_string(format!("{}.prompt", counter.display())).unwrap();
    assert!(
        retry_prompt.contains(&format!("{accepted_base}...{merged_before}")),
        "post-land retry prompt must render the persisted accepted change; reflog:\n{candidate_reflog}\nprompt:\n{retry_prompt}"
    );
    let accepted_files = git::run_stdout(
        &main_dir,
        &["diff", "--name-only", &format!("{accepted_base}...{merged_before}")],
        "inspect accepted Attempt change",
    )
    .unwrap();
    assert!(accepted_files.lines().any(|path| path == "loop-output.txt"));
    assert!(accepted_files.lines().any(|path| path == "merge-fix.txt"));
    assert!(
        !accepted_files.lines().any(|path| path == "target-only.txt"),
        "the immutable accepted range excludes unrelated target-only history: {accepted_files}"
    );

    // The merged candidate commit is unchanged and no expertise reached main.
    assert_eq!(
        merged_commit_of(&main_dir),
        merged_before,
        "a post-land handoff-only retry leaves the merged commit unchanged"
    );
    assert!(
        !main_dir.join(".fluent/expertise/late.md").exists(),
        "a post-land handoff-only retry writes no expertise to the merged branch"
    );
}

#[test]
fn concurrent_learner_retry_and_land_never_mutate_after_merge() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    let counter = tmp.path().join("learner-counter");
    let retry_started = tmp.path().join("retry-started");
    let retry_release = tmp.path().join("retry-release");
    let land_blocked = tmp.path().join("land-lock-blocked");
    let bin_dir = tmp.path().join("bin-serialize");
    write_mock_claude(
        &bin_dir,
        &contended_learner_mock_script(
            &counter,
            &retry_started,
            &retry_release,
        ),
    );

    create_and_run_learner_attempt(&main_dir, &bin_dir);

    let retry = std::process::Command::new(assert_cmd::cargo::cargo_bin("fluent"))
        .current_dir(&main_dir)
        .args(["attempt", "run", "work-1", "attempt-1", "--no-sandbox"])
        .env("PATH", mock_path(&bin_dir))
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    for _ in 0..500 {
        if retry_started.exists() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(retry_started.exists(), "learner retry reached its dirty window");

    let land = std::process::Command::new(assert_cmd::cargo::cargo_bin("fluent"))
        .current_dir(&main_dir)
        .args([
            "merge-candidate",
            "land",
            "work-1",
            "attempt-1-merge-candidate",
            "--no-sandbox",
            "--no-post-merge-review",
        ])
        .env("PATH", mock_path(&bin_dir))
        .env("FLUENT_TEST_LAND_LOCK_BLOCKED_PATH", &land_blocked)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    for _ in 0..500 {
        if land_blocked.exists() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let land_reached_contended_boundary = land_blocked.exists();

    fs::write(&retry_release, "release\n").unwrap();
    let retry_output = retry.wait_with_output().unwrap();
    let land_output = land.wait_with_output().unwrap();

    assert!(
        land_reached_contended_boundary,
        "land must report actual contention while retry holds the boundary; stderr={}",
        String::from_utf8_lossy(&land_output.stderr)
    );
    assert!(
        retry_output.status.success(),
        "learner retry failed: {}",
        String::from_utf8_lossy(&retry_output.stderr)
    );
    assert!(
        land_output.status.success(),
        "land failed after retry released the boundary: {}",
        String::from_utf8_lossy(&land_output.stderr)
    );

    // The retry's out-of-bounds commit is confined and discarded before land
    // inspects the candidate workspace.
    assert!(!main_dir.join("transient-learner.txt").exists());
    assert!(is_merged(&main_dir));
}

#[test]
fn post_land_handoff_only_restores_hostile_git_mutations() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    let counter = tmp.path().join("learner-counter-hostile");
    let bin_dir = tmp.path().join("bin-hostile-git");
    write_mock_claude(
        &bin_dir,
        &hostile_post_land_learner_mock_script(&counter),
    );

    create_and_run_learner_attempt(&main_dir, &bin_dir);
    land_work_1(&main_dir, &bin_dir, true);
    let merged = merged_commit_of(&main_dir);
    fs::write(main_dir.join("preexisting-staged.txt"), "staged before retry\n").unwrap();
    git::run(
        &main_dir,
        &["add", "preexisting-staged.txt"],
        "stage preexisting target file",
    )
    .unwrap();
    fs::write(main_dir.join("preexisting-untracked.txt"), "untracked before retry\n").unwrap();
    fs::write(main_dir.join("README.md"), "# Test\n\nunstaged before retry\n").unwrap();
    git::run(
        &main_dir,
        &["update-index", "--assume-unchanged", "loop-output.txt"],
        "set preexisting index flag",
    )
    .unwrap();
    let target_status_before = git::run_stdout(
        &main_dir,
        &["status", "--porcelain", "--untracked-files=all"],
        "target status before hostile retry",
    )
    .unwrap();
    let index_flags_before = git::run_stdout(
        &main_dir,
        &["ls-files", "-v", "loop-output.txt"],
        "target index flags before hostile retry",
    )
    .unwrap();
    rerun_learner_attempt(&main_dir, &bin_dir);

    assert_eq!(
        git::run_stdout(&main_dir, &["rev-parse", "HEAD"], "target HEAD").unwrap(),
        merged
    );
    let candidate = main_dir.join("../work-6-work-1-attempt-1");
    assert_eq!(
        git::run_stdout(&candidate, &["rev-parse", "HEAD"], "candidate HEAD").unwrap(),
        merged
    );
    assert!(
        git::run_stdout(
            &candidate,
            &["status", "--porcelain", "--untracked-files=all"],
            "candidate status",
        )
        .unwrap()
        .is_empty(),
        "candidate index and worktree are restored"
    );
    assert!(!main_dir.join("target-mutated.txt").exists());
    assert_eq!(
        git::run_stdout(
            &main_dir,
            &["status", "--porcelain", "--untracked-files=all"],
            "target status after hostile retry",
        )
        .unwrap(),
        target_status_before,
        "the guard preserves staged, unstaged, and untracked target state"
    );
    assert_eq!(
        git::run_stdout(
            &main_dir,
            &["ls-files", "-v", "loop-output.txt"],
            "target index flags after hostile retry",
        )
        .unwrap(),
        index_flags_before,
        "the guard preserves complete index flags"
    );
    assert!(!candidate.join("candidate-index.txt").exists());
    assert!(
        !git::run_raw(
            &main_dir,
            &["show-ref", "--verify", "--quiet", "refs/heads/unauthorized"],
        )
        .unwrap()
        .status
        .success(),
        "unauthorized refs are removed"
    );
    assert!(
        work_item_value(&main_dir, "work-1")["attempts"][0]["learning"]["status"]
            == "failed",
        "a protected Git mutation rejects the Learner result"
    );
}

#[test]
fn sandboxed_post_land_coder_cannot_mutate_candidate_or_shared_git() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    let counter = tmp.path().join("learner-counter-sandbox-hostile");
    let bin_dir = tmp.path().join("bin-sandbox-hostile");
    write_mock_claude(
        &bin_dir,
        &hostile_post_land_learner_mock_script(&counter),
    );

    create_and_run_learner_attempt(&main_dir, &bin_dir);
    land_work_1(&main_dir, &bin_dir, true);
    let merged = merged_commit_of(&main_dir);
    rerun_learner_attempt_sandboxed(&main_dir, &bin_dir);

    let candidate = main_dir.join("../work-6-work-1-attempt-1");
    assert_eq!(git_head(&main_dir), merged);
    assert_eq!(git_head(&candidate), merged);
    assert!(!main_dir.join("target-mutated.txt").exists());
    assert!(!candidate.join("candidate-index.txt").exists());
    assert!(
        !git::run_raw(
            &main_dir,
            &["show-ref", "--verify", "--quiet", "refs/heads/unauthorized"],
        )
        .unwrap()
        .status
        .success(),
        "the effective Seatbelt profile denies shared-Git ref writes"
    );
}

#[test]
fn failed_post_land_coder_still_restores_candidate_index_and_worktree() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    let counter = tmp.path().join("learner-counter-failed-dirty");
    let bin_dir = tmp.path().join("bin-failed-dirty");
    write_mock_claude(
        &bin_dir,
        &failing_dirty_post_land_learner_mock_script(&counter),
    );

    create_and_run_learner_attempt(&main_dir, &bin_dir);
    land_work_1(&main_dir, &bin_dir, true);
    rerun_learner_attempt(&main_dir, &bin_dir);

    let candidate = main_dir.join("../work-6-work-1-attempt-1");
    assert!(
        git::run_stdout(
            &candidate,
            &["status", "--porcelain", "--untracked-files=all"],
            "candidate status after failed coder",
        )
        .unwrap()
        .is_empty(),
        "cleanup runs even when the handoff-only coder exits nonzero"
    );
    assert!(!candidate.join("failed-retry.txt").exists());
}

#[test]
fn concurrent_post_land_retries_run_the_learner_once() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    let counter = tmp.path().join("learner-counter-concurrent-retry");
    let bin_dir = tmp.path().join("bin-concurrent-retry");
    write_mock_claude(
        &bin_dir,
        &post_land_learner_mock_script(
            &counter,
            r#"{"learning_summary":"one retry","follow_ups":[]}"#,
            false,
        ),
    );

    create_and_run_learner_attempt(&main_dir, &bin_dir);
    land_work_1(&main_dir, &bin_dir, true);

    let spawn_retry = || {
        std::process::Command::new(assert_cmd::cargo::cargo_bin("fluent"))
            .current_dir(&main_dir)
            .args(["attempt", "run", "work-1", "attempt-1", "--no-sandbox"])
            .env("PATH", mock_path(&bin_dir))
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap()
    };
    let first = spawn_retry();
    let second = spawn_retry();
    let first_output = first.wait_with_output().unwrap();
    let second_output = second.wait_with_output().unwrap();
    assert!(first_output.status.success());
    assert!(second_output.status.success());

    let invocations = fs::read_to_string(format!("{}.invocations", counter.display())).unwrap();
    assert_eq!(
        invocations.lines().count(),
        2,
        "one initial failure plus exactly one post-land retry"
    );
}

#[test]
fn post_land_retry_resets_interrupted_candidate_commit_to_merged_commit() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    let counter = tmp.path().join("learner-counter-crash-window");
    let bin_dir = tmp.path().join("bin-crash-window");
    write_mock_claude(
        &bin_dir,
        &post_land_learner_mock_script(
            &counter,
            r#"{"learning_summary":"anchored retry","follow_ups":[]}"#,
            false,
        ),
    );

    create_and_run_learner_attempt(&main_dir, &bin_dir);
    land_work_1(&main_dir, &bin_dir, true);
    let merged = merged_commit_of(&main_dir);
    let candidate = main_dir.join("../work-6-work-1-attempt-1");
    fs::write(candidate.join("interrupted.txt"), "unauthorized\n").unwrap();
    git::run(&candidate, &["add", "interrupted.txt"], "stage interrupted commit").unwrap();
    git::run(
        &candidate,
        &["commit", "-m", "Interrupted Learner commit"],
        "create interrupted commit",
    )
    .unwrap();

    rerun_learner_attempt(&main_dir, &bin_dir);

    assert_eq!(
        fs::read_to_string(format!("{}.head", counter.display()))
            .unwrap()
            .trim(),
        merged,
        "the coder starts from the durable merged commit"
    );
    assert_eq!(
        git::run_stdout(&candidate, &["rev-parse", "HEAD"], "candidate HEAD").unwrap(),
        merged
    );
    assert!(!candidate.join("interrupted.txt").exists());
}

#[test]
fn post_land_expertise_proposal_materializes_observation_only() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    let counter = tmp.path().join("learner-counter");
    let bin_dir = tmp.path().join("bin-expertise-proposal");
    // The retry's only output is a denied expertise commit; it must surface as a
    // non-corrective Observation, not as Work.
    write_mock_claude(
        &bin_dir,
        &post_land_learner_mock_script(
            &counter,
            r#"{"learning_summary":"missed expertise","follow_ups":[]}"#,
            true,
        ),
    );

    create_and_run_learner_attempt(&main_dir, &bin_dir);
    land_work_1(&main_dir, &bin_dir, true);
    rerun_learner_attempt(&main_dir, &bin_dir);

    let observations = open_observation_files(&main_dir);
    assert_eq!(observations.len(), 1, "the missed expertise becomes one Observation");
    let content =
        fs::read_to_string(main_dir.join(".fluent/observations").join(&observations[0])).unwrap();
    assert!(
        content.contains("follow-up-id: expertise-"),
        "the Observation is the synthesized expertise proposal; content:\n{content}"
    );
    // It derives no Work: only the originating Work Item exists.
    assert_eq!(
        work_item_json_count(&main_dir),
        1,
        "an expertise proposal materializes as an Observation only"
    );
}

// --- Failure preservation, resume, cleanup (Step 5) ---

const OBS_FU1: &str = "followup-land-a1478b19201ae32c3d73895587323e1200206c0803f6469558d8b376c53c3a43-a0eebf1952dae493547552e76655314a612b44205eec110b5745ccbbf378b4eb";
const HANDOFF_PATH: &str = ".fluent/work/artifacts/work-1/attempt-1/learner/handoff.json";

/// The recorded follow-up-processing failure stage for the landed candidate, or
/// an empty string when none is recorded.
fn follow_up_failure_stage(main_dir: &Path) -> String {
    work_item_value(main_dir, "work-1")["merge_candidates"][0]["merge_state"]["follow_up_failure"]
        ["stage"]
        .as_str()
        .unwrap_or("")
        .to_string()
}

fn is_merged(main_dir: &Path) -> bool {
    work_item_value(main_dir, "work-1")["merge_candidates"][0]["merge_state"]["status"] == "merged"
}

/// Run a corrective learner attempt (committing its authority), leaving a
/// verified handoff ready to land. Returns the mock bin dir.
fn run_corrective_attempt(main_dir: &Path, tmp: &TempDir, bin_name: &str, execute: bool) -> PathBuf {
    commit_authority(main_dir);
    if execute {
        write_follow_up_policy(main_dir, "follow-up:\n  mode: execute\n");
    }
    let bin_dir = tmp.path().join(bin_name);
    write_mock_claude(
        &bin_dir,
        &learner_land_mock_script(&learner_draft(&[corrective_follow_up_json("fu-1")])),
    );
    create_and_run_learner_attempt(main_dir, &bin_dir);
    bin_dir
}

#[test]
fn invalid_learner_handoff_preserves_land_and_records_recovery() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    let bin_dir = tmp.path().join("bin-invalid");
    write_mock_claude(
        &bin_dir,
        &learner_land_mock_script(
            r#"{"learning_summary":"learned","follow_ups":[{"id":"fu-1","summary":"Later","corrective":false}]}"#,
        ),
    );
    create_and_run_learner_attempt(&main_dir, &bin_dir);

    // Corrupt the verified handoff so it no longer matches its digest.
    fs::write(main_dir.join(HANDOFF_PATH), b"{ not a valid handoff").unwrap();
    land_work_1(&main_dir, &bin_dir, true);

    // The merge stays successful, nothing materializes, and a retryable failure
    // is recorded with a next action.
    assert!(is_merged(&main_dir), "a malformed handoff does not undo the land");
    assert!(open_observation_files(&main_dir).is_empty());
    assert_eq!(follow_up_failure_stage(&main_dir), "validate-handoff");
    let next_action = work_item_value(&main_dir, "work-1")["merge_candidates"][0]["merge_state"]
        ["follow_up_failure"]["next_action"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(next_action.contains("merge-candidate land"));
}

#[test]
fn mismatched_learner_handoff_provenance_is_rejected() {
    use fluent::follow_up::{
        LearnerHandoffV1, LearningRecord, NormalizedFollowUpBatchV1, PostLandOrigin,
    };
    // A handoff whose origin does not match the Merge Candidate being processed
    // is rejected before any Observation or Work is produced.
    let handoff = LearnerHandoffV1 {
        schema_version: LearnerHandoffV1::SCHEMA_VERSION,
        source_work_item_id: "work-1".to_string(),
        source_attempt_id: "attempt-1".to_string(),
        source_merge_candidate_id: Some("attempt-1-merge-candidate".to_string()),
        learning: LearningRecord::default(),
        follow_ups: Vec::new(),
    };
    let mismatched = PostLandOrigin {
        work_item_id: "work-1".to_string(),
        attempt_id: "attempt-1".to_string(),
        merge_candidate_id: "a-different-candidate".to_string(),
        merged_commit: "abc123".to_string(),
    };
    let error = NormalizedFollowUpBatchV1::from_learner_handoff(&handoff, mismatched).unwrap_err();
    assert!(
        error.to_string().contains("does not match"),
        "an origin-mismatched handoff is rejected: {error}"
    );
}

#[test]
fn rerun_merged_candidate_resumes_handoff_only() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    let bin_dir = tmp.path().join("bin-rerun");
    write_mock_claude(
        &bin_dir,
        &learner_land_mock_script(
            r#"{"learning_summary":"learned","follow_ups":[{"id":"fu-1","summary":"Consolidate","corrective":false}]}"#,
        ),
    );
    create_and_run_learner_attempt(&main_dir, &bin_dir);
    land_work_1(&main_dir, &bin_dir, true);
    let merged_before = merged_commit_of(&main_dir);
    let observations_before = open_observation_files(&main_dir);

    // Re-landing an already-merged candidate must resume follow-up processing
    // without rebasing or re-merging. A mock that fails any rebase proves the
    // resume path never rebases.
    let rerun_bin = tmp.path().join("bin-rerun-norebase");
    write_mock_claude(&rerun_bin, &rebase_failing_mock_script());
    land_work_1(&main_dir, &rerun_bin, true);

    assert_eq!(merged_commit_of(&main_dir), merged_before, "the merge is not repeated");
    assert_eq!(
        open_observation_files(&main_dir),
        observations_before,
        "resume reuses the same Observation"
    );
}

#[test]
fn cleanup_preserves_descendant_context_after_origin_removal() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    let bin_dir = run_corrective_attempt(&main_dir, &tmp, "bin-descendant", true);
    land_work_1(&main_dir, &bin_dir, true);
    let origin_merged_commit = merged_commit_of(&main_dir);

    let origin_item = main_dir.join(".fluent/work/items/work-1.json");
    let origin_attempts = main_dir.join(".fluent/work/attempts/work-1");
    let origin_tasks = main_dir.join(".fluent/work/tasks/work-1");
    let origin_artifacts = main_dir.join(".fluent/work/artifacts/work-1");
    let candidate_workspace = main_dir.join("../work-6-work-1-attempt-1");

    // Land normally removes the candidate worktree. Re-register the persisted
    // source branch at its managed path so real cleanup must remove every kind
    // of origin state, including a registered worktree and optional artifacts.
    assert!(!candidate_workspace.exists());
    let merged_commit = merged_commit_of(&main_dir);
    git::run(
        &main_dir,
        &[
            "worktree",
            "add",
            "--detach",
            candidate_workspace.to_str().unwrap(),
            &merged_commit,
        ],
        "recreate origin worktree for cleanup",
    )
    .unwrap();
    assert!(origin_item.exists());
    assert!(origin_attempts.is_dir());
    assert!(origin_tasks.is_dir());
    assert!(origin_artifacts.is_dir());

    fluent_cmd()
        .current_dir(&main_dir)
        .args(["cleanup", "--apply"])
        .assert()
        .success()
        .stdout(predicate::str::contains("cleaned Work Item work-1"));

    assert!(!origin_item.exists(), "cleanup removes the originating Work Item");
    assert!(!origin_attempts.exists(), "cleanup removes the originating Attempt records");
    assert!(!origin_tasks.exists(), "cleanup removes the originating Task records");
    assert!(!candidate_workspace.exists(), "cleanup removes the registered candidate worktree");
    assert!(!origin_artifacts.exists(), "cleanup removes optional origin artifacts");

    // The derived Work stays inspectable with self-contained corrective context
    // and provenance identifiers.
    let derived = work_item_value(&main_dir, DERIVED_FU1);
    assert!(derived["corrective_context"]["objective"].is_string());
    assert_eq!(derived["origin"]["merged_commit"], origin_merged_commit);
    assert_eq!(derived["origin"]["observation_id"], OBS_FU1);

    // The Observation stays inspectable with its origin identifiers.
    let show = fluent_cmd()
        .current_dir(&main_dir)
        .args(["observation", "show", OBS_FU1])
        .output()
        .unwrap();
    assert!(show.status.success());
    let body = String::from_utf8_lossy(&show.stdout);
    assert!(body.contains("work-item-id: work-1"));
    assert!(body.contains("merge-candidate-id: attempt-1-merge-candidate"));
}

/// Run a plain (non-corrective) learner attempt, leaving a verified handoff.
fn run_plain_attempt(main_dir: &Path, tmp: &TempDir, bin_name: &str) -> PathBuf {
    let bin_dir = tmp.path().join(bin_name);
    write_mock_claude(
        &bin_dir,
        &learner_land_mock_script(
            r#"{"learning_summary":"learned","follow_ups":[{"id":"fu-1","summary":"x","corrective":false}]}"#,
        ),
    );
    create_and_run_learner_attempt(main_dir, &bin_dir);
    bin_dir
}

#[test]
fn observation_failure_does_not_undo_land() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    let bin_dir = run_plain_attempt(&main_dir, &tmp, "bin-obs-fail");

    // Claim the deterministic Observation id with a body-only file so the
    // Observation stage cannot write over it.
    let obs_dir = main_dir.join(".fluent/observations");
    fs::create_dir_all(&obs_dir).unwrap();
    fs::write(obs_dir.join(format!("{OBS_FU1}.md")), "a manual note\n").unwrap();

    land_work_1(&main_dir, &bin_dir, true);

    assert!(is_merged(&main_dir), "an Observation failure does not undo the land");
    assert_eq!(follow_up_failure_stage(&main_dir), "observation");
}

#[test]
fn promotion_failure_does_not_undo_land() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    let bin_dir = run_corrective_attempt(&main_dir, &tmp, "bin-promo-fail", true);

    // Claim the derived Work Item's path with unreadable content so the Work
    // stage fails.
    let items_dir = main_dir.join(".fluent/work/items");
    fs::create_dir_all(&items_dir).unwrap();
    fs::write(items_dir.join(format!("{DERIVED_FU1}.json")), "{ not json").unwrap();

    land_work_1(&main_dir, &bin_dir, true);

    assert!(is_merged(&main_dir), "a promotion failure does not undo the land");
    assert_eq!(follow_up_failure_stage(&main_dir), "work");
    // The Observation stage completed before the failure.
    assert!(main_dir.join(format!(".fluent/observations/{OBS_FU1}.md")).exists());
}

#[test]
fn queue_failure_does_not_undo_land() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    let bin_dir = run_corrective_attempt(&main_dir, &tmp, "bin-queue-fail", true);

    // Make the derived Work Item's queue ledger path a directory so the queue
    // stage cannot write it.
    fs::create_dir_all(queue_ledger_path(&main_dir, DERIVED_FU1)).unwrap();

    land_work_1(&main_dir, &bin_dir, true);

    assert!(is_merged(&main_dir), "a queue failure does not undo the land");
    assert_eq!(follow_up_failure_stage(&main_dir), "queue");
    // The Work stage completed: the descendant is authorized.
    assert_eq!(
        work_item_value(&main_dir, DERIVED_FU1)["authorization"]["state"],
        "execution-ready"
    );
}

#[test]
fn followup_retry_resumes_each_partial_failure_exactly_once() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    let bin_dir = run_corrective_attempt(&main_dir, &tmp, "bin-resume", true);

    // First land fails at the Observation stage.
    let obs_path = main_dir.join(format!(".fluent/observations/{OBS_FU1}.md"));
    fs::create_dir_all(obs_path.parent().unwrap()).unwrap();
    fs::write(&obs_path, "a manual note\n").unwrap();
    land_work_1(&main_dir, &bin_dir, true);
    assert_eq!(follow_up_failure_stage(&main_dir), "observation");

    // Clear the obstruction and resume by re-landing the merged candidate. A
    // rebase-failing mock proves the resume never re-merges.
    fs::remove_file(&obs_path).unwrap();
    let rerun = tmp.path().join("bin-resume-norebase");
    write_mock_claude(&rerun, &rebase_failing_mock_script());
    land_work_1(&main_dir, &rerun, true);

    // Each effect is produced exactly once and the failure is cleared.
    assert_eq!(open_observation_files(&main_dir).len(), 1);
    assert_eq!(
        work_item_value(&main_dir, DERIVED_FU1)["authorization"]["state"],
        "execution-ready"
    );
    assert_eq!(dispatch_count(&main_dir, DERIVED_FU1), 1);
    assert_eq!(follow_up_failure_stage(&main_dir), "", "a completed resume clears the failure");
}

#[test]
fn followup_retry_recovers_work_creation_exactly_once() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    let bin_dir = run_corrective_attempt(&main_dir, &tmp, "bin-work-resume", true);
    let derived_path = main_dir
        .join(".fluent/work/items")
        .join(format!("{DERIVED_FU1}.json"));

    fs::create_dir_all(derived_path.parent().unwrap()).unwrap();
    fs::write(&derived_path, "{ not json").unwrap();
    land_work_1(&main_dir, &bin_dir, true);
    assert_eq!(follow_up_failure_stage(&main_dir), "work");
    assert_eq!(open_observation_files(&main_dir).len(), 1);

    fs::remove_file(&derived_path).unwrap();
    let rerun = tmp.path().join("bin-work-resume-norebase");
    write_mock_claude(&rerun, &rebase_failing_mock_script());
    land_work_1(&main_dir, &rerun, true);
    land_work_1(&main_dir, &rerun, true);

    let derived = work_item_value(&main_dir, DERIVED_FU1);
    assert_eq!(open_observation_files(&main_dir).len(), 1);
    assert_eq!(work_item_json_count(&main_dir), 2);
    assert_eq!(derived["lineage"]["charged"], true);
    assert_eq!(dispatch_count(&main_dir, DERIVED_FU1), 1);
    assert_eq!(follow_up_failure_stage(&main_dir), "");
}

#[test]
fn followup_retry_recovers_queue_dispatch_exactly_once() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    let bin_dir = run_corrective_attempt(&main_dir, &tmp, "bin-queue-resume", true);
    let ledger_path = queue_ledger_path(&main_dir, DERIVED_FU1);

    fs::create_dir_all(&ledger_path).unwrap();
    land_work_1(&main_dir, &bin_dir, true);
    assert_eq!(follow_up_failure_stage(&main_dir), "queue");
    assert_eq!(open_observation_files(&main_dir).len(), 1);
    assert_eq!(work_item_json_count(&main_dir), 2);
    assert_eq!(work_item_value(&main_dir, DERIVED_FU1)["lineage"]["charged"], true);

    fs::remove_dir(&ledger_path).unwrap();
    let rerun = tmp.path().join("bin-queue-resume-norebase");
    write_mock_claude(&rerun, &rebase_failing_mock_script());
    land_work_1(&main_dir, &rerun, true);
    land_work_1(&main_dir, &rerun, true);

    assert_eq!(open_observation_files(&main_dir).len(), 1);
    assert_eq!(work_item_json_count(&main_dir), 2);
    assert_eq!(work_item_value(&main_dir, DERIVED_FU1)["lineage"]["charged"], true);
    assert_eq!(dispatch_count(&main_dir, DERIVED_FU1), 1);
    assert_eq!(follow_up_failure_stage(&main_dir), "");
}

#[test]
fn concurrent_followup_materialization_converges_exactly_once() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    let bin_dir = run_corrective_attempt(&main_dir, &tmp, "bin-concurrent", true);

    // First land materializes nothing (Observation stage obstructed).
    let obs_path = main_dir.join(format!(".fluent/observations/{OBS_FU1}.md"));
    fs::create_dir_all(obs_path.parent().unwrap()).unwrap();
    fs::write(&obs_path, "a manual note\n").unwrap();
    land_work_1(&main_dir, &bin_dir, true);
    fs::remove_file(&obs_path).unwrap();

    // Two processors race to materialize the same landed follow-up for the first
    // time; a rebase-failing mock proves neither re-merges.
    let rerun = tmp.path().join("bin-concurrent-norebase");
    write_mock_claude(&rerun, &rebase_failing_mock_script());
    let bin = assert_cmd::cargo::cargo_bin("fluent");
    let handles: Vec<_> = (0..2)
        .map(|_| {
            let bin = bin.clone();
            let main = main_dir.clone();
            let path = mock_path(&rerun);
            std::thread::spawn(move || {
                std::process::Command::new(&bin)
                    .current_dir(&main)
                    .args([
                        "merge-candidate",
                        "land",
                        "work-1",
                        "attempt-1-merge-candidate",
                        "--no-sandbox",
                        "--no-post-merge-review",
                    ])
                    .env("PATH", path)
                    .env("FLUENT_NO_UPDATE_CHECK", "1")
                    .env_remove("FLUENT_TASK_KIND")
                    .output()
                    .unwrap()
            })
        })
        .collect();
    for handle in handles {
        assert!(handle.join().unwrap().status.success());
    }

    // The processors converged on one Observation, one derived Work Item, and
    // one queue entry.
    assert_eq!(open_observation_files(&main_dir).len(), 1);
    assert_eq!(work_item_json_count(&main_dir), 2, "one root plus one derived Work Item");
    assert_eq!(dispatch_count(&main_dir, DERIVED_FU1), 1);
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

    fluent_cmd()
        .current_dir(&main_dir)
        .args([
            "work-item",
            "create",
            "work-1",
            "--title",
            "Fail transcript",
        ])
        .assert()
        .success();
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "create", "work-1", "attempt-1"])
        .assert()
        .success();
    fluent_cmd()
        .current_dir(&main_dir)
        .args([
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

    let transcript =
        main_dir.join(".fluent/work/artifacts/work-1/attempt-1/attempt-1-write-1/transcript.jsonl");
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

    fluent_cmd()
        .current_dir(&main_dir)
        .args(["work-item", "create", "work-1", "--title", "Sandbox test"])
        .assert()
        .success();
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "create", "work-1", "attempt-1"])
        .assert()
        .success();
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["task", "run", "work-1", "attempt-1", "attempt-1-write-1"])
        .env("PATH", mock_path(&bin_dir))
        .env("SANDBOX_PROFILE_LOG", &sandbox_profile_log)
        .env("CLAUDE_CODE_OAUTH_TOKEN", "mock-token")
        .assert()
        .success();

    let artifact_dir = fs::canonicalize(
        main_dir.join(".fluent/work/artifacts/work-1/attempt-1/attempt-1-write-1"),
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
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["review", "work-1", "attempt-1"])
        .assert()
        .success();

    let writer_artifact_dir =
        main_dir.join(".fluent/work/artifacts/work-1/attempt-1/attempt-1-write-1");
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

    fluent_cmd()
        .current_dir(&main_dir)
        .args([
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
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["review", "work-1", "attempt-1"])
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

    fluent_cmd()
        .current_dir(&main_dir)
        .args([
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
        .join(".fluent/work/artifacts/work-1/attempt-1/attempt-1-review-tests/transcript.jsonl");
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
        main_dir.join(".fluent/work/artifacts/work-1/attempt-1/attempt-1-review-tests/review.md");
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
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["review", "work-1", "attempt-1"])
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

    fluent_cmd()
        .current_dir(&main_dir)
        .args([
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
        .join(".fluent/work/artifacts/work-1/attempt-1/attempt-1-review-tests/transcript.jsonl");
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
fn reviewer_sandbox_does_not_include_other_reviewer_artifact_dirs() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    create_completed_work_attempt(&tmp, &main_dir);
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["review", "work-1", "attempt-1"])
        .assert()
        .success();

    // Complete two review tasks so their artifact dirs exist with transcripts
    let review_tests_artifact =
        main_dir.join(".fluent/work/artifacts/work-1/attempt-1/attempt-1-review-tests");
    let review_documentation_artifact =
        main_dir.join(".fluent/work/artifacts/work-1/attempt-1/attempt-1-review-documentation");
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

    fluent_cmd()
        .current_dir(&main_dir)
        .args([
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
fn work_create_persists_instructions_and_attempt_copies_them_to_write_task() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    let instructions_path = tmp.path().join("instructions.md");
    fs::write(
        &instructions_path,
        "Brief: implement durable task instructions.\n\n- Keep extra args as flags.\n",
    )
    .unwrap();

    fluent_cmd()
        .current_dir(&main_dir)
        .args([
            "work-item",
            "create",
            "work-1",
            "--title",
            "Instruction contract",
            "--instructions-file",
            &instructions_path.to_string_lossy(),
        ])
        .assert()
        .success();
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "create", "work-1", "attempt-1"])
        .assert()
        .success();

    let output = fluent_cmd()
        .current_dir(&main_dir)
        .args(["work-item", "show", "work-1"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "work-item show failed: stdout={} stderr={}",
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

    fluent_cmd()
        .current_dir(&main_dir)
        .args([
            "work-item",
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
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "create", "work-1", "attempt-1"])
        .assert()
        .success();

    let output = fluent_cmd()
        .current_dir(&main_dir)
        .args(["work-item", "show", "work-1"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "work-item show failed: stdout={} stderr={}",
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

    fluent_cmd()
        .current_dir(&main_dir)
        .args([
            "work-item",
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
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "create", "work-1", "attempt-1"])
        .assert()
        .success();

    let output = fluent_cmd()
        .current_dir(&main_dir)
        .args(["work-item", "show", "work-1"])
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
fn work_review_plans_review_tasks_for_completed_attempt() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    create_completed_work_attempt(&tmp, &main_dir);

    fluent_cmd()
        .current_dir(&main_dir)
        .args(["review", "work-1", "attempt-1"])
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
        ".fluent/work/artifacts/work-1/attempt-1/attempt-1-review-tests"
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
    fluent_cmd()
        .current_dir(&main_dir)
        .args([
            "work-item",
            "create",
            "work-1",
            "--title",
            "Review too early",
        ])
        .assert()
        .success();
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "create", "work-1", "attempt-1"])
        .assert()
        .success();
    let item_path = main_dir.join(".fluent/work/items/work-1.json");
    let before = fs::read_to_string(&item_path).unwrap();

    fluent_cmd()
        .current_dir(&main_dir)
        .args(["review", "work-1", "attempt-1"])
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
    fluent_cmd()
        .current_dir(&main_dir)
        .args([
            "work-item",
            "create",
            "work-1",
            "--title",
            "Review codebase",
            "--brief-file",
            &brief_path.to_string_lossy(),
        ])
        .assert()
        .success();

    fluent_cmd()
        .current_dir(&main_dir)
        .args([
            "review",
            "codebase",
            "work-1",
            "attempt-review",
            "--from-working-tree",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Created review-only Attempt attempt-review against source checkout with 5 task(s)",
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
        ".fluent/work/artifacts/work-1/attempt-review/attempt-review-review-tests"
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
fn work_review_codebase_default_creates_worktree_attempt_with_tester() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    fluent_cmd()
        .current_dir(&main_dir)
        .args([
            "work-item",
            "create",
            "work-1",
            "--title",
            "Review codebase",
        ])
        .assert()
        .success();

    fluent_cmd()
        .current_dir(&main_dir)
        .args(["review", "codebase", "work-1", "attempt-review"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Created review-only Attempt attempt-review against per-branch worktree with 6 task(s)",
        ))
        .stdout(predicate::str::contains("attempt-review-tester"));

    let value = read_work_show_json(&main_dir, "work-1");
    let attempt = &value["attempts"][0];
    assert_eq!(attempt["kind"], "review-only");
    let tasks = attempt["tasks"].as_array().unwrap();
    assert_eq!(tasks.len(), 1 + review::REVIEWERS.len());
    assert_eq!(tasks[0]["kind"], "tester");
    assert_eq!(tasks[0]["id"], "attempt-review-tester");
    assert_eq!(
        tasks[0]["workspace_access"]["reads"][0]["path"],
        "../work-review-main"
    );
    for task in tasks.iter().skip(1) {
        assert_eq!(task["kind"], "review");
        assert_eq!(
            task["workspace_access"]["reads"][0]["path"],
            "../work-review-main"
        );
        assert_eq!(task["depends_on"], "attempt-review-tester");
    }
}

#[test]
fn work_review_codebase_missing_or_duplicate_leaves_state_unchanged() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    fluent_cmd()
        .current_dir(&main_dir)
        .args([
            "work-item",
            "create",
            "work-1",
            "--title",
            "Review codebase",
        ])
        .assert()
        .success();

    fluent_cmd()
        .current_dir(&main_dir)
        .args([
            "review",
            "codebase",
            "work-1",
            "attempt-review",
            "--from-working-tree",
        ])
        .assert()
        .success();
    let item_path = main_dir.join(".fluent/work/items/work-1.json");
    let before = fs::read_to_string(&item_path).unwrap();

    fluent_cmd()
        .current_dir(&main_dir)
        .args(["review", "codebase", "missing-work", "attempt-review"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "Work Item \"missing-work\" not found",
        ));
    fluent_cmd()
        .current_dir(&main_dir)
        .args([
            "review",
            "codebase",
            "work-1",
            "attempt-review",
            "--from-working-tree",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "Attempt \"attempt-review\" already exists",
        ));

    assert_eq!(fs::read_to_string(item_path).unwrap(), before);
    assert!(
        !main_dir
            .join(".fluent/work/items/missing-work.json")
            .exists()
    );
}

#[test]
fn work_task_run_review_materializes_bundled_skill_without_project_skills() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    fs::remove_dir_all(main_dir.join("skills")).unwrap();
    git::run(&main_dir, &["add", "."], "stage skill removal").unwrap();
    git::run(
        &main_dir,
        &["commit", "-m", "drop project-local skills"],
        "commit skill removal",
    )
    .unwrap();
    fluent_cmd()
        .current_dir(&main_dir)
        .args([
            "work-item",
            "create",
            "work-1",
            "--title",
            "Review codebase",
        ])
        .assert()
        .success();
    fluent_cmd()
        .current_dir(&main_dir)
        .args([
            "review",
            "codebase",
            "work-1",
            "attempt-review",
            "--from-working-tree",
        ])
        .assert()
        .success();

    let bin_dir = tmp.path().join("bin-review-only-bundled-skill");
    write_mock_claude(&bin_dir, &review_only_mock_script("pass"));

    // Review succeeds because the binary carries bundled skills and
    // materializes them to .fluent/work/skills/ on demand.
    fluent_cmd()
        .current_dir(&main_dir)
        .args([
            "task",
            "run",
            "work-1",
            "attempt-review",
            "attempt-review-review-tests",
            "--no-sandbox",
        ])
        .env("PATH", mock_path(&bin_dir))
        .assert()
        .success();

    let materialized = main_dir.join(".fluent/work/skills/review-tests/SKILL.md");
    assert!(
        materialized.is_file(),
        "bundled review skill should be materialized at {}",
        materialized.display()
    );
}

#[test]
fn work_task_run_completes_attempt_after_all_review_tasks_complete() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    create_completed_work_attempt(&tmp, &main_dir);
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["review", "work-1", "attempt-1"])
        .assert()
        .success();

    let bin_dir = tmp.path().join("bin-review");
    write_mock_claude(
        &bin_dir,
        "#!/bin/bash\nprintf 'Verdict: pass\\n' > review.md\nexit 0\n",
    );

    // The tester task must run before reviewers to complete the lifecycle.
    // Without tester.yaml it produces an error-result file but still marks
    // the task complete, which is enough to satisfy the attempt loop.
    fluent_cmd()
        .current_dir(&main_dir)
        .args([
            "task",
            "run",
            "work-1",
            "attempt-1",
            "attempt-1-tester",
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
        fluent_cmd()
            .current_dir(&main_dir)
            .args([
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
                    ".fluent/work/artifacts/work-1/attempt-1/attempt-1-review-{role}/review.md"
                ))
                .exists()
        );
    }
}

#[test]
fn work_attempt_run_drives_write_reviews_and_passes() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["work-item", "create", "work-1", "--title", "Attempt loop"])
        .assert()
        .success();
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "create", "work-1", "attempt-1"])
        .assert()
        .success();

    let bin_dir = tmp.path().join("bin-loop-pass");
    write_mock_claude(&bin_dir, &loop_mock_script("pass"));

    fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "run", "work-1", "attempt-1", "--no-sandbox"])
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
    assert_eq!(candidate["merge_review_state"], "pending");
    assert!(
        main_dir
            .join(".fluent/work/artifacts/work-1/attempt-1/attempt-1-review-tests/review.md")
            .exists()
    );

    let inspection = fluent_cmd()
        .current_dir(&main_dir)
        .args([
            "merge-candidate",
            "show",
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
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "run", "work-1", "attempt-1", "--no-sandbox"])
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
    fluent_cmd()
        .current_dir(&main_dir)
        .args([
            "work-item",
            "create",
            "work-1",
            "--title",
            "Review codebase",
        ])
        .assert()
        .success();
    fluent_cmd()
        .current_dir(&main_dir)
        .args([
            "review",
            "codebase",
            "work-1",
            "attempt-review",
            "--from-working-tree",
        ])
        .assert()
        .success();

    let main_head = git_head(&main_dir);
    let bin_dir = tmp.path().join("bin-review-only-pass");
    write_mock_claude(&bin_dir, &review_only_mock_script("pass"));

    fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "run", "work-1", "attempt-review", "--no-sandbox"])
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
    assert_no_non_fluent_changes(&main_dir);
}

#[test]
fn work_attempt_run_review_only_rejects_source_changes() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    fluent_cmd()
        .current_dir(&main_dir)
        .args([
            "work-item",
            "create",
            "work-1",
            "--title",
            "Review codebase",
        ])
        .assert()
        .success();
    fluent_cmd()
        .current_dir(&main_dir)
        .args([
            "review",
            "codebase",
            "work-1",
            "attempt-review",
            "--from-working-tree",
        ])
        .assert()
        .success();

    let main_head = git_head(&main_dir);
    let bin_dir = tmp.path().join("bin-review-only-dirty");
    write_mock_claude(&bin_dir, &review_only_dirty_source_mock_script());

    fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "run", "work-1", "attempt-review", "--no-sandbox"])
        .env("PATH", mock_path(&bin_dir))
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "Review Task changed non-Fluent source files",
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
    assert_no_non_fluent_changes(&main_dir);
}

#[test]
fn work_attempt_run_review_only_restores_changed_source_head() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    fluent_cmd()
        .current_dir(&main_dir)
        .args([
            "work-item",
            "create",
            "work-1",
            "--title",
            "Review codebase",
        ])
        .assert()
        .success();
    fluent_cmd()
        .current_dir(&main_dir)
        .args([
            "review",
            "codebase",
            "work-1",
            "attempt-review",
            "--from-working-tree",
        ])
        .assert()
        .success();

    let main_head = git_head(&main_dir);
    let bin_dir = tmp.path().join("bin-review-only-head");
    write_mock_claude(&bin_dir, &review_only_changed_head_mock_script());

    fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "run", "work-1", "attempt-review", "--no-sandbox"])
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
    assert_no_non_fluent_changes(&main_dir);
}

#[test]
fn work_attempt_run_review_only_requires_recorded_source_commit() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    fluent_cmd()
        .current_dir(&main_dir)
        .args([
            "work-item",
            "create",
            "work-1",
            "--title",
            "Review codebase",
        ])
        .assert()
        .success();
    fluent_cmd()
        .current_dir(&main_dir)
        .args([
            "review",
            "codebase",
            "work-1",
            "attempt-review",
            "--from-working-tree",
        ])
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

    fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "run", "work-1", "attempt-review", "--no-sandbox"])
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
fn work_attempt_run_review_only_rejects_fluent_state_changes() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    fs::create_dir_all(main_dir.join(".fluent/expertise")).unwrap();
    fs::write(
        main_dir.join(".fluent/expertise/decisions.md"),
        "# Decisions\n\n",
    )
    .unwrap();
    git::run(
        &main_dir,
        &["add", ".fluent/expertise/decisions.md"],
        "stage decisions",
    )
    .unwrap();
    git::run(
        &main_dir,
        &["commit", "-m", "record decisions"],
        "commit decisions",
    )
    .unwrap();

    fluent_cmd()
        .current_dir(&main_dir)
        .args([
            "work-item",
            "create",
            "work-1",
            "--title",
            "Review codebase",
        ])
        .assert()
        .success();
    fluent_cmd()
        .current_dir(&main_dir)
        .args([
            "review",
            "codebase",
            "work-1",
            "attempt-review",
            "--from-working-tree",
        ])
        .assert()
        .success();

    let bin_dir = tmp.path().join("bin-review-only-fluent-dirty");
    write_mock_claude(&bin_dir, &review_only_dirty_fluent_mock_script());

    fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "run", "work-1", "attempt-review", "--no-sandbox"])
        .env("PATH", mock_path(&bin_dir))
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "changed source checkout outside managed artifact area",
        ))
        .stderr(predicate::str::contains(".fluent/expertise/decisions.md"))
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
    fluent_cmd()
        .current_dir(&main_dir)
        .args([
            "work-item",
            "create",
            "work-1",
            "--title",
            "Review codebase",
        ])
        .assert()
        .success();
    fluent_cmd()
        .current_dir(&main_dir)
        .args([
            "review",
            "codebase",
            "work-1",
            "attempt-review",
            "--from-working-tree",
        ])
        .assert()
        .success();

    let bin_dir = tmp.path().join("bin-review-only-work-state-dirty");
    write_mock_claude(&bin_dir, &review_only_dirty_work_state_mock_script());

    fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "run", "work-1", "attempt-review", "--no-sandbox"])
        .env("PATH", mock_path(&bin_dir))
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "changed source checkout outside managed artifact area",
        ))
        .stderr(predicate::str::contains(".fluent/work/items/work-1.json"))
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
        !fs::read_to_string(main_dir.join(".fluent/work/items/work-1.json"))
            .unwrap()
            .contains("reviewer edit")
    );
}

#[test]
fn work_attempt_run_review_only_restores_mixed_source_and_fluent_changes() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    fs::create_dir_all(main_dir.join(".fluent/expertise")).unwrap();
    fs::write(
        main_dir.join(".fluent/expertise/decisions.md"),
        "# Decisions\n\n",
    )
    .unwrap();
    git::run(
        &main_dir,
        &["add", ".fluent/expertise/decisions.md"],
        "stage decisions",
    )
    .unwrap();
    git::run(
        &main_dir,
        &["commit", "-m", "record decisions"],
        "commit decisions",
    )
    .unwrap();

    fluent_cmd()
        .current_dir(&main_dir)
        .args([
            "work-item",
            "create",
            "work-1",
            "--title",
            "Review codebase",
        ])
        .assert()
        .success();
    fluent_cmd()
        .current_dir(&main_dir)
        .args([
            "review",
            "codebase",
            "work-1",
            "attempt-review",
            "--from-working-tree",
        ])
        .assert()
        .success();

    let bin_dir = tmp.path().join("bin-review-only-mixed-dirty");
    write_mock_claude(&bin_dir, &review_only_dirty_source_and_fluent_mock_script());

    fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "run", "work-1", "attempt-review", "--no-sandbox"])
        .env("PATH", mock_path(&bin_dir))
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "changed source checkout outside managed artifact area",
        ))
        .stderr(predicate::str::contains(".fluent/expertise/decisions.md"))
        .stdout(predicate::str::contains("Merge Candidate").not())
        .stdout(predicate::str::contains("follow-up").not());

    assert_eq!(
        fs::read_to_string(main_dir.join("README.md")).unwrap(),
        "test"
    );
    assert_eq!(
        fs::read_to_string(main_dir.join(".fluent/expertise/decisions.md")).unwrap(),
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
    fluent_cmd()
        .current_dir(&main_dir)
        .args([
            "work-item",
            "create",
            "work-1",
            "--title",
            "Review codebase",
        ])
        .assert()
        .success();
    fluent_cmd()
        .current_dir(&main_dir)
        .args([
            "review",
            "codebase",
            "work-1",
            "attempt-review",
            "--from-working-tree",
        ])
        .assert()
        .success();

    let bin_dir = tmp.path().join("bin-review-only-fail");
    write_mock_claude(&bin_dir, &review_only_mock_script("fail"));

    fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "run", "work-1", "attempt-review", "--no-sandbox"])
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
    assert_no_non_fluent_changes(&main_dir);
}

#[test]
fn work_attempt_run_review_only_uncertain_needs_user() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    fluent_cmd()
        .current_dir(&main_dir)
        .args([
            "work-item",
            "create",
            "work-1",
            "--title",
            "Review codebase",
        ])
        .assert()
        .success();
    fluent_cmd()
        .current_dir(&main_dir)
        .args([
            "review",
            "codebase",
            "work-1",
            "attempt-review",
            "--from-working-tree",
        ])
        .assert()
        .success();

    let bin_dir = tmp.path().join("bin-review-only-uncertain");
    write_mock_claude(&bin_dir, &review_only_mock_script("uncertain"));

    fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "run", "work-1", "attempt-review", "--no-sandbox"])
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
                artifact["path"] == ".fluent/work/artifacts/work-1/attempt-review/needs-user.md"
            })
    );
    assert_eq!(review_only_write_task_count(attempt), 0);
    assert!(merge_candidates_are_empty(&value));
    assert!(
        main_dir
            .join(".fluent/work/artifacts/work-1/attempt-review/needs-user.md")
            .is_file()
    );
    assert_no_non_fluent_changes(&main_dir);
}

#[test]
fn work_merge_candidate_failed_check_leaves_target_unchanged() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    fluent_cmd()
        .current_dir(&main_dir)
        .args([
            "work-item",
            "create",
            "work-1",
            "--title",
            "Merge check failure",
        ])
        .assert()
        .success();
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "create", "work-1", "attempt-1"])
        .assert()
        .success();

    let bin_dir = tmp.path().join("bin-check-fail");
    write_mock_claude(&bin_dir, &rebase_mock_script("pass"));
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "run", "work-1", "attempt-1", "--no-sandbox"])
        .env("PATH", mock_path(&bin_dir))
        .assert()
        .success();
    write_executable_hook(
        &main_dir,
        "check-pre-merge",
        "#!/bin/sh\nprintf check-failed >&2\nexit 1\n",
    );

    let main_before = git_head(&main_dir);
    fluent_cmd()
        .current_dir(&main_dir)
        .args([
            "merge-candidate",
            "land",
            "work-1",
            "attempt-1-merge-candidate",
            "--no-sandbox",
        ])
        .env("PATH", mock_path(&bin_dir))
        .assert()
        .failure()
        .stderr(predicate::str::contains("check-pre-merge failed (exit 1)"));

    // Target (main) must be unchanged; the candidate may have been rebased
    // before the check ran, which is expected and not a failure.
    assert_eq!(git_head(&main_dir), main_before);
    let value = read_work_show_json(&main_dir, "work-1");
    let candidate = &value["merge_candidates"][0];
    assert_eq!(candidate["merge_review_state"], "pending");
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
    fluent_cmd()
        .current_dir(&main_dir)
        .args([
            "work-item",
            "create",
            "work-1",
            "--title",
            "Cleanup warning",
        ])
        .assert()
        .success();
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "create", "work-1", "attempt-1"])
        .assert()
        .success();

    let bin_dir = tmp.path().join("bin-cleanup-warning-pass");
    write_mock_claude(&bin_dir, &rebase_mock_script("pass"));
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "run", "work-1", "attempt-1", "--no-sandbox"])
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

    fluent_cmd()
        .current_dir(&main_dir)
        .args([
            "merge-candidate",
            "land",
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
    assert_eq!(candidate["merge_review_state"], "passed");
    assert_eq!(candidate["merge_state"]["status"], "merged");
    assert_eq!(candidate["merge_state"]["merged_commit"], candidate_head);
}

#[test]
fn work_merge_candidate_rerun_after_cleanup_preserves_landed_state() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["work-item", "create", "work-1", "--title", "Cleanup rerun"])
        .assert()
        .success();
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "create", "work-1", "attempt-1"])
        .assert()
        .success();

    let bin_dir = tmp.path().join("bin-cleanup-rerun-pass");
    write_mock_claude(&bin_dir, &rebase_mock_script("pass"));
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "run", "work-1", "attempt-1", "--no-sandbox"])
        .env("PATH", mock_path(&bin_dir))
        .assert()
        .success();

    let candidate_workspace = main_dir.join("../work-6-work-1-attempt-1");

    fluent_cmd()
        .current_dir(&main_dir)
        .args([
            "merge-candidate",
            "land",
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

    // The merge rebases the candidate onto main before fast-forwarding, so
    // main's new HEAD is the rebased candidate head. Capture it now to
    // verify a rerun preserves this landed state instead of re-merging.
    let candidate_head = git_head(&main_dir);
    assert!(main_dir.join("loop-output.txt").is_file());
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

    fluent_cmd()
        .current_dir(&main_dir)
        .args([
            "merge-candidate",
            "land",
            "work-1",
            "attempt-1-merge-candidate",
            "--no-sandbox",
        ])
        .env("PATH", mock_path(&fail_bin))
        .assert()
        .success()
        .stderr(predicate::str::contains("should-not-run").not())
        .stderr(predicate::str::contains("reviewer should not rerun").not());

    assert!(!candidate_workspace.exists());
    let value = read_work_show_json(&main_dir, "work-1");
    let candidate = &value["merge_candidates"][0];
    assert_eq!(candidate["merge_review_state"], "passed");
    assert_eq!(candidate["merge_state"]["status"], "merged");
    // The merged_commit captured in state matches the HEAD landed on first
    // merge; a rerun must preserve it.
    assert_eq!(candidate["merge_state"]["merged_commit"], candidate_head);
}

#[test]
fn work_merge_candidate_rejects_stale_stored_provenance_without_rewrite() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    fluent_cmd()
        .current_dir(&main_dir)
        .args([
            "work-item",
            "create",
            "work-1",
            "--title",
            "Stale candidate",
        ])
        .assert()
        .success();
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "create", "work-1", "attempt-1"])
        .assert()
        .success();

    let bin_dir = tmp.path().join("bin-stale-provenance");
    write_mock_claude(&bin_dir, &rebase_mock_script("pass"));
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "run", "work-1", "attempt-1", "--no-sandbox"])
        .env("PATH", mock_path(&bin_dir))
        .assert()
        .success();

    let candidate_path =
        main_dir.join(".fluent/work/merge-candidates/work-1/attempt-1-merge-candidate.json");
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

    fluent_cmd()
        .current_dir(&main_dir)
        .args([
            "merge-candidate",
            "land",
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
    fluent_cmd()
        .current_dir(&main_dir)
        .args([
            "work-item",
            "create",
            "work-1",
            "--title",
            "Rebase candidate",
        ])
        .assert()
        .success();
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "create", "work-1", "attempt-1"])
        .assert()
        .success();

    let bin_dir = tmp.path().join("bin-rebase-pass");
    write_mock_claude(&bin_dir, &rebase_mock_script("pass"));
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "run", "work-1", "attempt-1", "--no-sandbox"])
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

    fluent_cmd()
        .current_dir(&main_dir)
        .args([
            "merge-candidate",
            "land",
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
    fluent_cmd()
        .current_dir(&main_dir)
        .args([
            "work-item",
            "create",
            "work-1",
            "--title",
            "Rebase conflict",
        ])
        .assert()
        .success();
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "create", "work-1", "attempt-1"])
        .assert()
        .success();

    let bin_dir = tmp.path().join("bin-rebase-conflict");
    write_mock_claude(&bin_dir, &rebase_give_up_mock_script());
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "run", "work-1", "attempt-1", "--no-sandbox"])
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

    fluent_cmd()
        .current_dir(&main_dir)
        .args([
            "merge-candidate",
            "land",
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
    fluent_cmd()
        .current_dir(&main_dir)
        .args([
            "work-item",
            "create",
            "work-1",
            "--title",
            "Trivial conflict",
        ])
        .assert()
        .success();
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "create", "work-1", "attempt-1"])
        .assert()
        .success();

    let bin_dir = tmp.path().join("bin-rebase-resolve");
    write_mock_claude(&bin_dir, &rebase_conflict_resolve_mock_script());
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "run", "work-1", "attempt-1", "--no-sandbox"])
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

    fluent_cmd()
        .current_dir(&main_dir)
        .args([
            "merge-candidate",
            "land",
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
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["work-item", "create", "work-1", "--title", "Give up"])
        .assert()
        .success();
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "create", "work-1", "attempt-1"])
        .assert()
        .success();

    let bin_dir = tmp.path().join("bin-rebase-giveup");
    write_mock_claude(&bin_dir, &rebase_give_up_mock_script());
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "run", "work-1", "attempt-1", "--no-sandbox"])
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

    fluent_cmd()
        .current_dir(&main_dir)
        .args([
            "merge-candidate",
            "land",
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
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["work-item", "create", "work-1", "--title", "Agent crash"])
        .assert()
        .success();
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "create", "work-1", "attempt-1"])
        .assert()
        .success();

    let bin_dir = tmp.path().join("bin-rebase-crash");
    write_mock_claude(&bin_dir, &rebase_crash_mock_script());
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "run", "work-1", "attempt-1", "--no-sandbox"])
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

    fluent_cmd()
        .current_dir(&main_dir)
        .args([
            "merge-candidate",
            "land",
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
    fluent_cmd()
        .current_dir(&main_dir)
        .args([
            "work-item",
            "create",
            "work-1",
            "--title",
            "Provenance update",
        ])
        .assert()
        .success();
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "create", "work-1", "attempt-1"])
        .assert()
        .success();

    let bin_dir = tmp.path().join("bin-rebase-prov");
    write_mock_claude(&bin_dir, &rebase_mock_script("pass"));
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "run", "work-1", "attempt-1", "--no-sandbox"])
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

    fluent_cmd()
        .current_dir(&main_dir)
        .args([
            "merge-candidate",
            "land",
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
fn work_merge_candidate_land_no_post_merge_review_skips_queue_entry() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["work-item", "create", "work-1", "--title", "Quiet land"])
        .assert()
        .success();
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "create", "work-1", "attempt-1"])
        .assert()
        .success();

    let bin_dir = tmp.path().join("bin-quiet-land");
    write_mock_claude(&bin_dir, &rebase_mock_script("pass"));
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "run", "work-1", "attempt-1", "--no-sandbox"])
        .env("PATH", mock_path(&bin_dir))
        .assert()
        .success();

    fluent_cmd()
        .current_dir(&main_dir)
        .args([
            "merge-candidate",
            "land",
            "work-1",
            "attempt-1-merge-candidate",
            "--no-sandbox",
            "--no-post-merge-review",
        ])
        .env("PATH", mock_path(&bin_dir))
        .assert()
        .success();

    let value = read_work_show_json(&main_dir, "work-1");
    let candidate = &value["merge_candidates"][0];
    assert_eq!(candidate["merge_state"]["status"], "merged");

    let queue_path = main_dir.join(".fluent/work/post-merge-review-queue.json");
    if queue_path.exists() {
        let queue: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&queue_path).unwrap()).unwrap();
        let entries = queue["entries"].as_array().expect("queue entries array");
        assert!(
            entries.is_empty(),
            "post-merge review queue must have no entries with --no-post-merge-review"
        );
    }
}

#[test]
fn work_merge_candidate_land_default_enqueues_post_merge_review() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["work-item", "create", "work-1", "--title", "Default land"])
        .assert()
        .success();
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "create", "work-1", "attempt-1"])
        .assert()
        .success();

    let bin_dir = tmp.path().join("bin-default-land");
    write_mock_claude(&bin_dir, &rebase_mock_script("pass"));
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "run", "work-1", "attempt-1", "--no-sandbox"])
        .env("PATH", mock_path(&bin_dir))
        .assert()
        .success();

    fluent_cmd()
        .current_dir(&main_dir)
        .args([
            "merge-candidate",
            "land",
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

    let queue_path = main_dir.join(".fluent/work/post-merge-review-queue.json");
    let queue: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&queue_path).unwrap()).unwrap();
    let entries = queue["entries"].as_array().expect("queue entries array");
    assert_eq!(
        entries.len(),
        1,
        "default land must append exactly one post-merge review queue entry"
    );
    assert_eq!(entries[0]["source_work_item_id"], "work-1");
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

    fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "run", "work-1", "attempt-1", "--no-sandbox"])
        .env("PATH", mock_path(&bin_dir))
        .env("FLUENT_MAX_TOTAL_WRITE_ROUNDS", "3")
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
        ".fluent/work/artifacts/work-1/attempt-1/attempt-1-review-documentation/review.md"
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
        fs::read_to_string(main_dir.join(".fluent/work/artifacts/work-1/attempt-1/needs-user.md"))
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

    fluent_cmd()
        .current_dir(&main_dir)
        .args([
            "work-item",
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
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "create", "work-1", "attempt-1"])
        .assert()
        .success();
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "run", "work-1", "attempt-1", "--no-sandbox"])
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

    fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "run", "work-1", "attempt-1", "--no-sandbox"])
        .env("PATH", mock_path(&bin_dir))
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Planned write Task attempt-1-write-2",
        ))
        .stdout(predicate::str::contains("Completed Task attempt-1-write-2"))
        .stdout(predicate::str::contains(
            "Planned 2 review Tasks for Attempt attempt-1",
        ))
        .stdout(predicate::str::contains("attempt-1-tester-2"))
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
            .join(".fluent/work/artifacts/work-1/attempt-1/needs-user.md")
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
        ".fluent/work/artifacts/work-1/attempt-1/attempt-1-review-documentation/review.md"
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
    assert_eq!(second_round_inputs.len(), 3);
    assert_eq!(
        second_round_inputs[0]["path"],
        ".fluent/work/artifacts/work-1/attempt-1/attempt-1-review-documentation/review.md"
    );
    assert_eq!(
        second_round_inputs[0]["producer_id"],
        "attempt-1-review-documentation"
    );
    assert_eq!(
        second_round_inputs[1]["path"],
        ".fluent/work/artifacts/work-1/attempt-1/attempt-1-tester-2/tester-results.json"
    );
    assert_eq!(second_round_inputs[1]["producer_id"], "attempt-1-tester-2");
    assert_eq!(
        second_round_inputs[2]["path"],
        ".fluent/work/progress/work-1/attempt-1/progress.md"
    );
    assert_eq!(second_round_inputs[2]["producer_id"], "writer");
}

#[test]
fn work_attempt_run_counts_already_planned_followup_against_budget() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    create_completed_work_attempt(&tmp, &main_dir);

    write_planned_followup_task(&main_dir, Vec::new());

    let bin_dir = tmp.path().join("bin-loop-preplanned-followup");
    write_mock_claude(&bin_dir, &stateful_loop_mock_script("fail"));
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "run", "work-1", "attempt-1", "--no-sandbox"])
        .env("PATH", mock_path(&bin_dir))
        .env("FLUENT_MAX_TOTAL_WRITE_ROUNDS", "3")
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
        fs::read_to_string(main_dir.join(".fluent/work/artifacts/work-1/attempt-1/needs-user.md"))
            .unwrap();
    assert!(handoff.contains("write-round ceiling"));
    assert!(handoff.contains("attempt-1-review-2-tests/review.md"));
}

#[test]
fn work_attempt_run_rejects_unmanaged_completed_review_artifact_area_path() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    create_completed_work_attempt(&tmp, &main_dir);
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["review", "work-1", "attempt-1"])
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
        fluent_cmd()
            .current_dir(&main_dir)
            .args([
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

    fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "run", "work-1", "attempt-1", "--no-sandbox"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("Task artifact area path must"));

    assert_json_unchanged(&task_path, &before);
}

#[test]
fn work_attempt_run_marks_uncertain_reviews_needs_user() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    create_completed_work_attempt(&tmp, &main_dir);

    let bin_dir = tmp.path().join("bin-loop-uncertain");
    write_mock_claude(&bin_dir, &loop_mock_script("uncertain"));

    fluent_cmd()
        .current_dir(&main_dir)
        .args([
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
            "Attempt attempt-1 needs user input: .fluent/work/artifacts/work-1/attempt-1/needs-user.md",
        ));

    let value = read_work_show_json(&main_dir, "work-1");
    let attempt = &value["attempts"][0];
    assert_eq!(attempt["status"], "needs-user");
    assert_eq!(attempt["review_state"], "uncertain");
    let handoff =
        fs::read_to_string(main_dir.join(".fluent/work/artifacts/work-1/attempt-1/needs-user.md"))
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

    fluent_cmd()
        .current_dir(&main_dir)
        .args([
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
            "Attempt attempt-1 needs user input: .fluent/work/artifacts/work-1/attempt-1/needs-user.md",
        ));

    let value = read_work_show_json(&main_dir, "work-1");
    let attempt = &value["attempts"][0];
    assert_eq!(attempt["status"], "needs-user");
    assert_eq!(attempt["review_state"], "uncertain");
    let handoff =
        fs::read_to_string(main_dir.join(".fluent/work/artifacts/work-1/attempt-1/needs-user.md"))
            .unwrap();
    assert!(handoff.contains("uncertain or missing review verdicts"));
    assert!(handoff.contains("attempt-1-review-tests/review.md"));
}

#[test]
fn work_attempt_run_stops_when_task_executor_fails() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["work-item", "create", "work-1", "--title", "Attempt loop"])
        .assert()
        .success();
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "create", "work-1", "attempt-1"])
        .assert()
        .success();

    let bin_dir = tmp.path().join("bin-loop-failure");
    write_mock_claude(&bin_dir, "#!/bin/bash\nexit 7\n");

    fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "run", "work-1", "attempt-1", "--no-sandbox"])
        .env("PATH", mock_path(&bin_dir))
        .env("FLUENT_MAX_TASK_RETRIES", "0")
        .assert()
        .failure()
        .stderr(predicate::str::contains("Coder exited with code 7"));

    let value = read_work_show_json(&main_dir, "work-1");
    let attempt = &value["attempts"][0];
    assert_eq!(attempt["status"], "needs-user");
    assert_eq!(attempt["tasks"][0]["status"], "failed");
    assert_eq!(attempt["tasks"].as_array().unwrap().len(), 1);
}

#[test]
fn work_task_run_rejects_unmanaged_review_read_workspace_path() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    create_completed_work_attempt(&tmp, &main_dir);
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["review", "work-1", "attempt-1"])
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

        fluent_cmd()
            .current_dir(&main_dir)
            .args([
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

        assert_json_unchanged(&task_path, &before);
    }
    assert!(
        !main_dir
            .join(".fluent/work/artifacts/work-1/attempt-1/attempt-1-review-tests")
            .exists()
    );
}

#[test]
fn work_task_run_rejects_malformed_review_context() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    create_completed_work_attempt(&tmp, &main_dir);
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["review", "work-1", "attempt-1"])
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

        fluent_cmd()
            .current_dir(&main_dir)
            .args([
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

        assert_json_unchanged(&task_path, &before);
    }
    assert!(
        !main_dir
            .join(".fluent/work/artifacts/work-1/attempt-1/attempt-1-review-tests")
            .exists()
    );
}

#[test]
fn work_task_run_fails_review_task_without_artifact() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    create_completed_work_attempt(&tmp, &main_dir);
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["review", "work-1", "attempt-1"])
        .assert()
        .success();
    let bin_dir = tmp.path().join("bin-review");
    write_mock_claude(&bin_dir, "#!/bin/bash\nexit 0\n");

    fluent_cmd()
        .current_dir(&main_dir)
        .args([
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
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["review", "work-1", "attempt-1"])
        .assert()
        .success();

    let review_dir =
        main_dir.join(".fluent/work/artifacts/work-1/attempt-1/attempt-1-review-tests");
    let review_path = review_dir.join("review.md");
    fs::create_dir_all(&review_dir).unwrap();
    fs::write(&review_path, "Verdict: pass\n\nstale\n").unwrap();

    let bin_dir = tmp.path().join("bin-review");
    write_mock_claude(&bin_dir, "#!/bin/bash\nexit 0\n");

    fluent_cmd()
        .current_dir(&main_dir)
        .args([
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
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["review", "work-1", "attempt-1"])
        .assert()
        .success();

    let task_path =
        work_task_record_path(&main_dir, "work-1", "attempt-1", "attempt-1-review-tests");
    let planned = fs::read_to_string(&task_path).unwrap();
    let outside_absolute = tmp.path().join("outside-review-absolute");
    let outside_absolute = outside_absolute.to_string_lossy().to_string();
    for path in [
        "../outside-review-artifacts",
        ".fluent/work/artifacts",
        ".fluent/work/artifacts/../outside-review-artifacts",
        outside_absolute.as_str(),
    ] {
        let mut value: serde_json::Value = serde_json::from_str(&planned).unwrap();
        value["artifact_area"]["path"] = serde_json::Value::String(path.to_string());
        write_json_value(&task_path, &value);
        let before = fs::read_to_string(&task_path).unwrap();

        fluent_cmd()
            .current_dir(&main_dir)
            .args([
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

        assert_json_unchanged(&task_path, &before);
    }

    assert!(!main_dir.join("../outside-review-artifacts").exists());
    assert!(
        !main_dir
            .join(".fluent/work/outside-review-artifacts")
            .exists()
    );
    assert!(!Path::new(&outside_absolute).exists());
}

#[test]
fn work_task_run_pauses_attempt_when_review_coder_exits_nonzero() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    create_completed_work_attempt(&tmp, &main_dir);
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["review", "work-1", "attempt-1"])
        .assert()
        .success();

    let bin_dir = tmp.path().join("bin-review");
    write_mock_claude(&bin_dir, "#!/bin/bash\nexit 7\n");

    fluent_cmd()
        .current_dir(&main_dir)
        .args([
            "task",
            "run",
            "work-1",
            "attempt-1",
            "attempt-1-review-tests",
            "--no-sandbox",
        ])
        .env("PATH", mock_path(&bin_dir))
        .env("FLUENT_MAX_TASK_RETRIES", "0")
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
    assert_eq!(value["attempts"][0]["status"], "needs-user");
    assert_eq!(review_task["status"], "failed");
}

#[test]
fn work_task_run_recovers_review_task_when_coder_succeeds_on_retry() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    create_completed_work_attempt(&tmp, &main_dir);
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["review", "work-1", "attempt-1"])
        .assert()
        .success();

    let bin_dir = tmp.path().join("bin-review");
    let attempt_file = tmp.path().join("attempt-count");
    write_mock_claude(
        &bin_dir,
        &format!(
            r##"#!/bin/bash
ATTEMPT_FILE='{attempt_file}'
if [ ! -f "$ATTEMPT_FILE" ]; then
    printf '1' > "$ATTEMPT_FILE"
    exit 7
fi
printf 'Verdict: pass\n' > review.md
exit 0
"##,
            attempt_file = attempt_file.display()
        ),
    );

    fluent_cmd()
        .current_dir(&main_dir)
        .args([
            "task",
            "run",
            "work-1",
            "attempt-1",
            "attempt-1-review-tests",
            "--no-sandbox",
        ])
        .env("PATH", mock_path(&bin_dir))
        .env("FLUENT_MAX_TASK_RETRIES", "2")
        .assert()
        .success();

    let value = read_work_show_json(&main_dir, "work-1");
    let review_task = value["attempts"][0]["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|task| task["id"] == "attempt-1-review-tests")
        .unwrap();
    assert_eq!(
        review_task["status"], "complete",
        "review task should complete after recovery on retry"
    );
    assert_ne!(
        value["attempts"][0]["status"], "failed",
        "attempt should not be failed after successful retry"
    );
    assert_ne!(
        value["attempts"][0]["status"], "needs-user",
        "attempt should not be needs-user after successful retry"
    );
}

#[test]
fn work_task_run_persistent_coder_error_pauses_attempt_at_needs_user() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    create_completed_work_attempt(&tmp, &main_dir);
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["review", "work-1", "attempt-1"])
        .assert()
        .success();

    let bin_dir = tmp.path().join("bin-review");
    write_mock_claude(&bin_dir, "#!/bin/bash\nexit 7\n");

    fluent_cmd()
        .current_dir(&main_dir)
        .args([
            "task",
            "run",
            "work-1",
            "attempt-1",
            "attempt-1-review-tests",
            "--no-sandbox",
        ])
        .env("PATH", mock_path(&bin_dir))
        .env("FLUENT_MAX_TASK_RETRIES", "2")
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
    assert_eq!(
        value["attempts"][0]["status"], "needs-user",
        "attempt should pause at needs-user, not fail terminally"
    );
    assert_eq!(review_task["status"], "failed");
}

#[test]
fn work_task_run_handoff_file_includes_task_id() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    create_completed_work_attempt(&tmp, &main_dir);
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["review", "work-1", "attempt-1"])
        .assert()
        .success();

    let bin_dir = tmp.path().join("bin-review");
    write_mock_claude(&bin_dir, "#!/bin/bash\nexit 7\n");

    fluent_cmd()
        .current_dir(&main_dir)
        .args([
            "task",
            "run",
            "work-1",
            "attempt-1",
            "attempt-1-review-tests",
            "--no-sandbox",
        ])
        .env("PATH", mock_path(&bin_dir))
        .env("FLUENT_MAX_TASK_RETRIES", "0")
        .assert()
        .failure();

    let handoff_path = main_dir
        .join(".fluent/work/artifacts/work-1/attempt-1/needs-user-attempt-1-review-tests.md");
    assert!(
        handoff_path.exists(),
        "handoff file should include the task ID in its name"
    );
    let content = fs::read_to_string(&handoff_path).unwrap();
    assert!(
        content.contains("attempt-1-review-tests"),
        "handoff content should reference the failing task"
    );
    let old_path = main_dir.join(".fluent/work/artifacts/work-1/attempt-1/needs-user.md");
    assert!(
        !old_path.exists(),
        "old fixed-name handoff should not exist"
    );
}

#[test]
fn work_task_run_auth_rejection_pauses_attempt_after_one_retry() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    create_completed_work_attempt(&tmp, &main_dir);
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["review", "work-1", "attempt-1"])
        .assert()
        .success();

    let bin_dir = tmp.path().join("bin-auth-401");
    let counter_file = tmp.path().join("auth-invocation-count");
    write_mock_claude(
        &bin_dir,
        &format!(
            r##"#!/bin/bash
HAS_PROMPT=0
for arg in "$@"; do
  if [ "$arg" = "-p" ]; then HAS_PROMPT=1; break; fi
done
if [ "$HAS_PROMPT" = 0 ]; then exit 0; fi
COUNTER='{counter}'
count=$(cat "$COUNTER" 2>/dev/null || echo 0)
count=$((count + 1))
printf '%s' "$count" > "$COUNTER"
echo '{{"type":"result","api_error_status":401,"request_id":"req-test-401"}}'
exit 1
"##,
            counter = counter_file.display()
        ),
    );

    fluent_cmd()
        .current_dir(&main_dir)
        .args([
            "task",
            "run",
            "work-1",
            "attempt-1",
            "attempt-1-review-tests",
            "--no-sandbox",
        ])
        .env("PATH", mock_path(&bin_dir))
        .env("FLUENT_MAX_TASK_RETRIES", "2")
        .assert()
        .failure()
        .stderr(predicate::str::contains("401"))
        .stderr(predicate::str::contains("re-authenticate"));

    let invocations: u32 = fs::read_to_string(&counter_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_eq!(
        invocations, 3,
        "expected 3 invocations: original 401, refresh probe, one retry"
    );

    let value = read_work_show_json(&main_dir, "work-1");
    assert_eq!(
        value["attempts"][0]["status"], "needs-user",
        "attempt should pause at needs-user on auth rejection"
    );

    let handoff_path = main_dir
        .join(".fluent/work/artifacts/work-1/attempt-1/needs-user-attempt-1-review-tests.md");
    assert!(handoff_path.exists(), "handoff file should exist");
    let handoff = fs::read_to_string(&handoff_path).unwrap();
    assert!(
        handoff.contains("re-authenticate"),
        "handoff should instruct user to re-authenticate: {handoff}"
    );
}

#[test]
fn work_task_run_fails_review_task_that_dirties_candidate_workspace() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    create_completed_work_attempt(&tmp, &main_dir);
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["review", "work-1", "attempt-1"])
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

    fluent_cmd()
        .current_dir(&main_dir)
        .args([
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
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["review", "work-1", "attempt-1"])
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

    fluent_cmd()
        .current_dir(&main_dir)
        .args([
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
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["review", "work-1", "attempt-1"])
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

    fluent_cmd()
        .current_dir(&main_dir)
        .args([
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
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["review", "work-1", "attempt-1"])
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

    fluent_cmd()
        .current_dir(&main_dir)
        .args([
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
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["review", "work-1", "attempt-1"])
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

    fluent_cmd()
        .current_dir(&main_dir)
        .args([
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
        main_dir.join(".fluent/work/artifacts/work-1/attempt-1/attempt-1-review-tests"),
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

    fluent_cmd()
        .current_dir(&main_dir)
        .args(["work-item", "create", "work-1", "--title", "Run task"])
        .assert()
        .success();
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "create", "work-1", "attempt-1"])
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

    fluent_cmd()
        .current_dir(&main_dir)
        .args([
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

    fluent_cmd()
        .current_dir(&main_dir)
        .args(["work-item", "create", "work-1", "--title", "Run task"])
        .assert()
        .success();
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "create", "work-1", "attempt-1"])
        .assert()
        .success();

    fluent_cmd()
        .current_dir(&main_dir)
        .args([
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
fn work_task_run_pauses_attempt_when_write_coder_exits_nonzero() {
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

    fluent_cmd()
        .current_dir(&main_dir)
        .args(["work-item", "create", "work-1", "--title", "Run task"])
        .assert()
        .success();
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "create", "work-1", "attempt-1"])
        .assert()
        .success();

    fluent_cmd()
        .current_dir(&main_dir)
        .args([
            "task",
            "run",
            "work-1",
            "attempt-1",
            "attempt-1-write-1",
            "--no-sandbox",
        ])
        .env("PATH", mock_path(&bin_dir))
        .env("FLUENT_MAX_TASK_RETRIES", "0")
        .assert()
        .failure()
        .stderr(predicate::str::contains("Coder exited with code 7"));

    let value = read_work_show_json(&main_dir, "work-1");
    let attempt = &value["attempts"][0];
    let task = &attempt["tasks"][0];
    assert_eq!(attempt["status"], "needs-user");
    assert_eq!(task["status"], "failed");
    assert!(task.get("output").is_none());
}

#[test]
fn work_task_run_write_auth_rejection_pauses_attempt_after_one_retry() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    seed_project_expertise(&main_dir);

    let bin_dir = tmp.path().join("bin-write-auth-401");
    let counter_file = tmp.path().join("write-auth-invocation-count");
    write_mock_claude(
        &bin_dir,
        &format!(
            r##"#!/bin/bash
HAS_PROMPT=0
for arg in "$@"; do
  if [ "$arg" = "-p" ]; then HAS_PROMPT=1; break; fi
done
if [ "$HAS_PROMPT" = 0 ]; then exit 0; fi
COUNTER='{counter}'
count=$(cat "$COUNTER" 2>/dev/null || echo 0)
count=$((count + 1))
printf '%s' "$count" > "$COUNTER"
echo '{{"type":"result","api_error_status":401,"request_id":"req-test-401"}}'
exit 1
"##,
            counter = counter_file.display()
        ),
    );

    fluent_cmd()
        .current_dir(&main_dir)
        .args(["work-item", "create", "work-1", "--title", "Run task"])
        .assert()
        .success();
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "create", "work-1", "attempt-1"])
        .assert()
        .success();

    fluent_cmd()
        .current_dir(&main_dir)
        .args([
            "task",
            "run",
            "work-1",
            "attempt-1",
            "attempt-1-write-1",
            "--no-sandbox",
        ])
        .env("PATH", mock_path(&bin_dir))
        .env("FLUENT_MAX_TASK_RETRIES", "2")
        .assert()
        .failure()
        .stderr(predicate::str::contains("401"))
        .stderr(predicate::str::contains("re-authenticate"));

    let invocations: u32 = fs::read_to_string(&counter_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_eq!(
        invocations, 3,
        "expected 3 invocations: original 401, refresh probe, one retry"
    );

    let value = read_work_show_json(&main_dir, "work-1");
    assert_eq!(
        value["attempts"][0]["status"], "needs-user",
        "attempt should pause at needs-user on auth rejection"
    );

    let handoff_path =
        main_dir.join(".fluent/work/artifacts/work-1/attempt-1/needs-user-attempt-1-write-1.md");
    assert!(handoff_path.exists(), "handoff file should exist");
    let handoff = fs::read_to_string(&handoff_path).unwrap();
    assert!(
        handoff.contains("re-authenticate"),
        "handoff should instruct user to re-authenticate: {handoff}"
    );
}

#[test]
fn work_task_run_tester_persistent_error_pauses_attempt_at_needs_user() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);

    create_completed_work_attempt(&tmp, &main_dir);
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["review", "work-1", "attempt-1"])
        .assert()
        .success();

    let artifact_dir = main_dir.join(".fluent/work/artifacts/work-1/attempt-1/attempt-1-tester");
    let blocker = artifact_dir.join("tester-results.json");
    let _ = fs::remove_file(&blocker);
    fs::create_dir_all(&blocker).unwrap();

    let bin_dir = tmp.path().join("bin-tester");
    write_mock_claude(&bin_dir, "#!/bin/bash\nexit 0\n");

    fluent_cmd()
        .current_dir(&main_dir)
        .args([
            "task",
            "run",
            "work-1",
            "attempt-1",
            "attempt-1-tester",
            "--no-sandbox",
        ])
        .env("PATH", mock_path(&bin_dir))
        .env("FLUENT_MAX_TASK_RETRIES", "2")
        .assert()
        .failure();

    let value = read_work_show_json(&main_dir, "work-1");
    let tester_task = value["attempts"][0]["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|task| task["id"] == "attempt-1-tester")
        .unwrap();
    assert_eq!(
        value["attempts"][0]["status"], "needs-user",
        "attempt should pause at needs-user after persistent tester error"
    );
    assert_eq!(tester_task["status"], "failed");
}

#[test]
fn work_task_run_tester_recovers_when_error_is_transient() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);

    let attempt_file = tmp.path().join("tester-attempt-count");
    let artifact_dir = main_dir.join(".fluent/work/artifacts/work-1/attempt-1/attempt-1-tester");
    let blocker = artifact_dir.join("tester-results.json");

    let tester_yaml = main_dir.join(".fluent/tester.yaml");
    fs::create_dir_all(tester_yaml.parent().unwrap()).unwrap();
    fs::write(
        &tester_yaml,
        format!(
            "commands:\n  - command: |\n      \
             if [ ! -f '{attempt_file}' ]; then\n        \
             printf '1' > '{attempt_file}'\n        \
             mkdir -p '{blocker}'\n      \
             else\n        \
             rm -rf '{blocker}'\n      \
             fi\n    \
             test_harness: shell-harness\n",
            attempt_file = attempt_file.display(),
            blocker = blocker.display(),
        ),
    )
    .unwrap();
    git::run(&main_dir, &["add", ".fluent/tester.yaml"], "stage").unwrap();
    git::run(&main_dir, &["commit", "-m", "Add tester.yaml"], "commit").unwrap();

    create_completed_work_attempt(&tmp, &main_dir);
    // The baseline tester capture ran the script once, consuming the "first
    // call" state. Reset so the actual tester task still exercises the retry.
    let _ = fs::remove_file(&attempt_file);
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["review", "work-1", "attempt-1"])
        .assert()
        .success();

    let bin_dir = tmp.path().join("bin-tester");
    write_mock_claude(&bin_dir, "#!/bin/bash\nexit 0\n");

    fluent_cmd()
        .current_dir(&main_dir)
        .args([
            "task",
            "run",
            "work-1",
            "attempt-1",
            "attempt-1-tester",
            "--no-sandbox",
        ])
        .env("PATH", mock_path(&bin_dir))
        .env("FLUENT_MAX_TASK_RETRIES", "2")
        .assert()
        .success();

    let value = read_work_show_json(&main_dir, "work-1");
    let tester_task = value["attempts"][0]["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|task| task["id"] == "attempt-1-tester")
        .unwrap();
    assert_eq!(
        tester_task["status"], "complete",
        "tester task should complete after recovery on retry"
    );
    assert_ne!(
        value["attempts"][0]["status"], "failed",
        "attempt should not be failed after successful retry"
    );
    assert_ne!(
        value["attempts"][0]["status"], "needs-user",
        "attempt should not be needs-user after successful retry"
    );
}

#[test]
fn work_task_run_rejects_success_without_commits() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    let bin_dir = tmp.path().join("bin");
    write_mock_claude(&bin_dir, "#!/bin/bash\nexit 0\n");

    fluent_cmd()
        .current_dir(&main_dir)
        .args(["work-item", "create", "work-1", "--title", "Run task"])
        .assert()
        .success();
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "create", "work-1", "attempt-1"])
        .assert()
        .success();

    fluent_cmd()
        .current_dir(&main_dir)
        .args([
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

    fluent_cmd()
        .current_dir(&main_dir)
        .args(["work-item", "create", "work-1", "--title", "Run task"])
        .assert()
        .success();
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "create", "work-1", "attempt-1"])
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

    fluent_cmd()
        .current_dir(&main_dir)
        .args([
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

    fluent_cmd()
        .current_dir(&main_dir)
        .args(["work-item", "create", "work-1", "--title", "Run task"])
        .assert()
        .success();
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "create", "work-1", "attempt-1"])
        .assert()
        .success();

    let workspace = main_dir.join("../work-6-work-1-attempt-1");
    fs::create_dir_all(&workspace).unwrap();
    let item_path = main_dir.join(".fluent/work/items/work-1.json");
    let before = fs::read_to_string(&item_path).unwrap();

    fluent_cmd()
        .current_dir(&main_dir)
        .args([
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

    fluent_cmd()
        .current_dir(&main_dir)
        .args(["work-item", "create", "work-1", "--title", "Run task"])
        .assert()
        .success();
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "create", "work-1", "attempt-1"])
        .assert()
        .success();

    git::run(
        &main_dir,
        &["branch", "work/work-1/attempt-1/attempt-1-write-1", "HEAD"],
        "create task branch",
    )
    .unwrap();

    fluent_cmd()
        .current_dir(&main_dir)
        .args([
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

    fluent_cmd()
        .current_dir(&main_dir)
        .args(["work-item", "create", "work-1", "--title", "Run task"])
        .assert()
        .success();
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "create", "work-1", "attempt-1"])
        .assert()
        .success();

    let task_path = work_task_record_path(&main_dir, "work-1", "attempt-1", "attempt-1-write-1");
    let mut value = read_json_value(&task_path);
    value["status"] = serde_json::Value::String("failed".to_string());
    write_json_value(&task_path, &value);
    let before = fs::read_to_string(&task_path).unwrap();

    fluent_cmd()
        .current_dir(&main_dir)
        .args([
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

    assert_json_unchanged(&task_path, &before);
    assert!(!main_dir.join("../work-6-work-1-attempt-1").exists());
}

#[test]
fn work_task_run_rejects_non_write_task() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);

    fluent_cmd()
        .current_dir(&main_dir)
        .args(["work-item", "create", "work-1", "--title", "Run task"])
        .assert()
        .success();
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "create", "work-1", "attempt-1"])
        .assert()
        .success();

    let task_path = work_task_record_path(&main_dir, "work-1", "attempt-1", "attempt-1-write-1");
    let mut value = read_json_value(&task_path);
    value["kind"] = serde_json::Value::String("probe".to_string());
    write_json_value(&task_path, &value);
    let before = fs::read_to_string(&task_path).unwrap();

    fluent_cmd()
        .current_dir(&main_dir)
        .args([
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

    assert_json_unchanged(&task_path, &before);
    assert!(!main_dir.join("../work-6-work-1-attempt-1").exists());
}

#[test]
fn work_task_run_requires_one_writable_workspace() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);

    fluent_cmd()
        .current_dir(&main_dir)
        .args(["work-item", "create", "work-1", "--title", "Run task"])
        .assert()
        .success();
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "create", "work-1", "attempt-1"])
        .assert()
        .success();

    let task_path = work_task_record_path(&main_dir, "work-1", "attempt-1", "attempt-1-write-1");
    let mut value = read_json_value(&task_path);
    value["workspace_access"]["writes"] = serde_json::Value::Array(Vec::new());
    write_json_value(&task_path, &value);
    let before = fs::read_to_string(&task_path).unwrap();

    fluent_cmd()
        .current_dir(&main_dir)
        .args([
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

    assert_json_unchanged(&task_path, &before);
    assert!(!main_dir.join("../work-6-work-1-attempt-1").exists());

    let mut value: serde_json::Value = serde_json::from_str(&before).unwrap();
    value["workspace_access"]["writes"] = serde_json::json!([
        {"id": "candidate", "path": "../work-6-work-1-attempt-1"},
        {"id": "other", "path": "../work-6-work-1-other"}
    ]);
    write_json_value(&task_path, &value);
    let before = fs::read_to_string(&task_path).unwrap();

    fluent_cmd()
        .current_dir(&main_dir)
        .args([
            "task",
            "run",
            "work-1",
            "attempt-1",
            "attempt-1-write-1",
            "--no-sandbox",
        ])
        .assert()
        .failure();

    assert_json_unchanged(&task_path, &before);
    assert!(!main_dir.join("../work-6-work-1-attempt-1").exists());
}

#[test]
fn work_task_run_rejects_unmanaged_writable_workspace_path() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);

    fluent_cmd()
        .current_dir(&main_dir)
        .args(["work-item", "create", "work-1", "--title", "Run task"])
        .assert()
        .success();
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "create", "work-1", "attempt-1"])
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

        fluent_cmd()
            .current_dir(&main_dir)
            .args([
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

        assert_json_unchanged(&task_path, &before);
    }

    assert!(!main_dir.join("../outside-workspace").exists());
    assert!(!main_dir.join("../work-6-work-1-other-attempt").exists());
    assert!(!main_dir.join(".fluent/work/outside").exists());
    assert!(!Path::new(&outside_absolute).exists());
}

#[test]
fn work_task_run_missing_ids_leave_work_item_unchanged() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);

    fluent_cmd()
        .current_dir(&main_dir)
        .args(["work-item", "create", "work-1", "--title", "Run task"])
        .assert()
        .success();
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "create", "work-1", "attempt-1"])
        .assert()
        .success();
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "create", "work-1", "attempt-2"])
        .assert()
        .success();

    let item_path = main_dir.join(".fluent/work/items/work-1.json");
    let before = fs::read_to_string(&item_path).unwrap();

    for args in [
        [
            "task",
            "run",
            "missing-work",
            "attempt-1",
            "attempt-1-write-1",
            "--no-sandbox",
        ],
        [
            "task",
            "run",
            "work-1",
            "missing-attempt",
            "attempt-1-write-1",
            "--no-sandbox",
        ],
        [
            "task",
            "run",
            "work-1",
            "attempt-1",
            "missing-task",
            "--no-sandbox",
        ],
        [
            "task",
            "run",
            "work-1",
            "attempt-2",
            "attempt-1-write-1",
            "--no-sandbox",
        ],
    ] {
        fluent_cmd()
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

    let output = fluent_cmd()
        .current_dir(tmp.path())
        .args(["work-item", "list"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "work list failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stdout.contains("→ Next:"),
        "the list result must stay on stdout without the next-action hint: {stdout}"
    );
    assert!(
        stderr.contains("→ Next:"),
        "a populated work-item list should print a next-action hint on stderr: {stderr}"
    );
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
    fs::create_dir_all(tmp.path().join(".fluent/runs/legacy-run")).unwrap();
    fs::write(
        tmp.path().join(".fluent/runs/legacy-run/status"),
        "complete",
    )
    .unwrap();

    fluent_cmd()
        .current_dir(tmp.path())
        .args(["work-item", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("No Work Items found"));

    fluent_cmd()
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

    let output = fluent_cmd()
        .current_dir(tmp.path())
        .args(["work-item", "show", "work-1"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "work-item show failed: stdout={} stderr={}",
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

    fluent_cmd()
        .current_dir(tmp.path())
        .args(["work-item", "show", "missing-work"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "Work Item \"missing-work\" not found",
        ));
}

#[test]
fn work_show_rejects_invalid_work_item_id() {
    let tmp = TempDir::new().unwrap();

    fluent_cmd()
        .current_dir(tmp.path())
        .args(["work-item", "show", "../escape"])
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
    let before = fs::read_to_string(tmp.path().join(".fluent/work/items/work-1.json")).unwrap();

    fluent_cmd()
        .current_dir(tmp.path())
        .args(["merge-candidate", "show", "missing-work", "candidate-1"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "Work Item \"missing-work\" not found",
        ));

    fluent_cmd()
        .current_dir(tmp.path())
        .args(["merge-candidate", "show", "work-1", "candidate-1"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "Merge Candidate \"candidate-1\" not found in Work Item \"work-1\"",
        ));

    let after = fs::read_to_string(tmp.path().join(".fluent/work/items/work-1.json")).unwrap();
    assert_eq!(after, before);
}

#[test]
fn work_list_reports_invalid_stored_json_path() {
    let tmp = TempDir::new().unwrap();
    let items_dir = tmp.path().join(".fluent/work/items");
    fs::create_dir_all(&items_dir).unwrap();
    fs::write(items_dir.join("bad.json"), "{").unwrap();

    fluent_cmd()
        .current_dir(tmp.path())
        .args(["work-item", "list"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(".fluent/work/items/bad.json"))
        .stderr(predicate::str::contains("failed to parse"));
}

#[test]
fn work_list_reports_stored_work_item_id_mismatch() {
    let tmp = TempDir::new().unwrap();
    let items_dir = tmp.path().join(".fluent/work/items");
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

    fluent_cmd()
        .current_dir(tmp.path())
        .args(["work-item", "list"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(".fluent/work/items/work-1.json"))
        .stderr(predicate::str::contains("contains id work-2"))
        .stderr(predicate::str::contains("expected work-1"));
}

#[test]
fn work_list_reports_invalid_stored_work_item_id() {
    let tmp = TempDir::new().unwrap();
    let items_dir = tmp.path().join(".fluent/work/items");
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

    fluent_cmd()
        .current_dir(tmp.path())
        .args(["work-item", "list"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("bad\\\\id"))
        .stderr(predicate::str::contains("cannot be used as a file name"));
}

#[test]
fn work_list_reports_invalid_stored_model() {
    let tmp = TempDir::new().unwrap();
    let items_dir = tmp.path().join(".fluent/work/items");
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
    let attempts_dir = tmp.path().join(".fluent/work/attempts/work-invalid");
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

    fluent_cmd()
        .current_dir(tmp.path())
        .args(["work-item", "list"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            ".fluent/work/attempts/work-invalid/attempt-1.json",
        ))
        .stderr(predicate::str::contains("invalid work model"))
        .stderr(predicate::str::contains("expected work-invalid"));
}

fn write_work_item_json(project_root: &Path, id: &str, title: &str) {
    let items_dir = project_root.join(".fluent/work/items");
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
    let output = fluent_cmd()
        .current_dir(project_root)
        .args(["work-item", "show", work_item_id])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "work-item show failed: stdout={} stderr={}",
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
        .join(".fluent/work/tasks")
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

/// Compare two JSON-serialized texts semantically (ignore field order).
/// Use this when a test asserts a JSON file was not modified by Fluent:
/// Fluent's struct-order serde output and the test helper's alphabetical
/// serde_json::Value output produce equal Values but unequal text.
fn assert_json_unchanged(path: &Path, before: &str) {
    let current: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
    let baseline: serde_json::Value = serde_json::from_str(before).unwrap();
    assert_eq!(current, baseline, "JSON at {} was modified", path.display());
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
        .join(".fluent/work/attempts")
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
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["work-item", "create", "work-1", "--title", "Cleanup work"])
        .assert()
        .success();
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "create", "work-1", "attempt-1"])
        .assert()
        .success();
    fluent_cmd()
        .current_dir(&main_dir)
        .args([
            "work-item",
            "create",
            "work-active",
            "--title",
            "Active work",
        ])
        .assert()
        .success();
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "create", "work-active", "attempt-1"])
        .assert()
        .success();

    let item_path = main_dir.join(".fluent/work/items/work-1.json");
    let attempt_path = main_dir.join(".fluent/work/attempts/work-1/attempt-1.json");
    let task_path = main_dir.join(".fluent/work/tasks/work-1/attempt-1/attempt-1-write-1.json");
    let mut attempt = read_json_path(&attempt_path);
    attempt["status"] = serde_json::Value::String("complete".to_string());
    write_json_path(&attempt_path, &attempt);
    let mut task = read_json_path(&task_path);
    task["status"] = serde_json::Value::String("complete".to_string());
    task["artifact_area"] = serde_json::json!({
        "path": ".fluent/work/artifacts/work-1/attempt-1/attempt-1-write-1"
    });
    task["output"] = serde_json::json!({
        "workspace_id": "candidate",
        "workspace_path": "../work-6-work-1-attempt-1",
        "source_branch": "main",
        "commit": git_head(&main_dir)
    });
    write_json_path(&task_path, &task);

    let artifact_dir = main_dir.join(".fluent/work/artifacts/work-1/attempt-1/attempt-1-write-1");
    let artifact_parent = main_dir.join(".fluent/work/artifacts/work-1/attempt-1");
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

    let active_item_path = main_dir.join(".fluent/work/items/work-active.json");
    let active_attempt_path = main_dir.join(".fluent/work/attempts/work-active/attempt-1.json");
    let active_task_path =
        main_dir.join(".fluent/work/tasks/work-active/attempt-1/attempt-1-write-1.json");
    let mut active_attempt = read_json_path(&active_attempt_path);
    active_attempt["status"] = serde_json::Value::String("executing".to_string());
    write_json_path(&active_attempt_path, &active_attempt);
    let mut active_task = read_json_path(&active_task_path);
    active_task["status"] = serde_json::Value::String("executing".to_string());
    active_task["artifact_area"] = serde_json::json!({
        "path": ".fluent/work/artifacts/work-active/attempt-1/attempt-1-active"
    });
    write_json_path(&active_task_path, &active_task);

    let active_artifact_dir =
        main_dir.join(".fluent/work/artifacts/work-active/attempt-1/attempt-1-active");
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

    fluent_cmd()
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

    fluent_cmd()
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
    fluent_cmd()
        .current_dir(&main_dir)
        .args([
            "work-item",
            "create",
            "work-active",
            "--title",
            "Active work",
        ])
        .assert()
        .success();

    let artifacts_dir = main_dir.join(".fluent/work/artifacts");
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

    fluent_cmd()
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

    fluent_cmd()
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
    fluent_cmd()
        .current_dir(&main_dir)
        .args([
            "work-item",
            "create",
            "work-unregistered",
            "--title",
            "Unregistered cleanup work",
        ])
        .assert()
        .success();
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "create", "work-unregistered", "attempt-1"])
        .assert()
        .success();

    let item_path = main_dir.join(".fluent/work/items/work-unregistered.json");
    let workspace_path = "../work-17-work-unregistered-attempt-1";
    let workspace_dir = main_dir.join(workspace_path);
    fs::create_dir_all(&workspace_dir).unwrap();
    fs::write(workspace_dir.join("user-file.txt"), "keep me").unwrap();

    let attempt_path = main_dir.join(".fluent/work/attempts/work-unregistered/attempt-1.json");
    let task_path =
        main_dir.join(".fluent/work/tasks/work-unregistered/attempt-1/attempt-1-write-1.json");
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

    fluent_cmd()
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
    fluent_cmd()
        .current_dir(&main_dir)
        .args([
            "work-item",
            "create",
            "work-failed",
            "--title",
            "Failed work",
        ])
        .assert()
        .success();
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "create", "work-failed", "attempt-1"])
        .assert()
        .success();
    fluent_cmd()
        .current_dir(&main_dir)
        .args([
            "work-item",
            "create",
            "work-pending-merge",
            "--title",
            "Pending merge work",
        ])
        .assert()
        .success();
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "create", "work-pending-merge", "attempt-1"])
        .assert()
        .success();

    let failed_item_path = main_dir.join(".fluent/work/items/work-failed.json");
    let failed_attempt_path = main_dir.join(".fluent/work/attempts/work-failed/attempt-1.json");
    let failed_task_path =
        main_dir.join(".fluent/work/tasks/work-failed/attempt-1/attempt-1-write-1.json");
    let mut failed_attempt = read_json_path(&failed_attempt_path);
    failed_attempt["status"] = serde_json::Value::String("failed".to_string());
    write_json_path(&failed_attempt_path, &failed_attempt);
    let mut failed_task = read_json_path(&failed_task_path);
    failed_task["status"] = serde_json::Value::String("failed".to_string());
    write_json_path(&failed_task_path, &failed_task);

    let pending_item_path = main_dir.join(".fluent/work/items/work-pending-merge.json");
    let pending_workspace = "../work-18-work-pending-merge-attempt-1";
    let head = git_head(&main_dir);
    let pending_attempt_path =
        main_dir.join(".fluent/work/attempts/work-pending-merge/attempt-1.json");
    let pending_task_path =
        main_dir.join(".fluent/work/tasks/work-pending-merge/attempt-1/attempt-1-write-1.json");
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
        main_dir.join(".fluent/work/merge-candidates/work-pending-merge/candidate-1.json");
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

    fluent_cmd()
        .current_dir(&main_dir)
        .arg("cleanup")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "would clean Work Item work-failed",
        ))
        .stdout(predicate::str::contains("work-pending-merge").not());

    fluent_cmd()
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
    fluent_cmd()
        .current_dir(&main_dir)
        .args([
            "work-item",
            "create",
            "work-active-task",
            "--title",
            "Active task cleanup work",
        ])
        .assert()
        .success();
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "create", "work-active-task", "attempt-1"])
        .assert()
        .success();

    let item_path = main_dir.join(".fluent/work/items/work-active-task.json");
    let attempt_path = main_dir.join(".fluent/work/attempts/work-active-task/attempt-1.json");
    let task_path =
        main_dir.join(".fluent/work/tasks/work-active-task/attempt-1/attempt-1-write-1.json");
    let mut attempt = read_json_path(&attempt_path);
    attempt["status"] = serde_json::Value::String("failed".to_string());
    write_json_path(&attempt_path, &attempt);
    let mut task = read_json_path(&task_path);
    task["status"] = serde_json::Value::String("executing".to_string());
    task["artifact_area"] = serde_json::json!({
        "path": ".fluent/work/artifacts/work-active-task/attempt-1/attempt-1-write-1"
    });
    write_json_path(&task_path, &task);

    let artifact_dir =
        main_dir.join(".fluent/work/artifacts/work-active-task/attempt-1/attempt-1-write-1");
    fs::create_dir_all(&artifact_dir).unwrap();
    fs::write(artifact_dir.join("result.md"), "active task artifact").unwrap();

    fluent_cmd()
        .current_dir(&main_dir)
        .arg("cleanup")
        .assert()
        .success()
        .stdout(predicate::str::contains("work-active-task").not());

    fluent_cmd()
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
    fluent_cmd()
        .current_dir(&main_dir)
        .args([
            "work-item",
            "create",
            "work-merge-cleanup",
            "--title",
            "Merge cleanup work",
        ])
        .assert()
        .success();
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "create", "work-merge-cleanup", "attempt-1"])
        .assert()
        .success();

    let item_path = main_dir.join(".fluent/work/items/work-merge-cleanup.json");
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
    let attempt_path = main_dir.join(".fluent/work/attempts/work-merge-cleanup/attempt-1.json");
    let task_path =
        main_dir.join(".fluent/work/tasks/work-merge-cleanup/attempt-1/attempt-1-write-1.json");
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
        main_dir.join(".fluent/work/merge-candidates/work-merge-cleanup/candidate-1.json");
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
                        "path": ".fluent/work/artifacts/work-merge-cleanup/attempt-1/candidate-1/merge/checks/checks.json"
                    }
                ],
                "review_artifacts": [
                    {
                        "producer_id": "merge-review-tests",
                        "path": ".fluent/work/artifacts/work-merge-cleanup/attempt-1/candidate-1/merge/reviews/tests/review.md"
                    }
                ]
            }
        }),
    );

    let check_artifact = main_dir.join(
        ".fluent/work/artifacts/work-merge-cleanup/attempt-1/candidate-1/merge/checks/checks.json",
    );
    let attempt_artifact_dir = main_dir.join(".fluent/work/artifacts/work-merge-cleanup/attempt-1");
    let candidate_artifact_dir = attempt_artifact_dir.join("candidate-1");
    let review_artifact = main_dir
        .join(".fluent/work/artifacts/work-merge-cleanup/attempt-1/candidate-1/merge/reviews/tests/review.md");
    fs::create_dir_all(check_artifact.parent().unwrap()).unwrap();
    fs::create_dir_all(review_artifact.parent().unwrap()).unwrap();
    fs::write(&check_artifact, "{}").unwrap();
    fs::write(&review_artifact, "Verdict: pass\n").unwrap();

    fluent_cmd()
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

    fluent_cmd()
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
    // by fluent work task run) make commits outside our wrapper.
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
    // Mirror the real project's gitignore so Fluent-managed runtime state
    // under .fluent/work/ doesn't appear as uncommitted changes.
    fs::write(
        main_dir.join(".gitignore"),
        ".fluent/*\n!.fluent/expertise/\n!.fluent/hooks/\n!.fluent/Dockerfile\n!.fluent/tester.yaml\n!.fluent/extract-tester-results\n",
    )
    .unwrap();
    for role in [
        "documentation",
        "behaviors",
        "architecture",
        "skills",
        "tests",
    ] {
        let skill_dir = main_dir.join(format!("skills/review-{role}"));
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(skill_dir.join("SKILL.md"), "stub skill for tests").unwrap();
    }
    let fluent_dir = main_dir.join(".fluent");
    fs::create_dir_all(&fluent_dir).unwrap();
    fs::write(
        fluent_dir.join("tester.yaml"),
        "commands:\n  - command: \"true\"\n    test_harness: shell-harness\n",
    )
    .unwrap();
    let extractor_path = fluent_dir.join("extract-tester-results");
    fs::write(&extractor_path, "#!/bin/sh\necho '[]'\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&extractor_path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&extractor_path, perms).unwrap();
    }
    git::run(&main_dir, &["add", "."], "stage files").unwrap();
    git::run(&main_dir, &["commit", "-m", "init"], "initial commit").unwrap();

    main_dir
}

fn seed_project_expertise(main_dir: &Path) {
    let expertise_dir = main_dir.join(".fluent/expertise");
    fs::create_dir_all(&expertise_dir).unwrap();
    fs::write(
        expertise_dir.join("INDEX.md"),
        "# Project Expertise Index\n",
    )
    .unwrap();
    fs::write(expertise_dir.join("overview.md"), "# Overview\n").unwrap();
    git::run(main_dir, &["add", ".fluent/expertise"], "stage expertise").unwrap();
    git::run(
        main_dir,
        &["commit", "-m", "Seed project expertise overview"],
        "commit expertise",
    )
    .unwrap();
}

fn create_completed_work_attempt(tmp: &TempDir, main_dir: &Path) {
    create_completed_work_attempt_with_instructions(tmp, main_dir, None);
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

    let mut create_args = vec!["work-item", "create", "work-1", "--title", "Run review"];
    if let Some(instructions) = instructions {
        create_args.extend(["--instructions", instructions]);
    }
    fluent_cmd()
        .current_dir(main_dir)
        .args(create_args)
        .assert()
        .success();
    fluent_cmd()
        .current_dir(main_dir)
        .args(["attempt", "create", "work-1", "attempt-1"])
        .assert()
        .success();
    fluent_cmd()
        .current_dir(main_dir)
        .args([
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
    // Pre-write a stub tester-results.json at the path where the tester task
    // will later produce output. Most tests that use this helper plan reviews
    // and then run a review task directly, skipping the tester. The reviewer
    // requires its input artifacts to exist, so we satisfy that here.
    let tester_results = main_dir
        .join(".fluent/work/artifacts/work-1/attempt-1/attempt-1-tester/tester-results.json");
    fs::create_dir_all(tester_results.parent().unwrap()).unwrap();
    // The baseline tester capture may have run a tester.yaml script that left
    // a directory at this path; remove it so the stub file can be written.
    if tester_results.is_dir() {
        fs::remove_dir_all(&tester_results).unwrap();
    }
    fs::write(
        &tester_results,
        r#"{"commands":[],"tests":[],"summary":{"total":0,"pass":0,"fail":0,"skipped":0},"error":null}"#,
    )
    .unwrap();
}

fn loop_mock_script(verdict: &str) -> String {
    format!(
        r##"#!/bin/bash
# Non-prompt invocations (e.g., --version from capture_coder_info) should
# not write any files.
HAS_PROMPT=0
for arg in "$@"; do
  if [ "$arg" = "-p" ]; then
    HAS_PROMPT=1
    break
  fi
done
if [ "$HAS_PROMPT" = 0 ]; then
  exit 0
fi
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

# Non-prompt invocations (e.g., --version from capture_coder_info) should
# not write any files.
if [ -z "$PROMPT" ]; then
  exit 0
fi

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

# Non-prompt invocations (e.g., --version) should not write any files.
if [ -z "$PROMPT" ]; then
  exit 0
fi

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

# Non-prompt invocations (e.g., --version) should not write any files.
if [ -z "$PROMPT" ]; then
  exit 0
fi

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

# Non-prompt invocations (e.g., --version) should not write any files.
if [ -z "$PROMPT" ]; then
  exit 0
fi

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

// Bash snippet that all coder mocks should prepend so non-prompt invocations
// (e.g., `claude --version` from capture_coder_info) don't write any files
// into the surrounding workspace.
const MOCK_PROMPT_GUARD: &str = r##"HAS_PROMPT=0
for arg in "$@"; do
  if [ "$arg" = "-p" ]; then HAS_PROMPT=1; break; fi
done
if [ "$HAS_PROMPT" = 0 ]; then exit 0; fi
"##;

fn review_only_mock_script(verdict: &str) -> String {
    format!(
        r##"#!/bin/bash
{guard}printf 'Verdict: {verdict}\n\nReview-only result.\n' > review.md
exit 0
"##,
        guard = MOCK_PROMPT_GUARD,
    )
}

fn review_only_dirty_source_mock_script() -> String {
    format!(
        r##"#!/bin/bash
{guard}printf 'reviewer edit\n' >> ../../../../../../README.md
printf 'Verdict: pass\n\nReview-only result.\n' > review.md
exit 0
"##,
        guard = MOCK_PROMPT_GUARD,
    )
}

fn review_only_changed_head_mock_script() -> String {
    format!(
        r##"#!/bin/bash
{guard}repo="$(pwd)/../../../../../../"
git -C "$repo" config user.email test@example.com
git -C "$repo" config user.name "Test User"
printf 'reviewer commit\n' > "$repo/reviewer-commit.txt"
git -C "$repo" add reviewer-commit.txt
git -C "$repo" commit -m "Mutate source head" >/dev/null
printf 'Verdict: pass\n\nReview-only result.\n' > review.md
exit 0
"##,
        guard = MOCK_PROMPT_GUARD,
    )
}

fn review_only_dirty_fluent_mock_script() -> String {
    format!(
        r##"#!/bin/bash
{guard}printf 'reviewer edit\n' >> ../../../../../../.fluent/expertise/decisions.md
printf 'Verdict: pass\n\nReview-only result.\n' > review.md
exit 0
"##,
        guard = MOCK_PROMPT_GUARD,
    )
}

fn review_only_dirty_work_state_mock_script() -> String {
    format!(
        r##"#!/bin/bash
{guard}printf 'reviewer edit\n' >> ../../../../items/work-1.json
printf 'Verdict: pass\n\nReview-only result.\n' > review.md
exit 0
"##,
        guard = MOCK_PROMPT_GUARD,
    )
}

fn review_only_dirty_source_and_fluent_mock_script() -> String {
    format!(
        r##"#!/bin/bash
{guard}printf 'reviewer source edit\n' >> ../../../../../../README.md
printf 'reviewer fluent edit\n' >> ../../../../../../.fluent/expertise/decisions.md
printf 'Verdict: pass\n\nReview-only result.\n' > review.md
exit 0
"##,
        guard = MOCK_PROMPT_GUARD,
    )
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

fn assert_no_non_fluent_changes(path: &Path) {
    let status = git::run_stdout(
        path,
        &[
            "status",
            "--porcelain",
            "--untracked-files=all",
            "--",
            ".",
            ":(exclude).fluent",
        ],
        "check for non-fluent changes",
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
HAS_PROMPT=0
for arg in "$@"; do
  if [ "$arg" = "-p" ]; then HAS_PROMPT=1; break; fi
done
if [ "$HAS_PROMPT" = 0 ]; then exit 0; fi
case "$PWD" in
  */work-6-work-1-attempt-1)
    count_file="$PWD/.fluent-loop-write-count"
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
HAS_PROMPT=0
for arg in "$@"; do
  if [ "$arg" = "-p" ]; then HAS_PROMPT=1; break; fi
done
if [ "$HAS_PROMPT" = 0 ]; then exit 0; fi
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
HAS_PROMPT=0
for arg in "$@"; do
  if [ "$arg" = "-p" ]; then HAS_PROMPT=1; break; fi
done
if [ "$HAS_PROMPT" = 0 ]; then exit 0; fi
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

if [ "${FLUENT_TASK_KIND:-}" = "behavior-tests" ]; then
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
    let hooks_dir = project_root.join(".fluent/hooks");
    fs::create_dir_all(&hooks_dir).unwrap();
    let path = hooks_dir.join(name);
    fs::write(&path, script).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    // The test gitignore exempts .fluent/hooks/, so commit the hook so it
    // doesn't appear as uncommitted in later merge prechecks.
    let relative = format!(".fluent/hooks/{name}");
    git::run(project_root, &["add", &relative], "stage hook").unwrap();
    git::run(
        project_root,
        &["commit", "-m", &format!("Add hook {name}")],
        "commit hook",
    )
    .unwrap();
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

fn write_post_merge_review_queue_entry(
    project_root: &Path,
    target_branch: &str,
    merged_commit: &str,
    source_work_item_id: &str,
) {
    let queue_path = project_root.join(".fluent/work/post-merge-review-queue.json");
    fs::create_dir_all(queue_path.parent().unwrap()).unwrap();
    let body = format!(
        "{{\"entries\":[{{\"target_branch\":\"{target_branch}\",\"merged_commit\":\"{merged_commit}\",\"merged_at_unix\":0,\"source_work_item_id\":\"{source_work_item_id}\",\"source_merge_candidate_id\":\"{source_work_item_id}-attempt-1-merge-candidate\"}}]}}"
    );
    fs::write(&queue_path, body).unwrap();
}

#[test]
fn post_merge_review_creates_worktree_and_runs_tester_then_reviewers() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    let main_head = git_head(&main_dir);
    let expected_worktree = tmp.path().join("work-review-main");

    let bin_dir = tmp.path().join("bin-post-merge-walking-skeleton");
    write_mock_claude(&bin_dir, &review_only_mock_script("pass"));

    write_post_merge_review_queue_entry(&main_dir, "main", &main_head, "source-work");

    fluent_cmd()
        .current_dir(&main_dir)
        .args(["post-merge-review", "--debounce-seconds", "0"])
        .env("PATH", mock_path(&bin_dir))
        .assert()
        .success();

    assert!(
        expected_worktree.exists(),
        "review-only worktree must be created at {}",
        expected_worktree.display()
    );
    assert!(
        expected_worktree.join(".git").exists(),
        "review-only worktree must be a registered git worktree"
    );

    let short = &main_head[..8.min(main_head.len())];
    let work_item_id = format!("post-merge-main-{short}");
    let value = read_work_show_json(&main_dir, &work_item_id);
    let attempt = &value["attempts"][0];
    assert_eq!(attempt["kind"], "post-merge-review");
    assert_eq!(attempt["status"], "complete");
    assert_eq!(attempt["review_state"], "passed");

    let tasks = attempt["tasks"]
        .as_array()
        .expect("attempt has tasks array");
    assert_eq!(
        tasks.len(),
        1 + review::REVIEWERS.len(),
        "1 tester + {} reviewers",
        review::REVIEWERS.len()
    );
    assert_eq!(tasks[0]["kind"], "tester");
    assert_eq!(tasks[0]["status"], "complete");
    assert_eq!(
        tasks[0]["workspace_access"]["reads"][0]["path"],
        "../work-review-main"
    );
    for task in tasks.iter().skip(1) {
        assert_eq!(task["kind"], "review");
        assert_eq!(task["status"], "complete");
        assert_eq!(
            task["workspace_access"]["reads"][0]["path"],
            "../work-review-main"
        );
        assert_eq!(task["depends_on"], "attempt-1-tester");
    }
    assert_eq!(git_head(&main_dir), main_head);

    // Re-run on a new commit on the same branch: the worktree must
    // be reused (not recreated) and resynced to the new commit.
    commit_file(&main_dir, "follow-up.txt", "second\n", "second commit");
    let new_head = git_head(&main_dir);
    write_post_merge_review_queue_entry(&main_dir, "main", &new_head, "source-work-2");
    let worktree_inode_before = fs::metadata(&expected_worktree).unwrap();

    fluent_cmd()
        .current_dir(&main_dir)
        .args(["post-merge-review", "--debounce-seconds", "0"])
        .env("PATH", mock_path(&bin_dir))
        .assert()
        .success();

    let worktree_inode_after = fs::metadata(&expected_worktree).unwrap();
    use std::os::unix::fs::MetadataExt;
    assert_eq!(
        worktree_inode_before.ino(),
        worktree_inode_after.ino(),
        "worktree directory must be the same inode after re-sync"
    );
    let worktree_head =
        git::run_stdout(&expected_worktree, &["rev-parse", "HEAD"], "worktree head").unwrap();
    assert_eq!(
        worktree_head, new_head,
        "review-only worktree must be synced to the new target commit"
    );
}

#[test]
fn work_attempt_run_rejects_review_only_worktree_already_in_flight() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    for wi in ["work-1", "work-2"] {
        fluent_cmd()
            .current_dir(&main_dir)
            .args(["work-item", "create", wi, "--title", "Review codebase"])
            .assert()
            .success();
        fluent_cmd()
            .current_dir(&main_dir)
            .args(["review", "codebase", wi, "attempt-review"])
            .assert()
            .success();
    }

    fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "run", "work-2", "attempt-review", "--no-sandbox"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("already in flight"))
        .stderr(predicate::str::contains("\"work-1\""));
}

#[test]
fn post_merge_review_defers_queue_entry_when_worktree_in_flight() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    fluent_cmd()
        .current_dir(&main_dir)
        .args([
            "work-item",
            "create",
            "work-1",
            "--title",
            "Review codebase",
        ])
        .assert()
        .success();
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["review", "codebase", "work-1", "attempt-review"])
        .assert()
        .success();

    let main_head = git_head(&main_dir);
    write_post_merge_review_queue_entry(&main_dir, "main", &main_head, "source-work");

    fluent_cmd()
        .current_dir(&main_dir)
        .args(["post-merge-review", "--debounce-seconds", "0"])
        .assert()
        .success()
        .stderr(predicate::str::contains("Deferring post-merge review"))
        .stderr(predicate::str::contains("\"work-1\""));

    let queue_path = main_dir.join(".fluent/work/post-merge-review-queue.json");
    let queue: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&queue_path).unwrap()).unwrap();
    let entries = queue["entries"].as_array().expect("queue entries array");
    assert_eq!(
        entries.len(),
        1,
        "deferred entry must remain in the queue for the next pass"
    );
    assert_eq!(entries[0]["target_branch"], "main");
    let short = &main_head[..8.min(main_head.len())];
    let post_merge_item_path = main_dir
        .join(".fluent/work/items")
        .join(format!("post-merge-main-{short}.json"));
    assert!(
        !post_merge_item_path.exists(),
        "no post-merge Work Item should be created while the worktree is in flight"
    );
}

fn create_review_only_worktree(main_dir: &Path, tmp: &TempDir, branch: &str) -> PathBuf {
    git::run(main_dir, &["branch", branch], "create branch").unwrap();
    let path = tmp.path().join(format!("work-review-{branch}"));
    git::run(
        main_dir,
        &[
            "worktree",
            "add",
            "--detach",
            &path.to_string_lossy(),
            branch,
        ],
        "create review-only worktree",
    )
    .unwrap();
    path
}

fn seed_in_flight_review_only_attempt(main_dir: &Path, work_item_id: &str, branch: &str) {
    git::run(main_dir, &["checkout", branch], "checkout branch for seed").unwrap();
    fluent_cmd()
        .current_dir(main_dir)
        .args([
            "work-item",
            "create",
            work_item_id,
            "--title",
            "Seed in-flight",
        ])
        .assert()
        .success();
    fluent_cmd()
        .current_dir(main_dir)
        .args(["review", "codebase", work_item_id, "attempt-review"])
        .assert()
        .success();
    git::run(main_dir, &["checkout", "main"], "checkout main").unwrap();
}

#[test]
fn review_only_worktree_prune_default_removes_orphan_keeps_others() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    let keep = create_review_only_worktree(&main_dir, &tmp, "keep-me");
    let gone = create_review_only_worktree(&main_dir, &tmp, "gone");
    let busy = create_review_only_worktree(&main_dir, &tmp, "busy");
    seed_in_flight_review_only_attempt(&main_dir, "work-busy", "busy");
    git::run(
        &main_dir,
        &["branch", "-D", "gone"],
        "delete orphaned branch",
    )
    .unwrap();
    git::run(&main_dir, &["branch", "-D", "busy"], "delete in-use branch").unwrap();

    let output = fluent_cmd()
        .current_dir(&main_dir)
        .args(["cleanup", "--apply"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "cleanup failed: stdout={stdout} stderr={stderr}"
    );
    assert!(stdout.contains("removed review-only worktree") && stdout.contains("work-review-gone"));
    assert!(stdout.contains("in-use review-only worktree") && stdout.contains("work-review-busy"));
    assert!(stdout.contains("\"work-busy\""));
    assert!(!gone.exists(), "orphan worktree should be removed");
    assert!(busy.exists(), "in-use worktree must remain");
    assert!(keep.exists(), "live worktree must remain");
}

#[test]
fn review_only_worktree_prune_all_removes_everything_but_in_use() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    let keep = create_review_only_worktree(&main_dir, &tmp, "keep-me");
    let busy = create_review_only_worktree(&main_dir, &tmp, "busy");
    seed_in_flight_review_only_attempt(&main_dir, "work-busy", "busy");

    fluent_cmd()
        .current_dir(&main_dir)
        .args(["cleanup", "--apply", "--prune-all-review-worktrees"])
        .assert()
        .success()
        .stdout(predicate::str::contains("removed review-only worktree"))
        .stdout(predicate::str::contains("work-review-keep-me"))
        .stdout(predicate::str::contains("in-use review-only worktree"))
        .stdout(predicate::str::contains("work-review-busy"));
    assert!(
        !keep.exists(),
        "live worktree should be removed by --prune-all-review-worktrees"
    );
    assert!(
        busy.exists(),
        "in-use worktree must remain even with --prune-all-review-worktrees"
    );
}

#[test]
fn post_merge_review_auto_prunes_orphan_worktree_before_processing_queue() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    let orphan = create_review_only_worktree(&main_dir, &tmp, "gone");
    git::run(&main_dir, &["branch", "-D", "gone"], "delete orphan branch").unwrap();

    let main_head = git_head(&main_dir);
    write_post_merge_review_queue_entry(&main_dir, "main", &main_head, "source-work");

    let bin_dir = tmp.path().join("bin-post-merge-auto-prune");
    write_mock_claude(&bin_dir, &review_only_mock_script("pass"));

    let output = fluent_cmd()
        .current_dir(&main_dir)
        .args(["post-merge-review", "--debounce-seconds", "0"])
        .env("PATH", mock_path(&bin_dir))
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "post-merge-review failed: stderr={stderr}"
    );
    assert!(
        stderr.contains("Auto-pruned orphan review-only worktree"),
        "stderr must announce auto-prune: {stderr}"
    );
    assert!(
        stderr.contains("work-review-gone"),
        "auto-prune notice must name the orphan path: {stderr}"
    );

    assert!(
        !orphan.exists(),
        "orphan worktree must be removed before queue processing"
    );

    let short = &main_head[..8.min(main_head.len())];
    let post_merge_item_path = main_dir
        .join(".fluent/work/items")
        .join(format!("post-merge-main-{short}.json"));
    assert!(
        post_merge_item_path.exists(),
        "queue entry must still be processed in the same pass"
    );
    let queue_path = main_dir.join(".fluent/work/post-merge-review-queue.json");
    let queue: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&queue_path).unwrap()).unwrap();
    assert!(
        queue["entries"].as_array().unwrap().is_empty(),
        "processed entry must be cleared from the queue"
    );
}

#[test]
fn review_only_worktree_prune_dry_run_changes_nothing() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    let gone = create_review_only_worktree(&main_dir, &tmp, "gone");
    let busy = create_review_only_worktree(&main_dir, &tmp, "busy");
    seed_in_flight_review_only_attempt(&main_dir, "work-busy", "busy");
    git::run(
        &main_dir,
        &["branch", "-D", "gone"],
        "delete orphaned branch",
    )
    .unwrap();
    git::run(&main_dir, &["branch", "-D", "busy"], "delete in-use branch").unwrap();

    fluent_cmd()
        .current_dir(&main_dir)
        .args(["cleanup"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "would remove review-only worktree",
        ))
        .stdout(predicate::str::contains("work-review-gone"))
        .stdout(predicate::str::contains(
            "would skip in-use review-only worktree",
        ))
        .stdout(predicate::str::contains("work-review-busy"));
    assert!(gone.exists(), "dry-run must not remove anything");
    assert!(busy.exists(), "dry-run must not remove anything");
}

// --- Observations management ---

#[test]
fn observations_add_with_inline_content() {
    let tmp = TempDir::new().unwrap();

    let output = fluent_cmd()
        .current_dir(tmp.path())
        .args(["observation", "create", "Test observation content"])
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

    let obs_dir = tmp.path().join(".fluent/observations");
    let file = obs_dir.join(format!("{id}.md"));
    assert!(file.exists(), "observation file should exist");
    let content = fs::read_to_string(&file).unwrap();
    assert!(content.contains("Test observation content"));
}

#[test]
fn observations_add_from_stdin() {
    let tmp = TempDir::new().unwrap();

    let output = fluent_cmd()
        .current_dir(tmp.path())
        .args(["observation", "create"])
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

    let file = tmp.path().join(format!(".fluent/observations/{id}.md"));
    assert!(file.exists(), "observation file should exist");
    let content = fs::read_to_string(&file).unwrap();
    assert!(content.contains("Observation from stdin"));
}

#[test]
fn observations_add_empty_stdin_errors() {
    let tmp = TempDir::new().unwrap();

    let output = fluent_cmd()
        .current_dir(tmp.path())
        .args(["observation", "create"])
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
    let obs_dir = tmp.path().join(".fluent/observations");
    fs::create_dir_all(&obs_dir).unwrap();
    fs::write(
        obs_dir.join("20260612-000000-test-obs.md"),
        "Test obs body\n",
    )
    .unwrap();

    let output = fluent_cmd()
        .current_dir(tmp.path())
        .args([
            "observation",
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

    let output = fluent_cmd()
        .current_dir(tmp.path())
        .args(["observation", "resolve", "nonexistent-id", "whatever"])
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
    let obs_dir = tmp.path().join(".fluent/observations");
    fs::create_dir_all(&obs_dir).unwrap();
    fs::write(
        obs_dir.join("20260612-143000-unique-entry.md"),
        "Unique observation\n",
    )
    .unwrap();

    let output = fluent_cmd()
        .current_dir(tmp.path())
        .args(["observation", "resolve", "20260612-143", "Done"])
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
    let obs_dir = tmp.path().join(".fluent/observations");
    fs::create_dir_all(&obs_dir).unwrap();
    fs::write(obs_dir.join("20260612-000000-alpha.md"), "a\n").unwrap();
    fs::write(obs_dir.join("20260612-000000-bravo.md"), "b\n").unwrap();

    let output = fluent_cmd()
        .current_dir(tmp.path())
        .args(["observation", "resolve", "20260612", "Done"])
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
    let obs_dir = tmp.path().join(".fluent/observations");
    fs::create_dir_all(&obs_dir).unwrap();
    fs::write(obs_dir.join("20260612-120000-second.md"), "Second entry\n").unwrap();
    fs::write(obs_dir.join("20260611-100000-first.md"), "First entry\n").unwrap();
    fs::write(obs_dir.join("20260613-080000-third.md"), "Third entry\n").unwrap();

    let output = fluent_cmd()
        .current_dir(tmp.path())
        .args(["observation", "list"])
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
    let obs_dir = tmp.path().join(".fluent/observations");
    let resolved_dir = obs_dir.join("resolved");
    fs::create_dir_all(&resolved_dir).unwrap();
    fs::write(obs_dir.join("20260612-open.md"), "Open observation body\n").unwrap();
    fs::write(
        resolved_dir.join("20260611-resolved.md"),
        "Resolved observation body\n",
    )
    .unwrap();

    // Show open observation
    let output = fluent_cmd()
        .current_dir(tmp.path())
        .args(["observation", "show", "20260612-open"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Open observation body"));

    // Show resolved observation (falls back to resolved dir)
    let output = fluent_cmd()
        .current_dir(tmp.path())
        .args(["observation", "show", "20260611-resolved"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Resolved observation body"));

    // Show unknown observation
    let output = fluent_cmd()
        .current_dir(tmp.path())
        .args(["observation", "show", "nonexistent"])
        .output()
        .unwrap();
    assert!(!output.status.success());
}

#[test]
fn observations_migrate_splits_monolithic_files() {
    let tmp = TempDir::new().unwrap();
    let fluent = tmp.path().join(".fluent");
    fs::create_dir_all(&fluent).unwrap();

    fs::write(
        fluent.join("observations.md"),
        "# Observations\n\nOpen queue.\n\n---\n\n\
         2026-06-12 \u{2014} First open observation\nDetails here.\n\n\
         2026-06-12 \u{2014} Second open observation\nMore details.\n",
    )
    .unwrap();

    fs::write(
        fluent.join("observations-resolved.md"),
        "# Resolved Observations\n\nResolved queue.\n\n---\n\n\
         2026-06-11 \u{2014} Old resolved observation\n\u{2192} Resolved: fixed.\n",
    )
    .unwrap();

    let output = fluent_cmd()
        .current_dir(tmp.path())
        .args(["observation", "migrate"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "migrate should succeed: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Monolithic files removed
    assert!(
        !fluent.join("observations.md").exists(),
        "observations.md should be removed"
    );
    assert!(
        !fluent.join("observations-resolved.md").exists(),
        "observations-resolved.md should be removed"
    );

    // Per-file layout exists
    let obs_dir = fluent.join("observations");
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
    let output2 = fluent_cmd()
        .current_dir(tmp.path())
        .args(["observation", "migrate"])
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

    let output = LoggedCommand::cargo_bin("fluent")
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
        content.contains("command: fluent version"),
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
fn log_command_skips_on_fluent_tests_skip_log() {
    let log_dir = log::test_log_dir_path();
    let test_name = log::test_current_test_name();
    let log_path = log_dir.join(format!("{test_name}.log"));

    let _ = fs::remove_file(&log_path);

    // SAFETY: this test runs a single LoggedCommand synchronously and
    // restores the variable immediately; no other thread reads
    // FLUENT_TESTS_SKIP_LOG in this window.
    unsafe { std::env::set_var("FLUENT_TESTS_SKIP_LOG", "1") };
    let output = LoggedCommand::cargo_bin("fluent")
        .arg("version")
        .output()
        .unwrap();
    unsafe { std::env::remove_var("FLUENT_TESTS_SKIP_LOG") };

    assert!(output.status.success());
    assert!(
        !log_path.exists(),
        "log file should NOT be created when skip is set"
    );
}

#[test]
#[serial(env_skip_log)]
fn log_command_failed_command_appends_to_failed_sentinel() {
    log::clear_failed_sentinel();
    let log_dir = log::test_log_dir_path();
    let _ = fs::create_dir_all(&log_dir);

    let failed_path = log_dir.join(".failed");

    let tmp = TempDir::new().unwrap();
    let output = LoggedCommand::cargo_bin("fluent")
        .current_dir(tmp.path())
        .args(["work-item", "show", "nonexistent-work-item-for-test"])
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
    let output = fluent_cmd()
        .current_dir(tmp.path())
        .args(["auto-merge", "some-work-item", "--all"])
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
    let output = fluent_cmd()
        .current_dir(tmp.path())
        .args(["auto-merge"])
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
    let output = fluent_cmd()
        .current_dir(tmp.path())
        .args(["auto-merge", "nonexistent-work-item"])
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
    fluent_cmd()
        .current_dir(tmp.path())
        .args(["work-item", "create", "wi-skip", "--title", "Test skip"])
        .output()
        .unwrap();

    // Write a completed attempt with review_state passed
    let attempt_dir = tmp.path().join(".fluent/work/attempts/wi-skip");
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
    let mc_dir = tmp.path().join(".fluent/work/merge-candidates/wi-skip");
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

    let child = std::process::Command::new(assert_cmd::cargo::cargo_bin("fluent"))
        .current_dir(tmp.path())
        .args(["auto-merge", "wi-skip", "--poll-seconds", "1"])
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

    fluent_cmd()
        .current_dir(tmp.path())
        .args(["work-item", "create", "wi-sig", "--title", "Test signal"])
        .output()
        .unwrap();

    let mut child = std::process::Command::new(assert_cmd::cargo::cargo_bin("fluent"))
        .current_dir(tmp.path())
        .args(["auto-merge", "wi-sig", "--poll-seconds", "1"])
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
        .args([
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--allow-empty",
            "-m",
            "init",
        ])
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
        elapsed.as_millis() < 2000,
        "should not have slept for retries (exact retry behavior covered by git_wrapper_retries_on_lock_error), elapsed: {elapsed:?}"
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
        ".fluent/runs",
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
        ".fluent/runs",
        "fluent run ",
        "fluent resume",
        "fluent watch",
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
            // Exclude the DeleteLegacyRunModel EARS section (starts at the
            // `## DeleteLegacyRunModel` heading). Compute the boundary at
            // runtime so future edits to behaviors.md don't break this test.
            let behaviors = fs::read_to_string(project_root.join("documentation/behaviors.md"))
                .unwrap_or_default();
            let section_start = behaviors
                .lines()
                .position(|line| line.contains("## DeleteLegacyRunModel"))
                .map(|i| i + 1)
                .unwrap_or(0);
            line_num < section_start
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

    let allowed_prefixes = ["write-", "review-", "rebase-", "seed-", "learner-"];
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
            "Unexpected prompt file: {name}. Only write-*, review-*, rebase-*, seed-*, and learner-* prompts should exist."
        );
    }
}

// =========================================================================
// CLI verification tests — deleted subcommands absent from help
// =========================================================================

#[test]
fn deleted_subcommands_absent_from_help() {
    let output = fluent_cmd().args(["--help"]).output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let in_commands = stdout
        .lines()
        .skip_while(|line| !line.contains("Commands:"))
        .take_while(|line| !line.is_empty() || line.contains("Commands:"))
        .collect::<Vec<_>>()
        .join("\n");
    for cmd in ["run", "resume", "watch", "summary", "pull", "shell"] {
        assert!(
            !in_commands.lines().any(|line| line.trim().starts_with(cmd)),
            "Deleted subcommand {cmd:?} should not appear in Commands section:\n{in_commands}"
        );
    }
    assert!(
        in_commands.contains("work-item"),
        "work-item subcommand should appear"
    );
    assert!(
        in_commands.contains("status"),
        "status subcommand should appear"
    );
    assert!(
        in_commands.contains("review"),
        "review subcommand should appear"
    );
    assert!(
        in_commands.contains("merge-candidate"),
        "merge-candidate subcommand should appear"
    );
}

// =========================================================================
// Queue CLI tests
// =========================================================================

#[test]
fn work_queue_add_and_list_round_trip() {
    let tmp = TempDir::new().unwrap();
    write_work_item_json(tmp.path(), "wi-q1", "Queue test");

    fluent_cmd()
        .current_dir(tmp.path())
        .args(["queue", "add", "wi-q1", "--priority", "5"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Queued Work Item wi-q1"));

    fluent_cmd()
        .current_dir(tmp.path())
        .args(["queue", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("5"))
        .stdout(predicate::str::contains("queued"))
        .stdout(predicate::str::contains("wi-q1"));
}

#[test]
fn work_queue_add_unknown_work_item_errors() {
    let tmp = TempDir::new().unwrap();
    fs::create_dir_all(tmp.path().join(".fluent/work/items")).unwrap();

    fluent_cmd()
        .current_dir(tmp.path())
        .args(["queue", "add", "nonexistent"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("not found"));
}

#[test]
fn work_queue_add_existing_with_priority_updates_only_priority() {
    let tmp = TempDir::new().unwrap();
    write_work_item_json(tmp.path(), "wi-q2", "Priority update");

    fluent_cmd()
        .current_dir(tmp.path())
        .args(["queue", "add", "wi-q2", "--priority", "3"])
        .assert()
        .success();

    fluent_cmd()
        .current_dir(tmp.path())
        .args(["queue", "add", "wi-q2", "--priority", "10"])
        .assert()
        .success();

    let output = fluent_cmd()
        .current_dir(tmp.path())
        .args(["queue", "list"])
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

    fluent_cmd()
        .current_dir(tmp.path())
        .args(["queue", "add", "wi-fmt"])
        .assert()
        .success();

    let output = fluent_cmd()
        .current_dir(tmp.path())
        .args(["queue", "list"])
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

    fluent_cmd()
        .current_dir(tmp.path())
        .args(["queue", "add", "wi-rm"])
        .assert()
        .success();

    fluent_cmd()
        .current_dir(tmp.path())
        .args(["queue", "remove", "wi-rm"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Removed wi-rm"));

    fluent_cmd()
        .current_dir(tmp.path())
        .args(["queue", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("empty"));
}

#[test]
fn work_queue_remove_unknown_errors() {
    let tmp = TempDir::new().unwrap();
    fs::create_dir_all(tmp.path().join(".fluent/work/items")).unwrap();

    fluent_cmd()
        .current_dir(tmp.path())
        .args(["queue", "remove", "nonexistent"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("not queued"));
}

// =========================================================================
// Queue command matrix (behavior-scoped)
// =========================================================================

fn latest_dispatch_status(project_root: &Path, work_item_id: &str) -> String {
    let value = read_json_path(
        &project_root
            .join(".fluent/work/queue")
            .join(format!("{work_item_id}.json")),
    );
    value["dispatches"]
        .as_array()
        .and_then(|d| d.last())
        .and_then(|d| d["status"].as_str())
        .unwrap_or("")
        .to_string()
}

fn dispatch_count(project_root: &Path, work_item_id: &str) -> usize {
    let value = read_json_path(
        &project_root
            .join(".fluent/work/queue")
            .join(format!("{work_item_id}.json")),
    );
    value["dispatches"].as_array().map(|d| d.len()).unwrap_or(0)
}

#[test]
fn explicit_queue_add_after_terminal_creates_new_dispatch() {
    use fluent::queue::{self, DispatchStatus};

    let tmp = TempDir::new().unwrap();
    write_work_item_json(tmp.path(), "wi-term", "Requeue after terminal");

    // First dispatch, driven to a terminal disposition.
    fluent_cmd()
        .current_dir(tmp.path())
        .args(["queue", "add", "wi-term"])
        .assert()
        .success();
    let token = queue::claim(tmp.path(), "wi-term", "attempt-1")
        .unwrap()
        .unwrap();
    queue::reconcile(tmp.path(), &token, DispatchStatus::Failed).unwrap();
    assert_eq!(latest_dispatch_status(tmp.path(), "wi-term"), "failed");

    // An explicit re-add opens a new active dispatch while keeping history.
    fluent_cmd()
        .current_dir(tmp.path())
        .args(["queue", "add", "wi-term"])
        .assert()
        .success();

    assert_eq!(
        dispatch_count(tmp.path(), "wi-term"),
        2,
        "history preserved"
    );
    assert_eq!(latest_dispatch_status(tmp.path(), "wi-term"), "queued");
}

#[test]
fn queue_add_unknown_work_item_errors() {
    let tmp = TempDir::new().unwrap();
    fs::create_dir_all(tmp.path().join(".fluent/work/items")).unwrap();

    fluent_cmd()
        .current_dir(tmp.path())
        .args(["queue", "add", "ghost"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("not found"));

    assert!(
        !tmp.path().join(".fluent/work/queue/ghost.json").exists(),
        "no queue entry is created for an unknown Work Item"
    );
}

#[test]
fn queue_add_ineligible_work_errors() {
    let tmp = TempDir::new().unwrap();
    let items = tmp.path().join(".fluent/work/items");
    fs::create_dir_all(&items).unwrap();
    // A proposed Work Item is visible but not authorized to execute.
    fs::write(
        items.join("wi-proposed.json"),
        r#"{"id":"wi-proposed","title":"Proposed","authorization":{"state":"proposed"}}"#,
    )
    .unwrap();

    fluent_cmd()
        .current_dir(tmp.path())
        .args(["queue", "add", "wi-proposed"])
        .assert()
        .failure();

    assert!(
        !tmp.path()
            .join(".fluent/work/queue/wi-proposed.json")
            .exists(),
        "no queue entry is created for proposed Work"
    );
}

#[test]
fn queue_add_rejects_suspended_attempt_and_pending_candidate() {
    use fluent::work_model::{
        AttemptReviewState, AttemptStatus, MergeCandidateMergeStatus, TaskOutput, TaskStatus,
        WorkItem, WorkModelStore, WorkspaceAccess, WorkspaceRef,
    };

    // A Work Item whose Attempt is suspended at needs-user.
    let suspended = TempDir::new().unwrap();
    {
        let store = WorkModelStore::new(suspended.path());
        let mut item = WorkItem::planned("wi-susp", "Suspended");
        item.add_initial_attempt("attempt-1").unwrap();
        item.attempts[0].status = AttemptStatus::NeedsUser;
        item.attempts[0].tasks[0].status = TaskStatus::NeedsUser;
        store.create_work_item(&item).unwrap();
    }
    fluent_cmd()
        .current_dir(suspended.path())
        .args(["queue", "add", "wi-susp"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("needs-user"));
    assert!(
        !suspended
            .path()
            .join(".fluent/work/queue/wi-susp.json")
            .exists()
    );

    // A Work Item whose Merge Candidate is pending land.
    let pending = TempDir::new().unwrap();
    {
        let store = WorkModelStore::new(pending.path());
        let mut item = WorkItem::planned("wi-pend", "Pending land");
        item.add_initial_attempt("attempt-1").unwrap();
        let attempt = item.attempts.last_mut().unwrap();
        let task = attempt.tasks.last_mut().unwrap();
        task.status = TaskStatus::Complete;
        task.workspace_access = WorkspaceAccess {
            reads: vec![WorkspaceRef {
                id: "target".to_string(),
                path: ".".to_string(),
            }],
            writes: vec![WorkspaceRef {
                id: "candidate".to_string(),
                path: "../work-wi-pend-attempt-1".to_string(),
            }],
        };
        task.output = Some(TaskOutput {
            workspace_id: "candidate".to_string(),
            workspace_path: "../work-wi-pend-attempt-1".to_string(),
            source_branch: "main".to_string(),
            base_commit: None,
            commit: "abc123".to_string(),
        });
        attempt.status = AttemptStatus::Complete;
        attempt.review_state = Some(AttemptReviewState::Passed);
        item.create_or_get_merge_candidate("attempt-1").unwrap();
        assert_eq!(
            item.merge_candidates[0].merge_state.status,
            MergeCandidateMergeStatus::Pending
        );
        store.create_work_item(&item).unwrap();
    }
    fluent_cmd()
        .current_dir(pending.path())
        .args(["queue", "add", "wi-pend"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("pending land"));
    assert!(
        !pending
            .path()
            .join(".fluent/work/queue/wi-pend.json")
            .exists()
    );
}

#[test]
fn queue_list_shows_priority_time_status_and_work_id() {
    let tmp = TempDir::new().unwrap();
    write_work_item_json(tmp.path(), "wi-list", "List format");

    fluent_cmd()
        .current_dir(tmp.path())
        .args(["queue", "add", "wi-list", "--priority", "7"])
        .assert()
        .success();

    let output = fluent_cmd()
        .current_dir(tmp.path())
        .args(["queue", "list"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let line = stdout
        .lines()
        .find(|l| l.contains("wi-list"))
        .expect("wi-list listed");
    assert!(line.starts_with("7 "), "priority first: {line}");
    assert!(line.contains('T'), "queue time present: {line}");
    assert!(line.contains("queued"), "status present: {line}");
    assert!(line.contains("wi-list"), "work item id present: {line}");
}

// =========================================================================
// Scheduler CLI tests
// =========================================================================

#[test]
fn work_scheduler_run_exits_clean_on_sigterm_when_idle() {
    let tmp = TempDir::new().unwrap();
    fs::create_dir_all(tmp.path().join(".fluent/work/items")).unwrap();

    let mut child = std::process::Command::new(assert_cmd::cargo::cargo_bin("fluent"))
        .current_dir(tmp.path())
        .args(["scheduler", "run", "--poll-seconds", "1"])
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

    fluent_cmd()
        .current_dir(project)
        .args(["queue", "add", "wi-sched", "--priority", "1"])
        .assert()
        .success();

    let queue_entry_path = project.join(".fluent/work/queue/wi-sched.json");
    assert!(queue_entry_path.exists());
    let before: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&queue_entry_path).unwrap()).unwrap();
    let before_dispatches = before["dispatches"].as_array().expect("dispatch ledger");
    assert_eq!(before_dispatches.last().unwrap()["status"], "queued");

    let child = std::process::Command::new(assert_cmd::cargo::cargo_bin("fluent"))
        .current_dir(project)
        .env(
            "PATH",
            format!(
                "{}:{}",
                bin_dir.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .args(["scheduler", "run", "--poll-seconds", "1"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();

    let latest_status = |value: &serde_json::Value| -> String {
        value["dispatches"]
            .as_array()
            .and_then(|d| d.last())
            .and_then(|d| d["status"].as_str())
            .unwrap_or("")
            .to_string()
    };

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        std::thread::sleep(std::time::Duration::from_millis(500));
        let entry: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&queue_entry_path).unwrap()).unwrap();
        let s = latest_status(&entry);
        if s == "candidate-ready" || s == "failed" {
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
    let status = latest_status(&after);
    assert!(
        status == "candidate-ready" || status == "failed",
        "queue entry should be terminal after scheduler runs, got: {status}"
    );
}

/// A per-run token, derived from the unique temp directory name, so attempt
/// worktrees (created as `../work-*-<id>-*` siblings in the shared temp root)
/// never collide across test runs or with orphans from a killed scheduler.
fn run_token(tmp: &TempDir) -> String {
    tmp.path()
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("run")
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .to_lowercase()
}

/// Best-effort removal of the sibling attempt worktrees a scheduler run created,
/// so a hard-killed scheduler does not leave orphans behind.
fn remove_sibling_worktrees(project: &Path, token: &str) {
    if let Some(parent) = project.parent() {
        if let Ok(entries) = fs::read_dir(parent) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name.starts_with("work-") && name.contains(token) {
                    let _ = fs::remove_dir_all(entry.path());
                }
            }
        }
    }
}

/// Count Work Items whose latest dispatch is claimed or running.
fn count_active_dispatches(queue_dir: &Path) -> usize {
    let mut active = 0;
    let Ok(entries) = fs::read_dir(queue_dir) else {
        return 0;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        let status = value["dispatches"]
            .as_array()
            .and_then(|d| d.last())
            .and_then(|d| d["status"].as_str())
            .unwrap_or("");
        if status == "claimed" || status == "running" {
            active += 1;
        }
    }
    active
}

#[test]
fn scheduler_runs_four_work_items_concurrently_by_default() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path();
    init_git_repo(project);

    // A writer that blocks until released, so the claimed set stays stable long
    // enough to observe concurrency even under heavy parallel test load.
    let release = project.join("release");
    let bin_dir = project.join("mock-bin");
    write_mock_claude(
        &bin_dir,
        r#"#!/bin/bash
for _ in $(seq 1 600); do
  [ -f "$FLUENT_TEST_RELEASE" ] && break
  sleep 0.1
done
git add -A 2>/dev/null
git commit --allow-empty -m "mock write" 2>/dev/null
exit 0
"#,
    );

    let token = run_token(&tmp);
    for n in 0..6 {
        let id = format!("wc{token}{n}");
        write_work_item_json(project, &id, "Concurrency");
        fluent_cmd()
            .current_dir(project)
            .args(["queue", "add", &id])
            .assert()
            .success();
    }

    let queue_dir = project.join(".fluent/work/queue");
    let mut child = std::process::Command::new(assert_cmd::cargo::cargo_bin("fluent"))
        .current_dir(project)
        .env(
            "PATH",
            format!(
                "{}:{}",
                bin_dir.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .env("FLUENT_TEST_RELEASE", &release)
        .args(["scheduler", "run", "--poll-seconds", "1"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();

    // The default capacity is four; the blocked writers hold their slots so the
    // count stably reaches four and never exceeds it while two stay queued.
    let mut max_active = 0;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while std::time::Instant::now() < deadline {
        let active = count_active_dispatches(&queue_dir);
        max_active = max_active.max(active);
        assert!(
            active <= 4,
            "local scheduler must never exceed its capacity of four, saw {active}"
        );
        if max_active == 4 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(150));
    }

    // Release the blocked writers, then stop the scheduler.
    fs::write(&release, b"go").unwrap();
    let _ = child.kill();
    let _ = child.wait();
    remove_sibling_worktrees(project, &token);

    assert_eq!(
        max_active, 4,
        "scheduler should fill the default capacity of four concurrently"
    );
}

#[test]
fn second_scheduler_run_reuses_live_coordinator() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path();
    fs::create_dir_all(project.join(".fluent/work/items")).unwrap();

    // Start a live coordinator that idles while holding its lease.
    let mut first = std::process::Command::new(assert_cmd::cargo::cargo_bin("fluent"))
        .current_dir(project)
        .args(["scheduler", "run", "--poll-seconds", "5"])
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_secs(2));

    // A second start finds the live coordinator, reports reuse, and returns.
    fluent_cmd()
        .current_dir(project)
        .args(["scheduler", "run", "--poll-seconds", "5"])
        .assert()
        .success()
        .stderr(predicate::str::contains("reusing live coordinator"));

    send_signal(first.id(), "TERM");
    let status = first.wait().unwrap();
    assert!(status.success(), "the live coordinator exits cleanly");
}

#[test]
fn failed_scheduled_work_requeues_as_new_attempt_without_losing_history() {
    use fluent::queue::{self, DispatchStatus};
    use fluent::work_model::{AttemptStatus, WorkItem, WorkModelStore};

    let tmp = TempDir::new().unwrap();
    let project = tmp.path();
    let store = WorkModelStore::new(project);
    store
        .create_work_item(&WorkItem::planned("wi-req", "Requeue after failure"))
        .unwrap();
    queue::add(project, "wi-req", None).unwrap();

    // A scheduled dispatch fails: its bound Attempt is failed and the dispatch
    // reconciles to failed.
    let token = queue::claim(project, "wi-req", "attempt-1")
        .unwrap()
        .unwrap();
    let mut item = store.read_work_item("wi-req").unwrap();
    item.add_initial_attempt("attempt-1").unwrap();
    item.attempts[0].status = AttemptStatus::Failed;
    store.write_work_item(&item).unwrap();
    queue::reconcile(project, &token, DispatchStatus::Failed).unwrap();

    // The recovery action is an explicit re-queue.
    fluent_cmd()
        .current_dir(project)
        .args(["queue", "add", "wi-req"])
        .assert()
        .success();

    let ledger = queue::read_ledger(project, "wi-req").unwrap().unwrap();
    // The failed dispatch survives as history; a fresh dispatch is active.
    assert_eq!(ledger.dispatches.len(), 2);
    assert_eq!(ledger.dispatches[0].status, DispatchStatus::Failed);
    assert_eq!(
        ledger.dispatches[0].bound_attempt_id.as_deref(),
        Some("attempt-1")
    );
    let active = ledger.active().unwrap();
    assert_eq!(active.status, DispatchStatus::Queued);

    // Claiming the new dispatch binds a fresh Attempt distinct from the failed
    // one.
    let next = queue::claim(project, "wi-req", "attempt-2")
        .unwrap()
        .unwrap();
    assert_eq!(next.bound_attempt_id, "attempt-2");
}

#[test]
fn descendant_execution_failure_preserves_land_and_reports_recovery() {
    use fluent::queue::{self, DispatchStatus};
    use fluent::work_model::{
        AttemptStatus, CorrectiveContext, DerivedProvenance, ExecutionAuthority, WorkItem,
        WorkLineage, WorkModelStore,
    };

    let tmp = TempDir::new().unwrap();
    let project = tmp.path();
    let store = WorkModelStore::new(project);

    // An autonomous descendant derived from a landed correction.
    let provenance = DerivedProvenance {
        work_item_id: Some("root-wi".to_string()),
        merged_commit: Some("deadbeef".to_string()),
        ..Default::default()
    };
    let context = CorrectiveContext {
        objective: "Restore the retry cap".to_string(),
        requirement: "Retries stop after the configured cap".to_string(),
        evidence: "Merged commit deadbeef removed the cap".to_string(),
        included_scope: "src/retry.rs".to_string(),
        excluded_scope: "unrelated tuning".to_string(),
        verification: "cargo test retry::cap".to_string(),
    };
    let lineage = WorkLineage {
        root_id: Some("root-wi".to_string()),
        ..Default::default()
    };
    let item = WorkItem::derived_corrective(
        "wi-desc",
        "Autonomous descendant",
        provenance,
        context,
        lineage,
        Some(ExecutionAuthority::Automatic),
    )
    .unwrap();
    store.create_work_item(&item).unwrap();

    fluent_cmd()
        .current_dir(project)
        .args(["queue", "add", "wi-desc"])
        .assert()
        .success();

    // The scheduled Attempt fails.
    let token = queue::claim(project, "wi-desc", "attempt-1")
        .unwrap()
        .unwrap();
    let mut item = store.read_work_item("wi-desc").unwrap();
    item.add_initial_attempt("attempt-1").unwrap();
    item.attempts[0].status = AttemptStatus::Failed;
    store.write_work_item(&item).unwrap();
    queue::reconcile(project, &token, DispatchStatus::Failed).unwrap();

    // Durable failure evidence: the dispatch is failed.
    assert_eq!(
        queue::read_ledger(project, "wi-desc")
            .unwrap()
            .unwrap()
            .latest()
            .unwrap()
            .status,
        DispatchStatus::Failed
    );

    // The originating land is untouched: the derived provenance still points at
    // the merged commit.
    let after = store.read_work_item("wi-desc").unwrap();
    assert_eq!(
        after
            .origin
            .provenance()
            .and_then(|p| p.merged_commit.as_deref()),
        Some("deadbeef")
    );

    // Status exposes the failure and its recovery action.
    let output = fluent_cmd()
        .current_dir(project)
        .args(["status"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("wi-desc"), "status lists the descendant");
    assert!(
        stdout.contains("failed"),
        "status reports the failed outcome: {stdout}"
    );
}

#[test]
fn idle_scheduler_shutdown_exits_immediately() {
    let tmp = TempDir::new().unwrap();
    fs::create_dir_all(tmp.path().join(".fluent/work/items")).unwrap();

    // A long poll interval would delay a naive shutdown; an idle scheduler must
    // exit promptly on SIGTERM regardless.
    let mut child = std::process::Command::new(assert_cmd::cargo::cargo_bin("fluent"))
        .current_dir(tmp.path())
        .args(["scheduler", "run", "--poll-seconds", "30"])
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();

    std::thread::sleep(std::time::Duration::from_millis(1500));
    let signalled = std::time::Instant::now();
    send_signal(child.id(), "TERM");
    let status = child.wait().unwrap();
    let elapsed = signalled.elapsed();

    assert!(status.success(), "idle scheduler exits cleanly");
    assert!(
        elapsed < std::time::Duration::from_secs(10),
        "idle scheduler exits without waiting the poll interval, took {elapsed:?}"
    );
}

#[test]
fn status_reports_queued_work_without_live_scheduler() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path();
    write_work_item_json(project, "wi-waiting", "Waiting Work");

    fluent_cmd()
        .current_dir(project)
        .args(["queue", "add", "wi-waiting"])
        .assert()
        .success();

    // No coordinator is running, so status must flag stopped execution and name
    // the start action.
    fluent_cmd()
        .current_dir(project)
        .args(["status"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Execution is stopped"))
        .stdout(predicate::str::contains("fluent scheduler run"));
}

#[test]
fn scheduler_restart_processes_durable_waiting_work() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path();
    init_git_repo(project);

    // One slot at a time so a second Work Item waits durably.
    let config_dir = project.join(".fluent");
    fs::create_dir_all(&config_dir).unwrap();
    fs::write(
        config_dir.join("config.yaml"),
        "scheduler:\n  local-concurrency: 1\n",
    )
    .unwrap();

    let bin_dir = project.join("mock-bin");
    write_mock_claude(
        &bin_dir,
        r#"#!/bin/bash
sleep 2
git add -A 2>/dev/null
git commit --allow-empty -m "mock write" 2>/dev/null
exit 0
"#,
    );
    let path_env = format!(
        "{}:{}",
        bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let token = run_token(&tmp);
    let wi_a = format!("wa{token}");
    let wi_b = format!("wb{token}");
    for id in [&wi_a, &wi_b] {
        write_work_item_json(project, id, "Restart");
        fluent_cmd()
            .current_dir(project)
            .args(["queue", "add", id])
            .assert()
            .success();
    }

    let is_terminal = |id: &str| {
        let s = latest_dispatch_status(project, id);
        s == "candidate-ready" || s == "failed"
    };

    // First coordinator: processes one Work Item, then is stopped.
    let first = std::process::Command::new(assert_cmd::cargo::cargo_bin("fluent"))
        .current_dir(project)
        .env("PATH", &path_env)
        .args(["scheduler", "run", "--poll-seconds", "1"])
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while !(is_terminal(&wi_a) || is_terminal(&wi_b)) {
        assert!(
            std::time::Instant::now() < deadline,
            "first coordinator processed nothing"
        );
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    send_signal(first.id(), "TERM");
    let first_status = first.wait_with_output().unwrap();
    assert!(
        first_status.status.success(),
        "first coordinator drains cleanly"
    );

    // The remaining Work stayed durably queued; a restart processes it.
    let second = std::process::Command::new(assert_cmd::cargo::cargo_bin("fluent"))
        .current_dir(project)
        .env("PATH", &path_env)
        .args(["scheduler", "run", "--poll-seconds", "1"])
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(40);
    while !(is_terminal(&wi_a) && is_terminal(&wi_b)) {
        assert!(
            std::time::Instant::now() < deadline,
            "restart did not process the durable waiting Work"
        );
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    send_signal(second.id(), "TERM");
    let _ = second.wait_with_output().unwrap();
    remove_sibling_worktrees(project, &token);

    assert!(is_terminal(&wi_a) && is_terminal(&wi_b));
}

// ---------------------------------------------------------------------------
// B1–B5: Read commands for attempts, merge candidates, and tasks
// ---------------------------------------------------------------------------

fn write_rich_work_item(project_root: &Path) {
    let base = project_root.join(".fluent/work");

    let items_dir = base.join("items");
    fs::create_dir_all(&items_dir).unwrap();
    fs::write(
        items_dir.join("wi-read.json"),
        r#"{"id": "wi-read", "title": "Read test"}"#,
    )
    .unwrap();

    let attempts_dir = base.join("attempts/wi-read");
    fs::create_dir_all(&attempts_dir).unwrap();
    fs::write(
        attempts_dir.join("attempt-1.json"),
        r#"{"id": "attempt-1", "work_item_id": "wi-read", "order": 0, "status": "complete", "review_state": "passed"}"#,
    )
    .unwrap();
    fs::write(
        attempts_dir.join("attempt-2.json"),
        r#"{"id": "attempt-2", "work_item_id": "wi-read", "order": 1, "status": "executing"}"#,
    )
    .unwrap();

    let tasks_dir = base.join("tasks/wi-read/attempt-1");
    fs::create_dir_all(&tasks_dir).unwrap();
    fs::write(
        tasks_dir.join("attempt-1-write-1.json"),
        r#"{
  "order": 0,
  "id": "attempt-1-write-1",
  "kind": "write",
  "status": "complete",
  "role": "writer",
  "work_item_id": "wi-read",
  "attempt_id": "attempt-1",
  "workspace_access": { "reads": [], "writes": [{"id": "ws-1", "path": "/tmp/ws"}] },
  "output": {
    "workspace_id": "ws-1",
    "workspace_path": "/tmp/ws",
    "source_branch": "work/wi-read/attempt-1",
    "commit": "abc123"
  }
}"#,
    )
    .unwrap();
    fs::write(
        tasks_dir.join("attempt-1-review-1.json"),
        r#"{
  "order": 1,
  "id": "attempt-1-review-1",
  "kind": "review",
  "status": "complete",
  "role": "reviewer",
  "work_item_id": "wi-read",
  "attempt_id": "attempt-1",
  "workspace_access": { "reads": [{"id": "ws-1", "path": "/tmp/ws"}], "writes": [] },
  "artifact_area": { "path": ".fluent/work/artifacts/wi-read/attempt-1/attempt-1-review-1" },
  "review_context": {
    "candidate_workspace_id": "ws-1",
    "candidate_workspace_path": "/tmp/ws",
    "source_branch": "work/wi-read/attempt-1",
    "candidate_commit": "abc123"
  }
}"#,
    )
    .unwrap();

    let mc_dir = base.join("merge-candidates/wi-read");
    fs::create_dir_all(&mc_dir).unwrap();
    fs::write(
        mc_dir.join("mc-1.json"),
        r#"{
  "id": "mc-1",
  "attempt_id": "attempt-1",
  "source_workspace": {"id": "ws-1", "path": "/tmp/ws"},
  "target_workspace": {"id": "target", "path": "."},
  "source_branch": "work/wi-read/attempt-1",
  "target_branch": "work/wi-read/attempt-1",
  "candidate_commit": "abc123",
  "review_state": "passed",
  "merge_state": {"status": "pending"}
}"#,
    )
    .unwrap();
}

#[test]
fn attempt_list_prints_attempts() {
    let tmp = TempDir::new().unwrap();
    write_rich_work_item(tmp.path());

    let output = fluent_cmd()
        .current_dir(tmp.path())
        .args(["attempt", "list", "wi-read"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("attempt-1"),
        "should list attempt-1: {stdout}"
    );
    assert!(
        stdout.contains("complete"),
        "should show complete status: {stdout}"
    );
    assert!(
        stdout.contains("attempt-2"),
        "should list attempt-2: {stdout}"
    );
    assert!(
        stdout.contains("executing"),
        "should show executing status: {stdout}"
    );
}

#[test]
fn attempt_show_prints_attempt_json() {
    let tmp = TempDir::new().unwrap();
    write_rich_work_item(tmp.path());

    let output = fluent_cmd()
        .current_dir(tmp.path())
        .args(["attempt", "show", "wi-read", "attempt-1"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["id"], "attempt-1");
    assert_eq!(value["status"], "complete");
    assert!(value["tasks"].as_array().unwrap().len() == 2);
}

#[test]
fn merge_candidate_list_prints_candidates() {
    let tmp = TempDir::new().unwrap();
    write_rich_work_item(tmp.path());

    let output = fluent_cmd()
        .current_dir(tmp.path())
        .args(["merge-candidate", "list", "wi-read"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("mc-1"), "should list mc-1: {stdout}");
    assert!(
        stdout.contains("passed"),
        "should show review state: {stdout}"
    );
    assert!(
        stdout.contains("pending"),
        "should show merge status: {stdout}"
    );
}

#[test]
fn task_list_prints_tasks() {
    let tmp = TempDir::new().unwrap();
    write_rich_work_item(tmp.path());

    let output = fluent_cmd()
        .current_dir(tmp.path())
        .args(["task", "list", "wi-read", "attempt-1"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("attempt-1-write-1"),
        "should list write task: {stdout}"
    );
    assert!(stdout.contains("write"), "should show write kind: {stdout}");
    assert!(
        stdout.contains("attempt-1-review-1"),
        "should list review task: {stdout}"
    );
    assert!(
        stdout.contains("review"),
        "should show review kind: {stdout}"
    );
}

#[test]
fn task_show_prints_task_json() {
    let tmp = TempDir::new().unwrap();
    write_rich_work_item(tmp.path());

    let output = fluent_cmd()
        .current_dir(tmp.path())
        .args(["task", "show", "wi-read", "attempt-1", "attempt-1-write-1"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["id"], "attempt-1-write-1");
    assert_eq!(value["kind"], "write");
    assert_eq!(value["status"], "complete");
}

// -------------------------------------------------------------------------
// Update: helpers
// -------------------------------------------------------------------------

fn target_triple() -> String {
    let arch = std::env::consts::ARCH;
    match std::env::consts::OS {
        "macos" => format!("{arch}-apple-darwin"),
        "linux" => format!("{arch}-unknown-linux-gnu"),
        other => format!("{arch}-{other}"),
    }
}

/// Build a fixture release directory that `fluent update` can fetch via
/// `file://` URLs through curl. Returns (api_base, release_repo) env values.
///
/// The fixture contains:
/// - `repos/{owner}/{repo}/releases/latest` — GitHub API JSON
/// - `download/v{version}/fluent-{triple}` — the binary asset
fn setup_fixture_release(dir: &Path, version: &str, binary_content: &[u8]) -> (String, String) {
    let owner = "test-owner";
    let repo = "fluent";
    let triple = target_triple();
    let asset_name = format!("fluent-{triple}");
    let tag = format!("v{version}");

    let download_dir = dir.join("download").join(&tag);
    fs::create_dir_all(&download_dir).unwrap();

    let binary_path = download_dir.join(&asset_name);
    fs::write(&binary_path, binary_content).unwrap();

    let binary_url = format!("file://{}", binary_path.to_string_lossy());

    let assets = vec![serde_json::json!({
        "name": asset_name,
        "browser_download_url": binary_url,
    })];

    let release_json = serde_json::json!({
        "tag_name": tag,
        "assets": assets,
    });

    let api_dir = dir.join("repos").join(owner).join(repo).join("releases");
    fs::create_dir_all(&api_dir).unwrap();
    fs::write(
        api_dir.join("latest"),
        serde_json::to_string_pretty(&release_json).unwrap(),
    )
    .unwrap();

    let api_base = format!("file://{}", dir.to_string_lossy());
    let release_repo = format!("{owner}/{repo}");
    (api_base, release_repo)
}

// -------------------------------------------------------------------------
// Update: performing an update
// -------------------------------------------------------------------------

#[test]
fn update_reports_up_to_date() {
    let tmp = TempDir::new().unwrap();
    let fixture_dir = tmp.path().join("fixture");
    fs::create_dir_all(&fixture_dir).unwrap();

    let current_version = env!("CARGO_PKG_VERSION");
    let (api_base, release_repo) =
        setup_fixture_release(&fixture_dir, current_version, b"binary-content");

    let output = fluent_cmd()
        .current_dir(tmp.path())
        .env("FLUENT_API_BASE", &api_base)
        .env("FLUENT_RELEASE_REPO", &release_repo)
        .env("FLUENT_NO_UPDATE_CHECK", "1")
        .arg("update")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "update should succeed when up to date: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("up to date"),
        "should report up to date; got stderr:\n{stderr}"
    );
}

#[test]
fn update_replaces_binary_and_rematerializes_skills() {
    let tmp = TempDir::new().unwrap();
    let fixture_dir = tmp.path().join("fixture");
    fs::create_dir_all(&fixture_dir).unwrap();

    let bin_dir = tmp.path().join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let fake_binary = bin_dir.join("fluent");
    fs::write(&fake_binary, b"old-binary-content").unwrap();

    let new_content = b"new-binary-content-v999";
    let (api_base, release_repo) = setup_fixture_release(&fixture_dir, "999.0.0", new_content);

    let cache_path = tmp.path().join("update-cache.json");

    let output = fluent_cmd()
        .current_dir(tmp.path())
        .env("FLUENT_API_BASE", &api_base)
        .env("FLUENT_RELEASE_REPO", &release_repo)
        .env("FLUENT_BINARY_PATH", fake_binary.to_str().unwrap())
        .env("FLUENT_UPDATE_CACHE_PATH", cache_path.to_str().unwrap())
        .env("FLUENT_NO_UPDATE_CHECK", "1")
        .arg("update")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "update should succeed: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );

    let replaced = fs::read(&fake_binary).unwrap();
    assert_eq!(
        replaced, new_content,
        "binary should be replaced with the downloaded content"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("999.0.0"),
        "should report the new version; got stderr:\n{stderr}"
    );

    assert!(
        stderr.contains("skills re-materialization"),
        "should attempt skills re-materialization; got stderr:\n{stderr}"
    );
}

#[test]
fn update_offline_preserves_binary() {
    let tmp = TempDir::new().unwrap();
    let bin_dir = tmp.path().join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let fake_binary = bin_dir.join("fluent");
    let original = b"original-binary";
    fs::write(&fake_binary, original).unwrap();

    let cache_path = tmp.path().join("update-cache.json");

    let output = fluent_cmd()
        .current_dir(tmp.path())
        .env("FLUENT_API_BASE", "file:///nonexistent/path")
        .env("FLUENT_RELEASE_REPO", "no-owner/no-repo")
        .env("FLUENT_BINARY_PATH", fake_binary.to_str().unwrap())
        .env("FLUENT_UPDATE_CACHE_PATH", cache_path.to_str().unwrap())
        .env("FLUENT_NO_UPDATE_CHECK", "1")
        .arg("update")
        .output()
        .unwrap();

    assert!(!output.status.success(), "update should fail when offline");

    let preserved = fs::read(&fake_binary).unwrap();
    assert_eq!(
        preserved, original,
        "binary should be preserved when offline"
    );
}

#[test]
fn update_replace_leaves_working_binary_on_failure() {
    let tmp = TempDir::new().unwrap();
    let fixture_dir = tmp.path().join("fixture");
    fs::create_dir_all(&fixture_dir).unwrap();

    let bin_dir = tmp.path().join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let fake_binary = bin_dir.join("fluent");
    let original = b"original-binary";
    fs::write(&fake_binary, original).unwrap();

    let triple = target_triple();
    let asset_name = format!("fluent-{triple}");
    let tag = "v999.0.0";

    let release_json = serde_json::json!({
        "tag_name": tag,
        "assets": [
            {
                "name": &asset_name,
                "browser_download_url": "file:///nonexistent/binary",
            },
        ],
    });

    let api_dir = fixture_dir.join("repos/test-owner/fluent/releases");
    fs::create_dir_all(&api_dir).unwrap();
    fs::write(
        api_dir.join("latest"),
        serde_json::to_string_pretty(&release_json).unwrap(),
    )
    .unwrap();

    let cache_path = tmp.path().join("update-cache.json");

    let output = fluent_cmd()
        .current_dir(tmp.path())
        .env(
            "FLUENT_API_BASE",
            format!("file://{}", fixture_dir.to_string_lossy()),
        )
        .env("FLUENT_RELEASE_REPO", "test-owner/fluent")
        .env("FLUENT_BINARY_PATH", fake_binary.to_str().unwrap())
        .env("FLUENT_UPDATE_CACHE_PATH", cache_path.to_str().unwrap())
        .env("FLUENT_NO_UPDATE_CHECK", "1")
        .arg("update")
        .output()
        .unwrap();

    assert!(
        !output.status.success(),
        "update should fail when download fails"
    );

    let preserved = fs::read(&fake_binary).unwrap();
    assert_eq!(
        preserved, original,
        "binary should be preserved when download fails"
    );
}

// -------------------------------------------------------------------------
// Update check and nudge
// -------------------------------------------------------------------------

#[test]
fn update_check_queries_update_endpoint() {
    let tmp = TempDir::new().unwrap();
    let fixture_dir = tmp.path().join("fixture");
    fs::create_dir_all(&fixture_dir).unwrap();

    let (api_base, release_repo) = setup_fixture_release(&fixture_dir, "999.0.0", b"new-binary");

    let cache_path = tmp.path().join("update-cache.json");

    let output = fluent_cmd()
        .current_dir(tmp.path())
        .env("FLUENT_API_BASE", &api_base)
        .env("FLUENT_RELEASE_REPO", &release_repo)
        .env("FLUENT_UPDATE_CACHE_PATH", cache_path.to_str().unwrap())
        .env_remove("FLUENT_NO_UPDATE_CHECK")
        .arg("version")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "update check should succeed via FLUENT_API_BASE override; stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("fluent update"),
        "update check through endpoint should produce nudge when behind; got stderr:\n{stderr}"
    );
}

#[test]
fn update_check_never_replaces_binary() {
    let tmp = TempDir::new().unwrap();
    let fixture_dir = tmp.path().join("fixture");
    fs::create_dir_all(&fixture_dir).unwrap();

    let bin_dir = tmp.path().join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let fake_binary = bin_dir.join("fluent");
    let original = b"original-binary";
    fs::write(&fake_binary, original).unwrap();

    let (api_base, release_repo) = setup_fixture_release(&fixture_dir, "999.0.0", b"new-binary");

    let cache_path = tmp.path().join("update-cache.json");

    // Run a non-update command (version) with the update check enabled.
    // The nudge may appear on stderr, but the binary must not change.
    let output = fluent_cmd()
        .current_dir(tmp.path())
        .env("FLUENT_API_BASE", &api_base)
        .env("FLUENT_RELEASE_REPO", &release_repo)
        .env("FLUENT_BINARY_PATH", fake_binary.to_str().unwrap())
        .env("FLUENT_UPDATE_CACHE_PATH", cache_path.to_str().unwrap())
        .env_remove("FLUENT_NO_UPDATE_CHECK")
        .arg("version")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "version command should succeed even with update available"
    );

    let preserved = fs::read(&fake_binary).unwrap();
    assert_eq!(
        preserved, original,
        "binary must not be replaced during an update check (B2)"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("fluent update"),
        "nudge should appear when behind; got stderr:\n{stderr}"
    );
}

#[test]
fn update_check_offline_is_silent_and_nonfatal() {
    let tmp = TempDir::new().unwrap();
    let cache_path = tmp.path().join("update-cache.json");

    let output = fluent_cmd()
        .current_dir(tmp.path())
        .env("FLUENT_API_BASE", "file:///nonexistent/path")
        .env("FLUENT_RELEASE_REPO", "no-owner/no-repo")
        .env("FLUENT_UPDATE_CACHE_PATH", cache_path.to_str().unwrap())
        .env_remove("FLUENT_NO_UPDATE_CHECK")
        .arg("version")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "command should succeed even when update check fails offline"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.to_lowercase().contains("error"),
        "offline check should not print errors; got stderr:\n{stderr}"
    );
}

#[test]
fn update_check_is_cached_within_interval() {
    let tmp = TempDir::new().unwrap();
    let cache_path = tmp.path().join("update-cache.json");

    // Write a fresh cache entry saying 999.0.0 is latest (which is "behind").
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let cache = serde_json::json!({
        "checked_at": now,
        "latest_version": "999.0.0",
    });
    fs::write(&cache_path, serde_json::to_string(&cache).unwrap()).unwrap();

    // Point the API at a nonexistent path — if the code queries the source,
    // it would fail. Since the cache is fresh, it should NOT query.
    let output = fluent_cmd()
        .current_dir(tmp.path())
        .env(
            "FLUENT_API_BASE",
            "file:///nonexistent/should-not-be-queried",
        )
        .env("FLUENT_RELEASE_REPO", "no-owner/no-repo")
        .env("FLUENT_UPDATE_CACHE_PATH", cache_path.to_str().unwrap())
        .env_remove("FLUENT_NO_UPDATE_CHECK")
        .arg("version")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "command should succeed with cached check"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("fluent update"),
        "cached check showing behind should print nudge; got stderr:\n{stderr}"
    );
}

#[test]
fn update_check_env_opt_out_suppresses_check_and_nudge() {
    let tmp = TempDir::new().unwrap();
    let cache_path = tmp.path().join("update-cache.json");

    // Write a cache saying we're behind.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let cache = serde_json::json!({
        "checked_at": now,
        "latest_version": "999.0.0",
    });
    fs::write(&cache_path, serde_json::to_string(&cache).unwrap()).unwrap();

    let output = fluent_cmd()
        .current_dir(tmp.path())
        .env("FLUENT_NO_UPDATE_CHECK", "1")
        .env("FLUENT_UPDATE_CACHE_PATH", cache_path.to_str().unwrap())
        .arg("version")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "version should succeed with opt-out"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("fluent update"),
        "opt-out should suppress nudge; got stderr:\n{stderr}"
    );
}

// ---------------------------------------------------------------------------
// Skills add
// ---------------------------------------------------------------------------

#[test]
fn skills_add_materializes_full_skill_and_references() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    fs::create_dir_all(&home).unwrap();

    fluent_cmd()
        .env("HOME", home.to_str().unwrap())
        .args(["skills", "add"])
        .assert()
        .success();

    let skills_dir = home.join(".claude/skills");
    let fluent_skill = skills_dir.join("fluent/SKILL.md");
    assert!(
        fluent_skill.exists(),
        "fluent/SKILL.md must exist after skills add"
    );

    let content = fs::read_to_string(&fluent_skill).unwrap();
    assert!(
        !content.contains("fluent-shim: true"),
        "materialized fluent skill must be the full skill, not the shim"
    );

    let refs_dir = skills_dir.join("fluent/references");
    assert!(
        refs_dir.is_dir(),
        "fluent/references/ must exist after skills add"
    );

    let review_skill = skills_dir.join("review-tests/SKILL.md");
    assert!(
        review_skill.exists(),
        "review skills must also be materialized"
    );
}

#[test]
fn skills_add_bare_is_alias_for_skills_add() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    fs::create_dir_all(&home).unwrap();

    fluent_cmd()
        .env("HOME", home.to_str().unwrap())
        .args(["skills"])
        .assert()
        .success();

    let fluent_skill = home.join(".claude/skills/fluent/SKILL.md");
    assert!(
        fluent_skill.exists(),
        "bare 'fluent skills' must install skills (backward compat)"
    );
}

#[test]
fn skills_add_replaces_shim_marked_directory() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");

    // Pre-install a shim-marked fluent skill in a secondary agent directory.
    let agent_skills = home.join(".codex/skills");
    let shim_dir = agent_skills.join("fluent");
    fs::create_dir_all(&shim_dir).unwrap();
    fs::write(
        shim_dir.join("SKILL.md"),
        "---\nname: fluent\nfluent-shim: true\n---\nShim content\n",
    )
    .unwrap();

    fluent_cmd()
        .env("HOME", home.to_str().unwrap())
        .args(["skills", "add"])
        .assert()
        .success();

    let content = fs::read_to_string(shim_dir.join("SKILL.md")).unwrap();
    assert!(
        !content.contains("fluent-shim: true"),
        "shim-marked directory must be replaced with the full skill"
    );
    assert!(
        shim_dir.join("references").is_dir(),
        "replaced directory must contain references"
    );
}

#[test]
fn skills_add_does_not_clobber_unmarked_directory() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");

    // Pre-install a real (non-shim) fluent skill in a secondary agent directory.
    let agent_skills = home.join(".codex/skills");
    let real_dir = agent_skills.join("fluent");
    fs::create_dir_all(&real_dir).unwrap();
    let custom_content = "---\nname: fluent\n---\nCustom full skill\n";
    fs::write(real_dir.join("SKILL.md"), custom_content).unwrap();

    fluent_cmd()
        .env("HOME", home.to_str().unwrap())
        .args(["skills", "add"])
        .assert()
        .success();

    let content = fs::read_to_string(real_dir.join("SKILL.md")).unwrap();
    assert_eq!(
        content, custom_content,
        "unmarked fluent skill must not be overwritten"
    );
}

#[test]
fn skills_add_default_is_global() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    fs::create_dir_all(&home).unwrap();

    // Create a project directory without any pre-existing fluent skill
    let project = tmp.path().join("project");
    fs::create_dir_all(&project).unwrap();

    fluent_cmd()
        .env("HOME", home.to_str().unwrap())
        .current_dir(&project)
        .args(["skills", "add"])
        .assert()
        .success();

    assert!(
        home.join(".claude/skills/fluent/SKILL.md").exists(),
        "default install should go to global directory"
    );
    assert!(
        !project.join(".claude/skills/fluent/SKILL.md").exists(),
        "should not install to project when no project-level skill exists"
    );
}

#[test]
fn skills_add_default_updates_existing_project_skill() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    fs::create_dir_all(&home).unwrap();

    // Pre-install a project-level fluent skill
    let project = tmp.path().join("project");
    let project_skills = project.join(".claude/skills/fluent");
    fs::create_dir_all(&project_skills).unwrap();
    fs::write(
        project_skills.join("SKILL.md"),
        "---\nname: fluent\nfluent-shim: true\n---\nold shim\n",
    )
    .unwrap();

    fluent_cmd()
        .env("HOME", home.to_str().unwrap())
        .current_dir(&project)
        .args(["skills", "add"])
        .assert()
        .success();

    let content = fs::read_to_string(project_skills.join("SKILL.md")).unwrap();
    assert!(
        !content.contains("fluent-shim: true"),
        "project-level fluent skill should be updated when it already exists"
    );
}

#[test]
fn skills_add_project_flag_targets_project() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    fs::create_dir_all(&home).unwrap();

    let project = tmp.path().join("project");
    fs::create_dir_all(&project).unwrap();

    fluent_cmd()
        .env("HOME", home.to_str().unwrap())
        .current_dir(&project)
        .args(["skills", "add", "--project"])
        .assert()
        .success();

    assert!(
        project.join(".claude/skills/fluent/SKILL.md").exists(),
        "--project should install to project directory"
    );
    assert!(
        !home.join(".claude/skills/fluent/SKILL.md").exists(),
        "--project should not install to global directory"
    );
}

#[test]
fn skills_add_global_flag_skips_project() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    fs::create_dir_all(&home).unwrap();

    // Pre-install a project-level fluent skill
    let project = tmp.path().join("project");
    let project_skills = project.join(".claude/skills/fluent");
    fs::create_dir_all(&project_skills).unwrap();
    let old_content = "---\nname: fluent\n---\nproject skill\n";
    fs::write(project_skills.join("SKILL.md"), old_content).unwrap();

    fluent_cmd()
        .env("HOME", home.to_str().unwrap())
        .current_dir(&project)
        .args(["skills", "add", "-g"])
        .assert()
        .success();

    assert!(
        home.join(".claude/skills/fluent/SKILL.md").exists(),
        "-g should install to global directory"
    );
    let content = fs::read_to_string(project_skills.join("SKILL.md")).unwrap();
    assert_eq!(
        content, old_content,
        "-g should not update project-level skill"
    );
}

#[test]
fn skills_add_agent_flag_targets_agents() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    fs::create_dir_all(&home).unwrap();

    fluent_cmd()
        .env("HOME", home.to_str().unwrap())
        .args(["skills", "add", "--agent", "codex"])
        .assert()
        .success();

    assert!(
        home.join(".codex/skills/fluent/SKILL.md").exists(),
        "--agent codex should install to .codex/skills/"
    );
}

#[test]
fn skills_add_agent_wildcard_targets_all_agents() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    fs::create_dir_all(&home).unwrap();

    fluent_cmd()
        .env("HOME", home.to_str().unwrap())
        .args(["skills", "add", "--agent", "*"])
        .assert()
        .success();

    assert!(
        home.join(".claude/skills/fluent/SKILL.md").exists(),
        "--agent * should install to .claude/skills/"
    );
    assert!(
        home.join(".codex/skills/fluent/SKILL.md").exists(),
        "--agent * should install to .codex/skills/"
    );
}

#[test]
fn skills_add_writes_to_data_directory() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    fs::create_dir_all(&home).unwrap();

    fluent_cmd()
        .env("HOME", home.to_str().unwrap())
        .args(["skills", "add"])
        .assert()
        .success();

    let data_skill = home.join(".local/share/fluent/skills/fluent/SKILL.md");
    assert!(
        data_skill.exists(),
        "skills add must write full skill to data directory for hand-off"
    );
    let content = fs::read_to_string(&data_skill).unwrap();
    assert!(
        !content.contains("fluent-shim: true"),
        "data directory skill must be the full skill"
    );
}

#[test]
fn init_installs_full_fluent_skill() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let project = tmp.path().join("project");
    fs::create_dir_all(&project).unwrap();

    fluent_cmd()
        .env("HOME", home.to_str().unwrap())
        .current_dir(&project)
        .args(["init"])
        .assert()
        .success();

    assert!(
        project.join(".fluent").is_dir(),
        "fluent init must create .fluent/"
    );
    assert!(
        home.join(".claude/skills/fluent/SKILL.md").exists(),
        "fluent init must install the full skill to global directory"
    );
    let content = fs::read_to_string(home.join(".claude/skills/fluent/SKILL.md")).unwrap();
    assert!(
        !content.contains("fluent-shim: true"),
        "installed skill must be the full skill, not the shim"
    );
}

#[test]
fn init_succeeds_when_skill_installation_fails() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    fs::create_dir_all(&project).unwrap();

    // Without HOME, cmd_skills_add fails, but init should still succeed.
    let output = fluent_cmd()
        .env_remove("HOME")
        .current_dir(&project)
        .args(["init"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "init must succeed even when skill installation fails"
    );
    assert!(
        project.join(".fluent").is_dir(),
        "init must create .fluent/ even when skill installation fails"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("warning: could not install skills"),
        "init must print a warning when skill installation fails: {stderr}"
    );
}

#[test]
fn init_reinit_installs_skills() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let project = tmp.path().join("project");
    fs::create_dir_all(&project).unwrap();

    // First init creates .fluent/
    fluent_cmd()
        .env("HOME", home.to_str().unwrap())
        .current_dir(&project)
        .args(["init"])
        .assert()
        .success();

    // Remove the installed skill to prove re-init installs it again
    let skill_path = home.join(".claude/skills/fluent/SKILL.md");
    assert!(skill_path.exists(), "first init must install the skill");
    fs::remove_dir_all(home.join(".claude/skills/fluent")).unwrap();

    // Second init on already-initialized project
    fluent_cmd()
        .env("HOME", home.to_str().unwrap())
        .current_dir(&project)
        .args(["init"])
        .assert()
        .success()
        .stderr(predicate::str::contains("Already initialized"));

    assert!(
        skill_path.exists(),
        "re-init must install skills to global directory"
    );
}

#[test]
fn skills_add_refreshes_stale_installation() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    let skills_dir = home.join(".claude/skills");
    let fluent_dir = skills_dir.join("fluent");
    fs::create_dir_all(&fluent_dir).unwrap();

    // Pre-install an outdated full skill
    fs::write(
        fluent_dir.join("SKILL.md"),
        "---\nname: fluent\n---\nOld version\n",
    )
    .unwrap();

    fluent_cmd()
        .env("HOME", home.to_str().unwrap())
        .args(["skills", "add"])
        .assert()
        .success();

    let content = fs::read_to_string(fluent_dir.join("SKILL.md")).unwrap();
    assert!(
        !content.contains("Old version"),
        "skills add must overwrite stale full skill with the current binary's version"
    );
    assert!(
        content.contains("fluent"),
        "refreshed skill must contain fluent content"
    );
}

#[test]
fn skills_show_prints_skill_path() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    let data_dir = home.join(".local/share/fluent/skills/fluent");
    fs::create_dir_all(&data_dir).unwrap();
    fs::write(data_dir.join("SKILL.md"), "test content").unwrap();

    let output = fluent_cmd()
        .env("HOME", home.to_str().unwrap())
        .args(["skills", "show", "--path", "fluent"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.trim().ends_with("fluent/SKILL.md"),
        "should print path to SKILL.md: {stdout}"
    );
}

#[test]
fn skills_show_prints_skill_content() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    let data_dir = home.join(".local/share/fluent/skills/fluent");
    fs::create_dir_all(&data_dir).unwrap();
    fs::write(data_dir.join("SKILL.md"), "skill body here\n").unwrap();

    let output = fluent_cmd()
        .env("HOME", home.to_str().unwrap())
        .args(["skills", "show", "fluent"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        stdout, "skill body here\n",
        "should print SKILL.md content to stdout"
    );
}

// -------------------------------------------------------------------------
// Task lease liveness
// -------------------------------------------------------------------------

#[test]
fn lease_acquire_creates_file_and_drop_frees_lock() {
    let tmp = TempDir::new().unwrap();
    let lock_path = tmp.path().join("task.lock");

    assert!(
        !fluent::lease::is_leased(&lock_path),
        "non-existent lock file should not read as leased"
    );

    let lease = fluent::lease::acquire(&lock_path).unwrap();
    // Within the same process on macOS, flock is per-process so is_leased
    // cannot detect it. Verify the lock file was created instead.
    assert!(
        lock_path.exists(),
        "lock file should exist after acquisition"
    );

    drop(lease);
    assert!(
        !fluent::lease::is_leased(&lock_path),
        "released lock should not read as leased"
    );
}

#[test]
fn lease_child_process_holder_reads_as_leased_from_parent() {
    let tmp = TempDir::new().unwrap();
    let lock_path = tmp.path().join("child.lock");

    let mut child = std::process::Command::new("python3")
        .args([
            "-c",
            &format!(
                concat!(
                    "import fcntl, sys, time\n",
                    "f = open('{}', 'w')\n",
                    "fcntl.flock(f, fcntl.LOCK_EX)\n",
                    "print('ready', flush=True)\n",
                    "time.sleep(60)\n",
                ),
                lock_path.display()
            ),
        ])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();

    let stdout = child.stdout.as_mut().unwrap();
    let mut buf = String::new();
    use std::io::BufRead;
    std::io::BufReader::new(stdout).read_line(&mut buf).unwrap();
    assert!(buf.contains("ready"), "child should signal readiness");

    assert!(
        fluent::lease::is_leased(&lock_path),
        "lock held by child process should read as leased"
    );

    child.kill().unwrap();
    child.wait().unwrap();
    assert!(
        !fluent::lease::is_leased(&lock_path),
        "lock should not read as leased after child exits"
    );
}

#[test]
fn attempt_run_reclaims_stale_executing_task_and_advances() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);

    let bin_dir = tmp.path().join("bin-reclaim");
    write_mock_claude(&bin_dir, &loop_mock_script("pass"));

    fluent_cmd()
        .current_dir(&main_dir)
        .args(["work-item", "create", "work-1", "--title", "Reclaim stale"])
        .assert()
        .success();
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "create", "work-1", "attempt-1"])
        .assert()
        .success();

    let task_path = work_task_record_path(&main_dir, "work-1", "attempt-1", "attempt-1-write-1");
    let mut task_value = read_json_value(&task_path);
    task_value["status"] = serde_json::json!("executing");
    write_json_value(&task_path, &task_value);

    let attempt_path = main_dir
        .join(".fluent/work/attempts/work-1")
        .join("attempt-1.json");
    let mut attempt_value = read_json_value(&attempt_path);
    attempt_value["status"] = serde_json::json!("executing");
    write_json_value(&attempt_path, &attempt_value);

    let output = fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "run", "work-1", "attempt-1", "--no-sandbox"])
        .env("PATH", mock_path(&bin_dir))
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "attempt run should reclaim stale task: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Completed Task attempt-1-write-1"),
        "stale task should be re-run and complete: {stdout}"
    );
}

#[test]
fn attempt_run_refuses_to_advance_when_lease_is_held() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);

    fluent_cmd()
        .current_dir(&main_dir)
        .args(["work-item", "create", "work-1", "--title", "Live lease"])
        .assert()
        .success();
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "create", "work-1", "attempt-1"])
        .assert()
        .success();

    let task_path = work_task_record_path(&main_dir, "work-1", "attempt-1", "attempt-1-write-1");
    let mut task_value = read_json_value(&task_path);
    task_value["status"] = serde_json::json!("executing");
    write_json_value(&task_path, &task_value);

    let attempt_path = main_dir
        .join(".fluent/work/attempts/work-1")
        .join("attempt-1.json");
    let mut attempt_value = read_json_value(&attempt_path);
    attempt_value["status"] = serde_json::json!("executing");
    write_json_value(&attempt_path, &attempt_value);

    let lock_path = fluent::lease::task_lock_path(&main_dir, "work-1", "attempt-1-write-1");
    fs::create_dir_all(lock_path.parent().unwrap()).unwrap();

    let mut holder = std::process::Command::new("python3")
        .args([
            "-c",
            &format!(
                concat!(
                    "import fcntl, sys\n",
                    "f = open('{}', 'w')\n",
                    "fcntl.flock(f, fcntl.LOCK_EX)\n",
                    "print('ready', flush=True)\n",
                    "sys.stdin.readline()\n",
                ),
                lock_path.display()
            ),
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();

    {
        let stdout = holder.stdout.as_mut().unwrap();
        let mut buf = String::new();
        use std::io::BufRead;
        std::io::BufReader::new(stdout).read_line(&mut buf).unwrap();
    }

    fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "run", "work-1", "attempt-1", "--no-sandbox"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("cannot be advanced"));

    drop(holder.stdin.take());
    holder.wait().unwrap();
}

#[test]
fn lease_child_process_exit_frees_lock() {
    let tmp = TempDir::new().unwrap();
    let lock_path = tmp.path().join("exit.lock");

    let mut child = std::process::Command::new("python3")
        .args([
            "-c",
            &format!(
                concat!(
                    "import fcntl, sys\n",
                    "f = open('{}', 'w')\n",
                    "fcntl.flock(f, fcntl.LOCK_EX)\n",
                    "print('ready', flush=True)\n",
                    "sys.stdin.readline()\n",
                ),
                lock_path.display()
            ),
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();

    let stdout = child.stdout.as_mut().unwrap();
    let mut buf = String::new();
    use std::io::BufRead;
    std::io::BufReader::new(stdout).read_line(&mut buf).unwrap();

    assert!(fluent::lease::is_leased(&lock_path));

    drop(child.stdin.take());
    child.wait().unwrap();

    assert!(
        !fluent::lease::is_leased(&lock_path),
        "lock should be freed after child process exits"
    );
}

#[test]
fn status_shows_stale_executing_task_as_interrupted() {
    let tmp = TempDir::new().unwrap();

    fluent_cmd()
        .current_dir(tmp.path())
        .args([
            "work-item",
            "create",
            "work-1",
            "--title",
            "Stale executing",
        ])
        .assert()
        .success();
    fluent_cmd()
        .current_dir(tmp.path())
        .args(["attempt", "create", "work-1", "attempt-1"])
        .assert()
        .success();

    let task_path = work_task_record_path(tmp.path(), "work-1", "attempt-1", "attempt-1-write-1");
    let mut task_value = read_json_value(&task_path);
    task_value["status"] = serde_json::json!("executing");
    write_json_value(&task_path, &task_value);

    let attempt_path = tmp
        .path()
        .join(".fluent/work/attempts/work-1")
        .join("attempt-1.json");
    let mut attempt_value = read_json_value(&attempt_path);
    attempt_value["status"] = serde_json::json!("executing");
    write_json_value(&attempt_path, &attempt_value);

    let output = fluent_cmd()
        .current_dir(tmp.path())
        .arg("status")
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("[interrupted]"),
        "task should show as interrupted: {stdout}"
    );
    assert!(
        stdout.contains("task-ready"),
        "action should be task-ready, not executing: {stdout}"
    );
}

#[test]
fn abandon_succeeds_when_executing_task_has_no_lease() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);

    fluent_cmd()
        .current_dir(&main_dir)
        .args(["work-item", "create", "work-1", "--title", "Stale abandon"])
        .assert()
        .success();
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "create", "work-1", "attempt-1"])
        .assert()
        .success();

    let task_path = work_task_record_path(&main_dir, "work-1", "attempt-1", "attempt-1-write-1");
    let mut task_value = read_json_value(&task_path);
    task_value["status"] = serde_json::json!("executing");
    write_json_value(&task_path, &task_value);

    let attempt_path = main_dir
        .join(".fluent/work/attempts/work-1")
        .join("attempt-1.json");
    let mut attempt_value = read_json_value(&attempt_path);
    attempt_value["status"] = serde_json::json!("failed");
    write_json_value(&attempt_path, &attempt_value);

    fluent_cmd()
        .current_dir(&main_dir)
        .args(["work-item", "abandon", "work-1", "--reason", "stale"])
        .assert()
        .success();
}

#[test]
fn abandon_fails_when_executing_task_lease_is_held() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);

    fluent_cmd()
        .current_dir(&main_dir)
        .args(["work-item", "create", "work-1", "--title", "Live abandon"])
        .assert()
        .success();
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "create", "work-1", "attempt-1"])
        .assert()
        .success();

    let task_path = work_task_record_path(&main_dir, "work-1", "attempt-1", "attempt-1-write-1");
    let mut task_value = read_json_value(&task_path);
    task_value["status"] = serde_json::json!("executing");
    write_json_value(&task_path, &task_value);

    let attempt_path = main_dir
        .join(".fluent/work/attempts/work-1")
        .join("attempt-1.json");
    let mut attempt_value = read_json_value(&attempt_path);
    attempt_value["status"] = serde_json::json!("failed");
    write_json_value(&attempt_path, &attempt_value);

    let lock_path = fluent::lease::task_lock_path(&main_dir, "work-1", "attempt-1-write-1");
    fs::create_dir_all(lock_path.parent().unwrap()).unwrap();

    let mut holder = std::process::Command::new("python3")
        .args([
            "-c",
            &format!(
                concat!(
                    "import fcntl, sys\n",
                    "f = open('{}', 'w')\n",
                    "fcntl.flock(f, fcntl.LOCK_EX)\n",
                    "print('ready', flush=True)\n",
                    "sys.stdin.readline()\n",
                ),
                lock_path.display()
            ),
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();

    {
        let stdout = holder.stdout.as_mut().unwrap();
        let mut buf = String::new();
        use std::io::BufRead;
        std::io::BufReader::new(stdout).read_line(&mut buf).unwrap();
    }

    fluent_cmd()
        .current_dir(&main_dir)
        .args(["work-item", "abandon", "work-1", "--reason", "live"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("cannot be abandoned"));

    drop(holder.stdin.take());
    holder.wait().unwrap();
}

#[test]
fn review_diff_uses_three_dot_when_source_branch_advanced() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    create_completed_work_attempt(&tmp, &main_dir);

    // Advance main with an unrelated commit after the WI diverged.
    commit_file(
        &main_dir,
        "unrelated.txt",
        "sibling landed\n",
        "Advance main with sibling work",
    );

    fluent_cmd()
        .current_dir(&main_dir)
        .args(["review", "work-1", "attempt-1"])
        .assert()
        .success();

    // Mock that captures the reviewer prompt to a log file.
    let prompt_log = tmp.path().join("reviewer-prompt.log");
    let bin_dir = tmp.path().join("bin-review-three-dot");
    write_mock_claude(
        &bin_dir,
        &format!(
            r##"#!/bin/bash
{guard}PROMPT=""
while [ "$#" -gt 0 ]; do
  if [ "$1" = "-p" ]; then
    shift
    PROMPT="$1"
    break
  fi
  shift
done
printf '%s' "$PROMPT" > '{log}'
printf 'Verdict: pass\n\nReview passed.\n' > review.md
exit 0
"##,
            guard = MOCK_PROMPT_GUARD,
            log = prompt_log.display(),
        ),
    );

    fluent_cmd()
        .current_dir(&main_dir)
        .args([
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

    let prompt = fs::read_to_string(&prompt_log).unwrap();
    // The diff command in the prompt should use three-dot for merge-base.
    assert!(
        prompt.contains("git") && prompt.contains("diff") && prompt.contains("main..."),
        "review diff command should use three-dot (merge-base), got prompt:\n{}",
        &prompt[..prompt.len().min(800)]
    );
}

#[test]
fn land_rebase_verify_uses_three_dot_when_target_advanced() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    fluent_cmd()
        .current_dir(&main_dir)
        .args([
            "work-item",
            "create",
            "work-1",
            "--title",
            "Land with advanced target",
        ])
        .assert()
        .success();
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "create", "work-1", "attempt-1"])
        .assert()
        .success();

    // Run the attempt loop (write + reviews) using a mock that captures
    // the rebase prompt.
    let rebase_prompt_log = tmp.path().join("rebase-prompt.log");
    let bin_dir = tmp.path().join("bin-land-three-dot");
    write_mock_claude(
        &bin_dir,
        &format!(
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

if [ -z "$PROMPT" ]; then
  exit 0
fi

if echo "$PROMPT" | grep -q "Rebase the candidate branch"; then
  printf '%s' "$PROMPT" > '{log}'
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
    printf 'Verdict: pass\n\nLoop review.\n' > review.md
    ;;
esac
exit 0
"##,
            log = rebase_prompt_log.display(),
        ),
    );

    // Drive write + reviews to produce a merge candidate.
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "run", "work-1", "attempt-1", "--no-sandbox"])
        .env("PATH", mock_path(&bin_dir))
        .assert()
        .success();

    // Advance main with an unrelated commit so the land triggers a rebase.
    commit_file(
        &main_dir,
        "target-only.txt",
        "target advanced\n",
        "Advance target",
    );

    // Land — should trigger rebase and the prompt should use three-dot.
    fluent_cmd()
        .current_dir(&main_dir)
        .args([
            "merge-candidate",
            "land",
            "work-1",
            "attempt-1-merge-candidate",
            "--no-sandbox",
        ])
        .env("PATH", mock_path(&bin_dir))
        .assert()
        .success();

    let rebase_prompt = fs::read_to_string(&rebase_prompt_log).unwrap();
    assert!(
        rebase_prompt.contains("...HEAD"),
        "rebase-verify prompt should use three-dot diffs, got:\n{}",
        &rebase_prompt[..rebase_prompt.len().min(800)]
    );
    // Verify no two-dot "..HEAD" remains (three-dot "...HEAD" is fine).
    let without_three_dot = rebase_prompt.replace("...HEAD", "");
    assert!(
        !without_three_dot.contains("..HEAD"),
        "rebase-verify prompt should not use two-dot diffs"
    );
}

// Land lock serialization
// -------------------------------------------------------------------------

#[test]
fn land_lock_blocks_concurrent_acquire_until_released() {
    let tmp = TempDir::new().unwrap();
    let lock_path = tmp.path().join("land.lock");

    let mut holder = std::process::Command::new("python3")
        .args([
            "-c",
            &format!(
                concat!(
                    "import fcntl, sys\n",
                    "f = open('{}', 'w')\n",
                    "fcntl.flock(f, fcntl.LOCK_EX)\n",
                    "print('ready', flush=True)\n",
                    "sys.stdin.readline()\n",
                ),
                lock_path.display()
            ),
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();

    {
        let stdout = holder.stdout.as_mut().unwrap();
        let mut buf = String::new();
        use std::io::BufRead;
        std::io::BufReader::new(stdout).read_line(&mut buf).unwrap();
        assert!(buf.contains("ready"));
    }

    assert!(
        fluent::land_lock::is_locked(&lock_path),
        "lock held by child should read as locked"
    );

    let lock_path_clone = lock_path.clone();
    let handle = std::thread::spawn(move || fluent::land_lock::acquire(&lock_path_clone));

    std::thread::sleep(std::time::Duration::from_millis(200));
    assert!(!handle.is_finished(), "blocking acquire should wait");

    drop(holder.stdin.take());
    holder.wait().unwrap();

    let guard = handle.join().unwrap();
    assert!(guard.is_ok(), "acquire should succeed after holder exits");

    drop(guard);
    assert!(
        !fluent::land_lock::is_locked(&lock_path),
        "lock should be free after guard is dropped"
    );
}

#[test]
fn land_lock_acquire_and_drop_cycle() {
    let tmp = TempDir::new().unwrap();
    let lock_path = fluent::land_lock::lock_path(tmp.path());

    assert!(
        !fluent::land_lock::is_locked(&lock_path),
        "non-existent lock should not read as locked"
    );

    let guard = fluent::land_lock::acquire(&lock_path).unwrap();
    assert!(lock_path.exists(), "lock file should exist after acquire");

    drop(guard);
    assert!(
        !fluent::land_lock::is_locked(&lock_path),
        "lock should be free after guard is dropped"
    );
}

#[test]
fn land_lock_child_exit_frees_lock() {
    let tmp = TempDir::new().unwrap();
    let lock_path = tmp.path().join("land-exit.lock");

    let mut child = std::process::Command::new("python3")
        .args([
            "-c",
            &format!(
                concat!(
                    "import fcntl, sys\n",
                    "f = open('{}', 'w')\n",
                    "fcntl.flock(f, fcntl.LOCK_EX)\n",
                    "print('ready', flush=True)\n",
                    "sys.stdin.readline()\n",
                ),
                lock_path.display()
            ),
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();

    let stdout = child.stdout.as_mut().unwrap();
    let mut buf = String::new();
    use std::io::BufRead;
    std::io::BufReader::new(stdout).read_line(&mut buf).unwrap();

    assert!(fluent::land_lock::is_locked(&lock_path));

    drop(child.stdin.take());
    child.wait().unwrap();

    assert!(
        !fluent::land_lock::is_locked(&lock_path),
        "lock should be freed after holder process exits"
    );
}

// -------------------------------------------------------------------------
// Primary-flow command guidance
// -------------------------------------------------------------------------

#[test]
fn work_item_create_output_names_attempt_create_next() {
    let tmp = TempDir::new().unwrap();

    let output = fluent_cmd()
        .current_dir(tmp.path())
        .args([
            "work-item",
            "create",
            "test-guidance",
            "--title",
            "Guidance test",
        ])
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stdout.contains("Created Work Item"),
        "result line should be on stdout"
    );
    assert!(
        stderr.contains("attempt create"),
        "next-action should name attempt create on stderr"
    );
}

#[test]
fn quiet_mode_omits_next_action_hints() {
    let tmp = TempDir::new().unwrap();

    let output = fluent_cmd()
        .current_dir(tmp.path())
        .env("FLUENT_QUIET", "1")
        .args(["work-item", "create", "test-quiet", "--title", "Quiet test"])
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stdout.contains("Created Work Item"),
        "result line should still be on stdout"
    );
    assert!(
        !stderr.contains("attempt create"),
        "next-action should be suppressed in quiet mode"
    );
}

#[test]
fn next_action_is_appended_not_interleaved_into_result() {
    let tmp = TempDir::new().unwrap();

    let output = fluent_cmd()
        .current_dir(tmp.path())
        .args(["work-item", "create", "test-order", "--title", "Order test"])
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stdout.contains("Created Work Item"),
        "result should be on stdout"
    );
    assert!(
        !stdout.contains("attempt create"),
        "next-action should not be interleaved into stdout result"
    );
    assert!(
        stderr.contains("attempt create"),
        "next-action should appear on stderr"
    );
}

#[test]
fn attempt_create_output_names_attempt_run_next() {
    let tmp = TempDir::new().unwrap();
    write_work_item_json(tmp.path(), "work-g1", "Guidance");

    let output = fluent_cmd()
        .current_dir(tmp.path())
        .args(["attempt", "create", "work-g1", "attempt-1"])
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stdout.contains("Created Attempt"),
        "result line should be on stdout"
    );
    assert!(
        stderr.contains("attempt run"),
        "next-action should name attempt run on stderr"
    );
}

#[test]
#[serial]
fn attempt_run_output_names_next_action_for_merge_candidate_ready() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["work-item", "create", "work-1", "--title", "Guidance MC"])
        .assert()
        .success();
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "create", "work-1", "attempt-1"])
        .assert()
        .success();

    let bin_dir = tmp.path().join("bin-guidance-pass");
    write_mock_claude(&bin_dir, &loop_mock_script("pass"));

    let output = fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "run", "work-1", "attempt-1", "--no-sandbox"])
        .env("PATH", mock_path(&bin_dir))
        .output()
        .unwrap();

    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("merge-candidate"),
        "next-action for MergeCandidateReady should name merge-candidate on stderr; got:\n{stderr}"
    );
}

#[test]
#[serial]
fn attempt_run_output_names_next_action_for_failed() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["work-item", "create", "work-1", "--title", "Guidance fail"])
        .assert()
        .success();
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "create", "work-1", "attempt-1"])
        .assert()
        .success();

    let bin_dir = tmp.path().join("bin-guidance-fail");
    write_mock_claude(&bin_dir, &stateful_loop_mock_script("fail"));

    let output = fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "run", "work-1", "attempt-1", "--no-sandbox"])
        .env("PATH", mock_path(&bin_dir))
        .env("FLUENT_MAX_TOTAL_WRITE_ROUNDS", "1")
        .output()
        .unwrap();

    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("resolve") || stderr.contains("attempt run"),
        "next-action for failed/needs-user should name recovery on stderr; got:\n{stderr}"
    );
}

#[test]
#[serial]
fn needs_user_auth_output_names_reauth_and_attempt_run() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["work-item", "create", "work-1", "--title", "Auth pause"])
        .assert()
        .success();
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "create", "work-1", "attempt-1"])
        .assert()
        .success();

    let bin_dir = tmp.path().join("bin-guidance-auth");
    write_mock_claude(
        &bin_dir,
        r##"#!/bin/bash
HAS_PROMPT=0
for arg in "$@"; do
  if [ "$arg" = "-p" ]; then HAS_PROMPT=1; break; fi
done
if [ "$HAS_PROMPT" = 0 ]; then exit 0; fi
echo '{"type":"result","api_error_status":401,"request_id":"req-test-auth"}'
exit 1
"##,
    );

    let output = fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "run", "work-1", "attempt-1", "--no-sandbox"])
        .env("PATH", mock_path(&bin_dir))
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("re-authenticate") && stderr.contains("attempt run"),
        "auth pause next-action should name re-authenticate and attempt run on stderr; got:\n{stderr}"
    );
}

#[test]
fn empty_status_output_primes_planning_stages() {
    let tmp = TempDir::new().unwrap();

    let output = fluent_cmd()
        .current_dir(tmp.path())
        .arg("status")
        .output()
        .unwrap();

    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("brief"),
        "empty status should mention brief on stderr; got:\n{stderr}"
    );
    assert!(
        stderr.contains("behaviors"),
        "empty status should mention behaviors on stderr; got:\n{stderr}"
    );
    assert!(
        stderr.contains("approach"),
        "empty status should mention approach on stderr; got:\n{stderr}"
    );
    assert!(
        stderr.contains("work-item create"),
        "empty status should name work-item create on stderr; got:\n{stderr}"
    );
}

#[test]
fn planning_primer_points_to_fluent_skill() {
    let tmp = TempDir::new().unwrap();

    let output = fluent_cmd()
        .current_dir(tmp.path())
        .arg("status")
        .output()
        .unwrap();

    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("fluent skill"),
        "planning primer should include drift-pointer on stderr; got:\n{stderr}"
    );
}

#[test]
fn status_names_next_action_for_actionable_state() {
    let tmp = TempDir::new().unwrap();

    fluent_cmd()
        .current_dir(tmp.path())
        .args(["work-item", "create", "work-1", "--title", "Actionable"])
        .assert()
        .success();
    fluent_cmd()
        .current_dir(tmp.path())
        .args(["attempt", "create", "work-1", "attempt-1"])
        .assert()
        .success();

    let status = fluent_cmd()
        .current_dir(tmp.path())
        .arg("status")
        .output()
        .unwrap();
    assert!(status.status.success());
    let status_stderr = String::from_utf8_lossy(&status.stderr);
    assert!(
        status_stderr.contains("fluent attempt run work-1"),
        "populated status should name the runnable attempt on stderr; got:\n{status_stderr}"
    );

    let show = fluent_cmd()
        .current_dir(tmp.path())
        .args(["work-item", "show", "work-1"])
        .output()
        .unwrap();
    assert!(show.status.success());
    let show_stderr = String::from_utf8_lossy(&show.stderr);
    assert!(
        show_stderr.contains("fluent attempt run work-1"),
        "work-item show should name the runnable attempt on stderr; got:\n{show_stderr}"
    );
}

#[test]
#[serial]
fn side_commands_name_next_action() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);

    fluent_cmd()
        .current_dir(&main_dir)
        .args(["work-item", "create", "work-1", "--title", "Side"])
        .assert()
        .success();

    let list = fluent_cmd()
        .current_dir(&main_dir)
        .args(["work-item", "list"])
        .output()
        .unwrap();
    assert!(list.status.success());
    let list_stderr = String::from_utf8_lossy(&list.stderr);
    assert!(
        list_stderr.contains("fluent status") || list_stderr.contains("work-item show"),
        "non-empty work-item list should name a next step; got:\n{list_stderr}"
    );

    let obs = fluent_cmd()
        .current_dir(&main_dir)
        .args(["observation", "create", "A note to remember."])
        .output()
        .unwrap();
    assert!(obs.status.success());
    let obs_stderr = String::from_utf8_lossy(&obs.stderr);
    assert!(
        obs_stderr.contains("observation list"),
        "observation create should name a next step; got:\n{obs_stderr}"
    );

    let clean = fluent_cmd()
        .current_dir(&main_dir)
        .arg("cleanup")
        .output()
        .unwrap();
    assert!(clean.status.success());
    let clean_stderr = String::from_utf8_lossy(&clean.stderr);
    assert!(
        clean_stderr.contains("fluent status"),
        "cleanup should name a next step; got:\n{clean_stderr}"
    );
}

#[test]
#[serial]
fn attempt_run_write_round_conveys_iteration() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["work-item", "create", "work-1", "--title", "Iterating"])
        .assert()
        .success();
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "create", "work-1", "attempt-1"])
        .assert()
        .success();

    let bin_dir = tmp.path().join("bin-write-round-iteration");
    write_mock_claude(&bin_dir, &stateful_loop_mock_script("fail"));

    let output = fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "run", "work-1", "attempt-1", "--no-sandbox"])
        .env("PATH", mock_path(&bin_dir))
        .output()
        .unwrap();

    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("follow-up") || stderr.contains("keep iterating"),
        "a planned write round should read as iterating, not stuck; got:\n{stderr}"
    );
}

#[test]
#[serial]
fn needs_user_generic_output_names_handoff_file() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    create_completed_work_attempt(&tmp, &main_dir);

    let bin_dir = tmp.path().join("bin-generic-needs-user");
    write_mock_claude(&bin_dir, &loop_mock_script("uncertain"));

    let output = fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "run", "work-1", "attempt-1", "--no-sandbox"])
        .env("PATH", mock_path(&bin_dir))
        .output()
        .unwrap();

    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("handoff") && stderr.contains("needs-user.md"),
        "generic needs-user next-action should name the handoff file; got:\n{stderr}"
    );
    assert!(
        stderr.contains("attempt run"),
        "generic needs-user next-action should still name attempt run; got:\n{stderr}"
    );
}

#[test]
#[serial]
fn needs_user_auth_output_is_coder_aware() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["work-item", "create", "work-1", "--title", "Auth pause"])
        .assert()
        .success();
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "create", "work-1", "attempt-1"])
        .assert()
        .success();

    let bin_dir = tmp.path().join("bin-auth-coder-aware");
    write_mock_claude(
        &bin_dir,
        r##"#!/bin/bash
HAS_PROMPT=0
for arg in "$@"; do
  if [ "$arg" = "-p" ]; then HAS_PROMPT=1; break; fi
done
if [ "$HAS_PROMPT" = 0 ]; then exit 0; fi
echo '{"type":"result","api_error_status":401,"request_id":"req-test-auth"}'
exit 1
"##,
    );

    let output = fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "run", "work-1", "attempt-1", "--no-sandbox"])
        .env("PATH", mock_path(&bin_dir))
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("claude /login"),
        "auth pause next-action should name the Claude re-auth step; got:\n{stderr}"
    );
    assert!(
        stderr.contains("attempt run"),
        "auth pause next-action should name attempt run; got:\n{stderr}"
    );
}

#[test]
#[serial]
fn attempt_run_review_only_names_artifact() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);
    fluent_cmd()
        .current_dir(&main_dir)
        .args([
            "work-item",
            "create",
            "work-1",
            "--title",
            "Review codebase",
        ])
        .assert()
        .success();
    fluent_cmd()
        .current_dir(&main_dir)
        .args([
            "review",
            "codebase",
            "work-1",
            "attempt-review",
            "--from-working-tree",
        ])
        .assert()
        .success();

    let bin_dir = tmp.path().join("bin-review-only-artifact");
    write_mock_claude(&bin_dir, &review_only_mock_script("fail"));

    let output = fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "run", "work-1", "attempt-review", "--no-sandbox"])
        .env("PATH", mock_path(&bin_dir))
        .output()
        .unwrap();

    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("review.md"),
        "review-only next-action should name a review artifact; got:\n{stderr}"
    );
    assert!(
        !stderr.contains("proceed with the next step"),
        "review-only next-action must not be the generic phrasing; got:\n{stderr}"
    );
}

#[test]
fn empty_work_item_list_primes_planning_stages() {
    let tmp = TempDir::new().unwrap();

    let output = fluent_cmd()
        .current_dir(tmp.path())
        .args(["work-item", "list"])
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stdout.contains("No Work Items found"),
        "result should indicate no items on stdout"
    );
    assert!(
        stderr.contains("brief") && stderr.contains("work-item create"),
        "empty list should print planning primer on stderr; got:\n{stderr}"
    );
}

#[test]
fn attempt_run_prints_resolved_coder_plan() {
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

    fluent_cmd()
        .current_dir(&main_dir)
        .args(["work-item", "create", "plan-print", "--title", "Plan print"])
        .assert()
        .success();
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "create", "plan-print", "attempt-1"])
        .assert()
        .success();

    let output = fluent_cmd()
        .current_dir(&main_dir)
        .args([
            "attempt",
            "run",
            "plan-print",
            "--no-sandbox",
            "--write-model",
            "my-model",
            "--effort",
            "high",
        ])
        .env("PATH", mock_path(&bin_dir))
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Coder plan:"),
        "should print coder plan header; stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("writer") && stderr.contains("my-model"),
        "should show writer model; stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("effort=high"),
        "should show effort; stderr:\n{stderr}"
    );

    // Verify FLUENT_QUIET suppresses it.
    fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "create", "plan-print", "attempt-2"])
        .assert()
        .success();

    let quiet_output = fluent_cmd()
        .current_dir(&main_dir)
        .args(["attempt", "run", "plan-print", "attempt-2", "--no-sandbox"])
        .env("PATH", mock_path(&bin_dir))
        .env("FLUENT_QUIET", "1")
        .output()
        .unwrap();

    let quiet_stderr = String::from_utf8_lossy(&quiet_output.stderr);
    assert!(
        !quiet_stderr.contains("Coder plan:"),
        "FLUENT_QUIET should suppress coder plan; stderr:\n{quiet_stderr}"
    );
}

#[test]
fn attempt_coder_model_flags_override_config() {
    let tmp = TempDir::new().unwrap();
    let main_dir = setup_git_project(&tmp);

    // Write a project config that sets writer to codex with a specific model.
    let config_dir = main_dir.join(".fluent");
    fs::create_dir_all(&config_dir).unwrap();
    fs::write(
        config_dir.join("config.yaml"),
        "coders:\n  writer:\n    coder: codex\n    model: config-model\n    effort: low\n",
    )
    .unwrap();

    fluent_cmd()
        .current_dir(&main_dir)
        .args([
            "work-item",
            "create",
            "override-test",
            "--title",
            "Override",
        ])
        .assert()
        .success();

    // Create attempt with CLI flags that override the config.
    fluent_cmd()
        .current_dir(&main_dir)
        .args([
            "attempt",
            "create",
            "override-test",
            "attempt-1",
            "--write-model",
            "cli-model",
            "--effort",
            "high",
        ])
        .assert()
        .success();

    let value = work_item_value(&main_dir, "override-test");
    let mapping = &value["attempts"][0]["coder_mapping"];

    // --write-model should override the config model for write.
    assert_eq!(
        mapping["write"]["model"], "cli-model",
        "CLI --write-model should override config"
    );
    // Coder should still come from config since no CLI coder override.
    assert_eq!(
        mapping["write"]["coder"], "codex",
        "config coder used when CLI does not override"
    );
    // --effort (global) should apply to all roles.
    assert_eq!(
        mapping["write"]["effort"], "high",
        "CLI --effort should override config effort for write"
    );
    assert_eq!(
        mapping["review"]["effort"], "high",
        "CLI --effort should apply to review"
    );
    assert_eq!(
        mapping["behavior-tests"]["effort"], "high",
        "CLI --effort should apply to behavior-tests"
    );
}
