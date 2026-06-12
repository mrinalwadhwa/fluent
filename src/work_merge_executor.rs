use anyhow::{Context, Result, bail};
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use crate::coder::CoderKind;
use crate::content::ContentResolver;
use crate::hooks::{self, HookContext, HookOutcome};
use crate::work_model::{
    ArtifactRef, MergeCandidateMergeState, MergeCandidateMergeStatus, MergeCandidateReviewState,
    WORK_ARTIFACTS_DIR, WorkItem, WorkModelError, WorkModelStorageError, WorkModelStore,
    resolve_expected_candidate_workspace_path,
};
use crate::worktree;

pub struct WorkMergeConfig<'a> {
    pub project_root: &'a Path,
    pub store: &'a WorkModelStore,
    pub work_item_id: &'a str,
    pub merge_candidate_id: &'a str,
    pub resolver: &'a ContentResolver,
    pub extra_args: &'a [String],
    pub coder_kind: CoderKind,
    pub no_sandbox: bool,
}

pub struct WorkMergeOutcome {
    pub merge_candidate_id: String,
    pub merged_commit: String,
}

pub fn merge_candidate(config: WorkMergeConfig<'_>) -> Result<WorkMergeOutcome> {
    let item = read_work_item_or_not_found(config.store, config.work_item_id)?;
    item.ensure_not_abandoned()?;
    let candidate = item
        .merge_candidates
        .iter()
        .find(|candidate| candidate.id == config.merge_candidate_id)
        .cloned()
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Merge Candidate {:?} not found in Work Item {:?}",
                config.merge_candidate_id,
                config.work_item_id
            )
        })?;
    if let Err(error) = candidate.validate(&item) {
        if can_record_validation_failure(&error) {
            record_candidate_failure(
                config.store,
                config.work_item_id,
                &candidate.id,
                error.to_string(),
                Vec::new(),
                Vec::new(),
            )?;
        }
        bail!("{error}");
    }

    if candidate.merge_state.status == MergeCandidateMergeStatus::Merged
        && let Some(merged_commit) = candidate.merge_state.merged_commit.clone()
    {
        return Ok(WorkMergeOutcome {
            merge_candidate_id: candidate.id,
            merged_commit,
        });
    }

    let source_workspace = resolve_managed_candidate_workspace_path(
        config.project_root,
        &candidate.source_workspace.path,
        config.work_item_id,
        &candidate.attempt_id,
    )?;
    let target_workspace =
        resolve_workspace_path(config.project_root, &candidate.target_workspace.path);
    let artifact_dir = merge_artifact_dir(
        config.project_root,
        config.work_item_id,
        &candidate.attempt_id,
        &candidate.id,
    );
    fs::create_dir_all(&artifact_dir)?;

    set_candidate_executing(config.store, config.work_item_id, &candidate.id)?;

    let result = execute_merge(
        &config,
        &item,
        &candidate,
        &source_workspace,
        &target_workspace,
        &artifact_dir,
    );
    recover_landed_candidate_result(config.store, config.work_item_id, &candidate.id, result)
}

fn recover_landed_candidate_result(
    store: &WorkModelStore,
    work_item_id: &str,
    candidate_id: &str,
    result: Result<WorkMergeOutcome>,
) -> Result<WorkMergeOutcome> {
    match result {
        Ok(outcome) => Ok(outcome),
        Err(error) => {
            if let Some(merged_commit) = candidate_merged_commit(store, work_item_id, candidate_id)?
            {
                eprintln!(
                    "  Warning: Merge Candidate {candidate_id} landed, but post-landing merge cleanup failed: {error}",
                );
                return Ok(WorkMergeOutcome {
                    merge_candidate_id: candidate_id.to_string(),
                    merged_commit,
                });
            }
            if !candidate_has_failure(store, work_item_id, candidate_id)? {
                record_candidate_failure(
                    store,
                    work_item_id,
                    candidate_id,
                    error.to_string(),
                    Vec::new(),
                    Vec::new(),
                )?;
            }
            Err(error)
        }
    }
}

fn execute_merge(
    config: &WorkMergeConfig<'_>,
    _item: &WorkItem,
    candidate: &crate::work_model::MergeCandidate,
    source_workspace: &Path,
    target_workspace: &Path,
    artifact_dir: &Path,
) -> Result<WorkMergeOutcome> {
    ensure_same_git_repository(config.project_root, source_workspace)?;
    ensure_same_git_repository(config.project_root, target_workspace)?;
    ensure_registered_worktree(config.project_root, source_workspace)?;
    ensure_clean_worktree(source_workspace)?;
    ensure_clean_worktree(target_workspace)?;
    let target_head_before = git_stdout(
        target_workspace,
        &["rev-parse", &candidate.target_branch],
        "resolve target branch",
    )?;
    let source_head = head_commit(source_workspace)?;
    if source_head != candidate.candidate_commit {
        bail!(
            "Merge Candidate {:?} expected source commit {} but workspace is at {}",
            candidate.id,
            candidate.candidate_commit,
            source_head
        );
    }

    ensure_clean_worktree(source_workspace)?;
    rebase_candidate(source_workspace, &candidate.target_branch)?;
    ensure_clean_worktree(source_workspace)?;

    let check_artifacts = match run_merge_checks(config, candidate, source_workspace, artifact_dir)
    {
        Ok(artifacts) => artifacts,
        Err(error) => {
            let artifacts = check_artifacts_for_failure(config.project_root, artifact_dir);
            record_candidate_failure(
                config.store,
                config.work_item_id,
                &candidate.id,
                error.to_string(),
                artifacts,
                Vec::new(),
            )?;
            return Err(error);
        }
    };

    let outcome = finalize_merge(
        config,
        candidate,
        source_workspace,
        target_workspace,
        &target_head_before,
        check_artifacts,
        Vec::new(),
    )?;

    let entry = crate::post_merge_review::QueueEntry {
        target_branch: candidate.target_branch.clone(),
        merged_commit: outcome.merged_commit.clone(),
        merged_at_unix: crate::post_merge_review::now_unix(),
        source_work_item_id: config.work_item_id.to_string(),
        source_merge_candidate_id: candidate.id.clone(),
    };
    if let Err(error) = crate::post_merge_review::queue_and_spawn(
        config.project_root,
        entry,
        crate::post_merge_review::debounce_seconds(),
    ) {
        eprintln!("  Warning: post-merge review queue/spawn failed: {error}");
    }
    Ok(outcome)
}

fn finalize_merge(
    config: &WorkMergeConfig<'_>,
    candidate: &crate::work_model::MergeCandidate,
    source_workspace: &Path,
    target_workspace: &Path,
    target_head_before: &str,
    check_artifacts: Vec<ArtifactRef>,
    review_artifacts: Vec<ArtifactRef>,
) -> Result<WorkMergeOutcome> {
    let merged_commit = head_commit(source_workspace)?;
    let target_head_now = git_stdout(
        target_workspace,
        &["rev-parse", &candidate.target_branch],
        "resolve target branch before merge",
    )?;
    if target_head_now != target_head_before {
        bail!(
            "Target branch {} moved from {} to {}; retry merge",
            candidate.target_branch,
            target_head_before,
            target_head_now
        );
    }

    git(
        target_workspace,
        &["checkout", &candidate.target_branch],
        "checkout target branch",
    )?;
    git(
        target_workspace,
        &["merge", "--ff-only", &merged_commit],
        "fast-forward target branch",
    )?;

    record_candidate_merged(
        config.store,
        config.work_item_id,
        &candidate.id,
        &merged_commit,
        check_artifacts,
        review_artifacts,
    )?;
    if let Err(error) = cleanup_managed_workspace(config.project_root, source_workspace) {
        eprintln!(
            "  Warning: Merge Candidate {} landed, but managed workspace cleanup failed: {error}",
            candidate.id
        );
    }

    Ok(WorkMergeOutcome {
        merge_candidate_id: candidate.id.clone(),
        merged_commit,
    })
}

/// Extract artifact paths for failed/uncertain reviewers so the
/// follow-up writer can read concrete findings.

/// Invoke the configured coder against the candidate workspace with
/// the failed merge-time review artifacts as input, asking the
/// coder to address the findings and commit. Errors if no new
/// commits result or the worktree is left dirty.

/// Run the `check-pre-merge` hook against the rebased candidate
/// workspace. If it fails and a `fix-pre-merge` hook exists, run that,
/// commit any changes it produced, and re-run `check-pre-merge`.
///
/// Returns the merge-check artifacts (hook log paths) so they can be
/// recorded on the Merge Candidate.
fn run_merge_checks(
    config: &WorkMergeConfig<'_>,
    candidate: &crate::work_model::MergeCandidate,
    source_workspace: &Path,
    artifact_dir: &Path,
) -> Result<Vec<ArtifactRef>> {
    let hooks_dir = artifact_dir.join("hooks");
    let context = HookContext {
        work_item_id: Some(config.work_item_id.to_string()),
        attempt_id: Some(candidate.attempt_id.clone()),
        merge_candidate_id: Some(candidate.id.clone()),
        candidate_commit: Some(candidate.candidate_commit.clone()),
        artifact_dir: Some(artifact_dir.to_path_buf()),
        log_dir: hooks_dir.clone(),
        ..Default::default()
    };

    let mut artifacts = Vec::new();

    let Some(check_outcome) = hooks::run_hook(
        config.project_root,
        "check-pre-merge",
        source_workspace,
        &context,
    )?
    else {
        return Ok(artifacts);
    };
    artifacts.push(hook_artifact(config.project_root, &check_outcome));
    if check_outcome.passed {
        return Ok(artifacts);
    }

    // check-pre-merge failed; try fix-pre-merge before giving up.
    if hooks::find_hook(config.project_root, "fix-pre-merge").is_none() {
        bail!(
            "check-pre-merge failed (exit {}). Log: {}",
            check_outcome.exit_code,
            check_outcome.log_path.display()
        );
    }

    if worktree_is_dirty(source_workspace)? {
        bail!("check-pre-merge failed and fix-pre-merge cannot run: candidate worktree is dirty");
    }

    let baseline_commit = head_commit(source_workspace)?;
    let fix_outcome = hooks::run_hook(
        config.project_root,
        "fix-pre-merge",
        source_workspace,
        &context,
    )?
    .expect("fix-pre-merge presence checked above");
    artifacts.push(hook_artifact(config.project_root, &fix_outcome));
    if !fix_outcome.passed {
        bail!(
            "fix-pre-merge failed (exit {}). Log: {}",
            fix_outcome.exit_code,
            fix_outcome.log_path.display()
        );
    }

    if worktree_is_dirty(source_workspace)? {
        commit_autofix(source_workspace)?;
    }
    let after_commit = head_commit(source_workspace)?;
    if after_commit == baseline_commit {
        // Nothing produced; fix didn't help. Re-run check anyway to
        // surface the original failure once more for the artifact.
    }

    let recheck_outcome = hooks::run_hook(
        config.project_root,
        "check-pre-merge",
        source_workspace,
        &context,
    )?
    .expect("check-pre-merge presence already confirmed");
    artifacts.push(hook_artifact(config.project_root, &recheck_outcome));
    if !recheck_outcome.passed {
        bail!(
            "check-pre-merge failed after fix-pre-merge (exit {}). Log: {}",
            recheck_outcome.exit_code,
            recheck_outcome.log_path.display()
        );
    }
    Ok(artifacts)
}

fn hook_artifact(project_root: &Path, outcome: &HookOutcome) -> ArtifactRef {
    ArtifactRef {
        producer_id: format!("merge-hook-{}", outcome.name),
        path: path_for_model(project_root, &outcome.log_path),
    }
}

fn worktree_is_dirty(worktree_dir: &Path) -> Result<bool> {
    let output = Command::new("git")
        .args(["-C", &worktree_dir.to_string_lossy()])
        .args([
            "status",
            "--porcelain",
            "--untracked-files=normal",
            "--",
            ".",
            ":(exclude).factory",
        ])
        .output()
        .context("Failed to run git status")?;
    Ok(!output.stdout.is_empty())
}

fn commit_autofix(worktree_dir: &Path) -> Result<()> {
    git(
        worktree_dir,
        &["add", "--", ".", ":(exclude).factory"],
        "stage fix-pre-merge changes",
    )?;
    git(
        worktree_dir,
        &[
            "commit",
            "-m",
            "Apply fix-pre-merge changes",
            "-m",
            "- Apply changes produced by the project's fix-pre-merge hook before landing.",
        ],
        "commit fix-pre-merge changes",
    )
}

fn check_artifacts_for_failure(project_root: &Path, artifact_dir: &Path) -> Vec<ArtifactRef> {
    let hooks_dir = artifact_dir.join("hooks");
    if hooks_dir.is_dir() {
        vec![ArtifactRef {
            producer_id: "merge-hooks".to_string(),
            path: path_for_model(project_root, &hooks_dir),
        }]
    } else {
        Vec::new()
    }
}

fn set_candidate_executing(
    store: &WorkModelStore,
    work_item_id: &str,
    candidate_id: &str,
) -> Result<()> {
    update_candidate(store, work_item_id, candidate_id, |candidate| {
        candidate.review_state = MergeCandidateReviewState::Pending;
        candidate.merge_state = MergeCandidateMergeState {
            status: MergeCandidateMergeStatus::Executing,
            merged_commit: None,
            failure_reason: None,
            check_artifacts: Vec::new(),
            review_artifacts: Vec::new(),
        };
    })
}

fn record_candidate_failure(
    store: &WorkModelStore,
    work_item_id: &str,
    candidate_id: &str,
    reason: String,
    check_artifacts: Vec<ArtifactRef>,
    review_artifacts: Vec<ArtifactRef>,
) -> Result<()> {
    update_candidate(store, work_item_id, candidate_id, |candidate| {
        if candidate.merge_state.status == MergeCandidateMergeStatus::Merged
            && candidate.merge_state.merged_commit.is_some()
        {
            return;
        }
        if !review_artifacts.is_empty()
            || candidate.review_state == MergeCandidateReviewState::Reviewing
        {
            candidate.review_state = MergeCandidateReviewState::Failed;
        }
        candidate.merge_state = MergeCandidateMergeState {
            status: MergeCandidateMergeStatus::Failed,
            merged_commit: None,
            failure_reason: Some(reason),
            check_artifacts,
            review_artifacts,
        };
    })
}

fn record_candidate_merged(
    store: &WorkModelStore,
    work_item_id: &str,
    candidate_id: &str,
    merged_commit: &str,
    check_artifacts: Vec<ArtifactRef>,
    review_artifacts: Vec<ArtifactRef>,
) -> Result<()> {
    update_candidate(store, work_item_id, candidate_id, |candidate| {
        candidate.review_state = MergeCandidateReviewState::Passed;
        candidate.merge_state = MergeCandidateMergeState {
            status: MergeCandidateMergeStatus::Merged,
            merged_commit: Some(merged_commit.to_string()),
            failure_reason: None,
            check_artifacts,
            review_artifacts,
        };
    })
}

fn update_candidate(
    store: &WorkModelStore,
    work_item_id: &str,
    candidate_id: &str,
    update: impl FnOnce(&mut crate::work_model::MergeCandidate),
) -> Result<()> {
    let mut item = read_work_item_or_not_found(store, work_item_id)?;
    let candidate = item
        .merge_candidates
        .iter_mut()
        .find(|candidate| candidate.id == candidate_id)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Merge Candidate {:?} not found in Work Item {:?}",
                candidate_id,
                work_item_id
            )
        })?;
    update(candidate);
    store.write_work_item(&item)?;
    Ok(())
}

fn candidate_merged_commit(
    store: &WorkModelStore,
    work_item_id: &str,
    candidate_id: &str,
) -> Result<Option<String>> {
    let item = read_work_item_or_not_found(store, work_item_id)?;
    let candidate = item
        .merge_candidates
        .iter()
        .find(|candidate| candidate.id == candidate_id)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Merge Candidate {:?} not found in Work Item {:?}",
                candidate_id,
                work_item_id
            )
        })?;
    if candidate.merge_state.status == MergeCandidateMergeStatus::Merged {
        Ok(candidate.merge_state.merged_commit.clone())
    } else {
        Ok(None)
    }
}

fn candidate_has_failure(
    store: &WorkModelStore,
    work_item_id: &str,
    candidate_id: &str,
) -> Result<bool> {
    let item = read_work_item_or_not_found(store, work_item_id)?;
    let candidate = item
        .merge_candidates
        .iter()
        .find(|candidate| candidate.id == candidate_id)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Merge Candidate {:?} not found in Work Item {:?}",
                candidate_id,
                work_item_id
            )
        })?;
    Ok(matches!(
        candidate.merge_state.status,
        MergeCandidateMergeStatus::Failed | MergeCandidateMergeStatus::NeedsUser
    ))
}

fn can_record_validation_failure(error: &WorkModelError) -> bool {
    matches!(
        error,
        WorkModelError::MergeCandidateAttemptReviewsNotPassed { .. }
    )
}

fn read_work_item_or_not_found(store: &WorkModelStore, id: &str) -> Result<WorkItem> {
    match store.read_work_item_for_merge_recovery(id) {
        Ok(item) => Ok(item),
        Err(WorkModelStorageError::ReadFile { source, .. })
            if source.kind() == ErrorKind::NotFound =>
        {
            bail!("Work Item {id:?} not found")
        }
        Err(error) => Err(error.into()),
    }
}

fn merge_artifact_dir(
    project_root: &Path,
    work_item_id: &str,
    attempt_id: &str,
    candidate_id: &str,
) -> PathBuf {
    project_root
        .join(WORK_ARTIFACTS_DIR)
        .join(work_item_id)
        .join(attempt_id)
        .join(candidate_id)
        .join("merge")
}

fn resolve_workspace_path(project_root: &Path, path: &str) -> PathBuf {
    let path = PathBuf::from(path);
    if path.is_absolute() {
        path
    } else {
        project_root.join(path)
    }
}

fn resolve_managed_candidate_workspace_path(
    project_root: &Path,
    path: &str,
    work_item_id: &str,
    attempt_id: &str,
) -> Result<PathBuf> {
    Ok(resolve_expected_candidate_workspace_path(
        project_root,
        path,
        work_item_id,
        attempt_id,
        "Merge Candidate source",
    )?)
}

fn rebase_candidate(source_workspace: &Path, target_branch: &str) -> Result<()> {
    let output = Command::new("git")
        .args(["-C", &source_workspace.to_string_lossy()])
        .args(["rebase", target_branch])
        .output()
        .context("Failed to run git rebase")?;
    if output.status.success() {
        return Ok(());
    }
    Command::new("git")
        .args(["-C", &source_workspace.to_string_lossy()])
        .args(["rebase", "--abort"])
        .output()
        .ok();
    bail!(
        "Rebase failed while updating Merge Candidate against {target_branch}:\n{}",
        command_output(&output)
    )
}

fn cleanup_managed_workspace(project_root: &Path, source_workspace: &Path) -> Result<()> {
    let output = Command::new("git")
        .args(["-C", &project_root.to_string_lossy()])
        .args([
            "worktree",
            "remove",
            "--force",
            &source_workspace.to_string_lossy(),
        ])
        .output()
        .context("Failed to remove managed workspace")?;
    if output.status.success() {
        Ok(())
    } else {
        bail!(
            "Failed to remove managed workspace {}:\n{}",
            source_workspace.display(),
            command_output(&output)
        )
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
    let output = Command::new("git")
        .args(["-C", &project_root.to_string_lossy()])
        .args(["worktree", "list", "--porcelain"])
        .output()?;
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

fn ensure_clean_worktree(workspace_path: &Path) -> Result<()> {
    let status = worktree_status(workspace_path)?;
    if !status.is_empty() {
        bail!(
            "Workspace {} has uncommitted changes",
            workspace_path.display()
        );
    }
    Ok(())
}

fn worktree_status(workspace_path: &Path) -> Result<String> {
    let output = git_output(
        workspace_path,
        &[
            "status",
            "--porcelain",
            "--untracked-files=normal",
            "--",
            ".",
            ":(exclude).factory",
        ],
        "check worktree status",
    )?;
    if !output.status.success() {
        bail!(
            "Failed to check worktree status:\n{}",
            command_output(&output)
        );
    }
    if !output.stdout.is_empty() {
        return Ok(String::from_utf8_lossy(&output.stdout).to_string());
    }
    Ok(String::new())
}

fn head_commit(repo: &Path) -> Result<String> {
    git_stdout(repo, &["rev-parse", "HEAD"], "resolve HEAD")
}

fn git(repo: &Path, args: &[&str], action: &str) -> Result<()> {
    let output = git_output(repo, args, action)?;
    if output.status.success() {
        return Ok(());
    }
    bail!("Failed to {action}:\n{}", command_output(&output))
}

fn git_stdout(repo: &Path, args: &[&str], action: &str) -> Result<String> {
    let output = git_output(repo, args, action)?;
    if !output.status.success() {
        bail!("Failed to {action}:\n{}", command_output(&output));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn git_output(repo: &Path, args: &[&str], action: &str) -> Result<Output> {
    Command::new("git")
        .args(["-C", &repo.to_string_lossy()])
        .args(args)
        .output()
        .with_context(|| format!("Failed to {action}"))
}

fn command_output(output: &Output) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let mut combined = String::new();
    if !stdout.trim().is_empty() {
        combined.push_str("stdout:\n");
        combined.push_str(stdout.trim_end());
        combined.push('\n');
    }
    if !stderr.trim().is_empty() {
        combined.push_str("stderr:\n");
        combined.push_str(stderr.trim_end());
        combined.push('\n');
    }
    if combined.is_empty() {
        combined.push_str("(no output)\n");
    }
    combined
}

fn path_for_model(project_root: &Path, path: &Path) -> String {
    path.strip_prefix(project_root)
        .unwrap_or(path)
        .to_string_lossy()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content::ContentResolver;
    use crate::work_model::WorkItemAbandonment;
    use crate::work_model::{AttemptReviewState, AttemptStatus, TaskOutput, TaskStatus, WorkItem};

    #[test]
    fn merge_candidate_rejects_abandoned_work_item_without_mutating_state() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = WorkModelStore::new(tmp.path());
        let mut item = WorkItem {
            id: "work-1".to_string(),
            title: "Keep abandoned merge terminal".to_string(),
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

        let error = match merge_candidate(WorkMergeConfig {
            project_root: tmp.path(),
            store: &store,
            work_item_id: "work-1",
            merge_candidate_id: "attempt-1-merge-candidate",
            resolver: &resolver,
            extra_args: &[],
            coder_kind: CoderKind::Codex,
            no_sandbox: true,
        }) {
            Ok(_) => panic!("abandoned Work Item should reject merge execution"),
            Err(error) => error,
        };

        assert!(error.to_string().contains("is abandoned"));
        let stored = store.read_work_item("work-1").unwrap();
        assert!(stored.abandonment.is_some());
        assert!(stored.merge_candidates.is_empty());
    }

    fn landed_candidate_store() -> (tempfile::TempDir, WorkModelStore, String, String, String) {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = WorkModelStore::new(tmp.path());
        let mut item = WorkItem {
            id: "work-1".to_string(),
            title: "Preserve landed state".to_string(),
            planning_context: None,
            instructions: None,
            abandonment: None,
            attempts: Vec::new(),
            merge_candidates: Vec::new(),
        };
        item.add_initial_attempt("attempt-1").unwrap();

        let attempt = item.attempts.first_mut().unwrap();
        attempt.status = AttemptStatus::Complete;
        attempt.review_state = Some(AttemptReviewState::Passed);
        let task = attempt.tasks.first_mut().unwrap();
        let workspace = task.workspace_access.writes.first().unwrap().clone();
        task.status = TaskStatus::Complete;
        task.output = Some(TaskOutput {
            workspace_id: workspace.id,
            workspace_path: workspace.path,
            source_branch: "main".to_string(),
            commit: "abc123".to_string(),
        });

        let candidate_id = item.create_or_get_merge_candidate("attempt-1").unwrap();
        store.create_work_item(&item).unwrap();
        record_candidate_merged(
            &store,
            "work-1",
            &candidate_id,
            "abc123",
            vec![ArtifactRef {
                producer_id: "checks".to_string(),
                path: ".factory/work/artifacts/checks.json".to_string(),
            }],
            vec![ArtifactRef {
                producer_id: "reviewer".to_string(),
                path: ".factory/work/artifacts/review.md".to_string(),
            }],
        )
        .unwrap();

        (
            tmp,
            store,
            "work-1".to_string(),
            candidate_id,
            "abc123".to_string(),
        )
    }

    #[test]
    fn post_landing_error_returns_landed_outcome_without_rewriting_state() {
        let (_tmp, store, work_item_id, candidate_id, merged_commit) = landed_candidate_store();

        let outcome = recover_landed_candidate_result(
            &store,
            &work_item_id,
            &candidate_id,
            Err(anyhow::anyhow!("candidate workspace is gone")),
        )
        .unwrap();

        assert_eq!(outcome.merge_candidate_id, candidate_id);
        assert_eq!(outcome.merged_commit, merged_commit);

        let item = store.read_work_item(&work_item_id).unwrap();
        let candidate = item
            .merge_candidates
            .iter()
            .find(|candidate| candidate.id == candidate_id)
            .unwrap();
        assert_eq!(candidate.review_state, MergeCandidateReviewState::Passed);
        assert_eq!(
            candidate.merge_state.status,
            MergeCandidateMergeStatus::Merged
        );
        assert_eq!(
            candidate.merge_state.merged_commit.as_deref(),
            Some(merged_commit.as_str())
        );
        assert!(candidate.merge_state.failure_reason.is_none());
        assert_eq!(candidate.merge_state.check_artifacts.len(), 1);
        assert_eq!(candidate.merge_state.review_artifacts.len(), 1);
    }

    #[test]
    fn record_failure_keeps_landed_candidate_landed() {
        let (_tmp, store, work_item_id, candidate_id, merged_commit) = landed_candidate_store();

        record_candidate_failure(
            &store,
            &work_item_id,
            &candidate_id,
            "late cleanup failed".to_string(),
            Vec::new(),
            Vec::new(),
        )
        .unwrap();

        let item = store.read_work_item(&work_item_id).unwrap();
        let candidate = item
            .merge_candidates
            .iter()
            .find(|candidate| candidate.id == candidate_id)
            .unwrap();
        assert_eq!(candidate.review_state, MergeCandidateReviewState::Passed);
        assert_eq!(
            candidate.merge_state.status,
            MergeCandidateMergeStatus::Merged
        );
        assert_eq!(
            candidate.merge_state.merged_commit.as_deref(),
            Some(merged_commit.as_str())
        );
        assert!(candidate.merge_state.failure_reason.is_none());
    }
}
