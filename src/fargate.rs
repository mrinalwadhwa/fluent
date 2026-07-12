use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::coder::CoderKind;
use crate::credential;
use crate::git;

struct FargateConfig {
    cluster: String,
    run_task: String,
    s3_bucket: String,
    subnets: String,
    security_group: String,
    region: String,
}

/// Resolve FargateConfig from the JIT bootstrap state file. If the
/// state file is missing or incomplete, fall back to `fargate.env`
/// (legacy) for backward compatibility with hand-deployed setups.
fn load_config() -> Result<FargateConfig> {
    let home = std::env::var("HOME").unwrap_or_default();

    if let Ok(state) = crate::fargate_bootstrap::FargateState::load() {
        if state.stack_deployed
            && state.cluster_arn.is_some()
            && state.s3_bucket.is_some()
            && state.subnets.is_some()
            && state.security_group_id.is_some()
        {
            let run_task = state
                .task_def_arn
                .as_deref()
                .map(task_def_family)
                .unwrap_or_else(|| "fluent-run".to_string());
            return Ok(FargateConfig {
                cluster: state.cluster_arn.unwrap(),
                run_task,
                s3_bucket: state.s3_bucket.unwrap(),
                subnets: state.subnets.unwrap(),
                security_group: state.security_group_id.unwrap(),
                region: state.region.unwrap_or_else(|| "us-west-1".to_string()),
            });
        }
    }

    let cfg_path = format!("{home}/.config/fluent/fargate.env");
    if Path::new(&cfg_path).exists() {
        let content = fs::read_to_string(&cfg_path)?;
        for line in content.lines() {
            if let Some((key, value)) = line.split_once('=') {
                // SAFETY: Called during single-threaded initialization.
                unsafe { std::env::set_var(key.trim(), value.trim()) };
            }
        }
    }

    Ok(FargateConfig {
        cluster: env_required("FLUENT_CLUSTER")?,
        run_task: std::env::var("FLUENT_RUN_TASK").unwrap_or_else(|_| "fluent-run".into()),
        s3_bucket: env_required("FLUENT_S3_BUCKET")?,
        subnets: env_required("FLUENT_SUBNETS")?,
        security_group: env_required("FLUENT_SECURITY_GROUP")?,
        region: std::env::var("FLUENT_REGION")
            .or_else(|_| std::env::var("AWS_DEFAULT_REGION"))
            .unwrap_or_else(|_| "us-west-1".into()),
    })
}

/// Resolve the Fluent source root used by the JIT bootstrap to build
/// images. Order:
///   1. `FLUENT_SOURCE_ROOT` env var if set
///   2. Walk up from the project root looking for a directory that
///      contains both `Cargo.toml` and `infrastructure/run/Dockerfile`
///   3. Use the project root itself if it looks like the Fluent
///      source tree.
pub fn resolve_fluent_source_root_from(project_root: &Path) -> Result<std::path::PathBuf> {
    resolve_fluent_source_root(project_root)
}

fn resolve_fluent_source_root(project_root: &Path) -> Result<std::path::PathBuf> {
    if let Ok(env_path) = std::env::var("FLUENT_SOURCE_ROOT") {
        let path = std::path::PathBuf::from(&env_path);
        if !path.join("infrastructure/run/Dockerfile").exists() {
            anyhow::bail!(
                "FLUENT_SOURCE_ROOT={} does not contain infrastructure/run/Dockerfile",
                env_path
            );
        }
        return Ok(path);
    }
    let mut candidate = project_root.to_path_buf();
    loop {
        if candidate.join("infrastructure/run/Dockerfile").exists()
            && candidate.join("Cargo.toml").exists()
        {
            return Ok(candidate);
        }
        if !candidate.pop() {
            break;
        }
    }
    anyhow::bail!(
        "Could not locate the Fluent source tree. Set FLUENT_SOURCE_ROOT to the directory containing infrastructure/run/Dockerfile and Cargo.toml."
    )
}

/// Run JIT bootstrap (CFN deploy + base image + project image) and
/// return the resulting FargateConfig. Called by Work-model launches
/// before each Fargate task.
fn bootstrap_and_load_config(project_root: &Path) -> Result<FargateConfig> {
    let fluent_source_root = resolve_fluent_source_root(project_root)?;
    let region = std::env::var("FLUENT_REGION")
        .or_else(|_| std::env::var("AWS_DEFAULT_REGION"))
        .unwrap_or_else(|_| "us-west-1".to_string());
    let force_rebuild = std::env::var("FLUENT_FARGATE_FORCE_REBUILD")
        .ok()
        .map(|v| !matches!(v.as_str(), "" | "0" | "false" | "no"))
        .unwrap_or(false);
    crate::fargate_bootstrap::ensure_setup(&crate::fargate_bootstrap::BootstrapConfig {
        project_root: project_root.to_path_buf(),
        fluent_source_root,
        region,
        force_rebuild,
    })?;
    load_config()
}

fn task_def_family(arn: &str) -> String {
    if let Some(family_rev) = arn.rsplit('/').next() {
        if let Some((family, _rev)) = family_rev.rsplit_once(':') {
            return family.to_string();
        }
        return family_rev.to_string();
    }
    arn.to_string()
}

fn env_required(name: &str) -> Result<String> {
    std::env::var(name).map_err(|_| {
        anyhow::anyhow!(
            "{name} not set. Run infrastructure/setup.sh or set in ~/.config/fluent/fargate.env"
        )
    })
}

/// Path where Fluent records the ECS task ARN for a running
/// Fargate-executed Work Attempt. Lives outside of the durable
/// Work model JSON so it can be cleaned up freely after the task
/// finishes.
fn work_attempt_runtime_dir(project_root: &Path, work_item_id: &str, attempt_id: &str) -> PathBuf {
    project_root
        .join(".fluent/work/runtime/attempts")
        .join(work_item_id)
        .join(attempt_id)
}

fn work_merge_runtime_dir(
    project_root: &Path,
    work_item_id: &str,
    merge_candidate_id: &str,
) -> PathBuf {
    project_root
        .join(".fluent/work/runtime/merges")
        .join(work_item_id)
        .join(merge_candidate_id)
}

fn record_task_arn(runtime_dir: &Path, task_arn: &str) -> Result<()> {
    fs::create_dir_all(runtime_dir)?;
    fs::write(runtime_dir.join("fargate-task-arn"), task_arn)?;
    Ok(())
}

fn read_recorded_task_arn(runtime_dir: &Path) -> Option<String> {
    fs::read_to_string(runtime_dir.join("fargate-task-arn"))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn stop_ecs_task(config: &FargateConfig, task_arn: &str) -> Result<()> {
    let output = Command::new("aws")
        .args(["ecs", "stop-task"])
        .args(["--region", &config.region])
        .args(["--cluster", &config.cluster])
        .args(["--task", task_arn])
        .args([
            "--reason",
            "fluent work attempt/merge stop requested by operator",
        ])
        .output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let s = stderr.trim();
        if s.contains("InvalidParameterException")
            || s.to_lowercase().contains("not found")
            || s.to_lowercase().contains("stopped")
        {
            eprintln!("  Task already stopped or not found.");
            return Ok(());
        }
        anyhow::bail!("Failed to stop Fargate task: {s}");
    }
    eprintln!("  Stopped Fargate task: {task_arn}");
    Ok(())
}

/// Snapshot of an ECS task's current status, suitable for printing.
struct EcsTaskStatus {
    last_status: String,
    desired_status: String,
    stop_code: Option<String>,
    stopped_reason: Option<String>,
}

fn describe_ecs_task(config: &FargateConfig, task_arn: &str) -> Result<EcsTaskStatus> {
    let output = Command::new("aws")
        .args(["ecs", "describe-tasks"])
        .args(["--region", &config.region])
        .args(["--cluster", &config.cluster])
        .args(["--tasks", task_arn])
        .args(["--output", "json"])
        .output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("Failed to describe Fargate task: {}", stderr.trim());
    }
    let json: serde_json::Value = serde_json::from_slice(&output.stdout)
        .context("Failed to parse aws ecs describe-tasks output as JSON")?;
    let task = json
        .get("tasks")
        .and_then(|t| t.as_array())
        .and_then(|arr| arr.first())
        .ok_or_else(|| anyhow::anyhow!("No task returned for ARN {task_arn}"))?;
    let last_status = task
        .get("lastStatus")
        .and_then(|s| s.as_str())
        .unwrap_or("UNKNOWN")
        .to_string();
    let desired_status = task
        .get("desiredStatus")
        .and_then(|s| s.as_str())
        .unwrap_or("UNKNOWN")
        .to_string();
    let stop_code = task
        .get("stopCode")
        .and_then(|s| s.as_str())
        .map(|s| s.to_string());
    let stopped_reason = task
        .get("stoppedReason")
        .and_then(|s| s.as_str())
        .map(|s| s.to_string());
    Ok(EcsTaskStatus {
        last_status,
        desired_status,
        stop_code,
        stopped_reason,
    })
}

fn watch_ecs_task(config: &FargateConfig, task_arn: &str, interval_secs: u64) -> Result<()> {
    let interval = std::time::Duration::from_secs(interval_secs.max(1));
    let mut previous_status: Option<String> = None;
    eprintln!("  Watching Fargate task {task_arn}");
    eprintln!("  Poll interval: {interval_secs}s. Ctrl+C to stop watching (task keeps running).");
    loop {
        let status = describe_ecs_task(config, task_arn)?;
        if Some(&status.last_status) != previous_status.as_ref() {
            eprintln!(
                "  [{}] last={} desired={}",
                chrono::Utc::now().to_rfc3339(),
                status.last_status,
                status.desired_status
            );
            previous_status = Some(status.last_status.clone());
        }
        if status.last_status == "STOPPED" {
            if let Some(code) = &status.stop_code {
                eprintln!("  stopCode: {code}");
            }
            if let Some(reason) = &status.stopped_reason {
                eprintln!("  stoppedReason: {reason}");
            }
            return Ok(());
        }
        std::thread::sleep(interval);
    }
}

/// Watch the Fargate task associated with a Work Attempt until it
/// reaches the STOPPED state. Prints state transitions and the
/// final stopCode + stoppedReason.
pub fn watch_work_attempt(
    project_root: &Path,
    work_item_id: &str,
    attempt_id: &str,
    interval_secs: u64,
) -> Result<()> {
    let runtime_dir = work_attempt_runtime_dir(project_root, work_item_id, attempt_id);
    let task_arn = read_recorded_task_arn(&runtime_dir).ok_or_else(|| {
        anyhow::anyhow!(
            "No Fargate task recorded for Work Attempt {work_item_id}/{attempt_id}; was it launched with --runtime fargate?"
        )
    })?;
    let config = load_config()?;
    credential::inject_credentials()?;
    watch_ecs_task(&config, &task_arn, interval_secs)
}

/// Watch the Fargate task associated with a Merge Candidate until
/// it reaches the STOPPED state.
pub fn watch_work_merge(
    project_root: &Path,
    work_item_id: &str,
    merge_candidate_id: &str,
    interval_secs: u64,
) -> Result<()> {
    let runtime_dir = work_merge_runtime_dir(project_root, work_item_id, merge_candidate_id);
    let task_arn = read_recorded_task_arn(&runtime_dir).ok_or_else(|| {
        anyhow::anyhow!(
            "No Fargate task recorded for Merge Candidate {work_item_id}/{merge_candidate_id}; was it launched with --runtime fargate?"
        )
    })?;
    let config = load_config()?;
    credential::inject_credentials()?;
    watch_ecs_task(&config, &task_arn, interval_secs)
}

/// Stop a Fargate-executed Work Attempt's ECS task. Idempotent: if
/// no task ARN is recorded or the task is already gone, returns Ok.
pub fn stop_work_attempt(project_root: &Path, work_item_id: &str, attempt_id: &str) -> Result<()> {
    let runtime_dir = work_attempt_runtime_dir(project_root, work_item_id, attempt_id);
    let Some(task_arn) = read_recorded_task_arn(&runtime_dir) else {
        eprintln!(
            "  No Fargate task recorded for Work Attempt {work_item_id}/{attempt_id}; nothing to stop."
        );
        return Ok(());
    };
    let config = load_config()?;
    credential::inject_credentials()?;
    stop_ecs_task(&config, &task_arn)?;
    // Leave the recorded ARN in place so a follow-up pull can correlate
    // S3 keys; cleanup of the runtime dir is the user's call via
    // `fluent cleanup --apply`.
    Ok(())
}

/// Stop a Fargate-executed Merge Candidate's ECS task. Idempotent.
pub fn stop_work_merge(
    project_root: &Path,
    work_item_id: &str,
    merge_candidate_id: &str,
) -> Result<()> {
    let runtime_dir = work_merge_runtime_dir(project_root, work_item_id, merge_candidate_id);
    let Some(task_arn) = read_recorded_task_arn(&runtime_dir) else {
        eprintln!(
            "  No Fargate task recorded for Merge Candidate {work_item_id}/{merge_candidate_id}; nothing to stop."
        );
        return Ok(());
    };
    let config = load_config()?;
    credential::inject_credentials()?;
    stop_ecs_task(&config, &task_arn)?;
    Ok(())
}

/// Read the Merge Candidate from the Work model store and return the
/// basenames of any sibling worktrees that exist next to the project
/// root and need to be uploaded for the merge to operate on. Today
/// that is just the candidate's source workspace; review-time sibling
/// worktrees are created in-container.
fn merge_candidate_sibling_worktrees(
    project_root: &Path,
    work_item_id: &str,
    merge_candidate_id: &str,
) -> Result<Vec<String>> {
    let store = crate::work_model::WorkModelStore::new(project_root);
    let item = store
        .read_work_item(work_item_id)
        .map_err(|e| anyhow::anyhow!("Failed to read Work Item {work_item_id}: {e}"))?;
    let candidate = item
        .merge_candidates
        .iter()
        .find(|c| c.id == merge_candidate_id)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Merge Candidate {merge_candidate_id} not found in Work Item {work_item_id}"
            )
        })?;
    let source_path = Path::new(&candidate.source_workspace.path);
    let Some(basename) = source_path
        .file_name()
        .and_then(|n| n.to_str())
        .map(|s| s.to_string())
    else {
        return Ok(Vec::new());
    };
    let parent = project_root
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Project root has no parent"))?;
    let absolute = parent.join(&basename);
    if absolute.is_dir() {
        Ok(vec![basename])
    } else {
        Ok(Vec::new())
    }
}

/// Resolve project_root → (parent, basename). The parent becomes the
/// tar `-C` directory and the basename is the single top-level entry
/// included in the tar (matching the `/worktrees/<name>` container
/// layout).
fn project_root_components(project_root: &Path) -> Result<(PathBuf, String)> {
    let parent = project_root
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Project root has no parent"))?
        .to_path_buf();
    let name = project_root
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| anyhow::anyhow!("Project root has no basename"))?
        .to_string();
    Ok((parent, name))
}

/// Upload the project worktree to S3 as `<bucket>/<key>`. The tar's
/// single top-level entry is the project basename — matching what the
/// container entrypoint expects to find under `/worktrees`. Common
/// regeneratable directories (build artifacts, node modules, local
/// scratch, the local Fargate ARN tracking dir) are excluded to keep
/// the upload small.
fn upload_project_workspace(config: &FargateConfig, project_root: &Path, key: &str) -> Result<()> {
    upload_worktrees(config, project_root, key, &[])
}

/// Upload the project worktree plus any named sibling worktrees that
/// live next to it as a single tar archive matching the `/worktrees`
/// container layout. Used for merge launches that need the candidate
/// (and optionally review) worktrees alongside the project.
fn upload_worktrees(
    config: &FargateConfig,
    project_root: &Path,
    key: &str,
    extra_siblings: &[String],
) -> Result<()> {
    let (parent, name) = project_root_components(project_root)?;
    eprintln!(
        "  Uploading project workspace to s3://{}/{key}",
        config.s3_bucket
    );
    let excludes = [
        format!("{name}/target"),
        format!("{name}/node_modules"),
        format!("{name}/.scratch"),
        format!("{name}/.fluent/work/runtime"),
        format!("{name}/.git/lfs"),
    ];
    let parent_str = parent.to_string_lossy().into_owned();
    let mut tar_args: Vec<String> = vec!["cf".into(), "-".into()];
    // bsdtar on macOS embeds extended attributes / resource forks as
    // PAX header data and AppleDouble (._<file>) entries. On Linux
    // extraction those become literal ._<file> files that confuse
    // Work model JSON readers ("stream did not contain valid
    // UTF-8"). Disable that.
    tar_args.push("--no-mac-metadata".into());
    tar_args.push("--no-xattrs".into());
    tar_args.push("--exclude=._*".into());
    tar_args.push("--exclude=.DS_Store".into());
    for ex in &excludes {
        tar_args.push(format!("--exclude={ex}"));
    }
    tar_args.push("-C".into());
    tar_args.push(parent_str);
    tar_args.push(name);
    for sibling in extra_siblings {
        eprintln!("  Including sibling worktree: {sibling}");
        tar_args.push(sibling.clone());
    }

    let mut tar_child = Command::new("tar")
        .env("COPYFILE_DISABLE", "1")
        .args(&tar_args)
        .stdout(std::process::Stdio::piped())
        .spawn()?;
    let tar_stdout = tar_child
        .stdout
        .take()
        .context("Failed to capture workspace archive output")?;
    let upload_status = Command::new("aws")
        .args(["s3", "cp", "--region", &config.region])
        .args(["-", &format!("s3://{}/{key}", config.s3_bucket)])
        .stdin(tar_stdout)
        .status()?;
    let tar_status = tar_child
        .wait()
        .context("Failed to wait for workspace archive command")?;
    if !upload_status.success() {
        anyhow::bail!("Failed to upload workspace to S3");
    }
    if !tar_status.success() {
        anyhow::bail!("Failed to archive workspace for upload");
    }
    Ok(())
}

/// Download a Fluent worktrees tarball at `<bucket>/<key>` into
/// `project_root`'s parent — restoring the project worktree plus any
/// sibling candidate/review worktrees the remote produced. Runs
/// `git worktree repair` afterwards so the sibling `.git` gitfiles
/// and the main `.git/worktrees/*` gitdir entries relink against the
/// local absolute paths instead of the remote's `/worktrees/...`
/// container paths.
fn pull_worktrees(config: &FargateConfig, project_root: &Path, key: &str) -> Result<()> {
    let parent = project_root
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Project root has no parent"))?;
    eprintln!("  Source: s3://{}/{key}", config.s3_bucket);
    eprintln!("  Target: {}", parent.display());

    let mut child = Command::new("aws")
        .args(["s3", "cp", "--region", &config.region])
        .args([&format!("s3://{}/{key}", config.s3_bucket), "-"])
        .stdout(std::process::Stdio::piped())
        .spawn()?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("Failed to capture s3 stream"))?;
    let tar_status = Command::new("tar")
        .args(["xf", "-", "-C", &parent.to_string_lossy()])
        .stdin(stdout)
        .status()?;
    let s3_status = child.wait().context("Failed to wait for s3 cp command")?;
    if !s3_status.success() {
        anyhow::bail!("Failed to download workspace from S3");
    }
    if !tar_status.success() {
        anyhow::bail!("Failed to extract workspace");
    }

    repair_sibling_worktrees(project_root)?;

    eprintln!("  Project and sibling worktrees extracted from S3.");
    Ok(())
}

/// Run `git worktree repair` on each sibling worktree next to the
/// project root so the embedded `.git` gitfile and the main
/// `.git/worktrees/*` gitdir entries point at the current local
/// absolute paths.
fn repair_sibling_worktrees(project_root: &Path) -> Result<()> {
    let Some(parent) = project_root.parent() else {
        return Ok(());
    };
    let Ok(entries) = fs::read_dir(parent) else {
        return Ok(());
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !path.is_dir() {
            continue;
        }
        if !name.starts_with("work-") && !name.starts_with("review-") {
            continue;
        }
        eprintln!("  Repairing sibling worktree linkage: {}", path.display());
        let _ = git::run_raw(
            project_root,
            &["worktree", "repair", &path.to_string_lossy()],
        );
    }
    Ok(())
}

/// Build the coder-specific environment overrides for an ECS task.
/// Always includes `FLUENT_CODER`. For Claude, reads the OAuth token
/// from the environment. For Codex, reads `~/.codex/auth.json` from
/// the host and validates `auth_mode == "chatgpt"` before returning.
pub fn coder_task_overrides(coder: CoderKind) -> Result<Vec<(String, String)>> {
    let mut env = vec![("FLUENT_CODER".to_string(), coder.as_str().to_string())];
    match coder {
        CoderKind::Claude => {
            let oauth = std::env::var("CLAUDE_CODE_OAUTH_TOKEN")
                .map_err(|_| anyhow::anyhow!("No Claude auth token available"))?;
            env.push(("CLAUDE_CODE_OAUTH_TOKEN".to_string(), oauth));
        }
        CoderKind::Codex => {
            let home = std::env::var("HOME").unwrap_or_default();
            let auth_path = PathBuf::from(&home).join(".codex/auth.json");
            let auth_json = fs::read_to_string(&auth_path).map_err(|e| {
                anyhow::anyhow!(
                    "Cannot read Codex auth from {}: {e}. \
                     Log in with `codex` locally first.",
                    auth_path.display()
                )
            })?;
            let parsed: serde_json::Value = serde_json::from_str(&auth_json)
                .map_err(|e| anyhow::anyhow!("Failed to parse {}: {e}", auth_path.display()))?;
            let auth_mode = parsed
                .get("auth_mode")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if auth_mode != "chatgpt" {
                anyhow::bail!(
                    "Fargate Codex requires ChatGPT subscription auth \
                     (host auth_mode = \"{auth_mode}\"). \
                     Switch to subscription auth or use --coder claude."
                );
            }
            env.push(("CODEX_AUTH_JSON".to_string(), auth_json));
        }
        CoderKind::Pi => {
            anyhow::bail!(
                "Pi coder is local-only and cannot run on Fargate. \
                 Use --coder claude or --coder codex for remote execution."
            );
        }
    }
    Ok(env)
}

fn run_ecs_task(config: &FargateConfig, environment: serde_json::Value) -> Result<String> {
    let overrides = serde_json::json!({
        "containerOverrides": [{
            "name": "run",
            "environment": environment,
        }]
    });
    let output = Command::new("aws")
        .args(["ecs", "run-task"])
        .args(["--region", &config.region])
        .args(["--cluster", &config.cluster])
        .args(["--task-definition", &config.run_task])
        .args(["--launch-type", "FARGATE"])
        .args(["--enable-execute-command"])
        .args([
            "--network-configuration",
            &format!(
                "awsvpcConfiguration={{subnets=[{}],securityGroups=[{}],assignPublicIp=ENABLED}}",
                config.subnets, config.security_group
            ),
        ])
        .args(["--overrides", &overrides.to_string()])
        .args(["--query", "tasks[0].taskArn"])
        .args(["--output", "text"])
        .output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("Failed to start Fargate task: {}", stderr.trim());
    }
    let task_arn = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if task_arn.is_empty() || task_arn == "None" {
        anyhow::bail!("Failed to start Fargate task: no task ARN returned");
    }
    Ok(task_arn)
}

/// Upload the project workspace to S3 and launch a Fargate task that
/// runs `fluent work attempt run` for the given Work Item / Attempt.
pub fn launch_work_attempt(
    project_root: &Path,
    work_item_id: &str,
    attempt_id: &str,
    coder: CoderKind,
) -> Result<()> {
    let coder_env = coder_task_overrides(coder)?;

    let config = bootstrap_and_load_config(project_root)?;
    credential::inject_credentials()?;

    let (_parent, project_name) = project_root_components(project_root)?;

    let upload_key = format!("work/{work_item_id}/{attempt_id}/workspace-in.tar");
    eprintln!("  Fluent           fargate work attempt run ({work_item_id} {attempt_id})");
    upload_project_workspace(&config, project_root, &upload_key)?;

    eprintln!("  Starting Fargate task...");
    let mut env_overrides: Vec<serde_json::Value> = vec![
        serde_json::json!({"name": "FLUENT_WORK_ITEM_ID", "value": work_item_id}),
        serde_json::json!({"name": "FLUENT_WORK_ATTEMPT_ID", "value": attempt_id}),
        serde_json::json!({"name": "FLUENT_PROJECT_NAME", "value": project_name}),
        serde_json::json!({"name": "FLUENT_S3_BUCKET", "value": config.s3_bucket}),
        serde_json::json!({"name": "FLUENT_REGION", "value": config.region}),
    ];
    for (k, v) in &coder_env {
        env_overrides.push(serde_json::json!({"name": k, "value": v}));
    }
    let task_arn = run_ecs_task(&config, serde_json::Value::Array(env_overrides))?;
    eprintln!("  Task: {task_arn}");

    let runtime_dir = work_attempt_runtime_dir(project_root, work_item_id, attempt_id);
    record_task_arn(&runtime_dir, &task_arn)?;

    eprintln!("  Attempt is executing on Fargate.");
    eprintln!("  Use \"fluent work attempt watch {work_item_id} {attempt_id}\" to follow status.");
    eprintln!(
        "  Use \"fluent work attempt pull {work_item_id} {attempt_id}\" to retrieve results when the task finishes."
    );
    eprintln!("  Use \"fluent work attempt stop {work_item_id} {attempt_id}\" to stop the task.");

    Ok(())
}

/// Download the completed Work Attempt worktrees tarball from S3 and
/// extract into project_root's parent, restoring the project root and
/// any sibling candidate/review worktrees.
pub fn pull_work_attempt(project_root: &Path, work_item_id: &str, attempt_id: &str) -> Result<()> {
    let config = load_config()?;
    let key = format!("work/{work_item_id}/{attempt_id}/workspace-out.tar");
    pull_worktrees(&config, project_root, &key)
}

/// Upload the project workspace plus the Merge Candidate's source
/// worktree to S3 and launch a Fargate task that runs
/// `fluent work merge`.
pub fn launch_work_merge(
    project_root: &Path,
    work_item_id: &str,
    merge_candidate_id: &str,
    coder: CoderKind,
    skip_post_merge_review: bool,
) -> Result<()> {
    let coder_env = coder_task_overrides(coder)?;

    let config = bootstrap_and_load_config(project_root)?;
    credential::inject_credentials()?;

    let (_parent, project_name) = project_root_components(project_root)?;

    let sibling_worktrees =
        merge_candidate_sibling_worktrees(project_root, work_item_id, merge_candidate_id)?;
    let upload_key = format!("work-merge/{work_item_id}/{merge_candidate_id}/workspace-in.tar");
    eprintln!("  Fluent           fargate work merge ({work_item_id} {merge_candidate_id})");
    upload_worktrees(&config, project_root, &upload_key, &sibling_worktrees)?;

    eprintln!("  Starting Fargate task...");
    let mut env_overrides: Vec<serde_json::Value> = vec![
        serde_json::json!({"name": "FLUENT_WORK_ITEM_ID", "value": work_item_id}),
        serde_json::json!({"name": "FLUENT_WORK_MERGE_CANDIDATE_ID", "value": merge_candidate_id}),
        serde_json::json!({"name": "FLUENT_PROJECT_NAME", "value": project_name}),
        serde_json::json!({"name": "FLUENT_S3_BUCKET", "value": config.s3_bucket}),
        serde_json::json!({"name": "FLUENT_REGION", "value": config.region}),
    ];
    if skip_post_merge_review {
        env_overrides
            .push(serde_json::json!({"name": "FLUENT_NO_POST_MERGE_REVIEW", "value": "1"}));
    }
    for (k, v) in &coder_env {
        env_overrides.push(serde_json::json!({"name": k, "value": v}));
    }
    let task_arn = run_ecs_task(&config, serde_json::Value::Array(env_overrides))?;
    eprintln!("  Task: {task_arn}");

    let runtime_dir = work_merge_runtime_dir(project_root, work_item_id, merge_candidate_id);
    record_task_arn(&runtime_dir, &task_arn)?;

    eprintln!("  Merge is executing on Fargate.");
    eprintln!(
        "  Use \"fluent work merge-watch {work_item_id} {merge_candidate_id}\" to follow status."
    );
    eprintln!(
        "  Use \"fluent work merge-pull {work_item_id} {merge_candidate_id}\" to retrieve results."
    );
    eprintln!(
        "  Use \"fluent work merge-stop {work_item_id} {merge_candidate_id}\" to stop the task."
    );

    Ok(())
}

/// Download the completed Merge Candidate worktrees tarball from S3.
pub fn pull_work_merge(
    project_root: &Path,
    work_item_id: &str,
    merge_candidate_id: &str,
) -> Result<()> {
    let config = load_config()?;
    let key = format!("work-merge/{work_item_id}/{merge_candidate_id}/workspace-out.tar");
    pull_worktrees(&config, project_root, &key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    #[test]
    #[serial]
    fn claude_overrides_include_oauth_token_and_fluent_coder() {
        unsafe { std::env::set_var("CLAUDE_CODE_OAUTH_TOKEN", "test-token-123") };
        let result = coder_task_overrides(CoderKind::Claude).unwrap();
        unsafe { std::env::remove_var("CLAUDE_CODE_OAUTH_TOKEN") };
        assert!(
            result
                .iter()
                .any(|(k, v)| k == "FLUENT_CODER" && v == "claude"),
            "must include FLUENT_CODER=claude"
        );
        assert!(
            result
                .iter()
                .any(|(k, v)| k == "CLAUDE_CODE_OAUTH_TOKEN" && v == "test-token-123"),
            "must include CLAUDE_CODE_OAUTH_TOKEN"
        );
    }

    #[test]
    #[serial]
    fn codex_overrides_include_auth_json_and_fluent_coder() {
        let tmp = tempfile::tempdir().unwrap();
        let codex_dir = tmp.path().join(".codex");
        fs::create_dir_all(&codex_dir).unwrap();
        let auth_json = r#"{"auth_mode":"chatgpt","refresh_token":"tok"}"#;
        fs::write(codex_dir.join("auth.json"), auth_json).unwrap();

        unsafe { std::env::set_var("HOME", tmp.path().to_str().unwrap()) };
        let result = coder_task_overrides(CoderKind::Codex).unwrap();
        assert!(
            result
                .iter()
                .any(|(k, v)| k == "FLUENT_CODER" && v == "codex"),
            "must include FLUENT_CODER=codex"
        );
        assert!(
            result
                .iter()
                .any(|(k, v)| k == "CODEX_AUTH_JSON" && v == auth_json),
            "must include CODEX_AUTH_JSON with auth.json content"
        );
    }

    #[test]
    #[serial]
    fn codex_overrides_err_when_host_auth_file_missing() {
        let tmp = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("HOME", tmp.path().to_str().unwrap()) };
        let result = coder_task_overrides(CoderKind::Codex);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("Cannot read Codex auth"),
            "error should mention missing auth: {err}"
        );
    }

    #[test]
    #[serial]
    fn codex_overrides_err_when_host_auth_mode_is_apikey() {
        let tmp = tempfile::tempdir().unwrap();
        let codex_dir = tmp.path().join(".codex");
        fs::create_dir_all(&codex_dir).unwrap();
        fs::write(
            codex_dir.join("auth.json"),
            r#"{"auth_mode":"apikey","api_key":"sk-test"}"#,
        )
        .unwrap();

        unsafe { std::env::set_var("HOME", tmp.path().to_str().unwrap()) };
        let result = coder_task_overrides(CoderKind::Codex);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("Fargate Codex requires ChatGPT subscription auth"),
            "error should mention subscription auth: {err}"
        );
    }
}
