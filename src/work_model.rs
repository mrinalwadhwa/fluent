use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::error::Error;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;

pub const WORK_MODEL_DIR: &str = ".factory/work";
pub const WORK_ITEMS_DIR: &str = "items";
pub const WORK_ARTIFACTS_DIR: &str = ".factory/work/artifacts";

/// Durable unit of planned Factory work.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkItem {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub attempts: Vec<Attempt>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub merge_candidates: Vec<MergeCandidate>,
}

impl WorkItem {
    pub fn add_initial_attempt(
        &mut self,
        attempt_id: impl Into<String>,
    ) -> Result<(), WorkModelError> {
        let attempt_id = attempt_id.into();
        validate_id("attempt", &attempt_id)?;
        if self.attempts.iter().any(|attempt| attempt.id == attempt_id) {
            return Err(WorkModelError::AttemptAlreadyExists { id: attempt_id });
        }

        let task_id = format!("{attempt_id}-write");
        self.attempts.push(Attempt {
            id: attempt_id.clone(),
            work_item_id: self.id.clone(),
            status: AttemptStatus::Planned,
            tasks: vec![Task {
                id: task_id,
                kind: TaskKind::Write,
                status: TaskStatus::Planned,
                role: "author".to_string(),
                work_item_id: self.id.clone(),
                attempt_id: Some(attempt_id.clone()),
                workspace_access: WorkspaceAccess {
                    reads: Vec::new(),
                    writes: vec![WorkspaceRef {
                        id: "candidate".to_string(),
                        path: format!(".factory/work/workspaces/{attempt_id}"),
                    }],
                },
                artifact_area: None,
                review_context: None,
                input_artifacts: Vec::new(),
                output: None,
            }],
            review_state: None,
            artifacts: Vec::new(),
        });

        self.validate()
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
        let existing_review_tasks = attempt
            .tasks
            .iter()
            .filter(|task| task.kind == TaskKind::Review)
            .count();
        let next_round = existing_review_tasks / roles.len() + 1;
        let round = (next_round > 1).then_some(next_round);
        self.add_review_tasks_with_round(attempt_id, roles, round)
    }

    fn add_review_tasks_with_round(
        &mut self,
        attempt_id: &str,
        roles: &[&str],
        round: Option<usize>,
    ) -> Result<Vec<String>, WorkModelError> {
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

        let candidate = WorkspaceRef {
            id: write_output.workspace_id.clone(),
            path: write_output.workspace_path.clone(),
        };
        let mut task_ids = Vec::new();
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
            attempt.tasks.push(Task {
                id: task_id.clone(),
                kind: TaskKind::Review,
                status: TaskStatus::Planned,
                role: (*role).to_string(),
                work_item_id: self.id.clone(),
                attempt_id: Some(attempt_id.to_string()),
                workspace_access: WorkspaceAccess::read_only(vec![candidate.clone()]),
                artifact_area: Some(TaskArtifactArea {
                    path: format!("{WORK_ARTIFACTS_DIR}/{attempt_id}/{task_id}"),
                }),
                review_context: Some(ReviewContext {
                    candidate_workspace_id: write_output.workspace_id.clone(),
                    candidate_workspace_path: write_output.workspace_path.clone(),
                    source_branch: write_output.source_branch.clone(),
                    candidate_commit: write_output.commit.clone(),
                }),
                input_artifacts: Vec::new(),
                output: None,
            });
            task_ids.push(task_id);
        }
        attempt.status = AttemptStatus::Reviewing;
        attempt.review_state = Some(AttemptReviewState::NotReviewed);

        self.validate()?;
        Ok(task_ids)
    }

    pub fn add_followup_write_task(
        &mut self,
        attempt_id: &str,
        input_artifacts: Vec<ArtifactRef>,
    ) -> Result<String, WorkModelError> {
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

        let next = attempt
            .tasks
            .iter()
            .filter(|task| task.kind == TaskKind::Write && task.id.contains("-followup-"))
            .count()
            + 1;
        let task_id = format!("{attempt_id}-followup-{next}");
        validate_id("task", &task_id)?;
        if attempt.tasks.iter().any(|task| task.id == task_id) {
            return Err(WorkModelError::TaskAlreadyExists { id: task_id });
        }

        attempt.tasks.push(Task {
            id: task_id.clone(),
            kind: TaskKind::Write,
            status: TaskStatus::Planned,
            role: "author".to_string(),
            work_item_id: self.id.clone(),
            attempt_id: Some(attempt_id.to_string()),
            workspace_access: WorkspaceAccess {
                reads: Vec::new(),
                writes: vec![WorkspaceRef {
                    id: write_output.workspace_id,
                    path: write_output.workspace_path,
                }],
            },
            artifact_area: None,
            review_context: None,
            input_artifacts,
            output: None,
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
        });

        self.validate()?;
        Ok(candidate_id)
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

/// One execution history branch for a work item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attempt {
    pub id: String,
    pub work_item_id: String,
    pub status: AttemptStatus,
    #[serde(default)]
    pub tasks: Vec<Task>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub review_state: Option<AttemptReviewState>,
    #[serde(default)]
    pub artifacts: Vec<ArtifactRef>,
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
            task.validate()?;
            if self.status == AttemptStatus::Complete && task.status != TaskStatus::Complete {
                return Err(WorkModelError::CompleteAttemptHasIncompleteTask {
                    attempt_id: self.id.clone(),
                    task_id: task.id.clone(),
                    task_status: task.status.clone(),
                });
            }
        }
        Ok(())
    }
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<TaskOutput>,
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
    Report,
    Learn,
    Probe,
}

impl TaskKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Write => "write",
            Self::Review => "review",
            Self::Merge => "merge",
            Self::Report => "report",
            Self::Learn => "learn",
            Self::Probe => "probe",
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
            "report" => Ok(Self::Report),
            "learn" => Ok(Self::Learn),
            "probe" => Ok(Self::Probe),
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
    pub landed_commit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub check_artifacts: Vec<ArtifactRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub review_artifacts: Vec<ArtifactRef>,
}

impl Default for MergeCandidateMergeState {
    fn default() -> Self {
        Self {
            status: MergeCandidateMergeStatus::Pending,
            landed_commit: None,
            failure_reason: None,
            check_artifacts: Vec::new(),
            review_artifacts: Vec::new(),
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
    Landed,
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

    pub fn work_item_path(&self, id: &str) -> Result<PathBuf, WorkModelStorageError> {
        work_item_file_name(id).map(|file_name| self.work_items_dir().join(file_name))
    }

    pub fn list_work_items(&self) -> Result<Vec<WorkItem>, WorkModelStorageError> {
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

        paths
            .into_iter()
            .map(|path| self.read_work_item_file(&path, true))
            .collect()
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

        let json =
            to_json_pretty(work_item).map_err(|source| WorkModelStorageError::ParseFile {
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
                .map_err(|source| WorkModelStorageError::WriteFile { path, source })
        } else {
            fs::write(&path, json)
                .map_err(|source| WorkModelStorageError::WriteFile { path, source })
        }
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
        let work_item: WorkItem =
            from_json(&content).map_err(|source| WorkModelStorageError::ParseFile {
                path: path.to_path_buf(),
                source,
            })?;
        if let Some(expected) = path.file_stem().and_then(|stem| stem.to_str()) {
            work_item_file_name(expected)?;
            if work_item.id != expected {
                return Err(WorkModelStorageError::WorkItemIdMismatch {
                    path: path.to_path_buf(),
                    expected: expected.to_string(),
                    actual: work_item.id.clone(),
                });
            }
        }
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
}

fn work_item_file_name(id: &str) -> Result<String, WorkModelStorageError> {
    if !is_file_safe_id(id) {
        return Err(WorkModelStorageError::InvalidWorkItemId { id: id.to_string() });
    }
    Ok(format!("{id}.json"))
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
            output: None,
        }
    }

    #[test]
    fn task_kind_parses_generic_kinds() {
        assert_eq!("write".parse::<TaskKind>().unwrap(), TaskKind::Write);
        assert_eq!("review".parse::<TaskKind>().unwrap(), TaskKind::Review);
        assert_eq!("merge".parse::<TaskKind>().unwrap(), TaskKind::Merge);
        assert_eq!("report".parse::<TaskKind>().unwrap(), TaskKind::Report);
        assert_eq!("learn".parse::<TaskKind>().unwrap(), TaskKind::Learn);
        assert_eq!("probe".parse::<TaskKind>().unwrap(), TaskKind::Probe);
        assert!("triage".parse::<TaskKind>().is_err());
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
    fn review_tasks_use_latest_completed_write_output() {
        let mut work_item = WorkItem {
            id: "work-1".to_string(),
            title: "Review latest candidate".to_string(),
            attempts: vec![Attempt {
                id: "attempt-1".to_string(),
                work_item_id: "work-1".to_string(),
                status: AttemptStatus::Planned,
                tasks: vec![
                    completed_write_task("attempt-1-write", "original"),
                    completed_write_task("attempt-1-followup-1", "followup"),
                ],
                review_state: Some(AttemptReviewState::Failed),
                artifacts: Vec::new(),
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
            ".factory/work/workspaces/attempt-1-followup"
        );
    }

    #[test]
    fn attempt_artifacts_round_trip_with_work_item() {
        let work_item = WorkItem {
            id: "work-1".to_string(),
            title: "Define the core work model".to_string(),
            attempts: vec![Attempt {
                id: "attempt-1".to_string(),
                work_item_id: "work-1".to_string(),
                status: AttemptStatus::Complete,
                tasks: vec![task(TaskKind::Write, vec![workspace("candidate")])],
                review_state: Some(AttemptReviewState::Passed),
                artifacts: vec![ArtifactRef {
                    producer_id: "task-1".to_string(),
                    path: ".factory/tasks/task-1/report.md".to_string(),
                }],
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
            attempts: vec![Attempt {
                id: "attempt-1".to_string(),
                work_item_id: "work-1".to_string(),
                status: AttemptStatus::Complete,
                tasks: vec![
                    completed_write_task("attempt-1-write", "original"),
                    completed_write_task("attempt-1-followup-1", "followup"),
                ],
                review_state: Some(AttemptReviewState::Passed),
                artifacts: Vec::new(),
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
            ".factory/work/workspaces/attempt-1-followup"
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
            attempts: vec![Attempt {
                id: "attempt-1".to_string(),
                work_item_id: "work-1".to_string(),
                status: AttemptStatus::Complete,
                tasks: vec![completed_write_task("attempt-1-write", "original")],
                review_state: Some(AttemptReviewState::Passed),
                artifacts: Vec::new(),
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
            attempts: vec![Attempt {
                id: "attempt-1".to_string(),
                work_item_id: "work-1".to_string(),
                status: AttemptStatus::Complete,
                tasks: vec![completed_write_task("attempt-1-write", "original")],
                review_state: Some(AttemptReviewState::Passed),
                artifacts: Vec::new(),
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
            attempts: vec![Attempt {
                id: "attempt-1".to_string(),
                work_item_id: "work-1".to_string(),
                status: AttemptStatus::Reviewing,
                tasks: vec![completed_write_task("attempt-1-write", "original")],
                review_state: Some(AttemptReviewState::Uncertain),
                artifacts: Vec::new(),
            }],
            merge_candidates: Vec::new(),
        };
        work_item.merge_candidates.push(MergeCandidate {
            id: "attempt-1-merge-candidate".to_string(),
            attempt_id: "attempt-1".to_string(),
            source_workspace: WorkspaceRef {
                id: "candidate".to_string(),
                path: ".factory/work/workspaces/attempt-1-original".to_string(),
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
            attempts: vec![Attempt {
                id: "attempt-1".to_string(),
                work_item_id: "work-1".to_string(),
                status: AttemptStatus::Complete,
                tasks: vec![completed_write_task("attempt-1-write", "original")],
                review_state: Some(AttemptReviewState::Passed),
                artifacts: Vec::new(),
            }],
            merge_candidates: Vec::new(),
        };
        work_item.merge_candidates.push(MergeCandidate {
            id: "attempt-1-merge-candidate".to_string(),
            attempt_id: "attempt-1".to_string(),
            source_workspace: WorkspaceRef {
                id: "candidate".to_string(),
                path: ".factory/work/workspaces/attempt-1-original".to_string(),
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
            attempts: vec![Attempt {
                id: "attempt-1".to_string(),
                work_item_id: "work-1".to_string(),
                status: AttemptStatus::Reviewing,
                tasks: vec![completed_write_task("attempt-1-write", "original")],
                review_state: Some(AttemptReviewState::Failed),
                artifacts: Vec::new(),
            }],
            merge_candidates: Vec::new(),
        };
        work_item.merge_candidates.push(MergeCandidate {
            id: "attempt-1-merge-candidate".to_string(),
            attempt_id: "attempt-1".to_string(),
            source_workspace: WorkspaceRef {
                id: "candidate".to_string(),
                path: ".factory/work/workspaces/attempt-1-original".to_string(),
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
                landed_commit: None,
                failure_reason: Some("Attempt review failed".to_string()),
                check_artifacts: Vec::new(),
                review_artifacts: Vec::new(),
            },
        });

        work_item.validate().unwrap();
    }

    #[test]
    fn failed_merge_candidate_still_requires_candidate_provenance() {
        let mut work_item = WorkItem {
            id: "work-1".to_string(),
            title: "Validate failed merge candidate provenance".to_string(),
            attempts: vec![Attempt {
                id: "attempt-1".to_string(),
                work_item_id: "work-1".to_string(),
                status: AttemptStatus::Complete,
                tasks: vec![completed_write_task("attempt-1-write", "original")],
                review_state: Some(AttemptReviewState::Passed),
                artifacts: Vec::new(),
            }],
            merge_candidates: Vec::new(),
        };
        work_item.merge_candidates.push(MergeCandidate {
            id: "attempt-1-merge-candidate".to_string(),
            attempt_id: "attempt-1".to_string(),
            source_workspace: WorkspaceRef {
                id: "candidate".to_string(),
                path: ".factory/work/workspaces/attempt-1-original".to_string(),
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
                landed_commit: None,
                failure_reason: Some("candidate_commit mismatch".to_string()),
                check_artifacts: Vec::new(),
                review_artifacts: Vec::new(),
            },
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
            status: AttemptStatus::Reviewing,
            tasks: Vec::new(),
            review_state: Some(AttemptReviewState::Uncertain),
            artifacts: Vec::new(),
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
        };

        assert_eq!(attempt.review_state, Some(AttemptReviewState::Uncertain));
        assert_eq!(candidate.review_state, MergeCandidateReviewState::Passed);
    }

    fn completed_write_task(id: &str, suffix: &str) -> Task {
        Task {
            id: id.to_string(),
            kind: TaskKind::Write,
            status: TaskStatus::Complete,
            role: "author".to_string(),
            work_item_id: "work-1".to_string(),
            attempt_id: Some("attempt-1".to_string()),
            workspace_access: WorkspaceAccess {
                reads: Vec::new(),
                writes: vec![WorkspaceRef {
                    id: "candidate".to_string(),
                    path: format!(".factory/work/workspaces/attempt-1-{suffix}"),
                }],
            },
            artifact_area: None,
            review_context: None,
            input_artifacts: Vec::new(),
            output: Some(TaskOutput {
                workspace_id: "candidate".to_string(),
                workspace_path: format!(".factory/work/workspaces/attempt-1-{suffix}"),
                source_branch: "main".to_string(),
                commit: format!("commit-{suffix}"),
            }),
        }
    }
}
