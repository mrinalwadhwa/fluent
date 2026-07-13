use anyhow::{Result, bail};
use std::collections::HashSet;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;

use crate::content::ContentResolver;
use crate::review::{self, Verdict};
use crate::review_only_worktree;
use crate::work_model::{
    ArtifactRef, Attempt, AttemptReviewState, AttemptStatus, Task, TaskKind, TaskStatus, WorkItem,
    WorkModelStorageError, WorkModelStore, work_artifact_path,
};
use crate::work_task_executor::{self, WorkTaskRunConfig};

const DEFAULT_MAX_PARALLEL_REVIEWERS: usize = 5;
const DEFAULT_MAX_TOTAL_WRITE_ROUNDS: usize = 10;
const DEFAULT_MAX_NO_PROGRESS_ROUNDS: usize = 2;

fn max_parallel_reviewers() -> usize {
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkAttemptRunOutcome {
    RanTask { task_id: String, output: String },
    PlannedReviews { task_ids: Vec<String> },
    MergeCandidateReady { candidate_id: String },
    PlannedWriteRound { task_id: String },
    NeedsUser { handoff_path: String },
    ReviewOnlyComplete,
    ReviewOnlyFailed,
}

pub struct WorkAttemptRunResult {
    pub outcomes: Vec<WorkAttemptRunOutcome>,
}

pub fn run_attempt(config: WorkAttemptRunConfig<'_>) -> Result<WorkAttemptRunResult> {
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

        reject_terminal_attempt(attempt.status.clone())?;

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
            config.store.write_work_item(&item)?;
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
                    let lock_path = crate::lease::task_lock_path(
                        config.project_root,
                        config.work_item_id,
                        task_id,
                    );
                    if crate::lease::is_leased(&lock_path) {
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
            let outcome = interpret_reviews(
                config.project_root,
                config.store,
                item,
                config.attempt_id,
                can_advance,
            )?;
            let should_stop = matches!(
                outcome,
                WorkAttemptRunOutcome::MergeCandidateReady { .. }
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

fn reject_terminal_attempt(status: AttemptStatus) -> Result<()> {
    match status {
        AttemptStatus::Failed => bail!("Attempt is failed and cannot be advanced"),
        AttemptStatus::NeedsUser => bail!("Attempt needs user input before it can advance"),
        _ => Ok(()),
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
            crate::work_model::set_attempt_terminal(
                &mut item.attempts[attempt_index],
                AttemptStatus::NeedsUser,
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
        crate::work_model::set_attempt_terminal(
            &mut item.attempts[attempt_index],
            AttemptStatus::NeedsUser,
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
    let candidate_id = item.create_or_get_merge_candidate(attempt_id)?;
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
            attempts: Vec::new(),
            merge_candidates: Vec::new(),
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
            artifacts: Vec::new(),
            created_at: None,
            completed_at: None,
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
            artifacts: Vec::new(),
            created_at: None,
            completed_at: None,
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
            attempts: Vec::new(),
            merge_candidates: Vec::new(),
        };
        item.add_initial_attempt("attempt-1").unwrap();

        let attempt = &mut item.attempts[0];
        attempt.tasks[0].status = TaskStatus::Complete;
        attempt.tasks[0].output = Some(TaskOutput {
            workspace_id: "candidate".to_string(),
            workspace_path: ".fluent/work/workspaces/work-1/attempt-1/candidate".to_string(),
            source_branch: "work/attempt-1".to_string(),
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
        let outcome = interpret_reviews(tmp.path(), &store, item, "attempt-1", true).unwrap();

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
        let outcome = interpret_reviews(tmp.path(), &store, item, "attempt-1", true).unwrap();

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
        let outcome = interpret_reviews(tmp.path(), &store, item, "attempt-1", false).unwrap();

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
        let outcome = interpret_reviews(tmp.path(), &store, item, "attempt-1", true).unwrap();
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
        let outcome = interpret_reviews(tmp.path(), &store, item, "attempt-1", true).unwrap();
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
        let outcome = interpret_reviews(tmp.path(), &store, item, "attempt-1", true).unwrap();
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
        let outcome_pass =
            interpret_reviews(tmp_pass.path(), &store_pass, item_pass, "attempt-1", true).unwrap();
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
        let outcome = interpret_reviews(tmp.path(), &store, item, "attempt-1", true).unwrap();

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
        let outcome = interpret_reviews(tmp.path(), &store, item, "attempt-1", true).unwrap();

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
        let outcome = interpret_reviews(tmp.path(), &store, item, "attempt-1", false).unwrap();

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
        let outcome = interpret_reviews(tmp.path(), &store, item, "attempt-1", true).unwrap();

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
        let outcome = interpret_reviews(tmp.path(), &store, item, "attempt-1", true).unwrap();

        assert!(
            !matches!(outcome, WorkAttemptRunOutcome::MergeCandidateReady { .. }),
            "tester error must block even when baseline also errored; got {outcome:?}"
        );
    }
}
