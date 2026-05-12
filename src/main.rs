use anyhow::{bail, Result};
use clap::Parser;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::Duration;

use factory::agent::{BareClaudeCode, SandboxedClaudeCode};
use factory::cli::{Cli, Commands};
use factory::content::ContentResolver;
use factory::credential;
use factory::run::{self, Run};
use factory::sandbox;
use factory::session::{self, DefaultHooks, SessionConfig};
use factory::worktree;

fn main() -> Result<()> {
    let cli = Cli::parse();

    let cwd = std::env::current_dir()?;
    let sandbox_root = cli
        .sandbox_root
        .as_deref()
        .map(PathBuf::from)
        .unwrap_or_else(|| cwd.clone());
    let sandbox_root = fs::canonicalize(&sandbox_root).unwrap_or(sandbox_root);

    // --logs: tail the log file
    if cli.logs {
        let log_file = dirs_log_file();
        if !log_file.exists() {
            bail!("No log file yet — run factory first");
        }
        let status = Command::new("tail")
            .args(["-f", &log_file.to_string_lossy()])
            .status()?;
        std::process::exit(status.code().unwrap_or(1));
    }

    // --force: kill existing Claude processes
    if cli.force {
        kill_existing_claude()?;
    }

    let resolver = ContentResolver::new(Some(&sandbox_root));

    // --dry-run: render sandbox profile and exit
    if cli.dry_run {
        sandbox::check_prerequisites()?;
        let home = std::env::var("HOME").unwrap_or_default();
        let profile =
            sandbox::render_profile(&resolver, &home, &sandbox_root.to_string_lossy())?;
        println!("--- Rendered Seatbelt profile ---");
        println!("HOME         = {home}");
        println!("SANDBOX_ROOT = {}", sandbox_root.display());
        println!("---------------------------------");
        println!("{}", fs::read_to_string(&profile.path)?);
        return Ok(());
    }

    match cli.command {
        Some(Commands::Run {
            run_id,
            backend,
            no_sandbox,
            extra_args,
        }) => match backend.as_str() {
            "local" => {
                if no_sandbox || cli.no_sandbox {
                    cmd_run_bare(&sandbox_root, run_id.as_deref(), &resolver, &extra_args)?;
                } else {
                    cmd_run_local(&sandbox_root, run_id.as_deref(), &resolver, &extra_args)?;
                }
            }
            "fargate" => {
                cmd_run_fargate(&sandbox_root, run_id.as_deref())?;
            }
            other => bail!("Unknown backend '{other}'. Available: local, fargate."),
        },
        Some(Commands::Status { path }) => {
            let search_root = path.map(PathBuf::from).unwrap_or(cwd);
            cmd_status(&search_root)?;
        }
        Some(Commands::Watch { interval }) => {
            cmd_watch(&cwd, interval)?;
        }
        Some(Commands::Pull { run_id }) => {
            cmd_pull(&cwd, run_id.as_deref())?;
        }
        Some(Commands::Shell { run_id }) => {
            cmd_shell(&cwd, run_id.as_deref())?;
        }
        Some(Commands::Resume { run_id, extra_args }) => {
            cmd_resume(&cwd, run_id.as_deref(), &resolver, &extra_args)?;
        }
        Some(Commands::Init) => {
            cmd_init(&cwd)?;
        }
        None => {
            cmd_interactive(&sandbox_root, &resolver, &cli.extra_args)?;
        }
    }

    Ok(())
}

fn cmd_interactive(
    sandbox_root: &Path,
    resolver: &ContentResolver,
    extra_args: &[String],
) -> Result<()> {
    sandbox::check_prerequisites()?;
    credential::inject_credentials()?;
    credential::setup_git_signing();

    let home = std::env::var("HOME").unwrap_or_default();
    let profile = sandbox::render_profile(resolver, &home, &sandbox_root.to_string_lossy())?;
    let system_prompt = resolver
        .resolve_content("prompts/author.md")
        .unwrap_or_default();

    eprintln!("  Factory           interactive session");
    eprintln!("  Sandbox root      {}", sandbox_root.display());

    let agent = SandboxedClaudeCode {
        sandbox_profile: Some(profile.path.to_string_lossy().to_string()),
    };
    use factory::agent::Agent;
    agent.run_interactive(&system_prompt, sandbox_root, extra_args)?;
    Ok(())
}

fn cmd_run_local(
    source_root: &Path,
    run_id: Option<&str>,
    resolver: &ContentResolver,
    extra_args: &[String],
) -> Result<()> {
    sandbox::check_prerequisites()?;
    credential::inject_credentials()?;
    credential::setup_git_signing();

    let run = run::resolve_run(source_root, run_id)?;
    let wt_result = worktree::setup_run_worktree(source_root, &run.id, &run.dir)?;

    // Record backend
    fs::write(run.dir.join("backend"), "local")?;
    fs::write(run.dir.join("handle"), std::process::id().to_string())?;

    let worktree_dir = &wt_result.worktree_dir;
    worktree::disable_commit_signing(worktree_dir)?;

    let home = std::env::var("HOME").unwrap_or_default();
    let profile = sandbox::render_profile(resolver, &home, &worktree_dir.to_string_lossy())?;
    let system_prompt = resolver
        .resolve_content("prompts/author.md")
        .unwrap_or_default();

    eprintln!("  Factory           session loop (run: {})", run.id);
    eprintln!("  Worktree          {}", worktree_dir.display());

    let wt_run = Run {
        id: run.id.clone(),
        dir: worktree_dir.join(format!(".factory/runs/{}", run.id)),
    };

    let config = SessionConfig {
        run: wt_run,
        system_prompt,
        working_dir: worktree_dir.clone(),
        extra_args: extra_args.to_vec(),
        resolver: ContentResolver::new(Some(worktree_dir)),
    };

    let agent = SandboxedClaudeCode {
        sandbox_profile: Some(profile.path.to_string_lossy().to_string()),
    };

    session::run_session_loop(&agent, &config, &DefaultHooks)?;
    Ok(())
}

fn cmd_run_bare(
    search_root: &Path,
    run_id: Option<&str>,
    resolver: &ContentResolver,
    extra_args: &[String],
) -> Result<()> {
    let run = run::resolve_run(search_root, run_id)?;

    let (working_dir, wt_run) = if worktree::is_git_repo(search_root) {
        let wt_result = worktree::setup_run_worktree(search_root, &run.id, &run.dir)?;
        let worktree_dir = wt_result.worktree_dir;
        worktree::disable_commit_signing(&worktree_dir)?;

        eprintln!("factory: bare session loop (run: {})", run.id);
        eprintln!("  Worktree          {}", worktree_dir.display());

        let wt_run = Run {
            id: run.id.clone(),
            dir: worktree_dir.join(format!(".factory/runs/{}", run.id)),
        };
        (worktree_dir, wt_run)
    } else {
        eprintln!("factory: bare session loop (run: {})", run.id);
        (search_root.to_path_buf(), run)
    };

    let system_prompt = resolver
        .resolve_content("prompts/author.md")
        .unwrap_or_default();

    let config = SessionConfig {
        run: wt_run,
        system_prompt,
        working_dir: working_dir.clone(),
        extra_args: extra_args.to_vec(),
        resolver: ContentResolver::new(Some(&working_dir)),
    };

    let agent = BareClaudeCode;
    session::run_session_loop(&agent, &config, &DefaultHooks)?;
    Ok(())
}

fn cmd_run_fargate(source_root: &Path, run_id: Option<&str>) -> Result<()> {
    let config = load_fargate_config()?;
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

    fs::write(run.dir.join("backend"), "fargate")?;
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

fn cmd_status(search_root: &Path) -> Result<()> {
    let runs_dir = search_root.join(".factory/runs");

    if !runs_dir.is_dir() {
        println!("No runs found in {}", search_root.display());
        return Ok(());
    }

    let runs = run::list_runs(search_root)?;

    println!(
        "{:<20} {:<16} {:<10} {}",
        "RUN", "STATUS", "BACKEND", "BRIEF"
    );
    println!(
        "{:<20} {:<16} {:<10} {}",
        "---", "------", "-------", "-----"
    );

    for run in &runs {
        let status = run
            .status()
            .map(|s| s.to_string())
            .unwrap_or_else(|_| "-".into());
        let backend = run.backend();
        let brief = run.brief_summary();

        println!("{:<20} {:<16} {:<10} {}", run.id, status, backend, brief);
    }

    Ok(())
}

fn cmd_watch(search_root: &Path, interval: u64) -> Result<()> {
    eprintln!("  Watching factory runs (every {interval}s)...");
    eprintln!("  Press Ctrl+C to stop.\n");

    let mut last_output = String::new();

    loop {
        let runs = run::list_runs(search_root).unwrap_or_default();
        let mut current_output = String::new();

        current_output.push_str(&format!(
            "{:<20} {:<16} {:<10} {}\n",
            "RUN", "STATUS", "BACKEND", "BRIEF"
        ));
        current_output.push_str(&format!(
            "{:<20} {:<16} {:<10} {}\n",
            "---", "------", "-------", "-----"
        ));

        for run in &runs {
            let status = run
                .status()
                .map(|s| s.to_string())
                .unwrap_or_else(|_| "-".into());
            let backend = run.backend();
            let brief = run.brief_summary();
            current_output.push_str(&format!(
                "{:<20} {:<16} {:<10} {}\n",
                run.id, status, backend, brief
            ));
        }

        if current_output != last_output && !last_output.is_empty() {
            // Check for notification-worthy changes
            for run in &runs {
                let status_str = run
                    .status()
                    .map(|s| s.to_string())
                    .unwrap_or_default();
                match status_str.as_str() {
                    "complete" | "needs-user" | "failed" => {
                        notify_status_change(&run.id, &status_str);
                        eprintln!("  [NOTIFY] Run {}: {}", run.id, status_str);
                    }
                    _ => {}
                }
            }
        }

        last_output.clone_from(&current_output);
        print!("{current_output}");
        println!("---");
        thread::sleep(Duration::from_secs(interval));
    }
}

fn cmd_pull(search_root: &Path, run_id: Option<&str>) -> Result<()> {
    let config = load_fargate_config()?;
    let runs_dir = search_root.join(".factory/runs");

    let run_id = if let Some(id) = run_id {
        id.to_string()
    } else {
        let mut found = None;
        if runs_dir.is_dir() {
            for entry in fs::read_dir(&runs_dir)? {
                let entry = entry?;
                let backend =
                    fs::read_to_string(entry.path().join("backend")).unwrap_or_default();
                if backend.trim() == "fargate" {
                    found = Some(entry.file_name().to_string_lossy().to_string());
                    break;
                }
            }
        }
        found.ok_or_else(|| anyhow::anyhow!("No fargate run found."))?
    };

    let run_dir = runs_dir.join(&run_id);
    let worktree_path =
        fs::read_to_string(run_dir.join("worktree")).unwrap_or_default();
    let target = if !worktree_path.is_empty() && Path::new(&worktree_path).is_dir() {
        PathBuf::from(worktree_path)
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

fn cmd_shell(search_root: &Path, run_id: Option<&str>) -> Result<()> {
    let config = load_fargate_config()?;
    let run = run::resolve_run(search_root, run_id)?;

    let task_arn = run
        .handle()
        .ok_or_else(|| anyhow::anyhow!("No task handle found for run {}", run.id))?;

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

fn cmd_resume(
    search_root: &Path,
    run_id: Option<&str>,
    resolver: &ContentResolver,
    extra_args: &[String],
) -> Result<()> {
    let run = run::resolve_resumable_run(search_root, run_id)?;

    eprintln!("  Resuming run {}", run.id);

    sandbox::check_prerequisites()?;
    credential::inject_credentials()?;
    credential::setup_git_signing();

    let home = std::env::var("HOME").unwrap_or_default();
    let profile =
        sandbox::render_profile(resolver, &home, &search_root.to_string_lossy())?;
    let system_prompt = resolver
        .resolve_content("prompts/author.md")
        .unwrap_or_default();

    eprintln!(
        "  Factory           resume session (run: {})",
        run.id
    );

    let agent = SandboxedClaudeCode {
        sandbox_profile: Some(profile.path.to_string_lossy().to_string()),
    };
    use factory::agent::Agent;
    agent.run_interactive(&system_prompt, search_root, extra_args)?;
    Ok(())
}

fn cmd_init(cwd: &Path) -> Result<()> {
    let factory_dir = cwd.join(".factory");
    fs::create_dir_all(factory_dir.join("runs"))?;
    fs::create_dir_all(factory_dir.join("expertise"))?;
    eprintln!("  Initialized .factory/ in {}", cwd.display());
    Ok(())
}

// -------------------------------------------------------------------------
// Helpers
// -------------------------------------------------------------------------

struct FargateConfig {
    cluster: String,
    run_task: String,
    s3_bucket: String,
    subnets: String,
    security_group: String,
    region: String,
}

fn load_fargate_config() -> Result<FargateConfig> {
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

fn dirs_log_file() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    PathBuf::from(home).join(".local/state/factory/factory.log")
}

fn kill_existing_claude() -> Result<()> {
    let output = Command::new("pgrep")
        .args(["-f", "claude"])
        .output();
    if let Ok(output) = output {
        if output.status.success() {
            let pids = String::from_utf8_lossy(&output.stdout);
            eprintln!("  Stopping existing Claude Code process(es)...");
            for pid in pids.lines() {
                let pid = pid.trim();
                if !pid.is_empty() {
                    Command::new("kill").arg(pid).output().ok();
                }
            }
            thread::sleep(Duration::from_secs(3));

            let output = Command::new("pgrep")
                .args(["-f", "claude"])
                .output();
            if let Ok(output) = output {
                if output.status.success() {
                    let pids = String::from_utf8_lossy(&output.stdout);
                    for pid in pids.lines() {
                        let pid = pid.trim();
                        if !pid.is_empty() {
                            Command::new("kill")
                                .args(["-9", pid])
                                .output()
                                .ok();
                        }
                    }
                    thread::sleep(Duration::from_millis(500));
                }
            }
            eprintln!("  Existing Claude Code stopped.");
        }
    }
    Ok(())
}

fn notify_status_change(run_id: &str, status: &str) {
    Command::new("osascript")
        .args([
            "-e",
            &format!(
                "display notification \"Run {run_id}: {status}\" with title \"Factory\""
            ),
        ])
        .output()
        .ok();
}
