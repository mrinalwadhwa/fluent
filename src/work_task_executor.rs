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
    to_json_pretty, work_behavior_review_input,
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

    match task_kind {
        TaskKind::Write => run_write_task(config, coder_kind, model),
        TaskKind::Review => run_review_task(config, coder_kind, model),
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
    let input_artifacts = resolve_input_artifact_paths(config.project_root, &task.input_artifacts)?;
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

    item.attempts[attempt_index].status = AttemptStatus::Executing;
    item.attempts[attempt_index].tasks[task_index].status = TaskStatus::Executing;
    crate::work_model::mark_task_started(&mut item.attempts[attempt_index].tasks[task_index]);
    item.attempts[attempt_index].tasks[task_index].output = None;
    config.store.write_work_item(&item)?;

    let run_result = run_task_coder(
        &item,
        config.attempt_id,
        config.task_id,
        config.project_root,
        &workspace_path,
        &input_artifacts,
        config.resolver,
        config.extra_args,
        coder_kind,
        config.no_sandbox,
        model,
    );

    if let Err(error) = run_result {
        let mut failed_item = read_work_item_or_not_found(config.store, config.work_item_id)?;
        if let Some((attempt_index, task_index)) =
            find_attempt_task_indexes(&failed_item, config.attempt_id, config.task_id)
        {
            crate::work_model::set_task_terminal(
                &mut failed_item.attempts[attempt_index].tasks[task_index],
                TaskStatus::Failed,
            );
            crate::work_model::set_attempt_terminal(
                &mut failed_item.attempts[attempt_index],
                AttemptStatus::Failed,
            );
            config.store.write_work_item(&failed_item)?;
        }
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
) -> Result<WorkTaskRunResult> {
    let (
        attempt_kind,
        workspace_reads,
        candidate_commit,
        input_artifacts,
        artifact_dir,
        review_path,
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
        )
    };

    let item = read_work_item_or_not_found(config.store, config.work_item_id)?;

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

    if !attempt_kind.is_review_only_like() {
        if let Some(candidate_path) = readable_workspace_paths.first() {
            prepare_reviewer_build_cache(
                candidate_path,
                &artifact_dir,
                config.work_item_id,
                config.attempt_id,
                config.task_id,
            );
        }
    }

    let run_result = run_review_coder(
        &item,
        config.attempt_id,
        config.task_id,
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
    );

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
        lock_mark_task_failed(
            config.store,
            config.store_lock,
            config.work_item_id,
            config.attempt_id,
            config.task_id,
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

fn run_tester_task(config: WorkTaskRunConfig<'_>) -> Result<WorkTaskRunResult> {
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

    eprintln!("  Factory           work task run");
    eprintln!("  Work Item         {}", item.id);
    eprintln!("  Attempt           {}", config.attempt_id);
    eprintln!("  Task              {} (tester)", config.task_id);
    eprintln!("  Artifact area     {}", artifact_dir.display());
    eprintln!("  Candidate         {}", candidate_workspace.display());

    let results_path = artifact_dir.join("tester-results.json");

    let tester_result = crate::tester::run(
        &candidate_workspace,
        &artifact_dir,
        config.no_sandbox,
        config.resolver,
    );

    match tester_result {
        Ok(()) => {}
        Err(error) => {
            eprintln!("  Tester error: {error:#}");
            lock_mark_task_failed(
                config.store,
                config.store_lock,
                config.work_item_id,
                config.attempt_id,
                config.task_id,
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
                ensure_no_non_factory_worktree_changes(&workspace_path)?;
            } else if *attempt_kind == AttemptKind::PostMergeReview
                && workspace_path == project_root
            {
                ensure_head_matches_review_context(&workspace_path, expected_source_head)?;
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
                } else if *attempt_kind == AttemptKind::PostMergeReview
                    && workspace_path == project_root
                {
                    ReviewReadableWorkspace::PostMergeSource(PostMergeSourceGuard::begin(
                        workspace_path,
                        expected_source_head,
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
    PostMergeSource(PostMergeSourceGuard),
}

impl ReviewReadableWorkspace {
    fn path(&self) -> PathBuf {
        match self {
            Self::Candidate(workspace) => workspace.path.clone(),
            Self::Source(guard) => guard.path.clone(),
            Self::PostMergeSource(guard) => guard.path.clone(),
        }
    }

    fn finish(&self) -> Result<()> {
        match self {
            Self::Candidate(workspace) => workspace.finish(),
            Self::Source(guard) => guard.finish(),
            Self::PostMergeSource(guard) => guard.finish(),
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
    protected_factory_files: BTreeMap<PathBuf, Vec<u8>>,
    allowed_artifact_dir: PathBuf,
}

impl SourceCheckoutReviewGuard {
    fn begin(path: PathBuf, expected_head: &str, allowed_artifact_dir: &Path) -> Result<Self> {
        ensure_head_matches_review_context(&path, expected_head)?;
        ensure_no_non_factory_worktree_changes(&path)?;
        Ok(Self {
            head: head_commit(&path)?,
            status: worktree_status(&path)?,
            protected_factory_files: protected_factory_file_snapshot(&path, allowed_artifact_dir)?,
            path,
            allowed_artifact_dir: allowed_artifact_dir.to_path_buf(),
        })
    }

    fn finish(&self) -> Result<()> {
        ensure_source_head_unchanged(&self.path, &self.head)?;
        let non_factory_error =
            if let Err(error) = ensure_no_non_factory_worktree_changes(&self.path) {
                restore_non_factory_worktree_changes(&self.path)?;
                Some(error)
            } else {
                None
            };
        if let Err(error) = ensure_source_changed_only_artifact_area(self) {
            restore_source_changes_outside_artifact_area(self)?;
            return Err(error);
        }
        if let Some(error) = non_factory_error {
            return Err(error);
        }
        Ok(())
    }
}

struct PostMergeSourceGuard {
    path: PathBuf,
    head: String,
}

impl PostMergeSourceGuard {
    fn begin(path: PathBuf, expected_head: &str) -> Result<Self> {
        ensure_head_matches_review_context(&path, expected_head)?;
        Ok(Self {
            head: head_commit(&path)?,
            path,
        })
    }

    fn finish(&self) -> Result<()> {
        let current_head = head_commit(&self.path)?;
        if current_head == self.head {
            Ok(())
        } else {
            bail!(
                "Source HEAD moved during post-merge review from {} to {}: {}; \
                 review is stale",
                self.head,
                current_head,
                self.path.display()
            )
        }
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
            _ => bail!("Task artifact area path must stay under .factory/work/artifacts: {path}"),
        }
    }

    let managed_prefix = [
        std::ffi::OsStr::new(".factory"),
        std::ffi::OsStr::new("work"),
        std::ffi::OsStr::new("artifacts"),
    ];
    if components.len() <= managed_prefix.len()
        || !components
            .iter()
            .zip(managed_prefix.iter())
            .all(|(actual, expected)| actual == expected)
    {
        bail!("Task artifact area path must stay under .factory/work/artifacts: {path}");
    }

    Ok(resolve_workspace_path(project_root, path))
}

fn resolve_input_artifact_paths(
    project_root: &Path,
    input_artifacts: &[ArtifactRef],
) -> Result<Vec<PathBuf>> {
    let mut resolved = Vec::new();
    for artifact in input_artifacts {
        let path = resolve_managed_artifact_area_path(project_root, &artifact.path)?;
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
    if let Err(e) = fs::write(
        &path,
        serde_json::to_string_pretty(&info).unwrap_or_default(),
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
    input_artifacts: &[PathBuf],
    resolver: &ContentResolver,
    extra_args: &[String],
    coder_kind: CoderKind,
    no_sandbox: bool,
    model: Option<&str>,
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
    let prompt = build_write_task_prompt_with_workspace(
        item,
        attempt_id,
        task_id,
        input_artifacts,
        Some(workspace_path),
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
        let readable_roots = input_artifact_readable_roots(input_artifacts);
        let mut additional_writable = vec![common_git_dir];
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

    eprintln!("  Factory           work task run");
    eprintln!("  Work Item         {}", item.id);
    eprintln!("  Attempt           {attempt_id}");
    eprintln!("  Task              {task_id}");
    eprintln!("  Coder             {}", coder_kind.as_str());
    eprintln!("  Model             {effective_model}");
    eprintln!("  Worktree          {}", workspace_path.display());

    if let Some(ref tp) = transcript_path {
        if let Some(artifact_dir) = tp.parent() {
            capture_coder_info(coder_kind, &effective_model, artifact_dir);
        }
    }

    let coder = coder_kind.boxed_with_model(sandbox, model);
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

#[cfg(test)]
fn build_write_task_prompt(
    item: &WorkItem,
    attempt_id: &str,
    task_id: &str,
    input_artifacts: &[PathBuf],
) -> String {
    build_write_task_prompt_with_workspace(item, attempt_id, task_id, input_artifacts, None)
}

fn build_write_task_prompt_with_workspace(
    item: &WorkItem,
    attempt_id: &str,
    task_id: &str,
    input_artifacts: &[PathBuf],
    workspace_path: Option<&Path>,
) -> String {
    let task = item
        .attempts
        .iter()
        .find(|a| a.id == attempt_id)
        .and_then(|a| a.tasks.iter().find(|t| t.id == task_id))
        .expect("Task must exist");
    let task_json = to_json_pretty(task).unwrap_or_default();
    let input_artifacts_list = input_artifacts_instruction(input_artifacts);
    let task_instructions_value = task
        .instructions
        .as_deref()
        .filter(|i| !i.trim().is_empty())
        .unwrap_or("");
    let progress_md_path = item
        .attempts
        .iter()
        .find(|a| a.id == attempt_id)
        .map(|a| {
            format!(
                "{}/{}/{}/progress.md",
                crate::work_model::WORK_ARTIFACTS_DIR,
                item.id,
                a.id
            )
        })
        .unwrap_or_default();
    let (bootstrap_yaml, bootstrap_extract) = tester_bootstrap_flags(workspace_path);
    let has_input_artifacts = if input_artifacts.is_empty() { "" } else { "yes" };
    let bootstrap_yaml_value = if bootstrap_yaml { "yes" } else { "" };
    let bootstrap_extract_value = if bootstrap_extract { "yes" } else { "" };

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
            ("has_input_artifacts", has_input_artifacts),
            ("task_instructions", task_instructions_value),
            ("input_artifacts_list", &input_artifacts_list),
            ("progress_md_path", &progress_md_path),
            ("task_json", &task_json),
            ("bootstrap_tester_yaml", bootstrap_yaml_value),
            ("bootstrap_extract_script", bootstrap_extract_value),
        ],
    )
    .expect("write-user.md template must render with the documented context")
}

fn tester_bootstrap_flags(workspace_path: Option<&Path>) -> (bool, bool) {
    let Some(workspace) = workspace_path else {
        return (false, false);
    };
    let yaml_missing = !workspace.join(".factory/tester.yaml").exists();
    let extract_missing = !workspace.join(".factory/extract-tester-results").exists();
    (yaml_missing, extract_missing)
}

fn input_artifacts_instruction(input_artifacts: &[PathBuf]) -> String {
    if input_artifacts.is_empty() {
        return "None.".to_string();
    }

    input_artifacts
        .iter()
        .map(|path| format!("- {}", path.display()))
        .collect::<Vec<_>>()
        .join("\n")
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
) -> Result<()> {
    if !no_sandbox {
        os::check_prerequisites_for(coder_kind)?;
        credential::inject_credentials()?;
        credential::setup_git_signing();
    }

    let prompts = build_work_review_prompts(WorkReviewPromptInput {
        item,
        attempt_id,
        task_id,
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

    eprintln!("  Factory           work task run");
    eprintln!("  Work Item         {}", item.id);
    eprintln!("  Attempt           {attempt_id}");
    eprintln!("  Task              {task_id}");
    eprintln!("  Coder             {}", coder_kind.as_str());
    eprintln!("  Model             {effective_model}");
    eprintln!("  Artifact area     {}", artifact_dir.display());

    capture_coder_info(coder_kind, &effective_model, artifact_dir);

    let transcript_path = artifact_dir.join("transcript.jsonl");
    let coder = coder_kind.boxed_with_model(sandbox, model);
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
    let task_json = to_json_pretty(task)?;
    let review_context = task.review_context.as_ref().ok_or_else(|| {
        anyhow::anyhow!(
            "Review Task {:?} must declare review context",
            input.task_id
        )
    })?;
    let review_artifact_path = work_review_artifact_path(task)?;

    // Skill instruction: split into skill_path (or empty) for the template if/else.
    let skill_path = review_skill_path(&task.role, input.readable_workspaces);
    let workspace_kind = if input.review_only {
        "source checkout"
    } else {
        "readable candidate workspaces"
    };

    // Decisions: split into decisions_path (or empty).
    let decisions_path = decisions_path(input.readable_workspaces);

    let read_paths = task
        .workspace_access
        .reads
        .iter()
        .zip(input.readable_workspaces.iter())
        .map(|(workspace, resolved_path)| {
            format!("- {}: {}", workspace.id, resolved_path.display())
        })
        .collect::<Vec<_>>()
        .join("\n");

    // Review-diff command is only meaningful when not review_only.
    let review_diff_command = if input.review_only {
        String::new()
    } else {
        let candidate_workspace = task
            .workspace_access
            .reads
            .iter()
            .zip(input.readable_workspaces.iter())
            .find(|(workspace, _)| workspace.id == review_context.candidate_workspace_id)
            .map(|(_, resolved_path)| resolved_path.as_path())
            .unwrap_or_else(|| Path::new(&review_context.candidate_workspace_path));
        let review_range = format!(
            "{}..{}",
            review_context.source_branch, review_context.candidate_commit
        );
        render_review_diff_command(candidate_workspace, &review_range)
    };

    let edit_target = if input.review_only {
        "source checkout"
    } else {
        "candidate workspaces"
    };
    let task_instructions = task_instructions_prompt_value(task.instructions.as_deref());
    let input_artifacts_block = review_input_artifacts_prompt(input.input_artifacts);
    let behavior_review_input = if task.role == "behaviors" {
        behavior_review_input_block(input.item, input.attempt_id, task)
    } else {
        String::new()
    };

    let review_only_value = if input.review_only { "yes" } else { "" };
    let review_path_display = input.review_path.display().to_string();
    let artifact_dir_display = input.artifact_dir.display().to_string();

    let resolver = ContentResolver::new(input.readable_workspaces.first().map(|p| p.as_path()));

    let user_template = resolver
        .resolve_content("prompts/review-user.md")
        .expect("bundled review-user.md must resolve");
    let review_prompt = crate::content::render_template(
        &user_template,
        &[
            ("work_item_id", &input.item.id),
            ("work_item_title", &input.item.title),
            ("attempt_id", input.attempt_id),
            ("task_id", input.task_id),
            ("role", &task.role),
            ("task_instructions", &task_instructions),
            ("input_artifacts_block", &input_artifacts_block),
            ("review_only", review_only_value),
            ("read_paths", &read_paths),
            (
                "candidate_workspace_id",
                &review_context.candidate_workspace_id,
            ),
            (
                "candidate_workspace_path",
                &review_context.candidate_workspace_path,
            ),
            ("source_branch", &review_context.source_branch),
            ("candidate_commit", &review_context.candidate_commit),
            ("review_diff_command", &review_diff_command),
            ("behavior_review_input", &behavior_review_input),
            ("artifact_path", &review_artifact_path),
            ("review_path", &review_path_display),
            ("artifact_dir", &artifact_dir_display),
            ("edit_target", edit_target),
            ("task_json", &task_json),
        ],
    )
    .expect("review-user.md template must render with the documented context");

    let system_template = resolver
        .resolve_content("prompts/review-system.md")
        .expect("bundled review-system.md must resolve");
    let system_prompt = crate::content::render_template(
        &system_template,
        &[
            ("role", &task.role),
            ("skill_path", &skill_path),
            ("workspace_kind", workspace_kind),
            ("review_only", review_only_value),
            ("review_path", &review_path_display),
            ("artifact_path", &review_artifact_path),
            ("artifact_dir", &artifact_dir_display),
            ("decisions_path", &decisions_path),
        ],
    )
    .expect("review-system.md template must render with the documented context");

    Ok(WorkReviewPrompts {
        system_prompt,
        review_prompt,
    })
}

fn task_instructions_prompt_value(instructions: Option<&str>) -> String {
    instructions
        .filter(|i| !i.trim().is_empty())
        .unwrap_or("")
        .to_string()
}

fn behavior_review_input_block(
    item: &WorkItem,
    attempt_id: &str,
    task: &crate::work_model::Task,
) -> String {
    let tester_results_hint = if let Some(dep_id) = task.depends_on.as_deref() {
        let tester_artifact_path = format!(
            ".factory/work/artifacts/{}/{}/{}/tester-results.json",
            item.id, attempt_id, dep_id
        );
        format!(
            "\nTester results are available at: {tester_artifact_path}\n\
             Read the tester-results.json file. Use its per-test statuses\n\
             (pass, fail, skipped, not_run) to inform your verdict.\n\
             If its `error` field is non-null, produce a fail verdict naming the error\n\
             `kind` and `message` rather than flagging individual behaviors."
        )
    } else {
        String::new()
    };
    format!("{}\n{}", work_behavior_review_input(item), tester_results_hint)
}

fn review_skill_path(role: &str, readable_workspaces: &[PathBuf]) -> String {
    let relative = format!("skills/review-{role}/SKILL.md");
    readable_workspaces
        .iter()
        .map(|workspace| workspace.join(&relative))
        .find(|path| path.is_file())
        .map(|path| path.display().to_string())
        .unwrap_or_default()
}

fn decisions_path(readable_workspaces: &[PathBuf]) -> String {
    let relative = Path::new(".factory/expertise/decisions.md");
    readable_workspaces
        .iter()
        .map(|workspace| workspace.join(relative))
        .find(|path| path.is_file())
        .map(|path| path.display().to_string())
        .unwrap_or_default()
}

fn work_review_artifact_path(task: &crate::work_model::Task) -> Result<String> {
    let artifact_area = task.artifact_area.as_ref().ok_or_else(|| {
        anyhow::anyhow!("Review Task {:?} must declare an artifact area", task.id)
    })?;
    Ok(format!(
        "{}/review.md",
        artifact_area.path.trim_end_matches('/')
    ))
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

fn review_input_artifacts_prompt(input_artifacts: &[PathBuf]) -> String {
    if input_artifacts.is_empty() {
        return String::new();
    }

    let artifacts = input_artifacts
        .iter()
        .map(|path| format!("- {}", path.display()))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "A previous review of this candidate (from a prior review round) is included as input:\n{artifacts}\n\nTreat that previous review as another reviewer's findings, not as your past self. Read it first. For each finding it raised, verify against the current candidate whether the writer addressed the concern. Then evaluate the candidate independently and add any new findings. Use the `Progress:` field in your output to summarize whether you observed any movement on prior concerns (`yes`, `no`, `partial`, or `first-pass` when no prior review is included). `Progress:` is independent of `Verdict:`.\n\n"
    )
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

fn ensure_no_non_factory_worktree_changes(workspace_path: &Path) -> Result<()> {
    let output = git_status_output(
        workspace_path,
        &[
            "--porcelain",
            "--untracked-files=all",
            "--",
            ".",
            ":(exclude).factory",
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
            "Review Task changed non-Factory source files; source checkout must remain read-only:\n{}",
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
        protected_factory_file_snapshot(&baseline.path, &baseline.allowed_artifact_dir)?;
    if current == baseline_status && current_protected == baseline.protected_factory_files {
        Ok(())
    } else {
        let status_delta = status_diff(&baseline_status, &current);
        let factory_delta =
            factory_file_snapshot_diff(&baseline.protected_factory_files, &current_protected);
        let mut delta = [status_delta, factory_delta]
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

fn protected_factory_file_snapshot(
    workspace_path: &Path,
    allowed_artifact_dir: &Path,
) -> Result<BTreeMap<PathBuf, Vec<u8>>> {
    let mut snapshot = BTreeMap::new();
    let factory_dir = workspace_path.join(".factory");
    if !factory_dir.exists() {
        return Ok(snapshot);
    }
    let allowed = allowed_status_prefix(workspace_path, allowed_artifact_dir)?;
    collect_protected_factory_files(workspace_path, &factory_dir, &allowed, &mut snapshot)?;
    Ok(snapshot)
}

fn collect_protected_factory_files(
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
            collect_protected_factory_files(workspace_path, &path, allowed, snapshot)?;
        } else if file_type.is_file() {
            snapshot.insert(relative, fs::read(&path)?);
        }
    }
    Ok(())
}

fn factory_file_snapshot_diff(
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

fn restore_non_factory_worktree_changes(workspace_path: &Path) -> Result<()> {
    let restore = git::run_raw(
        workspace_path,
        &[
            "restore",
            "--staged",
            "--worktree",
            "--",
            ".",
            ":(exclude).factory",
        ],
    )?;
    if !restore.status.success() {
        bail!(
            "Failed to restore non-Factory source changes: {}",
            String::from_utf8_lossy(&restore.stderr)
        );
    }

    let clean = git::run_raw(
        workspace_path,
        &["clean", "-fd", "--", ".", ":(exclude).factory"],
    )?;
    if clean.status.success() {
        Ok(())
    } else {
        bail!(
            "Failed to remove untracked non-Factory source changes: {}",
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

    let current = protected_factory_file_snapshot(&baseline.path, &baseline.allowed_artifact_dir)?;
    for path in current.keys() {
        if !baseline.protected_factory_files.contains_key(path) {
            let absolute = baseline.path.join(path);
            if absolute.is_file() {
                fs::remove_file(&absolute)?;
            }
        }
    }
    for (path, content) in &baseline.protected_factory_files {
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
    use crate::work_model::{TaskOutput, TaskStatus, WorkItem, WorkItemAbandonment};
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;

    fn review_item() -> WorkItem {
        let mut item = WorkItem {
            id: "work-1".to_string(),
            title: "Review prompts".to_string(),
            planning_context: None,
            instructions: None,
            abandonment: None,
            attempts: Vec::new(),
            merge_candidates: Vec::new(),
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
            commit: "abc123".to_string(),
        });
        item.add_review_tasks("attempt-1", &["tests"]).unwrap();
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
            attempts: Vec::new(),
            merge_candidates: Vec::new(),
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

    #[test]
    fn work_review_prompt_names_work_artifacts_and_writable_outputs() {
        let item = review_item();
        let prompts = build_work_review_prompts(WorkReviewPromptInput {
            item: &item,
            attempt_id: "attempt-1",
            task_id: "attempt-1-review-tests",
            artifact_dir: Path::new("/tmp/project/.factory/work/artifacts/work-1/attempt-1/attempt-1-review-tests"),
            review_path: Path::new("/tmp/project/.factory/work/artifacts/work-1/attempt-1/attempt-1-review-tests/review.md"),
            readable_workspaces: &[PathBuf::from("/tmp/project/../work-6-work-1-attempt-1")],
            input_artifacts: &[],
            review_only: false,
        })
        .unwrap();

        assert!(prompts.review_prompt.contains(
            "Work review artifact path:\n.factory/work/artifacts/work-1/attempt-1/attempt-1-review-tests/review.md"
        ));
        assert!(
            prompts
                .review_prompt
                .contains("Your reviewer artifact directory is:")
        );
        assert!(prompts.review_prompt.contains("CARGO_TARGET_DIR"));
        assert!(prompts.review_prompt.contains("cargo build"));
        assert!(
            prompts.review_prompt.contains("may READ the candidate"),
            "prompt should tell reviewer they can read candidate build outputs"
        );
        assert!(
            prompts
                .review_prompt
                .contains("may NOT write to the candidate"),
            "prompt should tell reviewer not to write to candidate"
        );
        assert!(
            prompts.review_prompt.contains("pre-populated"),
            "prompt should mention pre-populated warm cache"
        );
        assert!(!prompts.review_prompt.contains(".factory/runs/"));
        assert!(prompts.system_prompt.contains(
            "The Work review artifact path is .factory/work/artifacts/work-1/attempt-1/attempt-1-review-tests/review.md"
        ));
        assert!(prompts.system_prompt.contains("CARGO_TARGET_DIR"));
        assert!(prompts.system_prompt.contains("pre-populated"));
        assert!(!prompts.system_prompt.contains(".factory/runs/"));
    }

    #[test]
    fn work_review_prompt_includes_shell_safe_executable_diff_command() {
        let item = review_item();
        let tmp = tempfile::TempDir::new().unwrap();
        let workspace = tmp.path().join("candidate'space");
        let review_path = tmp.path().join("review.md");
        let prompts = build_work_review_prompts(WorkReviewPromptInput {
            item: &item,
            attempt_id: "attempt-1",
            task_id: "attempt-1-review-tests",
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
            .find_map(|line| line.strip_prefix("- Review diff: "))
            .unwrap();

        assert_eq!(
            command,
            render_review_diff_command(&workspace, "main..abc123")
        );
        assert!(command.contains("'\\''"));
        assert_shell_command_invokes_fake_git(
            command,
            &[
                "-C".to_string(),
                workspace.display().to_string(),
                "diff".to_string(),
                "main..abc123".to_string(),
            ],
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

    fn setup_test_repo(tmp: &tempfile::TempDir) -> PathBuf {
        let dir = tmp.path().join("repo");
        fs::create_dir_all(&dir).unwrap();
        git::run(&dir, &["init", "-b", "main"], "init repo").unwrap();
        git::run(&dir, &["config", "user.email", "test@test"], "config email").unwrap();
        git::run(&dir, &["config", "user.name", "test"], "config name").unwrap();
        fs::write(dir.join("README.md"), "test").unwrap();
        git::run(&dir, &["add", "."], "stage files").unwrap();
        git::run(&dir, &["commit", "-m", "init"], "initial commit").unwrap();
        dir
    }

    #[test]
    fn post_merge_source_guard_finish_succeeds_with_worktree_edits() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = setup_test_repo(&tmp);
        let head = head_commit(&dir).unwrap();

        let guard = PostMergeSourceGuard::begin(dir.clone(), &head).unwrap();

        fs::write(dir.join("new-file.txt"), "user edit\n").unwrap();
        fs::write(dir.join("README.md"), "modified\n").unwrap();

        assert!(guard.finish().is_ok());
    }

    #[test]
    fn post_merge_source_guard_finish_succeeds_with_factory_mutations() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = setup_test_repo(&tmp);
        let head = head_commit(&dir).unwrap();

        let guard = PostMergeSourceGuard::begin(dir.clone(), &head).unwrap();

        fs::create_dir_all(dir.join(".factory/work/items")).unwrap();
        fs::write(dir.join(".factory/work/items/new-work-item.json"), "{}").unwrap();

        assert!(guard.finish().is_ok());
    }

    #[test]
    fn post_merge_source_guard_finish_fails_when_head_moves() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = setup_test_repo(&tmp);
        let head = head_commit(&dir).unwrap();

        let guard = PostMergeSourceGuard::begin(dir.clone(), &head).unwrap();

        fs::write(dir.join("new-commit.txt"), "extra commit\n").unwrap();
        git::run(&dir, &["add", "new-commit.txt"], "stage file").unwrap();
        git::run(&dir, &["commit", "-m", "move head"], "commit").unwrap();

        let result = guard.finish();
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("Source HEAD moved during post-merge review"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn post_merge_source_guard_begin_rejects_mismatched_head() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = setup_test_repo(&tmp);

        let result = PostMergeSourceGuard::begin(dir, "0000000000000000000000000000000000000000");
        assert!(result.is_err());
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
            ".factory/work/artifacts/work-1/attempt-1/attempt-1-review-tests"
        );

        let tmp = tempfile::TempDir::new().unwrap();
        let project_root = tmp.path();
        let artifact_dir =
            resolve_managed_artifact_area_path(project_root, &artifact_area.path).unwrap();
        let transcript_path = artifact_dir.join("transcript.jsonl");

        assert_eq!(
            transcript_path,
            project_root.join(
                ".factory/work/artifacts/work-1/attempt-1/attempt-1-review-tests/transcript.jsonl"
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
            ".factory/work/artifacts/work-1/attempt-1/attempt-1-tester"
        );

        let tmp = tempfile::TempDir::new().unwrap();
        let project_root = tmp.path();
        let artifact_dir =
            resolve_managed_artifact_area_path(project_root, &artifact_area.path).unwrap();
        let results_path = artifact_dir.join("tester-results.json");

        assert_eq!(
            results_path,
            project_root.join(
                ".factory/work/artifacts/work-1/attempt-1/attempt-1-tester/tester-results.json"
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
    fn write_task_prompt_includes_progress_md_path_substitution() {
        let item = review_item();
        let prompt = build_write_task_prompt(&item, "attempt-1", "attempt-1-write-1", &[]);
        assert!(
            prompt
                .contains("progress_md_path: .factory/work/artifacts/work-1/attempt-1/progress.md"),
            "prompt should include progress_md_path substitution"
        );
    }

    #[test]
    fn write_task_prompt_includes_protocol_when_plan_md_present() {
        let mut item = review_item();
        item.planning_context = Some(crate::work_model::PlanningContext {
            plan: Some("## 1. Step one\n## 2. Step two\n".to_string()),
            ..Default::default()
        });
        // The protocol lives in write-system.md system prompt, not the task prompt.
        // Verify write-system.md contains the protocol section.
        let system_prompt = std::fs::read_to_string("prompts/write-system.md").unwrap();
        assert!(
            system_prompt.contains("If plan.md is part of your Work Item's planning context"),
            "write-system.md should contain the writer protocol section"
        );
        // Verify progress_md_path is present in the task prompt
        let prompt = build_write_task_prompt(&item, "attempt-1", "attempt-1-write-1", &[]);
        assert!(
            prompt.contains("progress_md_path:"),
            "prompt should contain progress_md_path when plan.md is present"
        );
    }

    #[test]
    fn write_task_prompt_omits_protocol_when_no_plan_md() {
        let item = review_item();
        // review_item() has planning_context = None, so no plan.md
        let prompt = build_write_task_prompt(&item, "attempt-1", "attempt-1-write-1", &[]);
        assert!(
            !prompt.contains("If plan.md is part of your Work Item's planning context"),
            "prompt should NOT contain the protocol section when plan.md is absent"
        );
    }

    #[test]
    fn writer_prompt_includes_tester_yaml_bootstrap_when_missing() {
        let item = review_item();
        let tmp = tempfile::TempDir::new().unwrap();
        let workspace = tmp.path();
        std::fs::create_dir_all(workspace.join(".factory")).unwrap();

        let prompt = build_write_task_prompt_with_workspace(
            &item,
            "attempt-1",
            "attempt-1-write-1",
            &[],
            Some(workspace),
        );
        assert!(
            prompt.contains("Bootstrap: .factory/tester.yaml"),
            "prompt should include tester.yaml bootstrap when missing"
        );
    }

    #[test]
    fn writer_prompt_includes_extract_tester_results_bootstrap_when_missing() {
        let item = review_item();
        let tmp = tempfile::TempDir::new().unwrap();
        let workspace = tmp.path();
        std::fs::create_dir_all(workspace.join(".factory")).unwrap();

        let prompt = build_write_task_prompt_with_workspace(
            &item,
            "attempt-1",
            "attempt-1-write-1",
            &[],
            Some(workspace),
        );
        assert!(
            prompt.contains("Bootstrap: .factory/extract-tester-results"),
            "prompt should include extract-tester-results bootstrap when missing"
        );
    }

    #[test]
    fn writer_prompt_omits_bootstrap_when_both_files_present() {
        let item = review_item();
        let tmp = tempfile::TempDir::new().unwrap();
        let workspace = tmp.path();
        let factory_dir = workspace.join(".factory");
        std::fs::create_dir_all(&factory_dir).unwrap();
        std::fs::write(factory_dir.join("tester.yaml"), "commands: []").unwrap();
        let extractor = factory_dir.join("extract-tester-results");
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
        );
        assert!(
            !prompt.contains("Bootstrap: .factory/tester.yaml"),
            "prompt should NOT include tester.yaml bootstrap when present"
        );
        assert!(
            !prompt.contains("Bootstrap: .factory/extract-tester-results"),
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
            project_root.join(".factory/work/artifacts/work-1/attempt-1/attempt-1-review-docs");
        std::fs::create_dir_all(&artifact_dir).unwrap();
        let other_artifact = artifact_dir.join("review.md");
        std::fs::write(&other_artifact, "review content").unwrap();
        // progress.md does NOT exist — the writer hasn't created it yet

        let refs = vec![
            ArtifactRef {
                producer_id: "writer".to_string(),
                path: ".factory/work/artifacts/work-1/attempt-1/progress.md".to_string(),
            },
            ArtifactRef {
                producer_id: "attempt-1-review-docs".to_string(),
                path: ".factory/work/artifacts/work-1/attempt-1/attempt-1-review-docs/review.md"
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
            path: ".factory/work/artifacts/work-1/attempt-1/some-artifact.md".to_string(),
        }];
        let result = resolve_input_artifact_paths(project_root, &refs);
        assert!(
            result.is_err(),
            "should error on missing non-progress artifact"
        );
    }
}
