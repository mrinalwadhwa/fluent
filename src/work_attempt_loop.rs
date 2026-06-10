use anyhow::{Result, bail};
use std::collections::HashSet;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use crate::coder::CoderKind;
use crate::content::ContentResolver;
use crate::review::{self, Verdict};
use crate::work_model::{
    ArtifactRef, Attempt, AttemptKind, AttemptReviewState, AttemptStatus, Task, TaskKind,
    TaskStatus, WorkItem, WorkModelStorageError, WorkModelStore, work_artifact_path,
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
    MergeCandidateReady { candidate_id: String },
    PlannedFollowup { task_id: String },
    NeedsUser { handoff_path: String },
    ReviewOnlyComplete,
    ReviewOnlyFailed,
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

        reject_terminal_attempt(attempt.status.clone())?;

        if attempt.kind != AttemptKind::ReviewOnly
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

        if attempt.kind != AttemptKind::ReviewOnly
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
            let outcome =
                interpret_reviews(config.project_root, config.store, item, config.attempt_id)?;
            let should_stop = matches!(
                outcome,
                WorkAttemptRunOutcome::MergeCandidateReady { .. }
                    | WorkAttemptRunOutcome::PlannedFollowup { .. }
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

fn reject_terminal_attempt(status: AttemptStatus) -> Result<()> {
    match status {
        AttemptStatus::Failed => bail!("Attempt is failed and cannot be advanced"),
        AttemptStatus::NeedsUser => bail!("Attempt needs user input before it can advance"),
        _ => Ok(()),
    }
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
            TaskKind::Review if task.status != TaskStatus::Complete => return true,
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
        .any(|task| task.kind == TaskKind::Review)
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

    if !failed.is_empty() {
        item.attempts[attempt_index].review_state = Some(AttemptReviewState::Failed);
        if item.attempts[attempt_index].kind == AttemptKind::ReviewOnly {
            item.attempts[attempt_index].status = AttemptStatus::Failed;
            store.write_work_item(&item)?;
            return Ok(WorkAttemptRunOutcome::ReviewOnlyFailed);
        }
        item.attempts[attempt_index].status = AttemptStatus::Planned;
        let task_id = item.add_followup_write_task(attempt_id, failed)?;
        store.write_work_item(&item)?;
        return Ok(WorkAttemptRunOutcome::PlannedFollowup { task_id });
    }

    if !uncertain.is_empty() {
        let handoff_path =
            write_needs_user_handoff(project_root, &item.id, attempt_id, &uncertain)?;
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
    if item.attempts[attempt_index].kind == AttemptKind::ReviewOnly {
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
    let start = if attempt.kind == AttemptKind::ReviewOnly {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::work_model::{Attempt, TaskArtifactArea, WorkspaceAccess};

    #[test]
    fn completed_review_round_is_not_open() {
        let tasks = vec![
            Task {
                id: "attempt-1-write".to_string(),
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
                output: None,
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
                output: None,
            },
        ];

        assert!(!has_open_review_round(&tasks));
    }

    #[test]
    fn latest_review_artifacts_rejects_unmanaged_artifact_area() {
        let tasks = vec![
            Task {
                id: "attempt-1-write".to_string(),
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
                output: None,
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
                output: None,
            },
        ];

        let attempt = Attempt {
            id: "attempt-1".to_string(),
            work_item_id: "work-1".to_string(),
            kind: AttemptKind::Write,
            status: AttemptStatus::Complete,
            tasks,
            review_state: Some(AttemptReviewState::Passed),
            artifacts: Vec::new(),
        };

        let error = latest_review_artifacts(Path::new("/tmp/project"), &attempt)
            .expect_err("unmanaged artifact area should fail");
        assert!(
            error
                .to_string()
                .contains("Task artifact area path must stay under .factory/work/artifacts")
        );
    }

    #[test]
    fn initial_write_uses_full_review_roles() {
        let attempt = attempt_with_tasks(vec![write_task("attempt-1-write", Vec::new())]);

        assert_eq!(next_review_roles(&attempt), review::REVIEWERS);
    }

    #[test]
    fn followup_write_uses_failed_input_review_role() {
        let attempt = attempt_with_tasks(vec![
            write_task("attempt-1-write", Vec::new()),
            review_task("attempt-1-review-tests", "tests"),
            write_task(
                "attempt-1-followup-1",
                vec![ArtifactRef {
                    producer_id: "attempt-1-review-tests".to_string(),
                    path:
                        ".factory/work/artifacts/work-1/attempt-1/attempt-1-review-tests/review.md"
                            .to_string(),
                }],
            ),
        ]);

        assert_eq!(next_review_roles(&attempt), vec!["tests"]);
    }

    #[test]
    fn followup_write_uses_failed_roles_in_default_order() {
        let attempt = attempt_with_tasks(vec![
            write_task("attempt-1-write", Vec::new()),
            review_task("attempt-1-review-tests", "tests"),
            review_task("attempt-1-review-documentation", "documentation"),
            write_task(
                "attempt-1-followup-1",
                vec![
                    ArtifactRef {
                        producer_id: "attempt-1-review-tests".to_string(),
                        path: ".factory/work/artifacts/work-1/attempt-1/attempt-1-review-tests/review.md"
                            .to_string(),
                    },
                    ArtifactRef {
                        producer_id: "attempt-1-review-documentation".to_string(),
                        path: ".factory/work/artifacts/work-1/attempt-1/attempt-1-review-documentation/review.md"
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
            write_task("attempt-1-write", Vec::new()),
            review_task("attempt-1-review-tests", "tests"),
            write_task(
                "attempt-1-followup-1",
                vec![ArtifactRef {
                    producer_id: "missing-review-task".to_string(),
                    path: ".factory/work/artifacts/work-1/attempt-1/missing-review-task/review.md"
                        .to_string(),
                }],
            ),
        ]);

        assert_eq!(next_review_roles(&attempt), review::REVIEWERS);
    }

    fn attempt_with_tasks(tasks: Vec<Task>) -> Attempt {
        Attempt {
            id: "attempt-1".to_string(),
            work_item_id: "work-1".to_string(),
            kind: AttemptKind::Write,
            status: AttemptStatus::Planned,
            tasks,
            review_state: Some(AttemptReviewState::NotReviewed),
            artifacts: Vec::new(),
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
            output: None,
        }
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
            output: None,
        }
    }
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
