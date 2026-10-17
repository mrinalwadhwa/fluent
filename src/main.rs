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
    AttemptCommands, AttemptEvidenceCommands, Cli, Commands, FargateCommands, KeepAwakeCommands,
    MergeCandidateCommands, ObservationCommands, QueueCommands, ReviewCommands, SchedulerCommands,
    SkillsCommands, SlackCommands, TaskCommands, TerminalCommands, TesterCommands,
    WorkItemCommands,
};
use fluent::coder::CoderKind;
use fluent::content::ContentResolver;
use fluent::credential;
use fluent::dashboard;
use fluent::fargate;
use fluent::fargate_bootstrap;
use fluent::git;
use fluent::guidance;
use fluent::keep_awake;
use fluent::observations;
use fluent::os;
use fluent::post_merge_review;
use fluent::review;
use fluent::skill_install::{self, InstallOutcome};
use fluent::slack_collaboration::{self, LaunchAgentServiceManager, RunnerControl};
use fluent::update;
use fluent::version;
use fluent::work_attempt_loop::{self, WorkAttemptRunConfig, WorkAttemptRunOutcome};
use fluent::work_merge_executor::{self, WorkMergeConfig};
use fluent::work_model::{
    self, PlanningContext, ReleaseContract, ReleaseCriterion, WorkItem, WorkModelError,
    WorkModelStorageError, WorkModelStore, to_json_pretty,
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
        Some(Commands::AgentSource { command }) => match command {
            fluent::cli::AgentSourceCommands::Terminal {
                project_root,
                identity,
                columns,
                rows,
            } => {
                fluent::dashboard_sources::helper_terminal(
                    &project_root,
                    &identity,
                    columns,
                    rows,
                )?;
                return Ok(());
            }
            fluent::cli::AgentSourceCommands::Snapshot { project_root } => {
                fluent::dashboard_sources::helper_snapshot(&project_root)?;
                return Ok(());
            }
        },
        Some(Commands::Terminal { command }) => match command {
            TerminalCommands::List { source } => {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&fluent::terminal_sessions::list(&source)?)?
                );
            }
            TerminalCommands::Attach {
                session_id,
                source,
                control,
            } => {
                let mut attachment = fluent::terminal_sessions::resolve(&source, &session_id)?;
                if control
                    && attachment.acquire_control()?
                        == fluent::terminal_sessions::ControlOutcome::Occupied
                {
                    eprintln!("Control is occupied; attaching in Observe mode");
                }
                let code = attachment.run()?;
                if code != 0 {
                    bail!("terminal attachment exited with {code}");
                }
            }
            TerminalCommands::Explain {
                source,
                work_item_id,
                attempt_id,
                task_id,
            } => {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&fluent::terminal_sessions::availability(
                        &source,
                        &work_item_id,
                        &attempt_id,
                        &task_id,
                    )?)?
                );
            }
        },
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
            post_merge_review,
            poll_seconds,
        }) => {
            cmd_auto_merge(
                &cwd,
                work_item_id,
                all,
                no_sandbox || cli.no_sandbox,
                coder.as_deref().or(cli.coder.as_deref()),
                post_merge_review,
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
        Some(Commands::Init {
            coder_profile,
            follow_up_mode,
            write_coder,
            write_model,
            write_effort,
            review_coder,
            review_model,
            review_effort,
        }) => {
            cmd_init(
                &cwd,
                fluent::setup::InitSetupInputs {
                    coder_profile,
                    follow_up_mode,
                    write_coder,
                    write_model,
                    write_effort,
                    review_coder,
                    review_model,
                    review_effort,
                },
            )?;
        }
        Some(Commands::Dashboard { path, projects }) => {
            let search_root = path.map(PathBuf::from).unwrap_or(cwd);
            dashboard::run_dashboard_with_projects(
                &search_root,
                &projects.into_iter().map(PathBuf::from).collect::<Vec<_>>(),
            )?;
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
            cmd_skills(&cwd, command)?;
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
        Some(Commands::Discord { command }) => cmd_discord(command)?,
        Some(Commands::Slack { command }) => cmd_slack(command)?,
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

fn cmd_discord(command: fluent::cli::DiscordCommands) -> Result<()> {
    use fluent::cli::DiscordCommands;
    use fluent::discord_collaboration as discord;
    use fluent::slack_collaboration::ServiceManager;
    let home =
        PathBuf::from(std::env::var("HOME").context("Discord runner requires the owner home")?);
    match command {
        DiscordCommands::Setup => print!("{}", include_str!("../docs/discord-runner.md")),
        DiscordCommands::CheckoutIdentity { path } => {
            println!("{}", slack_collaboration::checkout_identity(&path)?)
        }
        DiscordCommands::Check => {
            discord::configured_client(&home)?;
            println!("Discord setup is ready.");
        }
        DiscordCommands::Register => {
            let (config, client) = discord::configured_client(&home)?;
            client.register_command(&config.guild_id)?;
            println!("Registered /fluent in the approved server.");
        }
        DiscordCommands::ProviderLogin {
            project,
            thread,
            provider,
        } => discord::provider_login(&home, &project, &thread, &provider)?,
        DiscordCommands::Enable => {
            let (config, _) = discord::configured_client(&home)?;
            discord::enable_runner(
                &config,
                &mut discord::DiscordServiceManager::new(&home),
                &std::env::current_exe()?,
            )?;
            println!("Enabled Discord runner.");
        }
        DiscordCommands::Run => discord::run_configured_runner(&home)?,
        DiscordCommands::Restart => discord::DiscordServiceManager::new(&home).restart()?,
        DiscordCommands::Stop => discord::DiscordServiceManager::new(&home).stop()?,
        DiscordCommands::Status => println!(
            "{}",
            discord::runner_status(
                &discord::read_config(&home)?,
                &discord::read_operational_state(&home)?
            )
        ),
    }
    Ok(())
}

fn slack_home() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .context("Slack runner requires a home directory")
}

fn cmd_slack(command: SlackCommands) -> Result<()> {
    match command {
        SlackCommands::Manifest => print!("{}", slack_collaboration::SOCKET_MODE_MANIFEST),
        SlackCommands::Setup => print!("{}", include_str!("../docs/slack-runner.md")),
        SlackCommands::CheckoutIdentity { path } => {
            println!("{}", slack_collaboration::checkout_identity(&path)?);
        }
        SlackCommands::ProviderLogin {
            project,
            thread,
            provider,
        } => {
            slack_collaboration::authenticate_provider_for_thread(
                &slack_home()?,
                &project,
                &thread,
                &provider,
            )?;
            println!("queued provider-login recovery for Slack thread {thread}");
        }
        SlackCommands::ReconfirmCheckout { project, thread } => {
            slack_collaboration::request_checkout_reconfirmation(
                &slack_home()?,
                &project,
                &thread,
            )?;
            println!("queued checkout reconfirmation for Slack thread {thread}");
        }
        SlackCommands::Enable => {
            let home = slack_home()?;
            let executable = std::env::current_exe()?;
            let mut manager = LaunchAgentServiceManager::new(&home);
            slack_collaboration::enable_runner(&mut manager, &executable).map_err(|error| {
                slack_collaboration::classify_host_boundary(error, "fluent slack enable")
            })?;
            println!("enabled Slack runner at {}", manager.plist.display());
        }
        SlackCommands::Restart => {
            let mut manager = LaunchAgentServiceManager::new(&slack_home()?);
            slack_collaboration::control_runner(&mut manager, RunnerControl::Restart)?;
            println!("restarted Slack runner");
        }
        SlackCommands::Stop => {
            let mut manager = LaunchAgentServiceManager::new(&slack_home()?);
            slack_collaboration::control_runner(&mut manager, RunnerControl::Stop)?;
            println!("stopped Slack runner");
        }
        SlackCommands::Status => {
            let home = slack_home()?;
            let config = slack_collaboration::read_config(&home)?;
            let operational = slack_collaboration::read_runner_operational_state(&home)?;
            println!(
                "{}",
                slack_collaboration::runner_status(
                    &config,
                    operational.connected,
                    operational.last_successful_connection,
                    operational.failure,
                )
            );
        }
        SlackCommands::Run => {
            slack_collaboration::run_configured_runner(&slack_home()?).map_err(|error| {
                slack_collaboration::classify_host_boundary(error, "fluent slack run")
            })?
        }
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
            learner_mode,
            input_artifact,
            authorize_large_scope,
            release,
            release_criterion,
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
            let has_scope_inputs = planning_context.as_ref().is_some_and(|context| {
                context.behavior_count() > 0 || context.required_plan_steps() > 0
            });
            let planning_config = has_scope_inputs
                .then(|| fluent::config::resolve_planning_config(project_root))
                .transpose()?;
            let planning_scope = planning_context.as_ref().and_then(|context| {
                planning_config.as_ref().and_then(|config| {
                    context.scope_assessment(config.scope_limit.value, authorize_large_scope)
                })
            });
            let local_scope_evidence = planning_scope
                .as_ref()
                .map(|scope| {
                    let mut unreadable_items = 0_usize;
                    let items = store.list_work_item_results().map(|items| {
                        items
                            .into_iter()
                            .filter_map(|result| match result {
                                Ok(item) => Some(item),
                                Err(_) => {
                                    unreadable_items = unreadable_items.saturating_add(1);
                                    None
                                }
                            })
                            .collect::<Vec<_>>()
                    })?;
                    Ok::<_, fluent::work_model::WorkModelStorageError>((
                        fluent::scope_calibration::summarize_local_scope_evidence(
                            &items,
                            scope.scope_units(),
                        ),
                        unreadable_items,
                    ))
                })
                .transpose()?;
            if let (Some(scope), Some((evidence, unreadable_items)), Some(config)) =
                (&planning_scope, &local_scope_evidence, &planning_config)
            {
                eprintln!(
                    "Scope assessment: {} approved behaviors, {} required plan steps; configured reference {} ({})",
                    scope.behavior_count,
                    scope.required_steps,
                    scope.limit,
                    config.scope_limit.source
                );
                eprintln!(
                    "{}",
                    fluent::scope_calibration::format_local_scope_evidence(evidence)
                );
                if *unreadable_items > 0 {
                    let record = if *unreadable_items == 1 {
                        "record"
                    } else {
                        "records"
                    };
                    eprintln!(
                        "Project-local evidence omitted {} unreadable Work Item {}.",
                        unreadable_items, record
                    );
                }
            }
            if let Some(scope) = &planning_scope
                && scope.is_large()
                && !scope.large_scope_authorized
            {
                bail!(
                    "scope breadth exceeds the configured reference {}; minimum {} groups under the configured reference; this is not an optimal Work Item count. Consider independently landable outcomes, or rerun with --authorize-large-scope after approving the combined scope",
                    scope.limit,
                    scope.recommended_slices
                );
            }
            let release_contract = build_release_contract(release, release_criterion)?;
            let mut item = WorkItem {
                planning_context,
                planning_scope,
                release_contract,
                instructions,
                learner_mode: learner_mode.unwrap_or_default(),
                ..WorkItem::planned(id, title)
            };
            store.create_work_item_with_inputs(&mut item, &input_artifact)?;
            println!("Created Work Item {}", item.id);
            if let Some(scope) = item
                .planning_scope
                .as_ref()
                .filter(|scope| scope.is_large())
            {
                eprintln!(
                    "Authorized large scope: {} behaviors, {} required steps, reference {}, minimum {} policy groups (not an optimal Work Item count)",
                    scope.behavior_count,
                    scope.required_steps,
                    scope.limit,
                    scope.recommended_slices
                );
            }
            if guidance::guidance_enabled() {
                eprintln!("{}", guidance::after_work_item_create());
            }
        }
        WorkItemCommands::List => {
            let items = store.list_work_items()?;
            if items.is_empty() {
                println!("No Work Items found");
                if guidance::guidance_enabled() {
                    eprintln!("{}", guidance::empty_status_primer());
                }
            } else {
                println!("{:<24} TITLE", "ID");
                for item in items {
                    println!("{:<24} {}", item.id, item.title);
                    for warning in item.compatibility_warnings() {
                        eprintln!("Warning: {warning}");
                    }
                }
                if guidance::guidance_enabled() {
                    eprintln!("{}", guidance::after_work_item_list());
                }
            }
        }
        WorkItemCommands::Show { id } => match store.read_work_item_snapshot(&id) {
            Ok(item) => {
                let output = work_status::work_item_show_value(&item, project_root)?;
                print!("{}", to_json_pretty(&output)?);
                if guidance::guidance_enabled() {
                    let row = work_status::summarize_work_item(&item, Some(project_root));
                    if let Some(hint) = guidance::next_action_for_status_row(&row) {
                        eprintln!("{hint}");
                    }
                }
            }
            Err(WorkModelStorageError::ReadFile { source, .. })
                if source.kind() == ErrorKind::NotFound =>
            {
                bail!("Work Item {id:?} not found");
            }
            Err(error) => return Err(error.into()),
        },
        WorkItemCommands::Upgrade { id } => {
            let ids = match id {
                Some(id) => vec![id],
                None => store.list_work_item_ids()?,
            };
            for work_item_id in ids {
                let report = store.upgrade_work_item(&work_item_id)?;
                if report.already_current {
                    println!("Work Item {work_item_id} already uses the current model format");
                } else {
                    println!(
                        "Upgraded Work Item {work_item_id} to model record schema {} (audit {})",
                        fluent::work_model::WORK_MODEL_RECORD_SCHEMA_VERSION,
                        report.audit_path.as_deref().unwrap_or("<none>")
                    );
                }
            }
        }
        WorkItemCommands::RebuildMetrics { id } => {
            let items = match id {
                Some(id) => vec![store.read_work_item(&id)?],
                None => store.list_work_items()?,
            };
            for item in &items {
                work_status::refresh_persisted_metrics(project_root, item)?;
            }
            println!("Rebuilt persisted metrics for {} Work Item(s)", items.len());
        }
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
            item.abandon(reason, Some(project_root))?;
            store.write_work_item(&item)?;
            println!("Abandoned Work Item {}", item.id);
        }
        WorkItemCommands::Authorize { id } => {
            cmd_work_item_authorize(project_root, &store, &id)?;
        }
        WorkItemCommands::ClassifyFinding {
            id,
            finding_id,
            summary,
            blocker_for,
        } => {
            let classification = store.mutate_work_item(&id, |item| {
                let contract = item.release_contract.as_mut().ok_or_else(|| {
                    WorkModelError::ReleaseContractRequired {
                        work_item_id: item.id.clone(),
                    }
                })?;
                let finding = contract.classify_finding(finding_id, summary, blocker_for)?;
                Ok(finding.classification.as_str().to_string())
            })?;
            println!("Classified release finding for Work Item {id}: {classification}");
        }
    }
    Ok(())
}

/// Authorize a proposed Work Item to execute and reconcile its regular-queue
/// dispatch. The authorization, lineage charge, and durable enqueue intent are
/// persisted in one locked Work mutation before the queue is touched, so a crash
/// between the two is recoverable. Repeated authorization preserves the existing
/// authorization and lineage charge and repairs a missing dispatch without
/// reviving terminal or canceled history. This never authorizes landing.
fn cmd_work_item_authorize(project_root: &Path, store: &WorkModelStore, id: &str) -> Result<()> {
    fluent::follow_up::authorize_derived_work_item(project_root, store, id)?;
    println!("Authorized Work Item {id}");
    Ok(())
}

// -------------------------------------------------------------------------
// Attempt
// -------------------------------------------------------------------------

/// Resolve the coder to name in an auth-recovery hint: the coder that ran the
/// Attempt's paused task. Returns `None` when no paused task is present.
fn resolve_paused_coder(attempt: &work_model::Attempt) -> Option<String> {
    let task = attempt.tasks.iter().find(|task| {
        matches!(
            task.status,
            work_model::TaskStatus::Failed | work_model::TaskStatus::NeedsUser
        )
    })?;
    let pair = attempt.coder_mapping.for_task_kind(task.kind);
    Some(pair.coder.as_str().to_string())
}

/// Resolve an existing Attempt's effective mapping for display and launch
/// selection. Execution persists explicit inputs through a fresh mutation.
fn resolve_run_coder_mapping(
    store: &WorkModelStore,
    work_item_id: &str,
    attempt_id: &str,
    cli_inputs: &work_model::CoderMappingInputs,
) -> Result<work_model::CoderMapping> {
    let item = store.read_work_item(work_item_id)?;
    let attempt = item
        .attempts
        .iter()
        .find(|attempt| attempt.id == attempt_id)
        .ok_or_else(|| {
            anyhow::anyhow!("Attempt {attempt_id:?} not found in Work Item {work_item_id:?}")
        })?;
    attempt.coder_mapping.overlay_run_inputs(cli_inputs)
}

fn reject_retired_behavior_test_args(extra_args: &[String]) -> Result<()> {
    if extra_args.iter().any(|argument| {
        argument == "--behavior-tests-coder"
            || argument.starts_with("--behavior-tests-coder=")
            || argument == "--behavior-tests-model"
            || argument.starts_with("--behavior-tests-model=")
            || argument == "--behavior-tests-effort"
            || argument.starts_with("--behavior-tests-effort=")
    }) {
        bail!(
            "the behavior-tests coder role is retired; configure writer and reviewer roles instead"
        );
    }
    Ok(())
}

fn cmd_attempt(
    project_root: &Path,
    command: AttemptCommands,
    global_coder: Option<&str>,
    global_no_sandbox: bool,
    resolver: &ContentResolver,
) -> Result<()> {
    let store = WorkModelStore::new(project_root);
    match command {
        AttemptCommands::Evidence {
            command:
                AttemptEvidenceCommands::Classify {
                    work_item_id,
                    attempt_id,
                    candidate,
                    review_artifact,
                    evidence_digest,
                    resolution,
                },
        } => {
            let classification = fluent::host_evidence::classify_for_code_change(
                project_root,
                &store,
                &work_item_id,
                &attempt_id,
                &candidate,
                &review_artifact,
                &evidence_digest,
                &resolution,
            )?;
            println!(
                "Authorized host-evidence source correction Writer {} for Attempt {}",
                classification.writer_task_id, attempt_id
            );
            println!("Audit: {}", classification.audit_artifact);
            println!("Next: fluent queue resume {work_item_id} --attempt {attempt_id}");
        }
        AttemptCommands::Evidence {
            command:
                AttemptEvidenceCommands::Run {
                    work_item_id,
                    attempt_id,
                    review_artifact,
                    command,
                    working_directory,
                    timeout_seconds,
                },
        } => {
            let run = fluent::host_evidence::run_focused(
                project_root,
                &store,
                &work_item_id,
                &attempt_id,
                &review_artifact,
                command.as_deref(),
                working_directory.as_deref(),
                timeout_seconds,
                resolver,
            )?;
            println!(
                "Scheduled verification Task {} for Attempt {attempt_id}",
                run.task_id
            );
            println!(
                "Next: fluent task run {work_item_id} {attempt_id} {}",
                run.task_id
            );
            println!("Then: fluent attempt run {work_item_id} {attempt_id}");
        }
        AttemptCommands::Evidence {
            command:
                AttemptEvidenceCommands::Attach {
                    work_item_id,
                    attempt_id,
                    candidate,
                    evidence_file,
                    review_artifact,
                    recheck_artifact,
                    plan_row,
                    required_check,
                    finding,
                },
        } => {
            if let Some(finding) = finding {
                let recovery = fluent::host_evidence::attach_finding(
                    project_root,
                    &store,
                    &work_item_id,
                    &attempt_id,
                    &candidate,
                    &finding,
                    Path::new(&evidence_file),
                )?;
                println!(
                    "Attached host evidence {} for finding {}; targeted review: tests",
                    recovery.attachment.digest, finding
                );
                return Ok(());
            }
            if let Some(required_check) = required_check {
                let recovery = fluent::host_evidence::attach_required_check(
                    project_root,
                    &store,
                    &work_item_id,
                    &attempt_id,
                    &candidate,
                    &required_check,
                    Path::new(&evidence_file),
                )?;
                println!(
                    "Attached host evidence {} for required check {}; targeted review: tests",
                    recovery.attachment.digest, required_check
                );
                return Ok(());
            }
            if let Some(plan_row) = plan_row {
                anyhow::ensure!(
                    review_artifact.is_empty() && recheck_artifact.is_none(),
                    "--plan-row cannot be combined with review or recheck artifacts"
                );
                let recovery = fluent::host_evidence::attach_plan_row(
                    project_root,
                    &store,
                    &work_item_id,
                    &attempt_id,
                    &candidate,
                    &plan_row,
                    Path::new(&evidence_file),
                )?;
                println!(
                    "Attached Plan-row host evidence {} for Attempt {}; targeted reviews: {}",
                    recovery.attachment.digest,
                    attempt_id,
                    recovery
                        .targets
                        .iter()
                        .map(|target| target.role.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
                return Ok(());
            }
            let recovery = fluent::host_evidence::attach(
                project_root,
                &store,
                &work_item_id,
                &attempt_id,
                &candidate,
                Path::new(&evidence_file),
                &review_artifact,
                recheck_artifact.as_deref(),
            )?;
            println!(
                "Attached host evidence {} for Attempt {}; targeted reviews: {}",
                recovery.attachment.digest,
                attempt_id,
                recovery
                    .targets
                    .iter()
                    .map(|target| target.role.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        AttemptCommands::Create {
            work_item_id,
            attempt_id,
            terminal_mode,
            coder,
            model,
            effort,
            write_coder,
            write_model,
            write_effort,
            review_coder,
            review_model,
            review_effort,
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
            let coder_mapping = work_model::resolve_coder_mapping(
                &fluent::config::attempt_creation_inputs(project_root)?.merge_cli(
                    write_coder,
                    write_model,
                    review_coder,
                    review_model,
                    coder.or_else(|| global_coder.map(str::to_string)),
                    model,
                    write_effort,
                    review_effort,
                    effort,
                ),
            )?;
            let terminal_mode =
                fluent::config::attempt_terminal_mode(project_root, terminal_mode.as_deref())?;
            item.add_initial_attempt_with_settings(
                attempt_id.clone(),
                coder_mapping,
                terminal_mode,
            )?;

            store.write_work_item(&item)?;
            println!("Created Attempt {attempt_id} for Work Item {work_item_id}");
            if guidance::guidance_enabled() {
                eprintln!("{}", guidance::after_attempt_create());
            }
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
            let item = match store.read_work_item_snapshot(&work_item_id) {
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
        AttemptCommands::Extend {
            work_item_id,
            attempt_id,
            additional_write_rounds,
            resolution,
        } => {
            let extension = work_attempt_loop::extend_round_cap(
                project_root,
                &store,
                &work_item_id,
                &attempt_id,
                additional_write_rounds,
                &resolution,
            )?;
            println!(
                "Approved {} additional Writer round(s) for Attempt {} (cap {} -> {})",
                extension.additional_write_rounds, attempt_id, extension.old_cap, extension.new_cap
            );
            println!(
                "Reviewed candidate remains {}. Next: fluent queue resume {work_item_id} --attempt {attempt_id}",
                extension.candidate_commit
            );
        }
        AttemptCommands::RepairTester {
            work_item_id,
            attempt_id,
            resolution,
        } => {
            let repair = fluent::tester_harness_recovery::request(
                project_root,
                &store,
                &work_item_id,
                &attempt_id,
                &resolution,
            )?;
            if repair.already_requested {
                println!(
                    "Tester repair Writer {} is already recorded for candidate {}",
                    repair.task_id, repair.candidate_commit
                );
            } else {
                println!(
                    "Approved Tester repair Writer {} for candidate {}",
                    repair.task_id, repair.candidate_commit
                );
            }
            println!("Next: fluent queue resume {work_item_id} --attempt {attempt_id}");
        }
        AttemptCommands::RepairCandidateAncestry {
            work_item_id,
            attempt_id,
            resolution,
            restore_interrupted,
            rejected_candidate,
        } => {
            if restore_interrupted {
                let restored = fluent::candidate_ancestry_recovery::restore_interrupted(
                    project_root,
                    &store,
                    &work_item_id,
                    &attempt_id,
                    rejected_candidate
                        .as_deref()
                        .context("--rejected-candidate is required")?,
                    &resolution,
                )?;
                println!(
                    "Restored interrupted Writer {}: {} -> {} (operation {})",
                    restored.task_id,
                    restored.rejected_candidate,
                    restored.recorded_candidate,
                    restored.operation_id
                );
                println!("Audit: {}", restored.audit_artifact);
                println!(
                    "Cancellation remains active. Resume separately: fluent attempt run {work_item_id} {attempt_id}"
                );
                return Ok(());
            }
            let repair = fluent::candidate_ancestry_recovery::repair(
                project_root,
                &store,
                &work_item_id,
                &attempt_id,
                &resolution,
            )?;
            if repair.already_repaired {
                println!(
                    "Candidate ancestry repair {} is already recorded at {}",
                    repair.task_id, repair.recovered_candidate_commit
                );
            } else {
                println!(
                    "Repaired candidate ancestry {} -> {} with protected base {}",
                    repair.source_candidate_commit,
                    repair.recovered_candidate_commit,
                    repair.protected_base_commit
                );
            }
            println!(
                "Next: {}",
                fluent::candidate_ancestry_recovery::scheduler_action(&work_item_id, &attempt_id)
            );
        }
        AttemptCommands::RepairCandidateMetadata {
            work_item_id,
            attempt_id,
            resolution,
        } => {
            let repair = fluent::candidate_metadata_recovery::repair(
                project_root,
                &store,
                &work_item_id,
                &attempt_id,
                &resolution,
            )?;
            if repair.already_authorized {
                println!(
                    "Candidate metadata recovery Writer {} is already authorized for {}",
                    repair.task_id, repair.candidate_commit
                );
            } else {
                println!(
                    "Authorized candidate metadata recovery Writer {} for {} (protected base {})",
                    repair.task_id, repair.candidate_commit, repair.protected_base_commit
                );
            }
            println!("Audit: {}", repair.audit_artifact);
            println!("Next: fluent queue resume {work_item_id} --attempt {attempt_id}");
        }
        AttemptCommands::RecoverUsageLimit {
            work_item_id,
            attempt_id,
            resolution,
        } => {
            let approval = fluent::usage_limit_recovery::approve(
                project_root,
                &store,
                &work_item_id,
                &attempt_id,
                &resolution,
            )?;
            println!(
                "Recorded usage-limit approval {} for Writer {} at partial candidate {}",
                approval.operation_id, approval.task_id, approval.partial_commit
            );
            println!("Audit: {}", approval.audit_artifact);
            println!("Next: fluent attempt run {work_item_id} {attempt_id}");
        }
        AttemptCommands::RecoverAssignmentContext {
            work_item_id,
            attempt_id,
            task,
            candidate,
            assignment_digest,
            input,
            original_file,
            resolution,
        } => {
            let operation = fluent::assignment_context_recovery::recover(
                project_root,
                &store,
                &fluent::assignment_context_recovery::RecoveryRequest {
                    work: &work_item_id,
                    attempt: &attempt_id,
                    task: &task,
                    candidate: &candidate,
                    assignment_digest: &assignment_digest,
                    input: &input,
                    original_file: &original_file,
                    resolution: &resolution,
                },
            )?;
            println!("Recorded assignment context recovery {operation}; no Tasks were launched");
            let item = store.read_work_item_snapshot(&work_item_id)?;
            let attempt = item.attempts.iter().find(|a| a.id == attempt_id);
            if let Some(action) =
                fluent::work_status::attempt_recovery_next_action(attempt, Some(project_root))
            {
                println!("Next: {action}");
            }
        }
        AttemptCommands::RepairWriterCompletion {
            work_item_id,
            attempt_id,
            resolution,
        } => {
            let repair = fluent::writer_completion_recovery::repair(
                project_root,
                &store,
                &work_item_id,
                &attempt_id,
                &resolution,
            )?;
            if repair.already_applied {
                println!(
                    "Writer-completion recovery {} is already recorded for candidate {}",
                    repair.task_id, repair.candidate_commit
                );
            } else {
                println!(
                    "Authorized Writer-completion recovery {} for candidate {}",
                    repair.task_id, repair.candidate_commit
                );
            }
            if repair.superseded_task_ids.is_empty() {
                println!("No duplicate Writer tasks were superseded");
            } else {
                println!(
                    "Superseded duplicate Writer task(s): {}",
                    repair.superseded_task_ids.join(", ")
                );
            }
            if !repair.reconciled_task_ids.is_empty() {
                println!(
                    "Reconciled stale Writer task(s): {}",
                    repair.reconciled_task_ids.join(", ")
                );
            }
            println!("Audit: {}", repair.audit_artifact);
            if repair.reconciled_task_ids.is_empty() {
                println!("Next: fluent queue resume {work_item_id} --attempt {attempt_id}");
            } else {
                println!("Next: fluent attempt run {work_item_id} {attempt_id}");
            }
        }
        AttemptCommands::ReviseTestReferences {
            work_item_id,
            attempt_id,
            reference,
            resolution,
        } => {
            let updates = reference
                .iter()
                .map(|value| {
                    let (id, reference) = value.split_once('=').ok_or_else(|| {
                        anyhow::anyhow!("invalid --reference {value:?}; expected ID=REFERENCE")
                    })?;
                    Ok(fluent::planning_reference_recovery::TestReferenceUpdate {
                        requirement_id: id.to_string(),
                        reference: reference.to_string(),
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            let revision = fluent::planning_reference_recovery::revise(
                project_root,
                &store,
                &work_item_id,
                &attempt_id,
                &updates,
                &resolution,
            )?;
            if revision.already_applied {
                println!(
                    "Test-reference revision {} is already applied to Attempt {}",
                    revision.revision_id, attempt_id
                );
            } else {
                println!(
                    "Revised {} behavior Test reference(s) on Attempt {}",
                    revision.updated_requirements, attempt_id
                );
            }
            println!("Audit: {}", revision.audit_artifact);
            println!("Next: fluent queue resume {work_item_id} --attempt {attempt_id}");
        }
        AttemptCommands::Ownership { command } => match command {
            fluent::cli::OwnershipCommands::Preview {
                work_item_id,
                attempt_id,
                proposal_file,
            } => {
                let proposal = serde_json::from_slice(&fs::read(proposal_file)?)?;
                let preview = fluent::ownership_recovery::preview(
                    project_root,
                    &store,
                    &work_item_id,
                    &attempt_id,
                    &proposal,
                )?;
                println!("{}", serde_json::to_string_pretty(&preview)?);
            }
            fluent::cli::OwnershipCommands::Approve {
                work_item_id,
                attempt_id,
                preview_file,
                resolution,
            } => {
                let preview = serde_json::from_slice(&fs::read(preview_file)?)?;
                let approval = fluent::ownership_recovery::approve(
                    project_root,
                    &store,
                    &work_item_id,
                    &attempt_id,
                    &preview,
                    &resolution,
                )?;
                println!("{}", serde_json::to_string_pretty(&approval)?);
            }
        },
        AttemptCommands::ReviseApproach {
            work_item_id,
            attempt_id,
            approach_file,
            resolution,
        } => {
            let bytes = fs::read(&approach_file).with_context(|| {
                format!("read replacement Approach {}", approach_file.display())
            })?;
            let revision = fluent::planning_approach_recovery::revise(
                project_root,
                &store,
                &work_item_id,
                &attempt_id,
                &bytes,
                &resolution,
            )?;
            if revision.already_applied {
                println!(
                    "Approach revision {} is already applied to Attempt {}",
                    revision.revision_id, attempt_id
                );
            } else {
                println!(
                    "Approved corrective Writer {} for Approach revision {}",
                    revision.writer_task_id, revision.revision_id
                );
            }
            println!("Audit: {}", revision.audit_artifact);
            println!("Next: fluent queue resume {work_item_id} --attempt {attempt_id}");
        }
        AttemptCommands::Run {
            work_item_id,
            attempt_id,
            no_sandbox,
            tester_no_sandbox,
            dispatch_id,
            coder,
            model,
            effort,
            write_coder,
            write_model,
            write_effort,
            review_coder,
            review_model,
            review_effort,
            runtime,
            extra_args,
        } => {
            reject_retired_behavior_test_args(&extra_args)?;
            // Reject execution while the Work Item is proposed, before resolving
            // an Attempt to run, so proposed Work reports that human
            // authorization is required rather than that it has no Attempts.
            let initial_item = match store.read_work_item(&work_item_id) {
                Ok(item) => {
                    item.ensure_execution_ready()?;
                    item
                }
                Err(WorkModelStorageError::ReadFile { source, .. })
                    if source.kind() == ErrorKind::NotFound =>
                {
                    bail!("Work Item {work_item_id:?} not found");
                }
                Err(error) => return Err(error.into()),
            };
            let attempt_id = match attempt_id {
                Some(id) => id,
                None => {
                    initial_item.latest_attempt_id()
                        .ok_or_else(|| anyhow::anyhow!(
                            "Work Item {work_item_id:?} has no Attempts; create one first with: fluent attempt create {work_item_id}"
                        ))?
                        .to_string()
                }
            };
            // Capture direct recovery authority before initialization can race
            // with cancellation. Scheduler children use their earlier admission.
            let resume_cancellation_requested_at = match dispatch_id.as_deref() {
                Some(dispatch_id) => fluent::queue::cancellation_resume_for_dispatch(
                    project_root,
                    &work_item_id,
                    &attempt_id,
                    dispatch_id,
                )?,
                None => initial_item
                    .attempts
                    .iter()
                    .find(|attempt| attempt.id == attempt_id)
                    .filter(|attempt| attempt.cancellation_requested())
                    .and_then(|attempt| attempt.cancellation.as_ref())
                    .map(|stop| stop.requested_at.clone()),
            };
            let cli_inputs = work_model::CoderMappingInputs::default().merge_cli(
                write_coder,
                write_model,
                review_coder,
                review_model,
                coder.or_else(|| global_coder.map(str::to_string)),
                model,
                write_effort,
                review_effort,
                effort,
            );
            let coder_mapping =
                resolve_run_coder_mapping(&store, &work_item_id, &attempt_id, &cli_inputs)?;
            let runtime = runtime.unwrap_or_else(|| "local".to_string());
            match runtime.as_str() {
                "fargate" => {
                    let coder_mapping = if cli_inputs.has_run_overrides() {
                        let _land_lock = fluent::land_lock::acquire(
                            &fluent::land_lock::lock_path(project_root),
                        )?;
                        work_model::overlay_attempt_coder_mapping(
                            &store,
                            &work_item_id,
                            &attempt_id,
                            &cli_inputs,
                        )?
                    } else {
                        coder_mapping
                    };
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

            // Establish the enclosing host boundary before provider readiness,
            // worktree creation, Task reservation, or any other durable Attempt
            // transition. An explicit no-sandbox run keeps its existing route.
            if !(no_sandbox || global_no_sandbox) {
                os::preflight_host_execution(project_root, resolver)?;
            }

            if guidance::guidance_enabled() {
                eprintln!("{}", guidance::format_coder_plan(&coder_mapping));
            }

            if tester_no_sandbox {
                eprintln!("  Tester recovery   validating exact paused Tester capability");
            }
            let result = work_attempt_loop::run_attempt(WorkAttemptRunConfig {
                project_root,
                store: &store,
                work_item_id: &work_item_id,
                attempt_id: &attempt_id,
                resolver,
                extra_args: &extra_args,
                no_sandbox: no_sandbox || global_no_sandbox,
                tester_no_sandbox,
                resume_cancellation_requested_at: resume_cancellation_requested_at.as_deref(),
                coder_mapping_inputs: cli_inputs.has_run_overrides().then_some(&cli_inputs),
            })?;
            // A foreground Attempt does not claim a scheduler dispatch. If it
            // advances an Attempt that was previously paused by the scheduler,
            // reconcile that exact queue entry before printing status hints so
            // direct and queued recovery expose the same durable outcome.
            fluent::scheduler::reconcile_direct_attempt(project_root, &work_item_id, &attempt_id)?;
            // Resolve the runtime context the guidance hints need — why the
            // Attempt paused, which coder to re-authenticate, and where its latest
            // review verdicts live — from the persisted Attempt.
            let (pause_kind, auth_coder, review_artifact, evidence_action) = {
                let item = store.read_work_item(&work_item_id).ok();
                let attempt = item
                    .as_ref()
                    .and_then(|item| item.attempts.iter().find(|a| a.id == attempt_id));
                (
                    attempt.and_then(|a| a.pause_kind.clone()),
                    attempt.and_then(resolve_paused_coder),
                    attempt.and_then(|a| {
                        work_attempt_loop::latest_review_artifact_relpaths(project_root, a)
                            .into_iter()
                            .next()
                    }),
                    attempt.and_then(|attempt| {
                        fluent::work_status::attempt_recovery_next_action(
                            Some(attempt),
                            Some(project_root),
                        )
                    }),
                )
            };
            let hint_ctx = guidance::AttemptRunContext {
                pause_kind: pause_kind.as_ref(),
                coder: auth_coder.as_deref(),
                review_artifact: review_artifact.as_deref(),
                evidence_action: evidence_action.as_deref(),
            };
            let mut pending_recovery = None;
            for outcome in &result.outcomes {
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
                    WorkAttemptRunOutcome::PlannedFinalTester { task_id } => {
                        println!(
                            "Planned final canonical Tester Task {task_id} for Attempt {attempt_id}"
                        );
                    }
                    WorkAttemptRunOutcome::PlannedFailedCommandRecheck { task_id } => {
                        println!(
                            "Planned failed-command Tester recheck {task_id} for Attempt {attempt_id}"
                        );
                    }
                    WorkAttemptRunOutcome::MergeCandidateReady { candidate_id } => {
                        println!(
                            "Attempt {attempt_id} reviews and Learner passed; Merge Candidate \
                             {candidate_id} is ready"
                        );
                    }
                    WorkAttemptRunOutcome::FollowUpRecoveryPending {
                        candidate_id,
                        stage,
                        next_action,
                    } => {
                        println!(
                            "Merge Candidate {candidate_id} is merged; follow-up recovery is \
                             pending at {stage}"
                        );
                        pending_recovery = Some((candidate_id, next_action));
                    }
                    WorkAttemptRunOutcome::LearnerNotReady {
                        candidate_id,
                        reason,
                        relaunchable,
                    } => {
                        println!(
                            "Attempt {attempt_id} reviews passed, but Merge Candidate \
                             {candidate_id} is not ready: {reason}"
                        );
                        if !relaunchable {
                            println!(
                                "The Learner cannot be relaunched safely; its host evidence \
                                 requires human recovery"
                            );
                        }
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
                if guidance::guidance_enabled() {
                    if let Some(hint) =
                        guidance::after_attempt_run(outcome, &work_item_id, &hint_ctx)
                    {
                        eprintln!("{hint}");
                    }
                }
            }
            if let Some((candidate_id, next_action)) = pending_recovery {
                bail!(
                    "Merge Candidate {candidate_id} follow-up recovery remains pending. \
                     {next_action}"
                );
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
        AttemptCommands::Cancel {
            work_item_id,
            attempt_id,
        } => {
            let canceled = work_attempt_loop::cancel_local_attempt(
                project_root,
                &store,
                &work_item_id,
                &attempt_id,
            )?;
            if canceled == 0 {
                println!("Attempt {attempt_id} is already suspended after local cancellation");
            } else {
                println!(
                    "Canceled {canceled} local Task(s) for Attempt {attempt_id}; run `fluent attempt run {work_item_id}` to resume or abandon the Work Item"
                );
            }
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
                    let review = format!("{:?}", candidate.merge_review_state).to_lowercase();
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
                if guidance::guidance_enabled() {
                    eprintln!("{}", guidance::after_merge_candidate_show());
                }
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
            post_merge_review,
            no_post_merge_review,
            coder,
            model,
            effort,
            runtime,
            extra_args,
        } => {
            // Resolve one affirmative policy before any durable mutation. The
            // positive option enables the detached post-merge review; omission
            // and the legacy negative spelling both leave it disabled. Passing
            // both spellings is contradictory, so reject the command up front —
            // before touching the target ref, Merge Candidate, Learner handoff,
            // or either queue.
            if post_merge_review && no_post_merge_review {
                bail!("--post-merge-review and --no-post-merge-review conflict; pass at most one");
            }
            let run_post_merge_review = post_merge_review;
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
            let runtime = runtime.unwrap_or_else(|| "local".to_string());
            match runtime.as_str() {
                "fargate" => {
                    if model.is_some() || effort.is_some() {
                        bail!(
                            "--model and --effort are not supported with --runtime fargate for merge-candidate land"
                        );
                    }
                    let coder_kind = CoderKind::resolve(coder.as_deref().or(global_coder))?;
                    fargate::launch_work_merge(
                        project_root,
                        &work_item_id,
                        &merge_candidate_id,
                        coder_kind,
                        run_post_merge_review,
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
                // The executor resolves this mapping only after it has
                // validated and, when required, settled the Merge Candidate.
                coder_kind: CoderKind::Claude,
                coder_override: coder
                    .as_deref()
                    .map(|coder| CoderKind::resolve(Some(coder)))
                    .transpose()?,
                model: model.as_deref(),
                effort: effort.as_deref(),
                use_attempt_mapping: true,
                no_sandbox: no_sandbox || global_no_sandbox,
                run_post_merge_review,
            })?;
            println!(
                "Merged Merge Candidate {} at {}",
                result.merge_candidate_id, result.merged_commit
            );
            if guidance::guidance_enabled() {
                eprintln!("{}", guidance::after_merge_candidate_land());
            }
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
            let item = match store.read_work_item_snapshot(&work_item_id) {
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
        TaskCommands::CheckHandoff {
            work_item_id,
            attempt_id,
            task_id,
        } => {
            let report = work_attempt_loop::check_writer_handoff(
                project_root,
                &work_item_id,
                &attempt_id,
                &task_id,
            )?;
            print!("{}", to_json_pretty(&report)?);
        }
        TaskCommands::Run {
            work_item_id,
            attempt_id,
            task_id,
            no_sandbox,
            coder,
            model,
            effort,
            write_coder,
            write_model,
            write_effort,
            review_coder,
            review_model,
            review_effort,
            extra_args,
        } => {
            reject_retired_behavior_test_args(&extra_args)?;
            let cli_inputs = work_model::CoderMappingInputs::default().merge_cli(
                write_coder,
                write_model,
                review_coder,
                review_model,
                coder.or_else(|| global_coder.map(str::to_string)),
                model,
                write_effort,
                review_effort,
                effort,
            );
            work_model::overlay_attempt_coder_mapping(
                &store,
                &work_item_id,
                &attempt_id,
                &cli_inputs,
            )?;

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
        QueueCommands::Resume {
            work_item_id,
            attempt,
            priority,
            no_sandbox,
        } => {
            let outcome = fluent::queue::resume_with_tester_no_sandbox(
                project_root,
                &work_item_id,
                &attempt,
                priority,
                no_sandbox,
            )?;
            match outcome {
                fluent::queue::ResumeDispatchOutcome::Created => {
                    println!("Queued resume of Attempt {attempt} for Work Item {work_item_id}")
                }
                fluent::queue::ResumeDispatchOutcome::ExistingMatching => println!(
                    "Resume of Attempt {attempt} for Work Item {work_item_id} is already queued"
                ),
            }
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
        QueueCommands::Show { work_item_id } => {
            let dispatch = fluent::queue::show(project_root, &work_item_id)?;
            println!("{}", serde_json::to_string_pretty(&dispatch)?);
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
        TesterCommands::Check { no_sandbox } => {
            match fluent::tester::check(project_root, no_sandbox || global_no_sandbox, &resolver)? {
                fluent::tester::TesterOutcome::Passed => println!("Tester passed"),
                fluent::tester::TesterOutcome::TestFailures => {
                    bail!("Tester completed with test-suite failures")
                }
                fluent::tester::TesterOutcome::HarnessError => {
                    bail!("Tester harness could not produce trustworthy evidence")
                }
            }
        }
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
            fluent::scheduler_service::prepare_run_signal_handler();
            let home = fluent::scheduler_service::runtime_home(project_root)?;
            let identity = fluent::scheduler_service::assign_checkout_identity(project_root)?;
            fluent::scheduler_service::register_checkout(&home, project_root, &identity)?;
            let build = fluent::scheduler_service::executable_build_identity(
                &std::env::current_exe().context("resolve scheduler executable")?,
            )?;
            fluent::scheduler_service::run_service(&home, build, poll)?;
        }
        SchedulerCommands::Status => match fluent::scheduler::status(project_root)? {
            fluent::scheduler::SchedulerStatus::Stopped => {
                println!("Scheduler: stopped");
            }
            fluent::scheduler::SchedulerStatus::Running(metadata) => {
                println!(
                    "Scheduler: {}",
                    match metadata.state {
                        fluent::scheduler::CoordinatorState::Running => "running",
                        fluent::scheduler::CoordinatorState::Draining => "draining",
                    }
                );
                println!("Instance:  {}", metadata.instance_id);
                println!(
                    "Build:     {} ({})",
                    metadata.build.version, metadata.build.hash
                );
                println!("Started:   {}", metadata.started_at);
                println!("PID:       {} (display only)", metadata.pid);
            }
        },
        SchedulerCommands::Stop { all } => {
            if all {
                let home = fluent::scheduler_service::runtime_home(project_root)?;
                let stopped = fluent::scheduler_service::stop_service(&home)?;
                println!(
                    "Stopped shared scheduler {} running {} ({})",
                    stopped.instance_id, stopped.build.version, stopped.build.hash
                );
            } else {
                let stopped = fluent::scheduler::stop(project_root)?;
                println!(
                    "Stopped scheduler {} running {} ({})",
                    stopped.instance_id, stopped.build.version, stopped.build.hash
                );
            }
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
            let resolver = ContentResolver::new(Some(project_root));
            fluent::tester::check_structural(project_root, false, &resolver)
                .context("Tester is not ready; review state was not created")?;
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
            work_task_executor::prepare_ordinary_review_evidence(
                &project_root,
                &mut item,
                &attempt_id,
                &task_ids,
            )?;
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
    post_merge_review: bool,
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
    fluent::auto_merge::run(
        project_root,
        mode,
        poll,
        coder_kind,
        no_sandbox,
        post_merge_review,
    )?;
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
    // Bound the daemon to one live process per target branch: acquire the
    // singleton lease before draining and exit immediately if a live daemon
    // already holds it. The lease is held for the whole run (including the
    // debounce sleep) and releases on drop when this process exits.
    let _lease = match target.as_deref() {
        Some(branch) => match post_merge_review::acquire_daemon_lease(project_root, branch) {
            Some(lease) => Some(lease),
            None => {
                println!(
                    "Post-merge review: a live daemon already holds the lease for {branch}; exiting"
                );
                return Ok(());
            }
        },
        None => None,
    };
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
        ObservationCommands::Create { content } => {
            observations::add(project_root, content)?;
            if guidance::guidance_enabled() {
                eprintln!("{}", guidance::after_observation_create());
            }
            Ok(())
        }
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
        if guidance::guidance_enabled() {
            eprintln!("{}", guidance::after_cleanup());
        }
        return Ok(());
    }

    if apply {
        println!("Cleanup results:");
    } else {
        println!("Dry run. Use --apply to clean:");
    }

    for result in work_results {
        match result {
            WorkCleanupResult::RetainedSource {
                work_item_id,
                asset,
                reason,
            } => {
                println!("  retained Work Item {work_item_id}: {asset}: {reason}");
            }
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

    if guidance::guidance_enabled() {
        eprintln!("{}", guidance::after_cleanup());
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
    let is_empty = work_status.is_empty();
    print!("{}", work_status::format_work_status(&work_status));

    // Surface stalled execution: queued Work with no live coordinator to claim
    // it stays queued until the scheduler is started or recovered.
    let execution_is_stopped = fluent::scheduler::execution_is_stopped(search_root)?;
    if execution_is_stopped {
        println!(
            "\nExecution is stopped: queued Work is waiting but no scheduler is running.\n\
             Start or recover it with: fluent scheduler run"
        );
    }

    if guidance::guidance_enabled() {
        if is_empty {
            eprintln!("{}", guidance::empty_status_primer());
        } else if execution_is_stopped {
            eprintln!("\n→ Next: fluent scheduler run");
        } else if let Some(hint) = guidance::status_next_action(&work_status) {
            eprintln!("{hint}");
        }
    }
    Ok(())
}

fn cmd_init(cwd: &Path, inputs: fluent::setup::InitSetupInputs) -> Result<()> {
    let configured_setup = fluent::setup::ConfiguredSetup::from_inputs(inputs)?;
    if let Some(setup) = configured_setup.as_ref() {
        require_git_repository_root(cwd)?;
        fluent::setup::preflight_durable_paths(cwd)?;
        fluent::config::preflight_project_coder_config(cwd)?;
        let resolver = ContentResolver::new(Some(cwd));
        os::preflight_host_execution(cwd, &resolver)?;
        fluent::setup::preflight_providers(&setup.mapping)?;
    }
    let fluent_dir = cwd.join(".fluent");
    if fluent_dir.exists() {
        write_gitignore_if_absent(&fluent_dir)?;
        eprintln!(
            "  Already initialized: .fluent/ exists in {}",
            cwd.display()
        );
    } else {
        fs::create_dir_all(fluent_dir.join("expertise"))?;
        write_gitignore_if_absent(&fluent_dir)?;
        eprintln!("  Initialized .fluent/ in {}", cwd.display());
    }

    if let Err(e) = seed_agent_instructions(cwd) {
        eprintln!("  warning: could not seed agent instructions: {e}");
    }
    if let Some(setup) = configured_setup {
        let saved =
            fluent::config::apply_project_coder_profile(cwd, &setup.mapping, setup.follow_up_mode)
                .context("first-time setup is incomplete")?;
        eprintln!("  Saved coder profile:");
        for (role, pair) in [("writer", &saved.write), ("reviewer", &saved.review)] {
            let model = if pair.model.is_empty() {
                "provider default"
            } else {
                pair.model.as_str()
            };
            eprintln!(
                "    {role}: {} {} {}",
                pair.coder.as_str(),
                model,
                pair.effort.as_deref().unwrap_or("default")
            );
        }
        eprintln!(
            "  Each new Attempt stores this effective mapping unless you supply Attempt overrides."
        );
    }
    match instruction_files_requiring_git_resolution(cwd) {
        Ok(paths) if !paths.is_empty() => {
            eprintln!("{}", guidance::after_init_instruction_changes(&paths));
        }
        Ok(_) => {}
        Err(e) => eprintln!("  warning: could not inspect agent instruction changes: {e}"),
    }

    eprintln!();
    eprintln!("  fluent keeps its learned project notes and test config in .fluent/ and");
    eprintln!("  commits them alongside your changes, so they persist across runs.");

    if cwd.file_name().and_then(|n| n.to_str()) != Some("main") {
        eprintln!();
        eprintln!("  Tip: place your repo at <project>/main/ so attempt worktrees");
        eprintln!("  land as siblings under <project>/ instead of scattering into the");
        eprintln!("  parent directory.");
    }

    Ok(())
}

/// Require configured setup to run from the root of a Git worktree.
///
/// Keep legacy bare `fluent init` available outside repositories for existing
/// automation, while preventing configured setup from probing providers or
/// creating project state in an arbitrary nested directory.
fn require_git_repository_root(cwd: &Path) -> Result<()> {
    let root = git::run_stdout(
        cwd,
        &["rev-parse", "--show-toplevel"],
        "resolve the repository root for configured init",
    )
    .context("configured init requires a Git repository root")?;
    let canonical_cwd = fs::canonicalize(cwd)
        .with_context(|| format!("canonicalize configured init directory {}", cwd.display()))?;
    let canonical_root =
        fs::canonicalize(&root).with_context(|| format!("canonicalize repository root {root}"))?;
    if canonical_cwd != canonical_root {
        bail!(
            "configured init requires the repository root; run it from {}",
            canonical_root.display()
        );
    }
    Ok(())
}

// -------------------------------------------------------------------------
// Agent-instructions craft section
// -------------------------------------------------------------------------

const CRAFT_BEGIN: &str = "<!-- BEGIN fluent -->";
const CRAFT_END: &str = "<!-- END fluent -->";

const CRAFT_SECTION: &str = "\
<!-- BEGIN fluent -->
## Driving fluent

This project uses **fluent** to build changes through a structured, reviewed lifecycle.
The **fluent skill** is the deep reference — invoke it for the full stage procedures.
This section is the always-loaded summary and is enough to drive fluent on its own.

- **Lifecycle:** capture a brief → define behaviors → design an approach → plan →
  delegated execute → review → Learner → ready Merge Candidate. A human inspects and
  lands it. The first four stages are a user conversation — work through them one
  question at a time; don't skip them.
- **Work model:** a Work Item holds the plan; an Attempt runs writer → tester → reviewers
  in rounds, then runs the Learner after the reviewers pass. Only a successful Learner
  run makes the Merge Candidate ready. A relaunchable failure remains non-ready and
  `fluent attempt run` retries only the Learner; a non-relaunchable evidence failure
  stays blocked for human recovery and must not rerun or land.
- **Follow the next-action line:** most fluent commands print a `→ Next:` next-action line
  to stderr naming the command to run next; a state with no actionable step prints none.
  Run `fluent status` at session start and after any gap to see what needs attention, then
  follow the `→ Next:` line when it prints.
- **Observations:** capture durable lessons and future work with `fluent observation
  create`; list them with `fluent observation list`.
- **Pause for the user:** when a decision genuinely needs a human, fluent sets
  `needs-user` and pauses. Read the named handoff file, resolve it, then use the exact
  `fluent queue resume <work-item-id> --attempt <attempt-id>` action from `fluent status`
  to return scheduler-owned work to the queue. `fluent attempt run` remains the direct,
  foreground recovery path.
- **Committed scaffolding:** fluent commits its `.fluent/` notes and test config alongside
  your code changes, so its learned project state persists across runs.
- **Local Preview:** Attempts run locally in the foreground. Corrective follow-ups become
  proposed Work by default. `fluent work-item authorize` authorizes and queues Work;
  execution starts only while a human runs `fluent scheduler run`. The scheduler stops at a
  ready Merge Candidate after successful Learning, and every ready candidate is inspected
  and landed by a human.
  Post-merge review is off by default; opt in per land with
  `fluent merge-candidate land --post-merge-review`. `fluent auto-merge`, automatic
  scheduler lifecycle, automatic landing, and Fargate are outside this path. For an
  uninitialized project, use the full fluent skill to choose a follow-up mode and explicit
  coder profile before running configured `fluent init`. Bare `fluent init` remains available
  for compatible automation; the full skill passes the profile and follow-up flags, preflights
  providers, and saves the writer/reviewer mapping atomically.

### Ask the user easy-to-answer questions

One question at a time; leave a blank line after the question stem. Use two shapes:

- **Decision** — pick one option. Label the options (a)/(b)/(c), each self-contained; put
  the recommended option first and mark it `(recommended: <why>)`. The answer is a single
  letter.
- **Confirm gate** — approve or route back: \"Reply **yes (y)**, or name what to revise:
  (a).../(b).../(c)...\". The default is yes; a bare `y` is accepted.

Avoid the anti-pattern: an unlabeled \"X or Y?\" that forces the user to re-describe an option.

See the fluent skill for the full brief/behaviors/approach/plan procedure.
<!-- END fluent -->
";

fn seed_agent_instructions(cwd: &Path) -> Result<()> {
    let candidates = ["AGENTS.md", "CLAUDE.md"];
    let mut targets: Vec<PathBuf> = Vec::new();

    for name in &candidates {
        let path = cwd.join(name);
        if path.exists() {
            let canonical = path.canonicalize().unwrap_or_else(|_| path.clone());
            if !targets
                .iter()
                .any(|t| t.canonicalize().unwrap_or_else(|_| t.clone()) == canonical)
            {
                targets.push(path);
            }
        }
    }

    if targets.is_empty() {
        targets.push(cwd.join("AGENTS.md"));
    }

    for target in &targets {
        let content = if target.exists() {
            fs::read_to_string(target)?
        } else {
            String::new()
        };
        let updated = upsert_craft_block(&content);
        if updated != content {
            fs::write(target, updated)?;
        }
    }

    Ok(())
}

fn instruction_files_requiring_git_resolution(cwd: &Path) -> Result<Vec<String>> {
    let inside_worktree = git::run_raw(cwd, &["rev-parse", "--is-inside-work-tree"])?;
    if !inside_worktree.status.success() {
        return Ok(Vec::new());
    }

    let mut changed = Vec::new();
    for name in ["AGENTS.md", "CLAUDE.md"] {
        let output = git::run_raw(
            cwd,
            &["status", "--porcelain", "--untracked-files=all", "--", name],
        )?;
        if !output.status.success() {
            anyhow::bail!(
                "git status failed while checking {}: {}",
                name,
                String::from_utf8_lossy(&output.stderr)
            );
        }
        if !output.stdout.is_empty() {
            changed.push(name.to_string());
        }
    }
    Ok(changed)
}

fn upsert_craft_block(content: &str) -> String {
    if let (Some(begin), Some(end)) = (content.find(CRAFT_BEGIN), content.find(CRAFT_END)) {
        let end_of_marker = end + CRAFT_END.len();
        let after = if content[end_of_marker..].starts_with('\n') {
            &content[end_of_marker + 1..]
        } else {
            &content[end_of_marker..]
        };
        format!("{}{}{}", &content[..begin], CRAFT_SECTION, after)
    } else if content.is_empty() {
        CRAFT_SECTION.to_string()
    } else {
        let separator = if content.ends_with('\n') {
            "\n"
        } else {
            "\n\n"
        };
        format!("{}{}{}", content, separator, CRAFT_SECTION)
    }
}

// -------------------------------------------------------------------------
// Skills
// -------------------------------------------------------------------------

fn cmd_skills(cwd: &Path, command: Option<SkillsCommands>) -> Result<()> {
    match command {
        Some(SkillsCommands::Add {
            global,
            project,
            agent,
        }) => {
            cmd_skills_add(cwd, global, project, agent)?;
        }
        None => {
            cmd_skills_add(cwd, false, false, None)?;
        }
        Some(SkillsCommands::Show { path, name }) => {
            cmd_skills_show(path, &name)?;
        }
    }
    Ok(())
}

fn cmd_skills_add(
    cwd: &Path,
    force_global: bool,
    force_project: bool,
    agent: Option<String>,
) -> Result<()> {
    let home = std::env::var("HOME")
        .map_err(|_| anyhow::anyhow!("HOME not set; cannot locate agent skills directory"))?;

    let install_dirs = resolve_install_dirs(cwd, &home, force_global, force_project, &agent)?;
    preflight_external_shim_handoffs(cwd, &home, &install_dirs)?;

    let names = fluent::content::bundled_skill_names();

    // Always write the full skill to the fixed data directory so the shim
    // can read it for hand-off regardless of which agent directories exist.
    let data_dir = skills_data_dir(&home);
    for name in &names {
        work_task_executor::materialize_skill(name, &data_dir)?;
    }

    for dir in &install_dirs {
        let (agent_name, scope) = install_identity(cwd, dir);
        let lock_path = external_skill_lock_path(cwd, &home, dir);
        let fluent_outcome = install_skill_with_external_handoff(
            "fluent",
            dir,
            agent_name,
            scope,
            Some(&lock_path),
        )?;
        let mut outcomes = vec![fluent_outcome];
        if fluent_outcome != InstallOutcome::Conflict {
            outcomes.extend(
                names
                    .iter()
                    .filter(|name| **name != "fluent")
                    .map(|name| install_skill(name, dir, agent_name, scope))
                    .collect::<Result<Vec<_>>>()?,
            );
        }
        if fluent_outcome == InstallOutcome::Conflict {
            eprintln!("No bundled skills installed to {}", dir.display());
        } else if outcomes
            .iter()
            .all(|outcome| *outcome == InstallOutcome::Current)
        {
            eprintln!("Skills are current in {}", dir.display());
        } else if outcomes
            .iter()
            .any(|outcome| *outcome == InstallOutcome::ReplacedShim)
        {
            eprintln!(
                "Installed {} skills to {} (replaced Fluent shim)",
                names.len(),
                dir.display()
            );
        } else {
            eprintln!("Installed {} skills to {}", names.len(), dir.display());
        }
    }

    if install_dirs.is_empty() {
        eprintln!(
            "Wrote bundled skills to {}. No Fluent bootstrap or managed installation was found; use --agent with --project or --global to create one.",
            data_dir.display()
        );
    }

    let mut reported_dirs = install_dirs.clone();
    for dir in fluent_installation_dirs(cwd, &home)? {
        if !reported_dirs.contains(&dir) {
            reported_dirs.push(dir);
        }
    }
    let scopes = reported_dirs
        .iter()
        .map(|dir| install_identity(cwd, dir).1)
        .collect::<Vec<_>>();
    if scopes.contains(&"global") && scopes.contains(&"project") {
        let installations = reported_dirs
            .iter()
            .map(|dir| {
                let (_, scope) = install_identity(cwd, dir);
                format!("{scope} {}", dir.join("fluent").display())
            })
            .collect::<Vec<_>>()
            .join("; ");
        eprintln!(
            "Managed Fluent installations: {installations}. The agent may display both entries."
        );
    }

    Ok(())
}

fn fluent_installation_dirs(cwd: &Path, home: &str) -> Result<Vec<PathBuf>> {
    shim_candidate_roots(cwd, home)
        .into_iter()
        .filter(|dir| dir.join("fluent").is_dir())
        .filter_map(|dir| {
            let (agent, scope) = install_identity(cwd, &dir);
            match skill_install::is_managed_bundled_skill("fluent", &dir, agent, scope) {
                Ok(true) => Some(Ok(dir)),
                Ok(false) => None,
                Err(error) => Some(Err(error)),
            }
        })
        .collect()
}

fn install_skill(skill: &str, dir: &Path, agent: &str, scope: &str) -> Result<InstallOutcome> {
    let outcome = skill_install::install_bundled_skill(skill, dir, agent, scope)?;
    if outcome == InstallOutcome::Conflict {
        eprintln!(
            "Skipped unmarked skill at {}; remove it manually before Fluent can install there",
            dir.join(skill).display()
        );
    }
    Ok(outcome)
}

fn install_skill_with_external_handoff(
    skill: &str,
    dir: &Path,
    agent: &str,
    scope: &str,
    lock_path: Option<&Path>,
) -> Result<InstallOutcome> {
    let should_handoff = skill == "fluent" && is_sidecar_free_fluent_shim(dir)?;
    let handoff = if should_handoff {
        lock_path
            .map(skill_install::prepare_external_skill_lock_handoff)
            .transpose()?
            .flatten()
    } else {
        None
    };
    complete_external_skill_handoff(handoff, &dir.join(skill), || {
        install_skill(skill, dir, agent, scope)
    })
}

fn is_sidecar_free_fluent_shim(skills_dir: &Path) -> Result<bool> {
    if !is_fluent_shim(skills_dir) {
        return Ok(false);
    }
    let sidecar = skills_dir.join("fluent/.fluent-managed.json");
    match fs::symlink_metadata(&sidecar) {
        Ok(_) => Ok(false),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(true),
        Err(error) => {
            Err(error).with_context(|| format!("Failed to inspect {}", sidecar.display()))
        }
    }
}

fn complete_external_skill_handoff<F>(
    handoff: Option<skill_install::ExternalSkillLockHandoff>,
    managed_path: &Path,
    install: F,
) -> Result<InstallOutcome>
where
    F: FnOnce() -> Result<InstallOutcome>,
{
    if let Some(handoff) = handoff.as_ref() {
        handoff.apply()?;
    }

    let result = install();
    match result {
        Ok(InstallOutcome::ReplacedShim) => {
            if let Some(handoff) = handoff {
                eprintln!(
                    "Removed Fluent's bootstrap entry from {}; Fluent now manages {}",
                    handoff.path().display(),
                    managed_path.display()
                );
            }
            Ok(InstallOutcome::ReplacedShim)
        }
        Ok(outcome) => {
            if let Some(handoff) = handoff {
                handoff.restore()?;
            }
            Ok(outcome)
        }
        Err(error) => {
            if let Some(handoff) = handoff
                && let Err(restore_error) = handoff.restore()
            {
                bail!("{error:#}; also failed to restore external skill lock: {restore_error:#}");
            }
            Err(error)
        }
    }
}

fn install_identity(cwd: &Path, dir: &Path) -> (&'static str, &'static str) {
    if dir == cwd.join(".claude/skills") {
        ("claude", "project")
    } else if dir == cwd.join(".codex/skills") {
        ("codex", "project")
    } else if dir == cwd.join(".agents/skills") {
        ("universal", "project")
    } else if dir.ends_with(".codex/skills") {
        ("codex", "global")
    } else if dir.ends_with(".agents/skills") || dir.ends_with(".config/agents/skills") {
        ("universal", "global")
    } else {
        ("claude", "global")
    }
}

fn resolve_install_dirs(
    cwd: &Path,
    home: &str,
    force_global: bool,
    force_project: bool,
    agent: &Option<String>,
) -> Result<Vec<PathBuf>> {
    let explicit_scope = force_global || force_project;
    match (explicit_scope, agent.as_ref()) {
        (true, None) => bail!("--project and --global require --agent <codex|claude|*>"),
        (false, Some(_)) => bail!("--agent requires either --project or --global"),
        _ => {}
    }

    if let Some(agents) = agent {
        let home = PathBuf::from(home);
        let mut dirs = Vec::new();
        for a in agents.split(',') {
            let a = a.trim();
            let selected = match a {
                "*" => vec!["claude", "codex"],
                "claude" => vec!["claude"],
                "codex" => vec!["codex"],
                "" => bail!("--agent must name codex, claude, or *"),
                other => bail!("Unsupported agent {other:?}; expected codex, claude, or *"),
            };
            for selected_agent in selected {
                let dir = if force_project {
                    cwd.join(format!(".{selected_agent}/skills"))
                } else {
                    home.join(format!(".{selected_agent}/skills"))
                };
                if !dirs.contains(&dir) {
                    dirs.push(dir);
                }
            }
        }
        if dirs.is_empty() {
            bail!("--agent must name codex, claude, or *");
        }
        return Ok(dirs);
    }

    Ok(shim_candidate_roots(cwd, home)
        .into_iter()
        .filter(|dir| dir.join("fluent").exists())
        .collect())
}

fn is_fluent_shim(skills_dir: &Path) -> bool {
    skill_install::is_fluent_shim(&skills_dir.join("fluent"))
}

/// Candidate directories where the external `skills` CLI may install a shim.
fn shim_candidate_roots(cwd: &Path, home: &str) -> Vec<PathBuf> {
    let mut roots = project_shim_candidate_roots(cwd);
    roots.extend(global_shim_candidate_roots(home));
    roots
}

fn project_shim_candidate_roots(cwd: &Path) -> Vec<PathBuf> {
    vec![
        cwd.join(".agents/skills"),
        cwd.join(".claude/skills"),
        cwd.join(".codex/skills"),
    ]
}

fn global_shim_candidate_roots(home: &str) -> Vec<PathBuf> {
    let home = PathBuf::from(home);
    vec![
        home.join(".agents/skills"),
        home.join(".config/agents/skills"),
        home.join(".claude/skills"),
        home.join(".codex/skills"),
    ]
}

fn preflight_external_shim_handoffs(
    cwd: &Path,
    home: &str,
    install_dirs: &[PathBuf],
) -> Result<()> {
    for dir in install_dirs {
        if is_sidecar_free_fluent_shim(&dir)? {
            let lock_path = external_skill_lock_path(cwd, home, &dir);
            skill_install::prepare_external_skill_lock_handoff(&lock_path)?;
        }
    }
    Ok(())
}

fn external_skill_lock_path(cwd: &Path, home: &str, skills_dir: &Path) -> PathBuf {
    let (_, scope) = install_identity(cwd, skills_dir);
    if scope == "project" {
        cwd.join("skills-lock.json")
    } else {
        PathBuf::from(home).join(".agents/.skill-lock.json")
    }
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
        let content = fs::read_to_string(&skill_md)
            .with_context(|| format!("Cannot read skill {:?} at {}", name, skill_md.display()))?;
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
        approved_revisions: Default::default(),
    };

    Ok((!context.is_empty()).then_some(context))
}

fn read_optional_file(path: Option<String>) -> Result<Option<String>> {
    path.map(fs::read_to_string).transpose().map_err(Into::into)
}

fn build_release_contract(release: bool, criteria: Vec<String>) -> Result<Option<ReleaseContract>> {
    if !release {
        return Ok(None);
    }
    if criteria.is_empty() {
        bail!("release work requires at least one --release-criterion ID=STATEMENT");
    }
    let criteria = criteria
        .into_iter()
        .map(|value| {
            let (id, statement) = value.split_once('=').ok_or_else(|| {
                anyhow::anyhow!("invalid --release-criterion {value:?}; expected ID=STATEMENT")
            })?;
            Ok(ReleaseCriterion {
                id: id.trim().to_string(),
                statement: statement.trim().to_string(),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(Some(ReleaseContract::new(criteria)?))
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

fn write_gitignore_if_absent(fluent_dir: &Path) -> Result<()> {
    let gitignore = fluent_dir.join(".gitignore");
    if !gitignore.exists() {
        fs::write(&gitignore, fluent::setup::FLUENT_GITIGNORE)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use fluent::coder::CoderKind;
    use fluent::work_model::{
        Attempt, AttemptKind, AttemptStatus, CoderMapping, CoderModelPair, Task, TaskKind,
        TaskStatus, WorkspaceAccess,
    };

    /// Build an Attempt whose single task is paused, with a distinct coder in
    /// each mapping slot so the resolved coder identifies which slot was used.
    fn attempt_with_paused_task(kind: TaskKind) -> Attempt {
        let pair = |coder: CoderKind| CoderModelPair {
            coder,
            model: String::new(),
            effort: None,
        };
        Attempt {
            id: "a1".to_string(),
            work_item_id: "w1".to_string(),
            kind: AttemptKind::Write,
            status: AttemptStatus::NeedsUser,
            coder_mapping: CoderMapping {
                write: pair(CoderKind::Pi),
                review: pair(CoderKind::Claude),
            },
            tasks: vec![Task {
                id: "a1-task-1".to_string(),
                kind,
                status: TaskStatus::NeedsUser,
                role: "author".to_string(),
                instructions: None,
                work_item_id: "w1".to_string(),
                attempt_id: Some("a1".to_string()),
                workspace_access: WorkspaceAccess {
                    reads: Vec::new(),
                    writes: Vec::new(),
                },
                artifact_area: None,
                review_context: None,
                writer_correction_assignment: None,
                verification_context: None,
                evidence_review_context: None,
                input_artifacts: Vec::new(),
                depends_on: None,
                output: None,
                created_at: None,
                started_at: None,
                completed_at: None,
            }],
            review_state: None,
            pause_kind: None,
            artifacts: Vec::new(),
            created_at: None,
            completed_at: None,
            ..Default::default()
        }
    }

    #[test]
    fn resolve_paused_coder_routes_through_canonical_mapping() {
        // A paused Tester task resolves to the write coder through the
        // canonical `for_task_kind` mapping.
        assert_eq!(
            resolve_paused_coder(&attempt_with_paused_task(TaskKind::Tester)).as_deref(),
            Some("pi")
        );
        assert_eq!(
            resolve_paused_coder(&attempt_with_paused_task(TaskKind::Review)).as_deref(),
            Some("claude")
        );
    }

    #[test]
    fn resolve_paused_coder_none_without_paused_task() {
        let mut attempt = attempt_with_paused_task(TaskKind::Write);
        attempt.tasks[0].status = TaskStatus::Complete;
        assert_eq!(resolve_paused_coder(&attempt), None);
    }

    #[test]
    fn failed_shim_replacement_restores_external_lock() {
        let tmp = tempfile::tempdir().unwrap();
        let lock_path = tmp.path().join("skills-lock.json");
        let original = serde_json::json!({
            "version": 1,
            "skills": {
                "fluent": {
                    "source": "mrinalwadhwa/fluent",
                    "sourceType": "github",
                    "skillPath": "skills/fluent/SKILL.md",
                    "computedHash": "shim-hash"
                }
            }
        });
        fs::write(&lock_path, serde_json::to_vec_pretty(&original).unwrap()).unwrap();
        let handoff = skill_install::prepare_external_skill_lock_handoff(&lock_path)
            .unwrap()
            .unwrap();

        let result = complete_external_skill_handoff(
            Some(handoff),
            &tmp.path().join(".agents/skills/fluent"),
            || bail!("injected install failure"),
        );

        assert!(result.is_err());
        let restored: serde_json::Value =
            serde_json::from_slice(&fs::read(lock_path).unwrap()).unwrap();
        assert_eq!(restored, original);
    }
}
