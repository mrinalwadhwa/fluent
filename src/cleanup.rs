use anyhow::{Context, Result};
use std::collections::HashSet;
use std::fs;
use std::path::{Component, Path, PathBuf};

use crate::git;
use crate::review;
use crate::work_model::{
    Attempt, AttemptStatus, MergeCandidate, MergeCandidateMergeStatus, MergeReviewState, Task,
    TaskStatus, WORK_ARTIFACTS_DIR, WorkItem, WorkModelStore,
    resolve_expected_candidate_workspace_path,
};

#[derive(Debug, Clone)]
pub struct CleanupOptions {
    pub apply: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorktreeCleanup {
    None,
    WouldRemove(PathBuf),
    Removed(PathBuf),
    SkippedUnregistered(PathBuf),
    Missing(PathBuf),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkBranchCleanup {
    WouldRemove(String),
    Removed(String),
    Missing(String),
}

#[derive(Debug, Clone)]
pub enum WorkCleanupResult {
    WorkItem(WorkItemCleanupResult),
    OrphanArtifact(OrphanWorkArtifactCleanupResult),
}

#[derive(Debug, Clone)]
pub struct WorkItemCleanupResult {
    pub work_item_id: String,
    pub applied: bool,
    pub item_path: PathBuf,
    pub state_paths: Vec<PathBuf>,
    pub artifacts: Vec<PathBuf>,
    pub worktrees: Vec<WorktreeCleanup>,
    pub branches: Vec<WorkBranchCleanup>,
}

#[derive(Debug, Clone)]
pub struct OrphanWorkArtifactCleanupResult {
    pub work_item_id: String,
    pub applied: bool,
    pub artifact_root: PathBuf,
}

#[derive(Debug, Clone)]
pub struct StrandedReviewerWorktreeCleanupResult {
    pub path: PathBuf,
    pub work_item_id: String,
    pub attempt_id: String,
    pub reviewer: String,
    pub applied: bool,
}

pub fn cleanup_work_items(
    search_root: &Path,
    options: &CleanupOptions,
) -> Result<Vec<WorkCleanupResult>> {
    let source_root = cleanup_source_root(search_root)?;
    let store = WorkModelStore::new(&source_root);
    let work_items = store.list_work_items()?;
    let stored_work_item_ids = work_items
        .iter()
        .map(|work_item| work_item.id.clone())
        .collect::<HashSet<_>>();
    let candidates = cleanup_work_item_candidates(&source_root, work_items);
    let registered = registered_worktrees(&source_root)?;
    let mut results = Vec::new();

    for work_item in candidates {
        let plan = work_cleanup_plan(&source_root, &store, &work_item, &registered, options.apply)?;
        if options.apply {
            apply_work_item_cleanup(&plan)?;
        }
        results.push(WorkCleanupResult::WorkItem(plan));
    }

    for plan in
        orphan_work_artifact_cleanup_plans(&source_root, &stored_work_item_ids, options.apply)?
    {
        if options.apply {
            apply_orphan_work_artifact_cleanup(&plan)?;
        }
        results.push(WorkCleanupResult::OrphanArtifact(plan));
    }

    Ok(results)
}

pub fn cleanup_stranded_reviewer_worktrees(
    search_root: &Path,
    options: &CleanupOptions,
) -> Result<Vec<StrandedReviewerWorktreeCleanupResult>> {
    let source_root = cleanup_source_root(search_root)?;
    let store = WorkModelStore::new(&source_root);
    let sibling_root = source_root.parent().unwrap_or(&source_root);
    let entries = match fs::read_dir(sibling_root) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err).context("Failed to read sibling directory"),
    };

    let registered = registered_worktrees(&source_root)?;
    let mut results = Vec::new();
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some((work_item_id, attempt_id, reviewer)) = parse_reviewer_worktree_name(&name) else {
            continue;
        };

        if has_executing_merge_candidate(&store, &work_item_id) {
            continue;
        }

        let path = entry.path();
        if options.apply {
            if path_is_registered(&path, &registered) {
                let _ = remove_registered_worktree(&source_root, &path);
            }
            let _ = fs::remove_dir_all(&path);
        }
        results.push(StrandedReviewerWorktreeCleanupResult {
            path,
            work_item_id,
            attempt_id,
            reviewer,
            applied: options.apply,
        });
    }

    results.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(results)
}

fn parse_reviewer_worktree_name(name: &str) -> Option<(String, String, String)> {
    let rest = name.strip_prefix("review-")?;
    let (bytelen_str, rest) = rest.split_once('-')?;
    let bytelen: usize = bytelen_str.parse().ok()?;
    if rest.len() < bytelen {
        return None;
    }
    let work_item_id = &rest[..bytelen];
    let rest = rest.get(bytelen + 1..)?;
    for &reviewer in review::REVIEWERS {
        if let Some(attempt_id) = rest.strip_suffix(&format!("-{reviewer}"))
            && !attempt_id.is_empty()
        {
            return Some((
                work_item_id.to_string(),
                attempt_id.to_string(),
                reviewer.to_string(),
            ));
        }
    }
    None
}

fn has_executing_merge_candidate(store: &WorkModelStore, work_item_id: &str) -> bool {
    let Ok(item) = store.read_work_item(work_item_id) else {
        return false;
    };
    item.merge_candidates
        .iter()
        .any(|candidate| candidate.merge_state.status == MergeCandidateMergeStatus::Executing)
}

fn cleanup_source_root(search_root: &Path) -> Result<PathBuf> {
    let search_root = fs::canonicalize(search_root).unwrap_or_else(|_| search_root.to_path_buf());
    Ok(search_root)
}

fn cleanup_work_item_candidates(project_root: &Path, work_items: Vec<WorkItem>) -> Vec<WorkItem> {
    work_items
        .into_iter()
        .filter(|work_item| {
            work_item_is_cleanable(work_item)
                && !retains_for_follow_up_recovery(project_root, work_item)
        })
        .collect()
}

/// Whether a Work Item's landed learning recovery is still live, so cleanup must
/// retain its origin (Work Item, Attempt, Merge Candidate, worktree, and managed
/// artifacts). Recovery is live while a landed Attempt has no successful Learner
/// record, or any landed candidate has a durable failure, an incomplete post-land
/// journal, or a pending imported post-land operation.
fn retains_for_follow_up_recovery(project_root: &Path, work_item: &WorkItem) -> bool {
    work_item.merge_candidates.iter().any(|candidate| {
        if candidate.merge_state.status != MergeCandidateMergeStatus::Merged {
            return false;
        }
        let learning_succeeded = work_item
            .attempts
            .iter()
            .find(|attempt| attempt.id == candidate.attempt_id)
            .and_then(|attempt| attempt.learning.as_ref())
            .is_some_and(|learning| learning.is_succeeded());
        if !learning_succeeded || candidate.merge_state.follow_up_failure.is_some() {
            return true;
        }
        let operation_id =
            crate::follow_up::operation_id_for_candidate(&work_item.id, &candidate.id);
        has_incomplete_post_land_operation(project_root, &operation_id)
    })
}

/// Whether a recorded post-land operation exists but has not completed — an
/// incomplete journal or a pending imported operation awaiting replay.
fn has_incomplete_post_land_operation(project_root: &Path, operation_id: &str) -> bool {
    let dir = crate::follow_up::follow_ups_root(project_root).join(operation_id);
    if !dir.join("operation.json").exists() {
        return false;
    }
    match fs::read(dir.join("journal.json")) {
        Ok(bytes) => !serde_json::from_slice::<serde_json::Value>(&bytes)
            .ok()
            .and_then(|value| value.get("completed").and_then(|c| c.as_bool()))
            .unwrap_or(false),
        // An operation recorded without a completed journal is still pending.
        Err(_) => true,
    }
}

fn work_item_is_cleanable(work_item: &WorkItem) -> bool {
    if work_item.abandonment.is_some() {
        return work_item_has_no_active_execution(work_item);
    }

    !work_item.attempts.is_empty()
        && work_item.attempts.iter().all(attempt_is_terminal)
        && work_item
            .merge_candidates
            .iter()
            .all(merge_candidate_is_terminal)
}

fn work_item_has_no_active_execution(work_item: &WorkItem) -> bool {
    work_item.attempts.iter().all(|attempt| {
        !matches!(
            attempt.status,
            AttemptStatus::Executing | AttemptStatus::Reviewing
        ) && attempt
            .tasks
            .iter()
            .all(|task| task.status != TaskStatus::Executing)
    }) && work_item.merge_candidates.iter().all(|candidate| {
        candidate.merge_review_state != MergeReviewState::Reviewing
            && candidate.merge_state.status != MergeCandidateMergeStatus::Executing
    })
}

fn attempt_is_terminal(attempt: &Attempt) -> bool {
    matches!(
        attempt.status,
        AttemptStatus::Complete | AttemptStatus::Failed
    ) && !attempt.tasks.is_empty()
        && attempt.tasks.iter().all(task_is_terminal)
}

fn task_is_terminal(task: &Task) -> bool {
    matches!(task.status, TaskStatus::Complete | TaskStatus::Failed)
}

fn merge_candidate_is_terminal(candidate: &MergeCandidate) -> bool {
    matches!(
        candidate.merge_state.status,
        MergeCandidateMergeStatus::Merged | MergeCandidateMergeStatus::Failed
    ) || matches!(candidate.merge_review_state, MergeReviewState::Failed)
}

fn work_cleanup_plan(
    source_root: &Path,
    store: &WorkModelStore,
    work_item: &WorkItem,
    registered: &[PathBuf],
    apply: bool,
) -> Result<WorkItemCleanupResult> {
    let mut worktrees = Vec::new();
    let mut branches = Vec::new();

    for attempt in &work_item.attempts {
        for task in &attempt.tasks {
            for workspace in task
                .workspace_access
                .writes
                .iter()
                .chain(task.workspace_access.reads.iter())
            {
                let Ok(path) = resolve_expected_candidate_workspace_path(
                    source_root,
                    &workspace.path,
                    &work_item.id,
                    &attempt.id,
                    "Work cleanup",
                ) else {
                    continue;
                };
                push_unique_worktree(
                    &mut worktrees,
                    cleanup_managed_worktree(source_root, &path, registered, apply)?,
                );
            }

            let branch_name = format!("work/{}/{}/{}", work_item.id, attempt.id, task.id);
            push_unique_branch(
                &mut branches,
                cleanup_work_branch(source_root, &branch_name, apply)?,
            );
        }
    }

    for candidate in &work_item.merge_candidates {
        if let Ok(path) = resolve_expected_candidate_workspace_path(
            source_root,
            &candidate.source_workspace.path,
            &work_item.id,
            &candidate.attempt_id,
            "Work cleanup",
        ) {
            push_unique_worktree(
                &mut worktrees,
                cleanup_managed_worktree(source_root, &path, registered, apply)?,
            );
        }
    }

    let item_path = store.work_item_path(&work_item.id)?;
    let work_root = source_root.join(".fluent/work");
    let state_paths = vec![
        store.work_attempts_dir().join(&work_item.id),
        store.work_tasks_dir().join(&work_item.id),
        store.work_merge_candidates_dir().join(&work_item.id),
        work_root.join("runtime/attempts").join(&work_item.id),
        work_root.join("runtime/merges").join(&work_item.id),
    ];
    let artifacts = work_item_artifact_paths(source_root, work_item);

    Ok(WorkItemCleanupResult {
        work_item_id: work_item.id.clone(),
        applied: apply,
        item_path,
        state_paths,
        artifacts,
        worktrees,
        branches,
    })
}

fn orphan_work_artifact_cleanup_plans(
    source_root: &Path,
    stored_work_item_ids: &HashSet<String>,
    apply: bool,
) -> Result<Vec<OrphanWorkArtifactCleanupResult>> {
    let artifacts_dir = source_root.join(WORK_ARTIFACTS_DIR);
    let entries = match fs::read_dir(&artifacts_dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err).context("Failed to read Work artifacts directory"),
    };

    let mut plans = Vec::new();
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let work_item_id = entry.file_name().to_string_lossy().into_owned();
        if stored_work_item_ids.contains(&work_item_id) {
            continue;
        }
        plans.push(OrphanWorkArtifactCleanupResult {
            work_item_id,
            applied: apply,
            artifact_root: entry.path(),
        });
    }
    plans.sort_by(|left, right| left.artifact_root.cmp(&right.artifact_root));
    Ok(plans)
}

fn push_unique_worktree(worktrees: &mut Vec<WorktreeCleanup>, cleanup: WorktreeCleanup) {
    let cleanup_path = match &cleanup {
        WorktreeCleanup::None => return,
        WorktreeCleanup::WouldRemove(path)
        | WorktreeCleanup::Removed(path)
        | WorktreeCleanup::SkippedUnregistered(path)
        | WorktreeCleanup::Missing(path) => path,
    };
    if worktrees.iter().any(|existing| match existing {
        WorktreeCleanup::None => false,
        WorktreeCleanup::WouldRemove(path)
        | WorktreeCleanup::Removed(path)
        | WorktreeCleanup::SkippedUnregistered(path)
        | WorktreeCleanup::Missing(path) => path == cleanup_path,
    }) {
        return;
    }
    worktrees.push(cleanup);
}

fn push_unique_branch(branches: &mut Vec<WorkBranchCleanup>, cleanup: WorkBranchCleanup) {
    let branch_name = match &cleanup {
        WorkBranchCleanup::WouldRemove(branch)
        | WorkBranchCleanup::Removed(branch)
        | WorkBranchCleanup::Missing(branch) => branch,
    };
    if branches.iter().any(|existing| match existing {
        WorkBranchCleanup::WouldRemove(branch)
        | WorkBranchCleanup::Removed(branch)
        | WorkBranchCleanup::Missing(branch) => branch == branch_name,
    }) {
        return;
    }
    branches.push(cleanup);
}

fn cleanup_managed_worktree(
    search_root: &Path,
    path: &Path,
    registered: &[PathBuf],
    apply: bool,
) -> Result<WorktreeCleanup> {
    if !path.exists() {
        return Ok(WorktreeCleanup::Missing(path.to_path_buf()));
    }

    if !path_is_registered(path, registered) {
        return Ok(WorktreeCleanup::SkippedUnregistered(path.to_path_buf()));
    }

    if !apply {
        return Ok(WorktreeCleanup::WouldRemove(path.to_path_buf()));
    }

    remove_registered_worktree(search_root, path)?;
    Ok(WorktreeCleanup::Removed(path.to_path_buf()))
}

fn cleanup_work_branch(
    source_root: &Path,
    branch_name: &str,
    apply: bool,
) -> Result<WorkBranchCleanup> {
    if !git_branch_exists(source_root, branch_name)? {
        return Ok(WorkBranchCleanup::Missing(branch_name.to_string()));
    }

    if !apply {
        return Ok(WorkBranchCleanup::WouldRemove(branch_name.to_string()));
    }

    git::run(
        source_root,
        &["branch", "-D", branch_name],
        "remove Work branch",
    )?;

    Ok(WorkBranchCleanup::Removed(branch_name.to_string()))
}

fn git_branch_exists(source_root: &Path, branch_name: &str) -> Result<bool> {
    let ref_arg = format!("refs/heads/{branch_name}");
    let output = git::run_raw(source_root, &["show-ref", "--verify", "--quiet", &ref_arg])?;
    match output.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => anyhow::bail!(
            "Failed to check Work branch {branch_name:?}:\n{}",
            String::from_utf8_lossy(&output.stderr)
        ),
    }
}

fn work_item_artifact_paths(source_root: &Path, work_item: &WorkItem) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for attempt in &work_item.attempts {
        for task in &attempt.tasks {
            if let Some(area) = &task.artifact_area {
                push_unique_artifact_path(source_root, &mut paths, &area.path);
            }
        }
        for artifact in &attempt.artifacts {
            push_unique_artifact_path(source_root, &mut paths, &artifact.path);
        }
    }
    for candidate in &work_item.merge_candidates {
        for artifact in candidate
            .merge_state
            .check_artifacts
            .iter()
            .chain(candidate.merge_state.review_artifacts.iter())
        {
            push_unique_artifact_path(source_root, &mut paths, &artifact.path);
        }
    }
    paths.sort();
    paths
}

fn push_unique_artifact_path(source_root: &Path, paths: &mut Vec<PathBuf>, path: &str) {
    let artifact_root = source_root.join(WORK_ARTIFACTS_DIR);
    let resolved = match resolve_managed_artifact_path(source_root, path) {
        Some(path) => path,
        None => return,
    };
    if resolved == artifact_root || paths.iter().any(|existing| existing == &resolved) {
        return;
    }
    paths.push(resolved);
}

fn resolve_managed_artifact_path(source_root: &Path, path: &str) -> Option<PathBuf> {
    let relative_path = Path::new(path);
    if relative_path.is_absolute() {
        return None;
    }
    if relative_path
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        return None;
    }
    let resolved = source_root.join(relative_path);
    let artifact_root = source_root.join(WORK_ARTIFACTS_DIR);
    if resolved.starts_with(&artifact_root) {
        Some(resolved)
    } else {
        None
    }
}

fn apply_work_item_cleanup(plan: &WorkItemCleanupResult) -> Result<()> {
    for artifact in &plan.artifacts {
        remove_artifact_path(artifact)?;
    }
    prune_empty_artifact_parents(plan)?;

    match fs::remove_file(&plan.item_path) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(err).context("Failed to remove Work Item state"),
    }
    for path in &plan.state_paths {
        match fs::remove_dir_all(path) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(err).context("Failed to remove Work state collection"),
        }
    }

    Ok(())
}

fn remove_artifact_path(path: &Path) -> Result<()> {
    if path.is_dir() {
        fs::remove_dir_all(path).context("Failed to remove Work artifact directory")?;
    } else {
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(err).context("Failed to remove Work artifact file"),
        }
    }
    Ok(())
}

fn apply_orphan_work_artifact_cleanup(plan: &OrphanWorkArtifactCleanupResult) -> Result<()> {
    match fs::remove_dir_all(&plan.artifact_root) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).context("Failed to remove orphan Work artifact root"),
    }
}

fn prune_empty_artifact_parents(plan: &WorkItemCleanupResult) -> Result<()> {
    let Some(work_dir) = plan.item_path.parent().and_then(Path::parent) else {
        return Ok(());
    };
    let artifact_root = work_dir.join("artifacts");
    for artifact in &plan.artifacts {
        let mut current = artifact.parent();
        while let Some(dir) = current {
            if dir == artifact_root || !dir.starts_with(&artifact_root) {
                break;
            }
            match fs::remove_dir(dir) {
                Ok(()) => current = dir.parent(),
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => current = dir.parent(),
                Err(err) if err.kind() == std::io::ErrorKind::DirectoryNotEmpty => break,
                Err(err) => return Err(err).context("Failed to prune Work artifact directory"),
            }
        }
    }
    Ok(())
}

fn remove_registered_worktree(search_root: &Path, path: &Path) -> Result<()> {
    let path_str = path.to_string_lossy();
    git::run(
        search_root,
        &["worktree", "remove", "--force", &path_str],
        "remove registered worktree",
    )
}

fn registered_worktrees(search_root: &Path) -> Result<Vec<PathBuf>> {
    let output = match git::run_raw(search_root, &["worktree", "list", "--porcelain"]) {
        Ok(o) => o,
        Err(_) => return Ok(Vec::new()),
    };
    if !output.status.success() {
        return Ok(Vec::new());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(stdout
        .lines()
        .filter_map(|line| line.strip_prefix("worktree "))
        .map(PathBuf::from)
        .collect())
}

fn path_is_registered(path: &Path, registered: &[PathBuf]) -> bool {
    let canonical_path = path.canonicalize().ok();
    registered.iter().any(|registered_path| {
        if registered_path == path {
            return true;
        }
        match (&canonical_path, registered_path.canonicalize().ok()) {
            (Some(path), Some(registered)) => path == &registered,
            _ => false,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::work_model::{
        AttemptReviewState, AttemptStatus, TaskOutput, TaskStatus, WorkItemAbandonment,
        WorkspaceAccess,
    };
    use tempfile::TempDir;

    #[test]
    fn abandoned_needs_user_work_item_is_cleanup_candidate() {
        let mut item = WorkItem {
            id: "work-stale".to_string(),
            title: "Stale work".to_string(),
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
        item.attempts[0].status = AttemptStatus::NeedsUser;
        item.attempts[0].tasks[0].status = TaskStatus::NeedsUser;

        let candidates = cleanup_work_item_candidates(Path::new("/nonexistent"), vec![item]);

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].id, "work-stale");
    }

    #[test]
    fn abandoned_work_item_with_executing_task_is_not_cleanup_candidate() {
        let mut item = WorkItem {
            id: "work-active".to_string(),
            title: "Active work".to_string(),
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
        item.attempts[0].status = AttemptStatus::Failed;
        item.attempts[0].tasks[0].status = TaskStatus::Executing;
        item.attempts[0].tasks[0].workspace_access = WorkspaceAccess::read_only(Vec::new());

        let candidates = cleanup_work_item_candidates(Path::new("/nonexistent"), vec![item]);

        assert!(candidates.is_empty());
    }

    #[test]
    fn abandoned_work_item_with_reviewing_attempt_is_not_cleanup_candidate() {
        let mut item = WorkItem {
            id: "work-active".to_string(),
            title: "Active review".to_string(),
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
        item.attempts[0].status = AttemptStatus::Reviewing;

        let candidates = cleanup_work_item_candidates(Path::new("/nonexistent"), vec![item]);

        assert!(candidates.is_empty());
    }

    #[test]
    fn abandoned_work_item_with_active_merge_candidate_is_not_cleanup_candidate() {
        let reviewing_candidate = MergeCandidate {
            id: "candidate-reviewing".to_string(),
            attempt_id: "attempt-1".to_string(),
            source_workspace: crate::work_model::WorkspaceRef {
                id: "candidate".to_string(),
                path: "../work-6-work-active-attempt-1".to_string(),
            },
            target_workspace: crate::work_model::WorkspaceRef {
                id: "target".to_string(),
                path: ".".to_string(),
            },
            source_branch: "main".to_string(),
            target_branch: "main".to_string(),
            candidate_commit: "abc123".to_string(),
            merge_review_state: MergeReviewState::Reviewing,
            merge_state: crate::work_model::MergeCandidateMergeState::default(),
            created_at: None,
            started_at: None,
            completed_at: None,
        };
        let mut merging_candidate = reviewing_candidate.clone();
        merging_candidate.id = "candidate-merging".to_string();
        merging_candidate.merge_review_state = MergeReviewState::Pending;
        merging_candidate.merge_state.status = MergeCandidateMergeStatus::Executing;

        for candidate in [reviewing_candidate, merging_candidate] {
            let item = WorkItem {
                id: "work-active".to_string(),
                title: "Active candidate".to_string(),
                planning_context: None,
                instructions: None,
                abandonment: Some(WorkItemAbandonment {
                    reason: Some("replacement landed".to_string()),
                }),
                post_merge_review_fix_depth: None,
                attempts: Vec::new(),
                merge_candidates: vec![candidate],
                ..Default::default()
            };

            let candidates = cleanup_work_item_candidates(Path::new("/nonexistent"), vec![item]);

            assert!(candidates.is_empty());
        }
    }

    #[test]
    fn cleanup_retains_retryable_learning_and_post_land_origins() {
        use crate::follow_up::ArtifactRef;
        use crate::work_model::{AttemptLearning, FollowUpProcessingFailure};
        use tempfile::TempDir;

        fn landed_item(id: &str) -> WorkItem {
            let mut item = WorkItem::planned(id, "Landed work");
            item.add_initial_attempt("attempt-1").unwrap();
            item.attempts[0].status = AttemptStatus::Complete;
            for task in &mut item.attempts[0].tasks {
                task.status = TaskStatus::Complete;
            }
            item.attempts[0].learning = Some(AttemptLearning::succeeded(
                1,
                ArtifactRef {
                    path: "handoff.json".to_string(),
                    digest: "sha256:test".to_string(),
                },
            ));
            let mut candidate = MergeCandidate {
                id: "attempt-1-merge-candidate".to_string(),
                attempt_id: "attempt-1".to_string(),
                source_workspace: crate::work_model::WorkspaceRef {
                    id: "candidate".to_string(),
                    path: "../work-6-work-attempt-1".to_string(),
                },
                target_workspace: crate::work_model::WorkspaceRef {
                    id: "target".to_string(),
                    path: ".".to_string(),
                },
                source_branch: "main".to_string(),
                target_branch: "main".to_string(),
                candidate_commit: "abc123".to_string(),
                merge_review_state: MergeReviewState::Passed,
                merge_state: crate::work_model::MergeCandidateMergeState::default(),
                created_at: None,
                started_at: None,
                completed_at: None,
            };
            candidate.merge_state.status = MergeCandidateMergeStatus::Merged;
            candidate.merge_state.merged_commit = Some("abc123".to_string());
            item.merge_candidates.push(candidate);
            item
        }

        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // A completed landed origin with no live recovery is reapable.
        assert_eq!(
            cleanup_work_item_candidates(root, vec![landed_item("work-complete")]).len(),
            1,
            "a completed origin is reapable"
        );

        // A failed, retryable Learner record retains the origin.
        let mut failed_learning = landed_item("work-failed-learning");
        failed_learning.attempts[0].learning = Some(AttemptLearning::failed(1, "learner crashed"));
        assert!(
            cleanup_work_item_candidates(root, vec![failed_learning]).is_empty(),
            "a retryable Learner record retains its origin"
        );

        // A merged legacy Attempt without a Learning record must remain
        // recoverable instead of losing the candidate workspace it needs.
        let mut missing_learning = landed_item("work-missing-learning");
        missing_learning.attempts[0].learning = None;
        assert!(
            cleanup_work_item_candidates(root, vec![missing_learning]).is_empty(),
            "a missing legacy Learning record retains its origin"
        );

        // Validation can fail before an operation journal exists. The durable
        // candidate failure alone must keep its origin available for retry.
        let mut candidate_failure = landed_item("work-candidate-failure");
        candidate_failure.merge_candidates[0]
            .merge_state
            .follow_up_failure = Some(FollowUpProcessingFailure {
            stage: "validate-handoff".to_string(),
            message: "handoff digest mismatch".to_string(),
            next_action: "retry".to_string(),
        });
        assert!(
            cleanup_work_item_candidates(root, vec![candidate_failure]).is_empty(),
            "a candidate follow-up failure retains its origin before journaling"
        );

        // An incomplete post-land journal retains the origin.
        let op_dir = crate::follow_up::follow_ups_root(root).join(
            crate::follow_up::operation_id_for_candidate(
                "work-incomplete",
                "attempt-1-merge-candidate",
            ),
        );
        fs::create_dir_all(&op_dir).unwrap();
        fs::write(op_dir.join("operation.json"), "{}").unwrap();
        fs::write(op_dir.join("journal.json"), r#"{"completed":false}"#).unwrap();
        assert!(
            cleanup_work_item_candidates(root, vec![landed_item("work-incomplete")]).is_empty(),
            "an incomplete post-land journal retains its origin"
        );

        // A pending imported operation with no journal yet retains the origin.
        let import_dir = crate::follow_up::follow_ups_root(root).join(
            crate::follow_up::operation_id_for_candidate(
                "work-import",
                "attempt-1-merge-candidate",
            ),
        );
        fs::create_dir_all(&import_dir).unwrap();
        fs::write(import_dir.join("operation.json"), "{}").unwrap();
        assert!(
            cleanup_work_item_candidates(root, vec![landed_item("work-import")]).is_empty(),
            "a pending imported post-land operation retains its origin"
        );

        // Once the journal completes, the origin becomes reapable.
        fs::write(op_dir.join("journal.json"), r#"{"completed":true}"#).unwrap();
        assert_eq!(
            cleanup_work_item_candidates(root, vec![landed_item("work-incomplete")]).len(),
            1,
            "a completed journal no longer blocks cleanup"
        );
    }

    #[test]
    fn parse_reviewer_worktree_name_extracts_components() {
        let result = parse_reviewer_worktree_name("review-6-work-1-attempt-1-tests");
        assert_eq!(
            result,
            Some((
                "work-1".to_string(),
                "attempt-1".to_string(),
                "tests".to_string()
            ))
        );
    }

    #[test]
    fn parse_reviewer_worktree_name_handles_long_work_item_id() {
        let result = parse_reviewer_worktree_name("review-17-my-long-work-item-a1-architecture");
        assert_eq!(
            result,
            Some((
                "my-long-work-item".to_string(),
                "a1".to_string(),
                "architecture".to_string()
            ))
        );
    }

    #[test]
    fn parse_reviewer_worktree_name_rejects_non_reviewer_suffix() {
        assert_eq!(
            parse_reviewer_worktree_name("review-6-work-1-attempt-1-unknown"),
            None
        );
    }

    #[test]
    fn parse_reviewer_worktree_name_rejects_non_matching_names() {
        assert_eq!(
            parse_reviewer_worktree_name("work-6-work-1-attempt-1"),
            None
        );
        assert_eq!(
            parse_reviewer_worktree_name("review-bad-work-1-tests"),
            None
        );
        assert_eq!(parse_reviewer_worktree_name("review-999-x-a-tests"), None);
    }

    fn init_test_repo(project: &Path) {
        fs::create_dir_all(project).unwrap();
        for args in [
            &["init", "-b", "main"] as &[&str],
            &["config", "user.email", "test@test"],
            &["config", "user.name", "test"],
        ] {
            git::run(project, args, "init test repo").unwrap();
        }
        fs::write(project.join("README.md"), "test\n").unwrap();
        git::run(project, &["add", "README.md"], "stage test file").unwrap();
        git::run(project, &["commit", "-m", "init"], "initial commit").unwrap();
    }

    #[test]
    fn stranded_reviewer_worktree_detected_for_non_executing_work_item() {
        let tmp = TempDir::new().unwrap();
        let project = tmp.path().join("project");
        init_test_repo(&project);

        let store = WorkModelStore::new(&project);
        let mut item = WorkItem {
            id: "work-1".to_string(),
            title: "Stranded test".to_string(),
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

        let stranded = tmp.path().join("review-6-work-1-attempt-1-tests");
        fs::create_dir_all(&stranded).unwrap();

        let active = tmp.path().join("review-6-work-1-attempt-1-architecture");
        fs::create_dir_all(&active).unwrap();

        let results =
            cleanup_stranded_reviewer_worktrees(&project, &CleanupOptions { apply: false })
                .unwrap();

        assert_eq!(results.len(), 2);
        let arch = results
            .iter()
            .find(|r| r.reviewer == "architecture")
            .expect("architecture result");
        assert_eq!(arch.work_item_id, "work-1");
        assert_eq!(arch.attempt_id, "attempt-1");
        assert!(!arch.applied);
        let tests_result = results
            .iter()
            .find(|r| r.reviewer == "tests")
            .expect("tests result");
        assert_eq!(tests_result.work_item_id, "work-1");
        assert_eq!(tests_result.attempt_id, "attempt-1");
        assert!(!tests_result.applied);
    }

    #[test]
    fn stranded_reviewer_worktree_preserved_for_executing_merge_candidate() {
        let tmp = TempDir::new().unwrap();
        let project = tmp.path().join("project");
        init_test_repo(&project);

        let store = WorkModelStore::new(&project);
        let mut item = WorkItem {
            id: "work-1".to_string(),
            title: "Active merge".to_string(),
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
        attempt.status = AttemptStatus::Complete;
        attempt.review_state = Some(AttemptReviewState::Passed);
        let task = attempt.tasks.first_mut().unwrap();
        task.status = TaskStatus::Complete;
        task.output = Some(TaskOutput {
            workspace_id: "candidate".to_string(),
            workspace_path: "../work-6-work-1-attempt-1".to_string(),
            source_branch: "main".to_string(),
            base_commit: None,
            commit: "abc123".to_string(),
        });
        let candidate_id = item.create_or_get_merge_candidate("attempt-1").unwrap();
        store.create_work_item(&item).unwrap();

        let mut stored = store.read_work_item("work-1").unwrap();
        let candidate = stored
            .merge_candidates
            .iter_mut()
            .find(|c| c.id == candidate_id)
            .unwrap();
        candidate.merge_state.status = MergeCandidateMergeStatus::Executing;
        store.write_work_item(&stored).unwrap();

        let reviewer_dir = tmp.path().join("review-6-work-1-attempt-1-tests");
        fs::create_dir_all(&reviewer_dir).unwrap();

        let results =
            cleanup_stranded_reviewer_worktrees(&project, &CleanupOptions { apply: true }).unwrap();

        assert!(results.is_empty());
        assert!(reviewer_dir.exists());
    }

    #[test]
    fn stranded_reviewer_worktree_removed_on_apply() {
        let tmp = TempDir::new().unwrap();
        let project = tmp.path().join("project");
        init_test_repo(&project);

        let store = WorkModelStore::new(&project);
        let mut item = WorkItem {
            id: "work-1".to_string(),
            title: "Stranded apply test".to_string(),
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

        let reviewer_dir = tmp.path().join("review-6-work-1-attempt-1-tests");
        fs::create_dir_all(&reviewer_dir).unwrap();

        let results =
            cleanup_stranded_reviewer_worktrees(&project, &CleanupOptions { apply: true }).unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].work_item_id, "work-1");
        assert_eq!(results[0].attempt_id, "attempt-1");
        assert_eq!(results[0].reviewer, "tests");
        assert!(results[0].applied);
        assert!(!reviewer_dir.exists());
    }

    #[test]
    fn terminal_work_item_cleanup_removes_runtime_arn_dirs() {
        let tmp = TempDir::new().unwrap();
        let project = tmp.path().join("project");
        fs::create_dir_all(&project).unwrap();
        let store = WorkModelStore::new(&project);

        let item = WorkItem {
            id: "runtime-cleanup".to_string(),
            title: "Cleanup removes Fargate runtime ARN dirs".to_string(),
            planning_context: None,
            instructions: None,
            abandonment: Some(crate::work_model::WorkItemAbandonment {
                reason: Some("test cleanup".to_string()),
            }),
            post_merge_review_fix_depth: None,
            attempts: Vec::new(),
            merge_candidates: Vec::new(),
            ..Default::default()
        };
        store.create_work_item(&item).unwrap();

        let attempts_runtime =
            project.join(".fluent/work/runtime/attempts/runtime-cleanup/attempt-1");
        fs::create_dir_all(&attempts_runtime).unwrap();
        fs::write(attempts_runtime.join("fargate-task-arn"), "arn-1").unwrap();
        let merges_runtime = project.join(".fluent/work/runtime/merges/runtime-cleanup/cand-1");
        fs::create_dir_all(&merges_runtime).unwrap();
        fs::write(merges_runtime.join("fargate-task-arn"), "arn-2").unwrap();

        let results = cleanup_work_items(&project, &CleanupOptions { apply: true }).unwrap();

        let work_item_result = results
            .iter()
            .find_map(|r| match r {
                WorkCleanupResult::WorkItem(item) => Some(item),
                _ => None,
            })
            .expect("Work Item cleanup result present");
        assert!(work_item_result.applied);
        assert!(!attempts_runtime.exists());
        assert!(!merges_runtime.exists());
    }
}
