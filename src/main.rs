use anyhow::{bail, Result};
use clap::Parser;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::Duration;

use factory::coder::{BareClaudeCode, Coder, SandboxedClaudeCode};
use factory::cli::{Cli, Commands};
use factory::content::ContentResolver;
use factory::credential;
use factory::dashboard;
use factory::fargate;
use factory::run::{self, Run};
use factory::os;
use factory::session::{self, DefaultHooks, SandboxedHooks, SessionConfig};
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
        os::check_prerequisites()?;
        let home = std::env::var("HOME").unwrap_or_default();
        let profile =
            os::render_profile(&resolver, &home, &sandbox_root.to_string_lossy())?;
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
            runtime,
            no_sandbox,
            extra_args,
        }) => match runtime.as_str() {
            "local" => {
                if no_sandbox || cli.no_sandbox {
                    cmd_run_bare(&sandbox_root, run_id.as_deref(), &resolver, &extra_args)?;
                } else {
                    cmd_run_local(&sandbox_root, run_id.as_deref(), &resolver, &extra_args)?;
                }
            }
            "fargate" => {
                fargate::launch(&sandbox_root, run_id.as_deref())?;
            }
            other => bail!("Unknown runtime '{other}'. Available: local, fargate."),
        },
        Some(Commands::Status { path }) => {
            let search_root = path.map(PathBuf::from).unwrap_or(cwd);
            cmd_status(&search_root)?;
        }
        Some(Commands::Watch { interval, timeout }) => {
            cmd_watch(&cwd, interval, timeout)?;
        }
        Some(Commands::Pull { run_id }) => {
            fargate::pull(&cwd, run_id.as_deref())?;
        }
        Some(Commands::Shell { run_id }) => {
            fargate::shell(&cwd, run_id.as_deref())?;
        }
        Some(Commands::Resume { run_id, extra_args }) => {
            cmd_resume(&cwd, run_id.as_deref(), &resolver, &extra_args)?;
        }
        Some(Commands::Init) => {
            cmd_init(&cwd)?;
        }
        Some(Commands::Dashboard { run_id, path }) => {
            let search_root = path.map(PathBuf::from).unwrap_or(cwd);
            dashboard::run_dashboard(&search_root, run_id.as_deref())?;
        }
        Some(Commands::Land { run_id }) => {
            cmd_land(&cwd, run_id.as_deref())?;
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
    os::check_prerequisites()?;
    credential::inject_credentials()?;
    credential::setup_git_signing();

    let home = std::env::var("HOME").unwrap_or_default();
    let profile = os::render_profile(resolver, &home, &sandbox_root.to_string_lossy())?;
    let system_prompt = resolver
        .resolve_content("prompts/author.md")
        .unwrap_or_default();

    eprintln!("  Factory           interactive session");
    eprintln!("  Sandbox root      {}", sandbox_root.display());

    let author = SandboxedClaudeCode {
        sandbox_profile: Some(profile.path.to_string_lossy().to_string()),
    };
    author.run_interactive(&system_prompt, sandbox_root, extra_args)?;
    Ok(())
}

fn cmd_run_local(
    source_root: &Path,
    run_id: Option<&str>,
    resolver: &ContentResolver,
    extra_args: &[String],
) -> Result<()> {
    os::check_prerequisites()?;
    credential::inject_credentials()?;
    credential::setup_git_signing();

    let run = run::resolve_run(source_root, run_id)?;
    let wt_result = worktree::setup_run_worktree(source_root, &run.id, &run.dir)?;

    // Record runtime
    fs::write(run.dir.join("runtime"), "local")?;
    fs::write(run.dir.join("handle"), std::process::id().to_string())?;

    let worktree_dir = &wt_result.worktree_dir;
    worktree::disable_commit_signing(worktree_dir)?;

    // Sandbox root must include the git directory — worktrees reference
    // the main repo's .git via a relative path. Use the parent of the
    // worktree (the factory workspace) as the sandbox root.
    let sandbox_root = worktree_dir
        .parent()
        .unwrap_or(worktree_dir)
        .to_string_lossy();
    let home = std::env::var("HOME").unwrap_or_default();
    let profile = os::render_profile(resolver, &home, &sandbox_root)?;
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

    let author = SandboxedClaudeCode {
        sandbox_profile: Some(profile.path.to_string_lossy().to_string()),
    };

    session::run_session_loop(&author, &config, &SandboxedHooks)?;
    Ok(())
}

fn cmd_run_bare(
    search_root: &Path,
    run_id: Option<&str>,
    resolver: &ContentResolver,
    extra_args: &[String],
) -> Result<()> {
    let run = run::resolve_run(search_root, run_id)?;

    // Record runtime and handle
    fs::write(run.dir.join("runtime"), "local")?;
    fs::write(run.dir.join("handle"), std::process::id().to_string())?;

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

    let author = BareClaudeCode;
    session::run_session_loop(&author, &config, &DefaultHooks)?;
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
        "RUN", "STATUS", "RUNTIME", "BRIEF"
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
        let runtime = run.runtime();
        let brief = run.brief_summary();

        println!("{:<20} {:<16} {:<10} {}", run.id, status, runtime, brief);
    }

    Ok(())
}

fn cmd_watch(search_root: &Path, interval: u64, timeout: u64) -> Result<()> {
    use std::collections::HashSet;

    eprintln!("  Watching factory runs (every {interval}s)...");
    if timeout > 0 {
        eprintln!("  Timeout: {timeout}s");
    } else {
        eprintln!("  Press Ctrl+C to stop.");
    }
    eprintln!();

    let start = std::time::Instant::now();
    let parent_pid = std::os::unix::process::parent_id();
    let mut last_output = String::new();
    let mut notified: HashSet<(String, String)> = HashSet::new();

    loop {
        // Exit if parent died (orphaned process — ppid changes)
        if std::os::unix::process::parent_id() != parent_pid {
            eprintln!("  Parent process exited — stopping watch.");
            break;
        }

        // Exit if timeout reached
        if timeout > 0 && start.elapsed().as_secs() >= timeout {
            eprintln!("  Timeout reached — stopping watch.");
            break;
        }

        let runs = run::list_runs(search_root).unwrap_or_default();
        let mut current_output = String::new();

        current_output.push_str(&format!(
            "{:<20} {:<16} {:<10} {}\n",
            "RUN", "STATUS", "RUNTIME", "BRIEF"
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
            let runtime = run.runtime();
            let brief = run.brief_summary();
            current_output.push_str(&format!(
                "{:<20} {:<16} {:<10} {}\n",
                run.id, status, runtime, brief
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
                        let key = (run.id.clone(), status_str.clone());
                        if !notified.contains(&key) {
                            notified.insert(key);
                            let body = run.notification_body();
                            notify_status_change(&body);
                            eprintln!("  [NOTIFY] Run {}: {}", run.id, status_str);
                        }
                    }
                    _ => {}
                }
            }
        }

        last_output.clone_from(&current_output);
        print!("{current_output}");
        println!("---");

        // Sleep in 1-second chunks to allow prompt timeout/ppid checks
        for _ in 0..interval {
            thread::sleep(Duration::from_secs(1));
            if timeout > 0 && start.elapsed().as_secs() >= timeout {
                eprintln!("  Timeout reached — stopping watch.");
                return Ok(());
            }
            if std::os::unix::process::parent_id() != parent_pid {
                eprintln!("  Parent process exited — stopping watch.");
                return Ok(());
            }
        }
    }

    Ok(())
}


fn cmd_resume(
    search_root: &Path,
    run_id: Option<&str>,
    resolver: &ContentResolver,
    extra_args: &[String],
) -> Result<()> {
    let run = run::resolve_resumable_run(search_root, run_id)?;

    eprintln!("  Resuming run {}", run.id);

    os::check_prerequisites()?;
    credential::inject_credentials()?;
    credential::setup_git_signing();

    let home = std::env::var("HOME").unwrap_or_default();
    let profile =
        os::render_profile(resolver, &home, &search_root.to_string_lossy())?;
    let system_prompt = resolver
        .resolve_content("prompts/author.md")
        .unwrap_or_default();

    eprintln!(
        "  Factory           resume session (run: {})",
        run.id
    );

    let author = SandboxedClaudeCode {
        sandbox_profile: Some(profile.path.to_string_lossy().to_string()),
    };
    author.run_interactive(&system_prompt, search_root, extra_args)?;
    Ok(())
}

fn cmd_init(cwd: &Path) -> Result<()> {
    let factory_dir = cwd.join(".factory");
    if factory_dir.exists() {
        eprintln!("  Already initialized: .factory/ exists in {}", cwd.display());
        return Ok(());
    }
    fs::create_dir_all(factory_dir.join("runs"))?;
    fs::create_dir_all(factory_dir.join("expertise"))?;
    eprintln!("  Initialized .factory/ in {}", cwd.display());
    Ok(())
}

fn cmd_land(search_root: &Path, run_id: Option<&str>) -> Result<()> {
    let run = run::resolve_landable_run(search_root, run_id)?;

    // Verify reviews passed — check both source and worktree run dirs
    if run.effective_reviews_passed() == Some(false) {
        bail!("Cannot land run {}: reviews did not pass", run.id);
    }

    eprintln!("  Landing run {}...", run.id);

    worktree::land_run(search_root, &run.id, &run.dir)?;
    run.set_status(&run::RunStatus::Landed)?;

    eprintln!("  Run {} landed successfully.", run.id);
    Ok(())
}

// -------------------------------------------------------------------------
// Helpers
// -------------------------------------------------------------------------


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

fn notify_status_change(body: &str) {
    let escaped = body.replace('\\', "\\\\").replace('"', "\\\"");
    Command::new("osascript")
        .args([
            "-e",
            &format!("display notification \"{escaped}\" with title \"Factory\""),
        ])
        .output()
        .ok();
}
