use anyhow::{Result, bail};
use std::fs;
use std::io::ErrorKind;
use std::path::Path;

use crate::coder::CoderKind;
use crate::content::ContentResolver;
use crate::review::{self, Verdict};
use crate::work_model::{
    ArtifactRef, AttemptReviewState, AttemptStatus, Task, TaskKind, TaskStatus, WorkItem,
    WorkModelStorageError, WorkModelStore,
};
use crate::work_task_executor::{self, WorkTaskRunConfig};

pub struct WorkAttemptRunConfig<'a> {
    pub project_root: &'a Path,
    pub store: &'a WorkModelStore,
    pub work_item_id: &'a str,
    pub attempt_id: &'a str,
    pub resolver: &'a ContentResolver,
    pub extra_args: &'a [String],
    pub coder_kind: CoderKind,
    pub no_sandbox: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkAttemptRunOutcome {
    RanTask { task_id: String, output: String },
    PlannedReviews { task_ids: Vec<String> },
    ReviewsPassed,
    PlannedFollowup { task_id: String },
    NeedsUser { handoff_path: String },
}

pub struct WorkAttemptRunResult {
    pub outcomes: Vec<WorkAttemptRunOutcome>,
}

pub fn run_attempt(config: WorkAttemptRunConfig<'_>) -> Result<WorkAttemptRunResult> {
    let mut outcomes = Vec::new();

    loop {
        let item = read_work_item_or_not_found(config.store, config.work_item_id)?;
        let attempt = item
            .attempts
            .iter()
            .find(|attempt| attempt.id == config.attempt_id)
            .ok_or_else(|| anyhow::anyhow!("Attempt {:?} not found", config.attempt_id))?;

        reject_terminal_attempt(attempt.status.clone(), attempt.review_state.clone())?;

        if let Some(task) = attempt
            .tasks
            .iter()
            .find(|task| task.status == TaskStatus::Planned)
        {
            let result = work_task_executor::run_task(WorkTaskRunConfig {
                project_root: config.project_root,
                store: config.store,
                work_item_id: config.work_item_id,
                attempt_id: config.attempt_id,
                task_id: &task.id,
                resolver: config.resolver,
                extra_args: config.extra_args,
                coder_kind: config.coder_kind,
                no_sandbox: config.no_sandbox,
            })?;
            outcomes.push(WorkAttemptRunOutcome::RanTask {
                task_id: result.task_id,
                output: result.output,
            });
            continue;
        }

        if attempt
            .tasks
            .iter()
            .any(|task| task.status == TaskStatus::Executing)
        {
            bail!(
                "Attempt {:?} has an executing Task and cannot be advanced",
                config.attempt_id
            );
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

        if has_completed_write(attempt.tasks.as_slice())
            && !has_review_after_latest_write(attempt.tasks.as_slice())
        {
            let mut item = item;
            let task_ids = item.add_next_review_tasks(config.attempt_id, review::REVIEWERS)?;
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
            let outcome =
                interpret_reviews(config.project_root, config.store, item, config.attempt_id)?;
            let should_stop = matches!(
                outcome,
                WorkAttemptRunOutcome::ReviewsPassed
                    | WorkAttemptRunOutcome::PlannedFollowup { .. }
                    | WorkAttemptRunOutcome::NeedsUser { .. }
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

fn reject_terminal_attempt(
    status: AttemptStatus,
    review_state: Option<AttemptReviewState>,
) -> Result<()> {
    match (status, review_state) {
        (AttemptStatus::Failed, _) => bail!("Attempt is failed and cannot be advanced"),
        (AttemptStatus::NeedsUser, _) => bail!("Attempt needs user input before it can advance"),
        (AttemptStatus::Complete, Some(AttemptReviewState::Passed)) => {
            bail!("Attempt reviews passed; Merge Candidate creation is not implemented yet")
        }
        _ => Ok(()),
    }
}

fn has_completed_write(tasks: &[Task]) -> bool {
    tasks
        .iter()
        .any(|task| task.kind == TaskKind::Write && task.status == TaskStatus::Complete)
}

fn has_open_review_round(tasks: &[Task]) -> bool {
    let mut saw_review = false;
    for task in tasks.iter().rev() {
        match task.kind {
            TaskKind::Write => return false,
            TaskKind::Review => {
                saw_review = true;
                if task.status != TaskStatus::Complete {
                    return true;
                }
            }
            _ => {}
        }
    }
    saw_review
}

fn has_review_after_latest_write(tasks: &[Task]) -> bool {
    let Some(last_write_index) = tasks.iter().rposition(|task| task.kind == TaskKind::Write) else {
        return false;
    };
    tasks[last_write_index + 1..]
        .iter()
        .any(|task| task.kind == TaskKind::Review)
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

fn interpret_reviews(
    project_root: &Path,
    store: &WorkModelStore,
    mut item: WorkItem,
    attempt_id: &str,
) -> Result<WorkAttemptRunOutcome> {
    let attempt_index = item
        .attempts
        .iter()
        .position(|attempt| attempt.id == attempt_id)
        .ok_or_else(|| anyhow::anyhow!("Attempt {:?} not found", attempt_id))?;

    let review_artifacts = latest_review_artifacts(&item.attempts[attempt_index].tasks);
    if review_artifacts.is_empty() {
        bail!("Attempt {:?} has no completed review artifacts", attempt_id);
    }

    let mut failed = Vec::new();
    let mut uncertain = Vec::new();
    for artifact in &review_artifacts {
        let path = project_root.join(&artifact.path);
        let content = fs::read_to_string(&path).unwrap_or_default();
        match review::extract_verdict(&content) {
            Verdict::Pass => {}
            Verdict::Fail => failed.push(artifact.clone()),
            Verdict::Uncertain => uncertain.push(artifact.clone()),
        }
    }

    if !failed.is_empty() {
        item.attempts[attempt_index].review_state = Some(AttemptReviewState::Failed);
        item.attempts[attempt_index].status = AttemptStatus::Planned;
        let task_id = item.add_followup_write_task(attempt_id, failed)?;
        store.write_work_item(&item)?;
        return Ok(WorkAttemptRunOutcome::PlannedFollowup { task_id });
    }

    if !uncertain.is_empty() {
        let handoff_path = write_needs_user_handoff(project_root, attempt_id, &uncertain)?;
        item.attempts[attempt_index].review_state = Some(AttemptReviewState::Uncertain);
        item.attempts[attempt_index].status = AttemptStatus::NeedsUser;
        item.attempts[attempt_index].artifacts.push(ArtifactRef {
            producer_id: "attempt-loop".to_string(),
            path: handoff_path.clone(),
        });
        store.write_work_item(&item)?;
        return Ok(WorkAttemptRunOutcome::NeedsUser { handoff_path });
    }

    item.attempts[attempt_index].review_state = Some(AttemptReviewState::Passed);
    item.attempts[attempt_index].status = AttemptStatus::Complete;
    store.write_work_item(&item)?;
    Ok(WorkAttemptRunOutcome::ReviewsPassed)
}

fn latest_review_artifacts(tasks: &[Task]) -> Vec<ArtifactRef> {
    let Some(last_write_index) = tasks.iter().rposition(|task| task.kind == TaskKind::Write) else {
        return Vec::new();
    };
    tasks[last_write_index + 1..]
        .iter()
        .filter(|task| task.kind == TaskKind::Review && task.status == TaskStatus::Complete)
        .filter_map(|task| {
            task.artifact_area.as_ref().map(|area| ArtifactRef {
                producer_id: task.id.clone(),
                path: format!("{}/review.md", area.path),
            })
        })
        .collect()
}

fn write_needs_user_handoff(
    project_root: &Path,
    attempt_id: &str,
    uncertain: &[ArtifactRef],
) -> Result<String> {
    let relative_path = format!(".factory/work/artifacts/{attempt_id}/needs-user.md");
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
