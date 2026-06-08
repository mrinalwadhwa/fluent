use serde::{Deserialize, Serialize};
use std::error::Error;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;

pub const WORK_MODEL_DIR: &str = ".factory/work";
pub const WORK_ITEMS_DIR: &str = "items";

/// Durable unit of planned Factory work.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkItem {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub attempts: Vec<Attempt>,
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
            }],
            review_state: None,
            artifacts: Vec::new(),
        });

        self.validate()
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
        if self.kind == TaskKind::Review && self.artifact_area.is_none() {
            return Err(WorkModelError::ReviewTaskMissingArtifactArea {
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
    InvalidId {
        kind: &'static str,
        id: String,
    },
    AttemptAlreadyExists {
        id: String,
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
            Self::MultipleWriteWorkspaces { count } => {
                write!(f, "task writes {count} workspaces; at most one is allowed")
            }
            Self::ReviewTaskWritesWorkspace { task_id } => {
                write!(f, "review task {task_id} cannot write a workspace")
            }
            Self::ReviewTaskMissingArtifactArea { task_id } => {
                write!(f, "review task {task_id} must declare an artifact area")
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
            .map(|path| self.read_work_item_file(&path))
            .collect()
    }

    pub fn read_work_item(&self, id: &str) -> Result<WorkItem, WorkModelStorageError> {
        let path = self.work_item_path(id)?;
        self.read_work_item_file(&path)
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

    fn read_work_item_file(&self, path: &Path) -> Result<WorkItem, WorkModelStorageError> {
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
        work_item
            .validate()
            .map_err(|source| WorkModelStorageError::InvalidModel {
                path: path.to_path_buf(),
                source,
            })?;
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
