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
    execute_merge_with_coder(
        config,
        item,
        candidate,
        source_workspace,
        target_workspace,
        artifact_dir,
        |sandbox| config.coder_kind.boxed(sandbox),
    )
}

/// Execute a merge with a caller-supplied rebase-coder factory. Production builds
/// the real coder for the resolved kind; tests inject a fake to drive the rebase
/// failure route and prove the Task and Merge Candidate settle together. The
/// factory is consumed only by the rebase step; merge checks and post-merge review
/// build their own coders unchanged.
fn execute_merge_with_coder(
    config: &WorkMergeConfig<'_>,
    item: &WorkItem,
    candidate: &crate::work_model::MergeCandidate,
    source_workspace: &Path,
    target_workspace: &Path,
    artifact_dir: &Path,
    make_rebase_coder: impl FnOnce(CoderSandbox) -> Box<dyn crate::coder::Coder>,
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
    let rebase_outcome = match rebase_candidate_with_coder(
        config,
        item,
        candidate,
        source_workspace,
        &candidate.target_branch,
        artifact_dir,
        make_rebase_coder,
    ) {
        Ok(outcome) => outcome,
        Err(error) => {
            // The rebase finalizer already settled the reserved Rebase Task and this
            // Merge Candidate together, in one atomic mutation, before returning the
            // typed primary. Nothing more to persist here — a second Candidate write
            // is exactly the cross-step window that atomic settlement closes.
            return Err(error);
        }
    };
    match rebase_outcome {
        RebaseOutcome::NeedsUser { .. } => {
            // The finalizer already settled the Task and Candidate together to
            // resumable NeedsUser; do not write the Candidate a second time.
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
    // sandbox, launch, verification, or terminal-write failure settles the reserved
    // Rebase Task and its Merge Candidate together in one atomic mutation before
    // returning the primary error, so neither is stranded and recovery never finds
    // them in cross-step disagreement. A give-up settles both to resumable NeedsUser
    // through the same reducer; only success writes the Task Complete on its own (the
    // Candidate continues to merge checks and is not yet terminal).
    match run_reserved_rebase(
        config,
        candidate,
        source_workspace,
        target_branch,
        &rebase_artifact_dir,
        &rebase_task_id,
        make_coder,
    ) {
        Ok(RebaseOutcome::NeedsUser { diagnostic }) => {
            // A give-up is a resumable pause, not a hard failure.
            settle_reserved_rebase_together(
                config,
                &candidate.attempt_id,
                &rebase_task_id,
                &candidate.id,
                false,
                &diagnostic,
            )?;
            Ok(RebaseOutcome::NeedsUser { diagnostic })
        }
        Ok(outcome) => Ok(outcome),
        Err(err) => Err(settle_reserved_rebase_failure(
            config,
            &candidate.attempt_id,
            &rebase_task_id,
            &candidate.id,
            err,
        )),
    }
}

/// Run the reserved rebase body. Every fallible step returns through `?`/`bail!`
/// to the caller's terminal finalizer rather than stranding the reserved Task. A
/// give-up aborts and returns `NeedsUser` with the Task still Executing, so the
/// caller settles the Task and Merge Candidate together atomically; success records
/// the Task `Complete` on its own, since the Candidate then continues to merge
/// checks and is not yet terminal.
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
    // Persist the coder's supervision report in the rebase artifact directory, then
    // take its terminal outcome, so a group-sweep diagnostic is durable rather than
    // dropped with the ManagedChild.
    let completion = coder.run_captured_reported(
        &prompt,
        &system_prompt,
        source_workspace,
        config.extra_args,
        &[],
        Some(&capture),
    );
    let exit_code = match crate::coder::finish_supervised_coder_run(completion, rebase_artifact_dir)
    {
        Ok(code) => code,
        Err(err) => {
            // A typed pump/coder failure returns to the terminal finalizer, which
            // leaves a durable terminal Task; abort the in-progress rebase first and
            // compose a genuine abort failure as a typed secondary rather than
            // dropping it, without masking the pump/coder primary.
            let err = match abort_rebase_if_in_progress(source_workspace) {
                Ok(()) => err,
                Err(abort_err) => err.context(format!(
                    "additionally failed to abort the in-progress rebase: {abort_err:#}"
                )),
            };
            return Err(err);
        }
    };

    let give_up_path = rebase_artifact_dir.join("give-up.md");

    if give_up_path.exists() {
        let abort = abort_rebase_if_in_progress(source_workspace);
        let mut diagnostic = fs::read_to_string(&give_up_path)
            .unwrap_or_else(|_| "Rebase agent gave up (no diagnostic)".to_string());
        if let Err(abort_err) = abort {
            diagnostic.push_str(&format!(
                "\n\nAdditionally, aborting the in-progress rebase failed: {abort_err:#}"
            ));
        }
        // The reserved Task stays Executing here; the caller settles it together with
        // the Merge Candidate to resumable NeedsUser in one atomic mutation.
        Ok(RebaseOutcome::NeedsUser { diagnostic })
    } else if exit_code == 0 {
        if let Err(reason) = verify_rebase_completed(source_workspace, target_branch) {
            let abort = abort_rebase_if_in_progress(source_workspace);
            let mut message = format!(
                "Rebase coder exited 0 but verification failed: {reason} \
                 while rebasing Merge Candidate {:?} against {target_branch}",
                candidate.id
            );
            if let Err(abort_err) = abort {
                message.push_str(&format!(
                    "; additionally failed to abort the in-progress rebase: {abort_err:#}"
                ));
            }
            bail!("{message}");
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
        let abort = abort_rebase_if_in_progress(source_workspace);
        let mut message = format!(
            "Rebase agent failed (exit code {exit_code}) while rebasing \
             Merge Candidate {:?} against {target_branch}",
            candidate.id
        );
        if let Err(abort_err) = abort {
            message.push_str(&format!(
                "; additionally failed to abort the in-progress rebase: {abort_err:#}"
            ));
        }
        bail!("{message}")
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

/// Settle a reserved Rebase Task and its Merge Candidate together in one atomic
/// model mutation, so a post-reservation failure never strands the Task Executing
/// and never leaves the Task and Candidate in cross-step disagreement.
///
/// Terminalizing the Task in one `write_work_item` transaction and settling the
/// Candidate in a separate one leaves a crash window in which the Task is terminal
/// while the Candidate is still `Executing`; post-run recovery then reclassifies the
/// Candidate out of step with its Task. Routing both writes through a single
/// `mutate_work_item` reducer — which requires the exact Attempt, reserved Rebase
/// Task, and Candidate under one held model lock — makes their settlement
/// all-or-nothing.
///
/// A typed transcript-pump infrastructure failure is resumable — the transport, not
/// the rebase, is the fault — so both entities settle to `NeedsUser`; any other
/// failure settles both to a hard `Failed`. The primary error is preserved; a
/// failure to persist the settlement is attached as context rather than masking the
/// typed primary.
fn settle_reserved_rebase_failure(
    config: &WorkMergeConfig<'_>,
    attempt_id: &str,
    rebase_task_id: &str,
    candidate_id: &str,
    primary: anyhow::Error,
) -> anyhow::Error {
    let hard_failure = primary
        .downcast_ref::<crate::transcript_pump::TranscriptPumpError>()
        .is_none();
    match settle_reserved_rebase_together(
        config,
        attempt_id,
        rebase_task_id,
        candidate_id,
        hard_failure,
        &primary.to_string(),
    ) {
        Ok(()) => primary,
        Err(state_err) => primary.context(format!(
            "additionally failed to settle the reserved Rebase Task and Merge Candidate together: {state_err}"
        )),
    }
}

/// The joint terminal disposition of a reserved Rebase Task and its Merge Candidate.
/// Computed once from BOTH freshly-read peer states so the two entities can never be
/// persisted in disagreement (never Failed/NeedsUser, Merged/NeedsUser, or
/// Complete/Failed splits).
#[derive(Clone, Copy)]
enum RebaseSettlement {
    /// The Candidate already landed: preserve it and complete an active Rebase Task.
    Merged,
    /// Both settle to a hard terminal failure.
    Failed,
    /// Both settle to a resumable pause a supported resume can retry.
    NeedsUser,
}

/// Settle the reserved Rebase Task and its Candidate together in one
/// `mutate_work_item` transaction, deciding ONE joint disposition from both peers'
/// freshly-read states before mutating either. Missing entities are model-integrity
/// failures, never silent no-ops.
///
/// Precedence: a `Merged` Candidate is preserved and its active Task is completed; a
/// hard failure — or either peer already `Failed` — settles both `Failed`; otherwise
/// a resumable fault settles both `NeedsUser`. An equal joint terminal state is a
/// no-op that keeps the first reason and timestamps.
///
/// Invariant: a `Complete` Rebase Task is valid only beside a `Merged` Candidate, so a
/// non-Merged disposition forces the Task off any inconsistent `Complete` rather than
/// leaving a `Complete`-Task / `Failed`- or `NeedsUser`-Candidate split.
fn settle_reserved_rebase_together(
    config: &WorkMergeConfig<'_>,
    attempt_id: &str,
    rebase_task_id: &str,
    candidate_id: &str,
    hard_failure: bool,
    diagnostic: &str,
) -> Result<()> {
    let attempt_id = attempt_id.to_string();
    let rebase_task_id = rebase_task_id.to_string();
    let candidate_id = candidate_id.to_string();
    let diagnostic = diagnostic.to_string();
    config
        .store
        .mutate_work_item(config.work_item_id, move |item| {
            let attempt_idx = item
                .attempts
                .iter()
                .position(|a| a.id == attempt_id)
                .ok_or(WorkModelError::AttemptNotFound {
                    id: attempt_id.clone(),
                })?;
            let task_idx = item.attempts[attempt_idx]
                .tasks
                .iter()
                .position(|t| t.id == rebase_task_id)
                .ok_or(WorkModelError::TaskNotFound {
                    id: rebase_task_id.clone(),
                })?;
            let candidate_idx = item
                .merge_candidates
                .iter()
                .position(|c| c.id == candidate_id)
                .ok_or(WorkModelError::MergeCandidateNotFound {
                    candidate_id: candidate_id.clone(),
                })?;

            // Decide ONE joint disposition from both peers' current states first.
            let candidate_state = &item.merge_candidates[candidate_idx].merge_state;
            let candidate_merged = candidate_state.status == MergeCandidateMergeStatus::Merged
                && candidate_state.merged_commit.is_some();
            let candidate_failed =
                candidate_state.status == MergeCandidateMergeStatus::Failed;
            let task_failed = item.attempts[attempt_idx].tasks[task_idx].status
                == TaskStatus::Failed;
            let settlement = if candidate_merged {
                RebaseSettlement::Merged
            } else if hard_failure || task_failed || candidate_failed {
                RebaseSettlement::Failed
            } else {
                RebaseSettlement::NeedsUser
            };

            // Apply the SAME joint disposition to both entities. Only the Merged arm may
            // leave (or complete) the Task in a preserved state; a non-Merged disposition
            // FORCES the Task to match its Candidate — including downgrading an
            // inconsistent pre-existing `Complete`, which is only ever valid beside a
            // Merged Candidate — so the peers can never be persisted as a split.
            let task = &mut item.attempts[attempt_idx].tasks[task_idx];
            match settlement {
                RebaseSettlement::Merged => settle_task_terminal(task, TaskStatus::Complete),
                RebaseSettlement::Failed => force_non_merged_task_terminal(task, TaskStatus::Failed),
                RebaseSettlement::NeedsUser => {
                    force_non_merged_task_terminal(task, TaskStatus::NeedsUser)
                }
            }
            let candidate = &mut item.merge_candidates[candidate_idx];
            match settlement {
                // A landed Candidate is left exactly as it merged.
                RebaseSettlement::Merged => {}
                RebaseSettlement::Failed => settle_candidate_terminal(
                    candidate,
                    MergeCandidateMergeStatus::Failed,
                    &diagnostic,
                ),
                RebaseSettlement::NeedsUser => settle_candidate_terminal(
                    candidate,
                    MergeCandidateMergeStatus::NeedsUser,
                    &diagnostic,
                ),
            }
            Ok(())
        })?;
    Ok(())
}

/// Terminalize a reserved Rebase Task with monotonic precedence: a recorded
/// `Complete`/`Failed` terminal is preserved and a hard `Failed` upgrades a resumable
/// `NeedsUser`, so an idempotent re-settlement or a dominating fault never regresses,
/// and an equal terminal state keeps the first timestamps.
fn settle_task_terminal(task: &mut Task, terminal: TaskStatus) {
    let applies = match (&task.status, &terminal) {
        (TaskStatus::Complete | TaskStatus::Failed, _) => false,
        (TaskStatus::NeedsUser, TaskStatus::Failed) => true,
        (TaskStatus::NeedsUser, _) => false,
        // Planned / Executing / Reviewing accept any terminal transition.
        _ => true,
    };
    if applies {
        crate::work_model::set_task_terminal(task, terminal);
    }
}

/// Force a reserved Rebase Task to a non-Merged joint terminal, downgrading an
/// inconsistent pre-existing `Complete` so the Task can never disagree with its
/// non-Merged Candidate. A Complete Rebase Task is only valid beside a Merged Candidate
/// (handled by the Merged arm), so beside a non-Merged Candidate it is resolved toward
/// the joint disposition rather than preserved as a split. An already-`Failed` Task is
/// preserved (idempotent, keeps the first reason and timestamps), and an equal
/// `NeedsUser` is likewise preserved while a hard `Failed` still upgrades it.
fn force_non_merged_task_terminal(task: &mut Task, terminal: TaskStatus) {
    let applies = match (&task.status, &terminal) {
        (TaskStatus::Failed, _) => false,
        (TaskStatus::NeedsUser, TaskStatus::NeedsUser) => false,
        _ => true,
    };
    if applies {
        crate::work_model::set_task_terminal(task, terminal);
    }
}

/// Settle a Merge Candidate to a terminal merge state in step with its Rebase Task,
/// respecting the same precedence: a `Merged` Candidate is preserved, a hard `Failed`
/// dominates a resumable `NeedsUser`, and an equal terminal state keeps the first
/// diagnostic and timestamps.
fn settle_candidate_terminal(
    candidate: &mut MergeCandidate,
    terminal: MergeCandidateMergeStatus,
    diagnostic: &str,
) {
    use MergeCandidateMergeStatus::{Failed, Merged, NeedsUser};
    let applies = match (&candidate.merge_state.status, &terminal) {
        (Merged, _) if candidate.merge_state.merged_commit.is_some() => false,
        (Failed, _) => false,
        (NeedsUser, Failed) => true,
        (NeedsUser, _) => false,
        _ => true,
    };
    if !applies {
        return;
    }
    candidate.merge_state = MergeCandidateMergeState {
        status: terminal.clone(),
        merged_commit: None,
        failure_reason: Some(diagnostic.to_string()),
        check_artifacts: Vec::new(),
        review_artifacts: Vec::new(),
        auto_merge_skipped: None,
        follow_up_failure: None,
    };
    crate::work_model::set_merge_candidate_terminal(candidate, terminal);
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
    // A structurally absent reserved Rebase Task is a model-integrity failure, not
    // a silent no-op: terminalizing a Task that the reservation should have created
    // must record its state or surface why it could not, so a missing entity never
    // masquerades as a successful terminal write.
    let task = attempt
        .tasks
        .iter_mut()
        .find(|t| t.id == task_id)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Rebase Task {:?} not found in Attempt {:?}",
                task_id,
                attempt_id
            )
        })?;
    if matches!(
        status,
        TaskStatus::Complete | TaskStatus::Failed | TaskStatus::NeedsUser
    ) {
        crate::work_model::set_task_terminal(task, status);
    } else {
        task.status = status;
    }
    store.write_work_item(&item)?;
    Ok(())
}

/// Whether a rebase is currently in progress in `workspace`. Decides whether an
/// abort is a required cleanup step or a no-op: a coder that failed before the
/// rebase started — or a workspace that is not a git repository — leaves no state
/// to abort, so a benign "no rebase in progress" is never treated as a failure.
fn rebase_in_progress(workspace: &Path) -> bool {
    for state in ["rebase-merge", "rebase-apply"] {
        if let Ok(relative) = git::run_stdout(
            workspace,
            &["rev-parse", "--git-path", state],
            "check rebase state path",
        ) {
            if workspace.join(relative.trim()).exists() {
                return true;
            }
        }
    }
    false
}

/// Abort an in-progress rebase, returning a typed diagnostic when a genuine abort
/// fails so a failed cleanup is never silently dropped through `.ok()`.
///
/// A rebase that is in progress but cannot be aborted is a real integrity fault;
/// callers compose it as a typed secondary rather than masking the primary. When
/// no rebase is in progress the abort is a no-op and returns `Ok`.
fn abort_rebase_if_in_progress(workspace: &Path) -> Result<()> {
    if !rebase_in_progress(workspace) {
        return Ok(());
    }
    let output =
        git::run_raw(workspace, &["rebase", "--abort"]).context("spawn git rebase --abort")?;
    if output.status.success() {
        return Ok(());
    }
    bail!(
        "git rebase --abort failed (exit {}): {}",
        output
            .status
            .code()
            .map(|code| code.to_string())
            .unwrap_or_else(|| "terminated by signal".to_string()),
        String::from_utf8_lossy(&output.stderr).trim()
    )
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

    /// Build a Work Item whose Attempt carries a completed Write Task and one valid
    /// Merge Candidate in `Executing` merge state, rooted at `source_workspace`. The
    /// model rejects a candidate without a completed Write Task and matching
    /// source/target/branch/commit provenance, so the rebase-settlement fixtures
    /// construct the full valid state (via `create_or_get_merge_candidate`) rather
    /// than attaching a bare candidate that `create_work_item` would refuse.
    fn executing_candidate_item(
        work_id: &str,
        source_workspace: &Path,
    ) -> (WorkItem, MergeCandidate) {
        let mut item = WorkItem {
            id: work_id.to_string(),
            title: "Rebase failure settlement".to_string(),
            ..Default::default()
        };
        item.add_initial_attempt("attempt-1").unwrap();

        let attempt = item.attempts.first_mut().unwrap();
        attempt.status = AttemptStatus::Complete;
        attempt.review_state = Some(AttemptReviewState::Passed);

        let task = attempt.tasks.first_mut().unwrap();
        let workspace_id = task.workspace_access.writes.first().unwrap().id.clone();
        task.status = TaskStatus::Complete;
        task.output = Some(TaskOutput {
            workspace_id,
            workspace_path: source_workspace.to_string_lossy().into_owned(),
            source_branch: "main".to_string(),
            base_commit: None,
            commit: "abc123".to_string(),
        });

        let candidate_id = item.create_or_get_merge_candidate("attempt-1").unwrap();
        let candidate = item
            .merge_candidates
            .iter_mut()
            .find(|candidate| candidate.id == candidate_id)
            .unwrap();
        candidate.merge_state.status = MergeCandidateMergeStatus::Executing;
        let candidate = candidate.clone();
        (item, candidate)
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
        // Rebase Task is reserved Executing — settles that Task AND its Merge Candidate
        // together in one atomic mutation before returning (neither left Executing for
        // outer recovery), preserves the typed pump primary, and records a resumable
        // NeedsUser terminal for both (the transport, not the rebase, is the fault)
        // like every other reserved phase.
        let tmp = tempfile::tempdir().unwrap();
        let project_root = tmp.path();
        let source_workspace = project_root.join("workspace");
        std::fs::create_dir_all(&source_workspace).unwrap();
        let artifact_dir = project_root.join("artifacts");

        let store = WorkModelStore::new(project_root);
        let (item, candidate) = executing_candidate_item("work-1", &source_workspace);
        store.create_work_item(&item).unwrap();
        let item = store.read_work_item("work-1").unwrap();

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

        // The reserved Rebase Task and its Candidate are durably terminal together,
        // neither left Executing, and a pump fault records a resumable NeedsUser for
        // both (not a hard Failed).
        let after = store.read_work_item("work-1").unwrap();
        let rebase_task = after.attempts[0]
            .tasks
            .iter()
            .find(|t| t.kind == TaskKind::Rebase)
            .expect("the rebase task was reserved");
        assert_eq!(
            rebase_task.status,
            TaskStatus::NeedsUser,
            "a transcript-pump fault terminalizes the Rebase Task to resumable NeedsUser, \
             never left Executing and never a hard Failed"
        );
        assert_eq!(
            after.merge_candidates[0].merge_state.status,
            MergeCandidateMergeStatus::NeedsUser,
            "the Merge Candidate settles to NeedsUser together with its Task, never left \
             Executing"
        );
    }

    #[test]
    fn settle_reserved_rebase_failure_settles_task_and_candidate_by_disposition() {
        // B7: the atomic settlement reducer terminalizes the reserved Rebase Task and
        // its Merge Candidate together in one mutation, keyed on the fault disposition.
        // A resumable transcript-pump fault settles both to NeedsUser; any other fault
        // settles both to a hard Failed. Missing entities are model-integrity errors,
        // never silent no-ops.
        let tmp = tempfile::TempDir::new().unwrap();
        let store = WorkModelStore::new(tmp.path());
        let resolver = ContentResolver::new(None);
        let ws = tmp.path().join("ws");

        // Two independent Work Items so each holds exactly one valid Candidate for its
        // Attempt. Each Candidate is built from a completed Write Task and reserves an
        // Executing Rebase Task, exactly the state a post-reservation failure leaves.
        let reserve = |work_id: &str| -> (MergeCandidate, String) {
            let (mut item, candidate) = executing_candidate_item(work_id, &ws);
            let rebase_task_id = next_rebase_task_id(&item, "attempt-1");
            let now = crate::work_model::now_iso8601();
            item.attempts[0].tasks.push(Task {
                id: rebase_task_id.clone(),
                kind: TaskKind::Rebase,
                status: TaskStatus::Executing,
                role: "rebase".to_string(),
                instructions: None,
                work_item_id: work_id.to_string(),
                attempt_id: Some("attempt-1".to_string()),
                workspace_access: WorkspaceAccess {
                    reads: Vec::new(),
                    writes: vec![candidate.source_workspace.clone()],
                },
                artifact_area: Some(crate::work_model::TaskArtifactArea {
                    path: work_artifact_path(work_id, "attempt-1", &rebase_task_id),
                }),
                review_context: None,
                input_artifacts: Vec::new(),
                depends_on: None,
                output: None,
                created_at: Some(now.clone()),
                started_at: Some(now),
                completed_at: None,
            });
            store.create_work_item(&item).unwrap();
            (candidate, rebase_task_id)
        };
        let (pump_candidate, pump_task_id) = reserve("work-pump");
        let (hard_candidate, hard_task_id) = reserve("work-hard");

        let config = |work_id: &'static str| WorkMergeConfig {
            project_root: tmp.path(),
            store: &store,
            work_item_id: work_id,
            merge_candidate_id: "attempt-1-merge-candidate",
            resolver: &resolver,
            extra_args: &[],
            coder_kind: CoderKind::Codex,
            no_sandbox: true,
            skip_post_merge_review: false,
        };

        let pump_primary = settle_reserved_rebase_failure(
            &config("work-pump"),
            "attempt-1",
            &pump_task_id,
            &pump_candidate.id,
            anyhow::Error::new(crate::transcript_pump::TranscriptPumpError::new(
                "write transcript-pump status: no space left on device".to_string(),
            )),
        );
        assert!(
            pump_primary
                .downcast_ref::<crate::transcript_pump::TranscriptPumpError>()
                .is_some(),
            "the typed pump primary survives an atomic settlement"
        );
        let hard_primary = settle_reserved_rebase_failure(
            &config("work-hard"),
            "attempt-1",
            &hard_task_id,
            &hard_candidate.id,
            anyhow::anyhow!("rebase agent failed (exit code 3)"),
        );
        assert!(hard_primary.to_string().contains("exit code 3"));

        let pump_stored = store.read_work_item("work-pump").unwrap();
        assert_eq!(
            pump_stored.attempts[0]
                .tasks
                .iter()
                .find(|t| t.kind == TaskKind::Rebase)
                .unwrap()
                .status,
            TaskStatus::NeedsUser,
            "a pump fault settles the Rebase Task to NeedsUser"
        );
        assert_eq!(
            pump_stored.merge_candidates[0].merge_state.status,
            MergeCandidateMergeStatus::NeedsUser,
            "a resumable transcript-pump fault settles the Candidate to NeedsUser in step"
        );

        let hard_stored = store.read_work_item("work-hard").unwrap();
        assert_eq!(
            hard_stored.attempts[0]
                .tasks
                .iter()
                .find(|t| t.kind == TaskKind::Rebase)
                .unwrap()
                .status,
            TaskStatus::Failed,
            "any other fault settles the Rebase Task to a hard Failed"
        );
        assert_eq!(
            hard_stored.merge_candidates[0].merge_state.status,
            MergeCandidateMergeStatus::Failed,
            "any other fault settles the Candidate to a hard Failed in step"
        );

        // A missing Rebase Task is a model-integrity failure, surfaced as context on
        // the primary rather than a silent no-op.
        let missing = settle_reserved_rebase_failure(
            &config("work-pump"),
            "attempt-1",
            "attempt-1-rebase-absent",
            &pump_candidate.id,
            anyhow::anyhow!("primary rebase fault"),
        );
        let rendered = format!("{missing:#}");
        assert!(
            rendered.contains("primary rebase fault") && rendered.contains("not found"),
            "a missing entity composes as context on the preserved primary: {rendered}"
        );
    }

    #[test]
    fn settle_reserved_rebase_computes_one_joint_disposition_never_a_split() {
        // B7: the settlement decides ONE joint disposition from BOTH freshly-read peer
        // states, so the Task and Candidate can never be persisted in disagreement.
        // A resumable pump fault whose peer Candidate is already Failed settles BOTH
        // Failed (Failed dominates); a Merged Candidate is preserved and its active Task
        // is Completed even under a hard fault (never Merged/Failed or Merged/NeedsUser).
        let tmp = tempfile::TempDir::new().unwrap();
        let store = WorkModelStore::new(tmp.path());
        let resolver = ContentResolver::new(None);
        let ws = tmp.path().join("ws");

        // Reserve an Executing Rebase Task under a valid Candidate, then force the
        // Candidate into `status` before settling.
        let reserve = |work_id: &str, status: MergeCandidateMergeStatus| -> (String, String) {
            let (mut item, candidate) = executing_candidate_item(work_id, &ws);
            let rebase_task_id = next_rebase_task_id(&item, "attempt-1");
            let now = crate::work_model::now_iso8601();
            item.attempts[0].tasks.push(Task {
                id: rebase_task_id.clone(),
                kind: TaskKind::Rebase,
                status: TaskStatus::Executing,
                role: "rebase".to_string(),
                instructions: None,
                work_item_id: work_id.to_string(),
                attempt_id: Some("attempt-1".to_string()),
                workspace_access: WorkspaceAccess {
                    reads: Vec::new(),
                    writes: vec![candidate.source_workspace.clone()],
                },
                artifact_area: Some(crate::work_model::TaskArtifactArea {
                    path: work_artifact_path(work_id, "attempt-1", &rebase_task_id),
                }),
                review_context: None,
                input_artifacts: Vec::new(),
                depends_on: None,
                output: None,
                created_at: Some(now.clone()),
                started_at: Some(now),
                completed_at: None,
            });
            let stored = item.merge_candidates[0].id.clone();
            let is_merged = status == MergeCandidateMergeStatus::Merged;
            let candidate = &mut item.merge_candidates[0];
            candidate.merge_state.status = status;
            if is_merged {
                candidate.merge_state.merged_commit = Some("deadbeef".to_string());
            }
            store.create_work_item(&item).unwrap();
            (stored, rebase_task_id)
        };
        let config = |work_id: &'static str| WorkMergeConfig {
            project_root: tmp.path(),
            store: &store,
            work_item_id: work_id,
            merge_candidate_id: "attempt-1-merge-candidate",
            resolver: &resolver,
            extra_args: &[],
            coder_kind: CoderKind::Codex,
            no_sandbox: true,
            skip_post_merge_review: false,
        };

        // Peer Candidate already Failed + a resumable pump fault → BOTH Failed.
        let (peer_candidate, peer_task) =
            reserve("work-peer-failed", MergeCandidateMergeStatus::Failed);
        settle_reserved_rebase_failure(
            &config("work-peer-failed"),
            "attempt-1",
            &peer_task,
            &peer_candidate,
            anyhow::Error::new(crate::transcript_pump::TranscriptPumpError::new(
                "write transcript-pump status: no space left on device".to_string(),
            )),
        );
        let stored = store.read_work_item("work-peer-failed").unwrap();
        assert_eq!(
            stored.attempts[0]
                .tasks
                .iter()
                .find(|t| t.kind == TaskKind::Rebase)
                .unwrap()
                .status,
            TaskStatus::Failed,
            "a peer-Failed Candidate drives the Task to Failed, never a NeedsUser/Failed split"
        );
        assert_eq!(
            stored.merge_candidates[0].merge_state.status,
            MergeCandidateMergeStatus::Failed
        );

        // Merged Candidate + a hard fault → Candidate preserved Merged, Task Completed.
        let (merged_candidate, merged_task) =
            reserve("work-merged", MergeCandidateMergeStatus::Merged);
        settle_reserved_rebase_failure(
            &config("work-merged"),
            "attempt-1",
            &merged_task,
            &merged_candidate,
            anyhow::anyhow!("rebase agent failed (exit code 3)"),
        );
        let stored = store.read_work_item("work-merged").unwrap();
        assert_eq!(
            stored.merge_candidates[0].merge_state.status,
            MergeCandidateMergeStatus::Merged,
            "a landed Candidate is preserved, never regressed to Failed/NeedsUser"
        );
        assert_eq!(
            stored.attempts[0]
                .tasks
                .iter()
                .find(|t| t.kind == TaskKind::Rebase)
                .unwrap()
                .status,
            TaskStatus::Complete,
            "the active Rebase Task is completed in step with a Merged Candidate"
        );
    }

    #[test]
    fn settle_reserved_rebase_forces_a_complete_task_off_a_non_merged_split() {
        // B7 fidelity: an (inconsistent) already-`Complete` Rebase Task beside a
        // non-Merged Candidate must be resolved toward the joint disposition, never
        // persisted as a Complete-Task / Failed- or NeedsUser-Candidate split. A
        // Complete Rebase Task is only valid beside a Merged Candidate.
        let tmp = tempfile::TempDir::new().unwrap();
        let store = WorkModelStore::new(tmp.path());
        let resolver = ContentResolver::new(None);
        let ws = tmp.path().join("ws");

        // Reserve a *Complete* Rebase Task under an Executing (non-Merged) Candidate.
        let reserve = |work_id: &str| -> (String, String) {
            let (mut item, candidate) = executing_candidate_item(work_id, &ws);
            let rebase_task_id = next_rebase_task_id(&item, "attempt-1");
            let now = crate::work_model::now_iso8601();
            item.attempts[0].tasks.push(Task {
                id: rebase_task_id.clone(),
                kind: TaskKind::Rebase,
                status: TaskStatus::Complete,
                role: "rebase".to_string(),
                instructions: None,
                work_item_id: work_id.to_string(),
                attempt_id: Some("attempt-1".to_string()),
                workspace_access: WorkspaceAccess {
                    reads: Vec::new(),
                    writes: vec![candidate.source_workspace.clone()],
                },
                artifact_area: Some(crate::work_model::TaskArtifactArea {
                    path: work_artifact_path(work_id, "attempt-1", &rebase_task_id),
                }),
                review_context: None,
                input_artifacts: Vec::new(),
                depends_on: None,
                output: None,
                created_at: Some(now.clone()),
                started_at: Some(now.clone()),
                completed_at: Some(now),
            });
            let stored = item.merge_candidates[0].id.clone();
            store.create_work_item(&item).unwrap();
            (stored, rebase_task_id)
        };
        let config = |work_id: &'static str| WorkMergeConfig {
            project_root: tmp.path(),
            store: &store,
            work_item_id: work_id,
            merge_candidate_id: "attempt-1-merge-candidate",
            resolver: &resolver,
            extra_args: &[],
            coder_kind: CoderKind::Codex,
            no_sandbox: true,
            skip_post_merge_review: false,
        };
        let rebase_status = |work_id: &str| -> TaskStatus {
            store
                .read_work_item(work_id)
                .unwrap()
                .attempts[0]
                .tasks
                .iter()
                .find(|t| t.kind == TaskKind::Rebase)
                .unwrap()
                .status
                .clone()
        };

        // A hard/generic fault → BOTH Failed, never Complete/Failed.
        let (hard_candidate, hard_task) = reserve("work-complete-hard");
        settle_reserved_rebase_failure(
            &config("work-complete-hard"),
            "attempt-1",
            &hard_task,
            &hard_candidate,
            anyhow::anyhow!("rebase agent failed (exit code 3)"),
        );
        assert_eq!(
            rebase_status("work-complete-hard"),
            TaskStatus::Failed,
            "an inconsistent Complete Task is forced to Failed, never a Complete/Failed split"
        );
        assert_eq!(
            store.read_work_item("work-complete-hard").unwrap().merge_candidates[0]
                .merge_state
                .status,
            MergeCandidateMergeStatus::Failed,
        );

        // A resumable pump fault → BOTH NeedsUser, never Complete/NeedsUser.
        let (pump_candidate, pump_task) = reserve("work-complete-pump");
        settle_reserved_rebase_failure(
            &config("work-complete-pump"),
            "attempt-1",
            &pump_task,
            &pump_candidate,
            anyhow::Error::new(crate::transcript_pump::TranscriptPumpError::new(
                "write transcript-pump status: no space left on device".to_string(),
            )),
        );
        assert_eq!(
            rebase_status("work-complete-pump"),
            TaskStatus::NeedsUser,
            "an inconsistent Complete Task is forced to NeedsUser, never a Complete/NeedsUser split"
        );
        assert_eq!(
            store.read_work_item("work-complete-pump").unwrap().merge_candidates[0]
                .merge_state
                .status,
            MergeCandidateMergeStatus::NeedsUser,
        );
    }

    #[test]
    fn rebase_pump_failure_settles_task_and_candidate_together_through_merge_route() {
        // B7: driving the real execute_merge route (not the rebase seam in isolation),
        // a transcript-pump fault during the rebase launch settles BOTH the reserved
        // Rebase Task and the Merge Candidate to resumable NeedsUser before returning,
        // so post-run recovery never finds a still-Executing Candidate to reclassify
        // Failed out of step with its NeedsUser Task. The typed pump primary is kept.
        let tmp = tempfile::TempDir::new().unwrap();
        // Nest the project so any managed sibling resolves inside this TempDir.
        let project_root = tmp.path().join("project");
        fs::create_dir_all(&project_root).unwrap();
        let git = |args: &[&str]| {
            crate::git::run(&project_root, args, "merge route test setup").unwrap();
        };
        git(&["init", "-q", "-b", "main"]);
        git(&["config", "user.email", "t@t.co"]);
        git(&["config", "user.name", "t"]);
        git(&["commit", "-q", "--allow-empty", "-m", "baseline"]);

        // A registered source worktree for the candidate; the clean main checkout is
        // the target worktree on the same repository.
        let source_workspace = tmp.path().join("source");
        git(&[
            "worktree",
            "add",
            "-q",
            "--detach",
            source_workspace.to_str().unwrap(),
        ]);
        let target_workspace = project_root.clone();
        let artifact_dir = tmp.path().join("artifacts");
        fs::create_dir_all(&artifact_dir).unwrap();

        let store = WorkModelStore::new(project_root.as_path());
        let (item, candidate) = executing_candidate_item("work-1", &source_workspace);
        store.create_work_item(&item).unwrap();
        let item = store.read_work_item("work-1").unwrap();

        let resolver = ContentResolver::new(None);
        let config = WorkMergeConfig {
            project_root: project_root.as_path(),
            store: &store,
            work_item_id: "work-1",
            merge_candidate_id: "attempt-1-merge-candidate",
            resolver: &resolver,
            extra_args: &[],
            coder_kind: CoderKind::Codex,
            no_sandbox: true,
            skip_post_merge_review: true,
        };

        let Err(error) = execute_merge_with_coder(
            &config,
            &item,
            &candidate,
            &source_workspace,
            &target_workspace,
            &artifact_dir,
            |_sandbox| {
                Box::new(RecordingRebaseCoder {
                    recorded: std::sync::Arc::new(std::sync::Mutex::new(None)),
                    outcome: FakeOutcome::PumpError(
                        "write transcript-pump status: no space left on device".to_string(),
                    ),
                })
            },
        ) else {
            panic!("a transcript-pump failure must return an error");
        };

        assert!(
            error
                .downcast_ref::<crate::transcript_pump::TranscriptPumpError>()
                .is_some(),
            "the typed transcript-pump primary must be preserved through the merge route"
        );

        let after = store.read_work_item("work-1").unwrap();
        let rebase_task = after.attempts[0]
            .tasks
            .iter()
            .find(|t| t.kind == TaskKind::Rebase)
            .expect("the rebase task was reserved");
        assert_eq!(
            rebase_task.status,
            TaskStatus::NeedsUser,
            "the reserved Rebase Task is durably NeedsUser, never left Executing"
        );
        let candidate_status = &after.merge_candidates[0].merge_state.status;
        assert!(
            matches!(candidate_status, MergeCandidateMergeStatus::NeedsUser),
            "the Merge Candidate settles to NeedsUser together with its Task, not a hard \
             Failed and never left Executing: {candidate_status:?}"
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
    fn settle_candidate_terminal_preserves_a_merged_candidate() {
        // A rebase settlement must never clobber a Candidate that already landed:
        // Merged with a recorded commit dominates any later Failed/NeedsUser fault.
        let (_tmp, store, work_item_id, candidate_id, _merged) = landed_candidate_store();

        let mut item = store.read_work_item(&work_item_id).unwrap();
        let candidate = item
            .merge_candidates
            .iter_mut()
            .find(|c| c.id == candidate_id)
            .unwrap();
        settle_candidate_terminal(
            candidate,
            MergeCandidateMergeStatus::Failed,
            "should not overwrite a landed candidate",
        );
        assert_eq!(
            candidate.merge_state.status,
            MergeCandidateMergeStatus::Merged,
            "a Merged candidate is preserved against a later hard Failed"
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
    fn update_rebase_task_status_requires_the_reserved_task() {
        // B7: terminalizing a Rebase Task that is structurally absent is a model-
        // integrity failure, not a silent success — a missing reserved entity must
        // surface rather than report a clean terminal write.
        let tmp = tempfile::TempDir::new().unwrap();
        let store = WorkModelStore::new(tmp.path());
        let mut item = WorkItem {
            id: "work-1".to_string(),
            title: "Missing rebase task".to_string(),
            ..Default::default()
        };
        item.add_initial_attempt("attempt-1").unwrap();
        store.create_work_item(&item).unwrap();

        let error = update_rebase_task_status(
            &store,
            "work-1",
            "attempt-1",
            "attempt-1-rebase",
            TaskStatus::Failed,
        )
        .expect_err("a missing reserved Rebase Task must fail, not silently no-op");
        assert!(
            error.to_string().contains("Rebase Task"),
            "the error names the missing Rebase Task: {error}"
        );
    }

    #[test]
    fn abort_rebase_if_in_progress_aborts_a_conflicting_rebase() {
        // A rebase left in progress by a coder failure is a real cleanup step: the
        // checked abort detects the in-progress state, aborts it, and reports success
        // with the state cleared — never silently dropping the outcome through `.ok()`.
        let tmp = tempfile::TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        fs::create_dir_all(&repo).unwrap();
        init_test_repo(&repo);

        git::run(&repo, &["checkout", "-b", "feature"], "branch").unwrap();
        fs::write(repo.join("file.txt"), "feature change").unwrap();
        git::run(&repo, &["add", "."], "stage").unwrap();
        git::run(&repo, &["commit", "-m", "feature"], "commit").unwrap();

        git::run(&repo, &["checkout", "main"], "checkout").unwrap();
        fs::write(repo.join("file.txt"), "main change").unwrap();
        git::run(&repo, &["add", "."], "stage").unwrap();
        git::run(&repo, &["commit", "-m", "diverge"], "commit").unwrap();

        git::run(&repo, &["checkout", "feature"], "checkout").unwrap();
        // A conflicting rebase leaves the workspace mid-rebase.
        let _ = git::run_raw(&repo, &["rebase", "main"]);
        assert!(
            rebase_in_progress(&repo),
            "the conflicting rebase must leave the workspace mid-rebase"
        );

        abort_rebase_if_in_progress(&repo).expect("aborting an in-progress rebase must succeed");
        assert!(
            !rebase_in_progress(&repo),
            "the checked abort clears the in-progress rebase state"
        );
    }

    #[test]
    fn abort_rebase_if_in_progress_is_a_no_op_without_a_rebase() {
        // No rebase in progress — including a workspace that is not a git repository —
        // is a benign no-op, never a spurious cleanup failure.
        let tmp = tempfile::TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        fs::create_dir_all(&repo).unwrap();
        init_test_repo(&repo);
        abort_rebase_if_in_progress(&repo).expect("a clean repo aborts to a no-op");

        let non_repo = tmp.path().join("not-git");
        fs::create_dir_all(&non_repo).unwrap();
        abort_rebase_if_in_progress(&non_repo).expect("a non-git workspace aborts to a no-op");
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
