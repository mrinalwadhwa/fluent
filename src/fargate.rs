use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::credential;
use crate::run;
use crate::worktree;

struct FargateConfig {
    cluster: String,
    run_task: String,
    s3_bucket: String,
    subnets: String,
    security_group: String,
    region: String,
}

fn load_config() -> Result<FargateConfig> {
    let home = std::env::var("HOME").unwrap_or_default();
    let cfg_path = format!("{home}/.config/factory/fargate.env");

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
        cluster: env_required("FACTORY_CLUSTER")?,
        run_task: std::env::var("FACTORY_RUN_TASK").unwrap_or_else(|_| "factory-run".into()),
        s3_bucket: env_required("FACTORY_S3_BUCKET")?,
        subnets: env_required("FACTORY_SUBNETS")?,
        security_group: env_required("FACTORY_SECURITY_GROUP")?,
        region: std::env::var("FACTORY_REGION")
            .or_else(|_| std::env::var("AWS_DEFAULT_REGION"))
            .unwrap_or_else(|_| "us-west-1".into()),
    })
}

fn env_required(name: &str) -> Result<String> {
    std::env::var(name).map_err(|_| {
        anyhow::anyhow!(
            "{name} not set. Run infrastructure/setup.sh or set in ~/.config/factory/fargate.env"
        )
    })
}

/// Path where Factory records the ECS task ARN for a running
/// Fargate-executed Work Attempt. Lives outside of the durable
/// Work model JSON so it can be cleaned up freely after the task
/// finishes.
fn work_attempt_runtime_dir(project_root: &Path, work_item_id: &str, attempt_id: &str) -> PathBuf {
    project_root
        .join(".factory/work/runtime/attempts")
        .join(work_item_id)
        .join(attempt_id)
}

fn work_merge_runtime_dir(
    project_root: &Path,
    work_item_id: &str,
    merge_candidate_id: &str,
) -> PathBuf {
    project_root
        .join(".factory/work/runtime/merges")
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
            "factory work attempt/merge stop requested by operator",
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
    // `factory cleanup --apply`.
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

/// Upload Merge Candidate workspace + state to S3 and launch a
/// Fargate task that runs `factory work merge` for the given Work
/// Item + Merge Candidate.
pub fn launch_work_merge(
    project_root: &Path,
    work_item_id: &str,
    merge_candidate_id: &str,
) -> Result<()> {
    let config = load_config()?;
    credential::inject_credentials()?;

    let oauth = std::env::var("CLAUDE_CODE_OAUTH_TOKEN")
        .map_err(|_| anyhow::anyhow!("No Claude auth token available"))?;

    let upload_key = format!("work-merge/{work_item_id}/{merge_candidate_id}/workspace-in.tar");
    eprintln!("  Factory           fargate work merge ({work_item_id} {merge_candidate_id})");
    eprintln!(
        "  Uploading project workspace to s3://{}/{upload_key}",
        config.s3_bucket
    );
    let mut tar_child = Command::new("tar")
        .args(["cf", "-", "-C", &project_root.to_string_lossy(), "."])
        .stdout(std::process::Stdio::piped())
        .spawn()?;

    let tar_stdout = tar_child
        .stdout
        .take()
        .context("Failed to capture workspace archive output")?;
    let upload_status = Command::new("aws")
        .args(["s3", "cp", "--region", &config.region])
        .args(["-", &format!("s3://{}/{upload_key}", config.s3_bucket)])
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

    eprintln!("  Starting Fargate task...");
    let overrides = serde_json::json!({
        "containerOverrides": [{
            "name": "run",
            "environment": [
                {"name": "FACTORY_WORK_ITEM_ID", "value": work_item_id},
                {"name": "FACTORY_WORK_MERGE_CANDIDATE_ID", "value": merge_candidate_id},
                {"name": "FACTORY_S3_BUCKET", "value": config.s3_bucket},
                {"name": "FACTORY_REGION", "value": config.region},
                {"name": "CLAUDE_CODE_OAUTH_TOKEN", "value": oauth}
            ]
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
    eprintln!("  Task: {task_arn}");

    let runtime_dir = work_merge_runtime_dir(project_root, work_item_id, merge_candidate_id);
    record_task_arn(&runtime_dir, &task_arn)?;

    eprintln!("  Merge is executing on Fargate.");
    eprintln!(
        "  Use \"factory work attempt merge-pull {work_item_id} {merge_candidate_id}\" to retrieve results."
    );
    eprintln!(
        "  Use \"factory work attempt merge-stop {work_item_id} {merge_candidate_id}\" to stop the task."
    );

    Ok(())
}

/// Download the completed Merge Candidate workspace + state from S3.
pub fn pull_work_merge(
    project_root: &Path,
    work_item_id: &str,
    merge_candidate_id: &str,
) -> Result<()> {
    let config = load_config()?;
    let key = format!("work-merge/{work_item_id}/{merge_candidate_id}/workspace-out.tar");

    eprintln!(
        "  Downloading completed Merge Candidate {work_item_id}/{merge_candidate_id} from S3..."
    );
    eprintln!("  Source: s3://{}/{key}", config.s3_bucket);
    eprintln!("  Target: {}", project_root.display());

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
        .args(["xf", "-", "-C", &project_root.to_string_lossy()])
        .stdin(stdout)
        .status()?;
    let s3_status = child.wait().context("Failed to wait for s3 cp command")?;
    if !s3_status.success() {
        anyhow::bail!("Failed to download workspace from S3");
    }
    if !tar_status.success() {
        anyhow::bail!("Failed to extract workspace");
    }
    eprintln!("  Workspace and Work state extracted from S3.");
    Ok(())
}

/// Upload Work Item state + source workspace to S3 and launch a
/// Fargate task that runs `factory work attempt run` for the given
/// Work Item / Attempt pair.
pub fn launch_work_attempt(
    project_root: &Path,
    work_item_id: &str,
    attempt_id: &str,
) -> Result<()> {
    let config = load_config()?;
    credential::inject_credentials()?;

    let oauth = std::env::var("CLAUDE_CODE_OAUTH_TOKEN")
        .map_err(|_| anyhow::anyhow!("No Claude auth token available"))?;

    let upload_key = format!("work/{work_item_id}/{attempt_id}/workspace-in.tar");
    eprintln!("  Factory           fargate work attempt run ({work_item_id} {attempt_id})");
    eprintln!(
        "  Uploading project workspace to s3://{}/{upload_key}",
        config.s3_bucket
    );
    let mut tar_child = Command::new("tar")
        .args(["cf", "-", "-C", &project_root.to_string_lossy(), "."])
        .stdout(std::process::Stdio::piped())
        .spawn()?;

    let tar_stdout = tar_child
        .stdout
        .take()
        .context("Failed to capture workspace archive output")?;
    let upload_status = Command::new("aws")
        .args(["s3", "cp", "--region", &config.region])
        .args(["-", &format!("s3://{}/{upload_key}", config.s3_bucket)])
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

    eprintln!("  Starting Fargate task...");
    let overrides = serde_json::json!({
        "containerOverrides": [{
            "name": "run",
            "environment": [
                {"name": "FACTORY_WORK_ITEM_ID", "value": work_item_id},
                {"name": "FACTORY_WORK_ATTEMPT_ID", "value": attempt_id},
                {"name": "FACTORY_S3_BUCKET", "value": config.s3_bucket},
                {"name": "FACTORY_REGION", "value": config.region},
                {"name": "CLAUDE_CODE_OAUTH_TOKEN", "value": oauth}
            ]
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
    eprintln!("  Task: {task_arn}");

    let runtime_dir = work_attempt_runtime_dir(project_root, work_item_id, attempt_id);
    record_task_arn(&runtime_dir, &task_arn)?;

    eprintln!("  Attempt is executing on Fargate.");
    eprintln!(
        "  Use \"factory work attempt pull {work_item_id} {attempt_id}\" to retrieve results when the task finishes."
    );
    eprintln!("  Use \"factory work attempt stop {work_item_id} {attempt_id}\" to stop the task.");

    Ok(())
}

/// Download the completed Work Attempt workspace + state from S3
/// into the project workspace, overlaying changes back into local
/// `.factory/work/` and any sibling candidate worktrees the remote
/// created or modified.
pub fn pull_work_attempt(project_root: &Path, work_item_id: &str, attempt_id: &str) -> Result<()> {
    let config = load_config()?;
    let key = format!("work/{work_item_id}/{attempt_id}/workspace-out.tar");

    eprintln!("  Downloading completed Work Attempt {work_item_id}/{attempt_id} from S3...");
    eprintln!("  Source: s3://{}/{key}", config.s3_bucket);
    eprintln!("  Target: {}", project_root.display());

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
        .args(["xf", "-", "-C", &project_root.to_string_lossy()])
        .stdin(stdout)
        .status()?;
    let s3_status = child.wait().context("Failed to wait for s3 cp command")?;
    if !s3_status.success() {
        anyhow::bail!("Failed to download workspace from S3");
    }
    if !tar_status.success() {
        anyhow::bail!("Failed to extract workspace");
    }
    eprintln!("  Workspace and Work state extracted from S3.");
    Ok(())
}

/// Upload worktree to S3, start Fargate task, record runtime metadata.
pub fn launch(source_root: &Path, run_id: Option<&str>) -> Result<()> {
    let config = load_config()?;
    credential::inject_credentials()?;

    let run = run::resolve_run(source_root, run_id)?;
    let wt_result = worktree::setup_run_worktree(source_root, &run.id, &run.dir)?;

    eprintln!("  Factory           fargate run (run: {})", run.id);

    let oauth = std::env::var("CLAUDE_CODE_OAUTH_TOKEN")
        .map_err(|_| anyhow::anyhow!("No Claude auth token available"))?;

    // Upload worktree to S3
    eprintln!("  Uploading worktree to S3...");
    let mut tar_child = Command::new("tar")
        .args([
            "cf",
            "-",
            "-C",
            &wt_result.worktree_dir.to_string_lossy(),
            ".",
        ])
        .stdout(std::process::Stdio::piped())
        .spawn()?;

    let tar_stdout = tar_child
        .stdout
        .take()
        .context("Failed to capture workspace archive output")?;
    let upload_status = Command::new("aws")
        .args(["s3", "cp", "--region", &config.region])
        .args([
            "-",
            &format!("s3://{}/runs/{}/workspace-in.tar", config.s3_bucket, run.id),
        ])
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

    // Start ECS task
    eprintln!("  Starting Fargate task...");
    let overrides = serde_json::json!({
        "containerOverrides": [{
            "name": "run",
            "environment": [
                {"name": "FACTORY_RUN_ID", "value": run.id},
                {"name": "FACTORY_S3_BUCKET", "value": config.s3_bucket},
                {"name": "FACTORY_REGION", "value": config.region},
                {"name": "CLAUDE_CODE_OAUTH_TOKEN", "value": oauth}
            ]
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
    eprintln!("  Task: {task_arn}");

    fs::write(run.dir.join("runtime"), "fargate")?;
    fs::write(run.dir.join("handle"), &task_arn)?;

    eprintln!("  Run is executing on Fargate.");
    eprintln!("  Worktree: {}", wt_result.worktree_dir.display());
    eprintln!("  Use \"factory status\" to check progress.");
    eprintln!("  Use \"factory shell\" to attach to the running session.");
    eprintln!("  Use \"factory pull\" to retrieve results.");

    Ok(())
}

/// Download completed workspace from S3 into the run's worktree.
pub fn pull(search_root: &Path, run_id: Option<&str>) -> Result<()> {
    let runs_dir = search_root.join(".factory/runs");

    let run_id = if let Some(id) = run_id {
        id.to_string()
    } else {
        let mut found = None;
        if runs_dir.is_dir() {
            for entry in fs::read_dir(&runs_dir)? {
                let entry = entry?;
                let runtime = fs::read_to_string(entry.path().join("runtime")).unwrap_or_default();
                if runtime.trim() == "fargate" {
                    found = Some(entry.file_name().to_string_lossy().to_string());
                    break;
                }
            }
        }
        found.ok_or_else(|| anyhow::anyhow!("No fargate run found."))?
    };

    let config = load_config()?;

    let run_dir = runs_dir.join(&run_id);
    let worktree_path = fs::read_to_string(run_dir.join("worktree")).unwrap_or_default();
    let target = if !worktree_path.is_empty() && Path::new(&worktree_path).is_dir() {
        std::path::PathBuf::from(worktree_path)
    } else {
        let project_root = search_root.parent().unwrap_or(search_root);
        let target = project_root.join(&run_id);
        fs::create_dir_all(&target)?;
        target
    };

    eprintln!("  Downloading workspace for run {run_id}...");
    eprintln!("  Target: {}", target.display());

    let s3_pipe = Command::new("aws")
        .args(["s3", "cp", "--region", &config.region])
        .args([
            &format!("s3://{}/runs/{run_id}/workspace.tar", config.s3_bucket),
            "-",
        ])
        .stdout(std::process::Stdio::piped())
        .spawn()?;

    Command::new("tar")
        .args(["xf", "-", "-C", &target.to_string_lossy()])
        .stdin(s3_pipe.stdout.unwrap())
        .status()?;

    eprintln!("  Workspace downloaded to {}", target.display());
    Ok(())
}

/// Open an interactive shell into the running Fargate container.
pub fn shell(search_root: &Path, run_id: Option<&str>) -> Result<()> {
    let run = run::resolve_run(search_root, run_id)?;

    let task_arn = run
        .handle()
        .ok_or_else(|| anyhow::anyhow!("No task handle found for run {}", run.id))?;

    let config = load_config()?;

    eprintln!("  Connecting to run {}...", run.id);
    let status = Command::new("aws")
        .args(["ecs", "execute-command"])
        .args(["--region", &config.region])
        .args(["--cluster", &config.cluster])
        .args(["--task", &task_arn])
        .args(["--container", "run"])
        .args(["--command", "/bin/bash"])
        .args(["--interactive"])
        .status()?;

    std::process::exit(status.code().unwrap_or(1));
}
