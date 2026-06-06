use anyhow::{Result, bail};
use clap::Parser;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::Duration;

use factory::cli::{Cli, Commands};
use factory::coder::{CoderKind, CoderSandbox};
use factory::content::ContentResolver;
use factory::credential;
use factory::dashboard;
use factory::fargate;
use factory::land;
use factory::os;
use factory::parallel;
use factory::plan;
use factory::run::{self, Run};
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
        let coder_kind = CoderKind::resolve(cli.coder.as_deref())?;
        os::check_prerequisites_for(coder_kind)?;
        let home = std::env::var("HOME").unwrap_or_default();
        let profile = os::render_profile_for_roots_for_coder(
            &resolver,
            &home,
            std::slice::from_ref(&sandbox_root),
            coder_kind,
        )?;
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
            coder,
            extra_args,
        }) => match runtime.as_str() {
            "local" => {
                let coder_kind = CoderKind::resolve(coder.as_deref().or(cli.coder.as_deref()))?;
                if no_sandbox || cli.no_sandbox {
                    cmd_run_bare(
                        &sandbox_root,
                        run_id.as_deref(),
                        &resolver,
                        &extra_args,
                        coder_kind,
                    )?;
                } else {
                    cmd_run_local(
                        &sandbox_root,
                        run_id.as_deref(),
                        &resolver,
                        &extra_args,
                        coder_kind,
                    )?;
                }
            }
            "fargate" => {
                let coder_kind = CoderKind::resolve(coder.as_deref().or(cli.coder.as_deref()))?;
                if coder_kind != CoderKind::Claude {
                    bail!("Fargate runtime currently supports only the claude coder");
                }
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
        Some(Commands::Resume {
            run_id,
            coder,
            extra_args,
        }) => {
            let coder_kind = CoderKind::resolve(coder.as_deref().or(cli.coder.as_deref()))?;
            cmd_resume(&cwd, run_id.as_deref(), &resolver, &extra_args, coder_kind)?;
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
            let coder_kind = CoderKind::resolve(cli.coder.as_deref())?;
            cmd_interactive(&sandbox_root, &resolver, &cli.extra_args, coder_kind)?;
        }
    }

    Ok(())
}

fn cmd_interactive(
    sandbox_root: &Path,
    resolver: &ContentResolver,
    extra_args: &[String],
    coder_kind: CoderKind,
) -> Result<()> {
    os::check_prerequisites_for(coder_kind)?;
    credential::inject_credentials()?;
    credential::setup_git_signing();

    let (sandbox, _sandbox_profile) = build_coder_sandbox(coder_kind, resolver, sandbox_root, &[])?;
    let system_prompt = resolver
        .resolve_content("prompts/author.md")
        .unwrap_or_default();

    eprintln!("  Factory           interactive session");
    eprintln!("  Sandbox root      {}", sandbox_root.display());

    let author = coder_kind.boxed(sandbox);
    author.run_interactive(&system_prompt, sandbox_root, extra_args)?;
    Ok(())
}

fn cmd_run_local(
    source_root: &Path,
    run_id: Option<&str>,
    resolver: &ContentResolver,
    extra_args: &[String],
    coder_kind: CoderKind,
) -> Result<()> {
    os::check_prerequisites_for(coder_kind)?;
    credential::inject_credentials()?;
    credential::setup_git_signing();

    let run = run::resolve_run(source_root, run_id)?;

    // Record runtime
    fs::write(run.dir.join("runtime"), "local")?;
    fs::write(run.dir.join("handle"), std::process::id().to_string())?;
    fs::write(run.dir.join("coder"), coder_kind.as_str())?;

    // Check for a parallel plan
    if let Some(parsed_plan) = try_parse_parallel_plan(&run) {
        let common_git_dir = worktree::git_common_dir(source_root)?;
        let sandbox = build_parallel_coder_sandbox(coder_kind, vec![common_git_dir]);
        let system_prompt = resolver
            .resolve_content("prompts/author.md")
            .unwrap_or_default();

        eprintln!("  Factory           parallel plan (run: {})", run.id);

        return parallel::run_parallel_plan(
            source_root,
            &run,
            &parsed_plan,
            &system_prompt,
            extra_args,
            coder_kind,
            sandbox,
        );
    }

    // Standard single-run flow
    let wt_result = worktree::setup_run_worktree(source_root, &run.id, &run.dir)?;

    let worktree_dir = &wt_result.worktree_dir;
    worktree::disable_commit_signing(worktree_dir)?;

    let common_git_dir = worktree::git_common_dir(source_root)?;
    let (sandbox, _sandbox_profile) =
        build_coder_sandbox(coder_kind, resolver, worktree_dir, &[common_git_dir])?;
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

    let author = coder_kind.boxed(sandbox);

    if coder_kind == CoderKind::Claude {
        session::run_session_loop(&*author, &config, &SandboxedHooks, coder_kind)?;
    } else {
        session::run_session_loop(&*author, &config, &DefaultHooks, coder_kind)?;
    }
    Ok(())
}

fn cmd_run_bare(
    search_root: &Path,
    run_id: Option<&str>,
    resolver: &ContentResolver,
    extra_args: &[String],
    coder_kind: CoderKind,
) -> Result<()> {
    let run = run::resolve_run(search_root, run_id)?;

    // Record runtime and handle
    fs::write(run.dir.join("runtime"), "local")?;
    fs::write(run.dir.join("handle"), std::process::id().to_string())?;
    fs::write(run.dir.join("coder"), coder_kind.as_str())?;

    // Check for a parallel plan (requires git)
    if worktree::is_git_repo(search_root) {
        if let Some(parsed_plan) = try_parse_parallel_plan(&run) {
            let system_prompt = resolver
                .resolve_content("prompts/author.md")
                .unwrap_or_default();

            eprintln!("factory: bare parallel plan (run: {})", run.id);

            return parallel::run_parallel_plan(
                search_root,
                &run,
                &parsed_plan,
                &system_prompt,
                extra_args,
                coder_kind,
                CoderSandbox::None,
            );
        }
    }

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

    let author = coder_kind.boxed(CoderSandbox::None);
    session::run_session_loop(&*author, &config, &DefaultHooks, coder_kind)?;
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
                let status_str = run.status().map(|s| s.to_string()).unwrap_or_default();
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
    coder_kind: CoderKind,
) -> Result<()> {
    let run = run::resolve_resumable_run(search_root, run_id)?;

    eprintln!("  Resuming run {}", run.id);

    os::check_prerequisites_for(coder_kind)?;
    credential::inject_credentials()?;
    credential::setup_git_signing();

    let (sandbox, _sandbox_profile) = build_coder_sandbox(coder_kind, resolver, search_root, &[])?;
    let system_prompt = resolver
        .resolve_content("prompts/author.md")
        .unwrap_or_default();

    eprintln!("  Factory           resume session (run: {})", run.id);

    let author = coder_kind.boxed(sandbox);
    author.run_interactive(&system_prompt, search_root, extra_args)?;
    Ok(())
}

fn cmd_init(cwd: &Path) -> Result<()> {
    let factory_dir = cwd.join(".factory");
    if factory_dir.exists() {
        eprintln!(
            "  Already initialized: .factory/ exists in {}",
            cwd.display()
        );
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

    // Parallel parent runs have no worktree — their children were already
    // merged by the orchestrator. Just verify children are landed and set
    // the parent status.
    let children_file = run.dir.join("children");
    if children_file.exists() {
        let children_content = fs::read_to_string(&children_file)?;
        let runs_dir = search_root.join(".factory/runs");
        for child_id in children_content.lines().filter(|l| !l.is_empty()) {
            let child_dir = runs_dir.join(child_id);
            let child_run = run::Run {
                id: child_id.to_string(),
                dir: child_dir,
            };
            let status = child_run.effective_status()?;
            if status != run::RunStatus::Landed {
                bail!(
                    "Cannot land parent run {}: child {} has status '{}', expected 'landed'",
                    run.id,
                    child_id,
                    status
                );
            }
        }
    } else {
        land::land_worktree_run(search_root, &run)?;
    }

    run.set_status(&run::RunStatus::Landed)?;

    eprintln!("  Run {} landed successfully.", run.id);
    Ok(())
}

fn build_coder_sandbox(
    coder_kind: CoderKind,
    resolver: &ContentResolver,
    working_dir: &Path,
    additional_writable_roots: &[PathBuf],
) -> Result<(CoderSandbox, Option<os::SandboxProfile>)> {
    match coder_kind {
        CoderKind::Claude => {
            let home = std::env::var("HOME").unwrap_or_default();
            let mut roots = vec![working_dir.to_path_buf()];
            roots.extend(additional_writable_roots.iter().cloned());
            let profile =
                os::render_profile_for_roots_for_coder(resolver, &home, &roots, coder_kind)?;
            let sandbox = CoderSandbox::SeatbeltProfile(profile.path.to_string_lossy().to_string());
            Ok((sandbox, Some(profile)))
        }
        CoderKind::Codex => {
            let home = std::env::var("HOME").unwrap_or_default();
            let mut roots = vec![working_dir.to_path_buf()];
            roots.extend(additional_writable_roots.iter().cloned());
            let profile =
                os::render_profile_for_roots_for_coder(resolver, &home, &roots, coder_kind)?;
            let sandbox = CoderSandbox::SeatbeltProfile(profile.path.to_string_lossy().to_string());
            Ok((sandbox, Some(profile)))
        }
    }
}

fn build_parallel_coder_sandbox(
    coder_kind: CoderKind,
    additional_writable_roots: Vec<PathBuf>,
) -> CoderSandbox {
    match coder_kind {
        CoderKind::Claude | CoderKind::Codex => CoderSandbox::SeatbeltRoots {
            writable_roots: additional_writable_roots,
        },
    }
}

// -------------------------------------------------------------------------
// Helpers
// -------------------------------------------------------------------------

/// Try to parse the run's plan.md as a parallel plan.
///
/// Returns `Some(plan)` if plan.md exists and describes a parallel
/// execution (multiple groups or any parallel group with multiple steps).
/// Returns `None` if the plan is missing, unparseable, or sequential-only.
fn try_parse_parallel_plan(run: &Run) -> Option<plan::Plan> {
    let content = fs::read_to_string(run.dir.join("plan.md")).ok()?;
    let parsed = plan::parse_plan(&content).ok()?;
    if parsed.needs_orchestrator() {
        Some(parsed)
    } else {
        None
    }
}

fn dirs_log_file() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    PathBuf::from(home).join(".local/state/factory/factory.log")
}

fn kill_existing_claude() -> Result<()> {
    let output = Command::new("pgrep").args(["-f", "claude"]).output();
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

            let output = Command::new("pgrep").args(["-f", "claude"]).output();
            if let Ok(output) = output {
                if output.status.success() {
                    let pids = String::from_utf8_lossy(&output.stdout);
                    for pid in pids.lines() {
                        let pid = pid.trim();
                        if !pid.is_empty() {
                            Command::new("kill").args(["-9", pid]).output().ok();
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
