use anyhow::Result;
use std::fs;
use std::path::Path;
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
        run_task: std::env::var("FACTORY_RUN_TASK")
            .unwrap_or_else(|_| "factory-run".into()),
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
    let tar_child = Command::new("tar")
        .args([
            "cf",
            "-",
            "-C",
            &wt_result.worktree_dir.to_string_lossy(),
            ".",
        ])
        .stdout(std::process::Stdio::piped())
        .spawn()?;

    Command::new("aws")
        .args(["s3", "cp", "--region", &config.region])
        .args([
            "-",
            &format!(
                "s3://{}/runs/{}/workspace-in.tar",
                config.s3_bucket, run.id
            ),
        ])
        .stdin(tar_child.stdout.unwrap())
        .status()?;

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

    let task_arn = String::from_utf8_lossy(&output.stdout).trim().to_string();
    eprintln!("  Task: {task_arn}");

    fs::write(run.dir.join("runtime"), "fargate")?;
    fs::write(run.dir.join("handle"), &task_arn)?;

    eprintln!("  Run is executing on Fargate.");
    eprintln!(
        "  Worktree: {}",
        wt_result.worktree_dir.display()
    );
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
                let runtime =
                    fs::read_to_string(entry.path().join("runtime")).unwrap_or_default();
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
    let worktree_path =
        fs::read_to_string(run_dir.join("worktree")).unwrap_or_default();
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
            &format!(
                "s3://{}/runs/{run_id}/workspace.tar",
                config.s3_bucket
            ),
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
