use anyhow::{Context, Result, bail};
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use crate::coder::{CoderKind, CoderSandbox};
use crate::content::ContentResolver;
use crate::git;
use crate::hooks::{self, HookContext, HookOutcome};
use crate::work_model::{
    ArtifactRef, MergeCandidate, MergeCandidateMergeState, MergeCandidateMergeStatus,
    MergeReviewState, Task, TaskKind, TaskStatus, WORK_ARTIFACTS_DIR, WorkItem, WorkModelError,
    WorkModelStorageError, WorkModelStore, WorkspaceAccess,
    resolve_expected_candidate_workspace_path, work_artifact_path,
};
use crate::worktree;
use crate::{credential, os};

pub struct WorkMergeConfig<'a> {
    pub project_root: &'a Path,
    pub store: &'a WorkModelStore,
    pub work_item_id: &'a str,
    pub merge_candidate_id: &'a str,
    pub resolver: &'a ContentResolver,
    pub extra_args: &'a [String],
    pub coder_kind: CoderKind,
    pub no_sandbox: bool,
    pub skip_post_merge_review: bool,
}

pub struct WorkMergeOutcome {
    pub merge_candidate_id: String,
    pub merged_commit: String,
}

#[derive(Debug)]
enum RebaseOutcome {
    Success { new_tip: String },
    NeedsUser { diagnostic: String },
}

pub fn merge_candidate(config: WorkMergeConfig<'_>) -> Result<WorkMergeOutcome> {
    // Serialize the full land boundary against Learner retry: durable state
    // reads and writes, workspace resolution and cleanliness checks, rebase,
    // merge, and post-land recovery all observe one stable candidate state.
    let land_lock_path = crate::land_lock::lock_path(config.project_root);
    let _land_lock = crate::land_lock::acquire(&land_lock_path)
        .map_err(|e| anyhow::anyhow!("failed to acquire land lock: {e}"))?;

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
        // The candidate already landed. Do not resolve workspaces, rebase, run
        // checks, or repeat the merge; resume any incomplete learner handoff
        // processing so a re-invocation converges idempotently.
        let outcome = WorkMergeOutcome {
            merge_candidate_id: candidate.id,
            merged_commit,
        };
        process_landed_follow_ups(&config, &outcome);
        return Ok(outcome);
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
    let outcome =
        recover_landed_candidate_result(config.store, config.work_item_id, &candidate.id, result)?;
    // The candidate is durably merged. Process its learner handoff in the source
    // project; a failure here never undoes the successful land.
    process_landed_follow_ups(&config, &outcome);
    Ok(outcome)
}

/// Materialize a landed Merge Candidate's learner handoff into the local
/// Observation backlog. Runs only once a candidate is durably merged, so nothing
/// materializes before merge. Any failure is a retryable follow-up-processing
/// failure that leaves the successful land intact; the persisted operation and
/// journal let a later `merge-candidate land` resume it.
fn process_landed_follow_ups(config: &WorkMergeConfig<'_>, outcome: &WorkMergeOutcome) {
    if let Err(error) = process_landed_follow_ups_at_boundary(
        config.project_root,
        config.store,
        config.work_item_id,
        &outcome.merge_candidate_id,
        &outcome.merged_commit,
    ) {
        eprintln!("  Warning: failed to update follow-up-processing recovery state: {error}");
    }
}

/// Process one landed handoff through the shared durable recovery boundary used
/// by both land and `attempt run`. Returns whether materialization completed;
/// an effect failure records candidate recovery state and returns `Ok(false)`.
pub fn process_landed_follow_ups_at_boundary(
    project_root: &Path,
    store: &WorkModelStore,
    work_item_id: &str,
    candidate_id: &str,
    merged_commit: &str,
) -> Result<bool> {
    match try_process_landed_follow_ups(
        project_root,
        store,
        work_item_id,
        candidate_id,
        merged_commit,
    ) {
        Ok(()) => {
            // Processing completed. A failed or legacy-missing Learner may have
            // caused land to retain the managed candidate workspace; remove it
            // before clearing the recovery root so cleanup cannot lose it.
            if let Err(error) = cleanup_recovered_candidate_workspace(
                project_root,
                store,
                work_item_id,
                candidate_id,
            ) {
                let stage = "cleanup-workspace";
                let next_action = format!(
                    "Re-run `fluent merge-candidate land {} {}` to finish cleanup.",
                    work_item_id, candidate_id
                );
                record_follow_up_failure(
                    store,
                    work_item_id,
                    candidate_id,
                    stage,
                    &error.to_string(),
                    &next_action,
                )?;
                eprintln!(
                    "  Warning: Merge Candidate {} follow-ups completed, but retained workspace \
                     cleanup failed: {error}",
                    candidate_id,
                );
                return Ok(false);
            }
            clear_follow_up_failure(store, work_item_id, candidate_id)?;
            Ok(true)
        }
        Err(error) => {
            // The merge stays successful. Record a retryable follow-up-processing
            // failure naming the first incomplete stage so a later land resumes.
            let origin = store.read_work_item(work_item_id).ok().and_then(|item| {
                item.merge_candidates
                    .iter()
                    .find(|candidate| candidate.id == candidate_id)
                    .map(|candidate| crate::follow_up::PostLandOrigin {
                        work_item_id: work_item_id.to_string(),
                        attempt_id: candidate.attempt_id.clone(),
                        merge_candidate_id: candidate_id.to_string(),
                        merged_commit: merged_commit.to_string(),
                    })
            });
            let stage = origin
                .as_ref()
                .and_then(|origin| {
                    crate::follow_up::first_incomplete_stage_for_origin(project_root, origin)
                })
                .unwrap_or_else(|| "validate-handoff".to_string());
            let next_action = format!(
                "Re-run `fluent merge-candidate land {} {}` to resume follow-up processing.",
                work_item_id, candidate_id
            );
            record_follow_up_failure(
                store,
                work_item_id,
                candidate_id,
                &stage,
                &error.to_string(),
                &next_action,
            )?;
            eprintln!(
                "  Warning: Merge Candidate {} landed, but learner follow-up processing did not \
                 complete at stage {stage}: {error}",
                candidate_id,
            );
            Ok(false)
        }
    }
}

/// Record a retryable follow-up-processing failure on a landed candidate without
/// changing its merged status.
fn record_follow_up_failure(
    store: &WorkModelStore,
    work_item_id: &str,
    candidate_id: &str,
    stage: &str,
    message: &str,
    next_action: &str,
) -> Result<()> {
    let mut item = read_work_item_or_not_found(store, work_item_id)?;
    if let Some(candidate) = item
        .merge_candidates
        .iter_mut()
        .find(|candidate| candidate.id == candidate_id)
    {
        candidate.merge_state.follow_up_failure =
            Some(crate::work_model::FollowUpProcessingFailure {
                stage: stage.to_string(),
                message: message.to_string(),
                next_action: next_action.to_string(),
            });
        store.write_work_item(&item)?;
    }
    Ok(())
}

/// Clear a recorded follow-up-processing failure once processing completes.
fn clear_follow_up_failure(
    store: &WorkModelStore,
    work_item_id: &str,
    candidate_id: &str,
) -> Result<()> {
    let mut item = read_work_item_or_not_found(store, work_item_id)?;
    if let Some(candidate) = item
        .merge_candidates
        .iter_mut()
        .find(|candidate| candidate.id == candidate_id)
        && candidate.merge_state.follow_up_failure.is_some()
    {
        candidate.merge_state.follow_up_failure = None;
        store.write_work_item(&item)?;
    }
    Ok(())
}

fn try_process_landed_follow_ups(
    project_root: &Path,
    store: &WorkModelStore,
    work_item_id: &str,
    candidate_id: &str,
    merged_commit: &str,
) -> Result<()> {
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
    let attempt_id = candidate.attempt_id.clone();
    let attempt = item
        .attempts
        .iter()
        .find(|attempt| attempt.id == attempt_id)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Attempt {:?} not found for Merge Candidate {:?}",
                attempt_id,
                candidate_id
            )
        })?;

    // Only a successful learner run leaves a handoff to materialize. A failed or
    // absent learner run has nothing to process here; its recovery runs the
    // Learner again and materializes the recovered handoff itself.
    crate::follow_up::materialize_learner_handoff(
        project_root,
        work_item_id,
        attempt,
        candidate_id,
        merged_commit,
    )
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
    item: &WorkItem,
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

    let target_head_before = git::run_stdout(
        target_workspace,
        &["rev-parse", &candidate.target_branch],
        "resolve target branch",
    )?;

    ensure_clean_worktree(source_workspace)?;
    let rebase_outcome = rebase_candidate(
        config,
        item,
        candidate,
        source_workspace,
        &candidate.target_branch,
        artifact_dir,
    )?;
    match rebase_outcome {
        RebaseOutcome::NeedsUser { diagnostic } => {
            record_candidate_needs_user(
                config.store,
                config.work_item_id,
                &candidate.id,
                diagnostic,
            )?;
            bail!(
                "Rebase agent could not resolve conflicts for Merge Candidate {:?}; \
                 status set to needs-user",
                candidate.id
            );
        }
        RebaseOutcome::Success { new_tip } => {
            regenerate_provenance(
                config.store,
                config.work_item_id,
                &candidate.id,
                &candidate.attempt_id,
                &target_head_before,
                &new_tip,
            )?;
        }
    }
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

    if !config.skip_post_merge_review {
        let item = read_work_item_or_not_found(config.store, config.work_item_id)?;
        let fix_depth = crate::post_merge_review::fix_depth_for(&item);
        let entry = crate::post_merge_review::QueueEntry {
            target_branch: candidate.target_branch.clone(),
            merged_commit: outcome.merged_commit.clone(),
            merged_at_unix: crate::post_merge_review::now_unix(),
            source_work_item_id: config.work_item_id.to_string(),
            source_merge_candidate_id: candidate.id.clone(),
            base_commit: target_head_before.clone(),
            fix_depth,
        };
        if let Err(error) = crate::post_merge_review::queue_and_spawn(
            config.project_root,
            entry,
            crate::post_merge_review::debounce_seconds(),
            fix_depth,
        ) {
            eprintln!("  Warning: post-merge review queue/spawn failed: {error}");
        }
    }
    Ok(outcome)
}

/// Whether the Attempt behind a landed candidate has a retryable (failed) Learner
/// record. When it does, the land retains the candidate workspace so a post-land
/// handoff-only Learner retry has a workspace to run against.
fn candidate_learning_is_retryable(
    store: &WorkModelStore,
    work_item_id: &str,
    attempt_id: &str,
) -> Result<bool> {
    let item = read_work_item_or_not_found(store, work_item_id)?;
    Ok(!item
        .attempts
        .iter()
        .find(|attempt| attempt.id == attempt_id)
        .and_then(|attempt| attempt.learning.as_ref())
        .is_some_and(|learning| learning.is_succeeded()))
}

fn cleanup_recovered_candidate_workspace(
    project_root: &Path,
    store: &WorkModelStore,
    work_item_id: &str,
    candidate_id: &str,
) -> Result<()> {
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
    let learning_succeeded = item
        .attempts
        .iter()
        .find(|attempt| attempt.id == candidate.attempt_id)
        .and_then(|attempt| attempt.learning.as_ref())
        .is_some_and(|learning| learning.is_succeeded());
    if !learning_succeeded {
        return Ok(());
    }
    let source_workspace = resolve_managed_candidate_workspace_path(
        project_root,
        &candidate.source_workspace.path,
        work_item_id,
        &candidate.attempt_id,
    )?;
    if source_workspace.exists() {
        cleanup_managed_workspace(project_root, &source_workspace)?;
    }
    Ok(())
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
    let target_head_now = git::run_stdout(
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

    git::run(
        target_workspace,
        &["checkout", &candidate.target_branch],
        "checkout target branch",
    )?;
    git::run(
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
    // Retain the workspace when the Learner is still retryable: a failed Learner
    // run recovers post-land as a handoff-only retry against this same workspace,
    // so removing it now would strand that documented recovery.
    if candidate_learning_is_retryable(config.store, config.work_item_id, &candidate.attempt_id)? {
        eprintln!(
            "  Merge Candidate {} landed; retaining its workspace for a retryable Learner run",
            candidate.id
        );
    } else if let Err(error) = cleanup_managed_workspace(config.project_root, source_workspace) {
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
    let output = git::run_raw(
        worktree_dir,
        &[
            "status",
            "--porcelain",
            "--untracked-files=normal",
            "--",
            ".",
            ":(exclude).fluent",
        ],
    )?;
    Ok(!output.stdout.is_empty())
}

fn autofix_commit_message() -> (&'static str, &'static str) {
    (
        "Conform code to project standards",
        "- Apply the fix-pre-merge hook's changes so check-pre-merge passes.",
    )
}

fn commit_autofix(worktree_dir: &Path) -> Result<()> {
    git::run(
        worktree_dir,
        &["add", "--", ".", ":(exclude).fluent"],
        "stage fix-pre-merge changes",
    )?;
    let (subject, body) = autofix_commit_message();
    git::run(
        worktree_dir,
        &["commit", "-m", subject, "-m", body],
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
        candidate.merge_review_state = MergeReviewState::Pending;
        candidate.merge_state = MergeCandidateMergeState {
            status: MergeCandidateMergeStatus::Executing,
            merged_commit: None,
            failure_reason: None,
            check_artifacts: Vec::new(),
            review_artifacts: Vec::new(),
            auto_merge_skipped: None,
            follow_up_failure: None,
        };
        crate::work_model::mark_merge_candidate_started(candidate);
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
            || candidate.merge_review_state == MergeReviewState::Reviewing
        {
            candidate.merge_review_state = MergeReviewState::Failed;
        }
        candidate.merge_state = MergeCandidateMergeState {
            status: MergeCandidateMergeStatus::Failed,
            merged_commit: None,
            failure_reason: Some(reason),
            check_artifacts,
            review_artifacts,
            auto_merge_skipped: None,
            follow_up_failure: None,
        };
        crate::work_model::set_merge_candidate_terminal(
            candidate,
            MergeCandidateMergeStatus::Failed,
        );
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
        candidate.merge_review_state = MergeReviewState::Passed;
        candidate.merge_state = MergeCandidateMergeState {
            status: MergeCandidateMergeStatus::Merged,
            merged_commit: Some(merged_commit.to_string()),
            failure_reason: None,
            check_artifacts,
            review_artifacts,
            auto_merge_skipped: None,
            follow_up_failure: None,
        };
        crate::work_model::set_merge_candidate_terminal(
            candidate,
            MergeCandidateMergeStatus::Merged,
        );
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

fn rebase_candidate(
    config: &WorkMergeConfig<'_>,
    item: &WorkItem,
    candidate: &MergeCandidate,
    source_workspace: &Path,
    target_branch: &str,
    artifact_dir: &Path,
) -> Result<RebaseOutcome> {
    rebase_candidate_with_coder(
        config,
        item,
        candidate,
        source_workspace,
        target_branch,
        artifact_dir,
        |sandbox| config.coder_kind.boxed(sandbox),
    )
}

/// Rebase a Merge Candidate with a caller-supplied coder factory.
///
/// Production builds the real coder for the resolved kind; tests inject a fake to
/// drive the launch-threading and failure-ordering paths deterministically. Once
/// the Rebase Task is reserved `Executing`, the entire remaining body — setup,
/// prompt render, sandbox build, coder launch, verification, head lookup, and
/// terminal-status writes — funnels through one terminal finalizer, so no `?` or
/// render failure can strand the Task `Executing` for outer Merge-Candidate
/// recovery. The typed transcript-pump primary is preserved on that path.
fn rebase_candidate_with_coder(
    config: &WorkMergeConfig<'_>,
    item: &WorkItem,
    candidate: &MergeCandidate,
    source_workspace: &Path,
    target_branch: &str,
    artifact_dir: &Path,
    make_coder: impl FnOnce(CoderSandbox) -> Box<dyn crate::coder::Coder>,
) -> Result<RebaseOutcome> {
    let rebase_task_id = next_rebase_task_id(item, &candidate.attempt_id);
    let rebase_artifact_dir = artifact_dir.join(&rebase_task_id);
    fs::create_dir_all(&rebase_artifact_dir)?;

    let now = crate::work_model::now_iso8601();
    let rebase_task = Task {
        id: rebase_task_id.clone(),
        kind: TaskKind::Rebase,
        status: TaskStatus::Executing,
        role: "rebase".to_string(),
        instructions: None,
        work_item_id: config.work_item_id.to_string(),
        attempt_id: Some(candidate.attempt_id.clone()),
        workspace_access: WorkspaceAccess {
            reads: Vec::new(),
            writes: vec![candidate.source_workspace.clone()],
        },
        artifact_area: Some(crate::work_model::TaskArtifactArea {
            path: work_artifact_path(config.work_item_id, &candidate.attempt_id, &rebase_task_id),
        }),
        review_context: None,
        input_artifacts: Vec::new(),
        depends_on: None,
        output: None,
        created_at: Some(now.clone()),
        started_at: Some(now),
        completed_at: None,
    };
    add_rebase_task_to_attempt(
        config.store,
        config.work_item_id,
        &candidate.attempt_id,
        rebase_task,
    )?;

    // Everything after the reservation runs inside the finalizer: a setup, prompt,
    // sandbox, launch, verification, or terminal-write failure durably terminalizes
    // the reserved Rebase Task as Failed before returning the primary error.
    match run_reserved_rebase(
        config,
        candidate,
        source_workspace,
        target_branch,
        &rebase_artifact_dir,
        &rebase_task_id,
        make_coder,
    ) {
        Ok(outcome) => Ok(outcome),
        Err(err) => Err(terminalize_rebase_failure(
            config,
            &candidate.attempt_id,
            &rebase_task_id,
            err,
        )),
    }
}

/// Run the reserved rebase body. Every fallible step returns through `?`/`bail!`
/// to the caller's terminal finalizer rather than stranding the reserved Task; the
/// give-up and success paths persist their own resumable/terminal status and return
/// `Ok`. A give-up aborts and records `NeedsUser`; success records `Complete`.
fn run_reserved_rebase(
    config: &WorkMergeConfig<'_>,
    candidate: &MergeCandidate,
    source_workspace: &Path,
    target_branch: &str,
    rebase_artifact_dir: &Path,
    rebase_task_id: &str,
    make_coder: impl FnOnce(CoderSandbox) -> Box<dyn crate::coder::Coder>,
) -> Result<RebaseOutcome> {
    let workspace_resolver = ContentResolver::new(Some(source_workspace));
    let system_prompt = workspace_resolver
        .resolve_content("prompts/rebase-system.md")
        .unwrap_or_default();

    let user_template = workspace_resolver
        .resolve_content("prompts/rebase-user.md")
        .ok_or_else(|| anyhow::anyhow!("bundled rebase-user.md must resolve"))?;
    let artifact_dir_display = rebase_artifact_dir.display().to_string();
    let prompt = crate::content::render_template(
        &user_template,
        &[
            ("target_branch", target_branch),
            ("artifact_dir", &artifact_dir_display),
        ],
    )
    .context("render rebase-user.md template with the documented context")?;

    let transcript_path = rebase_artifact_dir.join("transcript.jsonl");

    if !config.no_sandbox {
        os::check_prerequisites_for(config.coder_kind)?;
        credential::inject_credentials()?;
        credential::setup_git_signing();
    }

    let (sandbox, _sandbox_profile) = if config.no_sandbox {
        (CoderSandbox::None, None)
    } else {
        let common_git_dir = worktree::git_common_dir(source_workspace)?;
        build_coder_sandbox(
            config.coder_kind,
            config.resolver,
            source_workspace,
            &[common_git_dir, rebase_artifact_dir.to_path_buf()],
        )?
    };

    eprintln!("  Fluent           work rebase");
    eprintln!("  Work Item         {}", config.work_item_id);
    eprintln!("  Attempt           {}", candidate.attempt_id);
    eprintln!("  Target            {target_branch}");
    eprintln!("  Worktree          {}", source_workspace.display());

    // Resolve this project's pump config and thread it into the rebase-agent
    // launch, so the rebase pump uses the same layered thresholds as every other
    // entry point rather than a prior operation's state.
    let pump_config = crate::transcript_pump::resolve_config(config.project_root);
    let capture = crate::coder::TranscriptCapture::with_config(&transcript_path, pump_config);

    let coder = make_coder(sandbox);
    let exit_code = match coder.run_captured(
        &prompt,
        &system_prompt,
        source_workspace,
        config.extra_args,
        &[],
        Some(&capture),
    ) {
        Ok(code) => code,
        Err(err) => {
            // A typed pump/coder failure returns to the terminal finalizer, which
            // leaves a durable terminal Task; abort the in-progress rebase first.
            git::run_raw(source_workspace, &["rebase", "--abort"]).ok();
            return Err(err);
        }
    };

    let give_up_path = rebase_artifact_dir.join("give-up.md");

    if give_up_path.exists() {
        git::run_raw(source_workspace, &["rebase", "--abort"]).ok();
        let diagnostic = fs::read_to_string(&give_up_path)
            .unwrap_or_else(|_| "Rebase agent gave up (no diagnostic)".to_string());
        update_rebase_task_status(
            config.store,
            config.work_item_id,
            &candidate.attempt_id,
            rebase_task_id,
            TaskStatus::NeedsUser,
        )?;
        Ok(RebaseOutcome::NeedsUser { diagnostic })
    } else if exit_code == 0 {
        if let Err(reason) = verify_rebase_completed(source_workspace, target_branch) {
            git::run_raw(source_workspace, &["rebase", "--abort"]).ok();
            bail!(
                "Rebase coder exited 0 but verification failed: {reason} \
                 while rebasing Merge Candidate {:?} against {target_branch}",
                candidate.id
            );
        }
        let new_tip = head_commit(source_workspace)?;
        update_rebase_task_status(
            config.store,
            config.work_item_id,
            &candidate.attempt_id,
            rebase_task_id,
            TaskStatus::Complete,
        )?;
        Ok(RebaseOutcome::Success { new_tip })
    } else {
        git::run_raw(source_workspace, &["rebase", "--abort"]).ok();
        bail!(
            "Rebase agent failed (exit code {exit_code}) while rebasing \
             Merge Candidate {:?} against {target_branch}",
            candidate.id
        )
    }
}

fn next_rebase_task_id(item: &WorkItem, attempt_id: &str) -> String {
    let attempt = item.attempts.iter().find(|a| a.id == attempt_id);
    let existing_count = attempt
        .map(|a| {
            a.tasks
                .iter()
                .filter(|t| t.kind == TaskKind::Rebase)
                .count()
        })
        .unwrap_or(0);
    if existing_count == 0 {
        format!("{attempt_id}-rebase")
    } else {
        format!("{attempt_id}-rebase-{}", existing_count + 1)
    }
}

fn add_rebase_task_to_attempt(
    store: &WorkModelStore,
    work_item_id: &str,
    attempt_id: &str,
    task: Task,
) -> Result<()> {
    let mut item = read_work_item_or_not_found(store, work_item_id)?;
    let attempt = item
        .attempts
        .iter_mut()
        .find(|a| a.id == attempt_id)
        .ok_or_else(|| anyhow::anyhow!("Attempt {:?} not found", attempt_id))?;
    attempt.tasks.push(task);
    store.write_work_item(&item)?;
    Ok(())
}

/// Durably terminalize a reserved Rebase Task as Failed before returning `primary`,
/// so a post-reservation failure never strands the Task Executing. The primary error
/// (which may be a typed transcript-pump failure) is preserved; a failure to persist
/// the terminal state is attached as context rather than masking the primary.
fn terminalize_rebase_failure(
    config: &WorkMergeConfig<'_>,
    attempt_id: &str,
    task_id: &str,
    primary: anyhow::Error,
) -> anyhow::Error {
    if let Err(state_err) = update_rebase_task_status(
        config.store,
        config.work_item_id,
        attempt_id,
        task_id,
        TaskStatus::Failed,
    ) {
        return primary.context(format!(
            "additionally failed to persist terminal Rebase Task state: {state_err}"
        ));
    }
    primary
}

fn update_rebase_task_status(
    store: &WorkModelStore,
    work_item_id: &str,
    attempt_id: &str,
    task_id: &str,
    status: TaskStatus,
) -> Result<()> {
    let mut item = read_work_item_or_not_found(store, work_item_id)?;
    let attempt = item
        .attempts
        .iter_mut()
        .find(|a| a.id == attempt_id)
        .ok_or_else(|| anyhow::anyhow!("Attempt {:?} not found", attempt_id))?;
    if let Some(task) = attempt.tasks.iter_mut().find(|t| t.id == task_id) {
        if matches!(
            status,
            TaskStatus::Complete | TaskStatus::Failed | TaskStatus::NeedsUser
        ) {
            crate::work_model::set_task_terminal(task, status);
        } else {
            task.status = status;
        }
    }
    store.write_work_item(&item)?;
    Ok(())
}

fn record_candidate_needs_user(
    store: &WorkModelStore,
    work_item_id: &str,
    candidate_id: &str,
    diagnostic: String,
) -> Result<()> {
    update_candidate(store, work_item_id, candidate_id, |candidate| {
        if candidate.merge_state.status == MergeCandidateMergeStatus::Merged
            && candidate.merge_state.merged_commit.is_some()
        {
            return;
        }
        candidate.merge_state = MergeCandidateMergeState {
            status: MergeCandidateMergeStatus::NeedsUser,
            merged_commit: None,
            failure_reason: Some(diagnostic),
            check_artifacts: Vec::new(),
            review_artifacts: Vec::new(),
            auto_merge_skipped: None,
            follow_up_failure: None,
        };
        crate::work_model::set_merge_candidate_terminal(
            candidate,
            MergeCandidateMergeStatus::NeedsUser,
        );
    })
}

fn regenerate_provenance(
    store: &WorkModelStore,
    work_item_id: &str,
    candidate_id: &str,
    attempt_id: &str,
    accepted_base: &str,
    new_tip: &str,
) -> Result<()> {
    let mut item = read_work_item_or_not_found(store, work_item_id)?;

    let attempt = item
        .attempts
        .iter_mut()
        .find(|a| a.id == attempt_id)
        .ok_or_else(|| anyhow::anyhow!("Attempt {:?} not found", attempt_id))?;

    let write_task_ids: std::collections::HashSet<String> = attempt
        .tasks
        .iter()
        .filter(|task| task.kind == TaskKind::Write && task.status == TaskStatus::Complete)
        .map(|task| task.id.clone())
        .collect();

    for task in &mut attempt.tasks {
        if task.kind == TaskKind::Write && task.status == TaskStatus::Complete {
            if let Some(ref mut output) = task.output {
                output.base_commit = Some(accepted_base.to_string());
                output.commit = new_tip.to_string();
            }
        }
    }

    // Only artifact references that represent Write output commits move to the
    // new tip. Learner handoff, Tester, reviewer, and other non-Write references
    // are preserved: rewriting them would corrupt pointers that are not commits.
    for artifact in &mut attempt.artifacts {
        if write_task_ids.contains(&artifact.producer_id) {
            artifact.path = new_tip.to_string();
        }
    }

    let candidate = item
        .merge_candidates
        .iter_mut()
        .find(|c| c.id == candidate_id)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Merge Candidate {:?} not found in Work Item {:?}",
                candidate_id,
                work_item_id
            )
        })?;
    candidate.candidate_commit = new_tip.to_string();

    store.write_work_item(&item)?;
    Ok(())
}

fn build_coder_sandbox(
    coder_kind: CoderKind,
    resolver: &ContentResolver,
    working_dir: &Path,
    additional_writable_roots: &[PathBuf],
) -> Result<(CoderSandbox, Option<os::SandboxProfile>)> {
    let home = std::env::var("HOME").unwrap_or_default();
    let mut roots = vec![working_dir.to_path_buf()];
    roots.extend(additional_writable_roots.iter().cloned());
    let profile =
        os::render_profile_for_access_for_coder(resolver, &home, &roots, &[], coder_kind)?;
    let sandbox = CoderSandbox::SeatbeltProfile(profile.path.to_string_lossy().to_string());
    Ok((sandbox, Some(profile)))
}

fn cleanup_managed_workspace(project_root: &Path, source_workspace: &Path) -> Result<()> {
    let wt = source_workspace.to_string_lossy();
    git::run(
        project_root,
        &["worktree", "remove", "--force", &wt],
        "remove managed workspace",
    )
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

fn ensure_clean_worktree(workspace_path: &Path) -> Result<()> {
    let status = worktree_status(workspace_path)?;
    if !status.is_empty() {
        bail!(
            "Workspace {} has uncommitted changes:\n{}",
            workspace_path.display(),
            status
        );
    }
    Ok(())
}

fn worktree_status(workspace_path: &Path) -> Result<String> {
    let output = git::run_stdout(
        workspace_path,
        &[
            "status",
            "--porcelain",
            "--untracked-files=normal",
            "--",
            ".",
            ":(exclude).fluent",
        ],
        "check worktree status",
    )?;
    Ok(output)
}

fn head_commit(repo: &Path) -> Result<String> {
    git::run_stdout(repo, &["rev-parse", "HEAD"], "resolve HEAD")
}

fn verify_rebase_completed(workspace: &Path, target_branch: &str) -> Result<(), String> {
    let rebase_merge = git::run_stdout(
        workspace,
        &["rev-parse", "--git-path", "rebase-merge"],
        "check rebase-merge path",
    )
    .map_err(|e| format!("failed to resolve rebase-merge path: {e}"))?;
    if workspace.join(&rebase_merge).exists() {
        return Err("rebase still in progress (rebase-merge state present)".to_string());
    }

    let rebase_apply = git::run_stdout(
        workspace,
        &["rev-parse", "--git-path", "rebase-apply"],
        "check rebase-apply path",
    )
    .map_err(|e| format!("failed to resolve rebase-apply path: {e}"))?;
    if workspace.join(&rebase_apply).exists() {
        return Err("rebase still in progress (rebase-apply state present)".to_string());
    }

    let output = git::run_raw(
        workspace,
        &["merge-base", "--is-ancestor", target_branch, "HEAD"],
    )
    .map_err(|e| format!("failed to check merge-base ancestry: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "target branch {target_branch} is not an ancestor of HEAD"
        ));
    }

    Ok(())
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
    fn rebase_agent_installs_resolved_pump_config() {
        // B5: before launching the rebase agent, the merge executor resolves this
        // project's layered pump thresholds (project over user over built-in
        // default) and threads that immutable value into the rebase capture —
        // `rebase_candidate` calls `transcript_pump::resolve_config(project_root)`
        // and passes the result to `run_captured`. This verifies that resolution,
        // hermetically, with explicit config paths and no HOME mutation.
        use std::time::Duration;
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("project.yaml");
        std::fs::write(&project, "transcript:\n  console-preview-limit: 4096\n").unwrap();
        let user = dir.path().join("user.yaml");
        std::fs::write(
            &user,
            "transcript:\n  console-preview-limit: 1024\n  status-flush-interval-ms: 250\n",
        )
        .unwrap();

        let resolved = crate::transcript_pump::resolve_config_from(&project, Some(&user));
        assert_eq!(
            resolved.console_preview_limit, 4096,
            "the rebase agent installs the project value over the user layer"
        );
        assert_eq!(
            resolved.status_flush_interval,
            Duration::from_millis(250),
            "a key only the user layer sets falls through to the rebase agent's config"
        );

        // A malformed config fails closed to the built-in default rather than
        // leaking a stale value into the rebase launch.
        let malformed = dir.path().join("malformed.yaml");
        std::fs::write(&malformed, "transcript:\n  console-preview-limit: nope\n").unwrap();
        let reset = crate::transcript_pump::resolve_config_from(&malformed, None);
        assert_eq!(
            reset.console_preview_limit,
            crate::transcript_pump::TranscriptPumpConfig::default().console_preview_limit,
            "a malformed config must fail closed to the built-in default"
        );
    }

    /// What a recording fake coder returns after observing its launch inputs.
    enum FakeOutcome {
        /// A typed transcript-pump infrastructure failure.
        PumpError(String),
        /// Any other error, used to stop the rebase right after recording.
        GenericError(String),
    }

    /// A fake coder that records the resolved transcript capture threaded into
    /// `run_captured` and then returns a configured outcome, so a rebase launch can
    /// be driven deterministically without a real coder process.
    struct RecordingRebaseCoder {
        recorded: std::sync::Arc<std::sync::Mutex<Option<(PathBuf, usize)>>>,
        outcome: FakeOutcome,
    }

    impl crate::coder::Coder for RecordingRebaseCoder {
        fn run(
            &self,
            _prompt: &str,
            _system_prompt: &str,
            _working_dir: &Path,
            _extra_args: &[String],
            _extra_env: &[(String, String)],
            _transcript_file: Option<&Path>,
        ) -> Result<i32> {
            unreachable!("the rebase route launches through run_captured")
        }

        fn run_captured(
            &self,
            _prompt: &str,
            _system_prompt: &str,
            _working_dir: &Path,
            _extra_args: &[String],
            _extra_env: &[(String, String)],
            capture: Option<&crate::coder::TranscriptCapture<'_>>,
        ) -> Result<i32> {
            if let Some(capture) = capture {
                *self.recorded.lock().unwrap() =
                    Some((capture.path.to_path_buf(), capture.config.console_preview_limit));
            }
            match &self.outcome {
                FakeOutcome::PumpError(message) => Err(anyhow::Error::new(
                    crate::transcript_pump::TranscriptPumpError::new(message.clone()),
                )),
                FakeOutcome::GenericError(message) => Err(anyhow::anyhow!("{message}")),
            }
        }

        fn run_interactive(
            &self,
            _system_prompt: &str,
            _working_dir: &Path,
            _extra_args: &[String],
            _extra_env: &[(String, String)],
        ) -> Result<i32> {
            unreachable!("the rebase route never runs interactively")
        }
    }

    fn merge_candidate_fixture(source_workspace: &Path) -> MergeCandidate {
        MergeCandidate {
            id: "attempt-1-merge-candidate".to_string(),
            attempt_id: "attempt-1".to_string(),
            source_workspace: crate::work_model::WorkspaceRef {
                id: "candidate".to_string(),
                path: source_workspace.to_string_lossy().into_owned(),
            },
            target_workspace: crate::work_model::WorkspaceRef {
                id: "target".to_string(),
                path: source_workspace.to_string_lossy().into_owned(),
            },
            source_branch: "work/attempt-1".to_string(),
            target_branch: "main".to_string(),
            candidate_commit: "abc123".to_string(),
            merge_review_state: MergeReviewState::Pending,
            merge_state: MergeCandidateMergeState::default(),
            created_at: None,
            started_at: None,
            completed_at: None,
        }
    }

    #[test]
    fn rebase_launch_threads_resolved_capture() {
        // B8: the rebase launch route threads the project's resolved, immutable
        // TranscriptCapture into run_captured — not a dropped or default config. A
        // distinctive project console-preview-limit must arrive at the coder
        // verbatim; this fails if the route drops or re-resolves the capture.
        let tmp = tempfile::tempdir().unwrap();
        let project_root = tmp.path();
        // A distinctive project pump threshold the launch must carry through.
        let config_dir = project_root.join(".fluent");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(
            config_dir.join("config.yaml"),
            "transcript:\n  console-preview-limit: 7777\n",
        )
        .unwrap();

        let source_workspace = project_root.join("workspace");
        std::fs::create_dir_all(&source_workspace).unwrap();
        let artifact_dir = project_root.join("artifacts");

        let store = WorkModelStore::new(project_root);
        let mut item = WorkItem {
            id: "work-1".to_string(),
            title: "Rebase capture threading".to_string(),
            ..Default::default()
        };
        item.add_initial_attempt("attempt-1").unwrap();
        store.create_work_item(&item).unwrap();
        let item = store.read_work_item("work-1").unwrap();

        let candidate = merge_candidate_fixture(&source_workspace);
        let resolver = ContentResolver::new(None);
        let config = WorkMergeConfig {
            project_root,
            store: &store,
            work_item_id: "work-1",
            merge_candidate_id: "attempt-1-merge-candidate",
            resolver: &resolver,
            extra_args: &[],
            coder_kind: CoderKind::Codex,
            no_sandbox: true,
            skip_post_merge_review: false,
        };

        let recorded = std::sync::Arc::new(std::sync::Mutex::new(None));
        let recorded_for_coder = std::sync::Arc::clone(&recorded);
        let result = rebase_candidate_with_coder(
            &config,
            &item,
            &candidate,
            &source_workspace,
            "main",
            &artifact_dir,
            move |_sandbox| {
                Box::new(RecordingRebaseCoder {
                    recorded: recorded_for_coder,
                    outcome: FakeOutcome::GenericError(
                        "stop the rebase after recording the capture".to_string(),
                    ),
                })
            },
        );
        assert!(
            result.is_err(),
            "the fake coder stops the rebase after recording the capture"
        );

        let recorded = recorded.lock().unwrap();
        let (path, limit) = recorded
            .as_ref()
            .expect("the rebase route must pass a capture to run_captured, not drop it");
        assert_eq!(
            path,
            &artifact_dir
                .join("attempt-1-rebase")
                .join("transcript.jsonl"),
            "the capture must carry the rebase transcript path"
        );
        assert_eq!(
            *limit, 7777,
            "the resolved project pump threshold must be threaded verbatim, not defaulted"
        );
    }

    #[test]
    fn rebase_pump_failure_terminalizes_task_before_return() {
        // B7: a typed transcript-pump failure during the rebase launch — after the
        // Rebase Task is reserved Executing — durably terminalizes that Task before
        // returning (never leaving it Executing for outer recovery) and preserves the
        // typed pump primary.
        let tmp = tempfile::tempdir().unwrap();
        let project_root = tmp.path();
        let source_workspace = project_root.join("workspace");
        std::fs::create_dir_all(&source_workspace).unwrap();
        let artifact_dir = project_root.join("artifacts");

        let store = WorkModelStore::new(project_root);
        let mut item = WorkItem {
            id: "work-1".to_string(),
            title: "Rebase pump failure".to_string(),
            ..Default::default()
        };
        item.add_initial_attempt("attempt-1").unwrap();
        store.create_work_item(&item).unwrap();
        let item = store.read_work_item("work-1").unwrap();

        let candidate = merge_candidate_fixture(&source_workspace);
        let resolver = ContentResolver::new(None);
        let config = WorkMergeConfig {
            project_root,
            store: &store,
            work_item_id: "work-1",
            merge_candidate_id: "attempt-1-merge-candidate",
            resolver: &resolver,
            extra_args: &[],
            coder_kind: CoderKind::Codex,
            no_sandbox: true,
            skip_post_merge_review: false,
        };

        let error = rebase_candidate_with_coder(
            &config,
            &item,
            &candidate,
            &source_workspace,
            "main",
            &artifact_dir,
            |_sandbox| {
                Box::new(RecordingRebaseCoder {
                    recorded: std::sync::Arc::new(std::sync::Mutex::new(None)),
                    outcome: FakeOutcome::PumpError(
                        "write transcript-pump status: no space left on device".to_string(),
                    ),
                })
            },
        )
        .expect_err("a transcript-pump failure must return an error");

        assert!(
            error
                .downcast_ref::<crate::transcript_pump::TranscriptPumpError>()
                .is_some(),
            "the typed transcript-pump primary must be preserved, not flattened to a string"
        );

        // The reserved Rebase Task is durably terminal, never left Executing.
        let after = store.read_work_item("work-1").unwrap();
        let rebase_task = after.attempts[0]
            .tasks
            .iter()
            .find(|t| t.kind == TaskKind::Rebase)
            .expect("the rebase task was reserved");
        assert_eq!(
            rebase_task.status,
            TaskStatus::Failed,
            "the Rebase Task must be durably terminalized before return, never left Executing"
        );
    }

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

        let error = match merge_candidate(WorkMergeConfig {
            project_root: tmp.path(),
            store: &store,
            work_item_id: "work-1",
            merge_candidate_id: "attempt-1-merge-candidate",
            resolver: &resolver,
            extra_args: &[],
            coder_kind: CoderKind::Codex,
            no_sandbox: true,
            skip_post_merge_review: false,
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
        let workspace = task.workspace_access.writes.first().unwrap().clone();
        task.status = TaskStatus::Complete;
        task.output = Some(TaskOutput {
            workspace_id: workspace.id,
            workspace_path: workspace.path,
            source_branch: "main".to_string(),
            base_commit: None,
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
                path: ".fluent/work/artifacts/checks.json".to_string(),
            }],
            vec![ArtifactRef {
                producer_id: "reviewer".to_string(),
                path: ".fluent/work/artifacts/review.md".to_string(),
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
        assert_eq!(candidate.merge_review_state, MergeReviewState::Passed);
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
        assert_eq!(candidate.merge_review_state, MergeReviewState::Passed);
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

    fn completed_write_item() -> (tempfile::TempDir, WorkModelStore, WorkItem, String) {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = WorkModelStore::new(tmp.path());
        let mut item = WorkItem {
            id: "work-1".to_string(),
            title: "Provenance test".to_string(),
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
        let workspace = task.workspace_access.writes.first().unwrap().clone();
        task.status = TaskStatus::Complete;
        task.output = Some(TaskOutput {
            workspace_id: workspace.id.clone(),
            workspace_path: workspace.path.clone(),
            source_branch: "main".to_string(),
            base_commit: None,
            commit: "old-sha-1".to_string(),
        });

        let second_write = Task {
            id: "attempt-1-write-2".to_string(),
            kind: TaskKind::Write,
            status: TaskStatus::Complete,
            role: "author".to_string(),
            instructions: None,
            work_item_id: "work-1".to_string(),
            attempt_id: Some("attempt-1".to_string()),
            workspace_access: WorkspaceAccess {
                reads: Vec::new(),
                writes: vec![workspace.clone()],
            },
            artifact_area: None,
            review_context: None,
            input_artifacts: Vec::new(),
            depends_on: None,
            output: Some(TaskOutput {
                workspace_id: workspace.id,
                workspace_path: workspace.path,
                source_branch: "main".to_string(),
                base_commit: None,
                commit: "old-sha-2".to_string(),
            }),
            created_at: None,
            started_at: None,
            completed_at: None,
        };
        attempt.tasks.push(second_write);
        attempt.artifacts.push(ArtifactRef {
            producer_id: "attempt-1-write-1".to_string(),
            path: "old-sha-1".to_string(),
        });
        attempt.artifacts.push(ArtifactRef {
            producer_id: "attempt-1-write-2".to_string(),
            path: "old-sha-2".to_string(),
        });

        let candidate_id = item.create_or_get_merge_candidate("attempt-1").unwrap();
        store.create_work_item(&item).unwrap();
        (tmp, store, item, candidate_id)
    }

    #[test]
    fn regenerate_provenance_updates_all_write_tasks_and_candidate() {
        let (_tmp, store, _item, candidate_id) = completed_write_item();

        regenerate_provenance(
            &store,
            "work-1",
            &candidate_id,
            "attempt-1",
            "new-base-sha",
            "new-tip-sha",
        )
        .unwrap();

        let item = store.read_work_item("work-1").unwrap();
        let attempt = &item.attempts[0];

        for task in &attempt.tasks {
            if task.kind == TaskKind::Write && task.status == TaskStatus::Complete {
                assert_eq!(
                    task.output.as_ref().unwrap().commit,
                    "new-tip-sha",
                    "write task {} commit should be updated",
                    task.id
                );
                assert_eq!(
                    task.output.as_ref().unwrap().base_commit.as_deref(),
                    Some("new-base-sha")
                );
            }
        }

        for artifact in &attempt.artifacts {
            assert_eq!(
                artifact.path, "new-tip-sha",
                "attempt artifact {} path should be updated",
                artifact.producer_id
            );
        }

        let candidate = item
            .merge_candidates
            .iter()
            .find(|c| c.id == candidate_id)
            .unwrap();
        assert_eq!(candidate.candidate_commit, "new-tip-sha");
    }

    #[test]
    fn regenerate_provenance_leaves_non_write_tasks_unchanged() {
        let (_tmp, store, _item, candidate_id) = completed_write_item();

        // Add a rebase task with its own commit to verify it is not modified
        let mut item = store.read_work_item("work-1").unwrap();
        let attempt = item.attempts.first_mut().unwrap();
        let workspace = attempt.tasks[0].workspace_access.writes[0].clone();
        let rebase_task = Task {
            id: "attempt-1-rebase".to_string(),
            kind: TaskKind::Rebase,
            status: TaskStatus::Complete,
            role: "rebase".to_string(),
            instructions: None,
            work_item_id: "work-1".to_string(),
            attempt_id: Some("attempt-1".to_string()),
            workspace_access: WorkspaceAccess {
                reads: Vec::new(),
                writes: vec![workspace],
            },
            artifact_area: None,
            review_context: None,
            input_artifacts: Vec::new(),
            depends_on: None,
            output: None,
            created_at: None,
            started_at: None,
            completed_at: None,
        };
        attempt.tasks.push(rebase_task);
        store.write_work_item(&item).unwrap();

        regenerate_provenance(
            &store,
            "work-1",
            &candidate_id,
            "attempt-1",
            "new-base-sha",
            "new-tip-sha",
        )
        .unwrap();

        let item = store.read_work_item("work-1").unwrap();
        let attempt = &item.attempts[0];

        // Write tasks should be updated
        for task in &attempt.tasks {
            if task.kind == TaskKind::Write && task.status == TaskStatus::Complete {
                assert_eq!(
                    task.output.as_ref().unwrap().commit,
                    "new-tip-sha",
                    "write task {} should be updated",
                    task.id
                );
            }
        }

        // Rebase task should remain unmodified
        let rebase = attempt
            .tasks
            .iter()
            .find(|t| t.kind == TaskKind::Rebase)
            .unwrap();
        assert!(
            rebase.output.is_none(),
            "rebase task output should remain None"
        );

        let candidate = item
            .merge_candidates
            .iter()
            .find(|c| c.id == candidate_id)
            .unwrap();
        assert_eq!(candidate.candidate_commit, "new-tip-sha");
    }

    #[test]
    fn regenerate_provenance_updates_write_commit_artifacts_only() {
        let (_tmp, store, _item, candidate_id) = completed_write_item();

        // A non-Write artifact reference — e.g. a Tester result — is not a commit.
        let mut item = store.read_work_item("work-1").unwrap();
        item.attempts[0].artifacts.push(ArtifactRef {
            producer_id: "attempt-1-tester".to_string(),
            path: ".fluent/work/artifacts/work-1/attempt-1/attempt-1-tester/tester-results.json"
                .to_string(),
        });
        store.write_work_item(&item).unwrap();

        regenerate_provenance(
            &store,
            "work-1",
            &candidate_id,
            "attempt-1",
            "new-base-sha",
            "new-tip-sha",
        )
        .unwrap();

        let item = store.read_work_item("work-1").unwrap();
        let artifacts = &item.attempts[0].artifacts;
        for artifact in artifacts {
            if artifact.producer_id.contains("-write-") {
                assert_eq!(
                    artifact.path, "new-tip-sha",
                    "write-commit artifact {} moves to the new tip",
                    artifact.producer_id
                );
            }
        }
        let tester = artifacts
            .iter()
            .find(|a| a.producer_id == "attempt-1-tester")
            .unwrap();
        assert_eq!(
            tester.path,
            ".fluent/work/artifacts/work-1/attempt-1/attempt-1-tester/tester-results.json",
            "a non-Write artifact reference is preserved"
        );
    }

    #[test]
    fn regenerate_provenance_preserves_learner_handoff_reference() {
        let (_tmp, store, _item, candidate_id) = completed_write_item();

        let handoff = crate::follow_up::ArtifactRef {
            path: ".fluent/work/artifacts/work-1/attempt-1/learner/handoff.json".to_string(),
            digest: "sha256:abc".to_string(),
        };
        let mut item = store.read_work_item("work-1").unwrap();
        item.attempts[0].learning = Some(crate::work_model::AttemptLearning::succeeded(
            1,
            handoff.clone(),
        ));
        store.write_work_item(&item).unwrap();

        regenerate_provenance(
            &store,
            "work-1",
            &candidate_id,
            "attempt-1",
            "new-base-sha",
            "new-tip-sha",
        )
        .unwrap();

        let item = store.read_work_item("work-1").unwrap();
        let learning = item.attempts[0].learning.as_ref().unwrap();
        assert_eq!(
            learning.handoff.as_ref().unwrap(),
            &handoff,
            "the learner handoff reference survives a rebase unchanged"
        );
    }

    #[test]
    fn next_rebase_task_id_increments() {
        let mut item = WorkItem {
            id: "work-1".to_string(),
            title: "ID generation".to_string(),
            planning_context: None,
            instructions: None,
            abandonment: None,
            post_merge_review_fix_depth: None,
            attempts: Vec::new(),
            merge_candidates: Vec::new(),
            ..Default::default()
        };
        item.add_initial_attempt("attempt-1").unwrap();

        assert_eq!(next_rebase_task_id(&item, "attempt-1"), "attempt-1-rebase");

        let rebase_task = |id: &str, status: TaskStatus| Task {
            id: id.to_string(),
            kind: TaskKind::Rebase,
            status,
            role: "rebase".to_string(),
            instructions: None,
            work_item_id: "work-1".to_string(),
            attempt_id: Some("attempt-1".to_string()),
            workspace_access: WorkspaceAccess {
                reads: Vec::new(),
                writes: Vec::new(),
            },
            artifact_area: None,
            review_context: None,
            input_artifacts: Vec::new(),
            depends_on: None,
            output: None,
            created_at: None,
            started_at: None,
            completed_at: None,
        };

        item.attempts[0]
            .tasks
            .push(rebase_task("attempt-1-rebase", TaskStatus::Complete));
        assert_eq!(
            next_rebase_task_id(&item, "attempt-1"),
            "attempt-1-rebase-2"
        );

        item.attempts[0]
            .tasks
            .push(rebase_task("attempt-1-rebase-2", TaskStatus::Failed));
        assert_eq!(
            next_rebase_task_id(&item, "attempt-1"),
            "attempt-1-rebase-3"
        );
    }

    #[test]
    fn record_candidate_needs_user_sets_status_and_diagnostic() {
        let (_tmp, store, _item, candidate_id) = completed_write_item();

        record_candidate_needs_user(
            &store,
            "work-1",
            &candidate_id,
            "Cannot resolve semantic conflict in lib.rs".to_string(),
        )
        .unwrap();

        let item = store.read_work_item("work-1").unwrap();
        let candidate = item
            .merge_candidates
            .iter()
            .find(|c| c.id == candidate_id)
            .unwrap();
        assert_eq!(
            candidate.merge_state.status,
            MergeCandidateMergeStatus::NeedsUser
        );
        assert_eq!(
            candidate.merge_state.failure_reason.as_deref(),
            Some("Cannot resolve semantic conflict in lib.rs")
        );
    }

    #[test]
    fn record_needs_user_preserves_landed_candidate() {
        let (_tmp, store, work_item_id, candidate_id, _merged) = landed_candidate_store();

        record_candidate_needs_user(
            &store,
            &work_item_id,
            &candidate_id,
            "should not overwrite".to_string(),
        )
        .unwrap();

        let item = store.read_work_item(&work_item_id).unwrap();
        let candidate = item
            .merge_candidates
            .iter()
            .find(|c| c.id == candidate_id)
            .unwrap();
        assert_eq!(
            candidate.merge_state.status,
            MergeCandidateMergeStatus::Merged
        );
    }

    #[test]
    fn task_kind_rebase_serializes_round_trip() {
        let json = serde_json::to_string(&TaskKind::Rebase).unwrap();
        assert_eq!(json, r#""rebase""#);
        let kind: TaskKind = serde_json::from_str(&json).unwrap();
        assert_eq!(kind, TaskKind::Rebase);
    }

    fn init_test_repo(dir: &Path) {
        git::run(dir, &["init", "-b", "main"], "init").unwrap();
        git::run(dir, &["config", "user.email", "test@test"], "config").unwrap();
        git::run(dir, &["config", "user.name", "test"], "config").unwrap();
        fs::write(dir.join("file.txt"), "initial").unwrap();
        git::run(dir, &["add", "."], "stage").unwrap();
        git::run(dir, &["commit", "-m", "initial"], "commit").unwrap();
    }

    #[test]
    fn rebase_in_progress_after_exit_zero_is_failed() {
        let tmp = tempfile::TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        fs::create_dir_all(&repo).unwrap();
        init_test_repo(&repo);

        // Create a branch with a conflicting change
        git::run(&repo, &["checkout", "-b", "feature"], "branch").unwrap();
        fs::write(repo.join("file.txt"), "feature change").unwrap();
        git::run(&repo, &["add", "."], "stage").unwrap();
        git::run(&repo, &["commit", "-m", "feature"], "commit").unwrap();

        git::run(&repo, &["checkout", "main"], "checkout").unwrap();
        fs::write(repo.join("file.txt"), "main change").unwrap();
        git::run(&repo, &["add", "."], "stage").unwrap();
        git::run(&repo, &["commit", "-m", "diverge"], "commit").unwrap();

        git::run(&repo, &["checkout", "feature"], "checkout").unwrap();
        // Start a rebase that will conflict
        let _ = git::run_raw(&repo, &["rebase", "main"]);

        let result = verify_rebase_completed(&repo, "main");
        assert!(result.is_err());
        assert!(
            result.unwrap_err().contains("rebase still in progress"),
            "should detect in-progress rebase"
        );
    }

    #[test]
    fn rebase_head_not_on_target_is_failed() {
        let tmp = tempfile::TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        fs::create_dir_all(&repo).unwrap();
        init_test_repo(&repo);

        // Create a second branch with its own commit
        git::run(&repo, &["checkout", "-b", "other"], "branch").unwrap();
        fs::write(repo.join("other.txt"), "other branch").unwrap();
        git::run(&repo, &["add", "."], "stage").unwrap();
        git::run(&repo, &["commit", "-m", "other"], "commit").unwrap();

        // Advance main past the fork point
        git::run(&repo, &["checkout", "main"], "checkout").unwrap();
        fs::write(repo.join("main.txt"), "main advance").unwrap();
        git::run(&repo, &["add", "."], "stage").unwrap();
        git::run(&repo, &["commit", "-m", "advance main"], "commit").unwrap();

        // Switch to 'other' — HEAD is NOT descended from current main tip
        git::run(&repo, &["checkout", "other"], "checkout").unwrap();

        let result = verify_rebase_completed(&repo, "main");
        assert!(result.is_err());
        assert!(
            result.unwrap_err().contains("not an ancestor of HEAD"),
            "should detect target not ancestor of HEAD"
        );
    }

    #[test]
    fn verified_rebase_is_success() {
        let tmp = tempfile::TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        fs::create_dir_all(&repo).unwrap();
        init_test_repo(&repo);

        // Create a feature branch and rebase it onto main (no conflict)
        git::run(&repo, &["checkout", "-b", "feature"], "branch").unwrap();
        fs::write(repo.join("feature.txt"), "feature work").unwrap();
        git::run(&repo, &["add", "."], "stage").unwrap();
        git::run(&repo, &["commit", "-m", "feature"], "commit").unwrap();

        // main is still an ancestor of feature HEAD (no divergence)
        let result = verify_rebase_completed(&repo, "main");
        assert!(result.is_ok(), "clean rebase should verify as success");
    }

    #[test]
    fn merge_reviewer_requires_review_skill() {
        let tmp = tempfile::TempDir::new().unwrap();
        let result = crate::work_task_executor::review_skill_path("nonexistent", tmp.path());
        assert!(result.is_err(), "unknown review role should error");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("Required review-nonexistent skill not found"),
            "error should name the missing skill: {err}"
        );
    }

    #[test]
    fn autofix_commit_subject_names_no_hook_or_process() {
        let (subject, _body) = autofix_commit_message();
        assert!(!subject.is_empty(), "subject must not be empty");
        let lower = subject.to_lowercase();
        for banned in ["fix-pre-merge", "hook", "before landing"] {
            assert!(
                !lower.contains(banned),
                "subject must not contain \"{banned}\": {subject}"
            );
        }
    }
}
