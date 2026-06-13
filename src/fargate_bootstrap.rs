//! Just-in-time Fargate setup. The first time `--runtime fargate` is
//! used, this module deploys the CloudFormation stack, builds and
//! pushes the Factory base image, builds and pushes the project image
//! (when the project provides `.factory/Dockerfile`), and writes
//! everything Factory's launch code needs into
//! `~/.config/factory/fargate.state.json`. Subsequent invocations
//! short-circuit when nothing has changed.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::Command;

const STACK_NAME: &str = "factory";
const FACTORY_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Durable state file recording what's been deployed. One file per
/// user, cross-project.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FargateState {
    #[serde(default)]
    pub stack_deployed: bool,
    pub region: Option<String>,
    pub cluster_arn: Option<String>,
    pub task_def_arn: Option<String>,
    pub repo_uri: Option<String>,
    pub s3_bucket: Option<String>,
    pub subnets: Option<String>,
    pub security_group_id: Option<String>,
    pub base_image_hash: Option<String>,
    pub base_image_pushed_at: Option<String>,
    #[serde(default)]
    pub project_image_hashes: BTreeMap<String, String>,
}

impl FargateState {
    pub fn state_path() -> Result<PathBuf> {
        let home = std::env::var("HOME").context("HOME not set")?;
        Ok(PathBuf::from(home).join(".config/factory/fargate.state.json"))
    }

    pub fn load() -> Result<Self> {
        let path = Self::state_path()?;
        if !path.exists() {
            return Ok(Self::default());
        }
        let content = fs::read_to_string(&path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse Fargate state file {}", path.display()))
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::state_path()?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create {}", parent.display()))?;
        }
        let content = serde_json::to_string_pretty(self)?;
        fs::write(&path, format!("{content}\n"))
            .with_context(|| format!("Failed to write {}", path.display()))?;
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct BootstrapConfig {
    pub project_root: PathBuf,
    /// Root of the Factory source tree (contains `infrastructure/`,
    /// `sandboxes/`, `Cargo.toml`, `src/`, etc.). Required because the
    /// base image build needs the full Factory source as build
    /// context.
    pub factory_source_root: PathBuf,
    pub region: String,
    pub force_rebuild: bool,
}

/// Top-level entry point. Idempotent. Call this before every Fargate
/// launch (`launch_work_attempt`, `launch_work_merge`). Returns the
/// resolved Fargate state so the caller can fill in
/// `~/.config/factory/fargate.env` equivalents (`FACTORY_CLUSTER`,
/// `FACTORY_S3_BUCKET`, etc.).
pub fn ensure_setup(config: &BootstrapConfig) -> Result<FargateState> {
    let mut state = FargateState::load()?;
    let region = config.region.clone();

    if !state.stack_deployed || config.force_rebuild {
        deploy_stack(config, &mut state)?;
    } else if state.region.as_deref() != Some(&region) {
        eprintln!(
            "  Warning: requested region {region} differs from deployed region {}",
            state.region.as_deref().unwrap_or("(unknown)")
        );
    }

    let repo_uri = state
        .repo_uri
        .as_deref()
        .context("Repo URI missing from Fargate state — stack must be deployed first")?
        .to_string();
    let region = config.region.as_str();

    let base_tag = base_image_tag();
    let base_hash = compute_base_image_hash(&config.factory_source_root)?;
    let base_changed = state.base_image_hash.as_deref() != Some(&base_hash);
    if base_changed || config.force_rebuild {
        let ecr_has_base = ecr_image_tag_exists(region, &repo_uri, &base_tag)?;
        if ecr_has_base && !config.force_rebuild {
            eprintln!("  Base image {repo_uri}:{base_tag} already in ECR, skipping build.");
            state.base_image_hash = Some(base_hash);
            state.save()?;
        } else {
            build_and_push_base_image(config, &state, &base_hash)?;
            state.base_image_hash = Some(base_hash);
            state.base_image_pushed_at = Some(chrono::Utc::now().to_rfc3339());
            state.save()?;
        }
    }

    let base_image_uri = format!("{repo_uri}:{base_tag}");
    let stub_created = ensure_project_dockerfile_stub(&config.project_root)?;
    if stub_created {
        eprintln!(
            "  Created .factory/Dockerfile stub. Customize it with project-specific toolchains."
        );
    }

    let project_dockerfile = config.project_root.join(".factory/Dockerfile");
    if project_dockerfile.exists() {
        let project_name = project_basename(&config.project_root)?;
        let dockerfile_sha = sha256_file(&project_dockerfile)?;
        let project_tag = project_image_tag(&dockerfile_sha);
        let project_hash = hash_file(&project_dockerfile)?;
        let previous_hash = state.project_image_hashes.get(&project_name).cloned();
        let project_changed = previous_hash.as_deref() != Some(&project_hash);
        if base_changed || project_changed || config.force_rebuild {
            let ecr_has_project = ecr_image_tag_exists(region, &repo_uri, &project_tag)?;
            if ecr_has_project && !config.force_rebuild {
                eprintln!(
                    "  Project image {repo_uri}:{project_tag} already in ECR, skipping build."
                );
            } else {
                build_and_push_project_image(config, &state, &project_tag, &base_image_uri)?;
            }
            state
                .project_image_hashes
                .insert(project_name, project_hash);
            state.save()?;
        }

        let project_image_uri = format!("{repo_uri}:{project_tag}");
        register_task_definition_revision(&state, &project_image_uri)?;
    }

    Ok(state)
}

fn project_basename(project_root: &Path) -> Result<String> {
    project_root
        .file_name()
        .and_then(|n| n.to_str())
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow::anyhow!("Project root has no basename"))
}

fn deploy_stack(config: &BootstrapConfig, state: &mut FargateState) -> Result<()> {
    let region = &config.region;
    eprintln!("  Discovering default VPC in {region}...");
    let vpc_id = aws_text_output(&[
        "ec2",
        "describe-vpcs",
        "--region",
        region,
        "--filters",
        "Name=is-default,Values=true",
        "--query",
        "Vpcs[0].VpcId",
        "--output",
        "text",
    ])?;
    if vpc_id == "None" || vpc_id.is_empty() {
        bail!("No default VPC found in {region}");
    }
    let subnets_raw = aws_text_output(&[
        "ec2",
        "describe-subnets",
        "--region",
        region,
        "--filters",
        &format!("Name=vpc-id,Values={vpc_id}"),
        "--query",
        "Subnets[*].SubnetId",
        "--output",
        "text",
    ])?;
    if subnets_raw.is_empty() {
        bail!("No subnets found in VPC {vpc_id}");
    }
    let subnets = subnets_raw.replace('\t', ",");
    eprintln!("  VPC:     {vpc_id}");
    eprintln!("  Subnets: {subnets}");

    let template = config
        .factory_source_root
        .join("infrastructure/cloudformation.yaml");
    if !template.exists() {
        bail!(
            "CloudFormation template not found at {}",
            template.display()
        );
    }

    eprintln!("  Deploying CloudFormation stack '{STACK_NAME}'...");
    run_aws(&[
        "cloudformation",
        "deploy",
        "--region",
        region,
        "--stack-name",
        STACK_NAME,
        "--template-file",
        &template.to_string_lossy(),
        "--parameter-overrides",
        &format!("VpcId={vpc_id}"),
        &format!("SubnetIds={subnets}"),
        "--capabilities",
        "CAPABILITY_NAMED_IAM",
        "--no-fail-on-empty-changeset",
    ])?;

    let cluster_arn = read_stack_output(region, "ClusterArn")?;
    let task_def_arn = read_stack_output(region, "RunTaskDefArn")?;
    let repo_uri = read_stack_output(region, "RepoUri")?;
    let s3_bucket = read_stack_output(region, "WorkspaceBucketName")?;
    let security_group_id = read_stack_output(region, "TaskSecurityGroupId")?;

    eprintln!("  Cluster:   {cluster_arn}");
    eprintln!("  Task def:  {task_def_arn}");
    eprintln!("  S3 bucket: {s3_bucket}");

    state.region = Some(region.clone());
    state.cluster_arn = Some(cluster_arn);
    state.task_def_arn = Some(task_def_arn);
    state.repo_uri = Some(repo_uri);
    state.s3_bucket = Some(s3_bucket);
    state.security_group_id = Some(security_group_id);
    state.subnets = Some(subnets);
    state.stack_deployed = true;
    state.save()?;
    Ok(())
}

fn build_and_push_base_image(
    config: &BootstrapConfig,
    state: &FargateState,
    base_hash: &str,
) -> Result<()> {
    let repo_uri = state
        .repo_uri
        .as_deref()
        .context("Repo URI missing from Fargate state — stack must be deployed first")?;
    let region = config.region.as_str();
    let account_id = aws_text_output(&[
        "sts",
        "get-caller-identity",
        "--query",
        "Account",
        "--output",
        "text",
    ])?;
    docker_login_ecr(&account_id, region)?;

    let tag = base_image_tag();
    let dockerfile = config
        .factory_source_root
        .join("infrastructure/run/Dockerfile");
    let context_dir = &config.factory_source_root;
    let remote_tag = format!("{repo_uri}:{tag}");
    eprintln!("  Building base image (hash {base_hash})...");
    run_docker(&[
        "build",
        "--platform",
        "linux/amd64",
        "--load",
        "-f",
        &dockerfile.to_string_lossy(),
        "-t",
        &remote_tag,
        &context_dir.to_string_lossy(),
    ])?;

    eprintln!("  Pushing base image to {remote_tag}...");
    run_docker(&["push", &remote_tag])?;
    Ok(())
}

fn build_and_push_project_image(
    config: &BootstrapConfig,
    state: &FargateState,
    image_tag: &str,
    base_image_uri: &str,
) -> Result<()> {
    let repo_uri = state
        .repo_uri
        .as_deref()
        .context("Repo URI missing from Fargate state")?;
    let region = config.region.as_str();
    let account_id = aws_text_output(&[
        "sts",
        "get-caller-identity",
        "--query",
        "Account",
        "--output",
        "text",
    ])?;
    docker_login_ecr(&account_id, region)?;

    let dockerfile = config.project_root.join(".factory/Dockerfile");
    let remote_tag = format!("{repo_uri}:{image_tag}");
    eprintln!("  Building project image ({image_tag})...");
    run_docker(&[
        "build",
        "--platform",
        "linux/amd64",
        "--load",
        "-f",
        &dockerfile.to_string_lossy(),
        "--build-arg",
        &format!("FACTORY_BASE_URI={base_image_uri}"),
        "-t",
        &remote_tag,
        &config.project_root.to_string_lossy(),
    ])?;

    eprintln!("  Pushing project image to {remote_tag}...");
    run_docker(&["push", &remote_tag])?;
    Ok(())
}

fn compute_base_image_hash(factory_source_root: &Path) -> Result<String> {
    let mut hasher = DefaultHasher::new();
    for relative in [
        "infrastructure/run/Dockerfile",
        "infrastructure/run/entrypoint.sh",
    ] {
        let path = factory_source_root.join(relative);
        if path.exists() {
            fs::read(&path)
                .with_context(|| format!("Failed to read {}", path.display()))?
                .hash(&mut hasher);
        }
    }
    option_env!("FACTORY_BUILD_COMMIT")
        .unwrap_or("unknown")
        .hash(&mut hasher);
    Ok(format!("{:x}", hasher.finish()))
}

fn hash_file(path: &Path) -> Result<String> {
    let mut hasher = DefaultHasher::new();
    fs::read(path)
        .with_context(|| format!("Failed to read {}", path.display()))?
        .hash(&mut hasher);
    Ok(format!("{:x}", hasher.finish()))
}

pub fn base_image_tag() -> String {
    format!("factory-base-{FACTORY_VERSION}")
}

pub fn project_image_tag(dockerfile_sha256: &str) -> String {
    let prefix = &dockerfile_sha256[..12.min(dockerfile_sha256.len())];
    format!("project-{prefix}")
}

pub fn sha256_file(path: &Path) -> Result<String> {
    let content = fs::read(path).with_context(|| format!("Failed to read {}", path.display()))?;
    let mut hasher = Sha256::new();
    hasher.update(&content);
    Ok(format!("{:x}", hasher.finalize()))
}

pub fn project_image_tag_for_dockerfile(project_root: &Path) -> Result<String> {
    let dockerfile = project_root.join(".factory/Dockerfile");
    if !dockerfile.exists() {
        bail!("No .factory/Dockerfile found at {}", dockerfile.display());
    }
    let sha = sha256_file(&dockerfile)?;
    Ok(project_image_tag(&sha))
}

fn ecr_image_tag_exists(region: &str, repo_uri: &str, tag: &str) -> Result<bool> {
    let repo_name = repo_name_from_uri(repo_uri)?;
    let output = Command::new("aws")
        .args([
            "ecr",
            "describe-images",
            "--region",
            region,
            "--repository-name",
            &repo_name,
            "--image-ids",
            &format!("imageTag={tag}"),
            "--query",
            "imageDetails[0].imageDigest",
            "--output",
            "text",
        ])
        .output()
        .context("Failed to launch aws CLI")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("ImageNotFoundException") || stderr.contains("does not exist") {
            return Ok(false);
        }
        bail!(
            "aws ecr describe-images --image-ids imageTag={tag} failed:\n{}",
            stderr.trim()
        );
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Ok(!text.is_empty() && text != "None")
}

fn repo_name_from_uri(repo_uri: &str) -> Result<String> {
    repo_uri
        .split('/')
        .skip(1)
        .collect::<Vec<_>>()
        .join("/")
        .parse::<String>()
        .map(|s| {
            if s.is_empty() {
                repo_uri.to_string()
            } else {
                s
            }
        })
        .context("Failed to extract repository name from URI")
}

fn ensure_project_dockerfile_stub(project_root: &Path) -> Result<bool> {
    let dockerfile = project_root.join(".factory/Dockerfile");
    if dockerfile.exists() {
        return Ok(false);
    }
    let parent = dockerfile
        .parent()
        .context("Cannot determine parent of .factory/Dockerfile")?;
    fs::create_dir_all(parent).with_context(|| format!("Failed to create {}", parent.display()))?;
    let stub = format!(
        "# Project-specific Factory runtime image.\n\
         # Extend the Factory base with any toolchains your merge checks\n\
         # need (rustc, go, mvn, etc.).\n\
         ARG FACTORY_BASE_URI\n\
         FROM ${{FACTORY_BASE_URI}}\n",
    );
    fs::write(&dockerfile, &stub)
        .with_context(|| format!("Failed to write {}", dockerfile.display()))?;
    Ok(true)
}

fn register_task_definition_revision(
    state: &FargateState,
    project_image_uri: &str,
) -> Result<String> {
    let region = state
        .region
        .as_deref()
        .context("Region missing from Fargate state")?;
    let task_def_arn = state
        .task_def_arn
        .as_deref()
        .context("Task definition ARN missing from Fargate state")?;
    let family = task_def_family(task_def_arn);

    let current_json = aws_text_output(&[
        "ecs",
        "describe-task-definition",
        "--region",
        region,
        "--task-definition",
        &family,
        "--query",
        "taskDefinition.containerDefinitions[0].image",
        "--output",
        "text",
    ])?;

    if current_json.trim() == project_image_uri {
        eprintln!("  Task definition already references {project_image_uri}, skipping revision.");
        return Ok(task_def_arn.to_string());
    }

    eprintln!("  Registering task definition revision for {project_image_uri}...");
    let full_def = aws_text_output(&[
        "ecs",
        "describe-task-definition",
        "--region",
        region,
        "--task-definition",
        &family,
        "--query",
        "taskDefinition",
        "--output",
        "json",
    ])?;

    let mut def: serde_json::Value =
        serde_json::from_str(&full_def).context("Failed to parse task definition JSON")?;
    if let Some(containers) = def
        .get_mut("containerDefinitions")
        .and_then(|v| v.as_array_mut())
    {
        for container in containers.iter_mut() {
            container["image"] = serde_json::Value::String(project_image_uri.to_string());
        }
    }

    for key in &[
        "taskDefinitionArn",
        "revision",
        "status",
        "requiresAttributes",
        "compatibilities",
        "registeredAt",
        "registeredBy",
    ] {
        def.as_object_mut().map(|m| m.remove(*key));
    }

    let def_str = serde_json::to_string(&def)?;
    let new_arn = aws_text_output(&[
        "ecs",
        "register-task-definition",
        "--region",
        region,
        "--cli-input-json",
        &def_str,
        "--query",
        "taskDefinition.taskDefinitionArn",
        "--output",
        "text",
    ])?;

    eprintln!("  New task definition revision: {new_arn}");
    Ok(new_arn)
}

fn task_def_family(task_def_arn: &str) -> String {
    if let Some(family_rev) = task_def_arn.rsplit('/').next() {
        if let Some(family) = family_rev.rsplit_once(':') {
            return family.0.to_string();
        }
        return family_rev.to_string();
    }
    task_def_arn.to_string()
}

fn aws_text_output(args: &[&str]) -> Result<String> {
    let output = Command::new("aws")
        .args(args)
        .output()
        .context("Failed to launch aws CLI (is it installed and on PATH?)")?;
    if !output.status.success() {
        bail!(
            "aws {} failed:\n{}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn run_aws(args: &[&str]) -> Result<()> {
    let status = Command::new("aws")
        .args(args)
        .status()
        .context("Failed to launch aws CLI")?;
    if !status.success() {
        bail!("aws {} exited with status {status}", args.join(" "));
    }
    Ok(())
}

fn run_docker(args: &[&str]) -> Result<()> {
    let status = Command::new("docker")
        .args(args)
        .status()
        .context("Failed to launch docker (is it installed and on PATH?)")?;
    if !status.success() {
        bail!("docker {} exited with status {status}", args.join(" "));
    }
    Ok(())
}

fn docker_login_ecr(account_id: &str, region: &str) -> Result<()> {
    let registry = format!("{account_id}.dkr.ecr.{region}.amazonaws.com");
    eprintln!("  Authenticating Docker with ECR ({registry})...");
    let password_output = Command::new("aws")
        .args(["ecr", "get-login-password", "--region", region])
        .output()
        .context("Failed to launch aws ecr get-login-password")?;
    if !password_output.status.success() {
        bail!(
            "aws ecr get-login-password failed:\n{}",
            String::from_utf8_lossy(&password_output.stderr).trim()
        );
    }
    let mut login = Command::new("docker")
        .args(["login", "--username", "AWS", "--password-stdin", &registry])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .context("Failed to spawn docker login")?;
    {
        use std::io::Write;
        let stdin = login.stdin.as_mut().context("docker login has no stdin")?;
        stdin.write_all(&password_output.stdout)?;
    }
    let status = login.wait()?;
    if !status.success() {
        bail!("docker login exited with status {status}");
    }
    Ok(())
}

fn read_stack_output(region: &str, key: &str) -> Result<String> {
    let query = format!("Stacks[0].Outputs[?OutputKey=='{key}'].OutputValue");
    aws_text_output(&[
        "cloudformation",
        "describe-stacks",
        "--region",
        region,
        "--stack-name",
        STACK_NAME,
        "--query",
        &query,
        "--output",
        "text",
    ])
}

/// Outcome of a teardown operation.
#[derive(Debug)]
pub struct TeardownOutcome {
    pub stack_deleted: bool,
    pub ecr_deleted: bool,
    pub s3_deleted: bool,
    pub state_file_removed: bool,
}

impl std::fmt::Display for TeardownOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut removed = Vec::new();
        if self.stack_deleted {
            removed.push("CloudFormation stack");
        }
        if self.ecr_deleted {
            removed.push("ECR repository");
        }
        if self.s3_deleted {
            removed.push("S3 bucket");
        }
        if self.state_file_removed {
            removed.push("state file");
        }
        if removed.is_empty() {
            write!(f, "Nothing to tear down")
        } else {
            write!(f, "Removed: {}", removed.join(", "))
        }
    }
}

/// Tear down Fargate infrastructure. Idempotent: re-running after a
/// successful teardown reports nothing to tear down.
pub fn teardown(keep_ecr: bool, keep_s3: bool) -> Result<TeardownOutcome> {
    let state_path = FargateState::state_path()?;
    let has_state_file = state_path.exists();

    if !has_state_file {
        return Ok(TeardownOutcome {
            stack_deleted: false,
            ecr_deleted: false,
            s3_deleted: false,
            state_file_removed: false,
        });
    }

    let state = FargateState::load()?;
    let region = state.region.as_deref().unwrap_or("us-west-1");

    let stack_exists = if state.stack_deployed {
        check_stack_exists(region)?
    } else {
        false
    };

    let mut ecr_deleted = false;
    let mut s3_deleted = false;

    if !keep_ecr {
        eprintln!(
            "  ECR repository contains base image tags (factory-base-*) and project image tags (project-*)."
        );
        ecr_deleted = delete_ecr_repository(region)?;
    }

    if !keep_s3 {
        if let Some(bucket) = state.s3_bucket.as_deref() {
            s3_deleted = empty_and_delete_s3_bucket(region, bucket)?;
        } else if stack_exists {
            if let Ok(bucket_name) = read_stack_output(region, "WorkspaceBucketName") {
                if !bucket_name.is_empty() && bucket_name != "None" {
                    s3_deleted = empty_and_delete_s3_bucket(region, &bucket_name)?;
                }
            }
        }
    }

    let stack_deleted = if stack_exists {
        delete_cloudformation_stack(region)?;
        true
    } else {
        false
    };

    fs::remove_file(&state_path)
        .with_context(|| format!("Failed to remove {}", state_path.display()))?;

    Ok(TeardownOutcome {
        stack_deleted,
        ecr_deleted,
        s3_deleted,
        state_file_removed: true,
    })
}

fn check_stack_exists(region: &str) -> Result<bool> {
    let output = Command::new("aws")
        .args([
            "cloudformation",
            "describe-stacks",
            "--region",
            region,
            "--stack-name",
            STACK_NAME,
            "--query",
            "Stacks[0].StackStatus",
            "--output",
            "text",
        ])
        .output()
        .context("Failed to launch aws CLI")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("does not exist") || stderr.contains("ValidationError") {
            return Ok(false);
        }
        bail!(
            "aws cloudformation describe-stacks failed:\n{}",
            stderr.trim()
        );
    }
    let status = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Ok(!status.is_empty() && status != "None")
}

fn delete_ecr_repository(region: &str) -> Result<bool> {
    eprintln!("  Deleting ECR repository...");
    let output = Command::new("aws")
        .args([
            "ecr",
            "describe-repositories",
            "--region",
            region,
            "--repository-names",
            "factory/run",
            "--query",
            "repositories[0].repositoryName",
            "--output",
            "text",
        ])
        .output()
        .context("Failed to launch aws CLI")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("RepositoryNotFoundException") {
            eprintln!("  ECR repository not found, skipping.");
            return Ok(false);
        }
        bail!("aws ecr describe-repositories failed:\n{}", stderr.trim());
    }
    run_aws(&[
        "ecr",
        "delete-repository",
        "--region",
        region,
        "--repository-name",
        "factory/run",
        "--force",
    ])?;
    eprintln!("  ECR repository deleted.");
    Ok(true)
}

fn empty_and_delete_s3_bucket(region: &str, bucket: &str) -> Result<bool> {
    eprintln!("  Emptying S3 bucket {bucket}...");
    let empty_result = Command::new("aws")
        .args([
            "s3",
            "rm",
            &format!("s3://{bucket}"),
            "--recursive",
            "--region",
            region,
        ])
        .output()
        .context("Failed to launch aws s3 rm")?;
    if !empty_result.status.success() {
        let stderr = String::from_utf8_lossy(&empty_result.stderr);
        if stderr.contains("NoSuchBucket") {
            eprintln!("  S3 bucket not found, skipping.");
            return Ok(false);
        }
        bail!("aws s3 rm failed:\n{}", stderr.trim());
    }

    eprintln!("  Deleting S3 bucket {bucket}...");
    let delete_result = Command::new("aws")
        .args(["s3", "rb", &format!("s3://{bucket}"), "--region", region])
        .output()
        .context("Failed to launch aws s3 rb")?;
    if !delete_result.status.success() {
        let stderr = String::from_utf8_lossy(&delete_result.stderr);
        if stderr.contains("NoSuchBucket") {
            eprintln!("  S3 bucket already deleted.");
            return Ok(true);
        }
        bail!("aws s3 rb failed:\n{}", stderr.trim());
    }
    eprintln!("  S3 bucket deleted.");
    Ok(true)
}

fn delete_cloudformation_stack(region: &str) -> Result<()> {
    eprintln!("  Deleting CloudFormation stack '{STACK_NAME}'...");
    run_aws(&[
        "cloudformation",
        "delete-stack",
        "--region",
        region,
        "--stack-name",
        STACK_NAME,
    ])?;

    eprintln!("  Waiting for stack deletion...");
    run_aws(&[
        "cloudformation",
        "wait",
        "stack-delete-complete",
        "--region",
        region,
        "--stack-name",
        STACK_NAME,
    ])?;

    eprintln!("  CloudFormation stack deleted.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn fargate_state_round_trip() {
        let mut state = FargateState::default();
        state.stack_deployed = true;
        state.region = Some("us-west-1".into());
        state.repo_uri = Some("123.dkr.ecr.us-west-1.amazonaws.com/factory/run".into());
        state
            .project_image_hashes
            .insert("main".into(), "abc123".into());
        let json = serde_json::to_string(&state).unwrap();
        let parsed: FargateState = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.stack_deployed, true);
        assert_eq!(parsed.region.as_deref(), Some("us-west-1"));
        assert_eq!(
            parsed.project_image_hashes.get("main").map(|s| s.as_str()),
            Some("abc123")
        );
    }

    #[test]
    fn hash_file_is_stable_for_same_content() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("a.txt");
        fs::write(&path, "hello").unwrap();
        let h1 = hash_file(&path).unwrap();
        let h2 = hash_file(&path).unwrap();
        assert_eq!(h1, h2);
    }

    #[test]
    fn hash_file_changes_with_content() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("a.txt");
        fs::write(&path, "hello").unwrap();
        let h1 = hash_file(&path).unwrap();
        fs::write(&path, "goodbye").unwrap();
        let h2 = hash_file(&path).unwrap();
        assert_ne!(h1, h2);
    }

    #[test]
    fn teardown_outcome_display_nothing() {
        let outcome = TeardownOutcome {
            stack_deleted: false,
            ecr_deleted: false,
            s3_deleted: false,
            state_file_removed: false,
        };
        assert_eq!(outcome.to_string(), "Nothing to tear down");
    }

    #[test]
    fn teardown_outcome_display_all_removed() {
        let outcome = TeardownOutcome {
            stack_deleted: true,
            ecr_deleted: true,
            s3_deleted: true,
            state_file_removed: true,
        };
        let display = outcome.to_string();
        assert!(display.contains("CloudFormation stack"));
        assert!(display.contains("ECR repository"));
        assert!(display.contains("S3 bucket"));
        assert!(display.contains("state file"));
    }

    #[test]
    fn teardown_outcome_display_partial_keep_ecr() {
        let outcome = TeardownOutcome {
            stack_deleted: true,
            ecr_deleted: false,
            s3_deleted: true,
            state_file_removed: true,
        };
        let display = outcome.to_string();
        assert!(display.contains("CloudFormation stack"));
        assert!(!display.contains("ECR repository"));
        assert!(display.contains("S3 bucket"));
    }

    #[test]
    fn teardown_outcome_display_partial_keep_s3() {
        let outcome = TeardownOutcome {
            stack_deleted: true,
            ecr_deleted: true,
            s3_deleted: false,
            state_file_removed: true,
        };
        let display = outcome.to_string();
        assert!(display.contains("CloudFormation stack"));
        assert!(display.contains("ECR repository"));
        assert!(!display.contains("S3 bucket"));
    }

    #[test]
    fn base_image_tag_includes_version() {
        let tag = base_image_tag();
        assert!(tag.starts_with("factory-base-"));
        assert!(tag.contains(FACTORY_VERSION));
    }

    #[test]
    fn project_image_tag_from_hash_deterministic_12_hex() {
        let tag = project_image_tag("a3f2b8c9d4e1ff00112233445566778899aabbcc");
        assert_eq!(tag, "project-a3f2b8c9d4e1");
    }

    #[test]
    fn project_image_tag_from_hash_short_input() {
        let tag = project_image_tag("abcd");
        assert_eq!(tag, "project-abcd");
    }

    #[test]
    fn sha256_file_is_stable() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("test.txt");
        fs::write(&path, "hello world").unwrap();
        let h1 = sha256_file(&path).unwrap();
        let h2 = sha256_file(&path).unwrap();
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 64);
    }

    #[test]
    fn sha256_file_changes_with_content() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("test.txt");
        fs::write(&path, "hello").unwrap();
        let h1 = sha256_file(&path).unwrap();
        fs::write(&path, "goodbye").unwrap();
        let h2 = sha256_file(&path).unwrap();
        assert_ne!(h1, h2);
    }

    #[test]
    fn ensure_project_dockerfile_stub_creates_when_missing() {
        let tmp = TempDir::new().unwrap();
        let project = tmp.path().join("my-project");
        fs::create_dir_all(&project).unwrap();
        let result = ensure_project_dockerfile_stub(&project).unwrap();
        assert!(result);
        let content = fs::read_to_string(project.join(".factory/Dockerfile")).unwrap();
        assert!(content.contains("ARG FACTORY_BASE_URI"));
        assert!(content.contains("FROM ${FACTORY_BASE_URI}"));
        assert!(content.contains("# Project-specific Factory runtime image."));
    }

    #[test]
    fn ensure_project_dockerfile_stub_skips_when_exists() {
        let tmp = TempDir::new().unwrap();
        let project = tmp.path().join("my-project");
        fs::create_dir_all(project.join(".factory")).unwrap();
        fs::write(project.join(".factory/Dockerfile"), "FROM custom:image\n").unwrap();
        let result = ensure_project_dockerfile_stub(&project).unwrap();
        assert!(!result);
        let content = fs::read_to_string(project.join(".factory/Dockerfile")).unwrap();
        assert_eq!(content, "FROM custom:image\n");
    }

    #[test]
    fn repo_name_from_uri_extracts_name() {
        let name =
            repo_name_from_uri("123456789012.dkr.ecr.us-west-2.amazonaws.com/factory/run").unwrap();
        assert_eq!(name, "factory/run");
    }

    #[test]
    fn task_def_family_extracts_from_arn() {
        assert_eq!(
            task_def_family("arn:aws:ecs:us-west-2:123:task-definition/factory-run:5"),
            "factory-run"
        );
    }

    #[test]
    fn task_def_family_handles_plain_name() {
        assert_eq!(task_def_family("factory-run"), "factory-run");
    }
}
