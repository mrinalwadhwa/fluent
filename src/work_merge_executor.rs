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

#[derive(Clone, Copy)]
pub struct WorkMergeConfig<'a> {
    pub project_root: &'a Path,
    pub store: &'a WorkModelStore,
    pub work_item_id: &'a str,
    pub merge_candidate_id: &'a str,
    pub resolver: &'a ContentResolver,
    pub extra_args: &'a [String],
    pub coder_kind: CoderKind,
    /// An invocation-only coder override for a land that otherwise inherits the
    /// owning Attempt's Writer mapping.
    pub coder_override: Option<CoderKind>,
    /// The optional model resolved at the command boundary.
    pub model: Option<&'a str>,
    /// The optional reasoning effort resolved at the command boundary.
    pub effort: Option<&'a str>,
    /// Resolve the local land mapping from the owning Attempt after Merge
    /// Candidate validation has established the candidate's durable state.
    pub use_attempt_mapping: bool,
    pub no_sandbox: bool,
    /// Affirmative post-merge review policy. Every caller supplies this
    /// explicitly; an omitted CLI option and the legacy `--no-post-merge-review`
    /// spelling both resolve to `false`, so a fresh land schedules no detached
    /// post-merge review unless the operator opts in with `--post-merge-review`.
    pub run_post_merge_review: bool,
}

#[derive(Debug)]
pub struct WorkMergeOutcome {
    pub merge_candidate_id: String,
    pub merged_commit: String,
}

/// Private execution envelope for a fresh-land attempt. The base commit is
/// captured before merge side effects and remains available even when the
/// low-level merge returns an error after durably recording the land.
struct MergeExecution {
    result: Result<WorkMergeOutcome>,
    base_commit: Option<String>,
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

    // Resolve an interactive land only after the executor has validated the
    // candidate. Validation failures must retain this boundary so they settle
    // the candidate as failed before any mapping lookup can report an error.
    let resolved_mapping;
    let config = if config.use_attempt_mapping {
        resolved_mapping = resolve_attempt_land_mapping(&item, &candidate, &config)?;
        WorkMergeConfig {
            coder_kind: resolved_mapping.coder,
            coder_override: None,
            model: (!resolved_mapping.model.is_empty()).then_some(resolved_mapping.model.as_str()),
            effort: resolved_mapping.effort.as_deref(),
            use_attempt_mapping: false,
            ..config
        }
    } else {
        config
    };

    // Advancement gate: a candidate may land only once its Attempt's Learner has
    // SUCCEEDED. A non-succeeded Learner blocks landing with the same durable reason
    // as Merge Candidate validation and Attempt readiness. The block is retryable —
    // the candidate is left intact so it lands once the Learner succeeds — so it is
    // not recorded as a candidate failure.
    if let Err(error) = candidate.validate_advancement(&item) {
        bail!("{error}");
    }

    // Resolve the frozen reviewed identity once from the durable model. A value
    // here means this Attempt is no-expertise and its Learner has SUCCEEDED, so the
    // reviewed Writer SHA is frozen and the land route may only ever produce that
    // exact commit. Capture-mode Work resolves `None` and keeps its existing path.
    let frozen_reviewed_sha = frozen_no_expertise_reviewed_sha(&item, &candidate);
    // The already-Merged recovery check derives the reviewed Writer SHA independently
    // of Learning status: an already-Merged no-expertise record must compare its
    // merged commit with the reviewed commit even when Learning is absent, pending,
    // or failed (B4ac). Capture-mode Work still resolves `None` and resumes normally.
    let reviewed_sha_for_merged_check = no_expertise_reviewed_sha(&item, &candidate);

    if candidate.merge_state.status == MergeCandidateMergeStatus::Merged {
        // The candidate already landed. Do not resolve workspaces, rebase, run
        // checks, or repeat the merge; resume any incomplete learner handoff
        // processing so a re-invocation converges idempotently.
        //
        // For a no-expertise Attempt, already-Merged recovery is total and
        // mode-aware. Branch on `Merged` here, before any workspace resolution, and
        // derive the reviewed Writer SHA from the model independently of Learning.
        // The persisted merged_commit must be present AND exactly equal to it: a
        // merged_commit that is absent or divergent is a fresh-Attempt contradiction
        // that fails closed before any workspace, artifact, Git, model, or follow-up
        // effect (B4aj). Capture-mode Work keeps its legacy recovery — a present
        // merged_commit resumes, and a missing one falls through unchanged rather
        // than globally changing how a missing commit is recovered.
        if let Some(reviewed_sha) = reviewed_sha_for_merged_check.as_deref() {
            let merged_commit =
                candidate
                    .merge_state
                    .merged_commit
                    .as_deref()
                    .ok_or_else(|| {
                        fresh_attempt_required(format!(
                            "no-expertise Merge Candidate {:?} is marked merged but records no \
                             merged commit; the frozen reviewed SHA is {reviewed_sha}",
                            candidate.id
                        ))
                    })?;
            if merged_commit != reviewed_sha {
                bail!(
                    "no-expertise Merge Candidate {:?} is marked merged at {} but the frozen \
                     reviewed SHA is {}; refusing to treat this land as successful. A fresh \
                     Attempt with new tests, reviews, and Learning is required.",
                    candidate.id,
                    merged_commit,
                    reviewed_sha
                );
            }
            let outcome = WorkMergeOutcome {
                merge_candidate_id: candidate.id,
                merged_commit: merged_commit.to_string(),
            };
            if let Err(error) = process_landed_follow_ups(&config, &outcome) {
                eprintln!(
                    "  Warning: failed to update follow-up-processing recovery state: {error}"
                );
            }
            return Ok(outcome);
        }
        if let Some(merged_commit) = candidate.merge_state.merged_commit.clone() {
            let outcome = WorkMergeOutcome {
                merge_candidate_id: candidate.id,
                merged_commit,
            };
            if let Err(error) = process_landed_follow_ups(&config, &outcome) {
                eprintln!(
                    "  Warning: failed to update follow-up-processing recovery state: {error}"
                );
            }
            return Ok(outcome);
        }
    }

    // A fresh, unmerged no-expertise candidate whose Learning succeeded lands its
    // frozen reviewed SHA exactly: an identity-preserving preflight runs BEFORE any
    // executing mark, Rebase Task, rebase coder, merge artifact, or Git mutation,
    // and the exact-SHA path skips the rebase and provenance-regeneration steps
    // entirely (B4u–B4x). Capture mode never enters here (B4z).
    if let Some(reviewed_sha) = frozen_reviewed_sha {
        let source_workspace = resolve_managed_candidate_workspace_path(
            config.project_root,
            &candidate.source_workspace.path,
            config.work_item_id,
            &candidate.attempt_id,
        )?;
        let target_workspace =
            resolve_workspace_path(config.project_root, &candidate.target_workspace.path);
        return land_frozen_no_expertise(
            &config,
            &candidate,
            &source_workspace,
            &target_workspace,
            &reviewed_sha,
        );
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

    let execution = execute_merge(
        &config,
        &item,
        &candidate,
        &source_workspace,
        &target_workspace,
        &artifact_dir,
    );
    finish_fresh_land_with(
        execution,
        |result| {
            recover_landed_candidate_result(
                config.store,
                config.work_item_id,
                &candidate.id,
                result,
            )
        },
        |outcome| process_landed_follow_ups(&config, outcome),
        |outcome, base_commit| {
            schedule_post_merge_review(&config, &candidate, outcome, base_commit)
        },
        config.run_post_merge_review,
    )
}

fn resolve_attempt_land_mapping(
    item: &WorkItem,
    candidate: &MergeCandidate,
    config: &WorkMergeConfig<'_>,
) -> Result<crate::work_model::CoderModelPair> {
    let attempt = item
        .attempts
        .iter()
        .find(|attempt| attempt.id == candidate.attempt_id)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Attempt {:?} owning Merge Candidate {:?} not found in Work Item {:?}",
                candidate.attempt_id,
                candidate.id,
                item.id
            )
        })?;
    let mut mapping = attempt
        .coder_mapping
        .for_task_kind(TaskKind::Rebase)
        .clone();
    if let Some(coder) = config.coder_override {
        mapping.coder = coder;
    }
    if let Some(model) = config.model {
        mapping.model = model.to_string();
    }
    if let Some(effort) = config.effort {
        mapping.effort = Some(effort.to_string());
    }
    Ok(mapping)
}

// A `#[cfg(test)]`-only observer of the follow-up-processing persistence error the
// shared land coordinator surfaces as a warning. It never alters control flow;
// it only lets a test confirm the failure originated inside the real Work-model
// write path and retained its typed storage cause (B4ak). Production carries no
// observer.
#[cfg(test)]
#[derive(Clone)]
struct FollowUpPersistObservation {
    rendered: String,
    has_typed_storage_cause: bool,
}

#[cfg(test)]
thread_local! {
    static LAST_FOLLOW_UP_PERSIST_ERROR: std::cell::RefCell<Option<FollowUpPersistObservation>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn observe_follow_up_persist_error(error: &anyhow::Error) {
    let has_typed_storage_cause = error
        .chain()
        .any(|cause| cause.is::<crate::work_model::WorkModelStorageError>());
    LAST_FOLLOW_UP_PERSIST_ERROR.with(|slot| {
        *slot.borrow_mut() = Some(FollowUpPersistObservation {
            rendered: format!("{error:#}"),
            has_typed_storage_cause,
        });
    });
}

/// Complete the ordered fresh-land coordinator with injectable boundaries so
/// its persistence gate and recovered-land data flow can be tested directly.
fn finish_fresh_land_with(
    execution: MergeExecution,
    recover: impl FnOnce(Result<WorkMergeOutcome>) -> Result<WorkMergeOutcome>,
    process_follow_ups: impl FnOnce(&WorkMergeOutcome) -> Result<bool>,
    schedule: impl FnOnce(&WorkMergeOutcome, &str),
    run_post_merge_review: bool,
) -> Result<WorkMergeOutcome> {
    let outcome = recover(execution.result)?;

    // Both Ok(true) and Ok(false) mean the complete/incomplete recovery result
    // is durable. Err means persistence is unknown, so optional review must not
    // get ahead of the recovery boundary.
    let follow_up_result = process_follow_ups(&outcome);
    if let Err(error) = &follow_up_result {
        #[cfg(test)]
        observe_follow_up_persist_error(error);
        eprintln!("  Warning: failed to update follow-up-processing recovery state: {error}");
    }

    if run_post_merge_review
        && follow_up_result.is_ok()
        && let Some(base_commit) = execution.base_commit
    {
        schedule(&outcome, &base_commit);
    }

    Ok(outcome)
}

/// Schedule the optional detached post-merge review for a fresh land, after the
/// landed Learner handoff recovery result is durable. It runs under the existing
/// debounce, singleton-lease, and corrective-depth rules; a queue/spawn failure
/// warns without failing the already-durable land. Reading the Work Item only
/// resolves the corrective fix depth, so a read failure skips the optional
/// review rather than discarding the completed merge.
fn schedule_post_merge_review(
    config: &WorkMergeConfig<'_>,
    candidate: &crate::work_model::MergeCandidate,
    outcome: &WorkMergeOutcome,
    base_commit: &str,
) {
    let fix_depth = match read_work_item_or_not_found(config.store, config.work_item_id) {
        Ok(item) => crate::post_merge_review::fix_depth_for(&item),
        Err(error) => {
            eprintln!(
                "  Warning: post-merge review scheduling skipped; reading Work Item failed: {error}"
            );
            return;
        }
    };
    let entry = crate::post_merge_review::QueueEntry {
        target_branch: candidate.target_branch.clone(),
        merged_commit: outcome.merged_commit.clone(),
        merged_at_unix: crate::post_merge_review::now_unix(),
        source_work_item_id: config.work_item_id.to_string(),
        source_merge_candidate_id: candidate.id.clone(),
        base_commit: base_commit.to_string(),
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

/// Materialize a landed Merge Candidate's learner handoff into the local
/// Observation backlog. Runs only once a candidate is durably merged, so nothing
/// materializes before merge. Any failure is a retryable follow-up-processing
/// failure that leaves the successful land intact; the persisted operation and
/// journal let a later `merge-candidate land` resume it.
fn process_landed_follow_ups(
    config: &WorkMergeConfig<'_>,
    outcome: &WorkMergeOutcome,
) -> Result<bool> {
    process_landed_follow_ups_at_boundary(
        config.project_root,
        config.store,
        config.work_item_id,
        &outcome.merge_candidate_id,
        &outcome.merged_commit,
    )
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

/// Resolve a no-expertise Attempt's reviewed Writer SHA from its latest completed
/// Write Task output, independently of the current Learning status. The reviewed SHA
/// is a durable property of the reviewed Attempt, not a value that disappears when a
/// legacy or recovery record's Learning is absent, `InProgress`, `HandoffPending`, or
/// `Failed`. It is derived from the model alone, so it is workspace-independent and
/// survives a cleaned-up candidate workspace. Capture-mode Work resolves `None`.
fn no_expertise_reviewed_sha(
    item: &WorkItem,
    candidate: &crate::work_model::MergeCandidate,
) -> Option<String> {
    if item.learner_mode != crate::work_model::LearnerMode::NoExpertise {
        return None;
    }
    let attempt = item
        .attempts
        .iter()
        .find(|attempt| attempt.id == candidate.attempt_id)?;
    attempt
        .tasks
        .iter()
        .rev()
        .find(|task| task.kind == TaskKind::Write && task.status == TaskStatus::Complete)
        .and_then(|task| task.output.as_ref())
        .map(|output| output.commit.clone())
}

/// Resolve the frozen reviewed Writer SHA for selecting the fresh, unmerged exact-SHA
/// land route: the Work is no-expertise AND that Attempt's Learner has SUCCEEDED. The
/// reviewed SHA itself is derived by [`no_expertise_reviewed_sha`], matching the
/// model-level frozen-identity tuple. `Learning == Succeeded` gates only route
/// SELECTION; it never gates identity derivation, so the already-Merged recovery
/// check still verifies a reviewed SHA for a non-succeeded record. Capture-mode Work
/// and not-yet-succeeded Learners resolve `None`, so the exact-SHA land branch never
/// runs for them.
fn frozen_no_expertise_reviewed_sha(
    item: &WorkItem,
    candidate: &crate::work_model::MergeCandidate,
) -> Option<String> {
    let attempt = item
        .attempts
        .iter()
        .find(|attempt| attempt.id == candidate.attempt_id)?;
    if !attempt
        .learning
        .as_ref()
        .is_some_and(|learning| learning.is_succeeded())
    {
        return None;
    }
    no_expertise_reviewed_sha(item, candidate)
}

/// The retryable diagnostic returned whenever an exact-SHA no-expertise land cannot
/// proceed. It never rewrites the approved commit; a fresh Attempt with new tests,
/// reviews, and Learning is the only forward path.
fn fresh_attempt_required(reason: impl std::fmt::Display) -> anyhow::Error {
    anyhow::anyhow!(
        "{reason}. A fresh Attempt with new tests, reviews, and Learning is required; the \
         approved commit is not rewritten."
    )
}

/// Read the current head of `target_branch` in the target workspace.
fn resolve_target_head(target_workspace: &Path, target_branch: &str) -> Result<String> {
    git::run_stdout(
        target_workspace,
        &["rev-parse", target_branch],
        "resolve target branch",
    )
}

/// Whether the candidate workspace is clean (no tracked or untracked change, none
/// of the `.fluent` exclusion that the merge-time dirtiness check allows). A frozen
/// land requires a fully clean tree so the reviewed SHA lands byte-for-byte.
fn candidate_fully_clean(workspace: &Path) -> Result<bool> {
    let status = git::run_stdout(
        workspace,
        &["status", "--porcelain", "--untracked-files=all"],
        "check candidate cleanliness",
    )?;
    Ok(status.trim().is_empty())
}

/// Assert the frozen no-expertise preconditions against live Git: the candidate
/// workspace is clean with HEAD at the reviewed SHA, and `target_head` is an
/// ancestor of the reviewed SHA. Any failure returns the fresh-Attempt diagnostic
/// without mutating Git.
fn assert_frozen_preconditions(
    source_workspace: &Path,
    target_workspace: &Path,
    target_branch: &str,
    reviewed_sha: &str,
    expected_target_head: &str,
) -> Result<()> {
    if !candidate_fully_clean(source_workspace)? {
        return Err(fresh_attempt_required(format!(
            "no-expertise land found the candidate workspace {} dirty; the frozen reviewed SHA \
             {reviewed_sha} cannot be landed",
            source_workspace.display()
        )));
    }
    let head = head_commit(source_workspace)?;
    if head != reviewed_sha {
        return Err(fresh_attempt_required(format!(
            "no-expertise land found candidate HEAD {head} but the frozen reviewed SHA is \
             {reviewed_sha}"
        )));
    }
    let target_head = resolve_target_head(target_workspace, target_branch)?;
    if target_head != expected_target_head {
        return Err(fresh_attempt_required(format!(
            "no-expertise land found target {target_branch} at {target_head}, expected \
             {expected_target_head}"
        )));
    }
    let ancestry = git::run_raw(
        target_workspace,
        &["merge-base", "--is-ancestor", &target_head, reviewed_sha],
    )?;
    if !ancestry.status.success() {
        return Err(fresh_attempt_required(format!(
            "no-expertise land found target head {target_head} is not an ancestor of the frozen \
             reviewed SHA {reviewed_sha}"
        )));
    }
    Ok(())
}

/// Land a frozen no-expertise Merge Candidate at its exact reviewed SHA.
///
/// Order (B4u–B4x): read the live identity BEFORE any side effect; run
/// check-pre-merge in a disposable detached worktree created at the reviewed SHA
/// (never fix-pre-merge); require the disposable worktree to stay clean at the
/// reviewed SHA and remove it; reacquire the live preconditions; fast-forward the
/// target to exactly the reviewed SHA; and persist that exact SHA. Any preflight or
/// check failure leaves the candidate unstarted-or-failed with the live source and
/// target Git unchanged and reports that a fresh Attempt is required. The rebase
/// coder is never constructed and provenance is never regenerated on this path.
fn land_frozen_no_expertise(
    config: &WorkMergeConfig<'_>,
    candidate: &crate::work_model::MergeCandidate,
    source_workspace: &Path,
    target_workspace: &Path,
    reviewed_sha: &str,
) -> Result<WorkMergeOutcome> {
    ensure_same_git_repository(config.project_root, source_workspace)?;
    ensure_same_git_repository(config.project_root, target_workspace)?;
    ensure_registered_worktree(config.project_root, source_workspace)?;

    // Preflight: read the frozen identity from live Git before touching any durable
    // state or Git. On any mismatch, bail before marking executing, creating a
    // Rebase Task, launching a coder, or creating merge artifacts (B4u/B4v).
    let target_head_before = resolve_target_head(target_workspace, &candidate.target_branch)?;
    assert_frozen_preconditions(
        source_workspace,
        target_workspace,
        &candidate.target_branch,
        reviewed_sha,
        &target_head_before,
    )?;
    // The target must satisfy the same cleanliness policy capture landing enforces,
    // checked here before the candidate is marked executing or either live repository
    // is changed (B4ai). An initially dirty target fails with no side effect.
    ensure_clean_worktree(target_workspace)?;

    // Preflight passed. Mark the candidate executing and create the artifact area,
    // then run the exact-SHA check and fast-forward under the recovery finalizer.
    let artifact_dir = merge_artifact_dir(
        config.project_root,
        config.work_item_id,
        &candidate.attempt_id,
        &candidate.id,
    );
    fs::create_dir_all(&artifact_dir)?;
    set_candidate_executing(config.store, config.work_item_id, &candidate.id)?;

    // Feed the exact-SHA execution and its pre-land base through the same durable
    // follow-up/scheduling coordinator capture landing uses (B4ab): recover the
    // durable merge, durably record complete/incomplete follow-up processing, and
    // schedule the optional post-merge review only when that recovery result is
    // durable. A follow-up-persistence error returns the successful landed outcome
    // unchanged and schedules nothing.
    let result = execute_frozen_no_expertise_land(
        config,
        candidate,
        source_workspace,
        target_workspace,
        &artifact_dir,
        reviewed_sha,
        &target_head_before,
    );
    let execution = MergeExecution {
        result,
        base_commit: Some(target_head_before.clone()),
    };
    finish_fresh_land_with(
        execution,
        |result| {
            recover_landed_candidate_result(
                config.store,
                config.work_item_id,
                &candidate.id,
                result,
            )
        },
        |outcome| process_landed_follow_ups(config, outcome),
        |outcome, base_commit| schedule_post_merge_review(config, candidate, outcome, base_commit),
        config.run_post_merge_review,
    )
}

/// Run check-pre-merge in a disposable exact-SHA worktree, then fast-forward the
/// target to the frozen reviewed SHA. Never invokes fix-pre-merge and never rebases
/// or regenerates provenance.
fn execute_frozen_no_expertise_land(
    config: &WorkMergeConfig<'_>,
    candidate: &crate::work_model::MergeCandidate,
    source_workspace: &Path,
    target_workspace: &Path,
    artifact_dir: &Path,
    reviewed_sha: &str,
    target_head_before: &str,
) -> Result<WorkMergeOutcome> {
    let check_artifacts = match run_frozen_pre_merge_check(
        config,
        candidate,
        source_workspace,
        artifact_dir,
        reviewed_sha,
    ) {
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

    // Reacquire the live preconditions before mutating the target: the check ran in
    // an isolated worktree, but the live candidate and target must still hold before
    // the fast-forward. Any drift fails closed with the live Git unchanged.
    assert_frozen_preconditions(
        source_workspace,
        target_workspace,
        &candidate.target_branch,
        reviewed_sha,
        target_head_before,
    )
    .map_err(|error| {
        // Record the failure so the candidate does not remain Executing.
        let _ = record_candidate_failure(
            config.store,
            config.work_item_id,
            &candidate.id,
            error.to_string(),
            Vec::new(),
            Vec::new(),
        );
        error
    })?;

    // Recheck the target cleanliness immediately before the fast-forward (B4ai): the
    // isolated check ran without touching the live target, but a target dirtied while
    // it ran must fail closed with the dirty target preserved and the live candidate
    // unchanged, never fast-forwarded over.
    if let Err(error) = ensure_clean_worktree(target_workspace) {
        let _ = record_candidate_failure(
            config.store,
            config.work_item_id,
            &candidate.id,
            error.to_string(),
            Vec::new(),
            Vec::new(),
        );
        return Err(error);
    }

    // Fast-forward the target to exactly the reviewed SHA. No rebase, no provenance
    // regeneration, no autofix commit — the approved commit lands verbatim.
    git::run(
        target_workspace,
        &["checkout", &candidate.target_branch],
        "checkout target branch",
    )?;
    git::run(
        target_workspace,
        &["merge", "--ff-only", reviewed_sha],
        "fast-forward target branch to frozen reviewed SHA",
    )?;

    record_candidate_merged(
        config.store,
        config.work_item_id,
        &candidate.id,
        reviewed_sha,
        check_artifacts,
        Vec::new(),
    )?;
    // A succeeded no-expertise Learner is never retryable, so the candidate
    // workspace is cleaned up like any other landed candidate.
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
        merged_commit: reviewed_sha.to_string(),
    })
}

/// Run only `check-pre-merge` against a disposable detached worktree created at the
/// frozen reviewed SHA. fix-pre-merge is never invoked. The disposable worktree must
/// remain clean at the reviewed SHA after the check; otherwise the land fails with a
/// fresh-Attempt diagnostic and the worktree is removed (retained only if removal
/// itself fails, never promoted to the live candidate). Durable logs are directed
/// into the Merge Candidate artifact area (B4w/B4x).
fn run_frozen_pre_merge_check(
    config: &WorkMergeConfig<'_>,
    candidate: &crate::work_model::MergeCandidate,
    source_workspace: &Path,
    artifact_dir: &Path,
    reviewed_sha: &str,
) -> Result<Vec<ArtifactRef>> {
    // No check hook: nothing to run, no disposable worktree needed.
    if hooks::find_hook(config.project_root, "check-pre-merge").is_none() {
        return Ok(Vec::new());
    }

    let disposable = artifact_dir.join("exact-sha-check");
    // A stale disposable worktree from a prior interrupted land is cleaned first.
    if disposable.exists() {
        let _ = git::run_raw(
            source_workspace,
            &[
                "worktree",
                "remove",
                "--force",
                &disposable.to_string_lossy(),
            ],
        );
        let _ = fs::remove_dir_all(&disposable);
    }
    git::run(
        source_workspace,
        &[
            "worktree",
            "add",
            "--detach",
            &disposable.to_string_lossy(),
            reviewed_sha,
        ],
        "create disposable exact-SHA worktree",
    )?;

    let outcome =
        run_frozen_check_in_worktree(config, candidate, &disposable, artifact_dir, reviewed_sha);

    // Always attempt to remove the disposable worktree through the real
    // registered-worktree remover. Successful removal is a land precondition, not a
    // warning-only best effort: a passing check whose disposable worktree cannot be
    // removed fails the land before the second live precondition check or any target
    // Git mutation, retaining at most that isolated worktree for cleanup (B4aa).
    let removed = remove_disposable_worktree_checked(source_workspace, &disposable);

    match outcome {
        Ok(artifacts) => match removed {
            Ok(()) => Ok(artifacts),
            Err(cleanup) => Err(fresh_attempt_required(format!(
                "check-pre-merge passed for the frozen reviewed SHA {reviewed_sha}, but removing \
                 its disposable exact-SHA worktree {} failed: {cleanup:#}. The disposable worktree \
                 is retained for cleanup and the land is aborted before any target mutation",
                disposable.display()
            ))),
        },
        Err(error) => match removed {
            Ok(()) => Err(error),
            // Both the hook and cleanup failed: preserve the hook failure as the
            // primary and attach the cleanup failure so retaining the isolated
            // worktree is only accepted with both failures visible.
            Err(cleanup) => Err(error.context(format!(
                "additionally, the disposable exact-SHA worktree {} could not be removed and is \
                 retained for cleanup: {cleanup:#}",
                disposable.display()
            ))),
        },
    }
}

/// Run the `check-pre-merge` hook inside the disposable exact-SHA worktree and
/// require it to pass and to leave the worktree clean at the reviewed SHA. Never
/// runs fix-pre-merge. Returns the check artifact on success.
fn run_frozen_check_in_worktree(
    config: &WorkMergeConfig<'_>,
    candidate: &crate::work_model::MergeCandidate,
    disposable: &Path,
    artifact_dir: &Path,
    reviewed_sha: &str,
) -> Result<Vec<ArtifactRef>> {
    let hooks_dir = artifact_dir.join("hooks");
    let context = HookContext {
        work_item_id: Some(config.work_item_id.to_string()),
        attempt_id: Some(candidate.attempt_id.clone()),
        merge_candidate_id: Some(candidate.id.clone()),
        candidate_commit: Some(reviewed_sha.to_string()),
        artifact_dir: Some(artifact_dir.to_path_buf()),
        log_dir: hooks_dir,
        ..Default::default()
    };

    let check_outcome =
        hooks::run_hook(config.project_root, "check-pre-merge", disposable, &context)?
            .expect("check-pre-merge presence checked by caller");
    let artifacts = vec![hook_artifact(config.project_root, &check_outcome)];
    if !check_outcome.passed {
        return Err(fresh_attempt_required(format!(
            "check-pre-merge failed (exit {}) for the frozen reviewed SHA {reviewed_sha}. Log: {}",
            check_outcome.exit_code,
            check_outcome.log_path.display()
        )));
    }

    // The check must not have dirtied, staged, or committed in the disposable
    // worktree; fix-pre-merge is never invoked to repair it.
    if !candidate_fully_clean(disposable)? {
        return Err(fresh_attempt_required(
            "check-pre-merge dirtied its disposable exact-SHA worktree",
        ));
    }
    let head = head_commit(disposable)?;
    if head != reviewed_sha {
        return Err(fresh_attempt_required(format!(
            "check-pre-merge moved the disposable exact-SHA worktree HEAD from {reviewed_sha} to \
             {head}"
        )));
    }
    Ok(artifacts)
}

/// Remove a disposable exact-SHA worktree and its directory through the real
/// registered-worktree remover. A failure propagates so the caller can treat
/// successful removal as a land precondition (B4aa).
fn remove_disposable_worktree(source_workspace: &Path, disposable: &Path) -> Result<()> {
    git::run(
        source_workspace,
        &[
            "worktree",
            "remove",
            "--force",
            &disposable.to_string_lossy(),
        ],
        "remove disposable exact-SHA worktree",
    )?;
    if disposable.exists() {
        fs::remove_dir_all(disposable).with_context(|| {
            format!(
                "remove disposable exact-SHA worktree directory {}",
                disposable.display()
            )
        })?;
    }
    Ok(())
}

// A `#[cfg(test)]`-only fault boundary that makes disposable-worktree removal fail
// deterministically. Production has NO injectable removal path: the non-test build
// below always calls the real registered-worktree remover directly (B4aa).
#[cfg(test)]
thread_local! {
    static DISPOSABLE_REMOVAL_FAULT: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
}

/// A `#[cfg(test)]` guard that forces the next disposable-worktree removals to fail
/// for the duration of its lifetime, so the passing-check cleanup-failure land path
/// can be driven through the public boundary without a real filesystem fault.
#[cfg(test)]
struct DisposableRemovalFaultGuard;

#[cfg(test)]
impl DisposableRemovalFaultGuard {
    fn engage() -> Self {
        DISPOSABLE_REMOVAL_FAULT.with(|fault| fault.set(true));
        DisposableRemovalFaultGuard
    }
}

#[cfg(test)]
impl Drop for DisposableRemovalFaultGuard {
    fn drop(&mut self) {
        DISPOSABLE_REMOVAL_FAULT.with(|fault| fault.set(false));
    }
}

/// Remove the disposable exact-SHA worktree. Production always calls the real
/// registered-worktree remover; only a `#[cfg(test)]` build carries the injectable
/// fault used to prove cleanup failure blocks the land.
#[cfg(test)]
fn remove_disposable_worktree_checked(source_workspace: &Path, disposable: &Path) -> Result<()> {
    if DISPOSABLE_REMOVAL_FAULT.with(|fault| fault.get()) {
        // Leave the isolated worktree in place, mirroring a real removal failure that
        // retains at most that disposable worktree for cleanup.
        return Err(anyhow::anyhow!(
            "injected disposable exact-SHA worktree removal fault for {}",
            disposable.display()
        ));
    }
    remove_disposable_worktree(source_workspace, disposable)
}

#[cfg(not(test))]
fn remove_disposable_worktree_checked(source_workspace: &Path, disposable: &Path) -> Result<()> {
    remove_disposable_worktree(source_workspace, disposable)
}

// A `#[cfg(test)]`-only override for constructing the rebase coder. Production has NO
// injectable coder: the non-test `build_rebase_coder` below always builds the real
// coder for the resolved kind. Only a test build can substitute an in-process rebase
// coder so the capture land route can be driven through public `merge_candidate`
// while route selection, validation, artifact creation, provenance regeneration, and
// final coordination all stay real (B4al).
#[cfg(test)]
thread_local! {
    static REBASE_CODER_OVERRIDE: std::cell::RefCell<
        Option<Box<dyn Fn(CoderKind, Option<String>, Option<String>) -> Box<dyn crate::coder::Coder>>>,
    > = const { std::cell::RefCell::new(None) };
}

/// A `#[cfg(test)]` guard that substitutes the rebase coder for the duration of its
/// lifetime. The override is consulted only by `build_rebase_coder`; it never replaces
/// the merge route, the coordinator, or production coder construction.
#[cfg(test)]
struct RebaseCoderOverrideGuard;

#[cfg(test)]
impl RebaseCoderOverrideGuard {
    fn engage(
        factory: impl Fn(CoderKind, Option<String>, Option<String>) -> Box<dyn crate::coder::Coder>
        + 'static,
    ) -> Self {
        REBASE_CODER_OVERRIDE.with(|slot| *slot.borrow_mut() = Some(Box::new(factory)));
        RebaseCoderOverrideGuard
    }
}

#[cfg(test)]
impl Drop for RebaseCoderOverrideGuard {
    fn drop(&mut self) {
        REBASE_CODER_OVERRIDE.with(|slot| *slot.borrow_mut() = None);
    }
}

/// Build the rebase coder for a capture-mode merge. Production always builds the real
/// coder for the resolved kind; only a `#[cfg(test)]` build carries the injectable
/// override used to drive the capture land route through the public boundary (B4al).
#[cfg(test)]
fn build_rebase_coder(
    config: &WorkMergeConfig<'_>,
    sandbox: CoderSandbox,
) -> Box<dyn crate::coder::Coder> {
    if let Some(coder) = REBASE_CODER_OVERRIDE.with(|slot| {
        slot.borrow().as_ref().map(|f| {
            f(
                config.coder_kind,
                config.model.map(str::to_string),
                config.effort.map(str::to_string),
            )
        })
    }) {
        return coder;
    }
    config
        .coder_kind
        .boxed_with_model(sandbox, config.model, config.effort)
}

#[cfg(not(test))]
fn build_rebase_coder(
    config: &WorkMergeConfig<'_>,
    sandbox: CoderSandbox,
) -> Box<dyn crate::coder::Coder> {
    config
        .coder_kind
        .boxed_with_model(sandbox, config.model, config.effort)
}

fn execute_merge(
    config: &WorkMergeConfig<'_>,
    item: &WorkItem,
    candidate: &crate::work_model::MergeCandidate,
    source_workspace: &Path,
    target_workspace: &Path,
    artifact_dir: &Path,
) -> MergeExecution {
    let mut base_commit = None;
    let result = execute_merge_with_coder(
        config,
        item,
        candidate,
        source_workspace,
        target_workspace,
        artifact_dir,
        &mut base_commit,
        |sandbox| build_rebase_coder(config, sandbox),
    );
    MergeExecution {
        result,
        base_commit,
    }
}

/// Execute a merge with a caller-supplied rebase-coder factory. Production builds
/// the real coder for the resolved kind; tests inject a fake to drive the rebase
/// failure route and prove the Task and Merge Candidate settle together. The
/// factory is consumed only by the rebase step; merge checks build their own
/// coders unchanged. The caller-provided base slot is populated as soon as the
/// target head is resolved, before any merge side effect, so a durably landed
/// merge recovered from a later error still retains its review range.
fn execute_merge_with_coder(
    config: &WorkMergeConfig<'_>,
    item: &WorkItem,
    candidate: &crate::work_model::MergeCandidate,
    source_workspace: &Path,
    target_workspace: &Path,
    artifact_dir: &Path,
    base_commit: &mut Option<String>,
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
    *base_commit = Some(target_head_before.clone());

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

    finalize_merge(
        config,
        candidate,
        source_workspace,
        target_workspace,
        &target_head_before,
        check_artifacts,
        Vec::new(),
    )
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

    // The landed commit is the final tip, which includes any fix-pre-merge autofix
    // commit made after the rebase. The first provenance regeneration ran against
    // the pre-autofix rebase tip, so settle the Write provenance and candidate
    // pointer onto the actually-landed commit here: every land pointer — the latest
    // completed Write output, candidate_commit, merged_commit, the returned outcome,
    // and target HEAD — then names one regenerated SHA (B4al). When no autofix ran
    // the merged tip equals the rebase tip and this is a no-op.
    regenerate_provenance(
        config.store,
        config.work_item_id,
        &candidate.id,
        &candidate.attempt_id,
        target_head_before,
        &merged_commit,
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

fn autofix_commit_message() -> &'static str {
    "Conform code to project standards"
}

fn commit_autofix(worktree_dir: &Path) -> Result<()> {
    git::run(
        worktree_dir,
        &["add", "--", ".", ":(exclude).fluent"],
        "stage fix-pre-merge changes",
    )?;
    let subject = autofix_commit_message();
    git::run(
        worktree_dir,
        &["commit", "-m", subject],
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
    // A Codex login problem must not create an executing Rebase Task. Keep the
    // prepared home alive through the eventual sandboxed launch.
    let codex_worker = if config.coder_kind == CoderKind::Codex && !cfg!(test) {
        let worker =
            crate::codex_worker::CodexWorkerEnvironment::prepare().map_err(anyhow::Error::new)?;
        worker.preflight().map_err(anyhow::Error::new)?;
        Some(worker)
    } else {
        None
    };
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
        codex_worker.as_ref(),
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
    codex_worker: Option<&crate::codex_worker::CodexWorkerEnvironment>,
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

    if !config.no_sandbox || codex_worker.is_some() {
        os::check_prerequisites_for(config.coder_kind)?;
        credential::inject_credentials()?;
        credential::setup_git_signing();
    }

    let (sandbox, _sandbox_profile) = if config.no_sandbox && codex_worker.is_none() {
        (CoderSandbox::None, None)
    } else {
        let common_git_dir = worktree::git_common_dir(source_workspace)?;
        build_coder_sandbox_with_codex_home(
            config.coder_kind,
            config.resolver,
            source_workspace,
            &[common_git_dir, rebase_artifact_dir.to_path_buf()],
            codex_worker.map(|worker| worker.home()),
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
        &codex_worker
            .map(|worker| vec![worker.launch_env()])
            .unwrap_or_default(),
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
            let candidate_failed = candidate_state.status == MergeCandidateMergeStatus::Failed;
            let task_failed =
                item.attempts[attempt_idx].tasks[task_idx].status == TaskStatus::Failed;
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
                RebaseSettlement::Failed => {
                    force_non_merged_task_terminal(task, TaskStatus::Failed)
                }
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

    // A frozen no-expertise identity must never be retargeted. Regenerating
    // provenance rewrites the reviewed Writer SHA across the attempt, which is
    // exactly the pointer move the freeze forbids, so this refuses to run rather
    // than staging a change the model guard would reject.
    if let Some(candidate) = item
        .merge_candidates
        .iter()
        .find(|candidate| candidate.id == candidate_id)
        && frozen_no_expertise_reviewed_sha(&item, candidate).is_some()
    {
        return Err(fresh_attempt_required(format!(
            "refusing to regenerate provenance for the frozen no-expertise identity of Merge \
             Candidate {candidate_id:?}"
        )));
    }

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

fn build_coder_sandbox_with_codex_home(
    coder_kind: CoderKind,
    resolver: &ContentResolver,
    working_dir: &Path,
    additional_writable_roots: &[PathBuf],
    codex_home: Option<&Path>,
) -> Result<(CoderSandbox, Option<os::SandboxProfile>)> {
    let home = std::env::var("HOME").unwrap_or_default();
    let mut roots = vec![working_dir.to_path_buf()];
    roots.extend(additional_writable_roots.iter().cloned());
    let profile = if let Some(codex_home) = codex_home {
        os::render_profile_for_access_for_coder_with_denied_writes_and_codex_home(
            resolver,
            &home,
            &roots,
            &[],
            &[],
            coder_kind,
            Some(codex_home),
        )?
    } else {
        os::render_profile_for_access_for_coder(resolver, &home, &roots, &[], coder_kind)?
    };
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
    fn non_codex_rebase_sandbox_retains_shared_temp_write_grants() {
        let workspace = tempfile::tempdir().unwrap();
        let resolver = ContentResolver::new(None);
        let (_sandbox, profile) = build_coder_sandbox_with_codex_home(
            CoderKind::Claude,
            &resolver,
            workspace.path(),
            &[],
            None,
        )
        .unwrap();

        let content = std::fs::read_to_string(profile.unwrap().path).unwrap();
        for root in crate::os::shared_temp_roots() {
            assert!(
                crate::os::profile_grants_write(&content, &root),
                "non-Codex rebases retain the shared temp grant on {}: {content}",
                root.display()
            );
        }
    }

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
                *self.recorded.lock().unwrap() = Some((
                    capture.path.to_path_buf(),
                    capture.config.console_preview_limit,
                ));
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
            coder_override: None,
            model: None,
            effort: None,
            use_attempt_mapping: false,
            no_sandbox: true,
            run_post_merge_review: false,
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

    /// A rebase coder that reports a non-clean supervision report, so the production
    /// rebase boundary must persist it as a sidecar.
    struct SupervisionRebaseCoder;

    impl crate::coder::Coder for SupervisionRebaseCoder {
        fn run(
            &self,
            _prompt: &str,
            _system_prompt: &str,
            _working_dir: &Path,
            _extra_args: &[String],
            _extra_env: &[(String, String)],
            _transcript_file: Option<&Path>,
        ) -> Result<i32> {
            unreachable!("the rebase route launches through run_captured_reported")
        }

        fn run_captured_reported(
            &self,
            _prompt: &str,
            _system_prompt: &str,
            _working_dir: &Path,
            _extra_args: &[String],
            _extra_env: &[(String, String)],
            _capture: Option<&crate::coder::TranscriptCapture<'_>>,
        ) -> crate::coder::CoderRunCompletion {
            crate::coder::CoderRunCompletion {
                terminal: Ok(0),
                report: crate::coder::CoderSupervisionReport {
                    launches: vec![crate::coder::CoderLaunchSupervision {
                        exit_code: Some(0),
                        group_sweep: crate::coder::GroupSweepDisposition::Unconfirmed(
                            crate::coder::ProcessOpDiagnostic {
                                operation: "kill process group".to_string(),
                                kind: Some("PermissionDenied".to_string()),
                                errno: Some(1),
                                message: Some("Operation not permitted".to_string()),
                            },
                        ),
                    }],
                },
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

    #[test]
    fn rebase_route_persists_the_coder_supervision_sidecar() {
        // B5/B6: the production rebase boundary persists the per-launch supervision
        // report as coder-supervision.json in the rebase artifact dir, before the
        // post-coder verification runs.
        let tmp = tempfile::tempdir().unwrap();
        let project_root = tmp.path();
        let source_workspace = project_root.join("workspace");
        std::fs::create_dir_all(&source_workspace).unwrap();
        let artifact_dir = project_root.join("artifacts");

        let store = WorkModelStore::new(project_root);
        let mut item = WorkItem {
            id: "work-1".to_string(),
            title: "Rebase supervision sidecar".to_string(),
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
            coder_override: None,
            model: None,
            effort: None,
            use_attempt_mapping: false,
            no_sandbox: true,
            run_post_merge_review: false,
        };

        // The coder exits 0 with an unconfirmed sweep; the post-coder verification then
        // fails on the non-repo workspace, but the sidecar is persisted beforehand.
        let _ = rebase_candidate_with_coder(
            &config,
            &item,
            &candidate,
            &source_workspace,
            "main",
            &artifact_dir,
            move |_sandbox| Box::new(SupervisionRebaseCoder),
        );
        assert!(
            artifact_dir
                .join("attempt-1-rebase")
                .join(crate::coder::CODER_SUPERVISION_SIDECAR)
                .exists(),
            "the production rebase boundary persists coder-supervision.json"
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
            coder_override: None,
            model: None,
            effort: None,
            use_attempt_mapping: false,
            no_sandbox: true,
            run_post_merge_review: false,
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
            coder_override: None,
            model: None,
            effort: None,
            use_attempt_mapping: false,
            no_sandbox: true,
            run_post_merge_review: false,
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
            coder_override: None,
            model: None,
            effort: None,
            use_attempt_mapping: false,
            no_sandbox: true,
            run_post_merge_review: false,
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
            coder_override: None,
            model: None,
            effort: None,
            use_attempt_mapping: false,
            no_sandbox: true,
            run_post_merge_review: false,
        };
        let rebase_status = |work_id: &str| -> TaskStatus {
            store.read_work_item(work_id).unwrap().attempts[0]
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
            store
                .read_work_item("work-complete-hard")
                .unwrap()
                .merge_candidates[0]
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
            store
                .read_work_item("work-complete-pump")
                .unwrap()
                .merge_candidates[0]
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
            coder_override: None,
            model: None,
            effort: None,
            use_attempt_mapping: false,
            no_sandbox: true,
            run_post_merge_review: false,
        };

        let mut base_commit = None;
        let Err(error) = execute_merge_with_coder(
            &config,
            &item,
            &candidate,
            &source_workspace,
            &target_workspace,
            &artifact_dir,
            &mut base_commit,
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
            coder_override: None,
            model: None,
            effort: None,
            use_attempt_mapping: false,
            no_sandbox: true,
            run_post_merge_review: false,
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
    fn fresh_land_records_follow_up_outcome_before_post_merge_review() {
        let (_tmp, store, work_item_id, candidate_id, merged_commit) = landed_candidate_store();
        let outcome = finish_fresh_land_with(
            MergeExecution {
                result: Ok(WorkMergeOutcome {
                    merge_candidate_id: candidate_id.clone(),
                    merged_commit: merged_commit.clone(),
                }),
                base_commit: Some("base".to_string()),
            },
            |result| result,
            |_outcome| {
                record_follow_up_failure(
                    &store,
                    &work_item_id,
                    &candidate_id,
                    "observation",
                    "observation materialization failed",
                    "retry land",
                )?;
                Ok(false)
            },
            |_outcome, base_commit| {
                assert_eq!(base_commit, "base");
                let item = store.read_work_item(&work_item_id).unwrap();
                let candidate = item
                    .merge_candidates
                    .iter()
                    .find(|candidate| candidate.id == candidate_id)
                    .unwrap();
                assert_eq!(
                    candidate
                        .merge_state
                        .follow_up_failure
                        .as_ref()
                        .map(|failure| failure.stage.as_str()),
                    Some("observation"),
                    "the incomplete follow-up result must be durable at the exact scheduling boundary"
                );
            },
            true,
        )
        .unwrap();

        assert_eq!(outcome.merged_commit, merged_commit);
    }

    #[test]
    fn fresh_land_skips_post_merge_review_when_follow_up_persistence_is_unknown() {
        let scheduled = std::cell::Cell::new(false);
        let outcome = finish_fresh_land_with(
            MergeExecution {
                result: Ok(WorkMergeOutcome {
                    merge_candidate_id: "candidate-1".to_string(),
                    merged_commit: "merged".to_string(),
                }),
                base_commit: Some("base".to_string()),
            },
            |result| result,
            |_outcome| Err(anyhow::anyhow!("persist recovery state")),
            |_outcome, _base_commit| scheduled.set(true),
            true,
        )
        .unwrap();

        assert_eq!(outcome.merged_commit, "merged");
        assert!(
            !scheduled.get(),
            "unknown recovery persistence must suppress optional review"
        );
    }

    #[test]
    fn recovered_fresh_land_preserves_base_commit_for_post_merge_review() {
        let (_tmp, store, work_item_id, candidate_id, merged_commit) = landed_candidate_store();
        let scheduled = std::cell::RefCell::new(Vec::new());

        let outcome = finish_fresh_land_with(
            MergeExecution {
                result: Err(anyhow::anyhow!("cleanup failed after durable merge")),
                base_commit: Some("pre-land-base".to_string()),
            },
            |result| recover_landed_candidate_result(&store, &work_item_id, &candidate_id, result),
            |_outcome| Ok(true),
            |outcome, base_commit| {
                scheduled
                    .borrow_mut()
                    .push((outcome.merged_commit.clone(), base_commit.to_string()));
            },
            true,
        )
        .unwrap();

        assert_eq!(outcome.merged_commit, merged_commit);
        assert_eq!(
            scheduled.into_inner(),
            vec![(merged_commit, "pre-land-base".to_string())],
            "a recovered fresh land must schedule once with its real pre-land base"
        );
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
    fn commit_autofix_writes_approved_message() {
        // Drive the real commit_autofix path in an isolated repository and
        // inspect the complete persisted message, not just the helper's
        // in-memory result. Equality with the production source proves the Git
        // boundary persisted the message with full fidelity; equality with the
        // declared approved wording proves the source still emits the approved
        // maintenance subject.
        let tmp = tempfile::TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        fs::create_dir_all(&repo).unwrap();
        init_test_repo(&repo);

        // A fix-pre-merge change leaves the candidate worktree dirty.
        fs::write(repo.join("file.txt"), "reformatted").unwrap();
        commit_autofix(&repo).expect("commit_autofix persists the staged change");

        let persisted = git::run_stdout(&repo, &["log", "-1", "--format=%B"], "read message")
            .expect("read the persisted commit message");

        // Fidelity: the Git boundary holds exactly the production source, so
        // any persisted divergence fails.
        assert_eq!(
            persisted,
            autofix_commit_message(),
            "persisted message must equal the production message source"
        );

        // Policy (B2): the complete persisted message equals the approved
        // maintenance wording.
        assert_eq!(
            persisted, "Conform code to project standards",
            "persisted message must equal the approved wording"
        );

        // Shape (B1): nonempty, exactly one subject and no body.
        assert!(!persisted.is_empty(), "message must not be empty");
        assert!(
            !persisted.contains('\n'),
            "message must be a single subject line with no body: {persisted:?}"
        );
    }

    // --- Exact-SHA no-expertise land (B4u–B4z) ---

    fn write_hook(project_root: &Path, name: &str, script: &str) {
        use std::os::unix::fs::OpenOptionsExt;
        let hooks_dir = project_root.join(".fluent/hooks");
        fs::create_dir_all(&hooks_dir).unwrap();
        let mut opts = fs::OpenOptions::new();
        opts.create(true).write(true).truncate(true).mode(0o755);
        use std::io::Write;
        let mut file = opts.open(hooks_dir.join(name)).unwrap();
        file.write_all(script.as_bytes()).unwrap();
    }

    struct FrozenLandFixture {
        _tmp: tempfile::TempDir,
        /// The outer temporary root, a parent of both `project_root` and
        /// `source_workspace` and outside every artifact area and disposable
        /// worktree. Absolute test sentinels and event logs placed here survive
        /// disposable-worktree cleanup, so an erroneous hook invocation stays
        /// observable (B4an).
        outer_root: PathBuf,
        project_root: PathBuf,
        source_workspace: PathBuf,
        store: WorkModelStore,
        reviewed_sha: String,
    }

    /// Build a real git repository with a `main` target and a registered candidate
    /// worktree checked out at a frozen reviewed Writer SHA, then persist a
    /// no-expertise Work Item whose Attempt's Learner has SUCCEEDED at that SHA.
    fn frozen_land_fixture(learner_succeeded: bool) -> FrozenLandFixture {
        build_frozen_fixture(|item, _reviewed_sha| {
            if learner_succeeded {
                item.attempts[0].learning = Some(crate::work_model::AttemptLearning::succeeded(
                    1,
                    crate::follow_up::ArtifactRef {
                        path: ".fluent/work/artifacts/work-1/attempt-1/learner/handoff.json"
                            .to_string(),
                        digest: "sha256:frozen".to_string(),
                    },
                ));
            }
        })
    }

    /// Build the frozen-land repository, registered candidate worktree, and
    /// no-expertise Work Item, then apply `configure` to the item (with the reviewed
    /// SHA) immediately before it is created fresh. Creating the item fresh bypasses
    /// the frozen-identity write guard, so a cell may persist an already-Merged state
    /// with an absent or divergent merged_commit that the guard would otherwise
    /// reject on a write to an existing aggregate.
    fn build_frozen_fixture(configure: impl FnOnce(&mut WorkItem, &str)) -> FrozenLandFixture {
        let tmp = tempfile::TempDir::new().unwrap();
        let project_root = tmp.path().join("project");
        fs::create_dir_all(&project_root).unwrap();
        let git = |cwd: &Path, args: &[&str]| {
            git::run(cwd, args, "frozen land fixture setup").unwrap();
        };
        git(&project_root, &["init", "-q", "-b", "main"]);
        git(&project_root, &["config", "user.email", "t@t.co"]);
        git(&project_root, &["config", "user.name", "t"]);
        fs::write(project_root.join("file.txt"), "base").unwrap();
        git(&project_root, &["add", "."]);
        git(&project_root, &["commit", "-q", "-m", "baseline"]);

        // The candidate advances one reviewed commit past main on a branch. main
        // stays an ancestor of the reviewed SHA.
        git(&project_root, &["checkout", "-q", "-b", "work/attempt-1"]);
        fs::write(project_root.join("file.txt"), "reviewed change").unwrap();
        git(&project_root, &["add", "."]);
        git(&project_root, &["commit", "-q", "-m", "reviewed"]);
        let reviewed_sha =
            git::run_stdout(&project_root, &["rev-parse", "HEAD"], "reviewed sha").unwrap();
        git(&project_root, &["checkout", "-q", "main"]);

        // The candidate source workspace is a registered sibling worktree checked out
        // at the reviewed SHA, at the model-required managed path.
        let ws_path = crate::work_model::initial_candidate_workspace_path("work-1", "attempt-1");
        let source_workspace = project_root
            .parent()
            .unwrap()
            .join(Path::new(&ws_path).file_name().map(Path::new).unwrap());
        git(
            &project_root,
            &[
                "worktree",
                "add",
                "-q",
                "--detach",
                source_workspace.to_str().unwrap(),
                &reviewed_sha,
            ],
        );

        let store = WorkModelStore::new(project_root.as_path());
        let mut item = WorkItem {
            id: "work-1".to_string(),
            title: "Frozen no-expertise land".to_string(),
            learner_mode: crate::work_model::LearnerMode::NoExpertise,
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
            workspace_path: ws_path,
            source_branch: "main".to_string(),
            base_commit: None,
            commit: reviewed_sha.clone(),
        });
        item.create_or_get_merge_candidate("attempt-1").unwrap();
        configure(&mut item, &reviewed_sha);
        store.create_work_item(&item).unwrap();

        let outer_root = tmp.path().to_path_buf();
        FrozenLandFixture {
            _tmp: tmp,
            outer_root,
            project_root,
            source_workspace,
            store,
            reviewed_sha,
        }
    }

    fn frozen_config<'a>(
        fx: &'a FrozenLandFixture,
        resolver: &'a ContentResolver,
        run_post_merge_review: bool,
    ) -> WorkMergeConfig<'a> {
        WorkMergeConfig {
            project_root: fx.project_root.as_path(),
            store: &fx.store,
            work_item_id: "work-1",
            merge_candidate_id: "attempt-1-merge-candidate",
            resolver,
            extra_args: &[],
            coder_kind: CoderKind::Codex,
            coder_override: None,
            model: None,
            effort: None,
            use_attempt_mapping: false,
            no_sandbox: true,
            run_post_merge_review,
        }
    }

    fn target_head(fx: &FrozenLandFixture) -> String {
        git::run_stdout(&fx.project_root, &["rev-parse", "main"], "target head").unwrap()
    }

    /// A complete snapshot of a workspace's Git state, used to prove a rejected land
    /// mutates nothing. Retains HEAD, the raw staged index, porcelain status, the
    /// staged blob of every tracked path, and the working-tree bytes of every tracked
    /// path plus every untracked payload. `.git` and `.fluent` are excluded from the
    /// status and untracked bytes: the durable Work model and merge-artifact area are
    /// asserted separately, and the land lock, hooks, and store all live under
    /// `.fluent`, so including them would compare orchestration noise, not the checkout.
    #[derive(Debug, PartialEq, Eq)]
    struct GitSnapshot {
        head: String,
        index: String,
        status: String,
        staged_blobs: Vec<(String, Vec<u8>)>,
        worktree_bytes: Vec<(String, Vec<u8>)>,
    }

    fn is_orchestration_path(path: &str) -> bool {
        matches!(path.split('/').next(), Some(".git") | Some(".fluent"))
    }

    fn snapshot_git(workspace: &Path) -> GitSnapshot {
        let head = head_commit(workspace).unwrap();
        let index =
            git::run_stdout(workspace, &["ls-files", "--stage", "-z"], "snapshot index").unwrap();
        let raw_status = git::run_stdout(
            workspace,
            &["status", "--porcelain", "--untracked-files=all"],
            "snapshot status",
        )
        .unwrap();
        let status: String = raw_status
            .lines()
            .filter(|line| !is_orchestration_path(line.get(3..).unwrap_or("")))
            .map(|line| format!("{line}\n"))
            .collect();

        let tracked =
            git::run_stdout(workspace, &["ls-files", "-z"], "snapshot tracked paths").unwrap();
        let mut staged_blobs = Vec::new();
        let mut worktree_bytes = Vec::new();
        for path in tracked.split('\0').filter(|entry| !entry.is_empty()) {
            let blob = git::run_raw(workspace, &["show", &format!(":{path}")])
                .unwrap()
                .stdout;
            staged_blobs.push((path.to_string(), blob));
            if let Ok(bytes) = fs::read(workspace.join(path)) {
                worktree_bytes.push((path.to_string(), bytes));
            }
        }
        for line in raw_status.lines() {
            if let Some(path) = line.strip_prefix("?? ") {
                let path = path.trim();
                if is_orchestration_path(path) {
                    continue;
                }
                if let Ok(bytes) = fs::read(workspace.join(path)) {
                    worktree_bytes.push((path.to_string(), bytes));
                }
            }
        }
        staged_blobs.sort();
        worktree_bytes.sort();
        GitSnapshot {
            head,
            index,
            status,
            staged_blobs,
            worktree_bytes,
        }
    }

    /// The set of worktrees git currently has registered for a repository, one
    /// absolute path per registered worktree. Used to prove a rejected land leaks no
    /// disposable worktree beyond the one it is documented to retain (B4as).
    fn registered_worktrees(project_root: &Path) -> std::collections::BTreeSet<PathBuf> {
        git::run_stdout(
            project_root,
            &["worktree", "list", "--porcelain"],
            "worktree list",
        )
        .unwrap()
        .lines()
        .filter_map(|line| line.strip_prefix("worktree "))
        .map(|path| PathBuf::from(path.trim()))
        .collect()
    }

    #[test]
    fn no_expertise_land_preflight_reads_identity_before_side_effects() {
        // B4u: the preflight reads the live frozen identity and, when the candidate
        // is clean at the reviewed SHA with main an ancestor, lands the exact SHA
        // without ever marking executing before the read passes.
        let fx = frozen_land_fixture(true);
        let resolver = ContentResolver::new(None);
        let outcome = merge_candidate(frozen_config(&fx, &resolver, false)).unwrap();
        assert_eq!(outcome.merged_commit, fx.reviewed_sha);
        assert_eq!(
            target_head(&fx),
            fx.reviewed_sha,
            "the target fast-forwards to exactly the reviewed SHA"
        );
    }

    #[test]
    fn no_expertise_land_blocks_rebase_before_git_or_model_mutation() {
        // B4v: a mismatched live candidate (HEAD moved off the reviewed SHA) leaves
        // the candidate unstarted, both Git heads unchanged, reports that a fresh
        // Attempt is required, and never constructs a rebase coder.
        let fx = frozen_land_fixture(true);
        let target_before = target_head(&fx);

        // Advance the candidate HEAD off the reviewed SHA.
        git::run(
            &fx.source_workspace,
            &["commit", "-q", "--allow-empty", "-m", "drift"],
            "drift candidate",
        )
        .unwrap();
        let candidate_head_before = head_commit(&fx.source_workspace).unwrap();

        let resolver = ContentResolver::new(None);
        let error = merge_candidate(frozen_config(&fx, &resolver, false)).unwrap_err();
        assert!(
            error.to_string().contains("fresh Attempt"),
            "the diagnostic requires a fresh Attempt: {error}"
        );

        let stored = fx.store.read_work_item("work-1").unwrap();
        let candidate = &stored.merge_candidates[0];
        assert_eq!(
            candidate.merge_state.status,
            MergeCandidateMergeStatus::Pending,
            "a preflight failure leaves the candidate unstarted"
        );
        assert!(candidate.merge_state.merged_commit.is_none());
        assert_eq!(target_head(&fx), target_before, "target Git is unchanged");
        assert_eq!(
            head_commit(&fx.source_workspace).unwrap(),
            candidate_head_before,
            "source Git is unchanged"
        );
        // A rebase coder produces exactly one artifact: a reserved Rebase Task. Its
        // absence proves the frozen route bailed before any rebase-coder construction.
        assert!(
            !stored.attempts[0]
                .tasks
                .iter()
                .any(|task| task.kind == TaskKind::Rebase),
            "no Rebase Task was created, so no rebase coder was constructed"
        );
    }

    #[test]
    fn no_expertise_land_blocks_when_target_not_ancestor() {
        // B4v: a target head that is not an ancestor of the reviewed SHA blocks the
        // land with the fresh-Attempt diagnostic and no Git mutation.
        let fx = frozen_land_fixture(true);
        // Advance main to a commit unrelated to the reviewed SHA so main is no longer
        // an ancestor of it.
        git::run(
            &fx.project_root,
            &["commit", "-q", "--allow-empty", "-m", "advance main"],
            "advance main",
        )
        .unwrap();
        let target_before = target_head(&fx);

        let resolver = ContentResolver::new(None);
        let error = merge_candidate(frozen_config(&fx, &resolver, false)).unwrap_err();
        assert!(error.to_string().contains("fresh Attempt"));
        assert_eq!(target_head(&fx), target_before, "target Git is unchanged");
        let stored = fx.store.read_work_item("work-1").unwrap();
        assert_eq!(
            stored.merge_candidates[0].merge_state.status,
            MergeCandidateMergeStatus::Pending
        );
    }

    #[test]
    fn no_expertise_land_fast_forwards_exact_reviewed_sha_without_rebase() {
        // B4w: with a passing check-pre-merge run in a disposable exact-SHA worktree,
        // the target fast-forwards to the exact reviewed SHA and that SHA is persisted
        // as merged_commit — no rebase, no provenance regeneration, no autofix.
        let fx = frozen_land_fixture(true);
        write_hook(&fx.project_root, "check-pre-merge", "#!/bin/sh\nexit 0\n");

        let resolver = ContentResolver::new(None);
        let outcome = merge_candidate(frozen_config(&fx, &resolver, false)).unwrap();
        assert_eq!(outcome.merged_commit, fx.reviewed_sha);
        assert_eq!(target_head(&fx), fx.reviewed_sha);

        let stored = fx.store.read_work_item("work-1").unwrap();
        let candidate = &stored.merge_candidates[0];
        assert_eq!(
            candidate.merge_state.status,
            MergeCandidateMergeStatus::Merged
        );
        assert_eq!(
            candidate.merge_state.merged_commit.as_deref(),
            Some(fx.reviewed_sha.as_str())
        );
        // No rebase task was ever created.
        assert!(
            !stored.attempts[0]
                .tasks
                .iter()
                .any(|task| task.kind == TaskKind::Rebase),
            "the frozen route never creates a Rebase Task"
        );
        // The disposable worktree was removed.
        assert!(
            !fx.project_root
                .join(WORK_ARTIFACTS_DIR)
                .join("work-1/attempt-1/attempt-1-merge-candidate/merge/exact-sha-check")
                .exists(),
            "the disposable exact-SHA worktree is removed after a passing check"
        );
    }

    #[test]
    fn regenerate_provenance_rejects_frozen_no_expertise_identity() {
        // B4w: regenerate_provenance refuses to run for a frozen no-expertise
        // identity, since retargeting the reviewed SHA is exactly the pointer move
        // the freeze forbids.
        let fx = frozen_land_fixture(true);
        let error = regenerate_provenance(
            &fx.store,
            "work-1",
            "attempt-1-merge-candidate",
            "attempt-1",
            "new-base",
            "new-tip",
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("fresh Attempt")
                || error.to_string().contains("regenerate provenance"),
            "regenerate_provenance rejects the frozen identity: {error}"
        );
        // The reviewed SHA is untouched.
        let stored = fx.store.read_work_item("work-1").unwrap();
        assert_eq!(
            stored.attempts[0]
                .tasks
                .iter()
                .rev()
                .find(|t| t.kind == TaskKind::Write)
                .unwrap()
                .output
                .as_ref()
                .unwrap()
                .commit,
            fx.reviewed_sha
        );
    }

    #[test]
    fn no_expertise_land_never_runs_fix_pre_merge_or_changes_reviewed_sha() {
        // B4x: a failing check-pre-merge never invokes fix-pre-merge, fails the
        // candidate with a fresh-Attempt diagnostic, and leaves the live candidate and
        // target unchanged with no autofix commit.
        let fx = frozen_land_fixture(true);
        write_hook(&fx.project_root, "check-pre-merge", "#!/bin/sh\nexit 1\n");
        // A fix-pre-merge sentinel at an absolute path in the outer temporary root,
        // outside the project, source workspace, artifact area, and every disposable
        // worktree. If the hook ever runs, the marker survives disposable-worktree
        // cleanup, so an erroneous invocation stays observable and fails the test.
        let sentinel = fx.outer_root.join("fix-ran-marker");
        write_hook(
            &fx.project_root,
            "fix-pre-merge",
            &format!("#!/bin/sh\ntouch '{}'\nexit 0\n", sentinel.display()),
        );
        let target_before = target_head(&fx);

        let resolver = ContentResolver::new(None);
        let error = merge_candidate(frozen_config(&fx, &resolver, false)).unwrap_err();
        assert!(error.to_string().contains("fresh Attempt"), "{error}");

        assert_eq!(target_head(&fx), target_before, "target unchanged");
        assert_eq!(
            head_commit(&fx.source_workspace).unwrap(),
            fx.reviewed_sha,
            "live candidate HEAD unchanged"
        );
        assert!(
            !sentinel.exists(),
            "fix-pre-merge is never invoked on the frozen route, even after cleanup"
        );
        let stored = fx.store.read_work_item("work-1").unwrap();
        assert_eq!(
            stored.merge_candidates[0].merge_state.status,
            MergeCandidateMergeStatus::Failed,
            "a failed exact-SHA check fails the candidate"
        );
        assert!(
            stored.merge_candidates[0]
                .merge_state
                .merged_commit
                .is_none()
        );
    }

    #[test]
    fn no_expertise_mutating_pre_merge_check_cannot_change_live_candidate_or_target() {
        // B4x: a check-pre-merge that dirties/stages/commits in its cwd runs only in
        // the disposable worktree; the live candidate and target stay unchanged and
        // the candidate fails with a fresh-Attempt diagnostic.
        let fx = frozen_land_fixture(true);
        write_hook(
            &fx.project_root,
            "check-pre-merge",
            "#!/bin/sh\necho mutated > file.txt\ngit add -A\ngit commit -q -m sneaky\nexit 0\n",
        );
        let target_before = target_head(&fx);

        let resolver = ContentResolver::new(None);
        let error = merge_candidate(frozen_config(&fx, &resolver, false)).unwrap_err();
        assert!(error.to_string().contains("fresh Attempt"), "{error}");

        assert_eq!(target_head(&fx), target_before, "target unchanged");
        assert_eq!(
            head_commit(&fx.source_workspace).unwrap(),
            fx.reviewed_sha,
            "live candidate HEAD unchanged despite a committing check"
        );
        assert!(
            candidate_fully_clean(&fx.source_workspace).unwrap(),
            "live candidate stays clean"
        );
        let stored = fx.store.read_work_item("work-1").unwrap();
        assert_eq!(
            stored.merge_candidates[0].merge_state.status,
            MergeCandidateMergeStatus::Failed
        );
    }

    #[test]
    fn no_expertise_merged_resume_requires_frozen_reviewed_sha() {
        // B4y: an already-Merged no-expertise candidate resumes as an idempotent
        // success only when its persisted merged_commit equals the frozen reviewed
        // SHA; a divergent merged_commit fails closed. The cleaned-up candidate
        // workspace is not required.
        let fx = frozen_land_fixture(true);
        // Mark merged at the exact reviewed SHA and remove the candidate workspace.
        record_candidate_merged(
            &fx.store,
            "work-1",
            "attempt-1-merge-candidate",
            &fx.reviewed_sha,
            Vec::new(),
            Vec::new(),
        )
        .unwrap();
        git::run(
            &fx.project_root,
            &[
                "worktree",
                "remove",
                "--force",
                fx.source_workspace.to_str().unwrap(),
            ],
            "remove candidate workspace",
        )
        .unwrap();

        let resolver = ContentResolver::new(None);
        let outcome = merge_candidate(frozen_config(&fx, &resolver, false)).unwrap();
        assert_eq!(
            outcome.merged_commit, fx.reviewed_sha,
            "a matching merged_commit resumes as an idempotent success without the workspace"
        );
    }

    #[test]
    fn no_expertise_merged_resume_fails_closed_on_divergent_merged_commit() {
        // B4y: an already-Merged no-expertise candidate whose persisted merged_commit
        // is NOT the frozen reviewed SHA must fail closed rather than report success.
        // A fresh Work Item can persist such a divergent value (the frozen-identity
        // model guard fires only on writes to an existing item), so the land route
        // itself must catch it.
        let fx = frozen_land_fixture(true);
        let mut item = fx.store.read_work_item("work-1").unwrap();
        item.merge_candidates[0].merge_state.status = MergeCandidateMergeStatus::Merged;
        item.merge_candidates[0].merge_state.merged_commit =
            Some("deadbeefdeadbeefdeadbeefdeadbeefdeadbeef".to_string());
        // Persist through a fresh store so no prior aggregate triggers the guard.
        let fresh_dir = tempfile::TempDir::new().unwrap();
        let fresh_store = WorkModelStore::new(fresh_dir.path());
        fresh_store.create_work_item(&item).unwrap();

        let resolver = ContentResolver::new(None);
        let config = WorkMergeConfig {
            project_root: fresh_dir.path(),
            store: &fresh_store,
            work_item_id: "work-1",
            merge_candidate_id: "attempt-1-merge-candidate",
            resolver: &resolver,
            extra_args: &[],
            coder_kind: CoderKind::Codex,
            coder_override: None,
            model: None,
            effort: None,
            use_attempt_mapping: false,
            no_sandbox: true,
            run_post_merge_review: false,
        };
        let error = merge_candidate(config).unwrap_err();
        assert!(
            error.to_string().contains("fresh Attempt")
                && error.to_string().contains("frozen reviewed SHA"),
            "a divergent merged_commit fails closed with the fresh-Attempt diagnostic: {error}"
        );
    }

    #[test]
    fn capture_land_still_allows_fix_pre_merge() {
        // B4z: a capture-mode land keeps the existing check→fix→recheck path. A
        // capture Work Item is never gated into the frozen branch, so run_merge_checks
        // still runs fix-pre-merge after a failing check.
        let tmp = tempfile::TempDir::new().unwrap();
        let project_root = tmp.path().join("project");
        fs::create_dir_all(&project_root).unwrap();
        init_test_repo(&project_root);
        // A check that fails until the fix marker exists, then passes.
        write_hook(
            &project_root,
            "check-pre-merge",
            "#!/bin/sh\ntest -f conformed.txt\n",
        );
        write_hook(
            &project_root,
            "fix-pre-merge",
            "#!/bin/sh\necho ok > conformed.txt\nexit 0\n",
        );

        let store = WorkModelStore::new(project_root.as_path());
        let (item, candidate) = executing_candidate_item("work-1", &project_root);
        // Capture mode: the frozen branch must not engage.
        assert!(
            frozen_no_expertise_reviewed_sha(&item, &candidate).is_none(),
            "capture-mode Work never resolves a frozen reviewed SHA"
        );

        let artifact_dir = tmp.path().join("artifacts");
        fs::create_dir_all(&artifact_dir).unwrap();
        let resolver = ContentResolver::new(None);
        let config = WorkMergeConfig {
            project_root: project_root.as_path(),
            store: &store,
            work_item_id: "work-1",
            merge_candidate_id: "attempt-1-merge-candidate",
            resolver: &resolver,
            extra_args: &[],
            coder_kind: CoderKind::Codex,
            coder_override: None,
            model: None,
            effort: None,
            use_attempt_mapping: false,
            no_sandbox: true,
            run_post_merge_review: false,
        };
        let artifacts =
            run_merge_checks(&config, &candidate, &project_root, &artifact_dir).unwrap();
        assert!(
            project_root.join("conformed.txt").exists(),
            "capture-mode land still runs fix-pre-merge to conform the tree"
        );
        assert!(
            artifacts.len() >= 3,
            "check, fix, and recheck artifacts are all recorded: {}",
            artifacts.len()
        );
    }

    #[test]
    fn no_expertise_passing_check_cleanup_failure_blocks_land() {
        // B4as: a passing check-pre-merge whose disposable exact-SHA worktree cannot be
        // removed fails the land before the second live precondition check or any target
        // Git mutation. It persists EXACTLY the returned diagnostic as the failure
        // reason, leaves the candidate exactly Failed with no merged commit or follow-up
        // result, writes no real post-merge-review queue entry, preserves the complete
        // source and target Git/index/status/byte state, and leaves the registered
        // worktree set equal to the pre-call set plus exactly the one retained disposable
        // worktree — no unspecified subset and no extra leak.
        let fx = frozen_land_fixture(true);
        write_hook(&fx.project_root, "check-pre-merge", "#!/bin/sh\nexit 0\n");
        let src = fx.source_workspace.as_path();
        let target = fx.project_root.as_path();

        // Snapshot both complete checkouts and the exact registered-worktree set before.
        let source_before = snapshot_git(src);
        let target_before = snapshot_git(target);
        let registered_before = registered_worktrees(target);

        let resolver = ContentResolver::new(None);
        let error = {
            let _fault = DisposableRemovalFaultGuard::engage();
            merge_candidate(frozen_config(&fx, &resolver, true)).unwrap_err()
        };
        let returned = error.to_string();
        assert!(
            returned.contains("disposable exact-SHA worktree") && returned.contains("failed"),
            "the diagnostic reports the cleanup failure: {returned}"
        );
        assert!(
            returned.contains("fresh Attempt"),
            "the diagnostic requires a fresh Attempt: {returned}"
        );

        // Both complete checkouts survive byte-for-byte: no target fast-forward, no live
        // candidate mutation, no index/status/byte drift on either side.
        assert_eq!(
            snapshot_git(src),
            source_before,
            "the complete source checkout is unchanged"
        );
        assert_eq!(
            snapshot_git(target),
            target_before,
            "the complete target checkout is unchanged"
        );

        let stored = fx.store.read_work_item("work-1").unwrap();
        let candidate = &stored.merge_candidates[0];
        assert_eq!(
            candidate.merge_state.status,
            MergeCandidateMergeStatus::Failed,
            "a passing-check cleanup failure fails the candidate exactly"
        );
        // The persisted failure reason equals the returned diagnostic EXACTLY.
        let reason = candidate
            .merge_state
            .failure_reason
            .as_deref()
            .expect("a failed candidate retains its failure reason");
        assert_eq!(
            reason, returned,
            "the persisted failure reason equals the returned diagnostic exactly"
        );
        assert!(
            candidate.merge_state.merged_commit.is_none(),
            "a blocked land records no merged commit"
        );
        assert!(
            candidate.merge_state.follow_up_failure.is_none(),
            "a land blocked before recovery records no follow-up result"
        );
        // No post-merge review is scheduled: the land failed before recovery, so the
        // real queue-persistence boundary is never reached.
        let queue = crate::post_merge_review::load_queue(&fx.project_root).unwrap();
        assert!(
            queue.entries.is_empty(),
            "a blocked land writes no post-merge-review queue entry"
        );

        // The registered-worktree delta is exactly the one retained disposable worktree:
        // no pre-existing worktree is removed and no additional worktree leaks.
        let registered_after = registered_worktrees(target);
        assert!(
            registered_before
                .difference(&registered_after)
                .next()
                .is_none(),
            "no pre-existing worktree is removed"
        );
        let added: Vec<PathBuf> = registered_after
            .difference(&registered_before)
            .cloned()
            .collect();
        assert_eq!(
            added.len(),
            1,
            "exactly one disposable worktree is retained; none other leaks: {added:?}"
        );
        let expected_disposable = fx
            .project_root
            .join(WORK_ARTIFACTS_DIR)
            .join("work-1/attempt-1/attempt-1-merge-candidate/merge/exact-sha-check");
        assert!(
            expected_disposable.exists(),
            "the isolated disposable worktree is retained for cleanup"
        );
        assert_eq!(
            added[0].canonicalize().unwrap(),
            expected_disposable.canonicalize().unwrap(),
            "the one retained worktree is exactly the isolated exact-SHA disposable worktree"
        );
    }

    #[test]
    fn no_expertise_land_records_follow_up_outcome_before_post_merge_review() {
        // B4ak: the public exact-SHA land route completes the real Work-model write of
        // the incomplete follow-up result BEFORE it enters the real queue-persistence
        // leaf. A scoped observer at that leaf confirms the durable follow-up failure is
        // already stored at entry, then the real queue write runs and persists the
        // complete entry. The succeeded Learner's handoff artifact is deliberately
        // absent, so the real follow-up materialization boundary fails and records a
        // durable incomplete result — leaving the queue empty would mean the exact-SHA
        // route bypassed the shared coordinator.
        let fx = frozen_land_fixture(true);
        write_hook(&fx.project_root, "check-pre-merge", "#!/bin/sh\nexit 0\n");
        // Pin the corrective fix depth at the cap so the detached-runner spawn is
        // suppressed while the real queue persistence still runs.
        let cap = crate::post_merge_review::max_post_merge_review_fix_depth();
        fx.store
            .mutate_work_item("work-1", |item| {
                item.post_merge_review_fix_depth = Some(cap);
                Ok(())
            })
            .unwrap();
        let base_before = target_head(&fx);

        // Capture, at entry to the real queue-persistence leaf, both the complete
        // queue entry and the candidate as durably stored at that instant.
        let observed_candidate: std::rc::Rc<
            std::cell::RefCell<Option<crate::work_model::MergeCandidate>>,
        > = std::rc::Rc::default();
        let observed_entry: std::rc::Rc<
            std::cell::RefCell<Option<crate::post_merge_review::QueueEntry>>,
        > = std::rc::Rc::default();
        let store_at_queue = fx.store.clone();
        let observed_candidate_w = observed_candidate.clone();
        let observed_entry_w = observed_entry.clone();

        let resolver = ContentResolver::new(None);
        let outcome = {
            let _observer = crate::post_merge_review::observe_queue_append(move |entry| {
                let item = store_at_queue.read_work_item("work-1").unwrap();
                *observed_candidate_w.borrow_mut() = Some(item.merge_candidates[0].clone());
                *observed_entry_w.borrow_mut() = Some(entry.clone());
            });
            merge_candidate(frozen_config(&fx, &resolver, true)).unwrap()
        };
        assert_eq!(outcome.merged_commit, fx.reviewed_sha);

        // At entry to the real queue write, the durable candidate already carried the
        // exact incomplete follow-up failure, proving the Work-model write preceded the
        // queue-persistence leaf.
        let at_queue = observed_candidate
            .borrow()
            .clone()
            .expect("the observer saw the real queue-persistence leaf");
        assert_eq!(
            at_queue.merge_state.status,
            MergeCandidateMergeStatus::Merged,
            "the candidate is durably merged before the queue write"
        );
        let durable_failure = at_queue
            .merge_state
            .follow_up_failure
            .clone()
            .expect("the durable follow-up failure is stored before the queue write");

        // The stored aggregate matches what the observer saw.
        let stored = fx.store.read_work_item("work-1").unwrap();
        assert_eq!(
            stored.merge_candidates[0]
                .merge_state
                .follow_up_failure
                .as_ref(),
            Some(&durable_failure),
            "the durable follow-up failure observed at the queue leaf is the persisted one"
        );

        // The real queue leaf then wrote exactly one complete entry.
        let queue = crate::post_merge_review::load_queue(&fx.project_root).unwrap();
        assert_eq!(
            queue.entries.len(),
            1,
            "a real post-merge-review queue entry is persisted through the shared coordinator"
        );
        let entry = &queue.entries[0];
        let expected = crate::post_merge_review::QueueEntry {
            target_branch: stored.merge_candidates[0].target_branch.clone(),
            merged_commit: fx.reviewed_sha.clone(),
            // Only the runtime timestamp is captured from the entry and compared
            // consistently; every other field is independently reconstructed.
            merged_at_unix: entry.merged_at_unix,
            source_work_item_id: "work-1".to_string(),
            source_merge_candidate_id: "attempt-1-merge-candidate".to_string(),
            base_commit: base_before.clone(),
            fix_depth: cap,
        };
        assert_eq!(
            *entry, expected,
            "the persisted queue entry carries the complete land provenance"
        );
        // The entry the observer saw at the leaf equals the entry that was persisted.
        assert_eq!(
            observed_entry.borrow().clone(),
            Some(entry.clone()),
            "the observed entry equals the persisted entry"
        );
    }

    #[test]
    fn no_expertise_land_skips_post_merge_review_when_follow_up_persistence_is_unknown() {
        // B4ak: when the real Work-model storage write for the follow-up result fails
        // so the result is unknown, the public exact-SHA land returns its already-durable
        // landed outcome unchanged, keeps the candidate Merged, persists no follow-up
        // result, and schedules no post-merge review (no queue entry). The failure
        // originates inside the real Work-model write path — validation and storage are
        // entered and only the atomic write fails — and retains its typed storage cause.
        let fx = frozen_land_fixture(true);
        write_hook(&fx.project_root, "check-pre-merge", "#!/bin/sh\nexit 0\n");

        LAST_FOLLOW_UP_PERSIST_ERROR.with(|slot| *slot.borrow_mut() = None);
        let resolver = ContentResolver::new(None);
        let outcome = {
            // Fault only the one follow-up-result write for this Work Item, at the real
            // WorkModelStore persistence leaf. Every other write in the land commits.
            let _fault = crate::work_model::persist_fault::arm_follow_up_write("work-1");
            merge_candidate(frozen_config(&fx, &resolver, true)).unwrap()
        };
        // The land already became durable: the exact landed outcome is returned.
        assert_eq!(outcome.merged_commit, fx.reviewed_sha);
        let stored = fx.store.read_work_item("work-1").unwrap();
        assert_eq!(
            stored.merge_candidates[0].merge_state.status,
            MergeCandidateMergeStatus::Merged,
            "the land stays successful even when follow-up persistence is unknown"
        );
        assert_eq!(
            stored.merge_candidates[0]
                .merge_state
                .merged_commit
                .as_deref(),
            Some(fx.reviewed_sha.as_str()),
            "the durable merged commit is exactly the reviewed SHA"
        );
        // No speculative follow-up result became durable: the faulted write never
        // persisted, so the candidate carries no follow-up failure.
        assert!(
            stored.merge_candidates[0]
                .merge_state
                .follow_up_failure
                .is_none(),
            "an unknown follow-up persistence records no durable follow-up result"
        );

        // The failure came from inside the real Work-model write path and kept its
        // typed WorkModelStorageError cause — not an ad-hoc pre-write short circuit.
        let observed = LAST_FOLLOW_UP_PERSIST_ERROR
            .with(|slot| slot.borrow().clone())
            .expect("the coordinator surfaced the follow-up persistence failure");
        assert!(
            observed.has_typed_storage_cause,
            "the surfaced failure retains its typed WorkModelStorageError cause: {}",
            observed.rendered
        );
        assert!(
            observed
                .rendered
                .contains("injected atomic Work-model storage fault"),
            "the failure originated at the real atomic storage write: {}",
            observed.rendered
        );

        // Persistence unknown suppresses the optional post-merge review: no queue entry.
        let queue = crate::post_merge_review::load_queue(&fx.project_root).unwrap();
        assert!(
            queue.entries.is_empty(),
            "unknown follow-up persistence writes no post-merge-review queue entry"
        );
    }

    #[test]
    fn no_expertise_merged_resume_requires_reviewed_sha_for_every_learning_state() {
        // B4aj: an already-Merged no-expertise candidate derives its reviewed Writer
        // SHA from the latest completed Write output independently of Learning, before
        // resolving any workspace. The full five-by-three matrix
        // ({None, InProgress, HandoffPending, Failed, Succeeded} ×
        // {missing, divergent, matching}) is driven through public `merge_candidate`
        // with the source workspace removed before every cell. A missing or divergent
        // merged_commit fails closed with the complete aggregate and external target
        // Git unchanged and no merge artifact area created; a matching one resumes
        // idempotently without the workspace.
        let handoff = crate::follow_up::ArtifactRef {
            path: ".fluent/work/artifacts/work-1/attempt-1/learner/handoff.json".to_string(),
            digest: "sha256:frozen".to_string(),
        };
        let learning_states: Vec<Option<crate::work_model::AttemptLearning>> = vec![
            None,
            Some(crate::work_model::AttemptLearning::in_progress(1)),
            Some(crate::work_model::AttemptLearning::handoff_pending(1)),
            Some(crate::work_model::AttemptLearning::failed(1, "retry me")),
            Some(crate::work_model::AttemptLearning::succeeded(
                1,
                handoff.clone(),
            )),
        ];

        #[derive(Clone, Copy)]
        enum MergedColumn {
            Missing,
            Divergent,
            Matching,
        }

        for learning in &learning_states {
            for column in [
                MergedColumn::Missing,
                MergedColumn::Divergent,
                MergedColumn::Matching,
            ] {
                let learning = learning.clone();
                let fx = build_frozen_fixture(|item, reviewed_sha| {
                    item.attempts[0].learning = learning.clone();
                    item.merge_candidates[0].merge_state.status = MergeCandidateMergeStatus::Merged;
                    item.merge_candidates[0].merge_state.merged_commit = match column {
                        MergedColumn::Missing => None,
                        MergedColumn::Divergent => {
                            Some("deadbeefdeadbeefdeadbeefdeadbeefdeadbeef".to_string())
                        }
                        MergedColumn::Matching => Some(reviewed_sha.to_string()),
                    };
                });

                // Remove the source workspace before every cell: a successful matching
                // resume then proves workspace-independence, and a rejected cell proves
                // the reject decision precedes any workspace resolution.
                git::run(
                    &fx.project_root,
                    &[
                        "worktree",
                        "remove",
                        "--force",
                        fx.source_workspace.to_str().unwrap(),
                    ],
                    "remove candidate workspace",
                )
                .unwrap();
                assert!(
                    !fx.source_workspace.exists(),
                    "the candidate workspace is absent before every cell"
                );

                let before = fx.store.read_work_item("work-1").unwrap();
                let target_before = target_head(&fx);
                let merge_artifact_area = fx
                    .project_root
                    .join(WORK_ARTIFACTS_DIR)
                    .join("work-1/attempt-1/attempt-1-merge-candidate/merge");
                let resolver = ContentResolver::new(None);

                match column {
                    MergedColumn::Missing | MergedColumn::Divergent => {
                        let error =
                            merge_candidate(frozen_config(&fx, &resolver, false)).unwrap_err();
                        assert!(
                            error.to_string().contains("fresh Attempt")
                                && error.to_string().contains("frozen reviewed SHA"),
                            "a missing or divergent merged_commit fails closed for every \
                             Learning state: {error}"
                        );
                        // Fail-closed before any effect: the whole persisted aggregate and
                        // the external target Git are preserved exactly, and no merge
                        // artifact area was created.
                        let after = fx.store.read_work_item("work-1").unwrap();
                        assert_eq!(
                            after, before,
                            "a rejected already-Merged cell changes no persisted state"
                        );
                        assert_eq!(
                            target_head(&fx),
                            target_before,
                            "a rejected already-Merged cell leaves the target Git unchanged"
                        );
                        assert!(
                            !merge_artifact_area.exists(),
                            "a rejected already-Merged cell creates no merge artifact area"
                        );
                    }
                    MergedColumn::Matching => {
                        let outcome =
                            merge_candidate(frozen_config(&fx, &resolver, false)).unwrap();
                        assert_eq!(
                            outcome.merged_commit, fx.reviewed_sha,
                            "a matching merged_commit resumes idempotently for every \
                             Learning state"
                        );
                        let recovered = fx.store.read_work_item("work-1").unwrap();
                        assert_eq!(
                            recovered.merge_candidates[0].merge_state.status,
                            MergeCandidateMergeStatus::Merged,
                            "a matching recovery stays merged"
                        );
                        assert_eq!(
                            recovered.merge_candidates[0]
                                .merge_state
                                .merged_commit
                                .as_deref(),
                            Some(fx.reviewed_sha.as_str()),
                            "a matching recovery keeps the reviewed SHA as the merged commit"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn no_expertise_dirty_candidate_fails_before_model_or_artifact_mutation() {
        // B4am: a dirty live no-expertise candidate fails at preflight, before any
        // Work-model, artifact, source Git, target Git, index, worktree, or byte
        // mutation. The complete Work Item aggregate and BOTH complete source/target
        // checkouts — HEAD, raw index, porcelain status, staged and unstaged
        // representations, and tracked/untracked payload bytes — are preserved
        // byte-for-byte, and no merge-artifact area is created.
        let fx = frozen_land_fixture(true);
        let src = fx.source_workspace.as_path();
        let target = fx.project_root.as_path();
        // Stage one version of a tracked file, overwrite it with different unstaged
        // bytes, and add an untracked binary payload.
        fs::write(src.join("file.txt"), "staged version\n").unwrap();
        git::run(src, &["add", "file.txt"], "stage tracked file").unwrap();
        fs::write(src.join("file.txt"), "unstaged version\n").unwrap();
        let payload: &[u8] = &[0u8, 1, 2, 3, 255, 254, 0, 42];
        fs::write(src.join("payload.bin"), payload).unwrap();

        // Snapshot the complete Work Item and both complete checkouts before the land.
        let item_before = fx.store.read_work_item("work-1").unwrap();
        let source_before = snapshot_git(src);
        let target_before = snapshot_git(target);

        let resolver = ContentResolver::new(None);
        let error = merge_candidate(frozen_config(&fx, &resolver, false)).unwrap_err();
        assert!(
            error.to_string().contains("fresh Attempt"),
            "a dirty candidate requires a fresh Attempt: {error}"
        );

        // The whole Work Item aggregate is unchanged: no field, candidate state, or
        // executing mark moved.
        let item_after = fx.store.read_work_item("work-1").unwrap();
        assert_eq!(
            item_after, item_before,
            "a dirty-source rejection changes no persisted Work Item state"
        );
        // Both complete checkouts survive the rejected preflight byte-for-byte.
        assert_eq!(
            snapshot_git(src),
            source_before,
            "the complete source checkout is unchanged"
        );
        assert_eq!(
            snapshot_git(target),
            target_before,
            "the complete target checkout is unchanged"
        );
        // No merge-artifact area was created.
        assert!(
            !target
                .join(WORK_ARTIFACTS_DIR)
                .join("work-1/attempt-1/attempt-1-merge-candidate/merge")
                .exists(),
            "a preflight failure creates no merge artifacts"
        );
    }

    #[test]
    fn no_expertise_pre_merge_check_mutation_matrix_preserves_live_git() {
        // B4ad: dirty-only, staged-only, and committed changes made by
        // check-pre-merge in the disposable worktree each fail the land without
        // invoking fix-pre-merge, and each preserves the live candidate and target.
        let cases: [(&str, &str); 3] = [
            (
                "dirty-only",
                "#!/bin/sh\necho mutated > extra.txt\nexit 0\n",
            ),
            (
                "staged-only",
                "#!/bin/sh\necho mutated > extra.txt\ngit add -A\nexit 0\n",
            ),
            (
                "committed",
                "#!/bin/sh\necho mutated > file.txt\ngit add -A\ngit commit -q -m sneaky\nexit 0\n",
            ),
        ];
        for (label, check) in cases {
            let fx = frozen_land_fixture(true);
            write_hook(&fx.project_root, "check-pre-merge", check);
            // A fix-pre-merge sentinel at an absolute path in the outer temporary root,
            // embedded shell-safe directly in the hook rather than through an
            // environment variable the hook runner never defines. It lives outside the
            // project, source workspace, artifact area, and every disposable worktree,
            // so an erroneous invocation survives cleanup and remains observable.
            let sentinel = fx.outer_root.join("fix-ran-marker");
            write_hook(
                &fx.project_root,
                "fix-pre-merge",
                &format!("#!/bin/sh\ntouch '{}'\nexit 0\n", sentinel.display()),
            );
            let target_before = target_head(&fx);

            let resolver = ContentResolver::new(None);
            let error = merge_candidate(frozen_config(&fx, &resolver, false)).unwrap_err();
            assert!(
                error.to_string().contains("fresh Attempt"),
                "[{label}] a mutating check requires a fresh Attempt: {error}"
            );
            assert!(
                !sentinel.exists(),
                "[{label}] fix-pre-merge is never invoked on the frozen route, even after cleanup"
            );
            assert_eq!(
                target_head(&fx),
                target_before,
                "[{label}] target Git is unchanged"
            );
            assert_eq!(
                head_commit(&fx.source_workspace).unwrap(),
                fx.reviewed_sha,
                "[{label}] the live candidate HEAD is unchanged"
            );
            assert!(
                candidate_fully_clean(&fx.source_workspace).unwrap(),
                "[{label}] the live candidate stays clean"
            );
            let stored = fx.store.read_work_item("work-1").unwrap();
            assert_eq!(
                stored.merge_candidates[0].merge_state.status,
                MergeCandidateMergeStatus::Failed,
                "[{label}] a mutating check fails the candidate"
            );
        }
    }

    #[test]
    fn no_expertise_land_rechecks_live_source_and_target_after_isolated_check() {
        // B4ad: live source drift and live target drift during the isolated check are
        // each rejected by the second precondition check before the fast-forward.

        // Source drift: the check advances the live candidate HEAD off the reviewed
        // SHA while running in its disposable worktree.
        let fx = frozen_land_fixture(true);
        write_hook(
            &fx.project_root,
            "check-pre-merge",
            &format!(
                "#!/bin/sh\ngit -C '{}' commit -q --allow-empty -m drift\nexit 0\n",
                fx.source_workspace.display()
            ),
        );
        let target_before = target_head(&fx);
        let resolver = ContentResolver::new(None);
        let error = merge_candidate(frozen_config(&fx, &resolver, false)).unwrap_err();
        assert!(
            error.to_string().contains("fresh Attempt"),
            "live source drift requires a fresh Attempt: {error}"
        );
        assert_eq!(
            target_head(&fx),
            target_before,
            "the target never fast-forwards after live source drift"
        );
        let stored = fx.store.read_work_item("work-1").unwrap();
        assert!(
            stored.merge_candidates[0]
                .merge_state
                .merged_commit
                .is_none()
        );
        assert_eq!(
            stored.merge_candidates[0].merge_state.status,
            MergeCandidateMergeStatus::Failed
        );

        // Target drift: the check advances the live target branch while running in its
        // disposable worktree.
        let fx = frozen_land_fixture(true);
        write_hook(
            &fx.project_root,
            "check-pre-merge",
            &format!(
                "#!/bin/sh\ngit -C '{}' commit -q --allow-empty -m target-drift\nexit 0\n",
                fx.project_root.display()
            ),
        );
        let resolver = ContentResolver::new(None);
        let error = merge_candidate(frozen_config(&fx, &resolver, false)).unwrap_err();
        assert!(
            error.to_string().contains("fresh Attempt"),
            "live target drift requires a fresh Attempt: {error}"
        );
        assert_ne!(
            target_head(&fx),
            fx.reviewed_sha,
            "the target never fast-forwards to the reviewed SHA after target drift"
        );
        let stored = fx.store.read_work_item("work-1").unwrap();
        assert!(
            stored.merge_candidates[0]
                .merge_state
                .merged_commit
                .is_none()
        );
        assert_eq!(
            head_commit(&fx.source_workspace).unwrap(),
            fx.reviewed_sha,
            "the live candidate is unchanged after target drift"
        );
    }

    #[test]
    fn no_expertise_dirty_target_fails_before_side_effects() {
        // B4ai: an initially dirty target fails preflight before the candidate is
        // marked executing or either live repository is changed, preserving the dirty
        // target as found and leaving the live candidate unchanged.
        let fx = frozen_land_fixture(true);
        fs::write(fx.project_root.join("file.txt"), "dirty target").unwrap();
        let target_before = target_head(&fx);

        let resolver = ContentResolver::new(None);
        let error = merge_candidate(frozen_config(&fx, &resolver, false)).unwrap_err();
        assert!(
            error.to_string().contains("uncommitted changes"),
            "a dirty target fails the existing cleanliness policy: {error}"
        );

        let stored = fx.store.read_work_item("work-1").unwrap();
        assert_eq!(
            stored.merge_candidates[0].merge_state.status,
            MergeCandidateMergeStatus::Pending,
            "a dirty target never marks the candidate executing"
        );
        assert_eq!(target_head(&fx), target_before, "target head is unchanged");
        assert_eq!(
            fs::read_to_string(fx.project_root.join("file.txt")).unwrap(),
            "dirty target",
            "the dirty target is preserved as found"
        );
        assert_eq!(
            head_commit(&fx.source_workspace).unwrap(),
            fx.reviewed_sha,
            "the live candidate is unchanged"
        );
        assert!(
            !fx.project_root
                .join(WORK_ARTIFACTS_DIR)
                .join("work-1/attempt-1/attempt-1-merge-candidate/merge")
                .exists(),
            "a preflight failure creates no merge artifacts"
        );
    }

    #[test]
    fn no_expertise_land_rechecks_target_cleanliness_after_isolated_check() {
        // B4ai: a target dirtied during the isolated check fails before the
        // fast-forward, preserving the dirty target and leaving the live candidate
        // unchanged.
        let fx = frozen_land_fixture(true);
        write_hook(
            &fx.project_root,
            "check-pre-merge",
            &format!(
                "#!/bin/sh\necho 'dirtied during check' > '{}/file.txt'\nexit 0\n",
                fx.project_root.display()
            ),
        );
        let target_before = target_head(&fx);

        let resolver = ContentResolver::new(None);
        let error = merge_candidate(frozen_config(&fx, &resolver, false)).unwrap_err();
        assert!(
            error.to_string().contains("uncommitted changes"),
            "a target dirtied during the check fails the cleanliness recheck: {error}"
        );

        assert_eq!(
            target_head(&fx),
            target_before,
            "the target never fast-forwards after being dirtied during the check"
        );
        assert_eq!(
            fs::read_to_string(fx.project_root.join("file.txt")).unwrap(),
            "dirtied during check\n",
            "the dirty target is preserved as found"
        );
        assert_eq!(
            head_commit(&fx.source_workspace).unwrap(),
            fx.reviewed_sha,
            "the live candidate is unchanged"
        );
        let stored = fx.store.read_work_item("work-1").unwrap();
        assert!(
            stored.merge_candidates[0]
                .merge_state
                .merged_commit
                .is_none()
        );
        assert_eq!(
            stored.merge_candidates[0].merge_state.status,
            MergeCandidateMergeStatus::Failed,
            "the already-started candidate fails when the target is dirtied mid-check"
        );
    }

    /// A rebase coder that performs the real `git rebase main` in the working tree and
    /// returns a clean supervision report, so the full capture rebase → provenance →
    /// check/fix/recheck route can be driven through the shared execution path.
    struct RealRebaseCoder;

    impl crate::coder::Coder for RealRebaseCoder {
        fn run(
            &self,
            _prompt: &str,
            _system_prompt: &str,
            _working_dir: &Path,
            _extra_args: &[String],
            _extra_env: &[(String, String)],
            _transcript_file: Option<&Path>,
        ) -> Result<i32> {
            unreachable!("the rebase route launches through run_captured_reported")
        }

        fn run_captured_reported(
            &self,
            _prompt: &str,
            _system_prompt: &str,
            working_dir: &Path,
            _extra_args: &[String],
            _extra_env: &[(String, String)],
            _capture: Option<&crate::coder::TranscriptCapture<'_>>,
        ) -> crate::coder::CoderRunCompletion {
            let terminal =
                match git::run(working_dir, &["rebase", "main"], "capture rebase onto main") {
                    Ok(()) => Ok(0),
                    Err(error) => Err(anyhow::anyhow!("{error}")),
                };
            crate::coder::CoderRunCompletion {
                terminal,
                report: crate::coder::CoderSupervisionReport::default(),
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

    #[test]
    fn public_land_route_threads_resolved_model_and_effort_to_rebase_coder() {
        // B4al: a capture-mode land driven through public `merge_candidate` still
        // reaches the rebase and provenance regeneration steps and still runs
        // check → fix → recheck. The capture route is never gated into the frozen
        // exact-SHA branch, and the ordered hook invocations are observable in an
        // absolute external event record. The in-process rebase coder is injected only
        // through a `#[cfg(test)]` seam; route selection, validation, artifact
        // creation, provenance regeneration, and final coordination all stay real.
        let tmp = tempfile::TempDir::new().unwrap();
        let project_root = tmp.path().join("project");
        fs::create_dir_all(&project_root).unwrap();
        let git = |cwd: &Path, args: &[&str]| {
            git::run(cwd, args, "capture land fixture setup").unwrap();
        };
        git(&project_root, &["init", "-q", "-b", "main"]);
        git(&project_root, &["config", "user.email", "t@t.co"]);
        git(&project_root, &["config", "user.name", "t"]);
        fs::write(project_root.join("file.txt"), "base").unwrap();
        git(&project_root, &["add", "."]);
        git(&project_root, &["commit", "-q", "-m", "baseline"]);

        // The candidate branches from the baseline and adds one commit.
        git(&project_root, &["checkout", "-q", "-b", "work/attempt-1"]);
        fs::write(project_root.join("candidate.txt"), "candidate work").unwrap();
        git(&project_root, &["add", "."]);
        git(&project_root, &["commit", "-q", "-m", "candidate work"]);
        let candidate_commit =
            git::run_stdout(&project_root, &["rev-parse", "HEAD"], "candidate sha").unwrap();
        git(&project_root, &["checkout", "-q", "main"]);
        // main advances beyond the candidate's base, so a rebase is required.
        fs::write(project_root.join("main-advance.txt"), "main moved").unwrap();
        git(&project_root, &["add", "."]);
        git(&project_root, &["commit", "-q", "-m", "advance main"]);

        // The candidate source workspace is a registered worktree on its branch.
        let ws_path = crate::work_model::initial_candidate_workspace_path("work-1", "attempt-1");
        let source_workspace = project_root
            .parent()
            .unwrap()
            .join(Path::new(&ws_path).file_name().map(Path::new).unwrap());
        git(
            &project_root,
            &[
                "worktree",
                "add",
                "-q",
                source_workspace.to_str().unwrap(),
                "work/attempt-1",
            ],
        );

        // Each hook appends its name to an absolute external event log in the outer
        // temporary root, outside the project, source workspace, and artifact area, so
        // the ordered check → fix → recheck sequence is observable after the land.
        let event_log = tmp.path().join("hook-events.log");
        let event_log_arg = event_log.display().to_string();
        // A check that fails until fix-pre-merge conforms the tree.
        write_hook(
            &project_root,
            "check-pre-merge",
            &format!("#!/bin/sh\necho check >> '{event_log_arg}'\ntest -f conformed.txt\n"),
        );
        write_hook(
            &project_root,
            "fix-pre-merge",
            &format!(
                "#!/bin/sh\necho fix >> '{event_log_arg}'\necho ok > conformed.txt\n\
                 git add -A\ngit commit -q -m conform\nexit 0\n"
            ),
        );

        let store = WorkModelStore::new(project_root.as_path());
        let mut item = WorkItem {
            id: "work-1".to_string(),
            title: "Capture land".to_string(),
            // Capture is the default learner mode.
            ..Default::default()
        };
        item.add_initial_attempt("attempt-1").unwrap();
        let attempt = item.attempts.first_mut().unwrap();
        attempt.status = AttemptStatus::Complete;
        attempt.review_state = Some(AttemptReviewState::Passed);
        // A succeeded Learner lets the candidate advance; capture mode keeps it on the
        // rebase route because it never resolves a frozen no-expertise reviewed SHA.
        attempt.learning = Some(crate::work_model::AttemptLearning::succeeded(
            1,
            crate::follow_up::ArtifactRef {
                path: ".fluent/work/artifacts/work-1/attempt-1/learner/handoff.json".to_string(),
                digest: "sha256:capture".to_string(),
            },
        ));
        let task = attempt.tasks.first_mut().unwrap();
        let workspace_id = task.workspace_access.writes.first().unwrap().id.clone();
        task.status = TaskStatus::Complete;
        task.output = Some(TaskOutput {
            workspace_id,
            workspace_path: ws_path,
            source_branch: "main".to_string(),
            base_commit: None,
            commit: candidate_commit.clone(),
        });
        item.create_or_get_merge_candidate("attempt-1").unwrap();
        item.attempts[0].coder_mapping.write = crate::work_model::CoderModelPair {
            coder: CoderKind::Codex,
            model: "gpt-5.6-terra".to_string(),
            effort: Some("high".to_string()),
        };
        store.create_work_item(&item).unwrap();
        let item = store.read_work_item("work-1").unwrap();
        let candidate = item.merge_candidates[0].clone();

        // The capture route is never gated into the frozen exact-SHA branch.
        assert!(
            frozen_no_expertise_reviewed_sha(&item, &candidate).is_none(),
            "capture-mode Work never resolves a frozen reviewed SHA"
        );

        // The pre-land target head is the base the rebase regenerates provenance off.
        let target_head_before =
            git::run_stdout(&project_root, &["rev-parse", "main"], "target before").unwrap();

        let resolver = ContentResolver::new(None);
        let config = WorkMergeConfig {
            project_root: project_root.as_path(),
            store: &store,
            work_item_id: "work-1",
            merge_candidate_id: "attempt-1-merge-candidate",
            resolver: &resolver,
            extra_args: &[],
            coder_kind: CoderKind::Codex,
            coder_override: Some(CoderKind::Pi),
            model: None,
            effort: Some("medium"),
            use_attempt_mapping: true,
            no_sandbox: true,
            run_post_merge_review: false,
        };
        // Drive the real public route; only the rebase coder is injected.
        let observed_mapping = std::sync::Arc::new(std::sync::Mutex::new(None));
        let observed_for_coder = std::sync::Arc::clone(&observed_mapping);
        let outcome = {
            let _coder = RebaseCoderOverrideGuard::engage(move |coder, model, effort| {
                *observed_for_coder.lock().unwrap() = Some((coder, model, effort));
                Box::new(RealRebaseCoder)
            });
            merge_candidate(config).unwrap()
        };

        assert_eq!(
            *observed_mapping.lock().unwrap(),
            Some((
                CoderKind::Pi,
                Some("gpt-5.6-terra".to_string()),
                Some("medium".to_string()),
            )),
            "the real public land route passes sparse land overrides and stored fields to the rebase constructor"
        );

        // The rebase ran: a Rebase Task was created and completed.
        let stored = store.read_work_item("work-1").unwrap();
        assert!(
            stored.attempts[0]
                .tasks
                .iter()
                .any(|task| task.kind == TaskKind::Rebase && task.status == TaskStatus::Complete),
            "the capture route creates and completes a Rebase Task"
        );

        // The regenerated Write provenance names the old base and the regenerated SHA:
        // the latest completed Write output records base_commit at the pre-land target
        // head and commit off the pre-rebase candidate commit. That regenerated SHA is
        // the single commit every land pointer must name.
        let write_output = stored.attempts[0]
            .tasks
            .iter()
            .rev()
            .find(|task| task.kind == TaskKind::Write && task.status == TaskStatus::Complete)
            .expect("a completed Write task carries regenerated provenance")
            .output
            .clone()
            .expect("the completed Write task has output");
        assert_eq!(
            write_output.base_commit.as_deref(),
            Some(target_head_before.as_str()),
            "regenerated Write provenance records the pre-land target head as the base"
        );
        let regenerated_sha = write_output.commit.clone();
        assert_ne!(
            regenerated_sha, candidate_commit,
            "provenance regeneration retargets the write output off the pre-rebase commit"
        );

        // Every persisted and returned land pointer names that one regenerated SHA.
        let merge_candidate = &stored.merge_candidates[0];
        assert_eq!(
            merge_candidate.merge_state.status,
            MergeCandidateMergeStatus::Merged,
            "the capture candidate lands"
        );
        assert_eq!(
            merge_candidate.candidate_commit, regenerated_sha,
            "the persisted Merge Candidate candidate_commit names the regenerated SHA"
        );
        assert_eq!(
            merge_candidate.merge_state.merged_commit.as_deref(),
            Some(regenerated_sha.as_str()),
            "the persisted merged_commit names the regenerated SHA"
        );
        assert_eq!(
            outcome.merged_commit, regenerated_sha,
            "the returned outcome names the regenerated SHA"
        );
        let target_head_after =
            git::run_stdout(&project_root, &["rev-parse", "main"], "target after").unwrap();
        assert_eq!(
            target_head_after, regenerated_sha,
            "the target HEAD fast-forwarded to exactly the regenerated SHA"
        );

        // check → fix → recheck ran: the conform commit is part of the regenerated tip,
        // even after the managed workspace cleanup a succeeded Learner triggers.
        let conformed_spec = format!("{regenerated_sha}:conformed.txt");
        assert!(
            git::run_raw(&project_root, &["cat-file", "-e", &conformed_spec])
                .unwrap()
                .status
                .success(),
            "fix-pre-merge conformed the tree and the conform commit is part of the landed tip"
        );
        // The absolute external event record is byte-exact `check\nfix\ncheck\n`, proving
        // fix-pre-merge ran between two checks on the public route.
        let events = fs::read_to_string(&event_log).unwrap();
        assert_eq!(
            events, "check\nfix\ncheck\n",
            "the capture route runs check, then fix, then a recheck"
        );
    }
}
