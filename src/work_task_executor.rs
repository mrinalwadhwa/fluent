use anyhow::{Context, Result, bail};
use std::collections::BTreeMap;
use std::fs;
use std::io::ErrorKind;
use std::path::{Component, Path, PathBuf};

use crate::coder::{CoderKind, CoderSandbox};
use crate::content::ContentResolver;
use crate::credential;
use crate::git;
use crate::hooks;
use crate::os;
use crate::prep;
use crate::review_diff_command::render_review_diff_command;
use crate::work_model::{
    ArtifactRef, AttemptKind, AttemptStatus, TaskKind, TaskOutput, TaskStatus, WorkItem,
    WorkModelStorageError, WorkModelStore, WorkspaceRef, resolve_expected_candidate_workspace_path,
    resolve_managed_sibling_workspace_path, to_json_pretty, work_artifact_path,
};
use crate::worktree;

pub struct WorkTaskRunConfig<'a> {
    pub project_root: &'a Path,
    pub store: &'a WorkModelStore,
    pub work_item_id: &'a str,
    pub attempt_id: &'a str,
    pub task_id: &'a str,
    pub resolver: &'a ContentResolver,
    pub extra_args: &'a [String],
    pub no_sandbox: bool,
    pub store_lock: Option<&'a std::sync::Mutex<()>>,
}

pub struct WorkTaskRunResult {
    pub task_id: String,
    pub output: String,
}

pub fn run_task(config: WorkTaskRunConfig<'_>) -> Result<WorkTaskRunResult> {
    let item = read_work_item_or_not_found(config.store, config.work_item_id)?;
    item.ensure_not_abandoned()?;
    let (attempt_index, task_index) =
        find_attempt_task_indexes(&item, config.attempt_id, config.task_id)
            .ok_or_else(|| anyhow::anyhow!("Task {:?} not found", config.task_id))?;

    let task_kind = item.attempts[attempt_index].tasks[task_index].kind;
    let mapping_pair = item.attempts[attempt_index]
        .coder_mapping
        .for_task_kind(task_kind);
    let coder_kind = mapping_pair.coder;
    let model = if mapping_pair.model.is_empty() {
        None
    } else {
        Some(mapping_pair.model.as_str())
    };
    let effort = mapping_pair.effort.as_deref();

    match task_kind {
        TaskKind::Write => run_write_task(config, coder_kind, model, effort),
        TaskKind::Review => run_review_task(config, coder_kind, model, effort),
        TaskKind::BehaviorTests => {
            bail!("BehaviorTests tasks are retired; use Tester tasks instead")
        }
        TaskKind::Tester => run_tester_task(config),
        kind => bail!(
            "Task {:?} is kind {kind}; unsupported by task run",
            config.task_id
        ),
    }
}

fn run_write_task(
    config: WorkTaskRunConfig<'_>,
    coder_kind: CoderKind,
    model: Option<&str>,
    effort: Option<&str>,
) -> Result<WorkTaskRunResult> {
    let mut item = read_work_item_or_not_found(config.store, config.work_item_id)?;
    let attempt_index = item
        .attempts
        .iter()
        .position(|attempt| attempt.id == config.attempt_id)
        .ok_or_else(|| anyhow::anyhow!("Attempt {:?} not found", config.attempt_id))?;
    let task_index = item.attempts[attempt_index]
        .tasks
        .iter()
        .position(|task| task.id == config.task_id)
        .ok_or_else(|| anyhow::anyhow!("Task {:?} not found", config.task_id))?;

    let task = &item.attempts[attempt_index].tasks[task_index];
    if task.kind != TaskKind::Write {
        bail!(
            "Task {:?} is kind {}; expected write",
            config.task_id,
            task.kind
        );
    }
    if task.status != TaskStatus::Planned {
        bail!(
            "Task {:?} is {}; expected planned",
            config.task_id,
            task.status
        );
    }
    if task.workspace_access.writes.len() != 1 {
        bail!(
            "Task {:?} must declare exactly one writable workspace; found {}",
            config.task_id,
            task.workspace_access.writes.len()
        );
    }

    let workspace = task.workspace_access.writes[0].clone();
    let workspace_path = resolve_managed_workspace_path(
        config.project_root,
        &workspace.path,
        config.work_item_id,
        config.attempt_id,
    )?;
    let prior_reviews = resolve_input_artifact_paths(config.project_root, &task.input_artifacts)?;
    let source_branch = current_branch(config.project_root)?;
    let branch_name = format!(
        "work/{}/{}/{}",
        config.work_item_id, config.attempt_id, config.task_id
    );

    prepare_task_worktree(
        config.project_root,
        &workspace_path,
        &branch_name,
        &source_branch,
    )?;
    worktree::disable_commit_signing(&workspace_path)?;
    let baseline_commit = head_commit(&workspace_path)?;

    let is_first_write = !item.attempts[attempt_index]
        .tasks
        .iter()
        .any(|t| t.kind == TaskKind::Tester);
    if is_first_write {
        capture_baseline_tester(
            config.project_root,
            &workspace_path,
            config.work_item_id,
            config.attempt_id,
            config.no_sandbox,
            config.resolver,
        );
    }

    let lock_path =
        crate::lease::task_lock_path(config.project_root, config.work_item_id, config.task_id);
    let _lease = crate::lease::acquire(&lock_path)
        .with_context(|| format!("Failed to acquire lease for Task {:?}", config.task_id))?;

    item.attempts[attempt_index].status = AttemptStatus::Executing;
    item.attempts[attempt_index].tasks[task_index].status = TaskStatus::Executing;
    crate::work_model::mark_task_started(&mut item.attempts[attempt_index].tasks[task_index]);
    item.attempts[attempt_index].tasks[task_index].output = None;
    config.store.write_work_item(&item)?;

    let mut run_result = run_task_coder(
        &item,
        config.attempt_id,
        config.task_id,
        config.project_root,
        &workspace_path,
        &prior_reviews,
        config.resolver,
        config.extra_args,
        coder_kind,
        config.no_sandbox,
        model,
        effort,
    );
    let mut retries = 0;
    while run_result.is_err() && !is_auth_error(&run_result) && retries < max_task_retries() {
        retries += 1;
        eprintln!(
            "  Retrying coder (attempt {}/{})",
            retries + 1,
            max_task_retries() + 1
        );
        run_result = run_task_coder(
            &item,
            config.attempt_id,
            config.task_id,
            config.project_root,
            &workspace_path,
            &prior_reviews,
            config.resolver,
            config.extra_args,
            coder_kind,
            config.no_sandbox,
            model,
            effort,
        );
    }

    if let Err(error) = run_result {
        let auth_message = error
            .downcast_ref::<crate::claude_auth::AuthError>()
            .map(|e| e.user_message());
        mark_task_failed_attempt_needs_user(
            config.store,
            config.project_root,
            config.work_item_id,
            config.attempt_id,
            config.task_id,
            auth_message.as_deref(),
            &crate::notify::notify,
        )?;
        return Err(error);
    }

    if let Err(error) = ensure_clean_worktree(&workspace_path) {
        mark_task_failed(
            config.store,
            config.work_item_id,
            config.attempt_id,
            config.task_id,
        )?;
        return Err(error);
    }
    let produced_count = match commits_ahead(&workspace_path, &baseline_commit) {
        Ok(count) => count,
        Err(error) => {
            mark_task_failed(
                config.store,
                config.work_item_id,
                config.attempt_id,
                config.task_id,
            )?;
            return Err(error);
        }
    };
    if produced_count == 0 {
        mark_task_failed(
            config.store,
            config.work_item_id,
            config.attempt_id,
            config.task_id,
        )?;
        bail!(
            "Task {:?} has no committed Task output; commit the Task output before completing",
            config.task_id
        );
    }
    let commit = head_commit(&workspace_path)?;

    let output = TaskOutput {
        workspace_id: workspace.id.clone(),
        workspace_path: workspace.path.clone(),
        source_branch,
        base_commit: Some(baseline_commit),
        commit: commit.clone(),
    };
    let mut completed_item = read_work_item_or_not_found(config.store, config.work_item_id)?;
    let (attempt_index, task_index) =
        find_attempt_task_indexes(&completed_item, config.attempt_id, config.task_id)
            .ok_or_else(|| anyhow::anyhow!("Task {:?} not found", config.task_id))?;
    crate::work_model::set_task_terminal(
        &mut completed_item.attempts[attempt_index].tasks[task_index],
        TaskStatus::Complete,
    );
    completed_item.attempts[attempt_index].tasks[task_index].output = Some(output);
    completed_item.attempts[attempt_index]
        .artifacts
        .push(ArtifactRef {
            producer_id: config.task_id.to_string(),
            path: commit.clone(),
        });
    let all_complete = completed_item.attempts[attempt_index]
        .tasks
        .iter()
        .all(|task| task.status == TaskStatus::Complete);
    if all_complete {
        crate::work_model::set_attempt_terminal(
            &mut completed_item.attempts[attempt_index],
            AttemptStatus::Complete,
        );
    } else {
        completed_item.attempts[attempt_index].status = AttemptStatus::Executing;
    }
    config.store.write_work_item(&completed_item)?;

    Ok(WorkTaskRunResult {
        task_id: config.task_id.to_string(),
        output: commit,
    })
}

fn run_review_task(
    config: WorkTaskRunConfig<'_>,
    coder_kind: CoderKind,
    model: Option<&str>,
    effort: Option<&str>,
) -> Result<WorkTaskRunResult> {
    let lock_path =
        crate::lease::task_lock_path(config.project_root, config.work_item_id, config.task_id);
    let _lease = crate::lease::acquire(&lock_path)
        .with_context(|| format!("Failed to acquire lease for Task {:?}", config.task_id))?;

    let (
        attempt_kind,
        workspace_reads,
        candidate_commit,
        input_artifacts,
        artifact_dir,
        review_path,
        role,
    ) = {
        let _lock = config
            .store_lock
            .map(|m| m.lock().unwrap_or_else(|e| e.into_inner()));
        let mut item = read_work_item_or_not_found(config.store, config.work_item_id)?;
        let (attempt_index, task_index) =
            find_attempt_task_indexes(&item, config.attempt_id, config.task_id)
                .ok_or_else(|| anyhow::anyhow!("Task {:?} not found", config.task_id))?;

        let task = &item.attempts[attempt_index].tasks[task_index];
        let attempt_kind = item.attempts[attempt_index].kind.clone();
        if task.status != TaskStatus::Planned {
            bail!(
                "Task {:?} is {}; expected planned",
                config.task_id,
                task.status
            );
        }
        if !task.workspace_access.writes.is_empty() {
            bail!("Review Task {:?} cannot write a workspace", config.task_id);
        }
        if task.workspace_access.reads.is_empty() {
            bail!(
                "Review Task {:?} must declare at least one readable candidate workspace",
                config.task_id
            );
        }
        let artifact_area = task
            .artifact_area
            .as_ref()
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Review Task {:?} must declare an artifact area",
                    config.task_id
                )
            })?
            .path
            .clone();
        let review_context = task.review_context.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "Review Task {:?} must declare review context",
                config.task_id
            )
        })?;
        let workspace_reads = task.workspace_access.reads.clone();
        let role = task.role.clone();
        let candidate_commit = review_context.candidate_commit.clone();
        let input_artifacts =
            resolve_input_artifact_paths(config.project_root, &task.input_artifacts)?;
        let artifact_dir = resolve_managed_artifact_area_path(config.project_root, &artifact_area)?;
        let review_path = artifact_dir.join("review.md");

        if !workspace_reads.iter().any(|workspace| {
            workspace.id == review_context.candidate_workspace_id
                && workspace.path == review_context.candidate_workspace_path
        }) {
            bail!(
                "Review Task {:?} review context candidate must match a readable workspace",
                config.task_id
            );
        }
        ReviewReadableWorkspaces::preflight(
            config.project_root,
            config.work_item_id,
            config.attempt_id,
            &attempt_kind,
            &workspace_reads,
            &candidate_commit,
        )?;
        fs::create_dir_all(&artifact_dir)?;
        if review_path.is_file() {
            fs::remove_file(&review_path)?;
        } else if review_path.exists() {
            bail!(
                "Review Task {:?} artifact path exists but is not a file: {}",
                config.task_id,
                review_path.display()
            );
        }

        item.attempts[attempt_index].status = AttemptStatus::Reviewing;
        item.attempts[attempt_index].tasks[task_index].status = TaskStatus::Executing;
        crate::work_model::mark_task_started(&mut item.attempts[attempt_index].tasks[task_index]);
        item.attempts[attempt_index].tasks[task_index].output = None;
        config.store.write_work_item(&item)?;

        (
            attempt_kind,
            workspace_reads,
            candidate_commit,
            input_artifacts,
            artifact_dir,
            review_path,
            role,
        )
    };

    let item = read_work_item_or_not_found(config.store, config.work_item_id)?;

    // Materialize planning artifacts and bundled expertise BEFORE the source
    // checkout review guard snapshots the workspace. Otherwise the guard
    // treats these Fluent-managed files as reviewer-induced changes when
    // diffing against its baseline.
    materialize_planning_files(&item, config.project_root)?;
    materialize_general_expertise(config.project_root)?;
    // Materialize this Task's review-<role> skill here as well, so it is part of
    // the guard's baseline. review_skill_path would otherwise write it during
    // prompt construction, after the snapshot, and the source-checkout guard
    // rejects that as a reviewer-induced change.
    materialize_skill(
        &format!("review-{role}"),
        &review_skills_dir(config.project_root),
    )?;

    let readable_workspaces = match ReviewReadableWorkspaces::resolve(
        config.project_root,
        config.work_item_id,
        config.attempt_id,
        &attempt_kind,
        &workspace_reads,
        &candidate_commit,
        &artifact_dir,
    ) {
        Ok(workspaces) => workspaces,
        Err(error) => {
            lock_mark_task_failed(
                config.store,
                config.store_lock,
                config.work_item_id,
                config.attempt_id,
                config.task_id,
            )?;
            return Err(error);
        }
    };
    let readable_workspace_paths = readable_workspaces.paths();

    if let Some(candidate_path) = readable_workspace_paths.first() {
        prepare_reviewer_build_cache(
            candidate_path,
            &artifact_dir,
            config.work_item_id,
            config.attempt_id,
            config.task_id,
        );
    }

    let mut run_result = run_review_coder(
        &item,
        config.attempt_id,
        config.task_id,
        config.project_root,
        &artifact_dir,
        &review_path,
        &readable_workspace_paths,
        &input_artifacts,
        attempt_kind.is_review_only_like(),
        config.resolver,
        config.extra_args,
        coder_kind,
        config.no_sandbox,
        model,
        effort,
    );
    let mut retries = 0;
    while run_result.is_err() && !is_auth_error(&run_result) && retries < max_task_retries() {
        retries += 1;
        eprintln!(
            "  Retrying coder (attempt {}/{})",
            retries + 1,
            max_task_retries() + 1
        );
        run_result = run_review_coder(
            &item,
            config.attempt_id,
            config.task_id,
            config.project_root,
            &artifact_dir,
            &review_path,
            &readable_workspace_paths,
            &input_artifacts,
            attempt_kind.is_review_only_like(),
            config.resolver,
            config.extra_args,
            coder_kind,
            config.no_sandbox,
            model,
            effort,
        );
    }

    if let Err(error) = readable_workspaces.finish() {
        lock_mark_task_failed(
            config.store,
            config.store_lock,
            config.work_item_id,
            config.attempt_id,
            config.task_id,
        )?;
        return Err(error);
    }

    if let Err(error) = run_result {
        let auth_message = error
            .downcast_ref::<crate::claude_auth::AuthError>()
            .map(|e| e.user_message());
        lock_mark_task_failed_attempt_needs_user(
            config.store,
            config.store_lock,
            config.project_root,
            config.work_item_id,
            config.attempt_id,
            config.task_id,
            auth_message.as_deref(),
            &crate::notify::notify,
        )?;
        return Err(error);
    }

    if !review_path.is_file() {
        lock_mark_task_failed(
            config.store,
            config.store_lock,
            config.work_item_id,
            config.attempt_id,
            config.task_id,
        )?;
        bail!(
            "Review Task {:?} completed without writing {}",
            config.task_id,
            review_path.display()
        );
    }

    {
        let _lock = config
            .store_lock
            .map(|m| m.lock().unwrap_or_else(|e| e.into_inner()));
        let mut completed_item = read_work_item_or_not_found(config.store, config.work_item_id)?;
        let (attempt_index, task_index) =
            find_attempt_task_indexes(&completed_item, config.attempt_id, config.task_id)
                .ok_or_else(|| anyhow::anyhow!("Task {:?} not found", config.task_id))?;
        crate::work_model::set_task_terminal(
            &mut completed_item.attempts[attempt_index].tasks[task_index],
            TaskStatus::Complete,
        );
        completed_item.attempts[attempt_index]
            .artifacts
            .push(ArtifactRef {
                producer_id: config.task_id.to_string(),
                path: path_for_model(config.project_root, &review_path),
            });
        let all_complete = completed_item.attempts[attempt_index]
            .tasks
            .iter()
            .all(|task| task.status == TaskStatus::Complete);
        if all_complete {
            crate::work_model::set_attempt_terminal(
                &mut completed_item.attempts[attempt_index],
                AttemptStatus::Complete,
            );
        } else {
            completed_item.attempts[attempt_index].status = AttemptStatus::Reviewing;
        }
        config.store.write_work_item(&completed_item)?;
    }

    Ok(WorkTaskRunResult {
        task_id: config.task_id.to_string(),
        output: path_for_model(config.project_root, &review_path),
    })
}

fn capture_baseline_tester(
    project_root: &Path,
    candidate_workspace: &Path,
    work_item_id: &str,
    attempt_id: &str,
    no_sandbox: bool,
    resolver: &ContentResolver,
) {
    let baseline_artifact = format!("{attempt_id}-baseline-tester");
    let artifact_rel = work_artifact_path(work_item_id, attempt_id, &baseline_artifact);
    let artifact_dir = project_root.join(&artifact_rel);
    if let Err(e) = fs::create_dir_all(&artifact_dir) {
        eprintln!("  Baseline tester: failed to create artifact dir: {e:#}");
        return;
    }
    eprintln!("  Baseline tester   running on pre-write workspace");
    if let Err(e) = crate::tester::run(candidate_workspace, &artifact_dir, no_sandbox, resolver) {
        eprintln!("  Baseline tester: run failed: {e:#}");
    }
}

fn run_tester_task(config: WorkTaskRunConfig<'_>) -> Result<WorkTaskRunResult> {
    let lock_path =
        crate::lease::task_lock_path(config.project_root, config.work_item_id, config.task_id);
    let _lease = crate::lease::acquire(&lock_path)
        .with_context(|| format!("Failed to acquire lease for Task {:?}", config.task_id))?;

    let artifact_dir = {
        let _lock = config
            .store_lock
            .map(|m| m.lock().unwrap_or_else(|e| e.into_inner()));
        let mut item = read_work_item_or_not_found(config.store, config.work_item_id)?;
        let (attempt_index, task_index) =
            find_attempt_task_indexes(&item, config.attempt_id, config.task_id)
                .ok_or_else(|| anyhow::anyhow!("Task {:?} not found", config.task_id))?;

        let task = &item.attempts[attempt_index].tasks[task_index];
        if task.status != TaskStatus::Planned {
            bail!(
                "Task {:?} is {}; expected planned",
                config.task_id,
                task.status
            );
        }
        let artifact_area = task
            .artifact_area
            .as_ref()
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Tester Task {:?} must declare an artifact area",
                    config.task_id
                )
            })?
            .path
            .clone();
        let artifact_dir = resolve_managed_artifact_area_path(config.project_root, &artifact_area)?;
        fs::create_dir_all(&artifact_dir)?;

        item.attempts[attempt_index].status = AttemptStatus::Reviewing;
        item.attempts[attempt_index].tasks[task_index].status = TaskStatus::Executing;
        crate::work_model::mark_task_started(&mut item.attempts[attempt_index].tasks[task_index]);
        item.attempts[attempt_index].tasks[task_index].output = None;
        config.store.write_work_item(&item)?;

        artifact_dir
    };

    let item = read_work_item_or_not_found(config.store, config.work_item_id)?;
    let (attempt_index, task_index) =
        find_attempt_task_indexes(&item, config.attempt_id, config.task_id)
            .ok_or_else(|| anyhow::anyhow!("Task {:?} not found", config.task_id))?;
    let task = &item.attempts[attempt_index].tasks[task_index];
    let review_context = task.review_context.as_ref().ok_or_else(|| {
        anyhow::anyhow!(
            "Tester Task {:?} must declare review context",
            config.task_id
        )
    })?;

    let candidate_workspace = resolve_workspace_path(
        config.project_root,
        &review_context.candidate_workspace_path,
    );

    eprintln!("  Fluent           work task run");
    eprintln!("  Work Item         {}", item.id);
    eprintln!("  Attempt           {}", config.attempt_id);
    eprintln!("  Task              {} (tester)", config.task_id);
    eprintln!("  Artifact area     {}", artifact_dir.display());
    eprintln!("  Candidate         {}", candidate_workspace.display());

    let results_path = artifact_dir.join("tester-results.json");

    let mut tester_result = crate::tester::run(
        &candidate_workspace,
        &artifact_dir,
        config.no_sandbox,
        config.resolver,
    );
    let mut retries = 0;
    while tester_result.is_err() && retries < max_task_retries() {
        retries += 1;
        eprintln!(
            "  Retrying tester (attempt {}/{})",
            retries + 1,
            max_task_retries() + 1
        );
        tester_result = crate::tester::run(
            &candidate_workspace,
            &artifact_dir,
            config.no_sandbox,
            config.resolver,
        );
    }

    match tester_result {
        Ok(()) => {}
        Err(error) => {
            eprintln!("  Tester error: {error:#}");
            lock_mark_task_failed_attempt_needs_user(
                config.store,
                config.store_lock,
                config.project_root,
                config.work_item_id,
                config.attempt_id,
                config.task_id,
                None,
                &crate::notify::notify,
            )?;
            return Err(error);
        }
    }

    if !results_path.is_file() {
        lock_mark_task_failed(
            config.store,
            config.store_lock,
            config.work_item_id,
            config.attempt_id,
            config.task_id,
        )?;
        bail!(
            "Tester Task {:?} completed without writing {}",
            config.task_id,
            results_path.display()
        );
    }

    {
        let _lock = config
            .store_lock
            .map(|m| m.lock().unwrap_or_else(|e| e.into_inner()));
        let mut completed_item = read_work_item_or_not_found(config.store, config.work_item_id)?;
        let (attempt_index, task_index) =
            find_attempt_task_indexes(&completed_item, config.attempt_id, config.task_id)
                .ok_or_else(|| anyhow::anyhow!("Task {:?} not found", config.task_id))?;
        crate::work_model::set_task_terminal(
            &mut completed_item.attempts[attempt_index].tasks[task_index],
            TaskStatus::Complete,
        );
        completed_item.attempts[attempt_index]
            .artifacts
            .push(ArtifactRef {
                producer_id: config.task_id.to_string(),
                path: path_for_model(config.project_root, &results_path),
            });
        let all_complete = completed_item.attempts[attempt_index]
            .tasks
            .iter()
            .all(|task| task.status == TaskStatus::Complete);
        if all_complete {
            crate::work_model::set_attempt_terminal(
                &mut completed_item.attempts[attempt_index],
                AttemptStatus::Complete,
            );
        } else {
            completed_item.attempts[attempt_index].status = AttemptStatus::Reviewing;
        }
        config.store.write_work_item(&completed_item)?;
    }

    Ok(WorkTaskRunResult {
        task_id: config.task_id.to_string(),
        output: path_for_model(config.project_root, &results_path),
    })
}

struct ReviewReadableWorkspaces {
    workspaces: Vec<ReviewReadableWorkspace>,
}

impl ReviewReadableWorkspaces {
    fn preflight(
        project_root: &Path,
        work_item_id: &str,
        attempt_id: &str,
        attempt_kind: &AttemptKind,
        workspace_refs: &[WorkspaceRef],
        expected_source_head: &str,
    ) -> Result<()> {
        for workspace_ref in workspace_refs {
            let workspace_path = resolve_review_readable_workspace_path(
                project_root,
                workspace_ref,
                work_item_id,
                attempt_id,
                attempt_kind,
            )?;
            ensure_same_git_repository(project_root, &workspace_path)?;
            if *attempt_kind == AttemptKind::ReviewOnly && workspace_path == project_root {
                ensure_head_matches_review_context(&workspace_path, expected_source_head)?;
                ensure_no_non_fluent_worktree_changes(&workspace_path)?;
            } else {
                ensure_registered_worktree(project_root, &workspace_path)?;
                ensure_clean_worktree(&workspace_path)?;
            }
        }
        Ok(())
    }

    fn resolve(
        project_root: &Path,
        work_item_id: &str,
        attempt_id: &str,
        attempt_kind: &AttemptKind,
        workspace_refs: &[WorkspaceRef],
        expected_source_head: &str,
        artifact_dir: &Path,
    ) -> Result<Self> {
        let mut workspaces = Vec::new();
        for workspace_ref in workspace_refs {
            let workspace_path = resolve_review_readable_workspace_path(
                project_root,
                workspace_ref,
                work_item_id,
                attempt_id,
                attempt_kind,
            )?;
            ensure_same_git_repository(project_root, &workspace_path)?;
            let workspace =
                if *attempt_kind == AttemptKind::ReviewOnly && workspace_path == project_root {
                    ReviewReadableWorkspace::Source(SourceCheckoutReviewGuard::begin(
                        workspace_path,
                        expected_source_head,
                        artifact_dir,
                    )?)
                } else {
                    ensure_registered_worktree(project_root, &workspace_path)?;
                    ensure_clean_worktree(&workspace_path)?;
                    ReviewReadableWorkspace::Candidate(CandidateReviewWorkspace {
                        path: workspace_path.clone(),
                        head: head_commit(&workspace_path)?,
                    })
                };
            workspaces.push(workspace);
        }
        Ok(Self { workspaces })
    }

    fn paths(&self) -> Vec<PathBuf> {
        self.workspaces
            .iter()
            .map(ReviewReadableWorkspace::path)
            .collect()
    }

    fn finish(&self) -> Result<()> {
        for workspace in &self.workspaces {
            workspace.finish()?;
        }
        Ok(())
    }
}

enum ReviewReadableWorkspace {
    Candidate(CandidateReviewWorkspace),
    Source(SourceCheckoutReviewGuard),
}

impl ReviewReadableWorkspace {
    fn path(&self) -> PathBuf {
        match self {
            Self::Candidate(workspace) => workspace.path.clone(),
            Self::Source(guard) => guard.path.clone(),
        }
    }

    fn finish(&self) -> Result<()> {
        match self {
            Self::Candidate(workspace) => workspace.finish(),
            Self::Source(guard) => guard.finish(),
        }
    }
}

struct CandidateReviewWorkspace {
    path: PathBuf,
    head: String,
}

impl CandidateReviewWorkspace {
    fn finish(&self) -> Result<()> {
        ensure_head_unchanged(&self.path, &self.head)?;
        ensure_clean_worktree(&self.path)
    }
}

struct SourceCheckoutReviewGuard {
    path: PathBuf,
    head: String,
    status: Vec<String>,
    protected_fluent_files: BTreeMap<PathBuf, Vec<u8>>,
    allowed_artifact_dir: PathBuf,
}

impl SourceCheckoutReviewGuard {
    fn begin(path: PathBuf, expected_head: &str, allowed_artifact_dir: &Path) -> Result<Self> {
        ensure_head_matches_review_context(&path, expected_head)?;
        ensure_no_non_fluent_worktree_changes(&path)?;
        Ok(Self {
            head: head_commit(&path)?,
            status: worktree_status(&path)?,
            protected_fluent_files: protected_fluent_file_snapshot(&path, allowed_artifact_dir)?,
            path,
            allowed_artifact_dir: allowed_artifact_dir.to_path_buf(),
        })
    }

    fn finish(&self) -> Result<()> {
        ensure_source_head_unchanged(&self.path, &self.head)?;
        let non_fluent_error = if let Err(error) = ensure_no_non_fluent_worktree_changes(&self.path)
        {
            restore_non_fluent_worktree_changes(&self.path)?;
            Some(error)
        } else {
            None
        };
        if let Err(error) = ensure_source_changed_only_artifact_area(self) {
            restore_source_changes_outside_artifact_area(self)?;
            return Err(error);
        }
        if let Some(error) = non_fluent_error {
            return Err(error);
        }
        Ok(())
    }
}

fn read_work_item_or_not_found(store: &WorkModelStore, id: &str) -> Result<WorkItem> {
    match store.read_work_item(id) {
        Ok(item) => Ok(item),
        Err(WorkModelStorageError::ReadFile { source, .. })
            if source.kind() == ErrorKind::NotFound =>
        {
            bail!("Work Item {id:?} not found")
        }
        Err(error) => Err(error.into()),
    }
}

fn find_attempt_task_indexes(
    item: &WorkItem,
    attempt_id: &str,
    task_id: &str,
) -> Option<(usize, usize)> {
    let attempt_index = item
        .attempts
        .iter()
        .position(|attempt| attempt.id == attempt_id)?;
    let task_index = item.attempts[attempt_index]
        .tasks
        .iter()
        .position(|task| task.id == task_id)?;
    Some((attempt_index, task_index))
}

const DEFAULT_MAX_TASK_RETRIES: usize = 2;

fn max_task_retries() -> usize {
    std::env::var("FLUENT_MAX_TASK_RETRIES")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(DEFAULT_MAX_TASK_RETRIES)
}

fn mark_task_failed(
    store: &WorkModelStore,
    work_item_id: &str,
    attempt_id: &str,
    task_id: &str,
) -> Result<()> {
    let mut item = read_work_item_or_not_found(store, work_item_id)?;
    if let Some((attempt_index, task_index)) = find_attempt_task_indexes(&item, attempt_id, task_id)
    {
        crate::work_model::set_task_terminal(
            &mut item.attempts[attempt_index].tasks[task_index],
            TaskStatus::Failed,
        );
        crate::work_model::set_attempt_terminal(
            &mut item.attempts[attempt_index],
            AttemptStatus::Failed,
        );
        store.write_work_item(&item)?;
    }
    Ok(())
}

fn is_auth_error(result: &Result<()>) -> bool {
    result
        .as_ref()
        .err()
        .map_or(false, |e| e.is::<crate::claude_auth::AuthError>())
}

fn mark_task_failed_attempt_needs_user(
    store: &WorkModelStore,
    project_root: &Path,
    work_item_id: &str,
    attempt_id: &str,
    task_id: &str,
    auth_message: Option<&str>,
    notify_fn: &dyn Fn(&str, &str),
) -> Result<()> {
    let mut item = read_work_item_or_not_found(store, work_item_id)?;
    if let Some((attempt_index, task_index)) = find_attempt_task_indexes(&item, attempt_id, task_id)
    {
        crate::work_model::set_task_terminal(
            &mut item.attempts[attempt_index].tasks[task_index],
            TaskStatus::Failed,
        );
        let pause_kind = if auth_message.is_some() {
            crate::work_model::PauseKind::Auth
        } else {
            crate::work_model::PauseKind::RoundCap
        };
        crate::work_model::suspend_attempt(&mut item.attempts[attempt_index], pause_kind);
        if auth_message.is_some() {
            notify_fn(
                "Fluent",
                "Auth token expired. Run 'claude /login' to re-authenticate, then 'fluent attempt run'.",
            );
        }
        let handoff_path = write_task_error_handoff(
            project_root,
            work_item_id,
            attempt_id,
            task_id,
            auth_message,
        )?;
        item.attempts[attempt_index].artifacts.push(ArtifactRef {
            producer_id: task_id.to_string(),
            path: handoff_path,
        });
        store.write_work_item(&item)?;
    }
    Ok(())
}

fn write_task_error_handoff(
    project_root: &Path,
    work_item_id: &str,
    attempt_id: &str,
    task_id: &str,
    auth_message: Option<&str>,
) -> Result<String> {
    let filename = format!("needs-user-{task_id}.md");
    let relative_path = work_artifact_path(work_item_id, attempt_id, &filename);
    let path = project_root.join(&relative_path);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let body = if let Some(msg) = auth_message {
        format!("# Attempt needs user input\n\n{msg}\n")
    } else {
        format!(
            "# Attempt needs user input\n\nTask {task_id:?} failed after {} retries. \
             The coder execution errored persistently.\n",
            max_task_retries()
        )
    };
    fs::write(&path, body)?;
    Ok(relative_path)
}

fn lock_mark_task_failed(
    store: &WorkModelStore,
    store_lock: Option<&std::sync::Mutex<()>>,
    work_item_id: &str,
    attempt_id: &str,
    task_id: &str,
) -> Result<()> {
    let _lock = store_lock.map(|m| m.lock().unwrap_or_else(|e| e.into_inner()));
    mark_task_failed(store, work_item_id, attempt_id, task_id)
}

fn lock_mark_task_failed_attempt_needs_user(
    store: &WorkModelStore,
    store_lock: Option<&std::sync::Mutex<()>>,
    project_root: &Path,
    work_item_id: &str,
    attempt_id: &str,
    task_id: &str,
    auth_message: Option<&str>,
    notify_fn: &dyn Fn(&str, &str),
) -> Result<()> {
    let _lock = store_lock.map(|m| m.lock().unwrap_or_else(|e| e.into_inner()));
    mark_task_failed_attempt_needs_user(
        store,
        project_root,
        work_item_id,
        attempt_id,
        task_id,
        auth_message,
        notify_fn,
    )
}

fn resolve_workspace_path(project_root: &Path, path: &str) -> PathBuf {
    let path = PathBuf::from(path);
    if path.is_absolute() {
        path
    } else {
        project_root.join(path)
    }
}

fn resolve_managed_workspace_path(
    project_root: &Path,
    path: &str,
    work_item_id: &str,
    attempt_id: &str,
) -> Result<PathBuf> {
    resolve_managed_task_workspace_path(project_root, path, work_item_id, attempt_id, "writable")
}

fn resolve_managed_readable_workspace_path(
    project_root: &Path,
    path: &str,
    work_item_id: &str,
    attempt_id: &str,
) -> Result<PathBuf> {
    resolve_managed_task_workspace_path(project_root, path, work_item_id, attempt_id, "readable")
}

fn resolve_review_readable_workspace_path(
    project_root: &Path,
    workspace: &WorkspaceRef,
    work_item_id: &str,
    attempt_id: &str,
    attempt_kind: &AttemptKind,
) -> Result<PathBuf> {
    if attempt_kind.is_source_checkout_review() && workspace.id == "source" && workspace.path == "."
    {
        return Ok(project_root.to_path_buf());
    }
    if attempt_kind.is_review_only_like()
        && workspace.id == "source"
        && crate::review_only_worktree::is_review_only_worktree_workspace_path(&workspace.path)
    {
        return Ok(resolve_managed_sibling_workspace_path(
            project_root,
            &workspace.path,
            "Review-only worktree",
        )?);
    }

    resolve_managed_readable_workspace_path(project_root, &workspace.path, work_item_id, attempt_id)
}

fn resolve_managed_task_workspace_path(
    project_root: &Path,
    path: &str,
    work_item_id: &str,
    attempt_id: &str,
    access_kind: &str,
) -> Result<PathBuf> {
    Ok(resolve_expected_candidate_workspace_path(
        project_root,
        path,
        work_item_id,
        attempt_id,
        match access_kind {
            "writable" => "Task writable",
            "readable" => "Task readable",
            _ => "Task",
        },
    )?)
}

pub(crate) fn resolve_managed_artifact_area_path(
    project_root: &Path,
    path: &str,
) -> Result<PathBuf> {
    let relative_path = Path::new(path);
    if relative_path.is_absolute() {
        bail!("Task artifact area path must be relative: {path}");
    }

    let mut components = Vec::new();
    for component in relative_path.components() {
        match component {
            Component::Normal(part) => components.push(part.to_owned()),
            _ => bail!("Task artifact area path must stay under .fluent/work/artifacts: {path}"),
        }
    }

    let managed_prefix = [
        std::ffi::OsStr::new(".fluent"),
        std::ffi::OsStr::new("work"),
        std::ffi::OsStr::new("artifacts"),
    ];
    if components.len() <= managed_prefix.len()
        || !components
            .iter()
            .zip(managed_prefix.iter())
            .all(|(actual, expected)| actual == expected)
    {
        bail!("Task artifact area path must stay under .fluent/work/artifacts: {path}");
    }

    Ok(resolve_workspace_path(project_root, path))
}

fn resolve_input_artifact_path(project_root: &Path, path: &str) -> Result<PathBuf> {
    let relative_path = Path::new(path);
    if relative_path.is_absolute() {
        bail!("Input artifact path must be relative: {path}");
    }
    let mut components = Vec::new();
    for component in relative_path.components() {
        match component {
            Component::Normal(part) => components.push(part.to_owned()),
            _ => bail!(
                "Input artifact path must stay under .fluent/work/artifacts/ or .fluent/work/progress/: {path}"
            ),
        }
    }
    let valid = components.len() >= 3
        && components[0] == std::ffi::OsStr::new(".fluent")
        && components[1] == std::ffi::OsStr::new("work")
        && (components[2] == std::ffi::OsStr::new("artifacts")
            || components[2] == std::ffi::OsStr::new("progress"));
    if !valid {
        bail!(
            "Input artifact path must be under .fluent/work/artifacts/ or .fluent/work/progress/: {path}"
        );
    }
    Ok(resolve_workspace_path(project_root, path))
}

fn resolve_input_artifact_paths(
    project_root: &Path,
    input_artifacts: &[ArtifactRef],
) -> Result<Vec<PathBuf>> {
    let mut resolved = Vec::new();
    for artifact in input_artifacts {
        let path = resolve_input_artifact_path(project_root, &artifact.path)?;
        if !path.is_file() {
            // progress.md is lazily created by the writer; reviewers may
            // receive it as an input artifact before the writer has
            // initialized it. Skip missing progress.md entries rather
            // than failing the Task.
            if artifact.producer_id == "writer" && artifact.path.ends_with("/progress.md") {
                continue;
            }
            bail!(
                "Input artifact from Task {} does not exist or is not a file: {}",
                artifact.producer_id,
                path.display()
            );
        }
        resolved.push(path);
    }
    Ok(resolved)
}

fn prepare_task_worktree(
    project_root: &Path,
    workspace_path: &Path,
    branch_name: &str,
    source_ref: &str,
) -> Result<()> {
    if workspace_path.exists() {
        if !workspace_path.is_dir() {
            bail!(
                "Workspace path exists but is not a directory: {}",
                workspace_path.display()
            );
        }
        if !workspace_path.join(".git").exists() {
            bail!(
                "Workspace {} exists but is not a registered git worktree",
                workspace_path.display()
            );
        }
        ensure_same_git_repository(project_root, workspace_path)?;
        ensure_registered_worktree(project_root, workspace_path)?;
        return Ok(());
    }

    if let Some(parent) = workspace_path.parent() {
        fs::create_dir_all(parent)?;
    }

    if git_branch_exists(project_root, branch_name)? {
        bail!(
            "Task branch {branch_name:?} already exists but workspace {} is missing; remove or rebind the branch before running the Task",
            workspace_path.display()
        );
    }

    let ws = workspace_path.to_string_lossy();
    git::run(
        project_root,
        &["worktree", "add", "-b", branch_name, &ws, source_ref],
        "create task worktree",
    )
}

fn git_branch_exists(project_root: &Path, branch_name: &str) -> Result<bool> {
    let refspec = format!("refs/heads/{branch_name}");
    let output = git::run_raw(project_root, &["show-ref", "--verify", "--quiet", &refspec])?;
    match output.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => bail!(
            "Failed to check task branch {branch_name:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    }
}

fn ensure_same_git_repository(project_root: &Path, workspace_path: &Path) -> Result<()> {
    let source_common = fs::canonicalize(worktree::git_common_dir(project_root)?)?;
    let workspace_common = fs::canonicalize(worktree::git_common_dir(workspace_path)?)?;
    if source_common != workspace_common {
        bail!(
            "Workspace {} belongs to a different git repository",
            workspace_path.display()
        );
    }
    Ok(())
}

fn ensure_registered_worktree(project_root: &Path, workspace_path: &Path) -> Result<()> {
    let expected = fs::canonicalize(workspace_path)?;
    let output = git::run_raw(project_root, &["worktree", "list", "--porcelain"])?;
    if !output.status.success() {
        bail!(
            "Failed to list git worktrees: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let Some(path) = line.strip_prefix("worktree ") else {
            continue;
        };
        if fs::canonicalize(path).is_ok_and(|actual| actual == expected) {
            return Ok(());
        }
    }

    bail!(
        "Workspace {} exists but is not a registered git worktree",
        workspace_path.display()
    )
}

fn capture_coder_info(coder_kind: CoderKind, model: &str, artifact_dir: &Path) {
    let binary = coder_kind.as_str();
    let version = std::process::Command::new(binary)
        .arg("--version")
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                String::from_utf8(o.stdout)
                    .ok()
                    .map(|s| s.trim().to_string())
            } else {
                None
            }
        })
        .unwrap_or_else(|| "unknown".to_string());

    let info = serde_json::json!({
        "coder": binary,
        "version": version,
        "model": model,
        "captured_at": chrono::Utc::now().to_rfc3339(),
    });

    if let Err(e) = fs::create_dir_all(artifact_dir) {
        eprintln!("warning: cannot create artifact dir for coder-info.json: {e}");
        return;
    }
    let path = artifact_dir.join("coder-info.json");
    if let Err(e) = crate::atomic_write::atomic_write(
        &path,
        serde_json::to_string_pretty(&info)
            .unwrap_or_default()
            .as_bytes(),
    ) {
        eprintln!("warning: cannot write coder-info.json: {e}");
    }
}

fn run_task_coder(
    item: &WorkItem,
    attempt_id: &str,
    task_id: &str,
    project_root: &Path,
    workspace_path: &Path,
    prior_reviews: &[PathBuf],
    resolver: &ContentResolver,
    extra_args: &[String],
    coder_kind: CoderKind,
    no_sandbox: bool,
    model: Option<&str>,
    effort: Option<&str>,
) -> Result<()> {
    if !no_sandbox {
        os::check_prerequisites_for(coder_kind)?;
        credential::inject_credentials()?;
        credential::setup_git_signing();
    }

    let task = item
        .attempts
        .iter()
        .find(|attempt| attempt.id == attempt_id)
        .and_then(|attempt| attempt.tasks.iter().find(|task| task.id == task_id))
        .ok_or_else(|| anyhow::anyhow!("Task {task_id:?} not found"))?;
    materialize_planning_files(item, project_root)?;
    materialize_general_expertise(project_root)?;

    if should_seed_project_model(task.kind, workspace_path) {
        if let Err(err) = run_seed_project_model(
            workspace_path,
            resolver,
            extra_args,
            coder_kind,
            no_sandbox,
            model,
            effort,
        ) {
            eprintln!(
                "  Warning: seed project model failed, continuing without project expertise: {err}"
            );
        }
    }

    let progress_dir = progress_md_dir(project_root, &item.id, attempt_id);
    fs::create_dir_all(&progress_dir).with_context(|| {
        format!(
            "Failed to create progress dir at {}",
            progress_dir.display()
        )
    })?;
    let prompt = build_write_task_prompt_with_workspace(
        item,
        attempt_id,
        task_id,
        prior_reviews,
        Some(workspace_path),
        Some(project_root),
    );

    let transcript_path = task
        .artifact_area
        .as_ref()
        .map(|a| project_root.join(&a.path).join("transcript.jsonl"));
    if let Some(parent) = transcript_path.as_ref().and_then(|p| p.parent()) {
        fs::create_dir_all(parent).context("Failed to create writer transcript artifact dir")?;
    }

    let workspace_resolver = ContentResolver::new(Some(workspace_path));
    let system_prompt = workspace_resolver
        .resolve_content("prompts/write-system.md")
        .unwrap_or_default();
    let (sandbox, _sandbox_profile) = if no_sandbox {
        (CoderSandbox::None, None)
    } else {
        let common_git_dir = worktree::git_common_dir(workspace_path)?;
        let mut readable_roots = input_artifact_readable_roots(prior_reviews);
        readable_roots.push(planning_files_dir(project_root, &item.id));
        readable_roots.push(general_expertise_dir(project_root));
        let mut additional_writable = vec![common_git_dir, progress_dir.clone()];
        if let Some(ref tp) = transcript_path {
            if let Some(artifact_dir) = tp.parent() {
                additional_writable.push(artifact_dir.to_path_buf());
            }
        }
        build_coder_sandbox_with_writable_and_read_only_roots(
            coder_kind,
            resolver,
            workspace_path,
            &additional_writable,
            &readable_roots,
        )?
    };

    let effective_model = model
        .map(str::to_string)
        .unwrap_or_else(|| coder_kind.default_model());

    eprintln!("  Fluent           work task run");
    eprintln!("  Work Item         {}", item.id);
    eprintln!("  Attempt           {attempt_id}");
    eprintln!("  Task              {task_id}");
    eprintln!("  Coder             {}", coder_kind.as_str());
    eprintln!("  Model             {effective_model}");
    if let Some(e) = effort {
        eprintln!("  Effort            {e}");
    }
    eprintln!("  Worktree          {}", workspace_path.display());

    if let Some(ref tp) = transcript_path {
        if let Some(artifact_dir) = tp.parent() {
            capture_coder_info(coder_kind, &effective_model, artifact_dir);
        }
    }

    let coder = coder_kind.boxed_with_model(sandbox, model, effort);
    let exit_code = coder.run(
        &prompt,
        &system_prompt,
        workspace_path,
        extra_args,
        &[],
        transcript_path.as_deref(),
    )?;
    if let Some(tp) = &transcript_path {
        crate::usage::log_usage_from_transcript(
            tp,
            coder_kind.as_str(),
            &item.id,
            attempt_id,
            task_id,
        );
    }
    if exit_code == 0 {
        Ok(())
    } else {
        bail!("Coder exited with code {exit_code}")
    }
}

/// The immutable execution context a derived corrective Work Item renders into
/// Writer and reviewer prompts. Returns `None` for ordinary Work, which carries
/// planning artifacts instead.
fn corrective_execution_context(item: &WorkItem) -> Option<String> {
    item.corrective_context
        .as_ref()
        .and_then(|_| item.write_task_instructions())
}

#[cfg(test)]
fn build_write_task_prompt(
    item: &WorkItem,
    attempt_id: &str,
    task_id: &str,
    prior_reviews: &[PathBuf],
) -> String {
    build_write_task_prompt_with_workspace(item, attempt_id, task_id, prior_reviews, None, None)
}

fn build_write_task_prompt_with_workspace(
    item: &WorkItem,
    attempt_id: &str,
    task_id: &str,
    prior_reviews: &[PathBuf],
    workspace_path: Option<&Path>,
    project_root: Option<&Path>,
) -> String {
    let task = item
        .attempts
        .iter()
        .find(|a| a.id == attempt_id)
        .and_then(|a| a.tasks.iter().find(|t| t.id == task_id))
        .expect("Task must exist");
    let task_json = to_json_pretty(task).unwrap_or_default();
    let prior_reviews_list = prior_reviews_block(prior_reviews, "   ");
    let progress_md_pathbuf = project_root.and_then(|root| {
        item.attempts
            .iter()
            .find(|a| a.id == attempt_id)
            .map(|a| progress_md_path_for(root, &item.id, &a.id))
    });
    let progress_md_path = progress_md_pathbuf
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let has_progress_md = progress_md_pathbuf
        .as_ref()
        .map(|p| p.exists())
        .unwrap_or(false);
    let has_prior_reviews = !prior_reviews.is_empty();
    let planning = project_root
        .map(|root| compute_planning_paths(item, root))
        .unwrap_or_default();
    let brief_path = planning.brief();
    let behaviors_path = planning.behaviors();
    let approach_path = planning.approach();
    let plan_path = planning.plan();
    let general_expertise_index = project_root
        .map(|root| {
            general_expertise_dir(root)
                .join("INDEX.md")
                .display()
                .to_string()
        })
        .unwrap_or_default();
    let project_expertise_index_pathbuf =
        workspace_path.map(|ws| ws.join(".fluent/expertise/INDEX.md"));
    let project_expertise_index = project_expertise_index_pathbuf
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let has_project_expertise_index = project_expertise_index_pathbuf
        .as_ref()
        .map(|p| p.is_file())
        .unwrap_or(false);
    let (bootstrap_yaml, bootstrap_extract) = tester_bootstrap_flags(workspace_path);
    let bootstrap_yaml_value = if bootstrap_yaml { "yes" } else { "" };
    let bootstrap_extract_value = if bootstrap_extract { "yes" } else { "" };
    let bootstrap_anything_value = if bootstrap_yaml || bootstrap_extract {
        "yes"
    } else {
        ""
    };
    let has_prior_reviews_value = if has_prior_reviews { "yes" } else { "" };
    let has_progress_md_value = if has_progress_md { "yes" } else { "" };
    let has_project_expertise_index_value = if has_project_expertise_index {
        "yes"
    } else {
        ""
    };
    let corrective_context = corrective_execution_context(item).unwrap_or_default();
    let is_corrective_value = if corrective_context.is_empty() {
        ""
    } else {
        "yes"
    };

    let template = ContentResolver::new(workspace_path)
        .resolve_content("prompts/write-user.md")
        .expect("bundled write-user.md must resolve");
    crate::content::render_template(
        &template,
        &[
            ("work_item_id", &item.id),
            ("work_item_title", &item.title),
            ("attempt_id", attempt_id),
            ("task_id", task_id),
            ("role", &task.role),
            ("brief_path", &brief_path),
            ("behaviors_path", &behaviors_path),
            ("approach_path", &approach_path),
            ("plan_path", &plan_path),
            ("prior_reviews_list", &prior_reviews_list),
            ("progress_md_path", &progress_md_path),
            ("task_json", &task_json),
            ("is_corrective", is_corrective_value),
            ("corrective_context", &corrective_context),
            ("bootstrap_anything", bootstrap_anything_value),
            ("bootstrap_tester_yaml", bootstrap_yaml_value),
            ("bootstrap_extract_script", bootstrap_extract_value),
            ("has_prior_reviews", has_prior_reviews_value),
            ("has_progress_md", has_progress_md_value),
            ("general_expertise_index", &general_expertise_index),
            ("project_expertise_index", &project_expertise_index),
            (
                "has_project_expertise_index",
                has_project_expertise_index_value,
            ),
        ],
    )
    .expect("write-user.md template must render with the documented context")
}

#[derive(Default, Debug, Clone)]
struct PlanningFilePaths {
    brief: Option<PathBuf>,
    behaviors: Option<PathBuf>,
    approach: Option<PathBuf>,
    plan: Option<PathBuf>,
}

impl PlanningFilePaths {
    fn brief(&self) -> String {
        self.brief
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_default()
    }
    fn behaviors(&self) -> String {
        self.behaviors
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_default()
    }
    fn approach(&self) -> String {
        self.approach
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_default()
    }
    fn plan(&self) -> String {
        self.plan
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_default()
    }
}

fn planning_files_dir(project_root: &Path, work_item_id: &str) -> PathBuf {
    project_root.join(".fluent/work/items").join(work_item_id)
}

fn general_expertise_dir(project_root: &Path) -> PathBuf {
    project_root.join(".fluent/work/expertise")
}

/// Materialize the bundled general-expertise files at
/// `<project_root>/.fluent/work/expertise/<name>`. Writes are atomic
/// (write-to-temp + rename) so concurrent calls from parallel writer/reviewer
/// Tasks cannot tear a file.
fn materialize_general_expertise(project_root: &Path) -> Result<PathBuf> {
    let dir = general_expertise_dir(project_root);
    fs::create_dir_all(&dir).with_context(|| {
        format!(
            "Failed to create general expertise dir at {}",
            dir.display()
        )
    })?;
    for name in crate::content::GENERAL_EXPERTISE_FILES {
        let relative = format!("expertise/{name}");
        let content = crate::content::bundled_content(&relative)
            .ok_or_else(|| anyhow::anyhow!("missing bundled expertise file: {relative}"))?;
        let final_path = dir.join(name);
        crate::atomic_write::atomic_write(&final_path, content.as_bytes())
            .with_context(|| format!("Failed to write expertise at {}", final_path.display()))?;
    }
    Ok(dir)
}

/// Materialize a single bundled skill into `dest_dir/<skill_name>/`.
/// Writes all files carried in the binary for the named skill, with
/// references dereferenced (symlinks followed at build time). Returns
/// the path to the skill directory.
///
/// Fails if a referenced expertise file is absent from the bundle.
pub fn materialize_skill(skill_name: &str, dest_dir: &Path) -> Result<PathBuf> {
    let prefix = format!("{skill_name}/");
    let files = crate::content::bundled_skill_files_under(&prefix);
    if files.is_empty() {
        bail!("No bundled skill named {skill_name:?}");
    }

    let skill_dir = dest_dir.join(skill_name);
    for (rel_path, content) in &files {
        let file_path = dest_dir.join(rel_path);
        if let Some(parent) = file_path.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!(
                    "Failed to create directory for skill file {}",
                    file_path.display()
                )
            })?;
        }
        crate::atomic_write::atomic_write(&file_path, content.as_bytes())
            .with_context(|| format!("Failed to write skill at {}", file_path.display()))?;
    }
    Ok(skill_dir)
}

fn should_seed_project_model(task_kind: TaskKind, workspace_path: &Path) -> bool {
    task_kind == TaskKind::Write && !workspace_path.join(".fluent/expertise/INDEX.md").is_file()
}

fn run_seed_project_model(
    workspace_path: &Path,
    resolver: &ContentResolver,
    extra_args: &[String],
    coder_kind: CoderKind,
    no_sandbox: bool,
    model: Option<&str>,
    effort: Option<&str>,
) -> Result<()> {
    eprintln!("  Seeding project expertise model…");

    let workspace_resolver = ContentResolver::new(Some(workspace_path));
    let system_prompt = workspace_resolver
        .resolve_content("prompts/seed-system.md")
        .unwrap_or_default();

    let expertise_dir = workspace_path.join(".fluent/expertise");
    let index_path = expertise_dir.join("INDEX.md");
    let overview_path = expertise_dir.join("overview.md");

    let template = resolver
        .resolve_content("prompts/seed-user.md")
        .ok_or_else(|| anyhow::anyhow!("bundled seed-user.md must resolve"))?;
    let prompt = crate::content::render_template(
        &template,
        &[
            ("index_path", &index_path.display().to_string()),
            ("overview_path", &overview_path.display().to_string()),
            ("workspace_path", &workspace_path.display().to_string()),
        ],
    )
    .map_err(|e| anyhow::anyhow!("seed-user.md template error: {e}"))?;

    let (sandbox, _sandbox_profile) = if no_sandbox {
        (CoderSandbox::None, None)
    } else {
        let common_git_dir = worktree::git_common_dir(workspace_path)?;
        build_coder_sandbox_with_writable_and_read_only_roots(
            coder_kind,
            resolver,
            workspace_path,
            &[common_git_dir],
            &[],
        )?
    };

    let coder = coder_kind.boxed_with_model(sandbox, model, effort);
    let exit_code = coder.run(
        &prompt,
        &system_prompt,
        workspace_path,
        extra_args,
        &[],
        None,
    )?;
    if exit_code != 0 {
        bail!("Seed coder exited with code {exit_code}");
    }

    if !index_path.is_file() {
        bail!("Seed coder did not produce {}", index_path.display());
    }

    Ok(())
}

/// Inputs the Learner coder needs to refine expertise and write its handoff
/// draft.
pub struct LearnerRunInputs<'a> {
    /// The candidate worktree the Attempt produced.
    pub workspace_path: &'a Path,
    pub resolver: &'a ContentResolver,
    pub extra_args: &'a [String],
    pub coder_kind: CoderKind,
    pub no_sandbox: bool,
    pub model: Option<&'a str>,
    pub effort: Option<&'a str>,
    /// Reviewer `review.md` artifacts from every review round.
    pub review_artifact_paths: &'a [PathBuf],
    /// Tester `tester-results.json` artifacts from every review round.
    pub tester_artifact_paths: &'a [PathBuf],
    /// Command that renders the complete Attempt change.
    pub diff_command: &'a str,
    /// The managed handoff surface where the coder writes its untrusted draft.
    pub handoff_dir: &'a Path,
    /// Live repository roots that a handoff-only retry must never write.
    pub denied_write_roots: &'a [PathBuf],
    /// Whether the Learner runs post-land in handoff-only mode: after its
    /// originating Merge Candidate has merged, it may not mutate expertise or the
    /// merged branch, so it only produces a handoff.
    pub handoff_only: bool,
}

/// Whether the Learner runs in handoff-only mode. A post-land retry — one whose
/// originating Merge Candidate has already merged — is handoff-only, so it must
/// not mutate expertise or the merged branch.
pub fn learner_is_handoff_only(candidate_merged: bool) -> bool {
    candidate_merged
}

/// Whether the Learner may write `.fluent/expertise/` on this run. A handoff-only
/// post-land retry may not, so expertise stays read-only and only the managed
/// handoff surface is writable.
pub fn learner_expertise_writable(handoff_only: bool) -> bool {
    !handoff_only
}

/// Encode durable project knowledge a post-land handoff-only retry could not
/// write to expertise as a non-corrective follow-up, so it materializes as an
/// Observation only and can never become autonomously executable Work.
pub fn expertise_proposal_follow_up(
    id: impl Into<String>,
    summary: impl Into<String>,
) -> crate::follow_up::FollowUpDraftV1 {
    crate::follow_up::FollowUpDraftV1 {
        id: id.into(),
        summary: summary.into(),
        corrective: false,
        ..Default::default()
    }
}

/// Run the Learner: give the coder the complete change and every reviewer and
/// tester artifact, instruct it with the shared corrective criteria, and let it
/// refine durable expertise plus write one untrusted follow-up draft.
///
/// The coder is sandboxed to write only `.fluent/expertise/`, the designated
/// managed handoff surface, and the Git metadata an expertise commit needs — not
/// the Observation backlog, the Work model, or the rest of the workspace.
pub fn run_learner(inputs: LearnerRunInputs<'_>) -> Result<()> {
    eprintln!("  Running the Learner after passing reviews…");

    let workspace_path = inputs.workspace_path;
    let workspace_resolver = ContentResolver::new(Some(workspace_path));
    let system_prompt = workspace_resolver
        .resolve_content("prompts/learner-system.md")
        .unwrap_or_default();

    let learnings_dir = workspace_path.join(".fluent/expertise/learnings");
    let learnings_index_path = learnings_dir.join("INDEX.md");
    let expertise_index_path = workspace_path.join(".fluent/expertise/INDEX.md");

    let review_paths_rendered = render_path_list(inputs.review_artifact_paths);
    let tester_paths_rendered = render_path_list(inputs.tester_artifact_paths);

    let has_learnings_index = if learnings_index_path.is_file() {
        "yes"
    } else {
        ""
    };

    let draft_path = inputs.handoff_dir.join(crate::learner::DRAFT_FILE_NAME);

    let template = inputs
        .resolver
        .resolve_content("prompts/learner-user.md")
        .ok_or_else(|| anyhow::anyhow!("bundled learner-user.md must resolve"))?;
    let prompt = crate::content::render_template(
        &template,
        &[
            ("review_artifact_paths", &review_paths_rendered),
            ("tester_artifact_paths", &tester_paths_rendered),
            ("diff_command", inputs.diff_command),
            ("learnings_dir", &learnings_dir.display().to_string()),
            (
                "learnings_index_path",
                &learnings_index_path.display().to_string(),
            ),
            (
                "expertise_index_path",
                &expertise_index_path.display().to_string(),
            ),
            ("has_learnings_index", has_learnings_index),
            ("draft_path", &draft_path.display().to_string()),
            (
                "handoff_only",
                if inputs.handoff_only { "yes" } else { "" },
            ),
        ],
    )
    .map_err(|e| anyhow::anyhow!("learner-user.md template error: {e}"))?;

    let mut readable_roots: Vec<PathBuf> = inputs
        .review_artifact_paths
        .iter()
        .chain(inputs.tester_artifact_paths.iter())
        .filter_map(|p| p.parent().map(|parent| parent.to_path_buf()))
        .collect();
    readable_roots.sort();
    readable_roots.dedup();

    let expertise_dir = workspace_path.join(".fluent/expertise");
    fs::create_dir_all(&expertise_dir)?;
    fs::create_dir_all(inputs.handoff_dir)?;

    // A post-land retry handles persisted merged state, so `--no-sandbox`
    // cannot weaken its boundary. It always uses the trusted system Seatbelt
    // launcher and fails closed when the host cannot apply that profile.
    let (sandbox, _sandbox_profile) = if inputs.no_sandbox && !inputs.handoff_only {
        (CoderSandbox::None, None)
    } else {
        let common_git_dir = worktree::git_common_dir(workspace_path)?;
        readable_roots.push(workspace_path.to_path_buf());
        if learner_expertise_writable(inputs.handoff_only) {
            build_coder_sandbox_with_writable_and_read_only_roots(
                inputs.coder_kind,
                inputs.resolver,
                &expertise_dir,
                &[inputs.handoff_dir.to_path_buf(), common_git_dir],
                &readable_roots,
            )?
        } else {
            // Handoff-only: deny expertise writes. Expertise stays readable, but
            // only the managed handoff surface is writable. Git metadata is
            // readable for the accepted-change diff but never writable.
            readable_roots.push(expertise_dir.clone());
            readable_roots.push(common_git_dir.clone());
            let home = std::env::var("HOME").unwrap_or_default();
            let mut denied = vec![workspace_path.to_path_buf(), common_git_dir];
            denied.extend(inputs.denied_write_roots.iter().cloned());
            denied.sort();
            denied.dedup();
            let profile = os::render_profile_for_access_for_coder_with_denied_writes(
                inputs.resolver,
                &home,
                &[inputs.handoff_dir.to_path_buf()],
                &readable_roots,
                &denied,
                inputs.coder_kind,
            )?;
            let sandbox = CoderSandbox::TrustedSeatbeltProfile(
                profile.path.to_string_lossy().to_string(),
            );
            (sandbox, Some(profile))
        }
    };

    let coder = inputs
        .coder_kind
        .boxed_with_model(sandbox, inputs.model, inputs.effort);
    let exit_code = coder.run(
        &prompt,
        &system_prompt,
        workspace_path,
        inputs.extra_args,
        &[],
        None,
    )?;
    if exit_code != 0 {
        bail!("Learner coder exited with code {exit_code}");
    }

    Ok(())
}

fn render_path_list(paths: &[PathBuf]) -> String {
    if paths.is_empty() {
        return "- (none)".to_string();
    }
    paths
        .iter()
        .map(|p| format!("- {}", p.display()))
        .collect::<Vec<_>>()
        .join("\n")
}

fn progress_md_dir(project_root: &Path, work_item_id: &str, attempt_id: &str) -> PathBuf {
    project_root
        .join(crate::work_model::WORK_PROGRESS_DIR)
        .join(work_item_id)
        .join(attempt_id)
}

fn progress_md_path_for(project_root: &Path, work_item_id: &str, attempt_id: &str) -> PathBuf {
    progress_md_dir(project_root, work_item_id, attempt_id).join("progress.md")
}

/// Compute the absolute paths the writer/reviewer prompts reference for each
/// planning section. Returns `None` for sections with empty content.
fn compute_planning_paths(item: &WorkItem, project_root: &Path) -> PlanningFilePaths {
    let Some(ctx) = item.planning_context.as_ref() else {
        return PlanningFilePaths::default();
    };
    let dir = planning_files_dir(project_root, &item.id);
    PlanningFilePaths {
        brief: section_path(&dir, "brief.md", &ctx.brief),
        behaviors: section_path(&dir, "behaviors.md", &ctx.behaviors),
        approach: section_path(&dir, "approach.md", &ctx.approach),
        plan: section_path(&dir, "plan.md", &ctx.plan),
    }
}

fn section_path(dir: &Path, name: &str, content: &Option<String>) -> Option<PathBuf> {
    content
        .as_ref()
        .filter(|s| !s.trim().is_empty())
        .map(|_| dir.join(name))
}

/// Materialize planning sections as files at
/// `<project_root>/.fluent/work/items/<work-item-id>/<section>.md`. Writes are
/// atomic (write-to-temp + rename) so concurrent calls from parallel review
/// Tasks cannot tear a file. Returns the same paths as `compute_planning_paths`.
fn materialize_planning_files(item: &WorkItem, project_root: &Path) -> Result<PlanningFilePaths> {
    let Some(ctx) = item.planning_context.as_ref() else {
        return Ok(PlanningFilePaths::default());
    };
    let dir = planning_files_dir(project_root, &item.id);
    fs::create_dir_all(&dir)
        .with_context(|| format!("Failed to create planning files dir at {}", dir.display()))?;

    let brief = write_section_atomically(&dir, "brief.md", &ctx.brief)?;
    let behaviors = write_section_atomically(&dir, "behaviors.md", &ctx.behaviors)?;
    let approach = write_section_atomically(&dir, "approach.md", &ctx.approach)?;
    let plan = write_section_atomically(&dir, "plan.md", &ctx.plan)?;

    Ok(PlanningFilePaths {
        brief,
        behaviors,
        approach,
        plan,
    })
}

fn write_section_atomically(
    dir: &Path,
    name: &str,
    content: &Option<String>,
) -> Result<Option<PathBuf>> {
    let Some(text) = content.as_deref().filter(|s| !s.trim().is_empty()) else {
        return Ok(None);
    };
    let final_path = dir.join(name);
    crate::atomic_write::atomic_write(&final_path, text.as_bytes()).with_context(|| {
        format!(
            "Failed to write planning section at {}",
            final_path.display()
        )
    })?;
    Ok(Some(final_path))
}

fn tester_bootstrap_flags(workspace_path: Option<&Path>) -> (bool, bool) {
    let Some(workspace) = workspace_path else {
        return (false, false);
    };
    let yaml_missing = !workspace.join(".fluent/tester.yaml").exists();
    let extract_missing = !workspace.join(".fluent/extract-tester-results").exists();
    (yaml_missing, extract_missing)
}

fn prior_reviews_block(prior_reviews: &[PathBuf], indent: &str) -> String {
    if prior_reviews.is_empty() {
        return String::new();
    }

    prior_reviews
        .iter()
        .map(|path| format!("{indent}- {}", path.display()))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Filter resolved input artifacts to just the prior `review.md` files
/// (excluding tester-results.json, progress.md, and any other non-review
/// artifacts a reviewer Task may receive).
fn prior_reviews_only(input_artifacts: &[PathBuf]) -> Vec<PathBuf> {
    input_artifacts
        .iter()
        .filter(|path| {
            path.file_name()
                .map(|name| name == "review.md")
                .unwrap_or(false)
        })
        .cloned()
        .collect()
}

fn input_artifact_readable_roots(input_artifacts: &[PathBuf]) -> Vec<PathBuf> {
    input_artifacts
        .iter()
        .filter_map(|path| path.parent().map(Path::to_path_buf))
        .collect()
}

fn run_review_coder(
    item: &WorkItem,
    attempt_id: &str,
    task_id: &str,
    project_root: &Path,
    artifact_dir: &Path,
    review_path: &Path,
    readable_workspaces: &[PathBuf],
    input_artifacts: &[PathBuf],
    review_only: bool,
    resolver: &ContentResolver,
    extra_args: &[String],
    coder_kind: CoderKind,
    no_sandbox: bool,
    model: Option<&str>,
    effort: Option<&str>,
) -> Result<()> {
    if !no_sandbox {
        os::check_prerequisites_for(coder_kind)?;
        credential::inject_credentials()?;
        credential::setup_git_signing();
    }

    // Planning artifacts and bundled expertise were materialized earlier (in
    // run_review_task, before the source-checkout review guard snapshotted
    // the workspace). Build prompts here only; a missing review-<role> skill
    // will surface as a Result error.
    let prompts = build_work_review_prompts(WorkReviewPromptInput {
        item,
        attempt_id,
        task_id,
        project_root,
        artifact_dir,
        review_path,
        readable_workspaces,
        input_artifacts,
        review_only,
    })?;

    let (sandbox, _sandbox_profile) = if no_sandbox {
        (CoderSandbox::None, None)
    } else {
        let mut readable_roots = review_readable_sandbox_roots(readable_workspaces)?;
        readable_roots.extend(input_artifact_readable_roots(input_artifacts));
        readable_roots.push(planning_files_dir(project_root, &item.id));
        readable_roots.push(general_expertise_dir(project_root));
        readable_roots.push(review_skills_dir(project_root));
        build_coder_sandbox_with_read_only_roots(
            coder_kind,
            resolver,
            artifact_dir,
            &readable_roots,
        )?
    };

    let effective_model = model
        .map(str::to_string)
        .unwrap_or_else(|| coder_kind.default_model());

    eprintln!("  Fluent           work task run");
    eprintln!("  Work Item         {}", item.id);
    eprintln!("  Attempt           {attempt_id}");
    eprintln!("  Task              {task_id}");
    eprintln!("  Coder             {}", coder_kind.as_str());
    eprintln!("  Model             {effective_model}");
    eprintln!("  Artifact area     {}", artifact_dir.display());

    capture_coder_info(coder_kind, &effective_model, artifact_dir);

    let transcript_path = artifact_dir.join("transcript.jsonl");
    let coder = coder_kind.boxed_with_model(sandbox, model, effort);
    let exit_code = coder.run(
        &prompts.review_prompt,
        &prompts.system_prompt,
        artifact_dir,
        extra_args,
        &[],
        Some(&transcript_path),
    )?;
    crate::usage::log_usage_from_transcript(
        &transcript_path,
        coder_kind.as_str(),
        &item.id,
        attempt_id,
        task_id,
    );
    if exit_code == 0 {
        Ok(())
    } else {
        bail!("Coder exited with code {exit_code}")
    }
}

struct WorkReviewPrompts {
    system_prompt: String,
    review_prompt: String,
}

struct WorkReviewPromptInput<'a> {
    item: &'a WorkItem,
    attempt_id: &'a str,
    task_id: &'a str,
    project_root: &'a Path,
    artifact_dir: &'a Path,
    review_path: &'a Path,
    readable_workspaces: &'a [PathBuf],
    input_artifacts: &'a [PathBuf],
    review_only: bool,
}

fn build_work_review_prompts(input: WorkReviewPromptInput<'_>) -> Result<WorkReviewPrompts> {
    let task = input
        .item
        .attempts
        .iter()
        .find(|attempt| attempt.id == input.attempt_id)
        .and_then(|attempt| attempt.tasks.iter().find(|task| task.id == input.task_id))
        .ok_or_else(|| anyhow::anyhow!("Task {:?} not found", input.task_id))?;
    let review_context = task.review_context.as_ref().ok_or_else(|| {
        anyhow::anyhow!(
            "Review Task {:?} must declare review context",
            input.task_id
        )
    })?;

    let skill_path = review_skill_path(&task.role, input.project_root)?;

    // Decisions: split into decisions_path (or empty).
    let decisions_path = decisions_path(input.readable_workspaces);

    // For write reviews, always include the diff command using source_branch..candidate_commit.
    // For review-only, include one only when review_context.base_commit is set (post-merge review):
    // <base_commit>..HEAD shows the change that landed and triggered this review.
    let candidate_workspace = task
        .workspace_access
        .reads
        .iter()
        .zip(input.readable_workspaces.iter())
        .find(|(workspace, _)| workspace.id == review_context.candidate_workspace_id)
        .map(|(_, resolved_path)| resolved_path.as_path())
        .unwrap_or_else(|| Path::new(&review_context.candidate_workspace_path));
    let review_diff_command = if input.review_only {
        match review_context.base_commit.as_deref() {
            Some(base) if !base.is_empty() => {
                render_review_diff_command(candidate_workspace, &format!("{base}...HEAD"))
            }
            _ => String::new(),
        }
    } else {
        let review_range = format!(
            "{}...{}",
            review_context.source_branch, review_context.candidate_commit
        );
        render_review_diff_command(candidate_workspace, &review_range)
    };

    let reviewer_prior_reviews = prior_reviews_only(input.input_artifacts);
    let reviewer_prior_reviews_list = prior_reviews_block(&reviewer_prior_reviews, "");
    let reviewer_has_prior_reviews = if reviewer_prior_reviews.is_empty() {
        ""
    } else {
        "yes"
    };
    let is_review_tests_value = if task.role == "tests" { "yes" } else { "" };
    let is_review_behaviors_value = if task.role == "behaviors" { "yes" } else { "" };
    let is_review_architecture_value = if task.role == "architecture" {
        "yes"
    } else {
        ""
    };
    let is_review_documentation_value = if task.role == "documentation" {
        "yes"
    } else {
        ""
    };

    let review_path_display = input.review_path.display().to_string();
    let artifact_dir_display = input.artifact_dir.display().to_string();

    let planning = compute_planning_paths(input.item, input.project_root);
    let brief_path = planning.brief();
    let behaviors_path = planning.behaviors();
    let approach_path = planning.approach();
    let plan_path = planning.plan();

    let general_expertise_index = general_expertise_dir(input.project_root)
        .join("INDEX.md")
        .display()
        .to_string();
    let candidate_workspace_pathbuf = Path::new(&review_context.candidate_workspace_path);
    let project_expertise_index_pathbuf =
        candidate_workspace_pathbuf.join(".fluent/expertise/INDEX.md");
    let project_expertise_index = project_expertise_index_pathbuf.display().to_string();
    let has_project_expertise_index_value = if project_expertise_index_pathbuf.is_file() {
        "yes"
    } else {
        ""
    };

    let tester_results_path = task
        .depends_on
        .as_deref()
        .map(|dep_id| {
            input
                .project_root
                .join(".fluent/work/artifacts")
                .join(&input.item.id)
                .join(input.attempt_id)
                .join(dep_id)
                .join("tester-results.json")
                .display()
                .to_string()
        })
        .unwrap_or_default();
    let reviewer_progress_md_path =
        progress_md_path_for(input.project_root, &input.item.id, input.attempt_id)
            .display()
            .to_string();

    let resolver = ContentResolver::new(input.readable_workspaces.first().map(|p| p.as_path()));

    let corrective_context = corrective_execution_context(input.item).unwrap_or_default();
    let is_corrective_value = if corrective_context.is_empty() {
        ""
    } else {
        "yes"
    };

    let user_template_name = if input.review_only {
        "prompts/review-only-user.md"
    } else {
        "prompts/review-user.md"
    };
    let user_template = resolver
        .resolve_content(user_template_name)
        .unwrap_or_else(|| panic!("bundled {user_template_name} must resolve"));
    let review_prompt = crate::content::render_template(
        &user_template,
        &[
            ("work_item_id", &input.item.id),
            ("work_item_title", &input.item.title),
            ("role", &task.role),
            ("is_corrective", is_corrective_value),
            ("corrective_context", &corrective_context),
            ("brief_path", &brief_path),
            ("behaviors_path", &behaviors_path),
            ("approach_path", &approach_path),
            ("plan_path", &plan_path),
            ("general_expertise_index", &general_expertise_index),
            ("project_expertise_index", &project_expertise_index),
            (
                "has_project_expertise_index",
                has_project_expertise_index_value,
            ),
            ("skill_path", &skill_path),
            ("has_prior_reviews", reviewer_has_prior_reviews),
            ("is_review_tests", is_review_tests_value),
            ("is_review_behaviors", is_review_behaviors_value),
            ("is_review_architecture", is_review_architecture_value),
            ("is_review_documentation", is_review_documentation_value),
            ("prior_reviews_list", &reviewer_prior_reviews_list),
            (
                "candidate_workspace_path",
                &review_context.candidate_workspace_path,
            ),
            ("source_branch", &review_context.source_branch),
            ("candidate_commit", &review_context.candidate_commit),
            ("review_diff_command", &review_diff_command),
            ("tester_results_path", &tester_results_path),
            ("progress_md_path", &reviewer_progress_md_path),
            ("review_path", &review_path_display),
            ("artifact_dir", &artifact_dir_display),
            ("decisions_path", &decisions_path),
        ],
    )
    .expect("review-user.md template must render with the documented context");

    let system_template_name = if input.review_only {
        "prompts/review-only-system.md"
    } else {
        "prompts/review-system.md"
    };
    let system_template = resolver
        .resolve_content(system_template_name)
        .unwrap_or_else(|| panic!("bundled {system_template_name} must resolve"));
    let system_prompt = crate::content::render_template(&system_template, &[("role", &task.role)])
        .unwrap_or_else(|err| panic!("{system_template_name} template must render: {err}"));

    Ok(WorkReviewPrompts {
        system_prompt,
        review_prompt,
    })
}

fn review_skills_dir(project_root: &Path) -> PathBuf {
    project_root.join(".fluent/work/skills")
}

pub(crate) fn review_skill_path(role: &str, project_root: &Path) -> Result<String> {
    let skill_name = format!("review-{role}");
    let dest_dir = review_skills_dir(project_root);
    let skill_md = dest_dir.join(&skill_name).join("SKILL.md");
    if skill_md.is_file() {
        return Ok(skill_md.display().to_string());
    }
    match materialize_skill(&skill_name, &dest_dir) {
        Ok(skill_dir) => Ok(skill_dir.join("SKILL.md").display().to_string()),
        Err(e) => Err(anyhow::anyhow!(
            "Required review-{role} skill not found: {e}"
        )),
    }
}

fn decisions_path(readable_workspaces: &[PathBuf]) -> String {
    let relative = Path::new(".fluent/expertise/decisions.md");
    readable_workspaces
        .iter()
        .map(|workspace| workspace.join(relative))
        .find(|path| path.is_file())
        .map(|path| path.display().to_string())
        .unwrap_or_default()
}

fn prepare_reviewer_build_cache(
    candidate_workspace: &Path,
    artifact_dir: &Path,
    work_item_id: &str,
    attempt_id: &str,
    task_id: &str,
) {
    let hook_name = "prepare-pre-review";
    if hooks::find_hook(candidate_workspace, hook_name).is_some() {
        let log_dir = artifact_dir.join("hooks");
        let context = hooks::HookContext {
            work_item_id: Some(work_item_id.to_string()),
            attempt_id: Some(attempt_id.to_string()),
            task_id: Some(task_id.to_string()),
            reviewer_artifact_dir: Some(artifact_dir.to_path_buf()),
            log_dir,
            ..Default::default()
        };
        match hooks::run_hook(
            candidate_workspace,
            hook_name,
            candidate_workspace,
            &context,
        ) {
            Ok(Some(outcome)) if outcome.passed => {
                eprintln!(
                    "  Reviewer prep     {hook_name} hook passed (log: {})",
                    outcome.log_path.display()
                );
            }
            Ok(Some(outcome)) => {
                eprintln!(
                    "  Reviewer prep     {hook_name} hook failed (exit {}, log: {})",
                    outcome.exit_code,
                    outcome.log_path.display()
                );
            }
            Ok(None) => {}
            Err(err) => {
                eprintln!("  Reviewer prep     {hook_name} hook error: {err:#}");
            }
        }
    } else if let Some(toolchain) = prep::detect_toolchain(candidate_workspace) {
        match prep::populate_reviewer_cache(candidate_workspace, artifact_dir, toolchain) {
            Ok(()) => {
                eprintln!(
                    "  Reviewer prep     pre-populated {} build cache from candidate",
                    toolchain.name,
                );
            }
            Err(err) => {
                eprintln!(
                    "  Reviewer prep     failed to copy {} build cache: {err:#}",
                    toolchain.name,
                );
            }
        }
    }
}

fn review_readable_sandbox_roots(readable_workspaces: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut roots = Vec::new();
    for workspace in readable_workspaces {
        roots.push(workspace.clone());
        let common_git_dir = worktree::git_common_dir(workspace)?;
        if !roots.iter().any(|root| root == &common_git_dir) {
            roots.push(common_git_dir);
        }
    }
    Ok(roots)
}

fn build_coder_sandbox_with_writable_and_read_only_roots(
    coder_kind: CoderKind,
    resolver: &ContentResolver,
    working_dir: &Path,
    additional_writable_roots: &[PathBuf],
    readable_roots: &[PathBuf],
) -> Result<(CoderSandbox, Option<os::SandboxProfile>)> {
    let home = std::env::var("HOME").unwrap_or_default();
    let mut roots = vec![working_dir.to_path_buf()];
    roots.extend(additional_writable_roots.iter().cloned());
    let profile = os::render_profile_for_access_for_coder(
        resolver,
        &home,
        &roots,
        readable_roots,
        coder_kind,
    )?;
    let sandbox = CoderSandbox::SeatbeltProfile(profile.path.to_string_lossy().to_string());
    Ok((sandbox, Some(profile)))
}

fn build_coder_sandbox_with_read_only_roots(
    coder_kind: CoderKind,
    resolver: &ContentResolver,
    working_dir: &Path,
    readable_roots: &[PathBuf],
) -> Result<(CoderSandbox, Option<os::SandboxProfile>)> {
    let home = std::env::var("HOME").unwrap_or_default();
    let writable_roots = vec![working_dir.to_path_buf()];
    let profile = os::render_profile_for_access_for_coder(
        resolver,
        &home,
        &writable_roots,
        readable_roots,
        coder_kind,
    )?;
    let sandbox = CoderSandbox::SeatbeltProfile(profile.path.to_string_lossy().to_string());
    Ok((sandbox, Some(profile)))
}

fn ensure_clean_worktree(workspace_path: &Path) -> Result<()> {
    let output = git_status_output(workspace_path, &["--porcelain"])?;
    if !output.status.success() {
        bail!(
            "Failed to read task workspace status: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    if output.stdout.is_empty() {
        Ok(())
    } else {
        bail!(
            "Task workspace has uncommitted changes; commit or remove them before completing:\n{}",
            String::from_utf8_lossy(&output.stdout)
        )
    }
}

fn ensure_no_non_fluent_worktree_changes(workspace_path: &Path) -> Result<()> {
    let output = git_status_output(
        workspace_path,
        &[
            "--porcelain",
            "--untracked-files=all",
            "--",
            ".",
            ":(exclude).fluent",
        ],
    )?;
    if !output.status.success() {
        bail!(
            "Failed to read source checkout status: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    if output.stdout.is_empty() {
        Ok(())
    } else {
        bail!(
            "Review Task changed non-Fluent source files; source checkout must remain read-only:\n{}",
            String::from_utf8_lossy(&output.stdout)
        )
    }
}

fn ensure_head_matches_review_context(workspace_path: &Path, expected_head: &str) -> Result<()> {
    let current_head = head_commit(workspace_path)?;
    if current_head == expected_head {
        Ok(())
    } else {
        bail!(
            "Readable source checkout HEAD {current_head} does not match review context source commit {expected_head}: {}",
            workspace_path.display()
        )
    }
}

fn ensure_source_changed_only_artifact_area(baseline: &SourceCheckoutReviewGuard) -> Result<()> {
    let current_status = worktree_status(&baseline.path)?;
    let allowed = allowed_status_prefix(&baseline.path, &baseline.allowed_artifact_dir)?;
    let baseline_status = filtered_status_entries(&baseline.status, &allowed);
    let current = filtered_status_entries(&current_status, &allowed);
    let current_protected =
        protected_fluent_file_snapshot(&baseline.path, &baseline.allowed_artifact_dir)?;
    if current == baseline_status && current_protected == baseline.protected_fluent_files {
        Ok(())
    } else {
        let status_delta = status_diff(&baseline_status, &current);
        let fluent_delta =
            fluent_file_snapshot_diff(&baseline.protected_fluent_files, &current_protected);
        let mut delta = [status_delta, fluent_delta]
            .into_iter()
            .filter(|section| !section.is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        if delta.is_empty() {
            delta = "(source changed outside artifact area)".to_string();
        }
        bail!(
            "Review Task changed source checkout outside managed artifact area; only {} may change:\n{}",
            allowed.display(),
            delta
        )
    }
}

fn worktree_status(workspace_path: &Path) -> Result<Vec<String>> {
    let output = git_status_output(workspace_path, &["--porcelain", "--untracked-files=all"])?;
    if !output.status.success() {
        bail!(
            "Failed to read source checkout status: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::to_string)
        .collect())
}

fn git_status_output(workspace_path: &Path, args: &[&str]) -> Result<std::process::Output> {
    let mut full_args = vec!["status"];
    full_args.extend_from_slice(args);
    git::run_raw(workspace_path, &full_args)
}

fn allowed_status_prefix(workspace_path: &Path, allowed_artifact_dir: &Path) -> Result<PathBuf> {
    let artifact_dir = if allowed_artifact_dir.is_absolute() {
        allowed_artifact_dir.to_path_buf()
    } else {
        workspace_path.join(allowed_artifact_dir)
    };
    let relative = artifact_dir
        .strip_prefix(workspace_path)
        .with_context(|| {
            format!(
                "Artifact area {} must be inside source checkout {}",
                artifact_dir.display(),
                workspace_path.display()
            )
        })?
        .to_path_buf();
    Ok(relative)
}

fn filtered_status_entries(entries: &[String], allowed_prefix: &Path) -> Vec<String> {
    let mut filtered = entries
        .iter()
        .filter(|entry| !status_entry_touches_path(entry, allowed_prefix))
        .cloned()
        .collect::<Vec<_>>();
    filtered.sort();
    filtered
}

fn status_entry_touches_path(entry: &str, path_prefix: &Path) -> bool {
    let path = match entry.get(3..) {
        Some(path) => path,
        None => return false,
    };
    path.split(" -> ").any(|status_path| {
        let status_path = Path::new(status_path);
        status_path == path_prefix || status_path.starts_with(path_prefix)
    })
}

fn status_diff(baseline: &[String], current: &[String]) -> String {
    let mut lines = Vec::new();
    for entry in baseline {
        if !current.contains(entry) {
            lines.push(format!("- {entry}"));
        }
    }
    for entry in current {
        if !baseline.contains(entry) {
            lines.push(format!("+ {entry}"));
        }
    }
    lines.join("\n")
}

fn protected_fluent_file_snapshot(
    workspace_path: &Path,
    allowed_artifact_dir: &Path,
) -> Result<BTreeMap<PathBuf, Vec<u8>>> {
    let mut snapshot = BTreeMap::new();
    let fluent_dir = workspace_path.join(".fluent");
    if !fluent_dir.exists() {
        return Ok(snapshot);
    }
    let allowed = allowed_status_prefix(workspace_path, allowed_artifact_dir)?;
    collect_protected_fluent_files(workspace_path, &fluent_dir, &allowed, &mut snapshot)?;
    Ok(snapshot)
}

fn collect_protected_fluent_files(
    workspace_path: &Path,
    dir: &Path,
    allowed: &Path,
    snapshot: &mut BTreeMap<PathBuf, Vec<u8>>,
) -> Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let relative = path.strip_prefix(workspace_path)?.to_path_buf();
        if relative == allowed || relative.starts_with(allowed) {
            continue;
        }
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            collect_protected_fluent_files(workspace_path, &path, allowed, snapshot)?;
        } else if file_type.is_file() {
            snapshot.insert(relative, fs::read(&path)?);
        }
    }
    Ok(())
}

fn fluent_file_snapshot_diff(
    baseline: &BTreeMap<PathBuf, Vec<u8>>,
    current: &BTreeMap<PathBuf, Vec<u8>>,
) -> String {
    let mut lines = Vec::new();
    for (path, content) in baseline {
        match current.get(path) {
            Some(current_content) if current_content == content => {}
            Some(_) => lines.push(format!("~ {}", path.display())),
            None => lines.push(format!("- {}", path.display())),
        }
    }
    for path in current.keys() {
        if !baseline.contains_key(path) {
            lines.push(format!("+ {}", path.display()));
        }
    }
    lines.join("\n")
}

fn restore_non_fluent_worktree_changes(workspace_path: &Path) -> Result<()> {
    let restore = git::run_raw(
        workspace_path,
        &[
            "restore",
            "--staged",
            "--worktree",
            "--",
            ".",
            ":(exclude).fluent",
        ],
    )?;
    if !restore.status.success() {
        bail!(
            "Failed to restore non-Fluent source changes: {}",
            String::from_utf8_lossy(&restore.stderr)
        );
    }

    let clean = git::run_raw(
        workspace_path,
        &["clean", "-fd", "--", ".", ":(exclude).fluent"],
    )?;
    if clean.status.success() {
        Ok(())
    } else {
        bail!(
            "Failed to remove untracked non-Fluent source changes: {}",
            String::from_utf8_lossy(&clean.stderr)
        )
    }
}

fn restore_source_changes_outside_artifact_area(
    baseline: &SourceCheckoutReviewGuard,
) -> Result<()> {
    let allowed = allowed_status_prefix(&baseline.path, &baseline.allowed_artifact_dir)?;
    let excluded_pathspec = format!(":(exclude){}", allowed.display());
    let restore = git::run_raw(
        &baseline.path,
        &[
            "restore",
            "--staged",
            "--worktree",
            "--",
            ".",
            &excluded_pathspec,
        ],
    )?;
    if !restore.status.success() {
        bail!(
            "Failed to restore source changes outside managed artifact area: {}",
            String::from_utf8_lossy(&restore.stderr)
        );
    }

    let clean = git::run_raw(
        &baseline.path,
        &["clean", "-fd", "--", ".", &excluded_pathspec],
    )?;
    if !clean.status.success() {
        bail!(
            "Failed to remove untracked source changes outside managed artifact area: {}",
            String::from_utf8_lossy(&clean.stderr)
        )
    }

    let current = protected_fluent_file_snapshot(&baseline.path, &baseline.allowed_artifact_dir)?;
    for path in current.keys() {
        if !baseline.protected_fluent_files.contains_key(path) {
            let absolute = baseline.path.join(path);
            if absolute.is_file() {
                fs::remove_file(&absolute)?;
            }
        }
    }
    for (path, content) in &baseline.protected_fluent_files {
        let absolute = baseline.path.join(path);
        if let Some(parent) = absolute.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(absolute, content)?;
    }

    Ok(())
}

fn ensure_head_unchanged(workspace_path: &Path, baseline_head: &str) -> Result<()> {
    let current_head = head_commit(workspace_path)?;
    if current_head == baseline_head {
        Ok(())
    } else {
        reset_worktree_head(workspace_path, baseline_head)?;
        bail!(
            "Review Task changed readable candidate workspace HEAD from {baseline_head} to {current_head}: {}",
            workspace_path.display()
        )
    }
}

fn ensure_source_head_unchanged(workspace_path: &Path, baseline_head: &str) -> Result<()> {
    let current_head = head_commit(workspace_path)?;
    if current_head == baseline_head {
        Ok(())
    } else {
        reset_worktree_head(workspace_path, baseline_head)?;
        bail!(
            "Review Task changed readable source checkout HEAD from {baseline_head} to {current_head}: {}",
            workspace_path.display()
        )
    }
}

fn reset_worktree_head(workspace_path: &Path, target: &str) -> Result<()> {
    git::run(
        workspace_path,
        &["reset", "--hard", target],
        "restore readable candidate workspace HEAD",
    )
}

fn path_for_model(project_root: &Path, path: &Path) -> String {
    path.strip_prefix(project_root)
        .unwrap_or(path)
        .to_string_lossy()
        .to_string()
}

fn commits_ahead(workspace_path: &Path, source_ref: &str) -> Result<u32> {
    let range = format!("{source_ref}..HEAD");
    let stdout = git::run_stdout(
        workspace_path,
        &["rev-list", "--count", &range],
        &format!("compare task workspace with {source_ref}"),
    )?;
    Ok(stdout.parse()?)
}

fn head_commit(workspace_path: &Path) -> Result<String> {
    git::run_stdout(
        workspace_path,
        &["rev-parse", "HEAD"],
        "resolve task workspace HEAD",
    )
}

fn current_branch(project_root: &Path) -> Result<String> {
    let branch = git::run_stdout(
        project_root,
        &["rev-parse", "--abbrev-ref", "HEAD"],
        "resolve source branch",
    )?;
    if branch != "HEAD" {
        return Ok(branch);
    }

    git::run_stdout(
        project_root,
        &["rev-parse", "HEAD"],
        "resolve source commit",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content::ContentResolver;
    use crate::work_model::{
        CorrectiveAuditContext, CorrectiveAuthorityReference, CorrectiveContext,
        CorrectiveEvidenceReference, DerivedProvenance, ExecutionAuthority, TaskOutput,
        TaskStatus, WorkItem, WorkItemAbandonment, WorkLineage,
    };
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;
    use std::sync::{Arc, Mutex};

    fn review_item() -> WorkItem {
        review_item_with_role("tests")
    }

    fn review_item_with_role(role: &str) -> WorkItem {
        let mut item = WorkItem {
            id: "work-1".to_string(),
            title: "Review prompts".to_string(),
            planning_context: None,
            instructions: None,
            abandonment: None,
            post_merge_review_fix_depth: None,
            attempts: Vec::new(),
            merge_candidates: Vec::new(),
            ..Default::default()
        };
        item.add_initial_attempt("attempt-1").unwrap();
        let attempt = item.attempts.first_mut().unwrap();
        let task = attempt.tasks.first_mut().unwrap();
        let workspace = task.workspace_access.writes.first().unwrap().clone();
        task.status = TaskStatus::Complete;
        task.output = Some(TaskOutput {
            workspace_id: workspace.id,
            workspace_path: workspace.path,
            source_branch: "main".to_string(),
            base_commit: None,
            commit: "abc123".to_string(),
        });
        item.add_review_tasks("attempt-1", &[role]).unwrap();
        item
    }

    #[test]
    fn run_task_rejects_abandoned_work_item_without_mutating_state() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = WorkModelStore::new(tmp.path());
        let mut item = WorkItem {
            id: "work-1".to_string(),
            title: "Keep abandoned task terminal".to_string(),
            planning_context: None,
            instructions: None,
            abandonment: None,
            post_merge_review_fix_depth: None,
            attempts: Vec::new(),
            merge_candidates: Vec::new(),
            ..Default::default()
        };
        item.add_initial_attempt("attempt-1").unwrap();
        item.abandonment = Some(WorkItemAbandonment {
            reason: Some("replacement landed".to_string()),
        });
        store.create_work_item(&item).unwrap();
        let resolver = ContentResolver::new(None);

        let error = match run_task(WorkTaskRunConfig {
            project_root: tmp.path(),
            store: &store,
            work_item_id: "work-1",
            attempt_id: "attempt-1",
            task_id: "attempt-1-write-1",
            resolver: &resolver,
            extra_args: &[],
            no_sandbox: true,
            store_lock: None,
        }) {
            Ok(_) => panic!("abandoned Work Item should reject task run"),
            Err(error) => error,
        };

        assert!(error.to_string().contains("is abandoned"));
        let stored = store.read_work_item("work-1").unwrap();
        assert!(stored.abandonment.is_some());
        assert_eq!(stored.attempts[0].status, AttemptStatus::Planned);
        assert_eq!(stored.attempts[0].tasks[0].status, TaskStatus::Planned);
    }

    fn corrective_review_item(role: &str) -> WorkItem {
        let context = CorrectiveContext {
            objective: "Restore the retry guard".to_string(),
            requirement: "Retries stop after the configured cap".to_string(),
            evidence: "Merged commit abc123 removed the cap check".to_string(),
            included_scope: "src/retry.rs".to_string(),
            excluded_scope: "unrelated backoff tuning".to_string(),
            verification: "cargo test retry::cap_is_enforced".to_string(),
        };
        let mut item = WorkItem::derived_corrective(
            "work-1",
            "Restore the retry guard",
            DerivedProvenance::default(),
            context,
            WorkLineage::descendant_of("root-1", None),
            Some(ExecutionAuthority::Automatic),
        )
        .unwrap();
        item.corrective_audit = Some(CorrectiveAuditContext {
            follow_up_id: "fu-retry-cap".to_string(),
            source: "learner".to_string(),
            learning_summary: "The accepted change removed the retry cap".to_string(),
            expected_result: "The retry cap is enforced again".to_string(),
            target_paths: vec!["src/retry.rs".to_string()],
            unresolved_decisions: Vec::new(),
            authority: CorrectiveAuthorityReference {
                kind: "expertise-entry".to_string(),
                path: ".fluent/expertise/retry.md".to_string(),
                anchor: "Retries stop after the configured cap".to_string(),
                digest: "sha256:authority".to_string(),
            },
            evidence: vec![CorrectiveEvidenceReference {
                path: "review.md".to_string(),
                digest: "sha256:evidence".to_string(),
            }],
        });
        item.add_initial_attempt("attempt-1").unwrap();
        let attempt = item.attempts.first_mut().unwrap();
        let task = attempt.tasks.first_mut().unwrap();
        let workspace = task.workspace_access.writes.first().unwrap().clone();
        task.status = TaskStatus::Complete;
        task.output = Some(TaskOutput {
            workspace_id: workspace.id,
            workspace_path: workspace.path,
            source_branch: "main".to_string(),
            base_commit: None,
            commit: "abc123".to_string(),
        });
        item.add_review_tasks("attempt-1", &[role]).unwrap();
        item
    }

    #[test]
    fn corrective_prompts_and_task_records_retain_execution_context() {
        let tmp = tempfile::TempDir::new().unwrap();
        let project_root = tmp.path();
        let workspace = tmp.path().join("work-6-work-1-attempt-1");
        fs::create_dir_all(&workspace).unwrap();

        let item = corrective_review_item("tests");
        let context = item.write_task_instructions().unwrap();

        // The Writer receives the corrective execution context verbatim.
        let write_prompt = build_write_task_prompt_with_workspace(
            &item,
            "attempt-1",
            "attempt-1-write-1",
            &[],
            Some(&workspace),
            Some(project_root),
        );

        // Every reviewer receives the same corrective execution context.
        let artifact_dir =
            project_root.join(".fluent/work/artifacts/work-1/attempt-1/attempt-1-review-tests");
        let review_path = artifact_dir.join("review.md");
        let prompts = build_work_review_prompts(WorkReviewPromptInput {
            item: &item,
            attempt_id: "attempt-1",
            task_id: "attempt-1-review-tests",
            project_root,
            artifact_dir: &artifact_dir,
            review_path: &review_path,
            readable_workspaces: std::slice::from_ref(&workspace),
            input_artifacts: &[],
            review_only: false,
        })
        .unwrap();

        assert!(
            write_prompt.contains(&context),
            "writer prompt must contain the corrective execution context"
        );
        assert!(
            prompts.review_prompt.contains(&context),
            "reviewer prompt must contain the same corrective execution context"
        );
        for probe in [
            "Restore the retry guard",
            "Retries stop after the configured cap",
            "cargo test retry::cap_is_enforced",
            "The retry cap is enforced again",
            "src/retry.rs",
            ".fluent/expertise/retry.md",
            "sha256:evidence",
            "The accepted change removed the retry cap",
        ] {
            assert!(write_prompt.contains(probe), "writer missing {probe:?}");
            assert!(
                prompts.review_prompt.contains(probe),
                "reviewer missing {probe:?}"
            );
        }

        // The Tester Task persists the same context for durable inspection, but
        // its runner executes tester.yaml rather than consuming a prompt.
        let tester = item.attempts[0]
            .tasks
            .iter()
            .find(|task| task.kind == TaskKind::Tester)
            .expect("corrective Attempt has a Tester Task");
        assert_eq!(tester.instructions.as_deref(), Some(context.as_str()));
    }

    #[test]
    fn writer_and_every_reviewer_prompt_retain_audit_after_origin_cleanup() {
        let tmp = tempfile::TempDir::new().unwrap();
        let project_root = tmp.path().join("project");
        fs::create_dir_all(project_root.join(".fluent/expertise")).unwrap();
        for args in [
            &["init", "-b", "main"] as &[&str],
            &["config", "user.email", "test@test"],
            &["config", "user.name", "test"],
        ] {
            crate::git::run(&project_root, args, "initialize prompt cleanup repository").unwrap();
        }
        let authority_anchor = "Retries stop after the configured cap";
        fs::write(
            project_root.join(".fluent/expertise/retry.md"),
            format!("{authority_anchor}\n"),
        )
        .unwrap();
        crate::git::run(
            &project_root,
            &["add", ".fluent/expertise/retry.md"],
            "stage prompt cleanup authority",
        )
        .unwrap();
        crate::git::run(
            &project_root,
            &["commit", "-m", "Seed authority"],
            "commit prompt cleanup authority",
        )
        .unwrap();
        let merged_commit = String::from_utf8(
            crate::git::run_raw(&project_root, &["rev-parse", "HEAD"])
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_string();

        let store = WorkModelStore::new(&project_root);
        let mut origin = WorkItem::planned("root-1", "Origin work");
        origin.add_initial_attempt("origin-attempt").unwrap();
        let attempt = origin.attempts.first_mut().unwrap();
        let write = attempt.tasks.first_mut().unwrap();
        let write_workspace = write.workspace_access.writes.first().unwrap().clone();
        write.status = TaskStatus::Complete;
        write.output = Some(TaskOutput {
            workspace_id: write_workspace.id,
            workspace_path: write_workspace.path,
            source_branch: "main".to_string(),
            base_commit: None,
            commit: merged_commit.clone(),
        });
        attempt.status = crate::work_model::AttemptStatus::Complete;
        attempt.review_state = Some(crate::work_model::AttemptReviewState::Passed);
        attempt.learning = Some(crate::work_model::AttemptLearning::succeeded(
            1,
            crate::follow_up::ArtifactRef {
                path: ".fluent/work/handoffs/root-1/origin-attempt/handoff.json".to_string(),
                digest: "sha256:handoff".to_string(),
            },
        ));
        let candidate_id = origin
            .create_or_get_merge_candidate("origin-attempt")
            .unwrap();
        let candidate = origin
            .merge_candidates
            .iter_mut()
            .find(|candidate| candidate.id == candidate_id)
            .unwrap();
        candidate.merge_state.status = crate::work_model::MergeCandidateMergeStatus::Merged;
        candidate.merge_state.merged_commit = Some(merged_commit.clone());
        store.create_work_item(&origin).unwrap();

        let authority_digest = crate::follow_up::content_digest(authority_anchor.as_bytes());
        let corrective_context = CorrectiveContext {
            objective: "Restore the retry guard".to_string(),
            requirement: authority_anchor.to_string(),
            evidence: "Merged commit removed the retry cap".to_string(),
            included_scope: "src/retry.rs".to_string(),
            excluded_scope: "unrelated backoff tuning".to_string(),
            verification: "cargo test retry::cap_is_enforced".to_string(),
        };
        let batch = crate::follow_up::NormalizedFollowUpBatchV1 {
            schema_version: crate::follow_up::NormalizedFollowUpBatchV1::SCHEMA_VERSION,
            source: crate::follow_up::FollowUpSource::Learner,
            origin: crate::follow_up::PostLandOrigin {
                work_item_id: origin.id.clone(),
                attempt_id: "origin-attempt".to_string(),
                merge_candidate_id: candidate_id,
                merged_commit,
            },
            learning_summary: "The accepted change removed the retry cap".to_string(),
            follow_ups: vec![crate::follow_up::FollowUpDraftV1 {
                id: "fu-retry-cap".to_string(),
                summary: "Restore the retry guard".to_string(),
                corrective: true,
                corrective_context: Some(corrective_context),
                target_paths: vec!["src/retry.rs".to_string()],
                expected_result: "The retry cap is enforced again".to_string(),
                unresolved_decisions: Vec::new(),
                authority: Some(crate::follow_up::AuthorityLocator {
                    kind: crate::follow_up::AuthorityKind::ExpertiseEntry,
                    path: ".fluent/expertise/retry.md".to_string(),
                    anchor: authority_anchor.to_string(),
                    digest: authority_digest.clone(),
                }),
                evidence: vec![crate::follow_up::ArtifactRef {
                    path: "review.md".to_string(),
                    digest: "sha256:evidence".to_string(),
                }],
            }],
        };
        crate::follow_up::process_landed_batch(&project_root, &batch, None).unwrap();
        let origin_artifacts = project_root.join(".fluent/work/artifacts/root-1/origin-attempt");
        fs::create_dir_all(&origin_artifacts).unwrap();
        fs::write(origin_artifacts.join("origin.txt"), "origin artifact\n").unwrap();
        let mut seeded = store
            .list_work_items()
            .unwrap()
            .into_iter()
            .find(|item| item.origin.is_derived())
            .expect("landed follow-up creates derived Work");
        seeded
            .authorize_execution(ExecutionAuthority::Human)
            .unwrap();
        seeded.add_initial_attempt("attempt-1").unwrap();
        let write = seeded.attempts[0].tasks.first_mut().unwrap();
        let workspace_ref = write.workspace_access.writes.first().unwrap().clone();
        write.status = TaskStatus::Complete;
        write.output = Some(TaskOutput {
            workspace_id: workspace_ref.id,
            workspace_path: workspace_ref.path,
            source_branch: "main".to_string(),
            base_commit: None,
            commit: batch.origin.merged_commit.clone(),
        });
        seeded
            .add_review_tasks(
                "attempt-1",
                &["architecture", "behaviors", "documentation", "skills", "tests"],
            )
            .unwrap();
        store.write_work_item(&seeded).unwrap();

        let cleanup = crate::cleanup::cleanup_work_items(
            &project_root,
            &crate::cleanup::CleanupOptions { apply: true },
        )
        .unwrap();
        assert!(
            cleanup.iter().any(|result| matches!(
                result,
                crate::cleanup::WorkCleanupResult::WorkItem(item)
                    if item.work_item_id == "root-1"
            )),
            "real cleanup removes the landed origin"
        );
        assert!(store.read_work_item("root-1").is_err());
        assert!(!store.work_attempts_dir().join("root-1").exists());
        assert!(!store.work_tasks_dir().join("root-1").exists());
        assert!(!store.work_merge_candidates_dir().join("root-1").exists());
        assert!(!project_root.join(".fluent/work/artifacts/root-1").exists());

        let item = store.read_work_item(&seeded.id).unwrap();
        let workspace = tmp.path().join("candidate");
        fs::create_dir_all(&workspace).unwrap();
        let writer = build_write_task_prompt_with_workspace(
            &item,
            "attempt-1",
            "attempt-1-write-1",
            &[],
            Some(&workspace),
            Some(&project_root),
        );
        let audit_fields = [
            "Restore the retry guard",
            authority_anchor,
            "Merged commit removed the retry cap",
            "src/retry.rs",
            "unrelated backoff tuning",
            "cargo test retry::cap_is_enforced",
            "The retry cap is enforced again",
            "expertise-entry",
            ".fluent/expertise/retry.md",
            authority_digest.as_str(),
            "review.md",
            "sha256:evidence",
            "Unresolved decisions\nnone",
            "learner",
            "fu-retry-cap",
            "The accepted change removed the retry cap",
        ];
        for field in audit_fields {
            assert!(writer.contains(field), "Writer omitted {field:?} after cleanup");
        }

        for role in crate::review::REVIEWERS {
            let task_id = format!("attempt-1-review-{role}");
            let artifact_dir = tmp
                .path()
                .join(".fluent/work/artifacts/work-1/attempt-1")
                .join(&task_id);
            let prompts = build_work_review_prompts(WorkReviewPromptInput {
                item: &item,
                attempt_id: "attempt-1",
                task_id: &task_id,
                project_root: &project_root,
                artifact_dir: &artifact_dir,
                review_path: &artifact_dir.join("review.md"),
                readable_workspaces: std::slice::from_ref(&workspace),
                input_artifacts: &[],
                review_only: false,
            })
            .unwrap();
            for field in audit_fields {
                assert!(
                    prompts.review_prompt.contains(field),
                    "{role} reviewer omitted {field:?} after cleanup"
                );
            }
        }
    }

    #[test]
    fn work_review_prompt_names_work_artifacts_and_writable_outputs() {
        let tmp = tempfile::TempDir::new().unwrap();
        let project_root = tmp.path();
        let workspace = tmp.path().join("work-6-work-1-attempt-1");
        fs::create_dir_all(&workspace).unwrap();
        let artifact_dir =
            project_root.join(".fluent/work/artifacts/work-1/attempt-1/attempt-1-review-tests");
        let review_path = artifact_dir.join("review.md");

        let item = review_item();
        let prompts = build_work_review_prompts(WorkReviewPromptInput {
            item: &item,
            attempt_id: "attempt-1",
            task_id: "attempt-1-review-tests",
            project_root,
            artifact_dir: &artifact_dir,
            review_path: &review_path,
            readable_workspaces: std::slice::from_ref(&workspace),
            input_artifacts: &[],
            review_only: false,
        })
        .unwrap();

        assert!(
            prompts
                .review_prompt
                .contains(&review_path.display().to_string())
        );
        assert!(prompts.review_prompt.contains("CARGO_TARGET_DIR"));
        assert!(prompts.review_prompt.contains("cargo build"));
        assert!(
            prompts.review_prompt.contains("may READ the candidate"),
            "prompt should tell reviewer they can read candidate build outputs"
        );
        assert!(
            prompts.review_prompt.contains("Do not edit or commit"),
            "prompt should tell reviewer not to write to candidate"
        );
        assert!(
            prompts.review_prompt.contains("pre-populated"),
            "prompt should mention pre-populated warm cache"
        );
        assert!(!prompts.review_prompt.contains(".fluent/runs/"));
        // System prompt is now thin (identity + lifecycle); build cache + artifact details live in the user message.
        assert!(!prompts.system_prompt.contains("CARGO_TARGET_DIR"));
        assert!(!prompts.system_prompt.contains("pre-populated"));
        assert!(!prompts.system_prompt.contains(".fluent/runs/"));
    }

    #[test]
    fn work_review_prompt_renders_role_conditional_blocks() {
        let tmp = tempfile::TempDir::new().unwrap();
        let project_root = tmp.path();
        let workspace = tmp.path().join("work-6-work-1-attempt-1");
        fs::create_dir_all(&workspace).unwrap();
        let artifact_dir =
            project_root.join(".fluent/work/artifacts/work-1/attempt-1/attempt-1-review-behaviors");
        let review_path = artifact_dir.join("review.md");

        let item = review_item_with_role("behaviors");
        let prompts = build_work_review_prompts(WorkReviewPromptInput {
            item: &item,
            attempt_id: "attempt-1",
            task_id: "attempt-1-review-behaviors",
            project_root,
            artifact_dir: &artifact_dir,
            review_path: &review_path,
            readable_workspaces: std::slice::from_ref(&workspace),
            input_artifacts: &[],
            review_only: false,
        })
        .unwrap();

        // is_review_behaviors block bullets are present.
        assert!(
            prompts.review_prompt.contains("EARS statement"),
            "behaviors reviewer should see the EARS marker check"
        );
        assert!(
            prompts.review_prompt.contains("`Test:` reference"),
            "behaviors reviewer should see the Test: reference verification"
        );
        assert!(
            prompts.review_prompt.contains("tester-results.json"),
            "behaviors reviewer should see tester-results.json instructions"
        );

        // Other role blocks are NOT rendered.
        assert!(
            !prompts
                .review_prompt
                .contains("Verify `Untestable:` justifications from progress.md"),
            "behaviors reviewer should not see the tests-role Untestable check"
        );
        assert!(
            !prompts
                .review_prompt
                .contains("Each behavior in behaviors.md should have at least one test"),
            "behaviors reviewer should not see the tests-role behaviors-coverage check"
        );
        assert!(
            !prompts.review_prompt.contains("Flag structural choices"),
            "behaviors reviewer should not see the architecture-role checks"
        );
        assert!(
            !prompts.review_prompt.contains("polished prose"),
            "behaviors reviewer should not see the documentation-role checks"
        );
    }

    #[test]
    fn work_review_prompt_includes_shell_safe_executable_diff_command() {
        let item = review_item();
        let tmp = tempfile::TempDir::new().unwrap();
        let workspace = tmp.path().join("candidate'space");
        fs::create_dir_all(&workspace).unwrap();
        let review_path = tmp.path().join("review.md");
        let prompts = build_work_review_prompts(WorkReviewPromptInput {
            item: &item,
            attempt_id: "attempt-1",
            task_id: "attempt-1-review-tests",
            project_root: tmp.path(),
            artifact_dir: tmp.path(),
            review_path: &review_path,
            readable_workspaces: std::slice::from_ref(&workspace),
            input_artifacts: &[],
            review_only: false,
        })
        .unwrap();
        let command = prompts
            .review_prompt
            .lines()
            .find_map(|line| {
                let prefix = "1. Run the review diff command (`";
                let suffix = "`) to see what the Writer changed in this round.";
                line.strip_prefix(prefix)?.strip_suffix(suffix)
            })
            .unwrap();

        assert_eq!(
            command,
            render_review_diff_command(&workspace, "main...abc123")
        );
        assert!(command.contains("'\\''"));
        assert_shell_command_invokes_fake_git(
            command,
            &[
                "-C".to_string(),
                workspace.display().to_string(),
                "diff".to_string(),
                "main...abc123".to_string(),
            ],
        );
    }

    fn post_merge_review_item(base_commit: Option<String>) -> WorkItem {
        let mut item = WorkItem {
            id: "work-post-merge".to_string(),
            title: "Post-merge review of main".to_string(),
            planning_context: None,
            instructions: None,
            abandonment: None,
            post_merge_review_fix_depth: None,
            attempts: Vec::new(),
            merge_candidates: Vec::new(),
            ..Default::default()
        };
        item.add_post_merge_review_attempt(
            "attempt-1",
            &["tests"],
            "main",
            "merged-commit-abc",
            base_commit,
        )
        .unwrap();
        item
    }

    #[test]
    fn work_review_prompt_populates_diff_command_for_post_merge_when_base_commit_present() {
        let item = post_merge_review_item(Some("pre-merge-xyz".to_string()));
        let tmp = tempfile::TempDir::new().unwrap();
        let workspace = tmp.path().join("work-review-main");
        fs::create_dir_all(&workspace).unwrap();
        let review_path = tmp.path().join("review.md");
        let prompts = build_work_review_prompts(WorkReviewPromptInput {
            item: &item,
            attempt_id: "attempt-1",
            task_id: "attempt-1-review-tests",
            project_root: tmp.path(),
            artifact_dir: tmp.path(),
            review_path: &review_path,
            readable_workspaces: std::slice::from_ref(&workspace),
            input_artifacts: &[],
            review_only: true,
        })
        .unwrap();

        let command = prompts
            .review_prompt
            .lines()
            .find_map(|line| {
                let prefix = "2. Run the review diff command (`";
                let suffix = "`) to see the change that triggered this review.";
                line.strip_prefix(prefix)?.strip_suffix(suffix)
            })
            .expect("post-merge review prompt should render diff command with base_commit...HEAD");

        assert_eq!(
            command,
            render_review_diff_command(&workspace, "pre-merge-xyz...HEAD")
        );
    }

    #[test]
    fn work_review_prompt_omits_diff_command_for_review_only_without_base_commit() {
        let item = post_merge_review_item(None);
        let tmp = tempfile::TempDir::new().unwrap();
        let workspace = tmp.path().join("work-review-main");
        fs::create_dir_all(&workspace).unwrap();
        let review_path = tmp.path().join("review.md");
        let prompts = build_work_review_prompts(WorkReviewPromptInput {
            item: &item,
            attempt_id: "attempt-1",
            task_id: "attempt-1-review-tests",
            project_root: tmp.path(),
            artifact_dir: tmp.path(),
            review_path: &review_path,
            readable_workspaces: std::slice::from_ref(&workspace),
            input_artifacts: &[],
            review_only: true,
        })
        .unwrap();

        assert!(
            !prompts
                .review_prompt
                .contains("Run the review diff command"),
            "review-only without base_commit should skip the diff step"
        );
    }

    fn assert_shell_command_invokes_fake_git(command: &str, expected_args: &[String]) {
        let tmp = tempfile::TempDir::new().unwrap();
        let bin_dir = tmp.path().join("bin");
        fs::create_dir(&bin_dir).unwrap();
        let log_path = tmp.path().join("args.log");
        let git_path = bin_dir.join("git");
        fs::write(
            &git_path,
            format!(
                "#!/bin/sh\nprintf '<%s>\\n' \"$@\" > '{}'\n",
                log_path.display()
            ),
        )
        .unwrap();
        let mut permissions = fs::metadata(&git_path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&git_path, permissions).unwrap();

        let output = Command::new("/bin/sh")
            .arg("-c")
            .arg(command)
            .env("PATH", format!("{}:/usr/bin:/bin", bin_dir.display()))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let expected_log = expected_args
            .iter()
            .map(|arg| format!("<{arg}>\n"))
            .collect::<String>();
        assert_eq!(fs::read_to_string(log_path).unwrap(), expected_log);
    }

    #[test]
    fn review_task_transcript_path_resolved_from_artifact_area() {
        let item = review_item();
        let attempt = &item.attempts[0];
        let review_task = attempt
            .tasks
            .iter()
            .find(|t| t.kind == TaskKind::Review)
            .unwrap();
        let artifact_area = review_task.artifact_area.as_ref().unwrap();
        assert_eq!(
            artifact_area.path,
            ".fluent/work/artifacts/work-1/attempt-1/attempt-1-review-tests"
        );

        let tmp = tempfile::TempDir::new().unwrap();
        let project_root = tmp.path();
        let artifact_dir =
            resolve_managed_artifact_area_path(project_root, &artifact_area.path).unwrap();
        let transcript_path = artifact_dir.join("transcript.jsonl");

        assert_eq!(
            transcript_path,
            project_root.join(
                ".fluent/work/artifacts/work-1/attempt-1/attempt-1-review-tests/transcript.jsonl"
            )
        );
    }

    #[test]
    fn tester_task_artifact_path_resolved_from_artifact_area() {
        let item = review_item();
        let attempt = &item.attempts[0];
        let tester_task = attempt
            .tasks
            .iter()
            .find(|t| t.kind == TaskKind::Tester)
            .unwrap();
        let artifact_area = tester_task.artifact_area.as_ref().unwrap();
        assert_eq!(
            artifact_area.path,
            ".fluent/work/artifacts/work-1/attempt-1/attempt-1-tester"
        );

        let tmp = tempfile::TempDir::new().unwrap();
        let project_root = tmp.path();
        let artifact_dir =
            resolve_managed_artifact_area_path(project_root, &artifact_area.path).unwrap();
        let results_path = artifact_dir.join("tester-results.json");

        assert_eq!(
            results_path,
            project_root.join(
                ".fluent/work/artifacts/work-1/attempt-1/attempt-1-tester/tester-results.json"
            )
        );
    }

    #[test]
    fn tester_task_does_not_spawn_coder_process() {
        let item = review_item();
        let attempt = &item.attempts[0];
        let tester_task = attempt
            .tasks
            .iter()
            .find(|t| t.kind == TaskKind::Tester)
            .expect("should have a Tester task");
        assert_eq!(tester_task.kind, TaskKind::Tester);
        assert_ne!(tester_task.kind, TaskKind::BehaviorTests);
    }

    #[test]
    fn tester_task_does_not_write_transcript() {
        let item = review_item();
        let attempt = &item.attempts[0];
        let tester_task = attempt
            .tasks
            .iter()
            .find(|t| t.kind == TaskKind::Tester)
            .expect("should have a Tester task");
        let artifact_area = tester_task.artifact_area.as_ref().unwrap();

        let tmp = tempfile::TempDir::new().unwrap();
        let project_root = tmp.path();
        let artifact_dir =
            resolve_managed_artifact_area_path(project_root, &artifact_area.path).unwrap();
        std::fs::create_dir_all(&artifact_dir).unwrap();

        let transcript_path = artifact_dir.join("transcript.jsonl");
        assert!(
            !transcript_path.exists(),
            "Tester task should not create transcript.jsonl"
        );
    }

    #[test]
    fn tester_task_invokes_subcommand_not_coder() {
        let item = review_item();
        let attempt = &item.attempts[0];
        let tester_task = attempt
            .tasks
            .iter()
            .find(|t| t.kind == TaskKind::Tester)
            .expect("should have a Tester task");
        assert_eq!(tester_task.kind, TaskKind::Tester);
    }

    #[test]
    fn capture_coder_info_writes_json() {
        let dir = tempfile::tempdir().unwrap();
        let artifact_dir = dir.path().join("artifacts");
        std::fs::create_dir_all(&artifact_dir).unwrap();

        // capture_coder_info runs `<binary> --version` which may not be
        // available in test environments, but the function handles that
        // gracefully by writing "unknown" for the version.
        capture_coder_info(CoderKind::Claude, "test-model", &artifact_dir);

        let info_path = artifact_dir.join("coder-info.json");
        assert!(info_path.exists(), "coder-info.json should be created");

        let content = std::fs::read_to_string(&info_path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed["coder"], "claude");
        assert_eq!(parsed["model"], "test-model");
        assert!(parsed["captured_at"].is_string());
        assert!(parsed["version"].is_string());
    }

    #[test]
    fn materialize_general_expertise_writes_all_bundled_files() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = materialize_general_expertise(tmp.path()).unwrap();
        for name in crate::content::GENERAL_EXPERTISE_FILES {
            let path = dir.join(name);
            assert!(
                path.is_file(),
                "expected materialized expertise file at {}",
                path.display()
            );
            let body = std::fs::read_to_string(&path).unwrap();
            assert!(!body.is_empty(), "{} should not be empty", path.display());
        }
        assert_eq!(dir, tmp.path().join(".fluent/work/expertise"));
    }

    #[test]
    fn write_task_prompt_includes_general_expertise_index_path() {
        let item = review_item();
        let tmp = tempfile::TempDir::new().unwrap();
        let prompt = build_write_task_prompt_with_workspace(
            &item,
            "attempt-1",
            "attempt-1-write-1",
            &[],
            None,
            Some(tmp.path()),
        );
        let expected = tmp
            .path()
            .join(".fluent/work/expertise/INDEX.md")
            .display()
            .to_string();
        assert!(
            prompt.contains(&expected),
            "prompt should reference the general expertise INDEX path; got prompt:\n{prompt}"
        );
    }

    #[test]
    fn write_task_prompt_omits_project_expertise_index_when_missing() {
        let item = review_item();
        let tmp_workspace = tempfile::TempDir::new().unwrap();
        let prompt = build_write_task_prompt_with_workspace(
            &item,
            "attempt-1",
            "attempt-1-write-1",
            &[],
            Some(tmp_workspace.path()),
            None,
        );
        assert!(
            !prompt.contains("workspace-specific decisions"),
            "prompt should NOT include the project expertise line when missing; got prompt:\n{prompt}"
        );
    }

    #[test]
    fn write_task_prompt_includes_project_expertise_index_when_present() {
        let item = review_item();
        let tmp_workspace = tempfile::TempDir::new().unwrap();
        let project_expertise_dir = tmp_workspace.path().join(".fluent/expertise");
        std::fs::create_dir_all(&project_expertise_dir).unwrap();
        std::fs::write(project_expertise_dir.join("INDEX.md"), "# Project").unwrap();

        let prompt = build_write_task_prompt_with_workspace(
            &item,
            "attempt-1",
            "attempt-1-write-1",
            &[],
            Some(tmp_workspace.path()),
            None,
        );
        let expected = project_expertise_dir.join("INDEX.md").display().to_string();
        assert!(
            prompt.contains("learned model of THIS project"),
            "prompt should include the project expertise line when present; got prompt:\n{prompt}"
        );
        assert!(
            prompt.contains(&expected),
            "prompt should reference the project expertise INDEX path; got prompt:\n{prompt}"
        );
    }

    #[test]
    fn write_task_prompt_includes_progress_md_path_substitution() {
        let item = review_item();
        let tmp = tempfile::TempDir::new().unwrap();
        let prompt = build_write_task_prompt_with_workspace(
            &item,
            "attempt-1",
            "attempt-1-write-1",
            &[],
            None,
            Some(tmp.path()),
        );
        let expected_path = format!(
            "{}/.fluent/work/progress/work-1/attempt-1/progress.md",
            tmp.path().display()
        );
        assert!(
            prompt.contains(&expected_path),
            "prompt should include the absolute progress file path; got prompt:\n{prompt}"
        );
        assert!(
            prompt.contains("Create progress.md"),
            "first-round prompt (no progress.md) should include the Create progress.md instruction; got prompt:\n{prompt}"
        );
        assert!(
            !prompt.contains("Read progress.md"),
            "first-round prompt (no progress.md) should NOT include the Read instruction; got prompt:\n{prompt}"
        );
    }

    #[test]
    fn write_task_prompt_uses_read_progress_md_when_file_exists() {
        let item = review_item();
        let tmp = tempfile::TempDir::new().unwrap();
        let progress_dir = tmp.path().join(".fluent/work/progress/work-1/attempt-1");
        std::fs::create_dir_all(&progress_dir).unwrap();
        std::fs::write(progress_dir.join("progress.md"), "## Checklist\n").unwrap();

        let prompt = build_write_task_prompt_with_workspace(
            &item,
            "attempt-1",
            "attempt-1-write-1",
            &[],
            None,
            Some(tmp.path()),
        );
        assert!(
            prompt.contains("Read progress.md"),
            "follow-up-round prompt (progress.md exists) should include the Read instruction; got prompt:\n{prompt}"
        );
        assert!(
            !prompt.contains("Create progress.md"),
            "follow-up-round prompt (progress.md exists) should NOT include the Create instruction; got prompt:\n{prompt}"
        );
    }

    #[test]
    fn write_task_prompt_omits_prior_reviews_section_on_first_round() {
        let prompt = build_write_task_prompt(&review_item(), "attempt-1", "attempt-1-write-1", &[]);
        assert!(
            !prompt.contains("Read each prior review file"),
            "first-round prompt should NOT include the prior-reviews instruction; got prompt:\n{prompt}"
        );
        assert!(
            !prompt.contains("Address review finding:"),
            "first-round prompt should NOT include the prior-finding-record instruction; got prompt:\n{prompt}"
        );
    }

    #[test]
    fn write_task_prompt_includes_prior_reviews_section_when_present() {
        let tmp = tempfile::TempDir::new().unwrap();
        let review_path = tmp.path().join("review.md");
        std::fs::write(&review_path, "review body").unwrap();
        let prompt = build_write_task_prompt(
            &review_item(),
            "attempt-1",
            "attempt-1-write-1",
            std::slice::from_ref(&review_path),
        );
        assert!(
            prompt.contains("Read each prior review file"),
            "follow-up-round prompt should include the prior-reviews instruction; got prompt:\n{prompt}"
        );
        assert!(
            prompt.contains(&format!("   - {}", review_path.display())),
            "follow-up-round prompt should list each prior review path with sub-bullet indent; got prompt:\n{prompt}"
        );
        assert!(
            prompt.contains("Address review finding:"),
            "follow-up-round prompt should include the prior-finding-record instruction; got prompt:\n{prompt}"
        );
    }

    #[test]
    fn write_user_prompt_contains_phase_headings() {
        let prompt = build_write_task_prompt(&review_item(), "attempt-1", "attempt-1-write-1", &[]);
        assert!(
            prompt.contains("## Phase 1"),
            "user prompt should contain Phase 1 (Read the Work Item)"
        );
        assert!(
            prompt.contains("## Phase 2"),
            "user prompt should contain Phase 2 (Implement each planned step)"
        );
    }

    #[test]
    fn write_task_prompt_includes_progress_md_path_when_plan_present() {
        let mut item = review_item();
        item.planning_context = Some(crate::work_model::PlanningContext {
            plan: Some("## 1. Step one\n## 2. Step two\n".to_string()),
            ..Default::default()
        });
        let prompt = build_write_task_prompt(&item, "attempt-1", "attempt-1-write-1", &[]);
        assert!(
            prompt.contains("progress.md"),
            "user prompt should reference progress.md when the plan is present"
        );
    }

    #[test]
    fn writer_prompt_includes_tester_yaml_bootstrap_when_missing() {
        let item = review_item();
        let tmp = tempfile::TempDir::new().unwrap();
        let workspace = tmp.path();
        std::fs::create_dir_all(workspace.join(".fluent")).unwrap();

        let prompt = build_write_task_prompt_with_workspace(
            &item,
            "attempt-1",
            "attempt-1-write-1",
            &[],
            Some(workspace),
            None,
        );
        assert!(
            prompt.contains("`.fluent/tester.yaml` is missing"),
            "prompt should include tester.yaml bootstrap when missing"
        );
    }

    #[test]
    fn writer_prompt_includes_extract_tester_results_bootstrap_when_missing() {
        let item = review_item();
        let tmp = tempfile::TempDir::new().unwrap();
        let workspace = tmp.path();
        std::fs::create_dir_all(workspace.join(".fluent")).unwrap();

        let prompt = build_write_task_prompt_with_workspace(
            &item,
            "attempt-1",
            "attempt-1-write-1",
            &[],
            Some(workspace),
            None,
        );
        assert!(
            prompt.contains("`.fluent/extract-tester-results` is missing"),
            "prompt should include extract-tester-results bootstrap when missing"
        );
    }

    #[test]
    fn extract_tester_results_bootstrap_requires_unique_ids() {
        let item = review_item();
        let tmp = tempfile::TempDir::new().unwrap();
        let workspace = tmp.path();
        std::fs::create_dir_all(workspace.join(".fluent")).unwrap();

        let prompt = build_write_task_prompt_with_workspace(
            &item,
            "attempt-1",
            "attempt-1-write-1",
            &[],
            Some(workspace),
            None,
        );
        assert!(
            prompt.contains("globally unique"),
            "extract-tester-results bootstrap should require globally unique ids"
        );
    }

    #[test]
    fn writer_prompt_omits_bootstrap_when_both_files_present() {
        let item = review_item();
        let tmp = tempfile::TempDir::new().unwrap();
        let workspace = tmp.path();
        let fluent_dir = workspace.join(".fluent");
        std::fs::create_dir_all(&fluent_dir).unwrap();
        std::fs::write(fluent_dir.join("tester.yaml"), "commands: []").unwrap();
        let extractor = fluent_dir.join("extract-tester-results");
        std::fs::write(&extractor, "#!/bin/sh\necho '[]'\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&extractor).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&extractor, perms).unwrap();
        }

        let prompt = build_write_task_prompt_with_workspace(
            &item,
            "attempt-1",
            "attempt-1-write-1",
            &[],
            Some(workspace),
            None,
        );
        assert!(
            !prompt.contains("`.fluent/tester.yaml` is missing"),
            "prompt should NOT include tester.yaml bootstrap when present"
        );
        assert!(
            !prompt.contains("`.fluent/extract-tester-results` is missing"),
            "prompt should NOT include extract-tester-results bootstrap when present"
        );
    }

    #[test]
    fn resolve_input_artifact_paths_skips_missing_progress_md() {
        use crate::work_model::ArtifactRef;
        let tmp = tempfile::tempdir().unwrap();
        let project_root = tmp.path();
        // Create a non-progress artifact so the test has something to resolve
        let artifact_dir =
            project_root.join(".fluent/work/artifacts/work-1/attempt-1/attempt-1-review-docs");
        std::fs::create_dir_all(&artifact_dir).unwrap();
        let other_artifact = artifact_dir.join("review.md");
        std::fs::write(&other_artifact, "review content").unwrap();
        // progress.md does NOT exist — the writer hasn't created it yet

        let refs = vec![
            ArtifactRef {
                producer_id: "writer".to_string(),
                path: ".fluent/work/artifacts/work-1/attempt-1/progress.md".to_string(),
            },
            ArtifactRef {
                producer_id: "attempt-1-review-docs".to_string(),
                path: ".fluent/work/artifacts/work-1/attempt-1/attempt-1-review-docs/review.md"
                    .to_string(),
            },
        ];
        let resolved = resolve_input_artifact_paths(project_root, &refs).unwrap();
        // Only the existing review.md should be resolved; progress.md is skipped
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0], other_artifact);
    }

    #[test]
    fn resolve_input_artifact_paths_errors_on_missing_non_progress_md() {
        use crate::work_model::ArtifactRef;
        let tmp = tempfile::tempdir().unwrap();
        let project_root = tmp.path();

        let refs = vec![ArtifactRef {
            producer_id: "some-other-task".to_string(),
            path: ".fluent/work/artifacts/work-1/attempt-1/some-artifact.md".to_string(),
        }];
        let result = resolve_input_artifact_paths(project_root, &refs);
        assert!(
            result.is_err(),
            "should error on missing non-progress artifact"
        );
    }

    #[test]
    fn materialize_skill_writes_without_project_skills_dir() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dest = tmp.path().join("output");
        // dest does not contain a skills/ directory — materialize works from the binary alone
        let skill_dir = materialize_skill("review-tests", &dest).unwrap();
        assert!(
            skill_dir.join("SKILL.md").is_file(),
            "should write SKILL.md from bundled content"
        );
    }

    #[test]
    fn materialize_skill_dereferences_references() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dest = tmp.path().join("skills");
        materialize_skill("review-architecture", &dest).unwrap();
        let ref_path = dest.join("review-architecture/references/architecture.md");
        assert!(
            ref_path.is_file(),
            "references/architecture.md should be a real file"
        );
        assert!(
            !ref_path.is_symlink(),
            "references/architecture.md should not be a symlink"
        );
        let content = std::fs::read_to_string(&ref_path).unwrap();
        assert!(
            !content.is_empty(),
            "dereferenced reference should have real content"
        );
    }

    #[test]
    fn materialize_skill_errors_on_missing_bundled_reference() {
        let tmp = tempfile::TempDir::new().unwrap();
        let result = materialize_skill("nonexistent-skill", tmp.path());
        assert!(result.is_err(), "should error on unknown skill name");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("No bundled skill"),
            "error should name the problem: {err}"
        );
    }

    #[test]
    fn review_skill_path_materializes_from_bundled_content() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = review_skill_path("tests", tmp.path()).unwrap();
        assert!(
            path.contains(".fluent/work/skills/review-tests/SKILL.md"),
            "should resolve to materialized path: {path}"
        );
        assert!(
            Path::new(&path).is_file(),
            "materialized skill file should exist on disk"
        );
    }

    #[test]
    fn review_skill_path_reuses_already_materialized() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path1 = review_skill_path("architecture", tmp.path()).unwrap();
        let path2 = review_skill_path("architecture", tmp.path()).unwrap();
        assert_eq!(path1, path2, "repeated calls should return the same path");
    }

    #[test]
    fn review_skill_path_errors_on_unknown_role() {
        let tmp = tempfile::TempDir::new().unwrap();
        let result = review_skill_path("nonexistent", tmp.path());
        assert!(result.is_err(), "should error on unknown review role");
    }

    #[test]
    fn write_user_prompt_directs_commit_conventions() {
        let prompt = build_write_task_prompt(&review_item(), "attempt-1", "attempt-1-write-1", &[]);
        let d1_region = prompt
            .find("### D. Commit and advance")
            .expect("prompt should contain §D heading");
        let d1_text = &prompt[d1_region..];
        assert!(
            d1_text.contains("commit conventions"),
            "§D.1 should reference the project's commit conventions"
        );
        assert!(
            d1_text.contains("AGENTS.md") || d1_text.contains("CLAUDE.md"),
            "§D.1 should reference AGENTS.md or CLAUDE.md as the source of commit conventions"
        );
    }

    #[test]
    fn write_user_prompt_directs_codebase_orientation() {
        let prompt = build_write_task_prompt(&review_item(), "attempt-1", "attempt-1-write-1", &[]);
        assert!(
            prompt.contains("Understand the Work Item and the codebase"),
            "Phase 1 heading should reference understanding the codebase"
        );
        assert!(
            prompt.contains("AGENTS.md") && prompt.contains("CLAUDE.md"),
            "Phase 1 should direct the writer to follow the project's AGENTS.md / CLAUDE.md"
        );
        assert!(
            prompt.contains("existing code"),
            "Phase 1 should direct the writer to skim the existing code"
        );
        assert!(
            prompt.contains("conventions"),
            "Phase 1 should reference following the project's conventions"
        );
    }

    #[test]
    fn capture_baseline_tester_persists_results_as_artifact() {
        let tmp = tempfile::TempDir::new().unwrap();
        let project_root = tmp.path();
        let workspace = project_root.join("workspace");
        std::fs::create_dir_all(workspace.join(".fluent")).unwrap();
        std::fs::write(
            workspace.join(".fluent/tester.yaml"),
            "commands:\n  - command: \"echo ok\"\n    test_harness: shell-harness\n",
        )
        .unwrap();

        let resolver = ContentResolver::new(Some(project_root));
        capture_baseline_tester(
            project_root,
            &workspace,
            "work-1",
            "attempt-1",
            true,
            &resolver,
        );

        let results_path = project_root.join(
            ".fluent/work/artifacts/work-1/attempt-1/attempt-1-baseline-tester/tester-results.json",
        );
        assert!(
            results_path.exists(),
            "baseline tester should persist tester-results.json as artifact"
        );
    }

    #[test]
    fn seed_prompt_renders_with_output_paths() {
        let tmp = tempfile::TempDir::new().unwrap();
        let workspace = tmp.path();
        let resolver = ContentResolver::new(None);
        let template = resolver
            .resolve_content("prompts/seed-user.md")
            .expect("seed-user.md must resolve");

        let index_path = workspace.join(".fluent/expertise/INDEX.md");
        let overview_path = workspace.join(".fluent/expertise/overview.md");
        let rendered = crate::content::render_template(
            &template,
            &[
                ("index_path", &index_path.display().to_string()),
                ("overview_path", &overview_path.display().to_string()),
                ("workspace_path", &workspace.display().to_string()),
            ],
        )
        .expect("seed template must render");

        assert!(
            rendered.contains(&index_path.display().to_string()),
            "rendered seed prompt should contain the INDEX.md path"
        );
        assert!(
            rendered.contains(&overview_path.display().to_string()),
            "rendered seed prompt should contain the overview.md path"
        );
        assert!(
            rendered.contains(&workspace.display().to_string()),
            "rendered seed prompt should contain the workspace path"
        );
        assert!(
            rendered.contains("Seed project expertise overview"),
            "rendered seed prompt should include the commit message instruction"
        );
    }

    #[test]
    fn write_prompt_includes_project_index_after_seed() {
        let item = review_item();
        let tmp = tempfile::TempDir::new().unwrap();
        let workspace = tmp.path();

        assert!(
            should_seed_project_model(TaskKind::Write, workspace),
            "seed should fire before INDEX.md exists"
        );

        let prompt_before = build_write_task_prompt_with_workspace(
            &item,
            "attempt-1",
            "attempt-1-write-1",
            &[],
            Some(workspace),
            None,
        );
        assert!(
            !prompt_before.contains("learned model of THIS project"),
            "prompt should not include project expertise before seed"
        );

        let expertise_dir = workspace.join(".fluent/expertise");
        fs::create_dir_all(&expertise_dir).unwrap();
        fs::write(expertise_dir.join("INDEX.md"), "# Project Index\n").unwrap();
        fs::write(expertise_dir.join("overview.md"), "# Overview\n").unwrap();

        assert!(
            !should_seed_project_model(TaskKind::Write, workspace),
            "seed should not fire after INDEX.md exists"
        );

        let prompt_after = build_write_task_prompt_with_workspace(
            &item,
            "attempt-1",
            "attempt-1-write-1",
            &[],
            Some(workspace),
            None,
        );
        assert!(
            prompt_after.contains("learned model of THIS project"),
            "prompt should include project expertise after seed produces INDEX.md"
        );
        assert!(
            prompt_after.contains(&expertise_dir.join("INDEX.md").display().to_string()),
            "prompt should reference the INDEX.md path"
        );
    }

    #[test]
    fn should_seed_project_model_true_when_write_role_and_index_absent() {
        let tmp = tempfile::TempDir::new().unwrap();
        let workspace = tmp.path();
        assert!(should_seed_project_model(TaskKind::Write, workspace));
    }

    #[test]
    fn should_seed_project_model_false_when_index_present() {
        let tmp = tempfile::TempDir::new().unwrap();
        let workspace = tmp.path();
        let expertise_dir = workspace.join(".fluent/expertise");
        fs::create_dir_all(&expertise_dir).unwrap();
        fs::write(expertise_dir.join("INDEX.md"), "# Index\n").unwrap();
        assert!(!should_seed_project_model(TaskKind::Write, workspace));
    }

    #[test]
    fn should_seed_project_model_false_for_non_write_tasks() {
        let tmp = tempfile::TempDir::new().unwrap();
        let workspace = tmp.path();
        assert!(!should_seed_project_model(TaskKind::Review, workspace));
        assert!(!should_seed_project_model(TaskKind::Tester, workspace));
    }

    #[test]
    fn seed_session_uses_non_writer_system_prompt() {
        let resolver = ContentResolver::new(None);
        let seed_prompt = resolver
            .resolve_content("prompts/seed-system.md")
            .expect("seed-system.md must resolve");
        let write_prompt = resolver
            .resolve_content("prompts/write-system.md")
            .expect("write-system.md must resolve");

        assert_ne!(
            seed_prompt, write_prompt,
            "seed session must not reuse the writer's system prompt"
        );
        assert!(
            !seed_prompt.contains("Fluent Writer"),
            "seed system prompt must not identify as a Fluent Writer"
        );
    }

    #[test]
    fn learner_session_uses_non_writer_system_prompt() {
        let resolver = ContentResolver::new(None);
        let learner_prompt = resolver
            .resolve_content("prompts/learner-system.md")
            .expect("learner-system.md must resolve");
        let write_prompt = resolver
            .resolve_content("prompts/write-system.md")
            .expect("write-system.md must resolve");

        assert_ne!(
            learner_prompt, write_prompt,
            "the Learner session must not reuse the writer's system prompt"
        );
        assert!(
            !learner_prompt.contains("Fluent Writer"),
            "the Learner system prompt must not identify as a Fluent Writer"
        );
    }

    #[test]
    fn writer_session_uses_write_system_prompt() {
        let resolver = ContentResolver::new(None);
        let write_prompt = resolver
            .resolve_content("prompts/write-system.md")
            .expect("write-system.md must resolve");

        assert!(
            write_prompt.contains("Fluent Writer"),
            "writer system prompt must identify as Fluent Writer"
        );
    }

    #[test]
    fn learner_prompt_includes_attempt_diff_and_all_review_artifacts() {
        let tmp = tempfile::TempDir::new().unwrap();
        let workspace = tmp.path();
        let resolver = ContentResolver::new(None);
        let template = resolver
            .resolve_content("prompts/learner-user.md")
            .expect("learner-user.md must resolve");

        let learnings_dir = workspace.join(".fluent/expertise/learnings");
        let learnings_index_path = learnings_dir.join("INDEX.md");
        let expertise_index_path = workspace.join(".fluent/expertise/INDEX.md");

        let review_paths = "- /tmp/review-1/review.md\n- /tmp/review-2/review.md";
        let tester_paths =
            "- /tmp/tester-1/tester-results.json\n- /tmp/tester-2/tester-results.json";
        let diff_command = "git -C '/tmp/workspace' diff 'main...HEAD'";
        let draft_path = "/tmp/learner/follow-up-draft.json";

        let rendered = crate::content::render_template(
            &template,
            &[
                ("review_artifact_paths", review_paths),
                ("tester_artifact_paths", tester_paths),
                ("diff_command", diff_command),
                ("learnings_dir", &learnings_dir.display().to_string()),
                (
                    "learnings_index_path",
                    &learnings_index_path.display().to_string(),
                ),
                (
                    "expertise_index_path",
                    &expertise_index_path.display().to_string(),
                ),
                ("has_learnings_index", ""),
                ("draft_path", draft_path),
            ],
        )
        .expect("learner template must render");

        assert!(
            rendered.contains(diff_command),
            "the learner prompt must include the complete-change diff command"
        );
        assert!(
            rendered.contains("/tmp/review-1/review.md")
                && rendered.contains("/tmp/review-2/review.md"),
            "the learner prompt must include every review round's reviewer artifacts"
        );
        assert!(
            rendered.contains("/tmp/tester-1/tester-results.json")
                && rendered.contains("/tmp/tester-2/tester-results.json"),
            "the learner prompt must include every review round's tester artifacts"
        );
        assert!(
            rendered.contains(draft_path),
            "the learner prompt must name the draft path"
        );
    }

    fn learner_ctx(handoff_only: &'static str) -> Vec<(&'static str, &'static str)> {
        vec![
            ("review_artifact_paths", "- (none)"),
            ("tester_artifact_paths", "- (none)"),
            ("diff_command", "git diff"),
            ("learnings_dir", "/tmp/learnings"),
            ("learnings_index_path", "/tmp/learnings/INDEX.md"),
            ("expertise_index_path", "/tmp/expertise/INDEX.md"),
            ("has_learnings_index", ""),
            ("draft_path", "/tmp/draft.json"),
            ("handoff_only", handoff_only),
        ]
    }

    #[test]
    fn post_land_learner_retry_is_handoff_only() {
        // A retry whose Merge Candidate has already merged runs handoff-only, and
        // a handoff-only run may not write expertise.
        assert!(learner_is_handoff_only(true));
        assert!(!learner_is_handoff_only(false));
        assert!(
            !learner_expertise_writable(true),
            "handoff-only denies expertise writes"
        );
        assert!(
            learner_expertise_writable(false),
            "a pre-land run may refine expertise"
        );

        // The handoff-only branch of the learner prompt forbids commits and
        // expertise changes; the normal branch keeps the expertise instructions.
        let resolver = ContentResolver::new(None);
        let template = resolver.resolve_content("prompts/learner-user.md").unwrap();
        let handoff_only = crate::content::render_template(&template, &learner_ctx("yes")).unwrap();
        assert!(handoff_only.contains("post-land handoff-only run"));
        assert!(handoff_only.contains("will be discarded"));
        let normal = crate::content::render_template(&template, &learner_ctx("")).unwrap();
        assert!(normal.contains("Update expertise"));
        assert!(!normal.contains("post-land handoff-only run"));
    }

    #[test]
    fn post_land_expertise_proposal_is_non_corrective_followup() {
        let follow_up =
            expertise_proposal_follow_up("expertise-retry", "Capture retry cap knowledge");
        assert!(
            !follow_up.corrective,
            "an expertise proposal is never corrective"
        );
        assert!(follow_up.corrective_context.is_none());

        // The corrective host gate keeps such a proposal Observation-only, so it
        // can never become autonomously executable Work.
        let tmp = tempfile::TempDir::new().unwrap();
        assert!(
            !crate::follow_up::classify_follow_up(tmp.path(), &follow_up).is_corrective()
        );
    }

    #[test]
    fn learner_prompt_requires_bounded_authoritative_corrective_context() {
        let resolver = ContentResolver::new(None);
        let template = resolver
            .resolve_content("prompts/learner-user.md")
            .expect("learner-user.md must resolve");

        for required in [
            "authoritative",
            "violated",
            "evidence is concrete",
            "scope is bounded",
            "verification is deterministic",
            "No consequential product, interface, architecture, security, or permission",
        ] {
            assert!(
                template.contains(required),
                "corrective criteria must instruct on {required:?}; prompt:\n{template}"
            );
        }
    }

    #[test]
    fn auth_suspend_posts_reauth_notification() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = WorkModelStore::new(tmp.path());
        let mut item = WorkItem {
            id: "work-1".to_string(),
            title: "Notify test".to_string(),
            planning_context: None,
            instructions: None,
            abandonment: None,
            post_merge_review_fix_depth: None,
            attempts: Vec::new(),
            merge_candidates: Vec::new(),
            ..Default::default()
        };
        item.add_initial_attempt("attempt-1").unwrap();
        store.create_work_item(&item).unwrap();

        let calls: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
        let calls_clone = Arc::clone(&calls);
        let notify = move |title: &str, body: &str| {
            calls_clone
                .lock()
                .unwrap()
                .push((title.to_string(), body.to_string()));
        };

        mark_task_failed_attempt_needs_user(
            &store,
            tmp.path(),
            "work-1",
            "attempt-1",
            "attempt-1-write-1",
            Some("Auth token expired"),
            &notify,
        )
        .unwrap();

        let notifications = calls.lock().unwrap();
        assert_eq!(
            notifications.len(),
            1,
            "should post exactly one notification on auth suspend"
        );
        assert_eq!(notifications[0].0, "Fluent");
        assert!(
            notifications[0].1.contains("re-authenticate"),
            "notification should mention re-authentication: {}",
            notifications[0].1
        );
    }

    #[test]
    fn non_auth_suspend_skips_notification() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = WorkModelStore::new(tmp.path());
        let mut item = WorkItem {
            id: "work-1".to_string(),
            title: "Notify test".to_string(),
            planning_context: None,
            instructions: None,
            abandonment: None,
            post_merge_review_fix_depth: None,
            attempts: Vec::new(),
            merge_candidates: Vec::new(),
            ..Default::default()
        };
        item.add_initial_attempt("attempt-1").unwrap();
        store.create_work_item(&item).unwrap();

        let calls: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
        let calls_clone = Arc::clone(&calls);
        let notify = move |title: &str, body: &str| {
            calls_clone
                .lock()
                .unwrap()
                .push((title.to_string(), body.to_string()));
        };

        mark_task_failed_attempt_needs_user(
            &store,
            tmp.path(),
            "work-1",
            "attempt-1",
            "attempt-1-write-1",
            None,
            &notify,
        )
        .unwrap();

        let notifications = calls.lock().unwrap();
        assert_eq!(
            notifications.len(),
            0,
            "should not post a notification on non-auth suspend"
        );
    }

    #[test]
    fn learner_sandbox_confines_expertise_handoff_and_git_writes() {
        let tmp = tempfile::TempDir::new().unwrap();
        let workspace = tmp.path().join("workspace");
        let expertise_dir = workspace.join(".fluent/expertise");
        fs::create_dir_all(&expertise_dir).unwrap();

        let resolver = ContentResolver::new(None);
        let common_git_dir = workspace.join(".git");
        fs::create_dir_all(&common_git_dir).unwrap();

        // The managed handoff surface lives under the project's work area, not
        // in the candidate workspace.
        let handoff_dir = tmp
            .path()
            .join(".fluent/work/artifacts/work-1/attempt-1/learner");
        fs::create_dir_all(&handoff_dir).unwrap();
        let work_model_dir = tmp.path().join(".fluent/work/items");
        let observations_dir = tmp.path().join(".fluent/observations");

        let review_dir = tmp.path().join("reviews");
        fs::create_dir_all(&review_dir).unwrap();
        let readable_roots = vec![review_dir.clone(), workspace.clone()];

        let (_sandbox, profile) = build_coder_sandbox_with_writable_and_read_only_roots(
            CoderKind::Claude,
            &resolver,
            &expertise_dir,
            &[handoff_dir.clone(), common_git_dir.clone()],
            &readable_roots,
        )
        .unwrap();

        let profile = profile.expect("sandbox profile should be present");
        let content = fs::read_to_string(&profile.path).unwrap();

        let write_grant =
            |p: &Path| format!("(allow file-write* (subpath \"{}\"))", p.to_string_lossy());

        assert!(
            content.contains(&write_grant(&expertise_dir)),
            "the Learner may write .fluent/expertise; profile:\n{content}"
        );
        assert!(
            content.contains(&write_grant(&handoff_dir)),
            "the Learner may write the managed handoff surface; profile:\n{content}"
        );
        assert!(
            content.contains(&write_grant(&common_git_dir)),
            "the Learner may write Git metadata; profile:\n{content}"
        );
        assert!(
            content.contains(&format!(
                "(allow file-read*  (subpath \"{}\"))",
                workspace.to_string_lossy()
            )),
            "the Learner may read the workspace; profile:\n{content}"
        );
        assert!(
            !content.contains(&write_grant(&workspace)),
            "the Learner may not write the whole workspace; profile:\n{content}"
        );
        assert!(
            !content.contains(&write_grant(&work_model_dir)),
            "the Learner may not write the Work model; profile:\n{content}"
        );
        assert!(
            !content.contains(&write_grant(&observations_dir)),
            "the Learner may not write the Observation backlog; profile:\n{content}"
        );

        let handoff_readable_roots = vec![
            review_dir,
            workspace.clone(),
            expertise_dir.clone(),
            common_git_dir.clone(),
        ];
        let handoff_profile = os::render_profile_for_access_for_coder_with_denied_writes(
            &resolver,
            "/Users/test",
            &[handoff_dir.clone()],
            &handoff_readable_roots,
            &[workspace.clone(), common_git_dir.clone()],
            CoderKind::Claude,
        )
        .unwrap();
        let handoff_content = fs::read_to_string(handoff_profile.path).unwrap();
        assert!(handoff_content.contains(&write_grant(&handoff_dir)));
        assert!(!handoff_content.contains(&write_grant(&expertise_dir)));
        assert!(!handoff_content.contains(&write_grant(&common_git_dir)));
        assert!(handoff_content.contains(&format!(
            "(allow file-read*  (subpath \"{}\"))",
            common_git_dir.to_string_lossy()
        )));
        assert!(handoff_content.contains(&format!(
            "(deny file-write* (subpath \"{}\"))",
            workspace.to_string_lossy()
        )));
        assert!(!handoff_content.contains(
            "(allow file-write* (subpath \"/private/var/folders\"))"
        ));
    }
}
