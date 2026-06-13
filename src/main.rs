use anyhow::{Result, bail};
use clap::Parser;
use std::fs;
use std::io::ErrorKind;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::Duration;

use factory::cleanup::{
    self, CleanupOptions, WorkBranchCleanup, WorkCleanupResult, WorktreeCleanup,
};
use factory::cli;
use factory::cli::{
    Cli, Commands, FargateCommands, ObservationsCommands, WorkAttemptCommands, WorkCommands,
    WorkTaskCommands,
};
use factory::coder::{CoderKind, CoderSandbox};
use factory::content::ContentResolver;
use factory::credential;
use factory::dashboard;
use factory::fargate;
use factory::git;
use factory::fargate_bootstrap;
use factory::merge;
use factory::observations;
use factory::os;
use factory::parallel;
use factory::plan;
use factory::post_merge_review;
use factory::review;
use factory::run::{self, Run};
use factory::session::{self, DefaultHooks, SandboxedHooks, SessionConfig};
use factory::summary;
use factory::version;
use factory::work_attempt_loop::{self, WorkAttemptRunConfig, WorkAttemptRunOutcome};
use factory::work_merge_executor::{self, WorkMergeConfig};
use factory::work_model::{
    PlanningContext, WorkItem, WorkModelStorageError, WorkModelStore, to_json_pretty,
};
use factory::work_status;
use factory::work_task_executor::{self, WorkTaskRunConfig};
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
            in_place,
            preserve_run_metadata,
            coder,
            extra_args,
        }) => match runtime.as_str() {
            "local" => {
                let coder_kind = CoderKind::resolve(coder.as_deref().or(cli.coder.as_deref()))?;
                if in_place {
                    cmd_run_in_place(
                        &sandbox_root,
                        run_id.as_deref(),
                        &resolver,
                        &extra_args,
                        coder_kind,
                        preserve_run_metadata,
                    )?;
                } else if no_sandbox || cli.no_sandbox {
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
        Some(Commands::Review {
            run_id,
            reviewers,
            brief,
            no_sandbox,
            coder,
            extra_args,
        }) => {
            let coder_kind = CoderKind::resolve(coder.as_deref().or(cli.coder.as_deref()))?;
            cmd_review(
                &sandbox_root,
                run_id.as_deref(),
                reviewers.as_deref(),
                brief.as_deref(),
                &resolver,
                &extra_args,
                coder_kind,
                no_sandbox || cli.no_sandbox,
            )?;
        }
        Some(Commands::Status { runs, path }) => {
            let search_root = path.map(PathBuf::from).unwrap_or(cwd);
            cmd_status(&search_root, runs)?;
        }
        Some(Commands::Work { command }) => {
            cmd_work(
                &cwd,
                command,
                cli.coder.as_deref(),
                cli.no_sandbox,
                &resolver,
            )?;
        }
        Some(Commands::Summary { run_id }) => {
            let output = summary::summarize_run(&cwd, run_id.as_deref())?;
            print!("{output}");
        }
        Some(Commands::Cleanup { run_id, apply }) => {
            cmd_cleanup(&cwd, run_id, apply)?;
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
            no_sandbox,
            coder,
            extra_args,
        }) => {
            let coder_kind = CoderKind::resolve(coder.as_deref().or(cli.coder.as_deref()))?;
            cmd_resume(
                &cwd,
                run_id.as_deref(),
                &resolver,
                &extra_args,
                coder_kind,
                no_sandbox || cli.no_sandbox,
            )?;
        }
        Some(Commands::Init) => {
            cmd_init(&cwd)?;
        }
        Some(Commands::Dashboard { run_id, path }) => {
            let search_root = path.map(PathBuf::from).unwrap_or(cwd);
            dashboard::run_dashboard(&search_root, run_id.as_deref())?;
        }
        Some(Commands::Merge { run_id }) => {
            cmd_merge(&cwd, run_id.as_deref())?;
        }
        Some(Commands::Fargate { command }) => match command {
            FargateCommands::EnsureSetup { force_rebuild } => {
                let region = std::env::var("FACTORY_REGION")
                    .or_else(|_| std::env::var("AWS_DEFAULT_REGION"))
                    .unwrap_or_else(|_| "us-west-1".to_string());
                let factory_source_root = fargate::resolve_factory_source_root_from(&cwd)?;
                fargate_bootstrap::ensure_setup(&fargate_bootstrap::BootstrapConfig {
                    project_root: cwd.clone(),
                    factory_source_root,
                    region,
                    force_rebuild,
                })?;
                eprintln!("  Fargate setup complete.");
            }
            FargateCommands::Teardown { keep_ecr, keep_s3 } => {
                let outcome = fargate_bootstrap::teardown(keep_ecr, keep_s3)?;
                println!("{outcome}");
            }
        },
        Some(Commands::Version) => {
            println!("{}", version::version_string());
        }
        Some(Commands::Observations { command }) => {
            cmd_observations(&cwd, command)?;
        }
        None => {
            let coder_kind = CoderKind::resolve(cli.coder.as_deref())?;
            cmd_interactive(&sandbox_root, &resolver, &cli.extra_args, coder_kind)?;
        }
    }

    Ok(())
}

fn cmd_work(
    project_root: &Path,
    command: WorkCommands,
    global_coder: Option<&str>,
    global_no_sandbox: bool,
    resolver: &ContentResolver,
) -> Result<()> {
    let store = WorkModelStore::new(project_root);
    match command {
        WorkCommands::Create {
            id,
            title,
            instructions,
            instructions_file,
            planning_context,
            planning_context_file,
            brief_file,
            behaviors_file,
            approach_file,
            plan_file,
        } => {
            let instructions = match (instructions, instructions_file) {
                (Some(instructions), None) => Some(instructions),
                (None, Some(path)) => Some(fs::read_to_string(path)?),
                (None, None) => None,
                (Some(_), Some(_)) => unreachable!("clap rejects conflicting instruction inputs"),
            };
            let planning_context = read_planning_context(
                planning_context,
                planning_context_file,
                brief_file,
                behaviors_file,
                approach_file,
                plan_file,
            )?;
            let item = WorkItem {
                id,
                title,
                planning_context,
                instructions,
                abandonment: None,
                attempts: Vec::new(),
                merge_candidates: Vec::new(),
            };
            store.create_work_item(&item)?;
            println!("Created Work Item {}", item.id);
        }
        WorkCommands::List => {
            let items = store.list_work_items()?;
            if items.is_empty() {
                println!("No Work Items found");
            } else {
                println!("{:<24} TITLE", "ID");
                for item in items {
                    println!("{:<24} {}", item.id, item.title);
                }
            }
        }
        WorkCommands::Show { id } => match store.read_work_item(&id) {
            Ok(item) => {
                print!("{}", to_json_pretty(&item)?);
            }
            Err(WorkModelStorageError::ReadFile { source, .. })
                if source.kind() == ErrorKind::NotFound =>
            {
                bail!("Work Item {id:?} not found");
            }
            Err(error) => return Err(error.into()),
        },
        WorkCommands::Abandon { id, reason } => {
            let mut item = match store.read_work_item(&id) {
                Ok(item) => item,
                Err(WorkModelStorageError::ReadFile { source, .. })
                    if source.kind() == ErrorKind::NotFound =>
                {
                    bail!("Work Item {id:?} not found");
                }
                Err(error) => return Err(error.into()),
            };
            item.abandon(reason)?;
            store.write_work_item(&item)?;
            println!("Abandoned Work Item {}", item.id);
        }
        WorkCommands::Attempt {
            command,
            work_item_id,
            attempt_id,
        } => match command {
            Some(WorkAttemptCommands::Run {
                work_item_id,
                attempt_id,
                no_sandbox,
                coder,
                runtime,
                extra_args,
            }) => {
                let attempt_id = match attempt_id {
                    Some(id) => id,
                    None => {
                        let item = match store.read_work_item(&work_item_id) {
                            Ok(item) => item,
                            Err(WorkModelStorageError::ReadFile { source, .. })
                                if source.kind() == ErrorKind::NotFound =>
                            {
                                bail!("Work Item {work_item_id:?} not found");
                            }
                            Err(error) => return Err(error.into()),
                        };
                        item.latest_attempt_id()
                            .ok_or_else(|| anyhow::anyhow!(
                                "Work Item {work_item_id:?} has no Attempts; create one first with: factory work attempt {work_item_id}"
                            ))?
                            .to_string()
                    }
                };
                let coder_kind = CoderKind::resolve(coder.as_deref().or(global_coder))?;
                let runtime = runtime.unwrap_or_else(|| "local".to_string());
                match runtime.as_str() {
                    "fargate" => {
                        if coder_kind != CoderKind::Claude {
                            bail!("Fargate runtime currently supports only the claude coder");
                        }
                        fargate::launch_work_attempt(project_root, &work_item_id, &attempt_id)?;
                        println!(
                            "Launched Attempt {attempt_id} for Work Item {work_item_id} on Fargate"
                        );
                        return Ok(());
                    }
                    "local" => {}
                    other => bail!("Unknown runtime '{other}'. Available: local, fargate."),
                }
                let result = work_attempt_loop::run_attempt(WorkAttemptRunConfig {
                    project_root,
                    store: &store,
                    work_item_id: &work_item_id,
                    attempt_id: &attempt_id,
                    resolver,
                    extra_args: &extra_args,
                    coder_kind,
                    no_sandbox: no_sandbox || global_no_sandbox,
                })?;
                for outcome in result.outcomes {
                    match outcome {
                        WorkAttemptRunOutcome::RanTask { task_id, output } => {
                            println!("Completed Task {task_id} at {output}");
                        }
                        WorkAttemptRunOutcome::PlannedReviews { task_ids } => {
                            println!(
                                "Planned {} review Tasks for Attempt {attempt_id}",
                                task_ids.len()
                            );
                            for task_id in task_ids {
                                println!("{task_id}");
                            }
                        }
                        WorkAttemptRunOutcome::MergeCandidateReady { candidate_id } => {
                            println!(
                                "Attempt {attempt_id} reviews passed; Merge Candidate {candidate_id} is ready"
                            );
                        }
                        WorkAttemptRunOutcome::PlannedWriteRound { task_id } => {
                            println!("Planned write Task {task_id}");
                        }
                        WorkAttemptRunOutcome::NeedsUser { handoff_path } => {
                            println!("Attempt {attempt_id} needs user input: {handoff_path}");
                        }
                        WorkAttemptRunOutcome::ReviewOnlyComplete => {
                            println!("Review-only Attempt {attempt_id} passed");
                        }
                        WorkAttemptRunOutcome::ReviewOnlyFailed => {
                            println!("Review-only Attempt {attempt_id} failed");
                        }
                    }
                }
            }
            Some(WorkAttemptCommands::Pull {
                work_item_id,
                attempt_id,
            }) => {
                fargate::pull_work_attempt(project_root, &work_item_id, &attempt_id)?;
                println!(
                    "Pulled Attempt {attempt_id} workspace for Work Item {work_item_id} from Fargate"
                );
            }
            Some(WorkAttemptCommands::Stop {
                work_item_id,
                attempt_id,
            }) => {
                fargate::stop_work_attempt(project_root, &work_item_id, &attempt_id)?;
                println!(
                    "Stop requested for Attempt {attempt_id} of Work Item {work_item_id} (Fargate)"
                );
            }
            Some(WorkAttemptCommands::Watch {
                work_item_id,
                attempt_id,
                interval,
            }) => {
                fargate::watch_work_attempt(project_root, &work_item_id, &attempt_id, interval)?;
                println!(
                    "Fargate task for Attempt {attempt_id} of Work Item {work_item_id} reached STOPPED"
                );
            }
            None => {
                let work_item_id =
                    work_item_id.ok_or_else(|| anyhow::anyhow!("work item id is required"))?;
                let mut item = match store.read_work_item(&work_item_id) {
                    Ok(item) => item,
                    Err(WorkModelStorageError::ReadFile { source, .. })
                        if source.kind() == ErrorKind::NotFound =>
                    {
                        bail!("Work Item {work_item_id:?} not found");
                    }
                    Err(error) => return Err(error.into()),
                };
                let attempt_id = attempt_id.unwrap_or_else(|| item.next_attempt_id());
                item.add_initial_attempt(attempt_id.clone())?;
                store.write_work_item(&item)?;
                println!("Created Attempt {attempt_id} for Work Item {work_item_id}");
            }
        },
        WorkCommands::Review {
            work_item_id,
            attempt_id,
        } => {
            let mut item = match store.read_work_item(&work_item_id) {
                Ok(item) => item,
                Err(WorkModelStorageError::ReadFile { source, .. })
                    if source.kind() == ErrorKind::NotFound =>
                {
                    bail!("Work Item {work_item_id:?} not found");
                }
                Err(error) => return Err(error.into()),
            };
            let task_ids = item.add_review_tasks(&attempt_id, review::REVIEWERS)?;
            store.write_work_item(&item)?;
            println!(
                "Planned {} review Tasks for Attempt {attempt_id}",
                task_ids.len()
            );
            for task_id in task_ids {
                println!("{task_id}");
            }
        }
        WorkCommands::ReviewCodebase {
            work_item_id,
            attempt_id,
        } => {
            let mut item = match store.read_work_item(&work_item_id) {
                Ok(item) => item,
                Err(WorkModelStorageError::ReadFile { source, .. })
                    if source.kind() == ErrorKind::NotFound =>
                {
                    bail!("Work Item {work_item_id:?} not found");
                }
                Err(error) => return Err(error.into()),
            };
            let source_ref = current_ref(project_root)?;
            let source_commit = head_commit(project_root)?;
            let task_ids = item.add_review_only_attempt(
                attempt_id.clone(),
                review::REVIEWERS,
                source_ref,
                source_commit,
            )?;
            store.write_work_item(&item)?;
            println!(
                "Created review-only Attempt {attempt_id} with {} review Tasks",
                task_ids.len()
            );
            for task_id in task_ids {
                println!("{task_id}");
            }
        }
        WorkCommands::MergeCandidate {
            work_item_id,
            merge_candidate_id,
        } => match store.read_work_item(&work_item_id) {
            Ok(item) => {
                let Some(candidate) = item
                    .merge_candidates
                    .iter()
                    .find(|candidate| candidate.id == merge_candidate_id)
                else {
                    bail!(
                        "Merge Candidate {merge_candidate_id:?} not found in Work Item {work_item_id:?}"
                    );
                };
                print!("{}", to_json_pretty(candidate)?);
            }
            Err(WorkModelStorageError::ReadFile { source, .. })
                if source.kind() == ErrorKind::NotFound =>
            {
                bail!("Work Item {work_item_id:?} not found");
            }
            Err(error) => return Err(error.into()),
        },
        WorkCommands::Merge {
            work_item_id,
            merge_candidate_id,
            no_sandbox,
            coder,
            runtime,
            extra_args,
        } => {
            let merge_candidate_id = match merge_candidate_id {
                Some(id) => id,
                None => {
                    let item = match store.read_work_item(&work_item_id) {
                        Ok(item) => item,
                        Err(WorkModelStorageError::ReadFile { source, .. })
                            if source.kind() == ErrorKind::NotFound =>
                        {
                            bail!("Work Item {work_item_id:?} not found");
                        }
                        Err(error) => return Err(error.into()),
                    };
                    item.latest_merge_candidate_id()
                        .ok_or_else(|| {
                            anyhow::anyhow!("Work Item {work_item_id:?} has no Merge Candidates")
                        })?
                        .to_string()
                }
            };
            let coder_kind = CoderKind::resolve(coder.as_deref().or(global_coder))?;
            let runtime = runtime.unwrap_or_else(|| "local".to_string());
            match runtime.as_str() {
                "fargate" => {
                    if coder_kind != CoderKind::Claude {
                        bail!("Fargate runtime currently supports only the claude coder");
                    }
                    fargate::launch_work_merge(project_root, &work_item_id, &merge_candidate_id)?;
                    println!(
                        "Launched merge of {merge_candidate_id} for Work Item {work_item_id} on Fargate"
                    );
                    return Ok(());
                }
                "local" => {}
                other => bail!("Unknown runtime '{other}'. Available: local, fargate."),
            }
            let result = work_merge_executor::merge_candidate(WorkMergeConfig {
                project_root,
                store: &store,
                work_item_id: &work_item_id,
                merge_candidate_id: &merge_candidate_id,
                resolver,
                extra_args: &extra_args,
                coder_kind,
                no_sandbox: no_sandbox || global_no_sandbox,
            })?;
            println!(
                "Merged Merge Candidate {} at {}",
                result.merge_candidate_id, result.merged_commit
            );
        }
        WorkCommands::MergePull {
            work_item_id,
            merge_candidate_id,
        } => {
            fargate::pull_work_merge(project_root, &work_item_id, &merge_candidate_id)?;
            println!(
                "Pulled Merge Candidate {merge_candidate_id} workspace for Work Item {work_item_id} from Fargate"
            );
        }
        WorkCommands::MergeStop {
            work_item_id,
            merge_candidate_id,
        } => {
            fargate::stop_work_merge(project_root, &work_item_id, &merge_candidate_id)?;
            println!(
                "Stop requested for Merge Candidate {merge_candidate_id} of Work Item {work_item_id} (Fargate)"
            );
        }
        WorkCommands::MergeWatch {
            work_item_id,
            merge_candidate_id,
            interval,
        } => {
            fargate::watch_work_merge(project_root, &work_item_id, &merge_candidate_id, interval)?;
            println!(
                "Fargate task for Merge Candidate {merge_candidate_id} of Work Item {work_item_id} reached STOPPED"
            );
        }
        WorkCommands::Task { command } => match command {
            WorkTaskCommands::Run {
                work_item_id,
                attempt_id,
                task_id,
                no_sandbox,
                coder,
                extra_args,
            } => {
                let coder_kind = CoderKind::resolve(coder.as_deref().or(global_coder))?;
                let result = work_task_executor::run_task(WorkTaskRunConfig {
                    project_root,
                    store: &store,
                    work_item_id: &work_item_id,
                    attempt_id: &attempt_id,
                    task_id: &task_id,
                    resolver,
                    extra_args: &extra_args,
                    coder_kind,
                    no_sandbox: no_sandbox || global_no_sandbox,
                    store_lock: None,
                })?;
                println!("Completed Task {} at {}", result.task_id, result.output);
            }
        },
        WorkCommands::PostMergeReview { command } => match command {
            cli::WorkPostMergeReviewCommands::Run {
                debounce_seconds,
                target,
            } => {
                let secs = debounce_seconds.unwrap_or_else(post_merge_review::debounce_seconds);
                let outcome = post_merge_review::run(project_root, secs, target.as_deref())?;
                println!(
                    "Post-merge review: reviewed {} branch(es), {} errors",
                    outcome.reviewed.len(),
                    outcome.errors.len()
                );
                for per in &outcome.reviewed {
                    println!("  {} @ {}", per.target_branch, per.merged_commit);
                    if let Some(work_item) = &per.post_merge_review_fix_work_item {
                        println!("    post-merge-review-fix Work Item: {work_item}");
                    }
                }
                for error in &outcome.errors {
                    eprintln!("  error: {error}");
                }
            }
        },
    }
    Ok(())
}

fn read_planning_context(
    planning_context: Option<String>,
    planning_context_file: Option<String>,
    brief_file: Option<String>,
    behaviors_file: Option<String>,
    approach_file: Option<String>,
    plan_file: Option<String>,
) -> Result<Option<PlanningContext>> {
    let context = PlanningContext {
        brief: read_optional_file(brief_file)?,
        behaviors: read_optional_file(behaviors_file)?,
        approach: read_optional_file(approach_file)?,
        plan: read_optional_file(plan_file)?,
        combined: match (planning_context, planning_context_file) {
            (Some(context), None) => Some(context),
            (None, Some(path)) => Some(fs::read_to_string(path)?),
            (None, None) => None,
            (Some(_), Some(_)) => unreachable!("clap rejects conflicting planning context inputs"),
        },
    };

    Ok((!context.is_empty()).then_some(context))
}

fn read_optional_file(path: Option<String>) -> Result<Option<String>> {
    path.map(fs::read_to_string).transpose().map_err(Into::into)
}

fn head_commit(project_root: &Path) -> Result<String> {
    git::run_stdout(
        project_root,
        &["rev-parse", "HEAD"],
        "resolve source checkout HEAD",
    )
}

fn current_ref(project_root: &Path) -> Result<String> {
    let branch = git::run_stdout(
        project_root,
        &["rev-parse", "--abbrev-ref", "HEAD"],
        "resolve source checkout ref",
    )?;
    if branch == "HEAD" {
        head_commit(project_root)
    } else {
        Ok(branch)
    }
}

fn cmd_observations(project_root: &Path, command: ObservationsCommands) -> Result<()> {
    match command {
        ObservationsCommands::Add { content } => observations::add(project_root, content),
        ObservationsCommands::Resolve { id, resolution } => {
            observations::resolve(project_root, &id, resolution)
        }
        ObservationsCommands::List => observations::list(project_root),
        ObservationsCommands::Show { id } => observations::show(project_root, &id),
        ObservationsCommands::Migrate => observations::migrate(project_root),
    }
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
    author.run_interactive(&system_prompt, sandbox_root, extra_args, &[])?;
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
    if worktree::is_git_repo(search_root)
        && let Some(parsed_plan) = try_parse_parallel_plan(&run)
    {
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

fn cmd_run_in_place(
    workspace: &Path,
    run_id: Option<&str>,
    resolver: &ContentResolver,
    extra_args: &[String],
    coder_kind: CoderKind,
    preserve_run_metadata: bool,
) -> Result<()> {
    let run = run::resolve_run(workspace, run_id)?;

    if !preserve_run_metadata {
        fs::write(run.dir.join("runtime"), "local")?;
        fs::write(run.dir.join("handle"), std::process::id().to_string())?;
    }
    fs::write(run.dir.join("coder"), coder_kind.as_str())?;

    eprintln!("factory: in-place session loop (run: {})", run.id);

    let system_prompt = resolver
        .resolve_content("prompts/author.md")
        .unwrap_or_default();

    let config = SessionConfig {
        run,
        system_prompt,
        working_dir: workspace.to_path_buf(),
        extra_args: extra_args.to_vec(),
        resolver: ContentResolver::new(Some(workspace)),
    };

    let author = coder_kind.boxed(CoderSandbox::None);
    session::run_session_loop(&*author, &config, &DefaultHooks, coder_kind)?;
    Ok(())
}

fn cmd_review(
    search_root: &Path,
    run_id: Option<&str>,
    reviewers: Option<&str>,
    brief: Option<&str>,
    resolver: &ContentResolver,
    extra_args: &[String],
    coder_kind: CoderKind,
    no_sandbox: bool,
) -> Result<()> {
    let run_id = run::prepare_review_run(search_root, run_id, reviewers, brief)?;
    if no_sandbox {
        cmd_run_bare(search_root, Some(&run_id), resolver, extra_args, coder_kind)
    } else {
        cmd_run_local(search_root, Some(&run_id), resolver, extra_args, coder_kind)
    }
}

fn cmd_status(search_root: &Path, show_runs: bool) -> Result<()> {
    let runs_dir = search_root.join(".factory/runs");
    let work_status = work_status::load_work_status(search_root)?;

    if !show_runs && work_status.is_empty() {
        print!("{}", work_status::format_work_status(&work_status));
        return Ok(());
    }

    if !work_status.is_empty() {
        print!("{}", work_status::format_work_status(&work_status));
    }

    if show_runs && runs_dir.is_dir() {
        let runs = run::list_runs(search_root)?;

        if !work_status.is_empty() {
            println!();
        }
        println!("{:<20} {:<16} {:<10} BRIEF", "RUN", "STATUS", "RUNTIME");
        println!("{:<20} {:<16} {:<10} -----", "---", "------", "-------");

        for run in &runs {
            let status = run
                .effective_status()
                .map(|s| s.to_string())
                .unwrap_or_else(|_| "-".into());
            let runtime = run.runtime();
            let brief = run.brief_summary();

            println!("{:<20} {:<16} {:<10} {}", run.id, status, runtime, brief);
        }
    }

    if show_runs && !runs_dir.is_dir() && work_status.is_empty() {
        println!("No runs found in {}", search_root.display());
    }

    Ok(())
}

fn cmd_cleanup(search_root: &Path, run_id: Option<String>, apply: bool) -> Result<()> {
    let options = CleanupOptions { run_id, apply };
    let run_results = cleanup::cleanup_runs(search_root, &options)?;
    let work_results = cleanup::cleanup_work_items(search_root, &options)?;
    let reviewer_results = cleanup::cleanup_stranded_reviewer_worktrees(search_root, &options)?;

    if run_results.is_empty() && work_results.is_empty() && reviewer_results.is_empty() {
        println!("No cleanup candidates found.");
        return Ok(());
    }

    if apply {
        println!("Cleaned:");
    } else {
        println!("Dry run. Use --apply to clean:");
    }

    for result in run_results {
        let action = if result.applied {
            "cleaned"
        } else {
            "would clean"
        };
        println!("  {} {} ({})", action, result.run_id, result.status);
        match result.worktree {
            WorktreeCleanup::None => {}
            WorktreeCleanup::WouldRemove(path) => {
                println!("    would remove registered worktree {}", path.display());
            }
            WorktreeCleanup::Removed(path) => {
                println!("    removed registered worktree {}", path.display());
            }
            WorktreeCleanup::SkippedUnregistered(path) => {
                println!("    skipped unregistered worktree {}", path.display());
            }
            WorktreeCleanup::Missing(path) => {
                println!("    recorded worktree missing {}", path.display());
            }
        }
    }

    for result in work_results {
        match result {
            WorkCleanupResult::WorkItem(result) => {
                let action = if result.applied {
                    "cleaned Work Item"
                } else {
                    "would clean Work Item"
                };
                println!("  {} {}", action, result.work_item_id);
                if result.applied {
                    println!("    removed Work Item state {}", result.item_path.display());
                } else {
                    println!(
                        "    would remove Work Item state {}",
                        result.item_path.display()
                    );
                }
                for state_path in &result.state_paths {
                    if !state_path.exists() {
                        continue;
                    }
                    if result.applied {
                        println!("    removed Work state {}", state_path.display());
                    } else {
                        println!("    would remove Work state {}", state_path.display());
                    }
                }
                for worktree in result.worktrees {
                    match worktree {
                        WorktreeCleanup::None => {}
                        WorktreeCleanup::WouldRemove(path) => {
                            println!("    would remove registered worktree {}", path.display());
                        }
                        WorktreeCleanup::Removed(path) => {
                            println!("    removed registered worktree {}", path.display());
                        }
                        WorktreeCleanup::SkippedUnregistered(path) => {
                            println!("    skipped unregistered worktree {}", path.display());
                        }
                        WorktreeCleanup::Missing(path) => {
                            println!("    managed worktree missing {}", path.display());
                        }
                    }
                }
                for branch in result.branches {
                    match branch {
                        WorkBranchCleanup::WouldRemove(branch) => {
                            println!("    would remove Work branch {branch}");
                        }
                        WorkBranchCleanup::Removed(branch) => {
                            println!("    removed Work branch {branch}");
                        }
                        WorkBranchCleanup::Missing(_) => {}
                    }
                }
                for artifact in result.artifacts {
                    if result.applied {
                        println!("    removed Work artifact {}", artifact.display());
                    } else {
                        println!("    would remove Work artifact {}", artifact.display());
                    }
                }
            }
            WorkCleanupResult::OrphanArtifact(result) => {
                if result.applied {
                    println!(
                        "  removed orphan Work artifact root {}",
                        result.artifact_root.display()
                    );
                } else {
                    println!(
                        "  would remove orphan Work artifact root {}",
                        result.artifact_root.display()
                    );
                }
            }
        }
    }

    for result in reviewer_results {
        if result.applied {
            println!(
                "  removed stranded reviewer worktree {} (work-item: {}, reviewer: {})",
                result.path.display(),
                result.work_item_id,
                result.reviewer
            );
        } else {
            println!(
                "  would remove stranded reviewer worktree {} (work-item: {}, reviewer: {})",
                result.path.display(),
                result.work_item_id,
                result.reviewer
            );
        }
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
                .effective_status()
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
                    .effective_status()
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
    coder_kind: CoderKind,
    no_sandbox: bool,
) -> Result<()> {
    let run = run::resolve_resumable_run(search_root, run_id)?;

    eprintln!("  Resuming run {}", run.id);

    if !std::io::stdin().is_terminal() {
        return cmd_resume_headless(
            search_root,
            &run,
            resolver,
            extra_args,
            coder_kind,
            no_sandbox,
        );
    }

    if no_sandbox {
        os::check_coder_prerequisite(coder_kind)?;
    } else {
        os::check_prerequisites_for(coder_kind)?;
    }
    credential::inject_credentials()?;
    credential::setup_git_signing();

    let (sandbox, _sandbox_profile) = if no_sandbox {
        (CoderSandbox::None, None)
    } else {
        build_coder_sandbox(coder_kind, resolver, search_root, &[])?
    };
    let system_prompt = resolver
        .resolve_content("prompts/author.md")
        .unwrap_or_default();

    eprintln!("  Factory           resume session (run: {})", run.id);

    let author = coder_kind.boxed(sandbox);
    author.run_interactive(&system_prompt, search_root, extra_args, &[])?;
    Ok(())
}

fn cmd_resume_headless(
    search_root: &Path,
    run: &Run,
    resolver: &ContentResolver,
    extra_args: &[String],
    coder_kind: CoderKind,
    no_sandbox: bool,
) -> Result<()> {
    if run.dir.join("children").exists() && run.worktree_dir().is_none() {
        bail!(
            "Cannot headlessly resume parallel parent run {}. Resume a failed child run instead.",
            run.id
        );
    }

    if no_sandbox {
        os::check_coder_prerequisite(coder_kind)?;
    } else {
        os::check_prerequisites_for(coder_kind)?;
    }
    credential::inject_credentials()?;
    credential::setup_git_signing();

    let working_dir = run
        .worktree_dir()
        .unwrap_or_else(|| search_root.to_path_buf());
    let run_dir = run.live_artifact_dir();

    let mut extra_roots = Vec::new();
    if worktree::is_git_repo(&working_dir) {
        extra_roots.push(worktree::git_common_dir(&working_dir)?);
    }

    let worktree_resolver = ContentResolver::new(Some(&working_dir));
    let (sandbox, _sandbox_profile) = if no_sandbox {
        (CoderSandbox::None, None)
    } else {
        build_coder_sandbox(coder_kind, resolver, &working_dir, &extra_roots)?
    };
    let system_prompt = worktree_resolver
        .resolve_content("prompts/author.md")
        .unwrap_or_default();

    eprintln!("  Factory           session loop (run: {})", run.id);
    eprintln!("  Worktree          {}", working_dir.display());

    let config = SessionConfig {
        run: Run {
            id: run.id.clone(),
            dir: run_dir,
        },
        system_prompt,
        working_dir,
        extra_args: extra_args.to_vec(),
        resolver: worktree_resolver,
    };

    let author = coder_kind.boxed(sandbox);
    if coder_kind == CoderKind::Claude && !no_sandbox {
        session::run_session_loop(&*author, &config, &SandboxedHooks, coder_kind)?;
    } else {
        session::run_session_loop(&*author, &config, &DefaultHooks, coder_kind)?;
    }
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

fn cmd_merge(search_root: &Path, run_id: Option<&str>) -> Result<()> {
    let run = run::resolve_mergeable_run(search_root, run_id)?;

    // Verify reviews passed using the same live artifact rule as status.
    if run.effective_reviews_passed() == Some(false) {
        bail!("Cannot merge run {}: reviews did not pass", run.id);
    }

    eprintln!("  Merging run {}...", run.id);

    // Parallel parent runs have no worktree — their children were already
    // merged by the orchestrator. Just verify children are merged and set
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
            if status != run::RunStatus::Merged {
                bail!(
                    "Cannot merge parent run {}: child {} has status '{}', expected 'merged'",
                    run.id,
                    child_id,
                    status
                );
            }
        }
    } else {
        merge::merge_worktree_run(search_root, &run)?;
    }

    run.set_status(&run::RunStatus::Merged)?;

    eprintln!("  Run {} merged successfully.", run.id);
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
    if let Ok(output) = output
        && output.status.success()
    {
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
        if let Ok(output) = output
            && output.status.success()
        {
            let pids = String::from_utf8_lossy(&output.stdout);
            for pid in pids.lines() {
                let pid = pid.trim();
                if !pid.is_empty() {
                    Command::new("kill").args(["-9", pid]).output().ok();
                }
            }
            thread::sleep(Duration::from_millis(500));
        }
        eprintln!("  Existing Claude Code stopped.");
    }
    Ok(())
}

fn notify_status_change(body: &str) {
    factory::notify::notify("Factory", body);
}
