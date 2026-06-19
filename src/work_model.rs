use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use std::str::FromStr;

use crate::coder::CoderKind;

pub fn now_iso8601() -> String {
    chrono::Utc::now().to_rfc3339()
}

pub const WORK_MODEL_DIR: &str = ".factory/work";
pub const WORK_ITEMS_DIR: &str = "items";
pub const WORK_ATTEMPTS_DIR: &str = "attempts";
pub const WORK_TASKS_DIR: &str = "tasks";
pub const WORK_MERGE_CANDIDATES_DIR: &str = "merge-candidates";
pub const WORK_ARTIFACTS_DIR: &str = ".factory/work/artifacts";

pub fn work_artifact_path(work_item_id: &str, attempt_id: &str, artifact: &str) -> String {
    format!("{WORK_ARTIFACTS_DIR}/{work_item_id}/{attempt_id}/{artifact}")
}

pub fn initial_candidate_workspace_path(work_item_id: &str, attempt_id: &str) -> String {
    format!("../work-{}-{work_item_id}-{attempt_id}", work_item_id.len())
}

pub fn reviewer_workspace_path(work_item_id: &str, attempt_id: &str, reviewer: &str) -> String {
    format!(
        "../review-{}-{work_item_id}-{attempt_id}-{reviewer}",
        work_item_id.len()
    )
}

pub fn resolve_managed_sibling_workspace_path(
    project_root: &Path,
    path: &str,
    subject: &'static str,
) -> Result<PathBuf, ManagedWorkspacePathError> {
    let relative_path = Path::new(path);
    if relative_path.is_absolute() {
        return Err(ManagedWorkspacePathError::new(
            subject,
            path,
            ManagedWorkspacePathErrorKind::Absolute,
        ));
    }

    let mut components = relative_path.components();
    let Some(Component::ParentDir) = components.next() else {
        return Err(ManagedWorkspacePathError::new(
            subject,
            path,
            ManagedWorkspacePathErrorKind::OutsideManagedRoot,
        ));
    };
    let Some(Component::Normal(workspace_name)) = components.next() else {
        return Err(ManagedWorkspacePathError::new(
            subject,
            path,
            ManagedWorkspacePathErrorKind::OutsideManagedRoot,
        ));
    };
    let workspace_name_string = workspace_name.to_string_lossy();
    if !workspace_name_string.starts_with("work-")
        || workspace_name_string.len() <= "work-".len()
        || components.next().is_some()
    {
        return Err(ManagedWorkspacePathError::new(
            subject,
            path,
            ManagedWorkspacePathErrorKind::OutsideManagedRoot,
        ));
    };

    Ok(project_root
        .parent()
        .unwrap_or(project_root)
        .join(workspace_name))
}

pub fn resolve_expected_candidate_workspace_path(
    project_root: &Path,
    path: &str,
    work_item_id: &str,
    attempt_id: &str,
    subject: &'static str,
) -> Result<PathBuf, ManagedWorkspacePathError> {
    let resolved = resolve_managed_sibling_workspace_path(project_root, path, subject)?;
    let expected = initial_candidate_workspace_path(work_item_id, attempt_id);
    if path != expected {
        return Err(ManagedWorkspacePathError::new(
            subject,
            path,
            ManagedWorkspacePathErrorKind::UnexpectedCandidatePath { expected },
        ));
    }

    Ok(resolved)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedWorkspacePathError {
    subject: &'static str,
    path: String,
    kind: ManagedWorkspacePathErrorKind,
}

impl ManagedWorkspacePathError {
    fn new(subject: &'static str, path: &str, kind: ManagedWorkspacePathErrorKind) -> Self {
        Self {
            subject,
            path: path.to_string(),
            kind,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ManagedWorkspacePathErrorKind {
    Absolute,
    OutsideManagedRoot,
    UnexpectedCandidatePath { expected: String },
}

impl fmt::Display for ManagedWorkspacePathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.kind {
            ManagedWorkspacePathErrorKind::Absolute => {
                write!(
                    f,
                    "{} workspace path must be relative: {}",
                    self.subject, self.path
                )
            }
            ManagedWorkspacePathErrorKind::OutsideManagedRoot => {
                write!(
                    f,
                    "{} workspace path must stay under managed sibling workspaces: {}",
                    self.subject, self.path
                )
            }
            ManagedWorkspacePathErrorKind::UnexpectedCandidatePath { ref expected } => {
                write!(
                    f,
                    "{} workspace path must match expected candidate workspace {}: {}",
                    self.subject, expected, self.path
                )
            }
        }
    }
}

impl Error for ManagedWorkspacePathError {}

/// Durable unit of planned Factory work.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkItem {
    pub id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub planning_context: Option<PlanningContext>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub abandonment: Option<WorkItemAbandonment>,
    #[serde(default)]
    pub attempts: Vec<Attempt>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub merge_candidates: Vec<MergeCandidate>,
}

impl WorkItem {
    pub fn write_task_instructions(&self) -> Option<String> {
        self.instructions
            .clone()
            .or_else(|| self.planning_context.as_ref()?.to_instructions())
    }

    pub fn add_initial_attempt(
        &mut self,
        attempt_id: impl Into<String>,
    ) -> Result<(), WorkModelError> {
        self.ensure_not_abandoned()?;
        let attempt_id = attempt_id.into();
        validate_id("work item", &self.id)?;
        validate_id("attempt", &attempt_id)?;
        if self.attempts.iter().any(|attempt| attempt.id == attempt_id) {
            return Err(WorkModelError::AttemptAlreadyExists { id: attempt_id });
        }

        let task_id = format!("{attempt_id}-write-1");
        let workspace_path = initial_candidate_workspace_path(&self.id, &attempt_id);
        let artifact_path = work_artifact_path(&self.id, &attempt_id, &task_id);
        self.attempts.push(Attempt {
            id: attempt_id.clone(),
            work_item_id: self.id.clone(),
            kind: AttemptKind::Write,
            status: AttemptStatus::Planned,
            coder_mapping: CoderMapping::default(),
            tasks: vec![Task {
                id: task_id,
                kind: TaskKind::Write,
                status: TaskStatus::Planned,
                role: "author".to_string(),
                instructions: self.write_task_instructions(),
                work_item_id: self.id.clone(),
                attempt_id: Some(attempt_id.clone()),
                workspace_access: WorkspaceAccess {
                    reads: Vec::new(),
                    writes: vec![WorkspaceRef {
                        id: "candidate".to_string(),
                        path: workspace_path,
                    }],
                },
                artifact_area: Some(TaskArtifactArea {
                    path: artifact_path,
                }),
                review_context: None,
                input_artifacts: Vec::new(),
                depends_on: None,
                output: None,
                created_at: Some(now_iso8601()),
                started_at: None,
                completed_at: None,
            }],
            review_state: None,
            artifacts: Vec::new(),
            created_at: Some(now_iso8601()),
            completed_at: None,
        });

        self.validate()
    }

    pub fn add_review_only_attempt(
        &mut self,
        attempt_id: impl Into<String>,
        roles: &[&str],
        source_ref: impl Into<String>,
        source_commit: impl Into<String>,
    ) -> Result<Vec<String>, WorkModelError> {
        self.ensure_not_abandoned()?;
        let attempt_id = attempt_id.into();
        validate_id("work item", &self.id)?;
        validate_id("attempt", &attempt_id)?;
        if self.attempts.iter().any(|attempt| attempt.id == attempt_id) {
            return Err(WorkModelError::AttemptAlreadyExists { id: attempt_id });
        }

        let source = WorkspaceRef {
            id: "source".to_string(),
            path: ".".to_string(),
        };
        let source_ref = source_ref.into();
        let source_commit = source_commit.into();
        let review_task_instructions = self.write_task_instructions();
        let mut task_ids = Vec::new();
        let mut tasks = Vec::new();
        for role in roles {
            validate_id("review role", role)?;
            let task_id = format!("{attempt_id}-review-{role}");
            validate_id("task", &task_id)?;
            tasks.push(Task {
                id: task_id.clone(),
                kind: TaskKind::Review,
                status: TaskStatus::Planned,
                role: (*role).to_string(),
                instructions: review_task_instructions.clone(),
                work_item_id: self.id.clone(),
                attempt_id: Some(attempt_id.clone()),
                workspace_access: WorkspaceAccess::read_only(vec![source.clone()]),
                artifact_area: Some(TaskArtifactArea {
                    path: work_artifact_path(&self.id, &attempt_id, &task_id),
                }),
                review_context: Some(ReviewContext {
                    candidate_workspace_id: source.id.clone(),
                    candidate_workspace_path: source.path.clone(),
                    source_branch: source_ref.clone(),
                    candidate_commit: source_commit.clone(),
                }),
                input_artifacts: Vec::new(),
                depends_on: None,
                output: None,
                created_at: Some(now_iso8601()),
                started_at: None,
                completed_at: None,
            });
            task_ids.push(task_id);
        }

        self.attempts.push(Attempt {
            id: attempt_id,
            work_item_id: self.id.clone(),
            kind: AttemptKind::ReviewOnly,
            status: AttemptStatus::Reviewing,
            coder_mapping: CoderMapping::default(),
            tasks,
            review_state: Some(AttemptReviewState::NotReviewed),
            artifacts: Vec::new(),
            created_at: Some(now_iso8601()),
            completed_at: None,
        });

        self.validate()?;
        Ok(task_ids)
    }

    pub fn add_post_merge_review_attempt(
        &mut self,
        attempt_id: impl Into<String>,
        roles: &[&str],
        source_ref: impl Into<String>,
        source_commit: impl Into<String>,
    ) -> Result<Vec<String>, WorkModelError> {
        self.ensure_not_abandoned()?;
        let attempt_id = attempt_id.into();
        validate_id("work item", &self.id)?;
        validate_id("attempt", &attempt_id)?;
        if self.attempts.iter().any(|attempt| attempt.id == attempt_id) {
            return Err(WorkModelError::AttemptAlreadyExists { id: attempt_id });
        }

        let source = WorkspaceRef {
            id: "source".to_string(),
            path: ".".to_string(),
        };
        let source_ref = source_ref.into();
        let source_commit = source_commit.into();
        let review_task_instructions = self.write_task_instructions();
        let mut task_ids = Vec::new();
        let mut tasks = Vec::new();
        for role in roles {
            validate_id("review role", role)?;
            let task_id = format!("{attempt_id}-review-{role}");
            validate_id("task", &task_id)?;
            tasks.push(Task {
                id: task_id.clone(),
                kind: TaskKind::Review,
                status: TaskStatus::Planned,
                role: (*role).to_string(),
                instructions: review_task_instructions.clone(),
                work_item_id: self.id.clone(),
                attempt_id: Some(attempt_id.clone()),
                workspace_access: WorkspaceAccess::read_only(vec![source.clone()]),
                artifact_area: Some(TaskArtifactArea {
                    path: work_artifact_path(&self.id, &attempt_id, &task_id),
                }),
                review_context: Some(ReviewContext {
                    candidate_workspace_id: source.id.clone(),
                    candidate_workspace_path: source.path.clone(),
                    source_branch: source_ref.clone(),
                    candidate_commit: source_commit.clone(),
                }),
                input_artifacts: Vec::new(),
                depends_on: None,
                output: None,
                created_at: Some(now_iso8601()),
                started_at: None,
                completed_at: None,
            });
            task_ids.push(task_id);
        }

        self.attempts.push(Attempt {
            id: attempt_id,
            work_item_id: self.id.clone(),
            kind: AttemptKind::PostMergeReview,
            status: AttemptStatus::Reviewing,
            coder_mapping: CoderMapping::default(),
            tasks,
            review_state: Some(AttemptReviewState::NotReviewed),
            artifacts: Vec::new(),
            created_at: Some(now_iso8601()),
            completed_at: None,
        });

        self.validate()?;
        Ok(task_ids)
    }

    pub fn add_review_tasks(
        &mut self,
        attempt_id: &str,
        roles: &[&str],
    ) -> Result<Vec<String>, WorkModelError> {
        self.add_review_tasks_with_round(attempt_id, roles, None)
    }

    pub fn add_next_review_tasks(
        &mut self,
        attempt_id: &str,
        roles: &[&str],
    ) -> Result<Vec<String>, WorkModelError> {
        if roles.is_empty() {
            return Ok(Vec::new());
        }
        let Some(attempt) = self
            .attempts
            .iter()
            .find(|attempt| attempt.id == attempt_id)
        else {
            return Err(WorkModelError::AttemptNotFound {
                id: attempt_id.to_string(),
            });
        };
        let latest_review_round = attempt
            .tasks
            .iter()
            .filter(|task| task.kind == TaskKind::Review)
            .filter_map(|task| review_task_round(attempt_id, task))
            .max()
            .unwrap_or(0);
        let next_round = latest_review_round + 1;
        let round = (next_round > 1).then_some(next_round);
        self.add_review_tasks_with_round(attempt_id, roles, round)
    }

    fn add_review_tasks_with_round(
        &mut self,
        attempt_id: &str,
        roles: &[&str],
        round: Option<usize>,
    ) -> Result<Vec<String>, WorkModelError> {
        self.ensure_not_abandoned()?;
        let Some(attempt) = self
            .attempts
            .iter_mut()
            .find(|attempt| attempt.id == attempt_id)
        else {
            return Err(WorkModelError::AttemptNotFound {
                id: attempt_id.to_string(),
            });
        };

        let Some(latest_write) = attempt
            .tasks
            .iter()
            .rev()
            .find(|task| task.kind == TaskKind::Write && task.status == TaskStatus::Complete)
            .cloned()
        else {
            return Err(WorkModelError::AttemptMissingCompletedWriteTask {
                attempt_id: attempt_id.to_string(),
            });
        };
        let Some(write_output) = latest_write.output.as_ref().cloned() else {
            return Err(WorkModelError::AttemptMissingCompletedWriteTask {
                attempt_id: attempt_id.to_string(),
            });
        };
        let review_input_artifacts = review_input_artifacts_by_role(attempt, &latest_write);

        let candidate = WorkspaceRef {
            id: write_output.workspace_id.clone(),
            path: write_output.workspace_path.clone(),
        };
        let mut task_ids = Vec::new();

        let tester_task_id = {
            let tester_id = match round {
                Some(round) => format!("{attempt_id}-tester-{round}"),
                None => format!("{attempt_id}-tester"),
            };
            validate_id("task", &tester_id)?;
            if attempt.tasks.iter().any(|task| task.id == tester_id) {
                return Err(WorkModelError::TaskAlreadyExists { id: tester_id });
            }
            attempt.tasks.push(Task {
                id: tester_id.clone(),
                kind: TaskKind::Tester,
                status: TaskStatus::Planned,
                role: "tester".to_string(),
                instructions: None,
                work_item_id: self.id.clone(),
                attempt_id: Some(attempt_id.to_string()),
                workspace_access: WorkspaceAccess::read_only(vec![candidate.clone()]),
                artifact_area: Some(TaskArtifactArea {
                    path: work_artifact_path(&self.id, attempt_id, &tester_id),
                }),
                review_context: Some(ReviewContext {
                    candidate_workspace_id: write_output.workspace_id.clone(),
                    candidate_workspace_path: write_output.workspace_path.clone(),
                    source_branch: write_output.source_branch.clone(),
                    candidate_commit: write_output.commit.clone(),
                }),
                input_artifacts: Vec::new(),
                depends_on: None,
                output: None,
                created_at: Some(now_iso8601()),
                started_at: None,
                completed_at: None,
            });
            task_ids.push(tester_id.clone());
            tester_id
        };

        for role in roles {
            validate_id("review role", role)?;
            let task_id = match round {
                Some(round) => format!("{attempt_id}-review-{round}-{role}"),
                None => format!("{attempt_id}-review-{role}"),
            };
            validate_id("task", &task_id)?;
            if attempt.tasks.iter().any(|task| task.id == task_id) {
                return Err(WorkModelError::TaskAlreadyExists { id: task_id });
            }
            let mut task_input_artifacts = review_input_artifacts
                .get(*role)
                .cloned()
                .unwrap_or_default();
            task_input_artifacts.push(ArtifactRef {
                producer_id: tester_task_id.clone(),
                path: format!(
                    "{}/tester-results.json",
                    work_artifact_path(&self.id, attempt_id, &tester_task_id)
                ),
            });
            let progress_md_path = format!(
                "{WORK_ARTIFACTS_DIR}/{}/{}/progress.md",
                self.id, attempt_id,
            );
            task_input_artifacts.push(ArtifactRef {
                producer_id: "writer".to_string(),
                path: progress_md_path,
            });
            attempt.tasks.push(Task {
                id: task_id.clone(),
                kind: TaskKind::Review,
                status: TaskStatus::Planned,
                role: (*role).to_string(),
                instructions: None,
                work_item_id: self.id.clone(),
                attempt_id: Some(attempt_id.to_string()),
                workspace_access: WorkspaceAccess::read_only(vec![candidate.clone()]),
                artifact_area: Some(TaskArtifactArea {
                    path: work_artifact_path(&self.id, attempt_id, &task_id),
                }),
                review_context: Some(ReviewContext {
                    candidate_workspace_id: write_output.workspace_id.clone(),
                    candidate_workspace_path: write_output.workspace_path.clone(),
                    source_branch: write_output.source_branch.clone(),
                    candidate_commit: write_output.commit.clone(),
                }),
                input_artifacts: task_input_artifacts,
                depends_on: Some(tester_task_id.clone()),
                output: None,
                created_at: Some(now_iso8601()),
                started_at: None,
                completed_at: None,
            });
            task_ids.push(task_id);
        }
        attempt.status = AttemptStatus::Reviewing;
        attempt.review_state = Some(AttemptReviewState::NotReviewed);

        self.validate()?;
        Ok(task_ids)
    }

    pub fn add_next_write_round(
        &mut self,
        attempt_id: &str,
        input_artifacts: Vec<ArtifactRef>,
    ) -> Result<String, WorkModelError> {
        self.ensure_not_abandoned()?;
        let write_task_instructions = self.write_task_instructions();
        let Some(attempt) = self
            .attempts
            .iter_mut()
            .find(|attempt| attempt.id == attempt_id)
        else {
            return Err(WorkModelError::AttemptNotFound {
                id: attempt_id.to_string(),
            });
        };

        let Some(write_output) = attempt
            .tasks
            .iter()
            .rev()
            .find(|task| task.kind == TaskKind::Write && task.status == TaskStatus::Complete)
            .and_then(|task| task.output.as_ref())
            .cloned()
        else {
            return Err(WorkModelError::AttemptMissingCompletedWriteTask {
                attempt_id: attempt_id.to_string(),
            });
        };

        let next_round = attempt
            .tasks
            .iter()
            .filter(|task| task.kind == TaskKind::Write)
            .count()
            + 1;
        let task_id = format!("{attempt_id}-write-{next_round}");
        validate_id("task", &task_id)?;
        if attempt.tasks.iter().any(|task| task.id == task_id) {
            return Err(WorkModelError::TaskAlreadyExists { id: task_id });
        }

        attempt.tasks.push(Task {
            id: task_id.clone(),
            kind: TaskKind::Write,
            status: TaskStatus::Planned,
            role: "author".to_string(),
            instructions: write_task_instructions,
            work_item_id: self.id.clone(),
            attempt_id: Some(attempt_id.to_string()),
            workspace_access: WorkspaceAccess {
                reads: Vec::new(),
                writes: vec![WorkspaceRef {
                    id: write_output.workspace_id,
                    path: write_output.workspace_path,
                }],
            },
            artifact_area: Some(TaskArtifactArea {
                path: work_artifact_path(&self.id, attempt_id, &task_id),
            }),
            review_context: None,
            input_artifacts,
            depends_on: None,
            output: None,
            created_at: Some(now_iso8601()),
            started_at: None,
            completed_at: None,
        });
        attempt.status = AttemptStatus::Planned;
        attempt.review_state = Some(AttemptReviewState::Failed);

        self.validate()?;
        Ok(task_id)
    }

    pub fn create_or_get_merge_candidate(
        &mut self,
        attempt_id: &str,
    ) -> Result<String, WorkModelError> {
        self.ensure_not_abandoned()?;
        let candidate_id = format!("{attempt_id}-merge-candidate");
        validate_id("merge candidate", &candidate_id)?;
        if let Some(candidate) = self
            .merge_candidates
            .iter()
            .find(|candidate| candidate.attempt_id == attempt_id)
        {
            self.validate()?;
            return Ok(candidate.id.clone());
        }

        let Some(attempt) = self
            .attempts
            .iter()
            .find(|attempt| attempt.id == attempt_id)
        else {
            return Err(WorkModelError::AttemptNotFound {
                id: attempt_id.to_string(),
            });
        };
        if attempt.status != AttemptStatus::Complete
            || attempt.review_state != Some(AttemptReviewState::Passed)
        {
            return Err(WorkModelError::AttemptReviewsNotPassed {
                attempt_id: attempt_id.to_string(),
            });
        }
        let Some((write_task, write_output)) = attempt
            .tasks
            .iter()
            .rev()
            .find(|task| task.kind == TaskKind::Write && task.status == TaskStatus::Complete)
            .and_then(|task| task.output.as_ref().map(|output| (task, output)))
        else {
            return Err(WorkModelError::AttemptMissingCompletedWriteTask {
                attempt_id: attempt_id.to_string(),
            });
        };
        let target_workspace = write_task
            .workspace_access
            .reads
            .first()
            .cloned()
            .unwrap_or_else(|| WorkspaceRef {
                id: "target".to_string(),
                path: ".".to_string(),
            });

        if self
            .merge_candidates
            .iter()
            .any(|candidate| candidate.id == candidate_id)
        {
            return Err(WorkModelError::MergeCandidateAlreadyExists { id: candidate_id });
        }

        self.merge_candidates.push(MergeCandidate {
            id: candidate_id.clone(),
            attempt_id: attempt_id.to_string(),
            source_workspace: WorkspaceRef {
                id: write_output.workspace_id.clone(),
                path: write_output.workspace_path.clone(),
            },
            target_workspace,
            source_branch: write_output.source_branch.clone(),
            target_branch: write_output.source_branch.clone(),
            candidate_commit: write_output.commit.clone(),
            review_state: MergeCandidateReviewState::Pending,
            merge_state: MergeCandidateMergeState::default(),
            created_at: Some(now_iso8601()),
            started_at: None,
            completed_at: None,
        });

        self.validate()?;
        Ok(candidate_id)
    }

    pub fn abandon(&mut self, reason: Option<String>) -> Result<(), WorkModelError> {
        if let Some(attempt) = self.attempts.iter().find(|attempt| {
            matches!(
                attempt.status,
                AttemptStatus::Executing | AttemptStatus::Reviewing
            )
        }) {
            return Err(WorkModelError::WorkItemAbandonmentBlocked {
                work_item_id: self.id.clone(),
                reason: format!("Attempt {:?} is {}", attempt.id, attempt.status.as_str()),
            });
        }
        if let Some(task) = self
            .attempts
            .iter()
            .flat_map(|attempt| attempt.tasks.iter())
            .find(|task| task.status == TaskStatus::Executing)
        {
            return Err(WorkModelError::WorkItemAbandonmentBlocked {
                work_item_id: self.id.clone(),
                reason: format!("Task {:?} is executing", task.id),
            });
        }
        if let Some(candidate) = self.merge_candidates.iter().find(|candidate| {
            candidate.review_state == MergeCandidateReviewState::Reviewing
                || candidate.merge_state.status == MergeCandidateMergeStatus::Executing
        }) {
            return Err(WorkModelError::WorkItemAbandonmentBlocked {
                work_item_id: self.id.clone(),
                reason: format!("Merge Candidate {:?} is active", candidate.id),
            });
        }

        self.abandonment = Some(WorkItemAbandonment {
            reason: reason.and_then(|reason| {
                let reason = reason.trim().to_string();
                (!reason.is_empty()).then_some(reason)
            }),
        });
        self.validate()
    }

    pub fn next_attempt_id(&self) -> String {
        let used: HashSet<usize> = self
            .attempts
            .iter()
            .filter_map(|a| a.id.strip_prefix("attempt-")?.parse::<usize>().ok())
            .collect();
        let mut n = 1;
        while used.contains(&n) {
            n += 1;
        }
        format!("attempt-{n}")
    }

    pub fn latest_attempt_id(&self) -> Option<&str> {
        self.attempts.last().map(|a| a.id.as_str())
    }

    pub fn latest_merge_candidate_id(&self) -> Option<&str> {
        self.merge_candidates.last().map(|c| c.id.as_str())
    }

    pub fn ensure_not_abandoned(&self) -> Result<(), WorkModelError> {
        if self.abandonment.is_some() {
            return Err(WorkModelError::WorkItemAbandoned {
                work_item_id: self.id.clone(),
            });
        }
        Ok(())
    }

    pub fn validate(&self) -> Result<(), WorkModelError> {
        for attempt in &self.attempts {
            if attempt.work_item_id != self.id {
                return Err(WorkModelError::AttemptWorkItemMismatch {
                    attempt_id: attempt.id.clone(),
                    expected: self.id.clone(),
                    actual: attempt.work_item_id.clone(),
                });
            }
            attempt.validate(&self.id)?;
        }
        let mut merge_candidate_ids = HashSet::new();
        let mut merge_candidate_attempts = HashSet::new();
        for candidate in &self.merge_candidates {
            if !merge_candidate_ids.insert(candidate.id.as_str()) {
                return Err(WorkModelError::MergeCandidateAlreadyExists {
                    id: candidate.id.clone(),
                });
            }
            if !merge_candidate_attempts.insert(candidate.attempt_id.as_str()) {
                return Err(WorkModelError::MergeCandidateAttemptAlreadyExists {
                    attempt_id: candidate.attempt_id.clone(),
                });
            }
        }
        for candidate in &self.merge_candidates {
            candidate.validate(self)?;
        }
        Ok(())
    }
}

/// Durable marker that a Work Item was explicitly abandoned.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkItemAbandonment {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Approved planning context attached directly to a Work Item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct PlanningContext {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brief: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub behaviors: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approach: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub combined: Option<String>,
}

impl PlanningContext {
    pub fn to_instructions(&self) -> Option<String> {
        if let Some(combined) = non_empty_clone(&self.combined) {
            return Some(combined);
        }

        let mut sections = Vec::new();
        push_planning_section(&mut sections, "Brief", &self.brief);
        push_planning_section(&mut sections, "Behaviors", &self.behaviors);
        push_planning_section(&mut sections, "Approach", &self.approach);
        push_planning_section(&mut sections, "Plan", &self.plan);
        (!sections.is_empty()).then(|| sections.join("\n\n"))
    }

    pub fn is_empty(&self) -> bool {
        self.brief
            .as_ref()
            .is_none_or(|value| value.trim().is_empty())
            && self
                .behaviors
                .as_ref()
                .is_none_or(|value| value.trim().is_empty())
            && self
                .approach
                .as_ref()
                .is_none_or(|value| value.trim().is_empty())
            && self
                .plan
                .as_ref()
                .is_none_or(|value| value.trim().is_empty())
            && self
                .combined
                .as_ref()
                .is_none_or(|value| value.trim().is_empty())
    }
}

pub fn work_behavior_review_input(item: &WorkItem) -> String {
    let behavior_increment = item
        .planning_context
        .as_ref()
        .and_then(|context| context.behaviors.as_ref())
        .filter(|behaviors| !behaviors.trim().is_empty());

    match behavior_increment {
        Some(behaviors) => format!(
            "Work behavior review input:\nThe Work Item includes this behavior increment. Verify it against the codebase:\n\n{}\n",
            behaviors.trim()
        ),
        None => {
            "Work behavior review input:\nNo Work behavior increment was provided for this Work Item.\n".to_string()
        }
    }
}

fn non_empty_clone(value: &Option<String>) -> Option<String> {
    value
        .as_ref()
        .filter(|value| !value.trim().is_empty())
        .cloned()
}

fn push_planning_section(sections: &mut Vec<String>, title: &str, content: &Option<String>) {
    if let Some(content) = non_empty_clone(content) {
        sections.push(format!("# {title}\n\n{}", content.trim()));
    }
}

/// A (CoderKind, model name) pair for one Task kind in a coder mapping.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoderModelPair {
    pub coder: CoderKind,
    pub model: String,
}

/// Per-Task-kind coder mapping stored on each Attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoderMapping {
    pub write: CoderModelPair,
    pub review: CoderModelPair,
    #[serde(rename = "behavior-tests")]
    pub behavior_tests: CoderModelPair,
}

impl Default for CoderMapping {
    fn default() -> Self {
        let default_pair = CoderModelPair {
            coder: CoderKind::Claude,
            model: String::new(),
        };
        Self {
            write: default_pair.clone(),
            review: default_pair.clone(),
            behavior_tests: default_pair,
        }
    }
}

impl CoderMapping {
    pub fn for_task_kind(&self, kind: TaskKind) -> &CoderModelPair {
        match kind {
            TaskKind::Write => &self.write,
            TaskKind::Review => &self.review,
            TaskKind::BehaviorTests => &self.behavior_tests,
            TaskKind::Tester => &self.write,
            _ => &self.write,
        }
    }
}

/// Inputs for resolving a CoderMapping at Attempt creation time.
#[derive(Debug, Default)]
pub struct CoderMappingInputs {
    pub write_coder: Option<String>,
    pub write_model: Option<String>,
    pub review_coder: Option<String>,
    pub review_model: Option<String>,
    pub behavior_tests_coder: Option<String>,
    pub behavior_tests_model: Option<String>,
    pub global_coder: Option<String>,
}

impl CoderMappingInputs {
    pub fn from_env() -> Self {
        Self {
            write_coder: std::env::var("FACTORY_WRITE_CODER").ok(),
            write_model: std::env::var("FACTORY_WRITE_MODEL").ok(),
            review_coder: std::env::var("FACTORY_REVIEW_CODER").ok(),
            review_model: std::env::var("FACTORY_REVIEW_MODEL").ok(),
            behavior_tests_coder: std::env::var("FACTORY_BEHAVIOR_TESTS_CODER").ok(),
            behavior_tests_model: std::env::var("FACTORY_BEHAVIOR_TESTS_MODEL").ok(),
            global_coder: std::env::var("FACTORY_CODER").ok(),
        }
    }

    pub fn merge_cli(
        mut self,
        write_coder: Option<String>,
        write_model: Option<String>,
        review_coder: Option<String>,
        review_model: Option<String>,
        behavior_tests_coder: Option<String>,
        behavior_tests_model: Option<String>,
        global_coder: Option<String>,
    ) -> Self {
        if write_coder.is_some() {
            self.write_coder = write_coder;
        }
        if write_model.is_some() {
            self.write_model = write_model;
        }
        if review_coder.is_some() {
            self.review_coder = review_coder;
        }
        if review_model.is_some() {
            self.review_model = review_model;
        }
        if behavior_tests_coder.is_some() {
            self.behavior_tests_coder = behavior_tests_coder;
        }
        if behavior_tests_model.is_some() {
            self.behavior_tests_model = behavior_tests_model;
        }
        if global_coder.is_some() {
            self.global_coder = global_coder;
        }
        self
    }
}

/// Resolve a fully-populated CoderMapping from CLI flags, env vars, and defaults.
///
/// Precedence per Task kind:
/// 1. Per-Task-kind CLI flag / env var
/// 2. Global `FACTORY_CODER` / per-Coder model env var
/// 3. Coder's built-in default
pub fn resolve_coder_mapping(inputs: &CoderMappingInputs) -> Result<CoderMapping, anyhow::Error> {
    let global_kind = inputs
        .global_coder
        .as_deref()
        .map(|s| CoderKind::resolve(Some(s)))
        .transpose()?;

    let resolve_pair = |task_coder: &Option<String>,
                        task_model: &Option<String>|
     -> Result<CoderModelPair, anyhow::Error> {
        let coder = if let Some(c) = task_coder {
            CoderKind::resolve(Some(c))?
        } else {
            global_kind.unwrap_or(CoderKind::Claude)
        };

        let model = if let Some(m) = task_model {
            m.clone()
        } else {
            coder.default_model()
        };

        Ok(CoderModelPair { coder, model })
    };

    Ok(CoderMapping {
        write: resolve_pair(&inputs.write_coder, &inputs.write_model)?,
        review: resolve_pair(&inputs.review_coder, &inputs.review_model)?,
        behavior_tests: resolve_pair(&inputs.behavior_tests_coder, &inputs.behavior_tests_model)?,
    })
}

/// One execution history branch for a work item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attempt {
    pub id: String,
    pub work_item_id: String,
    #[serde(default, skip_serializing_if = "attempt_kind_is_write")]
    pub kind: AttemptKind,
    pub status: AttemptStatus,
    #[serde(default)]
    pub coder_mapping: CoderMapping,
    #[serde(default)]
    pub tasks: Vec<Task>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub review_state: Option<AttemptReviewState>,
    #[serde(default)]
    pub artifacts: Vec<ArtifactRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
}

impl Attempt {
    pub fn validate(&self, work_item_id: &str) -> Result<(), WorkModelError> {
        for task in &self.tasks {
            if task.work_item_id != work_item_id {
                return Err(WorkModelError::TaskWorkItemMismatch {
                    task_id: task.id.clone(),
                    expected: work_item_id.to_string(),
                    actual: task.work_item_id.clone(),
                });
            }
            if task.attempt_id.as_deref() != Some(self.id.as_str()) {
                return Err(WorkModelError::TaskAttemptMismatch {
                    task_id: task.id.clone(),
                    expected: self.id.clone(),
                    actual: task.attempt_id.clone(),
                });
            }
        }
        if self.kind.is_review_only_like() {
            self.validate_review_only_shape(work_item_id)?;
        }
        for task in &self.tasks {
            task.validate()?;
            if self.status == AttemptStatus::Complete
                && task.status != TaskStatus::Complete
                && task.kind != TaskKind::Rebase
                && task.kind != TaskKind::BehaviorTests
            {
                return Err(WorkModelError::CompleteAttemptHasIncompleteTask {
                    attempt_id: self.id.clone(),
                    task_id: task.id.clone(),
                    task_status: task.status.clone(),
                });
            }
        }
        Ok(())
    }

    fn validate_review_only_shape(&self, work_item_id: &str) -> Result<(), WorkModelError> {
        for task in &self.tasks {
            if task.kind != TaskKind::Review {
                return Err(WorkModelError::ReviewOnlyAttemptInvalidTask {
                    attempt_id: self.id.clone(),
                    task_id: task.id.clone(),
                    field: "kind",
                });
            }
            if task.workspace_access.reads.len() != 1
                || task.workspace_access.reads[0].id != "source"
                || task.workspace_access.reads[0].path != "."
            {
                return Err(WorkModelError::ReviewOnlyAttemptInvalidTask {
                    attempt_id: self.id.clone(),
                    task_id: task.id.clone(),
                    field: "workspace_access.reads",
                });
            }
            let Some(review_context) = task.review_context.as_ref() else {
                return Err(WorkModelError::ReviewOnlyAttemptInvalidTask {
                    attempt_id: self.id.clone(),
                    task_id: task.id.clone(),
                    field: "review_context",
                });
            };
            if review_context.candidate_workspace_id != "source"
                || review_context.candidate_workspace_path != "."
            {
                return Err(WorkModelError::ReviewOnlyAttemptInvalidTask {
                    attempt_id: self.id.clone(),
                    task_id: task.id.clone(),
                    field: "review_context.candidate_workspace",
                });
            }
            let expected_artifact_path = work_artifact_path(work_item_id, &self.id, &task.id);
            if task
                .artifact_area
                .as_ref()
                .is_none_or(|area| area.path != expected_artifact_path)
            {
                return Err(WorkModelError::ReviewOnlyAttemptInvalidTask {
                    attempt_id: self.id.clone(),
                    task_id: task.id.clone(),
                    field: "artifact_area.path",
                });
            }
        }
        Ok(())
    }
}

/// What an attempt is expected to produce.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum AttemptKind {
    #[default]
    Write,
    ReviewOnly,
    PostMergeReview,
}

impl AttemptKind {
    pub fn is_review_only_like(&self) -> bool {
        matches!(self, Self::ReviewOnly | Self::PostMergeReview)
    }

    pub fn is_source_checkout_review(&self) -> bool {
        matches!(self, Self::ReviewOnly | Self::PostMergeReview)
    }
}

fn attempt_kind_is_write(kind: &AttemptKind) -> bool {
    *kind == AttemptKind::Write
}

/// Coarse attempt lifecycle state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AttemptStatus {
    Planned,
    Executing,
    Reviewing,
    Complete,
    Failed,
    NeedsUser,
}

impl AttemptStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Planned => "planned",
            Self::Executing => "executing",
            Self::Reviewing => "reviewing",
            Self::Complete => "complete",
            Self::Failed => "failed",
            Self::NeedsUser => "needs-user",
        }
    }
}

/// Review state attached to an attempt as a whole.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AttemptReviewState {
    NotReviewed,
    Passed,
    Failed,
    Uncertain,
}

impl AttemptReviewState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::NotReviewed => "not-reviewed",
            Self::Passed => "passed",
            Self::Failed => "failed",
            Self::Uncertain => "uncertain",
        }
    }
}

/// Schedulable unit of work.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub kind: TaskKind,
    #[serde(default, skip_serializing_if = "task_status_is_planned")]
    pub status: TaskStatus,
    pub role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    pub work_item_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attempt_id: Option<String>,
    pub workspace_access: WorkspaceAccess,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact_area: Option<TaskArtifactArea>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub review_context: Option<ReviewContext>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub input_artifacts: Vec<ArtifactRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depends_on: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<TaskOutput>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
}

impl Task {
    pub fn validate(&self) -> Result<(), WorkModelError> {
        self.workspace_access.validate()?;
        if self.status == TaskStatus::Complete {
            if self.kind == TaskKind::Write && self.output.is_none() {
                return Err(WorkModelError::CompleteWriteTaskMissingOutput {
                    task_id: self.id.clone(),
                });
            }
        } else if self.output.is_some() {
            return Err(WorkModelError::IncompleteTaskHasOutput {
                task_id: self.id.clone(),
                status: self.status.clone(),
            });
        }
        if self.kind == TaskKind::Review && !self.workspace_access.writes.is_empty() {
            return Err(WorkModelError::ReviewTaskWritesWorkspace {
                task_id: self.id.clone(),
            });
        }
        if self.kind == TaskKind::Review && self.artifact_area.is_none() {
            return Err(WorkModelError::ReviewTaskMissingArtifactArea {
                task_id: self.id.clone(),
            });
        }
        if self.kind == TaskKind::Review && self.workspace_access.reads.is_empty() {
            return Err(WorkModelError::ReviewTaskMissingReadableWorkspace {
                task_id: self.id.clone(),
            });
        }
        if self.kind == TaskKind::Review {
            let review_context = self.review_context.as_ref().ok_or_else(|| {
                WorkModelError::ReviewTaskMissingContext {
                    task_id: self.id.clone(),
                }
            })?;
            let candidate_is_readable = self.workspace_access.reads.iter().any(|workspace| {
                workspace.id == review_context.candidate_workspace_id
                    && workspace.path == review_context.candidate_workspace_path
            });
            if !candidate_is_readable {
                return Err(WorkModelError::ReviewTaskContextCandidateNotReadable {
                    task_id: self.id.clone(),
                });
            }
        }
        Ok(())
    }
}

/// Coarse task lifecycle state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum TaskStatus {
    #[default]
    Planned,
    Executing,
    Complete,
    Failed,
    NeedsUser,
}

impl TaskStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Planned => "planned",
            Self::Executing => "executing",
            Self::Complete => "complete",
            Self::Failed => "failed",
            Self::NeedsUser => "needs-user",
        }
    }
}

impl fmt::Display for TaskStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

fn task_status_is_planned(status: &TaskStatus) -> bool {
    *status == TaskStatus::Planned
}

/// Generic scheduler-facing task kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TaskKind {
    Write,
    Review,
    Merge,
    Rebase,
    Report,
    Learn,
    Probe,
    BehaviorTests,
    Tester,
}

impl TaskKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Write => "write",
            Self::Review => "review",
            Self::Merge => "merge",
            Self::Rebase => "rebase",
            Self::Report => "report",
            Self::Learn => "learn",
            Self::Probe => "probe",
            Self::BehaviorTests => "behavior-tests",
            Self::Tester => "tester",
        }
    }
}

impl FromStr for TaskKind {
    type Err = ParseTaskKindError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim() {
            "write" => Ok(Self::Write),
            "review" => Ok(Self::Review),
            "merge" => Ok(Self::Merge),
            "rebase" => Ok(Self::Rebase),
            "report" => Ok(Self::Report),
            "learn" => Ok(Self::Learn),
            "probe" => Ok(Self::Probe),
            "behavior-tests" => Ok(Self::BehaviorTests),
            "tester" => Ok(Self::Tester),
            other => Err(ParseTaskKindError(other.to_string())),
        }
    }
}

impl fmt::Display for TaskKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Filesystem/git access a task requests.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceAccess {
    #[serde(default)]
    pub reads: Vec<WorkspaceRef>,
    #[serde(default)]
    pub writes: Vec<WorkspaceRef>,
}

impl WorkspaceAccess {
    pub fn read_only(reads: Vec<WorkspaceRef>) -> Self {
        Self {
            reads,
            writes: Vec::new(),
        }
    }

    pub fn validate(&self) -> Result<(), WorkModelError> {
        if self.writes.len() > 1 {
            return Err(WorkModelError::MultipleWriteWorkspaces {
                count: self.writes.len(),
            });
        }
        Ok(())
    }
}

/// Factory-managed filesystem/git context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceRef {
    pub id: String,
    pub path: String,
}

/// Area where a task may write its own artifacts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskArtifactArea {
    pub path: String,
}

/// Review scope derived from the write Task that produced a candidate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewContext {
    pub candidate_workspace_id: String,
    pub candidate_workspace_path: String,
    pub source_branch: String,
    pub candidate_commit: String,
}

/// Durable output produced by a completed task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskOutput {
    pub workspace_id: String,
    pub workspace_path: String,
    pub source_branch: String,
    pub commit: String,
}

/// Pointer to durable output from a task or attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactRef {
    pub producer_id: String,
    pub path: String,
}

/// Candidate merge result and its own review state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergeCandidate {
    pub id: String,
    pub attempt_id: String,
    pub source_workspace: WorkspaceRef,
    pub target_workspace: WorkspaceRef,
    pub source_branch: String,
    pub target_branch: String,
    pub candidate_commit: String,
    pub review_state: MergeCandidateReviewState,
    #[serde(default)]
    pub merge_state: MergeCandidateMergeState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
}

impl MergeCandidate {
    pub fn validate(&self, work_item: &WorkItem) -> Result<(), WorkModelError> {
        validate_id("merge candidate", &self.id)?;
        let Some(attempt) = work_item
            .attempts
            .iter()
            .find(|attempt| attempt.id == self.attempt_id)
        else {
            return Err(WorkModelError::MergeCandidateAttemptNotFound {
                candidate_id: self.id.clone(),
                attempt_id: self.attempt_id.clone(),
            });
        };
        let Some((write_task, write_output)) = attempt
            .tasks
            .iter()
            .rev()
            .find(|task| task.kind == TaskKind::Write && task.status == TaskStatus::Complete)
            .and_then(|task| task.output.as_ref().map(|output| (task, output)))
        else {
            return Err(WorkModelError::MergeCandidateMissingCompletedWriteTask {
                candidate_id: self.id.clone(),
                attempt_id: self.attempt_id.clone(),
            });
        };
        let expected_target_workspace = write_task
            .workspace_access
            .reads
            .first()
            .cloned()
            .unwrap_or_else(|| WorkspaceRef {
                id: "target".to_string(),
                path: ".".to_string(),
            });
        if self.source_workspace.id != write_output.workspace_id {
            return Err(WorkModelError::MergeCandidateProvenanceMismatch {
                candidate_id: self.id.clone(),
                field: "source_workspace.id",
            });
        }
        if self.source_workspace.path != write_output.workspace_path {
            return Err(WorkModelError::MergeCandidateProvenanceMismatch {
                candidate_id: self.id.clone(),
                field: "source_workspace.path",
            });
        }
        if self.target_workspace.id != expected_target_workspace.id {
            return Err(WorkModelError::MergeCandidateProvenanceMismatch {
                candidate_id: self.id.clone(),
                field: "target_workspace.id",
            });
        }
        if self.target_workspace.path != expected_target_workspace.path {
            return Err(WorkModelError::MergeCandidateProvenanceMismatch {
                candidate_id: self.id.clone(),
                field: "target_workspace.path",
            });
        }
        if self.source_branch != write_output.source_branch {
            return Err(WorkModelError::MergeCandidateProvenanceMismatch {
                candidate_id: self.id.clone(),
                field: "source_branch",
            });
        }
        if self.target_branch != write_output.source_branch {
            return Err(WorkModelError::MergeCandidateProvenanceMismatch {
                candidate_id: self.id.clone(),
                field: "target_branch",
            });
        }
        if self.candidate_commit != write_output.commit {
            return Err(WorkModelError::MergeCandidateProvenanceMismatch {
                candidate_id: self.id.clone(),
                field: "candidate_commit",
            });
        }
        if self.merge_state.status != MergeCandidateMergeStatus::Failed
            && (attempt.status != AttemptStatus::Complete
                || attempt.review_state != Some(AttemptReviewState::Passed))
        {
            return Err(WorkModelError::MergeCandidateAttemptReviewsNotPassed {
                candidate_id: self.id.clone(),
                attempt_id: self.attempt_id.clone(),
            });
        }
        Ok(())
    }
}

/// Review state attached to a merge candidate, not to the attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MergeCandidateReviewState {
    Pending,
    Reviewing,
    Passed,
    Failed,
}

/// Durable merge execution state for a merge candidate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergeCandidateMergeState {
    pub status: MergeCandidateMergeStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub merged_commit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub check_artifacts: Vec<ArtifactRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub review_artifacts: Vec<ArtifactRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_merge_skipped: Option<bool>,
}

impl Default for MergeCandidateMergeState {
    fn default() -> Self {
        Self {
            status: MergeCandidateMergeStatus::Pending,
            merged_commit: None,
            failure_reason: None,
            check_artifacts: Vec::new(),
            review_artifacts: Vec::new(),
            auto_merge_skipped: None,
        }
    }
}

/// Coarse merge execution status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum MergeCandidateMergeStatus {
    #[default]
    Pending,
    Executing,
    Failed,
    NeedsUser,
    Merged,
}

pub fn mark_task_started(task: &mut Task) {
    task.started_at.get_or_insert_with(now_iso8601);
}

pub fn set_task_terminal(task: &mut Task, status: TaskStatus) {
    debug_assert!(matches!(
        status,
        TaskStatus::Complete | TaskStatus::Failed | TaskStatus::NeedsUser
    ));
    task.status = status;
    task.completed_at.get_or_insert_with(now_iso8601);
}

pub fn set_attempt_terminal(attempt: &mut Attempt, status: AttemptStatus) {
    debug_assert!(matches!(
        status,
        AttemptStatus::Complete | AttemptStatus::Failed | AttemptStatus::NeedsUser
    ));
    attempt.status = status;
    attempt.completed_at.get_or_insert_with(now_iso8601);
}

pub fn mark_merge_candidate_started(candidate: &mut MergeCandidate) {
    candidate.started_at.get_or_insert_with(now_iso8601);
}

pub fn set_merge_candidate_terminal(
    candidate: &mut MergeCandidate,
    status: MergeCandidateMergeStatus,
) {
    debug_assert!(matches!(
        status,
        MergeCandidateMergeStatus::Merged
            | MergeCandidateMergeStatus::Failed
            | MergeCandidateMergeStatus::NeedsUser
    ));
    candidate.completed_at.get_or_insert_with(now_iso8601);
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkModelError {
    InvalidId {
        kind: &'static str,
        id: String,
    },
    AttemptAlreadyExists {
        id: String,
    },
    AttemptNotFound {
        id: String,
    },
    AttemptMissingCompletedWriteTask {
        attempt_id: String,
    },
    AttemptReviewsNotPassed {
        attempt_id: String,
    },
    TaskAlreadyExists {
        id: String,
    },
    TaskOrderAlreadyExists {
        attempt_id: String,
        order: usize,
    },
    MergeCandidateAlreadyExists {
        id: String,
    },
    MergeCandidateAttemptAlreadyExists {
        attempt_id: String,
    },
    MergeCandidateAttemptNotFound {
        candidate_id: String,
        attempt_id: String,
    },
    MergeCandidateAttemptReviewsNotPassed {
        candidate_id: String,
        attempt_id: String,
    },
    MergeCandidateMissingCompletedWriteTask {
        candidate_id: String,
        attempt_id: String,
    },
    MergeCandidateProvenanceMismatch {
        candidate_id: String,
        field: &'static str,
    },
    WorkItemAbandoned {
        work_item_id: String,
    },
    MultipleWriteWorkspaces {
        count: usize,
    },
    ReviewTaskWritesWorkspace {
        task_id: String,
    },
    ReviewTaskMissingArtifactArea {
        task_id: String,
    },
    ReviewTaskMissingReadableWorkspace {
        task_id: String,
    },
    ReviewTaskMissingContext {
        task_id: String,
    },
    ReviewTaskContextCandidateNotReadable {
        task_id: String,
    },
    ReviewOnlyAttemptInvalidTask {
        attempt_id: String,
        task_id: String,
        field: &'static str,
    },
    AttemptWorkItemMismatch {
        attempt_id: String,
        expected: String,
        actual: String,
    },
    TaskWorkItemMismatch {
        task_id: String,
        expected: String,
        actual: String,
    },
    TaskAttemptMismatch {
        task_id: String,
        expected: String,
        actual: Option<String>,
    },
    CompleteWriteTaskMissingOutput {
        task_id: String,
    },
    IncompleteTaskHasOutput {
        task_id: String,
        status: TaskStatus,
    },
    CompleteAttemptHasIncompleteTask {
        attempt_id: String,
        task_id: String,
        task_status: TaskStatus,
    },
    WorkItemAbandonmentBlocked {
        work_item_id: String,
        reason: String,
    },
}

impl fmt::Display for WorkModelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidId { kind, id } => {
                write!(f, "{kind} id {id:?} cannot be used as a file name")
            }
            Self::AttemptAlreadyExists { id } => {
                write!(f, "Attempt {id:?} already exists")
            }
            Self::AttemptNotFound { id } => {
                write!(f, "Attempt {id:?} not found")
            }
            Self::AttemptMissingCompletedWriteTask { attempt_id } => {
                write!(
                    f,
                    "Attempt {attempt_id:?} needs a completed write Task before review Tasks can be planned"
                )
            }
            Self::AttemptReviewsNotPassed { attempt_id } => {
                write!(
                    f,
                    "Attempt {attempt_id:?} must have passed reviews before creating a Merge Candidate"
                )
            }
            Self::TaskAlreadyExists { id } => {
                write!(f, "Task {id:?} already exists")
            }
            Self::TaskOrderAlreadyExists { attempt_id, order } => {
                write!(
                    f,
                    "Attempt {attempt_id:?} has multiple Tasks at order {order}"
                )
            }
            Self::MergeCandidateAlreadyExists { id } => {
                write!(f, "Merge Candidate {id:?} already exists")
            }
            Self::MergeCandidateAttemptAlreadyExists { attempt_id } => {
                write!(f, "Attempt {attempt_id:?} already has a Merge Candidate")
            }
            Self::MergeCandidateAttemptNotFound {
                candidate_id,
                attempt_id,
            } => {
                write!(
                    f,
                    "Merge Candidate {candidate_id:?} references missing Attempt {attempt_id:?}"
                )
            }
            Self::MergeCandidateAttemptReviewsNotPassed {
                candidate_id,
                attempt_id,
            } => {
                write!(
                    f,
                    "Merge Candidate {candidate_id:?} references Attempt {attempt_id:?} before reviews passed"
                )
            }
            Self::MergeCandidateMissingCompletedWriteTask {
                candidate_id,
                attempt_id,
            } => {
                write!(
                    f,
                    "Merge Candidate {candidate_id:?} references Attempt {attempt_id:?} without a completed write Task"
                )
            }
            Self::MergeCandidateProvenanceMismatch {
                candidate_id,
                field,
            } => {
                write!(
                    f,
                    "Merge Candidate {candidate_id:?} {field} does not match the latest completed write Task"
                )
            }
            Self::WorkItemAbandoned { work_item_id } => {
                write!(f, "Work Item {work_item_id:?} is abandoned")
            }
            Self::MultipleWriteWorkspaces { count } => {
                write!(f, "task writes {count} workspaces; at most one is allowed")
            }
            Self::ReviewTaskWritesWorkspace { task_id } => {
                write!(f, "review task {task_id} cannot write a workspace")
            }
            Self::ReviewTaskMissingArtifactArea { task_id } => {
                write!(f, "review task {task_id} must declare an artifact area")
            }
            Self::ReviewTaskMissingReadableWorkspace { task_id } => {
                write!(
                    f,
                    "review task {task_id} must declare at least one readable workspace"
                )
            }
            Self::ReviewTaskMissingContext { task_id } => {
                write!(f, "review task {task_id} must declare review context")
            }
            Self::ReviewTaskContextCandidateNotReadable { task_id } => {
                write!(
                    f,
                    "review task {task_id} review context candidate must match a readable workspace"
                )
            }
            Self::ReviewOnlyAttemptInvalidTask {
                attempt_id,
                task_id,
                field,
            } => {
                write!(
                    f,
                    "review-only Attempt {attempt_id:?} task {task_id} has invalid {field}"
                )
            }
            Self::AttemptWorkItemMismatch {
                attempt_id,
                expected,
                actual,
            } => {
                write!(
                    f,
                    "attempt {attempt_id} belongs to work item {actual}; expected {expected}"
                )
            }
            Self::TaskWorkItemMismatch {
                task_id,
                expected,
                actual,
            } => {
                write!(
                    f,
                    "task {task_id} belongs to work item {actual}; expected {expected}"
                )
            }
            Self::TaskAttemptMismatch {
                task_id,
                expected,
                actual,
            } => match actual {
                Some(actual) => {
                    write!(
                        f,
                        "task {task_id} belongs to attempt {actual}; expected {expected}"
                    )
                }
                None => write!(f, "task {task_id} must belong to attempt {expected}"),
            },
            Self::CompleteWriteTaskMissingOutput { task_id } => {
                write!(f, "completed write task {task_id} must record output")
            }
            Self::IncompleteTaskHasOutput { task_id, status } => {
                write!(f, "task {task_id} has output but status is {status}")
            }
            Self::CompleteAttemptHasIncompleteTask {
                attempt_id,
                task_id,
                task_status,
            } => {
                write!(
                    f,
                    "complete attempt {attempt_id} contains task {task_id} with status {task_status}"
                )
            }
            Self::WorkItemAbandonmentBlocked {
                work_item_id,
                reason,
            } => {
                write!(
                    f,
                    "Work Item {work_item_id:?} cannot be abandoned: {reason}"
                )
            }
        }
    }
}

impl Error for WorkModelError {}

#[derive(Debug)]
pub enum WorkModelStorageError {
    InvalidWorkItemId {
        id: String,
    },
    CreateDirectory {
        path: PathBuf,
        source: io::Error,
    },
    ReadDirectory {
        path: PathBuf,
        source: io::Error,
    },
    ReadFile {
        path: PathBuf,
        source: io::Error,
    },
    WriteFile {
        path: PathBuf,
        source: io::Error,
    },
    WorkItemAlreadyExists {
        path: PathBuf,
        id: String,
    },
    ParseFile {
        path: PathBuf,
        source: serde_json::Error,
    },
    WorkItemIdMismatch {
        path: PathBuf,
        expected: String,
        actual: String,
    },
    InvalidModel {
        path: PathBuf,
        source: WorkModelError,
    },
}

impl WorkModelStorageError {
    pub fn path(&self) -> Option<&Path> {
        match self {
            Self::InvalidWorkItemId { .. } => None,
            Self::CreateDirectory { path, .. }
            | Self::ReadDirectory { path, .. }
            | Self::ReadFile { path, .. }
            | Self::WriteFile { path, .. }
            | Self::WorkItemAlreadyExists { path, .. }
            | Self::ParseFile { path, .. }
            | Self::WorkItemIdMismatch { path, .. }
            | Self::InvalidModel { path, .. } => Some(path),
        }
    }
}

impl fmt::Display for WorkModelStorageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidWorkItemId { id } => {
                write!(f, "work item id {id:?} cannot be used as a file name")
            }
            Self::CreateDirectory { path, source } => {
                write!(f, "failed to create {}: {source}", path.display())
            }
            Self::ReadDirectory { path, source } => {
                write!(f, "failed to read {}: {source}", path.display())
            }
            Self::ReadFile { path, source } => {
                write!(f, "failed to read {}: {source}", path.display())
            }
            Self::WriteFile { path, source } => {
                write!(f, "failed to write {}: {source}", path.display())
            }
            Self::WorkItemAlreadyExists { id, .. } => {
                write!(f, "Work Item {id:?} already exists")
            }
            Self::ParseFile { path, source } => {
                write!(f, "failed to parse {}: {source}", path.display())
            }
            Self::WorkItemIdMismatch {
                path,
                expected,
                actual,
            } => {
                write!(
                    f,
                    "work item file {} contains id {actual}; expected {expected}",
                    path.display()
                )
            }
            Self::InvalidModel { path, source } => {
                write!(f, "invalid work model in {}: {source}", path.display())
            }
        }
    }
}

impl Error for WorkModelStorageError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InvalidWorkItemId { .. } => None,
            Self::CreateDirectory { source, .. }
            | Self::ReadDirectory { source, .. }
            | Self::ReadFile { source, .. }
            | Self::WriteFile { source, .. } => Some(source),
            Self::WorkItemAlreadyExists { .. } => None,
            Self::ParseFile { source, .. } => Some(source),
            Self::WorkItemIdMismatch { .. } => None,
            Self::InvalidModel { source, .. } => Some(source),
        }
    }
}

#[derive(Debug, Clone)]
pub struct WorkModelStore {
    project_root: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct WorkItemRecord {
    id: String,
    title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    planning_context: Option<PlanningContext>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    instructions: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    abandonment: Option<WorkItemAbandonment>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct AttemptRecord {
    id: String,
    work_item_id: String,
    #[serde(default, skip_serializing_if = "attempt_kind_is_write")]
    kind: AttemptKind,
    #[serde(default)]
    order: usize,
    status: AttemptStatus,
    #[serde(default)]
    coder_mapping: CoderMapping,
    #[serde(skip_serializing_if = "Option::is_none")]
    review_state: Option<AttemptReviewState>,
    #[serde(default)]
    artifacts: Vec<ArtifactRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    created_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    completed_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct TaskRecord {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    order: Option<usize>,
    #[serde(flatten)]
    task: Task,
}

impl From<&WorkItem> for WorkItemRecord {
    fn from(work_item: &WorkItem) -> Self {
        Self {
            id: work_item.id.clone(),
            title: work_item.title.clone(),
            planning_context: work_item.planning_context.clone(),
            instructions: work_item.instructions.clone(),
            abandonment: work_item.abandonment.clone(),
        }
    }
}

impl From<WorkItemRecord> for WorkItem {
    fn from(record: WorkItemRecord) -> Self {
        Self {
            id: record.id,
            title: record.title,
            planning_context: record.planning_context,
            instructions: record.instructions,
            abandonment: record.abandonment,
            attempts: Vec::new(),
            merge_candidates: Vec::new(),
        }
    }
}

impl AttemptRecord {
    fn from_attempt(attempt: &Attempt, order: usize) -> Self {
        Self {
            id: attempt.id.clone(),
            work_item_id: attempt.work_item_id.clone(),
            kind: attempt.kind.clone(),
            order,
            status: attempt.status.clone(),
            coder_mapping: attempt.coder_mapping.clone(),
            review_state: attempt.review_state.clone(),
            artifacts: attempt.artifacts.clone(),
            created_at: attempt.created_at.clone(),
            completed_at: attempt.completed_at.clone(),
        }
    }
}

impl From<AttemptRecord> for Attempt {
    fn from(record: AttemptRecord) -> Self {
        Self {
            id: record.id,
            work_item_id: record.work_item_id,
            kind: record.kind,
            status: record.status,
            coder_mapping: record.coder_mapping,
            tasks: Vec::new(),
            review_state: record.review_state,
            artifacts: record.artifacts,
            created_at: record.created_at,
            completed_at: record.completed_at,
        }
    }
}

impl TaskRecord {
    fn from_task(task: &Task, order: usize) -> Self {
        Self {
            order: Some(order),
            task: task.clone(),
        }
    }

    fn order_key(&self, attempt_id: &str) -> (usize, usize, usize, String) {
        self.order
            .map(|order| (0, order, 0, self.task.id.clone()))
            .unwrap_or_else(|| {
                let (group, role_order, id) = task_order_key(attempt_id, &self.task);
                (1, group, role_order, id)
            })
    }
}

impl WorkModelStore {
    pub fn new(project_root: impl Into<PathBuf>) -> Self {
        Self {
            project_root: project_root.into(),
        }
    }

    pub fn work_dir(&self) -> PathBuf {
        self.project_root.join(WORK_MODEL_DIR)
    }

    pub fn work_items_dir(&self) -> PathBuf {
        self.work_dir().join(WORK_ITEMS_DIR)
    }

    pub fn work_attempts_dir(&self) -> PathBuf {
        self.work_dir().join(WORK_ATTEMPTS_DIR)
    }

    pub fn work_tasks_dir(&self) -> PathBuf {
        self.work_dir().join(WORK_TASKS_DIR)
    }

    pub fn work_merge_candidates_dir(&self) -> PathBuf {
        self.work_dir().join(WORK_MERGE_CANDIDATES_DIR)
    }

    pub fn work_item_path(&self, id: &str) -> Result<PathBuf, WorkModelStorageError> {
        work_item_file_name(id).map(|file_name| self.work_items_dir().join(file_name))
    }

    pub fn work_attempt_path(
        &self,
        work_item_id: &str,
        attempt_id: &str,
    ) -> Result<PathBuf, WorkModelStorageError> {
        let work_item_dir = object_dir_name(work_item_id)?;
        let attempt_file = object_file_name(attempt_id)?;
        Ok(self
            .work_attempts_dir()
            .join(work_item_dir)
            .join(attempt_file))
    }

    pub fn work_task_path(
        &self,
        work_item_id: &str,
        attempt_id: &str,
        task_id: &str,
    ) -> Result<PathBuf, WorkModelStorageError> {
        let work_item_dir = object_dir_name(work_item_id)?;
        let attempt_dir = object_dir_name(attempt_id)?;
        let task_file = object_file_name(task_id)?;
        Ok(self
            .work_tasks_dir()
            .join(work_item_dir)
            .join(attempt_dir)
            .join(task_file))
    }

    pub fn work_merge_candidate_path(
        &self,
        work_item_id: &str,
        candidate_id: &str,
    ) -> Result<PathBuf, WorkModelStorageError> {
        let work_item_dir = object_dir_name(work_item_id)?;
        let candidate_file = object_file_name(candidate_id)?;
        Ok(self
            .work_merge_candidates_dir()
            .join(work_item_dir)
            .join(candidate_file))
    }

    pub fn list_work_items(&self) -> Result<Vec<WorkItem>, WorkModelStorageError> {
        self.list_work_item_paths()?
            .into_iter()
            .map(|path| self.read_work_item_file(&path, true))
            .collect()
    }

    pub fn list_work_item_results(
        &self,
    ) -> Result<Vec<Result<WorkItem, WorkModelStorageError>>, WorkModelStorageError> {
        Ok(self
            .list_work_item_paths()?
            .into_iter()
            .map(|path| self.read_work_item_file(&path, true))
            .collect())
    }

    fn list_work_item_paths(&self) -> Result<Vec<PathBuf>, WorkModelStorageError> {
        let dir = self.work_items_dir();
        if !dir.exists() {
            return Ok(Vec::new());
        }

        let mut paths = Vec::new();
        let entries =
            fs::read_dir(&dir).map_err(|source| WorkModelStorageError::ReadDirectory {
                path: dir.clone(),
                source,
            })?;
        for entry in entries {
            let entry = entry.map_err(|source| WorkModelStorageError::ReadDirectory {
                path: dir.clone(),
                source,
            })?;
            let path = entry.path();
            if path
                .extension()
                .is_some_and(|extension| extension == "json")
            {
                paths.push(path);
            }
        }
        paths.sort();
        Ok(paths)
    }

    pub fn read_work_item(&self, id: &str) -> Result<WorkItem, WorkModelStorageError> {
        let path = self.work_item_path(id)?;
        self.read_work_item_file(&path, true)
    }

    pub(crate) fn read_work_item_for_merge_recovery(
        &self,
        id: &str,
    ) -> Result<WorkItem, WorkModelStorageError> {
        let path = self.work_item_path(id)?;
        self.read_work_item_file(&path, false)
    }

    pub fn create_work_item(&self, work_item: &WorkItem) -> Result<(), WorkModelStorageError> {
        self.write_work_item_file(work_item, true)
    }

    pub fn write_work_item(&self, work_item: &WorkItem) -> Result<(), WorkModelStorageError> {
        self.write_work_item_file(work_item, false)
    }

    fn write_work_item_file(
        &self,
        work_item: &WorkItem,
        create_new: bool,
    ) -> Result<(), WorkModelStorageError> {
        let path = self.work_item_path(&work_item.id)?;
        work_item
            .validate()
            .map_err(|source| WorkModelStorageError::InvalidModel {
                path: path.clone(),
                source,
            })?;

        self.write_work_item_file_unchecked(work_item, create_new)
    }

    fn write_work_item_file_unchecked(
        &self,
        work_item: &WorkItem,
        create_new: bool,
    ) -> Result<(), WorkModelStorageError> {
        let path = self.work_item_path(&work_item.id)?;
        let dir = self.work_items_dir();
        fs::create_dir_all(&dir)
            .map_err(|source| WorkModelStorageError::CreateDirectory { path: dir, source })?;

        let record = WorkItemRecord::from(work_item);
        let json = to_json_pretty(&record).map_err(|source| WorkModelStorageError::ParseFile {
            path: path.clone(),
            source,
        })?;
        if create_new {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .map_err(|source| {
                    if source.kind() == io::ErrorKind::AlreadyExists {
                        WorkModelStorageError::WorkItemAlreadyExists {
                            path: path.clone(),
                            id: work_item.id.clone(),
                        }
                    } else {
                        WorkModelStorageError::WriteFile {
                            path: path.clone(),
                            source,
                        }
                    }
                })?;
            file.write_all(json.as_bytes())
                .map_err(|source| WorkModelStorageError::WriteFile {
                    path: path.clone(),
                    source,
                })?;
        } else {
            fs::write(&path, json).map_err(|source| WorkModelStorageError::WriteFile {
                path: path.clone(),
                source,
            })?;
        }

        self.write_attempt_records(work_item)?;
        self.write_merge_candidate_records(work_item)?;
        Ok(())
    }

    fn read_work_item_file(
        &self,
        path: &Path,
        validate: bool,
    ) -> Result<WorkItem, WorkModelStorageError> {
        let content =
            fs::read_to_string(path).map_err(|source| WorkModelStorageError::ReadFile {
                path: path.to_path_buf(),
                source,
            })?;
        let record: WorkItemRecord =
            from_json(&content).map_err(|source| WorkModelStorageError::ParseFile {
                path: path.to_path_buf(),
                source,
            })?;
        if let Some(expected) = path.file_stem().and_then(|stem| stem.to_str()) {
            work_item_file_name(expected)?;
            if record.id != expected {
                return Err(WorkModelStorageError::WorkItemIdMismatch {
                    path: path.to_path_buf(),
                    expected: expected.to_string(),
                    actual: record.id.clone(),
                });
            }
        }
        let mut work_item = self.assemble_split_work_item(record, validate)?;
        self.normalize_work_artifact_paths(&mut work_item)?;
        if validate {
            work_item
                .validate()
                .map_err(|source| WorkModelStorageError::InvalidModel {
                    path: path.to_path_buf(),
                    source,
                })?;
        }
        Ok(work_item)
    }

    fn assemble_split_work_item(
        &self,
        record: WorkItemRecord,
        validate: bool,
    ) -> Result<WorkItem, WorkModelStorageError> {
        let work_item_id = record.id.clone();

        let mut work_item = WorkItem::from(record);
        work_item.attempts = self.read_attempt_records(&work_item_id)?;
        self.reject_task_records_without_attempt(&work_item_id, &work_item.attempts)?;
        work_item.merge_candidates =
            self.read_merge_candidate_records(&work_item_id, &work_item, validate)?;
        Ok(work_item)
    }

    fn write_attempt_records(&self, work_item: &WorkItem) -> Result<(), WorkModelStorageError> {
        let attempts_dir = self.work_attempts_dir().join(&work_item.id);
        let tasks_item_dir = self.work_tasks_dir().join(&work_item.id);
        fs::create_dir_all(&attempts_dir).map_err(|source| {
            WorkModelStorageError::CreateDirectory {
                path: attempts_dir.clone(),
                source,
            }
        })?;
        fs::create_dir_all(&tasks_item_dir).map_err(|source| {
            WorkModelStorageError::CreateDirectory {
                path: tasks_item_dir.clone(),
                source,
            }
        })?;

        let mut attempt_files = HashSet::new();
        let mut attempt_dirs = HashSet::new();
        for (order, attempt) in work_item.attempts.iter().enumerate() {
            let attempt_path = self.work_attempt_path(&work_item.id, &attempt.id)?;
            let record = AttemptRecord::from_attempt(attempt, order);
            write_json_file(&attempt_path, &record)?;
            attempt_files.insert(attempt_path);

            let task_dir = tasks_item_dir.join(object_dir_name(&attempt.id)?);
            fs::create_dir_all(&task_dir).map_err(|source| {
                WorkModelStorageError::CreateDirectory {
                    path: task_dir.clone(),
                    source,
                }
            })?;
            attempt_dirs.insert(task_dir.clone());
            let mut task_files = HashSet::new();
            for (order, task) in attempt.tasks.iter().enumerate() {
                let task_path = self.work_task_path(&work_item.id, &attempt.id, &task.id)?;
                let record = TaskRecord::from_task(task, order);
                write_json_file(&task_path, &record)?;
                task_files.insert(task_path);
            }
            prune_json_files(&task_dir, &task_files)?;
        }
        prune_json_files(&attempts_dir, &attempt_files)?;
        prune_child_dirs(&tasks_item_dir, &attempt_dirs)?;
        Ok(())
    }

    fn write_merge_candidate_records(
        &self,
        work_item: &WorkItem,
    ) -> Result<(), WorkModelStorageError> {
        let candidates_dir = self.work_merge_candidates_dir().join(&work_item.id);
        fs::create_dir_all(&candidates_dir).map_err(|source| {
            WorkModelStorageError::CreateDirectory {
                path: candidates_dir.clone(),
                source,
            }
        })?;
        let mut candidate_files = HashSet::new();
        for candidate in &work_item.merge_candidates {
            let candidate_path = self.work_merge_candidate_path(&work_item.id, &candidate.id)?;
            write_json_file(&candidate_path, candidate)?;
            candidate_files.insert(candidate_path);
        }
        prune_json_files(&candidates_dir, &candidate_files)?;
        Ok(())
    }

    fn read_attempt_records(
        &self,
        work_item_id: &str,
    ) -> Result<Vec<Attempt>, WorkModelStorageError> {
        let attempts_dir = self.work_attempts_dir().join(work_item_id);
        if !attempts_dir.exists() {
            return Ok(Vec::new());
        }

        let mut attempts = Vec::new();
        for path in list_json_paths(&attempts_dir)? {
            let record = read_json_file::<AttemptRecord>(&path)?;
            let order = record.order;
            let mut attempt: Attempt = record.into();
            let expected = file_stem_id(&path)?;
            if attempt.id != expected {
                return Err(WorkModelStorageError::InvalidModel {
                    path,
                    source: WorkModelError::AttemptNotFound { id: expected },
                });
            }
            if attempt.work_item_id != work_item_id {
                return Err(WorkModelStorageError::InvalidModel {
                    path,
                    source: WorkModelError::AttemptWorkItemMismatch {
                        attempt_id: attempt.id,
                        expected: work_item_id.to_string(),
                        actual: attempt.work_item_id,
                    },
                });
            }
            attempt.tasks = self.read_task_records(work_item_id, &attempt.id)?;
            attempts.push((order, path, attempt));
        }
        attempts.sort_by(|(left_order, left_path, _), (right_order, right_path, _)| {
            left_order
                .cmp(right_order)
                .then_with(|| left_path.cmp(right_path))
        });
        Ok(attempts
            .into_iter()
            .map(|(_, _, attempt)| attempt)
            .collect())
    }

    fn read_task_records(
        &self,
        work_item_id: &str,
        attempt_id: &str,
    ) -> Result<Vec<Task>, WorkModelStorageError> {
        let tasks_dir = self.work_tasks_dir().join(work_item_id).join(attempt_id);
        if !tasks_dir.exists() {
            return Ok(Vec::new());
        }

        let mut tasks = Vec::new();
        let mut task_orders = HashSet::new();
        for path in list_json_paths(&tasks_dir)? {
            let record: TaskRecord = read_json_file(&path)?;
            let order_key = record.order_key(attempt_id);
            let task = record.task;
            let expected = file_stem_id(&path)?;
            if task.id != expected {
                return Err(WorkModelStorageError::InvalidModel {
                    path,
                    source: WorkModelError::TaskAlreadyExists { id: expected },
                });
            }
            if task.work_item_id != work_item_id {
                return Err(WorkModelStorageError::InvalidModel {
                    path,
                    source: WorkModelError::TaskWorkItemMismatch {
                        task_id: task.id,
                        expected: work_item_id.to_string(),
                        actual: task.work_item_id,
                    },
                });
            }
            if task.attempt_id.as_deref() != Some(attempt_id) {
                return Err(WorkModelStorageError::InvalidModel {
                    path,
                    source: WorkModelError::TaskAttemptMismatch {
                        task_id: task.id,
                        expected: attempt_id.to_string(),
                        actual: task.attempt_id,
                    },
                });
            }
            task.validate()
                .map_err(|source| WorkModelStorageError::InvalidModel {
                    path: path.clone(),
                    source,
                })?;
            if let Some(order) = record.order
                && !task_orders.insert(order)
            {
                return Err(WorkModelStorageError::InvalidModel {
                    path,
                    source: WorkModelError::TaskOrderAlreadyExists {
                        attempt_id: attempt_id.to_string(),
                        order,
                    },
                });
            }
            tasks.push((order_key, task));
        }
        tasks.sort_by(|(left_order, left_task), (right_order, right_task)| {
            left_order
                .cmp(right_order)
                .then_with(|| left_task.id.cmp(&right_task.id))
        });
        Ok(tasks.into_iter().map(|(_, task)| task).collect())
    }

    fn reject_task_records_without_attempt(
        &self,
        work_item_id: &str,
        attempts: &[Attempt],
    ) -> Result<(), WorkModelStorageError> {
        let tasks_item_dir = self.work_tasks_dir().join(work_item_id);
        if !tasks_item_dir.exists() {
            return Ok(());
        }

        let attempt_ids: HashSet<&str> =
            attempts.iter().map(|attempt| attempt.id.as_str()).collect();
        let entries = fs::read_dir(&tasks_item_dir).map_err(|source| {
            WorkModelStorageError::ReadDirectory {
                path: tasks_item_dir.clone(),
                source,
            }
        })?;
        for entry in entries {
            let entry = entry.map_err(|source| WorkModelStorageError::ReadDirectory {
                path: tasks_item_dir.clone(),
                source,
            })?;
            let path = entry.path();
            if !path.is_dir() || !self.collection_has_json_records(&path)? {
                continue;
            }
            let attempt_id = path
                .file_name()
                .and_then(|name| name.to_str())
                .map(str::to_string)
                .ok_or_else(|| WorkModelStorageError::InvalidWorkItemId {
                    id: path.display().to_string(),
                })?;
            object_dir_name(&attempt_id)?;
            if !attempt_ids.contains(attempt_id.as_str()) {
                return Err(WorkModelStorageError::InvalidModel {
                    path,
                    source: WorkModelError::AttemptNotFound { id: attempt_id },
                });
            }
        }
        Ok(())
    }

    fn collection_has_json_records(&self, dir: &Path) -> Result<bool, WorkModelStorageError> {
        if !dir.exists() {
            return Ok(false);
        }
        let entries = fs::read_dir(dir).map_err(|source| WorkModelStorageError::ReadDirectory {
            path: dir.to_path_buf(),
            source,
        })?;
        for entry in entries {
            let entry = entry.map_err(|source| WorkModelStorageError::ReadDirectory {
                path: dir.to_path_buf(),
                source,
            })?;
            let path = entry.path();
            if path.is_dir() {
                if self.collection_has_json_records(&path)? {
                    return Ok(true);
                }
            } else if path
                .extension()
                .is_some_and(|extension| extension == "json")
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn read_merge_candidate_records(
        &self,
        work_item_id: &str,
        work_item: &WorkItem,
        validate: bool,
    ) -> Result<Vec<MergeCandidate>, WorkModelStorageError> {
        let candidates_dir = self.work_merge_candidates_dir().join(work_item_id);
        if !candidates_dir.exists() {
            return Ok(Vec::new());
        }

        let mut candidates = Vec::new();
        for path in list_json_paths(&candidates_dir)? {
            let candidate: MergeCandidate = read_json_file(&path)?;
            let expected = file_stem_id(&path)?;
            if candidate.id != expected {
                return Err(WorkModelStorageError::InvalidModel {
                    path,
                    source: WorkModelError::MergeCandidateAlreadyExists { id: expected },
                });
            }
            if validate {
                candidate.validate(work_item).map_err(|source| {
                    WorkModelStorageError::InvalidModel {
                        path: path.clone(),
                        source,
                    }
                })?;
            }
            candidates.push(candidate);
        }
        Ok(candidates)
    }

    fn normalize_work_artifact_paths(
        &self,
        work_item: &mut WorkItem,
    ) -> Result<(), WorkModelStorageError> {
        for attempt in &mut work_item.attempts {
            for task in &mut attempt.tasks {
                if let Some(area) = &mut task.artifact_area {
                    self.normalize_artifact_path_value(&work_item.id, &attempt.id, &mut area.path)?;
                }
                for artifact in &mut task.input_artifacts {
                    self.normalize_artifact_path_value(
                        &work_item.id,
                        &attempt.id,
                        &mut artifact.path,
                    )?;
                }
            }
            for artifact in &mut attempt.artifacts {
                self.normalize_artifact_path_value(&work_item.id, &attempt.id, &mut artifact.path)?;
            }
        }
        for candidate in &mut work_item.merge_candidates {
            for artifact in candidate
                .merge_state
                .check_artifacts
                .iter_mut()
                .chain(candidate.merge_state.review_artifacts.iter_mut())
            {
                self.normalize_artifact_path_value(
                    &work_item.id,
                    &candidate.attempt_id,
                    &mut artifact.path,
                )?;
            }
        }
        Ok(())
    }

    fn normalize_artifact_path_value(
        &self,
        work_item_id: &str,
        attempt_id: &str,
        path: &mut String,
    ) -> Result<(), WorkModelStorageError> {
        let Some(normalized) = namespace_legacy_artifact_path(work_item_id, attempt_id, path)
        else {
            return Ok(());
        };
        self.migrate_artifact_path(path, &normalized)?;
        *path = normalized;
        Ok(())
    }

    fn migrate_artifact_path(
        &self,
        old_relative: &str,
        new_relative: &str,
    ) -> Result<(), WorkModelStorageError> {
        let old_path = self.project_root.join(old_relative);
        if !old_path.exists() {
            return Ok(());
        }
        let new_path = self.project_root.join(new_relative);
        if new_path.exists() {
            return Ok(());
        }
        if let Some(parent) = new_path.parent() {
            fs::create_dir_all(parent).map_err(|source| {
                WorkModelStorageError::CreateDirectory {
                    path: parent.to_path_buf(),
                    source,
                }
            })?;
        }
        fs::rename(&old_path, &new_path).map_err(|source| WorkModelStorageError::WriteFile {
            path: new_path,
            source,
        })
    }
}

fn namespace_legacy_artifact_path(
    work_item_id: &str,
    attempt_id: &str,
    path: &str,
) -> Option<String> {
    let prefix = format!("{WORK_ARTIFACTS_DIR}/");
    let rest = path.strip_prefix(&prefix)?;
    if rest
        .split('/')
        .next()
        .is_some_and(|segment| segment == work_item_id)
    {
        return None;
    }
    let legacy_rest = rest.strip_prefix(attempt_id)?.strip_prefix('/')?;
    Some(format!("{prefix}{work_item_id}/{attempt_id}/{legacy_rest}"))
}

fn work_item_file_name(id: &str) -> Result<String, WorkModelStorageError> {
    object_file_name(id)
}

fn object_dir_name(id: &str) -> Result<String, WorkModelStorageError> {
    if !is_file_safe_id(id) {
        return Err(WorkModelStorageError::InvalidWorkItemId { id: id.to_string() });
    }
    Ok(id.to_string())
}

fn object_file_name(id: &str) -> Result<String, WorkModelStorageError> {
    object_dir_name(id).map(|id| format!("{id}.json"))
}

fn file_stem_id(path: &Path) -> Result<String, WorkModelStorageError> {
    let Some(id) = path.file_stem().and_then(|stem| stem.to_str()) else {
        return Err(WorkModelStorageError::InvalidWorkItemId {
            id: path.display().to_string(),
        });
    };
    object_dir_name(id)
}

fn list_json_paths(dir: &Path) -> Result<Vec<PathBuf>, WorkModelStorageError> {
    let mut paths = Vec::new();
    let entries = fs::read_dir(dir).map_err(|source| WorkModelStorageError::ReadDirectory {
        path: dir.to_path_buf(),
        source,
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| WorkModelStorageError::ReadDirectory {
            path: dir.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        if path
            .extension()
            .is_some_and(|extension| extension == "json")
        {
            paths.push(path);
        }
    }
    paths.sort();
    Ok(paths)
}

fn task_order_key(attempt_id: &str, task: &Task) -> (usize, usize, String) {
    let write_prefix = format!("{attempt_id}-write-");
    if task.kind == TaskKind::Write
        && let Some(round) = task
            .id
            .strip_prefix(&write_prefix)
            .and_then(|round| round.parse::<usize>().ok())
    {
        return (round.saturating_sub(1) * 2, 0, task.id.clone());
    }

    let review_prefix = format!("{attempt_id}-review-");
    if task.kind == TaskKind::Review {
        let Some(suffix) = task.id.strip_prefix(&review_prefix) else {
            return (usize::MAX, 0, task.id.clone());
        };
        if let Some((round, role)) = suffix
            .split_once('-')
            .and_then(|(round, role)| round.parse::<usize>().ok().map(|round| (round, role)))
        {
            return (
                round.saturating_sub(1) * 2 + 1,
                review_role_order(role),
                role.to_string(),
            );
        }
        return (1, review_role_order(suffix), suffix.to_string());
    }

    (usize::MAX, 0, task.id.clone())
}

fn review_task_round(attempt_id: &str, task: &Task) -> Option<usize> {
    if task.kind != TaskKind::Review {
        return None;
    }
    let review_prefix = format!("{attempt_id}-review-");
    let suffix = task.id.strip_prefix(&review_prefix)?;
    suffix
        .split_once('-')
        .and_then(|(round, _)| round.parse::<usize>().ok())
        .or(Some(1))
}

fn review_input_artifacts_by_role(
    attempt: &Attempt,
    latest_write: &Task,
) -> HashMap<String, Vec<ArtifactRef>> {
    let mut roles_by_producer = HashMap::new();
    for task in &attempt.tasks {
        if task.kind == TaskKind::Review && task.status == TaskStatus::Complete {
            roles_by_producer.insert(task.id.as_str(), task.role.as_str());
        }
    }

    let mut artifacts_by_role: HashMap<String, Vec<ArtifactRef>> = HashMap::new();
    for artifact in &latest_write.input_artifacts {
        let Some(role) = roles_by_producer.get(artifact.producer_id.as_str()) else {
            continue;
        };
        artifacts_by_role
            .entry((*role).to_string())
            .or_default()
            .push(artifact.clone());
    }
    artifacts_by_role
}

fn review_role_order(role: &str) -> usize {
    match role {
        "documentation" => 0,
        "behaviors" => 1,
        "architecture" => 2,
        "skills" => 3,
        "tests" => 4,
        _ => usize::MAX,
    }
}

fn read_json_file<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, WorkModelStorageError> {
    let content = fs::read_to_string(path).map_err(|source| WorkModelStorageError::ReadFile {
        path: path.to_path_buf(),
        source,
    })?;
    from_json(&content).map_err(|source| WorkModelStorageError::ParseFile {
        path: path.to_path_buf(),
        source,
    })
}

fn write_json_file<T: Serialize>(path: &Path, value: &T) -> Result<(), WorkModelStorageError> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|source| WorkModelStorageError::CreateDirectory {
            path: dir.to_path_buf(),
            source,
        })?;
    }
    let json = to_json_pretty(value).map_err(|source| WorkModelStorageError::ParseFile {
        path: path.to_path_buf(),
        source,
    })?;
    fs::write(path, json).map_err(|source| WorkModelStorageError::WriteFile {
        path: path.to_path_buf(),
        source,
    })
}

fn prune_json_files(dir: &Path, keep: &HashSet<PathBuf>) -> Result<(), WorkModelStorageError> {
    if !dir.exists() {
        return Ok(());
    }
    for path in list_json_paths(dir)? {
        if !keep.contains(&path) {
            fs::remove_file(&path)
                .map_err(|source| WorkModelStorageError::WriteFile { path, source })?;
        }
    }
    Ok(())
}

fn prune_child_dirs(dir: &Path, keep: &HashSet<PathBuf>) -> Result<(), WorkModelStorageError> {
    if !dir.exists() {
        return Ok(());
    }
    let entries = fs::read_dir(dir).map_err(|source| WorkModelStorageError::ReadDirectory {
        path: dir.to_path_buf(),
        source,
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| WorkModelStorageError::ReadDirectory {
            path: dir.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        if path.is_dir() && !keep.contains(&path) {
            fs::remove_dir_all(&path)
                .map_err(|source| WorkModelStorageError::WriteFile { path, source })?;
        }
    }
    Ok(())
}

fn validate_id(kind: &'static str, id: &str) -> Result<(), WorkModelError> {
    if !is_file_safe_id(id) {
        return Err(WorkModelError::InvalidId {
            kind,
            id: id.to_string(),
        });
    }
    Ok(())
}

fn is_file_safe_id(id: &str) -> bool {
    !(id.is_empty() || id == "." || id == ".." || id.contains('/') || id.contains('\\'))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseTaskKindError(String);

impl fmt::Display for ParseTaskKindError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown task kind: {}", self.0)
    }
}

impl Error for ParseTaskKindError {}

pub fn to_json_pretty<T: Serialize>(value: &T) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(value).map(|json| format!("{json}\n"))
}

pub fn from_json<T: for<'de> Deserialize<'de>>(content: &str) -> Result<T, serde_json::Error> {
    serde_json::from_str(content)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace(id: &str) -> WorkspaceRef {
        WorkspaceRef {
            id: id.to_string(),
            path: format!("/workspaces/{id}"),
        }
    }

    fn task(kind: TaskKind, writes: Vec<WorkspaceRef>) -> Task {
        let review_context = (kind == TaskKind::Review).then(|| ReviewContext {
            candidate_workspace_id: "candidate".to_string(),
            candidate_workspace_path: "/workspaces/candidate".to_string(),
            source_branch: "main".to_string(),
            candidate_commit: "abc123".to_string(),
        });
        Task {
            id: "task-1".to_string(),
            kind,
            status: TaskStatus::Planned,
            role: "author".to_string(),
            instructions: None,
            work_item_id: "work-1".to_string(),
            attempt_id: Some("attempt-1".to_string()),
            workspace_access: WorkspaceAccess {
                reads: vec![workspace("source"), workspace("candidate")],
                writes,
            },
            artifact_area: Some(TaskArtifactArea {
                path: ".factory/tasks/task-1".to_string(),
            }),
            review_context,
            input_artifacts: Vec::new(),
            depends_on: None,
            output: None,
            created_at: None,
            started_at: None,
            completed_at: None,
        }
    }

    fn review_only_work_item() -> WorkItem {
        let mut work_item = WorkItem {
            id: "work-1".to_string(),
            title: "Review the codebase".to_string(),
            planning_context: None,
            instructions: None,
            abandonment: None,
            attempts: Vec::new(),
            merge_candidates: Vec::new(),
        };
        work_item
            .add_review_only_attempt("attempt-review", &["tests"], "main", "abc123")
            .unwrap();
        work_item
    }

    #[test]
    fn reviewer_workspace_path_encodes_bytelen_prefix() {
        assert_eq!(
            reviewer_workspace_path("work-1", "attempt-1", "tests"),
            "../review-6-work-1-attempt-1-tests"
        );
        assert_eq!(
            reviewer_workspace_path("my-long-work-item", "a1", "architecture"),
            "../review-17-my-long-work-item-a1-architecture"
        );
    }

    #[test]
    fn abandon_records_reason_on_inactive_work_item() {
        let mut work_item = WorkItem {
            id: "work-1".to_string(),
            title: "Abandon stale work".to_string(),
            planning_context: None,
            instructions: None,
            abandonment: None,
            attempts: Vec::new(),
            merge_candidates: Vec::new(),
        };
        work_item.add_initial_attempt("attempt-1").unwrap();
        work_item.attempts[0].status = AttemptStatus::NeedsUser;
        work_item.attempts[0].tasks[0].status = TaskStatus::NeedsUser;

        work_item
            .abandon(Some("replacement landed".to_string()))
            .unwrap();

        assert_eq!(
            work_item.abandonment.unwrap().reason.as_deref(),
            Some("replacement landed")
        );
    }

    #[test]
    fn abandon_rejects_executing_attempt_without_changing_marker() {
        let mut work_item = WorkItem {
            id: "work-1".to_string(),
            title: "Keep active work visible".to_string(),
            planning_context: None,
            instructions: None,
            abandonment: None,
            attempts: Vec::new(),
            merge_candidates: Vec::new(),
        };
        work_item.add_initial_attempt("attempt-1").unwrap();
        work_item.attempts[0].status = AttemptStatus::Executing;
        work_item.attempts[0].tasks[0].status = TaskStatus::Executing;

        let error = work_item.abandon(Some("stale".to_string())).unwrap_err();

        assert!(matches!(
            error,
            WorkModelError::WorkItemAbandonmentBlocked { .. }
        ));
        assert!(work_item.abandonment.is_none());
    }

    #[test]
    fn abandon_rejects_reviewing_attempt_without_changing_marker() {
        let mut work_item = WorkItem {
            id: "work-1".to_string(),
            title: "Keep active review visible".to_string(),
            planning_context: None,
            instructions: None,
            abandonment: None,
            attempts: Vec::new(),
            merge_candidates: Vec::new(),
        };
        work_item.add_initial_attempt("attempt-1").unwrap();
        work_item.attempts[0].status = AttemptStatus::Reviewing;

        let error = work_item.abandon(Some("stale".to_string())).unwrap_err();

        assert!(matches!(
            error,
            WorkModelError::WorkItemAbandonmentBlocked { .. }
        ));
        assert!(work_item.abandonment.is_none());
    }

    #[test]
    fn abandon_rejects_executing_task_without_changing_marker() {
        let mut work_item = WorkItem {
            id: "work-1".to_string(),
            title: "Keep active task visible".to_string(),
            planning_context: None,
            instructions: None,
            abandonment: None,
            attempts: Vec::new(),
            merge_candidates: Vec::new(),
        };
        work_item.add_initial_attempt("attempt-1").unwrap();
        work_item.attempts[0].status = AttemptStatus::Failed;
        work_item.attempts[0].tasks[0].status = TaskStatus::Executing;

        let error = work_item.abandon(Some("stale".to_string())).unwrap_err();

        assert!(matches!(
            error,
            WorkModelError::WorkItemAbandonmentBlocked { .. }
        ));
        assert!(work_item.abandonment.is_none());
    }

    #[test]
    fn abandon_rejects_active_merge_candidate_without_changing_marker() {
        let mut work_item = WorkItem {
            id: "work-1".to_string(),
            title: "Keep active candidate visible".to_string(),
            planning_context: None,
            instructions: None,
            abandonment: None,
            attempts: Vec::new(),
            merge_candidates: vec![MergeCandidate {
                id: "candidate-1".to_string(),
                attempt_id: "attempt-1".to_string(),
                source_workspace: workspace("candidate"),
                target_workspace: workspace("target"),
                source_branch: "main".to_string(),
                target_branch: "main".to_string(),
                candidate_commit: "abc123".to_string(),
                review_state: MergeCandidateReviewState::Reviewing,
                merge_state: MergeCandidateMergeState::default(),
                created_at: None,
                started_at: None,
                completed_at: None,
            }],
        };

        let error = work_item.abandon(Some("stale".to_string())).unwrap_err();

        assert!(matches!(
            error,
            WorkModelError::WorkItemAbandonmentBlocked { .. }
        ));
        assert!(work_item.abandonment.is_none());

        work_item.merge_candidates[0].review_state = MergeCandidateReviewState::Pending;
        work_item.merge_candidates[0].merge_state.status = MergeCandidateMergeStatus::Executing;

        let error = work_item.abandon(Some("stale".to_string())).unwrap_err();

        assert!(matches!(
            error,
            WorkModelError::WorkItemAbandonmentBlocked { .. }
        ));
        assert!(work_item.abandonment.is_none());
    }

    #[test]
    fn abandoned_work_item_rejects_initial_attempt_planning() {
        let mut work_item = WorkItem {
            id: "work-1".to_string(),
            title: "Keep abandoned work terminal".to_string(),
            planning_context: None,
            instructions: None,
            abandonment: Some(WorkItemAbandonment {
                reason: Some("replacement landed".to_string()),
            }),
            attempts: Vec::new(),
            merge_candidates: Vec::new(),
        };

        let error = work_item.add_initial_attempt("attempt-1").unwrap_err();

        assert!(matches!(error, WorkModelError::WorkItemAbandoned { .. }));
    }

    #[test]
    fn abandoned_work_item_rejects_review_only_attempt_planning() {
        let mut work_item = WorkItem {
            id: "work-1".to_string(),
            title: "Keep abandoned review-only work terminal".to_string(),
            planning_context: None,
            instructions: None,
            abandonment: Some(WorkItemAbandonment {
                reason: Some("replacement landed".to_string()),
            }),
            attempts: Vec::new(),
            merge_candidates: Vec::new(),
        };

        let error = work_item
            .add_review_only_attempt("attempt-review", &["tests"], "main", "abc123")
            .unwrap_err();

        assert!(matches!(error, WorkModelError::WorkItemAbandoned { .. }));
        assert!(work_item.attempts.is_empty());
    }

    #[test]
    fn abandoned_work_item_rejects_review_task_planning() {
        let mut work_item = work_item_with_completed_write("work-1");
        work_item.abandonment = Some(WorkItemAbandonment {
            reason: Some("replacement landed".to_string()),
        });

        let error = work_item
            .add_next_review_tasks("attempt-1", &["tests"])
            .unwrap_err();

        assert!(matches!(error, WorkModelError::WorkItemAbandoned { .. }));
        assert_eq!(work_item.attempts[0].tasks.len(), 1);
    }

    #[test]
    fn progress_md_in_reviewer_input_artifacts() {
        let mut work_item = work_item_with_completed_write("work-1");
        let task_ids = work_item
            .add_next_review_tasks("attempt-1", &["documentation", "behaviors", "tests"])
            .unwrap();

        for task_id in &task_ids {
            let review_task = work_item.attempts[0]
                .tasks
                .iter()
                .find(|t| t.id == *task_id)
                .unwrap();
            if review_task.kind == TaskKind::Review {
                assert!(
                    review_task.input_artifacts.iter().any(|ref_| {
                        ref_.producer_id == "writer"
                            && ref_.path == ".factory/work/artifacts/work-1/attempt-1/progress.md"
                    }),
                    "review task {} should have progress.md in input_artifacts",
                    task_id
                );
            }
        }
    }

    #[test]
    fn abandoned_work_item_rejects_followup_write_planning() {
        let mut work_item = work_item_with_completed_write("work-1");
        work_item.abandonment = Some(WorkItemAbandonment {
            reason: Some("replacement landed".to_string()),
        });

        let error = work_item
            .add_next_write_round("attempt-1", Vec::new())
            .unwrap_err();

        assert!(matches!(error, WorkModelError::WorkItemAbandoned { .. }));
        assert_eq!(work_item.attempts[0].tasks.len(), 1);
    }

    #[test]
    fn abandoned_work_item_rejects_merge_candidate_planning() {
        let mut work_item = work_item_with_completed_write("work-1");
        work_item.attempts[0].review_state = Some(AttemptReviewState::Passed);
        work_item.abandonment = Some(WorkItemAbandonment {
            reason: Some("replacement landed".to_string()),
        });

        let error = work_item
            .create_or_get_merge_candidate("attempt-1")
            .unwrap_err();

        assert!(matches!(error, WorkModelError::WorkItemAbandoned { .. }));
        assert!(work_item.merge_candidates.is_empty());
    }

    #[test]
    fn task_kind_parses_generic_kinds() {
        assert_eq!("write".parse::<TaskKind>().unwrap(), TaskKind::Write);
        assert_eq!("review".parse::<TaskKind>().unwrap(), TaskKind::Review);
        assert_eq!("merge".parse::<TaskKind>().unwrap(), TaskKind::Merge);
        assert_eq!("rebase".parse::<TaskKind>().unwrap(), TaskKind::Rebase);
        assert_eq!("report".parse::<TaskKind>().unwrap(), TaskKind::Report);
        assert_eq!("learn".parse::<TaskKind>().unwrap(), TaskKind::Learn);
        assert_eq!("probe".parse::<TaskKind>().unwrap(), TaskKind::Probe);
        assert_eq!(
            "behavior-tests".parse::<TaskKind>().unwrap(),
            TaskKind::BehaviorTests
        );
        assert!("triage".parse::<TaskKind>().is_err());
    }

    #[test]
    fn task_kind_behavior_tests_round_trips() {
        let json = serde_json::to_string(&TaskKind::BehaviorTests).unwrap();
        assert_eq!(json, r#""behavior-tests""#);
        let kind: TaskKind = serde_json::from_str(&json).unwrap();
        assert_eq!(kind, TaskKind::BehaviorTests);
        assert_eq!(TaskKind::BehaviorTests.as_str(), "behavior-tests");
        assert_eq!(format!("{}", TaskKind::BehaviorTests), "behavior-tests");
    }

    #[test]
    fn task_with_depends_on_round_trips() {
        let task = Task {
            id: "attempt-1-review-behaviors".to_string(),
            kind: TaskKind::Review,
            status: TaskStatus::Planned,
            role: "behaviors".to_string(),
            instructions: None,
            work_item_id: "work-1".to_string(),
            attempt_id: Some("attempt-1".to_string()),
            workspace_access: WorkspaceAccess {
                reads: vec![workspace("candidate")],
                writes: Vec::new(),
            },
            artifact_area: Some(TaskArtifactArea {
                path: ".factory/work/artifacts/work-1/attempt-1/attempt-1-review-behaviors"
                    .to_string(),
            }),
            review_context: Some(ReviewContext {
                candidate_workspace_id: "candidate".to_string(),
                candidate_workspace_path: "/workspaces/candidate".to_string(),
                source_branch: "main".to_string(),
                candidate_commit: "abc123".to_string(),
            }),
            input_artifacts: Vec::new(),
            depends_on: Some("attempt-1-behavior-tests".to_string()),
            output: None,
            created_at: None,
            started_at: None,
            completed_at: None,
        };

        let json = serde_json::to_string_pretty(&task).unwrap();
        assert!(json.contains(r#""depends_on": "attempt-1-behavior-tests""#));
        let parsed: Task = serde_json::from_str(&json).unwrap();
        assert_eq!(
            parsed.depends_on.as_deref(),
            Some("attempt-1-behavior-tests")
        );
    }

    #[test]
    fn task_without_depends_on_omits_field() {
        let task = Task {
            id: "t-1".to_string(),
            kind: TaskKind::Write,
            status: TaskStatus::Planned,
            role: "author".to_string(),
            instructions: None,
            work_item_id: "w-1".to_string(),
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
        let json = serde_json::to_string(&task).unwrap();
        assert!(!json.contains("depends_on"));
    }

    #[test]
    fn review_tasks_include_tester_task_when_candidate_exists() {
        let mut work_item = work_item_with_completed_write("work-1");
        work_item
            .add_next_review_tasks("attempt-1", &["documentation", "behaviors", "tests"])
            .unwrap();

        let tasks = &work_item.attempts[0].tasks;
        let tester_task = tasks
            .iter()
            .find(|t| t.kind == TaskKind::Tester)
            .expect("should have a Tester task");
        assert_eq!(tester_task.id, "attempt-1-tester");
        assert_eq!(tester_task.role, "tester");
        assert!(tester_task.depends_on.is_none());

        let behaviors_review = tasks
            .iter()
            .find(|t| t.role == "behaviors" && t.kind == TaskKind::Review)
            .expect("should have a behaviors review task");
        assert_eq!(
            behaviors_review.depends_on.as_deref(),
            Some("attempt-1-tester")
        );

        let doc_review = tasks.iter().find(|t| t.role == "documentation").unwrap();
        assert_eq!(doc_review.depends_on.as_deref(), Some("attempt-1-tester"));

        let tests_review = tasks.iter().find(|t| t.role == "tests").unwrap();
        assert_eq!(tests_review.depends_on.as_deref(), Some("attempt-1-tester"));
    }

    #[test]
    fn review_tasks_depend_on_tester_task() {
        let mut work_item = work_item_with_completed_write("work-1");
        work_item
            .add_next_review_tasks("attempt-1", &["documentation", "tests"])
            .unwrap();

        let tasks = &work_item.attempts[0].tasks;
        let tester_task = tasks
            .iter()
            .find(|t| t.kind == TaskKind::Tester)
            .expect("Tester task should be present");

        for task in tasks.iter().filter(|t| t.kind == TaskKind::Review) {
            assert_eq!(
                task.depends_on.as_deref(),
                Some(tester_task.id.as_str()),
                "reviewer task {} should depend on tester",
                task.role,
            );
        }
    }

    #[test]
    fn review_tasks_tester_id_includes_round_after_first() {
        let mut work_item = work_item_with_completed_write("work-1");
        work_item
            .add_next_review_tasks("attempt-1", &["tests"])
            .unwrap();
        // Complete all tasks so we can add another round
        for task in &mut work_item.attempts[0].tasks {
            crate::work_model::set_task_terminal(task, TaskStatus::Complete);
        }
        work_item.attempts[0].review_state = Some(AttemptReviewState::NotReviewed);
        work_item.attempts[0].status = AttemptStatus::Complete;
        // Add a second write task
        work_item
            .add_next_write_round("attempt-1", Vec::new())
            .unwrap();
        let write_idx = work_item.attempts[0]
            .tasks
            .iter()
            .rposition(|t| t.kind == TaskKind::Write)
            .unwrap();
        work_item.attempts[0].tasks[write_idx].status = TaskStatus::Complete;
        work_item.attempts[0].tasks[write_idx].output = Some(TaskOutput {
            workspace_id: "candidate".to_string(),
            workspace_path: "../work-6-work-1-attempt-1-second".to_string(),
            source_branch: "main".to_string(),
            commit: "commit-second".to_string(),
        });
        work_item.attempts[0].status = AttemptStatus::Complete;
        work_item
            .add_next_review_tasks("attempt-1", &["tests"])
            .unwrap();

        let tasks = &work_item.attempts[0].tasks;
        let tester_tasks: Vec<_> = tasks
            .iter()
            .filter(|t| t.kind == TaskKind::Tester)
            .collect();
        assert!(
            tester_tasks.iter().any(|t| t.id == "attempt-1-tester-2"),
            "second round tester should have -2 suffix; got {:?}",
            tester_tasks.iter().map(|t| &t.id).collect::<Vec<_>>()
        );
    }

    #[test]
    fn each_reviewer_task_receives_tester_results_in_input_artifacts() {
        let mut work_item = work_item_with_completed_write("work-1");
        work_item
            .add_next_review_tasks(
                "attempt-1",
                &[
                    "behaviors",
                    "tests",
                    "documentation",
                    "skills",
                    "architecture",
                ],
            )
            .unwrap();

        let tasks = &work_item.attempts[0].tasks;
        for task in tasks.iter().filter(|t| t.kind == TaskKind::Review) {
            let has_tester_results = task.input_artifacts.iter().any(|a| {
                a.path.ends_with("/tester-results.json") && a.producer_id.contains("tester")
            });
            assert!(
                has_tester_results,
                "reviewer task {} should have tester-results.json in input_artifacts",
                task.role,
            );
        }
    }

    #[test]
    fn no_tester_task_when_writer_failed() {
        let mut work_item = WorkItem {
            id: "work-1".to_string(),
            title: "Test failed writer".to_string(),
            planning_context: None,
            instructions: None,
            abandonment: None,
            attempts: vec![Attempt {
                id: "attempt-1".to_string(),
                work_item_id: "work-1".to_string(),
                kind: AttemptKind::Write,
                status: AttemptStatus::Failed,
                coder_mapping: CoderMapping::default(),
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
                        writes: vec![WorkspaceRef {
                            id: "candidate".to_string(),
                            path: "../work-6-work-1-attempt-1-initial".to_string(),
                        }],
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
                review_state: None,
                artifacts: Vec::new(),
                created_at: None,
                completed_at: None,
            }],
            merge_candidates: Vec::new(),
        };

        let result = work_item.add_next_review_tasks("attempt-1", &["tests"]);
        assert!(
            result.is_err(),
            "should error when no completed write task exists"
        );
        assert!(
            !work_item.attempts[0]
                .tasks
                .iter()
                .any(|t| t.kind == TaskKind::Tester),
            "no Tester task when writer failed"
        );
    }

    #[test]
    fn task_kind_serializes_as_lowercase_vocabulary() {
        let json = serde_json::to_string(&TaskKind::Review).unwrap();
        assert_eq!(json, r#""review""#);
        let kind: TaskKind = serde_json::from_str(&json).unwrap();
        assert_eq!(kind, TaskKind::Review);
    }

    #[test]
    fn workspace_access_allows_many_reads_and_one_write() {
        let task = task(TaskKind::Write, vec![workspace("candidate")]);
        assert!(task.validate().is_ok());
    }

    #[test]
    fn workspace_access_rejects_multiple_writes() {
        let task = task(
            TaskKind::Write,
            vec![workspace("candidate-a"), workspace("candidate-b")],
        );
        assert_eq!(
            task.validate().unwrap_err(),
            WorkModelError::MultipleWriteWorkspaces { count: 2 }
        );
    }

    #[test]
    fn review_task_reads_workspaces_and_writes_only_artifacts() {
        let review_task = task(TaskKind::Review, Vec::new());
        assert!(review_task.validate().is_ok());
        assert!(review_task.artifact_area.is_some());
    }

    #[test]
    fn initial_write_task_has_artifact_area_path() {
        let mut work_item = WorkItem {
            id: "work-1".to_string(),
            title: "Write with artifacts".to_string(),
            planning_context: None,
            instructions: None,
            abandonment: None,
            attempts: Vec::new(),
            merge_candidates: Vec::new(),
        };
        work_item.add_initial_attempt("attempt-1").unwrap();

        let write_task = &work_item.attempts[0].tasks[0];
        assert_eq!(write_task.kind, TaskKind::Write);
        assert_eq!(
            write_task.artifact_area.as_ref().unwrap().path,
            ".factory/work/artifacts/work-1/attempt-1/attempt-1-write-1"
        );
    }

    #[test]
    fn followup_write_task_has_artifact_area_path() {
        let mut work_item = work_item_with_completed_write("work-1");
        work_item
            .add_next_review_tasks("attempt-1", &["tests"])
            .unwrap();
        work_item.attempts[0].tasks[1].status = TaskStatus::Complete;
        let task_id = work_item
            .add_next_write_round(
                "attempt-1",
                vec![ArtifactRef {
                    producer_id: "attempt-1-review-tests".to_string(),
                    path:
                        ".factory/work/artifacts/work-1/attempt-1/attempt-1-review-tests/review.md"
                            .to_string(),
                }],
            )
            .unwrap();

        let followup_task = work_item.attempts[0]
            .tasks
            .iter()
            .find(|t| t.id == task_id)
            .unwrap();
        assert_eq!(followup_task.kind, TaskKind::Write);
        assert_eq!(
            followup_task.artifact_area.as_ref().unwrap().path,
            ".factory/work/artifacts/work-1/attempt-1/attempt-1-write-2"
        );
    }

    #[test]
    fn review_task_rejects_workspace_writes() {
        let review_task = task(TaskKind::Review, vec![workspace("candidate")]);
        assert_eq!(
            review_task.validate().unwrap_err(),
            WorkModelError::ReviewTaskWritesWorkspace {
                task_id: "task-1".to_string()
            }
        );
    }

    #[test]
    fn review_task_requires_artifact_area() {
        let mut review_task = task(TaskKind::Review, Vec::new());
        review_task.artifact_area = None;

        assert_eq!(
            review_task.validate().unwrap_err(),
            WorkModelError::ReviewTaskMissingArtifactArea {
                task_id: "task-1".to_string()
            }
        );
    }

    #[test]
    fn review_task_requires_readable_workspace() {
        let mut review_task = task(TaskKind::Review, Vec::new());
        review_task.workspace_access.reads = Vec::new();

        assert_eq!(
            review_task.validate().unwrap_err(),
            WorkModelError::ReviewTaskMissingReadableWorkspace {
                task_id: "task-1".to_string()
            }
        );
    }

    #[test]
    fn review_task_requires_review_context() {
        let mut review_task = task(TaskKind::Review, Vec::new());
        review_task.review_context = None;

        assert_eq!(
            review_task.validate().unwrap_err(),
            WorkModelError::ReviewTaskMissingContext {
                task_id: "task-1".to_string()
            }
        );
    }

    #[test]
    fn review_task_requires_context_candidate_to_be_readable() {
        let mut review_task = task(TaskKind::Review, Vec::new());
        review_task.review_context = Some(ReviewContext {
            candidate_workspace_id: "other".to_string(),
            candidate_workspace_path: "/workspaces/other".to_string(),
            source_branch: "main".to_string(),
            candidate_commit: "abc123".to_string(),
        });

        assert_eq!(
            review_task.validate().unwrap_err(),
            WorkModelError::ReviewTaskContextCandidateNotReadable {
                task_id: "task-1".to_string()
            }
        );
    }

    #[test]
    fn review_only_attempt_copies_work_item_planning_context_to_tasks() {
        let mut work_item = WorkItem {
            id: "work-1".to_string(),
            title: "Review the codebase".to_string(),
            planning_context: Some(PlanningContext {
                brief: Some("Review only skills/ and focus on prompts.\n".to_string()),
                ..PlanningContext::default()
            }),
            instructions: None,
            abandonment: None,
            attempts: Vec::new(),
            merge_candidates: Vec::new(),
        };

        work_item
            .add_review_only_attempt("attempt-review", &["skills"], "main", "abc123")
            .unwrap();

        let instructions = work_item.attempts[0].tasks[0]
            .instructions
            .as_deref()
            .unwrap();
        assert!(instructions.contains("# Brief"));
        assert!(instructions.contains("Review only skills/ and focus on prompts."));
    }

    #[test]
    fn review_only_attempt_rejects_write_tasks() {
        let mut work_item = review_only_work_item();
        let review_task = work_item.attempts[0].tasks[0].clone();
        let write_task = Task {
            kind: TaskKind::Write,
            review_context: None,
            artifact_area: None,
            workspace_access: WorkspaceAccess {
                reads: Vec::new(),
                writes: vec![WorkspaceRef {
                    id: "candidate".to_string(),
                    path: "../work-6-work-1-attempt-review".to_string(),
                }],
            },
            ..review_task
        };
        work_item.attempts[0].tasks[0] = write_task;

        assert_eq!(
            work_item.validate().unwrap_err(),
            WorkModelError::ReviewOnlyAttemptInvalidTask {
                attempt_id: "attempt-review".to_string(),
                task_id: "attempt-review-review-tests".to_string(),
                field: "kind"
            }
        );
    }

    #[test]
    fn review_only_attempt_rejects_non_source_reads() {
        let mut work_item = review_only_work_item();
        work_item.attempts[0].tasks[0].workspace_access.reads = vec![WorkspaceRef {
            id: "candidate".to_string(),
            path: "../work-6-work-1-attempt-review".to_string(),
        }];
        work_item.attempts[0].tasks[0]
            .review_context
            .as_mut()
            .unwrap()
            .candidate_workspace_id = "candidate".to_string();
        work_item.attempts[0].tasks[0]
            .review_context
            .as_mut()
            .unwrap()
            .candidate_workspace_path = "../work-6-work-1-attempt-review".to_string();

        assert_eq!(
            work_item.validate().unwrap_err(),
            WorkModelError::ReviewOnlyAttemptInvalidTask {
                attempt_id: "attempt-review".to_string(),
                task_id: "attempt-review-review-tests".to_string(),
                field: "workspace_access.reads"
            }
        );
    }

    #[test]
    fn review_only_attempt_rejects_non_source_context() {
        let mut work_item = review_only_work_item();
        work_item.attempts[0].tasks[0]
            .review_context
            .as_mut()
            .unwrap()
            .candidate_workspace_id = "candidate".to_string();

        assert_eq!(
            work_item.validate().unwrap_err(),
            WorkModelError::ReviewOnlyAttemptInvalidTask {
                attempt_id: "attempt-review".to_string(),
                task_id: "attempt-review-review-tests".to_string(),
                field: "review_context.candidate_workspace"
            }
        );
    }

    #[test]
    fn review_only_attempt_rejects_unmanaged_artifact_area() {
        let mut work_item = review_only_work_item();
        work_item.attempts[0].tasks[0]
            .artifact_area
            .as_mut()
            .unwrap()
            .path = ".factory/work/artifacts/other-attempt/attempt-review-review-tests".to_string();

        assert_eq!(
            work_item.validate().unwrap_err(),
            WorkModelError::ReviewOnlyAttemptInvalidTask {
                attempt_id: "attempt-review".to_string(),
                task_id: "attempt-review-review-tests".to_string(),
                field: "artifact_area.path"
            }
        );
    }

    #[test]
    fn review_tasks_use_latest_completed_write_output() {
        let mut work_item = WorkItem {
            id: "work-1".to_string(),
            title: "Review latest candidate".to_string(),
            planning_context: None,
            instructions: None,
            abandonment: None,
            attempts: vec![Attempt {
                id: "attempt-1".to_string(),
                work_item_id: "work-1".to_string(),
                kind: AttemptKind::Write,
                status: AttemptStatus::Planned,
                coder_mapping: CoderMapping::default(),
                tasks: vec![
                    completed_write_task("attempt-1-write-1", "original"),
                    completed_write_task("attempt-1-write-2", "followup"),
                ],
                review_state: Some(AttemptReviewState::Failed),
                artifacts: Vec::new(),
                created_at: None,
                completed_at: None,
            }],
            merge_candidates: Vec::new(),
        };

        work_item
            .add_next_review_tasks("attempt-1", &["tests"])
            .unwrap();

        let review_task = work_item.attempts[0]
            .tasks
            .iter()
            .find(|task| task.id == "attempt-1-review-tests")
            .unwrap();
        assert_eq!(
            review_task
                .review_context
                .as_ref()
                .unwrap()
                .candidate_commit,
            "commit-followup"
        );
        assert_eq!(
            review_task.workspace_access.reads[0].path,
            "../work-6-work-1-attempt-1-followup"
        );
    }

    #[test]
    fn review_artifact_paths_include_work_item_namespace() {
        let mut first = work_item_with_completed_write("work-alpha");
        let mut second = work_item_with_completed_write("work-beta");

        first
            .add_next_review_tasks("attempt-1", &["tests"])
            .unwrap();
        second
            .add_next_review_tasks("attempt-1", &["tests"])
            .unwrap();

        let first_review = first.attempts[0]
            .tasks
            .iter()
            .find(|t| t.role == "tests" && t.kind == TaskKind::Review)
            .unwrap();
        let second_review = second.attempts[0]
            .tasks
            .iter()
            .find(|t| t.role == "tests" && t.kind == TaskKind::Review)
            .unwrap();
        assert_eq!(
            first_review.artifact_area.as_ref().unwrap().path,
            ".factory/work/artifacts/work-alpha/attempt-1/attempt-1-review-tests"
        );
        assert_eq!(
            second_review.artifact_area.as_ref().unwrap().path,
            ".factory/work/artifacts/work-beta/attempt-1/attempt-1-review-tests"
        );
        assert_ne!(
            first_review.artifact_area.as_ref().unwrap().path,
            second_review.artifact_area.as_ref().unwrap().path,
        );
    }

    #[test]
    fn store_migrates_legacy_work_artifact_paths_on_read() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = WorkModelStore::new(tmp.path());
        let mut work_item = work_item_with_completed_write("work-1");
        work_item
            .add_next_review_tasks("attempt-1", &["tests"])
            .unwrap();
        // Complete tester and review tasks (tester is tasks[1], review-tests is tasks[2])
        for task in work_item.attempts[0].tasks.iter_mut().skip(1) {
            set_task_terminal(task, TaskStatus::Complete);
        }
        work_item
            .add_next_write_round(
                "attempt-1",
                vec![ArtifactRef {
                    producer_id: "attempt-1-review-tests".to_string(),
                    path:
                        ".factory/work/artifacts/work-1/attempt-1/attempt-1-review-tests/review.md"
                            .to_string(),
                }],
            )
            .unwrap();
        work_item.attempts[0].artifacts.push(ArtifactRef {
            producer_id: "attempt-1-review-tests".to_string(),
            path: ".factory/work/artifacts/work-1/attempt-1/attempt-1-review-tests/review.md"
                .to_string(),
        });
        work_item.merge_candidates.push(MergeCandidate {
            id: "attempt-1-merge-candidate".to_string(),
            attempt_id: "attempt-1".to_string(),
            source_workspace: WorkspaceRef {
                id: "candidate".to_string(),
                path: "../work-6-work-1-attempt-1-initial".to_string(),
            },
            target_workspace: WorkspaceRef {
                id: "target".to_string(),
                path: ".".to_string(),
            },
            source_branch: "main".to_string(),
            target_branch: "main".to_string(),
            candidate_commit: "commit-initial".to_string(),
            review_state: MergeCandidateReviewState::Failed,
            merge_state: MergeCandidateMergeState {
                status: MergeCandidateMergeStatus::Failed,
                merged_commit: None,
                failure_reason: Some("Review failed".to_string()),
                check_artifacts: vec![ArtifactRef {
                    producer_id: "merge-check".to_string(),
                    path: ".factory/work/artifacts/work-1/attempt-1/attempt-1-merge-candidate/merge/checks/checks.json"
                        .to_string(),
                }],
                review_artifacts: vec![ArtifactRef {
                    producer_id: "merge-review-tests".to_string(),
                    path: ".factory/work/artifacts/work-1/attempt-1/attempt-1-merge-candidate/merge/reviews/tests/review.md"
                        .to_string(),
                }],
                auto_merge_skipped: None,
            },
            created_at: None,
            started_at: None,
            completed_at: None,
        });
        store.create_work_item(&work_item).unwrap();

        let task_path = store
            .work_task_path("work-1", "attempt-1", "attempt-1-review-tests")
            .unwrap();
        let mut task_record: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&task_path).unwrap()).unwrap();
        task_record["artifact_area"]["path"] = serde_json::Value::String(
            ".factory/work/artifacts/attempt-1/attempt-1-review-tests".to_string(),
        );
        fs::write(&task_path, to_json_pretty(&task_record).unwrap()).unwrap();

        let attempt_path = store.work_attempt_path("work-1", "attempt-1").unwrap();
        let mut attempt_record: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&attempt_path).unwrap()).unwrap();
        attempt_record["artifacts"][0]["path"] = serde_json::Value::String(
            ".factory/work/artifacts/attempt-1/attempt-1-review-tests/review.md".to_string(),
        );
        fs::write(&attempt_path, to_json_pretty(&attempt_record).unwrap()).unwrap();

        let followup_path = store
            .work_task_path("work-1", "attempt-1", "attempt-1-write-2")
            .unwrap();
        let mut followup_record: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&followup_path).unwrap()).unwrap();
        followup_record["input_artifacts"][0]["path"] = serde_json::Value::String(
            ".factory/work/artifacts/attempt-1/attempt-1-review-tests/review.md".to_string(),
        );
        fs::write(&followup_path, to_json_pretty(&followup_record).unwrap()).unwrap();

        let candidate_path = store
            .work_merge_candidate_path("work-1", "attempt-1-merge-candidate")
            .unwrap();
        let mut candidate_record: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&candidate_path).unwrap()).unwrap();
        candidate_record["merge_state"]["check_artifacts"][0]["path"] = serde_json::Value::String(
            ".factory/work/artifacts/attempt-1/attempt-1-merge-candidate/merge/checks/checks.json"
                .to_string(),
        );
        candidate_record["merge_state"]["review_artifacts"][0]["path"] =
            serde_json::Value::String(
                ".factory/work/artifacts/attempt-1/attempt-1-merge-candidate/merge/reviews/tests/review.md"
                    .to_string(),
            );
        fs::write(&candidate_path, to_json_pretty(&candidate_record).unwrap()).unwrap();

        let legacy_dir = tmp
            .path()
            .join(".factory/work/artifacts/attempt-1/attempt-1-review-tests");
        fs::create_dir_all(&legacy_dir).unwrap();
        fs::write(legacy_dir.join("review.md"), "Verdict: pass\n").unwrap();

        let read = store.read_work_item("work-1").unwrap();

        let review_tests_task = read.attempts[0]
            .tasks
            .iter()
            .find(|t| t.id == "attempt-1-review-tests")
            .expect("review-tests task");
        assert_eq!(
            review_tests_task.artifact_area.as_ref().unwrap().path,
            ".factory/work/artifacts/work-1/attempt-1/attempt-1-review-tests"
        );
        assert_eq!(
            read.attempts[0].artifacts[0].path,
            ".factory/work/artifacts/work-1/attempt-1/attempt-1-review-tests/review.md"
        );
        let write_2_task = read.attempts[0]
            .tasks
            .iter()
            .find(|t| t.id.contains("write-2"))
            .expect("write-2 task");
        assert_eq!(
            write_2_task.input_artifacts[0].path,
            ".factory/work/artifacts/work-1/attempt-1/attempt-1-review-tests/review.md"
        );
        assert_eq!(
            read.merge_candidates[0].merge_state.check_artifacts[0].path,
            ".factory/work/artifacts/work-1/attempt-1/attempt-1-merge-candidate/merge/checks/checks.json"
        );
        assert_eq!(
            read.merge_candidates[0].merge_state.review_artifacts[0].path,
            ".factory/work/artifacts/work-1/attempt-1/attempt-1-merge-candidate/merge/reviews/tests/review.md"
        );
        assert!(
            tmp.path()
                .join(".factory/work/artifacts/work-1/attempt-1/attempt-1-review-tests/review.md")
                .is_file()
        );
        assert!(!legacy_dir.exists());
    }

    #[test]
    fn next_review_tasks_keep_round_number_after_full_round() {
        let mut work_item = WorkItem {
            id: "work-1".to_string(),
            title: "Review latest candidate".to_string(),
            planning_context: None,
            instructions: None,
            abandonment: None,
            attempts: vec![Attempt {
                id: "attempt-1".to_string(),
                work_item_id: "work-1".to_string(),
                kind: AttemptKind::Write,
                status: AttemptStatus::Planned,
                coder_mapping: CoderMapping::default(),
                tasks: vec![
                    completed_write_task("attempt-1-write-1", "initial"),
                    Task {
                        id: "attempt-1-review-documentation".to_string(),
                        kind: TaskKind::Review,
                        status: TaskStatus::Complete,
                        role: "documentation".to_string(),
                        instructions: None,
                        work_item_id: "work-1".to_string(),
                        attempt_id: Some("attempt-1".to_string()),
                        workspace_access: WorkspaceAccess::read_only(vec![workspace("candidate")]),
                        artifact_area: Some(TaskArtifactArea {
                            path:
                                ".factory/work/artifacts/work-1/attempt-1/attempt-1-review-documentation"
                                    .to_string(),
                        }),
                        review_context: Some(ReviewContext {
                            candidate_workspace_id: "candidate".to_string(),
                            candidate_workspace_path: "/workspaces/candidate".to_string(),
                            source_branch: "main".to_string(),
                            candidate_commit: "commit-initial".to_string(),
                        }),
                        input_artifacts: Vec::new(),
                        depends_on: None,
                        output: None,
                        created_at: None,
                        started_at: None,
                        completed_at: None,
                    },
                    Task {
                        id: "attempt-1-review-behaviors".to_string(),
                        kind: TaskKind::Review,
                        status: TaskStatus::Complete,
                        role: "behaviors".to_string(),
                        instructions: None,
                        work_item_id: "work-1".to_string(),
                        attempt_id: Some("attempt-1".to_string()),
                        workspace_access: WorkspaceAccess::read_only(vec![workspace("candidate")]),
                        artifact_area: Some(TaskArtifactArea {
                            path: ".factory/work/artifacts/work-1/attempt-1/attempt-1-review-behaviors"
                                .to_string(),
                        }),
                        review_context: Some(ReviewContext {
                            candidate_workspace_id: "candidate".to_string(),
                            candidate_workspace_path: "/workspaces/candidate".to_string(),
                            source_branch: "main".to_string(),
                            candidate_commit: "commit-initial".to_string(),
                        }),
                        input_artifacts: Vec::new(),
                        depends_on: None,
                        output: None,
                        created_at: None,
                        started_at: None,
                        completed_at: None,
                    },
                    completed_write_task("attempt-1-write-2", "followup"),
                ],
                review_state: Some(AttemptReviewState::NotReviewed),
                artifacts: Vec::new(),
                created_at: None,
                completed_at: None,
            }],
            merge_candidates: Vec::new(),
        };

        let task_ids = work_item
            .add_next_review_tasks("attempt-1", &["tests"])
            .unwrap();

        assert_eq!(
            task_ids,
            vec!["attempt-1-tester-2", "attempt-1-review-2-tests"]
        );
    }

    #[test]
    fn attempt_artifacts_round_trip_with_work_item() {
        let work_item = WorkItem {
            id: "work-1".to_string(),
            title: "Define the core work model".to_string(),
            planning_context: None,
            instructions: None,
            abandonment: None,
            attempts: vec![Attempt {
                id: "attempt-1".to_string(),
                work_item_id: "work-1".to_string(),
                kind: AttemptKind::Write,
                status: AttemptStatus::Complete,
                coder_mapping: CoderMapping::default(),
                tasks: vec![task(TaskKind::Write, vec![workspace("candidate")])],
                review_state: Some(AttemptReviewState::Passed),
                artifacts: vec![ArtifactRef {
                    producer_id: "task-1".to_string(),
                    path: ".factory/tasks/task-1/report.md".to_string(),
                }],
                created_at: None,
                completed_at: None,
            }],
            merge_candidates: Vec::new(),
        };

        let json = to_json_pretty(&work_item).unwrap();
        let decoded: WorkItem = from_json(&json).unwrap();

        assert_eq!(decoded, work_item);
        assert_eq!(
            decoded.attempts[0].artifacts[0].path,
            ".factory/tasks/task-1/report.md"
        );
    }

    #[test]
    fn merge_candidate_uses_latest_completed_write_output() {
        let mut work_item = WorkItem {
            id: "work-1".to_string(),
            title: "Create merge candidate".to_string(),
            planning_context: None,
            instructions: None,
            abandonment: None,
            attempts: vec![Attempt {
                id: "attempt-1".to_string(),
                work_item_id: "work-1".to_string(),
                kind: AttemptKind::Write,
                status: AttemptStatus::Complete,
                coder_mapping: CoderMapping::default(),
                tasks: vec![
                    completed_write_task("attempt-1-write-1", "original"),
                    completed_write_task("attempt-1-write-2", "followup"),
                ],
                review_state: Some(AttemptReviewState::Passed),
                artifacts: Vec::new(),
                created_at: None,
                completed_at: None,
            }],
            merge_candidates: Vec::new(),
        };

        let candidate_id = work_item
            .create_or_get_merge_candidate("attempt-1")
            .unwrap();

        assert_eq!(candidate_id, "attempt-1-merge-candidate");
        assert_eq!(work_item.merge_candidates.len(), 1);
        let candidate = &work_item.merge_candidates[0];
        assert_eq!(candidate.attempt_id, "attempt-1");
        assert_eq!(candidate.source_workspace.id, "candidate");
        assert_eq!(
            candidate.source_workspace.path,
            "../work-6-work-1-attempt-1-followup"
        );
        assert_eq!(candidate.target_workspace.id, "target");
        assert_eq!(candidate.target_workspace.path, ".");
        assert_eq!(candidate.source_branch, "main");
        assert_eq!(candidate.target_branch, "main");
        assert_eq!(candidate.candidate_commit, "commit-followup");
        assert_eq!(candidate.review_state, MergeCandidateReviewState::Pending);
        work_item.validate().unwrap();
    }

    #[test]
    fn merge_candidate_creation_is_idempotent() {
        let mut work_item = WorkItem {
            id: "work-1".to_string(),
            title: "Create merge candidate once".to_string(),
            planning_context: None,
            instructions: None,
            abandonment: None,
            attempts: vec![Attempt {
                id: "attempt-1".to_string(),
                work_item_id: "work-1".to_string(),
                kind: AttemptKind::Write,
                status: AttemptStatus::Complete,
                coder_mapping: CoderMapping::default(),
                tasks: vec![completed_write_task("attempt-1-write-1", "original")],
                review_state: Some(AttemptReviewState::Passed),
                artifacts: Vec::new(),
                created_at: None,
                completed_at: None,
            }],
            merge_candidates: Vec::new(),
        };

        let first = work_item
            .create_or_get_merge_candidate("attempt-1")
            .unwrap();
        let second = work_item
            .create_or_get_merge_candidate("attempt-1")
            .unwrap();

        assert_eq!(first, "attempt-1-merge-candidate");
        assert_eq!(second, first);
        assert_eq!(work_item.merge_candidates.len(), 1);
    }

    #[test]
    fn merge_candidate_validation_rejects_duplicate_attempt_candidate() {
        let mut work_item = WorkItem {
            id: "work-1".to_string(),
            title: "Keep one merge candidate per attempt".to_string(),
            planning_context: None,
            instructions: None,
            abandonment: None,
            attempts: vec![Attempt {
                id: "attempt-1".to_string(),
                work_item_id: "work-1".to_string(),
                kind: AttemptKind::Write,
                status: AttemptStatus::Complete,
                coder_mapping: CoderMapping::default(),
                tasks: vec![completed_write_task("attempt-1-write-1", "original")],
                review_state: Some(AttemptReviewState::Passed),
                artifacts: Vec::new(),
                created_at: None,
                completed_at: None,
            }],
            merge_candidates: Vec::new(),
        };
        work_item
            .create_or_get_merge_candidate("attempt-1")
            .unwrap();
        let mut duplicate = work_item.merge_candidates[0].clone();
        duplicate.id = "alternate-merge-candidate".to_string();
        work_item.merge_candidates.push(duplicate);

        assert_eq!(
            work_item.validate().unwrap_err(),
            WorkModelError::MergeCandidateAttemptAlreadyExists {
                attempt_id: "attempt-1".to_string(),
            }
        );
    }

    #[test]
    fn merge_candidate_validation_requires_passed_attempt() {
        let mut work_item = WorkItem {
            id: "work-1".to_string(),
            title: "Validate merge candidate attempt state".to_string(),
            planning_context: None,
            instructions: None,
            abandonment: None,
            attempts: vec![Attempt {
                id: "attempt-1".to_string(),
                work_item_id: "work-1".to_string(),
                kind: AttemptKind::Write,
                status: AttemptStatus::Reviewing,
                coder_mapping: CoderMapping::default(),
                tasks: vec![completed_write_task("attempt-1-write-1", "original")],
                review_state: Some(AttemptReviewState::Uncertain),
                artifacts: Vec::new(),
                created_at: None,
                completed_at: None,
            }],
            merge_candidates: Vec::new(),
        };
        work_item.merge_candidates.push(MergeCandidate {
            id: "attempt-1-merge-candidate".to_string(),
            attempt_id: "attempt-1".to_string(),
            source_workspace: WorkspaceRef {
                id: "candidate".to_string(),
                path: "../work-6-work-1-attempt-1-original".to_string(),
            },
            target_workspace: WorkspaceRef {
                id: "target".to_string(),
                path: ".".to_string(),
            },
            source_branch: "main".to_string(),
            target_branch: "main".to_string(),
            candidate_commit: "commit-original".to_string(),
            review_state: MergeCandidateReviewState::Pending,
            merge_state: MergeCandidateMergeState::default(),
            created_at: None,
            started_at: None,
            completed_at: None,
        });

        assert_eq!(
            work_item.validate().unwrap_err(),
            WorkModelError::MergeCandidateAttemptReviewsNotPassed {
                candidate_id: "attempt-1-merge-candidate".to_string(),
                attempt_id: "attempt-1".to_string(),
            }
        );
    }

    #[test]
    fn merge_candidate_validation_requires_latest_write_output() {
        let mut work_item = WorkItem {
            id: "work-1".to_string(),
            title: "Validate merge candidate provenance".to_string(),
            planning_context: None,
            instructions: None,
            abandonment: None,
            attempts: vec![Attempt {
                id: "attempt-1".to_string(),
                work_item_id: "work-1".to_string(),
                kind: AttemptKind::Write,
                status: AttemptStatus::Complete,
                coder_mapping: CoderMapping::default(),
                tasks: vec![completed_write_task("attempt-1-write-1", "original")],
                review_state: Some(AttemptReviewState::Passed),
                artifacts: Vec::new(),
                created_at: None,
                completed_at: None,
            }],
            merge_candidates: Vec::new(),
        };
        work_item.merge_candidates.push(MergeCandidate {
            id: "attempt-1-merge-candidate".to_string(),
            attempt_id: "attempt-1".to_string(),
            source_workspace: WorkspaceRef {
                id: "candidate".to_string(),
                path: "../work-6-work-1-attempt-1-original".to_string(),
            },
            target_workspace: WorkspaceRef {
                id: "target".to_string(),
                path: ".".to_string(),
            },
            source_branch: "main".to_string(),
            target_branch: "main".to_string(),
            candidate_commit: "stale-commit".to_string(),
            review_state: MergeCandidateReviewState::Pending,
            merge_state: MergeCandidateMergeState::default(),
            created_at: None,
            started_at: None,
            completed_at: None,
        });

        assert_eq!(
            work_item.validate().unwrap_err(),
            WorkModelError::MergeCandidateProvenanceMismatch {
                candidate_id: "attempt-1-merge-candidate".to_string(),
                field: "candidate_commit",
            }
        );
    }

    #[test]
    fn failed_merge_candidate_preserves_failed_review_state() {
        let mut work_item = WorkItem {
            id: "work-1".to_string(),
            title: "Preserve merge failure state".to_string(),
            planning_context: None,
            instructions: None,
            abandonment: None,
            attempts: vec![Attempt {
                id: "attempt-1".to_string(),
                work_item_id: "work-1".to_string(),
                kind: AttemptKind::Write,
                status: AttemptStatus::Reviewing,
                coder_mapping: CoderMapping::default(),
                tasks: vec![completed_write_task("attempt-1-write-1", "original")],
                review_state: Some(AttemptReviewState::Failed),
                artifacts: Vec::new(),
                created_at: None,
                completed_at: None,
            }],
            merge_candidates: Vec::new(),
        };
        work_item.merge_candidates.push(MergeCandidate {
            id: "attempt-1-merge-candidate".to_string(),
            attempt_id: "attempt-1".to_string(),
            source_workspace: WorkspaceRef {
                id: "candidate".to_string(),
                path: "../work-6-work-1-attempt-1-original".to_string(),
            },
            target_workspace: WorkspaceRef {
                id: "target".to_string(),
                path: ".".to_string(),
            },
            source_branch: "main".to_string(),
            target_branch: "main".to_string(),
            candidate_commit: "commit-original".to_string(),
            review_state: MergeCandidateReviewState::Pending,
            merge_state: MergeCandidateMergeState {
                status: MergeCandidateMergeStatus::Failed,
                merged_commit: None,
                failure_reason: Some("Attempt review failed".to_string()),
                check_artifacts: Vec::new(),
                review_artifacts: Vec::new(),
                auto_merge_skipped: None,
            },
            created_at: None,
            started_at: None,
            completed_at: None,
        });

        work_item.validate().unwrap();
    }

    #[test]
    fn failed_merge_candidate_still_requires_candidate_provenance() {
        let mut work_item = WorkItem {
            id: "work-1".to_string(),
            title: "Validate failed merge candidate provenance".to_string(),
            planning_context: None,
            instructions: None,
            abandonment: None,
            attempts: vec![Attempt {
                id: "attempt-1".to_string(),
                work_item_id: "work-1".to_string(),
                kind: AttemptKind::Write,
                status: AttemptStatus::Complete,
                coder_mapping: CoderMapping::default(),
                tasks: vec![completed_write_task("attempt-1-write-1", "original")],
                review_state: Some(AttemptReviewState::Passed),
                artifacts: Vec::new(),
                created_at: None,
                completed_at: None,
            }],
            merge_candidates: Vec::new(),
        };
        work_item.merge_candidates.push(MergeCandidate {
            id: "attempt-1-merge-candidate".to_string(),
            attempt_id: "attempt-1".to_string(),
            source_workspace: WorkspaceRef {
                id: "candidate".to_string(),
                path: "../work-6-work-1-attempt-1-original".to_string(),
            },
            target_workspace: WorkspaceRef {
                id: "target".to_string(),
                path: ".".to_string(),
            },
            source_branch: "main".to_string(),
            target_branch: "main".to_string(),
            candidate_commit: "stale-commit".to_string(),
            review_state: MergeCandidateReviewState::Pending,
            merge_state: MergeCandidateMergeState {
                status: MergeCandidateMergeStatus::Failed,
                merged_commit: None,
                failure_reason: Some("candidate_commit mismatch".to_string()),
                check_artifacts: Vec::new(),
                review_artifacts: Vec::new(),
                auto_merge_skipped: None,
            },
            created_at: None,
            started_at: None,
            completed_at: None,
        });

        assert_eq!(
            work_item.validate().unwrap_err(),
            WorkModelError::MergeCandidateProvenanceMismatch {
                candidate_id: "attempt-1-merge-candidate".to_string(),
                field: "candidate_commit",
            }
        );
    }

    #[test]
    fn merge_candidate_review_state_is_separate_from_attempt_review_state() {
        let attempt = Attempt {
            id: "attempt-1".to_string(),
            work_item_id: "work-1".to_string(),
            kind: AttemptKind::Write,
            status: AttemptStatus::Reviewing,
            coder_mapping: CoderMapping::default(),
            tasks: Vec::new(),
            review_state: Some(AttemptReviewState::Uncertain),
            artifacts: Vec::new(),
            created_at: None,
            completed_at: None,
        };
        let candidate = MergeCandidate {
            id: "candidate-1".to_string(),
            attempt_id: attempt.id.clone(),
            source_workspace: workspace("candidate"),
            target_workspace: workspace("main"),
            source_branch: "main".to_string(),
            target_branch: "main".to_string(),
            candidate_commit: "abc123".to_string(),
            review_state: MergeCandidateReviewState::Passed,
            merge_state: MergeCandidateMergeState::default(),
            created_at: None,
            started_at: None,
            completed_at: None,
        };

        assert_eq!(attempt.review_state, Some(AttemptReviewState::Uncertain));
        assert_eq!(candidate.review_state, MergeCandidateReviewState::Passed);
    }

    #[test]
    fn split_task_records_without_attempt_are_invalid() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = WorkModelStore::new(tmp.path());
        let mut work_item = WorkItem {
            id: "work-1".to_string(),
            title: "Split task orphan".to_string(),
            planning_context: None,
            instructions: None,
            abandonment: None,
            attempts: Vec::new(),
            merge_candidates: Vec::new(),
        };
        work_item.add_initial_attempt("attempt-1").unwrap();
        store.create_work_item(&work_item).unwrap();

        let attempt_path = store.work_attempt_path("work-1", "attempt-1").unwrap();
        fs::remove_file(&attempt_path).unwrap();

        let error = store
            .read_work_item("work-1")
            .expect_err("orphan task collection should be invalid");
        let message = error.to_string();
        assert!(
            message.contains(".factory/work/tasks/work-1/attempt-1"),
            "{message}"
        );
        assert!(
            message.contains("Attempt \"attempt-1\" not found"),
            "{message}"
        );
    }

    #[test]
    fn split_task_records_preserve_lifecycle_order() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = WorkModelStore::new(tmp.path());
        let mut work_item = WorkItem {
            id: "work-1".to_string(),
            title: "Split task order".to_string(),
            planning_context: None,
            instructions: None,
            abandonment: None,
            attempts: vec![Attempt {
                id: "attempt-1".to_string(),
                work_item_id: "work-1".to_string(),
                kind: AttemptKind::Write,
                status: AttemptStatus::Reviewing,
                coder_mapping: CoderMapping::default(),
                tasks: vec![
                    completed_write_task("attempt-1-write-1", "initial"),
                    Task {
                        id: "attempt-1-review-tests".to_string(),
                        kind: TaskKind::Review,
                        status: TaskStatus::Complete,
                        role: "tests".to_string(),
                        instructions: None,
                        work_item_id: "work-1".to_string(),
                        attempt_id: Some("attempt-1".to_string()),
                        workspace_access: WorkspaceAccess::read_only(vec![workspace("candidate")]),
                        artifact_area: Some(TaskArtifactArea {
                            path: ".factory/work/artifacts/work-1/attempt-1/attempt-1-review-tests"
                                .to_string(),
                        }),
                        review_context: Some(ReviewContext {
                            candidate_workspace_id: "candidate".to_string(),
                            candidate_workspace_path: "/workspaces/candidate".to_string(),
                            source_branch: "main".to_string(),
                            candidate_commit: "commit-initial".to_string(),
                        }),
                        input_artifacts: Vec::new(),
                        depends_on: None,
                        output: None,
                        created_at: None,
                        started_at: None,
                        completed_at: None,
                    },
                    completed_write_task("attempt-1-write-2", "followup"),
                ],
                review_state: Some(AttemptReviewState::NotReviewed),
                artifacts: Vec::new(),
                created_at: None,
                completed_at: None,
            }],
            merge_candidates: Vec::new(),
        };
        work_item
            .add_review_tasks_with_round("attempt-1", &["tests"], Some(2))
            .unwrap();
        store.create_work_item(&work_item).unwrap();

        let read = store.read_work_item("work-1").unwrap();
        let task_ids = read.attempts[0]
            .tasks
            .iter()
            .map(|task| task.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            task_ids,
            vec![
                "attempt-1-write-1",
                "attempt-1-review-tests",
                "attempt-1-write-2",
                "attempt-1-tester-2",
                "attempt-1-review-2-tests"
            ]
        );
    }

    fn completed_write_task(id: &str, suffix: &str) -> Task {
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
                writes: vec![WorkspaceRef {
                    id: "candidate".to_string(),
                    path: format!("../work-6-work-1-attempt-1-{suffix}"),
                }],
            },
            artifact_area: None,
            review_context: None,
            input_artifacts: Vec::new(),
            depends_on: None,
            output: Some(TaskOutput {
                workspace_id: "candidate".to_string(),
                workspace_path: format!("../work-6-work-1-attempt-1-{suffix}"),
                source_branch: "main".to_string(),
                commit: format!("commit-{suffix}"),
            }),
            created_at: None,
            started_at: None,
            completed_at: None,
        }
    }

    fn work_item_with_completed_write(id: &str) -> WorkItem {
        WorkItem {
            id: id.to_string(),
            title: "Review latest candidate".to_string(),
            planning_context: None,
            instructions: None,
            abandonment: None,
            attempts: vec![Attempt {
                id: "attempt-1".to_string(),
                work_item_id: id.to_string(),
                kind: AttemptKind::Write,
                status: AttemptStatus::Complete,
                coder_mapping: CoderMapping::default(),
                tasks: vec![Task {
                    work_item_id: id.to_string(),
                    ..completed_write_task("attempt-1-write-1", "initial")
                }],
                review_state: Some(AttemptReviewState::NotReviewed),
                artifacts: Vec::new(),
                created_at: None,
                completed_at: None,
            }],
            merge_candidates: Vec::new(),
        }
    }

    #[test]
    fn concurrent_writes_to_distinct_work_items_do_not_race() {
        use std::sync::Arc;
        use std::thread;

        let tmp = tempfile::TempDir::new().unwrap();
        let store = Arc::new(WorkModelStore::new(tmp.path()));

        let mut handles = Vec::new();
        for index in 0..6 {
            let store = Arc::clone(&store);
            handles.push(thread::spawn(
                move || -> Result<(), WorkModelStorageError> {
                    let id = format!("concurrent-work-{index}");
                    let mut item = WorkItem {
                        id: id.clone(),
                        title: format!("Concurrent Work Item {index}"),
                        planning_context: None,
                        instructions: None,
                        abandonment: None,
                        attempts: Vec::new(),
                        merge_candidates: Vec::new(),
                    };
                    item.add_initial_attempt("attempt-1").unwrap();
                    store.create_work_item(&item)?;

                    // Read, mutate, write — simulates the attempt-loop write
                    // pattern: every thread should only touch its own item's
                    // split files.
                    let mut item = store.read_work_item(&id)?;
                    item.attempts[0].status = AttemptStatus::Executing;
                    item.attempts[0].tasks[0].status = TaskStatus::Executing;
                    store.write_work_item(&item)?;
                    Ok(())
                },
            ));
        }

        for handle in handles {
            handle.join().unwrap().unwrap();
        }

        for index in 0..6 {
            let id = format!("concurrent-work-{index}");
            let item = store.read_work_item(&id).unwrap();
            assert_eq!(item.id, id);
            assert_eq!(item.attempts.len(), 1);
            assert_eq!(item.attempts[0].status, AttemptStatus::Executing);
            assert_eq!(item.attempts[0].tasks[0].status, TaskStatus::Executing);
        }
    }

    fn empty_work_item(id: &str) -> WorkItem {
        WorkItem {
            id: id.to_string(),
            title: "Test".to_string(),
            planning_context: None,
            instructions: None,
            abandonment: None,
            attempts: Vec::new(),
            merge_candidates: Vec::new(),
        }
    }

    #[test]
    fn next_attempt_id_empty_returns_attempt_1() {
        let item = empty_work_item("work-1");
        assert_eq!(item.next_attempt_id(), "attempt-1");
    }

    #[test]
    fn next_attempt_id_sequential_returns_next() {
        let mut item = empty_work_item("work-1");
        item.add_initial_attempt("attempt-1").unwrap();
        assert_eq!(item.next_attempt_id(), "attempt-2");

        item.add_initial_attempt("attempt-2").unwrap();
        assert_eq!(item.next_attempt_id(), "attempt-3");
    }

    #[test]
    fn next_attempt_id_with_gap_returns_smallest_unused() {
        let mut item = empty_work_item("work-1");
        item.add_initial_attempt("attempt-1").unwrap();
        item.add_initial_attempt("attempt-3").unwrap();
        assert_eq!(item.next_attempt_id(), "attempt-2");
    }

    #[test]
    fn next_attempt_id_ignores_non_numeric_ids() {
        let mut item = empty_work_item("work-1");
        item.add_initial_attempt("custom-name").unwrap();
        assert_eq!(item.next_attempt_id(), "attempt-1");

        item.add_initial_attempt("attempt-1").unwrap();
        assert_eq!(item.next_attempt_id(), "attempt-2");
    }

    #[test]
    fn latest_attempt_id_empty_returns_none() {
        let item = empty_work_item("work-1");
        assert_eq!(item.latest_attempt_id(), None);
    }

    #[test]
    fn latest_attempt_id_returns_last() {
        let mut item = empty_work_item("work-1");
        item.add_initial_attempt("attempt-1").unwrap();
        item.add_initial_attempt("attempt-2").unwrap();
        assert_eq!(item.latest_attempt_id(), Some("attempt-2"));
    }

    #[test]
    fn latest_merge_candidate_id_empty_returns_none() {
        let item = empty_work_item("work-1");
        assert_eq!(item.latest_merge_candidate_id(), None);
    }

    #[test]
    fn latest_merge_candidate_id_returns_last() {
        let mut item = empty_work_item("work-1");
        item.attempts.push(Attempt {
            id: "attempt-1".to_string(),
            work_item_id: "work-1".to_string(),
            kind: AttemptKind::Write,
            status: AttemptStatus::Complete,
            coder_mapping: CoderMapping::default(),
            tasks: vec![completed_write_task("attempt-1-write-1", "first")],
            review_state: Some(AttemptReviewState::Passed),
            artifacts: Vec::new(),
            created_at: None,
            completed_at: None,
        });
        item.create_or_get_merge_candidate("attempt-1").unwrap();
        assert_eq!(
            item.latest_merge_candidate_id(),
            Some("attempt-1-merge-candidate")
        );
    }

    #[test]
    fn post_merge_review_attempt_round_trips_through_storage() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = WorkModelStore::new(tmp.path());
        let mut item = WorkItem {
            id: "work-pmr".to_string(),
            title: "Post-merge review".to_string(),
            planning_context: None,
            instructions: None,
            abandonment: None,
            attempts: Vec::new(),
            merge_candidates: Vec::new(),
        };
        let task_ids = item
            .add_post_merge_review_attempt("attempt-1", &["skills", "tests"], "main", "abc123")
            .unwrap();
        assert_eq!(task_ids.len(), 2);
        assert_eq!(item.attempts[0].kind, AttemptKind::PostMergeReview);

        store.create_work_item(&item).unwrap();
        let loaded = store.read_work_item("work-pmr").unwrap();
        assert_eq!(loaded.attempts[0].kind, AttemptKind::PostMergeReview);
        assert_eq!(loaded.attempts[0].tasks.len(), 2);
    }

    #[test]
    fn attempt_kind_is_review_only_like() {
        assert!(!AttemptKind::Write.is_review_only_like());
        assert!(AttemptKind::ReviewOnly.is_review_only_like());
        assert!(AttemptKind::PostMergeReview.is_review_only_like());
    }

    #[test]
    fn attempt_kind_is_source_checkout_review() {
        assert!(!AttemptKind::Write.is_source_checkout_review());
        assert!(AttemptKind::ReviewOnly.is_source_checkout_review());
        assert!(AttemptKind::PostMergeReview.is_source_checkout_review());
    }

    #[test]
    fn post_merge_review_attempt_validates_same_as_review_only() {
        let mut item = WorkItem {
            id: "work-pmr".to_string(),
            title: "PMR".to_string(),
            planning_context: None,
            instructions: None,
            abandonment: None,
            attempts: Vec::new(),
            merge_candidates: Vec::new(),
        };
        item.add_post_merge_review_attempt("attempt-1", &["skills"], "main", "abc123")
            .unwrap();

        let attempt = &mut item.attempts[0];
        let task = &mut attempt.tasks[0];
        task.kind = TaskKind::Write;
        let err = item.validate();
        assert!(err.is_err(), "PostMergeReview should reject write tasks");
    }

    #[test]
    fn now_iso8601_returns_parseable_rfc3339() {
        let ts = now_iso8601();
        chrono::DateTime::parse_from_rfc3339(&ts).expect("should parse as RFC 3339");
    }

    #[test]
    fn task_default_serializes_without_timestamps() {
        let task = Task {
            id: "t-1".to_string(),
            kind: TaskKind::Write,
            status: TaskStatus::Planned,
            role: "author".to_string(),
            instructions: None,
            work_item_id: "w-1".to_string(),
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
        let json = serde_json::to_string(&task).unwrap();
        assert!(!json.contains("created_at"));
        assert!(!json.contains("started_at"));
        assert!(!json.contains("completed_at"));
    }

    #[test]
    fn task_with_timestamps_round_trips() {
        let task = Task {
            id: "t-1".to_string(),
            kind: TaskKind::Write,
            status: TaskStatus::Complete,
            role: "author".to_string(),
            instructions: None,
            work_item_id: "w-1".to_string(),
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
            created_at: Some("2026-06-12T10:00:00+00:00".to_string()),
            started_at: Some("2026-06-12T10:01:00+00:00".to_string()),
            completed_at: Some("2026-06-12T10:05:00+00:00".to_string()),
        };
        let json = serde_json::to_string(&task).unwrap();
        assert!(json.contains("\"created_at\":\"2026-06-12T10:00:00+00:00\""));
        let round_tripped: Task = serde_json::from_str(&json).unwrap();
        assert_eq!(round_tripped.created_at, task.created_at);
        assert_eq!(round_tripped.started_at, task.started_at);
        assert_eq!(round_tripped.completed_at, task.completed_at);
    }

    #[test]
    fn attempt_round_trips_with_timestamps() {
        let attempt = Attempt {
            id: "a-1".to_string(),
            work_item_id: "w-1".to_string(),
            kind: AttemptKind::Write,
            status: AttemptStatus::Complete,
            coder_mapping: CoderMapping::default(),
            tasks: Vec::new(),
            review_state: None,
            artifacts: Vec::new(),
            created_at: Some("2026-06-12T10:00:00+00:00".to_string()),
            completed_at: Some("2026-06-12T10:05:00+00:00".to_string()),
        };
        let json = serde_json::to_string(&attempt).unwrap();
        assert!(json.contains("\"created_at\""));
        assert!(json.contains("\"completed_at\""));
        let round_tripped: Attempt = serde_json::from_str(&json).unwrap();
        assert_eq!(round_tripped.created_at, attempt.created_at);
        assert_eq!(round_tripped.completed_at, attempt.completed_at);
    }

    #[test]
    fn merge_candidate_round_trips_with_timestamps() {
        let candidate = MergeCandidate {
            id: "mc-1".to_string(),
            attempt_id: "a-1".to_string(),
            source_workspace: workspace("src"),
            target_workspace: workspace("tgt"),
            source_branch: "main".to_string(),
            target_branch: "main".to_string(),
            candidate_commit: "abc123".to_string(),
            review_state: MergeCandidateReviewState::Pending,
            merge_state: MergeCandidateMergeState::default(),
            created_at: Some("2026-06-12T10:00:00+00:00".to_string()),
            started_at: Some("2026-06-12T10:01:00+00:00".to_string()),
            completed_at: Some("2026-06-12T10:05:00+00:00".to_string()),
        };
        let json = serde_json::to_string(&candidate).unwrap();
        let round_tripped: MergeCandidate = serde_json::from_str(&json).unwrap();
        assert_eq!(round_tripped.created_at, candidate.created_at);
        assert_eq!(round_tripped.started_at, candidate.started_at);
        assert_eq!(round_tripped.completed_at, candidate.completed_at);
    }

    #[test]
    fn legacy_json_without_timestamp_fields_deserializes_to_none() {
        let json = r#"{
            "id": "t-1",
            "kind": "write",
            "role": "author",
            "work_item_id": "w-1",
            "workspace_access": { "reads": [], "writes": [] }
        }"#;
        let task: Task = serde_json::from_str(json).unwrap();
        assert_eq!(task.created_at, None);
        assert_eq!(task.started_at, None);
        assert_eq!(task.completed_at, None);
    }

    #[test]
    fn set_task_terminal_sets_completed_at_and_status() {
        let mut task = Task {
            id: "t-1".to_string(),
            kind: TaskKind::Write,
            status: TaskStatus::Executing,
            role: "author".to_string(),
            instructions: None,
            work_item_id: "w-1".to_string(),
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
        set_task_terminal(&mut task, TaskStatus::Complete);
        assert_eq!(task.status, TaskStatus::Complete);
        assert!(task.completed_at.is_some());
        chrono::DateTime::parse_from_rfc3339(task.completed_at.as_ref().unwrap()).unwrap();
    }

    #[test]
    fn set_task_terminal_is_idempotent_on_completed_at() {
        let mut task = Task {
            id: "t-1".to_string(),
            kind: TaskKind::Write,
            status: TaskStatus::Executing,
            role: "author".to_string(),
            instructions: None,
            work_item_id: "w-1".to_string(),
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
            completed_at: Some("2026-01-01T00:00:00+00:00".to_string()),
        };
        set_task_terminal(&mut task, TaskStatus::Failed);
        assert_eq!(task.status, TaskStatus::Failed);
        assert_eq!(
            task.completed_at.as_deref(),
            Some("2026-01-01T00:00:00+00:00")
        );
    }

    #[test]
    fn mark_task_started_is_idempotent() {
        let mut task = Task {
            id: "t-1".to_string(),
            kind: TaskKind::Write,
            status: TaskStatus::Planned,
            role: "author".to_string(),
            instructions: None,
            work_item_id: "w-1".to_string(),
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
            started_at: Some("2026-01-01T00:00:00+00:00".to_string()),
            completed_at: None,
        };
        mark_task_started(&mut task);
        assert_eq!(
            task.started_at.as_deref(),
            Some("2026-01-01T00:00:00+00:00")
        );
    }

    #[test]
    fn set_attempt_terminal_round_trip() {
        let mut attempt = Attempt {
            id: "a-1".to_string(),
            work_item_id: "w-1".to_string(),
            kind: AttemptKind::Write,
            status: AttemptStatus::Executing,
            coder_mapping: CoderMapping::default(),
            tasks: Vec::new(),
            review_state: None,
            artifacts: Vec::new(),
            created_at: None,
            completed_at: None,
        };
        set_attempt_terminal(&mut attempt, AttemptStatus::Complete);
        assert_eq!(attempt.status, AttemptStatus::Complete);
        assert!(attempt.completed_at.is_some());
    }

    #[test]
    fn set_merge_candidate_terminal_round_trip() {
        let mut candidate = MergeCandidate {
            id: "mc-1".to_string(),
            attempt_id: "a-1".to_string(),
            source_workspace: workspace("src"),
            target_workspace: workspace("tgt"),
            source_branch: "main".to_string(),
            target_branch: "main".to_string(),
            candidate_commit: "abc123".to_string(),
            review_state: MergeCandidateReviewState::Pending,
            merge_state: MergeCandidateMergeState::default(),
            created_at: None,
            started_at: None,
            completed_at: None,
        };
        set_merge_candidate_terminal(&mut candidate, MergeCandidateMergeStatus::Merged);
        assert!(candidate.completed_at.is_some());
    }

    #[test]
    fn mark_merge_candidate_started_is_idempotent() {
        let mut candidate = MergeCandidate {
            id: "mc-1".to_string(),
            attempt_id: "a-1".to_string(),
            source_workspace: workspace("src"),
            target_workspace: workspace("tgt"),
            source_branch: "main".to_string(),
            target_branch: "main".to_string(),
            candidate_commit: "abc123".to_string(),
            review_state: MergeCandidateReviewState::Pending,
            merge_state: MergeCandidateMergeState::default(),
            created_at: None,
            started_at: Some("2026-01-01T00:00:00+00:00".to_string()),
            completed_at: None,
        };
        mark_merge_candidate_started(&mut candidate);
        assert_eq!(
            candidate.started_at.as_deref(),
            Some("2026-01-01T00:00:00+00:00")
        );
    }

    #[test]
    fn initial_attempt_populates_created_at_timestamps() {
        let mut work_item = WorkItem {
            id: "work-1".to_string(),
            title: "Timestamp test".to_string(),
            planning_context: None,
            instructions: None,
            abandonment: None,
            attempts: Vec::new(),
            merge_candidates: Vec::new(),
        };
        work_item.add_initial_attempt("attempt-1").unwrap();
        let attempt = &work_item.attempts[0];
        assert!(attempt.created_at.is_some());
        let task = &attempt.tasks[0];
        assert!(task.created_at.is_some());
        assert_eq!(task.started_at, None);
        assert_eq!(task.completed_at, None);
    }

    #[test]
    fn merge_candidate_creation_populates_created_at() {
        let mut work_item = work_item_with_completed_write("work-ts");
        work_item.attempts[0].review_state = Some(AttemptReviewState::Passed);
        let _candidate_id = work_item
            .create_or_get_merge_candidate("attempt-1")
            .unwrap();
        let candidate = &work_item.merge_candidates[0];
        assert!(candidate.created_at.is_some());
        assert_eq!(candidate.started_at, None);
        assert_eq!(candidate.completed_at, None);
    }

    #[test]
    fn merge_state_round_trips_with_auto_merge_skipped() {
        let state = MergeCandidateMergeState {
            status: MergeCandidateMergeStatus::Pending,
            merged_commit: None,
            failure_reason: None,
            check_artifacts: Vec::new(),
            review_artifacts: Vec::new(),
            auto_merge_skipped: Some(true),
        };
        let json = serde_json::to_string(&state).unwrap();
        let parsed: MergeCandidateMergeState = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.auto_merge_skipped, Some(true));
    }

    #[test]
    fn merge_state_skips_serializing_auto_merge_skipped_when_none() {
        let state = MergeCandidateMergeState::default();
        let json = serde_json::to_string(&state).unwrap();
        assert!(!json.contains("auto_merge_skipped"));
    }

    #[test]
    fn legacy_merge_state_json_deserializes_with_none_skipped() {
        let json = r#"{"status":"pending"}"#;
        let state: MergeCandidateMergeState = serde_json::from_str(json).unwrap();
        assert_eq!(state.auto_merge_skipped, None);
        assert_eq!(state.status, MergeCandidateMergeStatus::Pending);
    }

    #[test]
    fn mark_merge_candidate_auto_merge_skipped_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let store = WorkModelStore::new(dir.path());
        let mut work_item = work_item_with_completed_write("work-skip");
        work_item.attempts[0].review_state = Some(AttemptReviewState::Passed);
        work_item
            .create_or_get_merge_candidate("attempt-1")
            .unwrap();
        store.create_work_item(&work_item).unwrap();

        // Set auto_merge_skipped
        let mut item = store.read_work_item("work-skip").unwrap();
        item.merge_candidates[0].merge_state.auto_merge_skipped = Some(true);
        store.write_work_item(&item).unwrap();

        // Re-read and verify
        let reloaded = store.read_work_item("work-skip").unwrap();
        assert_eq!(
            reloaded.merge_candidates[0].merge_state.auto_merge_skipped,
            Some(true)
        );
    }

    #[test]
    fn coder_mapping_round_trips_json() {
        let mapping = CoderMapping {
            write: CoderModelPair {
                coder: CoderKind::Pi,
                model: "qwen3-30b-a3b".to_string(),
            },
            review: CoderModelPair {
                coder: CoderKind::Claude,
                model: "claude-opus-4-6".to_string(),
            },
            behavior_tests: CoderModelPair {
                coder: CoderKind::Codex,
                model: "o3".to_string(),
            },
        };
        let json = serde_json::to_string(&mapping).unwrap();
        assert!(json.contains("\"behavior-tests\""));
        let parsed: CoderMapping = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, mapping);
    }

    #[test]
    fn attempt_without_coder_mapping_deserializes_with_default() {
        let json = r#"{"id":"a1","work_item_id":"w1","status":"planned"}"#;
        let record: Attempt = serde_json::from_str(json).unwrap();
        assert_eq!(record.coder_mapping.write.coder, CoderKind::Claude);
        assert_eq!(record.coder_mapping.review.coder, CoderKind::Claude);
        assert_eq!(record.coder_mapping.behavior_tests.coder, CoderKind::Claude);
    }

    #[test]
    fn attempt_with_coder_mapping_round_trips() {
        let attempt = Attempt {
            id: "a1".to_string(),
            work_item_id: "w1".to_string(),
            kind: AttemptKind::Write,
            status: AttemptStatus::Planned,
            coder_mapping: CoderMapping {
                write: CoderModelPair {
                    coder: CoderKind::Pi,
                    model: "qwen3-30b-a3b".to_string(),
                },
                review: CoderModelPair {
                    coder: CoderKind::Claude,
                    model: "claude-opus-4-6".to_string(),
                },
                behavior_tests: CoderModelPair {
                    coder: CoderKind::Claude,
                    model: "claude-opus-4-6".to_string(),
                },
            },
            tasks: Vec::new(),
            review_state: None,
            artifacts: Vec::new(),
            created_at: None,
            completed_at: None,
        };
        let json = serde_json::to_string(&attempt).unwrap();
        let parsed: Attempt = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.coder_mapping.write.coder, CoderKind::Pi);
        assert_eq!(parsed.coder_mapping.review.coder, CoderKind::Claude);
    }

    #[test]
    fn coder_mapping_for_task_kind_returns_correct_pair() {
        let mapping = CoderMapping {
            write: CoderModelPair {
                coder: CoderKind::Pi,
                model: "write-model".to_string(),
            },
            review: CoderModelPair {
                coder: CoderKind::Claude,
                model: "review-model".to_string(),
            },
            behavior_tests: CoderModelPair {
                coder: CoderKind::Codex,
                model: "bt-model".to_string(),
            },
        };
        assert_eq!(mapping.for_task_kind(TaskKind::Write).coder, CoderKind::Pi);
        assert_eq!(
            mapping.for_task_kind(TaskKind::Review).coder,
            CoderKind::Claude
        );
        assert_eq!(
            mapping.for_task_kind(TaskKind::BehaviorTests).coder,
            CoderKind::Codex
        );
    }

    #[test]
    fn resolve_coder_mapping_default_when_nothing_set() {
        let inputs = CoderMappingInputs::default();
        let mapping = resolve_coder_mapping(&inputs).unwrap();
        assert_eq!(mapping.write.coder, CoderKind::Claude);
        assert_eq!(mapping.review.coder, CoderKind::Claude);
        assert_eq!(mapping.behavior_tests.coder, CoderKind::Claude);
        assert!(!mapping.write.model.is_empty());
    }

    #[test]
    fn resolve_coder_mapping_factory_coder_sets_all_task_kinds() {
        let inputs = CoderMappingInputs {
            global_coder: Some("pi".to_string()),
            ..Default::default()
        };
        let mapping = resolve_coder_mapping(&inputs).unwrap();
        assert_eq!(mapping.write.coder, CoderKind::Pi);
        assert_eq!(mapping.review.coder, CoderKind::Pi);
        assert_eq!(mapping.behavior_tests.coder, CoderKind::Pi);
    }

    #[test]
    fn resolve_coder_mapping_per_task_cli_flag_wins() {
        let inputs = CoderMappingInputs {
            global_coder: Some("claude".to_string()),
            write_coder: Some("pi".to_string()),
            write_model: Some("custom-model".to_string()),
            ..Default::default()
        };
        let mapping = resolve_coder_mapping(&inputs).unwrap();
        assert_eq!(mapping.write.coder, CoderKind::Pi);
        assert_eq!(mapping.write.model, "custom-model");
        assert_eq!(mapping.review.coder, CoderKind::Claude);
    }

    #[test]
    fn resolve_coder_mapping_stores_resolved_model_at_creation() {
        let inputs = CoderMappingInputs::default();
        let mapping = resolve_coder_mapping(&inputs).unwrap();
        assert!(
            !mapping.write.model.is_empty(),
            "model should be resolved at creation"
        );
        assert!(
            !mapping.review.model.is_empty(),
            "model should be resolved at creation"
        );
        assert!(
            !mapping.behavior_tests.model.is_empty(),
            "model should be resolved at creation"
        );
    }
}
