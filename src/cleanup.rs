use anyhow::{Context, Result, bail};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::run::{self, Run, RunStatus};
use crate::work_model::{
    Attempt, AttemptStatus, MergeCandidate, MergeCandidateMergeStatus, MergeCandidateReviewState,
    Task, TaskStatus, WORK_ARTIFACTS_DIR, WorkItem, WorkModelStore,
    resolve_expected_candidate_workspace_path,
};

#[derive(Debug, Clone)]
pub struct CleanupOptions {
    pub run_id: Option<String>,
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

#[derive(Debug, Clone)]
pub struct CleanupResult {
    pub run_id: String,
    pub status: RunStatus,
    pub applied: bool,
    pub worktree: WorktreeCleanup,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkBranchCleanup {
    WouldRemove(String),
    Removed(String),
    Missing(String),
}

#[derive(Debug, Clone)]
pub struct WorkCleanupResult {
    pub work_item_id: String,
    pub applied: bool,
    pub item_path: PathBuf,
    pub artifacts: Vec<PathBuf>,
    pub worktrees: Vec<WorktreeCleanup>,
    pub branches: Vec<WorkBranchCleanup>,
}

pub fn cleanup_runs(search_root: &Path, options: &CleanupOptions) -> Result<Vec<CleanupResult>> {
    let source_root = cleanup_source_root(search_root)?;
    let candidates = cleanup_candidates(&source_root, options.run_id.as_deref())?;
    let registered = registered_worktrees(&source_root)?;
    let mut results = Vec::new();

    for run in candidates {
        let status = run.status()?;
        let worktree = cleanup_worktree(&source_root, &run, &registered, options.apply)?;
        if options.apply {
            write_cleaned_marker(&run, &status, &worktree)?;
        }
        results.push(CleanupResult {
            run_id: run.id,
            status,
            applied: options.apply,
            worktree,
        });
    }

    Ok(results)
}

pub fn cleanup_work_items(
    search_root: &Path,
    options: &CleanupOptions,
) -> Result<Vec<WorkCleanupResult>> {
    if options.run_id.is_some() {
        return Ok(Vec::new());
    }

    let source_root = cleanup_source_root(search_root)?;
    let store = WorkModelStore::new(&source_root);
    let candidates = cleanup_work_item_candidates(&store)?;
    let registered = registered_worktrees(&source_root)?;
    let mut results = Vec::new();

    for work_item in candidates {
        let plan = work_cleanup_plan(&source_root, &store, &work_item, &registered, options.apply)?;
        if options.apply {
            apply_work_item_cleanup(&plan)?;
        }
        results.push(plan);
    }

    Ok(results)
}

pub fn run_is_cleaned(run: &Run) -> bool {
    run.dir.join("cleaned.md").exists()
}

fn cleanup_source_root(search_root: &Path) -> Result<PathBuf> {
    let search_root = fs::canonicalize(search_root).unwrap_or_else(|_| search_root.to_path_buf());
    let current_worktree = git_worktree_root(&search_root).unwrap_or_else(|| search_root.clone());
    let registered = registered_worktrees(&search_root)?;

    for candidate in registered {
        if candidate == current_worktree {
            continue;
        }
        if registry_points_to_worktree(&candidate, &current_worktree)? {
            return Ok(candidate);
        }
    }

    Ok(search_root)
}

fn git_worktree_root(search_root: &Path) -> Option<PathBuf> {
    let output = Command::new("git")
        .args(["-C", &search_root.to_string_lossy()])
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if path.is_empty() {
        None
    } else {
        Some(PathBuf::from(path))
    }
}

fn registry_points_to_worktree(registry_root: &Path, worktree_root: &Path) -> Result<bool> {
    let runs_dir = registry_root.join(".factory/runs");
    if !runs_dir.is_dir() {
        return Ok(false);
    }

    let canonical_worktree = worktree_root.canonicalize().ok();
    for entry in fs::read_dir(runs_dir)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let recorded = match fs::read_to_string(path.join("worktree")) {
            Ok(content) => content.trim().to_string(),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(err) => return Err(err).context("Failed to read run worktree path"),
        };
        if recorded.is_empty() {
            continue;
        }
        let recorded_path = PathBuf::from(recorded);
        if recorded_path == worktree_root {
            return Ok(true);
        }
        if let (Some(worktree), Ok(recorded)) = (&canonical_worktree, recorded_path.canonicalize())
        {
            if recorded == *worktree {
                return Ok(true);
            }
        }
    }

    Ok(false)
}

fn cleanup_candidates(search_root: &Path, run_id: Option<&str>) -> Result<Vec<Run>> {
    if let Some(id) = run_id {
        let run = run::resolve_run_by_id(search_root, id)?;
        if run_is_cleaned(&run) {
            return Ok(Vec::new());
        }
        ensure_cleanable(&run)?;
        return Ok(vec![run]);
    }

    let mut candidates = Vec::new();
    for run in run::list_runs(search_root)? {
        if run_is_cleaned(&run) {
            continue;
        }
        if is_cleanable_status(&run.status()?) {
            candidates.push(run);
        }
    }
    Ok(candidates)
}

fn ensure_cleanable(run: &Run) -> Result<()> {
    let status = run.status()?;
    if !is_cleanable_status(&status) {
        bail!(
            "Run {} has status '{}', expected complete or landed",
            run.id,
            status
        );
    }
    Ok(())
}

fn is_cleanable_status(status: &RunStatus) -> bool {
    matches!(status, RunStatus::Complete | RunStatus::Landed)
}

fn cleanup_work_item_candidates(store: &WorkModelStore) -> Result<Vec<WorkItem>> {
    let mut candidates = Vec::new();
    for work_item in store.list_work_items()? {
        if work_item_is_cleanable(&work_item) {
            candidates.push(work_item);
        }
    }
    Ok(candidates)
}

fn work_item_is_cleanable(work_item: &WorkItem) -> bool {
    !work_item.attempts.is_empty()
        && work_item.attempts.iter().all(attempt_is_terminal)
        && work_item
            .merge_candidates
            .iter()
            .all(merge_candidate_is_terminal)
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
        MergeCandidateMergeStatus::Landed | MergeCandidateMergeStatus::Failed
    ) || matches!(candidate.review_state, MergeCandidateReviewState::Failed)
}

fn work_cleanup_plan(
    source_root: &Path,
    store: &WorkModelStore,
    work_item: &WorkItem,
    registered: &[PathBuf],
    apply: bool,
) -> Result<WorkCleanupResult> {
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
    let artifacts = work_item_artifact_paths(source_root, work_item);

    Ok(WorkCleanupResult {
        work_item_id: work_item.id.clone(),
        applied: apply,
        item_path,
        artifacts,
        worktrees,
        branches,
    })
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

    let output = Command::new("git")
        .args(["-C", &source_root.to_string_lossy()])
        .args(["branch", "-D", branch_name])
        .output()
        .context("Failed to remove Work branch")?;
    if !output.status.success() {
        bail!(
            "Failed to remove Work branch {branch_name:?}:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    Ok(WorkBranchCleanup::Removed(branch_name.to_string()))
}

fn git_branch_exists(source_root: &Path, branch_name: &str) -> Result<bool> {
    let output = Command::new("git")
        .args(["-C", &source_root.to_string_lossy()])
        .args([
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch_name}"),
        ])
        .output()
        .context("Failed to check Work branch")?;
    match output.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => bail!(
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

fn apply_work_item_cleanup(plan: &WorkCleanupResult) -> Result<()> {
    for artifact in &plan.artifacts {
        remove_artifact_path(artifact)?;
    }
    prune_empty_artifact_parents(plan)?;

    match fs::remove_file(&plan.item_path) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(err).context("Failed to remove Work Item state"),
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

fn prune_empty_artifact_parents(plan: &WorkCleanupResult) -> Result<()> {
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

fn cleanup_worktree(
    search_root: &Path,
    run: &Run,
    registered: &[PathBuf],
    apply: bool,
) -> Result<WorktreeCleanup> {
    let Some(path) = recorded_worktree_path(run)? else {
        return Ok(WorktreeCleanup::None);
    };

    if !path.exists() {
        return Ok(WorktreeCleanup::Missing(path));
    }

    if !path_is_registered(&path, registered) {
        return Ok(WorktreeCleanup::SkippedUnregistered(path));
    }

    if !apply {
        return Ok(WorktreeCleanup::WouldRemove(path));
    }

    remove_registered_worktree(search_root, &path)?;

    Ok(WorktreeCleanup::Removed(path))
}

fn remove_registered_worktree(search_root: &Path, path: &Path) -> Result<()> {
    let output = Command::new("git")
        .args(["-C", &search_root.to_string_lossy()])
        .args(["worktree", "remove", "--force", &path.to_string_lossy()])
        .output()
        .context("Failed to remove registered worktree")?;

    if !output.status.success() {
        bail!(
            "Failed to remove worktree {}:\n{}",
            path.display(),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    Ok(())
}

fn recorded_worktree_path(run: &Run) -> Result<Option<PathBuf>> {
    match fs::read_to_string(run.dir.join("worktree")) {
        Ok(content) => {
            let trimmed = content.trim();
            if trimmed.is_empty() {
                Ok(None)
            } else {
                Ok(Some(PathBuf::from(trimmed)))
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err).context("Failed to read run worktree path"),
    }
}

fn registered_worktrees(search_root: &Path) -> Result<Vec<PathBuf>> {
    let output = Command::new("git")
        .args(["-C", &search_root.to_string_lossy()])
        .args(["worktree", "list", "--porcelain"])
        .output();

    let Ok(output) = output else {
        return Ok(Vec::new());
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

fn write_cleaned_marker(run: &Run, status: &RunStatus, worktree: &WorktreeCleanup) -> Result<()> {
    let cleaned_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    let worktree_line = match worktree {
        WorktreeCleanup::None => "Worktree: none recorded".to_string(),
        WorktreeCleanup::WouldRemove(path) => format!("Worktree: would remove {}", path.display()),
        WorktreeCleanup::Removed(path) => format!("Worktree: removed {}", path.display()),
        WorktreeCleanup::SkippedUnregistered(path) => {
            format!("Worktree: skipped unregistered {}", path.display())
        }
        WorktreeCleanup::Missing(path) => format!("Worktree: missing {}", path.display()),
    };
    let content = format!(
        "# Cleaned\n\nRun: {}\nStatus: {}\nCleaned at: unix-{cleaned_at}\nReason: stale terminal run cleanup\n{worktree_line}\n",
        run.id, status
    );
    fs::write(run.dir.join("cleaned.md"), content).context("Failed to write cleanup marker")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn create_run(root: &Path, id: &str, status: &str) -> PathBuf {
        let run_dir = root.join(format!(".factory/runs/{id}"));
        fs::create_dir_all(&run_dir).unwrap();
        fs::write(run_dir.join("status"), status).unwrap();
        run_dir
    }

    #[test]
    fn dry_run_does_not_write_marker() {
        let tmp = TempDir::new().unwrap();
        create_run(tmp.path(), "done", "complete");

        let results = cleanup_runs(
            tmp.path(),
            &CleanupOptions {
                run_id: None,
                apply: false,
            },
        )
        .unwrap();

        assert_eq!(results.len(), 1);
        assert!(!tmp.path().join(".factory/runs/done/cleaned.md").exists());
    }

    #[test]
    fn apply_writes_marker_without_status_change() {
        let tmp = TempDir::new().unwrap();
        create_run(tmp.path(), "done", "landed");

        cleanup_runs(
            tmp.path(),
            &CleanupOptions {
                run_id: None,
                apply: true,
            },
        )
        .unwrap();

        let run_dir = tmp.path().join(".factory/runs/done");
        assert_eq!(
            fs::read_to_string(run_dir.join("status")).unwrap(),
            "landed"
        );
        let marker = fs::read_to_string(run_dir.join("cleaned.md")).unwrap();
        assert!(marker.contains("Reason: stale terminal run cleanup"));
    }

    #[test]
    fn cleanup_skips_active_statuses() {
        let tmp = TempDir::new().unwrap();
        create_run(tmp.path(), "planned-run", "planned");
        create_run(tmp.path(), "needs-user-run", "needs-user");
        create_run(tmp.path(), "failed-run", "failed");

        let results = cleanup_runs(
            tmp.path(),
            &CleanupOptions {
                run_id: None,
                apply: false,
            },
        )
        .unwrap();

        assert!(results.is_empty());
    }

    #[test]
    fn unregistered_worktree_path_is_not_removed() {
        let tmp = TempDir::new().unwrap();
        let run_dir = create_run(tmp.path(), "done", "complete");
        let path = tmp.path().join("not-a-worktree");
        fs::create_dir_all(&path).unwrap();
        fs::write(run_dir.join("worktree"), path.to_str().unwrap()).unwrap();

        let results = cleanup_runs(
            tmp.path(),
            &CleanupOptions {
                run_id: None,
                apply: true,
            },
        )
        .unwrap();

        assert!(path.is_dir());
        assert_eq!(
            results[0].worktree,
            WorktreeCleanup::SkippedUnregistered(path)
        );
    }
}
