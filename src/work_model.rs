use serde::{Deserialize, Serialize};
use std::error::Error;
use std::fmt;
use std::str::FromStr;

/// Durable unit of planned Factory work.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkItem {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub attempts: Vec<Attempt>,
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

/// Schedulable unit of work.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub kind: TaskKind,
    pub role: String,
    pub work_item_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attempt_id: Option<String>,
    pub workspace_access: WorkspaceAccess,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact_area: Option<TaskArtifactArea>,
}

impl Task {
    pub fn validate(&self) -> Result<(), WorkModelError> {
        self.workspace_access.validate()?;
        if self.kind == TaskKind::Review && !self.workspace_access.writes.is_empty() {
            return Err(WorkModelError::ReviewTaskWritesWorkspace {
                task_id: self.id.clone(),
            });
        }
        Ok(())
    }
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
    pub review_state: MergeCandidateReviewState,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkModelError {
    MultipleWriteWorkspaces { count: usize },
    ReviewTaskWritesWorkspace { task_id: String },
}

impl fmt::Display for WorkModelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MultipleWriteWorkspaces { count } => {
                write!(f, "task writes {count} workspaces; at most one is allowed")
            }
            Self::ReviewTaskWritesWorkspace { task_id } => {
                write!(f, "review task {task_id} cannot write a workspace")
            }
        }
    }
}

impl Error for WorkModelError {}

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
        Task {
            id: "task-1".to_string(),
            kind,
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
            review_state: MergeCandidateReviewState::Passed,
        };

        assert_eq!(attempt.review_state, Some(AttemptReviewState::Uncertain));
        assert_eq!(candidate.review_state, MergeCandidateReviewState::Passed);
    }
}
