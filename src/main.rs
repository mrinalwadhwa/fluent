use anyhow::{Context, Result, bail};
use clap::Parser;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::Duration;

use fluent::cleanup::{
    self, CleanupOptions, WorkBranchCleanup, WorkCleanupResult, WorktreeCleanup,
};
use fluent::cli::{
    AttemptCommands, Cli, Commands, FargateCommands, KeepAwakeCommands, MergeCandidateCommands,
    ObservationCommands, QueueCommands, ReviewCommands, SchedulerCommands, SkillsCommands,
    TaskCommands, TesterCommands, WorkItemCommands,
};
use fluent::coder::CoderKind;
use fluent::content::ContentResolver;
use fluent::credential;
use fluent::dashboard;
use fluent::fargate;
use fluent::fargate_bootstrap;
use fluent::git;
use fluent::keep_awake;
use fluent::observations;
use fluent::os;
use fluent::post_merge_review;
use fluent::review;
use fluent::update;
use fluent::version;
use fluent::work_attempt_loop::{self, WorkAttemptRunConfig, WorkAttemptRunOutcome};
use fluent::work_merge_executor::{self, WorkMergeConfig};
use fluent::work_model::{
    self, PlanningContext, WorkItem, WorkModelStorageError, WorkModelStore, to_json_pretty,
};
use fluent::work_status;
use fluent::work_task_executor::{self, WorkTaskRunConfig};

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
            bail!("No log file yet — run fluent first");
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

    let is_update_command = matches!(cli.command, Some(Commands::Update));

    match cli.command {
        Some(Commands::Status { path }) => {
            let search_root = path.map(PathBuf::from).unwrap_or(cwd);
            cmd_status(&search_root)?;
        }
        Some(Commands::WorkItem { command }) => {
            cmd_work_item(&cwd, command)?;
        }
        Some(Commands::Attempt { command }) => {
            cmd_attempt(
                &cwd,
                command,
                cli.coder.as_deref(),
                cli.no_sandbox,
                &resolver,
            )?;
        }
        Some(Commands::MergeCandidate { command }) => {
            cmd_merge_candidate(
                &cwd,
                command,
                cli.coder.as_deref(),
                cli.no_sandbox,
                &resolver,
            )?;
        }
        Some(Commands::Task { command }) => {
            cmd_task(
                &cwd,
                command,
                cli.coder.as_deref(),
                cli.no_sandbox,
                &resolver,
            )?;
        }
        Some(Commands::Queue { command }) => {
            cmd_queue(&cwd, command)?;
        }
        Some(Commands::Tester { command }) => {
            cmd_tester(&cwd, command, cli.no_sandbox)?;
        }
        Some(Commands::Scheduler { command }) => {
            cmd_scheduler(&cwd, command)?;
        }
        Some(Commands::Review {
            command,
            work_item_id,
            attempt_id,
        }) => {
            cmd_review(&cwd, command, work_item_id, attempt_id)?;
        }
        Some(Commands::AutoMerge {
            work_item_id,
            all,
            no_sandbox,
            coder,
            poll_seconds,
        }) => {
            cmd_auto_merge(
                &cwd,
                work_item_id,
                all,
                no_sandbox || cli.no_sandbox,
                coder.as_deref().or(cli.coder.as_deref()),
                poll_seconds,
            )?;
        }
        Some(Commands::PostMergeReview {
            debounce_seconds,
            target,
        }) => {
            cmd_post_merge_review(&cwd, debounce_seconds, target)?;
        }
        Some(Commands::Cleanup {
            apply,
            prune_all_review_worktrees,
        }) => {
            cmd_cleanup(&cwd, apply, prune_all_review_worktrees)?;
        }
        Some(Commands::Init) => {
            cmd_init(&cwd)?;
        }
        Some(Commands::Dashboard { path }) => {
            let search_root = path.map(PathBuf::from).unwrap_or(cwd);
            dashboard::run_dashboard(&search_root)?;
        }
        Some(Commands::Fargate { command }) => match command {
            FargateCommands::EnsureSetup { force_rebuild } => {
                let region = std::env::var("FLUENT_REGION")
                    .or_else(|_| std::env::var("AWS_DEFAULT_REGION"))
                    .unwrap_or_else(|_| "us-west-1".to_string());
                let fluent_source_root = fargate::resolve_fluent_source_root_from(&cwd)?;
                fargate_bootstrap::ensure_setup(&fargate_bootstrap::BootstrapConfig {
                    project_root: cwd.clone(),
                    fluent_source_root,
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
        Some(Commands::Update) => {
            update::perform_update()?;
        }
        Some(Commands::Skills { command }) => {
            cmd_skills(command)?;
        }
        Some(Commands::Version) => {
            println!("{}", version::version_string());
        }
        Some(Commands::Observation { command }) => {
            cmd_observation(&cwd, command)?;
        }
        Some(Commands::KeepAwake { command }) => {
            cmd_keep_awake(command)?;
        }
        None => {
            let coder_kind = CoderKind::resolve(cli.coder.as_deref())?;
            cmd_interactive(&sandbox_root, &resolver, &cli.extra_args, coder_kind)?;
        }
    }

    if !is_update_command {
        update::maybe_nudge();
    }

    Ok(())
}

// -------------------------------------------------------------------------
// Work Item
// -------------------------------------------------------------------------

fn cmd_work_item(project_root: &Path, command: WorkItemCommands) -> Result<()> {
    let store = WorkModelStore::new(project_root);
    match command {
        WorkItemCommands::Create {
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
        WorkItemCommands::List => {
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
        WorkItemCommands::Show { id } => match store.read_work_item(&id) {
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
        WorkItemCommands::Abandon { id, reason } => {
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
    }
    Ok(())
}

// -------------------------------------------------------------------------
// Attempt
// -------------------------------------------------------------------------

fn cmd_attempt(
    project_root: &Path,
    command: AttemptCommands,
    global_coder: Option<&str>,
    global_no_sandbox: bool,
    resolver: &ContentResolver,
) -> Result<()> {
    let store = WorkModelStore::new(project_root);
    match command {
        AttemptCommands::Create {
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
            let attempt_id = attempt_id.unwrap_or_else(|| item.next_attempt_id());
            item.add_initial_attempt(attempt_id.clone())?;
            store.write_work_item(&item)?;
            println!("Created Attempt {attempt_id} for Work Item {work_item_id}");
        }
        AttemptCommands::List { work_item_id } => {
            let item = match store.read_work_item(&work_item_id) {
                Ok(item) => item,
                Err(WorkModelStorageError::ReadFile { source, .. })
                    if source.kind() == ErrorKind::NotFound =>
                {
                    bail!("Work Item {work_item_id:?} not found");
                }
                Err(error) => return Err(error.into()),
            };
            if item.attempts.is_empty() {
                println!("No Attempts found");
            } else {
                println!("{:<24} STATUS", "ID");
                for attempt in &item.attempts {
                    println!("{:<24} {}", attempt.id, attempt.status.as_str());
                }
            }
        }
        AttemptCommands::Show {
            work_item_id,
            attempt_id,
        } => {
            let item = match store.read_work_item(&work_item_id) {
                Ok(item) => item,
                Err(WorkModelStorageError::ReadFile { source, .. })
                    if source.kind() == ErrorKind::NotFound =>
                {
                    bail!("Work Item {work_item_id:?} not found");
                }
                Err(error) => return Err(error.into()),
            };
            let attempt = item
                .attempts
                .iter()
                .find(|a| a.id == attempt_id)
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "Attempt {attempt_id:?} not found in Work Item {work_item_id:?}"
                    )
                })?;
            print!("{}", to_json_pretty(attempt)?);
        }
        AttemptCommands::Run {
            work_item_id,
            attempt_id,
            no_sandbox,
            coder,
            write_coder,
            write_model,
            review_coder,
            review_model,
            behavior_tests_coder,
            behavior_tests_model,
            runtime,
            extra_args,
        } => {
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
                            "Work Item {work_item_id:?} has no Attempts; create one first with: fluent attempt create {work_item_id}"
                        ))?
                        .to_string()
                }
            };
            let coder_mapping = work_model::resolve_coder_mapping(
                &work_model::CoderMappingInputs::from_env().merge_cli(
                    write_coder,
                    write_model,
                    review_coder,
                    review_model,
                    behavior_tests_coder,
                    behavior_tests_model,
                    coder.or_else(|| global_coder.map(str::to_string)),
                ),
            )?;
            let runtime = runtime.unwrap_or_else(|| "local".to_string());
            match runtime.as_str() {
                "fargate" => {
                    let coder_kind = CoderKind::resolve(coder_mapping.write.coder.as_str().into())?;
                    fargate::launch_work_attempt(
                        project_root,
                        &work_item_id,
                        &attempt_id,
                        coder_kind,
                    )?;
                    println!(
                        "Launched Attempt {attempt_id} for Work Item {work_item_id} on Fargate"
                    );
                    return Ok(());
                }
                "local" => {}
                other => bail!("Unknown runtime '{other}'. Available: local, fargate."),
            }

            // Store the resolved mapping on the Attempt before running.
            {
                let mut item = store.read_work_item(&work_item_id)?;
                if let Some(attempt) = item.attempts.iter_mut().find(|a| a.id == attempt_id) {
                    attempt.coder_mapping = coder_mapping.clone();
                }
                store.write_work_item(&item)?;
            }

            let result = work_attempt_loop::run_attempt(WorkAttemptRunConfig {
                project_root,
                store: &store,
                work_item_id: &work_item_id,
                attempt_id: &attempt_id,
                resolver,
                extra_args: &extra_args,
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
        AttemptCommands::Pull {
            work_item_id,
            attempt_id,
        } => {
            fargate::pull_work_attempt(project_root, &work_item_id, &attempt_id)?;
            println!(
                "Pulled Attempt {attempt_id} workspace for Work Item {work_item_id} from Fargate"
            );
        }
        AttemptCommands::Stop {
            work_item_id,
            attempt_id,
        } => {
            fargate::stop_work_attempt(project_root, &work_item_id, &attempt_id)?;
            println!(
                "Stop requested for Attempt {attempt_id} of Work Item {work_item_id} (Fargate)"
            );
        }
        AttemptCommands::Watch {
            work_item_id,
            attempt_id,
            interval,
        } => {
            fargate::watch_work_attempt(project_root, &work_item_id, &attempt_id, interval)?;
            println!(
                "Fargate task for Attempt {attempt_id} of Work Item {work_item_id} reached STOPPED"
            );
        }
    }
    Ok(())
}

// -------------------------------------------------------------------------
// Merge Candidate
// -------------------------------------------------------------------------

fn cmd_merge_candidate(
    project_root: &Path,
    command: MergeCandidateCommands,
    global_coder: Option<&str>,
    global_no_sandbox: bool,
    resolver: &ContentResolver,
) -> Result<()> {
    let store = WorkModelStore::new(project_root);
    match command {
        MergeCandidateCommands::List { work_item_id } => {
            let item = match store.read_work_item(&work_item_id) {
                Ok(item) => item,
                Err(WorkModelStorageError::ReadFile { source, .. })
                    if source.kind() == ErrorKind::NotFound =>
                {
                    bail!("Work Item {work_item_id:?} not found");
                }
                Err(error) => return Err(error.into()),
            };
            if item.merge_candidates.is_empty() {
                println!("No Merge Candidates found");
            } else {
                println!("{:<24} {:<12} MERGE", "ID", "REVIEW");
                for candidate in &item.merge_candidates {
                    let review = format!("{:?}", candidate.review_state).to_lowercase();
                    let merge = format!("{:?}", candidate.merge_state.status).to_lowercase();
                    println!("{:<24} {:<12} {}", candidate.id, review, merge);
                }
            }
        }
        MergeCandidateCommands::Show {
            work_item_id,
            merge_candidate_id,
        } => match store.read_work_item(&work_item_id) {
            Ok(item) => {
                let candidate = item
                    .merge_candidates
                    .iter()
                    .find(|c| c.id == merge_candidate_id)
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "Merge Candidate {merge_candidate_id:?} not found in Work Item {work_item_id:?}"
                        )
                    })?;
                print!("{}", to_json_pretty(candidate)?);
            }
            Err(WorkModelStorageError::ReadFile { source, .. })
                if source.kind() == ErrorKind::NotFound =>
            {
                bail!("Work Item {work_item_id:?} not found");
            }
            Err(error) => return Err(error.into()),
        },
        MergeCandidateCommands::Land {
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
                    fargate::launch_work_merge(
                        project_root,
                        &work_item_id,
                        &merge_candidate_id,
                        coder_kind,
                    )?;
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
        MergeCandidateCommands::Pull {
            work_item_id,
            merge_candidate_id,
        } => {
            fargate::pull_work_merge(project_root, &work_item_id, &merge_candidate_id)?;
            println!(
                "Pulled Merge Candidate {merge_candidate_id} workspace for Work Item {work_item_id} from Fargate"
            );
        }
        MergeCandidateCommands::Stop {
            work_item_id,
            merge_candidate_id,
        } => {
            fargate::stop_work_merge(project_root, &work_item_id, &merge_candidate_id)?;
            println!(
                "Stop requested for Merge Candidate {merge_candidate_id} of Work Item {work_item_id} (Fargate)"
            );
        }
        MergeCandidateCommands::Watch {
            work_item_id,
            merge_candidate_id,
            interval,
        } => {
            fargate::watch_work_merge(project_root, &work_item_id, &merge_candidate_id, interval)?;
            println!(
                "Fargate task for Merge Candidate {merge_candidate_id} of Work Item {work_item_id} reached STOPPED"
            );
        }
    }
    Ok(())
}

// -------------------------------------------------------------------------
// Task
// -------------------------------------------------------------------------

fn cmd_task(
    project_root: &Path,
    command: TaskCommands,
    global_coder: Option<&str>,
    global_no_sandbox: bool,
    resolver: &ContentResolver,
) -> Result<()> {
    let store = WorkModelStore::new(project_root);
    match command {
        TaskCommands::List {
            work_item_id,
            attempt_id,
        } => {
            let item = match store.read_work_item(&work_item_id) {
                Ok(item) => item,
                Err(WorkModelStorageError::ReadFile { source, .. })
                    if source.kind() == ErrorKind::NotFound =>
                {
                    bail!("Work Item {work_item_id:?} not found");
                }
                Err(error) => return Err(error.into()),
            };
            let attempt = item
                .attempts
                .iter()
                .find(|a| a.id == attempt_id)
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "Attempt {attempt_id:?} not found in Work Item {work_item_id:?}"
                    )
                })?;
            if attempt.tasks.is_empty() {
                println!("No Tasks found");
            } else {
                println!("{:<24} {:<12} STATUS", "ID", "KIND");
                for task in &attempt.tasks {
                    println!("{:<24} {:<12} {}", task.id, task.kind, task.status);
                }
            }
        }
        TaskCommands::Show {
            work_item_id,
            attempt_id,
            task_id,
        } => {
            let item = match store.read_work_item(&work_item_id) {
                Ok(item) => item,
                Err(WorkModelStorageError::ReadFile { source, .. })
                    if source.kind() == ErrorKind::NotFound =>
                {
                    bail!("Work Item {work_item_id:?} not found");
                }
                Err(error) => return Err(error.into()),
            };
            let attempt = item
                .attempts
                .iter()
                .find(|a| a.id == attempt_id)
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "Attempt {attempt_id:?} not found in Work Item {work_item_id:?}"
                    )
                })?;
            let task = attempt
                .tasks
                .iter()
                .find(|t| t.id == task_id)
                .ok_or_else(|| {
                    anyhow::anyhow!("Task {task_id:?} not found in Attempt {attempt_id:?}")
                })?;
            print!("{}", to_json_pretty(task)?);
        }
        TaskCommands::Run {
            work_item_id,
            attempt_id,
            task_id,
            no_sandbox,
            coder,
            write_coder,
            write_model,
            review_coder,
            review_model,
            behavior_tests_coder,
            behavior_tests_model,
            extra_args,
        } => {
            let coder_mapping = work_model::resolve_coder_mapping(
                &work_model::CoderMappingInputs::from_env().merge_cli(
                    write_coder,
                    write_model,
                    review_coder,
                    review_model,
                    behavior_tests_coder,
                    behavior_tests_model,
                    coder.or_else(|| global_coder.map(str::to_string)),
                ),
            )?;

            // Store the resolved mapping on the Attempt before running.
            {
                let mut item = store.read_work_item(&work_item_id)?;
                if let Some(attempt) = item.attempts.iter_mut().find(|a| a.id == attempt_id) {
                    attempt.coder_mapping = coder_mapping;
                }
                store.write_work_item(&item)?;
            }

            let result = work_task_executor::run_task(WorkTaskRunConfig {
                project_root,
                store: &store,
                work_item_id: &work_item_id,
                attempt_id: &attempt_id,
                task_id: &task_id,
                resolver,
                extra_args: &extra_args,
                no_sandbox: no_sandbox || global_no_sandbox,
                store_lock: None,
            })?;
            println!("Completed Task {} at {}", result.task_id, result.output);
        }
    }
    Ok(())
}

// -------------------------------------------------------------------------
// Queue
// -------------------------------------------------------------------------

fn cmd_queue(project_root: &Path, command: QueueCommands) -> Result<()> {
    match command {
        QueueCommands::Add {
            work_item_id,
            priority,
        } => {
            fluent::queue::add(project_root, &work_item_id, priority)?;
            println!("Queued Work Item {work_item_id}");
        }
        QueueCommands::List => {
            let entries = fluent::queue::list(project_root)?;
            if entries.is_empty() {
                println!("Queue is empty");
            } else {
                for entry in entries {
                    println!(
                        "{} {} {} {}",
                        entry.priority, entry.queued_at, entry.status, entry.work_item_id
                    );
                }
            }
        }
        QueueCommands::Remove { work_item_id } => {
            fluent::queue::remove(project_root, &work_item_id)?;
            println!("Removed {work_item_id} from queue");
        }
    }
    Ok(())
}

// -------------------------------------------------------------------------
// Tester
// -------------------------------------------------------------------------

fn cmd_tester(project_root: &Path, command: TesterCommands, global_no_sandbox: bool) -> Result<()> {
    let store = WorkModelStore::new(project_root);
    let resolver = ContentResolver::new(Some(project_root));
    match command {
        TesterCommands::Run {
            work_item_id,
            attempt_id,
            task_id,
            no_sandbox,
        } => {
            let result = work_task_executor::run_task(WorkTaskRunConfig {
                project_root,
                store: &store,
                work_item_id: &work_item_id,
                attempt_id: &attempt_id,
                task_id: &task_id,
                resolver: &resolver,
                extra_args: &[],
                no_sandbox: no_sandbox || global_no_sandbox,
                store_lock: None,
            })?;
            println!(
                "Completed Tester Task {} at {}",
                result.task_id, result.output
            );
        }
    }
    Ok(())
}

// -------------------------------------------------------------------------
// Scheduler
// -------------------------------------------------------------------------

fn cmd_scheduler(project_root: &Path, command: SchedulerCommands) -> Result<()> {
    match command {
        SchedulerCommands::Run { poll_seconds } => {
            let poll = poll_seconds.unwrap_or(30);
            let invoker = fluent::scheduler::CliAttemptInvoker;
            fluent::scheduler::run(project_root, poll, &invoker)?;
        }
    }
    Ok(())
}

// -------------------------------------------------------------------------
// Review
// -------------------------------------------------------------------------

fn cmd_review(
    project_root: &Path,
    command: Option<ReviewCommands>,
    work_item_id: Option<String>,
    attempt_id: Option<String>,
) -> Result<()> {
    let store = WorkModelStore::new(project_root);
    match command {
        Some(ReviewCommands::Codebase {
            work_item_id,
            attempt_id,
            from_working_tree,
        }) => {
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
                from_working_tree,
            )?;
            store.write_work_item(&item)?;
            let variant = if from_working_tree {
                "source checkout"
            } else {
                "per-branch worktree"
            };
            println!(
                "Created review-only Attempt {attempt_id} against {variant} with {} task(s)",
                task_ids.len()
            );
            for task_id in task_ids {
                println!("{task_id}");
            }
        }
        None => {
            let work_item_id =
                work_item_id.ok_or_else(|| anyhow::anyhow!("work item id is required"))?;
            let attempt_id = attempt_id.ok_or_else(|| anyhow::anyhow!("attempt id is required"))?;
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
    }
    Ok(())
}

// -------------------------------------------------------------------------
// Auto-Merge
// -------------------------------------------------------------------------

fn cmd_auto_merge(
    project_root: &Path,
    work_item_id: Option<String>,
    all: bool,
    no_sandbox: bool,
    coder: Option<&str>,
    poll_seconds: Option<u64>,
) -> Result<()> {
    let mode = match (work_item_id, all) {
        (Some(id), false) => fluent::auto_merge::AutoMergeMode::Single(id),
        (None, true) => fluent::auto_merge::AutoMergeMode::All,
        (Some(_), true) => {
            bail!(
                "Cannot specify both a Work Item ID and --all; the two modes are mutually exclusive"
            );
        }
        (None, false) => {
            bail!("Specify either a Work Item ID or --all");
        }
    };
    let coder_kind = CoderKind::resolve(coder)?;
    let poll = poll_seconds.unwrap_or(30);
    fluent::auto_merge::run(project_root, mode, poll, coder_kind, no_sandbox)?;
    Ok(())
}

// -------------------------------------------------------------------------
// Post-Merge Review
// -------------------------------------------------------------------------

fn cmd_post_merge_review(
    project_root: &Path,
    debounce_seconds: Option<u64>,
    target: Option<String>,
) -> Result<()> {
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
    Ok(())
}

// -------------------------------------------------------------------------
// Observation
// -------------------------------------------------------------------------

fn cmd_observation(project_root: &Path, command: ObservationCommands) -> Result<()> {
    match command {
        ObservationCommands::Create { content } => observations::add(project_root, content),
        ObservationCommands::Resolve { id, resolution } => {
            observations::resolve(project_root, &id, resolution)
        }
        ObservationCommands::List => observations::list(project_root),
        ObservationCommands::Show { id } => observations::show(project_root, &id),
        ObservationCommands::Migrate => observations::migrate(project_root),
    }
}

// -------------------------------------------------------------------------
// Keep Awake
// -------------------------------------------------------------------------

fn cmd_keep_awake(command: KeepAwakeCommands) -> Result<()> {
    let sub = match command {
        KeepAwakeCommands::On => keep_awake::Subcommand::On,
        KeepAwakeCommands::Off => keep_awake::Subcommand::Off,
        KeepAwakeCommands::Status => keep_awake::Subcommand::Status,
        KeepAwakeCommands::Uninstall => keep_awake::Subcommand::Uninstall,
    };
    keep_awake::run(sub)
}

// -------------------------------------------------------------------------
// Cleanup (includes review-only worktree pruning)
// -------------------------------------------------------------------------

fn cmd_cleanup(search_root: &Path, apply: bool, prune_all_review_worktrees: bool) -> Result<()> {
    let options = CleanupOptions { apply };
    let work_results = cleanup::cleanup_work_items(search_root, &options)?;
    let reviewer_results = cleanup::cleanup_stranded_reviewer_worktrees(search_root, &options)?;

    // Review-only worktree pruning (folded from the old review-only-worktree prune command)
    let prune_store = WorkModelStore::new(search_root);
    let prune_options = fluent::review_only_worktree::PruneOptions {
        all: prune_all_review_worktrees,
        dry_run: !apply,
    };
    let prune_report =
        fluent::review_only_worktree::prune(&prune_store, search_root, prune_options)?;

    let has_work = !work_results.is_empty();
    let has_reviewers = !reviewer_results.is_empty();
    let has_prune = !prune_report.entries.is_empty();

    if !has_work && !has_reviewers && !has_prune {
        println!("No cleanup candidates found.");
        return Ok(());
    }

    if apply {
        println!("Cleaned:");
    } else {
        println!("Dry run. Use --apply to clean:");
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

    // Print review-only worktree prune results
    for entry in &prune_report.entries {
        match entry {
            fluent::review_only_worktree::PruneEntry::Removed { path } => {
                println!("  removed review-only worktree {}", path.display());
            }
            fluent::review_only_worktree::PruneEntry::SkippedInUse { path, in_flight } => {
                println!(
                    "  in-use review-only worktree {} (Work Item {:?} Attempt {:?})",
                    path.display(),
                    in_flight.work_item_id,
                    in_flight.attempt_id
                );
            }
            fluent::review_only_worktree::PruneEntry::SkippedNotOrphan { path } => {
                println!("  kept review-only worktree {}", path.display());
            }
            fluent::review_only_worktree::PruneEntry::WouldRemove { path } => {
                println!("  would remove review-only worktree {}", path.display());
            }
            fluent::review_only_worktree::PruneEntry::WouldSkipInUse { path, in_flight } => {
                println!(
                    "  would skip in-use review-only worktree {} (Work Item {:?} Attempt {:?})",
                    path.display(),
                    in_flight.work_item_id,
                    in_flight.attempt_id
                );
            }
        }
    }

    Ok(())
}

// -------------------------------------------------------------------------
// Other commands
// -------------------------------------------------------------------------

fn cmd_interactive(
    sandbox_root: &Path,
    resolver: &ContentResolver,
    extra_args: &[String],
    coder_kind: CoderKind,
) -> Result<()> {
    use fluent::coder::CoderSandbox;

    os::check_prerequisites_for(coder_kind)?;
    credential::inject_credentials()?;
    credential::setup_git_signing();

    let home = std::env::var("HOME").unwrap_or_default();
    let roots = vec![sandbox_root.to_path_buf()];
    let profile = os::render_profile_for_roots_for_coder(resolver, &home, &roots, coder_kind)?;
    let sandbox = CoderSandbox::SeatbeltProfile(profile.path.to_string_lossy().to_string());

    eprintln!("  Fluent           interactive session");
    eprintln!("  Sandbox root      {}", sandbox_root.display());

    let author = coder_kind.boxed(sandbox);
    author.run_interactive("", sandbox_root, extra_args, &[])?;
    Ok(())
}

fn cmd_status(search_root: &Path) -> Result<()> {
    let work_status = work_status::load_work_status(search_root)?;
    print!("{}", work_status::format_work_status(&work_status));
    Ok(())
}

fn cmd_init(cwd: &Path) -> Result<()> {
    let fluent_dir = cwd.join(".fluent");
    if fluent_dir.exists() {
        write_gitignore_if_absent(&fluent_dir)?;
        eprintln!(
            "  Already initialized: .fluent/ exists in {}",
            cwd.display()
        );
        return Ok(());
    }
    fs::create_dir_all(fluent_dir.join("expertise"))?;
    write_gitignore_if_absent(&fluent_dir)?;
    eprintln!("  Initialized .fluent/ in {}", cwd.display());
    Ok(())
}

// -------------------------------------------------------------------------
// Skills
// -------------------------------------------------------------------------

fn cmd_skills(command: Option<SkillsCommands>) -> Result<()> {
    match command {
        Some(SkillsCommands::Add {
            global: _,
            project: _,
            agent: _,
        })
        | None => {
            cmd_skills_add()?;
        }
        Some(SkillsCommands::Show { path, name }) => {
            cmd_skills_show(path, &name)?;
        }
    }
    Ok(())
}

fn cmd_skills_add() -> Result<()> {
    let home = std::env::var("HOME")
        .map_err(|_| anyhow::anyhow!("HOME not set; cannot locate agent skills directory"))?;

    let names = fluent::content::bundled_skill_names();

    // Write the full skill to the fixed data directory so the shim can
    // read it for hand-off regardless of which agent directories exist.
    let data_dir = skills_data_dir(&home);
    for name in &names {
        work_task_executor::materialize_skill(name, &data_dir)?;
    }

    // Install to the global skill roots.
    let global_dirs = global_skill_roots(&home);
    for dir in &global_dirs {
        for name in &names {
            work_task_executor::materialize_skill(name, dir)?;
        }
        eprintln!(
            "Installed {} skills to {}",
            names.len(),
            dir.display()
        );
    }

    // Scan for shim-marked fluent installations in candidate directories
    // beyond the global roots and replace them with the full skill.
    let scan_dirs = shim_scan_candidate_dirs(&home);
    for dir in &scan_dirs {
        if global_dirs.iter().any(|g| g == dir) {
            continue;
        }
        replace_shim_if_present(dir)?;
    }

    Ok(())
}

const SHIM_MARKER: &str = "fluent-shim: true";

fn is_fluent_shim(skills_dir: &Path) -> bool {
    let skill_md = skills_dir.join("fluent/SKILL.md");
    match fs::read_to_string(&skill_md) {
        Ok(content) => content.contains(SHIM_MARKER),
        Err(_) => false,
    }
}

fn replace_shim_if_present(skills_dir: &Path) -> Result<()> {
    if !is_fluent_shim(skills_dir) {
        return Ok(());
    }
    work_task_executor::materialize_skill("fluent", skills_dir)?;
    eprintln!(
        "Replaced fluent shim in {}",
        skills_dir.display()
    );
    Ok(())
}

/// Candidate directories where a fluent shim might have been installed by
/// the `skills` CLI. These are the known per-agent skill directories.
fn shim_scan_candidate_dirs(home: &str) -> Vec<PathBuf> {
    let home = PathBuf::from(home);
    let mut dirs = Vec::new();

    // Claude Code per-agent directories
    for agent in &["claude", "codex"] {
        let dir = home.join(format!(".{agent}/skills"));
        if dir.is_dir() {
            dirs.push(dir);
        }
    }

    // The main ~/.claude/skills is a global root, included for completeness
    // but will be skipped if already in global_dirs.
    let claude_skills = home.join(".claude/skills");
    if claude_skills.is_dir() && !dirs.contains(&claude_skills) {
        dirs.push(claude_skills);
    }

    dirs
}

fn cmd_skills_show(path_only: bool, name: &str) -> Result<()> {
    let home = std::env::var("HOME")
        .map_err(|_| anyhow::anyhow!("HOME not set; cannot locate data directory"))?;
    let data_dir = skills_data_dir(&home);
    let skill_dir = data_dir.join(name);

    if path_only {
        let skill_md = skill_dir.join("SKILL.md");
        println!("{}", skill_md.display());
    } else {
        let skill_md = skill_dir.join("SKILL.md");
        let content = fs::read_to_string(&skill_md).with_context(|| {
            format!("Cannot read skill {:?} at {}", name, skill_md.display())
        })?;
        print!("{content}");
    }
    Ok(())
}

/// Fixed data directory where `fluent skills add` writes the full skill for
/// hand-off reads by the shim.
fn skills_data_dir(home: &str) -> PathBuf {
    PathBuf::from(home)
        .join(".local")
        .join("share")
        .join("fluent")
        .join("skills")
}

/// Candidate global skill roots where agents may read skills from.
fn global_skill_roots(home: &str) -> Vec<PathBuf> {
    let home = PathBuf::from(home);
    let mut roots = Vec::new();

    // Claude Code global skills directory
    let claude_skills = home.join(".claude").join("skills");
    roots.push(claude_skills);

    roots
}

// -------------------------------------------------------------------------
// Helpers
// -------------------------------------------------------------------------

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

const FLUENT_GITIGNORE: &str = "\
# Fluent working state: everything here is ignored by default.
# Durable content is re-included below.
/*
!/.gitignore
!/expertise/
!/observations/
!/hooks/
!/Dockerfile
!/tester.yaml
!/extract-tester-results
";

fn write_gitignore_if_absent(fluent_dir: &Path) -> Result<()> {
    let gitignore = fluent_dir.join(".gitignore");
    if !gitignore.exists() {
        fs::write(&gitignore, FLUENT_GITIGNORE)?;
    }
    Ok(())
}

fn dirs_log_file() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    PathBuf::from(home).join(".local/state/fluent/fluent.log")
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
