use anyhow::{Context, Result, bail};
use std::collections::HashSet;
use std::fs;
use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;

use crate::coder::CoderKind;
use crate::content::ContentResolver;
use crate::git;
use crate::review::{self, Verdict};
use crate::review_diff_command;
use crate::review_only_worktree;
use crate::work_model::{
    ArtifactRef, Attempt, AttemptLearning, AttemptReviewState, AttemptStatus, CoderMapping,
    MergeCandidateMergeStatus, PauseKind, Task, TaskKind, TaskOutput, TaskStatus, WorkItem,
    WorkModelStorageError, WorkModelStore, resolve_managed_sibling_workspace_path,
    work_artifact_path,
};
use crate::work_task_executor::{self, WorkTaskRunConfig};

const DEFAULT_MAX_PARALLEL_REVIEWERS: usize = 5;
const DEFAULT_MAX_TOTAL_WRITE_ROUNDS: usize = 10;
const DEFAULT_MAX_NO_PROGRESS_ROUNDS: usize = 2;

/// The reviewer-parallelism limit applied within a single Attempt. This is
/// independent of the local scheduler's Work-Item concurrency: one scheduled
/// Attempt is one Work slot regardless of how many reviewers run inside it.
pub fn max_parallel_reviewers() -> usize {
    std::env::var("FLUENT_MAX_PARALLEL_REVIEWERS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(DEFAULT_MAX_PARALLEL_REVIEWERS)
        .max(1)
}

fn max_total_write_rounds() -> usize {
    std::env::var("FLUENT_MAX_TOTAL_WRITE_ROUNDS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(DEFAULT_MAX_TOTAL_WRITE_ROUNDS)
        .max(1)
}

fn max_no_progress_rounds() -> usize {
    std::env::var("FLUENT_MAX_NO_PROGRESS_ROUNDS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(DEFAULT_MAX_NO_PROGRESS_ROUNDS)
        .max(1)
}

pub struct WorkAttemptRunConfig<'a> {
    pub project_root: &'a Path,
    pub store: &'a WorkModelStore,
    pub work_item_id: &'a str,
    pub attempt_id: &'a str,
    pub resolver: &'a ContentResolver,
    pub extra_args: &'a [String],
    pub no_sandbox: bool,
    /// Mapping resolved by the CLI for this invocation. Persist it through a
    /// fresh field-level mutation under the land lock, never a stale model write.
    pub resolved_coder_mapping: Option<&'a CoderMapping>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkAttemptRunOutcome {
    RanTask {
        task_id: String,
        output: String,
    },
    PlannedReviews {
        task_ids: Vec<String>,
    },
    MergeCandidateReady {
        candidate_id: String,
    },
    FollowUpRecoveryPending {
        candidate_id: String,
        stage: String,
        next_action: String,
    },
    PlannedWriteRound {
        task_id: String,
    },
    NeedsUser {
        handoff_path: String,
    },
    ReviewOnlyComplete,
    ReviewOnlyFailed,
}

#[derive(Debug)]
pub struct WorkAttemptRunResult {
    pub outcomes: Vec<WorkAttemptRunOutcome>,
}

pub fn run_attempt(config: WorkAttemptRunConfig<'_>) -> Result<WorkAttemptRunResult> {
    if let Some(mapping) = config.resolved_coder_mapping {
        let _land_lock =
            crate::land_lock::acquire(&crate::land_lock::lock_path(config.project_root))?;
        let mut item = read_work_item_or_not_found(config.store, config.work_item_id)?;
        let attempt = item
            .attempts
            .iter_mut()
            .find(|attempt| attempt.id == config.attempt_id)
            .ok_or_else(|| anyhow::anyhow!("Attempt {:?} not found", config.attempt_id))?;
        attempt.coder_mapping = mapping.clone();
        config.store.write_work_item(&item)?;
    }

    let mut outcomes = Vec::new();
    let mut worktree_ensured = false;

    loop {
        let item = read_work_item_or_not_found(config.store, config.work_item_id)?;
        item.ensure_not_abandoned()?;
        let attempt = item
            .attempts
            .iter()
            .find(|attempt| attempt.id == config.attempt_id)
            .ok_or_else(|| anyhow::anyhow!("Attempt {:?} not found", config.attempt_id))?;

        match reject_terminal_attempt(attempt)? {
            TerminalCheck::Reopen => {
                let mut item = item;
                let attempt_mut = item
                    .attempts
                    .iter_mut()
                    .find(|a| a.id == config.attempt_id)
                    .unwrap();
                crate::work_model::reopen_attempt(attempt_mut);
                config.store.write_work_item(&item)?;
                continue;
            }
            TerminalCheck::Continue => {}
        }

        if !worktree_ensured && attempt.kind.is_review_only_like() {
            reject_if_review_only_worktree_in_flight(
                config.store,
                config.work_item_id,
                config.attempt_id,
                attempt,
            )?;
            ensure_review_only_worktree_if_applicable(config.project_root, attempt)?;
            worktree_ensured = true;
        }

        if !attempt.kind.is_review_only_like()
            && attempt.status == AttemptStatus::Complete
            && attempt.review_state == Some(AttemptReviewState::Passed)
        {
            let mut item = item;
            let candidate_id = item.create_or_get_merge_candidate(config.attempt_id)?;
            let attempt_index = item
                .attempts
                .iter()
                .position(|a| a.id == config.attempt_id)
                .expect("attempt exists");
            let learner_pending = item.attempts[attempt_index]
                .learning
                .as_ref()
                .map(|learning| learning.is_failed())
                .unwrap_or(true);
            let landed_success = item
                .merge_candidates
                .iter()
                .find(|candidate| candidate.id == candidate_id)
                .is_some_and(|candidate| {
                    candidate.merge_state.status == MergeCandidateMergeStatus::Merged
                        && item.attempts[attempt_index]
                            .learning
                            .as_ref()
                            .is_some_and(|learning| learning.is_succeeded())
                });
            if learner_pending || landed_success {
                // Serialize the retry against a concurrent land on the same
                // project so the two cannot both mutate the candidate. Under the
                // lock, read the fresh merge status: a candidate that has already
                // merged forces the retry into handoff-only mode, which never
                // mutates expertise or the merged branch.
                let _land_lock =
                    crate::land_lock::acquire(&crate::land_lock::lock_path(config.project_root))?;
                item = config.store.read_work_item(config.work_item_id)?;
                let candidate_id = item.create_or_get_merge_candidate(config.attempt_id)?;
                let attempt_index = item
                    .attempts
                    .iter()
                    .position(|a| a.id == config.attempt_id)
                    .ok_or_else(|| {
                        anyhow::anyhow!("Attempt {:?} disappeared during retry", config.attempt_id)
                    })?;
                let learner_still_pending = item.attempts[attempt_index]
                    .learning
                    .as_ref()
                    .map(|learning| learning.is_failed())
                    .unwrap_or(true);
                if item.attempts[attempt_index].status != AttemptStatus::Complete
                    || item.attempts[attempt_index].review_state != Some(AttemptReviewState::Passed)
                {
                    bail!(
                        "Attempt {:?} is no longer eligible for Learner retry",
                        config.attempt_id
                    );
                }
                let candidate = item
                    .merge_candidates
                    .iter()
                    .find(|candidate| candidate.id == candidate_id)
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "Merge Candidate {:?} disappeared during Learner retry",
                            candidate_id
                        )
                    })?;
                let merged_commit =
                    if candidate.merge_state.status == MergeCandidateMergeStatus::Merged {
                        Some(candidate.merge_state.merged_commit.clone().ok_or_else(|| {
                            anyhow::anyhow!(
                                "merged candidate {:?} has no persisted merged commit",
                                candidate_id
                            )
                        })?)
                    } else {
                        None
                    };
                let handoff_only =
                    work_task_executor::learner_is_handoff_only(merged_commit.is_some());

                if learner_still_pending {
                    let run_coder = |request: &LearnerCoderRequest<'_>| {
                        default_learner_run_coder(&config, request)
                    };
                    run_learner_step(
                        config.project_root,
                        &mut item,
                        attempt_index,
                        &candidate_id,
                        handoff_only,
                        &LearnerConfig {
                            run_coder: &run_coder,
                        },
                    );
                    config.store.write_work_item(&item)?;
                    if handoff_only
                        && item.attempts[attempt_index]
                            .learning
                            .as_ref()
                            .is_some_and(|learning| learning.is_failed())
                    {
                        outcomes.push(WorkAttemptRunOutcome::FollowUpRecoveryPending {
                            candidate_id,
                            stage: "learner".to_string(),
                            next_action: format!(
                                "Retry `fluent attempt run {} {}` on a host that can enforce the trusted Learner sandbox.",
                                config.work_item_id, config.attempt_id
                            ),
                        });
                        return Ok(WorkAttemptRunResult { outcomes });
                    }
                }

                // A successful post-land handoff always passes through the same
                // durable boundary as land. This also resumes a success that
                // crashed or failed after Learning persisted but before effects
                // completed, without invoking the coder again.
                if let Some(merged_commit) = merged_commit
                    && item.attempts[attempt_index]
                        .learning
                        .as_ref()
                        .is_some_and(|learning| learning.is_succeeded())
                {
                    let completed =
                        crate::work_merge_executor::process_landed_follow_ups_at_boundary(
                            config.project_root,
                            config.store,
                            config.work_item_id,
                            &candidate_id,
                            &merged_commit,
                        )?;
                    if !completed {
                        let refreshed = config.store.read_work_item(config.work_item_id)?;
                        let failure = refreshed
                            .merge_candidates
                            .iter()
                            .find(|candidate| candidate.id == candidate_id)
                            .and_then(|candidate| candidate.merge_state.follow_up_failure.as_ref())
                            .ok_or_else(|| {
                                anyhow::anyhow!(
                                    "Merge Candidate {:?} has incomplete follow-up recovery but \
                                     no durable recovery state",
                                    candidate_id
                                )
                            })?;
                        outcomes.push(WorkAttemptRunOutcome::FollowUpRecoveryPending {
                            candidate_id,
                            stage: failure.stage.clone(),
                            next_action: failure.next_action.clone(),
                        });
                        return Ok(WorkAttemptRunResult { outcomes });
                    }
                }
            } else {
                config.store.write_work_item(&item)?;
            }
            outcomes.push(WorkAttemptRunOutcome::MergeCandidateReady { candidate_id });
            return Ok(WorkAttemptRunResult { outcomes });
        }

        if let Some(task) = attempt
            .tasks
            .iter()
            .find(|task| task.status == TaskStatus::Planned && is_task_ready(task, &attempt.tasks))
        {
            if is_review_phase_task(task) && supports_parallel_review_phase(attempt) {
                let planned_review_ids: Vec<String> = attempt
                    .tasks
                    .iter()
                    .filter(|t| {
                        is_review_phase_task(t)
                            && t.status == TaskStatus::Planned
                            && is_task_ready(t, &attempt.tasks)
                    })
                    .map(|t| t.id.clone())
                    .collect();
                run_parallel_reviews(&config, &planned_review_ids, &mut outcomes)?;
                continue;
            }

            let result = work_task_executor::run_task(WorkTaskRunConfig {
                project_root: config.project_root,
                store: config.store,
                work_item_id: config.work_item_id,
                attempt_id: config.attempt_id,
                task_id: &task.id,
                resolver: config.resolver,
                extra_args: config.extra_args,
                no_sandbox: config.no_sandbox,
                store_lock: None,
            })?;
            outcomes.push(WorkAttemptRunOutcome::RanTask {
                task_id: result.task_id,
                output: result.output,
            });
            continue;
        }

        {
            let executing_tasks: Vec<String> = attempt
                .tasks
                .iter()
                .filter(|task| task.status == TaskStatus::Executing)
                .map(|task| task.id.clone())
                .collect();

            if !executing_tasks.is_empty() {
                let mut has_live = false;
                let mut stale_ids = Vec::new();
                for task_id in &executing_tasks {
                    if executing_task_is_live(config.project_root, config.work_item_id, task_id) {
                        has_live = true;
                    } else {
                        stale_ids.push(task_id.clone());
                    }
                }

                if has_live {
                    bail!(
                        "Attempt {:?} has an executing Task and cannot be advanced",
                        config.attempt_id
                    );
                }

                let mut item = item;
                let attempt_mut = item
                    .attempts
                    .iter_mut()
                    .find(|a| a.id == config.attempt_id)
                    .unwrap();
                for task in &mut attempt_mut.tasks {
                    if stale_ids.contains(&task.id) {
                        task.status = TaskStatus::Planned;
                    }
                }
                config.store.write_work_item(&item)?;
                continue;
            }
        }
        if let Some(task) = attempt
            .tasks
            .iter()
            .find(|task| matches!(task.status, TaskStatus::Failed | TaskStatus::NeedsUser))
        {
            bail!(
                "Attempt {:?} cannot advance because Task {:?} is {}",
                config.attempt_id,
                task.id,
                task.status
            );
        }

        if !attempt.kind.is_review_only_like()
            && has_completed_write(attempt.tasks.as_slice())
            && !has_review_after_latest_write(attempt.tasks.as_slice())
        {
            let review_roles = next_review_roles(attempt);
            let mut item = item;
            let task_ids = item.add_next_review_tasks(config.attempt_id, &review_roles)?;
            config.store.write_work_item(&item)?;
            outcomes.push(WorkAttemptRunOutcome::PlannedReviews {
                task_ids: task_ids.clone(),
            });
            continue;
        }

        if completed_review_tasks_after_latest_write(attempt.tasks.as_slice())
            .next()
            .is_some()
            && !has_open_review_round(attempt.tasks.as_slice())
        {
            let can_advance = can_advance_loop(config.project_root, attempt)?;
            let run_coder =
                |request: &LearnerCoderRequest<'_>| default_learner_run_coder(&config, request);
            let outcome = interpret_reviews(
                config.project_root,
                config.store,
                item,
                config.attempt_id,
                can_advance,
                Some(LearnerConfig {
                    run_coder: &run_coder,
                }),
            )?;
            let should_stop = matches!(
                outcome,
                WorkAttemptRunOutcome::MergeCandidateReady { .. }
                    | WorkAttemptRunOutcome::FollowUpRecoveryPending { .. }
                    | WorkAttemptRunOutcome::NeedsUser { .. }
                    | WorkAttemptRunOutcome::ReviewOnlyComplete
                    | WorkAttemptRunOutcome::ReviewOnlyFailed
            );
            outcomes.push(outcome);
            if should_stop {
                return Ok(WorkAttemptRunResult { outcomes });
            }
            continue;
        }

        bail!(
            "Attempt {:?} has no planned transition to advance",
            config.attempt_id
        );
    }
}

/// Run the Learner with the run-level configuration, mapping a per-run
/// `LearnerCoderRequest` onto the executor's `LearnerRunInputs`. Both Learner
/// entry points in `run_attempt` — the retry fast-path and the normal
/// review-interpretation path — share this adapter so a new input field is
/// threaded through one place.
fn default_learner_run_coder(
    config: &WorkAttemptRunConfig<'_>,
    request: &LearnerCoderRequest<'_>,
) -> Result<()> {
    work_task_executor::run_learner(work_task_executor::LearnerRunInputs {
        workspace_path: request.workspace_path,
        resolver: config.resolver,
        extra_args: config.extra_args,
        coder_kind: request.coder_kind,
        no_sandbox: config.no_sandbox,
        model: request.model,
        effort: request.effort,
        review_artifact_paths: request.review_artifact_paths,
        tester_artifact_paths: request.tester_artifact_paths,
        diff_command: request.diff_command,
        handoff_dir: request.handoff_dir,
        transcript_path: Some(request.transcript_path),
        handoff_only: request.handoff_only,
        denied_write_roots: request.denied_write_roots,
        repair: request.repair,
    })
}

struct SlotGuard {
    state: Arc<(Mutex<usize>, Condvar)>,
}

impl Drop for SlotGuard {
    fn drop(&mut self) {
        let (lock, cvar) = &*self.state;
        let mut count = lock.lock().unwrap_or_else(|e| e.into_inner());
        *count -= 1;
        cvar.notify_one();
    }
}

fn acquire_slot(state: &Arc<(Mutex<usize>, Condvar)>, cap: usize) -> SlotGuard {
    let (lock, cvar) = &**state;
    let mut count = lock.lock().unwrap_or_else(|e| e.into_inner());
    while *count >= cap {
        count = cvar.wait(count).unwrap_or_else(|e| e.into_inner());
    }
    *count += 1;
    SlotGuard {
        state: Arc::clone(state),
    }
}

fn run_parallel_reviews(
    config: &WorkAttemptRunConfig<'_>,
    task_ids: &[String],
    outcomes: &mut Vec<WorkAttemptRunOutcome>,
) -> Result<()> {
    let cap = max_parallel_reviewers();
    let semaphore = Arc::new((Mutex::new(0_usize), Condvar::new()));
    let store_lock = Mutex::new(());

    let results: Vec<Result<work_task_executor::WorkTaskRunResult>> = thread::scope(|scope| {
        let store_lock_ref = &store_lock;
        let handles: Vec<_> = task_ids
            .iter()
            .map(|task_id| {
                let sem = Arc::clone(&semaphore);
                scope.spawn(move || {
                    let _guard = acquire_slot(&sem, cap);
                    work_task_executor::run_task(WorkTaskRunConfig {
                        project_root: config.project_root,
                        store: config.store,
                        work_item_id: config.work_item_id,
                        attempt_id: config.attempt_id,
                        task_id,
                        resolver: config.resolver,
                        extra_args: config.extra_args,
                        no_sandbox: config.no_sandbox,
                        store_lock: Some(store_lock_ref),
                    })
                })
            })
            .collect();

        handles
            .into_iter()
            .map(|h| match h.join() {
                Ok(result) => result,
                Err(_) => Err(anyhow::anyhow!("Reviewer thread panicked")),
            })
            .collect()
    });

    let mut first_error = None;
    for result in results {
        match result {
            Ok(run_result) => {
                outcomes.push(WorkAttemptRunOutcome::RanTask {
                    task_id: run_result.task_id,
                    output: run_result.output,
                });
            }
            Err(error) => {
                if first_error.is_none() {
                    first_error = Some(error);
                }
            }
        }
    }

    if let Some(error) = first_error {
        return Err(error);
    }
    Ok(())
}

/// Whether the Attempt can fan its review-phase Tasks out in parallel.
///
/// Write Attempts always can. Review-only Attempts can when they read
/// from an isolated per-branch worktree — that's the case for
/// PostMergeReview today, and for ReviewCodebase once step 2 lands.
/// Review-only Attempts whose workspace is the source checkout (`.`)
/// must run serially: parallel reviewers would collide on the project
/// root's `.fluent/work/artifacts/` tree and trip the source-checkout
/// review guard.
fn supports_parallel_review_phase(attempt: &Attempt) -> bool {
    if !attempt.kind.is_review_only_like() {
        return true;
    }
    attempt
        .tasks
        .first()
        .and_then(|task| task.workspace_access.reads.first())
        .map(|workspace| {
            review_only_worktree::is_review_only_worktree_workspace_path(&workspace.path)
        })
        .unwrap_or(false)
}

/// If the Attempt's tasks read from a review-only worktree, refuse to
/// start when another review-only Attempt is already in flight against
/// the same worktree. Bails with a message naming the in-flight
/// Attempt so the operator can investigate.
fn reject_if_review_only_worktree_in_flight(
    store: &WorkModelStore,
    work_item_id: &str,
    attempt_id: &str,
    attempt: &Attempt,
) -> Result<()> {
    let Some(branch) = attempt
        .tasks
        .first()
        .and_then(|task| task.review_context.as_ref())
        .map(|context| context.source_branch.as_str())
    else {
        return Ok(());
    };
    let workspace_path = attempt
        .tasks
        .first()
        .and_then(|task| task.workspace_access.reads.first())
        .map(|workspace| workspace.path.as_str())
        .unwrap_or("");
    if !review_only_worktree::is_review_only_worktree_workspace_path(workspace_path) {
        return Ok(());
    }
    if let Some(in_flight) =
        review_only_worktree::detect_in_flight(store, branch, Some((work_item_id, attempt_id)))?
    {
        bail!(
            "Review-only worktree for branch {:?} is already in flight (Work Item {:?}, Attempt {:?}); \
             wait for it to complete or recover it before re-running",
            in_flight.branch,
            in_flight.work_item_id,
            in_flight.attempt_id
        );
    }
    Ok(())
}

/// If the Attempt's tasks read from a review-only worktree, make sure
/// the worktree exists at the recorded `candidate_commit` before any
/// Task runs. PostMergeReview Attempts always need this; ReviewCodebase
/// Attempts will once step 2 introduces the worktree path there.
fn ensure_review_only_worktree_if_applicable(project_root: &Path, attempt: &Attempt) -> Result<()> {
    let Some(task) = attempt.tasks.first() else {
        return Ok(());
    };
    let Some(workspace) = task.workspace_access.reads.first() else {
        return Ok(());
    };
    if !review_only_worktree::is_review_only_worktree_workspace_path(&workspace.path) {
        return Ok(());
    }
    let Some(context) = task.review_context.as_ref() else {
        return Ok(());
    };
    review_only_worktree::ensure(
        project_root,
        &context.source_branch,
        &context.candidate_commit,
    )
    .map(|_| ())
}

enum TerminalCheck {
    Continue,
    /// The pause is resumable: reopen the Attempt and retry. Auth pauses re-check
    /// the token; transcript-pump pauses retry after the operator fixes the
    /// broken console/transcript transport.
    Reopen,
}

fn reject_terminal_attempt(attempt: &Attempt) -> Result<TerminalCheck> {
    match attempt.status {
        AttemptStatus::Failed => bail!("Attempt is failed and cannot be advanced"),
        AttemptStatus::NeedsUser => match attempt.pause_kind {
            Some(PauseKind::Auth) => Ok(TerminalCheck::Reopen),
            Some(PauseKind::TranscriptPump) => Ok(TerminalCheck::Reopen),
            Some(PauseKind::Uncertain) => bail!(
                "Attempt is paused with uncertain reviews. \
                 Resolve the uncertain verdicts and re-run; \
                 resuming this pause kind is not yet supported."
            ),
            Some(PauseKind::RoundCap) => bail!(
                "Attempt is paused at the write-round cap. \
                 Address the failing reviews and re-run; \
                 resuming this pause kind is not yet supported."
            ),
            None => bail!("Attempt needs user input before it can advance"),
        },
        _ => Ok(TerminalCheck::Continue),
    }
}

/// Decide whether the Attempt loop may plan another write round.
///
/// Two backstops, both attempt-wide and env-tunable:
/// - Hard ceiling: total completed write rounds must be below
///   `FLUENT_MAX_TOTAL_WRITE_ROUNDS` (default 10).
/// - No-progress streak: consecutive trailing review rounds where ALL
///   completed reviewers reported `Progress: no` must be below
///   `FLUENT_MAX_NO_PROGRESS_ROUNDS` (default 2). A reviewer that
///   reports `yes`, `partial`, `first-pass`, or is missing the field
///   does NOT contribute to the no-progress streak — the rule is
///   lenient on purpose.
fn can_advance_loop(project_root: &Path, attempt: &Attempt) -> Result<bool> {
    let total_rounds = attempt
        .tasks
        .iter()
        .filter(|task| task.kind == TaskKind::Write)
        .count();
    if total_rounds >= max_total_write_rounds() {
        return Ok(false);
    }
    let streak = consecutive_no_progress_rounds(project_root, attempt)?;
    Ok(streak < max_no_progress_rounds())
}

/// Walk completed review rounds from the latest backwards. A round is
/// "no-progress" only when every completed review in it reported
/// `Progress: no`. Returns the consecutive trailing count.
fn consecutive_no_progress_rounds(project_root: &Path, attempt: &Attempt) -> Result<usize> {
    let mut by_round: std::collections::BTreeMap<usize, Vec<&Task>> =
        std::collections::BTreeMap::new();
    for task in &attempt.tasks {
        if task.kind != TaskKind::Review || task.status != TaskStatus::Complete {
            continue;
        }
        let round = review_task_round_number(&attempt.id, &task.id).unwrap_or(1);
        by_round.entry(round).or_default().push(task);
    }

    let mut streak = 0_usize;
    for (_round, tasks) in by_round.iter().rev() {
        let mut all_no = !tasks.is_empty();
        for task in tasks {
            let Some(area) = task.artifact_area.as_ref() else {
                all_no = false;
                break;
            };
            let dir =
                work_task_executor::resolve_managed_artifact_area_path(project_root, &area.path)?;
            let content = fs::read_to_string(dir.join("review.md")).unwrap_or_default();
            if review::extract_progress(&content) != review::Progress::No {
                all_no = false;
                break;
            }
        }
        if all_no {
            streak += 1;
        } else {
            break;
        }
    }
    Ok(streak)
}

fn review_task_round_number(attempt_id: &str, task_id: &str) -> Option<usize> {
    let prefix = format!("{attempt_id}-review-");
    let suffix = task_id.strip_prefix(&prefix)?;
    let (round, _role) = suffix.split_once('-')?;
    round.parse::<usize>().ok()
}

fn is_review_phase_task(task: &Task) -> bool {
    matches!(
        task.kind,
        TaskKind::Review | TaskKind::BehaviorTests | TaskKind::Tester
    )
}

fn is_task_ready(task: &Task, all_tasks: &[Task]) -> bool {
    let Some(dep_id) = task.depends_on.as_deref() else {
        return true;
    };
    all_tasks
        .iter()
        .any(|t| t.id == dep_id && t.status == TaskStatus::Complete)
}

fn has_completed_write(tasks: &[Task]) -> bool {
    tasks
        .iter()
        .any(|task| task.kind == TaskKind::Write && task.status == TaskStatus::Complete)
}

fn has_open_review_round(tasks: &[Task]) -> bool {
    for task in tasks.iter().rev() {
        match task.kind {
            TaskKind::Write => return false,
            TaskKind::Review | TaskKind::BehaviorTests | TaskKind::Tester
                if task.status != TaskStatus::Complete =>
            {
                return true;
            }
            _ => {}
        }
    }
    false
}

fn has_review_after_latest_write(tasks: &[Task]) -> bool {
    let Some(last_write_index) = tasks.iter().rposition(|task| task.kind == TaskKind::Write) else {
        return false;
    };
    tasks[last_write_index + 1..]
        .iter()
        .any(|task| is_review_phase_task(task))
}

fn next_review_roles(attempt: &Attempt) -> Vec<&'static str> {
    let Some(latest_write) = attempt
        .tasks
        .iter()
        .rev()
        .find(|task| task.kind == TaskKind::Write && task.status == TaskStatus::Complete)
    else {
        return review::REVIEWERS.to_vec();
    };

    if latest_write.input_artifacts.is_empty() {
        return review::REVIEWERS.to_vec();
    }

    let input_producer_ids = latest_write
        .input_artifacts
        .iter()
        .map(|artifact| artifact.producer_id.as_str())
        .collect::<HashSet<_>>();
    let roles = review::REVIEWERS
        .iter()
        .copied()
        .filter(|role| {
            attempt.tasks.iter().any(|task| {
                task.kind == TaskKind::Review
                    && task.status == TaskStatus::Complete
                    && input_producer_ids.contains(task.id.as_str())
                    && task.role == *role
            })
        })
        .collect::<Vec<_>>();

    if roles.is_empty() {
        review::REVIEWERS.to_vec()
    } else {
        roles
    }
}

/// Everything the Learner coder needs, assembled by the loop from the Attempt's
/// completed change and every review round's artifacts.
struct LearnerCoderRequest<'a> {
    workspace_path: &'a Path,
    review_artifact_paths: &'a [PathBuf],
    tester_artifact_paths: &'a [PathBuf],
    diff_command: &'a str,
    handoff_dir: &'a Path,
    transcript_path: &'a Path,
    denied_write_roots: &'a [PathBuf],
    coder_kind: CoderKind,
    model: Option<&'a str>,
    effort: Option<&'a str>,
    handoff_only: bool,
    /// When set, a bounded schema repair rather than a fresh audit.
    repair: Option<work_task_executor::SchemaRepairInput<'a>>,
}

/// The Learner's coder-run is injected so the orchestration around it — running
/// once per passing Attempt, confining commits, and persisting a handoff — can be
/// exercised without spawning a real coder.
struct LearnerConfig<'a> {
    run_coder: &'a dyn Fn(&LearnerCoderRequest<'_>) -> Result<()>,
}

/// Run the Learner for a passing code-producing Attempt and record its durable,
/// retryable outcome on the Attempt. A failure warns the operator and leaves the
/// Merge Candidate unaffected; the record can be retried later.
fn run_learner_step(
    project_root: &Path,
    item: &mut WorkItem,
    attempt_index: usize,
    candidate_id: &str,
    handoff_only: bool,
    config: &LearnerConfig<'_>,
) {
    let runs = item.attempts[attempt_index]
        .learning
        .as_ref()
        .map(|learning| learning.runs)
        .unwrap_or(0)
        + 1;
    match try_learn(
        project_root,
        item,
        attempt_index,
        candidate_id,
        handoff_only,
        config,
    ) {
        Ok(handoff_ref) => {
            item.attempts[attempt_index].learning =
                Some(AttemptLearning::succeeded(runs, handoff_ref));
        }
        Err(err) => {
            eprintln!("  Warning: learner failed, continuing without handoff: {err}");
            item.attempts[attempt_index].learning =
                Some(AttemptLearning::failed(runs, err.to_string()));
        }
    }
}

fn try_learn(
    project_root: &Path,
    item: &mut WorkItem,
    attempt_index: usize,
    candidate_id: &str,
    handoff_only: bool,
    config: &LearnerConfig<'_>,
) -> Result<crate::follow_up::ArtifactRef> {
    let work_item_id = item.id.clone();
    let attempt = &item.attempts[attempt_index];
    let attempt_id = attempt.id.clone();
    let (write_task_index, write_output) = attempt
        .tasks
        .iter()
        .enumerate()
        .rev()
        .find(|(_, task)| task.kind == TaskKind::Write && task.status == TaskStatus::Complete)
        .and_then(|(i, task)| task.output.as_ref().map(|output| (i, output.clone())))
        .ok_or_else(|| anyhow::anyhow!("no completed write task with output"))?;

    let workspace_path = resolve_managed_sibling_workspace_path(
        project_root,
        &write_output.workspace_path,
        "learner workspace",
    )?;

    let review_artifact_paths = all_review_artifact_paths(project_root, attempt)?;
    let tester_artifact_paths = all_tester_artifact_paths(project_root, attempt)?;

    let mapping_pair = attempt.coder_mapping.for_task_kind(TaskKind::Write);
    let coder_kind = mapping_pair.coder;
    let model: Option<String> = if mapping_pair.model.is_empty() {
        None
    } else {
        Some(mapping_pair.model.clone())
    };
    let effort: Option<String> = mapping_pair.effort.clone();

    // The commit the confinement compares against. Normally the Learner runs
    // right after the write task, so its baseline is the write commit. A post-
    // land retry runs against the current (possibly rebased) worktree tip, so its
    // baseline is the HEAD just before the Learner ran — that way only the
    // Learner's own changes, not the earlier rebase, are attributed to it.
    let baseline_commit = if handoff_only {
        item.merge_candidates
            .iter()
            .find(|candidate| candidate.id == candidate_id)
            .filter(|candidate| candidate.merge_state.status == MergeCandidateMergeStatus::Merged)
            .and_then(|candidate| candidate.merge_state.merged_commit.clone())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "handoff-only Learner retry has no persisted merged commit for {:?}",
                    candidate_id
                )
            })?
    } else {
        write_output.commit.clone()
    };

    let accepted_base = attempt
        .tasks
        .iter()
        .filter(|task| task.kind == TaskKind::Write && task.status == TaskStatus::Complete)
        .filter_map(|task| task.output.as_ref()?.base_commit.as_ref())
        .next()
        .cloned()
        .map(Ok)
        .unwrap_or_else(|| {
            if handoff_only {
                let candidate_commit = item
                    .merge_candidates
                    .iter()
                    .find(|candidate| candidate.id == candidate_id)
                    .map(|candidate| candidate.candidate_commit.as_str())
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "cannot recover accepted diff base: Merge Candidate is unavailable"
                        )
                    })?;
                recover_legacy_accepted_base(
                    &workspace_path,
                    &work_item_id,
                    attempt,
                    candidate_commit,
                    &baseline_commit,
                )
            } else {
                Ok(write_output.source_branch.clone())
            }
        })?;
    let accepted_tip = if handoff_only {
        baseline_commit.as_str()
    } else {
        write_output.commit.as_str()
    };
    let isolated_workspace = handoff_only
        .then(|| {
            HandoffOnlyWorkspace::create(
                project_root,
                &work_item_id,
                &attempt_id,
                &baseline_commit,
                &review_artifact_paths,
                &tester_artifact_paths,
            )
        })
        .transpose()?;
    let learner_workspace_path = isolated_workspace
        .as_ref()
        .map(|isolated| isolated.workspace_path.as_path())
        .unwrap_or(workspace_path.as_path());
    let real_handoff_dir =
        project_root.join(crate::learner::handoff_dir_rel(&work_item_id, &attempt_id));
    // Allocate a collision-safe, host-owned run directory from on-disk state. It
    // holds the transcript and submitted-draft evidence the coder cannot reach;
    // the coder-writable staging directory is a separate sibling beneath it.
    let run_dir = allocate_learner_run_dir(&real_handoff_dir.join("runs"))?;
    // The transcript lives on the host-owned run surface, outside every coder-
    // writable root: the host writes it, so it survives the disposable clone and
    // lets the one-refresh auth policy classify a 401.
    let transcript_path = run_dir.join("transcript.jsonl");
    // Pre-land the coder stages its draft in a writable subdir beneath the run
    // surface; post-land it stages inside the isolated clone because the live
    // project root is denied. Either way the host-owned evidence stays a non-
    // writable sibling of the staging directory.
    let pre_land_staging = run_dir.join("staging");
    let handoff_dir = isolated_workspace
        .as_ref()
        .map(|isolated| isolated.handoff_dir.as_path())
        .unwrap_or(pre_land_staging.as_path());
    let learner_review_artifact_paths = isolated_workspace
        .as_ref()
        .map(|isolated| isolated.review_artifact_paths.as_slice())
        .unwrap_or(review_artifact_paths.as_slice());
    let learner_tester_artifact_paths = isolated_workspace
        .as_ref()
        .map(|isolated| isolated.tester_artifact_paths.as_slice())
        .unwrap_or(tester_artifact_paths.as_slice());
    let diff_range = format!("{accepted_base}...{accepted_tip}");
    let diff_command =
        review_diff_command::render_review_diff_command(learner_workspace_path, &diff_range);
    let denied_write_roots = if handoff_only {
        let mut roots = vec![project_root.to_path_buf(), workspace_path.clone()];
        roots.push(crate::worktree::git_common_dir(project_root)?);
        roots.sort();
        roots.dedup();
        roots
    } else {
        Vec::new()
    };

    let coder_result = (config.run_coder)(&LearnerCoderRequest {
        workspace_path: learner_workspace_path,
        review_artifact_paths: learner_review_artifact_paths,
        tester_artifact_paths: learner_tester_artifact_paths,
        diff_command: &diff_command,
        handoff_dir,
        transcript_path: &transcript_path,
        denied_write_roots: &denied_write_roots,
        coder_kind,
        model: model.as_deref(),
        effort: effort.as_deref(),
        handoff_only,
        repair: None,
    });
    // Confine the Learner's commit. In the normal case an expertise commit is
    // accepted and advances the Merge Candidate tip. In a post-land handoff-only
    // run any mutation is denied and discarded, leaving the merged commit and
    // target branch untouched; the denied paths are recorded so the missed
    // expertise can be captured as non-corrective follow-ups.
    let confinement_result = apply_learner_confinement(
        learner_workspace_path,
        item,
        attempt_index,
        write_task_index,
        &write_output,
        candidate_id,
        handoff_only,
        &baseline_commit,
    );
    // Always attempt candidate cleanup, even when the coder fails. Handoff-only
    // cleanup acts on the disposable clone, so no restoration can overwrite
    // live target or shared-Git changes from another actor.
    let confinement = confinement_result?;
    coder_result?;
    // Publish the coder's staged draft to the canonical managed handoff path,
    // then snapshot it as immutable submitted-draft evidence on the host-owned
    // run surface the coder cannot reach.
    if let Some(isolated) = &isolated_workspace {
        isolated.publish_draft(project_root, &work_item_id, &attempt_id)?;
    } else {
        publish_pre_land_draft(project_root, &work_item_id, &attempt_id, handoff_dir)?;
    }
    record_submitted_draft(project_root, &work_item_id, &attempt_id, &run_dir)?;

    let mut draft = crate::learner::read_draft(project_root, &work_item_id, &attempt_id)?;
    for path in &confinement.denied_paths {
        draft
            .follow_ups
            .push(work_task_executor::expertise_proposal_follow_up(
                format!("expertise-{}", sanitize_denied_path(path)),
                format!(
                    "Capture durable project knowledge a post-land retry could not write to {path}"
                ),
            ));
    }
    // Normalize toward Observation-only and record every normalization on the
    // host-owned run surface, so a single malformed optional field cannot reject
    // the draft yet the change is never silent.
    let (draft, normalizations) = crate::learner::normalize_draft(draft);
    record_run_normalizations(&run_dir, &normalizations)?;

    // Bounded schema-repair loop. When a draft fails schema validation and the
    // configured budget remains, re-invoke the coder with the rejected draft and
    // exact error as a schema repair — not a fresh audit — and accept a repair
    // only when it preserves every prior follow-up id and its content.
    let mut repair_budget = crate::config::resolve_learner_schema_repair_budget(project_root)
        .unwrap_or(crate::config::DEFAULT_LEARNER_SCHEMA_REPAIR_BUDGET);
    let mut current_draft = draft;
    let mut current_run_dir = run_dir;
    let handoff = loop {
        let prior = current_draft.clone();
        match crate::learner::stamp_handoff(
            current_draft,
            &work_item_id,
            &attempt_id,
            candidate_id,
            confinement.expertise.clone(),
        ) {
            Ok(handoff) => break handoff,
            Err(error) => {
                // A rejected draft preserves its submitted bytes (already
                // recorded) and its full validation error as immutable run
                // artifacts before any repair may publish another draft.
                record_run_rejection(&current_run_dir, &error)?;
                if repair_budget == 0 {
                    return Err(error);
                }
                repair_budget -= 1;

                // A fresh run identity keeps the repair's transcript and
                // submitted-draft evidence immutable and distinct.
                let repair_run_dir = allocate_learner_run_dir(&real_handoff_dir.join("runs"))?;
                let repair_transcript = repair_run_dir.join("transcript.jsonl");
                let repair_staging_owned = repair_run_dir.join("staging");
                let repair_staging = isolated_workspace
                    .as_ref()
                    .map(|isolated| isolated.handoff_dir.as_path())
                    .unwrap_or(repair_staging_owned.as_path());
                let rejected_draft = serde_json::to_string(&prior)?;
                let validation_error = format!("{error:#}");

                (config.run_coder)(&LearnerCoderRequest {
                    workspace_path: learner_workspace_path,
                    review_artifact_paths: learner_review_artifact_paths,
                    tester_artifact_paths: learner_tester_artifact_paths,
                    diff_command: &diff_command,
                    handoff_dir: repair_staging,
                    transcript_path: &repair_transcript,
                    denied_write_roots: &denied_write_roots,
                    coder_kind,
                    model: model.as_deref(),
                    effort: effort.as_deref(),
                    handoff_only,
                    repair: Some(work_task_executor::SchemaRepairInput {
                        rejected_draft: &rejected_draft,
                        validation_error: &validation_error,
                    }),
                })?;
                if let Some(isolated) = &isolated_workspace {
                    isolated.publish_draft(project_root, &work_item_id, &attempt_id)?;
                } else {
                    publish_pre_land_draft(
                        project_root,
                        &work_item_id,
                        &attempt_id,
                        repair_staging,
                    )?;
                }
                record_submitted_draft(project_root, &work_item_id, &attempt_id, &repair_run_dir)?;

                let repaired =
                    crate::learner::read_draft(project_root, &work_item_id, &attempt_id)?;
                // Reject a repair that drops a prior follow-up id or rewrites a
                // prior follow-up's content: retain the earlier draft instead.
                if let Err(reject) = crate::learner::accept_schema_repair(&prior, &repaired) {
                    record_run_rejection(&repair_run_dir, &reject)?;
                    return Err(reject);
                }
                let (repaired, repair_notes) = crate::learner::normalize_draft(repaired);
                record_run_normalizations(&repair_run_dir, &repair_notes)?;
                current_draft = repaired;
                current_run_dir = repair_run_dir;
            }
        }
    };
    let handoff_ref =
        crate::learner::write_handoff(project_root, &work_item_id, &attempt_id, &handoff)?;
    Ok(handoff_ref)
}

/// Whether an executing Task is still live. Liveness is owned by the Task's
/// process-held lease, never by transcript age or pump-status timestamps: those
/// are diagnostics only and carry no authority to reclaim a Task. Recovery must
/// not resurrect or abandon a Task on the strength of how old its transcript is.
fn executing_task_is_live(project_root: &Path, work_item_id: &str, task_id: &str) -> bool {
    let lock_path = crate::lease::task_lock_path(project_root, work_item_id, task_id);
    crate::lease::is_leased(&lock_path)
}

/// Recover the immutable accepted base for an already-merged legacy Attempt
/// whose TaskOutput predates `base_commit`. Recovery succeeds only from one
/// intact rebase session bound to the retained candidate and original target;
/// missing, partial, or repeated sessions fail closed.
fn recover_legacy_accepted_base(
    workspace_path: &Path,
    work_item_id: &str,
    attempt: &Attempt,
    candidate_commit: &str,
    merged_commit: &str,
) -> Result<String> {
    let completed_writes = attempt
        .tasks
        .iter()
        .filter(|task| task.kind == TaskKind::Write && task.status == TaskStatus::Complete)
        .collect::<Vec<_>>();
    let first_write = completed_writes
        .first()
        .ok_or_else(|| anyhow::anyhow!("cannot recover accepted diff base: no completed Write"))?;
    let candidate_ref = format!(
        "refs/heads/work/{}/{}/{}",
        work_item_id, attempt.id, first_write.id
    );
    let persisted_tips = completed_writes
        .iter()
        .filter_map(|task| task.output.as_ref().map(|output| output.commit.as_str()))
        .collect::<HashSet<_>>();
    if completed_writes.iter().any(|task| task.output.is_none()) {
        bail!("cannot recover accepted diff base: a completed Write has no persisted rebase tip");
    }
    let persisted_tip = match persisted_tips
        .iter()
        .copied()
        .collect::<Vec<_>>()
        .as_slice()
    {
        [tip] if *tip == candidate_commit => *tip,
        _ => {
            bail!(
                "cannot recover accepted diff base: completed Writes and Merge Candidate do not identify one persisted rebase tip"
            )
        }
    };
    let source_branches: HashSet<&str> = attempt
        .tasks
        .iter()
        .filter(|task| task.kind == TaskKind::Write && task.status == TaskStatus::Complete)
        .filter_map(|task| {
            task.output
                .as_ref()
                .map(|output| output.source_branch.as_str())
        })
        .collect();
    if source_branches.len() != 1 {
        bail!(
            "cannot recover accepted diff base: completed Writes do not identify one source branch"
        );
    }
    let source_branch = source_branches
        .into_iter()
        .next()
        .expect("one source branch checked");
    let output = git::run_raw(
        workspace_path,
        &["reflog", "show", "--format=%H%x09%gs", "HEAD"],
    )?;
    if !output.status.success() {
        bail!("cannot recover accepted diff base: candidate reflog is unavailable");
    }
    let reflog = String::from_utf8(output.stdout)
        .map_err(|_| anyhow::anyhow!("cannot recover accepted diff base: reflog is not UTF-8"))?;
    let (base, rebased_tip) =
        parse_exact_rebase_base(&reflog, &candidate_ref, source_branch, persisted_tip)?;

    let ensure_ancestor = |ancestor: &str, descendant: &str, description: &str| -> Result<()> {
        let ancestry = git::run_raw(
            workspace_path,
            &["merge-base", "--is-ancestor", ancestor, descendant],
        )?;
        if !ancestry.status.success() {
            bail!("cannot recover accepted diff base: {description}");
        }
        Ok(())
    };
    ensure_ancestor(
        &base,
        &rebased_tip,
        "rebase start is not an ancestor of its finish",
    )?;
    ensure_ancestor(
        &rebased_tip,
        merged_commit,
        "rebase finish is not an ancestor of merged commit",
    )?;
    ensure_ancestor(
        merged_commit,
        source_branch,
        "merged commit is not reachable from the recorded target branch",
    )?;
    Ok(base)
}

fn parse_exact_rebase_base(
    reflog: &str,
    candidate_ref: &str,
    source_branch: &str,
    persisted_tip: &str,
) -> Result<(String, String)> {
    let entries: Vec<(&str, &str)> = reflog
        .lines()
        .map(|line| {
            line.split_once('\t').ok_or_else(|| {
                anyhow::anyhow!("cannot recover accepted diff base: malformed reflog entry")
            })
        })
        .collect::<Result<_>>()?;
    let expected_finish = format!("rebase (finish): returning to {candidate_ref}");
    let expected_start = format!("rebase (start): checkout {source_branch}");
    let exact_finish_count = entries
        .iter()
        .filter(|(_, subject)| *subject == expected_finish)
        .count();
    if exact_finish_count > 1 {
        bail!("cannot recover accepted diff base: rebase provenance is ambiguous");
    }
    let mut matching_finishes = 0usize;
    let mut sessions = Vec::new();

    for (index, (finish_commit, subject)) in entries.iter().enumerate() {
        if *subject != expected_finish {
            continue;
        }
        matching_finishes += 1;
        if *finish_commit != persisted_tip {
            bail!(
                "cannot recover accepted diff base: rebase finish does not match persisted rebase tip"
            );
        }
        for (start_commit, older_subject) in entries.iter().skip(index + 1) {
            if older_subject.starts_with("rebase (finish):") {
                break;
            }
            if older_subject.starts_with("rebase (start):") {
                if *older_subject == expected_start {
                    sessions.push(((*start_commit).to_string(), (*finish_commit).to_string()));
                }
                break;
            }
            if !is_accepted_rebase_action(older_subject) {
                bail!(
                    "cannot recover accepted diff base: rebase session contains a structural gap"
                );
            }
        }
    }

    match sessions.as_slice() {
        [session] if matching_finishes == 1 => Ok(session.clone()),
        [] => bail!("cannot recover accepted diff base: exact rebase provenance is unavailable"),
        _ => bail!("cannot recover accepted diff base: rebase provenance is ambiguous"),
    }
}

fn is_accepted_rebase_action(subject: &str) -> bool {
    [
        "rebase (pick):",
        "rebase (squash):",
        "rebase (fixup):",
        "rebase (reword):",
        "rebase (edit):",
        "rebase (drop):",
    ]
    .iter()
    .any(|prefix| subject.starts_with(prefix))
}

/// The outcome of confining the Learner's commit: the expertise it was allowed
/// to record, and, for a denied post-land run, the paths it tried to change.
struct LearnerConfinement {
    /// Expertise files accepted into the Merge Candidate (empty when denied).
    expertise: Vec<crate::follow_up::ArtifactRef>,
    /// Paths a post-land handoff-only run tried to change but were discarded.
    denied_paths: Vec<String>,
}

/// A disposable repository and handoff surface for a post-land Learner.
///
/// Even when the caller requests `--no-sandbox`, hostile Git and checkout
/// writes land only in this temporary copy. The host imports the one managed
/// draft after the coder and confinement checks succeed.
struct HandoffOnlyWorkspace {
    _temp: tempfile::TempDir,
    workspace_path: PathBuf,
    handoff_dir: PathBuf,
    review_artifact_paths: Vec<PathBuf>,
    tester_artifact_paths: Vec<PathBuf>,
}

const MAX_HANDOFF_DRAFT_BYTES: u64 = 1024 * 1024;
const MAX_LEARNER_ARTIFACT_BYTES: u64 = 16 * 1024 * 1024;
const MAX_LEARNER_ARTIFACT_TOTAL_BYTES: u64 = 64 * 1024 * 1024;

impl HandoffOnlyWorkspace {
    fn create(
        project_root: &Path,
        work_item_id: &str,
        attempt_id: &str,
        baseline_commit: &str,
        review_artifact_paths: &[PathBuf],
        tester_artifact_paths: &[PathBuf],
    ) -> Result<Self> {
        let temp = tempfile::Builder::new()
            .prefix("fluent-handoff-only-")
            .tempdir()?;
        let workspace_path = temp.path().join("candidate");
        let bundle_path = temp.path().join("candidate.bundle");
        let bundle = bundle_path
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("isolated bundle path is not UTF-8"))?;
        let destination = workspace_path
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("isolated Learner path is not UTF-8"))?;
        git::run(
            project_root,
            &["bundle", "create", bundle, "--all"],
            "bundle isolated handoff-only workspace",
        )?;
        git::run(
            project_root,
            &[
                "-c",
                "core.logAllRefUpdates=false",
                "clone",
                "--quiet",
                "--no-hardlinks",
                bundle,
                destination,
            ],
            "clone isolated handoff-only workspace",
        )?;
        fs::remove_file(&bundle_path)?;
        git::run(
            &workspace_path,
            &["checkout", "--quiet", "--detach", baseline_commit],
            "check out merged commit in isolated handoff-only workspace",
        )?;
        git::run(
            &workspace_path,
            &["remote", "remove", "origin"],
            "remove live origin from isolated handoff-only workspace",
        )?;
        let reflogs = workspace_path.join(".git/logs");
        if reflogs.exists() {
            fs::remove_dir_all(reflogs)?;
        }
        let workspace_path = fs::canonicalize(workspace_path)?;

        let handoff_dir = temp
            .path()
            .join("project")
            .join(crate::learner::handoff_dir_rel(work_item_id, attempt_id));
        fs::create_dir_all(&handoff_dir)?;
        let handoff_dir = fs::canonicalize(handoff_dir)?;
        let review_artifact_paths =
            copy_learner_artifacts(temp.path(), project_root, "reviews", review_artifact_paths)?;
        let tester_artifact_paths =
            copy_learner_artifacts(temp.path(), project_root, "testers", tester_artifact_paths)?;
        Ok(Self {
            _temp: temp,
            workspace_path,
            handoff_dir,
            review_artifact_paths,
            tester_artifact_paths,
        })
    }

    fn publish_draft(
        &self,
        project_root: &Path,
        work_item_id: &str,
        attempt_id: &str,
    ) -> Result<()> {
        let source = self.handoff_dir.join(crate::learner::DRAFT_FILE_NAME);
        let bytes = read_confined_regular_file(
            &self.handoff_dir,
            &source,
            MAX_HANDOFF_DRAFT_BYTES,
            "handoff-only Learner draft",
        )?;
        let relative = crate::learner::draft_path_rel(work_item_id, attempt_id);
        atomic_write_confined(project_root, Path::new(&relative), &bytes)?;
        Ok(())
    }
}

/// Allocate a collision-safe, host-owned run directory for one Learner
/// invocation from on-disk state. It scans existing `run-<N>` siblings and
/// creates the next free index with an exclusive create, so a deleted or absent
/// in-memory Learning record can never reuse an earlier run identity, and two
/// concurrent retries never share one directory.
fn allocate_learner_run_dir(runs_dir: &Path) -> Result<PathBuf> {
    fs::create_dir_all(runs_dir)
        .with_context(|| format!("create Learner runs dir at {}", runs_dir.display()))?;
    let mut next = 1u64;
    for entry in fs::read_dir(runs_dir)
        .with_context(|| format!("scan Learner runs dir at {}", runs_dir.display()))?
    {
        if let Some(index) = entry?
            .file_name()
            .to_str()
            .and_then(|name| name.strip_prefix("run-"))
            .and_then(|digits| digits.parse::<u64>().ok())
        {
            next = next.max(index + 1);
        }
    }
    loop {
        let candidate = runs_dir.join(format!("run-{next}"));
        match fs::create_dir(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(error) if error.kind() == ErrorKind::AlreadyExists => next += 1,
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("allocate Learner run dir at {}", candidate.display())
                });
            }
        }
    }
}

/// Publish a pre-land Learner's staged draft to the canonical managed handoff
/// path. The staged draft is read through symlink/size confinement so the coder
/// cannot redirect the host to an arbitrary file. A Learner that wrote no draft
/// still succeeds: `read_draft` then yields an empty draft.
fn publish_pre_land_draft(
    project_root: &Path,
    work_item_id: &str,
    attempt_id: &str,
    staging_dir: &Path,
) -> Result<()> {
    let source = staging_dir.join(crate::learner::DRAFT_FILE_NAME);
    if !source.exists() {
        return Ok(());
    }
    let bytes = read_confined_regular_file(
        staging_dir,
        &source,
        MAX_HANDOFF_DRAFT_BYTES,
        "Learner draft",
    )?;
    let relative = crate::learner::draft_path_rel(work_item_id, attempt_id);
    atomic_write_confined(project_root, Path::new(&relative), &bytes)?;
    Ok(())
}

/// Snapshot the canonical published draft as immutable submitted-draft evidence
/// on the host-owned run surface the coder cannot reach. The exclusive create
/// means the record cannot be silently overwritten; a Learner that produced no
/// draft records nothing.
fn record_submitted_draft(
    project_root: &Path,
    work_item_id: &str,
    attempt_id: &str,
    run_dir: &Path,
) -> Result<()> {
    let canonical = project_root.join(crate::learner::draft_path_rel(work_item_id, attempt_id));
    let bytes = match fs::read(&canonical) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("read published draft at {}", canonical.display()));
        }
    };
    let evidence = run_dir.join("submitted-draft.json");
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&evidence)
        .with_context(|| format!("record submitted draft evidence at {}", evidence.display()))?;
    file.write_all(&bytes)
        .with_context(|| format!("write submitted draft evidence at {}", evidence.display()))?;
    Ok(())
}

/// Record every draft normalization on the host-owned run surface, so a
/// downgrade toward Observation-only is durable rather than silent. Writes
/// nothing when no normalization occurred.
fn record_run_normalizations(run_dir: &Path, normalizations: &[String]) -> Result<()> {
    if normalizations.is_empty() {
        return Ok(());
    }
    let path = run_dir.join("normalizations.txt");
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .with_context(|| format!("record run normalizations at {}", path.display()))?;
    file.write_all(format!("{}\n", normalizations.join("\n")).as_bytes())
        .with_context(|| format!("write run normalizations at {}", path.display()))?;
    Ok(())
}

/// Preserve a rejected draft's full validation error as an immutable run
/// artifact beside the already-recorded submitted bytes, so a later repair
/// cannot publish another draft without the rejection surviving as evidence.
fn record_run_rejection(run_dir: &Path, error: &anyhow::Error) -> Result<()> {
    let path = run_dir.join("error.txt");
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .with_context(|| format!("record run rejection at {}", path.display()))?;
    file.write_all(format!("{error:#}\n").as_bytes())
        .with_context(|| format!("write run rejection at {}", path.display()))?;
    Ok(())
}

fn copy_learner_artifacts(
    isolated_root: &Path,
    source_root: &Path,
    category: &str,
    sources: &[PathBuf],
) -> Result<Vec<PathBuf>> {
    let destination = isolated_root.join("artifacts").join(category);
    fs::create_dir_all(&destination)?;
    let mut total = 0u64;
    sources
        .iter()
        .enumerate()
        .map(|(index, source)| {
            let bytes = read_confined_regular_file(
                source_root,
                source,
                MAX_LEARNER_ARTIFACT_BYTES,
                &format!("Learner {category} artifact"),
            )?;
            total = total
                .checked_add(bytes.len() as u64)
                .ok_or_else(|| anyhow::anyhow!("Learner artifacts exceed the aggregate limit"))?;
            if total > MAX_LEARNER_ARTIFACT_TOTAL_BYTES {
                bail!("Learner artifacts exceed the aggregate limit");
            }
            let extension = source
                .extension()
                .and_then(|value| value.to_str())
                .map(|value| format!(".{value}"))
                .unwrap_or_default();
            let copied = destination.join(format!("{index:03}-artifact{extension}"));
            fs::write(&copied, bytes)?;
            Ok(copied)
        })
        .collect()
}

#[cfg(unix)]
fn read_confined_regular_file(
    root: &Path,
    path: &Path,
    max_bytes: u64,
    description: &str,
) -> Result<Vec<u8>> {
    use rustix::fs::{Mode, OFlags, openat};
    use std::fs::File;
    use std::os::unix::fs::MetadataExt;

    let canonical_root = fs::canonicalize(root)
        .with_context(|| format!("resolve confined root {}", root.display()))?;
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("{description} has no parent"))?;
    let canonical_parent = fs::canonicalize(parent)
        .with_context(|| format!("resolve {description} parent {}", parent.display()))?;
    if !canonical_parent.starts_with(&canonical_root) {
        bail!(
            "{description} escapes its confined root: {}",
            path.display()
        );
    }
    let name = path
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("{description} has no file name"))?;
    let parent_file = File::open(&canonical_parent)?;
    let before = fs::symlink_metadata(path).map_err(|error| {
        if error.kind() == ErrorKind::NotFound {
            anyhow::anyhow!("{description} did not produce a fresh draft")
        } else {
            anyhow::anyhow!("cannot inspect {description} {}: {error}", path.display())
        }
    })?;
    if !before.file_type().is_file() || before.nlink() != 1 {
        bail!(
            "{description} is not a regular file or has aliases: {}",
            path.display()
        );
    }
    if before.len() > max_bytes {
        bail!("{description} exceeds the {max_bytes}-byte limit");
    }
    let fd = openat(
        &parent_file,
        name,
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK,
        Mode::empty(),
    )?;
    let mut file = File::from(fd);
    let opened = file.metadata()?;
    if !opened.file_type().is_file()
        || opened.nlink() != 1
        || opened.dev() != before.dev()
        || opened.ino() != before.ino()
        || opened.len() != before.len()
    {
        bail!("{description} changed while it was opened");
    }
    let mut bytes = Vec::with_capacity(opened.len() as usize);
    Read::by_ref(&mut file)
        .take(max_bytes + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max_bytes {
        bail!("{description} exceeds the {max_bytes}-byte limit");
    }
    let after = file.metadata()?;
    if after.dev() != opened.dev() || after.ino() != opened.ino() || after.len() != opened.len() {
        bail!("{description} changed while it was read");
    }
    Ok(bytes)
}

#[cfg(unix)]
fn atomic_write_confined(root: &Path, relative: &Path, bytes: &[u8]) -> Result<()> {
    use rustix::fs::{AtFlags, Mode, OFlags, openat, renameat, unlinkat};
    use std::fs::File;
    use std::os::unix::fs::MetadataExt;
    use std::path::Component;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEMPORARY: AtomicU64 = AtomicU64::new(0);

    if relative.is_absolute() {
        bail!("confined target must be relative");
    }
    let canonical_root = fs::canonicalize(root)?;
    let relative_parent = relative
        .parent()
        .ok_or_else(|| anyhow::anyhow!("confined target has no parent"))?;
    let mut current = canonical_root.clone();
    for component in relative_parent.components() {
        let Component::Normal(component) = component else {
            bail!("confined target contains an invalid path component");
        };
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_dir() => {}
            Ok(_) => bail!("confined target ancestor is not a directory"),
            Err(error) if error.kind() == ErrorKind::NotFound => fs::create_dir(&current)?,
            Err(error) => return Err(error.into()),
        }
    }
    let canonical_parent = fs::canonicalize(&current)?;
    if !canonical_parent.starts_with(&canonical_root) {
        bail!("confined target ancestor escapes the project root");
    }
    let expected_parent = fs::metadata(&canonical_parent)?;
    let parent = File::open(&canonical_parent)?;
    let opened_parent = parent.metadata()?;
    if expected_parent.dev() != opened_parent.dev() || expected_parent.ino() != opened_parent.ino()
    {
        bail!("confined target ancestor changed while it was opened");
    }
    let temporary_name = format!(
        ".fluent-handoff-{}-{}",
        std::process::id(),
        NEXT_TEMPORARY.fetch_add(1, Ordering::Relaxed)
    );
    let temporary_fd = openat(
        &parent,
        temporary_name.as_str(),
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC,
        Mode::RUSR | Mode::WUSR,
    )?;
    let mut temporary = File::from(temporary_fd);
    let target_name = relative
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("confined target has no file name"))?;
    let write_result = (|| -> Result<()> {
        temporary.write_all(bytes)?;
        temporary.flush()?;
        temporary.sync_all()?;
        renameat(&parent, temporary_name.as_str(), &parent, target_name)?;
        parent.sync_all()?;
        Ok(())
    })();
    if write_result.is_err() {
        let _ = unlinkat(&parent, temporary_name.as_str(), AtFlags::empty());
    }
    write_result
}

/// Reduce a denied path to a filename-safe component for a synthesized follow-up
/// id.
fn sanitize_denied_path(path: &str) -> String {
    let mut out: String = path
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    if out.is_empty() {
        out.push_str("unnamed");
    }
    out
}

/// Confine the Learner's commit. In a post-land handoff-only run, any commit is
/// discarded and its changed paths are returned as denied, leaving the merged
/// commit and target branch untouched. Otherwise an expertise commit confined to
/// `.fluent/expertise/` is promoted to the candidate tip and its changed files
/// returned; an out-of-bounds commit is discarded and reported as an error,
/// retaining the pre-Learner candidate tip.
fn apply_learner_confinement(
    workspace_path: &Path,
    item: &mut WorkItem,
    attempt_index: usize,
    write_task_index: usize,
    write_output: &TaskOutput,
    candidate_id: &str,
    handoff_only: bool,
    baseline_commit: &str,
) -> Result<LearnerConfinement> {
    let new_head = git::run_stdout(
        workspace_path,
        &["rev-parse", "HEAD"],
        "resolve post-learner HEAD",
    )?;
    let committed_changed = git::run_stdout(
        workspace_path,
        &["diff", "--name-only", baseline_commit, &new_head],
        "list learner changed paths",
    )?;
    let mut changed_paths: Vec<String> = committed_changed
        .lines()
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect();

    // A post-land retry may not mutate expertise or the merged branch. Discard
    // committed, staged, unstaged, and untracked changes and report their paths
    // so they become non-corrective follow-ups. Resetting and cleaning restores
    // both the candidate branch and its index before accepting the handoff.
    if handoff_only {
        for args in [
            vec!["diff", "--name-only"],
            vec!["diff", "--cached", "--name-only"],
            vec!["ls-files", "--others", "--exclude-standard"],
        ] {
            let changed = git::run_stdout(workspace_path, &args, "list denied learner changes")?;
            changed_paths.extend(
                changed
                    .lines()
                    .filter(|line| !line.is_empty())
                    .map(str::to_string),
            );
        }
        changed_paths.sort();
        changed_paths.dedup();
        git::run(
            workspace_path,
            &["reset", "--hard", baseline_commit],
            "discard post-land learner commit",
        )?;
        git::run(
            workspace_path,
            &["clean", "-fd"],
            "discard untracked post-land learner changes",
        )?;
        let restored_head = git::run_stdout(
            workspace_path,
            &["rev-parse", "HEAD"],
            "verify restored candidate HEAD",
        )?;
        let restored_status = git::run_stdout(
            workspace_path,
            &["status", "--porcelain", "--untracked-files=all"],
            "verify restored candidate index and worktree",
        )?;
        if restored_head != baseline_commit || !restored_status.is_empty() {
            bail!("handoff-only Learner candidate Git state could not be restored");
        }
        return Ok(LearnerConfinement {
            expertise: Vec::new(),
            denied_paths: changed_paths,
        });
    }

    if new_head == baseline_commit {
        return Ok(LearnerConfinement {
            expertise: Vec::new(),
            denied_paths: Vec::new(),
        });
    }

    let out_of_bounds: Vec<&str> = changed_paths
        .iter()
        .map(String::as_str)
        .filter(|path| !is_learner_path_in_bounds(path))
        .collect();
    if !out_of_bounds.is_empty() {
        for path in &out_of_bounds {
            eprintln!("  Warning: learner changed out-of-bounds path: {path}");
        }
        git::run(
            workspace_path,
            &["reset", "--hard", &write_output.commit],
            "discard out-of-bounds learner commit",
        )?;
        bail!("learner commit changed paths outside .fluent/expertise/");
    }

    // Confined expertise commit becomes the Merge Candidate's candidate commit.
    item.attempts[attempt_index].tasks[write_task_index].output = Some(TaskOutput {
        commit: new_head.clone(),
        ..write_output.clone()
    });
    if let Some(candidate) = item
        .merge_candidates
        .iter_mut()
        .find(|candidate| candidate.id == candidate_id)
    {
        candidate.candidate_commit = new_head.clone();
    }

    let expertise = changed_paths
        .iter()
        .map(|path| {
            let digest = git::run_stdout(
                workspace_path,
                &["rev-parse", &format!("{new_head}:{path}")],
                "digest expertise blob",
            )
            .unwrap_or_default();
            crate::follow_up::ArtifactRef {
                path: path.to_string(),
                digest: format!("git:{}", digest.trim()),
            }
        })
        .collect();
    Ok(LearnerConfinement {
        expertise,
        denied_paths: Vec::new(),
    })
}

fn is_learner_path_in_bounds(path: &str) -> bool {
    path.starts_with(".fluent/expertise/")
}

fn all_tester_artifact_paths(
    project_root: &Path,
    attempt: &crate::work_model::Attempt,
) -> Result<Vec<PathBuf>> {
    attempt
        .tasks
        .iter()
        .filter(|task| task.kind == TaskKind::Tester && task.status == TaskStatus::Complete)
        .map(|task| {
            let area = task.artifact_area.as_ref().ok_or_else(|| {
                anyhow::anyhow!("completed Tester {:?} has no artifact area", task.id)
            })?;
            Ok(
                work_task_executor::resolve_managed_artifact_area_path(project_root, &area.path)?
                    .join("tester-results.json"),
            )
        })
        .collect()
}

fn completed_review_tasks_after_latest_write(tasks: &[Task]) -> impl Iterator<Item = &Task> {
    let start = tasks
        .iter()
        .rposition(|task| task.kind == TaskKind::Write)
        .map(|index| index + 1)
        .unwrap_or(0);
    tasks[start..]
        .iter()
        .filter(|task| task.kind == TaskKind::Review && task.status == TaskStatus::Complete)
}

fn tester_failures(tester_results_path: &Path) -> usize {
    let content = match fs::read_to_string(tester_results_path) {
        Ok(c) => c,
        Err(_) => return 0,
    };
    let results: crate::tester::TesterResults = match serde_json::from_str(&content) {
        Ok(r) => r,
        Err(_) => return 0,
    };
    results.tests.iter().filter(|t| t.status == "fail").count()
}

fn failing_ids(path: &Path) -> HashSet<String> {
    let content = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return HashSet::new(),
    };
    let results: crate::tester::TesterResults = match serde_json::from_str(&content) {
        Ok(r) => r,
        Err(_) => return HashSet::new(),
    };
    results
        .tests
        .iter()
        .filter(|t| t.status == "fail")
        .map(|t| t.id.clone())
        .collect()
}

fn introduced_tester_failures(current_path: &Path, baseline_path: Option<&Path>) -> usize {
    match baseline_path {
        Some(bp) => {
            let current = failing_ids(current_path);
            let baseline = failing_ids(bp);
            current.difference(&baseline).count()
        }
        None => tester_failures(current_path),
    }
}

fn tester_errored(tester_results_path: &Path) -> bool {
    let content = match fs::read_to_string(tester_results_path) {
        Ok(c) => c,
        Err(_) => return false,
    };
    let results: crate::tester::TesterResults = match serde_json::from_str(&content) {
        Ok(r) => r,
        Err(_) => return false,
    };
    results.error.is_some()
}

fn baseline_tester_results_path(project_root: &Path, attempt: &Attempt) -> Option<PathBuf> {
    let work_item_id = &attempt.work_item_id;
    let attempt_id = &attempt.id;
    let baseline_artifact = format!("{attempt_id}-baseline-tester");
    let artifact_path = work_artifact_path(work_item_id, attempt_id, &baseline_artifact);
    let path = project_root
        .join(&artifact_path)
        .join("tester-results.json");
    if path.is_file() { Some(path) } else { None }
}

fn latest_tester_results_path(project_root: &Path, attempt: &Attempt) -> Option<PathBuf> {
    let start = attempt
        .tasks
        .iter()
        .rposition(|task| task.kind == TaskKind::Write)
        .map(|index| index + 1)
        .unwrap_or(0);
    attempt.tasks[start..]
        .iter()
        .rev()
        .find(|task| task.kind == TaskKind::Tester && task.status == TaskStatus::Complete)
        .and_then(|task| task.artifact_area.as_ref())
        .and_then(|area| {
            work_task_executor::resolve_managed_artifact_area_path(project_root, &area.path).ok()
        })
        .map(|dir| dir.join("tester-results.json"))
}

fn interpret_reviews(
    project_root: &Path,
    store: &WorkModelStore,
    mut item: WorkItem,
    attempt_id: &str,
    followup_budget_available: bool,
    learner_config: Option<LearnerConfig<'_>>,
) -> Result<WorkAttemptRunOutcome> {
    let attempt_index = item
        .attempts
        .iter()
        .position(|attempt| attempt.id == attempt_id)
        .ok_or_else(|| anyhow::anyhow!("Attempt {:?} not found", attempt_id))?;

    let review_artifacts = latest_review_artifacts(project_root, &item.attempts[attempt_index])?;
    if review_artifacts.is_empty() {
        bail!("Attempt {:?} has no completed review artifacts", attempt_id);
    }

    let mut failed = Vec::new();
    let mut uncertain = Vec::new();
    for review_artifact in &review_artifacts {
        let content = fs::read_to_string(&review_artifact.review_path).unwrap_or_default();
        match review::extract_verdict(&content) {
            Verdict::Pass => {}
            Verdict::Fail => failed.push(review_artifact.artifact.clone()),
            Verdict::Uncertain => uncertain.push(review_artifact.artifact.clone()),
        }
    }

    let tester_result_path =
        latest_tester_results_path(project_root, &item.attempts[attempt_index]);
    let baseline_path = baseline_tester_results_path(project_root, &item.attempts[attempt_index]);
    let tester_fail_count = tester_result_path
        .as_ref()
        .map(|p| introduced_tester_failures(p, baseline_path.as_deref()))
        .unwrap_or(0);
    let tester_has_error = tester_result_path
        .as_ref()
        .map(|p| tester_errored(p))
        .unwrap_or(false);

    let has_failures = !failed.is_empty() || tester_fail_count > 0 || tester_has_error;

    if has_failures {
        if tester_fail_count > 0 || tester_has_error {
            if let Some(ref path) = tester_result_path {
                if let Ok(relative) = path.strip_prefix(project_root) {
                    failed.push(ArtifactRef {
                        producer_id: "tester".to_string(),
                        path: relative.to_string_lossy().to_string(),
                    });
                }
            }
        }

        item.attempts[attempt_index].review_state = Some(AttemptReviewState::Failed);
        if item.attempts[attempt_index].kind.is_review_only_like() {
            crate::work_model::set_attempt_terminal(
                &mut item.attempts[attempt_index],
                AttemptStatus::Failed,
            );
            store.write_work_item(&item)?;
            return Ok(WorkAttemptRunOutcome::ReviewOnlyFailed);
        }
        if !followup_budget_available {
            let handoff_path =
                write_budget_exhausted_handoff(project_root, &item.id, attempt_id, &failed)?;
            crate::work_model::suspend_attempt(
                &mut item.attempts[attempt_index],
                crate::work_model::PauseKind::RoundCap,
            );
            item.attempts[attempt_index].artifacts.push(ArtifactRef {
                producer_id: "attempt-loop".to_string(),
                path: handoff_path.clone(),
            });
            store.write_work_item(&item)?;
            return Ok(WorkAttemptRunOutcome::NeedsUser { handoff_path });
        }
        item.attempts[attempt_index].status = AttemptStatus::Planned;
        let task_id = item.add_next_write_round(attempt_id, failed)?;
        store.write_work_item(&item)?;
        return Ok(WorkAttemptRunOutcome::PlannedWriteRound { task_id });
    }

    if !uncertain.is_empty() {
        let handoff_path =
            write_needs_user_handoff(project_root, &item.id, attempt_id, &uncertain)?;
        item.attempts[attempt_index].review_state = Some(AttemptReviewState::Uncertain);
        crate::work_model::suspend_attempt(
            &mut item.attempts[attempt_index],
            crate::work_model::PauseKind::Uncertain,
        );
        item.attempts[attempt_index].artifacts.push(ArtifactRef {
            producer_id: "attempt-loop".to_string(),
            path: handoff_path.clone(),
        });
        store.write_work_item(&item)?;
        return Ok(WorkAttemptRunOutcome::NeedsUser { handoff_path });
    }

    item.attempts[attempt_index].review_state = Some(AttemptReviewState::Passed);
    crate::work_model::set_attempt_terminal(
        &mut item.attempts[attempt_index],
        AttemptStatus::Complete,
    );
    if item.attempts[attempt_index].kind.is_review_only_like() {
        store.write_work_item(&item)?;
        return Ok(WorkAttemptRunOutcome::ReviewOnlyComplete);
    }
    // Allocate the Merge Candidate identity before the Learner runs, so the
    // handoff can reference it and a confined expertise commit can update its
    // tip.
    let candidate_id = item.create_or_get_merge_candidate(attempt_id)?;
    // Run the Learner for every code-producing Attempt, whether or not a
    // reviewer raised a finding.
    if let Some(ref learner_config) = learner_config {
        // The Learner runs before land here, so it is never handoff-only.
        run_learner_step(
            project_root,
            &mut item,
            attempt_index,
            &candidate_id,
            false,
            learner_config,
        );
    }
    store.write_work_item(&item)?;
    Ok(WorkAttemptRunOutcome::MergeCandidateReady { candidate_id })
}

#[derive(Debug)]
struct ReviewArtifact {
    artifact: ArtifactRef,
    review_path: PathBuf,
}

fn latest_review_artifacts(
    project_root: &Path,
    attempt: &crate::work_model::Attempt,
) -> Result<Vec<ReviewArtifact>> {
    let start = if attempt.kind.is_review_only_like() {
        0
    } else {
        let Some(last_write_index) = attempt
            .tasks
            .iter()
            .rposition(|task| task.kind == TaskKind::Write)
        else {
            return Ok(Vec::new());
        };
        last_write_index + 1
    };
    attempt.tasks[start..]
        .iter()
        .filter(|task| task.kind == TaskKind::Review && task.status == TaskStatus::Complete)
        .filter_map(|task| task.artifact_area.as_ref().map(|area| (task, area)))
        .map(|(task, area)| {
            let artifact_dir =
                work_task_executor::resolve_managed_artifact_area_path(project_root, &area.path)?;
            Ok(ReviewArtifact {
                artifact: ArtifactRef {
                    producer_id: task.id.clone(),
                    path: format!("{}/review.md", area.path),
                },
                review_path: artifact_dir.join("review.md"),
            })
        })
        .collect()
}

/// Relative paths of the latest review round's completed review artifacts, for
/// next-action guidance hints. Empty when no completed reviews exist.
pub fn latest_review_artifact_relpaths(
    project_root: &Path,
    attempt: &crate::work_model::Attempt,
) -> Vec<String> {
    latest_review_artifacts(project_root, attempt)
        .map(|artifacts| {
            artifacts
                .into_iter()
                .map(|artifact| artifact.artifact.path)
                .collect()
        })
        .unwrap_or_default()
}

fn all_review_artifact_paths(
    project_root: &Path,
    attempt: &crate::work_model::Attempt,
) -> Result<Vec<PathBuf>> {
    attempt
        .tasks
        .iter()
        .filter(|task| task.kind == TaskKind::Review && task.status == TaskStatus::Complete)
        .map(|task| {
            let area = task.artifact_area.as_ref().ok_or_else(|| {
                anyhow::anyhow!("completed review {:?} has no artifact area", task.id)
            })?;
            Ok(
                work_task_executor::resolve_managed_artifact_area_path(project_root, &area.path)?
                    .join("review.md"),
            )
        })
        .collect()
}

fn write_needs_user_handoff(
    project_root: &Path,
    work_item_id: &str,
    attempt_id: &str,
    uncertain: &[ArtifactRef],
) -> Result<String> {
    let relative_path = work_artifact_path(work_item_id, attempt_id, "needs-user.md");
    let path = project_root.join(&relative_path);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let artifacts = uncertain
        .iter()
        .map(|artifact| format!("- {}", artifact.path))
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(
        &path,
        format!(
            "# Attempt needs user input\n\nThe Attempt loop found uncertain or missing review verdicts.\n\n{artifacts}\n"
        ),
    )?;
    Ok(relative_path)
}

fn write_budget_exhausted_handoff(
    project_root: &Path,
    work_item_id: &str,
    attempt_id: &str,
    failed: &[ArtifactRef],
) -> Result<String> {
    let relative_path = work_artifact_path(work_item_id, attempt_id, "needs-user.md");
    let path = project_root.join(&relative_path);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let artifacts = failed
        .iter()
        .map(|artifact| format!("- {}", artifact.path))
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(
        &path,
        format!(
            "# Attempt needs user input\n\nThe Attempt loop stopped advancing: reviewers reported `Progress: no` for {} consecutive rounds, or the total write-round ceiling of {} was reached.\n\nFailed review artifacts still need attention:\n\n{artifacts}\n",
            max_no_progress_rounds(),
            max_total_write_rounds()
        ),
    )?;
    Ok(relative_path)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content::ContentResolver;
    use crate::work_model::AttemptKind;
    use crate::work_model::WorkItemAbandonment;
    use crate::work_model::{Attempt, CoderMapping, TaskArtifactArea, WorkspaceAccess};

    /// Acquire a Task lease, retrying to absorb macOS flock release-visibility
    /// latency under the thread-per-test harness.
    fn acquire_task_lease_eventually(
        project_root: &Path,
        work_item_id: &str,
        task_id: &str,
    ) -> crate::lease::TaskLease {
        let lock = crate::lease::task_lock_path(project_root, work_item_id, task_id);
        for _ in 0..50 {
            if let Ok(lease) = crate::lease::acquire(&lock) {
                return lease;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("could not acquire task lease");
    }

    #[test]
    fn task_recovery_does_not_consult_transcript_age() {
        // Liveness is decided solely by the process-held lease. A held lease
        // keeps a Task live even with no transcript at all, and a released lease
        // makes it stale even when a transcript was just written — recovery
        // never consults transcript age.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let work = "work-1";
        let task = "attempt-1-write-1";

        // A leased Task is live even though no transcript or pump status exists.
        let lease = acquire_task_lease_eventually(root, work, task);
        assert!(
            executing_task_is_live(root, work, task),
            "a leased Task is live regardless of transcript presence or age"
        );
        drop(lease);

        // Write a brand-new transcript and pump status for the same Task. If
        // recovery consulted transcript age, this fresh evidence would keep the
        // Task live; it must not.
        let artifact_dir = root
            .join(".fluent/work/artifacts")
            .join(work)
            .join("attempt-1")
            .join(task);
        fs::create_dir_all(&artifact_dir).unwrap();
        fs::write(artifact_dir.join("transcript.jsonl"), b"{\"type\":\"fresh\"}\n").unwrap();
        fs::write(
            artifact_dir.join("transcript-pump.json"),
            b"{\"schema_version\":1,\"state\":\"running\"}",
        )
        .unwrap();

        // Freed direction: bounded retry to absorb flock release-visibility.
        let mut stale = false;
        for _ in 0..50 {
            if !executing_task_is_live(root, work, task) {
                stale = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(
            stale,
            "a released lease makes the Task stale even with a freshly written transcript"
        );
    }

    fn walk_files(root: &Path) -> Vec<PathBuf> {
        let mut pending = vec![root.to_path_buf()];
        let mut files = Vec::new();
        while let Some(path) = pending.pop() {
            for entry in fs::read_dir(path).unwrap() {
                let entry = entry.unwrap();
                if entry.file_type().unwrap().is_dir() {
                    pending.push(entry.path());
                } else {
                    files.push(entry.path());
                }
            }
        }
        files
    }

    #[test]
    fn exact_legacy_rebase_base_accepts_post_rebase_fix() {
        let reflog = concat!(
            "merged\tcommit: Apply accepted merge fix\n",
            "rebased\trebase (finish): returning to refs/heads/work/candidate\n",
            "rebased\trebase (pick): Add accepted change\n",
            "base\trebase (start): checkout main\n",
        );

        assert_eq!(
            parse_exact_rebase_base(reflog, "refs/heads/work/candidate", "main", "rebased",)
                .unwrap(),
            ("base".to_string(), "rebased".to_string())
        );
    }

    #[test]
    fn exact_legacy_rebase_base_rejects_multiple_sessions() {
        let reflog = concat!(
            "tip-2\trebase (finish): returning to refs/heads/work/candidate\n",
            "base-2\trebase (start): checkout main\n",
            "tip-1\trebase (finish): returning to refs/heads/work/candidate\n",
            "base-1\trebase (start): checkout main\n",
        );

        let error = parse_exact_rebase_base(reflog, "refs/heads/work/candidate", "main", "tip-2")
            .unwrap_err();

        assert!(error.to_string().contains("ambiguous"));
    }

    #[test]
    fn exact_legacy_rebase_base_rejects_partial_session() {
        let reflog = concat!(
            "tip\trebase (finish): returning to refs/heads/work/candidate\n",
            "picked\trebase (pick): Add accepted change\n",
        );

        let error = parse_exact_rebase_base(reflog, "refs/heads/work/candidate", "main", "tip")
            .unwrap_err();

        assert!(error.to_string().contains("exact rebase provenance"));
    }

    #[test]
    fn exact_legacy_rebase_base_rejects_a_spliced_session() {
        let reflog = concat!(
            "tip\trebase (finish): returning to refs/heads/work/candidate\n",
            "other\tcommit: unrelated history\n",
            "base\trebase (start): checkout main\n",
        );

        let error = parse_exact_rebase_base(reflog, "refs/heads/work/candidate", "main", "tip")
            .unwrap_err();

        assert!(error.to_string().contains("structural gap"));
    }

    #[test]
    fn exact_legacy_rebase_base_rejects_a_different_persisted_tip() {
        let reflog = concat!(
            "substitute\trebase (finish): returning to refs/heads/work/candidate\n",
            "base\trebase (start): checkout main\n",
        );

        let error =
            parse_exact_rebase_base(reflog, "refs/heads/work/candidate", "main", "persisted-tip")
                .unwrap_err();

        assert!(error.to_string().contains("persisted rebase tip"));
    }

    #[test]
    fn exact_legacy_rebase_base_accepts_supported_rewritten_sessions() {
        for middle in [
            "tip\trebase (squash): Combine accepted changes\n",
            "tip\trebase (reword): Clarify accepted change\n",
            "tip\trebase (pick): Keep accepted change\n",
        ] {
            let reflog = format!(
                "tip\trebase (finish): returning to refs/heads/work/candidate\n{middle}base\trebase (start): checkout main\n"
            );
            assert_eq!(
                parse_exact_rebase_base(&reflog, "refs/heads/work/candidate", "main", "tip",)
                    .unwrap(),
                ("base".to_string(), "tip".to_string())
            );
        }
    }

    #[test]
    fn exact_legacy_rebase_base_rejects_unknown_rebase_actions() {
        let reflog = concat!(
            "tip\trebase (finish): returning to refs/heads/work/candidate\n",
            "tip\trebase (mystery): Rewrite accepted change\n",
            "base\trebase (start): checkout main\n",
        );

        let error = parse_exact_rebase_base(reflog, "refs/heads/work/candidate", "main", "tip")
            .unwrap_err();

        assert!(error.to_string().contains("structural gap"));
    }

    #[test]
    fn exact_legacy_rebase_base_rejects_expired_reflog() {
        let error =
            parse_exact_rebase_base("", "refs/heads/work/candidate", "main", "tip").unwrap_err();

        assert!(error.to_string().contains("exact rebase provenance"));
    }

    #[cfg(unix)]
    #[test]
    fn handoff_only_draft_import_rejects_symlink() {
        use std::os::unix::fs::symlink;

        let isolated_root = tempfile::TempDir::new().unwrap();
        let handoff_dir = isolated_root.path().join("handoff");
        fs::create_dir_all(&handoff_dir).unwrap();
        let outside = isolated_root.path().join("outside.json");
        fs::write(
            &outside,
            r#"{"learning_summary":"escaped","follow_ups":[]}"#,
        )
        .unwrap();
        symlink(&outside, handoff_dir.join(crate::learner::DRAFT_FILE_NAME)).unwrap();
        let isolated = HandoffOnlyWorkspace {
            workspace_path: isolated_root.path().join("candidate"),
            handoff_dir,
            review_artifact_paths: Vec::new(),
            tester_artifact_paths: Vec::new(),
            _temp: isolated_root,
        };
        let project = tempfile::TempDir::new().unwrap();

        let error = isolated
            .publish_draft(project.path(), "work-1", "attempt-1")
            .unwrap_err();

        assert!(error.to_string().contains("not a regular file"));
        assert!(
            !project
                .path()
                .join(crate::learner::draft_path_rel("work-1", "attempt-1"))
                .exists()
        );
    }

    #[test]
    fn allocate_learner_run_dir_never_reuses_an_identity_from_disk() {
        let runs = tempfile::TempDir::new().unwrap();
        // Existing on-disk run identities, including a gap, are never reused.
        fs::create_dir(runs.path().join("run-1")).unwrap();
        fs::create_dir(runs.path().join("run-3")).unwrap();
        fs::create_dir(runs.path().join("not-a-run")).unwrap();

        let allocated = allocate_learner_run_dir(runs.path()).unwrap();
        assert_eq!(
            allocated.file_name().unwrap().to_str().unwrap(),
            "run-4",
            "allocation advances past the highest existing on-disk index"
        );

        // A second allocation, even with the in-memory index forgotten, cannot
        // collide with the first.
        let second = allocate_learner_run_dir(runs.path()).unwrap();
        assert_eq!(second.file_name().unwrap().to_str().unwrap(), "run-5");
        assert_ne!(allocated, second);

        // Deleting the newest directory does not let a later allocation reclaim
        // the identity of a directory that already existed.
        fs::create_dir(runs.path().join("run-9")).unwrap();
        let third = allocate_learner_run_dir(runs.path()).unwrap();
        assert_eq!(third.file_name().unwrap().to_str().unwrap(), "run-10");
    }

    #[test]
    fn handoff_only_draft_import_requires_a_fresh_draft() {
        let isolated_root = tempfile::TempDir::new().unwrap();
        let handoff_dir = isolated_root.path().join("handoff");
        fs::create_dir_all(&handoff_dir).unwrap();
        let isolated = HandoffOnlyWorkspace {
            workspace_path: isolated_root.path().join("candidate"),
            handoff_dir,
            review_artifact_paths: Vec::new(),
            tester_artifact_paths: Vec::new(),
            _temp: isolated_root,
        };
        let project = tempfile::TempDir::new().unwrap();
        let stale = project
            .path()
            .join(crate::learner::draft_path_rel("work-1", "attempt-1"));
        fs::create_dir_all(stale.parent().unwrap()).unwrap();
        fs::write(&stale, r#"{"learning_summary":"stale","follow_ups":[]}"#).unwrap();

        let error = isolated
            .publish_draft(project.path(), "work-1", "attempt-1")
            .unwrap_err();

        assert!(error.to_string().contains("did not produce a fresh draft"));
        assert_eq!(
            fs::read_to_string(stale).unwrap(),
            r#"{"learning_summary":"stale","follow_ups":[]}"#
        );
    }

    #[cfg(unix)]
    #[test]
    fn handoff_only_draft_import_rejects_hardlinks_and_oversized_files() {
        let isolated_root = tempfile::TempDir::new().unwrap();
        let handoff_dir = isolated_root.path().join("handoff");
        fs::create_dir_all(&handoff_dir).unwrap();
        let draft = handoff_dir.join(crate::learner::DRAFT_FILE_NAME);
        fs::write(&draft, "{}\n").unwrap();
        fs::hard_link(&draft, handoff_dir.join("alias.json")).unwrap();
        let isolated = HandoffOnlyWorkspace {
            workspace_path: isolated_root.path().join("candidate"),
            handoff_dir: handoff_dir.clone(),
            review_artifact_paths: Vec::new(),
            tester_artifact_paths: Vec::new(),
            _temp: isolated_root,
        };
        let project = tempfile::TempDir::new().unwrap();

        let error = isolated
            .publish_draft(project.path(), "work-1", "attempt-1")
            .unwrap_err();
        assert!(error.to_string().contains("aliases"));

        fs::remove_file(handoff_dir.join("alias.json")).unwrap();
        fs::OpenOptions::new()
            .write(true)
            .open(&draft)
            .unwrap()
            .set_len(MAX_HANDOFF_DRAFT_BYTES + 1)
            .unwrap();
        let error = isolated
            .publish_draft(project.path(), "work-1", "attempt-1")
            .unwrap_err();
        assert!(error.to_string().contains("exceeds"));
    }

    #[cfg(unix)]
    #[test]
    fn handoff_only_draft_import_rejects_a_symlinked_target_ancestor() {
        use std::os::unix::fs::symlink;

        let isolated_root = tempfile::TempDir::new().unwrap();
        let handoff_dir = isolated_root.path().join("handoff");
        fs::create_dir_all(&handoff_dir).unwrap();
        fs::write(
            handoff_dir.join(crate::learner::DRAFT_FILE_NAME),
            r#"{"learning_summary":"safe","follow_ups":[]}"#,
        )
        .unwrap();
        let isolated = HandoffOnlyWorkspace {
            workspace_path: isolated_root.path().join("candidate"),
            handoff_dir,
            review_artifact_paths: Vec::new(),
            tester_artifact_paths: Vec::new(),
            _temp: isolated_root,
        };
        let project = tempfile::TempDir::new().unwrap();
        let outside = tempfile::TempDir::new().unwrap();
        symlink(outside.path(), project.path().join(".fluent")).unwrap();

        let error = isolated
            .publish_draft(project.path(), "work-1", "attempt-1")
            .unwrap_err();

        assert!(error.to_string().contains("ancestor"));
        assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 0);
    }

    #[test]
    fn handoff_only_artifacts_are_copied_under_the_isolated_root() {
        let live = tempfile::TempDir::new().unwrap();
        let review = live.path().join("review.md");
        let tester = live.path().join("tester-results.json");
        fs::write(&review, "review sentinel\n").unwrap();
        fs::write(&tester, "tester sentinel\n").unwrap();
        let isolated = tempfile::TempDir::new().unwrap();

        let reviews =
            copy_learner_artifacts(isolated.path(), live.path(), "reviews", &[review]).unwrap();
        let testers =
            copy_learner_artifacts(isolated.path(), live.path(), "testers", &[tester]).unwrap();

        assert_eq!(
            fs::read_to_string(&reviews[0]).unwrap(),
            "review sentinel\n"
        );
        assert_eq!(
            fs::read_to_string(&testers[0]).unwrap(),
            "tester sentinel\n"
        );
        assert!(reviews[0].starts_with(isolated.path()));
        assert!(testers[0].starts_with(isolated.path()));
        assert!(!reviews[0].starts_with(live.path()));
        assert!(!testers[0].starts_with(live.path()));
    }

    #[cfg(unix)]
    #[test]
    fn handoff_only_artifacts_reject_missing_hardlinked_and_escaped_inputs() {
        use std::os::unix::fs::symlink;

        let live = tempfile::TempDir::new().unwrap();
        let isolated = tempfile::TempDir::new().unwrap();
        let missing = live.path().join("missing.md");
        assert!(
            copy_learner_artifacts(isolated.path(), live.path(), "reviews", &[missing],).is_err()
        );

        let hardlinked = live.path().join("review.md");
        fs::write(&hardlinked, "review\n").unwrap();
        fs::hard_link(&hardlinked, live.path().join("review-alias.md")).unwrap();
        let error = copy_learner_artifacts(isolated.path(), live.path(), "reviews", &[hardlinked])
            .unwrap_err();
        assert!(error.to_string().contains("aliases"));

        let outside = tempfile::TempDir::new().unwrap();
        fs::write(outside.path().join("escaped.md"), "escaped\n").unwrap();
        symlink(outside.path(), live.path().join("artifact-alias")).unwrap();
        let escaped = live.path().join("artifact-alias/escaped.md");
        let error = copy_learner_artifacts(isolated.path(), live.path(), "reviews", &[escaped])
            .unwrap_err();
        assert!(error.to_string().contains("escapes"));
    }

    #[test]
    fn completed_missing_artifacts_reach_the_fail_closed_copy_boundary() {
        let project = tempfile::TempDir::new().unwrap();
        let isolated = tempfile::TempDir::new().unwrap();
        let artifact_area = work_artifact_path("work-1", "attempt-1", "attempt-1-review-1-tests");
        let attempt = attempt_with_tasks(vec![review_task_with_artifact(
            "attempt-1-review-1-tests",
            "tests",
            &artifact_area,
        )]);

        let declared = all_review_artifact_paths(project.path(), &attempt).unwrap();
        assert_eq!(declared.len(), 1);
        assert!(!declared[0].exists());
        let error = copy_learner_artifacts(isolated.path(), project.path(), "reviews", &declared)
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("resolve Learner reviews artifact parent")
        );
    }

    #[test]
    fn handoff_only_git_metadata_does_not_disclose_the_live_repository() {
        let tmp = tempfile::TempDir::new().unwrap();
        let project = tmp.path().join("live-project-sentinel");
        fs::create_dir_all(&project).unwrap();
        init_learner_repo(&project);
        fs::write(project.join("tracked.txt"), "tracked\n").unwrap();
        git::run(&project, &["add", "."], "add fixture").unwrap();
        git::run(&project, &["commit", "-m", "Add fixture"], "commit fixture").unwrap();
        let baseline =
            git::run_stdout(&project, &["rev-parse", "HEAD"], "resolve baseline").unwrap();

        let isolated =
            HandoffOnlyWorkspace::create(&project, "work-1", "attempt-1", &baseline, &[], &[])
                .unwrap();
        let live = project.to_string_lossy();
        let git_dir = isolated.workspace_path.join(".git");
        for entry in walk_files(&git_dir) {
            let bytes = fs::read(&entry).unwrap();
            assert!(
                !String::from_utf8_lossy(&bytes).contains(live.as_ref()),
                "Git metadata {} disclosed the live repository",
                entry.display()
            );
        }
    }

    #[test]
    fn run_attempt_rejects_abandoned_work_item_without_mutating_state() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = WorkModelStore::new(tmp.path());
        let mut item = WorkItem {
            id: "work-1".to_string(),
            title: "Keep abandoned attempt terminal".to_string(),
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

        let error = match run_attempt(WorkAttemptRunConfig {
            project_root: tmp.path(),
            store: &store,
            work_item_id: "work-1",
            attempt_id: "attempt-1",
            resolver: &resolver,
            extra_args: &[],
            no_sandbox: true,
            resolved_coder_mapping: None,
        }) {
            Ok(_) => panic!("abandoned Work Item should reject attempt run"),
            Err(error) => error,
        };

        assert!(error.to_string().contains("is abandoned"));
        let stored = store.read_work_item("work-1").unwrap();
        assert!(stored.abandonment.is_some());
        assert_eq!(stored.attempts[0].status, AttemptStatus::Planned);
        assert_eq!(stored.attempts[0].tasks[0].status, TaskStatus::Planned);
    }

    #[test]
    fn completed_review_round_is_not_open() {
        let tasks = vec![
            Task {
                id: "attempt-1-write-1".to_string(),
                kind: TaskKind::Write,
                status: TaskStatus::Complete,
                role: "author".to_string(),
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
            },
            Task {
                id: "attempt-1-review-tests".to_string(),
                kind: TaskKind::Review,
                status: TaskStatus::Complete,
                role: "tests".to_string(),
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
            },
        ];

        assert!(!has_open_review_round(&tasks));
    }

    #[test]
    fn latest_review_artifacts_rejects_unmanaged_artifact_area() {
        let tasks = vec![
            Task {
                id: "attempt-1-write-1".to_string(),
                kind: TaskKind::Write,
                status: TaskStatus::Complete,
                role: "author".to_string(),
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
            },
            Task {
                id: "attempt-1-review-tests".to_string(),
                kind: TaskKind::Review,
                status: TaskStatus::Complete,
                role: "tests".to_string(),
                instructions: None,
                work_item_id: "work-1".to_string(),
                attempt_id: Some("attempt-1".to_string()),
                workspace_access: WorkspaceAccess {
                    reads: Vec::new(),
                    writes: Vec::new(),
                },
                artifact_area: Some(TaskArtifactArea {
                    path: "../outside-review-artifacts".to_string(),
                }),
                review_context: None,
                input_artifacts: Vec::new(),
                depends_on: None,
                output: None,
                created_at: None,
                started_at: None,
                completed_at: None,
            },
        ];

        let attempt = Attempt {
            id: "attempt-1".to_string(),
            work_item_id: "work-1".to_string(),
            kind: AttemptKind::Write,
            status: AttemptStatus::Complete,
            coder_mapping: CoderMapping::default(),
            tasks,
            review_state: Some(AttemptReviewState::Passed),
            pause_kind: None,
            artifacts: Vec::new(),
            created_at: None,
            completed_at: None,
            ..Default::default()
        };

        let error = latest_review_artifacts(Path::new("/tmp/project"), &attempt)
            .expect_err("unmanaged artifact area should fail");
        assert!(
            error
                .to_string()
                .contains("Task artifact area path must stay under .fluent/work/artifacts")
        );
    }

    #[test]
    fn initial_write_uses_full_review_roles() {
        let attempt = attempt_with_tasks(vec![write_task("attempt-1-write-1", Vec::new())]);

        assert_eq!(next_review_roles(&attempt), review::REVIEWERS);
    }

    #[test]
    fn followup_write_uses_failed_input_review_role() {
        let attempt = attempt_with_tasks(vec![
            write_task("attempt-1-write-1", Vec::new()),
            review_task("attempt-1-review-tests", "tests"),
            write_task(
                "attempt-1-write-2",
                vec![ArtifactRef {
                    producer_id: "attempt-1-review-tests".to_string(),
                    path:
                        ".fluent/work/artifacts/work-1/attempt-1/attempt-1-review-tests/review.md"
                            .to_string(),
                }],
            ),
        ]);

        assert_eq!(next_review_roles(&attempt), vec!["tests"]);
    }

    #[test]
    fn followup_write_uses_failed_roles_in_default_order() {
        let attempt = attempt_with_tasks(vec![
            write_task("attempt-1-write-1", Vec::new()),
            review_task("attempt-1-review-tests", "tests"),
            review_task("attempt-1-review-documentation", "documentation"),
            write_task(
                "attempt-1-write-2",
                vec![
                    ArtifactRef {
                        producer_id: "attempt-1-review-tests".to_string(),
                        path: ".fluent/work/artifacts/work-1/attempt-1/attempt-1-review-tests/review.md"
                            .to_string(),
                    },
                    ArtifactRef {
                        producer_id: "attempt-1-review-documentation".to_string(),
                        path: ".fluent/work/artifacts/work-1/attempt-1/attempt-1-review-documentation/review.md"
                            .to_string(),
                    },
                ],
            ),
        ]);

        assert_eq!(next_review_roles(&attempt), vec!["documentation", "tests"]);
    }

    #[test]
    fn unmappable_followup_inputs_fall_back_to_full_review_roles() {
        let attempt = attempt_with_tasks(vec![
            write_task("attempt-1-write-1", Vec::new()),
            review_task("attempt-1-review-tests", "tests"),
            write_task(
                "attempt-1-write-2",
                vec![ArtifactRef {
                    producer_id: "missing-review-task".to_string(),
                    path: ".fluent/work/artifacts/work-1/attempt-1/missing-review-task/review.md"
                        .to_string(),
                }],
            ),
        ]);

        assert_eq!(next_review_roles(&attempt), review::REVIEWERS);
    }

    #[test]
    fn cap_allows_multiple_reviewers_in_flight_simultaneously() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::time::Duration;

        // No effective throttle: cap is well above the number of
        // spawned tasks. This proves parallel execution actually
        // overlaps, not just that the cap path doesn't crash with
        // a single task.
        let cap = 5_usize;
        let total_tasks = 4_usize;
        let semaphore = Arc::new((Mutex::new(0_usize), Condvar::new()));
        let in_flight = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));

        thread::scope(|scope| {
            let handles: Vec<_> = (0..total_tasks)
                .map(|_| {
                    let sem = Arc::clone(&semaphore);
                    let in_flight = Arc::clone(&in_flight);
                    let peak = Arc::clone(&peak);
                    scope.spawn(move || {
                        let _guard = acquire_slot(&sem, cap);
                        let current = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                        loop {
                            let old = peak.load(Ordering::SeqCst);
                            if current <= old
                                || peak
                                    .compare_exchange(
                                        old,
                                        current,
                                        Ordering::SeqCst,
                                        Ordering::SeqCst,
                                    )
                                    .is_ok()
                            {
                                break;
                            }
                        }
                        thread::sleep(Duration::from_millis(80));
                        in_flight.fetch_sub(1, Ordering::SeqCst);
                    })
                })
                .collect();

            for handle in handles {
                handle.join().unwrap();
            }
        });

        let observed_peak = peak.load(Ordering::SeqCst);
        assert!(
            observed_peak >= 2,
            "expected at least 2 reviewers in flight simultaneously under cap {cap}; observed peak was {observed_peak}"
        );
    }

    #[test]
    fn cap_enforcement_limits_in_flight_reviewers() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::time::Duration;

        let cap = 2_usize;
        let total_tasks = 5_usize;
        let semaphore = Arc::new((Mutex::new(0_usize), Condvar::new()));
        let in_flight = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));

        thread::scope(|scope| {
            let handles: Vec<_> = (0..total_tasks)
                .map(|_| {
                    let sem = Arc::clone(&semaphore);
                    let in_flight = Arc::clone(&in_flight);
                    let peak = Arc::clone(&peak);
                    scope.spawn(move || {
                        let _guard = acquire_slot(&sem, cap);
                        let current = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                        loop {
                            let old = peak.load(Ordering::SeqCst);
                            if current <= old
                                || peak
                                    .compare_exchange(
                                        old,
                                        current,
                                        Ordering::SeqCst,
                                        Ordering::SeqCst,
                                    )
                                    .is_ok()
                            {
                                break;
                            }
                        }
                        thread::sleep(Duration::from_millis(50));
                        in_flight.fetch_sub(1, Ordering::SeqCst);
                    })
                })
                .collect();

            for handle in handles {
                handle.join().unwrap();
            }
        });

        let observed_peak = peak.load(Ordering::SeqCst);
        assert!(
            observed_peak <= cap,
            "peak in-flight {observed_peak} exceeded cap {cap}"
        );
        assert!(observed_peak >= 1, "expected at least 1 in-flight reviewer");
    }

    fn attempt_with_tasks(tasks: Vec<Task>) -> Attempt {
        Attempt {
            id: "attempt-1".to_string(),
            work_item_id: "work-1".to_string(),
            kind: AttemptKind::Write,
            status: AttemptStatus::Planned,
            coder_mapping: CoderMapping::default(),
            tasks,
            review_state: Some(AttemptReviewState::NotReviewed),
            pause_kind: None,
            artifacts: Vec::new(),
            created_at: None,
            completed_at: None,
            ..Default::default()
        }
    }

    fn write_task(id: &str, input_artifacts: Vec<ArtifactRef>) -> Task {
        Task {
            id: id.to_string(),
            kind: TaskKind::Write,
            status: TaskStatus::Complete,
            role: "author".to_string(),
            instructions: None,
            work_item_id: "work-1".to_string(),
            attempt_id: Some("attempt-1".to_string()),
            workspace_access: WorkspaceAccess {
                reads: Vec::new(),
                writes: Vec::new(),
            },
            artifact_area: None,
            review_context: None,
            input_artifacts,
            depends_on: None,
            output: None,
            created_at: None,
            started_at: None,
            completed_at: None,
        }
    }

    #[test]
    fn tasks_ready_to_run_returns_independent_tasks_immediately() {
        let tasks = vec![
            write_task("attempt-1-write-1", Vec::new()),
            review_task("attempt-1-review-tests", "tests"),
        ];
        let review = &tasks[1];
        assert!(is_task_ready(review, &tasks));
    }

    #[test]
    fn tasks_ready_to_run_skips_reviewers_until_tester_complete() {
        let tester_task = Task {
            id: "attempt-1-tester".to_string(),
            kind: TaskKind::Tester,
            status: TaskStatus::Planned,
            role: "tester".to_string(),
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
        let behaviors_review = Task {
            id: "attempt-1-review-behaviors".to_string(),
            kind: TaskKind::Review,
            status: TaskStatus::Planned,
            role: "behaviors".to_string(),
            depends_on: Some("attempt-1-tester".to_string()),
            ..review_task("attempt-1-review-behaviors", "behaviors")
        };
        let tasks = vec![
            write_task("attempt-1-write-1", Vec::new()),
            tester_task,
            behaviors_review,
        ];

        assert!(
            is_task_ready(&tasks[1], &tasks),
            "Tester task has no depends_on, should be ready"
        );
        assert!(
            !is_task_ready(&tasks[2], &tasks),
            "behaviors review depends on incomplete Tester, should not be ready"
        );
    }

    #[test]
    fn tasks_ready_to_run_returns_dependent_after_tester_completes() {
        let tester_task = Task {
            id: "attempt-1-tester".to_string(),
            kind: TaskKind::Tester,
            status: TaskStatus::Complete,
            role: "tester".to_string(),
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
        let behaviors_review = Task {
            id: "attempt-1-review-behaviors".to_string(),
            kind: TaskKind::Review,
            status: TaskStatus::Planned,
            role: "behaviors".to_string(),
            depends_on: Some("attempt-1-tester".to_string()),
            ..review_task("attempt-1-review-behaviors", "behaviors")
        };
        let tasks = vec![
            write_task("attempt-1-write-1", Vec::new()),
            tester_task,
            behaviors_review,
        ];

        assert!(
            is_task_ready(&tasks[2], &tasks),
            "behaviors review should be ready after Tester completes"
        );
    }

    #[test]
    fn tester_task_is_review_phase_task() {
        let tester = Task {
            id: "tester".to_string(),
            kind: TaskKind::Tester,
            status: TaskStatus::Planned,
            role: "tester".to_string(),
            instructions: None,
            work_item_id: "w".to_string(),
            attempt_id: None,
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
        assert!(is_review_phase_task(&tester));
    }

    fn review_task(id: &str, role: &str) -> Task {
        Task {
            id: id.to_string(),
            kind: TaskKind::Review,
            status: TaskStatus::Complete,
            role: role.to_string(),
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
        }
    }

    fn tester_task(id: &str, artifact_path: &str) -> Task {
        Task {
            id: id.to_string(),
            kind: TaskKind::Tester,
            status: TaskStatus::Complete,
            role: "tester".to_string(),
            instructions: None,
            work_item_id: "work-1".to_string(),
            attempt_id: Some("attempt-1".to_string()),
            workspace_access: WorkspaceAccess {
                reads: Vec::new(),
                writes: Vec::new(),
            },
            artifact_area: Some(TaskArtifactArea {
                path: artifact_path.to_string(),
            }),
            review_context: None,
            input_artifacts: Vec::new(),
            depends_on: None,
            output: None,
            created_at: None,
            started_at: None,
            completed_at: None,
        }
    }

    use crate::work_model::{ReviewContext, WorkspaceRef};

    fn review_task_with_artifact(id: &str, role: &str, artifact_path: &str) -> Task {
        Task {
            artifact_area: Some(TaskArtifactArea {
                path: artifact_path.to_string(),
            }),
            workspace_access: WorkspaceAccess {
                reads: vec![WorkspaceRef {
                    id: "candidate".to_string(),
                    path: ".fluent/work/workspaces/work-1/attempt-1/candidate".to_string(),
                }],
                writes: Vec::new(),
            },
            review_context: Some(ReviewContext {
                candidate_workspace_id: "candidate".to_string(),
                candidate_workspace_path: ".fluent/work/workspaces/work-1/attempt-1/candidate"
                    .to_string(),
                source_branch: "work/attempt-1".to_string(),
                candidate_commit: "abc123".to_string(),
                base_commit: None,
            }),
            ..review_task(id, role)
        }
    }

    use crate::work_model::TaskOutput;

    fn make_interpret_reviews_fixture(
        project_root: &Path,
        review_verdict: &str,
        tester_json: Option<&str>,
    ) -> (WorkModelStore, WorkItem) {
        let store = WorkModelStore::new(project_root);
        let mut item = WorkItem {
            id: "work-1".to_string(),
            title: "Test item".to_string(),
            planning_context: None,
            instructions: None,
            abandonment: None,
            post_merge_review_fix_depth: None,
            attempts: Vec::new(),
            merge_candidates: Vec::new(),
            ..Default::default()
        };
        item.add_initial_attempt("attempt-1").unwrap();

        let attempt = &mut item.attempts[0];
        attempt.tasks[0].status = TaskStatus::Complete;
        attempt.tasks[0].output = Some(TaskOutput {
            workspace_id: "candidate".to_string(),
            workspace_path: ".fluent/work/workspaces/work-1/attempt-1/candidate".to_string(),
            source_branch: "work/attempt-1".to_string(),
            base_commit: None,
            commit: "abc123".to_string(),
        });

        let tester_artifact_path = work_artifact_path("work-1", "attempt-1", "attempt-1-tester");
        attempt
            .tasks
            .push(tester_task("attempt-1-tester", &tester_artifact_path));

        if let Some(json) = tester_json {
            let tester_dir = project_root.join(&tester_artifact_path);
            fs::create_dir_all(&tester_dir).unwrap();
            fs::write(tester_dir.join("tester-results.json"), json).unwrap();
        }

        let review_artifact_path =
            work_artifact_path("work-1", "attempt-1", "attempt-1-review-behaviors");
        attempt.tasks.push(review_task_with_artifact(
            "attempt-1-review-behaviors",
            "behaviors",
            &review_artifact_path,
        ));

        let review_dir = project_root.join(&review_artifact_path);
        fs::create_dir_all(&review_dir).unwrap();
        fs::write(
            review_dir.join("review.md"),
            format!("Verdict: {review_verdict}\n"),
        )
        .unwrap();

        store.create_work_item(&item).unwrap();
        (store, item)
    }

    fn passing_tester_json() -> &'static str {
        r#"{
            "commands": [],
            "tests": [
                {"id": "test_a", "test_harness": "cargo-nextest", "status": "pass", "duration_ms": 10, "failure_excerpt": null}
            ],
            "summary": {"total": 1, "pass": 1, "fail": 0, "skipped": 0},
            "error": null
        }"#
    }

    fn failing_tester_json() -> &'static str {
        r#"{
            "commands": [],
            "tests": [
                {"id": "test_a", "test_harness": "cargo-nextest", "status": "pass", "duration_ms": 10, "failure_excerpt": null},
                {"id": "test_b", "test_harness": "cargo-nextest", "status": "fail", "duration_ms": 5, "failure_excerpt": "assertion failed"}
            ],
            "summary": {"total": 2, "pass": 1, "fail": 1, "skipped": 0},
            "error": null
        }"#
    }

    #[test]
    fn tester_failures_counts_fail_status() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("tester-results.json");
        fs::write(&path, failing_tester_json()).unwrap();
        assert_eq!(tester_failures(&path), 1);
    }

    #[test]
    fn tester_failures_returns_zero_for_all_passing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("tester-results.json");
        fs::write(&path, passing_tester_json()).unwrap();
        assert_eq!(tester_failures(&path), 0);
    }

    #[test]
    fn tester_failures_returns_zero_for_missing_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("nonexistent.json");
        assert_eq!(tester_failures(&path), 0);
    }

    #[test]
    fn tester_failures_returns_zero_for_malformed_json() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("tester-results.json");
        fs::write(&path, "not valid json {{{").unwrap();
        assert_eq!(tester_failures(&path), 0);
    }

    #[test]
    fn tester_failure_blocks_merge_candidate() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (store, _) =
            make_interpret_reviews_fixture(tmp.path(), "PASS", Some(failing_tester_json()));

        let item = store.read_work_item("work-1").unwrap();
        let outcome = interpret_reviews(tmp.path(), &store, item, "attempt-1", true, None).unwrap();

        assert!(
            !matches!(outcome, WorkAttemptRunOutcome::MergeCandidateReady { .. }),
            "tester failure must block merge candidate; got {outcome:?}"
        );
    }

    #[test]
    fn tester_failure_routes_to_followup_within_budget() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (store, _) =
            make_interpret_reviews_fixture(tmp.path(), "PASS", Some(failing_tester_json()));

        let item = store.read_work_item("work-1").unwrap();
        let outcome = interpret_reviews(tmp.path(), &store, item, "attempt-1", true, None).unwrap();

        assert!(
            matches!(outcome, WorkAttemptRunOutcome::PlannedWriteRound { .. }),
            "tester failure with budget should schedule follow-up write; got {outcome:?}"
        );
    }

    #[test]
    fn tester_failure_records_needs_user_at_cap() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (store, _) =
            make_interpret_reviews_fixture(tmp.path(), "PASS", Some(failing_tester_json()));

        let item = store.read_work_item("work-1").unwrap();
        let outcome =
            interpret_reviews(tmp.path(), &store, item, "attempt-1", false, None).unwrap();

        assert!(
            matches!(outcome, WorkAttemptRunOutcome::NeedsUser { .. }),
            "tester failure at budget cap should record needs-user; got {outcome:?}"
        );
    }

    fn make_fixture_with_baseline(
        project_root: &Path,
        review_verdict: &str,
        tester_json: Option<&str>,
        baseline_json: Option<&str>,
    ) -> (WorkModelStore, WorkItem) {
        let (store, item) =
            make_interpret_reviews_fixture(project_root, review_verdict, tester_json);
        if let Some(json) = baseline_json {
            let baseline_dir = project_root.join(work_artifact_path(
                "work-1",
                "attempt-1",
                "attempt-1-baseline-tester",
            ));
            fs::create_dir_all(&baseline_dir).unwrap();
            fs::write(baseline_dir.join("tester-results.json"), json).unwrap();
        }
        (store, item)
    }

    fn tester_json_with_ids(fail_ids: &[&str], pass_ids: &[&str]) -> String {
        let mut tests = Vec::new();
        for id in fail_ids {
            tests.push(format!(
                r#"{{"id": "{}", "test_harness": "cargo-nextest", "status": "fail", "duration_ms": 5, "failure_excerpt": "assertion failed"}}"#,
                id
            ));
        }
        for id in pass_ids {
            tests.push(format!(
                r#"{{"id": "{}", "test_harness": "cargo-nextest", "status": "pass", "duration_ms": 10, "failure_excerpt": null}}"#,
                id
            ));
        }
        let tests_str = tests.join(", ");
        let total = fail_ids.len() + pass_ids.len();
        let pass = pass_ids.len();
        let fail = fail_ids.len();
        format!(
            r#"{{"commands": [], "tests": [{}], "summary": {{"total": {}, "pass": {}, "fail": {}, "skipped": 0}}, "error": null}}"#,
            tests_str, total, pass, fail
        )
    }

    #[test]
    fn failing_ids_extracts_fail_test_ids() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("results.json");
        fs::write(
            &path,
            tester_json_with_ids(&["test_x", "test_y"], &["test_z"]),
        )
        .unwrap();
        let ids = failing_ids(&path);
        assert_eq!(ids.len(), 2);
        assert!(ids.contains("test_x"));
        assert!(ids.contains("test_y"));
    }

    #[test]
    fn failing_ids_returns_empty_for_missing_file() {
        let ids = failing_ids(Path::new("/nonexistent/path.json"));
        assert!(ids.is_empty());
    }

    #[test]
    fn introduced_failures_subtracts_baseline() {
        let tmp = tempfile::TempDir::new().unwrap();
        let current = tmp.path().join("current.json");
        let baseline = tmp.path().join("baseline.json");
        fs::write(&current, tester_json_with_ids(&["test_a", "test_b"], &[])).unwrap();
        fs::write(&baseline, tester_json_with_ids(&["test_a"], &["test_c"])).unwrap();
        assert_eq!(introduced_tester_failures(&current, Some(&baseline)), 1);
    }

    #[test]
    fn introduced_failures_counts_all_without_baseline() {
        let tmp = tempfile::TempDir::new().unwrap();
        let current = tmp.path().join("current.json");
        fs::write(&current, tester_json_with_ids(&["test_a", "test_b"], &[])).unwrap();
        assert_eq!(introduced_tester_failures(&current, None), 2);
    }

    #[test]
    fn preexisting_failures_pass_gate_with_baseline() {
        let tmp = tempfile::TempDir::new().unwrap();
        let baseline_json = tester_json_with_ids(&["test_b"], &["test_a"]);
        let current_json = tester_json_with_ids(&["test_b"], &["test_a"]);
        let (store, _) = make_fixture_with_baseline(
            tmp.path(),
            "PASS",
            Some(&current_json),
            Some(&baseline_json),
        );
        let item = store.read_work_item("work-1").unwrap();
        let outcome = interpret_reviews(tmp.path(), &store, item, "attempt-1", true, None).unwrap();
        assert!(
            matches!(outcome, WorkAttemptRunOutcome::MergeCandidateReady { .. }),
            "pre-existing failure should pass gate when baseline matches; got {outcome:?}"
        );
    }

    #[test]
    fn introduced_failure_blocks_gate_with_baseline() {
        let tmp = tempfile::TempDir::new().unwrap();
        let baseline_json = tester_json_with_ids(&["test_b"], &["test_a", "test_c"]);
        let current_json = tester_json_with_ids(&["test_b", "test_c"], &["test_a"]);
        let (store, _) = make_fixture_with_baseline(
            tmp.path(),
            "PASS",
            Some(&current_json),
            Some(&baseline_json),
        );
        let item = store.read_work_item("work-1").unwrap();
        let outcome = interpret_reviews(tmp.path(), &store, item, "attempt-1", true, None).unwrap();
        assert!(
            !matches!(outcome, WorkAttemptRunOutcome::MergeCandidateReady { .. }),
            "introduced failure (test_c) should block gate; got {outcome:?}"
        );
    }

    #[test]
    fn no_baseline_falls_back_to_absolute_count() {
        let tmp = tempfile::TempDir::new().unwrap();
        let current_json = tester_json_with_ids(&["test_b"], &["test_a"]);
        let (store, _) = make_fixture_with_baseline(tmp.path(), "PASS", Some(&current_json), None);
        let item = store.read_work_item("work-1").unwrap();
        let outcome = interpret_reviews(tmp.path(), &store, item, "attempt-1", true, None).unwrap();
        assert!(
            !matches!(outcome, WorkAttemptRunOutcome::MergeCandidateReady { .. }),
            "without baseline, any failure should block gate; got {outcome:?}"
        );
    }

    #[test]
    fn passing_or_missing_tester_does_not_block() {
        let tmp_pass = tempfile::TempDir::new().unwrap();
        let (store_pass, _) =
            make_interpret_reviews_fixture(tmp_pass.path(), "PASS", Some(passing_tester_json()));
        let item_pass = store_pass.read_work_item("work-1").unwrap();
        let outcome_pass = interpret_reviews(
            tmp_pass.path(),
            &store_pass,
            item_pass,
            "attempt-1",
            true,
            None,
        )
        .unwrap();
        assert!(
            matches!(
                outcome_pass,
                WorkAttemptRunOutcome::MergeCandidateReady { .. }
            ),
            "passing tester should allow merge candidate; got {outcome_pass:?}"
        );

        let tmp_missing = tempfile::TempDir::new().unwrap();
        let (store_missing, _) = make_interpret_reviews_fixture(tmp_missing.path(), "PASS", None);
        let item_missing = store_missing.read_work_item("work-1").unwrap();
        let outcome_missing = interpret_reviews(
            tmp_missing.path(),
            &store_missing,
            item_missing,
            "attempt-1",
            true,
            None,
        )
        .unwrap();
        assert!(
            matches!(
                outcome_missing,
                WorkAttemptRunOutcome::MergeCandidateReady { .. }
            ),
            "missing tester results should allow merge candidate; got {outcome_missing:?}"
        );
    }

    fn errored_tester_json() -> &'static str {
        r#"{
            "commands": [],
            "tests": [],
            "summary": {"total": 0, "pass": 0, "fail": 0, "skipped": 0},
            "error": {"kind": "extractor_failure", "message": "extractor failed", "details": "exit code 1"}
        }"#
    }

    #[test]
    fn tester_error_blocks_merge_candidate() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (store, _) =
            make_interpret_reviews_fixture(tmp.path(), "PASS", Some(errored_tester_json()));

        let item = store.read_work_item("work-1").unwrap();
        let outcome = interpret_reviews(tmp.path(), &store, item, "attempt-1", true, None).unwrap();

        assert!(
            !matches!(outcome, WorkAttemptRunOutcome::MergeCandidateReady { .. }),
            "tester error must block merge candidate; got {outcome:?}"
        );
    }

    #[test]
    fn tester_error_routes_to_followup_within_budget() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (store, _) =
            make_interpret_reviews_fixture(tmp.path(), "PASS", Some(errored_tester_json()));

        let item = store.read_work_item("work-1").unwrap();
        let outcome = interpret_reviews(tmp.path(), &store, item, "attempt-1", true, None).unwrap();

        assert!(
            matches!(outcome, WorkAttemptRunOutcome::PlannedWriteRound { .. }),
            "tester error with budget should schedule follow-up write; got {outcome:?}"
        );
    }

    #[test]
    fn tester_error_records_needs_user_at_cap() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (store, _) =
            make_interpret_reviews_fixture(tmp.path(), "PASS", Some(errored_tester_json()));

        let item = store.read_work_item("work-1").unwrap();
        let outcome =
            interpret_reviews(tmp.path(), &store, item, "attempt-1", false, None).unwrap();

        assert!(
            matches!(outcome, WorkAttemptRunOutcome::NeedsUser { .. }),
            "tester error at budget cap should record needs-user; got {outcome:?}"
        );
    }

    #[test]
    fn errored_tester_does_not_fall_through_to_reviewers() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (store, _) =
            make_interpret_reviews_fixture(tmp.path(), "PASS", Some(errored_tester_json()));

        let item = store.read_work_item("work-1").unwrap();
        let outcome = interpret_reviews(tmp.path(), &store, item, "attempt-1", true, None).unwrap();

        assert!(
            !matches!(
                outcome,
                WorkAttemptRunOutcome::MergeCandidateReady { .. }
                    | WorkAttemptRunOutcome::ReviewOnlyComplete
            ),
            "errored tester must not fall through to reviewer pass path; got {outcome:?}"
        );
    }

    #[test]
    fn tester_error_blocks_regardless_of_baseline() {
        let tmp = tempfile::TempDir::new().unwrap();
        let errored_json = errored_tester_json();
        let (store, _) =
            make_fixture_with_baseline(tmp.path(), "PASS", Some(errored_json), Some(errored_json));

        let item = store.read_work_item("work-1").unwrap();
        let outcome = interpret_reviews(tmp.path(), &store, item, "attempt-1", true, None).unwrap();

        assert!(
            !matches!(outcome, WorkAttemptRunOutcome::MergeCandidateReady { .. }),
            "tester error must block even when baseline also errored; got {outcome:?}"
        );
    }

    fn init_learner_repo(path: &Path) {
        git::run(path, &["init", "--initial-branch=main"], "init").unwrap();
        git::run(path, &["config", "user.email", "t@t.com"], "email").unwrap();
        git::run(path, &["config", "user.name", "Test"], "name").unwrap();
        git::run(path, &["config", "commit.gpgsign", "false"], "gpg").unwrap();
    }

    /// A passing code-producing Attempt over `rounds` write rounds, with a real
    /// candidate worktree as a managed sibling of the project root so the Learner
    /// orchestration can resolve it and inspect its Git state.
    fn make_learner_passing_fixture(
        tmp: &Path,
        rounds: usize,
    ) -> (WorkModelStore, PathBuf, PathBuf, String) {
        let project_root = tmp.join("main");
        fs::create_dir_all(&project_root).unwrap();
        let store = WorkModelStore::new(&project_root);

        let workspace = tmp.join("work-1-candidate");
        fs::create_dir_all(&workspace).unwrap();
        init_learner_repo(&workspace);
        fs::write(workspace.join("src.rs"), "fn main() {}").unwrap();
        git::run(&workspace, &["add", "."], "add").unwrap();
        git::run(&workspace, &["commit", "-m", "initial"], "commit").unwrap();
        let base = git::run_stdout(&workspace, &["rev-parse", "HEAD"], "base").unwrap();

        let mut item = WorkItem {
            id: "work-1".to_string(),
            title: "Learner test".to_string(),
            attempts: Vec::new(),
            merge_candidates: Vec::new(),
            ..Default::default()
        };
        item.add_initial_attempt("attempt-1").unwrap();
        let output = TaskOutput {
            workspace_id: "candidate".to_string(),
            workspace_path: "../work-1-candidate".to_string(),
            source_branch: "main".to_string(),
            base_commit: None,
            commit: base.clone(),
        };

        let attempt = &mut item.attempts[0];
        attempt.tasks[0].status = TaskStatus::Complete;
        attempt.tasks[0].output = Some(output.clone());

        let review1_path = work_artifact_path("work-1", "attempt-1", "attempt-1-review-1-tests");
        attempt.tasks.push(review_task_with_artifact(
            "attempt-1-review-1-tests",
            "tests",
            &review1_path,
        ));
        let review1_dir = project_root.join(&review1_path);
        fs::create_dir_all(&review1_dir).unwrap();
        fs::write(review1_dir.join("review.md"), "Verdict: pass\n").unwrap();

        if rounds >= 2 {
            attempt.tasks.push(Task {
                id: "attempt-1-write-2".to_string(),
                kind: TaskKind::Write,
                status: TaskStatus::Complete,
                output: Some(output.clone()),
                ..write_task("attempt-1-write-2", Vec::new())
            });
            let review2_path =
                work_artifact_path("work-1", "attempt-1", "attempt-1-review-2-tests");
            attempt.tasks.push(review_task_with_artifact(
                "attempt-1-review-2-tests",
                "tests",
                &review2_path,
            ));
            let review2_dir = project_root.join(&review2_path);
            fs::create_dir_all(&review2_dir).unwrap();
            fs::write(review2_dir.join("review.md"), "Verdict: pass\n").unwrap();
        }

        store.create_work_item(&item).unwrap();
        (store, project_root, workspace, base)
    }

    /// A Learner coder stub that writes an untrusted draft into the handoff
    /// surface and makes no expertise commit.
    fn draft_only_coder(
        draft_json: &'static str,
    ) -> impl Fn(&LearnerCoderRequest<'_>) -> Result<()> {
        move |request: &LearnerCoderRequest<'_>| {
            fs::create_dir_all(request.handoff_dir).unwrap();
            fs::write(
                request.handoff_dir.join(crate::learner::DRAFT_FILE_NAME),
                draft_json,
            )
            .unwrap();
            Ok(())
        }
    }

    #[test]
    fn learner_runs_after_first_pass_without_findings() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (store, project_root, _workspace, _base) = make_learner_passing_fixture(tmp.path(), 1);

        let item = store.read_work_item("work-1").unwrap();
        let run_coder = draft_only_coder(r#"{"learning_summary":"learned","follow_ups":[]}"#);
        let outcome = interpret_reviews(
            &project_root,
            &store,
            item,
            "attempt-1",
            true,
            Some(LearnerConfig {
                run_coder: &run_coder,
            }),
        )
        .unwrap();

        assert!(
            matches!(outcome, WorkAttemptRunOutcome::MergeCandidateReady { .. }),
            "first-pass reviews passed — candidate ready; got {outcome:?}"
        );
        let stored = store.read_work_item("work-1").unwrap();
        let learning = stored.attempts[0]
            .learning
            .as_ref()
            .expect("learner ran on first pass");
        assert!(learning.is_succeeded());
        let handoff_ref = learning.handoff.as_ref().expect("handoff persisted");
        let handoff = crate::learner::load_verified_handoff(&project_root, handoff_ref).unwrap();
        assert_eq!(handoff.source_work_item_id, "work-1");
        assert_eq!(handoff.source_attempt_id, "attempt-1");
        assert_eq!(
            handoff.source_merge_candidate_id.as_deref(),
            Some("attempt-1-merge-candidate")
        );
    }

    #[test]
    fn learner_runs_after_passing_corrective_round() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (store, project_root, _workspace, _base) = make_learner_passing_fixture(tmp.path(), 2);

        let item = store.read_work_item("work-1").unwrap();
        let run_coder = draft_only_coder(r#"{"learning_summary":"learned","follow_ups":[]}"#);
        let outcome = interpret_reviews(
            &project_root,
            &store,
            item,
            "attempt-1",
            true,
            Some(LearnerConfig {
                run_coder: &run_coder,
            }),
        )
        .unwrap();

        assert!(matches!(
            outcome,
            WorkAttemptRunOutcome::MergeCandidateReady { .. }
        ));
        let stored = store.read_work_item("work-1").unwrap();
        assert!(
            stored.attempts[0]
                .learning
                .as_ref()
                .expect("learner ran after corrective round")
                .is_succeeded()
        );
    }

    #[test]
    fn learner_without_expertise_change_keeps_candidate_commit() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (store, project_root, _workspace, base) = make_learner_passing_fixture(tmp.path(), 1);

        let item = store.read_work_item("work-1").unwrap();
        let run_coder = draft_only_coder(r#"{"learning_summary":"","follow_ups":[]}"#);
        interpret_reviews(
            &project_root,
            &store,
            item,
            "attempt-1",
            true,
            Some(LearnerConfig {
                run_coder: &run_coder,
            }),
        )
        .unwrap();

        let stored = store.read_work_item("work-1").unwrap();
        assert_eq!(
            stored.merge_candidates[0].candidate_commit, base,
            "no expertise commit must leave the candidate commit unchanged"
        );
    }

    #[test]
    fn review_only_attempt_skips_learner() {
        let tmp = tempfile::TempDir::new().unwrap();
        let project_root = tmp.path().join("main");
        fs::create_dir_all(&project_root).unwrap();
        let store = WorkModelStore::new(&project_root);

        let mut item = WorkItem::planned("work-1", "Review only");
        item.add_review_only_attempt("attempt-1", &["tests"], "main", "abc123", true)
            .unwrap();
        let attempt = &mut item.attempts[0];
        for task in &mut attempt.tasks {
            let area = task.artifact_area.as_ref().unwrap().path.clone();
            let dir = project_root.join(&area);
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("review.md"), "Verdict: pass\n").unwrap();
            crate::work_model::set_task_terminal(task, TaskStatus::Complete);
        }
        store.create_work_item(&item).unwrap();

        let item = store.read_work_item("work-1").unwrap();
        let run_coder = |_: &LearnerCoderRequest<'_>| -> Result<()> {
            panic!("the Learner must not run for a review-only Attempt")
        };
        let outcome = interpret_reviews(
            &project_root,
            &store,
            item,
            "attempt-1",
            true,
            Some(LearnerConfig {
                run_coder: &run_coder,
            }),
        )
        .unwrap();

        assert!(matches!(outcome, WorkAttemptRunOutcome::ReviewOnlyComplete));
        let stored = store.read_work_item("work-1").unwrap();
        assert!(
            stored.attempts[0].learning.is_none(),
            "review-only Attempt records no learning"
        );
    }

    #[test]
    fn auth_pause_records_kind_and_leaves_attempt_resumable() {
        let mut attempt = Attempt {
            id: "attempt-1".to_string(),
            work_item_id: "work-1".to_string(),
            kind: AttemptKind::Write,
            status: AttemptStatus::Executing,
            coder_mapping: CoderMapping::default(),
            tasks: vec![
                Task {
                    id: "attempt-1-write-1".to_string(),
                    kind: TaskKind::Write,
                    status: TaskStatus::Complete,
                    role: "author".to_string(),
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
                },
                Task {
                    id: "attempt-1-review-tests".to_string(),
                    kind: TaskKind::Review,
                    status: TaskStatus::Failed,
                    role: "tests".to_string(),
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
                },
            ],
            review_state: None,
            pause_kind: None,
            artifacts: Vec::new(),
            created_at: None,
            completed_at: None,
            ..Default::default()
        };

        crate::work_model::suspend_attempt(&mut attempt, crate::work_model::PauseKind::Auth);

        assert_eq!(attempt.status, AttemptStatus::NeedsUser);
        assert_eq!(attempt.pause_kind, Some(crate::work_model::PauseKind::Auth));
        assert!(
            attempt.completed_at.is_some(),
            "suspended attempt should have completed_at set"
        );
    }

    #[test]
    fn budget_exhaustion_records_round_cap_pause_kind() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (store, _) =
            make_interpret_reviews_fixture(tmp.path(), "PASS", Some(failing_tester_json()));

        let item = store.read_work_item("work-1").unwrap();
        let outcome =
            interpret_reviews(tmp.path(), &store, item, "attempt-1", false, None).unwrap();

        assert!(
            matches!(outcome, WorkAttemptRunOutcome::NeedsUser { .. }),
            "budget exhaustion should produce needs-user; got {outcome:?}"
        );
        let stored = store.read_work_item("work-1").unwrap();
        assert_eq!(
            stored.attempts[0].pause_kind,
            Some(crate::work_model::PauseKind::RoundCap),
            "budget exhaustion should record RoundCap pause kind"
        );
    }

    #[test]
    fn uncertain_reviews_record_uncertain_pause_kind() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (store, _) = make_interpret_reviews_fixture(tmp.path(), "UNCERTAIN", None);

        let item = store.read_work_item("work-1").unwrap();
        let outcome = interpret_reviews(tmp.path(), &store, item, "attempt-1", true, None).unwrap();

        assert!(
            matches!(outcome, WorkAttemptRunOutcome::NeedsUser { .. }),
            "uncertain review should produce needs-user; got {outcome:?}"
        );
        let stored = store.read_work_item("work-1").unwrap();
        assert_eq!(
            stored.attempts[0].pause_kind,
            Some(crate::work_model::PauseKind::Uncertain),
            "uncertain review should record Uncertain pause kind"
        );
    }

    #[test]
    fn resume_auth_pause_reruns_only_auth_failed_tasks() {
        let mut attempt = Attempt {
            id: "attempt-1".to_string(),
            work_item_id: "work-1".to_string(),
            kind: AttemptKind::Write,
            status: AttemptStatus::NeedsUser,
            coder_mapping: CoderMapping::default(),
            tasks: vec![
                Task {
                    id: "attempt-1-write-1".to_string(),
                    kind: TaskKind::Write,
                    status: TaskStatus::Complete,
                    role: "author".to_string(),
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
                    output: Some(TaskOutput {
                        workspace_id: "candidate".to_string(),
                        workspace_path: "work/wi-1/attempt-1".to_string(),
                        source_branch: "main".to_string(),
                        base_commit: None,
                        commit: "abc123".to_string(),
                    }),
                    created_at: None,
                    started_at: None,
                    completed_at: None,
                },
                Task {
                    id: "attempt-1-review-tests".to_string(),
                    kind: TaskKind::Review,
                    status: TaskStatus::Failed,
                    role: "tests".to_string(),
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
                },
            ],
            review_state: None,
            pause_kind: Some(PauseKind::Auth),
            artifacts: Vec::new(),
            created_at: None,
            completed_at: Some("2026-07-16T12:00:00Z".to_string()),
            ..Default::default()
        };

        assert!(matches!(
            reject_terminal_attempt(&attempt).unwrap(),
            TerminalCheck::Reopen
        ));

        crate::work_model::reopen_attempt(&mut attempt);

        assert_eq!(attempt.status, AttemptStatus::Planned);
        assert!(attempt.pause_kind.is_none());
        assert!(attempt.completed_at.is_none());
        assert_eq!(
            attempt.tasks[0].status,
            TaskStatus::Complete,
            "writer task should stay Complete"
        );
        assert_eq!(
            attempt.tasks[1].status,
            TaskStatus::Planned,
            "auth-failed review task should reset to Planned"
        );
    }

    #[test]
    fn transcript_pump_pause_is_resumable() {
        // A transcript-pump pause is a supported resume, like an auth pause: the
        // Attempt reopens and the pump-failed coder task resets to Planned so it
        // retries once the operator has fixed the transport. It is never rejected
        // as a terminal write-round cap.
        let mut attempt = Attempt {
            id: "attempt-1".to_string(),
            status: AttemptStatus::NeedsUser,
            pause_kind: Some(PauseKind::TranscriptPump),
            tasks: vec![Task {
                id: "attempt-1-write-1".to_string(),
                kind: TaskKind::Write,
                status: TaskStatus::Failed,
                role: "author".to_string(),
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
            }],
            completed_at: Some("2026-07-21T12:00:00Z".to_string()),
            ..Default::default()
        };

        assert!(
            matches!(
                reject_terminal_attempt(&attempt).unwrap(),
                TerminalCheck::Reopen
            ),
            "a transcript-pump pause must resume, not bail as a terminal cap"
        );

        crate::work_model::reopen_attempt(&mut attempt);
        assert_eq!(attempt.status, AttemptStatus::Planned);
        assert!(attempt.pause_kind.is_none());
        assert_eq!(
            attempt.tasks[0].status,
            TaskStatus::Planned,
            "the pump-failed write task resets to Planned to retry"
        );
    }

    #[test]
    fn resume_auth_pause_advances_to_merge_candidate_on_pass() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = WorkModelStore::new(tmp.path());

        let (fixture_store, _) =
            make_interpret_reviews_fixture(tmp.path(), "PASS", Some(passing_tester_json()));
        let mut item = fixture_store.read_work_item("work-1").unwrap();

        item.attempts[0].status = AttemptStatus::NeedsUser;
        item.attempts[0].pause_kind = Some(PauseKind::Auth);
        item.attempts[0].completed_at = Some("2026-07-16T12:00:00Z".to_string());
        store.write_work_item(&item).unwrap();

        crate::work_model::reopen_attempt(&mut item.attempts[0]);

        let outcome = interpret_reviews(tmp.path(), &store, item, "attempt-1", true, None).unwrap();
        assert!(
            matches!(outcome, WorkAttemptRunOutcome::MergeCandidateReady { .. }),
            "reopened attempt with passing reviews should advance to merge candidate; got {outcome:?}"
        );
    }

    #[test]
    fn resume_unimplemented_kind_reports_clearly_and_leaves_suspended() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = WorkModelStore::new(tmp.path());
        let mut item = WorkItem {
            id: "work-1".to_string(),
            title: "Paused attempt".to_string(),
            planning_context: None,
            instructions: None,
            abandonment: None,
            post_merge_review_fix_depth: None,
            attempts: Vec::new(),
            merge_candidates: Vec::new(),
            ..Default::default()
        };
        item.add_initial_attempt("attempt-1").unwrap();
        crate::work_model::suspend_attempt(&mut item.attempts[0], PauseKind::Uncertain);
        store.create_work_item(&item).unwrap();
        let resolver = ContentResolver::new(None);

        let error = run_attempt(WorkAttemptRunConfig {
            project_root: tmp.path(),
            store: &store,
            work_item_id: "work-1",
            attempt_id: "attempt-1",
            resolver: &resolver,
            extra_args: &[],
            no_sandbox: true,
            resolved_coder_mapping: None,
        })
        .unwrap_err();

        let msg = error.to_string();
        assert!(
            msg.contains("not yet supported"),
            "should mention that resume is not yet supported: {msg}"
        );
        let stored = store.read_work_item("work-1").unwrap();
        assert_eq!(
            stored.attempts[0].status,
            AttemptStatus::NeedsUser,
            "attempt should remain suspended"
        );
        assert_eq!(
            stored.attempts[0].pause_kind,
            Some(PauseKind::Uncertain),
            "pause kind should be preserved"
        );
    }

    #[test]
    fn learner_path_expertise_subtree_is_in_bounds() {
        assert!(is_learner_path_in_bounds(".fluent/expertise/overview.md"));
        assert!(is_learner_path_in_bounds(
            ".fluent/expertise/learnings/INDEX.md"
        ));
        assert!(is_learner_path_in_bounds(".fluent/expertise/decisions.md"));
    }

    #[test]
    fn learner_path_outside_expertise_is_out_of_bounds() {
        assert!(!is_learner_path_in_bounds("src/main.rs"));
        assert!(!is_learner_path_in_bounds("README.md"));
        assert!(!is_learner_path_in_bounds(".fluent/tester.yaml"));
        assert!(!is_learner_path_in_bounds(
            ".fluent/expertise-notes/something.md"
        ));
        assert!(!is_learner_path_in_bounds("documentation/behaviors.md"));
    }

    #[test]
    fn learner_path_expertise_without_trailing_slash_is_out_of_bounds() {
        assert!(!is_learner_path_in_bounds(".fluent/expertise"));
    }

    #[test]
    fn learner_expertise_commit_becomes_candidate_commit() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (store, project_root, workspace, base) = make_learner_passing_fixture(tmp.path(), 1);

        let run_coder = |request: &LearnerCoderRequest<'_>| -> Result<()> {
            let expertise = request.workspace_path.join(".fluent/expertise");
            fs::create_dir_all(&expertise).unwrap();
            fs::write(expertise.join("learning.md"), "# Learning").unwrap();
            git::run(
                request.workspace_path,
                &["add", ".fluent/expertise/learning.md"],
                "add expertise",
            )
            .unwrap();
            git::run(
                request.workspace_path,
                &["commit", "-m", "Update expertise"],
                "commit",
            )
            .unwrap();
            fs::create_dir_all(request.handoff_dir).unwrap();
            fs::write(
                request.handoff_dir.join(crate::learner::DRAFT_FILE_NAME),
                r#"{"learning_summary":"x","follow_ups":[]}"#,
            )
            .unwrap();
            Ok(())
        };
        let item = store.read_work_item("work-1").unwrap();
        interpret_reviews(
            &project_root,
            &store,
            item,
            "attempt-1",
            true,
            Some(LearnerConfig {
                run_coder: &run_coder,
            }),
        )
        .unwrap();

        let expertise_head = git::run_stdout(&workspace, &["rev-parse", "HEAD"], "head").unwrap();
        assert_ne!(expertise_head, base);
        let stored = store.read_work_item("work-1").unwrap();
        assert_eq!(
            stored.merge_candidates[0].candidate_commit, expertise_head,
            "a confined expertise commit becomes the candidate commit"
        );
    }

    #[test]
    fn learner_commit_touching_source_is_discarded() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (store, project_root, workspace, base) = make_learner_passing_fixture(tmp.path(), 1);

        let run_coder = |request: &LearnerCoderRequest<'_>| -> Result<()> {
            let expertise = request.workspace_path.join(".fluent/expertise");
            fs::create_dir_all(&expertise).unwrap();
            fs::write(expertise.join("learning.md"), "# Learning").unwrap();
            fs::write(
                request.workspace_path.join("src.rs"),
                "fn main() { /* changed */ }",
            )
            .unwrap();
            git::run(request.workspace_path, &["add", "."], "add all").unwrap();
            git::run(
                request.workspace_path,
                &["commit", "-m", "straying"],
                "commit",
            )
            .unwrap();
            fs::create_dir_all(request.handoff_dir).unwrap();
            fs::write(
                request.handoff_dir.join(crate::learner::DRAFT_FILE_NAME),
                r#"{"learning_summary":"x","follow_ups":[]}"#,
            )
            .unwrap();
            Ok(())
        };
        let item = store.read_work_item("work-1").unwrap();
        let outcome = interpret_reviews(
            &project_root,
            &store,
            item,
            "attempt-1",
            true,
            Some(LearnerConfig {
                run_coder: &run_coder,
            }),
        )
        .unwrap();

        assert!(
            matches!(outcome, WorkAttemptRunOutcome::MergeCandidateReady { .. }),
            "the candidate is still produced when the learner commit is discarded"
        );
        let head_after = git::run_stdout(&workspace, &["rev-parse", "HEAD"], "after").unwrap();
        assert_eq!(
            head_after, base,
            "an out-of-bounds learner commit is discarded"
        );
        let stored = store.read_work_item("work-1").unwrap();
        assert_eq!(
            stored.merge_candidates[0].candidate_commit, base,
            "the candidate commit stays at the pre-learner tip"
        );
        assert!(
            stored.attempts[0].learning.as_ref().unwrap().is_failed(),
            "an out-of-bounds learner commit records a retryable failure"
        );
    }
}
