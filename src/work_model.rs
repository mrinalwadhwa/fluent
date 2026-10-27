use serde::{Deserialize, Deserializer, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::error::Error;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::io::ErrorKind;
use std::path::{Component, Path, PathBuf};
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use rustix::fs::{FlockOperation, flock};
use sha2::{Digest, Sha256};

use crate::coder::CoderKind;

pub fn now_iso8601() -> String {
    chrono::Utc::now().to_rfc3339()
}

pub const WORK_MODEL_DIR: &str = ".fluent/work";
pub const WORK_ITEMS_DIR: &str = "items";
pub const WORK_ATTEMPTS_DIR: &str = "attempts";
pub const WORK_TASKS_DIR: &str = "tasks";
pub const WORK_MERGE_CANDIDATES_DIR: &str = "merge-candidates";
const WORK_TRANSACTIONS_DIR: &str = "transactions";
pub const WORK_ARTIFACTS_DIR: &str = ".fluent/work/artifacts";
pub const WORK_PROGRESS_DIR: &str = ".fluent/work/progress";
pub const WORK_MODEL_WRITER_VERSION: &str = env!("CARGO_PKG_VERSION");
/// Version of the split Work Item and Attempt record format. Records written
/// before this marker omit the field and are read as version 0. They remain
/// readable, but the explicit `work-item upgrade` command rewrites them using
/// this version after checking every record.
pub const WORK_MODEL_RECORD_SCHEMA_VERSION: u32 = 14;
const NO_CHANGE_COMPLETION_RECORD_SCHEMA_VERSION: u32 = 15;
const CLAUDE_SESSION_RECORD_SCHEMA_VERSION: u32 = 16;
pub const WORK_MODEL_UPGRADE_SCHEMA_VERSION: u32 = 1;
pub const MIN_ADDITIONAL_WRITE_ROUNDS: usize = 1;
pub const MAX_ADDITIONAL_WRITE_ROUNDS: usize = 3;
pub const MAX_ROUND_CAP_RESOLUTION_BYTES: usize = 4_096;
pub const RECOVERY_OPERATION_SCHEMA_VERSION: u32 = 1;
/// Version of the immutable assignment captured for a corrective Writer.
pub const WRITER_CORRECTION_ASSIGNMENT_SCHEMA_VERSION: u32 = 2;
pub const MAX_WRITER_CORRECTION_ASSIGNMENT_INPUTS: usize = 512;
pub const MAX_WRITER_CORRECTION_ASSIGNMENT_FINDINGS: usize = 512;
pub const FAILED_COMMAND_RECHECK_ROLE: &str = "failed-command-recheck";
pub const TESTER_HARNESS_REPAIR_ROLE: &str = "tester-harness-repair";
pub const CANDIDATE_ANCESTRY_REPAIR_ROLE: &str = "candidate-ancestry-repair";
/// Writer role used by the explicit, audited metadata-only candidate recovery.
/// The role remains an ordinary Writer so its replacement candidate passes the
/// normal candidate gate, Tester, reviewers, and Learner safeguards.
pub const CANDIDATE_METADATA_REPAIR_ROLE: &str = "candidate-metadata-repair";
/// Instruction suffix that binds a derived project-check artifact to a
/// metadata recovery Writer's readable inputs.
pub(crate) const CANDIDATE_METADATA_DERIVED_PROJECT_CHECK_MARKER: &str =
    "\nDerived project-check artifact: ";

fn is_canonical_work_artifact_file(path: &str, filename: &str) -> bool {
    let source = Path::new(path);
    if source.as_os_str().is_empty() || source.is_absolute() {
        return false;
    }
    let mut rebuilt = PathBuf::new();
    for component in source.components() {
        let Component::Normal(part) = component else {
            return false;
        };
        rebuilt.push(part);
    }
    rebuilt.as_os_str() == source.as_os_str()
        && rebuilt.starts_with(WORK_ARTIFACTS_DIR)
        && rebuilt.file_name().is_some_and(|name| name == filename)
}

pub fn work_item_input_path(work_item_id: &str, index: usize, filename: &str) -> String {
    format!("{WORK_ARTIFACTS_DIR}/{work_item_id}/inputs/{index:04}-{filename}")
}

pub fn validate_work_item_id(id: &str) -> Result<(), WorkModelError> {
    validate_id("work item", id)
}

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

/// The persisted revision a store read observed. Aggregate writes compare it
/// under the Work Item's model lock before replacing any split records.
pub struct StorageRevision {
    observed: AtomicBool,
    value: AtomicU64,
}

impl StorageRevision {
    fn persisted(value: u64) -> Self {
        Self {
            observed: AtomicBool::new(true),
            value: AtomicU64::new(value),
        }
    }

    pub(crate) fn get(&self) -> Option<u64> {
        self.observed
            .load(Ordering::SeqCst)
            .then(|| self.value.load(Ordering::SeqCst))
    }

    fn set(&self, revision: u64) {
        self.value.store(revision, Ordering::SeqCst);
        self.observed.store(true, Ordering::SeqCst);
    }
}

impl Default for StorageRevision {
    fn default() -> Self {
        Self {
            observed: AtomicBool::new(false),
            value: AtomicU64::new(0),
        }
    }
}

impl Clone for StorageRevision {
    fn clone(&self) -> Self {
        match self.get() {
            Some(revision) => Self::persisted(revision),
            None => Self::default(),
        }
    }
}

impl fmt::Debug for StorageRevision {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.get().fmt(formatter)
    }
}

impl PartialEq for StorageRevision {
    fn eq(&self, other: &Self) -> bool {
        self.get() == other.get()
    }
}

impl Eq for StorageRevision {}

/// Policy stored on a Work Item that controls whether its pre-land Learner may
/// capture project expertise.
///
/// `Capture` is the default and preserves the ordinary pre-land Learner, which
/// may produce one canonical `Update expertise` commit. `NoExpertise` still runs
/// Learner and produces a handoff after reviews, but denies expertise writes and
/// candidate Git mutations so the reviewed Writer SHA is never retargeted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LearnerMode {
    /// Ordinary pre-land Learner that may capture project expertise.
    #[default]
    Capture,
    /// Pre-land Learner that audits the accepted change and produces a handoff
    /// while denying expertise and candidate Git mutations.
    NoExpertise,
}

impl LearnerMode {
    /// Whether this is the default `capture` policy. Used to omit the field from
    /// minimal split-record JSON so legacy Work stays byte-stable.
    pub fn is_capture(&self) -> bool {
        matches!(self, Self::Capture)
    }
}

impl fmt::Display for LearnerMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Capture => "capture",
            Self::NoExpertise => "no-expertise",
        })
    }
}

impl FromStr for LearnerMode {
    type Err = ParseLearnerModeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim() {
            "capture" => Ok(Self::Capture),
            "no-expertise" => Ok(Self::NoExpertise),
            other => Err(ParseLearnerModeError(other.to_string())),
        }
    }
}

/// Durable unit of planned Fluent work.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkItem {
    pub id: String,
    pub title: String,
    /// Fluent version that most recently wrote this Work Item's model records.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_writer_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub planning_context: Option<PlanningContext>,
    /// Deterministic intake diagnostic derived from approved behaviors and
    /// required Plan rows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub planning_scope: Option<PlanningScopeAssessment>,
    /// Fixed acceptance criteria and classified findings for release work.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_contract: Option<ReleaseContract>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub abandonment: Option<WorkItemAbandonment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub post_merge_review_fix_depth: Option<u64>,
    /// The pre-land Learner policy for this Work Item. A missing value resolves to
    /// the default `capture`. The aggregate always materializes the resolved mode
    /// (so `work-item show` reports it), while split storage omits the default so
    /// legacy and minimal records stay byte-stable.
    #[serde(default)]
    pub learner_mode: LearnerMode,
    /// How this Work Item came to exist. Legacy and ordinary planned Work are
    /// lineage roots; derived Work carries its originating provenance.
    #[serde(default, skip_serializing_if = "WorkOrigin::is_planned")]
    pub origin: WorkOrigin,
    /// Whether this Work Item may execute, and the authority that authorized it.
    /// Legacy Work with no stored authorization is treated as execution-ready.
    #[serde(
        default,
        skip_serializing_if = "ExecutionAuthorization::is_unattributed_ready"
    )]
    pub authorization: ExecutionAuthorization,
    /// The lineage this Work Item belongs to and whether it has been charged as
    /// an autonomous descendant. A default lineage marks an uncharged root.
    #[serde(default, skip_serializing_if = "WorkLineage::is_uncharged_root")]
    pub lineage: WorkLineage,
    /// Immutable corrective execution input for derived corrective Work created
    /// without brief, behaviors, approach, or plan artifacts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub corrective_context: Option<CorrectiveContext>,
    /// Accepted proposal details that make derived corrective Work auditable
    /// after cleanup removes its originating Work records and handoff.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub corrective_audit: Option<CorrectiveAuditContext>,
    /// A durable intent to enqueue this Work on the regular Work Queue once its
    /// execution is authorized. Recorded so an authorization that crashes before
    /// the queue write can be reconciled on retry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_enqueue: Option<EnqueueIntent>,
    /// Immutable snapshots of managed files approved when this Work Item was
    /// created. Every Task copies these references into its read boundary.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub input_artifacts: Vec<WorkItemInputArtifact>,
    /// Store-managed optimistic revision for aggregate split-record writes.
    #[doc(hidden)]
    #[serde(skip)]
    pub storage_revision: StorageRevision,
    #[serde(default)]
    pub attempts: Vec<Attempt>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub merge_candidates: Vec<MergeCandidate>,
    /// Immutable lifecycle entries derived while the aggregate is held under
    /// its model lock. Consumers keep their own cursors; they never infer a
    /// durable change by comparing a previous Slack reply with current state.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub transitions: Vec<DurableWorkTransition>,
}

impl PartialEq for WorkItem {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
            && self.title == other.title
            && self.planning_context == other.planning_context
            && self.planning_scope == other.planning_scope
            && self.release_contract == other.release_contract
            && self.instructions == other.instructions
            && self.abandonment == other.abandonment
            && self.post_merge_review_fix_depth == other.post_merge_review_fix_depth
            && self.learner_mode == other.learner_mode
            && self.origin == other.origin
            && self.authorization == other.authorization
            && self.lineage == other.lineage
            && self.corrective_context == other.corrective_context
            && self.corrective_audit == other.corrective_audit
            && self.pending_enqueue == other.pending_enqueue
            && self.input_artifacts == other.input_artifacts
            && self.attempts == other.attempts
            && self.merge_candidates == other.merge_candidates
            && self.transitions == other.transitions
    }
}

impl Eq for WorkItem {}

impl Default for WorkItem {
    fn default() -> Self {
        Self {
            id: String::new(),
            title: String::new(),
            model_writer_version: None,
            planning_context: None,
            planning_scope: None,
            release_contract: None,
            instructions: None,
            abandonment: None,
            post_merge_review_fix_depth: None,
            learner_mode: LearnerMode::default(),
            origin: WorkOrigin::default(),
            authorization: ExecutionAuthorization::default(),
            lineage: WorkLineage::default(),
            corrective_context: None,
            corrective_audit: None,
            pending_enqueue: None,
            input_artifacts: Vec::new(),
            storage_revision: StorageRevision::default(),
            attempts: Vec::new(),
            merge_candidates: Vec::new(),
            transitions: Vec::new(),
        }
    }
}

/// One bounded lifecycle change recorded with the aggregate that caused it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DurableWorkTransition {
    pub sequence: u64,
    pub object_kind: String,
    pub object_id: String,
    pub old_state: Option<String>,
    pub new_state: String,
    pub occurred_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elapsed_seconds: Option<u64>,
    #[serde(default)]
    pub origin: DurableTransitionOrigin,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_ref: Option<String>,
}

/// Closed attribution for a lifecycle mutation recorded in the Work ledger.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DurableTransitionOrigin {
    Fluent,
    SlackThread,
    Scheduler,
    AttemptExecutor,
    TaskExecutor,
    Landing,
    PostLand,
    External,
    #[default]
    Unknown,
}

impl std::fmt::Display for DurableTransitionOrigin {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let value = match self {
            Self::Fluent => "fluent",
            Self::SlackThread => "slack-thread",
            Self::Scheduler => "scheduler",
            Self::AttemptExecutor => "attempt-executor",
            Self::TaskExecutor => "task-executor",
            Self::Landing => "landing",
            Self::PostLand => "post-land",
            Self::External => "external",
            Self::Unknown => "unknown",
        };
        formatter.write_str(value)
    }
}

/// Bounded attribution supplied by the trusted caller that owns a mutation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DurableTransitionMetadata {
    pub origin: DurableTransitionOrigin,
    pub reason: Option<String>,
    pub evidence_ref: Option<String>,
}

/// One immutable managed file that a Work-model transaction must publish along
/// with its aggregate snapshot. The bytes live in the journal so recovery can
/// finish the same publication without depending on a sidecar that was only
/// written before the transaction began.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ManagedWorkItemFile {
    pub path: String,
    pub bytes: Vec<u8>,
    pub digest: String,
}

impl ManagedWorkItemFile {
    pub(crate) fn new(path: impl Into<String>, bytes: Vec<u8>) -> Self {
        let digest = format!("sha256:{:x}", Sha256::digest(&bytes));
        Self {
            path: path.into(),
            bytes,
            digest,
        }
    }
}

/// One explicit conversion of persisted Work Item records to the current
/// storage format. The record is written beside the Work Item so an operator
/// can see which older format was accepted and when it was converted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct WorkModelUpgradeRecord {
    pub schema_version: u32,
    pub operation_id: String,
    pub work_item_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_model_writer_version: Option<String>,
    pub to_model_writer_version: String,
    pub from_record_schema_version: u32,
    pub to_record_schema_version: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attempts: Vec<WorkModelAttemptUpgrade>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_migrations: Vec<WorkModelArtifactMigration>,
    pub upgraded_at: String,
}

/// The stored artifact reference converted by an explicit upgrade.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct WorkModelArtifactMigration {
    pub from_path: String,
    pub to_path: String,
}

/// The format conversion applied to one split Attempt record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct WorkModelAttemptUpgrade {
    pub attempt_id: String,
    pub from_record_schema_version: u32,
    pub to_record_schema_version: u32,
}

/// Result returned by the public Work Item format-upgrade operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkModelUpgradeReport {
    pub work_item_id: String,
    pub operation_id: Option<String>,
    pub from_model_writer_version: Option<String>,
    pub from_record_schema_version: u32,
    pub upgraded_attempts: Vec<WorkModelAttemptUpgrade>,
    pub audit_path: Option<String>,
    pub already_current: bool,
}

pub const MAX_TRANSITION_REASON_BYTES: usize = 1_024;
pub const MAX_TRANSITION_EVIDENCE_BYTES: usize = 512;

fn fluent_transition_metadata() -> DurableTransitionMetadata {
    DurableTransitionMetadata {
        origin: DurableTransitionOrigin::Fluent,
        reason: None,
        evidence_ref: None,
    }
}

impl WorkItem {
    pub fn compatibility_warnings(&self) -> Vec<String> {
        let mut warnings = Vec::new();
        if self
            .model_writer_version
            .as_deref()
            .is_some_and(|writer| version_is_newer(writer, WORK_MODEL_WRITER_VERSION))
        {
            warnings.push(format!(
                "Work Item was written by Fluent {}; installed Fluent {} is older; upgrade before mutation",
                self.model_writer_version.as_deref().unwrap_or("unknown"),
                WORK_MODEL_WRITER_VERSION
            ));
        }
        for attempt in &self.attempts {
            if let Some(PauseKind::Unknown(value)) = attempt.pause_kind.as_ref() {
                warnings.push(format!(
                    "Attempt {} uses unknown pause kind {:?}; upgrade Fluent before mutation",
                    attempt.id, value
                ));
            }
        }
        warnings
    }

    pub fn ensure_mutation_compatible(&self) -> Result<(), WorkModelError> {
        if let Some(warning) = self.compatibility_warnings().into_iter().next() {
            return Err(WorkModelError::UnsupportedStoredState {
                work_item_id: self.id.clone(),
                reason: warning,
            });
        }
        Ok(())
    }

    /// Validate the identity fields shared by all public recovery operations.
    /// Domain-specific recovery modules still validate their own artifacts and
    /// lifecycle shape. An exact prior operation is returned so those modules
    /// can replay it without creating another Task or evidence record.
    pub fn validate_recovery_operation(
        &self,
        project_root: &Path,
        request: &RecoveryOperationRequest,
    ) -> Result<Option<RecoveryOperationRecord>, WorkModelError> {
        let attempt = self
            .attempts
            .iter()
            .find(|attempt| attempt.id == request.attempt_id)
            .ok_or_else(|| WorkModelError::RecoveryOperationIneligible {
                attempt_id: request.attempt_id.clone(),
                reason: "Attempt is not present".to_string(),
            })?;
        request.validate_shape(&attempt.id)?;
        if request.work_item_id != self.id {
            return Err(WorkModelError::RecoveryOperationIneligible {
                attempt_id: attempt.id.clone(),
                reason: "recovery request names a different Work Item".to_string(),
            });
        }
        let operation_id = request.operation_id();
        if let Some(existing) = attempt.recovery_operation(&operation_id) {
            if existing.matches_request(request) {
                return Ok(Some(existing.clone()));
            }
            return Err(WorkModelError::RecoveryOperationConflict {
                attempt_id: attempt.id.clone(),
                operation_id,
            });
        }
        if let Some(candidate) = request.candidate_commit.as_deref()
            && attempt.current_candidate_commit() != Some(candidate)
        {
            return Err(WorkModelError::RecoveryOperationIneligible {
                attempt_id: attempt.id.clone(),
                reason: format!(
                    "candidate changed: expected {candidate}, found {}",
                    attempt.current_candidate_commit().unwrap_or("none")
                ),
            });
        }
        if let Some(task_id) = request.task_id.as_deref()
            && attempt
                .tasks
                .iter()
                .find(|task| task.id == task_id)
                .is_none_or(|task| task.status == TaskStatus::Superseded)
        {
            return Err(WorkModelError::RecoveryOperationIneligible {
                attempt_id: attempt.id.clone(),
                reason: format!("Task {task_id:?} is missing or superseded"),
            });
        }
        if let Some(evidence_digest) = request.evidence_digest.as_deref()
            && !attempt
                .evidence_recoveries
                .iter()
                .any(|recovery| recovery.attachment.digest == evidence_digest)
        {
            // Attachment operations are the one shared boundary that creates
            // the evidence record it names. Their specialized validation still
            // checks the immutable document before publication; every later
            // operation must find the digest already attached.
            let is_new_host_attachment = request.operation_kind.starts_with("host-evidence-")
                && request.operation_kind.ends_with("-attachment");
            if !is_new_host_attachment {
                return Err(WorkModelError::RecoveryOperationIneligible {
                    attempt_id: attempt.id.clone(),
                    reason: format!(
                        "evidence digest {evidence_digest:?} is not attached to the Attempt"
                    ),
                });
            }
        }
        if let Some(workspace_path) = request.workspace_path.as_deref() {
            let expected = attempt
                .tasks
                .iter()
                .rev()
                .find(|task| {
                    task.kind == TaskKind::Write
                        && task.status == TaskStatus::Complete
                        && task.output.is_some()
                })
                .and_then(|task| task.output.as_ref())
                .map(|output| output.workspace_path.as_str());
            if expected != Some(workspace_path) {
                return Err(WorkModelError::RecoveryOperationIneligible {
                    attempt_id: attempt.id.clone(),
                    reason: format!(
                        "workspace changed: expected {workspace_path:?}, found {}",
                        expected.unwrap_or("none")
                    ),
                });
            }
            resolve_managed_sibling_workspace_path(
                project_root,
                workspace_path,
                "recovery operation workspace",
            )
            .map_err(|error| WorkModelError::RecoveryOperationIneligible {
                attempt_id: attempt.id.clone(),
                reason: error.to_string(),
            })?;
        }
        Ok(None)
    }

    fn preserved_input_refs(&self) -> Vec<ArtifactRef> {
        self.input_artifacts
            .iter()
            .map(|input| ArtifactRef {
                producer_id: "work-item".to_string(),
                path: input.snapshot_path.clone(),
            })
            .collect()
    }

    fn with_preserved_inputs(&self, inputs: Vec<ArtifactRef>) -> Vec<ArtifactRef> {
        let mut combined = self.preserved_input_refs();
        for input in inputs {
            if !combined.contains(&input) {
                combined.push(input);
            }
        }
        combined
    }

    /// Create a Work Item through the ordinary human-approved planning flow:
    /// execution-ready as an uncharged lineage root with no corrective context.
    pub fn planned(id: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            authorization: ExecutionAuthorization::execution_ready(ExecutionAuthority::Human),
            ..Self::default()
        }
    }

    /// Create derived corrective Work from an immutable corrective context,
    /// without brief, behaviors, approach, or plan artifacts. When
    /// `ready_authority` is `Some`, the Work is created execution-ready and its
    /// lineage is charged once at that point; otherwise it is created proposed.
    pub fn derived_corrective(
        id: impl Into<String>,
        title: impl Into<String>,
        provenance: DerivedProvenance,
        context: CorrectiveContext,
        lineage: WorkLineage,
        ready_authority: Option<ExecutionAuthority>,
    ) -> Result<Self, WorkModelError> {
        context.validate()?;
        let mut item = Self {
            id: id.into(),
            title: title.into(),
            origin: WorkOrigin::Derived { provenance },
            authorization: ExecutionAuthorization::Proposed,
            lineage,
            corrective_context: Some(context),
            ..Self::default()
        };
        if let Some(authority) = ready_authority {
            item.mark_execution_ready(authority);
        }
        item.validate()?;
        Ok(item)
    }

    /// The complete input stored on corrective Tasks and rendered into Writer
    /// and reviewer prompts. Derived corrective Work uses this context in place
    /// of fabricated planning-artifact references.
    pub fn write_task_instructions(&self) -> Option<String> {
        if let Some(context) = self.corrective_context.as_ref() {
            let mut instructions = context.to_execution_context();
            if let Some(audit) = self.corrective_audit.as_ref() {
                instructions.push_str("\n\n");
                instructions.push_str(&audit.to_execution_context());
            }
            return Some(instructions);
        }
        self.instructions
            .clone()
            .or_else(|| self.planning_context.as_ref()?.to_instructions())
    }

    /// Reject Attempt creation or execution while the Work is proposed, reporting
    /// that human authorization is required.
    pub fn ensure_execution_ready(&self) -> Result<(), WorkModelError> {
        if self.authorization.is_proposed() {
            return Err(WorkModelError::WorkNotExecutionReady {
                work_item_id: self.id.clone(),
            });
        }
        if let Some(context) = &self.planning_context {
            context.validate_approved_revisions()?;
        }
        Ok(())
    }

    /// Fill the shared pause record for every Attempt that is waiting for a
    /// user. This is called immediately before an aggregate is persisted so
    /// direct lifecycle updates and older callers that assign fields directly
    /// still produce the same durable blocker and next-operation description.
    pub(crate) fn synchronize_pause_records(&mut self, project_root: &Path) {
        for attempt in &mut self.attempts {
            if attempt.status != AttemptStatus::NeedsUser {
                attempt.pause_record = None;
                continue;
            }
            let next_action = crate::work_status::derive_attempt_recovery_next_action(
                Some(attempt),
                Some(project_root),
            );
            let no_safe_operation_reason = next_action.is_none().then(|| {
                format!(
                    "No public recovery operation is currently eligible for pause kind {}.",
                    attempt
                        .pause_kind
                        .as_ref()
                        .map(PauseKind::as_str)
                        .unwrap_or("unknown")
                )
            });
            let mut generated =
                PauseRecord::generated(attempt, next_action, no_safe_operation_reason);
            if let Some(existing) = attempt.pause_record.as_ref() {
                if existing.same_content(&generated) {
                    continue;
                }
                generated.created_at = existing.created_at.clone();
            }
            attempt.pause_record = Some(generated);
        }
    }

    /// Approve one exact planning revision through the Work Item lifecycle.
    pub fn approve_planning_revision(
        &mut self,
        stage: &str,
        revision_id: &str,
        markdown: String,
    ) -> Result<(), WorkModelError> {
        self.ensure_planning_revision_can_advance(stage)?;
        self.planning_context
            .get_or_insert_default()
            .approve_revision(stage, revision_id, markdown)?;
        self.validate()
    }

    /// Reject planning revisions that would invalidate an existing execution plan.
    pub fn ensure_planning_revision_can_advance(&self, stage: &str) -> Result<(), WorkModelError> {
        self.ensure_not_abandoned()?;
        if !self.attempts.is_empty() {
            return Err(WorkModelError::InvalidPlanningRevision {
                reason: "cannot revise planning after an Attempt exists".to_string(),
            });
        }
        let stages = ["brief", "behaviors", "approach", "plan"];
        let stage_index = stages
            .iter()
            .position(|candidate| *candidate == stage)
            .ok_or_else(|| WorkModelError::InvalidPlanningRevision {
                reason: format!("unknown planning stage {stage:?}"),
            })?;
        let approved_index = self.planning_context.as_ref().and_then(|context| {
            let recorded = context
                .approved_revisions
                .keys()
                .filter_map(|approved| stages.iter().position(|candidate| *candidate == approved))
                .max();
            let legacy = [
                context.brief.as_ref(),
                context.behaviors.as_ref(),
                context.approach.as_ref(),
                context.plan.as_ref(),
            ]
            .iter()
            .rposition(|artifact| artifact.is_some());
            recorded.into_iter().chain(legacy).max()
        });
        let can_replace_current = approved_index == Some(stage_index);
        let is_next_stage =
            approved_index.map_or(stage_index == 0, |index| stage_index == index + 1);
        if can_replace_current || is_next_stage {
            return Ok(());
        }
        Err(WorkModelError::InvalidPlanningRevision {
            reason: format!("cannot approve {stage:?} before the preceding planning stage"),
        })
    }

    /// Authorize proposed Work to execute, charging its lineage exactly once the
    /// first time derived Work becomes execution-ready. Re-authorizing already
    /// execution-ready Work does not charge the lineage again.
    pub fn authorize_execution(
        &mut self,
        authority: ExecutionAuthority,
    ) -> Result<(), WorkModelError> {
        self.ensure_not_abandoned()?;
        self.mark_execution_ready(authority);
        self.validate()?;
        Ok(())
    }

    /// Record a durable intent to enqueue this Work on the regular Work Queue at
    /// `priority`, keyed by `origin_operation_id`. Set when derived corrective
    /// Work is created so automatic promotion or human authorization can
    /// reconcile exactly one dispatch even across a crash.
    pub fn set_enqueue_intent(&mut self, priority: i64, origin_operation_id: impl Into<String>) {
        self.pending_enqueue = Some(EnqueueIntent {
            priority,
            origin_operation_id: origin_operation_id.into(),
        });
    }

    /// Mark the Work execution-ready and charge its lineage the first time
    /// derived Work reaches that state. Idempotent with respect to the charge.
    fn mark_execution_ready(&mut self, authority: ExecutionAuthority) {
        self.authorization = ExecutionAuthorization::execution_ready(authority);
        if self.origin.is_derived() && !self.lineage.charged {
            self.lineage.charged = true;
        }
    }

    pub fn add_initial_attempt(
        &mut self,
        attempt_id: impl Into<String>,
    ) -> Result<(), WorkModelError> {
        self.add_initial_attempt_with_coder_mapping(attempt_id, CoderMapping::default())
    }

    /// Add one initial write Attempt and snapshot the resolved coder mapping
    /// that must remain authoritative for every later run and resume.
    pub fn add_initial_attempt_with_coder_mapping(
        &mut self,
        attempt_id: impl Into<String>,
        coder_mapping: CoderMapping,
    ) -> Result<(), WorkModelError> {
        self.add_initial_attempt_with_settings(attempt_id, coder_mapping, TerminalMode::Headless)
    }

    pub fn add_initial_attempt_with_settings(
        &mut self,
        attempt_id: impl Into<String>,
        coder_mapping: CoderMapping,
        terminal_mode: TerminalMode,
    ) -> Result<(), WorkModelError> {
        self.ensure_not_abandoned()?;
        self.ensure_execution_ready()?;
        let attempt_id = attempt_id.into();
        validate_id("work item", &self.id)?;
        validate_id("attempt", &attempt_id)?;
        if self.attempts.iter().any(|attempt| attempt.id == attempt_id) {
            return Err(WorkModelError::AttemptAlreadyExists { id: attempt_id });
        }

        let task_id = format!("{attempt_id}-write-1");
        let workspace_path = initial_candidate_workspace_path(&self.id, &attempt_id);
        let artifact_path = work_artifact_path(&self.id, &attempt_id, &task_id);
        // Stamp the immutable progress contract from the Plan's required rows before
        // the Attempt is persisted, so a marked Attempt carries its expected manifest
        // and the advancement gate can fail closed against it. A Plan with no
        // required rows leaves the Attempt unmarked and legacy-compatible.
        let mut progress_contract = self
            .planning_context
            .as_ref()
            .and_then(|context| context.plan.as_deref())
            .and_then(ProgressContract::from_plan);
        let writer_completion_contract = self
            .planning_context
            .as_ref()
            .and_then(WriterCompletionContract::from_planning_context);
        if let Some(contract) = &writer_completion_contract {
            contract
                .validate()
                .map_err(|reason| WorkModelError::InvalidPlanningRevision { reason })?;
            if let Some(progress) = &mut progress_contract {
                progress.required.retain(|entry| {
                    contract.requirements.iter().any(|row| {
                        row.id == format!("plan:{}", entry.id)
                            && writer_responsibility(row.responsibility)
                    })
                });
            }
        }
        let input_artifacts = self.preserved_input_refs();
        self.attempts.push(Attempt {
            id: attempt_id.clone(),
            work_item_id: self.id.clone(),
            kind: AttemptKind::Write,
            status: AttemptStatus::Planned,
            coder_mapping,
            terminal_mode,
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
                writer_correction_assignment: None,
                verification_context: None,
                evidence_review_context: None,
                input_artifacts,
                depends_on: None,
                output: None,
                created_at: Some(now_iso8601()),
                started_at: None,
                completed_at: None,
            }],
            review_state: None,
            pause_kind: None,
            artifacts: Vec::new(),
            created_at: Some(now_iso8601()),
            completed_at: None,
            progress_contract,
            writer_completion_contract,
            ..Default::default()
        });

        self.validate()
    }

    pub fn add_review_only_attempt(
        &mut self,
        attempt_id: impl Into<String>,
        roles: &[&str],
        source_ref: impl Into<String>,
        source_commit: impl Into<String>,
        from_working_tree: bool,
    ) -> Result<Vec<String>, WorkModelError> {
        let attempt_id = attempt_id.into();
        let source_ref = source_ref.into();
        let source_commit = source_commit.into();
        if from_working_tree {
            self.append_review_only_source_checkout_attempt(
                attempt_id,
                roles,
                source_ref,
                source_commit,
            )
        } else {
            self.append_review_only_worktree_attempt(
                attempt_id,
                AttemptKind::ReviewOnly,
                roles,
                source_ref,
                source_commit,
                None,
            )
        }
    }

    pub fn add_post_merge_review_attempt(
        &mut self,
        attempt_id: impl Into<String>,
        roles: &[&str],
        source_ref: impl Into<String>,
        source_commit: impl Into<String>,
        base_commit: Option<String>,
    ) -> Result<Vec<String>, WorkModelError> {
        let attempt_id = attempt_id.into();
        let source_ref = source_ref.into();
        let source_commit = source_commit.into();
        self.append_review_only_worktree_attempt(
            attempt_id,
            AttemptKind::PostMergeReview,
            roles,
            source_ref,
            source_commit,
            base_commit,
        )
    }

    fn append_review_only_source_checkout_attempt(
        &mut self,
        attempt_id: String,
        roles: &[&str],
        source_ref: String,
        source_commit: String,
    ) -> Result<Vec<String>, WorkModelError> {
        self.ensure_not_abandoned()?;
        validate_id("work item", &self.id)?;
        validate_id("attempt", &attempt_id)?;
        if self.attempts.iter().any(|attempt| attempt.id == attempt_id) {
            return Err(WorkModelError::AttemptAlreadyExists { id: attempt_id });
        }

        let source = WorkspaceRef {
            id: "source".to_string(),
            path: ".".to_string(),
        };
        let review_task_instructions = self.write_task_instructions();
        let preserved_inputs = self.preserved_input_refs();
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
                    base_commit: None,
                }),
                writer_correction_assignment: None,
                verification_context: None,
                evidence_review_context: None,
                input_artifacts: preserved_inputs.clone(),
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
            pause_kind: None,
            artifacts: Vec::new(),
            created_at: Some(now_iso8601()),
            completed_at: None,
            ..Default::default()
        });

        self.validate()?;
        Ok(task_ids)
    }

    fn append_review_only_worktree_attempt(
        &mut self,
        attempt_id: String,
        kind: AttemptKind,
        roles: &[&str],
        source_ref: String,
        source_commit: String,
        base_commit: Option<String>,
    ) -> Result<Vec<String>, WorkModelError> {
        debug_assert!(kind.is_review_only_like(), "kind must be review-only-like");
        self.ensure_not_abandoned()?;
        validate_id("work item", &self.id)?;
        validate_id("attempt", &attempt_id)?;
        if self.attempts.iter().any(|attempt| attempt.id == attempt_id) {
            return Err(WorkModelError::AttemptAlreadyExists { id: attempt_id });
        }

        let source = WorkspaceRef {
            id: "source".to_string(),
            path: crate::review_only_worktree::review_only_worktree_path(&source_ref),
        };
        let review_task_instructions = self.write_task_instructions();
        let preserved_inputs = self.preserved_input_refs();
        // Persist corrective input on the Tester Task for inspection. The
        // Tester runner executes tester.yaml and does not consume a prompt.
        let tester_instructions = self.write_task_instructions();
        let mut task_ids = Vec::new();
        let mut tasks = Vec::new();

        let tester_task_id = format!("{attempt_id}-tester");
        validate_id("task", &tester_task_id)?;
        tasks.push(Task {
            id: tester_task_id.clone(),
            kind: TaskKind::Tester,
            status: TaskStatus::Planned,
            role: "tester".to_string(),
            instructions: tester_instructions.clone(),
            work_item_id: self.id.clone(),
            attempt_id: Some(attempt_id.clone()),
            workspace_access: WorkspaceAccess::read_only(vec![source.clone()]),
            artifact_area: Some(TaskArtifactArea {
                path: work_artifact_path(&self.id, &attempt_id, &tester_task_id),
            }),
            review_context: Some(ReviewContext {
                candidate_workspace_id: source.id.clone(),
                candidate_workspace_path: source.path.clone(),
                source_branch: source_ref.clone(),
                candidate_commit: source_commit.clone(),
                base_commit: base_commit.clone(),
            }),
            writer_correction_assignment: None,
            verification_context: None,
            evidence_review_context: None,
            input_artifacts: Vec::new(),
            depends_on: None,
            output: None,
            created_at: Some(now_iso8601()),
            started_at: None,
            completed_at: None,
        });
        task_ids.push(tester_task_id.clone());

        for role in roles {
            validate_id("review role", role)?;
            let task_id = format!("{attempt_id}-review-{role}");
            validate_id("task", &task_id)?;
            let tester_results_artifact = ArtifactRef {
                producer_id: tester_task_id.clone(),
                path: format!(
                    "{}/tester-results.json",
                    work_artifact_path(&self.id, &attempt_id, &tester_task_id)
                ),
            };
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
                    base_commit: base_commit.clone(),
                }),
                writer_correction_assignment: None,
                verification_context: None,
                evidence_review_context: None,
                input_artifacts: {
                    let mut inputs = preserved_inputs.clone();
                    inputs.push(tester_results_artifact);
                    inputs
                },
                depends_on: Some(tester_task_id.clone()),
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
            kind,
            status: AttemptStatus::Reviewing,
            coder_mapping: CoderMapping::default(),
            tasks,
            review_state: Some(AttemptReviewState::NotReviewed),
            pause_kind: None,
            artifacts: Vec::new(),
            created_at: Some(now_iso8601()),
            completed_at: None,
            ..Default::default()
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
        if let Some(attempt) = self
            .attempts
            .iter()
            .find(|attempt| attempt.id == attempt_id)
        {
            attempt.ensure_not_canceled()?;
        }
        let preserved_inputs = self.preserved_input_refs();
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
        let tester_correction_artifacts =
            tester_correction_artifacts_for_tests_review(attempt, &latest_write);

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
            let mut task_input_artifacts = preserved_inputs.clone();
            for artifact in review_input_artifacts
                .get(*role)
                .cloned()
                .unwrap_or_default()
            {
                if !task_input_artifacts.contains(&artifact) {
                    task_input_artifacts.push(artifact);
                }
            }
            if *role == "tests" {
                for artifact in &tester_correction_artifacts {
                    if !task_input_artifacts.contains(artifact) {
                        task_input_artifacts.push(artifact.clone());
                    }
                }
            }
            for artifact in attempt.artifacts.iter().filter(|artifact| {
                artifact.producer_id == latest_write.id
                    && artifact.path.ends_with("/candidate-gate.json")
            }) {
                if !task_input_artifacts.contains(artifact) {
                    task_input_artifacts.push(artifact.clone());
                }
            }
            let writer_artifact_path = latest_write
                .artifact_area
                .as_ref()
                .map(|area| area.path.clone())
                .unwrap_or_else(|| work_artifact_path(&self.id, attempt_id, &latest_write.id));
            if latest_write.role == CANDIDATE_ANCESTRY_REPAIR_ROLE {
                for artifact in latest_write.input_artifacts.iter().filter(|artifact| {
                    artifact.producer_id == "candidate-ancestry-recovery"
                        && artifact.path.ends_with("/recovery.json")
                }) {
                    if !task_input_artifacts.contains(artifact) {
                        task_input_artifacts.push(artifact.clone());
                    }
                }
            } else {
                if latest_write.role == CANDIDATE_METADATA_REPAIR_ROLE {
                    for artifact in latest_write.input_artifacts.iter().filter(|artifact| {
                        artifact.producer_id == "candidate-metadata-recovery"
                            && artifact.path.ends_with("/repair.json")
                    }) {
                        if !task_input_artifacts.contains(artifact) {
                            task_input_artifacts.push(artifact.clone());
                        }
                    }
                }
                task_input_artifacts.push(ArtifactRef {
                    producer_id: latest_write.id.clone(),
                    path: format!("{writer_artifact_path}/transcript.jsonl"),
                });
            }
            if attempt.writer_completion_contract.is_some() {
                let completion = attempt
                    .artifacts
                    .iter()
                    .rev()
                    .find(|artifact| {
                        artifact.path.ends_with("/writer-completion.json")
                            && ((artifact.producer_id == "writer-completion-repair"
                                && artifact.path.contains(&format!(
                                    "/writer-completion-repairs/{}/",
                                    latest_write.id
                                )))
                                || artifact.producer_id == "planning-reference-recovery"
                                || (artifact.producer_id == "ownership-recovery"
                                    && artifact
                                        .path
                                        .contains(&format!("/ownership/{}/", latest_write.id))))
                    })
                    .cloned()
                    .unwrap_or_else(|| ArtifactRef {
                        producer_id: latest_write.id.clone(),
                        path: format!("{writer_artifact_path}/writer-completion.json"),
                    });
                task_input_artifacts.push(completion);
            }
            let progress_md_path =
                format!("{WORK_PROGRESS_DIR}/{}/{}/progress.md", self.id, attempt_id,);
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
                    base_commit: None,
                }),
                writer_correction_assignment: None,
                verification_context: None,
                evidence_review_context: None,
                input_artifacts: task_input_artifacts,
                depends_on: None,
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

    /// Plan the one canonical Tester gate after the latest code-review round passes.
    /// Review-only Attempts retain their Tester-first construction and never call
    /// this method.
    pub fn add_final_tester_task(&mut self, attempt_id: &str) -> Result<String, WorkModelError> {
        self.ensure_not_abandoned()?;
        if let Some(attempt) = self
            .attempts
            .iter()
            .find(|attempt| attempt.id == attempt_id)
        {
            attempt.ensure_not_canceled()?;
        }
        let tester_instructions = self.write_task_instructions();
        let Some(attempt) = self
            .attempts
            .iter_mut()
            .find(|attempt| attempt.id == attempt_id)
        else {
            return Err(WorkModelError::AttemptNotFound {
                id: attempt_id.to_string(),
            });
        };
        if attempt.kind.is_review_only_like() {
            return Err(WorkModelError::AttemptMissingCompletedWriteTask {
                attempt_id: attempt_id.to_string(),
            });
        }
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
        let review_round = attempt
            .tasks
            .iter()
            .filter(|task| task.kind == TaskKind::Review)
            .filter_map(|task| review_task_round(attempt_id, task))
            .max()
            .unwrap_or(1);
        let tester_id = if review_round == 1 {
            format!("{attempt_id}-tester")
        } else {
            format!("{attempt_id}-tester-{review_round}")
        };
        validate_id("task", &tester_id)?;
        if attempt.tasks.iter().any(|task| task.id == tester_id) {
            return Err(WorkModelError::TaskAlreadyExists { id: tester_id });
        }
        let candidate = WorkspaceRef {
            id: write_output.workspace_id.clone(),
            path: write_output.workspace_path.clone(),
        };
        attempt.tasks.push(Task {
            id: tester_id.clone(),
            kind: TaskKind::Tester,
            status: TaskStatus::Planned,
            role: "tester".to_string(),
            instructions: tester_instructions,
            work_item_id: self.id.clone(),
            attempt_id: Some(attempt_id.to_string()),
            workspace_access: WorkspaceAccess::read_only(vec![candidate]),
            artifact_area: Some(TaskArtifactArea {
                path: work_artifact_path(&self.id, attempt_id, &tester_id),
            }),
            review_context: Some(ReviewContext {
                candidate_workspace_id: write_output.workspace_id,
                candidate_workspace_path: write_output.workspace_path,
                source_branch: write_output.source_branch,
                candidate_commit: write_output.commit,
                base_commit: None,
            }),
            writer_correction_assignment: None,
            verification_context: None,
            evidence_review_context: None,
            input_artifacts: Vec::new(),
            depends_on: None,
            output: None,
            created_at: Some(now_iso8601()),
            started_at: None,
            completed_at: None,
        });
        attempt.status = AttemptStatus::Reviewing;
        attempt.review_state = Some(AttemptReviewState::NotReviewed);
        self.validate()?;
        Ok(tester_id)
    }

    /// Plan a serial host-owned Tester that reruns only the failed commands
    /// named by one prior final-Tester result. The executor derives and validates
    /// the exact command indexes from the attached structured result.
    pub fn add_failed_command_recheck_task(
        &mut self,
        attempt_id: &str,
        source_results: ArtifactRef,
    ) -> Result<String, WorkModelError> {
        self.ensure_not_abandoned()?;
        if let Some(attempt) = self
            .attempts
            .iter()
            .find(|attempt| attempt.id == attempt_id)
        {
            attempt.ensure_not_canceled()?;
        }
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
        let sequence = attempt
            .tasks
            .iter()
            .filter(|task| task.is_failed_command_recheck())
            .count()
            + 1;
        let task_id = format!("{attempt_id}-tester-recheck-{sequence}");
        validate_id("task", &task_id)?;
        if attempt.tasks.iter().any(|task| task.id == task_id) {
            return Err(WorkModelError::TaskAlreadyExists { id: task_id });
        }
        let candidate = WorkspaceRef {
            id: write_output.workspace_id.clone(),
            path: write_output.workspace_path.clone(),
        };
        attempt.tasks.push(Task {
            id: task_id.clone(),
            kind: TaskKind::Tester,
            status: TaskStatus::Planned,
            role: FAILED_COMMAND_RECHECK_ROLE.to_string(),
            instructions: None,
            work_item_id: self.id.clone(),
            attempt_id: Some(attempt_id.to_string()),
            workspace_access: WorkspaceAccess::read_only(vec![candidate]),
            artifact_area: Some(TaskArtifactArea {
                path: work_artifact_path(&self.id, attempt_id, &task_id),
            }),
            review_context: Some(ReviewContext {
                candidate_workspace_id: write_output.workspace_id,
                candidate_workspace_path: write_output.workspace_path,
                source_branch: write_output.source_branch,
                candidate_commit: write_output.commit,
                base_commit: None,
            }),
            writer_correction_assignment: None,
            verification_context: None,
            evidence_review_context: None,
            input_artifacts: vec![source_results.clone()],
            depends_on: Some(source_results.producer_id),
            output: None,
            created_at: Some(now_iso8601()),
            started_at: None,
            completed_at: None,
        });
        attempt.status = AttemptStatus::Reviewing;
        attempt.review_state = Some(AttemptReviewState::NotReviewed);
        self.validate()?;
        Ok(task_id)
    }

    pub fn add_next_write_round(
        &mut self,
        attempt_id: &str,
        input_artifacts: Vec<ArtifactRef>,
    ) -> Result<String, WorkModelError> {
        self.ensure_not_abandoned()?;
        if let Some(attempt) = self
            .attempts
            .iter()
            .find(|attempt| attempt.id == attempt_id)
        {
            attempt.ensure_not_canceled()?;
        }
        let write_task_instructions = self.write_task_instructions();
        let input_artifacts = self.with_preserved_inputs(input_artifacts);
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
            writer_correction_assignment: None,
            verification_context: None,
            evidence_review_context: None,
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

    /// Record one host-owned candidate ancestry repair as a completed Writer
    /// frontier. The source Writer and its rejected gate remain unchanged in
    /// history; ordinary gates and reviewers consume this new exact commit.
    pub(crate) fn add_completed_candidate_ancestry_repair(
        &mut self,
        attempt_id: &str,
        protected_base_commit: &str,
        recovered_commit: &str,
        recovery_artifact: ArtifactRef,
    ) -> Result<String, WorkModelError> {
        self.ensure_not_abandoned()?;
        let preserved_recovery = self.with_preserved_inputs(vec![recovery_artifact]);
        let Some(attempt) = self
            .attempts
            .iter_mut()
            .find(|attempt| attempt.id == attempt_id)
        else {
            return Err(WorkModelError::AttemptNotFound {
                id: attempt_id.to_string(),
            });
        };
        let source = attempt
            .tasks
            .iter()
            .rev()
            .find(|task| task.kind == TaskKind::Write && task.status == TaskStatus::Complete)
            .and_then(|task| task.output.clone())
            .ok_or_else(|| WorkModelError::AttemptMissingCompletedWriteTask {
                attempt_id: attempt_id.to_string(),
            })?;
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
        let timestamp = now_iso8601();
        attempt.tasks.push(Task {
            id: task_id.clone(),
            kind: TaskKind::Write,
            status: TaskStatus::Complete,
            role: CANDIDATE_ANCESTRY_REPAIR_ROLE.to_string(),
            instructions: None,
            work_item_id: self.id.clone(),
            attempt_id: Some(attempt_id.to_string()),
            workspace_access: WorkspaceAccess {
                reads: Vec::new(),
                writes: vec![WorkspaceRef {
                    id: source.workspace_id.clone(),
                    path: source.workspace_path.clone(),
                }],
            },
            artifact_area: Some(TaskArtifactArea {
                path: work_artifact_path(&self.id, attempt_id, &task_id),
            }),
            review_context: None,
            writer_correction_assignment: None,
            verification_context: None,
            evidence_review_context: None,
            input_artifacts: preserved_recovery,
            depends_on: None,
            output: Some(TaskOutput {
                workspace_id: source.workspace_id,
                workspace_path: source.workspace_path,
                source_branch: source.source_branch,
                base_commit: Some(protected_base_commit.to_string()),
                commit: recovered_commit.to_string(),
                no_change: None,
                learner_canonicalization: None,
            }),
            created_at: Some(timestamp.clone()),
            started_at: Some(timestamp.clone()),
            completed_at: Some(timestamp),
        });
        attempt.status = AttemptStatus::Planned;
        attempt.review_state = Some(AttemptReviewState::NotReviewed);
        attempt.pause_kind = None;
        attempt.completed_at = None;
        self.validate()?;
        Ok(task_id)
    }

    pub fn create_or_get_merge_candidate(
        &mut self,
        attempt_id: &str,
    ) -> Result<String, WorkModelError> {
        self.ensure_not_abandoned()?;
        if let Some(attempt) = self
            .attempts
            .iter()
            .find(|attempt| attempt.id == attempt_id)
        {
            attempt.ensure_not_canceled()?;
        }
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
            merge_review_state: MergeReviewState::Pending,
            merge_state: MergeCandidateMergeState::default(),
            created_at: Some(now_iso8601()),
            ready_at: None,
            started_at: None,
            completed_at: None,
        });

        self.validate()?;
        Ok(candidate_id)
    }

    /// Stamp the first instant a reviewed, learned candidate becomes eligible
    /// to land and use the same instant as the Attempt's end-to-end completion.
    pub fn mark_merge_candidate_ready(
        &mut self,
        attempt_id: &str,
    ) -> Result<String, WorkModelError> {
        let attempt_index = self
            .attempts
            .iter()
            .position(|attempt| attempt.id == attempt_id)
            .ok_or_else(|| WorkModelError::AttemptNotFound {
                id: attempt_id.to_string(),
            })?;
        let candidate_index = self
            .merge_candidates
            .iter()
            .position(|candidate| candidate.attempt_id == attempt_id)
            .ok_or_else(|| WorkModelError::MergeCandidateNotFound {
                candidate_id: format!("{attempt_id}-merge-candidate"),
            })?;
        let attempt = &mut self.attempts[attempt_index];
        let candidate = &mut self.merge_candidates[candidate_index];
        mark_attempt_merge_candidate_ready(attempt, candidate)?;
        let candidate_id = candidate.id.clone();
        self.validate()?;
        Ok(candidate_id)
    }

    /// Retract a pending candidate that never crossed Learning so the same
    /// Attempt can return to a corrective Writer. Landed, reviewed, failed, or
    /// Learner-approved candidates remain immutable at this boundary.
    pub(crate) fn retract_unready_merge_candidate_for_correction(
        &mut self,
        attempt_id: &str,
    ) -> Result<bool, WorkModelError> {
        let Some(index) = self
            .merge_candidates
            .iter()
            .position(|candidate| candidate.attempt_id == attempt_id)
        else {
            return Ok(false);
        };
        let candidate = &self.merge_candidates[index];
        if candidate.merge_state.status != MergeCandidateMergeStatus::Pending
            || candidate.merge_review_state != MergeReviewState::Pending
        {
            return Err(WorkModelError::MergeCandidateCorrectionIneligible {
                candidate_id: candidate.id.clone(),
                reason: "the candidate is no longer pending and unreviewed".to_string(),
            });
        }
        let attempt = self
            .attempts
            .iter()
            .find(|attempt| attempt.id == attempt_id)
            .ok_or_else(|| WorkModelError::AttemptNotFound {
                id: attempt_id.to_string(),
            })?;
        if attempt
            .learning
            .as_ref()
            .is_some_and(|learning| !learning.is_failed())
        {
            return Err(WorkModelError::MergeCandidateCorrectionIneligible {
                candidate_id: candidate.id.clone(),
                reason: "the candidate has already advanced into accepted Learning".to_string(),
            });
        }
        self.merge_candidates.remove(index);
        self.validate()?;
        Ok(true)
    }

    pub fn abandon(
        &mut self,
        reason: Option<String>,
        project_root: Option<&Path>,
    ) -> Result<(), WorkModelError> {
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
            .find(|task| {
                task.status == TaskStatus::Executing
                    && match project_root {
                        Some(root) => {
                            let lock_path = crate::lease::task_lock_path(root, &self.id, &task.id);
                            crate::lease::is_leased(&lock_path)
                        }
                        None => true,
                    }
            })
        {
            return Err(WorkModelError::WorkItemAbandonmentBlocked {
                work_item_id: self.id.clone(),
                reason: format!("Task {:?} is executing", task.id),
            });
        }
        if let Some(candidate) = self.merge_candidates.iter().find(|candidate| {
            candidate.merge_review_state == MergeReviewState::Reviewing
                || matches!(
                    candidate.merge_state.status,
                    MergeCandidateMergeStatus::Executing
                        | MergeCandidateMergeStatus::LandingPending
                )
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
        if let Some(scope) = &self.planning_scope {
            let expected_slices = scope.scope_units().div_ceil(scope.limit.max(1));
            let recorded_counts_match = self.planning_context.as_ref().is_some_and(|context| {
                context.required_plan_steps() == scope.required_steps
                    && (scope.calibration_version == 0
                        || context.behavior_count() == scope.behavior_count)
            });
            if scope.scope_units() == 0
                || scope.limit == 0
                || scope.recommended_slices != expected_slices
                || (scope.is_large() && !scope.large_scope_authorized)
                || !recorded_counts_match
            {
                return Err(WorkModelError::InvalidPlanningScope {
                    reason: "scope diagnostic is inconsistent or lacks large-scope authorization"
                        .to_string(),
                });
            }
        }
        if let Some(context) = &self.planning_context {
            context.validate_approved_revisions()?;
        }
        if let Some(contract) = &self.release_contract {
            contract.validate()?;
        }
        for attempt in &self.attempts {
            if attempt.work_item_id != self.id {
                return Err(WorkModelError::AttemptWorkItemMismatch {
                    attempt_id: attempt.id.clone(),
                    expected: self.id.clone(),
                    actual: attempt.work_item_id.clone(),
                });
            }
            attempt.validate(&self.id)?;
            let latest_completed_writer = attempt.tasks.iter().rposition(|task| {
                task.kind == TaskKind::Write && task.status == TaskStatus::Complete
            });
            let candidate = self
                .merge_candidates
                .iter()
                .find(|candidate| candidate.attempt_id == attempt.id);
            for (task_index, task) in attempt.tasks.iter().enumerate() {
                let canonicalization = task
                    .output
                    .as_ref()
                    .and_then(|output| output.learner_canonicalization.as_ref());
                let canonicalization_is_accounted_for = canonicalization.is_none_or(|transition| {
                    self.learner_mode.is_capture()
                        && attempt.learning.as_ref().is_some_and(|learning| {
                            matches!(
                                learning.status,
                                LearningStatus::HandoffPending | LearningStatus::Succeeded
                            ) || (learning.status == LearningStatus::Failed
                                && learning.failure_stage
                                    == Some(LearningFailureStage::HandoffPublication))
                        })
                        && latest_completed_writer == Some(task_index)
                        && candidate.is_some_and(|candidate| {
                            candidate.candidate_commit == transition.to_commit
                        })
                });
                if !canonicalization_is_accounted_for {
                    return Err(WorkModelError::InvalidTaskNoChangeOutput {
                        task_id: task.id.clone(),
                    });
                }
            }
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

    /// The shared advancement-readiness gate for one Attempt: delegates to
    /// [`Attempt::learning_advancement_readiness`] so Attempt readiness, Merge
    /// Candidate validation, and landing consume one contract and reject a
    /// non-succeeded Learner with the same durable reason. An active cancellation
    /// blocks advancement even when the Learner previously succeeded.
    pub fn attempt_learning_advancement(&self, attempt_id: &str) -> Result<(), WorkModelError> {
        let attempt = self
            .attempts
            .iter()
            .find(|attempt| attempt.id == attempt_id)
            .ok_or_else(|| WorkModelError::AttemptNotFound {
                id: attempt_id.to_string(),
            })?;
        attempt.ensure_not_canceled()?;
        attempt.learning_advancement_readiness()
    }
}

fn version_triplet(value: &str) -> Option<(u64, u64, u64)> {
    let core = value.split_once('-').map_or(value, |(core, _)| core);
    let mut parts = core.split('.');
    let parsed = (
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
    );
    parts.next().is_none().then_some(parsed)
}

fn version_is_newer(writer: &str, installed: &str) -> bool {
    matches!(
        (version_triplet(writer), version_triplet(installed)),
        (Some(writer), Some(installed)) if writer > installed
    )
}

fn model_upgrade_operation_id(
    work_item_id: &str,
    from_model_writer_version: Option<&str>,
    from_record_schema_version: u32,
    attempts: &[(String, u32)],
) -> String {
    let mut hasher = Sha256::new();
    for value in [
        "work-model-upgrade",
        work_item_id,
        from_model_writer_version.unwrap_or("<legacy>"),
        &from_record_schema_version.to_string(),
    ] {
        hasher.update((value.len() as u64).to_be_bytes());
        hasher.update(value.as_bytes());
    }
    for (attempt_id, version) in attempts {
        hasher.update((attempt_id.len() as u64).to_be_bytes());
        hasher.update(attempt_id.as_bytes());
        hasher.update(version.to_be_bytes());
    }
    format!("upgrade-{:x}", hasher.finalize())
}

fn model_upgrade_audit_path(work_item_id: &str, operation_id: &str) -> String {
    format!("{WORK_ARTIFACTS_DIR}/{work_item_id}/model-upgrades/{operation_id}.json")
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
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub approved_revisions: BTreeMap<String, ApprovedPlanningRevision>,
}

/// Immutable identity for planning bytes approved through the Work Item lifecycle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovedPlanningRevision {
    pub revision_id: String,
    pub sha256: String,
}

impl PlanningContext {
    fn approve_revision(
        &mut self,
        stage: &str,
        revision_id: &str,
        markdown: String,
    ) -> Result<(), WorkModelError> {
        if !["brief", "behaviors", "approach", "plan"].contains(&stage) {
            return Err(WorkModelError::InvalidPlanningRevision {
                reason: format!("unknown planning stage {stage:?}"),
            });
        }
        let sha256 = planning_revision_digest(&markdown);
        if revision_id != format!("{stage}:{sha256}") {
            return Err(WorkModelError::InvalidPlanningRevision {
                reason: "revision identity does not match approved planning bytes".to_string(),
            });
        }
        match stage {
            "brief" => self.brief = Some(markdown),
            "behaviors" => self.behaviors = Some(markdown),
            "approach" => self.approach = Some(markdown),
            "plan" => self.plan = Some(markdown),
            _ => unreachable!("planning stage was validated before assignment"),
        }
        self.approved_revisions.insert(
            stage.to_owned(),
            ApprovedPlanningRevision {
                revision_id: revision_id.to_owned(),
                sha256,
            },
        );
        Ok(())
    }

    fn validate_approved_revisions(&self) -> Result<(), WorkModelError> {
        for (stage, approval) in &self.approved_revisions {
            let markdown = match stage.as_str() {
                "brief" => self.brief.as_deref(),
                "behaviors" => self.behaviors.as_deref(),
                "approach" => self.approach.as_deref(),
                "plan" => self.plan.as_deref(),
                _ => None,
            }
            .ok_or_else(|| WorkModelError::InvalidPlanningRevision {
                reason: format!("approved {stage:?} revision has no planning bytes"),
            })?;
            let sha256 = planning_revision_digest(markdown);
            if sha256 != approval.sha256 || approval.revision_id != format!("{stage}:{sha256}") {
                return Err(WorkModelError::InvalidPlanningRevision {
                    reason: format!("approved {stage:?} revision does not match planning bytes"),
                });
            }
        }
        Ok(())
    }

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

    /// Count approved behaviors and required Plan rows without asking planning
    /// authors for another scope vocabulary. Both parsers already feed the
    /// execution completion contract.
    pub fn scope_assessment(
        &self,
        limit: u32,
        large_scope_authorized: bool,
    ) -> Option<PlanningScopeAssessment> {
        let behavior_count = self.behavior_count();
        let required_steps = self.required_plan_steps();
        let scope_units = behavior_count.max(required_steps);
        if scope_units == 0 {
            return None;
        }
        let recommended_slices = scope_units.div_ceil(limit.max(1));
        Some(PlanningScopeAssessment {
            calibration_version: PLANNING_SCOPE_CALIBRATION_VERSION,
            behavior_count,
            required_steps,
            limit,
            recommended_slices,
            large_scope_authorized,
        })
    }

    pub fn required_plan_steps(&self) -> u32 {
        let source = self.plan.as_deref().or(self.combined.as_deref());
        u32::try_from(
            source
                .map(parse_required_plan_rows)
                .unwrap_or_default()
                .len(),
        )
        .unwrap_or(u32::MAX)
    }

    pub fn behavior_count(&self) -> u32 {
        let source = self.behaviors.as_deref().or(self.combined.as_deref());
        u32::try_from(
            source
                .map(parse_behavior_completion_requirements)
                .unwrap_or_default()
                .len(),
        )
        .unwrap_or(u32::MAX)
    }
}

fn planning_revision_digest(markdown: &str) -> String {
    Sha256::digest(markdown.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Host-derived scope diagnostic recorded at Work creation.
pub const PLANNING_SCOPE_CALIBRATION_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanningScopeAssessment {
    /// Version 0 is the legacy Plan-only assessment. Version 1 records behavior
    /// and Plan breadth for project-local calibration.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub calibration_version: u32,
    #[serde(default)]
    pub behavior_count: u32,
    pub required_steps: u32,
    pub limit: u32,
    pub recommended_slices: u32,
    #[serde(default, skip_serializing_if = "is_false")]
    pub large_scope_authorized: bool,
}

impl PlanningScopeAssessment {
    pub fn scope_units(&self) -> u32 {
        if self.calibration_version == 0 {
            self.required_steps
        } else {
            self.behavior_count.max(self.required_steps)
        }
    }

    pub fn is_large(&self) -> bool {
        self.scope_units() > self.limit
    }
}

/// One immutable release acceptance criterion approved before execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseCriterion {
    pub id: String,
    pub statement: String,
}

/// Whether a release finding blocks an accepted criterion or remains proposed
/// follow-up work.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum ReleaseFindingClassification {
    ProposedFollowUp,
    ReleaseBlocker { criterion_id: String },
}

impl ReleaseFindingClassification {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ProposedFollowUp => "proposed-follow-up",
            Self::ReleaseBlocker { .. } => "release-blocker",
        }
    }
}

/// One finding classified against the release acceptance contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseFinding {
    pub id: String,
    pub summary: String,
    pub classification: ReleaseFindingClassification,
}

/// Fixed acceptance criteria plus append-only classifications discovered during
/// a release exercise.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseContract {
    pub criteria: Vec<ReleaseCriterion>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub findings: Vec<ReleaseFinding>,
}

impl ReleaseContract {
    pub fn new(criteria: Vec<ReleaseCriterion>) -> Result<Self, WorkModelError> {
        let contract = Self {
            criteria,
            findings: Vec::new(),
        };
        contract.validate()?;
        Ok(contract)
    }

    fn validate(&self) -> Result<(), WorkModelError> {
        if self.criteria.is_empty() {
            return Err(WorkModelError::InvalidReleaseContract {
                reason: "release work requires at least one accepted criterion".to_string(),
            });
        }
        let mut criterion_ids = HashSet::new();
        for criterion in &self.criteria {
            if criterion.id.trim().is_empty() || criterion.statement.trim().is_empty() {
                return Err(WorkModelError::InvalidReleaseContract {
                    reason: "release criteria require non-empty ids and statements".to_string(),
                });
            }
            validate_id("release criterion", &criterion.id)?;
            if !criterion_ids.insert(criterion.id.as_str()) {
                return Err(WorkModelError::InvalidReleaseContract {
                    reason: format!("release criterion {:?} is repeated", criterion.id),
                });
            }
        }
        let mut finding_ids = HashSet::new();
        for finding in &self.findings {
            if finding.id.trim().is_empty() || finding.summary.trim().is_empty() {
                return Err(WorkModelError::InvalidReleaseContract {
                    reason: "release findings require non-empty ids and summaries".to_string(),
                });
            }
            validate_id("release finding", &finding.id)?;
            if !finding_ids.insert(finding.id.as_str()) {
                return Err(WorkModelError::InvalidReleaseContract {
                    reason: format!("release finding {:?} is repeated", finding.id),
                });
            }
            if let ReleaseFindingClassification::ReleaseBlocker { criterion_id } =
                &finding.classification
                && !criterion_ids.contains(criterion_id.as_str())
            {
                return Err(WorkModelError::ReleaseCriterionNotFound {
                    criterion_id: criterion_id.clone(),
                });
            }
        }
        Ok(())
    }

    pub fn classify_finding(
        &mut self,
        id: String,
        summary: String,
        blocker_for: Option<String>,
    ) -> Result<&ReleaseFinding, WorkModelError> {
        self.validate()?;
        validate_id("release finding", &id)?;
        if summary.trim().is_empty() {
            return Err(WorkModelError::InvalidReleaseContract {
                reason: "release findings require non-empty summaries".to_string(),
            });
        }
        if self.findings.iter().any(|finding| finding.id == id) {
            return Err(WorkModelError::ReleaseFindingAlreadyExists { id });
        }
        let classification = match blocker_for {
            Some(criterion_id)
                if self
                    .criteria
                    .iter()
                    .any(|criterion| criterion.id == criterion_id) =>
            {
                ReleaseFindingClassification::ReleaseBlocker { criterion_id }
            }
            Some(criterion_id) => {
                return Err(WorkModelError::ReleaseCriterionNotFound { criterion_id });
            }
            None => ReleaseFindingClassification::ProposedFollowUp,
        };
        self.findings.push(ReleaseFinding {
            id,
            summary,
            classification,
        });
        Ok(self.findings.last().expect("finding was just appended"))
    }
}

fn non_empty_clone(value: &Option<String>) -> Option<String> {
    value
        .as_ref()
        .filter(|value| !value.trim().is_empty())
        .cloned()
}

// -------------------------------------------------------------------------
// Follow-up contracts: origin, authorization, lineage, corrective context
// -------------------------------------------------------------------------

/// How a Work Item came to exist.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum WorkOrigin {
    /// Created through the ordinary human-approved planning flow.
    #[default]
    Planned,
    /// Derived from a learner handoff Observation or a post-merge correction.
    Derived {
        #[serde(flatten)]
        provenance: DerivedProvenance,
    },
}

impl WorkOrigin {
    pub fn is_planned(&self) -> bool {
        matches!(self, Self::Planned)
    }

    pub fn is_derived(&self) -> bool {
        matches!(self, Self::Derived { .. })
    }

    pub fn provenance(&self) -> Option<&DerivedProvenance> {
        match self {
            Self::Derived { provenance } => Some(provenance),
            Self::Planned => None,
        }
    }
}

/// Where a derived Work Item traces back to: its originating Observation, Work
/// Item, Attempt, Merge Candidate, and merged commit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct DerivedProvenance {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_item_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merge_candidate_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merged_commit: Option<String>,
}

/// The authority that authorized a Work Item to execute.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ExecutionAuthority {
    Human,
    Automatic,
}

/// Whether a Work Item may execute, independent of queue and landing state.
///
/// Legacy Work with no stored authorization deserializes as execution-ready so
/// the new model does not strand previously created Work Items.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum ExecutionAuthorization {
    /// Visible Work that has not been authorized to execute.
    Proposed,
    /// Work whose execution has been authorized.
    ExecutionReady {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        authority: Option<ExecutionAuthority>,
    },
}

impl Default for ExecutionAuthorization {
    fn default() -> Self {
        Self::ExecutionReady { authority: None }
    }
}

impl ExecutionAuthorization {
    /// An execution-ready authorization attributed to `authority`.
    pub fn execution_ready(authority: ExecutionAuthority) -> Self {
        Self::ExecutionReady {
            authority: Some(authority),
        }
    }

    pub fn is_execution_ready(&self) -> bool {
        matches!(self, Self::ExecutionReady { .. })
    }

    pub fn is_proposed(&self) -> bool {
        matches!(self, Self::Proposed)
    }

    pub fn authority(&self) -> Option<ExecutionAuthority> {
        match self {
            Self::ExecutionReady { authority } => *authority,
            Self::Proposed => None,
        }
    }

    /// Whether this is the unattributed execution-ready default — the state
    /// legacy Work with no stored authorization takes. Such Work does not need
    /// its authorization persisted.
    pub fn is_unattributed_ready(&self) -> bool {
        matches!(self, Self::ExecutionReady { authority: None })
    }
}

/// The lineage a Work Item belongs to and whether it has been charged as an
/// autonomous descendant of that lineage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct WorkLineage {
    /// The root Work Item id of this lineage. Absent means this Work Item is the
    /// lineage root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_id: Option<String>,
    /// Whether this Work Item has been charged against its lineage's autonomous
    /// descendant budget.
    #[serde(default, skip_serializing_if = "is_false")]
    pub charged: bool,
    /// The autonomous descendant limit for this lineage. Absent means the
    /// resolved built-in default applies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub descendant_limit: Option<u32>,
}

fn is_false(value: &bool) -> bool {
    !*value
}

impl WorkLineage {
    /// A lineage rooted at `root_id` with an optional explicit descendant limit.
    pub fn descendant_of(root_id: impl Into<String>, descendant_limit: Option<u32>) -> Self {
        Self {
            root_id: Some(root_id.into()),
            charged: false,
            descendant_limit,
        }
    }

    /// The root Work Item id of this lineage, given the owning Work Item's id.
    pub fn root_id<'a>(&'a self, own_id: &'a str) -> &'a str {
        self.root_id.as_deref().unwrap_or(own_id)
    }

    pub fn is_root(&self) -> bool {
        self.root_id.is_none()
    }

    /// An uncharged root with no explicit limit — the default lineage that does
    /// not need to be persisted.
    pub fn is_uncharged_root(&self) -> bool {
        self.root_id.is_none() && !self.charged && self.descendant_limit.is_none()
    }

    /// Whether a lineage that has already charged `charged_descendants` may
    /// charge one more, given its `limit`.
    pub fn can_authorize_descendant(charged_descendants: u32, limit: u32) -> bool {
        charged_descendants < limit
    }
}

/// Immutable corrective execution input for derived corrective Work created
/// without brief, behaviors, approach, or plan artifacts. It stands in for the
/// planning artifacts stored on Tasks and rendered for Writers and reviewers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CorrectiveContext {
    /// What the corrective Work must accomplish.
    pub objective: String,
    /// The single authoritative requirement the result must satisfy.
    pub requirement: String,
    /// Evidence that motivated the correction.
    pub evidence: String,
    /// What is in scope for this correction.
    pub included_scope: String,
    /// What is explicitly out of scope.
    pub excluded_scope: String,
    /// The deterministic verification that decides whether the result is done.
    pub verification: String,
}

impl CorrectiveContext {
    /// A corrective context is a valid execution input only when every field is
    /// present, so it can wholly replace planning artifacts.
    pub fn validate(&self) -> Result<(), WorkModelError> {
        for (field, value) in [
            ("objective", &self.objective),
            ("requirement", &self.requirement),
            ("evidence", &self.evidence),
            ("included_scope", &self.included_scope),
            ("excluded_scope", &self.excluded_scope),
            ("verification", &self.verification),
        ] {
            if value.trim().is_empty() {
                return Err(WorkModelError::CorrectiveContextIncomplete { field });
            }
        }
        Ok(())
    }

    /// Render the authoritative execution block stored on corrective Tasks and
    /// included in Writer and reviewer prompts.
    pub fn to_execution_context(&self) -> String {
        format!(
            "# Corrective execution context\n\n\
             ## Objective\n{}\n\n\
             ## Authoritative requirement\n{}\n\n\
             ## Evidence\n{}\n\n\
             ## In scope\n{}\n\n\
             ## Out of scope\n{}\n\n\
             ## Deterministic verification\n{}",
            self.objective.trim(),
            self.requirement.trim(),
            self.evidence.trim(),
            self.included_scope.trim(),
            self.excluded_scope.trim(),
            self.verification.trim(),
        )
    }
}

/// The complete accepted proposal retained beside a derived Work Item's
/// corrective execution context. These fields preserve the decision's audit
/// trail after Fluent removes the originating Work records and handoff.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CorrectiveAuditContext {
    pub follow_up_id: String,
    /// Normalized proposal source, such as `learner` or `post-merge`.
    pub source: String,
    /// Accepted follow-up summary, retained independently from the Work title.
    #[serde(default)]
    pub summary: String,
    pub learning_summary: String,
    pub expected_result: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub target_paths: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unresolved_decisions: Vec<String>,
    pub authority: CorrectiveAuthorityReference,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<CorrectiveEvidenceReference>,
}

impl CorrectiveAuditContext {
    /// Render the accepted result, authority, and evidence into the corrective
    /// input stored on Tasks and included in Writer and reviewer prompts.
    pub fn to_execution_context(&self) -> String {
        let evidence = if self.evidence.is_empty() {
            "none".to_string()
        } else {
            self.evidence
                .iter()
                .map(|item| format!("- {} ({})", item.path, item.digest))
                .collect::<Vec<_>>()
                .join("\n")
        };
        let unresolved = if self.unresolved_decisions.is_empty() {
            "none".to_string()
        } else {
            self.unresolved_decisions
                .iter()
                .map(|decision| format!("- {decision}"))
                .collect::<Vec<_>>()
                .join("\n")
        };
        format!(
            "# Corrective proposal audit\n\n\
             ## Expected result\n{}\n\n\
             ## Target paths\n{}\n\n\
             ## Trusted authority\nKind: {}\nPath: {}\nAnchor: {}\nDigest: {}\n\n\
             ## Supporting evidence\n{}\n\n\
             ## Unresolved decisions\n{}\n\n\
             ## Follow-up source\nSource: {}\nFollow-up: {}\nSummary: {}\nLearning summary: {}",
            self.expected_result.trim(),
            self.target_paths
                .iter()
                .map(|path| format!("- {path}"))
                .collect::<Vec<_>>()
                .join("\n"),
            self.authority.kind,
            self.authority.path,
            self.authority.anchor,
            self.authority.digest,
            evidence,
            unresolved,
            self.source,
            self.follow_up_id,
            self.summary.trim(),
            self.learning_summary.trim(),
        )
    }
}

/// The exact committed authority accepted by the corrective host gate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CorrectiveAuthorityReference {
    pub kind: String,
    pub path: String,
    pub anchor: String,
    pub digest: String,
}

/// One digest-bearing supporting artifact from the accepted follow-up.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CorrectiveEvidenceReference {
    pub path: String,
    pub digest: String,
}

/// A durable intent to enqueue a Work Item on the regular Work Queue. Recorded
/// on derived corrective Work when it is created, and consumed by automatic
/// promotion or human authorization to reconcile exactly one dispatch — so an
/// authorization that persists but crashes before the queue write is
/// recoverable on retry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnqueueIntent {
    /// The regular-queue priority for this Work.
    pub priority: i64,
    /// A stable origin id so a reconcile recognizes its own dispatch and never
    /// creates a duplicate.
    pub origin_operation_id: String,
}

/// Coarse outcome of the most recent Learner run for an Attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LearningStatus {
    /// A durable, crash-observable reservation: the Learner run was persisted as
    /// started before its coder ran and before any handoff was written, so a crash
    /// mid-run leaves a retryable record rather than an orphan handoff with no
    /// durable learning state. Treated as pending (retryable) by the loop.
    InProgress,
    /// A durable, non-landable prepared state: the Learner coder ran and its output
    /// was accepted for the canonical handoff, but the handoff has not been exposed
    /// and `Succeeded` has not been persisted yet. Persisting this before writing
    /// the handoff means a crash in that window leaves a durable pending record
    /// rather than an exposed handoff with no learning state, and advancement stays
    /// blocked until the terminal `Succeeded`. Treated as pending (retryable) by the
    /// loop.
    HandoffPending,
    Succeeded,
    Failed,
}

/// Durable, retryable state of the Learner for a code-producing Attempt.
///
/// Recorded on the Attempt so a failed learning run can be retried on its own
/// without rerunning the Writer, Tester, or reviewers. A successful run carries
/// exactly one handoff reference; a failed run carries the diagnostic to warn
/// the operator and leaves the run retryable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttemptLearning {
    /// Whether the most recent Learner run succeeded or failed.
    pub status: LearningStatus,
    /// How many times the Learner has run for this Attempt.
    #[serde(default)]
    pub runs: u32,
    /// Reference to the persisted handoff, relative and digest-bearing. Present
    /// once a run has completed successfully.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handoff: Option<crate::follow_up::ArtifactRef>,
    /// Diagnostic from the last failed run, retained so a retry can complete the
    /// same record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_failure: Option<String>,
    /// The typed classification of the last failure, so a transcript-pump
    /// infrastructure failure stays discoverable rather than flattened to a string.
    /// Absent on records written before this field existed (backward-compatible).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_kind: Option<LearningFailureKind>,
    /// The host-owned stage reached before a failed run settled. Only handoff
    /// publication follows accepted canonicalization, so this durable marker can
    /// account for commit divergence retained after publication fails.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_stage: Option<LearningFailureStage>,
}

/// The host-owned Learning stage reached before a terminal failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LearningFailureStage {
    /// Fluent accepted and persisted the canonical candidate, then failed to
    /// publish its handoff.
    HandoffPublication,
}

/// The typed classification of a failed Learner run, retained on the durable
/// learning record so a transcript-pump primary is discoverable through recovery.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LearningFailureKind {
    /// A transcript-pump infrastructure failure — the console/transcript transport,
    /// not the coder's learning, is the fault. It stopped the coder before durable
    /// side effects, so a supported resume relaunches it safely.
    TranscriptPump,
    /// The Learner coder already ran, but persisting its host-side run evidence
    /// (the supervision sidecar) failed. Its expertise effects may already be on
    /// disk, so recovery must repair the evidence rather than relaunch the coder.
    EvidencePending,
    /// Any other persistent Learner failure.
    Generic,
}

impl LearningFailureKind {
    /// The generic per-phase recovery disposition this Learner failure consumes.
    pub fn phase_recovery(&self) -> CoderPhaseRecovery {
        match self {
            // The coder already ran; only host evidence persistence failed.
            Self::EvidencePending => CoderPhaseRecovery::CoderAlreadyRanEvidencePending,
            // A transcript-pump transport fault stops the coder before durable side
            // effects, and a generic coder-logic failure is safe to rerun.
            Self::TranscriptPump | Self::Generic => CoderPhaseRecovery::Relaunchable,
        }
    }
}

/// How recovery must treat a coder-backed phase after a post-launch failure.
///
/// Every coder-backed phase (Writer, Reviewer, Tester, Learner) reserves durable
/// state, launches its coder, then persists host-side evidence of the run. This
/// disposition separates a failure that stopped the coder *before* any durable
/// side effect — safe to relaunch — from one where the coder already ran and only
/// host evidence persistence failed. In the latter case the coder's side effects
/// may already be on disk, so recovery must repair the evidence out of band and
/// must never relaunch the coder, which would duplicate those effects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoderPhaseRecovery {
    /// The failure stopped the coder before any durable side effect. Recovery may
    /// safely relaunch the coder.
    Relaunchable,
    /// The coder already ran but its host evidence could not be durably persisted.
    /// Recovery must repair the evidence and must NOT relaunch the coder.
    CoderAlreadyRanEvidencePending,
}

impl CoderPhaseRecovery {
    /// Whether recovery may relaunch the phase's coder.
    pub fn is_relaunchable(&self) -> bool {
        matches!(self, Self::Relaunchable)
    }
}

/// The current progress-contract schema version. A new planned Attempt carries this
/// marker so a marked ("progress-contract") Attempt is distinguishable from a legacy
/// Attempt that deserializes without it, letting the advancement gate fail closed
/// only for marked Attempts.
pub const PROGRESS_CONTRACT_VERSION: u32 = 1;

/// The heading of the only authoritative section of an Attempt's progress file:
/// `## Required completion`. Every other section, historical Attempt file, banner,
/// and review prose is non-authoritative for advancement.
pub const REQUIRED_COMPLETION_HEADING: &str = "## Required completion";

/// One immutable expected required-progress entry: a stable id and the requirement
/// text derived from a required Plan row. The manifest is the authority the
/// advancement gate reconciles the current required-progress checklist against.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequiredProgressEntry {
    /// Stable top-level checklist id, e.g. `step-1`.
    pub id: String,
    /// The requirement text, derived from the required Plan row.
    pub requirement: String,
}

/// The durable progress contract stamped on a new planned Attempt with required
/// Plan rows: the schema version plus the immutable expected stable-id/requirement
/// manifest. Matrix-backed Attempts project it through the host; older Attempts
/// reconcile it against their Writer-owned checklist.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProgressContract {
    /// The progress-contract schema version this Attempt was marked with.
    pub version: u32,
    /// The immutable expected required-progress manifest, one entry per required
    /// Plan row, in Plan order.
    pub required: Vec<RequiredProgressEntry>,
}

/// Schema version for the immutable planning manifest that backs every
/// host-generated Writer completion matrix.
pub const WRITER_COMPLETION_CONTRACT_VERSION: u32 = 2;

/// Maximum serialized size accepted for one Writer completion matrix. The
/// matrix is prompt context and durable evidence, so fail before launching a
/// Writer instead of silently truncating an approved requirement.
pub const WRITER_COMPLETION_MATRIX_MAX_BYTES: usize = 128 * 1024;

/// Source class for one immutable completion-matrix requirement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WriterCompletionRequirementKind {
    Behavior,
    Approach,
    Plan,
    Finding,
}

/// Declare only the facts that determine whether one command result applies.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckApplicability {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub environment: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub comparison_refs: Vec<String>,
    #[serde(default)]
    pub freshness: CheckFreshness,
}

impl CheckApplicability {
    pub(crate) fn validate(&self) -> Result<(), String> {
        if self.environment.len() > 32 || self.comparison_refs.len() > 16 {
            return Err("check applicability exceeds the supported fact limit".into());
        }
        if self.environment.iter().any(|(key, value)| {
            key.trim().is_empty() || key.len() > 64 || value.trim().is_empty() || value.len() > 256
        }) {
            return Err(
                "check environment facts must have bounded nonempty names and values".into(),
            );
        }
        if let CheckFreshness::LiveState { max_age_seconds } = self.freshness {
            if !max_age_seconds.is_some_and(|seconds| seconds > 0 && seconds <= i64::MAX as u64) {
                return Err(
                    "live-state check requires a supported explicit freshness expiry".into(),
                );
            }
        }
        let mut names = std::collections::BTreeSet::new();
        if self.comparison_refs.iter().any(|name| {
            name.is_empty() || name.len() > 256 || name.starts_with('-') || !names.insert(name)
        }) {
            return Err("check comparison refs must be bounded, unique Git references".into());
        }
        Ok(())
    }

    pub fn is_default(&self) -> bool {
        self == &Self::default()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum CheckFreshness {
    #[default]
    CandidateCode,
    LiveState {
        max_age_seconds: Option<u64>,
    },
}

/// Resolve mutable comparison names when an authorized execution is captured.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvedCheckApplicability {
    pub definition: CheckApplicability,
    pub comparison_commits: BTreeMap<String, String>,
}

impl ResolvedCheckApplicability {
    pub fn is_default(&self) -> bool {
        self == &Self::default()
    }
}

/// One immutable requirement in the planning portion of a Writer completion
/// matrix. `verification_refs` preserves the approved `Test:` marker or Plan
/// verification cell; it does not create a new test-selector vocabulary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct PlanCommand {
    #[serde(default, skip_serializing_if = "CheckApplicability::is_default")]
    pub applicability: CheckApplicability,
    /// The exact shell invocation approved for this Plan row.
    pub command: String,
    /// The directory in which the command must run. `.` means the candidate
    /// worktree root.
    #[serde(default = "default_plan_working_directory")]
    pub working_directory: String,
    /// Environment values recorded separately from the command text. The exact
    /// command remains authoritative for compatibility with existing evidence.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub environment: BTreeMap<String, String>,
    /// Stable completion-matrix identity that supplied this command.
    pub source_row: String,
}

fn default_plan_working_directory() -> String {
    ".".to_string()
}

impl PlanCommand {
    fn from_legacy_texts<'a>(
        source_row: &str,
        texts: impl IntoIterator<Item = &'a str>,
    ) -> Option<Self> {
        let command = crate::plan_command::resolve_legacy_plan_command(texts)?;
        let environment = crate::plan_command::extract_command_environment(&command);
        Some(Self {
            applicability: CheckApplicability::default(),
            command,
            working_directory: default_plan_working_directory(),
            environment,
            source_row: source_row.to_string(),
        })
    }
}

/// The existing actor or workflow stage responsible for an approved obligation.
/// An absent assignment retains legacy Writer completion; commands grant no authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CompletionResponsibility {
    Writer,
    Host,
    Review,
    Tester,
    Learner,
    /// Preserve unrecognized input only long enough to reject contract admission.
    #[serde(other)]
    Unknown,
}

impl CompletionResponsibility {
    fn from_plan(value: &str) -> Self {
        match value {
            "writer" => Self::Writer,
            "host" => Self::Host,
            "review" => Self::Review,
            "tester" => Self::Tester,
            "learner" => Self::Learner,
            _ => Self::Unknown,
        }
    }
}

fn writer_responsibility(value: Option<CompletionResponsibility>) -> bool {
    value.is_none_or(|value| value == CompletionResponsibility::Writer)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WriterCompletionRequirement {
    /// Immutable approved responsibility; absent on legacy requirements.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub responsibility: Option<CompletionResponsibility>,
    pub id: String,
    pub kind: WriterCompletionRequirementKind,
    pub source: String,
    pub requirement: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub verification_refs: Vec<String>,
    /// Structured command captured from a Plan row when the Attempt is created.
    /// Older Attempts omit this field and use the host compatibility adapter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<PlanCommand>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) struct TestReferenceChange {
    pub requirement_id: String,
    pub old_references: Vec<String>,
    pub new_references: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum ApproachRevisionKind {
    /// The original recovery case: an uncertain Architecture review or the
    /// narrowly preserved historical provider-launch recovery.
    UncertainArchitecture,
    /// An operator-approved revision at a failed-review round-cap pause.
    FailedReviewRoundCap,
}

impl Default for ApproachRevisionKind {
    fn default() -> Self {
        Self::UncertainArchitecture
    }
}

impl ApproachRevisionKind {
    pub(crate) fn is_uncertain_architecture(&self) -> bool {
        matches!(self, Self::UncertainArchitecture)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) struct ApproachRevisionAudit {
    pub schema_version: u32,
    /// Old audits omit this field and deserialize as the original recovery
    /// case. New round-cap audits use schema version 2 and identify their
    /// admission contract explicitly.
    #[serde(
        default,
        skip_serializing_if = "ApproachRevisionKind::is_uncertain_architecture"
    )]
    pub revision_kind: ApproachRevisionKind,
    pub revision_id: String,
    pub work_item_id: String,
    pub attempt_id: String,
    pub candidate_commit: String,
    pub source_review_task_id: String,
    pub superseded_task_id: Option<String>,
    pub prior_approach_sha256: String,
    pub replacement_approach_sha256: String,
    pub prior_approach: String,
    pub replacement_approach: String,
    pub resolution: String,
    pub resolution_sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prior_contract: Option<WriterCompletionContract>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_completion: Option<PlanningCompletionSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub legacy_reconciliation: Option<LegacyApproachReconciliationReceipt>,
    /// The exact bounded extension that authorized a round-cap revision. The
    /// model record remains authoritative; this copy makes the immutable audit
    /// independently inspectable and binds every authorization field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub round_cap_extension: Option<RoundCapExtension>,
    /// All current review identities and their exact content digests at the
    /// source candidate, including passing roles that explain why the revised
    /// Approach was selected.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub supporting_reviews: Vec<RoundCapReviewArtifact>,
}

/// Evidence that permits one pre-typed provider failure to enter the ordinary
/// Approach-revision transaction without rewriting its historical Task state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) struct LegacyApproachReconciliationReceipt {
    pub failed_review_task_id: String,
    pub candidate_commit: String,
    pub handoff_sha256: String,
    pub stderr_sha256: String,
    pub execution_sha256: String,
    pub diagnostic_excerpt: String,
}

/// Bind a planning revision to its authoritative Writer evidence and contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PlanningCompletionSource {
    pub artifact: ArtifactRef,
    pub sha256: String,
    pub contract: WriterCompletionContract,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) struct TestReferenceRevisionAudit {
    pub schema_version: u32,
    pub revision_id: String,
    pub work_item_id: String,
    pub attempt_id: String,
    pub candidate_commit: String,
    pub resolution: String,
    pub resolution_sha256: String,
    pub changes: Vec<TestReferenceChange>,
    pub prior_behaviors_sha256: String,
    pub revised_behaviors_sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_completion: Option<PlanningCompletionSource>,
    pub completion_matrix_artifact: String,
    // Older reference audits predate ownership adoption and omit this binding.
    // Revisions after adoption must bind their exact completion evidence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_matrix_sha256: Option<String>,
    pub created_at: String,
}

/// Immutable planning requirements stamped on a new code-producing Attempt.
/// Legacy Attempts deserialize without this contract and retain their prior
/// admission behavior.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WriterCompletionContract {
    pub version: u32,
    pub requirements: Vec<WriterCompletionRequirement>,
}

impl WriterCompletionContract {
    pub fn validate(&self) -> Result<(), String> {
        if !(1..=WRITER_COMPLETION_CONTRACT_VERSION).contains(&self.version) {
            return Err(format!(
                "unsupported completion contract version {}",
                self.version
            ));
        }
        for row in &self.requirements {
            if let Some(command) = &row.command {
                command
                    .applicability
                    .validate()
                    .map_err(|error| format!("{}: {error}", row.id))?;
            }
            if row.responsibility == Some(CompletionResponsibility::Unknown)
                || (row.kind != WriterCompletionRequirementKind::Plan
                    && !writer_responsibility(row.responsibility))
            {
                return Err(format!(
                    "invalid responsibility for {}: use writer, host, review, tester, or learner; split mixed obligations",
                    row.id
                ));
            }
            if row.responsibility == Some(CompletionResponsibility::Host) && row.command.is_none() {
                return Err(format!(
                    "host obligation {} requires one exact verification command",
                    row.id
                ));
            }
        }
        Ok(())
    }

    pub fn from_planning_context(context: &PlanningContext) -> Option<Self> {
        let mut requirements = Vec::new();
        if let Some(combined) = context
            .combined
            .as_deref()
            .filter(|combined| !combined.trim().is_empty())
        {
            requirements.extend(parse_behavior_completion_requirements(combined));
            requirements.extend(parse_approach_completion_requirements(combined));
            requirements.extend(parse_plan_completion_requirements(combined));
        } else {
            if let Some(behaviors) = context.behaviors.as_deref() {
                requirements.extend(parse_behavior_completion_requirements(behaviors));
            }
            if let Some(approach) = context.approach.as_deref() {
                requirements.extend(parse_approach_completion_requirements(approach));
            }
            if let Some(plan) = context.plan.as_deref() {
                requirements.extend(parse_plan_completion_requirements(plan));
            }
        }
        if requirements.is_empty() {
            None
        } else {
            let mut identities = std::collections::HashMap::new();
            for requirement in &mut requirements {
                let count = identities.entry(requirement.id.clone()).or_insert(0usize);
                *count += 1;
                if *count > 1 {
                    requirement.id = format!("{}:{}", requirement.id, count);
                }
            }
            Some(Self {
                version: WRITER_COMPLETION_CONTRACT_VERSION,
                requirements,
            })
        }
    }

    /// Apply an authorized Approach replacement without reconstructing Plan rows.
    pub fn with_revised_approach(&self, prior: &str, replacement: &str) -> Result<Self, String> {
        let prior_rows = Self::from_planning_context(&PlanningContext {
            approach: Some(prior.into()),
            ..Default::default()
        })
        .map(|contract| contract.requirements)
        .unwrap_or_default();
        let replacement_rows = Self::from_planning_context(&PlanningContext {
            approach: Some(replacement.into()),
            ..Default::default()
        })
        .map(|contract| contract.requirements)
        .unwrap_or_default();
        let current = self
            .requirements
            .iter()
            .filter(|row| row.kind == WriterCompletionRequirementKind::Approach)
            .cloned()
            .collect::<Vec<_>>();
        if current != prior_rows {
            return Err("approved Approach source requirements changed".into());
        }
        let insertion = self
            .requirements
            .iter()
            .position(|row| {
                matches!(
                    row.kind,
                    WriterCompletionRequirementKind::Approach
                        | WriterCompletionRequirementKind::Plan
                )
            })
            .unwrap_or(self.requirements.len());
        let mut revised = self.clone();
        revised
            .requirements
            .retain(|row| row.kind != WriterCompletionRequirementKind::Approach);
        revised
            .requirements
            .splice(insertion..insertion, replacement_rows);
        revised.validate()?;
        Ok(revised)
    }

    /// Return every immutable planning row a corrective finding may cite.
    pub fn applicable_constraint_ids(&self) -> impl Iterator<Item = &str> {
        self.requirements.iter().filter_map(|entry| {
            matches!(
                entry.kind,
                WriterCompletionRequirementKind::Behavior
                    | WriterCompletionRequirementKind::Approach
                    | WriterCompletionRequirementKind::Plan
            )
            .then_some(entry.id.as_str())
        })
    }
}

/// An operator-proposed partition of existing approved Plan text. Parts retain
/// the original row as provenance and cannot remove or optionalize any text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnershipProposal {
    pub splits: Vec<OwnershipSplit>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnershipSplit {
    pub row_id: String,
    pub parts: Vec<OwnershipPart>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnershipPart {
    pub requirement: String,
    pub responsibility: CompletionResponsibility,
    /// Reuse an already completed identical source obligation, never a new claim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_from: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnershipPreview {
    pub schema_version: u32,
    pub preview_id: String,
    pub work_item_id: String,
    pub attempt_id: String,
    pub candidate_commit: String,
    pub writer_task_id: String,
    pub original_contract: WriterCompletionContract,
    pub proposal: OwnershipProposal,
    pub source_matrix: ArtifactRef,
    pub source_matrix_digest: String,
    pub planning_digest: String,
    pub tasks_digest: String,
    pub input_digests: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnershipApproval {
    pub preview: OwnershipPreview,
    pub resolution: String,
    pub approved_at: String,
    pub matrix_artifact: ArtifactRef,
}

impl OwnershipProposal {
    /// Preserve every source byte and verification reference while dividing who
    /// completes each part. A saved passing claim may support an identical part.
    pub fn apply(
        &self,
        original: &WriterCompletionContract,
        source: &WriterCompletionMatrix,
    ) -> Result<(WriterCompletionContract, WriterCompletionMatrix), String> {
        if self.splits.is_empty() {
            return Err("ownership proposal has no splits".into());
        }
        let mut seen = HashSet::new();
        for split in &self.splits {
            if !seen.insert(&split.row_id) {
                return Err("duplicate ownership split".into());
            }
            let row = original
                .requirements
                .iter()
                .find(|row| row.id == split.row_id)
                .ok_or("ownership split names an unknown row")?;
            if row.kind != WriterCompletionRequirementKind::Plan || row.responsibility.is_some() {
                return Err("ownership adoption requires an unassigned legacy Plan row".into());
            }
            if split.parts.is_empty()
                || split
                    .parts
                    .iter()
                    .any(|part| part.requirement.trim().is_empty())
                || split
                    .parts
                    .iter()
                    .map(|part| part.requirement.as_str())
                    .collect::<String>()
                    != row.requirement
            {
                return Err("ownership parts must preserve the entire original requirement in order, byte for byte".into());
            }
        }
        let mut contract = original.clone();
        contract.requirements.clear();
        let mut matrix = source.clone();
        matrix.rows.clear();
        for row in &original.requirements {
            let source_row = source
                .rows
                .iter()
                .find(|claim| claim.id == row.id)
                .ok_or("source matrix omits a requirement")?;
            let Some(split) = self.splits.iter().find(|split| split.row_id == row.id) else {
                contract.requirements.push(row.clone());
                matrix.rows.push(source_row.clone());
                continue;
            };
            for (index, part) in split.parts.iter().enumerate() {
                let mut requirement = row.clone();
                requirement.id = format!("{}:part-{}", row.id, index + 1);
                requirement.requirement = part.requirement.clone();
                requirement.responsibility = Some(part.responsibility);
                if let Some(command) = &mut requirement.command {
                    command.source_row = requirement.id.clone();
                } else if part.responsibility == CompletionResponsibility::Host {
                    // Inspect the entire approved row so splitting cannot hide ambiguity.
                    requirement.command = PlanCommand::from_legacy_texts(
                        &requirement.id,
                        std::iter::once(row.requirement.as_str())
                            .chain(row.verification_refs.iter().map(String::as_str)),
                    );
                }
                let mut claim = WriterCompletionRow::from(&requirement);
                if let Some(from) = &part.completion_from {
                    let evidence = source
                        .rows
                        .iter()
                        .find(|claim| claim.id == *from)
                        .ok_or("source completion row is missing")?;
                    if part.responsibility != CompletionResponsibility::Writer
                        || !writer_responsibility(evidence.responsibility)
                        || evidence.state != WriterCompletionState::Complete
                        || evidence.requirement.trim() != part.requirement.trim()
                        || evidence
                            .implementation_evidence
                            .iter()
                            .all(|claim| claim.trim().is_empty())
                        || evidence.verification.is_empty()
                        || evidence
                            .verification
                            .iter()
                            .any(|claim| claim.command.trim().is_empty())
                    {
                        return Err(
                            "a part can reuse only an identical completed Writer obligation".into(),
                        );
                    }
                    claim.state = evidence.state;
                    claim.implementation_evidence = evidence.implementation_evidence.clone();
                    claim.verification = evidence.verification.clone();
                }
                contract.requirements.push(requirement);
                matrix.rows.push(claim);
            }
        }
        // Corrective findings retain their exact identity and source evidence.
        matrix.rows.extend(
            source
                .rows
                .iter()
                .filter(|row| row.kind == WriterCompletionRequirementKind::Finding)
                .cloned(),
        );
        contract.validate()?;
        Ok((contract, matrix))
    }
}

fn completion_slug(value: &str) -> String {
    let mut slug = String::new();
    let mut pending_dash = false;
    for ch in value.chars().flat_map(char::to_lowercase) {
        if ch.is_ascii_alphanumeric() {
            if pending_dash && !slug.is_empty() {
                slug.push('-');
            }
            slug.push(ch);
            pending_dash = false;
        } else {
            pending_dash = true;
        }
    }
    slug.trim_matches('-').to_string()
}

fn is_markdown_heading(value: &str) -> bool {
    let marker_len = value.bytes().take_while(|byte| *byte == b'#').count();
    (1..=6).contains(&marker_len)
        && value
            .as_bytes()
            .get(marker_len)
            .is_none_or(u8::is_ascii_whitespace)
}

fn parse_behavior_completion_requirements(source: &str) -> Vec<WriterCompletionRequirement> {
    let lines = source.lines().collect::<Vec<_>>();
    let mut requirements = Vec::new();
    let mut area = "unscoped".to_string();
    let mut index = 0;
    while index < lines.len() {
        let trimmed = lines[index].trim();
        if let Some(heading) = trimmed.strip_prefix("## ") {
            area = completion_slug(heading);
            index += 1;
            continue;
        }
        let Some(behavior_id) = trimmed.strip_prefix("### ").filter(|id| {
            id.strip_prefix('B').is_some_and(|number| {
                !number.is_empty() && number.chars().all(|c| c.is_ascii_digit())
            })
        }) else {
            index += 1;
            continue;
        };
        let behavior_id = behavior_id.to_ascii_lowercase();
        let source_ref = format!("behaviors.md#{}:{}", area, behavior_id);
        index += 1;
        let mut statement = Vec::new();
        let mut verification_refs = Vec::new();
        while index < lines.len() && !lines[index].trim_start().starts_with("##") {
            let line = lines[index].trim();
            if let Some(reference) = line.strip_prefix("Test:") {
                let reference = reference.trim();
                if !reference.is_empty() {
                    verification_refs.push(reference.to_string());
                }
            } else if let Some(reason) = line.strip_prefix("Untestable:") {
                let reason = reason.trim();
                verification_refs.push(format!("Untestable: {reason}"));
            } else if !line.is_empty() {
                statement.push(line.to_string());
            }
            index += 1;
        }
        requirements.push(WriterCompletionRequirement {
            responsibility: None,
            id: format!("behavior:{area}:{behavior_id}"),
            kind: WriterCompletionRequirementKind::Behavior,
            source: source_ref,
            requirement: statement.join(" "),
            verification_refs,
            command: None,
        });
    }
    requirements
}

fn parse_approach_completion_requirements(source: &str) -> Vec<WriterCompletionRequirement> {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Section {
        Other,
        Decisions,
        Guidance,
    }

    let lines = source.lines().collect::<Vec<_>>();
    let mut section = Section::Other;
    let mut decision = None::<String>;
    let mut requirements = Vec::new();
    let mut guidance_index = 0usize;
    let mut index = 0usize;
    while index < lines.len() {
        let line = lines[index].trim();
        if let Some(heading) = line.strip_prefix("## ") {
            if heading == "Structure" {
                let mut structure = Vec::new();
                index += 1;
                while index < lines.len() && !lines[index].trim_start().starts_with("## ") {
                    let line = lines[index].trim();
                    if !line.is_empty() {
                        structure.push(line.to_string());
                    }
                    index += 1;
                }
                if !structure.is_empty() {
                    requirements.push(WriterCompletionRequirement {
                        responsibility: None,
                        id: "approach:structure".to_string(),
                        kind: WriterCompletionRequirementKind::Approach,
                        source: "approach.md#structure".to_string(),
                        requirement: structure.join(" "),
                        verification_refs: Vec::new(),
                        command: None,
                    });
                }
                section = Section::Other;
                decision = None;
                continue;
            }
            section = match heading {
                "Key decisions" => Section::Decisions,
                "Execution guidance" => Section::Guidance,
                _ => Section::Other,
            };
            decision = None;
            index += 1;
            continue;
        }
        if section == Section::Guidance && is_markdown_heading(line) {
            section = Section::Other;
            index += 1;
            continue;
        }
        if section == Section::Decisions {
            if let Some(heading) = line.strip_prefix("### ") {
                decision = Some(heading.trim().to_string());
            } else if let Some(choice) = line.strip_prefix("Choice:") {
                if let Some(heading) = decision.as_deref() {
                    let mut choice_lines = vec![choice.trim().to_string()];
                    let mut cursor = index + 1;
                    while cursor < lines.len() {
                        let continuation = lines[cursor].trim();
                        if continuation.starts_with("Why:")
                            || continuation.starts_with("Alternatives:")
                            || continuation.starts_with("Trade-offs:")
                            || continuation.starts_with('#')
                        {
                            break;
                        }
                        if !continuation.is_empty() {
                            choice_lines.push(continuation.to_string());
                        }
                        cursor += 1;
                    }
                    requirements.push(WriterCompletionRequirement {
                        responsibility: None,
                        id: format!("approach:decision:{}", completion_slug(heading)),
                        kind: WriterCompletionRequirementKind::Approach,
                        source: format!("approach.md#{}", completion_slug(heading)),
                        requirement: choice_lines.join(" "),
                        verification_refs: Vec::new(),
                        command: None,
                    });
                }
            }
        } else if section == Section::Guidance {
            if let Some(guidance) = line.strip_prefix("- ") {
                guidance_index += 1;
                let mut guidance_lines = vec![guidance.trim().to_string()];
                let mut cursor = index + 1;
                while cursor < lines.len() {
                    let continuation = lines[cursor].trim();
                    if is_markdown_heading(continuation) || continuation.starts_with("- ") {
                        break;
                    }
                    if !continuation.is_empty() {
                        guidance_lines.push(continuation.to_string());
                    }
                    cursor += 1;
                }
                requirements.push(WriterCompletionRequirement {
                    responsibility: None,
                    id: format!("approach:guidance:{guidance_index}"),
                    kind: WriterCompletionRequirementKind::Approach,
                    source: format!("approach.md#execution-guidance-{guidance_index}"),
                    requirement: guidance_lines.join(" "),
                    verification_refs: Vec::new(),
                    command: None,
                });
                index = cursor;
                continue;
            }
        }
        index += 1;
    }
    requirements
}

fn parse_plan_completion_requirements(source: &str) -> Vec<WriterCompletionRequirement> {
    let mut requirements = Vec::new();
    let mut responsibility_column = None;
    let mut applicability_column = None;
    for line in source.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with('|') {
            continue;
        }
        let cells = trimmed
            .trim_matches('|')
            .split('|')
            .map(str::trim)
            .collect::<Vec<_>>();
        if cells.first() == Some(&"#") {
            responsibility_column = cells.iter().position(|cell| *cell == "Responsibility");
            applicability_column = cells.iter().position(|cell| *cell == "Applicability");
        }
        if cells.len() < 3
            || cells[0].is_empty()
            || !cells[0].chars().all(|c| c.is_ascii_digit())
            || !cells
                .last()
                .is_some_and(|req| req.eq_ignore_ascii_case("yes"))
        {
            continue;
        }
        let verification_refs = cells
            .get(3)
            .filter(|verification| !verification.is_empty())
            .map(|verification| vec![(*verification).to_string()])
            .unwrap_or_default();
        let mut command_texts = vec![cells[1]];
        if let Some(verification) = cells.get(3).filter(|verification| !verification.is_empty()) {
            command_texts.push(*verification);
        }
        let row_id = format!("plan:step-{}", cells[0]);
        let mut command = PlanCommand::from_legacy_texts(&row_id, command_texts);
        if let Some(policy) = applicability_column
            .and_then(|index| cells.get(index))
            .filter(|text| !text.is_empty())
        {
            // A malformed declaration cannot fall back to an unrestricted command.
            command = command.and_then(|mut command| {
                command.applicability = serde_json::from_str(policy.trim_matches('`')).ok()?;
                Some(command)
            });
        }
        requirements.push(WriterCompletionRequirement {
            responsibility: responsibility_column.map(|column| {
                CompletionResponsibility::from_plan(cells.get(column).copied().unwrap_or(""))
            }),
            id: row_id.clone(),
            kind: WriterCompletionRequirementKind::Plan,
            source: format!("plan.md#step-{}", cells[0]),
            requirement: cells[1].to_string(),
            verification_refs,
            command,
        });
    }
    requirements
}

/// One open review finding added to a corrective Writer's task-specific matrix.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WriterCompletionFinding {
    pub id: String,
    pub role: String,
    pub source: String,
    pub requirement: String,
}

/// Provenance for one immutable input used to justify a corrective Writer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WriterCorrectionAssignmentInput {
    pub artifact: ArtifactRef,
    pub digest: String,
}

/// Whether the host captured this assignment during normal execution or while
/// explicitly reconstructing a legacy saved Writer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum WriterCorrectionAssignmentOrigin {
    #[default]
    Captured,
    LegacyReconstructed,
}

/// Host-owned findings and provenance that a corrective Writer must preserve.
/// The field lives on the Task record, outside all Writer-writable artifacts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WriterCorrectionAssignment {
    pub schema_version: u32,
    pub work_item_id: String,
    pub attempt_id: String,
    pub writer_task_id: String,
    pub source_task_id: String,
    pub source_candidate_commit: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_task_inputs: Vec<WriterCorrectionAssignmentInput>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<WriterCorrectionAssignmentInput>,
    pub findings: Vec<WriterCompletionFinding>,
    #[serde(default, skip_serializing_if = "is_captured_assignment")]
    pub origin: WriterCorrectionAssignmentOrigin,
}

/// Preserve the exact assignment before and after an explicit context recovery.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AssignmentContextRecoveryAudit {
    pub schema_version: u32,
    pub request_id: String,
    pub attempt_digest: String,
    pub before: WriterCorrectionAssignment,
    pub after: WriterCorrectionAssignment,
    pub original: WriterCorrectionAssignmentInput,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_free: Option<OutputFreeAssignmentContext>,
}

/// Retain the native failure and exact reservation alongside context proof.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OutputFreeAssignmentContext {
    pub task: Task,
    pub later_tasks: Vec<Task>,
    pub files: BTreeMap<String, String>,
}

fn is_captured_assignment(origin: &WriterCorrectionAssignmentOrigin) -> bool {
    *origin == WriterCorrectionAssignmentOrigin::Captured
}

impl WriterCorrectionAssignment {
    pub fn validate_for_task(&self, task: &Task) -> Result<(), String> {
        if !(1..=WRITER_CORRECTION_ASSIGNMENT_SCHEMA_VERSION).contains(&self.schema_version) {
            return Err(format!(
                "unsupported Writer correction assignment schema version {}",
                self.schema_version
            ));
        }
        for (name, value) in [
            ("Work Item", &self.work_item_id),
            ("Attempt", &self.attempt_id),
            ("Writer Task", &self.writer_task_id),
            ("source Task", &self.source_task_id),
            ("source candidate commit", &self.source_candidate_commit),
        ] {
            if value.trim().is_empty() || value.len() > 512 {
                return Err(format!("{name} binding is empty or too large"));
            }
        }
        if self.work_item_id != task.work_item_id {
            return Err("assignment Work Item does not match its Task".into());
        }
        if self.attempt_id != task.attempt_id.as_deref().unwrap_or("") {
            return Err("assignment Attempt does not match its Task".into());
        }
        if self.writer_task_id != task.id {
            return Err("assignment Writer Task does not match its Task".into());
        }
        if self.source_task_id == task.id {
            return Err("assignment source Task must precede its Writer Task".into());
        }
        if self.source_task_inputs.len() > MAX_WRITER_CORRECTION_ASSIGNMENT_INPUTS
            || self.evidence.len() > MAX_WRITER_CORRECTION_ASSIGNMENT_INPUTS
            || self.findings.len() > MAX_WRITER_CORRECTION_ASSIGNMENT_FINDINGS
        {
            return Err("assignment exceeds its bounded input or finding limit".into());
        }
        validate_assignment_inputs("source Task", &self.source_task_inputs)?;
        validate_assignment_inputs("evidence", &self.evidence)?;
        if !self.findings.is_empty() && self.evidence.is_empty() {
            return Err("assignment findings require bound evidence".into());
        }
        let mut finding_ids = HashSet::new();
        for finding in &self.findings {
            if finding.id.trim().is_empty()
                || finding.role.trim().is_empty()
                || finding.source.trim().is_empty()
                || finding.requirement.trim().is_empty()
                || finding.id.len() > 512
                || finding.role.len() > 128
                || finding.source.len() > 1024
                || finding.requirement.len() > 16 * 1024
                || !finding_ids.insert(finding.id.as_str())
            {
                return Err("assignment findings must be bounded, complete, and unique".into());
            }
        }
        Ok(())
    }
}

fn validate_assignment_inputs(
    label: &str,
    inputs: &[WriterCorrectionAssignmentInput],
) -> Result<(), String> {
    let mut refs = HashSet::new();
    for input in inputs {
        if input.artifact.producer_id.trim().is_empty()
            || input.artifact.path.trim().is_empty()
            || input.artifact.producer_id.len() > 512
            || input.artifact.path.len() > 1024
            || !valid_sha256_digest(&input.digest)
            || !refs.insert((&input.artifact.producer_id, &input.artifact.path))
        {
            return Err(format!(
                "{label} assignment inputs must have unique bounded artifact references and SHA-256 digests"
            ));
        }
    }
    Ok(())
}

fn valid_sha256_digest(value: &str) -> bool {
    let Some(hex) = value.strip_prefix("sha256:") else {
        return false;
    };
    hex.len() == 64 && hex.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum WriterCompletionState {
    #[default]
    Pending,
    Complete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WriterVerificationResult {
    Pass,
}

/// One focused verification claim supplied by the Writer. It is advisory input
/// to review; the host validates its shape, while the final Tester remains the
/// authoritative full-suite result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WriterVerificationClaim {
    pub command: String,
    pub result: WriterVerificationResult,
}

fn deserialize_string_or_vec<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum StringOrVec {
        String(String),
        Vec(Vec<String>),
    }

    Ok(match StringOrVec::deserialize(deserializer)? {
        StringOrVec::String(value) => vec![value],
        StringOrVec::Vec(values) => values,
    })
}

/// One row in a task-specific Writer completion matrix. The first five fields
/// are host-owned and immutable. Writers fill only the remaining claim fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct WriterCompletionRow {
    /// Immutable approved responsibility; absent on legacy requirements.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub responsibility: Option<CompletionResponsibility>,
    pub id: String,
    pub kind: WriterCompletionRequirementKind,
    pub source: String,
    pub requirement: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub verification_refs: Vec<String>,
    /// Host-owned structured command copied from the approved Plan row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<PlanCommand>,
    #[serde(default)]
    pub state: WriterCompletionState,
    #[serde(default, deserialize_with = "deserialize_string_or_vec")]
    pub implementation_evidence: Vec<String>,
    #[serde(default)]
    pub verification: Vec<WriterVerificationClaim>,
    #[serde(default)]
    pub applicable_constraints: Vec<String>,
}

impl From<&WriterCompletionRequirement> for WriterCompletionRow {
    fn from(requirement: &WriterCompletionRequirement) -> Self {
        Self {
            responsibility: requirement.responsibility,
            id: requirement.id.clone(),
            kind: requirement.kind,
            source: requirement.source.clone(),
            requirement: requirement.requirement.clone(),
            verification_refs: requirement.verification_refs.clone(),
            command: requirement.command.clone(),
            state: WriterCompletionState::Pending,
            implementation_evidence: Vec::new(),
            verification: Vec::new(),
            applicable_constraints: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct WriterCompletionMatrix {
    pub schema_version: u32,
    pub task_id: String,
    pub rows: Vec<WriterCompletionRow>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriterCompletionMatrixStatus {
    pub complete: usize,
    pub required: usize,
    pub incomplete_reason: Option<String>,
}

/// One deterministic repair to fields owned by the Writer. Host-owned row
/// identity is validated before any repair is allowed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct WriterCompletionRepair {
    pub row_id: String,
    pub kind: WriterCompletionRepairKind,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub removed_constraints: Vec<String>,
    #[serde(skip_serializing_if = "is_zero")]
    pub cleared_implementation_evidence: usize,
    #[serde(skip_serializing_if = "is_zero")]
    pub cleared_verification_claims: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum WriterCompletionRepairKind {
    RestoredHostOwnedFields,
    ClearedNonFindingConstraints,
    ClearedPendingClaims,
    RemovedUnknownFindingConstraints,
}

fn is_zero(value: &usize) -> bool {
    *value == 0
}

impl WriterCompletionMatrixStatus {
    pub fn is_complete(&self) -> bool {
        self.complete == self.required && self.incomplete_reason.is_none()
    }
}

impl WriterCompletionMatrix {
    /// Keep the exact unfinished Writer obligations when presenting current work.
    pub(crate) fn pending_writer_rows(&self) -> Vec<WriterCompletionRow> {
        self.rows
            .iter()
            .filter(|row| {
                writer_responsibility(row.responsibility)
                    && row.state == WriterCompletionState::Pending
            })
            .cloned()
            .collect()
    }

    /// Compare stable obligations and completed identities across continuations.
    /// Finding paths may change when the same finding is reported again; evidence
    /// wording and commit counts do not themselves establish additional work.
    pub(crate) fn completion_identity(&self) -> Vec<WriterCompletionRow> {
        self.rows
            .iter()
            .filter(|row| writer_responsibility(row.responsibility))
            .map(|row| {
                let mut identity = row.clone();
                if identity.kind == WriterCompletionRequirementKind::Finding {
                    identity.source.clear();
                }
                identity.implementation_evidence.clear();
                identity.verification.clear();
                identity.applicable_constraints.clear();
                identity
            })
            .collect()
    }

    /// Carry forward completed planning claims from an authoritative earlier
    /// matrix without changing findings or any claim the current Writer already
    /// made. Host-produced planning repairs use this when a later Writer was
    /// materialized from the superseded task artifact.
    pub(crate) fn inherit_completed_planning_claims(&mut self, prior: &Self) -> usize {
        let mut inherited = 0;
        for row in &mut self.rows {
            if row.kind == WriterCompletionRequirementKind::Finding
                || !writer_responsibility(row.responsibility)
                || row.state != WriterCompletionState::Pending
                || !row.implementation_evidence.is_empty()
                || !row.verification.is_empty()
                || !row.applicable_constraints.is_empty()
            {
                continue;
            }
            let Some(previous) = prior.rows.iter().find(|previous| {
                previous.id == row.id
                    && previous.kind == row.kind
                    && previous.source == row.source
                    && previous.requirement == row.requirement
                    && previous.verification_refs == row.verification_refs
                    && previous.command == row.command
                    && previous.responsibility == row.responsibility
                    && previous.state == WriterCompletionState::Complete
            }) else {
                continue;
            };
            row.state = previous.state;
            row.implementation_evidence = previous.implementation_evidence.clone();
            row.verification = previous.verification.clone();
            inherited += 1;
        }
        inherited
    }

    fn expected_against(
        &self,
        contract: &WriterCompletionContract,
        task_id: &str,
        findings: &[WriterCompletionFinding],
    ) -> Result<(), String> {
        let actual_bytes = serde_json::to_vec_pretty(self)
            .map_err(|error| format!("serialize Writer completion matrix: {error}"))?;
        if actual_bytes.len() > WRITER_COMPLETION_MATRIX_MAX_BYTES {
            return Err(format!(
                "Writer completion matrix is {} bytes; maximum is {} bytes",
                actual_bytes.len(),
                WRITER_COMPLETION_MATRIX_MAX_BYTES
            ));
        }
        if !(1..=WRITER_COMPLETION_CONTRACT_VERSION).contains(&self.schema_version) {
            return Err(format!(
                "Writer completion matrix schema version {} does not match {}",
                self.schema_version, WRITER_COMPLETION_CONTRACT_VERSION
            ));
        }
        if self.task_id != task_id {
            return Err(format!(
                "Writer completion matrix belongs to Task {:?}, expected {:?}",
                self.task_id, task_id
            ));
        }
        let expected = Self::skeleton(contract, task_id, findings, None)?;
        if self.rows.len() != expected.rows.len() {
            return Err(format!(
                "Writer completion matrix has {} rows; expected {} rows",
                self.rows.len(),
                expected.rows.len()
            ));
        }
        for (row, expected_row) in self.rows.iter().zip(&expected.rows) {
            if row.id != expected_row.id
                || row.kind != expected_row.kind
                || row.source != expected_row.source
                || row.requirement != expected_row.requirement
                || row.verification_refs != expected_row.verification_refs
                || row.command != expected_row.command
                || row.responsibility != expected_row.responsibility
            {
                return Err(format!(
                    "Writer completion matrix immutable row {:?} was omitted, reordered, or rewritten",
                    expected_row.id
                ));
            }
        }
        Ok(())
    }

    /// Normalize only Writer-owned completion claims. Every host-owned row field
    /// is checked against the planning contract before a repair is allowed.
    pub fn repair_writer_claims_against(
        &mut self,
        contract: &WriterCompletionContract,
        task_id: &str,
        findings: &[WriterCompletionFinding],
    ) -> Result<Vec<WriterCompletionRepair>, String> {
        let actual_bytes = serde_json::to_vec_pretty(self)
            .map_err(|error| format!("serialize Writer completion matrix: {error}"))?;
        if actual_bytes.len() > WRITER_COMPLETION_MATRIX_MAX_BYTES {
            return Err(format!(
                "Writer completion matrix is {} bytes; maximum is {} bytes",
                actual_bytes.len(),
                WRITER_COMPLETION_MATRIX_MAX_BYTES
            ));
        }
        if !(1..=WRITER_COMPLETION_CONTRACT_VERSION).contains(&self.schema_version) {
            return Err(format!(
                "Writer completion matrix schema version {} does not match {}",
                self.schema_version, WRITER_COMPLETION_CONTRACT_VERSION
            ));
        }
        if self.task_id != task_id {
            return Err(format!(
                "Writer completion matrix belongs to Task {:?}, expected {:?}",
                self.task_id, task_id
            ));
        }
        let expected = Self::skeleton(contract, task_id, findings, None)?;
        if self.rows.len() != expected.rows.len() {
            return Err(format!(
                "Writer completion matrix has {} rows; expected {} rows",
                self.rows.len(),
                expected.rows.len()
            ));
        }
        let mut repairs = Vec::new();
        for (row, expected_row) in self.rows.iter_mut().zip(&expected.rows) {
            if row.responsibility != expected_row.responsibility
                || (!writer_responsibility(row.responsibility)
                    && (row.state != WriterCompletionState::Pending
                        || !row.implementation_evidence.is_empty()
                        || !row.verification.is_empty()
                        || !row.applicable_constraints.is_empty()))
            {
                return Err(format!(
                    "Writer changed responsibility or claimed external completion for {}",
                    row.id
                ));
            }
            if row.id != expected_row.id || row.kind != expected_row.kind {
                return Err(format!(
                    "Writer completion matrix immutable row {:?} was omitted, reordered, or rewritten",
                    expected_row.id
                ));
            }
            if row.source != expected_row.source
                || row.requirement != expected_row.requirement
                || row.verification_refs != expected_row.verification_refs
                || row.command != expected_row.command
                || row.responsibility != expected_row.responsibility
            {
                row.source = expected_row.source.clone();
                row.requirement = expected_row.requirement.clone();
                row.verification_refs = expected_row.verification_refs.clone();
                row.command = expected_row.command.clone();
                repairs.push(WriterCompletionRepair {
                    row_id: row.id.clone(),
                    kind: WriterCompletionRepairKind::RestoredHostOwnedFields,
                    removed_constraints: Vec::new(),
                    cleared_implementation_evidence: 0,
                    cleared_verification_claims: 0,
                });
            }
        }
        let allowed_constraints = contract
            .applicable_constraint_ids()
            .collect::<std::collections::HashSet<_>>();
        for row in &mut self.rows {
            if row.state == WriterCompletionState::Pending
                && (!row.implementation_evidence.is_empty()
                    || !row.verification.is_empty()
                    || !row.applicable_constraints.is_empty())
            {
                repairs.push(WriterCompletionRepair {
                    row_id: row.id.clone(),
                    kind: WriterCompletionRepairKind::ClearedPendingClaims,
                    removed_constraints: std::mem::take(&mut row.applicable_constraints),
                    cleared_implementation_evidence: row.implementation_evidence.len(),
                    cleared_verification_claims: row.verification.len(),
                });
                row.implementation_evidence.clear();
                row.verification.clear();
                continue;
            }
            if row.kind != WriterCompletionRequirementKind::Finding
                && !row.applicable_constraints.is_empty()
            {
                repairs.push(WriterCompletionRepair {
                    row_id: row.id.clone(),
                    kind: WriterCompletionRepairKind::ClearedNonFindingConstraints,
                    removed_constraints: std::mem::take(&mut row.applicable_constraints),
                    cleared_implementation_evidence: 0,
                    cleared_verification_claims: 0,
                });
                continue;
            }
            if row.kind == WriterCompletionRequirementKind::Finding {
                let mut removed = Vec::new();
                row.applicable_constraints.retain(|reference| {
                    let declared_none = reference
                        .strip_prefix("none: ")
                        .is_some_and(|reason| !reason.trim().is_empty());
                    let keep = allowed_constraints.contains(reference.as_str()) || declared_none;
                    if !keep {
                        removed.push(reference.clone());
                    }
                    keep
                });
                if !removed.is_empty() {
                    repairs.push(WriterCompletionRepair {
                        row_id: row.id.clone(),
                        kind: WriterCompletionRepairKind::RemovedUnknownFindingConstraints,
                        removed_constraints: removed,
                        cleared_implementation_evidence: 0,
                        cleared_verification_claims: 0,
                    });
                }
            }
        }
        Ok(repairs)
    }

    pub fn skeleton(
        contract: &WriterCompletionContract,
        task_id: &str,
        findings: &[WriterCompletionFinding],
        prior: Option<&Self>,
    ) -> Result<Self, String> {
        contract.validate()?;
        if !(1..=WRITER_COMPLETION_CONTRACT_VERSION).contains(&contract.version) {
            return Err(format!(
                "Writer completion contract schema version {} does not match {}",
                contract.version, WRITER_COMPLETION_CONTRACT_VERSION
            ));
        }
        let mut rows = contract
            .requirements
            .iter()
            .map(WriterCompletionRow::from)
            .collect::<Vec<_>>();
        if let Some(prior) = prior {
            for row in &mut rows {
                let Some(previous) = prior.rows.iter().find(|previous| {
                    previous.id == row.id
                        && previous.kind == row.kind
                        && previous.source == row.source
                        && previous.requirement == row.requirement
                        && previous.verification_refs == row.verification_refs
                        && previous.command == row.command
                        && previous.responsibility == row.responsibility
                        && writer_responsibility(row.responsibility)
                        && previous.state == WriterCompletionState::Complete
                }) else {
                    continue;
                };
                row.state = previous.state;
                row.implementation_evidence = previous.implementation_evidence.clone();
                row.verification = previous.verification.clone();
            }
        }
        rows.extend(findings.iter().map(|finding| WriterCompletionRow {
            responsibility: None,
            id: finding.id.clone(),
            kind: WriterCompletionRequirementKind::Finding,
            source: finding.source.clone(),
            requirement: format!(
                "Address {} review finding: {}",
                finding.role, finding.requirement
            ),
            verification_refs: Vec::new(),
            command: None,
            state: WriterCompletionState::Pending,
            implementation_evidence: Vec::new(),
            verification: Vec::new(),
            applicable_constraints: Vec::new(),
        }));
        let matrix = Self {
            schema_version: WRITER_COMPLETION_CONTRACT_VERSION,
            task_id: task_id.to_string(),
            rows,
        };
        let bytes = serde_json::to_vec_pretty(&matrix)
            .map_err(|error| format!("serialize Writer completion matrix: {error}"))?;
        if bytes.len() > WRITER_COMPLETION_MATRIX_MAX_BYTES {
            return Err(format!(
                "Writer completion matrix is {} bytes; maximum is {} bytes",
                bytes.len(),
                WRITER_COMPLETION_MATRIX_MAX_BYTES
            ));
        }
        Ok(matrix)
    }

    pub fn status_against(
        &self,
        contract: &WriterCompletionContract,
        task_id: &str,
        findings: &[WriterCompletionFinding],
    ) -> Result<WriterCompletionMatrixStatus, String> {
        self.expected_against(contract, task_id, findings)?;
        let allowed_constraints = contract
            .applicable_constraint_ids()
            .collect::<std::collections::HashSet<_>>();
        let mut complete = 0usize;
        let mut incomplete_reason = None;
        for row in &self.rows {
            if !writer_responsibility(row.responsibility) {
                continue;
            }
            if row.state == WriterCompletionState::Pending
                && (!row.implementation_evidence.is_empty()
                    || !row.verification.is_empty()
                    || !row.applicable_constraints.is_empty())
            {
                return Err(format!(
                    "Writer completion row {:?} is pending but carries completion claims",
                    row.id
                ));
            }
            if row.state == WriterCompletionState::Pending {
                incomplete_reason.get_or_insert_with(|| {
                    format!("Writer completion row {:?} is still pending", row.id)
                });
                continue;
            }
            if row.kind != WriterCompletionRequirementKind::Finding
                && !row.applicable_constraints.is_empty()
            {
                return Err(format!(
                    "Writer completion row {:?} is not a review finding but carries constraint mappings",
                    row.id
                ));
            }
            if row
                .implementation_evidence
                .iter()
                .all(|value| value.trim().is_empty())
            {
                incomplete_reason.get_or_insert_with(|| {
                    format!(
                        "Writer completion row {:?} lacks implementation evidence",
                        row.id
                    )
                });
                continue;
            }
            if row.verification.is_empty()
                || row
                    .verification
                    .iter()
                    .any(|claim| claim.command.trim().is_empty())
            {
                incomplete_reason.get_or_insert_with(|| {
                    format!(
                        "Writer completion row {:?} lacks focused verification",
                        row.id
                    )
                });
                continue;
            }
            if row.kind == WriterCompletionRequirementKind::Finding {
                if row.applicable_constraints.is_empty() {
                    incomplete_reason.get_or_insert_with(|| {
                        format!(
                            "Writer completion finding {:?} lacks an applicable behavior, approach, or required Plan constraint",
                            row.id
                        )
                    });
                    continue;
                }
                for reference in &row.applicable_constraints {
                    let declared_none = reference
                        .strip_prefix("none: ")
                        .is_some_and(|reason| !reason.trim().is_empty());
                    if !allowed_constraints.contains(reference.as_str()) && !declared_none {
                        return Err(format!(
                            "Writer completion finding {:?} references unknown constraint {:?}",
                            row.id, reference
                        ));
                    }
                }
            }
            complete += 1;
        }
        Ok(WriterCompletionMatrixStatus {
            complete,
            required: self
                .rows
                .iter()
                .filter(|row| writer_responsibility(row.responsibility))
                .count(),
            incomplete_reason,
        })
    }
}

/// Validated completion state of the authoritative required-progress manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequiredProgressStatus {
    pub checked: usize,
    pub required: usize,
    pub incomplete_reason: Option<String>,
}

impl RequiredProgressStatus {
    pub fn is_complete(&self) -> bool {
        self.checked == self.required && self.incomplete_reason.is_none()
    }
}

impl ProgressContract {
    /// Derive required Plan progress from the validated Writer matrix without
    /// coupling it to task-specific review findings. Corrective rounds replace
    /// their finding frontier, but the approved Plan rows remain stable.
    pub(crate) fn status_against_writer_matrix(
        &self,
        matrix: &WriterCompletionMatrix,
    ) -> Result<RequiredProgressStatus, String> {
        let plan_rows = matrix
            .rows
            .iter()
            .filter(|row| {
                row.kind == WriterCompletionRequirementKind::Plan
                    && writer_responsibility(row.responsibility)
            })
            .collect::<Vec<_>>();
        if plan_rows.len() != self.required.len() {
            return Err(format!(
                "required-progress contract has {} rows but Writer completion matrix has {} Plan rows",
                self.required.len(),
                plan_rows.len()
            ));
        }

        let mut checked = 0usize;
        let mut incomplete_reason = None;
        for entry in &self.required {
            let matrix_id = format!("plan:{}", entry.id);
            let matching = plan_rows
                .iter()
                .filter(|row| row.id == matrix_id)
                .copied()
                .collect::<Vec<_>>();
            let [row] = matching.as_slice() else {
                return Err(format!(
                    "required-progress entry {:?} must map to exactly one Writer completion row {:?}",
                    entry.id, matrix_id
                ));
            };
            if row.requirement != entry.requirement {
                return Err(format!(
                    "required-progress entry {:?} disagrees with Writer completion row {:?}",
                    entry.id, matrix_id
                ));
            }
            if row.state == WriterCompletionState::Pending {
                incomplete_reason.get_or_insert_with(|| {
                    format!("Writer completion row {:?} is still pending", matrix_id)
                });
                continue;
            }
            if row
                .implementation_evidence
                .iter()
                .all(|value| value.trim().is_empty())
            {
                incomplete_reason.get_or_insert_with(|| {
                    format!(
                        "Writer completion row {:?} lacks implementation evidence",
                        matrix_id
                    )
                });
                continue;
            }
            if row.verification.is_empty()
                || row
                    .verification
                    .iter()
                    .any(|claim| claim.command.trim().is_empty())
            {
                incomplete_reason.get_or_insert_with(|| {
                    format!(
                        "Writer completion row {:?} lacks focused verification",
                        matrix_id
                    )
                });
                continue;
            }
            if !row.applicable_constraints.is_empty() {
                return Err(format!(
                    "Writer completion row {:?} is not a review finding but carries constraint mappings",
                    matrix_id
                ));
            }
            checked += 1;
        }

        Ok(RequiredProgressStatus {
            checked,
            required: self.required.len(),
            incomplete_reason,
        })
    }

    /// Derive an immutable manifest from a Plan's markdown step table, one entry per
    /// required row (`Req?` column `Yes`). Returns `None` when the Plan has no
    /// required rows, so an Attempt without required work carries no marker and
    /// stays legacy-compatible. Stable ids are `step-<#>` from the row's `#` column;
    /// requirements are the row's `State reached` cell.
    pub fn from_plan(plan: &str) -> Option<Self> {
        let required = parse_required_plan_rows(plan);
        if required.is_empty() {
            None
        } else {
            Some(Self {
                version: PROGRESS_CONTRACT_VERSION,
                required,
            })
        }
    }

    /// Render the authoritative `## Required completion` section from this manifest:
    /// one unchecked top-level entry per required row, in manifest order. This is the
    /// exact section the host materializes before the first Writer.
    pub fn render_required_completion_section(&self) -> String {
        let mut out = format!("{REQUIRED_COMPLETION_HEADING}\n\n");
        for entry in &self.required {
            out.push_str(&format!("- [ ] {} — {}\n", entry.id, entry.requirement));
        }
        out
    }

    /// Render completed required progress from the host-validated Writer matrix.
    /// The immutable requirement text always comes from this contract; the
    /// projection points at the matrix row instead of copying Writer-authored
    /// evidence back into a second completion authority.
    pub fn render_completed_required_progress_section(
        &self,
        matrix: &WriterCompletionMatrix,
        matrix_path: &str,
    ) -> Result<String, String> {
        let plan_rows = matrix
            .rows
            .iter()
            .filter(|row| {
                row.kind == WriterCompletionRequirementKind::Plan
                    && writer_responsibility(row.responsibility)
            })
            .collect::<Vec<_>>();
        if plan_rows.len() != self.required.len() {
            return Err(format!(
                "required-progress contract has {} rows but Writer completion matrix has {} Plan rows",
                self.required.len(),
                plan_rows.len()
            ));
        }

        let mut out = format!("{REQUIRED_COMPLETION_HEADING}\n\n");
        for entry in &self.required {
            let matrix_id = format!("plan:{}", entry.id);
            let matching = plan_rows
                .iter()
                .filter(|row| row.id == matrix_id)
                .copied()
                .collect::<Vec<_>>();
            let [row] = matching.as_slice() else {
                return Err(format!(
                    "required-progress entry {:?} must map to exactly one Writer completion row {:?}",
                    entry.id, matrix_id
                ));
            };
            if row.requirement != entry.requirement {
                return Err(format!(
                    "required-progress entry {:?} disagrees with Writer completion row {:?}",
                    entry.id, matrix_id
                ));
            }
            if row.state != WriterCompletionState::Complete {
                return Err(format!(
                    "Writer completion row {:?} is not complete",
                    matrix_id
                ));
            }
            out.push_str(&format!(
                "- [x] {} — {}; Evidence: {}#{}\n",
                entry.id, entry.requirement, matrix_path, matrix_id
            ));
        }
        Ok(out)
    }

    /// Replace only the `## Required completion` section with a host-rendered
    /// projection. Content before the section and every following `## ` section
    /// remain outside the host-owned boundary.
    pub fn project_completed_required_progress(
        &self,
        existing: &str,
        matrix: &WriterCompletionMatrix,
        matrix_path: &str,
    ) -> Result<String, String> {
        let replacement = self.render_completed_required_progress_section(matrix, matrix_path)?;
        let Some((start, end)) = required_completion_section_range(existing) else {
            return Ok(if existing.trim().is_empty() {
                replacement
            } else {
                format!("{}\n\n{replacement}", existing.trim_end())
            });
        };

        let mut projected = String::with_capacity(
            existing.len() - (end - start) + replacement.len() + usize::from(end < existing.len()),
        );
        projected.push_str(&existing[..start]);
        projected.push_str(&replacement);
        if end < existing.len() {
            projected.push('\n');
            projected.push_str(&existing[end..]);
        }
        Ok(projected)
    }
}

/// Return the byte range occupied by the required-completion section, including
/// its heading and everything before the next level-two heading.
fn required_completion_section_range(source: &str) -> Option<(usize, usize)> {
    let mut start = None;
    let mut offset = 0usize;
    for line in source.split_inclusive('\n') {
        let trimmed = line.trim();
        if start.is_none() {
            if trimmed == REQUIRED_COMPLETION_HEADING {
                start = Some(offset);
            }
        } else if trimmed.starts_with("## ") {
            return Some((start.expect("the section start is set"), offset));
        }
        offset += line.len();
    }
    start.map(|start| (start, source.len()))
}

/// One parsed top-level entry of the current `## Required completion` section.
struct ParsedRequiredEntry {
    id: String,
    requirement: String,
    checked: bool,
    evidence: String,
}

/// Extract the lines of the authoritative `## Required completion` section: the
/// lines after its heading up to the next `## ` heading or end of file. Returns
/// `None` when the section heading is absent, so historical Attempt files, banners,
/// and every other section are never scanned.
fn extract_required_completion_section(source: &str) -> Option<Vec<&str>> {
    let mut lines = source.lines();
    lines
        .by_ref()
        .find(|line| line.trim() == REQUIRED_COMPLETION_HEADING)?;
    let mut section = Vec::new();
    for line in lines {
        if line.trim_start().starts_with("## ") {
            break;
        }
        section.push(line);
    }
    Some(section)
}

/// Parse one top-level checklist line of the required-progress section. A top-level
/// entry is exactly `- [ ] <id> — <requirement>` or
/// `- [x] <id> — <requirement>; Evidence: <source>`; anything else is malformed.
fn parse_required_entry(line: &str) -> Result<ParsedRequiredEntry, String> {
    let (checked, rest) = if let Some(rest) = line.strip_prefix("- [x] ") {
        (true, rest)
    } else if let Some(rest) = line.strip_prefix("- [ ] ") {
        (false, rest)
    } else {
        return Err(format!("malformed required-progress line: {line:?}"));
    };
    let (id, after) = rest.split_once(" — ").ok_or_else(|| {
        format!("malformed required-progress entry (missing ` — ` separator): {line:?}")
    })?;
    let id = id.trim().to_string();
    if id.is_empty() {
        return Err(format!(
            "malformed required-progress entry (empty id): {line:?}"
        ));
    }
    if checked {
        let (requirement, evidence) = after.split_once("; Evidence:").ok_or_else(|| {
            format!("checked required-progress entry missing `; Evidence:`: {line:?}")
        })?;
        Ok(ParsedRequiredEntry {
            id,
            requirement: requirement.trim().to_string(),
            checked: true,
            evidence: evidence.trim().to_string(),
        })
    } else {
        Ok(ParsedRequiredEntry {
            id,
            requirement: after.trim().to_string(),
            checked: false,
            evidence: String::new(),
        })
    }
}

/// Parse a Plan's markdown step table into its required rows. A data row is a
/// pipe-delimited line whose first cell is a step number and whose last cell is the
/// `Req?` verdict; a row counts as required when that verdict is `Yes` (case-
/// insensitive). The header and separator rows are skipped. Requirement text is the
/// `State reached` cell (the second column).
fn parse_required_plan_rows(plan: &str) -> Vec<RequiredProgressEntry> {
    let mut rows = Vec::new();
    for line in plan.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with('|') {
            continue;
        }
        // Split interior cells, dropping the empty leading/trailing splits a
        // `| a | b |` row produces.
        let cells: Vec<&str> = trimmed
            .trim_matches('|')
            .split('|')
            .map(|cell| cell.trim())
            .collect();
        if cells.len() < 3 {
            continue;
        }
        let number = cells[0];
        if !number.chars().all(|c| c.is_ascii_digit()) || number.is_empty() {
            // Header row (`#`), separator row (`---`), or a non-row line.
            continue;
        }
        let req = cells[cells.len() - 1];
        if !req.eq_ignore_ascii_case("yes") {
            continue;
        }
        let requirement = cells[1].to_string();
        rows.push(RequiredProgressEntry {
            id: format!("step-{number}"),
            requirement,
        });
    }
    rows
}

impl AttemptLearning {
    /// A durable in-progress reservation persisted before the Learner's coder runs
    /// and before any handoff is written. It is crash-observable and retryable: a
    /// run that crashes after this write leaves this record, which the loop treats
    /// as still pending.
    pub fn in_progress(runs: u32) -> Self {
        Self {
            status: LearningStatus::InProgress,
            runs,
            handoff: None,
            last_failure: None,
            failure_kind: None,
            failure_stage: None,
        }
    }

    /// A durable, non-landable prepared state persisted after the Learner's output
    /// is accepted but BEFORE the canonical handoff is exposed. It carries no
    /// handoff reference yet and is pending, so advancement stays blocked and a
    /// crash before the handoff write leaves a retryable record rather than an
    /// exposed handoff with no durable learning state.
    pub fn handoff_pending(runs: u32) -> Self {
        Self {
            status: LearningStatus::HandoffPending,
            runs,
            handoff: None,
            last_failure: None,
            failure_kind: None,
            failure_stage: None,
        }
    }

    /// A completed learning record carrying its single handoff reference.
    pub fn succeeded(runs: u32, handoff: crate::follow_up::ArtifactRef) -> Self {
        Self {
            status: LearningStatus::Succeeded,
            runs,
            handoff: Some(handoff),
            last_failure: None,
            failure_kind: None,
            failure_stage: None,
        }
    }

    /// A failed, retryable learning record carrying its diagnostic.
    pub fn failed(runs: u32, reason: impl Into<String>) -> Self {
        Self::failed_with_kind(runs, reason, LearningFailureKind::Generic)
    }

    /// A failed learning record carrying its diagnostic and typed classification.
    pub fn failed_with_kind(
        runs: u32,
        reason: impl Into<String>,
        kind: LearningFailureKind,
    ) -> Self {
        Self {
            status: LearningStatus::Failed,
            runs,
            handoff: None,
            last_failure: Some(reason.into()),
            failure_kind: Some(kind),
            failure_stage: None,
        }
    }

    /// A failed handoff publication after Fluent accepted canonical Learning.
    pub fn failed_after_handoff_publication(
        runs: u32,
        reason: impl Into<String>,
        kind: LearningFailureKind,
    ) -> Self {
        Self {
            failure_stage: Some(LearningFailureStage::HandoffPublication),
            ..Self::failed_with_kind(runs, reason, kind)
        }
    }

    pub fn is_failed(&self) -> bool {
        self.status == LearningStatus::Failed
    }

    pub fn is_succeeded(&self) -> bool {
        self.status == LearningStatus::Succeeded
    }

    /// A durable in-progress reservation whose run has not yet reached a terminal
    /// success or failure. The loop treats it as still pending so a crashed run is
    /// retried rather than mistaken for a completed one.
    pub fn is_in_progress(&self) -> bool {
        self.status == LearningStatus::InProgress
    }

    /// A durable, non-landable prepared state: the coder ran and its output was
    /// accepted, but the canonical handoff has not been exposed and the terminal
    /// `Succeeded` has not been persisted. Pending, so advancement stays blocked.
    pub fn is_handoff_pending(&self) -> bool {
        self.status == LearningStatus::HandoffPending
    }

    /// Whether the Learner still needs a (re)run for this Attempt: no record yet, a
    /// failed run, or a durable in-progress reservation left by a crash.
    pub fn is_pending(&self) -> bool {
        !self.is_succeeded()
    }

    /// Whether recovery may relaunch the Learner coder for this record. A
    /// coder-already-ran/evidence-pending disposition is pending (non-succeeded)
    /// yet must never relaunch, since the coder's expertise effects may already be
    /// on disk. Every other state is safe to (re)launch.
    pub fn is_relaunchable(&self) -> bool {
        match self.failure_kind {
            Some(kind) => kind.phase_recovery().is_relaunchable(),
            None => true,
        }
    }
}

fn push_planning_section(sections: &mut Vec<String>, title: &str, content: &Option<String>) {
    if let Some(content) = non_empty_clone(content) {
        sections.push(format!("# {title}\n\n{}", content.trim()));
    }
}

/// Coder, model, and optional effort for one Task kind in a coder mapping.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoderModelPair {
    pub coder: CoderKind,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
}

/// Writer and reviewer coder mapping stored on each Attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoderMapping {
    pub write: CoderModelPair,
    pub review: CoderModelPair,
}

impl Default for CoderMapping {
    fn default() -> Self {
        let default_pair = CoderModelPair {
            coder: CoderKind::Claude,
            model: String::new(),
            effort: None,
        };
        Self {
            write: default_pair.clone(),
            review: default_pair,
        }
    }
}

impl CoderMapping {
    pub fn for_task_kind(&self, kind: TaskKind) -> &CoderModelPair {
        match kind {
            TaskKind::Write => &self.write,
            TaskKind::Review => &self.review,
            TaskKind::Tester => &self.write,
            _ => &self.write,
        }
    }

    /// Apply only the supplied run-time fields over this persisted mapping.
    ///
    /// Callers must pass command-line inputs only. Configuration and environment
    /// inputs select a mapping when an Attempt is created, not when it runs.
    pub fn overlay_run_inputs(&self, inputs: &CoderMappingInputs) -> Result<Self, anyhow::Error> {
        let mut mapping = self.clone();

        if let Some(coder) = inputs.global_coder.as_deref() {
            let coder = CoderKind::resolve(Some(coder))?;
            mapping.write.coder = coder;
            mapping.review.coder = coder;
        }

        let overlay_pair = |pair: &mut CoderModelPair,
                            coder: &Option<String>,
                            model: &Option<String>,
                            effort: &Option<String>|
         -> Result<(), anyhow::Error> {
            if let Some(coder) = coder.as_deref() {
                pair.coder = CoderKind::resolve(Some(coder))?;
            }
            if let Some(model) = model {
                pair.model = model.clone();
            }
            if let Some(effort) = effort {
                pair.effort = Some(effort.clone());
            }
            Ok(())
        };

        overlay_pair(
            &mut mapping.write,
            &inputs.write_coder,
            &inputs.write_model,
            &inputs.write_effort,
        )?;
        overlay_pair(
            &mut mapping.review,
            &inputs.review_coder,
            &inputs.review_model,
            &inputs.review_effort,
        )?;
        Ok(mapping)
    }
}

/// Sparse inputs for creation-time resolution or an explicit run-time overlay.
///
/// Creation may merge configuration and environment values into this shape.
/// Existing Attempt execution must construct it from command-line flags only.
#[derive(Debug, Default)]
pub struct CoderMappingInputs {
    pub write_coder: Option<String>,
    pub write_model: Option<String>,
    pub write_effort: Option<String>,
    pub review_coder: Option<String>,
    pub review_model: Option<String>,
    pub review_effort: Option<String>,
    pub global_coder: Option<String>,
}

impl CoderMappingInputs {
    /// Return whether this invocation supplied any run-time mapping field.
    pub fn has_run_overrides(&self) -> bool {
        self.write_coder.is_some()
            || self.write_model.is_some()
            || self.write_effort.is_some()
            || self.review_coder.is_some()
            || self.review_model.is_some()
            || self.review_effort.is_some()
            || self.global_coder.is_some()
    }

    pub fn from_env() -> Self {
        Self {
            write_coder: std::env::var("FLUENT_WRITE_CODER").ok(),
            write_model: std::env::var("FLUENT_WRITE_MODEL").ok(),
            write_effort: std::env::var("FLUENT_WRITE_EFFORT").ok(),
            review_coder: std::env::var("FLUENT_REVIEW_CODER").ok(),
            review_model: std::env::var("FLUENT_REVIEW_MODEL").ok(),
            review_effort: std::env::var("FLUENT_REVIEW_EFFORT").ok(),
            global_coder: std::env::var("FLUENT_CODER").ok(),
        }
    }

    /// Overlay `other` onto `self`: fields set in `other` win; unset fields
    /// fall through to `self`.
    pub fn merge(self, other: Self) -> Self {
        Self {
            write_coder: other.write_coder.or(self.write_coder),
            write_model: other.write_model.or(self.write_model),
            write_effort: other.write_effort.or(self.write_effort),
            review_coder: other.review_coder.or(self.review_coder),
            review_model: other.review_model.or(self.review_model),
            review_effort: other.review_effort.or(self.review_effort),
            global_coder: other.global_coder.or(self.global_coder),
        }
    }

    /// Overlay CLI flags onto the accumulated inputs. Per-role flags win
    /// over globals; `global_model` and `global_effort` expand to every role
    /// that has no explicit per-role value.
    pub fn merge_cli(
        mut self,
        write_coder: Option<String>,
        write_model: Option<String>,
        review_coder: Option<String>,
        review_model: Option<String>,
        global_coder: Option<String>,
        global_model: Option<String>,
        write_effort: Option<String>,
        review_effort: Option<String>,
        global_effort: Option<String>,
    ) -> Self {
        let has_write_model = write_model.is_some();
        let has_review_model = review_model.is_some();
        let has_write_effort = write_effort.is_some();
        let has_review_effort = review_effort.is_some();

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
        if global_coder.is_some() {
            self.global_coder = global_coder;
        }
        if let Some(ref m) = global_model {
            if !has_write_model {
                self.write_model = Some(m.clone());
            }
            if !has_review_model {
                self.review_model = Some(m.clone());
            }
        }
        if has_write_effort {
            self.write_effort = write_effort;
        }
        if has_review_effort {
            self.review_effort = review_effort;
        }
        if let Some(ref e) = global_effort {
            if !has_write_effort {
                self.write_effort = Some(e.clone());
            }
            if !has_review_effort {
                self.review_effort = Some(e.clone());
            }
        }
        self
    }
}

/// Apply explicit run-time mapping fields to a freshly locked Attempt record.
///
/// A flagless invocation performs a read only. With overrides, the reducer
/// derives the effective mapping from the Attempt read under the Work-model lock
/// and changes only `coder_mapping`, preserving both untouched mapping fields and
/// unrelated concurrent Work state.
pub fn overlay_attempt_coder_mapping(
    store: &WorkModelStore,
    work_item_id: &str,
    attempt_id: &str,
    inputs: &CoderMappingInputs,
) -> Result<CoderMapping, anyhow::Error> {
    if !inputs.has_run_overrides() {
        let item = store.read_work_item(work_item_id)?;
        return item
            .attempts
            .iter()
            .find(|attempt| attempt.id == attempt_id)
            .map(|attempt| attempt.coder_mapping.clone())
            .ok_or_else(|| {
                anyhow::anyhow!("Attempt {attempt_id:?} not found in Work Item {work_item_id:?}")
            });
    }

    let effective = store.mutate_work_item(work_item_id, |item| {
        let attempt = item
            .attempts
            .iter_mut()
            .find(|attempt| attempt.id == attempt_id)
            .ok_or_else(|| WorkModelError::AttemptNotFound {
                id: attempt_id.to_string(),
            })?;
        let effective = attempt.coder_mapping.overlay_run_inputs(inputs);
        if let Ok(mapping) = &effective {
            attempt.coder_mapping = mapping.clone();
        }
        Ok(effective)
    })??;
    Ok(effective)
}

/// Resolve a fully populated writer/reviewer mapping from layered inputs.
///
/// Precedence per role:
/// 1. Per-role CLI flag / env var
/// 2. Global `FLUENT_CODER` / per-Coder model env var
/// 3. Coder's built-in default
pub fn resolve_coder_mapping(inputs: &CoderMappingInputs) -> Result<CoderMapping, anyhow::Error> {
    let global_kind = inputs
        .global_coder
        .as_deref()
        .map(|s| CoderKind::resolve(Some(s)))
        .transpose()?;

    let resolve_pair = |task_coder: &Option<String>,
                        task_model: &Option<String>,
                        task_effort: &Option<String>|
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

        Ok(CoderModelPair {
            coder,
            model,
            effort: task_effort.clone(),
        })
    };

    Ok(CoderMapping {
        write: resolve_pair(
            &inputs.write_coder,
            &inputs.write_model,
            &inputs.write_effort,
        )?,
        review: resolve_pair(
            &inputs.review_coder,
            &inputs.review_model,
            &inputs.review_effort,
        )?,
    })
}

/// One failed correction artifact bound to the exact bytes an operator approved.
///
/// The serialized type name is retained because existing extensions used it for
/// reviewer output; current frontiers may also bind a rejected final-Tester gate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoundCapReviewArtifact {
    pub artifact: ArtifactRef,
    pub digest: String,
}

/// An append-only, bounded increase to a paused Attempt's Writer-round cap.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoundCapExtension {
    pub id: String,
    pub old_cap: usize,
    pub new_cap: usize,
    pub additional_write_rounds: usize,
    /// Evidence-backed Writer allowances already included in `new_cap`.
    #[serde(default, skip_serializing_if = "is_zero_usize")]
    pub evidence_write_rounds: usize,
    /// The configured cap and materialized Writer frontier observed before a
    /// new extension was approved. Together they bind the first extension's
    /// old cap to the authorized pre-extension state instead of trusting a
    /// self-consistent old/new/id tuple.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authorized_base_cap: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pre_extension_write_rounds: Option<usize>,
    pub candidate_commit: String,
    pub failed_reviews: Vec<RoundCapReviewArtifact>,
    /// The operator's exact decision for this candidate and failure frontier.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub human_resolution: String,
    /// Detects manual or partial edits to the stored human decision.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub human_resolution_digest: String,
    pub approved_at: String,
}

impl RoundCapExtension {
    fn matches_request(&self, request: &RoundCapExtensionRequest) -> bool {
        self.old_cap == request.old_cap
            && self.additional_write_rounds == request.additional_write_rounds
            && self.candidate_commit == request.candidate_commit
            && self.failed_reviews == request.failed_reviews
            && self.human_resolution == request.human_resolution.trim()
            && self.human_resolution_digest
                == round_cap_resolution_digest(request.human_resolution.trim())
    }

    pub fn validate_human_resolution(&self) -> Result<(), WorkModelError> {
        // Extensions written before human resolutions became mandatory remain
        // readable. New extensions always persist both fields together.
        if self.human_resolution.is_empty() && self.human_resolution_digest.is_empty() {
            return Ok(());
        }
        let normalized = normalize_round_cap_resolution(&self.human_resolution)?;
        if normalized != self.human_resolution
            || self.human_resolution_digest != round_cap_resolution_digest(&normalized)
        {
            return Err(WorkModelError::RoundCapResolutionInvalid {
                reason: "the stored human resolution no longer matches its approved bytes"
                    .to_string(),
            });
        }
        Ok(())
    }

    /// Prove a legacy approval's Writer count without consulting artifact files
    /// or allowing Tasks created later to explain an earlier cap gap.
    fn historical_write_rounds(&self, tasks: &[Task]) -> Result<usize, WorkModelError> {
        let invalid = |reason: &str| WorkModelError::RoundCapExtensionIneligible {
            reason: format!("cannot establish the historical Writer count: {reason}"),
        };
        let parse_time = |value: Option<&str>| {
            value
                .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
                .ok_or_else(|| invalid("missing or invalid timestamp"))
        };
        let approval = parse_time(Some(&self.approved_at))?;
        let mut previous_creation = None;
        let mut latest_writer = None;
        let mut count = 0;
        for writer in tasks.iter().filter(|task| task.kind == TaskKind::Write) {
            let created = parse_time(writer.created_at.as_deref())?;
            if previous_creation.is_some_and(|previous| previous >= created) {
                return Err(invalid(
                    "Writer creation order is contradictory or ambiguous",
                ));
            }
            previous_creation = Some(created);
            if created == approval {
                return Err(invalid("Writer creation coincides with approval"));
            }
            if created < approval {
                count += 1;
                latest_writer = Some((writer, created));
            }
        }
        let (writer, created) =
            latest_writer.ok_or_else(|| invalid("no Writer predates approval"))?;
        let completed = parse_time(writer.completed_at.as_deref())?;
        if writer.status != TaskStatus::Complete || completed < created || completed > approval {
            return Err(invalid(
                "the last pre-approval Writer was not complete at approval",
            ));
        }
        if writer.output.as_ref().map(|output| output.commit.as_str())
            != Some(self.candidate_commit.as_str())
        {
            return Err(invalid(
                "the completed Writer candidate does not match the approval",
            ));
        }
        Ok(count)
    }

    fn validate_structure(
        &self,
        previous: Option<&RoundCapExtension>,
        current_evidence_write_rounds: usize,
        current_materialized_write_rounds: usize,
        tasks: &[Task],
        require_bound_frontier: bool,
    ) -> Result<(), WorkModelError> {
        if !(MIN_ADDITIONAL_WRITE_ROUNDS..=MAX_ADDITIONAL_WRITE_ROUNDS)
            .contains(&self.additional_write_rounds)
        {
            return Err(WorkModelError::RoundCapExtensionIncrementOutOfRange {
                requested: self.additional_write_rounds,
            });
        }
        let expected_new_cap = self
            .old_cap
            .checked_add(self.additional_write_rounds)
            .ok_or_else(|| WorkModelError::RoundCapExtensionIneligible {
                reason: "the approved cap arithmetic overflows".to_string(),
            })?;
        if self.new_cap != expected_new_cap {
            return Err(WorkModelError::RoundCapExtensionIneligible {
                reason: format!(
                    "new cap {} does not equal old cap {} plus the approved increment {}",
                    self.new_cap, self.old_cap, self.additional_write_rounds
                ),
            });
        }
        if self.id != format!("round-cap-{}-{}", self.old_cap, self.new_cap) {
            return Err(WorkModelError::RoundCapExtensionIneligible {
                reason: "the extension identity does not match its cap bounds".to_string(),
            });
        }
        if self.candidate_commit.trim().is_empty() || self.approved_at.trim().is_empty() {
            return Err(WorkModelError::RoundCapExtensionIneligible {
                reason: "the extension is missing its candidate or approval timestamp".to_string(),
            });
        }
        if self.evidence_write_rounds > current_evidence_write_rounds {
            return Err(WorkModelError::RoundCapExtensionIneligible {
                reason: format!(
                    "the extension records {} evidence Writer rounds, but only {} exist",
                    self.evidence_write_rounds, current_evidence_write_rounds
                ),
            });
        }
        match (self.authorized_base_cap, self.pre_extension_write_rounds) {
            (Some(base_cap), Some(pre_extension_write_rounds)) => {
                if base_cap == 0 {
                    return Err(WorkModelError::RoundCapExtensionIneligible {
                        reason: "the authorized base cap must be positive".to_string(),
                    });
                }
                if pre_extension_write_rounds > current_materialized_write_rounds {
                    return Err(WorkModelError::RoundCapExtensionIneligible {
                        reason: format!(
                            "the extension records {} pre-extension Writer rounds, but only {} are materialized",
                            pre_extension_write_rounds, current_materialized_write_rounds
                        ),
                    });
                }
                if previous.is_none() {
                    let expected_old_cap = base_cap
                        .saturating_add(self.evidence_write_rounds)
                        .max(pre_extension_write_rounds);
                    if self.old_cap != expected_old_cap {
                        return Err(WorkModelError::RoundCapExtensionIneligible {
                            reason: format!(
                                "old cap {} does not match the authorized pre-extension cap {}",
                                self.old_cap, expected_old_cap
                            ),
                        });
                    }
                }
            }
            (None, None) => {}
            _ => {
                return Err(WorkModelError::RoundCapExtensionIneligible {
                    reason: "the authorized pre-extension frontier is incomplete".to_string(),
                });
            }
        }
        if require_bound_frontier {
            let (Some(authorized_base_cap), Some(pre_extension_write_rounds)) =
                (self.authorized_base_cap, self.pre_extension_write_rounds)
            else {
                return Err(WorkModelError::RoundCapExtensionIneligible {
                    reason: "the extension has no independently bound pre-extension frontier"
                        .to_string(),
                });
            };
            if authorized_base_cap > self.old_cap {
                return Err(WorkModelError::RoundCapExtensionIneligible {
                    reason: format!(
                        "the authorized base cap {} exceeds the extension old cap {}",
                        authorized_base_cap, self.old_cap
                    ),
                });
            }
            if pre_extension_write_rounds != self.old_cap {
                return Err(WorkModelError::RoundCapExtensionIneligible {
                    reason: format!(
                        "the extension records {} pre-extension Writer rounds, but its old cap is {}",
                        pre_extension_write_rounds, self.old_cap
                    ),
                });
            }
        }
        if let Some(previous) = previous {
            if let (Some(previous_base_cap), Some(base_cap)) =
                (previous.authorized_base_cap, self.authorized_base_cap)
                && previous_base_cap != base_cap
            {
                return Err(WorkModelError::RoundCapExtensionIneligible {
                    reason: "the extension chain changes its authorized base cap".to_string(),
                });
            }
            let evidence_delta = self
                .evidence_write_rounds
                .checked_sub(previous.evidence_write_rounds)
                .ok_or_else(|| WorkModelError::RoundCapExtensionIneligible {
                    reason: "evidence Writer rounds moved backward between extensions".to_string(),
                })?;
            let mut expected_old_cap =
                previous
                    .new_cap
                    .checked_add(evidence_delta)
                    .ok_or_else(|| WorkModelError::RoundCapExtensionIneligible {
                        reason: "the extension chain cap arithmetic overflows".to_string(),
                    })?;
            // Current approvals bind their Writer count explicitly. Only a
            // legacy gap needs reconstruction; contiguous legacy entries may
            // predate the Task timestamps required to establish that count.
            if let Some(writers) = self.pre_extension_write_rounds {
                expected_old_cap = expected_old_cap.max(writers);
            } else if self.old_cap > expected_old_cap {
                expected_old_cap = expected_old_cap.max(self.historical_write_rounds(tasks)?);
            }
            if self.old_cap != expected_old_cap {
                return Err(WorkModelError::RoundCapExtensionIneligible {
                    reason: format!(
                        "old cap {} does not continue the preceding cap {} and evidence delta {} with its historical Writer count",
                        self.old_cap, previous.new_cap, evidence_delta
                    ),
                });
            }
        } else if self.old_cap < self.evidence_write_rounds {
            return Err(WorkModelError::RoundCapExtensionIneligible {
                reason: "the old cap does not include the recorded evidence Writer rounds"
                    .to_string(),
            });
        }
        self.validate_human_resolution()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoundCapExtensionRequest {
    pub old_cap: usize,
    pub additional_write_rounds: usize,
    pub candidate_commit: String,
    pub failed_reviews: Vec<RoundCapReviewArtifact>,
    pub human_resolution: String,
}

pub fn normalize_round_cap_resolution(resolution: &str) -> Result<String, WorkModelError> {
    let normalized = resolution.trim();
    if normalized.is_empty() {
        return Err(WorkModelError::RoundCapResolutionInvalid {
            reason: "provide a non-empty --resolution for the corrective Writer".to_string(),
        });
    }
    if normalized.len() > MAX_ROUND_CAP_RESOLUTION_BYTES {
        return Err(WorkModelError::RoundCapResolutionInvalid {
            reason: format!(
                "the human resolution is {} bytes; limit is {} bytes",
                normalized.len(),
                MAX_ROUND_CAP_RESOLUTION_BYTES
            ),
        });
    }
    Ok(normalized.to_string())
}

pub fn round_cap_resolution_digest(resolution: &str) -> String {
    format!("sha256:{:x}", Sha256::digest(resolution.as_bytes()))
}

/// One execution history branch for a work item.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TerminalMode {
    #[default]
    Headless,
    Native,
}

impl TerminalMode {
    pub fn is_headless(&self) -> bool {
        *self == Self::Headless
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attempt {
    pub id: String,
    pub work_item_id: String,
    #[serde(default, skip_serializing_if = "attempt_kind_is_write")]
    pub kind: AttemptKind,
    pub status: AttemptStatus,
    #[serde(default)]
    pub coder_mapping: CoderMapping,
    /// Keep the resolved execution route through retries and continuation.
    #[serde(default, skip_serializing_if = "TerminalMode::is_headless")]
    pub terminal_mode: TerminalMode,
    #[serde(default)]
    pub tasks: Vec<Task>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub review_state: Option<AttemptReviewState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pause_kind: Option<PauseKind>,
    /// The exact exceptional recovery authorized by the current pause. Keep this
    /// separate from `pause_kind`: two Tester harness failures can share the same
    /// classification while only one requires a dispatch-scoped sandbox approval.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pause_recovery: Option<PauseRecovery>,
    /// One host-owned description of why this Attempt is waiting and the exact
    /// operation that is currently safe to try. Older records may omit it; the
    /// store fills it when a paused Attempt is written.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pause_record: Option<PauseRecord>,
    #[serde(default)]
    pub artifacts: Vec<ArtifactRef>,
    /// Append-only host-owned recovery frontiers for an unchanged candidate.
    /// Historical failed Writer Tasks remain untouched; this record explains the
    /// exceptional evidence-only route that supersedes them operationally.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_recoveries: Vec<EvidenceRecovery>,
    /// Append-only records for public recovery operations. The record binds
    /// one authorization and its immutable inputs to the resulting Attempt
    /// state, so a repeated request can be recognized without guessing from
    /// task names or artifact files.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recovery_operations: Vec<RecoveryOperationRecord>,
    /// Failed Reviewer launches retained as history but explicitly superseded
    /// by an audited Approach-revision corrective Writer.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub approach_revision_superseded_tasks: Vec<ApproachRevisionSupersession>,
    /// Append-only host-derived Writer outcomes and provider continuation
    /// identities. Legacy Attempts deserialize with no records.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub writer_runs: Vec<WriterRun>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancellation: Option<AttemptCancellation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
    /// Durable, retryable Learner state. Absent until the Learner first runs on
    /// this code-producing Attempt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub learning: Option<AttemptLearning>,
    /// The durable progress contract: the schema version and immutable expected
    /// required-progress manifest derived from the Plan's required rows. A new
    /// planned Attempt with required Plan rows is stamped with it; a legacy Attempt
    /// deserializes without it and stays compatible with the advancement gate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress_contract: Option<ProgressContract>,
    /// Immutable approved planning requirements used to generate and reconcile
    /// one task-specific Writer completion matrix per write round.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub writer_completion_contract: Option<WriterCompletionContract>,
    /// Explicit approvals for legitimate write-round-cap pauses.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub round_cap_extensions: Vec<RoundCapExtension>,
}

/// Persist the operator's stop request before settling any local execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttemptCancellation {
    pub requested_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settled_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resumed_at: Option<String>,
}

/// Bind one preserved failed Reviewer launch to its audited Approach revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApproachRevisionSupersession {
    pub failed_task_id: String,
    pub audit_artifact: ArtifactRef,
    pub source_review_task_id: String,
    pub candidate_commit: String,
    pub corrective_writer_task_id: String,
}

impl Default for Attempt {
    fn default() -> Self {
        Self {
            id: String::new(),
            work_item_id: String::new(),
            kind: AttemptKind::default(),
            status: AttemptStatus::Planned,
            coder_mapping: CoderMapping::default(),
            terminal_mode: TerminalMode::Headless,
            tasks: Vec::new(),
            review_state: None,
            pause_kind: None,
            pause_recovery: None,
            pause_record: None,
            artifacts: Vec::new(),
            evidence_recoveries: Vec::new(),
            recovery_operations: Vec::new(),
            approach_revision_superseded_tasks: Vec::new(),
            cancellation: None,
            writer_runs: Vec::new(),
            created_at: None,
            completed_at: None,
            learning: None,
            progress_contract: None,
            writer_completion_contract: None,
            round_cap_extensions: Vec::new(),
        }
    }
}

impl Attempt {
    pub fn cancellation_requested(&self) -> bool {
        self.cancellation
            .as_ref()
            .is_some_and(|stop| stop.resumed_at.is_none())
    }

    /// A fresh operator recovery may resume a fully settled stop. Workers never
    /// call this while ingesting scheduled verification results.
    pub(crate) fn resume_cancellation(&mut self) -> Result<(), WorkModelError> {
        if self.cancellation_requested() {
            let stop = self.cancellation.as_mut().unwrap();
            if stop.settled_at.is_none() {
                return Err(WorkModelError::AttemptResumeIneligible {
                    attempt_id: self.id.clone(),
                    reason: "local cancellation has not settled".into(),
                });
            }
            stop.resumed_at = Some(now_iso8601());
        }
        Ok(())
    }

    pub fn ensure_not_canceled(&self) -> Result<(), WorkModelError> {
        if self.cancellation_requested() {
            return Err(WorkModelError::AttemptResumeIneligible {
                attempt_id: self.id.clone(),
                reason:
                    "Attempt has a durable cancellation request; explicitly resume after settlement"
                        .into(),
            });
        }
        Ok(())
    }

    /// Return the latest completed Writer commit that this Attempt currently
    /// treats as its candidate.
    pub fn current_candidate_commit(&self) -> Option<&str> {
        self.tasks
            .iter()
            .rev()
            .find(|task| {
                task.kind == TaskKind::Write
                    && task.status == TaskStatus::Complete
                    && task.output.is_some()
            })
            .and_then(|task| task.output.as_ref().map(|output| output.commit.as_str()))
    }

    /// Return a previously recorded recovery operation by its stable identity.
    pub fn recovery_operation(&self, operation_id: &str) -> Option<&RecoveryOperationRecord> {
        self.recovery_operations
            .iter()
            .find(|record| record.operation_id == operation_id)
    }

    /// Append one recovery result, or return the identical prior result when a
    /// caller repeats the same request. A reused operation identity with
    /// different inputs is rejected rather than silently merged.
    pub fn record_recovery_operation(
        &mut self,
        request: &RecoveryOperationRequest,
        next_action: Option<String>,
    ) -> Result<RecoveryOperationRecord, WorkModelError> {
        request.validate_shape(&self.id)?;
        let operation_id = request.operation_id();
        if let Some(existing) = self.recovery_operation(&operation_id) {
            if existing.matches_request(request) {
                return Ok(existing.clone());
            }
            return Err(WorkModelError::RecoveryOperationConflict {
                attempt_id: self.id.clone(),
                operation_id,
            });
        }
        let record = RecoveryOperationRecord::from_request(request, self, next_action);
        self.recovery_operations.push(record.clone());
        Ok(record)
    }

    /// Return whether an audited Approach revision explicitly superseded this
    /// failed Reviewer launch while preserving its terminal Task outcome.
    pub fn is_approach_revision_superseded_task(&self, task_id: &str) -> bool {
        self.approach_revision_superseded_tasks
            .iter()
            .any(|superseded| {
                if superseded.failed_task_id != task_id
                    || !self.artifacts.contains(&superseded.audit_artifact)
                {
                    return false;
                }
                let Some(failed) = self.tasks.iter().find(|task| task.id == task_id) else {
                    return false;
                };
                let Some(source) = self
                    .tasks
                    .iter()
                    .find(|task| task.id == superseded.source_review_task_id)
                else {
                    return false;
                };
                let Some(writer) = self
                    .tasks
                    .iter()
                    .find(|task| task.id == superseded.corrective_writer_task_id)
                else {
                    return false;
                };
                failed.kind == TaskKind::Review
                    && failed.status == TaskStatus::Failed
                    && failed.review_context.as_ref().is_some_and(|context| {
                        context.candidate_commit == superseded.candidate_commit
                    })
                    && source.kind == TaskKind::Review
                    && source.role == "architecture"
                    && source.status == TaskStatus::Complete
                    && source.review_context.as_ref().is_some_and(|context| {
                        context.candidate_commit == superseded.candidate_commit
                    })
                    && writer.kind == TaskKind::Write
                    && writer.role == "approach-revision-correction"
                    && writer.input_artifacts.contains(&superseded.audit_artifact)
            })
    }

    pub fn all_tasks_complete_or_approach_superseded(&self) -> bool {
        self.tasks.iter().all(|task| {
            task.status == TaskStatus::Complete
                || (task.status == TaskStatus::Failed
                    && self.is_approach_revision_superseded_task(&task.id))
        })
    }
    /// Return the failed Tester superseded by one explicit harness-repair
    /// Writer. The Writer's immutable input binds it to that Tester Task, so a
    /// generic failed Task can never be ignored by merely using the role name.
    pub fn active_tester_harness_repair_source(&self) -> Option<&Task> {
        let repair = self.tasks.iter().rev().find(|task| {
            task.kind == TaskKind::Write
                && task.role == TESTER_HARNESS_REPAIR_ROLE
                && !matches!(task.status, TaskStatus::Failed)
        })?;
        let mut sources = self.tasks.iter().filter(|task| {
            if task.kind != TaskKind::Tester || task.status != TaskStatus::Failed {
                return false;
            }
            let Some(area) = task.artifact_area.as_ref() else {
                return false;
            };
            repair.input_artifacts.iter().any(|artifact| {
                artifact.producer_id == task.id
                    && ["tester-results.json", "commands.json"]
                        .iter()
                        .any(|name| artifact.path == format!("{}/{name}", area.path))
            })
        });
        let source = sources.next()?;
        sources.next().is_none().then_some(source)
    }

    pub fn has_tester_harness_repair_for(&self, task_id: &str) -> bool {
        self.active_tester_harness_repair_source()
            .is_some_and(|task| task.id == task_id)
    }

    /// Return whether a failed ordinary Writer was planned before an earlier
    /// Tester-harness repair finished. This ordering cannot represent a valid
    /// post-repair correction: it is a stale continuation created while the
    /// repair Writer was still unresolved.
    pub fn is_superseded_tester_harness_followup(&self, task_id: &str) -> bool {
        let Some((task_index, task)) = self
            .tasks
            .iter()
            .enumerate()
            .find(|(_, task)| task.id == task_id)
        else {
            return false;
        };
        if task.kind != TaskKind::Write
            || task.role == TESTER_HARNESS_REPAIR_ROLE
            || task.status != TaskStatus::Failed
        {
            return false;
        }
        let Some(created_at) = task.created_at.as_deref() else {
            return false;
        };
        let Ok(created_at) = chrono::DateTime::parse_from_rfc3339(created_at) else {
            return false;
        };
        self.tasks[..task_index].iter().rev().any(|repair| {
            let completed_before_creation = repair
                .completed_at
                .as_deref()
                .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
                .is_some_and(|completed_at| created_at <= completed_at);
            let has_receipt = self.artifacts.iter().any(|artifact| {
                artifact.producer_id == "tester-harness-recovery"
                    && artifact.path.ends_with(&format!(
                        "/tester-harness-repairs/{}/superseded-followups.json",
                        repair.id
                    ))
            });
            repair.kind == TaskKind::Write
                && repair.role == TESTER_HARNESS_REPAIR_ROLE
                && repair.status == TaskStatus::Complete
                && repair.output.is_some()
                && completed_before_creation
                && has_receipt
        })
    }

    pub fn materialized_write_rounds(&self) -> usize {
        self.tasks
            .iter()
            .filter(|task| {
                task.kind == TaskKind::Write
                    && task.status != TaskStatus::Superseded
                    && !self.is_superseded_tester_harness_followup(&task.id)
            })
            .count()
    }

    /// Return the latest Writer that remains part of the accepted execution
    /// history. Reconciled stale follow-ups stay in the audit trail but cannot
    /// become the candidate frontier again.
    pub fn latest_materialized_write_index(&self) -> Option<usize> {
        self.tasks
            .iter()
            .enumerate()
            .rev()
            .find_map(|(index, task)| {
                (task.kind == TaskKind::Write
                    && task.status != TaskStatus::Superseded
                    && !self.is_superseded_tester_harness_followup(&task.id))
                .then_some(index)
            })
    }

    /// Return whether live work or a hard failure prevents a round-cap
    /// approval. Failed Tasks already superseded by an accepted Tester repair
    /// remain as audit history and do not block that recovery.
    pub fn has_round_cap_task_blocker(&self) -> bool {
        self.tasks.iter().any(|task| {
            matches!(task.status, TaskStatus::Planned | TaskStatus::Executing)
                || (task.status == TaskStatus::Failed
                    && !self.is_superseded_tester_harness_followup(&task.id)
                    && !self.has_tester_harness_repair_for(&task.id)
                    && !self.is_approach_revision_superseded_task(&task.id))
        })
    }

    /// Return whether this Attempt carries the durable receipt produced when
    /// an accepted Tester-harness repair superseded stale Writer follow-ups.
    /// This is the exact recovery shape whose older scheduler dispatch may
    /// already be recorded as blocked.
    pub fn has_reconciled_tester_harness_repair(&self) -> bool {
        self.status == AttemptStatus::NeedsUser
            && self.pause_kind == Some(PauseKind::TesterHarness)
            && self.tasks.iter().any(|repair| {
                repair.kind == TaskKind::Write
                    && repair.role == TESTER_HARNESS_REPAIR_ROLE
                    && repair.status == TaskStatus::Complete
                    && repair.output.is_some()
                    && self.artifacts.iter().any(|artifact| {
                        artifact.producer_id == "tester-harness-recovery"
                            && artifact.path.ends_with(&format!(
                                "/tester-harness-repairs/{}/superseded-followups.json",
                                repair.id
                            ))
                    })
            })
    }

    /// Validate that this exact suspended Attempt can be explicitly resumed.
    ///
    /// Both direct Attempt execution and scheduler resume dispatches use this
    /// predicate so queue admission cannot drift from the Attempt loop's
    /// supported pause vocabulary. Checks that depend on live Git or artifact
    /// bytes remain in the Attempt loop and run immediately before reopening.
    pub fn ensure_resume_ready(&self) -> Result<(), WorkModelError> {
        let ineligible = |reason: String| {
            let reason = match self
                .pause_record
                .as_ref()
                .filter(|record| record.matches_attempt(self))
            {
                Some(record) => match (&record.next_operation, &record.no_safe_operation_reason) {
                    (Some(operation), _) => {
                        format!("{reason} Recorded next operation: `{}`.", operation.command)
                    }
                    (None, Some(no_safe)) => {
                        format!("{reason} No safe operation is currently recorded: {no_safe}")
                    }
                    _ => reason,
                },
                None => reason,
            };
            WorkModelError::AttemptResumeIneligible {
                attempt_id: self.id.clone(),
                reason,
            }
        };
        let failed_candidate_gate_correction = self.is_failed_candidate_gate_correction();
        let pending_evidence_reviews = self.has_pending_evidence_review_tasks();
        let pending_code_change_classification = self.has_pending_code_change_classification();
        let replayable_code_change_classification =
            self.has_replayable_code_change_classification();

        if self.status != AttemptStatus::NeedsUser
            && !failed_candidate_gate_correction
            && !pending_evidence_reviews
            && !pending_code_change_classification
        {
            return Err(ineligible(format!(
                "Attempt is {} and is not paused at needs-user",
                self.status.as_str()
            )));
        }
        if self.tasks.iter().any(|task| {
            task.status == TaskStatus::Executing
                || (task.status == TaskStatus::Failed
                    && !failed_candidate_gate_correction
                    && !((pending_code_change_classification
                        || replayable_code_change_classification)
                        && task.kind == TaskKind::Write
                        && task.output.is_none())
                    && !self.is_superseded_tester_harness_followup(&task.id)
                    && !self.has_tester_harness_repair_for(&task.id)
                    && !self.is_approach_revision_superseded_task(&task.id))
        }) {
            return Err(ineligible(
                "Attempt paused on recoverable infrastructure or completion evidence but also has a hard-failed or still-running Task; resolve it before re-running."
                    .to_string(),
            ));
        }
        if failed_candidate_gate_correction {
            return Ok(());
        }
        if pending_evidence_reviews {
            return Ok(());
        }
        if pending_code_change_classification {
            return Ok(());
        }

        match self.pause_kind.as_ref() {
            Some(PauseKind::Auth)
            | Some(PauseKind::TranscriptPump)
            | Some(PauseKind::HostSandbox)
            | Some(PauseKind::TesterHarness)
            | Some(PauseKind::ProviderUnavailable)
            | Some(PauseKind::Interrupted)
            | Some(PauseKind::WriterCompletion)
            | Some(PauseKind::CandidateGate) => Ok(()),
            Some(PauseKind::Uncertain) if self.is_legacy_candidate_gate_pause() => Ok(()),
            Some(PauseKind::Uncertain) if self.is_legacy_writer_completion_pause() => Ok(()),
            Some(PauseKind::Uncertain) if self.is_uncertain_review_pause() => {
                let completed_writer = self.tasks.iter().rposition(|task| {
                    task.kind == TaskKind::Write
                        && task.status == TaskStatus::Complete
                        && task.output.is_some()
                });
                let has_completed_review = completed_writer.is_some_and(|write_index| {
                    self.tasks[write_index + 1..].iter().any(|task| {
                        task.kind == TaskKind::Review && task.status == TaskStatus::Complete
                    })
                });
                if self
                    .tasks
                    .iter()
                    .all(|task| task.status == TaskStatus::Complete)
                    && has_completed_review
                {
                    Ok(())
                } else {
                    Err(ineligible(
                        "Attempt does not have a settled Writer and reviewer frontier for uncertain-review recovery."
                            .to_string(),
                    ))
                }
            }
            Some(PauseKind::Uncertain) => Err(ineligible(
                "Attempt has an unsupported uncertain pause shape; resuming it is not yet supported."
                    .to_string(),
            )),
            Some(PauseKind::HostEvidence) => Err(ineligible(
                "Attempt is waiting for candidate-bound host evidence. Attach the requested evidence before resuming."
                    .to_string(),
            )),
            Some(PauseKind::RoundCap) => {
                let write_rounds = self.materialized_write_rounds();
                if self.review_state == Some(AttemptReviewState::Failed)
                    && self
                        .round_cap_extensions
                        .last()
                        .is_some_and(|extension| extension.new_cap > write_rounds)
                {
                    self.validate_round_cap_extensions()?;
                    Ok(())
                } else {
                    Err(ineligible(
                        "Attempt is paused at the write-round cap. Approve an extension before re-running."
                            .to_string(),
                    ))
                }
            }
            Some(PauseKind::Unknown(value)) => Err(ineligible(format!(
                "Attempt is paused with unknown pause kind {value:?}; upgrade Fluent before mutating this Work Item."
            ))),
            None => Err(ineligible(
                "Attempt needs user input before it can advance".to_string(),
            )),
        }
    }

    fn is_legacy_writer_completion_pause(&self) -> bool {
        self.pause_kind == Some(PauseKind::Uncertain)
            && self.review_state == Some(AttemptReviewState::NotReviewed)
            && self.artifacts.iter().any(|artifact| {
                artifact.producer_id == "required-progress"
                    && artifact.path.ends_with("/writer-completion-needs-user.md")
            })
    }

    pub(crate) fn is_uncertain_review_pause(&self) -> bool {
        self.status == AttemptStatus::NeedsUser
            && self.pause_kind == Some(PauseKind::Uncertain)
            && self.review_state == Some(AttemptReviewState::Uncertain)
    }

    pub(crate) fn is_candidate_gate_pause(&self) -> bool {
        self.pause_kind == Some(PauseKind::CandidateGate)
            || self.is_legacy_candidate_gate_pause()
            || self.is_failed_candidate_gate_correction()
    }

    /// Return whether an older binary hard-failed the exact corrective Writer
    /// that a candidate-gate resume had planned. This narrow shape is safe to
    /// retry because the Task has no output, is the final Task, and still names
    /// the rejected gate produced by an earlier completed Writer.
    pub(crate) fn is_failed_candidate_gate_correction(&self) -> bool {
        if self.status != AttemptStatus::Failed
            || self.pause_kind.is_some()
            || self.review_state != Some(AttemptReviewState::Failed)
        {
            return false;
        }
        let Some(failed) = self.tasks.last() else {
            return false;
        };
        self.tasks[..self.tasks.len() - 1].iter().all(|task| {
            !matches!(
                task.status,
                TaskStatus::Failed | TaskStatus::Executing | TaskStatus::NeedsUser
            )
        }) && failed.kind == TaskKind::Write
            && failed.status == TaskStatus::Failed
            && failed.output.is_none()
            && failed.input_artifacts.iter().any(|artifact| {
                artifact
                    .path
                    .ends_with(crate::candidate_gate::CANDIDATE_GATE_FILE)
                    && self.artifacts.contains(artifact)
                    && self.tasks.iter().any(|task| {
                        task.id == artifact.producer_id
                            && task.kind == TaskKind::Write
                            && task.status == TaskStatus::Complete
                            && task.output.is_some()
                    })
            })
    }

    fn is_legacy_candidate_gate_pause(&self) -> bool {
        self.pause_kind == Some(PauseKind::Uncertain)
            && self.review_state == Some(AttemptReviewState::NotReviewed)
            && self.artifacts.iter().any(|artifact| {
                artifact.producer_id == "candidate-gate"
                    && artifact.path.ends_with("/candidate-gate-needs-user.md")
            })
            && self.artifacts.iter().any(|artifact| {
                artifact
                    .path
                    .ends_with(crate::candidate_gate::CANDIDATE_GATE_FILE)
            })
    }

    pub fn effective_write_round_cap(&self, base_cap: usize) -> usize {
        let evidence_corrections = self.evidence_write_rounds();
        let configured_cap = self.round_cap_extensions.last().map_or_else(
            || base_cap.saturating_add(evidence_corrections),
            |extension| {
                extension.new_cap.saturating_add(
                    evidence_corrections.saturating_sub(extension.evidence_write_rounds),
                )
            },
        );
        let materialized_write_rounds = self.materialized_write_rounds();
        configured_cap.max(materialized_write_rounds)
    }

    fn evidence_write_rounds(&self) -> usize {
        self.evidence_recoveries
            .iter()
            .filter(|recovery| {
                recovery.state == EvidenceRecoveryState::CodeChange
                    && recovery.targets.iter().any(|target| {
                        target
                            .review_task_id
                            .as_ref()
                            .is_some_and(|review_task_id| {
                                self.tasks.iter().any(|task| {
                                    task.kind == TaskKind::Write
                                        && task
                                            .input_artifacts
                                            .iter()
                                            .any(|artifact| artifact.producer_id == *review_task_id)
                                })
                            })
                    })
            })
            .count()
    }

    pub(crate) fn validate_round_cap_extensions(&self) -> Result<(), WorkModelError> {
        self.validate_round_cap_extensions_with_bound_frontier(false)
    }

    pub(crate) fn validate_round_cap_extensions_for_approach_revision(
        &self,
    ) -> Result<(), WorkModelError> {
        self.validate_round_cap_extensions_with_bound_frontier(true)
    }

    fn validate_round_cap_extensions_with_bound_frontier(
        &self,
        require_bound_frontier: bool,
    ) -> Result<(), WorkModelError> {
        let current_evidence_write_rounds = self.evidence_write_rounds();
        let current_materialized_write_rounds = self.materialized_write_rounds();
        let mut previous = None;
        for extension in &self.round_cap_extensions {
            extension
                .validate_structure(
                    previous,
                    current_evidence_write_rounds,
                    current_materialized_write_rounds,
                    &self.tasks,
                    require_bound_frontier,
                )
                .map_err(|error| match error {
                    WorkModelError::RoundCapExtensionIneligible { reason } => {
                        WorkModelError::RoundCapExtensionIneligible {
                            reason: format!("extension {}: {reason}", extension.id),
                        }
                    }
                    error => error,
                })?;
            previous = Some(extension);
        }
        Ok(())
    }

    pub fn record_round_cap_extension(
        &mut self,
        base_cap: usize,
        request: RoundCapExtensionRequest,
    ) -> Result<&RoundCapExtension, WorkModelError> {
        self.validate_round_cap_extensions()?;
        let human_resolution = normalize_round_cap_resolution(&request.human_resolution)?;
        if !(MIN_ADDITIONAL_WRITE_ROUNDS..=MAX_ADDITIONAL_WRITE_ROUNDS)
            .contains(&request.additional_write_rounds)
        {
            return Err(WorkModelError::RoundCapExtensionIncrementOutOfRange {
                requested: request.additional_write_rounds,
            });
        }
        if let Some(index) = self
            .round_cap_extensions
            .iter()
            .position(|extension| extension.matches_request(&request))
        {
            return Ok(&self.round_cap_extensions[index]);
        }
        let effective_cap = self.effective_write_round_cap(base_cap);
        if request.old_cap != effective_cap {
            return Err(WorkModelError::RoundCapExtensionStaleCap {
                expected: effective_cap,
                actual: request.old_cap,
            });
        }
        let new_cap = effective_cap.saturating_add(request.additional_write_rounds);
        let evidence_write_rounds = self.evidence_write_rounds();
        let pre_extension_write_rounds = self.materialized_write_rounds();
        let authorized_base_cap = self
            .round_cap_extensions
            .first()
            .and_then(|extension| extension.authorized_base_cap)
            .unwrap_or(base_cap);
        self.round_cap_extensions.push(RoundCapExtension {
            id: format!("round-cap-{effective_cap}-{new_cap}"),
            old_cap: effective_cap,
            new_cap,
            additional_write_rounds: request.additional_write_rounds,
            evidence_write_rounds,
            authorized_base_cap: Some(authorized_base_cap),
            pre_extension_write_rounds: Some(pre_extension_write_rounds),
            candidate_commit: request.candidate_commit,
            failed_reviews: request.failed_reviews,
            human_resolution_digest: round_cap_resolution_digest(&human_resolution),
            human_resolution,
            approved_at: now_iso8601(),
        });
        Ok(self
            .round_cap_extensions
            .last()
            .expect("extension appended"))
    }

    /// Return whether this is the narrowly recoverable legacy failure shape.
    /// The caller separately verifies the completed Writer's workspace is clean
    /// and still points at this candidate commit.
    pub fn is_evidence_only_recoverable(
        &self,
        candidate_commit: &str,
        review_artifacts: &[String],
    ) -> bool {
        if self.status != AttemptStatus::Failed || review_artifacts.is_empty() {
            return false;
        }
        let Some((completed_index, completed_writer)) =
            self.tasks.iter().enumerate().rev().find(|(_, task)| {
                task.kind == TaskKind::Write && task.status == TaskStatus::Complete
            })
        else {
            return false;
        };
        if completed_writer
            .output
            .as_ref()
            .is_none_or(|output| output.commit != candidate_commit)
        {
            return false;
        }
        let Some((failed_index, failed_writer)) = self
            .tasks
            .iter()
            .enumerate()
            .rev()
            .find(|(_, task)| task.kind == TaskKind::Write)
        else {
            return false;
        };
        failed_writer.status == TaskStatus::Failed
            && failed_writer.output.is_none()
            && failed_index > completed_index
            && failed_index + 1 == self.tasks.len()
            && failed_writer
                .input_artifacts
                .iter()
                .map(|input| &input.path)
                .collect::<std::collections::HashSet<_>>()
                == review_artifacts.iter().collect()
    }

    /// Record one immutable host-evidence recovery, or return the existing
    /// recovery when the same candidate, attachment, and frontier were accepted.
    /// Completed evidence bytes may support a later distinct frontier; its
    /// recovery keeps a separate identity and review history.
    pub fn attach_evidence_recovery(
        &mut self,
        recovery: EvidenceRecovery,
    ) -> Result<EvidenceRecovery, WorkModelError> {
        if let Some(existing) = self.evidence_recoveries.iter().find(|existing| {
            existing.candidate_commit == recovery.candidate_commit
                && existing.attachment == recovery.attachment
                && existing.targets.len() == recovery.targets.len()
                && existing.targets.iter().all(|target| {
                    recovery.targets.iter().any(|candidate| {
                        candidate.role == target.role
                            && candidate.prior_review_artifact == target.prior_review_artifact
                    })
                })
        }) {
            return Ok(existing.clone());
        }
        if self
            .evidence_recoveries
            .iter()
            .any(|existing| existing.id == recovery.id || existing.is_active())
        {
            return Err(WorkModelError::AttemptEvidenceRecoveryConflict {
                attempt_id: self.id.clone(),
            });
        }
        self.evidence_recoveries.push(recovery.clone());
        Ok(recovery)
    }

    pub fn active_evidence_recovery(&self) -> Option<&EvidenceRecovery> {
        self.evidence_recoveries
            .iter()
            .rev()
            .find(|recovery| recovery.is_active())
    }

    /// Return whether host evidence has already moved this Attempt into one
    /// exact, scheduler-resumable targeted review frontier.
    ///
    /// A completed targeted Review remains pending here until the Attempt loop
    /// consumes its verdict. Scheduler dispatches can become blocked while the
    /// provider is still running, so the next exact queue resume must be able
    /// to settle that immutable completed Task instead of requiring a direct
    /// Attempt invocation.
    pub fn has_pending_evidence_review_tasks(&self) -> bool {
        let Some(recovery) = self.active_evidence_recovery() else {
            return false;
        };
        if self.status != AttemptStatus::Reviewing
            || self.review_state != Some(AttemptReviewState::NotReviewed)
            || recovery.targets.is_empty()
        {
            return false;
        }
        let target_task_ids = recovery
            .targets
            .iter()
            .filter_map(|target| target.review_task_id.as_deref())
            .collect::<std::collections::HashSet<_>>();
        target_task_ids.len() == recovery.targets.len()
            && recovery.targets.iter().all(|target| {
                target.review_task_id.as_deref().is_some_and(|task_id| {
                    self.tasks.iter().any(|task| {
                        task.id == task_id
                            && task.kind == TaskKind::Review
                            && task.role == target.role
                            && matches!(task.status, TaskStatus::Planned | TaskStatus::Complete)
                            && task
                                .evidence_review_context
                                .as_ref()
                                .is_some_and(|context| {
                                    context.recovery_id == recovery.id
                                        && context.candidate_commit == recovery.candidate_commit
                                        && context.attachment == recovery.attachment
                                        && context.prior_review_artifact
                                            == target.prior_review_artifact
                                })
                    })
                })
            })
            && self.tasks.iter().all(|task| {
                task.status != TaskStatus::Planned || target_task_ids.contains(task.id.as_str())
            })
    }

    /// Return whether this Attempt owns a resumable corrective Writer created
    /// by an accepted host-evidence source classification.
    ///
    /// Queue admission uses this broader shape while recovering an older
    /// paused or interrupted Writer. A hard-failed Writer is not admitted
    /// without an explicit validated retry transition. The replay contract
    /// still validates every candidate and artifact binding before queue
    /// admission mutates anything.
    pub fn has_replayable_code_change_classification(&self) -> bool {
        let Some((recovery_index, recovery)) = self
            .evidence_recoveries
            .iter()
            .enumerate()
            .rev()
            .find(|(_, recovery)| {
                recovery.state == EvidenceRecoveryState::CodeChange
                    && recovery.code_change_classification.is_some()
            })
        else {
            return false;
        };
        if self
            .evidence_recoveries
            .iter()
            .skip(recovery_index + 1)
            .any(|recovery| {
                matches!(
                    recovery.state,
                    EvidenceRecoveryState::Reviewing | EvidenceRecoveryState::NeedsEvidence
                )
            })
        {
            return false;
        }
        let Some(classification) = recovery.code_change_classification.as_ref() else {
            return false;
        };
        let Some(writer) = self
            .tasks
            .iter()
            .find(|task| task.id == classification.writer_task_id)
        else {
            return false;
        };
        matches!(
            writer.status,
            TaskStatus::Planned | TaskStatus::Executing | TaskStatus::NeedsUser
        ) && writer.output.is_none()
    }

    /// Return whether this Attempt owns the planned corrective Writer created
    /// by an accepted host-evidence source classification.
    ///
    /// This narrow model shape lets queue admission resume the Writer after the
    /// classifier intentionally reopens the Attempt from `NeedsUser` to
    /// `Planned`. Legacy classified recoveries may omit the target digest here;
    /// queue admission migrates that field after live candidate and artifact
    /// checks.
    pub fn has_pending_code_change_classification(&self) -> bool {
        if self.status != AttemptStatus::Planned
            || self.pause_kind.is_some()
            || self.pause_recovery.is_some()
            || self.completed_at.is_some()
        {
            return false;
        }
        let Some((recovery_index, recovery)) = self
            .evidence_recoveries
            .iter()
            .enumerate()
            .rev()
            .find(|(_, recovery)| {
                recovery.state == EvidenceRecoveryState::CodeChange
                    && recovery.code_change_classification.is_some()
            })
        else {
            return false;
        };
        if self
            .evidence_recoveries
            .iter()
            .skip(recovery_index + 1)
            .any(|recovery| {
                matches!(
                    recovery.state,
                    EvidenceRecoveryState::Reviewing | EvidenceRecoveryState::NeedsEvidence
                )
            })
        {
            return false;
        }
        let Some(classification) = recovery.code_change_classification.as_ref() else {
            return false;
        };
        let Some((writer_index, writer)) = self.tasks.iter().enumerate().find(|(_, task)| {
            task.id == classification.writer_task_id
                && task.kind == TaskKind::Write
                && task.status == TaskStatus::Planned
                && task.output.is_none()
        }) else {
            return false;
        };
        if writer_index + 1 != self.tasks.len() {
            return false;
        }
        let Some(target) = recovery.targets.iter().find(|target| {
            target.role == "tests"
                && target.review_task_id.is_some()
                && (target.review_digest.as_deref() == Some(classification.review_digest.as_str())
                    || target.review_digest.is_none())
                && self.tasks.iter().any(|task| {
                    target.review_task_id.as_deref() == Some(task.id.as_str())
                        && task.kind == TaskKind::Review
                        && task.role == "tests"
                        && task.status == TaskStatus::Complete
                        && task.artifact_area.as_ref().is_some_and(|area| {
                            format!("{}/review.md", area.path) == classification.review_artifact
                        })
                        && task
                            .evidence_review_context
                            .as_ref()
                            .is_some_and(|context| {
                                context.recovery_id == recovery.id
                                    && context.candidate_commit == recovery.candidate_commit
                                    && context.attachment == recovery.attachment
                                    && context.prior_review_artifact == target.prior_review_artifact
                            })
                })
        }) else {
            return false;
        };
        let expected_review = ArtifactRef {
            producer_id: target.review_task_id.clone().unwrap_or_default(),
            path: classification.review_artifact.clone(),
        };
        let expected_evidence = ArtifactRef {
            producer_id: "host-evidence".to_string(),
            path: recovery.attachment.snapshot_path.clone(),
        };
        let exact_input = |expected: &ArtifactRef| {
            writer
                .input_artifacts
                .iter()
                .filter(|input| *input == expected)
                .count()
                == 1
                && !writer
                    .input_artifacts
                    .iter()
                    .any(|input| input.path == expected.path && input != expected)
        };
        self.artifacts.contains(&classification.audit_artifact)
            && exact_input(&expected_review)
            && exact_input(&expected_evidence)
            && exact_input(&classification.audit_artifact)
    }

    /// Materialize the authoritative `## Required completion` section into the
    /// current required-progress content when this Attempt carries a progress
    /// contract and the section is not already present. Idempotent: setup preserves
    /// an existing section; the completed-Writer gate separately owns matrix-backed
    /// projection. Returns the content to persist, or `None` when nothing changes.
    pub fn materialize_required_progress(&self, existing: &str) -> Option<String> {
        let contract = self.progress_contract.as_ref()?;
        if existing.contains(REQUIRED_COMPLETION_HEADING) {
            return None;
        }
        let section = contract.render_required_completion_section();
        let new_content = if existing.trim().is_empty() {
            section
        } else {
            format!("{}\n\n{section}", existing.trim_end())
        };
        Some(new_content)
    }

    /// Reconcile Writer-owned required-progress content against this Attempt's
    /// immutable manifest before a passing review may advance. Matrix-backed
    /// Attempts call this only to verify the host-rendered projection.
    ///
    /// An unmarked (legacy) Attempt has no contract, so this is `Ok(())` without
    /// scanning any prose. For a marked Attempt it reads ONLY the `## Required
    /// completion` section (historical files, banners, and other sections are
    /// ignored) and returns the durable reason a review pass must be rejected: a
    /// missing or malformed section, a set that differs from the manifest (a
    /// deleted, unknown, duplicate, or rewritten entry), or an unchecked or
    /// evidence-less required entry. It is `Ok(())` only when every manifest entry
    /// is checked with evidence and any extra top-level rows are already-checked
    /// legacy `Address review finding:` metadata.
    pub fn reconcile_required_progress(&self, source: &str) -> Result<(), String> {
        let status = self.required_progress_status(source)?;
        match status.incomplete_reason {
            Some(reason) => Err(reason),
            None => Ok(()),
        }
    }

    /// Validate the immutable manifest and count entries that are checked with
    /// evidence. Structural corruption returns an error; ordinary unfinished or
    /// missing work remains a valid incomplete status that a Writer may advance.
    pub fn required_progress_status(&self, source: &str) -> Result<RequiredProgressStatus, String> {
        let Some(contract) = self.progress_contract.as_ref() else {
            return Ok(RequiredProgressStatus {
                checked: 0,
                required: 0,
                incomplete_reason: None,
            });
        };
        let Some(section) = extract_required_completion_section(source) else {
            return Ok(RequiredProgressStatus {
                checked: 0,
                required: contract.required.len(),
                incomplete_reason: Some(format!(
                    "required progress is missing the `{REQUIRED_COMPLETION_HEADING}` section"
                )),
            });
        };

        // Parse every top-level checklist line. Existing Attempts may contain
        // already-checked review follow-ups written by the old prompt; only that
        // exact historical prefix is ignored. Unchecked follow-ups and every other
        // non-conforming line still fail closed through the required-entry parser.
        let mut entries = Vec::new();
        for line in section {
            if !line.starts_with("- ") {
                continue;
            }
            if line.starts_with("- [x] Address review finding:") {
                continue;
            }
            entries.push(parse_required_entry(line)?);
        }

        let manifest: std::collections::HashMap<&str, &str> = contract
            .required
            .iter()
            .map(|entry| (entry.id.as_str(), entry.requirement.as_str()))
            .collect();

        let mut seen = std::collections::HashSet::new();
        let mut checked = 0;
        let mut incomplete_reason = None;
        for entry in &entries {
            if !seen.insert(entry.id.as_str()) {
                return Err(format!("duplicate required-progress entry `{}`", entry.id));
            }
            match manifest.get(entry.id.as_str()) {
                None => {
                    return Err(format!(
                        "unknown required-progress entry `{}` is not in the manifest",
                        entry.id
                    ));
                }
                Some(&requirement) if requirement != entry.requirement => {
                    return Err(format!(
                        "required-progress entry `{}` requirement was rewritten",
                        entry.id
                    ));
                }
                Some(_) => {}
            }
            if entry.checked && !entry.evidence.is_empty() {
                checked += 1;
            } else if incomplete_reason.is_none() {
                incomplete_reason = Some(if !entry.checked {
                    format!("required-progress entry `{}` is unchecked", entry.id)
                } else {
                    format!(
                        "required-progress entry `{}` is checked without evidence",
                        entry.id
                    )
                });
            }
        }

        // Every manifest entry must be present: a deletion cannot manufacture closure.
        for required in &contract.required {
            if !seen.contains(required.id.as_str()) {
                incomplete_reason.get_or_insert_with(|| {
                    format!(
                        "required-progress entry `{}` is missing from the section",
                        required.id
                    )
                });
            }
        }
        Ok(RequiredProgressStatus {
            checked,
            required: contract.required.len(),
            incomplete_reason,
        })
    }

    /// The single advancement-readiness contract. An Attempt's change may advance —
    /// reach `MergeCandidateReady`, pass Merge Candidate landing validation, and
    /// land — only once its Learner run has SUCCEEDED. Any other learning state
    /// (absent, in-progress, prepared/HandoffPending, or failed whether relaunchable
    /// or not) blocks advancement with one durable reason, so no boundary can
    /// advance over a non-succeeded Learner.
    pub fn learning_advancement_readiness(&self) -> Result<(), WorkModelError> {
        let state = match self.learning.as_ref() {
            Some(learning) if learning.is_succeeded() => return Ok(()),
            None => "absent",
            Some(learning) if learning.is_in_progress() => "in progress",
            Some(learning) if learning.is_handoff_pending() => "prepared (handoff pending)",
            Some(learning) if !learning.is_relaunchable() => {
                "failed and non-relaunchable (evidence pending)"
            }
            Some(_) => "failed",
        };
        Err(WorkModelError::AttemptLearningNotSucceeded {
            attempt_id: self.id.clone(),
            state,
        })
    }

    pub fn validate(&self, work_item_id: &str) -> Result<(), WorkModelError> {
        for recovery in &self.evidence_recoveries {
            recovery.validate_carried_finding(self).map_err(|reason| {
                WorkModelError::HostEvidenceRecoveryIneligible {
                    attempt_id: self.id.clone(),
                    reason,
                }
            })?;
        }
        if let Some(contract) = &self.writer_completion_contract {
            contract
                .validate()
                .map_err(|reason| WorkModelError::InvalidPlanningRevision { reason })?;
        }
        if let Some(record) = self.pause_record.as_ref() {
            // Older callers may change the Attempt fields directly before the
            // next persistence boundary. Keep the stale record readable; the
            // status wrapper will not present its operation, and the store will
            // refresh or clear it before writing the next snapshot.
            record
                .validate(&self.id)
                .map_err(|reason| WorkModelError::InvalidPauseRecord {
                    attempt_id: self.id.clone(),
                    reason,
                })?;
        }
        for record in &self.recovery_operations {
            record.validate(work_item_id, &self.id).map_err(|reason| {
                WorkModelError::InvalidRecoveryOperation {
                    attempt_id: self.id.clone(),
                    reason,
                }
            })?;
        }
        self.validate_round_cap_extensions()?;
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
        match self.kind {
            AttemptKind::ReviewOnly => {
                if self.is_review_only_worktree_shape() {
                    self.validate_review_only_worktree_shape(work_item_id)?;
                } else {
                    self.validate_review_only_source_checkout_shape(work_item_id)?;
                }
            }
            AttemptKind::PostMergeReview => {
                self.validate_review_only_worktree_shape(work_item_id)?
            }
            AttemptKind::Write => {}
        }
        for task in &self.tasks {
            task.validate()?;
            if task.kind == TaskKind::Write
                && task.role == TESTER_HARNESS_REPAIR_ROLE
                && task.status != TaskStatus::Failed
                && self.active_tester_harness_repair_source().is_none()
            {
                return Err(WorkModelError::InvalidTesterHarnessRepairTask {
                    task_id: task.id.clone(),
                });
            }
            if self.status == AttemptStatus::Complete
                && task.status != TaskStatus::Complete
                && task.status != TaskStatus::Superseded
                && !matches!(task.kind, TaskKind::Rebase | TaskKind::Learn)
                && !self.is_superseded_tester_harness_followup(&task.id)
                && !self.has_tester_harness_repair_for(&task.id)
                && !self.is_approach_revision_superseded_task(&task.id)
            {
                return Err(WorkModelError::CompleteAttemptHasIncompleteTask {
                    attempt_id: self.id.clone(),
                    task_id: task.id.clone(),
                    task_status: task.status.clone(),
                });
            }
        }
        for (task_index, task) in self.tasks.iter().enumerate() {
            let Some(assignment) = task.writer_correction_assignment.as_ref() else {
                continue;
            };
            if task.kind != TaskKind::Write {
                return Err(WorkModelError::InvalidWriterCorrectionAssignment {
                    task_id: task.id.clone(),
                    reason: "assignment belongs only on a Writer Task".into(),
                });
            }
            let Some(source) = self.tasks[..task_index]
                .iter()
                .find(|source| source.id == assignment.source_task_id)
            else {
                return Err(WorkModelError::InvalidWriterCorrectionAssignment {
                    task_id: task.id.clone(),
                    reason: "assignment source Task is missing or follows the Writer".into(),
                });
            };
            let Some(output) = source.output.as_ref() else {
                return Err(WorkModelError::InvalidWriterCorrectionAssignment {
                    task_id: task.id.clone(),
                    reason: "assignment source Task has no completed candidate output".into(),
                });
            };
            if source.kind != TaskKind::Write
                || source.status != TaskStatus::Complete
                || output.commit != assignment.source_candidate_commit
            {
                return Err(WorkModelError::InvalidWriterCorrectionAssignment {
                    task_id: task.id.clone(),
                    reason: "assignment source Task does not own the bound candidate".into(),
                });
            }
            if !crate::recovery_contracts::assignment_source_inputs_match(task, source, assignment)
            {
                return Err(WorkModelError::InvalidWriterCorrectionAssignment {
                    task_id: task.id.clone(),
                    reason: "assignment source Task inputs changed".into(),
                });
            }
        }
        Ok(())
    }

    fn is_review_only_worktree_shape(&self) -> bool {
        self.tasks
            .first()
            .and_then(|task| task.workspace_access.reads.first())
            .map(|workspace| {
                crate::review_only_worktree::is_review_only_worktree_workspace_path(&workspace.path)
            })
            .unwrap_or(false)
    }

    fn validate_review_only_source_checkout_shape(
        &self,
        work_item_id: &str,
    ) -> Result<(), WorkModelError> {
        for task in &self.tasks {
            if task.kind != TaskKind::Review {
                return Err(WorkModelError::ReviewOnlyAttemptInvalidTask {
                    attempt_id: self.id.clone(),
                    task_id: task.id.clone(),
                    field: "kind",
                });
            }
            self.validate_review_only_task_shape(work_item_id, task, ".")?;
        }
        Ok(())
    }

    fn validate_review_only_worktree_shape(
        &self,
        work_item_id: &str,
    ) -> Result<(), WorkModelError> {
        let expected_workspace_path = self
            .tasks
            .iter()
            .find_map(|task| {
                task.workspace_access
                    .reads
                    .first()
                    .map(|workspace| workspace.path.clone())
            })
            .ok_or_else(|| WorkModelError::ReviewOnlyAttemptInvalidTask {
                attempt_id: self.id.clone(),
                task_id: String::new(),
                field: "tasks",
            })?;
        if !crate::review_only_worktree::is_review_only_worktree_workspace_path(
            &expected_workspace_path,
        ) {
            return Err(WorkModelError::ReviewOnlyAttemptInvalidTask {
                attempt_id: self.id.clone(),
                task_id: String::new(),
                field: "workspace_access.reads",
            });
        }

        let mut tester_count = 0;
        let mut review_count = 0;
        for task in &self.tasks {
            match task.kind {
                TaskKind::Tester => tester_count += 1,
                TaskKind::Review => review_count += 1,
                _ => {
                    return Err(WorkModelError::ReviewOnlyAttemptInvalidTask {
                        attempt_id: self.id.clone(),
                        task_id: task.id.clone(),
                        field: "kind",
                    });
                }
            }
            self.validate_review_only_task_shape(work_item_id, task, &expected_workspace_path)?;
        }
        if tester_count != 1 {
            return Err(WorkModelError::ReviewOnlyAttemptInvalidTask {
                attempt_id: self.id.clone(),
                task_id: String::new(),
                field: "tester",
            });
        }
        if review_count == 0 {
            return Err(WorkModelError::ReviewOnlyAttemptInvalidTask {
                attempt_id: self.id.clone(),
                task_id: String::new(),
                field: "review",
            });
        }
        Ok(())
    }

    fn validate_review_only_task_shape(
        &self,
        work_item_id: &str,
        task: &Task,
        expected_workspace_path: &str,
    ) -> Result<(), WorkModelError> {
        if task.workspace_access.reads.len() != 1
            || task.workspace_access.reads[0].id != "source"
            || task.workspace_access.reads[0].path != expected_workspace_path
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
            || review_context.candidate_workspace_path != expected_workspace_path
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

/// Why an Attempt suspended to `NeedsUser`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PauseKind {
    Auth,
    Uncertain,
    /// Host-owned required-progress or Writer completion evidence needs repair.
    /// Re-running re-evaluates the completed Writer before any review starts.
    WriterCompletion,
    /// Host-owned candidate checks rejected the candidate repeatedly or could
    /// not verify it. Explicit recovery either plans one corrective Writer for
    /// a rejected gate or rechecks an environment-blocked gate.
    CandidateGate,
    /// The candidate needs immutable evidence from a compatible host before its
    /// targeted review and canonical gates can continue. Re-running the Attempt
    /// cannot satisfy this pause; the operator must attach candidate-bound evidence.
    HostEvidence,
    RoundCap,
    /// A transcript-pump infrastructure failure (capture, status persistence, or
    /// transcript phase preservation) stopped the coder. The console transport,
    /// not the coder's work, is broken; a supported resume retries after the
    /// operator fixes it. It is deliberately not `RoundCap`, so it is not charged
    /// against the write-round budget or rejected as a terminal cap on resume.
    TranscriptPump,
    /// Fluent could not apply its production macOS Seatbelt boundary before the
    /// workload started. This is host infrastructure, so retrying the same Task
    /// after the enclosing sandbox recovers must not consume a work round.
    HostSandbox,
    /// The Tester could not produce trustworthy normalized evidence. Retrying
    /// resumes this same Tester Task after the project repairs its harness.
    TesterHarness,
    /// The provider exhausted its own bounded retries before the model used a
    /// tool or emitted tokens. Re-running resumes this exact Task.
    ProviderUnavailable,
    /// The operator stopped a locally owned coder process group. The same Task may
    /// resume only after Fluent verified that the prior group is gone.
    Interrupted,
    /// A pause written by a newer Fluent version. Read-only commands preserve
    /// and display the original value; mutation remains fail-closed.
    Unknown(String),
}

/// An executable recovery that cannot be derived from the coarse pause kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PauseRecovery {
    /// Resume through the scheduler with one exact Tester-only no-sandbox grant.
    TesterNoSandbox,
}

/// Version of the durable description stored for a paused Attempt.
pub const PAUSE_RECORD_SCHEMA_VERSION: u32 = 1;

/// The kind of public operation recorded as the next safe step for a pause.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PauseOperationKind {
    AttemptRun,
    QueueResume,
    EvidenceRun,
    EvidenceAttach,
    AttemptExtend,
    RepairTester,
    Other,
}

impl PauseOperationKind {
    fn from_command(command: &str) -> Self {
        let words = command.split_whitespace().collect::<Vec<_>>();
        match words.as_slice() {
            ["fluent", "attempt", "run", ..] => Self::AttemptRun,
            ["fluent", "queue", "resume", ..] => Self::QueueResume,
            ["fluent", "attempt", "evidence", "run", ..] => Self::EvidenceRun,
            ["fluent", "attempt", "evidence", "attach", ..] => Self::EvidenceAttach,
            ["fluent", "attempt", "extend", ..] => Self::AttemptExtend,
            ["fluent", "attempt", "repair-tester", ..] => Self::RepairTester,
            _ => Self::Other,
        }
    }
}

/// A precondition that must still hold before a recorded operation is used.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PausePrecondition {
    pub name: String,
    pub expected: String,
}

/// One exact, bounded public operation that can be offered for a pause.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PauseOperation {
    pub kind: PauseOperationKind,
    pub command: String,
    #[serde(default)]
    pub arguments: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub preconditions: Vec<PausePrecondition>,
}

impl PauseOperation {
    pub fn from_command(command: impl Into<String>, attempt: &Attempt) -> Option<Self> {
        let command = command.into();
        let command = command.trim().to_string();
        if command.is_empty() {
            return None;
        }
        let arguments = command
            .split_whitespace()
            .skip(1)
            .map(str::to_string)
            .collect::<Vec<_>>();
        let mut preconditions = vec![PausePrecondition {
            name: "attempt-status".to_string(),
            expected: AttemptStatus::NeedsUser.as_str().to_string(),
        }];
        if let Some(pause) = attempt.pause_kind.as_ref() {
            preconditions.push(PausePrecondition {
                name: "pause-kind".to_string(),
                expected: pause.as_str().to_string(),
            });
        }
        Some(Self {
            kind: PauseOperationKind::from_command(&command),
            command,
            arguments,
            preconditions,
        })
    }

    fn validate(&self, attempt_id: &str) -> Result<(), String> {
        if self.command.trim().is_empty() {
            return Err("next operation has an empty command".to_string());
        }
        if self.command.len() > 4_096 {
            return Err("next operation command exceeds 4096 bytes".to_string());
        }
        if self.arguments.len() > 64 || self.arguments.iter().any(|arg| arg.len() > 512) {
            return Err(format!(
                "next operation arguments are too large for Attempt {attempt_id:?}"
            ));
        }
        if self.preconditions.len() > 16
            || self
                .preconditions
                .iter()
                .any(|condition| condition.name.len() > 256 || condition.expected.len() > 256)
        {
            return Err("next operation preconditions are too large".to_string());
        }
        Ok(())
    }
}

/// The one durable description shared by status, handoffs, and CLI recovery
/// checks for an Attempt waiting at `needs-user`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct PauseRecord {
    pub schema_version: u32,
    pub pause_kind: PauseKind,
    pub blocker: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate_commit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    pub required_evidence_or_decision: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_operation: Option<PauseOperation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub no_safe_operation_reason: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl PauseRecord {
    fn blocker_for(kind: &PauseKind) -> String {
        match kind {
            PauseKind::Auth => "Provider authentication is required.".to_string(),
            PauseKind::Uncertain => "A review result is uncertain or incomplete.".to_string(),
            PauseKind::WriterCompletion => {
                "Writer completion evidence is missing or invalid.".to_string()
            }
            PauseKind::CandidateGate => "Candidate checks need operator recovery.".to_string(),
            PauseKind::HostEvidence => "Candidate-bound host evidence is required.".to_string(),
            PauseKind::RoundCap => "The authorized Writer rounds are exhausted.".to_string(),
            PauseKind::TranscriptPump => "Transcript or console transport failed.".to_string(),
            PauseKind::HostSandbox => "The host sandbox blocked execution.".to_string(),
            PauseKind::TesterHarness => "Tester evidence is not trustworthy yet.".to_string(),
            PauseKind::ProviderUnavailable => "The model provider is unavailable.".to_string(),
            PauseKind::Interrupted => "A previous Task stopped before settlement.".to_string(),
            PauseKind::Unknown(_) => "An unknown pause kind was stored.".to_string(),
        }
    }

    fn current_candidate(attempt: &Attempt) -> Option<String> {
        attempt
            .tasks
            .iter()
            .rev()
            .find(|task| {
                task.kind == TaskKind::Write
                    && task.status == TaskStatus::Complete
                    && task.output.is_some()
            })
            .and_then(|task| task.output.as_ref().map(|output| output.commit.clone()))
    }

    fn current_task(attempt: &Attempt) -> Option<String> {
        attempt
            .tasks
            .iter()
            .rev()
            .find(|task| matches!(task.status, TaskStatus::Failed | TaskStatus::NeedsUser))
            .map(|task| task.id.clone())
    }

    pub(crate) fn generated(
        attempt: &Attempt,
        next_action: Option<String>,
        no_safe_operation_reason: Option<String>,
    ) -> Self {
        let now = now_iso8601();
        Self {
            schema_version: PAUSE_RECORD_SCHEMA_VERSION,
            pause_kind: attempt.pause_kind.clone().unwrap_or(PauseKind::Unknown(
                "missing-pause-kind".to_string(),
            )),
            blocker: attempt
                .pause_kind
                .as_ref()
                .map(Self::blocker_for)
                .unwrap_or_else(|| "The Attempt needs user input before it can advance.".to_string()),
            candidate_commit: Self::current_candidate(attempt),
            task_id: Self::current_task(attempt),
            required_evidence_or_decision: "Resolve the blocker, then use the recorded operation when its preconditions still hold.".to_string(),
            next_operation: next_action.and_then(|command| PauseOperation::from_command(command, attempt)),
            no_safe_operation_reason,
            created_at: now.clone(),
            updated_at: now,
        }
    }

    pub fn validate(&self, attempt_id: &str) -> Result<(), String> {
        if self.schema_version != PAUSE_RECORD_SCHEMA_VERSION {
            return Err(format!(
                "pause record schema version {} is not supported",
                self.schema_version
            ));
        }
        if self.blocker.trim().is_empty() || self.blocker.len() > 4_096 {
            return Err("pause record blocker is empty or too large".to_string());
        }
        if self.required_evidence_or_decision.trim().is_empty()
            || self.required_evidence_or_decision.len() > 4_096
        {
            return Err("pause record required input is empty or too large".to_string());
        }
        if self
            .candidate_commit
            .as_ref()
            .is_some_and(|candidate| candidate.is_empty() || candidate.len() > 128)
        {
            return Err("pause record candidate commit is invalid".to_string());
        }
        if self
            .task_id
            .as_ref()
            .is_some_and(|task| task.is_empty() || task.len() > 256)
        {
            return Err("pause record task id is invalid".to_string());
        }
        if self.next_operation.is_some() == self.no_safe_operation_reason.is_some() {
            return Err(
                "pause record must contain exactly one next operation or no-safe reason"
                    .to_string(),
            );
        }
        if let Some(operation) = self.next_operation.as_ref() {
            operation.validate(attempt_id)?;
        }
        if self
            .no_safe_operation_reason
            .as_ref()
            .is_some_and(|reason| reason.trim().is_empty() || reason.len() > 4_096)
        {
            return Err("pause record no-safe reason is empty or too large".to_string());
        }
        if self.created_at.trim().is_empty() || self.updated_at.trim().is_empty() {
            return Err("pause record timestamps must not be empty".to_string());
        }
        Ok(())
    }

    pub fn matches_attempt(&self, attempt: &Attempt) -> bool {
        attempt.status == AttemptStatus::NeedsUser
            && attempt.pause_kind.as_ref() == Some(&self.pause_kind)
            && self.candidate_commit.as_ref().is_none_or(|candidate| {
                Self::current_candidate(attempt).as_deref() == Some(candidate)
            })
            && self.task_id.as_ref().is_none_or(|task_id| {
                attempt.tasks.iter().any(|task| {
                    task.id == *task_id
                        && matches!(task.status, TaskStatus::Failed | TaskStatus::NeedsUser)
                })
            })
    }

    fn same_content(&self, other: &Self) -> bool {
        self.schema_version == other.schema_version
            && self.pause_kind == other.pause_kind
            && self.blocker == other.blocker
            && self.candidate_commit == other.candidate_commit
            && self.task_id == other.task_id
            && self.required_evidence_or_decision == other.required_evidence_or_decision
            && self.next_operation == other.next_operation
            && self.no_safe_operation_reason == other.no_safe_operation_reason
    }
}

/// The immutable inputs that identify one public recovery request. Recovery
/// modules use this shared shape before they apply their domain-specific
/// checks, so a stale candidate or Task is rejected consistently.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryOperationRequest {
    pub operation_kind: String,
    pub work_item_id: String,
    pub attempt_id: String,
    pub candidate_commit: Option<String>,
    pub task_id: Option<String>,
    pub evidence_digest: Option<String>,
    pub workspace_path: Option<String>,
    pub authorization: Option<String>,
    pub input_digest: Option<String>,
}

impl RecoveryOperationRequest {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        operation_kind: impl Into<String>,
        work_item_id: impl Into<String>,
        attempt_id: impl Into<String>,
        candidate_commit: Option<String>,
        task_id: Option<String>,
        evidence_digest: Option<String>,
        workspace_path: Option<String>,
        authorization: Option<String>,
        input_digest: Option<String>,
    ) -> Self {
        Self {
            operation_kind: operation_kind.into().trim().to_string(),
            work_item_id: work_item_id.into().trim().to_string(),
            attempt_id: attempt_id.into().trim().to_string(),
            candidate_commit: candidate_commit.map(|value| value.trim().to_string()),
            task_id: task_id.map(|value| value.trim().to_string()),
            evidence_digest: evidence_digest.map(|value| value.trim().to_string()),
            workspace_path: workspace_path.map(|value| value.trim().to_string()),
            authorization: authorization.map(|value| value.trim().to_string()),
            input_digest: input_digest.map(|value| value.trim().to_string()),
        }
    }

    /// Stable identity for the exact operation inputs. Length-prefixing keeps
    /// adjacent fields unambiguous without depending on JSON formatting.
    pub fn operation_id(&self) -> String {
        let mut hasher = Sha256::new();
        for value in [
            self.operation_kind.as_str(),
            self.work_item_id.as_str(),
            self.attempt_id.as_str(),
            self.candidate_commit.as_deref().unwrap_or("<none>"),
            self.task_id.as_deref().unwrap_or("<none>"),
            self.evidence_digest.as_deref().unwrap_or("<none>"),
            self.workspace_path.as_deref().unwrap_or("<none>"),
            self.authorization.as_deref().unwrap_or("<none>"),
            self.input_digest.as_deref().unwrap_or("<none>"),
        ] {
            hasher.update((value.len() as u64).to_be_bytes());
            hasher.update(value.as_bytes());
        }
        format!("recovery-{:x}", hasher.finalize())
    }

    pub fn authorization_digest(&self) -> Option<String> {
        self.authorization
            .as_deref()
            .map(|value| format!("sha256:{:x}", Sha256::digest(value.as_bytes())))
    }

    fn validate_shape(&self, attempt_id: &str) -> Result<(), WorkModelError> {
        if self.operation_kind.is_empty() || self.operation_kind.len() > 128 {
            return Err(WorkModelError::RecoveryOperationIneligible {
                attempt_id: attempt_id.to_string(),
                reason: "recovery operation kind is empty or too long".to_string(),
            });
        }
        if self.work_item_id.is_empty() || self.attempt_id.is_empty() {
            return Err(WorkModelError::RecoveryOperationIneligible {
                attempt_id: attempt_id.to_string(),
                reason: "recovery operation must identify a Work Item and Attempt".to_string(),
            });
        }
        for (label, value, limit) in [
            ("candidate", self.candidate_commit.as_deref(), 128),
            ("Task", self.task_id.as_deref(), 256),
            ("evidence digest", self.evidence_digest.as_deref(), 256),
            ("workspace", self.workspace_path.as_deref(), 4_096),
            ("input digest", self.input_digest.as_deref(), 256),
        ] {
            if value.is_some_and(|value| value.is_empty() || value.len() > limit) {
                return Err(WorkModelError::RecoveryOperationIneligible {
                    attempt_id: attempt_id.to_string(),
                    reason: format!("recovery operation {label} is empty or too long"),
                });
            }
        }
        if self
            .authorization
            .as_deref()
            .is_some_and(|value| value.is_empty() || value.len() > MAX_ROUND_CAP_RESOLUTION_BYTES)
        {
            return Err(WorkModelError::RecoveryOperationIneligible {
                attempt_id: attempt_id.to_string(),
                reason: "recovery operation authorization is empty or too long".to_string(),
            });
        }
        Ok(())
    }
}

/// The result recorded for a recovery operation. Replays return the original
/// record; they do not append another result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RecoveryOperationResult {
    Applied,
    /// Preserve no-change completion authority across older split-record writers.
    CompletionRetry,
}

impl RecoveryOperationResult {
    fn for_kind(kind: &str) -> Self {
        if kind.starts_with("writer-completion-no-change-") {
            Self::CompletionRetry
        } else {
            Self::Applied
        }
    }
}

/// One append-only audit record for a public recovery operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct RecoveryOperationRecord {
    pub schema_version: u32,
    pub operation_id: String,
    pub operation_kind: String,
    pub work_item_id: String,
    pub attempt_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate_commit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authorization: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authorization_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_digest: Option<String>,
    pub result: RecoveryOperationResult,
    pub next_status: AttemptStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_pause_kind: Option<PauseKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_action: Option<String>,
    pub created_at: String,
    pub completed_at: String,
}

impl RecoveryOperationRecord {
    pub fn from_request(
        request: &RecoveryOperationRequest,
        attempt: &Attempt,
        next_action: Option<String>,
    ) -> Self {
        let now = now_iso8601();
        Self {
            schema_version: RECOVERY_OPERATION_SCHEMA_VERSION,
            operation_id: request.operation_id(),
            operation_kind: request.operation_kind.clone(),
            work_item_id: request.work_item_id.clone(),
            attempt_id: request.attempt_id.clone(),
            candidate_commit: request.candidate_commit.clone(),
            task_id: request.task_id.clone(),
            evidence_digest: request.evidence_digest.clone(),
            workspace_path: request.workspace_path.clone(),
            authorization: request.authorization.clone(),
            authorization_digest: request.authorization_digest(),
            input_digest: request.input_digest.clone(),
            result: RecoveryOperationResult::for_kind(&request.operation_kind),
            next_status: attempt.status.clone(),
            next_pause_kind: attempt.pause_kind.clone(),
            next_action,
            created_at: now.clone(),
            completed_at: now,
        }
    }

    pub(crate) fn matches_request(&self, request: &RecoveryOperationRequest) -> bool {
        self.schema_version == RECOVERY_OPERATION_SCHEMA_VERSION
            && self.operation_id == request.operation_id()
            && self.operation_kind == request.operation_kind
            && self.work_item_id == request.work_item_id
            && self.attempt_id == request.attempt_id
            && self.candidate_commit == request.candidate_commit
            && self.task_id == request.task_id
            && self.evidence_digest == request.evidence_digest
            && self.workspace_path == request.workspace_path
            && self.authorization == request.authorization
            && self.authorization_digest == request.authorization_digest()
            && self.input_digest == request.input_digest
            && self.result == RecoveryOperationResult::for_kind(&request.operation_kind)
    }

    fn validate(&self, work_item_id: &str, attempt_id: &str) -> Result<(), String> {
        if self.schema_version != RECOVERY_OPERATION_SCHEMA_VERSION {
            return Err(format!(
                "recovery operation schema version {} is not supported",
                self.schema_version
            ));
        }
        if self.operation_id.trim().is_empty()
            || self.operation_kind.trim().is_empty()
            || self.work_item_id != work_item_id
            || self.attempt_id != attempt_id
            || self.result != RecoveryOperationResult::for_kind(&self.operation_kind)
        {
            return Err("recovery operation identity is invalid".to_string());
        }
        if self.authorization_digest
            != self
                .authorization
                .as_deref()
                .map(|value| format!("sha256:{:x}", Sha256::digest(value.as_bytes())))
        {
            return Err("recovery operation authorization digest is invalid".to_string());
        }
        if self
            .next_action
            .as_deref()
            .is_some_and(|value| value.len() > 4_096)
        {
            return Err("recovery operation next action is too long".to_string());
        }
        if self.created_at.trim().is_empty() || self.completed_at.trim().is_empty() {
            return Err("recovery operation timestamps must not be empty".to_string());
        }
        Ok(())
    }
}

impl PauseKind {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Auth => "auth",
            Self::Uncertain => "uncertain",
            Self::WriterCompletion => "writer-completion",
            Self::CandidateGate => "candidate-gate",
            Self::HostEvidence => "host-evidence",
            Self::RoundCap => "round-cap",
            Self::TranscriptPump => "transcript-pump",
            Self::HostSandbox => "host-sandbox",
            Self::TesterHarness => "tester-harness",
            Self::ProviderUnavailable => "provider-unavailable",
            Self::Interrupted => "interrupted",
            Self::Unknown(value) => value,
        }
    }

    pub fn is_unknown(&self) -> bool {
        matches!(self, Self::Unknown(_))
    }
}

impl Serialize for PauseKind {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for PauseKind {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Ok(match value.as_str() {
            "auth" => Self::Auth,
            "uncertain" => Self::Uncertain,
            "writer-completion" => Self::WriterCompletion,
            "candidate-gate" => Self::CandidateGate,
            "host-evidence" => Self::HostEvidence,
            "round-cap" => Self::RoundCap,
            "transcript-pump" => Self::TranscriptPump,
            "host-sandbox" => Self::HostSandbox,
            "tester-harness" => Self::TesterHarness,
            "provider-unavailable" => Self::ProviderUnavailable,
            "interrupted" => Self::Interrupted,
            _ => Self::Unknown(value),
        })
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
    /// Host-owned context for a review that adjudicates evidence without a
    /// candidate change. Ordinary reviews deliberately omit this field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_review_context: Option<EvidenceReviewContext>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_context: Option<VerificationContext>,
    /// Host-owned assignment captured before a corrective Writer launches.
    /// Writer-writable completion artifacts never replace this record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub writer_correction_assignment: Option<WriterCorrectionAssignment>,
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

/// The authorized public check. Hash the complete definition to bind its policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerificationChecker {
    #[serde(
        default,
        skip_serializing_if = "ResolvedCheckApplicability::is_default"
    )]
    pub applicability: ResolvedCheckApplicability,
    pub schema_version: u32,
    pub command: String,
    pub working_directory: String,
    pub timeout_seconds: u64,
    pub sandbox_digest: String,
}

/// Immutable inputs for a deterministic verification worker; no coder is involved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerificationContext {
    pub candidate_commit: String,
    pub checker: VerificationChecker,
    pub checker_digest: String,
    pub(crate) review_artifact: BoundRecheckArtifact,
}

/// Immutable inputs and identity for an evidence-targeted reviewer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceReviewContext {
    pub recovery_id: String,
    pub candidate_commit: String,
    pub attachment: EvidenceAttachment,
    pub prior_review_artifact: String,
}

pub(crate) fn candidate_metadata_derived_project_check_path(task: &Task) -> Option<&str> {
    task.instructions
        .as_deref()?
        .rsplit_once(CANDIDATE_METADATA_DERIVED_PROJECT_CHECK_MARKER)
        .map(|(_, path)| path.trim())
}

impl Task {
    pub fn is_failed_command_recheck(&self) -> bool {
        self.kind == TaskKind::Tester && self.role == FAILED_COMMAND_RECHECK_ROLE
    }

    pub fn validate(&self) -> Result<(), WorkModelError> {
        self.workspace_access.validate()?;
        if let Some(assignment) = &self.writer_correction_assignment {
            assignment.validate_for_task(self).map_err(|reason| {
                WorkModelError::InvalidWriterCorrectionAssignment {
                    task_id: self.id.clone(),
                    reason,
                }
            })?;
        }
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
        if let Some((output, no_change)) = self.output.as_ref().and_then(|output| {
            output
                .no_change
                .as_ref()
                .map(|no_change| (output, no_change))
        }) {
            let expected_path = self
                .artifact_area
                .as_ref()
                .map(|area| format!("{}/no-change.json", area.path.trim_end_matches('/')));
            let commit_identity_is_valid = match &output.learner_canonicalization {
                None => output.base_commit.as_deref() == Some(output.commit.as_str()),
                Some(canonicalization) => {
                    output.base_commit.as_deref() == Some(canonicalization.from_commit.as_str())
                        && output.commit == canonicalization.to_commit
                        && canonicalization.from_commit != canonicalization.to_commit
                }
            };
            let invalid = self.kind != TaskKind::Write
                || !commit_identity_is_valid
                || no_change.schema_version != 1
                || no_change.reason.trim().is_empty()
                || no_change.verification.is_empty()
                || no_change
                    .verification
                    .iter()
                    .any(|entry| entry.command.trim().is_empty())
                || expected_path.as_deref() != Some(no_change.declaration_path.as_str());
            if invalid {
                return Err(WorkModelError::InvalidTaskNoChangeOutput {
                    task_id: self.id.clone(),
                });
            }
        } else if self
            .output
            .as_ref()
            .is_some_and(|output| output.learner_canonicalization.is_some())
        {
            return Err(WorkModelError::InvalidTaskNoChangeOutput {
                task_id: self.id.clone(),
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
        if self.role == FAILED_COMMAND_RECHECK_ROLE {
            let candidate_is_readable = self.review_context.as_ref().is_some_and(|context| {
                self.workspace_access.reads.iter().any(|workspace| {
                    workspace.id == context.candidate_workspace_id
                        && workspace.path == context.candidate_workspace_path
                })
            });
            let source_is_bound = self
                .input_artifacts
                .as_slice()
                .first()
                .is_some_and(|source| {
                    self.input_artifacts.len() == 1
                        && is_canonical_work_artifact_file(&source.path, "tester-results.json")
                        && self.depends_on.as_deref() == Some(source.producer_id.as_str())
                });
            if self.kind != TaskKind::Tester
                || self.artifact_area.is_none()
                || !candidate_is_readable
                || !source_is_bound
            {
                return Err(WorkModelError::InvalidFailedCommandRecheckTask {
                    task_id: self.id.clone(),
                });
            }
        }
        if self.role == TESTER_HARNESS_REPAIR_ROLE {
            let context_matches_write = self.review_context.as_ref().is_some_and(|context| {
                self.workspace_access
                    .writes
                    .as_slice()
                    .first()
                    .is_some_and(|workspace| {
                        self.workspace_access.writes.len() == 1
                            && workspace.id == context.candidate_workspace_id
                            && workspace.path == context.candidate_workspace_path
                    })
            });
            let authority_path = self.attempt_id.as_deref().map(|attempt_id| {
                work_artifact_path(
                    &self.work_item_id,
                    attempt_id,
                    &format!("tester-harness-repairs/{}/repair.json", self.id),
                )
            });
            let has_authority = authority_path.as_deref().is_some_and(|expected| {
                self.input_artifacts.iter().any(|artifact| {
                    artifact.producer_id == "tester-harness-repair-authority"
                        && artifact.path == expected
                })
            });
            let has_source_evidence = self.input_artifacts.iter().any(|artifact| {
                artifact.producer_id != "tester-harness-repair-authority"
                    && (is_canonical_work_artifact_file(&artifact.path, "tester-results.json")
                        || is_canonical_work_artifact_file(&artifact.path, "commands.json"))
            });
            if self.kind != TaskKind::Write
                || self.artifact_area.is_none()
                || !context_matches_write
                || !has_authority
                || !has_source_evidence
                || self.depends_on.is_some()
            {
                return Err(WorkModelError::InvalidTesterHarnessRepairTask {
                    task_id: self.id.clone(),
                });
            }
        }
        if self.role == CANDIDATE_ANCESTRY_REPAIR_ROLE {
            let output_matches_write = self.output.as_ref().is_some_and(|output| {
                self.workspace_access
                    .writes
                    .as_slice()
                    .first()
                    .is_some_and(|workspace| {
                        self.workspace_access.writes.len() == 1
                            && workspace.id == output.workspace_id
                            && workspace.path == output.workspace_path
                            && output.base_commit.is_some()
                            && output.no_change.is_none()
                            && output.learner_canonicalization.is_none()
                    })
            });
            let evidence_path = self.attempt_id.as_deref().map(|attempt_id| {
                work_artifact_path(
                    &self.work_item_id,
                    attempt_id,
                    &format!("candidate-ancestry-repairs/{}/recovery.json", self.id),
                )
            });
            let has_evidence = evidence_path.as_deref().is_some_and(|expected| {
                self.input_artifacts.iter().any(|artifact| {
                    artifact.producer_id == "candidate-ancestry-recovery"
                        && artifact.path == expected
                })
            });
            if self.kind != TaskKind::Write
                || self.status != TaskStatus::Complete
                || self.artifact_area.is_none()
                || !output_matches_write
                || !has_evidence
                || self.review_context.is_some()
                || self.depends_on.is_some()
            {
                return Err(WorkModelError::InvalidCandidateAncestryRepairTask {
                    task_id: self.id.clone(),
                });
            }
        }
        if self.role == CANDIDATE_METADATA_REPAIR_ROLE {
            let authority_path = self.attempt_id.as_deref().map(|attempt_id| {
                work_artifact_path(
                    &self.work_item_id,
                    attempt_id,
                    &format!("candidate-metadata-repairs/{}/repair.json", self.id),
                )
            });
            let has_authority = authority_path.as_deref().is_some_and(|expected| {
                self.input_artifacts.iter().any(|artifact| {
                    artifact.producer_id == "candidate-metadata-recovery"
                        && artifact.path == expected
                })
            });
            let has_source_gate = self.input_artifacts.iter().any(|artifact| {
                is_canonical_work_artifact_file(
                    &artifact.path,
                    crate::candidate_gate::CANDIDATE_GATE_FILE,
                )
            });
            let has_project_check_input = self.input_artifacts.iter().any(|artifact| {
                is_canonical_work_artifact_file(&artifact.path, "check-pre-merge.log")
            });
            let has_derived_project_check =
                match candidate_metadata_derived_project_check_path(self) {
                    Some(path) => {
                        is_canonical_work_artifact_file(path, "check-pre-merge.log")
                            && self
                                .input_artifacts
                                .iter()
                                .any(|artifact| artifact.path == path)
                    }
                    None => {
                        // Fluent 0.3.0 began recording the exact derived
                        // hook path in the Writer instructions. Older
                        // metadata-recovery Writers already persisted the
                        // canonical hook artifact, but had no marker to carry
                        // that relationship. Keep completed historical Tasks
                        // readable; newly planned Tasks must use the explicit
                        // marker so an arbitrary hook log cannot be admitted.
                        !has_project_check_input || self.status == TaskStatus::Complete
                    }
                };
            if self.kind != TaskKind::Write
                || self.artifact_area.is_none()
                || self.workspace_access.writes.len() != 1
                || !has_authority
                || !has_source_gate
                || !has_derived_project_check
                || self.review_context.is_some()
                || self.depends_on.is_some()
            {
                return Err(WorkModelError::InvalidCandidateMetadataRepairTask {
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
    /// A later authoritative result made this terminal Task outcome
    /// non-actionable while preserving it as durable history.
    Superseded,
}

impl TaskStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Planned => "planned",
            Self::Executing => "executing",
            Self::Complete => "complete",
            Self::Failed => "failed",
            Self::NeedsUser => "needs-user",
            Self::Superseded => "superseded",
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
    Tester,
    Verification,
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
            Self::Tester => "tester",
            Self::Verification => "verification",
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
            "tester" => Ok(Self::Tester),
            "verification" => Ok(Self::Verification),
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

/// Fluent-managed filesystem/git context.
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_commit: Option<String>,
}

/// Durable output produced by a completed task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskOutput {
    pub workspace_id: String,
    pub workspace_path: String,
    pub source_branch: String,
    /// The commit at the start of the accepted Attempt change. Persisting the
    /// commit, rather than only the branch name, keeps post-land diffs stable
    /// after the target branch advances.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_commit: Option<String>,
    pub commit: String,
    /// Evidence that a corrective Writer verified the existing candidate and
    /// intentionally preserved it instead of producing another commit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub no_change: Option<NoChangeOutput>,
    /// The host-owned capture Learner transition from the commit verified by a
    /// no-change Writer to the current canonical candidate commit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub learner_canonicalization: Option<LearnerCommitCanonicalization>,
}

/// Host-derived disposition of one completed Writer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WriterOutcome {
    Complete,
    Continue,
    Blocked,
}

impl WriterOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::Continue => "continue",
            Self::Blocked => "blocked",
        }
    }
}

/// Position of a Writer run relative to the first review round.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum WriterRunKind {
    #[default]
    Initial,
    PreReviewContinuation,
    Corrective,
}

/// Durable identity used to continue one provider session across Writer Tasks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WriterRun {
    pub task_id: String,
    pub outcome: WriterOutcome,
    #[serde(default)]
    pub kind: WriterRunKind,
    pub provider: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub continuation: u32,
    #[serde(default)]
    pub checked_required: usize,
    #[serde(default)]
    pub completed_matrix: usize,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub candidate_commit: String,
}

/// Typed provenance for a capture Learner's canonical expertise commit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LearnerCommitCanonicalization {
    pub from_commit: String,
    pub to_commit: String,
}

/// Typed reference to a follow-up Writer's Task-owned no-change declaration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoChangeOutput {
    pub schema_version: u32,
    pub declaration_path: String,
    pub reason: String,
    pub verification: Vec<NoChangeVerification>,
}

/// One command a follow-up Writer ran before declaring no correction necessary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoChangeVerification {
    pub command: String,
    pub result: NoChangeVerificationResult,
}

/// Machine-checkable outcome accepted in a no-change verification entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NoChangeVerificationResult {
    Pass,
}

/// Pointer to durable output from a task or attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactRef {
    pub producer_id: String,
    pub path: String,
}

/// Identity of an immutable host-produced evidence snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceAttachment {
    pub snapshot_path: String,
    pub digest: String,
}

/// One prior failed review explicitly addressed by host evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceReviewTarget {
    pub role: String,
    pub prior_review_artifact: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_task_id: Option<String>,
    /// Digest of the exact failed Review that paused this recovery.
    ///
    /// Older records omit this field; new evidence-needed pauses always save
    /// it before accepting a later source-correction classification.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_digest: Option<String>,
}

/// Identify one canonical command independently of execution Task order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequiredCheck {
    #[serde(
        default,
        skip_serializing_if = "ResolvedCheckApplicability::is_default"
    )]
    pub applicability: ResolvedCheckApplicability,
    pub id: String,
    pub work_item_id: String,
    pub attempt_id: String,
    pub candidate_commit: String,
    pub command: String,
    pub working_directory: String,
    pub source: RequiredCheckSource,
}

/// Preserve immutable source bytes and the original Task as provenance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum RequiredCheckSource {
    ReviewRequested {
        task_id: String,
        request: BoundRecheckArtifact,
        comparison_commit: Option<String>,
    },
    CanonicalTester {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reconstruction: Option<BoundRecheckArtifact>,
        task_id: String,
        command_index: usize,
        results: BoundRecheckArtifact,
        manifest: BoundRecheckArtifact,
        logs: Vec<BoundRecheckArtifact>,
        comparison_commit: Option<String>,
    },
}

impl RequiredCheckSource {
    pub fn is_canonical_tester(&self) -> bool {
        matches!(self, Self::CanonicalTester { .. })
    }
}

/// Append-only, host-owned recovery record for an unchanged candidate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceRecovery {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub carried_finding: Option<CarriedFindingEvidence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required_check: Option<RequiredCheck>,
    pub id: String,
    pub candidate_commit: String,
    pub attachment: EvidenceAttachment,
    pub targets: Vec<EvidenceReviewTarget>,
    pub state: EvidenceRecoveryState,
    pub created_at: String,
    /// One operator-authorized source correction for a failed evidence review.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code_change_classification: Option<HostEvidenceClassification>,
}

/// Bind an explicitly selected historical finding to current-candidate proof.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CarriedFindingBinding {
    pub schema_version: u32,
    pub work_item_id: String,
    pub attempt_id: String,
    pub writer_task_id: String,
    pub candidate_commit: String,
    pub finding: WriterCompletionFinding,
    pub assignment: WriterCorrectionAssignment,
    pub assignment_digest: String,
    pub contract_digest: String,
    pub source_review: WriterCorrectionAssignmentInput,
    pub source_matrix: BoundRecheckArtifact,
    pub command: String,
    pub working_directory: String,
    pub applicability: ResolvedCheckApplicability,
    pub evidence: BoundRecheckArtifact,
}

/// Retain the typed contract and the immutable bytes supplied to Tests Review.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CarriedFindingEvidence {
    pub binding: CarriedFindingBinding,
    pub artifact: BoundRecheckArtifact,
}

/// Durable result of classifying candidate-bound host evidence as a source
/// correction. The audit artifact binds the operator decision to its frontier.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostEvidenceClassification {
    pub operation_id: String,
    /// The fixed disposition namespace for this recovery operation.
    ///
    /// Older persisted classifications predate this field and deserialize as
    /// the only disposition this endpoint supports: `code-change`.
    #[serde(default = "default_host_evidence_classification_disposition")]
    pub disposition: String,
    pub review_artifact: String,
    pub review_digest: String,
    pub authorization: String,
    pub authorization_digest: String,
    pub audit_artifact: ArtifactRef,
    pub writer_task_id: String,
    pub classified_at: String,
}

fn default_host_evidence_classification_disposition() -> String {
    "code-change".to_string()
}

impl HostEvidenceClassification {
    pub fn matches_request(
        &self,
        review_artifact: &str,
        review_digest: &str,
        authorization: &str,
        authorization_digest: &str,
    ) -> bool {
        self.review_artifact == review_artifact
            && self.review_digest == review_digest
            && self.authorization == authorization
            && self.authorization_digest == authorization_digest
    }
}

impl EvidenceRecovery {
    fn validate_carried_finding(&self, attempt: &Attempt) -> Result<(), String> {
        let Some(saved) = &self.carried_finding else {
            return Ok(());
        };
        let binding = &saved.binding;
        if binding.schema_version != 1
            || self.required_check.is_some()
            || binding.work_item_id != attempt.work_item_id
            || binding.attempt_id != attempt.id
            || binding.candidate_commit != self.candidate_commit
            || binding.writer_task_id != binding.assignment.writer_task_id
            || binding.finding.role != "tests"
            || !binding.assignment.findings.contains(&binding.finding)
            || !binding.assignment.evidence.contains(&binding.source_review)
            || binding.finding.source != binding.source_review.artifact.path
            || binding.evidence.path != self.attachment.snapshot_path
            || binding.evidence.digest != self.attachment.digest
            || !valid_sha256_digest(&saved.artifact.digest)
            || !valid_sha256_digest(&binding.source_matrix.digest)
            || !valid_sha256_digest(&binding.contract_digest)
        {
            return Err("carried finding has an invalid schema or contradictory binding".into());
        }
        let bytes = serde_json::to_vec_pretty(binding).map_err(|e| e.to_string())?;
        let assignment = serde_json::to_vec(&binding.assignment).map_err(|e| e.to_string())?;
        if format!("sha256:{:x}", Sha256::digest(&bytes)) != saved.artifact.digest
            || format!("sha256:{:x}", Sha256::digest(&assignment)) != binding.assignment_digest
            || self.id != format!("carried-finding-{}", &saved.artifact.digest[7..])
            || saved.artifact.path
                != work_artifact_path(
                    &attempt.work_item_id,
                    &attempt.id,
                    &format!(
                        "carried-finding-evidence/{}.json",
                        &saved.artifact.digest[7..]
                    ),
                )
            || self.targets.len() != 1
            || self.targets[0].role != "tests"
            || self.targets[0].prior_review_artifact != saved.artifact.path
            || (self.state == EvidenceRecoveryState::Complete
                && self.targets[0].review_digest.is_none())
        {
            return Err(
                "carried finding has an invalid artifact, assignment, or Review identity".into(),
            );
        }
        Ok(())
    }

    pub fn is_active(&self) -> bool {
        self.state == EvidenceRecoveryState::Reviewing
    }
}

/// Current disposition of one evidence recovery frontier.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EvidenceRecoveryState {
    Reviewing,
    NeedsEvidence,
    CodeChange,
    Complete,
}

/// Durable identity for exact bytes approved as Work Item execution input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkItemInputArtifact {
    pub source_path: String,
    pub snapshot_path: String,
    pub digest: String,
}

/// Candidate merge result and its merge-review lifecycle state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergeCandidate {
    pub id: String,
    pub attempt_id: String,
    pub source_workspace: WorkspaceRef,
    pub target_workspace: WorkspaceRef,
    pub source_branch: String,
    pub target_branch: String,
    pub candidate_commit: String,
    #[serde(alias = "review_state")]
    pub merge_review_state: MergeReviewState,
    #[serde(default)]
    pub merge_state: MergeCandidateMergeState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    /// The instant this candidate first passed its Learning gate and became
    /// eligible to land. Legacy candidates omit it and remain readable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ready_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
}

impl MergeCandidate {
    pub fn validate(&self, work_item: &WorkItem) -> Result<(), WorkModelError> {
        validate_id("merge candidate", &self.id)?;
        if (self.merge_state.status == MergeCandidateMergeStatus::LandingPending)
            != self.merge_state.landing_intent.is_some()
            && self.merge_state.status != MergeCandidateMergeStatus::Merged
        {
            return Err(WorkModelError::InvalidRecoveryOperation {
                attempt_id: self.attempt_id.clone(),
                reason: "landing status and retained intent disagree".into(),
            });
        }
        if let Some(intent) = self.merge_state.landing_intent.as_ref()
            && (intent.schema_version != 1
                || intent.work_item_id != work_item.id
                || intent.attempt_id != self.attempt_id
                || intent.merge_candidate_id != self.id
                || intent.verified_commit != self.candidate_commit)
        {
            return Err(WorkModelError::InvalidRecoveryOperation {
                attempt_id: self.attempt_id.clone(),
                reason: "landing intent is not bound to this candidate".into(),
            });
        }
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
        // A host-sandbox recovery pause relaxes only the completed status:
        // reviews and provenance must still hold. Learner cancellation keeps a
        // pending candidate's Attempt Complete and uses the typed stop record
        // to block execution and advancement. Failed merges remain exempt.
        let host_sandbox_paused = attempt.status == AttemptStatus::NeedsUser
            && attempt.pause_kind == Some(PauseKind::HostSandbox);
        let interrupted_landing = attempt.status == AttemptStatus::NeedsUser
            && attempt.pause_kind == Some(PauseKind::Interrupted)
            && attempt.cancellation_requested()
            && matches!(
                self.merge_state.status,
                MergeCandidateMergeStatus::LandingPending | MergeCandidateMergeStatus::Merged
            )
            && self.merge_state.landing_intent.is_some();
        let status_ok =
            attempt.status == AttemptStatus::Complete || host_sandbox_paused || interrupted_landing;
        let reviews_passed = attempt.review_state == Some(AttemptReviewState::Passed);
        if self.merge_state.status != MergeCandidateMergeStatus::Failed
            && !(status_ok && reviews_passed)
        {
            return Err(WorkModelError::MergeCandidateAttemptReviewsNotPassed {
                candidate_id: self.id.clone(),
                attempt_id: self.attempt_id.clone(),
            });
        }
        // A failed landing is terminal history, not a candidate that can advance
        // toward landing. Its ready timestamp may legitimately predate a later
        // recovery pause or failed landing attempt, so do not make the failed
        // record unreadable when those timestamps no longer match. Pending,
        // needs-user, executing, and merged candidates still enforce the shared
        // readiness timestamp and Learner gate.
        if self.merge_state.status != MergeCandidateMergeStatus::Failed
            && let Some(ready_at) = self.ready_at.as_ref()
        {
            if !interrupted_landing {
                attempt.learning_advancement_readiness()?;
            }
            if attempt.completed_at.as_ref() != Some(ready_at) {
                return Err(WorkModelError::MergeCandidateReadinessTimestampMismatch {
                    candidate_id: self.id.clone(),
                    attempt_id: self.attempt_id.clone(),
                });
            }
        }
        Ok(())
    }

    /// The landing-readiness gate: a candidate may pass validation and land only
    /// once its Attempt's Learner has SUCCEEDED. An already-merged or terminally
    /// failed candidate is exempt — it is not advancing over a pending Learner but
    /// resuming idempotent post-land work or is already terminal. Shares the one
    /// durable reason [`Attempt::learning_advancement_readiness`] emits, so Merge
    /// Candidate validation and landing reject a non-succeeded Learner identically.
    /// This is a landing gate, kept separate from the structural [`Self::validate`]
    /// so persisting a candidate before its Learner runs stays valid.
    pub fn validate_advancement(&self, work_item: &WorkItem) -> Result<(), WorkModelError> {
        if matches!(
            self.merge_state.status,
            MergeCandidateMergeStatus::Merged | MergeCandidateMergeStatus::Failed
        ) {
            return Ok(());
        }
        work_item.attempt_learning_advancement(&self.attempt_id)
    }
}

/// Merge-time review lifecycle state for a merge candidate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MergeReviewState {
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
    /// A retryable follow-up-processing failure recorded after the candidate
    /// merged. Its presence never changes the successful merge status; it names
    /// the first incomplete stage so a retry can resume.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub follow_up_failure: Option<FollowUpProcessingFailure>,
    /// Authority for one verified target change, retained through result publication.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub landing_intent: Option<LandingIntent>,
}

/// The exact landing authorized before Git changes the target branch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LandingIntent {
    pub schema_version: u32,
    pub operation_id: String,
    pub work_item_id: String,
    pub attempt_id: String,
    pub merge_candidate_id: String,
    pub learner_mode: LearnerMode,
    pub repository: String,
    pub target_branch: String,
    pub target_workspace: String,
    pub target_before: String,
    pub verified_commit: String,
    pub evidence_digest: String,
    pub accepted_model_digest: String,
    pub authorization_digest: String,
    pub check_artifacts: Vec<ArtifactRef>,
    pub review_artifacts: Vec<ArtifactRef>,
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
            follow_up_failure: None,
            landing_intent: None,
        }
    }
}

/// A retryable failure recorded when processing a landed learner handoff did not
/// complete. The merge stays successful; this only records where to resume.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FollowUpProcessingFailure {
    /// The first stage that did not complete: e.g. `validate-handoff`,
    /// `observation`, `work`, or `queue`.
    pub stage: String,
    /// The diagnostic from the failed stage.
    pub message: String,
    /// The concrete next action an operator or the system takes to resume.
    pub next_action: String,
}

/// Coarse merge execution status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum MergeCandidateMergeStatus {
    #[default]
    Pending,
    Executing,
    /// A verified external effect has durable authority and may need replay.
    LandingPending,
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
        TaskStatus::Complete | TaskStatus::Failed | TaskStatus::NeedsUser | TaskStatus::Superseded
    ));
    task.status = status;
    task.completed_at.get_or_insert_with(now_iso8601);
}

pub fn set_attempt_terminal(attempt: &mut Attempt, status: AttemptStatus) {
    debug_assert!(matches!(
        status,
        AttemptStatus::Complete | AttemptStatus::Failed | AttemptStatus::NeedsUser
    ));
    let is_needs_user = status == AttemptStatus::NeedsUser;
    attempt.status = status;
    if !is_needs_user {
        attempt.pause_record = None;
    }
    attempt.completed_at.get_or_insert_with(now_iso8601);
}

pub fn suspend_attempt(attempt: &mut Attempt, kind: PauseKind) {
    attempt.status = AttemptStatus::NeedsUser;
    attempt.pause_kind = Some(kind);
    attempt.pause_recovery = None;
    attempt.completed_at.get_or_insert_with(now_iso8601);
}

/// Precedence rank of an Attempt's `(status, pause)` for monotonic transitions.
/// Higher dominates: a hard failure outranks a non-resumable `RoundCap`/
/// `Uncertain` pause, which outranks a resumable Auth/transcript-infrastructure
/// pause, which outranks ordinary completion, which outranks any active state.
fn attempt_state_rank(status: &AttemptStatus, pause: &Option<PauseKind>) -> u8 {
    match status {
        AttemptStatus::Failed => 5,
        AttemptStatus::NeedsUser => match pause {
            Some(PauseKind::Auth)
            | Some(PauseKind::TranscriptPump)
            | Some(PauseKind::HostSandbox)
            | Some(PauseKind::TesterHarness)
            | Some(PauseKind::WriterCompletion)
            | Some(PauseKind::CandidateGate) => 3,
            Some(PauseKind::ProviderUnavailable) | Some(PauseKind::Interrupted) => 3,
            // RoundCap, Uncertain, or an unclassified pause is non-resumable.
            _ => 4,
        },
        AttemptStatus::Complete => 2,
        AttemptStatus::Planned | AttemptStatus::Executing | AttemptStatus::Reviewing => 1,
    }
}

/// Transition an Attempt toward `status`/`pause` only when it dominates the
/// current state by monotonic precedence, then return whether it applied.
///
/// This is the single boundary through which concurrent Task outcomes update the
/// aggregate Attempt state. A terminal Attempt (Complete, a pause, or Failed) is
/// replaced only by a strictly higher-ranked terminal state, so a late peer
/// completion never overwrites a `NeedsUser`/`Failed` Attempt, a resumable pump
/// pause never masks a harder failure, and equal-rank terminal events preserve
/// the first result. An active target (Planned/Executing/Reviewing) never
/// overwrites a terminal Attempt. Intentional resume uses `reopen_attempt`, which
/// deliberately bypasses this guard.
pub fn transition_attempt(
    attempt: &mut Attempt,
    status: AttemptStatus,
    pause: Option<PauseKind>,
) -> bool {
    if attempt.cancellation_requested() {
        return false;
    }
    let current = attempt_state_rank(&attempt.status, &attempt.pause_kind);
    let incoming = attempt_state_rank(&status, &pause);
    // A terminal current state (rank >= 2) yields only to a strictly dominating
    // terminal state; active current states accept any transition.
    if current >= 2 && incoming <= current {
        return false;
    }
    let is_terminal = matches!(
        status,
        AttemptStatus::Complete | AttemptStatus::Failed | AttemptStatus::NeedsUser
    );
    let is_needs_user = status == AttemptStatus::NeedsUser;
    attempt.status = status;
    attempt.pause_kind = pause;
    attempt.pause_recovery = None;
    if !is_needs_user {
        attempt.pause_record = None;
    }
    if is_terminal {
        attempt.completed_at.get_or_insert_with(now_iso8601);
    } else {
        attempt.completed_at = None;
    }
    true
}

/// Reopen a resumably-paused Attempt so a supported resume can retry. Only the
/// Tasks that ended on the resumable pause — durably marked `NeedsUser` — are
/// replanned and have their terminal timestamp cleared. `Complete` Tasks, hard
/// `Failed` Tasks, and still-live `Executing` Tasks are left untouched, so resume
/// never discards a completed peer, silently retries a hard failure, or replans a
/// Task that is still running.
pub fn reopen_attempt(attempt: &mut Attempt) {
    if attempt.cancellation_requested() {
        return;
    }
    attempt.status = AttemptStatus::Planned;
    attempt.pause_kind = None;
    attempt.pause_recovery = None;
    attempt.pause_record = None;
    attempt.completed_at = None;
    for task in &mut attempt.tasks {
        if task.status == TaskStatus::NeedsUser {
            task.status = TaskStatus::Planned;
            task.completed_at = None;
        }
    }
}

pub fn mark_merge_candidate_started(candidate: &mut MergeCandidate) {
    candidate.started_at.get_or_insert_with(now_iso8601);
}

/// Record the first instant a code-producing Attempt and its Merge Candidate
/// become ready to land. Reusing `ready_at` makes retries idempotent and repairs
/// legacy Attempts whose `completed_at` stopped at review completion.
fn mark_attempt_merge_candidate_ready(
    attempt: &mut Attempt,
    candidate: &mut MergeCandidate,
) -> Result<(), WorkModelError> {
    attempt.ensure_not_canceled()?;
    if attempt.status != AttemptStatus::Complete
        || attempt.review_state != Some(AttemptReviewState::Passed)
    {
        return Err(WorkModelError::AttemptReviewsNotPassed {
            attempt_id: attempt.id.clone(),
        });
    }
    attempt.learning_advancement_readiness()?;
    let ready_at = candidate.ready_at.get_or_insert_with(now_iso8601).clone();
    attempt.completed_at = Some(ready_at);
    Ok(())
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
    StaleExternalApproval,
    TransitionLedgerModified,
    AttemptResumeIneligible {
        attempt_id: String,
        reason: String,
    },
    InvalidPauseRecord {
        attempt_id: String,
        reason: String,
    },
    InvalidRecoveryOperation {
        attempt_id: String,
        reason: String,
    },
    UnsupportedStoredRecordVersion {
        record_kind: &'static str,
        found: u32,
        supported: u32,
    },
    InvalidStoredWriterVersion {
        version: String,
    },
    WorkModelUpgradeConflict {
        work_item_id: String,
        operation_id: String,
    },
    RecoveryOperationIneligible {
        attempt_id: String,
        reason: String,
    },
    RecoveryOperationConflict {
        attempt_id: String,
        operation_id: String,
    },
    MergeCandidateReadinessTimestampMismatch {
        candidate_id: String,
        attempt_id: String,
    },
    RoundCapExtensionIncrementOutOfRange {
        requested: usize,
    },
    RoundCapExtensionStaleCap {
        expected: usize,
        actual: usize,
    },
    RoundCapResolutionInvalid {
        reason: String,
    },
    RoundCapExtensionIneligible {
        reason: String,
    },
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
    /// Host evidence was supplied for a real Attempt, but it no longer
    /// matches the immutable recovery frontier that the public command names.
    HostEvidenceRecoveryIneligible {
        attempt_id: String,
        reason: String,
    },
    AttemptEvidenceRecoveryConflict {
        attempt_id: String,
    },
    TesterHarnessRepairIneligible {
        reason: String,
    },
    LegacyLifecycleRecoveryConflict {
        attempt_id: String,
    },
    TaskNotFound {
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
    MergeCandidateNotFound {
        candidate_id: String,
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
    MergeCandidateCorrectionIneligible {
        candidate_id: String,
        reason: String,
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
    InvalidTaskNoChangeOutput {
        task_id: String,
    },
    InvalidWriterCorrectionAssignment {
        task_id: String,
        reason: String,
    },
    WriterCorrectionAssignmentModified {
        task_id: String,
    },
    InvalidFailedCommandRecheckTask {
        task_id: String,
    },
    InvalidTesterHarnessRepairTask {
        task_id: String,
    },
    InvalidCandidateAncestryRepairTask {
        task_id: String,
    },
    InvalidCandidateMetadataRepairTask {
        task_id: String,
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
    WorkNotExecutionReady {
        work_item_id: String,
    },
    UnsupportedStoredState {
        work_item_id: String,
        reason: String,
    },
    InvalidReleaseContract {
        reason: String,
    },
    InvalidPlanningScope {
        reason: String,
    },
    InvalidPlanningRevision {
        reason: String,
    },
    ReleaseContractRequired {
        work_item_id: String,
    },
    ReleaseCriterionNotFound {
        criterion_id: String,
    },
    ReleaseFindingAlreadyExists {
        id: String,
    },
    CorrectiveContextIncomplete {
        field: &'static str,
    },
    AttemptLearningNotSucceeded {
        attempt_id: String,
        state: &'static str,
    },
    FrozenReviewedIdentity {
        attempt_id: String,
        field: &'static str,
    },
}

impl fmt::Display for WorkModelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StaleExternalApproval => f.write_str("Work changed; request a fresh approval"),
            Self::TransitionLedgerModified => {
                f.write_str("durable transition history is append-only and host-owned")
            }
            Self::AttemptResumeIneligible { reason, .. } => f.write_str(reason),
            Self::InvalidPauseRecord { attempt_id, reason } => {
                write!(
                    f,
                    "Attempt {attempt_id:?} has an invalid pause record: {reason}"
                )
            }
            Self::InvalidRecoveryOperation { attempt_id, reason } => {
                write!(
                    f,
                    "Attempt {attempt_id:?} has an invalid recovery operation: {reason}"
                )
            }
            Self::UnsupportedStoredRecordVersion {
                record_kind,
                found,
                supported,
            } => write!(
                f,
                "{record_kind} record schema version {found} is newer than supported version {supported}; upgrade Fluent before reading it"
            ),
            Self::InvalidStoredWriterVersion { version } => write!(
                f,
                "stored Fluent model-writer version {version:?} is not a valid semantic version; refusing to guess its format"
            ),
            Self::WorkModelUpgradeConflict {
                work_item_id,
                operation_id,
            } => write!(
                f,
                "model upgrade {operation_id:?} for Work Item {work_item_id:?} has different audit bytes"
            ),
            Self::RecoveryOperationIneligible { attempt_id, reason } => {
                write!(
                    f,
                    "Recovery operation is not eligible for Attempt {attempt_id:?}: {reason}"
                )
            }
            Self::RecoveryOperationConflict {
                attempt_id,
                operation_id,
            } => write!(
                f,
                "Recovery operation {operation_id:?} conflicts with the existing record for Attempt {attempt_id:?}"
            ),
            Self::RoundCapExtensionIncrementOutOfRange { requested } => write!(
                f,
                "round-cap extensions must add {MIN_ADDITIONAL_WRITE_ROUNDS} to {MAX_ADDITIONAL_WRITE_ROUNDS} Writer rounds, not {requested}"
            ),
            Self::RoundCapExtensionStaleCap { expected, actual } => write!(
                f,
                "round-cap extension expected current cap {expected}, not stale cap {actual}"
            ),
            Self::RoundCapResolutionInvalid { reason } => {
                write!(f, "round-cap resolution is invalid: {reason}")
            }
            Self::RoundCapExtensionIneligible { reason } => {
                write!(f, "round-cap extension is not eligible: {reason}")
            }
            Self::InvalidId { kind, id } => {
                write!(f, "{kind} id {id:?} cannot be used as a file name")
            }
            Self::AttemptAlreadyExists { id } => {
                write!(f, "Attempt {id:?} already exists")
            }
            Self::AttemptNotFound { id } => {
                write!(f, "Attempt {id:?} not found")
            }
            Self::HostEvidenceRecoveryIneligible { attempt_id, reason } => {
                write!(
                    f,
                    "host evidence for Attempt {attempt_id:?} is not eligible: {reason}"
                )
            }
            Self::AttemptEvidenceRecoveryConflict { attempt_id } => {
                write!(
                    f,
                    "Attempt {attempt_id:?} already has an active evidence recovery"
                )
            }
            Self::TesterHarnessRepairIneligible { reason } => {
                write!(f, "Tester-harness repair is not eligible: {reason}")
            }
            Self::LegacyLifecycleRecoveryConflict { attempt_id } => write!(
                f,
                "Attempt {attempt_id:?} changed while legacy lifecycle recovery was being prepared"
            ),
            Self::TaskNotFound { id } => {
                write!(f, "Task {id:?} not found")
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
            Self::MergeCandidateNotFound { candidate_id } => {
                write!(f, "Merge Candidate {candidate_id:?} not found")
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
            Self::MergeCandidateReadinessTimestampMismatch {
                candidate_id,
                attempt_id,
            } => write!(
                f,
                "Merge Candidate {candidate_id:?} readiness time does not match Attempt {attempt_id:?} completion time"
            ),
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
            Self::MergeCandidateCorrectionIneligible {
                candidate_id,
                reason,
            } => write!(
                f,
                "Merge Candidate {candidate_id:?} cannot return to correction: {reason}"
            ),
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
            Self::InvalidTaskNoChangeOutput { task_id } => {
                write!(f, "task {task_id} has invalid no-change output")
            }
            Self::InvalidWriterCorrectionAssignment { task_id, reason } => write!(
                f,
                "task {task_id} has invalid Writer correction assignment: {reason}"
            ),
            Self::WriterCorrectionAssignmentModified { task_id } => write!(
                f,
                "task {task_id} has a host-owned Writer correction assignment that was removed or modified"
            ),
            Self::InvalidFailedCommandRecheckTask { task_id } => {
                write!(
                    f,
                    "task {task_id} has invalid failed-command recheck context"
                )
            }
            Self::InvalidTesterHarnessRepairTask { task_id } => {
                write!(
                    f,
                    "task {task_id} has invalid Tester-harness repair context"
                )
            }
            Self::InvalidCandidateAncestryRepairTask { task_id } => {
                write!(
                    f,
                    "task {task_id} has invalid candidate ancestry repair context"
                )
            }
            Self::InvalidCandidateMetadataRepairTask { task_id } => {
                write!(
                    f,
                    "task {task_id} has invalid candidate metadata repair context"
                )
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
            Self::WorkNotExecutionReady { work_item_id } => {
                write!(
                    f,
                    "Work Item {work_item_id:?} is proposed; human authorization is required before an Attempt can be created or run"
                )
            }
            Self::UnsupportedStoredState {
                work_item_id,
                reason,
            } => {
                write!(f, "Work Item {work_item_id:?} cannot be mutated: {reason}")
            }
            Self::InvalidReleaseContract { reason } => {
                write!(f, "invalid release acceptance contract: {reason}")
            }
            Self::InvalidPlanningScope { reason } => {
                write!(f, "invalid planning scope assessment: {reason}")
            }
            Self::InvalidPlanningRevision { reason } => {
                write!(f, "invalid planning revision: {reason}")
            }
            Self::ReleaseContractRequired { work_item_id } => {
                write!(
                    f,
                    "Work Item {work_item_id:?} has no release acceptance contract"
                )
            }
            Self::ReleaseCriterionNotFound { criterion_id } => write!(
                f,
                "release blocker does not map to an accepted release criterion: {criterion_id:?}"
            ),
            Self::ReleaseFindingAlreadyExists { id } => {
                write!(f, "release finding {id:?} already exists")
            }
            Self::CorrectiveContextIncomplete { field } => {
                write!(
                    f,
                    "corrective context is incomplete: field {field:?} must not be empty"
                )
            }
            Self::AttemptLearningNotSucceeded { attempt_id, state } => {
                write!(
                    f,
                    "Attempt {attempt_id:?} cannot advance: its Learner run is {state}, not succeeded"
                )
            }
            Self::FrozenReviewedIdentity { attempt_id, field } => {
                write!(
                    f,
                    "Attempt {attempt_id:?} has an exposed no-expertise handoff: its frozen reviewed \
                     {field} cannot change while Learning is HandoffPending or Succeeded"
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
    StaleWorkItem {
        path: PathBuf,
        id: String,
        expected_revision: Option<u64>,
        actual_revision: u64,
    },
    StorageRevisionExhausted {
        path: PathBuf,
        id: String,
    },
    SnapshotUnavailable {
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
            | Self::StaleWorkItem { path, .. }
            | Self::StorageRevisionExhausted { path, .. }
            | Self::SnapshotUnavailable { path, .. }
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
            Self::StaleWorkItem {
                id,
                expected_revision,
                actual_revision,
                ..
            } => {
                write!(
                    f,
                    "Work Item {id:?} changed after it was read (expected storage revision \
                     {expected_revision:?}, found {actual_revision}); reload before writing"
                )
            }
            Self::StorageRevisionExhausted { id, .. } => {
                write!(
                    f,
                    "Work Item {id:?} exhausted its storage revision; refusing to disable stale-write detection"
                )
            }
            Self::SnapshotUnavailable { id, .. } => write!(
                f,
                "Work Item {id:?} snapshot is unavailable: publication changed or host-owned recovery is pending"
            ),
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
            Self::StaleWorkItem { .. } => None,
            Self::StorageRevisionExhausted { .. } => None,
            Self::SnapshotUnavailable { .. } => None,
            Self::ParseFile { source, .. } => Some(source),
            Self::WorkItemIdMismatch { .. } => None,
            Self::InvalidModel { source, .. } => Some(source),
        }
    }
}

/// Wholly `#[cfg(test)]` faults at real Work-model persistence boundaries.
///
/// Production carries no injectable path: the non-test build authors and applies
/// transactions directly. Test builds can fault one keyed Work Item write before
/// transaction publication, while applying split records, or after the item record
/// publishes but before journal cleanup. Each fault is thread-local, RAII-restored,
/// and consumed once, so concurrent tests and unrelated writes remain unaffected.
#[cfg(test)]
pub(crate) mod persist_fault {
    use std::cell::RefCell;

    thread_local! {
        static FOLLOW_UP_WRITE_FAULT: RefCell<Option<String>> = const { RefCell::new(None) };
        static CREATION_AFTER_PUBLISH_FAULT: RefCell<Option<String>> = const { RefCell::new(None) };
        static SPLIT_WRITE_FAULT: RefCell<Option<String>> = const { RefCell::new(None) };
        static AFTER_ROOT_READ: RefCell<Option<(String, Box<dyn FnOnce()>)>> = const { RefCell::new(None) };
    }

    pub(crate) struct RootReadGuard;

    pub(crate) fn after_next_root_read(id: &str, action: impl FnOnce() + 'static) -> RootReadGuard {
        AFTER_ROOT_READ.with(|slot| *slot.borrow_mut() = Some((id.to_string(), Box::new(action))));
        RootReadGuard
    }

    pub(crate) fn after_root_read(id: &str) {
        let action = AFTER_ROOT_READ.with(|slot| {
            if slot
                .borrow()
                .as_ref()
                .is_some_and(|(expected, _)| expected == id)
            {
                slot.borrow_mut().take().map(|(_, action)| action)
            } else {
                None
            }
        });
        if let Some(action) = action {
            action();
        }
    }

    impl Drop for RootReadGuard {
        fn drop(&mut self) {
            AFTER_ROOT_READ.with(|slot| *slot.borrow_mut() = None);
        }
    }

    /// RAII guard: arm the persistence leaf to fail the next write that persists a
    /// follow-up-processing failure for `work_item_id`. Dropping it disarms.
    pub(crate) struct FollowUpWriteFaultGuard;

    pub(crate) struct CreationAfterPublishFaultGuard;

    pub(crate) struct SplitWriteFaultGuard;

    /// Arm the fault for exactly one follow-up-result write of `work_item_id`.
    pub(crate) fn arm_follow_up_write(work_item_id: &str) -> FollowUpWriteFaultGuard {
        FOLLOW_UP_WRITE_FAULT.with(|fault| *fault.borrow_mut() = Some(work_item_id.to_string()));
        FollowUpWriteFaultGuard
    }

    pub(crate) fn arm_creation_after_publish(work_item_id: &str) -> CreationAfterPublishFaultGuard {
        CREATION_AFTER_PUBLISH_FAULT
            .with(|fault| *fault.borrow_mut() = Some(work_item_id.to_string()));
        CreationAfterPublishFaultGuard
    }

    pub(crate) fn arm_split_write(work_item_id: &str) -> SplitWriteFaultGuard {
        SPLIT_WRITE_FAULT.with(|fault| *fault.borrow_mut() = Some(work_item_id.to_string()));
        SplitWriteFaultGuard
    }

    impl Drop for FollowUpWriteFaultGuard {
        fn drop(&mut self) {
            FOLLOW_UP_WRITE_FAULT.with(|fault| *fault.borrow_mut() = None);
        }
    }

    impl Drop for CreationAfterPublishFaultGuard {
        fn drop(&mut self) {
            CREATION_AFTER_PUBLISH_FAULT.with(|fault| *fault.borrow_mut() = None);
        }
    }

    impl Drop for SplitWriteFaultGuard {
        fn drop(&mut self) {
            SPLIT_WRITE_FAULT.with(|fault| *fault.borrow_mut() = None);
        }
    }

    /// Whether this write must fail: the armed Work Item id matches and the item
    /// being written carries the speculative follow-up-processing failure the
    /// land is trying to persist. Consumes the arming so only that one write fails.
    pub(crate) fn should_fault_follow_up_write(work_item: &super::WorkItem) -> bool {
        FOLLOW_UP_WRITE_FAULT.with(|fault| {
            let armed = fault.borrow().clone();
            let matches = armed.as_deref() == Some(work_item.id.as_str())
                && work_item
                    .merge_candidates
                    .iter()
                    .any(|candidate| candidate.merge_state.follow_up_failure.is_some());
            if matches {
                *fault.borrow_mut() = None;
            }
            matches
        })
    }

    pub(crate) fn should_fault_creation_after_publish(work_item: &super::WorkItem) -> bool {
        CREATION_AFTER_PUBLISH_FAULT.with(|fault| {
            let matches = fault.borrow().as_deref() == Some(work_item.id.as_str());
            if matches {
                *fault.borrow_mut() = None;
            }
            matches
        })
    }

    pub(crate) fn should_fault_split_write(work_item: &super::WorkItem) -> bool {
        SPLIT_WRITE_FAULT.with(|fault| {
            let matches = fault.borrow().as_deref() == Some(work_item.id.as_str());
            if matches {
                *fault.borrow_mut() = None;
            }
            matches
        })
    }
}

#[derive(Debug, Clone)]
pub struct WorkModelStore {
    project_root: PathBuf,
}

/// Published Work records and bounded diagnostics, observed without maintenance.
#[derive(Debug, Default)]
pub struct WorkModelSnapshot {
    pub items: Vec<WorkItem>,
    pub diagnostics: Vec<String>,
}

const WORK_MODEL_WRITER_ENVELOPE_FORMAT: &str = "fluent-work-item-writer";
// Fence both installation source lineages: neither predecessor preserves the
// union of revision audits, assignments, and separately authorized retries.
const WORK_MODEL_WRITER_ENVELOPE_SCHEMA_VERSION: u32 = 12;
const NO_CHANGE_COMPLETION_WRITER_ENVELOPE_SCHEMA_VERSION: u32 = 13;
const CLAUDE_SESSION_WRITER_ENVELOPE_SCHEMA_VERSION: u32 = 14;

fn is_supported_writer_envelope_schema(version: u32) -> bool {
    matches!(version, 1..=CLAUDE_SESSION_WRITER_ENVELOPE_SCHEMA_VERSION)
}

/// Fence readers that deserialize the existing writer field as Option<String>.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
enum StoredModelWriterVersion {
    Legacy(String),
    Envelope {
        format: String,
        schema_version: u32,
        version: String,
    },
}

fn deserialize_model_writer_version<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<StoredModelWriterVersion>, D::Error> {
    let writer = Option::<StoredModelWriterVersion>::deserialize(deserializer)?;
    if let Some(StoredModelWriterVersion::Envelope {
        format,
        schema_version,
        version,
    }) = &writer
    {
        if format != WORK_MODEL_WRITER_ENVELOPE_FORMAT
            || !is_supported_writer_envelope_schema(*schema_version)
        {
            return Err(serde::de::Error::custom(
                "unsupported Work Item writer envelope format/schema",
            ));
        }
        if version_triplet(version).is_none() {
            return Err(serde::de::Error::custom(
                "invalid Work Item writer envelope version",
            ));
        }
    }
    Ok(writer)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct WorkItemRecord {
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    schema_version: u32,
    id: String,
    title: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_model_writer_version"
    )]
    model_writer_version: Option<StoredModelWriterVersion>,
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    storage_revision: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    planning_context: Option<PlanningContext>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    planning_scope: Option<PlanningScopeAssessment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    release_contract: Option<ReleaseContract>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    instructions: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    abandonment: Option<WorkItemAbandonment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    post_merge_review_fix_depth: Option<u64>,
    #[serde(default, skip_serializing_if = "LearnerMode::is_capture")]
    learner_mode: LearnerMode,
    #[serde(default, skip_serializing_if = "WorkOrigin::is_planned")]
    origin: WorkOrigin,
    #[serde(
        default,
        skip_serializing_if = "ExecutionAuthorization::is_unattributed_ready"
    )]
    authorization: ExecutionAuthorization,
    #[serde(default, skip_serializing_if = "WorkLineage::is_uncharged_root")]
    lineage: WorkLineage,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    corrective_context: Option<CorrectiveContext>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    corrective_audit: Option<CorrectiveAuditContext>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pending_enqueue: Option<EnqueueIntent>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    input_artifacts: Vec<WorkItemInputArtifact>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    transitions: Vec<DurableWorkTransition>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct WorkItemTransaction {
    schema_version: u32,
    id: String,
    base_revision: Option<u64>,
    target_revision: u64,
    /// Prove that stored children still belong to the base revision during an
    /// interrupted publication. Legacy journals cannot supply this proof.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    base_children_digest: Option<String>,
    /// Managed sidecars are journaled before any split child or root record is
    /// published. Legacy journals omit this field and remain recoverable.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    managed_files: Vec<ManagedWorkItemFile>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    usage_limit_publications: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    context_retry_publications: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    context_retry_base: Option<WorkItem>,
    /// Retain the authorized base only for a context-recovery publication. Its
    /// digest lets replay validate children that already contain target records.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    assignment_context_base: Option<WorkItem>,
    work_item: WorkItem,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StoredRecordVersions {
    attempts: Vec<(String, u32)>,
}

impl WorkItemTransaction {
    // Source journals lack either managed revision files or retry authority.
    // Preserve their modeled formats, but publish the combined format separately.
    const SCHEMA_VERSION: u32 = 14;
    const NO_CHANGE_COMPLETION_SCHEMA_VERSION: u32 = 15;
    const CLAUDE_SESSION_SCHEMA_VERSION: u32 = 16;

    fn is_supported_schema(version: u32) -> bool {
        matches!(version, 1..=Self::CLAUDE_SESSION_SCHEMA_VERSION)
    }

    /// Keep the base and operation list mandatory when retry authority exists.
    /// Defaults read predecessor journals; they must not erase present proof.
    fn validate_context_retry(&self) -> Result<(), WorkModelError> {
        let check = || -> anyhow::Result<()> {
            use crate::recovery_contracts::context_retry::pending_publications;
            if pending_publications(&self.work_item, None).is_empty() {
                anyhow::ensure!(
                    self.context_retry_base.is_none() && self.context_retry_publications.is_empty(),
                    "unexpected retry journal metadata"
                );
                return Ok(());
            }
            let base = self
                .context_retry_base
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("retry journal lost its authorized base"))?;
            anyhow::ensure!(
                self.schema_version >= 9
                    && base.id == self.id
                    && self.base_children_digest.as_deref()
                        == Some(work_item_children_digest(base)?.as_str()),
                "retry journal base changed"
            );
            base.ensure_mutation_compatible()?;
            base.validate()?;
            anyhow::ensure!(
                self.context_retry_publications
                    == pending_publications(&self.work_item, Some(base)),
                "retry journal publication list changed"
            );
            Ok(())
        };
        check().map_err(|error| WorkModelError::InvalidRecoveryOperation {
            attempt_id: self.id.clone(),
            reason: format!("invalid context retry journal: {error:#}"),
        })
    }
}

fn is_zero_u64(value: &u64) -> bool {
    *value == 0
}

fn is_zero_usize(value: &usize) -> bool {
    *value == 0
}

fn is_zero_u32(value: &u32) -> bool {
    *value == 0
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct AttemptRecord {
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    schema_version: u32,
    id: String,
    work_item_id: String,
    #[serde(default, skip_serializing_if = "attempt_kind_is_write")]
    kind: AttemptKind,
    #[serde(default)]
    order: usize,
    status: AttemptStatus,
    #[serde(default)]
    coder_mapping: CoderMapping,
    #[serde(default, skip_serializing_if = "TerminalMode::is_headless")]
    terminal_mode: TerminalMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    review_state: Option<AttemptReviewState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pause_kind: Option<PauseKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pause_recovery: Option<PauseRecovery>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pause_record: Option<PauseRecord>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    recovery_operations: Vec<RecoveryOperationRecord>,
    #[serde(default)]
    artifacts: Vec<ArtifactRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    evidence_recoveries: Vec<EvidenceRecovery>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    approach_revision_superseded_tasks: Vec<ApproachRevisionSupersession>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    writer_runs: Vec<WriterRun>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cancellation: Option<AttemptCancellation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    created_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    completed_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    learning: Option<AttemptLearning>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    progress_contract: Option<ProgressContract>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    writer_completion_contract: Option<WriterCompletionContract>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    round_cap_extensions: Vec<RoundCapExtension>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct TaskRecord {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    order: Option<usize>,
    #[serde(flatten)]
    task: Task,
}

fn attempt_has_claude_session_authority(attempt: &Attempt) -> bool {
    attempt.recovery_operations.iter().any(|operation| {
        matches!(
            operation.operation_kind.as_str(),
            crate::recovery_contracts::claude_session::APPROVE
                | crate::recovery_contracts::claude_session::RESUME
                | crate::recovery_contracts::claude_session::START
        )
    })
}

fn has_claude_session_authority(item: &WorkItem) -> bool {
    item.attempts
        .iter()
        .any(attempt_has_claude_session_authority)
}

fn attempt_has_no_change_completion_authority(attempt: &Attempt) -> bool {
    attempt.recovery_operations.iter().any(|operation| {
        operation
            .operation_kind
            .starts_with("writer-completion-no-change-")
    })
}

fn has_no_change_completion_authority(work_item: &WorkItem) -> bool {
    work_item
        .attempts
        .iter()
        .any(attempt_has_no_change_completion_authority)
}

impl From<&WorkItem> for WorkItemRecord {
    fn from(work_item: &WorkItem) -> Self {
        let no_change_authority = has_no_change_completion_authority(work_item);
        Self {
            schema_version: if has_claude_session_authority(work_item) {
                CLAUDE_SESSION_RECORD_SCHEMA_VERSION
            } else if no_change_authority {
                NO_CHANGE_COMPLETION_RECORD_SCHEMA_VERSION
            } else {
                WORK_MODEL_RECORD_SCHEMA_VERSION
            },
            id: work_item.id.clone(),
            title: work_item.title.clone(),
            model_writer_version: Some(StoredModelWriterVersion::Envelope {
                format: WORK_MODEL_WRITER_ENVELOPE_FORMAT.to_string(),
                schema_version: if has_claude_session_authority(work_item) {
                    CLAUDE_SESSION_WRITER_ENVELOPE_SCHEMA_VERSION
                } else if no_change_authority {
                    NO_CHANGE_COMPLETION_WRITER_ENVELOPE_SCHEMA_VERSION
                } else {
                    WORK_MODEL_WRITER_ENVELOPE_SCHEMA_VERSION
                },
                version: WORK_MODEL_WRITER_VERSION.to_string(),
            }),
            storage_revision: work_item.storage_revision.get().unwrap_or(0),
            planning_context: work_item.planning_context.clone(),
            planning_scope: work_item.planning_scope.clone(),
            release_contract: work_item.release_contract.clone(),
            instructions: work_item.instructions.clone(),
            abandonment: work_item.abandonment.clone(),
            post_merge_review_fix_depth: work_item.post_merge_review_fix_depth,
            learner_mode: work_item.learner_mode,
            origin: work_item.origin.clone(),
            authorization: work_item.authorization,
            lineage: work_item.lineage.clone(),
            corrective_context: work_item.corrective_context.clone(),
            corrective_audit: work_item.corrective_audit.clone(),
            pending_enqueue: work_item.pending_enqueue.clone(),
            input_artifacts: work_item.input_artifacts.clone(),
            transitions: work_item.transitions.clone(),
        }
    }
}

impl From<WorkItemRecord> for WorkItem {
    fn from(record: WorkItemRecord) -> Self {
        Self {
            id: record.id,
            title: record.title,
            model_writer_version: record.model_writer_version.map(|writer| match writer {
                StoredModelWriterVersion::Legacy(version)
                | StoredModelWriterVersion::Envelope { version, .. } => version,
            }),
            storage_revision: StorageRevision::persisted(record.storage_revision),
            planning_context: record.planning_context,
            planning_scope: record.planning_scope,
            release_contract: record.release_contract,
            instructions: record.instructions,
            abandonment: record.abandonment,
            post_merge_review_fix_depth: record.post_merge_review_fix_depth,
            learner_mode: record.learner_mode,
            origin: record.origin,
            authorization: record.authorization,
            lineage: record.lineage,
            corrective_context: record.corrective_context,
            corrective_audit: record.corrective_audit,
            pending_enqueue: record.pending_enqueue,
            input_artifacts: record.input_artifacts,
            attempts: Vec::new(),
            merge_candidates: Vec::new(),
            transitions: record.transitions,
        }
    }
}

impl AttemptRecord {
    fn from_attempt(attempt: &Attempt, order: usize) -> Self {
        Self {
            schema_version: if attempt_has_claude_session_authority(attempt) {
                CLAUDE_SESSION_RECORD_SCHEMA_VERSION
            } else if attempt_has_no_change_completion_authority(attempt) {
                NO_CHANGE_COMPLETION_RECORD_SCHEMA_VERSION
            } else {
                WORK_MODEL_RECORD_SCHEMA_VERSION
            },
            id: attempt.id.clone(),
            work_item_id: attempt.work_item_id.clone(),
            kind: attempt.kind.clone(),
            order,
            status: attempt.status.clone(),
            coder_mapping: attempt.coder_mapping.clone(),
            terminal_mode: attempt.terminal_mode,
            review_state: attempt.review_state.clone(),
            pause_kind: attempt.pause_kind.clone(),
            pause_recovery: attempt.pause_recovery,
            pause_record: attempt.pause_record.clone(),
            recovery_operations: attempt.recovery_operations.clone(),
            artifacts: attempt.artifacts.clone(),
            evidence_recoveries: attempt.evidence_recoveries.clone(),
            approach_revision_superseded_tasks: attempt.approach_revision_superseded_tasks.clone(),
            cancellation: attempt.cancellation.clone(),
            writer_runs: attempt.writer_runs.clone(),
            created_at: attempt.created_at.clone(),
            completed_at: attempt.completed_at.clone(),
            learning: attempt.learning.clone(),
            progress_contract: attempt.progress_contract.clone(),
            writer_completion_contract: attempt.writer_completion_contract.clone(),
            round_cap_extensions: attempt.round_cap_extensions.clone(),
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
            terminal_mode: record.terminal_mode,
            tasks: Vec::new(),
            review_state: record.review_state,
            pause_kind: record.pause_kind,
            pause_recovery: record.pause_recovery,
            pause_record: record.pause_record,
            recovery_operations: record.recovery_operations,
            artifacts: record.artifacts,
            evidence_recoveries: record.evidence_recoveries,
            approach_revision_superseded_tasks: record.approach_revision_superseded_tasks,
            cancellation: record.cancellation,
            writer_runs: record.writer_runs,
            created_at: record.created_at,
            completed_at: record.completed_at,
            learning: record.learning,
            progress_contract: record.progress_contract,
            writer_completion_contract: record.writer_completion_contract,
            round_cap_extensions: record.round_cap_extensions,
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

fn reject_canceled_attempt_launches(
    prior: &WorkItem,
    target: &WorkItem,
    path: &Path,
) -> Result<(), WorkModelStorageError> {
    for next in &target.attempts {
        let Some(previous) = prior.attempts.iter().find(|attempt| attempt.id == next.id) else {
            continue;
        };
        let lost_stop = previous.cancellation_requested()
            && next.cancellation.as_ref().is_none_or(|stop| {
                stop.requested_at != previous.cancellation.as_ref().unwrap().requested_at
                    || (stop.resumed_at.is_some()
                        && previous.cancellation.as_ref().unwrap().settled_at.is_none())
            });
        let starts_task = next.tasks.iter().any(|task| {
            previous
                .tasks
                .iter()
                .find(|prior| prior.id == task.id)
                .is_none_or(|prior| {
                    task.status != prior.status
                        && matches!(task.status, TaskStatus::Executing | TaskStatus::Complete)
                })
        });
        let advances_learning = next.learning != previous.learning
            && next.learning.as_ref().is_some_and(|learning| {
                matches!(
                    learning.status,
                    LearningStatus::InProgress
                        | LearningStatus::HandoffPending
                        | LearningStatus::Succeeded
                )
            });
        // A pending reviewed candidate keeps its Attempt Complete during a
        // Learner stop. Permit that representation only while preserving the
        // existing candidate exactly, including its readiness and merge state.
        let preserves_pending_candidate = next.status == AttemptStatus::Complete
            && next.pause_kind.is_none()
            && prior.merge_candidates.iter().any(|candidate| {
                candidate.attempt_id == next.id
                    && candidate.merge_state.status == MergeCandidateMergeStatus::Pending
                    && target.merge_candidates.iter().any(|next| next == candidate)
            });
        if lost_stop
            || (next.cancellation_requested()
                && ((!preserves_pending_candidate
                    && (next.status != AttemptStatus::NeedsUser
                        || next.pause_kind != Some(PauseKind::Interrupted)))
                    || starts_task
                    || advances_learning))
        {
            return Err(WorkModelStorageError::InvalidModel {
                path: path.to_path_buf(),
                source: WorkModelError::AttemptResumeIneligible {
                    attempt_id: next.id.clone(),
                    reason:
                        "durable cancellation prevents downstream execution until explicit resume"
                            .into(),
                },
            });
        }
    }
    Ok(())
}

fn reject_writer_correction_assignment_change(
    project_root: &Path,
    prior: &WorkItem,
    target: &WorkItem,
    path: &Path,
) -> Result<(), WorkModelStorageError> {
    for prior_attempt in &prior.attempts {
        let Some(target_attempt) = target
            .attempts
            .iter()
            .find(|attempt| attempt.id == prior_attempt.id)
        else {
            if prior_attempt
                .tasks
                .iter()
                .any(|task| task.writer_correction_assignment.is_some())
            {
                let task_id = prior_attempt
                    .tasks
                    .iter()
                    .find(|task| task.writer_correction_assignment.is_some())
                    .map(|task| task.id.clone())
                    .unwrap_or_else(|| prior_attempt.id.clone());
                return Err(WorkModelStorageError::InvalidModel {
                    path: path.to_path_buf(),
                    source: WorkModelError::WriterCorrectionAssignmentModified { task_id },
                });
            }
            continue;
        };
        let prior_recoveries = prior_attempt
            .recovery_operations
            .iter()
            .filter(|r| {
                r.operation_kind.starts_with("usage-limit-writer-")
                    || r.operation_kind == "assignment-context-recovery"
                    || r.operation_kind.starts_with("writer-completion-context-")
            })
            .collect::<Vec<_>>();
        let target_recoveries = target_attempt
            .recovery_operations
            .iter()
            .filter(|r| {
                r.operation_kind.starts_with("usage-limit-writer-")
                    || r.operation_kind == "assignment-context-recovery"
                    || r.operation_kind.starts_with("writer-completion-context-")
            })
            .collect::<Vec<_>>();
        if !target_recoveries.starts_with(&prior_recoveries) {
            return Err(WorkModelStorageError::InvalidModel {
                path: path.to_path_buf(),
                source: WorkModelError::InvalidRecoveryOperation {
                    attempt_id: prior_attempt.id.clone(),
                    reason: "usage-limit recovery operations are append-only".into(),
                },
            });
        }
        for prior_task in &prior_attempt.tasks {
            let Some(prior_assignment) = prior_task.writer_correction_assignment.as_ref() else {
                continue;
            };
            let Some(target_task) = target_attempt
                .tasks
                .iter()
                .find(|task| task.id == prior_task.id)
            else {
                return Err(WorkModelStorageError::InvalidModel {
                    path: path.to_path_buf(),
                    source: WorkModelError::WriterCorrectionAssignmentModified {
                        task_id: prior_task.id.clone(),
                    },
                });
            };
            if target_task.writer_correction_assignment.as_ref() != Some(prior_assignment)
                && !crate::recovery_contracts::audited_assignment_context_change(
                    project_root,
                    prior_attempt,
                    target_attempt,
                    prior_task,
                    target_task,
                )
                .unwrap_or(false)
            {
                return Err(WorkModelStorageError::InvalidModel {
                    path: path.to_path_buf(),
                    source: WorkModelError::WriterCorrectionAssignmentModified {
                        task_id: prior_task.id.clone(),
                    },
                });
            }
        }
    }
    Ok(())
}

impl WorkModelStore {
    pub fn new(project_root: impl Into<PathBuf>) -> Self {
        Self {
            project_root: project_root.into(),
        }
    }

    pub(crate) fn project_root(&self) -> &Path {
        &self.project_root
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

    fn work_transactions_dir(&self) -> PathBuf {
        self.work_dir().join(WORK_TRANSACTIONS_DIR)
    }

    fn work_transaction_path(&self, id: &str) -> Result<PathBuf, WorkModelStorageError> {
        work_item_file_name(id).map(|file_name| self.work_transactions_dir().join(file_name))
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

    /// Read published records without locks, transaction recovery, or migration.
    /// A journal describes unfinished host work, never a replacement snapshot.
    pub fn snapshot_work_items(&self) -> Result<WorkModelSnapshot, WorkModelStorageError> {
        let mut snapshot = WorkModelSnapshot::default();
        for dir in [self.work_items_dir(), self.work_transactions_dir()] {
            let paths = match list_json_paths(&dir) {
                Ok(paths) => paths,
                Err(WorkModelStorageError::ReadDirectory { source, .. })
                    if source.kind() == io::ErrorKind::NotFound =>
                {
                    continue;
                }
                Err(error) if dir == self.work_items_dir() => return Err(error),
                Err(error) => {
                    snapshot
                        .diagnostics
                        .push(bounded_transition_text(&error.to_string(), 512));
                    continue;
                }
            };
            for path in paths {
                if dir == self.work_items_dir() {
                    match self.read_work_item_snapshot_file(&path) {
                        Ok(item) => snapshot.items.push(item),
                        Err(error) => snapshot
                            .diagnostics
                            .push(bounded_transition_text(&error.to_string(), 512)),
                    }
                } else {
                    let result =
                        file_stem_id(&path).and_then(|id| self.read_work_item_transaction(&id));
                    let detail = match result {
                        Ok(_) => String::new(),
                        Err(error) => format!(": {error}"),
                    };
                    snapshot.diagnostics.push(bounded_transition_text(
                        &format!(
                            "Work transaction {}: host-owned recovery is pending{detail}",
                            path.display()
                        ),
                        512,
                    ));
                }
            }
        }
        Ok(snapshot)
    }

    /// Read one published aggregate without performing host-owned maintenance.
    pub fn read_work_item_snapshot(&self, id: &str) -> Result<WorkItem, WorkModelStorageError> {
        self.read_work_item_snapshot_file(&self.work_item_path(id)?)
    }

    /// Inspect a valid interrupted target without publishing any of its records.
    pub(crate) fn read_work_item_recovery_target(
        &self,
        id: &str,
    ) -> Result<Option<WorkItem>, WorkModelStorageError> {
        Ok(self
            .read_work_item_transaction(id)?
            .map(|transaction| transaction.work_item))
    }

    fn read_work_item_snapshot_file(&self, path: &Path) -> Result<WorkItem, WorkModelStorageError> {
        let id = file_stem_id(path)?;
        let unavailable = || WorkModelStorageError::SnapshotUnavailable {
            path: path.to_path_buf(),
            id: id.clone(),
        };
        // A completed transaction may disappear between child reads and the
        // journal scan. Check both publication markers around every assembly.
        for _ in 0..3 {
            let before: WorkItemRecord = read_json_file(path)?;
            let transaction_before = self
                .read_work_item_transaction(&id)
                .map_err(|_| unavailable())?;
            let assembled = self.read_published_work_item_file(path, true);
            let after: WorkItemRecord = match read_json_file(path) {
                Ok(record) => record,
                Err(_) => continue,
            };
            let transaction_after = self
                .read_work_item_transaction(&id)
                .map_err(|_| unavailable())?;
            if before != after || transaction_before != transaction_after {
                continue;
            }
            let item = assembled?;
            if let Some(transaction) = transaction_after {
                let expected_digest = if Some(after.storage_revision) == transaction.base_revision {
                    transaction.base_children_digest
                } else if after.storage_revision == transaction.target_revision {
                    if !self.managed_files_are_published(&transaction.managed_files)? {
                        return Err(unavailable());
                    }
                    Some(work_item_children_digest(&transaction.work_item)?)
                } else {
                    None
                };
                if expected_digest.as_deref() != Some(work_item_children_digest(&item)?.as_str()) {
                    return Err(unavailable());
                }
            }
            return Ok(item);
        }
        Err(unavailable())
    }

    pub fn list_work_items(&self) -> Result<Vec<WorkItem>, WorkModelStorageError> {
        self.list_work_item_paths()?
            .into_iter()
            .map(|path| self.read_work_item_file(&path, true))
            .collect()
    }

    /// List Work Item IDs without deserializing their records. The upgrade
    /// command uses this so one newer/unknown record does not get guessed at
    /// merely while discovering the remaining IDs.
    pub fn list_work_item_ids(&self) -> Result<Vec<String>, WorkModelStorageError> {
        self.list_work_item_paths()?
            .into_iter()
            .map(|path| file_stem_id(&path))
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
        self.recover_pending_transactions()?;
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

    /// Convert one legacy Work Item and all of its split Attempt records to the
    /// current storage format. The operation is deliberately explicit: normal
    /// reads still accept older records, while this method records the source
    /// versions and rewrites every record through the normal transaction path.
    /// Repeating it after success reports `already_current` and performs no
    /// second rewrite.
    pub fn upgrade_work_item(
        &self,
        id: &str,
    ) -> Result<WorkModelUpgradeReport, WorkModelStorageError> {
        let path = self.work_item_path(id)?;
        let _lock = self.lock_work_item_model(id)?;
        self.recover_work_item_transaction(id)?;

        let stored = read_json_file::<WorkItemRecord>(&path)?;
        ensure_supported_record_schema(&path, "Work Item", stored.schema_version)?;
        let versions = self.read_stored_record_versions_unlocked(id)?;
        let item = self.read_work_item_file_unlocked(&path, true)?;
        item.ensure_mutation_compatible().map_err(|source| {
            WorkModelStorageError::InvalidModel {
                path: path.clone(),
                source,
            }
        })?;
        if let Some(writer) = item.model_writer_version.as_deref()
            && version_triplet(writer).is_none()
        {
            return Err(WorkModelStorageError::InvalidModel {
                path,
                source: WorkModelError::InvalidStoredWriterVersion {
                    version: writer.to_string(),
                },
            });
        }

        let mut upgraded = item.clone();
        let artifact_migrations = Self::plan_work_artifact_migrations(&mut upgraded);
        let current_schema = if has_claude_session_authority(&item) {
            CLAUDE_SESSION_RECORD_SCHEMA_VERSION
        } else if has_no_change_completion_authority(&item) {
            NO_CHANGE_COMPLETION_RECORD_SCHEMA_VERSION
        } else {
            WORK_MODEL_RECORD_SCHEMA_VERSION
        };
        let legacy_attempts = versions
            .attempts
            .iter()
            .filter_map(|(attempt_id, version)| {
                let attempt = item
                    .attempts
                    .iter()
                    .find(|attempt| attempt.id == *attempt_id)
                    .expect("loaded Attempt matches its persisted record");
                let target_schema = if attempt_has_claude_session_authority(attempt) {
                    CLAUDE_SESSION_RECORD_SCHEMA_VERSION
                } else if attempt_has_no_change_completion_authority(attempt) {
                    NO_CHANGE_COMPLETION_RECORD_SCHEMA_VERSION
                } else {
                    WORK_MODEL_RECORD_SCHEMA_VERSION
                };
                (*version != target_schema).then(|| WorkModelAttemptUpgrade {
                    attempt_id: attempt_id.clone(),
                    from_record_schema_version: *version,
                    to_record_schema_version: target_schema,
                })
            })
            .collect::<Vec<_>>();
        let needs_upgrade = stored.schema_version != current_schema
            || !matches!(
                stored.model_writer_version,
                Some(StoredModelWriterVersion::Envelope { .. })
            )
            || !artifact_migrations.is_empty()
            || !legacy_attempts.is_empty()
            || item.model_writer_version.as_deref() != Some(WORK_MODEL_WRITER_VERSION);
        if !needs_upgrade {
            return Ok(WorkModelUpgradeReport {
                work_item_id: id.to_string(),
                operation_id: None,
                from_model_writer_version: item.model_writer_version.clone(),
                from_record_schema_version: stored.schema_version,
                upgraded_attempts: Vec::new(),
                audit_path: None,
                already_current: true,
            });
        }

        let operation_id = model_upgrade_operation_id(
            id,
            item.model_writer_version.as_deref(),
            stored.schema_version,
            &versions.attempts,
        );
        let audit_relative = model_upgrade_audit_path(id, &operation_id);
        let audit_path = self.project_root.join(&audit_relative);
        let audit = WorkModelUpgradeRecord {
            schema_version: WORK_MODEL_UPGRADE_SCHEMA_VERSION,
            operation_id: operation_id.clone(),
            work_item_id: id.to_string(),
            from_model_writer_version: item.model_writer_version.clone(),
            to_model_writer_version: WORK_MODEL_WRITER_VERSION.to_string(),
            from_record_schema_version: stored.schema_version,
            to_record_schema_version: current_schema,
            attempts: legacy_attempts.clone(),
            artifact_migrations: artifact_migrations.clone(),
            upgraded_at: now_iso8601(),
        };
        if audit_path.exists() {
            let existing = read_json_file::<WorkModelUpgradeRecord>(&audit_path)?;
            if existing.schema_version != WORK_MODEL_UPGRADE_SCHEMA_VERSION
                || existing.operation_id != audit.operation_id
                || existing.work_item_id != audit.work_item_id
                || existing.from_model_writer_version != audit.from_model_writer_version
                || existing.to_model_writer_version != audit.to_model_writer_version
                || existing.from_record_schema_version != audit.from_record_schema_version
                || existing.to_record_schema_version != audit.to_record_schema_version
                || existing.attempts != audit.attempts
                || existing.artifact_migrations != audit.artifact_migrations
            {
                return Err(WorkModelStorageError::InvalidModel {
                    path: audit_path,
                    source: WorkModelError::WorkModelUpgradeConflict {
                        work_item_id: id.to_string(),
                        operation_id,
                    },
                });
            }
        } else {
            write_json_file(&audit_path, &audit)?;
        }

        for migration in &artifact_migrations {
            self.migrate_artifact_path(&migration.from_path, &migration.to_path)?;
        }
        upgraded.model_writer_version = Some(WORK_MODEL_WRITER_VERSION.to_string());
        upgraded.synchronize_pause_records(&self.project_root);
        upgraded
            .validate()
            .map_err(|source| WorkModelStorageError::InvalidModel {
                path: self.work_item_path(id).unwrap_or_default(),
                source,
            })?;
        self.write_work_item_file_unchecked_for_upgrade(&upgraded)?;

        Ok(WorkModelUpgradeReport {
            work_item_id: id.to_string(),
            operation_id: Some(audit.operation_id),
            from_model_writer_version: audit.from_model_writer_version,
            from_record_schema_version: audit.from_record_schema_version,
            upgraded_attempts: audit.attempts,
            audit_path: Some(audit_relative),
            already_current: false,
        })
    }

    fn read_stored_record_versions_unlocked(
        &self,
        work_item_id: &str,
    ) -> Result<StoredRecordVersions, WorkModelStorageError> {
        let path = self.work_item_path(work_item_id)?;
        let record = read_json_file::<WorkItemRecord>(&path)?;
        ensure_supported_record_schema(&path, "Work Item", record.schema_version)?;
        let attempts_dir = self.work_attempts_dir().join(work_item_id);
        let mut attempts = Vec::new();
        if attempts_dir.exists() {
            for path in list_json_paths(&attempts_dir)? {
                let record = read_json_file::<AttemptRecord>(&path)?;
                ensure_supported_record_schema(&path, "Attempt", record.schema_version)?;
                attempts.push((record.id, record.schema_version));
            }
        }
        attempts.sort_by(|left, right| left.0.cmp(&right.0));
        Ok(StoredRecordVersions { attempts })
    }

    /// Read a Work Item while the caller holds its model lock.
    pub(crate) fn read_work_item_under_model_lock(
        &self,
        id: &str,
    ) -> Result<WorkItem, WorkModelStorageError> {
        let path = self.work_item_path(id)?;
        self.recover_work_item_transaction(id)?;
        self.read_work_item_file_unlocked(&path, true)
    }

    pub(crate) fn read_work_item_for_merge_recovery(
        &self,
        id: &str,
    ) -> Result<WorkItem, WorkModelStorageError> {
        let path = self.work_item_path(id)?;
        self.read_work_item_file(&path, false)
    }

    pub fn create_work_item(&self, work_item: &WorkItem) -> Result<(), WorkModelStorageError> {
        let metadata = work_item
            .authorization
            .is_proposed()
            .then(fluent_transition_metadata);
        self.write_work_item_file(work_item, true, metadata.as_ref())
    }

    /// Create a Work Item and atomically attribute its initial lifecycle state.
    pub fn create_work_item_with_transition(
        &self,
        work_item: &WorkItem,
        metadata: &DurableTransitionMetadata,
    ) -> Result<(), WorkModelStorageError> {
        self.write_work_item_file(work_item, true, Some(metadata))
    }

    /// Preserve approved managed input files and publish their Work Item through
    /// one per-item serialized ownership boundary.
    pub fn create_work_item_with_inputs(
        &self,
        work_item: &mut WorkItem,
        requested: &[String],
    ) -> Result<(), anyhow::Error> {
        validate_work_item_id(&work_item.id)?;
        let canonical_project = fs::canonicalize(&self.project_root).map_err(|error| {
            anyhow::anyhow!(
                "Failed to resolve project root {}: {error}",
                self.project_root.display()
            )
        })?;
        let managed_roots = [WORK_ARTIFACTS_DIR, WORK_PROGRESS_DIR]
            .iter()
            .filter_map(|root| fs::canonicalize(canonical_project.join(root)).ok())
            .collect::<Vec<_>>();
        let mut approved = Vec::new();
        for value in requested {
            let requested_path = Path::new(value);
            let joined = if requested_path.is_absolute() {
                requested_path.to_path_buf()
            } else {
                canonical_project.join(requested_path)
            };
            let canonical = fs::canonicalize(&joined).map_err(|error| {
                anyhow::anyhow!(
                    "Work Item input artifact does not exist: {}: {error}",
                    joined.display()
                )
            })?;
            if !canonical.is_file() {
                return Err(anyhow::anyhow!(
                    "Work Item input artifact is not a regular file: {}",
                    joined.display()
                ));
            }
            if !managed_roots.iter().any(|root| canonical.starts_with(root)) {
                return Err(anyhow::anyhow!(
                    "Work Item input artifact must stay under .fluent/work/artifacts/ or .fluent/work/progress/: {}",
                    joined.display()
                ));
            }
            let source_path = canonical
                .strip_prefix(&canonical_project)
                .map_err(|error| {
                    anyhow::anyhow!(
                        "Managed input escaped project root: {}: {error}",
                        joined.display()
                    )
                })?
                .to_string_lossy()
                .to_string();
            let bytes = fs::read(&canonical).map_err(|error| {
                anyhow::anyhow!(
                    "Failed to read Work Item input artifact {}: {error}",
                    canonical.display()
                )
            })?;
            approved.push((source_path, canonical, bytes));
        }

        let mut target = work_item.clone();
        target.input_artifacts = approved
            .iter()
            .enumerate()
            .map(|(index, (source_path, canonical, bytes))| {
                let filename = canonical
                    .file_name()
                    .and_then(|name| name.to_str())
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "Work Item input filename is not UTF-8: {}",
                            canonical.display()
                        )
                    })?;
                Ok(WorkItemInputArtifact {
                    source_path: source_path.clone(),
                    snapshot_path: work_item_input_path(&target.id, index, filename),
                    digest: format!("sha256:{:x}", Sha256::digest(bytes)),
                })
            })
            .collect::<Result<Vec<_>, anyhow::Error>>()?;
        let item_path = self.work_item_path(&target.id)?;
        target
            .validate()
            .map_err(|source| WorkModelStorageError::InvalidModel {
                path: item_path.clone(),
                source,
            })?;

        let _lock = self.lock_work_item_model(&target.id)?;
        self.recover_work_item_transaction(&target.id)?;
        if item_path.exists() {
            return Err(WorkModelStorageError::WorkItemAlreadyExists {
                path: item_path,
                id: target.id.clone(),
            }
            .into());
        }

        let artifact_root = canonical_project.join(WORK_ARTIFACTS_DIR).join(&target.id);
        if !approved.is_empty() && artifact_root.exists() {
            return Err(anyhow::anyhow!(
                "Work Item artifact root already exists: {}",
                artifact_root.display()
            ));
        }
        if !approved.is_empty() {
            let input_root = artifact_root.join("inputs");
            fs::create_dir_all(&input_root).map_err(|error| {
                anyhow::anyhow!(
                    "Failed to create Work Item input directory {}: {error}",
                    input_root.display()
                )
            })?;
            for ((_, canonical, bytes), identity) in approved.iter().zip(&target.input_artifacts) {
                let snapshot = canonical_project.join(&identity.snapshot_path);
                if let Err(error) = crate::atomic_write::atomic_write(&snapshot, bytes) {
                    let rollback = fs::remove_dir_all(&artifact_root);
                    return match rollback {
                        Ok(()) => Err(anyhow::anyhow!(
                            "Failed to preserve Work Item input {}: {error}",
                            canonical.display()
                        )),
                        Err(rollback_error) => Err(anyhow::anyhow!(
                            "Failed to preserve Work Item input {}: {error}; failed to remove partial input tree {}: {rollback_error}",
                            canonical.display(),
                            artifact_root.display()
                        )),
                    };
                }
            }
        }

        if let Err(error) = self.write_work_item_file_unchecked(&target, true) {
            let transaction_path = self.work_transaction_path(&target.id)?;
            if !item_path.exists() && !transaction_path.exists() && artifact_root.exists() {
                fs::remove_dir_all(&artifact_root).map_err(|rollback_error| {
                    anyhow::anyhow!(
                        "{error}; failed to remove unpublished input tree {}: {rollback_error}",
                        artifact_root.display()
                    )
                })?;
            }
            return Err(error.into());
        }
        *work_item = target;
        Ok(())
    }

    pub fn write_work_item(&self, work_item: &WorkItem) -> Result<(), WorkModelStorageError> {
        self.write_work_item_file(work_item, false, Some(&fluent_transition_metadata()))
    }

    /// Replace an aggregate and atomically attribute any derived transitions.
    pub fn write_work_item_with_transition(
        &self,
        work_item: &WorkItem,
        metadata: &DurableTransitionMetadata,
    ) -> Result<(), WorkModelStorageError> {
        self.write_work_item_file(work_item, false, Some(metadata))
    }

    /// Mutate an aggregate on behalf of an explicitly identified external caller.
    ///
    /// Keeping this provenance on a named API prevents internal lifecycle owners
    /// from being mislabeled as external merely because they use typed metadata.
    pub fn mutate_work_item_from_external<T>(
        &self,
        id: &str,
        reason: impl Into<String>,
        evidence_ref: Option<String>,
        reducer: impl FnOnce(&mut WorkItem) -> Result<T, WorkModelError>,
    ) -> Result<T, WorkModelStorageError> {
        self.mutate_work_item_with_transition(
            id,
            &DurableTransitionMetadata {
                origin: DurableTransitionOrigin::External,
                reason: Some(reason.into()),
                evidence_ref,
            },
            reducer,
        )
    }

    /// Validate an interrupted external operation before recovering its publication.
    pub(crate) fn mutate_work_item_from_external_with_recovery_check<T>(
        &self,
        id: &str,
        reason: impl Into<String>,
        evidence_ref: Option<String>,
        check: impl FnOnce(&WorkItem) -> Result<(), WorkModelError>,
        reducer: impl FnOnce(&mut WorkItem) -> Result<T, WorkModelError>,
    ) -> Result<T, WorkModelStorageError> {
        self.mutate_work_item_with_recovery_check(
            id,
            &DurableTransitionMetadata {
                origin: DurableTransitionOrigin::External,
                reason: Some(reason.into()),
                evidence_ref,
            },
            &[],
            check,
            reducer,
        )
    }

    /// Atomically mutate a Work Item under its per-Work-Item model lock.
    ///
    /// Acquires the stable model lock once, recovers any pending transaction,
    /// reads the aggregate fresh under that lock, applies `reducer`, validates
    /// the result, and writes it back through the same held lock in one
    /// transaction — without recursively re-locking. Because no other process can
    /// interleave between the fresh read and the write, a reducer that inspects
    /// the current state (for example an attempt-precedence check) sees exactly
    /// the state it commits, closing the read-modify-write race that separate
    /// `read_work_item`/`write_work_item` calls leave open. A reducer that leaves
    /// the aggregate unchanged commits nothing (the write short-circuits on an
    /// identical snapshot), so a rejected mutation is a durable no-op.
    pub fn mutate_work_item<T>(
        &self,
        id: &str,
        reducer: impl FnOnce(&mut WorkItem) -> Result<T, WorkModelError>,
    ) -> Result<T, WorkModelStorageError> {
        self.mutate_work_item_with_transition(id, &fluent_transition_metadata(), reducer)
    }

    /// Mutate an aggregate and atomically attribute any derived transitions.
    pub fn mutate_work_item_with_transition<T>(
        &self,
        id: &str,
        metadata: &DurableTransitionMetadata,
        reducer: impl FnOnce(&mut WorkItem) -> Result<T, WorkModelError>,
    ) -> Result<T, WorkModelStorageError> {
        self.mutate_work_item_with_transition_and_files(id, metadata, &[], reducer)
    }

    /// Mutate an aggregate and journal immutable managed files in the same
    /// recoverable publication. The file bytes are installed only while the
    /// Work-model transaction is being applied, never as an orphaned
    /// pre-publication side effect.
    pub(crate) fn mutate_work_item_with_transition_and_files<T>(
        &self,
        id: &str,
        metadata: &DurableTransitionMetadata,
        managed_files: &[ManagedWorkItemFile],
        reducer: impl FnOnce(&mut WorkItem) -> Result<T, WorkModelError>,
    ) -> Result<T, WorkModelStorageError> {
        self.mutate_work_item_with_recovery_check(id, metadata, managed_files, |_| Ok(()), reducer)
    }

    fn mutate_work_item_with_recovery_check<T>(
        &self,
        id: &str,
        metadata: &DurableTransitionMetadata,
        managed_files: &[ManagedWorkItemFile],
        check: impl FnOnce(&WorkItem) -> Result<(), WorkModelError>,
        reducer: impl FnOnce(&mut WorkItem) -> Result<T, WorkModelError>,
    ) -> Result<T, WorkModelStorageError> {
        let path = self.work_item_path(id)?;
        let _lock = self.lock_work_item_model(id)?;
        if let Some(target) = self.read_work_item_recovery_target(id)? {
            check(&target).map_err(|source| WorkModelStorageError::InvalidModel {
                path: path.clone(),
                source,
            })?;
        }
        let mut work_item = self.read_work_item_under_model_lock(id)?;
        work_item.ensure_mutation_compatible().map_err(|source| {
            WorkModelStorageError::InvalidModel {
                path: path.clone(),
                source,
            }
        })?;
        let prior = work_item.clone();
        let output =
            reducer(&mut work_item).map_err(|source| WorkModelStorageError::InvalidModel {
                path: path.clone(),
                source,
            })?;
        self.normalize_work_artifact_paths(&mut work_item)?;
        if work_item == prior && managed_files.is_empty() {
            return Ok(output);
        }
        work_item.synchronize_pause_records(&self.project_root);
        reject_transition_history_change(&prior, &work_item, &path)?;
        reject_canceled_attempt_launches(&prior, &work_item, &path)?;
        if work_item != prior {
            append_transition_ledger(&prior, &mut work_item, metadata);
        }
        work_item
            .validate()
            .map_err(|source| WorkModelStorageError::InvalidModel {
                path: path.clone(),
                source,
            })?;
        self.write_work_item_file_unchecked_with_managed_files(&work_item, false, managed_files)?;
        Ok(output)
    }

    fn write_work_item_file(
        &self,
        work_item: &WorkItem,
        create_new: bool,
        metadata: Option<&DurableTransitionMetadata>,
    ) -> Result<(), WorkModelStorageError> {
        let path = self.work_item_path(&work_item.id)?;
        if !create_new {
            work_item.ensure_mutation_compatible().map_err(|source| {
                WorkModelStorageError::InvalidModel {
                    path: path.clone(),
                    source,
                }
            })?;
        }
        let _lock = self.lock_work_item_model(&work_item.id)?;
        if create_new && !work_item.transitions.is_empty() {
            return Err(WorkModelStorageError::InvalidModel {
                path: path.clone(),
                source: WorkModelError::TransitionLedgerModified,
            });
        }
        let transaction_path = self.work_transaction_path(&work_item.id)?;
        let recovered_transaction = !create_new && transaction_path.exists();
        if recovered_transaction {
            // Finish the host-authored snapshot before comparing a caller retry.
            // This restores one coherent aggregate even when the interrupted write
            // activated a legacy transition ledger that the caller never supplied.
            self.recover_work_item_transaction(&work_item.id)?;
        }
        let prior = if create_new {
            None
        } else if path.exists() {
            Some(self.read_work_item_file_unlocked(&path, false)?)
        } else {
            None
        };
        let mut target = work_item.clone();
        if let Some(prior) = prior.as_ref() {
            preserve_transition_history(prior, &mut target, &path)?;
            reject_canceled_attempt_launches(prior, &target, &path)?;
            reject_writer_correction_assignment_change(&self.project_root, prior, &target, &path)?;
            if recovered_transaction
                && target == *prior
                && let Some(revision) = prior.storage_revision.get()
            {
                target.storage_revision.set(revision);
            }
        }
        if let Some(metadata) = metadata {
            match prior.as_ref() {
                Some(prior) if target != *prior => {
                    append_transition_ledger(prior, &mut target, metadata);
                }
                None if create_new => append_durable_transitions(None, &mut target, metadata),
                _ => {}
            }
        }
        self.normalize_work_artifact_paths(&mut target)?;
        target.synchronize_pause_records(&self.project_root);
        target
            .validate()
            .map_err(|source| WorkModelStorageError::InvalidModel {
                path: path.clone(),
                source,
            })?;
        self.write_work_item_file_unchecked(&target, create_new)?;
        work_item
            .storage_revision
            .set(target.storage_revision.get().unwrap_or(0));
        Ok(())
    }

    /// Lock one Work Item's aggregate model before coordinating a mutation in
    /// another subsystem. Callers must acquire this before their subsystem
    /// lock and release the subsystem lock first.
    pub(crate) fn lock_work_item_model(&self, id: &str) -> Result<File, WorkModelStorageError> {
        self.work_item_path(id)?;
        let path = self
            .project_root
            .join(".fluent/work/locks")
            .join(id)
            .join("model.lock");
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|source| {
                WorkModelStorageError::CreateDirectory {
                    path: parent.to_path_buf(),
                    source,
                }
            })?;
        }
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .open(&path)
            .map_err(|source| WorkModelStorageError::WriteFile {
                path: path.clone(),
                source,
            })?;
        flock(&file, FlockOperation::LockExclusive)
            .map_err(io::Error::from)
            .map_err(|source| WorkModelStorageError::WriteFile { path, source })?;
        Ok(file)
    }

    fn write_work_item_file_unchecked(
        &self,
        work_item: &WorkItem,
        create_new: bool,
    ) -> Result<(), WorkModelStorageError> {
        self.write_work_item_file_unchecked_with_managed_files(work_item, create_new, &[])
    }

    fn write_work_item_file_unchecked_with_managed_files(
        &self,
        work_item: &WorkItem,
        create_new: bool,
        managed_files: &[ManagedWorkItemFile],
    ) -> Result<(), WorkModelStorageError> {
        self.write_work_item_file_unchecked_with_options(
            work_item,
            create_new,
            false,
            managed_files,
        )
    }

    /// Persist a format upgrade even when the typed Work Item is otherwise
    /// unchanged. Normal writes may short-circuit on an identical aggregate;
    /// an upgrade must still rewrite the split records with their new schema
    /// marker.
    fn write_work_item_file_unchecked_for_upgrade(
        &self,
        work_item: &WorkItem,
    ) -> Result<(), WorkModelStorageError> {
        self.write_work_item_file_unchecked_with_options(work_item, false, true, &[])
    }

    fn write_work_item_file_unchecked_with_options(
        &self,
        work_item: &WorkItem,
        create_new: bool,
        force_persist: bool,
        managed_files: &[ManagedWorkItemFile],
    ) -> Result<(), WorkModelStorageError> {
        let path = self.work_item_path(&work_item.id)?;
        let dir = self.work_items_dir();
        fs::create_dir_all(&dir)
            .map_err(|source| WorkModelStorageError::CreateDirectory { path: dir, source })?;

        if let Some(transaction) = self.read_work_item_transaction(&work_item.id)? {
            if managed_files.is_empty()
                && !force_persist
                && aggregate_snapshot_matches(&transaction.work_item, work_item)?
            {
                self.apply_work_item_transaction(&transaction)?;
                work_item.storage_revision.set(transaction.target_revision);
                return Ok(());
            }
            self.apply_work_item_transaction(&transaction)?;
        }

        let expected_revision = work_item.storage_revision.get();
        let mut usage_limit_publications =
            crate::usage_limit_contracts::pending_publication_ids(work_item, None);
        let mut context_retry_publications =
            crate::recovery_contracts::context_retry::pending_publications(work_item, None);
        let mut base_children_digest = None;
        let mut assignment_context_base = None;
        let mut context_retry_base = None;
        let base_revision = if path.exists() {
            let current: WorkItemRecord = read_json_file(&path)?;
            ensure_supported_record_schema(&path, "Work Item", current.schema_version)?;
            if create_new {
                return Err(WorkModelStorageError::WorkItemAlreadyExists {
                    path,
                    id: work_item.id.clone(),
                });
            }
            if expected_revision != Some(current.storage_revision) {
                return Err(WorkModelStorageError::StaleWorkItem {
                    path,
                    id: work_item.id.clone(),
                    expected_revision,
                    actual_revision: current.storage_revision,
                });
            }
            if let Ok(current_item) = self.read_work_item_file_unlocked(&path, false) {
                context_retry_publications =
                    crate::recovery_contracts::context_retry::pending_publications(
                        work_item,
                        Some(&current_item),
                    );
                usage_limit_publications = crate::usage_limit_contracts::pending_publication_ids(
                    work_item,
                    Some(&current_item),
                );
                if !crate::recovery_contracts::context_retry::pending_publications(work_item, None)
                    .is_empty()
                {
                    // Retain the base even for later ordinary writes so a
                    // stripped publication list cannot hide a new approval.
                    context_retry_base = Some(current_item.clone());
                }
                if managed_files.is_empty()
                    && !force_persist
                    && aggregate_snapshot_matches(&current_item, work_item)?
                {
                    work_item.storage_revision.set(current.storage_revision);
                    return Ok(());
                }
                base_children_digest = Some(work_item_children_digest(&current_item)?);
                reject_intake_contract_change(&current_item, work_item).map_err(|source| {
                    WorkModelStorageError::InvalidModel {
                        path: path.clone(),
                        source,
                    }
                })?;
                // B4s: freeze a no-expertise Attempt's reviewed identity while its
                // Learning is exposed. Reject before the transaction journal is
                // authored so no stale-handoff pointer move ever becomes durable.
                reject_frozen_no_expertise_identity_change(&current_item, work_item).map_err(
                    |source| WorkModelStorageError::InvalidModel {
                        path: path.clone(),
                        source,
                    },
                )?;
                reject_writer_correction_assignment_change(
                    &self.project_root,
                    &current_item,
                    work_item,
                    &path,
                )?;
                if work_item.attempts.iter().any(|attempt| {
                    attempt.recovery_operations.iter().any(|operation| {
                        operation.operation_kind == "assignment-context-recovery"
                            && !current_item.attempts.iter().any(|prior| {
                                prior.recovery_operation(&operation.operation_id).is_some()
                            })
                    })
                }) {
                    assignment_context_base = Some(current_item);
                }
            }
            Some(current.storage_revision)
        } else {
            None
        };
        let target_revision = match base_revision {
            Some(revision) => revision.checked_add(1).ok_or_else(|| {
                WorkModelStorageError::StorageRevisionExhausted {
                    path: path.clone(),
                    id: work_item.id.clone(),
                }
            })?,
            None => 0,
        };
        self.validate_managed_files(managed_files)?;
        let transaction = WorkItemTransaction {
            schema_version: if has_claude_session_authority(work_item) {
                WorkItemTransaction::CLAUDE_SESSION_SCHEMA_VERSION
            } else if has_no_change_completion_authority(work_item) {
                WorkItemTransaction::NO_CHANGE_COMPLETION_SCHEMA_VERSION
            } else {
                WorkItemTransaction::SCHEMA_VERSION
            },
            id: work_item.id.clone(),
            base_revision,
            target_revision,
            base_children_digest,
            managed_files: managed_files.to_vec(),
            usage_limit_publications,
            context_retry_publications,
            context_retry_base,
            assignment_context_base,
            work_item: work_item.clone(),
        };
        let transaction_path = self.work_transaction_path(&work_item.id)?;
        // A `#[cfg(test)]`-only fault at the real atomic persistence write. All
        // validation, locking, revision, and transaction authoring above ran; only
        // this final write of the durable transaction journal fails, mirroring a
        // real atomic storage failure and surfacing the typed storage boundary
        // (B4ak). Production never reaches this branch.
        #[cfg(test)]
        if persist_fault::should_fault_follow_up_write(work_item) {
            return Err(WorkModelStorageError::WriteFile {
                path: transaction_path,
                source: io::Error::other("injected atomic Work-model storage fault"),
            });
        }
        write_json_file(&transaction_path, &transaction)?;
        crate::usage_limit_contracts::test_interrupt_journal(work_item);
        if work_item.attempts.iter().any(|a| {
            a.recovery_operations
                .iter()
                .any(|r| r.operation_kind == "assignment-context-recovery")
        }) {
            crate::assignment_context_recovery::test_interrupt("journal");
        }
        if !transaction.context_retry_publications.is_empty() {
            if work_item.attempts.iter().any(|a| {
                a.recovery_operations.iter().any(|r| {
                    matches!(
                        r.operation_kind.as_str(),
                        crate::recovery_contracts::context_retry::START
                            | crate::recovery_contracts::context_retry::COMPLETION_START
                            | crate::recovery_contracts::claude_session::START
                    ) && transaction
                        .context_retry_publications
                        .contains(&r.operation_id)
                })
            }) {
                crate::recovery_contracts::context_retry::test_interrupt("start-journal");
            }
            crate::recovery_contracts::context_retry::test_interrupt("journal");
        }
        self.apply_work_item_transaction(&transaction)?;
        work_item.storage_revision.set(target_revision);
        Ok(())
    }

    fn validate_managed_files(
        &self,
        managed_files: &[ManagedWorkItemFile],
    ) -> Result<(), WorkModelStorageError> {
        let mut paths = HashSet::new();
        for file in managed_files {
            let path = self.validate_managed_file_destination(&file.path)?;
            if !paths.insert(file.path.as_str()) {
                return Err(self.invalid_managed_file(
                    path,
                    "the transaction lists the same managed file more than once",
                ));
            }
            let expected_digest = format!("sha256:{:x}", Sha256::digest(&file.bytes));
            if file.digest != expected_digest {
                return Err(self.invalid_managed_file(
                    path,
                    "the managed file digest does not match its journaled bytes",
                ));
            }
        }
        Ok(())
    }

    fn validate_managed_file_destination(
        &self,
        relative: &str,
    ) -> Result<PathBuf, WorkModelStorageError> {
        let relative_path = Path::new(relative);
        let components = relative_path.components().collect::<Vec<_>>();
        let managed_root = Path::new(WORK_ARTIFACTS_DIR);
        let valid = !relative_path.is_absolute()
            && components.len() > managed_root.components().count()
            && components
                .iter()
                .all(|component| matches!(component, Component::Normal(_)))
            && relative_path.starts_with(managed_root);
        let path = self.project_root.join(relative_path);
        if !valid {
            return Err(self.invalid_managed_file(
                path,
                "managed transaction files must use normal paths under .fluent/work/artifacts",
            ));
        }

        let mut current = self.project_root.clone();
        for (index, component) in components.iter().enumerate() {
            let Component::Normal(part) = component else {
                unreachable!("managed file path was checked above");
            };
            current.push(part);
            let is_last = index + 1 == components.len();
            match fs::symlink_metadata(&current) {
                Ok(metadata) if metadata.file_type().is_symlink() => {
                    return Err(self.invalid_managed_file(
                        path,
                        "managed transaction files must not traverse symlinks",
                    ));
                }
                Ok(metadata) if !is_last && !metadata.is_dir() => {
                    return Err(self.invalid_managed_file(
                        path,
                        "a managed transaction file parent is not a directory",
                    ));
                }
                Ok(metadata) if is_last && !metadata.is_file() => {
                    return Err(self.invalid_managed_file(
                        path,
                        "a managed transaction file destination is not a regular file",
                    ));
                }
                Ok(_) => {}
                Err(error) if error.kind() == ErrorKind::NotFound => {
                    // Missing parents and the final destination are installed
                    // by the transaction applier after its journal is durable.
                    break;
                }
                Err(error) => {
                    return Err(WorkModelStorageError::ReadFile {
                        path: current,
                        source: error,
                    });
                }
            }
        }
        Ok(path)
    }

    fn invalid_managed_file(&self, path: PathBuf, reason: &str) -> WorkModelStorageError {
        WorkModelStorageError::WriteFile {
            path,
            source: io::Error::other(reason),
        }
    }

    fn publish_managed_files(
        &self,
        managed_files: &[ManagedWorkItemFile],
    ) -> Result<(), WorkModelStorageError> {
        self.validate_managed_files(managed_files)?;
        for file in managed_files {
            let path = self.validate_managed_file_destination(&file.path)?;
            match fs::read(&path) {
                Ok(existing) if existing == file.bytes => continue,
                Ok(_) => {
                    return Err(self.invalid_managed_file(
                        path,
                        "managed recovery artifact already exists with different bytes",
                    ));
                }
                Err(error) if error.kind() == ErrorKind::NotFound => {
                    if let Some(parent) = path.parent() {
                        fs::create_dir_all(parent).map_err(|source| {
                            WorkModelStorageError::CreateDirectory {
                                path: parent.to_path_buf(),
                                source,
                            }
                        })?;
                    }
                    crate::atomic_write::atomic_write(&path, &file.bytes).map_err(|source| {
                        WorkModelStorageError::WriteFile {
                            path: path.clone(),
                            source,
                        }
                    })?;
                }
                Err(error) => {
                    return Err(WorkModelStorageError::ReadFile {
                        path,
                        source: error,
                    });
                }
            }
        }
        Ok(())
    }

    fn managed_files_are_published(
        &self,
        managed_files: &[ManagedWorkItemFile],
    ) -> Result<bool, WorkModelStorageError> {
        self.validate_managed_files(managed_files)?;
        for file in managed_files {
            let path = self.validate_managed_file_destination(&file.path)?;
            match fs::read(path) {
                Ok(existing) if existing == file.bytes => {}
                Ok(_) => return Ok(false),
                Err(error) if error.kind() == ErrorKind::NotFound => return Ok(false),
                Err(error) => {
                    return Err(WorkModelStorageError::ReadFile {
                        path: self.project_root.join(&file.path),
                        source: error,
                    });
                }
            }
        }
        Ok(true)
    }

    fn read_work_item_file(
        &self,
        path: &Path,
        validate: bool,
    ) -> Result<WorkItem, WorkModelStorageError> {
        let expected = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .ok_or_else(|| WorkModelStorageError::InvalidWorkItemId {
                id: path.display().to_string(),
            })?;
        work_item_file_name(expected)?;
        if !path.exists() && !self.work_transaction_path(expected)?.exists() {
            return self.read_work_item_file_unlocked(path, validate);
        }
        let _lock = self.lock_work_item_model(expected)?;
        self.recover_work_item_transaction(expected)?;
        self.read_work_item_file_unlocked(path, validate)
    }

    fn read_work_item_file_unlocked(
        &self,
        path: &Path,
        validate: bool,
    ) -> Result<WorkItem, WorkModelStorageError> {
        self.read_published_work_item_file(path, validate)
    }

    fn read_published_work_item_file(
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
        ensure_supported_record_schema(path, "Work Item", record.schema_version)?;
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
        #[cfg(test)]
        persist_fault::after_root_read(&record.id);
        let work_item = self.assemble_split_work_item(record, validate)?;
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

    fn read_work_item_transaction(
        &self,
        id: &str,
    ) -> Result<Option<WorkItemTransaction>, WorkModelStorageError> {
        let path = self.work_transaction_path(id)?;
        let transaction: WorkItemTransaction = match read_json_file(&path) {
            Ok(transaction) => transaction,
            Err(WorkModelStorageError::ReadFile { source, .. })
                if source.kind() == io::ErrorKind::NotFound =>
            {
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        let expected_target = match transaction.base_revision {
            Some(base) => base.checked_add(1),
            None => Some(0),
        };
        if !WorkItemTransaction::is_supported_schema(transaction.schema_version)
            || transaction.id != id
            || transaction.work_item.id != id
            || expected_target != Some(transaction.target_revision)
        {
            return Err(WorkModelStorageError::InvalidModel {
                path,
                source: WorkModelError::InvalidId {
                    kind: "work item transaction",
                    id: id.to_string(),
                },
            });
        }
        self.validate_managed_files(&transaction.managed_files)?;
        transaction.validate_context_retry().map_err(|source| {
            WorkModelStorageError::InvalidModel {
                path: path.clone(),
                source,
            }
        })?;
        transaction
            .work_item
            .validate()
            .map_err(|source| WorkModelStorageError::InvalidModel {
                path: path.clone(),
                source,
            })?;
        Ok(Some(transaction))
    }

    fn recover_work_item_transaction(&self, id: &str) -> Result<(), WorkModelStorageError> {
        if let Some(transaction) = self.read_work_item_transaction(id)? {
            self.apply_work_item_transaction(&transaction)?;
        }
        Ok(())
    }

    /// Reconcile only the journal bound to this explicit recovery request. A
    /// rejected request must not perform unrelated pending maintenance.
    pub(crate) fn reconcile_assignment_context_transaction(
        &self,
        operation: &RecoveryOperationRequest,
        audit: &AssignmentContextRecoveryAudit,
    ) -> anyhow::Result<bool> {
        let _lock = self.lock_work_item_model(&operation.work_item_id)?;
        let Some(transaction) = self.read_work_item_transaction(&operation.work_item_id)? else {
            return Ok(false);
        };
        let Some(base) = &transaction.assignment_context_base else {
            return Ok(false);
        };
        let (_, task) = crate::recovery_contracts::assignment_context_recovery_target(
            &self.project_root,
            &transaction.work_item,
            &operation.attempt_id,
            operation.task_id.as_deref().unwrap_or_default(),
            operation.candidate_commit.as_deref().unwrap_or_default(),
        )?;
        anyhow::ensure!(
            transaction
                .work_item
                .validate_recovery_operation(&self.project_root, operation)?
                .is_some()
                && task.writer_correction_assignment.as_ref() == Some(&audit.after)
                && base.attempts.iter().any(|attempt| {
                    attempt.id == operation.attempt_id
                        && crate::recovery_contracts::assignment_attempt_digest(attempt)
                            .is_ok_and(|digest| digest == audit.attempt_digest)
                }),
            "pending context recovery does not match this request"
        );
        self.apply_work_item_transaction(&transaction)?;
        Ok(true)
    }

    /// Validate the original authorization and every partially published child
    /// before replaying this journal. The journal base is never an observation.
    fn validate_assignment_context_publication(
        &self,
        transaction: &WorkItemTransaction,
        current: &WorkItem,
        base: &WorkItem,
    ) -> anyhow::Result<()> {
        use crate::recovery_contracts as contracts;
        let target = &transaction.work_item;
        anyhow::ensure!(
            transaction.schema_version >= 7
                && base.id == target.id
                && transaction.base_children_digest.as_deref()
                    == Some(work_item_children_digest(base)?.as_str()),
            "context recovery journal lost its authorized base"
        );
        base.ensure_mutation_compatible()?;
        base.validate()?;
        reject_writer_correction_assignment_change(
            &self.project_root,
            base,
            target,
            &self.work_item_path(&target.id)?,
        )?;
        let mut publications = 0;
        for attempt in &target.attempts {
            for operation in &attempt.recovery_operations {
                if operation.operation_kind != "assignment-context-recovery"
                    || base
                        .attempts
                        .iter()
                        .any(|prior| prior.recovery_operation(&operation.operation_id).is_some())
                {
                    continue;
                }
                publications += 1;
                let (_, task) = contracts::assignment_context_recovery_target(
                    &self.project_root,
                    target,
                    &attempt.id,
                    operation.task_id.as_deref().unwrap_or_default(),
                    operation.candidate_commit.as_deref().unwrap_or_default(),
                )?;
                contracts::validate_assignment_context_recovery_inputs(
                    &self.project_root,
                    task,
                    task.writer_correction_assignment
                        .as_ref()
                        .ok_or_else(|| anyhow::anyhow!("recovery has no assignment"))?,
                )?;
            }
        }
        anyhow::ensure!(
            publications == 1,
            "context recovery journal must bind one publication"
        );
        self.validate_recovery_children(current, base, target)
    }

    /// Each partially published child must still match the authorized base or target.
    fn validate_recovery_children(
        &self,
        current: &WorkItem,
        base: &WorkItem,
        target: &WorkItem,
    ) -> anyhow::Result<()> {
        let mut current_root = current.clone();
        current_root.attempts.clear();
        current_root.merge_candidates.clear();
        let mut base_root = base.clone();
        base_root.attempts.clear();
        base_root.merge_candidates.clear();
        let mut target_root = target.clone();
        target_root.attempts.clear();
        target_root.merge_candidates.clear();
        anyhow::ensure!(
            aggregate_snapshot_matches(&current_root, &base_root)?
                || aggregate_snapshot_matches(&current_root, &target_root)?,
            "Work changed during context recovery publication"
        );
        anyhow::ensure!(
            current.merge_candidates == base.merge_candidates
                && target.merge_candidates == base.merge_candidates
                && current.attempts.len() == base.attempts.len()
                && target.attempts.len() == base.attempts.len(),
            "Work children changed during context recovery publication"
        );
        for ((current, before), after) in current
            .attempts
            .iter()
            .zip(&base.attempts)
            .zip(&target.attempts)
        {
            let mut current_record = current.clone();
            current_record.tasks.clear();
            let mut before_record = before.clone();
            before_record.tasks.clear();
            let mut after_record = after.clone();
            after_record.tasks.clear();
            anyhow::ensure!(
                (current_record == before_record || current_record == after_record)
                    && current.tasks.len() == before.tasks.len()
                    && after.tasks.len() == before.tasks.len(),
                "Attempt changed during context recovery publication"
            );
            for ((current, before), after) in
                current.tasks.iter().zip(&before.tasks).zip(&after.tasks)
            {
                anyhow::ensure!(
                    current == before || current == after,
                    "Task changed during context recovery publication"
                );
            }
        }
        Ok(())
    }

    fn recover_pending_transactions(&self) -> Result<(), WorkModelStorageError> {
        let dir = self.work_transactions_dir();
        if !dir.exists() {
            return Ok(());
        }
        for path in list_json_paths(&dir)? {
            let id = file_stem_id(&path)?;
            let _lock = self.lock_work_item_model(&id)?;
            self.recover_work_item_transaction(&id)?;
        }
        Ok(())
    }

    fn apply_work_item_transaction(
        &self,
        transaction: &WorkItemTransaction,
    ) -> Result<(), WorkModelStorageError> {
        transaction.validate_context_retry().map_err(|source| {
            WorkModelStorageError::InvalidModel {
                path: self.work_dir(),
                source,
            }
        })?;
        crate::recovery_contracts::context_retry::validate_publication(
            &self.project_root,
            &transaction.work_item,
            &transaction.context_retry_publications,
        )
        .map_err(|error| WorkModelStorageError::InvalidModel {
            path: self.work_dir(),
            source: WorkModelError::InvalidRecoveryOperation {
                attempt_id: transaction.id.clone(),
                reason: format!("pending context retry is unverifiable: {error:#}"),
            },
        })?;
        crate::usage_limit_contracts::validate_publication(
            &self.project_root,
            &transaction.work_item,
            &transaction.usage_limit_publications,
        )
        .map_err(|error| WorkModelStorageError::InvalidModel {
            path: self.work_dir(),
            source: WorkModelError::InvalidRecoveryOperation {
                attempt_id: transaction.id.clone(),
                reason: format!("pending usage-limit authority is unverifiable: {error:#}"),
            },
        })?;
        let path = self.work_item_path(&transaction.id)?;
        let current_revision = if path.exists() {
            let current = read_json_file::<WorkItemRecord>(&path)?;
            ensure_supported_record_schema(&path, "Work Item", current.schema_version)?;
            Some(current.storage_revision)
        } else {
            None
        };
        let valid_revision = current_revision == transaction.base_revision
            || current_revision == Some(transaction.target_revision);
        if !valid_revision {
            return Err(WorkModelStorageError::StaleWorkItem {
                path,
                id: transaction.id.clone(),
                expected_revision: transaction.base_revision,
                actual_revision: current_revision.unwrap_or(0),
            });
        }

        if path.exists() {
            let current = self.read_work_item_file_unlocked(&path, false)?;
            if let Some(base) = &transaction.context_retry_base
                && !transaction.context_retry_publications.is_empty()
            {
                self.validate_recovery_children(&current, base, &transaction.work_item)
                    .map_err(|error| WorkModelStorageError::InvalidModel {
                        path: path.clone(),
                        source: WorkModelError::InvalidRecoveryOperation {
                            attempt_id: transaction.id.clone(),
                            reason: format!("pending context retry children changed: {error:#}"),
                        },
                    })?;
            }
            if let Some(base) = &transaction.assignment_context_base {
                self.validate_assignment_context_publication(transaction, &current, base)
                    .map_err(|error| WorkModelStorageError::InvalidModel {
                        path: path.clone(),
                        source: WorkModelError::InvalidRecoveryOperation {
                            attempt_id: transaction.id.clone(),
                            reason: format!("pending context authority is unverifiable: {error:#}"),
                        },
                    })?;
            } else {
                reject_writer_correction_assignment_change(
                    &self.project_root,
                    &current,
                    &transaction.work_item,
                    &path,
                )?;
            }
        }

        self.publish_managed_files(&transaction.managed_files)?;
        // Integration tests can stop after the journal and managed sidecars
        // become durable and before any split child is published. Production
        // builds do not carry this interruption path; retries exercise the
        // normal journal recovery.
        #[cfg(feature = "test-support")]
        if std::env::var("FLUENT_TEST_WORK_MODEL_INTERRUPT")
            .ok()
            .as_deref()
            == Some("before-child-publication")
        {
            return Err(WorkModelStorageError::WriteFile {
                path: self.work_transaction_path(&transaction.id)?,
                source: io::Error::other("injected Work-model publication interruption"),
            });
        }

        self.write_attempt_records(&transaction.work_item)?;
        self.write_merge_candidate_records(&transaction.work_item)?;
        // Exercise a durable journal with published children and the old root.
        #[cfg(feature = "test-support")]
        if std::env::var("FLUENT_TEST_WORK_MODEL_INTERRUPT")
            .ok()
            .as_deref()
            == Some(format!("before-root-publication:{}", transaction.id).as_str())
        {
            return Err(WorkModelStorageError::WriteFile {
                path: self.work_transaction_path(&transaction.id)?,
                source: io::Error::other("injected Work-model root publication interruption"),
            });
        }
        if !transaction.context_retry_publications.is_empty() {
            crate::recovery_contracts::context_retry::test_interrupt("children");
        }
        if transaction.work_item.attempts.iter().any(|a| {
            a.recovery_operations
                .iter()
                .any(|r| r.operation_kind == "assignment-context-recovery")
        }) {
            crate::assignment_context_recovery::test_interrupt("children");
        }
        // Exercise the real public split-record boundary with a fixture-specific
        // interruption. Production executables contain no environment override.
        #[cfg(feature = "test-support")]
        if std::env::var("FLUENT_TEST_INTERRUPT_CARRIED_FINDING_PUBLICATION")
            .ok()
            .as_deref()
            == Some(transaction.id.as_str())
            && transaction.work_item.attempts.iter().any(|a| {
                a.evidence_recoveries
                    .iter()
                    .any(|r| r.carried_finding.is_some())
            })
        {
            return Err(WorkModelStorageError::WriteFile {
                path: self.work_transaction_path(&transaction.id)?,
                source: io::Error::other("interrupted carried finding publication"),
            });
        }

        let mut record = WorkItemRecord::from(&transaction.work_item);
        record.storage_revision = transaction.target_revision;
        let json = to_json_pretty(&record).map_err(|source| WorkModelStorageError::ParseFile {
            path: path.clone(),
            source,
        })?;
        crate::atomic_write::atomic_write(&path, json.as_bytes()).map_err(|source| {
            WorkModelStorageError::WriteFile {
                path: path.clone(),
                source,
            }
        })?;
        let transaction_path = self.work_transaction_path(&transaction.id)?;
        #[cfg(test)]
        if persist_fault::should_fault_creation_after_publish(&transaction.work_item) {
            return Err(WorkModelStorageError::WriteFile {
                path: transaction_path,
                source: io::Error::other("injected post-publication transaction cleanup fault"),
            });
        }
        fs::remove_file(&transaction_path).map_err(|source| WorkModelStorageError::WriteFile {
            path: transaction_path,
            source,
        })?;
        Ok(())
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
            #[cfg(feature = "test-support")]
            self.test_interrupt_landing_child(work_item, "after-attempt-record")?;
            if attempt
                .recovery_operations
                .last()
                .is_some_and(|r| r.operation_kind == "assignment-context-recovery")
            {
                crate::assignment_context_recovery::test_interrupt("attempt");
            }
            if attempt.recovery_operations.last().is_some_and(|r| {
                r.operation_kind.starts_with("writer-completion-context-")
                    || r.operation_kind.starts_with("writer-completion-no-change-")
            }) {
                crate::recovery_contracts::context_retry::test_interrupt("attempt");
            }
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
                #[cfg(feature = "test-support")]
                self.test_interrupt_landing_child(work_item, "after-task-record")?;
                if attempt.recovery_operations.last().is_some_and(|r| {
                    r.operation_kind == "assignment-context-recovery"
                        && r.task_id.as_deref() == Some(task.id.as_str())
                }) {
                    crate::assignment_context_recovery::test_interrupt("task");
                }
                if attempt.recovery_operations.last().is_some_and(|r| {
                    (r.operation_kind.starts_with("writer-completion-context-")
                        || r.operation_kind.starts_with("writer-completion-no-change-"))
                        && r.task_id.as_deref() == Some(task.id.as_str())
                }) {
                    crate::recovery_contracts::context_retry::test_interrupt("task");
                }
                task_files.insert(task_path);
            }
            prune_json_files(&task_dir, &task_files)?;
        }
        prune_json_files(&attempts_dir, &attempt_files)?;
        prune_child_dirs(&tasks_item_dir, &attempt_dirs)?;
        Ok(())
    }

    #[cfg(feature = "test-support")]
    fn test_interrupt_landing_child(
        &self,
        work_item: &WorkItem,
        stage: &str,
    ) -> Result<(), WorkModelStorageError> {
        let key = format!("{stage}:{}", work_item.id);
        if std::env::var("FLUENT_TEST_WORK_MODEL_INTERRUPT")
            .ok()
            .as_deref()
            == Some(&key)
            && work_item.merge_candidates.iter().any(|candidate| {
                candidate.merge_state.status == MergeCandidateMergeStatus::Merged
                    && candidate.merge_state.landing_intent.is_some()
            })
        {
            return Err(WorkModelStorageError::WriteFile {
                path: self.work_transaction_path(&work_item.id)?,
                source: io::Error::other("injected landing child publication interruption"),
            });
        }
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
        #[cfg(test)]
        if persist_fault::should_fault_split_write(work_item) {
            return Err(WorkModelStorageError::WriteFile {
                path: candidates_dir,
                source: io::Error::other("injected split-record storage fault"),
            });
        }
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
        let mut attempts = Vec::new();
        for path in list_optional_json_paths(&attempts_dir)? {
            let record = read_json_file::<AttemptRecord>(&path)?;
            ensure_supported_record_schema(&path, "Attempt", record.schema_version)?;
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
        let mut tasks = Vec::new();
        let mut task_orders = HashSet::new();
        for path in list_optional_json_paths(&tasks_dir)? {
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
        let attempt_ids: HashSet<&str> =
            attempts.iter().map(|attempt| attempt.id.as_str()).collect();
        let entries = match fs::read_dir(&tasks_item_dir) {
            Ok(entries) => entries,
            Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(source) => {
                return Err(WorkModelStorageError::ReadDirectory {
                    path: tasks_item_dir,
                    source,
                });
            }
        };
        for entry in entries {
            let entry = entry.map_err(|source| WorkModelStorageError::ReadDirectory {
                path: tasks_item_dir.clone(),
                source,
            })?;
            let path = entry.path();
            let metadata =
                fs::metadata(&path).map_err(|source| WorkModelStorageError::ReadDirectory {
                    path: path.clone(),
                    source,
                })?;
            if !metadata.is_dir() || !self.collection_has_json_records(&path)? {
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
        let entries = match fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(source) => {
                return Err(WorkModelStorageError::ReadDirectory {
                    path: dir.to_path_buf(),
                    source,
                });
            }
        };
        for entry in entries {
            let entry = entry.map_err(|source| WorkModelStorageError::ReadDirectory {
                path: dir.to_path_buf(),
                source,
            })?;
            let path = entry.path();
            let metadata =
                fs::metadata(&path).map_err(|source| WorkModelStorageError::ReadDirectory {
                    path: path.clone(),
                    source,
                })?;
            if metadata.is_dir() {
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
        let mut candidates = Vec::new();
        for path in list_optional_json_paths(&candidates_dir)? {
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
        for migration in Self::plan_work_artifact_migrations(work_item) {
            self.migrate_artifact_path(&migration.from_path, &migration.to_path)?;
        }
        Ok(())
    }

    fn plan_work_artifact_migrations(work_item: &mut WorkItem) -> Vec<WorkModelArtifactMigration> {
        let mut migrations = Vec::new();
        let mut normalize = |work_item_id: &str, attempt_id: &str, path: &mut String| {
            if let Some(normalized) = namespace_legacy_artifact_path(work_item_id, attempt_id, path)
            {
                let migration = WorkModelArtifactMigration {
                    from_path: path.clone(),
                    to_path: normalized.clone(),
                };
                if !migrations.contains(&migration) {
                    migrations.push(migration);
                }
                *path = normalized;
            }
        };
        for attempt in &mut work_item.attempts {
            for task in &mut attempt.tasks {
                if let Some(area) = &mut task.artifact_area {
                    normalize(&work_item.id, &attempt.id, &mut area.path);
                }
                for artifact in &mut task.input_artifacts {
                    normalize(&work_item.id, &attempt.id, &mut artifact.path);
                }
            }
            for artifact in &mut attempt.artifacts {
                normalize(&work_item.id, &attempt.id, &mut artifact.path);
            }
        }
        for candidate in &mut work_item.merge_candidates {
            for artifact in candidate
                .merge_state
                .check_artifacts
                .iter_mut()
                .chain(candidate.merge_state.review_artifacts.iter_mut())
            {
                normalize(&work_item.id, &candidate.attempt_id, &mut artifact.path);
            }
        }
        migrations
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

fn reject_transition_history_change(
    prior: &WorkItem,
    proposed: &WorkItem,
    path: &Path,
) -> Result<(), WorkModelStorageError> {
    if proposed.transitions != prior.transitions {
        return Err(WorkModelStorageError::InvalidModel {
            path: path.to_path_buf(),
            source: WorkModelError::TransitionLedgerModified,
        });
    }
    Ok(())
}

fn preserve_transition_history(
    prior: &WorkItem,
    proposed: &mut WorkItem,
    path: &Path,
) -> Result<(), WorkModelStorageError> {
    if prior.transitions.starts_with(&proposed.transitions) {
        proposed.transitions.clone_from(&prior.transitions);
        return Ok(());
    }
    reject_transition_history_change(prior, proposed, path)
}

/// Append the current mutation, activating a legacy aggregate's ledger first.
///
/// An empty persisted ledger predates transition recording, so it cannot prove
/// that the aggregate has never changed. Snapshot every durable object state
/// with an unknown historical origin, then derive the caller-attributed change
/// from that snapshot. Both sets of entries persist in the mutation's single
/// aggregate transaction.
fn append_transition_ledger(
    prior: &WorkItem,
    work_item: &mut WorkItem,
    metadata: &DurableTransitionMetadata,
) {
    if prior.transitions.is_empty() {
        let mut baseline = prior.clone();
        append_durable_transitions(
            None,
            &mut baseline,
            &DurableTransitionMetadata {
                origin: DurableTransitionOrigin::Unknown,
                reason: None,
                evidence_ref: None,
            },
        );
        work_item.transitions.clone_from(&baseline.transitions);
        append_durable_transitions(Some(&baseline), work_item, metadata);
    } else {
        append_durable_transitions(Some(prior), work_item, metadata);
    }
}

fn append_durable_transitions(
    prior: Option<&WorkItem>,
    work_item: &mut WorkItem,
    metadata: &DurableTransitionMetadata,
) {
    let metadata = DurableTransitionMetadata {
        origin: metadata.origin,
        reason: metadata
            .reason
            .as_deref()
            .map(|text| bounded_transition_text(text, MAX_TRANSITION_REASON_BYTES)),
        evidence_ref: metadata
            .evidence_ref
            .as_deref()
            .map(|text| bounded_transition_text(text, MAX_TRANSITION_EVIDENCE_BYTES)),
    };
    let previous = prior
        .map(|item| {
            item.transitions
                .last()
                .map(|entry| entry.sequence)
                .unwrap_or(0)
        })
        .unwrap_or(0);
    let current = durable_object_states(work_item);
    let prior_states: HashMap<_, _> = prior
        .into_iter()
        .flat_map(|item| item.transitions.iter())
        .map(|transition| {
            (
                format!("{}:{}", transition.object_kind, transition.object_id),
                transition.new_state.clone(),
            )
        })
        .collect();
    // The aggregate already stores only emitted states. Record a fresh entry for
    // an object when its current state differs from its last durable state.
    let now = now_iso8601();
    let now_timestamp = chrono::DateTime::parse_from_rfc3339(&now).ok();
    for (object_kind, object_id, state, reason) in current {
        let key = format!("{object_kind}:{object_id}");
        let old_state = prior_states.get(&key).cloned();
        if old_state.as_deref() == Some(state.as_str()) {
            continue;
        }
        let sequence = work_item
            .transitions
            .last()
            .map(|entry| entry.sequence + 1)
            .unwrap_or(previous + 1);
        let elapsed_seconds = prior
            .and_then(|item| {
                item.transitions
                    .iter()
                    .rev()
                    .find(|entry| entry.object_kind == object_kind && entry.object_id == object_id)
            })
            .and_then(|entry| chrono::DateTime::parse_from_rfc3339(&entry.occurred_at).ok())
            .zip(now_timestamp)
            .map(|(started, ended)| (ended - started).num_seconds().max(0) as u64);
        work_item.transitions.push(DurableWorkTransition {
            sequence,
            object_kind,
            object_id,
            old_state,
            new_state: state,
            occurred_at: now.clone(),
            elapsed_seconds,
            origin: metadata.origin,
            reason: metadata.reason.clone().or(reason),
            evidence_ref: metadata.evidence_ref.clone(),
        });
    }
}

fn bounded_transition_text(value: &str, maximum: usize) -> String {
    if value.len() <= maximum {
        return value.to_owned();
    }
    let mut end = maximum;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}

fn durable_object_states(work_item: &WorkItem) -> Vec<(String, String, String, Option<String>)> {
    let mut states = vec![(
        "work-item".into(),
        work_item.id.clone(),
        if work_item.authorization.is_proposed() {
            "planned"
        } else {
            "authorized"
        }
        .into(),
        None,
    )];
    states.extend(work_item.attempts.iter().map(|attempt| {
        (
            "attempt".into(),
            attempt.id.clone(),
            attempt.status.as_str().into(),
            attempt
                .pause_kind
                .as_ref()
                .map(|pause| pause.as_str().to_string()),
        )
    }));
    states.extend(work_item.attempts.iter().flat_map(|attempt| {
        attempt.tasks.iter().map(|task| {
            (
                "task".into(),
                task.id.clone(),
                format!("{:?}", task.status).to_lowercase(),
                None,
            )
        })
    }));
    states.extend(work_item.merge_candidates.iter().map(|candidate| {
        let state = if candidate.merge_state.status == MergeCandidateMergeStatus::Pending
            && candidate.merge_review_state == MergeReviewState::Passed
        {
            "ready".to_owned()
        } else {
            format!("{:?}", candidate.merge_state.status).to_lowercase()
        };
        (
            "merge-candidate".into(),
            candidate.id.clone(),
            state,
            candidate.merge_state.failure_reason.clone(),
        )
    }));
    states
}

fn reject_intake_contract_change(
    current: &WorkItem,
    next: &WorkItem,
) -> Result<(), WorkModelError> {
    match (&current.planning_scope, &next.planning_scope) {
        (Some(current), Some(next)) if current != next => {
            return Err(WorkModelError::InvalidPlanningScope {
                reason: "the recorded intake diagnostic is immutable".to_string(),
            });
        }
        (Some(_), None) => {
            return Err(WorkModelError::InvalidPlanningScope {
                reason: "the recorded intake diagnostic cannot be removed".to_string(),
            });
        }
        (None, Some(_)) if !current.attempts.is_empty() => {
            return Err(WorkModelError::InvalidPlanningScope {
                reason: "scope authorization must be defined before the first Attempt".to_string(),
            });
        }
        _ => {}
    }
    match (
        current.release_contract.as_ref(),
        next.release_contract.as_ref(),
    ) {
        (Some(current), Some(next)) if current.criteria != next.criteria => {
            Err(WorkModelError::InvalidReleaseContract {
                reason: "accepted criteria are immutable after Work creation".to_string(),
            })
        }
        (Some(current), Some(next))
            if !next
                .findings
                .as_slice()
                .starts_with(current.findings.as_slice()) =>
        {
            Err(WorkModelError::InvalidReleaseContract {
                reason: "release finding classifications are append-only".to_string(),
            })
        }
        (Some(_), None) => Err(WorkModelError::InvalidReleaseContract {
            reason: "the release contract cannot be removed after Work creation".to_string(),
        }),
        (None, Some(_)) if !current.attempts.is_empty() => {
            Err(WorkModelError::InvalidReleaseContract {
                reason: "release acceptance must be defined before the first Attempt".to_string(),
            })
        }
        _ => Ok(()),
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

fn list_optional_json_paths(dir: &Path) -> Result<Vec<PathBuf>, WorkModelStorageError> {
    match list_json_paths(dir) {
        Err(WorkModelStorageError::ReadDirectory { source, .. })
            if source.kind() == io::ErrorKind::NotFound =>
        {
            Ok(Vec::new())
        }
        result => result,
    }
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

/// Return the Tester evidence that explains the latest correction to the tests
/// reviewer. A passing failed-command recheck supersedes the original failure;
/// otherwise only Tester artifacts attached directly to the latest Writer carry
/// forward. This prevents an older failure from leaking through later rounds.
fn tester_correction_artifacts_for_tests_review(
    attempt: &Attempt,
    latest_write: &Task,
) -> Vec<ArtifactRef> {
    let Some(latest_write_index) = attempt
        .tasks
        .iter()
        .position(|task| task.id == latest_write.id)
    else {
        return Vec::new();
    };

    if let Some(recheck) = attempt.tasks[latest_write_index + 1..]
        .iter()
        .rev()
        .find(|task| task.is_failed_command_recheck() && task.status == TaskStatus::Complete)
    {
        return recheck
            .artifact_area
            .as_ref()
            .map(|area| {
                vec![ArtifactRef {
                    producer_id: recheck.id.clone(),
                    path: format!("{}/tester-results.json", area.path),
                }]
            })
            .unwrap_or_default();
    }

    let tester_producers = attempt.tasks[..latest_write_index]
        .iter()
        .filter(|task| task.kind == TaskKind::Tester && task.status == TaskStatus::Complete)
        .map(|task| task.id.as_str())
        .collect::<HashSet<_>>();
    latest_write
        .input_artifacts
        .iter()
        .filter(|artifact| tester_producers.contains(artifact.producer_id.as_str()))
        .filter(|artifact| {
            artifact.path.ends_with("/tester-results.json")
                || crate::tester::is_command_log_path(Path::new(&artifact.path))
        })
        .cloned()
        .collect()
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

fn ensure_supported_record_schema(
    path: &Path,
    record_kind: &'static str,
    version: u32,
) -> Result<(), WorkModelStorageError> {
    if version <= CLAUDE_SESSION_RECORD_SCHEMA_VERSION {
        return Ok(());
    }
    Err(WorkModelStorageError::InvalidModel {
        path: path.to_path_buf(),
        source: WorkModelError::UnsupportedStoredRecordVersion {
            record_kind,
            found: version,
            supported: CLAUDE_SESSION_RECORD_SCHEMA_VERSION,
        },
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
    crate::atomic_write::atomic_write(path, json.as_bytes()).map_err(|source| {
        WorkModelStorageError::WriteFile {
            path: path.to_path_buf(),
            source,
        }
    })
}

/// The review round an Attempt's Task belongs to, parsed from its id. Mirrors the
/// no-expertise pointer-identity gate so the frozen-identity guard reads the same
/// final round. A pure-model derivation with no Git access. (B4s)
fn attempt_task_review_round(attempt_id: &str, task: &Task) -> Option<usize> {
    match task.kind {
        TaskKind::Review => {
            let suffix = task.id.strip_prefix(&format!("{attempt_id}-review-"))?;
            suffix
                .split_once('-')
                .and_then(|(round, _)| round.parse::<usize>().ok())
                .or(Some(1))
        }
        TaskKind::Tester => {
            let suffix = task.id.strip_prefix(&format!("{attempt_id}-tester"))?;
            if suffix.is_empty() {
                Some(1)
            } else {
                suffix
                    .strip_prefix('-')
                    .and_then(|n| n.parse::<usize>().ok())
            }
        }
        _ => None,
    }
}

/// The final (highest-numbered) review round among an Attempt's completed Tester and
/// built-in reviewer Tasks. (B4s)
fn attempt_final_review_round(attempt: &Attempt) -> usize {
    attempt
        .tasks
        .iter()
        .filter(|task| task.status == TaskStatus::Complete)
        .filter_map(|task| attempt_task_review_round(&attempt.id, task))
        .max()
        .unwrap_or(1)
}

/// The reviewed-identity tuple a no-expertise Attempt freezes once its Learning is
/// exposed as `HandoffPending` or `Succeeded`: the reviewed Writer SHA and its Write
/// Task, every Merge Candidate bound to the Attempt with its `candidate_commit`, and
/// every final-round Tester and built-in reviewer commit context. Comparing this
/// tuple before and after a proposed write detects any supported mutation that would
/// move the audited pointers out from under a published or settled handoff. (B4s)
#[derive(Debug, PartialEq, Eq)]
struct FrozenReviewedIdentityTuple {
    reviewed_sha: Option<String>,
    write_task_id: Option<String>,
    candidates: HashMap<String, String>,
    final_round_contexts: HashMap<String, String>,
}

fn frozen_reviewed_identity_tuple(
    item: &WorkItem,
    attempt: &Attempt,
) -> FrozenReviewedIdentityTuple {
    let write = attempt
        .tasks
        .iter()
        .rev()
        .find(|task| task.kind == TaskKind::Write && task.status == TaskStatus::Complete);
    let reviewed_sha = write
        .and_then(|task| task.output.as_ref())
        .map(|output| output.commit.clone());
    let write_task_id = write.map(|task| task.id.clone());

    let candidates = item
        .merge_candidates
        .iter()
        .filter(|candidate| candidate.attempt_id == attempt.id)
        .map(|candidate| (candidate.id.clone(), candidate.candidate_commit.clone()))
        .collect();

    let final_round = attempt_final_review_round(attempt);
    let final_round_contexts = attempt
        .tasks
        .iter()
        .filter(|task| task.status == TaskStatus::Complete)
        .filter(|task| {
            (task.kind == TaskKind::Tester
                || (task.kind == TaskKind::Review
                    && crate::review::REVIEWERS.contains(&task.role.as_str())))
                && attempt_task_review_round(&attempt.id, task) == Some(final_round)
        })
        .filter_map(|task| {
            task.review_context
                .as_ref()
                .map(|context| (task.id.clone(), context.candidate_commit.clone()))
        })
        .collect();

    FrozenReviewedIdentityTuple {
        reviewed_sha,
        write_task_id,
        candidates,
        final_round_contexts,
    }
}

/// Reject a supported write that moves a no-expertise Attempt's frozen reviewed
/// identity while its Learning record is exposed as `HandoffPending` or `Succeeded`,
/// so a handoff audited against the reviewed Writer SHA can never be made stale by a
/// later pointer move and a landed `merged_commit` can only ever be that exact SHA.
///
/// The check runs before the transaction journal is authored, covering both
/// field-level and whole-aggregate writes without changing the public schema, and
/// compares against the prior durable aggregate so a Learning-only transition, an
/// exact-SHA landing metadata update, or any unrelated field change still commits. A
/// change while Learning is `InProgress` (postflight detects it) or a repair after
/// relaunchable `Failed` stays allowed because neither status exposes a handoff.
/// Capture-mode Work is never frozen. (B4s)
fn reject_frozen_no_expertise_identity_change(
    prior: &WorkItem,
    proposed: &WorkItem,
) -> Result<(), WorkModelError> {
    if prior.learner_mode != LearnerMode::NoExpertise {
        return Ok(());
    }
    for attempt in &prior.attempts {
        let exposed = matches!(
            attempt.learning.as_ref(),
            Some(learning)
                if matches!(
                    learning.status,
                    LearningStatus::HandoffPending | LearningStatus::Succeeded
                )
        );
        if !exposed {
            continue;
        }
        let reject = |field: &'static str| WorkModelError::FrozenReviewedIdentity {
            attempt_id: attempt.id.clone(),
            field,
        };
        // The no-expertise policy that protects the frozen pointers may not change.
        if proposed.learner_mode != prior.learner_mode {
            return Err(reject("no-expertise policy"));
        }
        let Some(proposed_attempt) = proposed
            .attempts
            .iter()
            .find(|candidate| candidate.id == attempt.id)
        else {
            return Err(reject("reviewed Attempt"));
        };
        let prior_identity = frozen_reviewed_identity_tuple(prior, attempt);
        let proposed_identity = frozen_reviewed_identity_tuple(proposed, proposed_attempt);
        if proposed_identity.reviewed_sha != prior_identity.reviewed_sha
            || proposed_identity.write_task_id != prior_identity.write_task_id
        {
            return Err(reject("latest completed Write task or commit"));
        }
        if proposed_identity.candidates != prior_identity.candidates {
            return Err(reject("selected Merge Candidate id or commit"));
        }
        if proposed_identity.final_round_contexts != prior_identity.final_round_contexts {
            return Err(reject("final-round Tester or reviewer context"));
        }
        // A persisted `merged_commit` may only ever be the frozen reviewed SHA.
        if let Some(reviewed_sha) = prior_identity.reviewed_sha.as_deref() {
            for candidate in proposed
                .merge_candidates
                .iter()
                .filter(|candidate| candidate.attempt_id == attempt.id)
            {
                if let Some(merged) = candidate.merge_state.merged_commit.as_deref()
                    && merged != reviewed_sha
                {
                    return Err(reject("merged commit"));
                }
            }
        }
    }
    Ok(())
}

fn work_item_children_digest(item: &WorkItem) -> Result<String, WorkModelStorageError> {
    let bytes =
        serde_json::to_vec(&(&item.attempts, &item.merge_candidates)).map_err(|source| {
            WorkModelStorageError::ParseFile {
                path: PathBuf::from("<work-item-transaction>"),
                source,
            }
        })?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

fn aggregate_snapshot_matches(
    left: &WorkItem,
    right: &WorkItem,
) -> Result<bool, WorkModelStorageError> {
    let path = PathBuf::from("<work-item-transaction>");
    let left = to_json_pretty(left).map_err(|source| WorkModelStorageError::ParseFile {
        path: path.clone(),
        source,
    })?;
    let right = to_json_pretty(right)
        .map_err(|source| WorkModelStorageError::ParseFile { path, source })?;
    Ok(left == right)
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseLearnerModeError(String);

impl fmt::Display for ParseLearnerModeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "unknown learner mode: {} (expected capture or no-expertise)",
            self.0
        )
    }
}

impl Error for ParseLearnerModeError {}

pub fn to_json_pretty<T: Serialize>(value: &T) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(value).map(|json| format!("{json}\n"))
}

pub fn from_json<T: for<'de> Deserialize<'de>>(content: &str) -> Result<T, serde_json::Error> {
    serde_json::from_str(content)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writer_run_round_trips_and_defaults_new_progress_fields() {
        let run = WriterRun {
            task_id: "attempt-1-write-2".to_string(),
            outcome: WriterOutcome::Continue,
            kind: WriterRunKind::PreReviewContinuation,
            provider: "codex".to_string(),
            session_id: Some("thread-1".to_string()),
            continuation: 2,
            checked_required: 3,
            completed_matrix: 0,
            candidate_commit: "abc123".to_string(),
        };
        let encoded = serde_json::to_value(&run).unwrap();
        assert_eq!(serde_json::from_value::<WriterRun>(encoded).unwrap(), run);

        let legacy: WriterRun = serde_json::from_value(serde_json::json!({
            "task_id": "attempt-1-write-1",
            "outcome": "continue",
            "provider": "codex",
            "continuation": 0
        }))
        .unwrap();
        assert_eq!(legacy.kind, WriterRunKind::Initial);
        assert_eq!(legacy.checked_required, 0);
        assert!(legacy.candidate_commit.is_empty());
    }

    #[test]
    fn coder_already_ran_evidence_pending_is_non_relaunchable() {
        // The generic per-phase disposition: a coder that already ran but whose
        // host evidence could not be persisted must never be relaunched, while a
        // pre-side-effect failure stays relaunchable.
        assert!(!CoderPhaseRecovery::CoderAlreadyRanEvidencePending.is_relaunchable());
        assert!(CoderPhaseRecovery::Relaunchable.is_relaunchable());

        // The Learner specialization maps its evidence-pending failure onto the
        // generic non-relaunchable disposition, distinct from a resumable
        // transcript-pump fault and a rerunnable generic failure.
        assert_eq!(
            LearningFailureKind::EvidencePending.phase_recovery(),
            CoderPhaseRecovery::CoderAlreadyRanEvidencePending
        );
        assert!(
            !LearningFailureKind::EvidencePending
                .phase_recovery()
                .is_relaunchable()
        );
        assert!(
            LearningFailureKind::TranscriptPump
                .phase_recovery()
                .is_relaunchable()
        );
        assert!(
            LearningFailureKind::Generic
                .phase_recovery()
                .is_relaunchable()
        );

        // A learning record carrying the evidence-pending kind is itself
        // non-relaunchable, yet remains pending so it still blocks advancement.
        let evidence_pending = AttemptLearning::failed_with_kind(
            1,
            "learner produced a handoff but persisting it failed",
            LearningFailureKind::EvidencePending,
        );
        assert!(!evidence_pending.is_relaunchable());
        assert!(evidence_pending.is_pending());

        // A transcript-pump failure stays relaunchable; a succeeded record has no
        // failure kind and is trivially relaunchable (never re-run because it is
        // not pending).
        let pump =
            AttemptLearning::failed_with_kind(1, "pump", LearningFailureKind::TranscriptPump);
        assert!(pump.is_relaunchable());
    }

    #[test]
    fn progress_contract_derives_immutable_manifest_from_required_plan_rows() {
        // Required-progress production B1: a Plan's required rows (Req? = Yes) derive
        // an immutable manifest of stable-id/requirement entries; non-required rows
        // are excluded, and a Plan with no required rows leaves the Attempt unmarked
        // and legacy-compatible.
        let plan = "\
| # | State reached | Behaviors | Verification | Req? |
|---|---------------|-----------|--------------|------|
| 1 | Model records disposition | B1 | tests | Yes |
| 2 | Learner reserves once | B2 | tests | Yes |
| 3 | Optional cleanup | B3 | tests | No |
";
        let contract = ProgressContract::from_plan(plan).expect("required rows derive a contract");
        assert_eq!(contract.version, PROGRESS_CONTRACT_VERSION);
        assert_eq!(
            contract.required,
            vec![
                RequiredProgressEntry {
                    id: "step-1".to_string(),
                    requirement: "Model records disposition".to_string(),
                },
                RequiredProgressEntry {
                    id: "step-2".to_string(),
                    requirement: "Learner reserves once".to_string(),
                },
            ],
            "only required (Req? = Yes) rows become manifest entries, with step-<#> ids"
        );

        // The rendered section is exactly one unchecked top-level entry per required
        // row, in manifest order, under the authoritative heading.
        assert_eq!(
            contract.render_required_completion_section(),
            "## Required completion\n\n\
             - [ ] step-1 — Model records disposition\n\
             - [ ] step-2 — Learner reserves once\n"
        );

        // A Plan with no required rows carries no marker.
        let plan_no_required = "\
| # | State reached | Req? |
|---|---------------|------|
| 1 | Optional | No |
";
        assert!(ProgressContract::from_plan(plan_no_required).is_none());
        assert!(ProgressContract::from_plan("no table here").is_none());
    }

    #[test]
    fn completed_matrix_projects_exact_required_progress() {
        let contract = ProgressContract {
            version: PROGRESS_CONTRACT_VERSION,
            required: vec![RequiredProgressEntry {
                id: "step-1".to_string(),
                requirement: "Preserve the approved punctuation.".to_string(),
            }],
        };
        let matrix = WriterCompletionMatrix {
            schema_version: WRITER_COMPLETION_CONTRACT_VERSION,
            task_id: "attempt-1-write-1".to_string(),
            rows: vec![WriterCompletionRow {
                responsibility: None,
                id: "plan:step-1".to_string(),
                kind: WriterCompletionRequirementKind::Plan,
                source: "plan.md#step-1".to_string(),
                requirement: "Preserve the approved punctuation.".to_string(),
                verification_refs: vec!["cargo test projection".to_string()],
                command: None,
                state: WriterCompletionState::Complete,
                implementation_evidence: vec!["src/work_model.rs".to_string()],
                verification: vec![WriterVerificationClaim {
                    command: "cargo test projection".to_string(),
                    result: WriterVerificationResult::Pass,
                }],
                applicable_constraints: Vec::new(),
            }],
        };
        let existing = "# Progress\n\n\
                        ## Required completion\n\n\
                        - [x] step-1 — Preserve the approved punctuation; Evidence: writer edit\n\
                          - commit abc123\n\n\
                        ## Review follow-ups\n\n\
                        - [ ] Keep this finding\n";

        let projected = contract
            .project_completed_required_progress(
                existing,
                &matrix,
                ".fluent/work/artifacts/work-1/attempt-1/write-1/writer-completion.json",
            )
            .unwrap();

        assert!(projected.starts_with("# Progress\n\n"));
        assert!(projected.ends_with("## Review follow-ups\n\n- [ ] Keep this finding\n"));
        assert!(projected.contains(
            "- [x] step-1 — Preserve the approved punctuation.; Evidence: \
             .fluent/work/artifacts/work-1/attempt-1/write-1/writer-completion.json#plan:step-1"
        ));
        assert!(!projected.contains("writer edit"));
        assert!(!projected.contains("commit abc123"));
        assert_eq!(
            contract
                .project_completed_required_progress(
                    &projected,
                    &matrix,
                    ".fluent/work/artifacts/work-1/attempt-1/write-1/writer-completion.json",
                )
                .unwrap(),
            projected,
            "replaying the host projection is idempotent"
        );

        let restored = contract
            .project_completed_required_progress(
                "# Progress\n\n## Review follow-ups\n\n- [ ] Keep this finding\n",
                &matrix,
                ".fluent/work/artifacts/work-1/attempt-1/write-1/writer-completion.json",
            )
            .unwrap();
        assert!(restored.contains("## Required completion"));
        assert!(restored.contains("## Review follow-ups\n\n- [ ] Keep this finding"));
    }

    #[test]
    fn required_progress_projection_rejects_matrix_contract_disagreement() {
        let contract = ProgressContract {
            version: PROGRESS_CONTRACT_VERSION,
            required: vec![RequiredProgressEntry {
                id: "step-1".to_string(),
                requirement: "Approved requirement.".to_string(),
            }],
        };
        let matrix = WriterCompletionMatrix {
            schema_version: WRITER_COMPLETION_CONTRACT_VERSION,
            task_id: "attempt-1-write-1".to_string(),
            rows: vec![WriterCompletionRow {
                responsibility: None,
                id: "plan:step-1".to_string(),
                kind: WriterCompletionRequirementKind::Plan,
                source: "plan.md#step-1".to_string(),
                requirement: "Different requirement.".to_string(),
                verification_refs: Vec::new(),
                command: None,
                state: WriterCompletionState::Complete,
                implementation_evidence: vec!["src/lib.rs".to_string()],
                verification: vec![WriterVerificationClaim {
                    command: "cargo test".to_string(),
                    result: WriterVerificationResult::Pass,
                }],
                applicable_constraints: Vec::new(),
            }],
        };

        let error = contract
            .project_completed_required_progress("", &matrix, "writer-completion.json")
            .unwrap_err();

        assert!(error.contains("disagrees"));
    }

    #[test]
    fn canonical_plan_template_assigns_required_responsibility() {
        let reference = include_str!("../skills/fluent.full/references/plan-execution.md");
        let template = reference
            .split("## Plan format")
            .nth(1)
            .unwrap()
            .split("```markdown\n")
            .nth(1)
            .unwrap()
            .split("```")
            .next()
            .unwrap();
        let mut item = WorkItem::planned("template", "Use the canonical Plan template");
        item.planning_context = Some(PlanningContext {
            plan: Some(template.into()),
            ..Default::default()
        });
        item.add_initial_attempt("attempt-1").unwrap();
        let contract = item.attempts[0]
            .writer_completion_contract
            .as_ref()
            .unwrap();
        assert!(!contract.requirements.is_empty());
        assert!(
            contract
                .requirements
                .iter()
                .filter(|row| row.kind == WriterCompletionRequirementKind::Plan)
                .all(|row| row.responsibility.is_some()),
            "required template rows need explicit responsibility"
        );
        assert!(
            template
                .lines()
                .find(|line| line.starts_with("| #"))
                .unwrap()
                .ends_with("| Responsibility | Req? |")
        );
    }

    #[test]
    fn approach_revision_preserves_owned_plan_contract() {
        let prior = "# Approach\n\n## Key decisions\n\n### Keep boundary\n\nChoice: Original.\n";
        let replacement = prior.replace("Keep boundary", "Change boundary");
        let contract = WriterCompletionContract::from_planning_context(&PlanningContext {
            approach: Some(prior.into()),
            plan: Some("| # | State reached | Behaviors | Verification | Responsibility | Req? |\n| 1 | Check host. | | `bash tests/host.sh` | host | Yes |\n".into()),
            ..Default::default()
        }).unwrap();
        let revised = contract.with_revised_approach(prior, &replacement).unwrap();
        assert_eq!(
            revised
                .requirements
                .iter()
                .filter(|row| row.kind == WriterCompletionRequirementKind::Plan)
                .collect::<Vec<_>>(),
            contract
                .requirements
                .iter()
                .filter(|row| row.kind == WriterCompletionRequirementKind::Plan)
                .collect::<Vec<_>>()
        );
        assert!(
            revised
                .requirements
                .iter()
                .any(|row| row.id == "approach:decision:change-boundary")
        );
        assert!(contract.with_revised_approach(&replacement, prior).is_err());
    }

    #[test]
    fn explicit_plan_responsibility_survives_intake_and_rejects_writer_claims() {
        let mut item = WorkItem::planned("ownership", "Assign required work");
        item.planning_context = Some(PlanningContext {
            plan: Some(
                "| # | State reached | Behaviors | Verification | Responsibility | Req? |\n\
                |---|---|---|---|---|---|\n\
                | 1 | Implement the fix | | `cargo test fix` | writer | Yes |\n\
                | 2 | Verify the host | | `bash tests/host.sh` | host | Yes |\n\
                | 3 | Review the candidate | | Applicable reviews | review | Yes |\n\
                | 4 | Test the candidate | | Declared suite | tester | Yes |\n\
                | 5 | Record Learning | | Successful Learner | learner | Yes |\n"
                    .into(),
            ),
            ..Default::default()
        });
        item.add_initial_attempt("attempt-1").unwrap();
        let contract = item.attempts[0]
            .writer_completion_contract
            .as_ref()
            .unwrap();
        let value = serde_json::to_value(contract).unwrap();
        assert_eq!(value["requirements"][1]["responsibility"], "host");
        assert_eq!(contract.requirements.len(), 5);
        let mut matrix = WriterCompletionMatrix::skeleton(contract, "writer", &[], None).unwrap();
        matrix.rows[0].state = WriterCompletionState::Complete;
        matrix.rows[0].implementation_evidence = vec!["src/lib.rs: fix".into()];
        matrix.rows[0].verification = vec![WriterVerificationClaim {
            command: "cargo test fix".into(),
            result: WriterVerificationResult::Pass,
        }];
        let status = matrix.status_against(contract, "writer", &[]).unwrap();
        assert!(
            status.is_complete(),
            "only implementation belongs to the Writer: {status:?}"
        );
        assert_eq!(status.required, 1);
        assert_eq!(
            item.attempts[0]
                .progress_contract
                .as_ref()
                .unwrap()
                .required
                .len(),
            1
        );
        matrix.rows[1].state = WriterCompletionState::Complete;
        assert!(
            matrix
                .repair_writer_claims_against(contract, "writer", &[])
                .is_err()
        );
        let mut value = serde_json::to_value(
            WriterCompletionMatrix::skeleton(contract, "writer", &[], None).unwrap(),
        )
        .unwrap();
        value["rows"][1]["responsibility"] = serde_json::json!("writer");
        let mut tampered: WriterCompletionMatrix = serde_json::from_value(value).unwrap();
        assert!(
            tampered
                .repair_writer_claims_against(contract, "writer", &[])
                .is_err()
        );
    }

    #[test]
    fn explicit_plan_responsibility_rejects_unknown_or_mixed_assignments() {
        for responsibility in ["robot", "writer, host", ""] {
            let mut item = WorkItem::planned("ownership", "Reject ambiguous assignments");
            item.planning_context = Some(PlanningContext {
                plan: Some(format!(
                    "| # | State reached | Behaviors | Verification | Responsibility | Req? |\n| 1 | Fix and verify | | `bash tests/host.sh` | {responsibility} | Yes |\n"
                )),
                ..Default::default()
            });
            assert!(
                item.add_initial_attempt("attempt-1").is_err(),
                "accepted {responsibility:?}"
            );
            assert!(item.attempts.is_empty());
        }
    }

    #[test]
    fn writer_completion_contract_maps_approved_planning_inputs() {
        let context = PlanningContext {
            behaviors: Some(
                "# Behaviors (diff)\n\n\
                 ## Dashboard navigation\n\n\
                 ### B1\n\n\
                 WHEN the user presses Down,\n\
                 THE SYSTEM SHALL move the selection by one row.\n\
                 Test: tests/dashboard.rs (down_moves_selection)\n"
                    .to_string(),
            ),
            approach: Some(
                "# Approach\n\n\
                 ## Key decisions\n\n\
                 ### Keep selection in app state\n\n\
                 Choice: The app owns selection and rendering receives it by value.\n\n\
                 Why: Navigation semantics stay outside terminal drawing.\n\n\
                 ## Structure\n\n\
                 The app owns selection; render depends only on render data.\n\n\
                 ## Execution guidance\n\n\
                 - Keep render independent from app state,\n\
                   including layout-aware selection.\n\
                   Preserve explicit navigation boundaries.\n\n\
                 - Keep diagnostics reachable\n\
                   without coupling rendering to terminal input.\n\
                   #not-a-heading remains part of the constraint.\n\n\
                 ### Risks\n\n\
                 - Do not treat this risk as execution guidance.\n"
                    .to_string(),
            ),
            plan: Some(
                "| # | State reached | Behaviors | Verification | Req? |\n\
                 |---|---------------|-----------|--------------|------|\n\
                 | 1 | Down moves selection | Dashboard navigation:B1 | `cargo test down_moves_selection` | Yes |\n"
                    .to_string(),
            ),
            ..Default::default()
        };

        let contract = WriterCompletionContract::from_planning_context(&context)
            .expect("approved planning inputs derive a completion contract");
        assert_eq!(contract.version, WRITER_COMPLETION_CONTRACT_VERSION);
        assert_eq!(
            contract
                .requirements
                .iter()
                .map(|entry| (entry.kind, entry.id.as_str()))
                .collect::<Vec<_>>(),
            vec![
                (
                    WriterCompletionRequirementKind::Behavior,
                    "behavior:dashboard-navigation:b1"
                ),
                (
                    WriterCompletionRequirementKind::Approach,
                    "approach:decision:keep-selection-in-app-state"
                ),
                (
                    WriterCompletionRequirementKind::Approach,
                    "approach:structure"
                ),
                (
                    WriterCompletionRequirementKind::Approach,
                    "approach:guidance:1"
                ),
                (
                    WriterCompletionRequirementKind::Approach,
                    "approach:guidance:2"
                ),
                (WriterCompletionRequirementKind::Plan, "plan:step-1"),
            ]
        );
        assert_eq!(
            contract.requirements[3].requirement,
            "Keep render independent from app state, including layout-aware selection. Preserve explicit navigation boundaries."
        );
        assert_eq!(
            contract.requirements[4].requirement,
            "Keep diagnostics reachable without coupling rendering to terminal input. #not-a-heading remains part of the constraint."
        );
        assert_eq!(
            contract.requirements[0].verification_refs,
            vec!["tests/dashboard.rs (down_moves_selection)"]
        );
        assert_eq!(
            contract.requirements[5].verification_refs,
            vec!["`cargo test down_moves_selection`"]
        );
        assert_eq!(
            contract.requirements[5]
                .command
                .as_ref()
                .map(|command| command.command.as_str()),
            Some("cargo test down_moves_selection")
        );
        assert_eq!(
            contract.requirements[5]
                .command
                .as_ref()
                .map(|command| command.source_row.as_str()),
            Some("plan:step-1")
        );
        assert_eq!(
            contract.requirements[5]
                .command
                .as_ref()
                .map(|command| command.working_directory.as_str()),
            Some(".")
        );

        let mut item = WorkItem::planned("matrix-work", "Matrix work");
        item.planning_context = Some(context);
        item.add_initial_attempt("attempt-1").unwrap();
        assert_eq!(
            item.attempts[0].writer_completion_contract.as_ref(),
            Some(&contract),
            "the approved manifest is stamped before the Attempt is persisted"
        );
    }

    #[test]
    fn cleanup_recheck_behavior_enters_completion_contract() {
        let requirements =
            parse_behavior_completion_requirements(include_str!("../documentation/behaviors.md"));
        let recheck = requirements
            .iter()
            .find(|requirement| requirement.id == "behavior:cleanup:b11")
            .expect("cleanup recheck behavior must enter the completion contract");
        assert!(
            recheck
                .requirement
                .contains("recheck its current eligibility")
        );
        assert_eq!(
            recheck.verification_refs,
            vec![
                "tests/binary.rs (workflow_cleanup_rechecks_current_eligibility), tests/binary.rs (workflow_cleanup_holds_eligibility_through_removal)"
            ]
        );
    }

    #[test]
    fn public_continuation_heading_preserves_earlier_behavior_identities() {
        let requirements =
            parse_behavior_completion_requirements(include_str!("../documentation/behaviors.md"));
        for number in 20..=33 {
            let suffix = format!("b{number}");
            let earlier = requirements
                .iter()
                .find(|requirement| requirement.id == format!("behavior:tester-task:{suffix}"))
                .unwrap_or_else(|| panic!("Tester Task {suffix} must survive the new heading"));
            assert!(!earlier.requirement.is_empty());
            assert!(!earlier.verification_refs.is_empty());
        }
        assert!(requirements.iter().any(|requirement| {
            requirement.id == "behavior:public-workflow-continuation:b20"
                && requirement.requirement.contains("ordinary reviewer")
        }));
    }

    #[test]
    fn plan_check_captures_approved_applicability() {
        for (policy, valid) in [
            (
                r#"{"environment":{"os":"linux"},"comparison_refs":["main"]}"#,
                true,
            ),
            (r#"{"freshness":{"kind":"live-state"}}"#, false),
            ("invalid", false),
        ] {
            let context = PlanningContext {
                plan: Some(format!(
                    "| # | State reached | Behaviors | Verification | Applicability | Responsibility | Req? |\n| 1 | Check host. | | `bash check.sh` | {policy} | host | Yes |\n"
                )),
                ..Default::default()
            };
            let contract = WriterCompletionContract::from_planning_context(&context).unwrap();
            assert_eq!(contract.validate().is_ok(), valid, "{policy}");
            if valid {
                let command = contract.requirements[0].command.as_ref().unwrap();
                assert_eq!(command.applicability.environment["os"], "linux");
                assert_eq!(command.applicability.comparison_refs, ["main"]);
            }
        }
    }

    #[test]
    fn writer_completion_contract_separates_plan_command_from_explanation() {
        let context = PlanningContext {
            plan: Some(
                "| # | State reached | Behaviors | Verification | Req? |\n\
                 |---|---------------|-----------|--------------|------|\n\
                 | 6 | Run DGX_BRING_UP_LIVE=1 ./tests/dgx-bring-up-smoke.sh unchanged on spark1. | | The live smoke preserves isolation and reports content-free evidence. | Yes |\n"
                    .to_string(),
            ),
            ..Default::default()
        };

        let contract = WriterCompletionContract::from_planning_context(&context).unwrap();
        let requirement = contract
            .requirements
            .iter()
            .find(|requirement| requirement.id == "plan:step-6")
            .unwrap();
        assert_eq!(
            requirement.requirement,
            "Run DGX_BRING_UP_LIVE=1 ./tests/dgx-bring-up-smoke.sh unchanged on spark1."
        );
        assert_eq!(
            requirement.verification_refs,
            vec!["The live smoke preserves isolation and reports content-free evidence."]
        );
        let command = requirement.command.as_ref().unwrap();
        assert_eq!(
            command.command,
            "DGX_BRING_UP_LIVE=1 ./tests/dgx-bring-up-smoke.sh"
        );
        assert_eq!(command.source_row, "plan:step-6");
        assert_eq!(command.working_directory, ".");
        assert_eq!(
            command.environment,
            BTreeMap::from([("DGX_BRING_UP_LIVE".to_string(), "1".to_string())])
        );
    }

    #[test]
    fn writer_completion_matrix_reconciles_claims_and_corrective_findings() {
        let contract = WriterCompletionContract {
            version: WRITER_COMPLETION_CONTRACT_VERSION,
            requirements: vec![
                WriterCompletionRequirement {
                    responsibility: None,
                    id: "behavior:dashboard:b1".to_string(),
                    kind: WriterCompletionRequirementKind::Behavior,
                    source: "behaviors.md#dashboard:b1".to_string(),
                    requirement: "WHEN refreshed, THE SYSTEM SHALL preserve selection.".to_string(),
                    verification_refs: vec!["tests/dashboard.rs (preserves_selection)".to_string()],
                    command: None,
                },
                WriterCompletionRequirement {
                    responsibility: None,
                    id: "plan:step-2".to_string(),
                    kind: WriterCompletionRequirementKind::Plan,
                    source: "plan.md#step-2".to_string(),
                    requirement: "Keep Retry and Dismiss session-owned.".to_string(),
                    verification_refs: vec!["cargo test hud_actions".to_string()],
                    command: None,
                },
            ],
        };
        let findings = vec![WriterCompletionFinding {
            id: "finding:tests:stable-id".to_string(),
            role: "tests".to_string(),
            source: ".fluent/work/artifacts/review-tests/review.md".to_string(),
            requirement: "Assert overflow navigation directly".to_string(),
        }];
        let mut matrix =
            WriterCompletionMatrix::skeleton(&contract, "attempt-1-write-2", &findings, None)
                .unwrap();
        assert_eq!(matrix.rows.len(), 3);
        assert_eq!(
            contract.applicable_constraint_ids().collect::<Vec<_>>(),
            vec!["behavior:dashboard:b1", "plan:step-2"]
        );
        assert!(
            !matrix
                .status_against(&contract, "attempt-1-write-2", &findings)
                .unwrap()
                .is_complete()
        );

        for row in &mut matrix.rows {
            row.state = WriterCompletionState::Complete;
            row.implementation_evidence = vec!["src/dashboard.rs".to_string()];
            row.verification = vec![WriterVerificationClaim {
                command: "cargo test preserves_selection".to_string(),
                result: WriterVerificationResult::Pass,
            }];
        }
        assert!(
            matrix
                .status_against(&contract, "attempt-1-write-2", &findings)
                .unwrap()
                .incomplete_reason
                .as_deref()
                .is_some_and(
                    |reason| reason.contains("applicable behavior, approach, or required Plan")
                )
        );
        matrix.rows[2].applicable_constraints = vec!["plan:step-2".to_string()];
        assert!(
            matrix
                .status_against(&contract, "attempt-1-write-2", &findings)
                .unwrap()
                .is_complete()
        );

        let mut rewritten = matrix.clone();
        rewritten.rows[0].requirement.push_str(" rewritten");
        assert!(
            rewritten
                .status_against(&contract, "attempt-1-write-2", &findings)
                .unwrap_err()
                .contains("rewritten")
        );

        let mut missing = matrix.clone();
        missing.rows.pop();
        assert!(
            missing
                .status_against(&contract, "attempt-1-write-2", &findings)
                .unwrap_err()
                .contains("expected 3")
        );

        let mut unknown_constraint = matrix.clone();
        unknown_constraint.rows[2].applicable_constraints = vec!["behavior:unknown:b9".to_string()];
        assert!(
            unknown_constraint
                .status_against(&contract, "attempt-1-write-2", &findings)
                .unwrap_err()
                .contains("unknown constraint")
        );

        let mut contradictory = matrix.clone();
        contradictory.rows[0].state = WriterCompletionState::Pending;
        assert!(
            contradictory
                .status_against(&contract, "attempt-1-write-2", &findings)
                .unwrap_err()
                .contains("pending but carries completion claims")
        );
    }

    #[test]
    fn writer_completion_matrix_repairs_only_writer_owned_claims() {
        let contract = WriterCompletionContract {
            version: WRITER_COMPLETION_CONTRACT_VERSION,
            requirements: vec![WriterCompletionRequirement {
                responsibility: None,
                id: "behavior:dashboard:b1".to_string(),
                kind: WriterCompletionRequirementKind::Behavior,
                source: "behaviors.md#dashboard:b1".to_string(),
                requirement: "WHEN refreshed, THE SYSTEM SHALL preserve selection.".to_string(),
                verification_refs: vec!["tests/dashboard.rs (preserves_selection)".to_string()],
                command: None,
            }],
        };
        let findings = vec![WriterCompletionFinding {
            id: "finding:tests:stable-id".to_string(),
            role: "tests".to_string(),
            source: ".fluent/work/artifacts/review-tests/review.md".to_string(),
            requirement: "Assert overflow navigation directly".to_string(),
        }];
        let mut matrix =
            WriterCompletionMatrix::skeleton(&contract, "attempt-1-write-2", &findings, None)
                .unwrap();
        for row in &mut matrix.rows {
            row.state = WriterCompletionState::Complete;
            row.implementation_evidence = vec!["src/dashboard.rs".to_string()];
            row.verification = vec![WriterVerificationClaim {
                command: "cargo test preserves_selection".to_string(),
                result: WriterVerificationResult::Pass,
            }];
        }
        matrix.rows[0].applicable_constraints = vec![
            "behavior:dashboard:b1".to_string(),
            "plan:step-1".to_string(),
        ];
        matrix.rows[1].applicable_constraints = vec![
            "behavior:dashboard:b1".to_string(),
            "behavior:unknown:b9".to_string(),
        ];

        let repairs = matrix
            .repair_writer_claims_against(&contract, "attempt-1-write-2", &findings)
            .unwrap();

        assert_eq!(repairs.len(), 2);
        assert!(matrix.rows[0].applicable_constraints.is_empty());
        assert_eq!(
            matrix.rows[1].applicable_constraints,
            vec!["behavior:dashboard:b1"]
        );
        assert!(
            matrix
                .status_against(&contract, "attempt-1-write-2", &findings)
                .unwrap()
                .is_complete()
        );

        let mut rewritten = matrix;
        rewritten.rows[0].requirement = "A rewritten requirement".to_string();
        let repairs = rewritten
            .repair_writer_claims_against(&contract, "attempt-1-write-2", &findings)
            .unwrap();
        assert_eq!(
            repairs[0].kind,
            WriterCompletionRepairKind::RestoredHostOwnedFields
        );
        assert_eq!(
            rewritten.rows[0].requirement,
            "WHEN refreshed, THE SYSTEM SHALL preserve selection."
        );

        let mut wrong_identity = rewritten;
        wrong_identity.rows[0].id = "behavior:other:b1".to_string();
        assert!(
            wrong_identity
                .repair_writer_claims_against(&contract, "attempt-1-write-2", &findings)
                .unwrap_err()
                .contains("omitted, reordered, or rewritten")
        );
    }

    #[test]
    fn writer_completion_skeleton_does_not_propagate_invalid_planning_mappings() {
        let contract = WriterCompletionContract {
            version: WRITER_COMPLETION_CONTRACT_VERSION,
            requirements: vec![WriterCompletionRequirement {
                responsibility: None,
                id: "behavior:dashboard:b1".to_string(),
                kind: WriterCompletionRequirementKind::Behavior,
                source: "behaviors.md#dashboard:b1".to_string(),
                requirement: "WHEN refreshed, THE SYSTEM SHALL preserve selection.".to_string(),
                verification_refs: Vec::new(),
                command: None,
            }],
        };
        let mut prior =
            WriterCompletionMatrix::skeleton(&contract, "attempt-1-write-1", &[], None).unwrap();
        prior.rows[0].state = WriterCompletionState::Complete;
        prior.rows[0].implementation_evidence = vec!["src/dashboard.rs".to_string()];
        prior.rows[0].verification = vec![WriterVerificationClaim {
            command: "cargo test preserves_selection".to_string(),
            result: WriterVerificationResult::Pass,
        }];
        prior.rows[0].applicable_constraints = vec!["behavior:dashboard:b1".to_string()];

        let next =
            WriterCompletionMatrix::skeleton(&contract, "attempt-1-write-2", &[], Some(&prior))
                .unwrap();

        assert!(next.rows[0].applicable_constraints.is_empty());
        assert_eq!(next.rows[0].state, WriterCompletionState::Complete);
    }

    #[test]
    fn writer_completion_repair_resets_contradictory_pending_claims() {
        let contract = WriterCompletionContract {
            version: WRITER_COMPLETION_CONTRACT_VERSION,
            requirements: vec![WriterCompletionRequirement {
                responsibility: None,
                id: "behavior:dashboard:b1".to_string(),
                kind: WriterCompletionRequirementKind::Behavior,
                source: "behaviors.md#dashboard:b1".to_string(),
                requirement: "WHEN refreshed, THE SYSTEM SHALL preserve selection.".to_string(),
                verification_refs: Vec::new(),
                command: None,
            }],
        };
        let mut matrix =
            WriterCompletionMatrix::skeleton(&contract, "attempt-1-write-1", &[], None).unwrap();
        matrix.rows[0].implementation_evidence = vec!["src/dashboard.rs".to_string()];
        matrix.rows[0].verification = vec![WriterVerificationClaim {
            command: "cargo test preserves_selection".to_string(),
            result: WriterVerificationResult::Pass,
        }];
        matrix.rows[0].applicable_constraints = vec!["behavior:dashboard:b1".to_string()];

        let repairs = matrix
            .repair_writer_claims_against(&contract, "attempt-1-write-1", &[])
            .unwrap();

        assert_eq!(repairs.len(), 1);
        assert_eq!(
            repairs[0].kind,
            WriterCompletionRepairKind::ClearedPendingClaims
        );
        assert!(matrix.rows[0].implementation_evidence.is_empty());
        assert!(matrix.rows[0].verification.is_empty());
        assert!(matrix.rows[0].applicable_constraints.is_empty());
        assert!(
            !matrix
                .status_against(&contract, "attempt-1-write-1", &[])
                .unwrap()
                .is_complete()
        );
    }

    #[test]
    fn writer_completion_matrix_serializes_writer_owned_array_shapes() {
        let contract = WriterCompletionContract {
            version: WRITER_COMPLETION_CONTRACT_VERSION,
            requirements: vec![WriterCompletionRequirement {
                responsibility: None,
                id: "behavior:dashboard:b1".to_string(),
                kind: WriterCompletionRequirementKind::Behavior,
                source: "behaviors.md#dashboard:b1".to_string(),
                requirement: "Preserve selection".to_string(),
                verification_refs: Vec::new(),
                command: None,
            }],
        };
        let matrix = WriterCompletionMatrix::skeleton(&contract, "write-1", &[], None).unwrap();

        let value = serde_json::to_value(matrix).unwrap();
        let row = &value["rows"][0];
        assert_eq!(row["implementation-evidence"], serde_json::json!([]));
        assert_eq!(row["verification"], serde_json::json!([]));
        assert_eq!(row["applicable-constraints"], serde_json::json!([]));
    }

    #[test]
    fn writer_completion_matrix_accepts_scalar_implementation_evidence() {
        let matrix: WriterCompletionMatrix = serde_json::from_value(serde_json::json!({
            "schema-version": 1,
            "task-id": "write-1",
            "rows": [{
                "id": "plan:step-1",
                "kind": "plan",
                "source": "plan.md#step-1",
                "requirement": "Complete the change",
                "state": "complete",
                "implementation-evidence": "commit abc123",
                "verification": [{"command": "cargo test focused", "result": "pass"}],
                "applicable-constraints": []
            }]
        }))
        .unwrap();

        assert_eq!(
            matrix.rows[0].implementation_evidence,
            vec!["commit abc123"]
        );
        assert_eq!(
            serde_json::to_value(matrix).unwrap()["rows"][0]["implementation-evidence"],
            serde_json::json!(["commit abc123"])
        );
    }

    #[test]
    fn writer_completion_contract_uses_combined_context_as_authority() {
        let context = PlanningContext {
            behaviors: Some("## Ignored\n\n### B9\n\nSHALL be ignored".to_string()),
            combined: Some(
                "## Combined behavior\n\n\
                 ### B1\n\n\
                 WHEN combined, THE SYSTEM SHALL use this statement.\n\
                 Test: tests/combined.rs (uses_combined)\n\n\
                 ## Steps\n\n\
                 | # | State reached | Behaviors | Verification | Req? |\n\
                 |---|---------------|-----------|--------------|------|\n\
                 | 1 | Combined state | Combined behavior:B1 | combined check | Yes |\n"
                    .to_string(),
            ),
            ..Default::default()
        };

        let contract = WriterCompletionContract::from_planning_context(&context).unwrap();
        assert!(
            contract
                .requirements
                .iter()
                .any(|row| row.id == "behavior:combined-behavior:b1")
        );
        assert!(
            contract
                .requirements
                .iter()
                .any(|row| row.id == "plan:step-1")
        );
        assert!(
            contract
                .requirements
                .iter()
                .all(|row| row.id != "behavior:ignored:b9")
        );
    }

    #[test]
    fn next_writer_matrix_carries_planning_claims_but_reopens_findings() {
        let contract = WriterCompletionContract {
            version: WRITER_COMPLETION_CONTRACT_VERSION,
            requirements: vec![WriterCompletionRequirement {
                responsibility: None,
                id: "plan:step-1".to_string(),
                kind: WriterCompletionRequirementKind::Plan,
                source: "plan.md#step-1".to_string(),
                requirement: "Dashboard is navigable".to_string(),
                verification_refs: vec!["dashboard journey".to_string()],
                command: None,
            }],
        };
        let finding = WriterCompletionFinding {
            id: "finding:tests:stable-id".to_string(),
            role: "tests".to_string(),
            source: "review-tests/review.md".to_string(),
            requirement: "Add direct assertion".to_string(),
        };
        let mut prior = WriterCompletionMatrix::skeleton(
            &contract,
            "attempt-1-write-1",
            std::slice::from_ref(&finding),
            None,
        )
        .unwrap();
        for row in &mut prior.rows {
            row.state = WriterCompletionState::Complete;
            row.implementation_evidence = vec!["commit abc".to_string()];
            row.verification = vec![WriterVerificationClaim {
                command: "cargo test dashboard".to_string(),
                result: WriterVerificationResult::Pass,
            }];
        }

        let next = WriterCompletionMatrix::skeleton(
            &contract,
            "attempt-1-write-2",
            std::slice::from_ref(&finding),
            Some(&prior),
        )
        .unwrap();
        assert_eq!(next.rows[0].state, WriterCompletionState::Complete);
        assert_eq!(next.rows[1].state, WriterCompletionState::Pending);
        assert!(next.rows[1].implementation_evidence.is_empty());
    }

    #[test]
    fn writer_completion_matrix_refuses_unbounded_context() {
        let oversized_contract = WriterCompletionContract {
            version: WRITER_COMPLETION_CONTRACT_VERSION,
            requirements: vec![WriterCompletionRequirement {
                responsibility: None,
                id: "behavior:large:b1".to_string(),
                kind: WriterCompletionRequirementKind::Behavior,
                source: "behaviors.md#large:b1".to_string(),
                requirement: "x".repeat(WRITER_COMPLETION_MATRIX_MAX_BYTES),
                verification_refs: Vec::new(),
                command: None,
            }],
        };
        let error = WriterCompletionMatrix::skeleton(&oversized_contract, "write-1", &[], None)
            .unwrap_err();
        assert!(error.contains("maximum"));

        let contract = WriterCompletionContract {
            version: WRITER_COMPLETION_CONTRACT_VERSION,
            requirements: vec![WriterCompletionRequirement {
                responsibility: None,
                id: "behavior:bounded:b1".to_string(),
                kind: WriterCompletionRequirementKind::Behavior,
                source: "behaviors.md#bounded:b1".to_string(),
                requirement: "Keep evidence bounded".to_string(),
                verification_refs: Vec::new(),
                command: None,
            }],
        };
        let mut matrix = WriterCompletionMatrix::skeleton(&contract, "write-1", &[], None).unwrap();
        matrix.rows[0].state = WriterCompletionState::Complete;
        matrix.rows[0].implementation_evidence =
            vec!["x".repeat(WRITER_COMPLETION_MATRIX_MAX_BYTES)];
        matrix.rows[0].verification = vec![WriterVerificationClaim {
            command: "cargo test bounded".to_string(),
            result: WriterVerificationResult::Pass,
        }];
        assert!(
            matrix
                .status_against(&contract, "write-1", &[])
                .unwrap_err()
                .contains("maximum")
        );
    }

    #[test]
    fn planning_scope_counts_behaviors_and_required_plan_rows() {
        let context = PlanningContext {
            behaviors: Some(
                "## Dashboard\n\n### B1\n\nFirst.\n\n### B2\n\nSecond.\n\n### B3\n\nThird.\n"
                    .to_string(),
            ),
            plan: Some(
                "Long prose does not change scope.\n\n\
                 | # | State reached | Verification | Req? |\n\
                 |---|---------------|--------------|------|\n\
                 | 1 | Required one | test one | Yes |\n\
                 | 2 | Optional work | test two | Optional |\n\
                 | 3 | Required three | test three | yes |\n\
                 | 4 | TBD | | |\n"
                    .to_string(),
            ),
            ..Default::default()
        };

        let assessment = context.scope_assessment(1, true).unwrap();
        assert_eq!(assessment.behavior_count, 3);
        assert_eq!(assessment.required_steps, 2);
        assert_eq!(assessment.recommended_slices, 3);
        assert!(assessment.is_large());
        assert!(assessment.large_scope_authorized);
        assert_eq!(assessment.calibration_version, 1);

        let combined = PlanningContext {
            combined: Some(format!(
                "{}\n\n{}",
                context.behaviors.as_deref().unwrap(),
                context.plan.as_deref().unwrap()
            )),
            ..Default::default()
        };
        assert_eq!(combined.required_plan_steps(), 2);
        assert_eq!(combined.behavior_count(), 3);

        let boundary = context.scope_assessment(3, false).unwrap();
        assert!(!boundary.is_large());
        assert_eq!(boundary.recommended_slices, 1);
    }

    #[test]
    fn legacy_plan_only_scope_assessment_keeps_legacy_semantics() {
        let assessment: PlanningScopeAssessment =
            serde_json::from_str(r#"{"required_steps":2,"limit":2,"recommended_slices":1}"#)
                .unwrap();

        assert_eq!(assessment.calibration_version, 0);
        assert_eq!(assessment.behavior_count, 0);
        assert!(!assessment.is_large());
    }

    #[test]
    fn planning_scope_assessment_is_immutable_after_creation() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = WorkModelStore::new(tmp.path());
        let mut item = WorkItem::planned("bounded", "Bounded work");
        item.planning_context = Some(PlanningContext {
            plan: Some(
                "| # | State reached | Req? |\n\
                 |---|---------------|------|\n\
                 | 1 | First | Yes |\n\
                 | 2 | Second | Yes |\n"
                    .to_string(),
            ),
            ..Default::default()
        });
        item.planning_scope = item
            .planning_context
            .as_ref()
            .and_then(|context| context.scope_assessment(2, false));
        store.create_work_item(&item).unwrap();

        item.planning_scope.as_mut().unwrap().limit = 3;
        let error = store.write_work_item(&item).unwrap_err();
        assert!(error.to_string().contains("intake diagnostic is immutable"));

        let stored = store.read_work_item("bounded").unwrap();
        assert_eq!(stored.planning_scope.unwrap().limit, 2);
    }

    #[test]
    fn intake_contracts_cannot_be_added_after_first_attempt() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = WorkModelStore::new(tmp.path());
        let mut item = WorkItem::planned("started", "Started work");
        store.create_work_item(&item).unwrap();
        item.add_initial_attempt("attempt-1").unwrap();
        store.write_work_item(&item).unwrap();

        let plan = PlanningContext {
            plan: Some(
                "| # | State reached | Req? |\n\
                 |---|---------------|------|\n\
                 | 1 | First | Yes |\n"
                    .to_string(),
            ),
            ..Default::default()
        };
        item.planning_scope = plan.scope_assessment(12, false);
        item.planning_context = Some(plan);
        let scope_error = store.write_work_item(&item).unwrap_err();
        assert!(
            scope_error
                .to_string()
                .contains("scope authorization must be defined before the first Attempt")
        );

        item.planning_context = None;
        item.planning_scope = None;
        item.release_contract = Some(
            ReleaseContract::new(vec![ReleaseCriterion {
                id: "tests-pass".to_string(),
                statement: "Configured tests pass".to_string(),
            }])
            .unwrap(),
        );
        let release_error = store.write_work_item(&item).unwrap_err();
        assert!(
            release_error
                .to_string()
                .contains("release acceptance must be defined before the first Attempt")
        );
    }

    #[test]
    fn release_findings_fail_closed_unless_mapped_to_accepted_criteria() {
        let mut contract = ReleaseContract::new(vec![ReleaseCriterion {
            id: "tests-pass".to_string(),
            statement: "Configured tests pass".to_string(),
        }])
        .unwrap();
        let proposed = contract
            .classify_finding(
                "polish".to_string(),
                "Polish secondary docs".to_string(),
                None,
            )
            .unwrap();
        assert_eq!(
            proposed.classification,
            ReleaseFindingClassification::ProposedFollowUp
        );

        let error = contract
            .classify_finding(
                "proof".to_string(),
                "Proof is missing".to_string(),
                Some("unknown".to_string()),
            )
            .unwrap_err();
        assert_eq!(
            error,
            WorkModelError::ReleaseCriterionNotFound {
                criterion_id: "unknown".to_string()
            }
        );
        assert_eq!(contract.findings.len(), 1);

        assert!(
            contract
                .classify_finding("bad/id".to_string(), "Invalid identity".to_string(), None,)
                .is_err()
        );
        assert_eq!(contract.findings.len(), 1);
    }

    #[test]
    fn release_acceptance_criteria_are_immutable_after_creation() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = WorkModelStore::new(tmp.path());
        let mut item = WorkItem::planned("release", "Release");
        item.release_contract = Some(
            ReleaseContract::new(vec![ReleaseCriterion {
                id: "tests-pass".to_string(),
                statement: "Configured tests pass".to_string(),
            }])
            .unwrap(),
        );
        store.create_work_item(&item).unwrap();

        item.release_contract.as_mut().unwrap().criteria[0].statement =
            "A different acceptance bar".to_string();
        let error = store.write_work_item(&item).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("accepted criteria are immutable")
        );

        let stored = store.read_work_item("release").unwrap();
        assert_eq!(
            stored.release_contract.as_ref().unwrap().criteria[0].statement,
            "Configured tests pass"
        );

        let mut stored = stored;
        stored
            .release_contract
            .as_mut()
            .unwrap()
            .classify_finding("docs".to_string(), "Polish docs".to_string(), None)
            .unwrap();
        store.write_work_item(&stored).unwrap();
        stored.release_contract.as_mut().unwrap().findings[0].summary =
            "Rewrite the recorded decision".to_string();
        let error = store.write_work_item(&stored).unwrap_err();
        assert!(error.to_string().contains("append-only"));
    }

    #[test]
    fn materialize_required_progress_is_idempotent_and_preserves_checkmarks() {
        // Required-progress production B1: the host materializes the section into
        // empty progress content, but setup leaves an existing section untouched.
        // The completed-Writer gate owns the later matrix-backed replacement.
        let contract = ProgressContract {
            version: PROGRESS_CONTRACT_VERSION,
            required: vec![RequiredProgressEntry {
                id: "step-1".to_string(),
                requirement: "Do the thing".to_string(),
            }],
        };
        let attempt = Attempt {
            progress_contract: Some(contract),
            ..Default::default()
        };

        // Empty content: the section is materialized.
        let materialized = attempt
            .materialize_required_progress("")
            .expect("an empty file is materialized");
        assert!(materialized.contains("## Required completion"));
        assert!(materialized.contains("- [ ] step-1 — Do the thing"));

        // Pre-existing content makes setup materialization a no-op.
        let checked = "## Required completion\n\n- [x] step-1 — Do the thing; Evidence: src/x.rs\n";
        assert!(
            attempt.materialize_required_progress(checked).is_none(),
            "an existing section is preserved untouched"
        );

        // An Attempt without a contract materializes nothing.
        let legacy = Attempt::default();
        assert!(legacy.materialize_required_progress("").is_none());
    }

    #[test]
    fn reconcile_required_progress_enforces_manifest_and_checks() {
        // Advancement gates B2: a marked Attempt's current required progress must be
        // exactly its manifest with every entry checked and carrying evidence; every
        // other divergence is rejected, while a legacy Attempt and historical prose
        // are never scanned.
        let marked = Attempt {
            progress_contract: Some(ProgressContract {
                version: PROGRESS_CONTRACT_VERSION,
                required: vec![
                    RequiredProgressEntry {
                        id: "step-1".to_string(),
                        requirement: "First".to_string(),
                    },
                    RequiredProgressEntry {
                        id: "step-2".to_string(),
                        requirement: "Second".to_string(),
                    },
                ],
            }),
            ..Default::default()
        };

        // A legacy Attempt (no contract) is never gated, whatever the content.
        let legacy = Attempt::default();
        assert!(
            legacy
                .reconcile_required_progress("anything at all")
                .is_ok()
        );
        assert!(legacy.reconcile_required_progress("").is_ok());

        // Exactly the manifest, every entry checked with evidence, plus unrelated
        // historical prose in other sections that is ignored.
        let ok = "\
# Progress

- [x] some old historical note

## Required completion

- [x] step-1 — First; Evidence: src/a.rs
- [x] step-2 — Second; Evidence: tests/b.rs

## Notes

random banner prose that must be ignored
";
        assert!(marked.reconcile_required_progress(ok).is_ok());

        // Already-checked legacy review rows and their nested notes are historical
        // metadata. They do not change the required manifest or block advancement.
        let checked_legacy_follow_ups = "\
## Required completion

- [x] Address review finding: Preserve the schema
  - commit abc123
  - Evidence: tests/review.rs
- [x] step-1 — First; Evidence: src/a.rs
- [x] Address review finding: Keep compatibility narrow
  - commit def456
- [x] step-2 — Second; Evidence: tests/b.rs
";
        assert!(
            marked
                .reconcile_required_progress(checked_legacy_follow_ups)
                .is_ok(),
            "checked legacy review follow-ups are historical metadata"
        );

        let unchecked_legacy_follow_up = "\
## Required completion

- [x] step-1 — First; Evidence: src/a.rs
- [ ] Address review finding: Still unresolved
- [x] step-2 — Second; Evidence: tests/b.rs
";
        assert!(
            marked
                .reconcile_required_progress(unchecked_legacy_follow_up)
                .is_err(),
            "an unchecked legacy review follow-up must remain blocking"
        );

        let arbitrary_malformed_row = "\
## Required completion

- [x] step-1 — First; Evidence: src/a.rs
- [x] Historical note without the legacy label
- [x] step-2 — Second; Evidence: tests/b.rs
";
        assert!(
            marked
                .reconcile_required_progress(arbitrary_malformed_row)
                .is_err(),
            "an arbitrary malformed top-level row must remain blocking"
        );

        // Each violation is rejected.
        let cases = [
            // Missing section.
            "# Progress\n\n- [x] step-1 — First; Evidence: x\n",
            // Unchecked entry.
            "## Required completion\n\n- [x] step-1 — First; Evidence: x\n- [ ] step-2 — Second\n",
            // Checked without evidence.
            "## Required completion\n\n- [x] step-1 — First\n- [x] step-2 — Second; Evidence: y\n",
            // Deleted entry (step-2 missing).
            "## Required completion\n\n- [x] step-1 — First; Evidence: x\n",
            // Unknown entry.
            "## Required completion\n\n- [x] step-1 — First; Evidence: x\n- [x] step-2 — Second; Evidence: y\n- [x] step-9 — Ghost; Evidence: z\n",
            // Duplicate entry.
            "## Required completion\n\n- [x] step-1 — First; Evidence: x\n- [x] step-1 — First; Evidence: x\n- [x] step-2 — Second; Evidence: y\n",
            // Rewritten requirement.
            "## Required completion\n\n- [x] step-1 — Rewritten; Evidence: x\n- [x] step-2 — Second; Evidence: y\n",
            // Malformed checklist line.
            "## Required completion\n\n- step-1 First\n- [x] step-2 — Second; Evidence: y\n",
        ];
        for source in cases {
            assert!(
                marked.reconcile_required_progress(source).is_err(),
                "expected a required-progress violation for:\n{source}"
            );
        }
    }

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
            base_commit: None,
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
                path: ".fluent/tasks/task-1".to_string(),
            }),
            review_context,
            writer_correction_assignment: None,
            verification_context: None,
            evidence_review_context: None,
            input_artifacts: Vec::new(),
            depends_on: None,
            output: None,
            created_at: None,
            started_at: None,
            completed_at: None,
        }
    }

    #[test]
    fn duplicate_host_evidence_attachment_is_idempotent() {
        let mut attempt = Attempt {
            id: "attempt-1".to_string(),
            ..Attempt::default()
        };
        let recovery = EvidenceRecovery {
            carried_finding: None,
            required_check: None,
            id: "host-evidence-123".to_string(),
            candidate_commit: "abc123".to_string(),
            attachment: EvidenceAttachment {
                snapshot_path: ".fluent/work/artifacts/work/attempt-1/host-evidence/123.json"
                    .to_string(),
                digest: "sha256:123".to_string(),
            },
            targets: vec![EvidenceReviewTarget {
                role: "tests".to_string(),
                prior_review_artifact: ".fluent/work/artifacts/work/attempt-1/tests/review.md"
                    .to_string(),
                review_task_id: None,
                review_digest: None,
            }],
            state: EvidenceRecoveryState::Reviewing,
            created_at: "2026-08-03T17:59:47Z".to_string(),
            code_change_classification: None,
        };

        assert_eq!(
            attempt.attach_evidence_recovery(recovery.clone()).unwrap(),
            recovery
        );
        assert_eq!(
            attempt.attach_evidence_recovery(recovery.clone()).unwrap(),
            recovery
        );
        assert_eq!(attempt.evidence_recoveries, vec![recovery]);
    }

    #[test]
    fn host_evidence_classification_matches_request() {
        let classification = HostEvidenceClassification {
            operation_id: "host-evidence-classification-abc".to_string(),
            disposition: "code-change".to_string(),
            review_artifact: "review.md".to_string(),
            review_digest: "sha256:review".to_string(),
            authorization: "Correct the source.".to_string(),
            authorization_digest: "sha256:authorization".to_string(),
            audit_artifact: ArtifactRef {
                producer_id: "host-evidence-classification-abc".to_string(),
                path: "audit.json".to_string(),
            },
            writer_task_id: "attempt-1-write-2".to_string(),
            classified_at: "2026-09-01T00:00:00Z".to_string(),
        };
        assert!(classification.matches_request(
            "review.md",
            "sha256:review",
            "Correct the source.",
            "sha256:authorization",
        ));
        assert!(!classification.matches_request(
            "review.md",
            "sha256:review",
            "Authorize a different correction.",
            "sha256:authorization",
        ));
    }

    #[test]
    fn host_evidence_classification_is_idempotent() {
        use sha2::Digest;
        use std::fs;

        let project = tempfile::TempDir::new().unwrap();
        let root = project.path();
        let run_git = |args: &[&str]| {
            crate::git::run_stdout(
                root,
                args,
                "run host-evidence classification fixture Git command",
            )
            .unwrap()
        };
        run_git(&["init", "-q"]);
        run_git(&["config", "user.email", "test@example.com"]);
        run_git(&["config", "user.name", "Test User"]);
        fs::write(root.join("seed.txt"), "seed\n").unwrap();
        fs::write(root.join(".gitignore"), ".fluent/\n").unwrap();
        run_git(&["add", "seed.txt", ".gitignore"]);
        run_git(&["commit", "-qm", "seed"]);
        let candidate = run_git(&["rev-parse", "HEAD"]);

        let store = WorkModelStore::new(root);
        let mut item = WorkItem::planned("work-1", "Recover host evidence");
        item.add_initial_attempt("attempt-1").unwrap();
        let writer = &mut item.attempts[0].tasks[0];
        writer.status = TaskStatus::Complete;
        writer.output = Some(TaskOutput {
            workspace_id: "candidate".to_string(),
            workspace_path: root.display().to_string(),
            source_branch: "main".to_string(),
            base_commit: None,
            commit: candidate.clone(),
            no_change: None,
            learner_canonicalization: None,
        });
        item.add_review_tasks("attempt-1", &["tests"]).unwrap();
        let prior = item.attempts[0].tasks.last_mut().unwrap();
        prior.status = TaskStatus::Complete;
        let prior_path = format!("{}/review.md", prior.artifact_area.as_ref().unwrap().path);
        fs::create_dir_all(root.join(&prior_path).parent().unwrap()).unwrap();
        fs::write(root.join(&prior_path), "Verdict: fail\n").unwrap();

        let evidence_bytes = br#"{"schema_version":1,"producer":"trusted host","check":"cargo test exact","working_directory":".","result":"fail","run_at":"2026-09-02T00:00:00Z","output":"failed"}"#;
        let evidence_digest = format!("sha256:{:x}", sha2::Sha256::digest(evidence_bytes));
        let evidence_path = format!(
            ".fluent/work/artifacts/work-1/attempt-1/host-evidence/{}.json",
            &evidence_digest[7..]
        );
        fs::create_dir_all(root.join(&evidence_path).parent().unwrap()).unwrap();
        fs::write(root.join(&evidence_path), evidence_bytes).unwrap();
        let attachment = EvidenceAttachment {
            snapshot_path: evidence_path.clone(),
            digest: evidence_digest.clone(),
        };

        let recovery_id = "host-evidence-proof".to_string();
        let mut targeted = prior.clone();
        targeted.id = "attempt-1-evidence-proof-tests".to_string();
        targeted.status = TaskStatus::Complete;
        targeted.artifact_area = Some(TaskArtifactArea {
            path: work_artifact_path("work-1", "attempt-1", &targeted.id),
        });
        targeted.evidence_review_context = Some(EvidenceReviewContext {
            recovery_id: recovery_id.clone(),
            candidate_commit: candidate.clone(),
            attachment: attachment.clone(),
            prior_review_artifact: prior_path.clone(),
        });
        targeted.input_artifacts = vec![
            ArtifactRef {
                producer_id: "host-evidence".to_string(),
                path: evidence_path.clone(),
            },
            ArtifactRef {
                producer_id: prior.id.clone(),
                path: prior_path.clone(),
            },
        ];
        let targeted_path = format!(
            "{}/review.md",
            targeted.artifact_area.as_ref().unwrap().path
        );
        let targeted_content = "Verdict: fail\nDisposition: evidence-needed\nExternal-Condition: Restore the host.\nAdditional-Proof: Attach proof.\n";
        fs::create_dir_all(root.join(&targeted_path).parent().unwrap()).unwrap();
        fs::write(root.join(&targeted_path), targeted_content).unwrap();
        let review_digest = format!(
            "sha256:{:x}",
            sha2::Sha256::digest(targeted_content.as_bytes())
        );
        let recovery = EvidenceRecovery {
            carried_finding: None,
            required_check: None,
            id: recovery_id,
            candidate_commit: candidate.clone(),
            attachment,
            targets: vec![EvidenceReviewTarget {
                role: "tests".to_string(),
                prior_review_artifact: prior_path,
                review_task_id: Some(targeted.id.clone()),
                review_digest: Some(review_digest),
            }],
            state: EvidenceRecoveryState::NeedsEvidence,
            created_at: now_iso8601(),
            code_change_classification: None,
        };
        let attempt = &mut item.attempts[0];
        attempt.tasks.push(targeted);
        attempt.evidence_recoveries.push(recovery);
        attempt.status = AttemptStatus::NeedsUser;
        attempt.pause_kind = Some(PauseKind::Uncertain);
        attempt.review_state = Some(AttemptReviewState::Failed);
        attempt.completed_at = Some(now_iso8601());
        store.create_work_item(&item).unwrap();

        let first = crate::host_evidence::classify_for_code_change(
            root,
            &store,
            "work-1",
            "attempt-1",
            &candidate,
            &targeted_path,
            &evidence_digest,
            "Correct the source.",
        )
        .unwrap();
        let after_first = store.read_work_item("work-1").unwrap();
        let second = crate::host_evidence::classify_for_code_change(
            root,
            &store,
            "work-1",
            "attempt-1",
            &candidate,
            &targeted_path,
            &evidence_digest,
            "Correct the source.",
        )
        .unwrap();
        let after_second = store.read_work_item("work-1").unwrap();
        assert_eq!(first, second);
        assert_eq!(after_first, after_second);
        assert_eq!(after_second.attempts[0].evidence_recoveries.len(), 1);
        assert!(after_second.attempts[0].pause_kind.is_none());
        assert!(after_second.attempts[0].pause_recovery.is_none());
        assert!(after_second.attempts[0].completed_at.is_none());
        assert_eq!(
            after_second.attempts[0]
                .tasks
                .iter()
                .filter(|task| task.kind == TaskKind::Write)
                .count(),
            2
        );

        // A later recovery owns the current frontier, so replaying the first
        // request must be rejected rather than directing work to stale
        // correction state.
        let mut later_target = after_second.attempts[0]
            .tasks
            .iter()
            .find(|task| task.id == "attempt-1-evidence-proof-tests")
            .cloned()
            .unwrap();
        let later_recovery_id = "host-evidence-proof-later".to_string();
        later_target.id = "attempt-1-evidence-proof-later-tests".to_string();
        later_target.status = TaskStatus::Complete;
        later_target.completed_at = Some(now_iso8601());
        later_target.artifact_area = Some(TaskArtifactArea {
            path: work_artifact_path("work-1", "attempt-1", &later_target.id),
        });
        later_target.evidence_review_context = Some(EvidenceReviewContext {
            recovery_id: later_recovery_id.clone(),
            candidate_commit: candidate.clone(),
            attachment: EvidenceAttachment {
                snapshot_path: evidence_path.clone(),
                digest: evidence_digest.clone(),
            },
            prior_review_artifact: after_second.attempts[0].evidence_recoveries[0].targets[0]
                .prior_review_artifact
                .clone(),
        });
        let later_path = format!(
            "{}/review.md",
            later_target.artifact_area.as_ref().unwrap().path
        );
        let later_content = "Verdict: fail\nDisposition: evidence-needed\nExternal-Condition: Restore the host again.\nAdditional-Proof: Attach the second proof.\n";
        fs::create_dir_all(root.join(&later_path).parent().unwrap()).unwrap();
        fs::write(root.join(&later_path), later_content).unwrap();
        let later_recovery = EvidenceRecovery {
            carried_finding: None,
            required_check: None,
            id: later_recovery_id.clone(),
            candidate_commit: candidate.clone(),
            attachment: EvidenceAttachment {
                snapshot_path: evidence_path.clone(),
                digest: evidence_digest.clone(),
            },
            targets: vec![EvidenceReviewTarget {
                role: "tests".to_string(),
                prior_review_artifact: after_second.attempts[0].evidence_recoveries[0].targets[0]
                    .prior_review_artifact
                    .clone(),
                review_task_id: Some(later_target.id.clone()),
                review_digest: Some(format!(
                    "sha256:{:x}",
                    sha2::Sha256::digest(later_content.as_bytes())
                )),
            }],
            state: EvidenceRecoveryState::NeedsEvidence,
            created_at: now_iso8601(),
            code_change_classification: None,
        };
        store
            .mutate_work_item("work-1", |item| {
                let attempt = &mut item.attempts[0];
                attempt.tasks.push(later_target);
                attempt.evidence_recoveries.push(later_recovery);
                attempt.status = AttemptStatus::NeedsUser;
                attempt.pause_kind = Some(PauseKind::Uncertain);
                attempt.review_state = Some(AttemptReviewState::Failed);
                attempt.completed_at = Some(now_iso8601());
                Ok(())
            })
            .unwrap();
        let replay_error = crate::host_evidence::classify_for_code_change(
            root,
            &store,
            "work-1",
            "attempt-1",
            &candidate,
            &targeted_path,
            &evidence_digest,
            "Correct the source.",
        )
        .unwrap_err();
        assert!(
            replay_error
                .to_string()
                .contains("later active or NeedsEvidence recovery")
        );
        let after_distinct_replay = store.read_work_item("work-1").unwrap();
        assert_eq!(
            after_distinct_replay.attempts[0].evidence_recoveries.len(),
            2
        );
        assert_eq!(
            after_distinct_replay.attempts[0].evidence_recoveries[0].code_change_classification,
            after_second.attempts[0].evidence_recoveries[0].code_change_classification
        );
        assert!(
            after_distinct_replay.attempts[0].evidence_recoveries[1]
                .code_change_classification
                .is_none()
        );
    }

    #[test]
    fn completed_host_evidence_attachment_can_support_a_distinct_frontier() {
        let attachment = EvidenceAttachment {
            snapshot_path: ".fluent/work/artifacts/work/attempt-1/host-evidence/123.json"
                .to_string(),
            digest: "sha256:123".to_string(),
        };
        let mut attempt = Attempt {
            id: "attempt-1".to_string(),
            ..Attempt::default()
        };
        let prior = EvidenceRecovery {
            carried_finding: None,
            required_check: None,
            id: "host-evidence-123".to_string(),
            candidate_commit: "abc123".to_string(),
            attachment: attachment.clone(),
            targets: vec![EvidenceReviewTarget {
                role: "tests".to_string(),
                prior_review_artifact: "first-review.md".to_string(),
                review_task_id: Some("first-review".to_string()),
                review_digest: None,
            }],
            state: EvidenceRecoveryState::Complete,
            created_at: "2026-08-23T10:00:00Z".to_string(),
            code_change_classification: None,
        };
        attempt.attach_evidence_recovery(prior).unwrap();
        let later = EvidenceRecovery {
            carried_finding: None,
            required_check: None,
            id: "host-evidence-123-recheck".to_string(),
            candidate_commit: "abc123".to_string(),
            attachment,
            targets: vec![EvidenceReviewTarget {
                role: "tests".to_string(),
                prior_review_artifact: "failed-recheck-binding.json".to_string(),
                review_task_id: None,
                review_digest: None,
            }],
            state: EvidenceRecoveryState::Reviewing,
            created_at: "2026-08-23T11:00:00Z".to_string(),
            code_change_classification: None,
        };

        assert_eq!(
            attempt.attach_evidence_recovery(later.clone()).unwrap(),
            later
        );
        assert_eq!(attempt.evidence_recoveries.len(), 2);
    }

    #[test]
    fn attached_host_evidence_reviews_are_queue_resumable() {
        let attachment = EvidenceAttachment {
            snapshot_path: ".fluent/work/artifacts/work-1/attempt-1/host-evidence/proof.json"
                .to_string(),
            digest: "sha256:proof".to_string(),
        };
        let recovery = EvidenceRecovery {
            carried_finding: None,
            required_check: None,
            id: "host-evidence-proof".to_string(),
            candidate_commit: "candidate".to_string(),
            attachment: attachment.clone(),
            targets: vec![EvidenceReviewTarget {
                role: "tests".to_string(),
                prior_review_artifact:
                    ".fluent/work/artifacts/work-1/attempt-1/host-evidence/binding.json".to_string(),
                review_task_id: Some("attempt-1-evidence-proof-tests".to_string()),
                review_digest: None,
            }],
            state: EvidenceRecoveryState::Reviewing,
            created_at: "2026-08-22T20:00:00Z".to_string(),
            code_change_classification: None,
        };
        let mut review = task(TaskKind::Review, Vec::new());
        review.id = "attempt-1-evidence-proof-tests".to_string();
        review.role = "tests".to_string();
        review.evidence_review_context = Some(EvidenceReviewContext {
            recovery_id: recovery.id.clone(),
            candidate_commit: recovery.candidate_commit.clone(),
            attachment,
            prior_review_artifact: recovery.targets[0].prior_review_artifact.clone(),
        });
        let mut attempt = Attempt {
            id: "attempt-1".to_string(),
            work_item_id: "work-1".to_string(),
            status: AttemptStatus::Reviewing,
            review_state: Some(AttemptReviewState::NotReviewed),
            tasks: vec![review],
            evidence_recoveries: vec![recovery],
            ..Attempt::default()
        };

        assert!(attempt.has_pending_evidence_review_tasks());
        assert!(attempt.ensure_resume_ready().is_ok());

        attempt.tasks[0].status = TaskStatus::Complete;
        assert!(attempt.has_pending_evidence_review_tasks());
        assert!(attempt.ensure_resume_ready().is_ok());

        attempt.tasks[0].status = TaskStatus::Executing;
        assert!(!attempt.has_pending_evidence_review_tasks());
        assert!(attempt.ensure_resume_ready().is_err());

        attempt.tasks[0].status = TaskStatus::Complete;

        attempt.tasks.push(task(TaskKind::Write, Vec::new()));
        assert!(!attempt.has_pending_evidence_review_tasks());
        assert!(attempt.ensure_resume_ready().is_err());
    }

    #[test]
    fn failed_no_output_writer_with_evidence_only_inputs_is_recoverable() {
        let mut attempt = Attempt {
            id: "attempt-1".to_string(),
            status: AttemptStatus::Failed,
            ..Attempt::default()
        };
        let mut completed = task(TaskKind::Write, Vec::new());
        completed.id = "attempt-1-write-1".to_string();
        completed.status = TaskStatus::Complete;
        completed.output = Some(TaskOutput {
            workspace_id: "candidate".to_string(),
            workspace_path: "/workspace".to_string(),
            source_branch: "main".to_string(),
            base_commit: None,
            commit: "candidate-commit".to_string(),
            no_change: None,
            learner_canonicalization: None,
        });
        let mut failed = task(TaskKind::Write, Vec::new());
        failed.id = "attempt-1-write-2".to_string();
        failed.status = TaskStatus::Failed;
        failed.input_artifacts = vec![ArtifactRef {
            producer_id: "attempt-1-review-1".to_string(),
            path: ".fluent/work/artifacts/work-1/attempt-1/review/review.md".to_string(),
        }];
        attempt.tasks = vec![completed, failed];

        assert!(attempt.is_evidence_only_recoverable(
            "candidate-commit",
            &[".fluent/work/artifacts/work-1/attempt-1/review/review.md".to_string()],
        ));
    }

    #[test]
    fn evidence_needed_recovery_accepts_new_host_evidence() {
        let mut attempt = Attempt {
            id: "attempt-1".to_string(),
            ..Attempt::default()
        };
        let mut prior = EvidenceRecovery {
            carried_finding: None,
            required_check: None,
            id: "host-evidence-prior".to_string(),
            candidate_commit: "abc123".to_string(),
            attachment: EvidenceAttachment {
                snapshot_path: ".fluent/work/artifacts/work/attempt-1/host-evidence/prior.json"
                    .to_string(),
                digest: "sha256:prior".to_string(),
            },
            targets: Vec::new(),
            state: EvidenceRecoveryState::Reviewing,
            created_at: "2026-08-03T17:59:47Z".to_string(),
            code_change_classification: None,
        };
        attempt.attach_evidence_recovery(prior.clone()).unwrap();
        prior.state = EvidenceRecoveryState::NeedsEvidence;
        attempt.evidence_recoveries[0] = prior;

        let next = EvidenceRecovery {
            carried_finding: None,
            required_check: None,
            id: "host-evidence-next".to_string(),
            candidate_commit: "abc123".to_string(),
            attachment: EvidenceAttachment {
                snapshot_path: ".fluent/work/artifacts/work/attempt-1/host-evidence/next.json"
                    .to_string(),
                digest: "sha256:next".to_string(),
            },
            targets: Vec::new(),
            state: EvidenceRecoveryState::Reviewing,
            created_at: "2026-08-03T18:00:47Z".to_string(),
            code_change_classification: None,
        };

        assert_eq!(
            attempt.attach_evidence_recovery(next.clone()).unwrap(),
            next
        );
        assert_eq!(attempt.evidence_recoveries.len(), 2);
    }

    fn review_only_work_item() -> WorkItem {
        let mut work_item = WorkItem {
            id: "work-1".to_string(),
            title: "Review the codebase".to_string(),
            planning_context: None,
            instructions: None,
            abandonment: None,
            post_merge_review_fix_depth: None,
            attempts: Vec::new(),
            merge_candidates: Vec::new(),
            ..Default::default()
        };
        work_item
            .add_review_only_attempt("attempt-review", &["tests"], "main", "abc123", true)
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
            post_merge_review_fix_depth: None,
            attempts: Vec::new(),
            merge_candidates: Vec::new(),
            ..Default::default()
        };
        work_item.add_initial_attempt("attempt-1").unwrap();
        work_item.attempts[0].status = AttemptStatus::NeedsUser;
        work_item.attempts[0].tasks[0].status = TaskStatus::NeedsUser;

        work_item
            .abandon(Some("replacement landed".to_string()), None)
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
            post_merge_review_fix_depth: None,
            attempts: Vec::new(),
            merge_candidates: Vec::new(),
            ..Default::default()
        };
        work_item.add_initial_attempt("attempt-1").unwrap();
        work_item.attempts[0].status = AttemptStatus::Executing;
        work_item.attempts[0].tasks[0].status = TaskStatus::Executing;

        let error = work_item
            .abandon(Some("stale".to_string()), None)
            .unwrap_err();

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
            post_merge_review_fix_depth: None,
            attempts: Vec::new(),
            merge_candidates: Vec::new(),
            ..Default::default()
        };
        work_item.add_initial_attempt("attempt-1").unwrap();
        work_item.attempts[0].status = AttemptStatus::Reviewing;

        let error = work_item
            .abandon(Some("stale".to_string()), None)
            .unwrap_err();

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
            post_merge_review_fix_depth: None,
            attempts: Vec::new(),
            merge_candidates: Vec::new(),
            ..Default::default()
        };
        work_item.add_initial_attempt("attempt-1").unwrap();
        work_item.attempts[0].status = AttemptStatus::Failed;
        work_item.attempts[0].tasks[0].status = TaskStatus::Executing;

        let error = work_item
            .abandon(Some("stale".to_string()), None)
            .unwrap_err();

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
            post_merge_review_fix_depth: None,
            attempts: Vec::new(),
            merge_candidates: vec![MergeCandidate {
                id: "candidate-1".to_string(),
                attempt_id: "attempt-1".to_string(),
                source_workspace: workspace("candidate"),
                target_workspace: workspace("target"),
                source_branch: "main".to_string(),
                target_branch: "main".to_string(),
                candidate_commit: "abc123".to_string(),
                merge_review_state: MergeReviewState::Reviewing,
                merge_state: MergeCandidateMergeState::default(),
                created_at: None,
                ready_at: None,
                started_at: None,
                completed_at: None,
            }],
            ..Default::default()
        };

        let error = work_item
            .abandon(Some("stale".to_string()), None)
            .unwrap_err();

        assert!(matches!(
            error,
            WorkModelError::WorkItemAbandonmentBlocked { .. }
        ));
        assert!(work_item.abandonment.is_none());

        work_item.merge_candidates[0].merge_review_state = MergeReviewState::Pending;
        work_item.merge_candidates[0].merge_state.status = MergeCandidateMergeStatus::Executing;

        let error = work_item
            .abandon(Some("stale".to_string()), None)
            .unwrap_err();

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
            post_merge_review_fix_depth: None,
            attempts: Vec::new(),
            merge_candidates: Vec::new(),
            ..Default::default()
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
            post_merge_review_fix_depth: None,
            attempts: Vec::new(),
            merge_candidates: Vec::new(),
            ..Default::default()
        };

        let error = work_item
            .add_review_only_attempt("attempt-review", &["tests"], "main", "abc123", true)
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
                            && ref_.path == ".fluent/work/progress/work-1/attempt-1/progress.md"
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
        assert!("behavior-tests".parse::<TaskKind>().is_err());
        assert!("triage".parse::<TaskKind>().is_err());
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
                path: ".fluent/work/artifacts/work-1/attempt-1/attempt-1-review-behaviors"
                    .to_string(),
            }),
            review_context: Some(ReviewContext {
                candidate_workspace_id: "candidate".to_string(),
                candidate_workspace_path: "/workspaces/candidate".to_string(),
                source_branch: "main".to_string(),
                candidate_commit: "abc123".to_string(),
                base_commit: None,
            }),
            writer_correction_assignment: None,
            verification_context: None,
            evidence_review_context: None,
            input_artifacts: Vec::new(),
            depends_on: Some("attempt-1-review-tests".to_string()),
            output: None,
            created_at: None,
            started_at: None,
            completed_at: None,
        };

        let json = serde_json::to_string_pretty(&task).unwrap();
        assert!(json.contains(r#""depends_on": "attempt-1-review-tests""#));
        let parsed: Task = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.depends_on.as_deref(), Some("attempt-1-review-tests"));
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
            writer_correction_assignment: None,
            verification_context: None,
            evidence_review_context: None,
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
    fn code_review_tasks_do_not_plan_a_tester_before_review() {
        let mut work_item = work_item_with_completed_write("work-1");
        work_item
            .add_next_review_tasks("attempt-1", &["documentation", "behaviors", "tests"])
            .unwrap();

        let tasks = &work_item.attempts[0].tasks;
        assert!(
            tasks.iter().all(|task| task.kind != TaskKind::Tester),
            "code-producing review planning must not run the canonical suite before review"
        );
        assert!(
            tasks
                .iter()
                .filter(|task| task.kind == TaskKind::Review)
                .all(|task| task.depends_on.is_none()),
            "reviewers must be ready immediately after the Writer"
        );
    }

    #[test]
    fn code_review_tasks_do_not_receive_tester_results() {
        let mut work_item = work_item_with_completed_write("work-1");
        work_item
            .add_next_review_tasks("attempt-1", &["documentation", "tests"])
            .unwrap();

        let tasks = &work_item.attempts[0].tasks;
        for task in tasks.iter().filter(|t| t.kind == TaskKind::Review) {
            assert!(
                task.input_artifacts
                    .iter()
                    .all(|artifact| !artifact.path.ends_with("/tester-results.json")),
                "reviewer task {} must not claim final Tester evidence exists yet",
                task.role
            );
        }
    }

    #[test]
    fn tests_reviewer_receives_latest_tester_correction_evidence() {
        let mut item = work_item_with_completed_write("work-1");
        let tester_id = item.add_final_tester_task("attempt-1").unwrap();
        let tester = item.attempts[0]
            .tasks
            .iter_mut()
            .find(|task| task.id == tester_id)
            .unwrap();
        set_task_terminal(tester, TaskStatus::Complete);
        let tester_area = tester.artifact_area.as_ref().unwrap().path.clone();
        let evidence = vec![
            ArtifactRef {
                producer_id: tester_id.clone(),
                path: format!("{tester_area}/tester-results.json"),
            },
            ArtifactRef {
                producer_id: tester_id.clone(),
                path: format!("{tester_area}/commands/0-stdout.log"),
            },
            ArtifactRef {
                producer_id: tester_id.clone(),
                path: format!("{tester_area}/commands/0-stderr.log"),
            },
        ];
        let writer_id = item
            .add_next_write_round("attempt-1", evidence.clone())
            .unwrap();
        let writer = item.attempts[0]
            .tasks
            .iter_mut()
            .find(|task| task.id == writer_id)
            .unwrap();
        set_task_terminal(writer, TaskStatus::Complete);
        writer.output = Some(TaskOutput {
            workspace_id: "candidate".to_string(),
            workspace_path: "../candidate".to_string(),
            source_branch: "main".to_string(),
            base_commit: None,
            commit: "corrected".to_string(),
            no_change: None,
            learner_canonicalization: None,
        });

        item.add_next_review_tasks("attempt-1", &["documentation", "tests"])
            .unwrap();
        let tests = item.attempts[0]
            .tasks
            .iter()
            .rev()
            .find(|task| task.kind == TaskKind::Review && task.role == "tests")
            .unwrap();
        let documentation = item.attempts[0]
            .tasks
            .iter()
            .rev()
            .find(|task| task.kind == TaskKind::Review && task.role == "documentation")
            .unwrap();
        assert!(
            evidence
                .iter()
                .all(|artifact| tests.input_artifacts.contains(artifact))
        );
        assert!(
            evidence
                .iter()
                .all(|artifact| !documentation.input_artifacts.contains(artifact))
        );
        let tests_review_id = tests.id.clone();

        for review in item.attempts[0]
            .tasks
            .iter_mut()
            .filter(|task| task.kind == TaskKind::Review && task.status == TaskStatus::Planned)
        {
            set_task_terminal(review, TaskStatus::Complete);
        }
        let later_writer_id = item
            .add_next_write_round(
                "attempt-1",
                vec![ArtifactRef {
                    producer_id: tests_review_id.clone(),
                    path: format!(
                        "{}/review.md",
                        work_artifact_path("work-1", "attempt-1", &tests_review_id)
                    ),
                }],
            )
            .unwrap();
        let later_writer = item.attempts[0]
            .tasks
            .iter_mut()
            .find(|task| task.id == later_writer_id)
            .unwrap();
        set_task_terminal(later_writer, TaskStatus::Complete);
        later_writer.output = Some(TaskOutput {
            workspace_id: "candidate".to_string(),
            workspace_path: "../candidate".to_string(),
            source_branch: "main".to_string(),
            base_commit: None,
            commit: "later-correction".to_string(),
            no_change: None,
            learner_canonicalization: None,
        });
        item.add_next_review_tasks("attempt-1", &["tests"]).unwrap();
        let later_tests = item.attempts[0]
            .tasks
            .iter()
            .rev()
            .find(|task| task.kind == TaskKind::Review && task.role == "tests")
            .unwrap();
        assert!(
            evidence
                .iter()
                .all(|artifact| !later_tests.input_artifacts.contains(artifact))
        );
    }

    #[test]
    fn passing_recheck_supersedes_failed_tester_review_evidence() {
        let mut item = work_item_with_completed_write("work-1");
        let tester_id = item.add_final_tester_task("attempt-1").unwrap();
        let tester = item.attempts[0]
            .tasks
            .iter_mut()
            .find(|task| task.id == tester_id)
            .unwrap();
        set_task_terminal(tester, TaskStatus::Complete);
        let tester_area = tester.artifact_area.as_ref().unwrap().path.clone();
        let source_results = ArtifactRef {
            producer_id: tester_id.clone(),
            path: format!("{tester_area}/tester-results.json"),
        };
        let failed_log = ArtifactRef {
            producer_id: tester_id,
            path: format!("{tester_area}/commands/0-stderr.log"),
        };
        let writer_id = item
            .add_next_write_round(
                "attempt-1",
                vec![source_results.clone(), failed_log.clone()],
            )
            .unwrap();
        let writer = item.attempts[0]
            .tasks
            .iter_mut()
            .find(|task| task.id == writer_id)
            .unwrap();
        set_task_terminal(writer, TaskStatus::Complete);
        writer.output = Some(TaskOutput {
            workspace_id: "candidate".to_string(),
            workspace_path: "../candidate".to_string(),
            source_branch: "main".to_string(),
            base_commit: None,
            commit: "commit-initial".to_string(),
            no_change: None,
            learner_canonicalization: None,
        });
        let recheck_id = item
            .add_failed_command_recheck_task("attempt-1", source_results.clone())
            .unwrap();
        let recheck = item.attempts[0]
            .tasks
            .iter_mut()
            .find(|task| task.id == recheck_id)
            .unwrap();
        set_task_terminal(recheck, TaskStatus::Complete);
        let recheck_results = ArtifactRef {
            producer_id: recheck_id.clone(),
            path: format!(
                "{}/tester-results.json",
                recheck.artifact_area.as_ref().unwrap().path
            ),
        };

        item.add_next_review_tasks("attempt-1", &["tests"]).unwrap();
        let tests = item.attempts[0]
            .tasks
            .iter()
            .rev()
            .find(|task| task.kind == TaskKind::Review && task.role == "tests")
            .unwrap();
        assert!(tests.input_artifacts.contains(&recheck_results));
        assert!(!tests.input_artifacts.contains(&source_results));
        assert!(!tests.input_artifacts.contains(&failed_log));
    }

    #[test]
    fn later_code_review_round_also_omits_a_tester() {
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
            base_commit: None,
            commit: "commit-second".to_string(),
            no_change: None,
            learner_canonicalization: None,
        });
        work_item.attempts[0].status = AttemptStatus::Complete;
        work_item
            .add_next_review_tasks("attempt-1", &["tests"])
            .unwrap();

        let tasks = &work_item.attempts[0].tasks;
        assert!(
            tasks.iter().all(|task| task.kind != TaskKind::Tester),
            "corrective review rounds must not duplicate the canonical suite"
        );
    }

    #[test]
    fn each_code_reviewer_receives_writer_transcript_in_input_artifacts() {
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
            let has_writer_transcript = task.input_artifacts.iter().any(|artifact| {
                artifact.producer_id == "attempt-1-write-1"
                    && artifact
                        .path
                        .ends_with("/attempt-1-write-1/transcript.jsonl")
            });
            assert!(
                has_writer_transcript,
                "reviewer task {} should receive the Writer's concise verification summary",
                task.role,
            );
        }
    }

    #[test]
    fn final_tester_is_bound_to_the_reviewed_candidate_after_review() {
        let mut work_item = work_item_with_completed_write("work-1");
        work_item
            .add_next_review_tasks("attempt-1", &["tests"])
            .unwrap();
        for task in work_item.attempts[0]
            .tasks
            .iter_mut()
            .filter(|task| task.kind == TaskKind::Review)
        {
            crate::work_model::set_task_terminal(task, TaskStatus::Complete);
        }

        let tester_id = work_item.add_final_tester_task("attempt-1").unwrap();
        let tester = work_item.attempts[0]
            .tasks
            .iter()
            .find(|task| task.id == tester_id)
            .unwrap();
        assert_eq!(tester.id, "attempt-1-tester");
        assert_eq!(tester.kind, TaskKind::Tester);
        assert_eq!(tester.status, TaskStatus::Planned);
        assert_eq!(
            tester
                .review_context
                .as_ref()
                .map(|context| context.candidate_commit.as_str()),
            Some("commit-initial")
        );
        assert!(tester.depends_on.is_none());
    }

    #[test]
    fn failed_command_recheck_is_serial_and_bound_to_source_evidence() {
        let mut work_item = work_item_with_completed_write("work-1");
        work_item
            .add_next_review_tasks("attempt-1", &["tests"])
            .unwrap();
        for task in work_item.attempts[0]
            .tasks
            .iter_mut()
            .filter(|task| task.kind == TaskKind::Review)
        {
            set_task_terminal(task, TaskStatus::Complete);
        }
        let source_id = work_item.add_final_tester_task("attempt-1").unwrap();
        let source = work_item.attempts[0]
            .tasks
            .iter_mut()
            .find(|task| task.id == source_id)
            .unwrap();
        set_task_terminal(source, TaskStatus::Complete);
        let source_results = ArtifactRef {
            producer_id: source_id.clone(),
            path: format!(
                "{}/tester-results.json",
                work_artifact_path("work-1", "attempt-1", &source_id)
            ),
        };

        let recheck_id = work_item
            .add_failed_command_recheck_task("attempt-1", source_results.clone())
            .unwrap();

        let recheck = work_item.attempts[0]
            .tasks
            .iter()
            .find(|task| task.id == recheck_id)
            .unwrap();
        assert_eq!(recheck.role, FAILED_COMMAND_RECHECK_ROLE);
        assert_eq!(recheck.depends_on.as_deref(), Some(source_id.as_str()));
        assert_eq!(recheck.input_artifacts, vec![source_results]);
        assert_eq!(
            recheck
                .review_context
                .as_ref()
                .map(|context| context.candidate_commit.as_str()),
            Some("commit-initial")
        );
        assert_eq!(recheck.status, TaskStatus::Planned);

        let mut malformed = work_item.clone();
        let recheck = malformed.attempts[0]
            .tasks
            .iter_mut()
            .find(|task| task.id == recheck_id)
            .unwrap();
        recheck.input_artifacts[0].path = "../tester-results.json".to_string();
        assert!(matches!(
            malformed.validate(),
            Err(WorkModelError::InvalidFailedCommandRecheckTask { .. })
        ));
    }

    #[test]
    fn no_tester_task_when_writer_failed() {
        let mut work_item = WorkItem {
            id: "work-1".to_string(),
            title: "Test failed writer".to_string(),
            planning_context: None,
            instructions: None,
            abandonment: None,
            post_merge_review_fix_depth: None,
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
                    writer_correction_assignment: None,
                    verification_context: None,
                    evidence_review_context: None,
                    input_artifacts: Vec::new(),
                    depends_on: None,
                    output: None,
                    created_at: None,
                    started_at: None,
                    completed_at: None,
                }],
                review_state: None,
                pause_kind: None,
                artifacts: Vec::new(),
                created_at: None,
                completed_at: None,
                ..Default::default()
            }],
            merge_candidates: Vec::new(),
            ..Default::default()
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
            post_merge_review_fix_depth: None,
            attempts: Vec::new(),
            merge_candidates: Vec::new(),
            ..Default::default()
        };
        work_item.add_initial_attempt("attempt-1").unwrap();

        let write_task = &work_item.attempts[0].tasks[0];
        assert_eq!(write_task.kind, TaskKind::Write);
        assert_eq!(
            write_task.artifact_area.as_ref().unwrap().path,
            ".fluent/work/artifacts/work-1/attempt-1/attempt-1-write-1"
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
                        ".fluent/work/artifacts/work-1/attempt-1/attempt-1-review-tests/review.md"
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
            ".fluent/work/artifacts/work-1/attempt-1/attempt-1-write-2"
        );
    }

    fn preserved_input() -> WorkItemInputArtifact {
        WorkItemInputArtifact {
            source_path: ".fluent/work/artifacts/source/evidence.jsonl".to_string(),
            snapshot_path: ".fluent/work/artifacts/work-1/inputs/0000-evidence.jsonl".to_string(),
            digest: "sha256:approved".to_string(),
        }
    }

    #[test]
    fn every_write_task_copies_and_deduplicates_preserved_inputs() {
        let mut work_item = WorkItem {
            input_artifacts: vec![preserved_input()],
            ..WorkItem::planned("work-1", "Preserved input")
        };
        work_item.add_initial_attempt("attempt-1").unwrap();
        let initial = &mut work_item.attempts[0].tasks[0];
        initial.status = TaskStatus::Complete;
        initial.output = Some(TaskOutput {
            workspace_id: "candidate".to_string(),
            workspace_path: "../candidate".to_string(),
            source_branch: "main".to_string(),
            base_commit: None,
            commit: "abc123".to_string(),
            no_change: None,
            learner_canonicalization: None,
        });
        let preserved_ref = work_item.preserved_input_refs()[0].clone();
        work_item
            .add_next_write_round(
                "attempt-1",
                vec![
                    preserved_ref.clone(),
                    ArtifactRef {
                        producer_id: "attempt-1-review-tests".to_string(),
                        path: ".fluent/work/artifacts/work-1/attempt-1/review.md".to_string(),
                    },
                ],
            )
            .unwrap();

        for task in work_item.attempts[0]
            .tasks
            .iter()
            .filter(|task| task.kind == TaskKind::Write)
        {
            assert_eq!(
                task.input_artifacts
                    .iter()
                    .filter(|artifact| **artifact == preserved_ref)
                    .count(),
                1
            );
        }
    }

    #[test]
    fn reviewers_combine_preserved_writer_transcript_and_progress_inputs() {
        let mut work_item = work_item_with_completed_write("work-1");
        work_item.input_artifacts = vec![preserved_input()];
        work_item.add_review_tasks("attempt-1", &["tests"]).unwrap();

        let reviewer = work_item.attempts[0]
            .tasks
            .iter()
            .find(|task| task.kind == TaskKind::Review)
            .unwrap();
        assert_eq!(
            reviewer.input_artifacts[0],
            work_item.preserved_input_refs()[0]
        );
        assert!(
            reviewer
                .input_artifacts
                .iter()
                .any(|artifact| artifact.producer_id == "attempt-1-write-1"
                    && artifact.path.ends_with("/transcript.jsonl"))
        );
        assert!(
            reviewer
                .input_artifacts
                .iter()
                .any(|artifact| artifact.producer_id == "writer")
        );
    }

    #[test]
    fn source_checkout_review_only_tasks_receive_preserved_inputs() {
        let mut work_item = WorkItem {
            input_artifacts: vec![preserved_input()],
            ..WorkItem::planned("work-1", "Review source checkout")
        };
        work_item
            .add_review_only_attempt(
                "attempt-review",
                &["architecture", "tests"],
                "main",
                "abc123",
                true,
            )
            .unwrap();
        let preserved = work_item.preserved_input_refs();

        for reviewer in &work_item.attempts[0].tasks {
            assert_eq!(reviewer.kind, TaskKind::Review);
            assert_eq!(reviewer.input_artifacts, preserved);
        }
    }

    #[test]
    fn worktree_and_post_merge_reviewers_combine_preserved_and_tester_inputs() {
        for post_merge in [false, true] {
            let mut work_item = WorkItem {
                input_artifacts: vec![preserved_input()],
                ..WorkItem::planned("work-1", "Review managed worktree")
            };
            if post_merge {
                work_item
                    .add_post_merge_review_attempt(
                        "attempt-review",
                        &["tests"],
                        "main",
                        "abc123",
                        Some("base123".to_string()),
                    )
                    .unwrap();
            } else {
                work_item
                    .add_review_only_attempt("attempt-review", &["tests"], "main", "abc123", false)
                    .unwrap();
            }

            let attempt = &work_item.attempts[0];
            let tester = attempt
                .tasks
                .iter()
                .find(|task| task.kind == TaskKind::Tester)
                .unwrap();
            let reviewer = attempt
                .tasks
                .iter()
                .find(|task| task.kind == TaskKind::Review)
                .unwrap();
            assert!(tester.input_artifacts.is_empty());
            assert_eq!(
                reviewer.input_artifacts[0],
                work_item.preserved_input_refs()[0]
            );
            assert_eq!(reviewer.input_artifacts[1].producer_id, tester.id);
            assert!(
                reviewer.input_artifacts[1]
                    .path
                    .ends_with("/tester-results.json")
            );
        }
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
            base_commit: None,
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
            post_merge_review_fix_depth: None,
            attempts: Vec::new(),
            merge_candidates: Vec::new(),
            ..Default::default()
        };

        work_item
            .add_review_only_attempt("attempt-review", &["skills"], "main", "abc123", true)
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
            .path = ".fluent/work/artifacts/other-attempt/attempt-review-review-tests".to_string();

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
            post_merge_review_fix_depth: None,
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
                pause_kind: None,
                artifacts: Vec::new(),
                created_at: None,
                completed_at: None,
                ..Default::default()
            }],
            merge_candidates: Vec::new(),
            ..Default::default()
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
            ".fluent/work/artifacts/work-alpha/attempt-1/attempt-1-review-tests"
        );
        assert_eq!(
            second_review.artifact_area.as_ref().unwrap().path,
            ".fluent/work/artifacts/work-beta/attempt-1/attempt-1-review-tests"
        );
        assert_ne!(
            first_review.artifact_area.as_ref().unwrap().path,
            second_review.artifact_area.as_ref().unwrap().path,
        );
    }

    #[test]
    fn explicit_upgrade_owns_legacy_artifact_migration() {
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
                        ".fluent/work/artifacts/work-1/attempt-1/attempt-1-review-tests/review.md"
                            .to_string(),
                }],
            )
            .unwrap();
        work_item.attempts[0].artifacts.push(ArtifactRef {
            producer_id: "attempt-1-review-tests".to_string(),
            path: ".fluent/work/artifacts/work-1/attempt-1/attempt-1-review-tests/review.md"
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
            merge_review_state: MergeReviewState::Failed,
            merge_state: MergeCandidateMergeState {
                status: MergeCandidateMergeStatus::Failed,
                merged_commit: None,
                failure_reason: Some("Review failed".to_string()),
                check_artifacts: vec![ArtifactRef {
                    producer_id: "merge-check".to_string(),
                    path: ".fluent/work/artifacts/work-1/attempt-1/attempt-1-merge-candidate/merge/checks/checks.json"
                        .to_string(),
                }],
                review_artifacts: vec![ArtifactRef {
                    producer_id: "merge-review-tests".to_string(),
                    path: ".fluent/work/artifacts/work-1/attempt-1/attempt-1-merge-candidate/merge/reviews/tests/review.md"
                        .to_string(),
                }],
                auto_merge_skipped: None,
                follow_up_failure: None,
                landing_intent: None,
            },
            created_at: None,
            ready_at: None,
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
            ".fluent/work/artifacts/attempt-1/attempt-1-review-tests".to_string(),
        );
        fs::write(&task_path, to_json_pretty(&task_record).unwrap()).unwrap();

        let attempt_path = store.work_attempt_path("work-1", "attempt-1").unwrap();
        let mut attempt_record: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&attempt_path).unwrap()).unwrap();
        attempt_record["artifacts"][0]["path"] = serde_json::Value::String(
            ".fluent/work/artifacts/attempt-1/attempt-1-review-tests/review.md".to_string(),
        );
        fs::write(&attempt_path, to_json_pretty(&attempt_record).unwrap()).unwrap();

        let followup_path = store
            .work_task_path("work-1", "attempt-1", "attempt-1-write-2")
            .unwrap();
        let mut followup_record: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&followup_path).unwrap()).unwrap();
        followup_record["input_artifacts"][0]["path"] = serde_json::Value::String(
            ".fluent/work/artifacts/attempt-1/attempt-1-review-tests/review.md".to_string(),
        );
        fs::write(&followup_path, to_json_pretty(&followup_record).unwrap()).unwrap();

        let candidate_path = store
            .work_merge_candidate_path("work-1", "attempt-1-merge-candidate")
            .unwrap();
        let mut candidate_record: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&candidate_path).unwrap()).unwrap();
        candidate_record["merge_state"]["check_artifacts"][0]["path"] = serde_json::Value::String(
            ".fluent/work/artifacts/attempt-1/attempt-1-merge-candidate/merge/checks/checks.json"
                .to_string(),
        );
        candidate_record["merge_state"]["review_artifacts"][0]["path"] =
            serde_json::Value::String(
                ".fluent/work/artifacts/attempt-1/attempt-1-merge-candidate/merge/reviews/tests/review.md"
                    .to_string(),
            );
        fs::write(&candidate_path, to_json_pretty(&candidate_record).unwrap()).unwrap();

        let legacy_dir = tmp
            .path()
            .join(".fluent/work/artifacts/attempt-1/attempt-1-review-tests");
        fs::create_dir_all(&legacy_dir).unwrap();
        fs::write(legacy_dir.join("review.md"), "Verdict: pass\n").unwrap();

        let snapshot = store.read_work_item_snapshot("work-1").unwrap();
        assert!(
            snapshot.attempts[0].artifacts[0]
                .path
                .starts_with(".fluent/work/artifacts/attempt-1/")
        );
        assert!(legacy_dir.exists());
        let report = store.upgrade_work_item("work-1").unwrap();
        assert!(
            !report.already_current,
            "an artifact-only migration still needs an audited upgrade"
        );
        let audit: serde_json::Value =
            serde_json::from_slice(&fs::read(tmp.path().join(report.audit_path.unwrap())).unwrap())
                .unwrap();
        assert!(!audit["artifact-migrations"].as_array().unwrap().is_empty());
        assert!(store.upgrade_work_item("work-1").unwrap().already_current);
        let read = store.read_work_item("work-1").unwrap();

        let review_tests_task = read.attempts[0]
            .tasks
            .iter()
            .find(|t| t.id == "attempt-1-review-tests")
            .expect("review-tests task");
        assert_eq!(
            review_tests_task.artifact_area.as_ref().unwrap().path,
            ".fluent/work/artifacts/work-1/attempt-1/attempt-1-review-tests"
        );
        assert_eq!(
            read.attempts[0].artifacts[0].path,
            ".fluent/work/artifacts/work-1/attempt-1/attempt-1-review-tests/review.md"
        );
        let write_2_task = read.attempts[0]
            .tasks
            .iter()
            .find(|t| t.id.contains("write-2"))
            .expect("write-2 task");
        assert_eq!(
            write_2_task.input_artifacts[0].path,
            ".fluent/work/artifacts/work-1/attempt-1/attempt-1-review-tests/review.md"
        );
        assert_eq!(
            read.merge_candidates[0].merge_state.check_artifacts[0].path,
            ".fluent/work/artifacts/work-1/attempt-1/attempt-1-merge-candidate/merge/checks/checks.json"
        );
        assert_eq!(
            read.merge_candidates[0].merge_state.review_artifacts[0].path,
            ".fluent/work/artifacts/work-1/attempt-1/attempt-1-merge-candidate/merge/reviews/tests/review.md"
        );
        assert!(
            tmp.path()
                .join(".fluent/work/artifacts/work-1/attempt-1/attempt-1-review-tests/review.md")
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
            post_merge_review_fix_depth: None,
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
                                ".fluent/work/artifacts/work-1/attempt-1/attempt-1-review-documentation"
                                    .to_string(),
                        }),
                        review_context: Some(ReviewContext {
                            candidate_workspace_id: "candidate".to_string(),
                            candidate_workspace_path: "/workspaces/candidate".to_string(),
                            source_branch: "main".to_string(),
                            candidate_commit: "commit-initial".to_string(),
                            base_commit: None,
                        }),
                        writer_correction_assignment: None,
                        verification_context: None,
                        evidence_review_context: None,
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
                            path: ".fluent/work/artifacts/work-1/attempt-1/attempt-1-review-behaviors"
                                .to_string(),
                        }),
                        review_context: Some(ReviewContext {
                            candidate_workspace_id: "candidate".to_string(),
                            candidate_workspace_path: "/workspaces/candidate".to_string(),
                            source_branch: "main".to_string(),
                            candidate_commit: "commit-initial".to_string(),
                            base_commit: None,
                        }),
                        writer_correction_assignment: None,
                        verification_context: None,
                        evidence_review_context: None,
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
                pause_kind: None,
                artifacts: Vec::new(),
                created_at: None,
                completed_at: None,
                            ..Default::default()
            }],
            merge_candidates: Vec::new(),
            ..Default::default()
        };

        let task_ids = work_item
            .add_next_review_tasks("attempt-1", &["tests"])
            .unwrap();

        assert_eq!(task_ids, vec!["attempt-1-review-2-tests"]);
    }

    #[test]
    fn complete_attempt_can_run_a_native_learner_task() {
        let learner = Task {
            id: "attempt-1-learn-run-1".into(),
            kind: TaskKind::Learn,
            status: TaskStatus::Executing,
            role: "learner".into(),
            instructions: None,
            work_item_id: "work-1".into(),
            attempt_id: Some("attempt-1".into()),
            workspace_access: WorkspaceAccess {
                reads: Vec::new(),
                writes: Vec::new(),
            },
            artifact_area: Some(TaskArtifactArea {
                path: work_artifact_path("work-1", "attempt-1", "attempt-1-learn-run-1"),
            }),
            review_context: None,
            evidence_review_context: None,
            verification_context: None,
            writer_correction_assignment: None,
            input_artifacts: Vec::new(),
            depends_on: None,
            output: None,
            created_at: None,
            started_at: None,
            completed_at: None,
        };
        let attempt = Attempt {
            id: "attempt-1".into(),
            work_item_id: "work-1".into(),
            kind: AttemptKind::Write,
            status: AttemptStatus::Complete,
            tasks: vec![
                completed_write_task("attempt-1-write-1", "initial"),
                learner,
            ],
            review_state: Some(AttemptReviewState::Passed),
            ..Default::default()
        };
        assert!(attempt.validate("work-1").is_ok());
    }

    #[test]
    fn attempt_artifacts_round_trip_with_work_item() {
        let work_item = WorkItem {
            id: "work-1".to_string(),
            title: "Define the core work model".to_string(),
            planning_context: None,
            instructions: None,
            abandonment: None,
            post_merge_review_fix_depth: None,
            attempts: vec![Attempt {
                id: "attempt-1".to_string(),
                work_item_id: "work-1".to_string(),
                kind: AttemptKind::Write,
                status: AttemptStatus::Complete,
                coder_mapping: CoderMapping::default(),
                tasks: vec![task(TaskKind::Write, vec![workspace("candidate")])],
                review_state: Some(AttemptReviewState::Passed),
                pause_kind: None,
                artifacts: vec![ArtifactRef {
                    producer_id: "task-1".to_string(),
                    path: ".fluent/tasks/task-1/report.md".to_string(),
                }],
                created_at: None,
                completed_at: None,
                ..Default::default()
            }],
            merge_candidates: Vec::new(),
            ..Default::default()
        };

        let json = to_json_pretty(&work_item).unwrap();
        let decoded: WorkItem = from_json(&json).unwrap();

        assert_eq!(decoded, work_item);
        assert_eq!(
            decoded.attempts[0].artifacts[0].path,
            ".fluent/tasks/task-1/report.md"
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
            post_merge_review_fix_depth: None,
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
                pause_kind: None,
                artifacts: Vec::new(),
                created_at: None,
                completed_at: None,
                ..Default::default()
            }],
            merge_candidates: Vec::new(),
            ..Default::default()
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
        assert_eq!(candidate.merge_review_state, MergeReviewState::Pending);
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
            post_merge_review_fix_depth: None,
            attempts: vec![Attempt {
                id: "attempt-1".to_string(),
                work_item_id: "work-1".to_string(),
                kind: AttemptKind::Write,
                status: AttemptStatus::Complete,
                coder_mapping: CoderMapping::default(),
                tasks: vec![completed_write_task("attempt-1-write-1", "original")],
                review_state: Some(AttemptReviewState::Passed),
                pause_kind: None,
                artifacts: Vec::new(),
                created_at: None,
                completed_at: None,
                ..Default::default()
            }],
            merge_candidates: Vec::new(),
            ..Default::default()
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
    fn only_pending_unreviewed_merge_candidate_can_return_to_correction() {
        let mut work_item = WorkItem {
            id: "work-1".to_string(),
            title: "Return rejected evidence to the Writer".to_string(),
            attempts: vec![Attempt {
                id: "attempt-1".to_string(),
                work_item_id: "work-1".to_string(),
                kind: AttemptKind::Write,
                status: AttemptStatus::Complete,
                coder_mapping: CoderMapping::default(),
                tasks: vec![completed_write_task("attempt-1-write-1", "original")],
                review_state: Some(AttemptReviewState::Passed),
                learning: Some(AttemptLearning::failed(1, "missing Tester evidence")),
                ..Default::default()
            }],
            ..Default::default()
        };
        work_item
            .create_or_get_merge_candidate("attempt-1")
            .unwrap();

        let mut reviewed = work_item.clone();
        reviewed.merge_candidates[0].merge_review_state = MergeReviewState::Passed;
        assert!(matches!(
            reviewed.retract_unready_merge_candidate_for_correction("attempt-1"),
            Err(WorkModelError::MergeCandidateCorrectionIneligible { .. })
        ));
        let mut merged = work_item.clone();
        merged.merge_candidates[0].merge_state.status = MergeCandidateMergeStatus::Merged;
        assert!(matches!(
            merged.retract_unready_merge_candidate_for_correction("attempt-1"),
            Err(WorkModelError::MergeCandidateCorrectionIneligible { .. })
        ));

        assert!(
            work_item
                .retract_unready_merge_candidate_for_correction("attempt-1")
                .unwrap()
        );
        assert!(work_item.merge_candidates.is_empty());
    }

    #[test]
    fn learner_approved_merge_candidate_cannot_return_to_correction() {
        let mut work_item = WorkItem {
            id: "work-1".to_string(),
            title: "Preserve Learner approval".to_string(),
            attempts: vec![Attempt {
                id: "attempt-1".to_string(),
                work_item_id: "work-1".to_string(),
                kind: AttemptKind::Write,
                status: AttemptStatus::Complete,
                coder_mapping: CoderMapping::default(),
                tasks: vec![completed_write_task("attempt-1-write-1", "original")],
                review_state: Some(AttemptReviewState::Passed),
                learning: Some(AttemptLearning::succeeded(
                    1,
                    crate::follow_up::ArtifactRef {
                        path: "handoff.json".to_string(),
                        digest: "sha256:x".to_string(),
                    },
                )),
                ..Default::default()
            }],
            ..Default::default()
        };
        work_item
            .create_or_get_merge_candidate("attempt-1")
            .unwrap();

        let error = work_item
            .retract_unready_merge_candidate_for_correction("attempt-1")
            .expect_err("Learner approval makes the candidate immutable");
        assert!(matches!(
            error,
            WorkModelError::MergeCandidateCorrectionIneligible { .. }
        ));
        assert_eq!(work_item.merge_candidates.len(), 1);
    }

    #[test]
    fn merge_candidate_validation_rejects_non_succeeded_learning() {
        // Advancement gates B1: Merge Candidate validation (the landing gate) rejects
        // every non-succeeded Learner — absent, in-progress, prepared/HandoffPending,
        // or failed whether relaunchable or not — with one durable reason, and admits
        // only a SUCCEEDED Learner. An already-merged candidate is exempt so
        // idempotent post-land work still resumes.
        let mut work_item = WorkItem {
            id: "work-1".to_string(),
            title: "Advancement gate".to_string(),
            attempts: vec![Attempt {
                id: "attempt-1".to_string(),
                work_item_id: "work-1".to_string(),
                kind: AttemptKind::Write,
                status: AttemptStatus::Complete,
                coder_mapping: CoderMapping::default(),
                tasks: vec![completed_write_task("attempt-1-write-1", "original")],
                review_state: Some(AttemptReviewState::Passed),
                ..Default::default()
            }],
            merge_candidates: Vec::new(),
            ..Default::default()
        };
        work_item
            .create_or_get_merge_candidate("attempt-1")
            .unwrap();
        let handoff = crate::follow_up::ArtifactRef {
            path: "handoff.json".to_string(),
            digest: "sha256:x".to_string(),
        };

        // Every non-succeeded state blocks with the shared advancement reason.
        for learning in [
            None,
            Some(AttemptLearning::in_progress(1)),
            Some(AttemptLearning::handoff_pending(1)),
            Some(AttemptLearning::failed(1, "boom")),
            Some(AttemptLearning::failed_with_kind(
                1,
                "evidence pending",
                LearningFailureKind::EvidencePending,
            )),
        ] {
            work_item.attempts[0].learning = learning.clone();
            let candidate = work_item.merge_candidates[0].clone();
            let error = candidate
                .validate_advancement(&work_item)
                .expect_err("a non-succeeded Learner must block the landing gate");
            assert!(
                matches!(error, WorkModelError::AttemptLearningNotSucceeded { .. }),
                "the block is the shared advancement reason for {learning:?}: {error}"
            );
            assert!(
                error.to_string().contains("not succeeded"),
                "the durable reason names the non-succeeded Learner: {error}"
            );
        }

        // A succeeded Learner admits landing.
        work_item.attempts[0].learning = Some(AttemptLearning::succeeded(1, handoff));
        let candidate = work_item.merge_candidates[0].clone();
        candidate
            .validate_advancement(&work_item)
            .expect("a succeeded Learner passes the landing gate");

        // An already-merged candidate is exempt even with a non-succeeded Learner, so
        // idempotent post-land processing still resumes.
        work_item.attempts[0].learning = Some(AttemptLearning::failed(1, "boom"));
        let mut merged = work_item.merge_candidates[0].clone();
        merged.merge_state.status = MergeCandidateMergeStatus::Merged;
        merged
            .validate_advancement(&work_item)
            .expect("a merged candidate is exempt from the learner gate");
    }

    #[test]
    fn merge_candidate_validation_rejects_duplicate_attempt_candidate() {
        let mut work_item = WorkItem {
            id: "work-1".to_string(),
            title: "Keep one merge candidate per attempt".to_string(),
            planning_context: None,
            instructions: None,
            abandonment: None,
            post_merge_review_fix_depth: None,
            attempts: vec![Attempt {
                id: "attempt-1".to_string(),
                work_item_id: "work-1".to_string(),
                kind: AttemptKind::Write,
                status: AttemptStatus::Complete,
                coder_mapping: CoderMapping::default(),
                tasks: vec![completed_write_task("attempt-1-write-1", "original")],
                review_state: Some(AttemptReviewState::Passed),
                pause_kind: None,
                artifacts: Vec::new(),
                created_at: None,
                completed_at: None,
                ..Default::default()
            }],
            merge_candidates: Vec::new(),
            ..Default::default()
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
            post_merge_review_fix_depth: None,
            attempts: vec![Attempt {
                id: "attempt-1".to_string(),
                work_item_id: "work-1".to_string(),
                kind: AttemptKind::Write,
                status: AttemptStatus::Reviewing,
                coder_mapping: CoderMapping::default(),
                tasks: vec![completed_write_task("attempt-1-write-1", "original")],
                review_state: Some(AttemptReviewState::Uncertain),
                pause_kind: None,
                artifacts: Vec::new(),
                created_at: None,
                completed_at: None,
                ..Default::default()
            }],
            merge_candidates: Vec::new(),
            ..Default::default()
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
            merge_review_state: MergeReviewState::Pending,
            merge_state: MergeCandidateMergeState::default(),
            created_at: None,
            ready_at: None,
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

    /// Builds a reviewed Write Attempt with its pre-created Merge Candidate, then
    /// suspends the Attempt to the typed host-sandbox pause — the exact state a
    /// Learner host-sandbox preflight leaves behind after reviews have passed and
    /// the candidate already identifies the reviewed Writer commit.
    fn host_sandbox_paused_candidate_work_item() -> WorkItem {
        let mut work_item = WorkItem {
            id: "work-1".to_string(),
            title: "Preserve candidate across host-sandbox pause".to_string(),
            attempts: vec![Attempt {
                id: "attempt-1".to_string(),
                work_item_id: "work-1".to_string(),
                kind: AttemptKind::Write,
                status: AttemptStatus::Complete,
                coder_mapping: CoderMapping::default(),
                tasks: vec![completed_write_task("attempt-1-write-1", "original")],
                review_state: Some(AttemptReviewState::Passed),
                ..Default::default()
            }],
            merge_candidates: Vec::new(),
            ..Default::default()
        };
        work_item
            .create_or_get_merge_candidate("attempt-1")
            .expect("a reviewed attempt creates its candidate");
        suspend_attempt(&mut work_item.attempts[0], PauseKind::HostSandbox);
        work_item
    }

    #[test]
    fn canceled_pending_candidate_stays_valid_but_cannot_advance() {
        let mut item = host_sandbox_paused_candidate_work_item();
        let attempt = &mut item.attempts[0];
        attempt.status = AttemptStatus::Complete;
        attempt.pause_kind = None;
        attempt.cancellation = Some(AttemptCancellation {
            requested_at: now_iso8601(),
            settled_at: None,
            resumed_at: None,
        });
        let candidate = item.merge_candidates[0].clone();
        for learning in [
            None,
            Some(AttemptLearning::in_progress(1)),
            Some(AttemptLearning::failed(1, "canceled")),
            Some(AttemptLearning::succeeded(
                1,
                crate::follow_up::ArtifactRef {
                    path: "handoff.json".into(),
                    digest: "sha256:x".into(),
                },
            )),
        ] {
            item.attempts[0].learning = learning;
            item.validate()
                .expect("a durable stop must preserve its pending reviewed candidate");
            assert!(
                candidate.validate_advancement(&item).is_err(),
                "even a succeeded Learner cannot advance during cancellation"
            );
        }
        assert!(
            item.mark_merge_candidate_ready("attempt-1").is_err(),
            "a completed Learner cannot stamp readiness while cancellation is active"
        );
        for pause in [
            PauseKind::Interrupted,
            PauseKind::CandidateGate,
            PauseKind::Auth,
        ] {
            let mut invalid = item.clone();
            invalid.attempts[0].status = AttemptStatus::NeedsUser;
            invalid.attempts[0].pause_kind = Some(pause);
            assert!(candidate.validate(&invalid).is_err());
        }
        for review in [
            None,
            Some(AttemptReviewState::Failed),
            Some(AttemptReviewState::Uncertain),
        ] {
            let mut invalid = item.clone();
            invalid.attempts[0].review_state = review;
            assert!(candidate.validate(&invalid).is_err());
        }
        item.attempts[0].cancellation.as_mut().unwrap().resumed_at = Some(now_iso8601());
        item.validate().unwrap();
        assert!(
            candidate.validate_advancement(&item).is_ok(),
            "matching resume can restore advancement after succeeded Learning"
        );
    }

    #[test]
    fn host_sandbox_paused_candidate_is_structurally_valid_and_round_trips() {
        // B1: a reviewed candidate whose Attempt paused for a host-sandbox
        // preflight failure persists and reloads as a structurally valid Work
        // Item — the review evidence and the reviewed Writer commit survive.
        let work_item = host_sandbox_paused_candidate_work_item();
        assert_eq!(work_item.attempts[0].status, AttemptStatus::NeedsUser);
        assert_eq!(
            work_item.attempts[0].pause_kind,
            Some(PauseKind::HostSandbox)
        );
        work_item
            .validate()
            .expect("a host-sandbox-paused reviewed candidate is structurally valid");
        work_item.merge_candidates[0]
            .validate(&work_item)
            .expect("the candidate itself validates against its paused attempt");

        let tmp = tempfile::TempDir::new().unwrap();
        let store = WorkModelStore::new(tmp.path());
        store.create_work_item(&work_item).unwrap();
        let read = store.read_work_item("work-1").unwrap();
        read.validate()
            .expect("the reloaded Work Item stays structurally valid");
        assert_eq!(read.attempts[0].pause_kind, Some(PauseKind::HostSandbox));
        assert_eq!(
            read.merge_candidates.len(),
            1,
            "the pre-created candidate reloads alongside the typed pause"
        );
    }

    #[test]
    fn host_sandbox_paused_candidate_blocks_landing_until_learner_succeeds() {
        // B2: structural validity is not landing readiness. The host-sandbox
        // pause happens before the Learner runs, so the preserved candidate
        // stays blocked at the advancement gate until the Learner succeeds.
        let mut work_item = host_sandbox_paused_candidate_work_item();
        let candidate = work_item.merge_candidates[0].clone();
        let error = candidate
            .validate_advancement(&work_item)
            .expect_err("a candidate paused before its Learner cannot land");
        assert!(
            matches!(error, WorkModelError::AttemptLearningNotSucceeded { .. }),
            "the block is the shared non-succeeded-Learner reason: {error}"
        );

        // Once the host sandbox recovers and the Learner succeeds, the same
        // durable landing gate opens.
        let handoff = crate::follow_up::ArtifactRef {
            path: "handoff.json".to_string(),
            digest: "sha256:x".to_string(),
        };
        work_item.attempts[0].learning = Some(AttemptLearning::succeeded(1, handoff));
        work_item.merge_candidates[0]
            .clone()
            .validate_advancement(&work_item)
            .expect("a succeeded Learner passes the landing gate");
    }

    #[test]
    fn host_sandbox_exception_does_not_admit_neighboring_states() {
        // B3: the exception is exactly `needs-user` with `pause_kind:
        // host-sandbox`. Every other pause kind, and every non-passed review
        // state, keeps a pending candidate structurally invalid.
        for pause in [
            PauseKind::Auth,
            PauseKind::Uncertain,
            PauseKind::WriterCompletion,
            PauseKind::RoundCap,
            PauseKind::TranscriptPump,
        ] {
            let mut work_item = host_sandbox_paused_candidate_work_item();
            suspend_attempt(&mut work_item.attempts[0], pause.clone());
            assert_eq!(
                work_item.validate().unwrap_err(),
                WorkModelError::MergeCandidateAttemptReviewsNotPassed {
                    candidate_id: "attempt-1-merge-candidate".to_string(),
                    attempt_id: "attempt-1".to_string(),
                },
                "a {pause:?} pause must not inherit the host-sandbox exception"
            );
        }

        for (status, review) in [
            (AttemptStatus::Executing, AttemptReviewState::NotReviewed),
            (AttemptStatus::Reviewing, AttemptReviewState::Uncertain),
            (AttemptStatus::Complete, AttemptReviewState::Failed),
        ] {
            let mut work_item = host_sandbox_paused_candidate_work_item();
            work_item.attempts[0].status = status.clone();
            work_item.attempts[0].pause_kind = None;
            work_item.attempts[0].review_state = Some(review.clone());
            assert_eq!(
                work_item.validate().unwrap_err(),
                WorkModelError::MergeCandidateAttemptReviewsNotPassed {
                    candidate_id: "attempt-1-merge-candidate".to_string(),
                    attempt_id: "attempt-1".to_string(),
                },
                "a {status:?}/{review:?} attempt keeps its pending candidate invalid"
            );
        }

        // The host-sandbox exception relaxes only the completed status; it never
        // relaxes the passed-review requirement. A `needs-user`/`host-sandbox`
        // pause whose reviews are absent, uncertain, or failed keeps its pending
        // candidate structurally invalid.
        for review in [
            AttemptReviewState::NotReviewed,
            AttemptReviewState::Uncertain,
            AttemptReviewState::Failed,
        ] {
            let mut work_item = host_sandbox_paused_candidate_work_item();
            work_item.attempts[0].review_state = Some(review.clone());
            assert_eq!(
                work_item.attempts[0].status,
                AttemptStatus::NeedsUser,
                "the pause leaves the attempt in needs-user"
            );
            assert_eq!(
                work_item.attempts[0].pause_kind,
                Some(PauseKind::HostSandbox)
            );
            assert_eq!(
                work_item.validate().unwrap_err(),
                WorkModelError::MergeCandidateAttemptReviewsNotPassed {
                    candidate_id: "attempt-1-merge-candidate".to_string(),
                    attempt_id: "attempt-1".to_string(),
                },
                "a host-sandbox pause with {review:?} reviews stays invalid"
            );
        }
    }

    #[test]
    fn merge_candidate_validation_requires_latest_write_output() {
        let mut work_item = WorkItem {
            id: "work-1".to_string(),
            title: "Validate merge candidate provenance".to_string(),
            planning_context: None,
            instructions: None,
            abandonment: None,
            post_merge_review_fix_depth: None,
            attempts: vec![Attempt {
                id: "attempt-1".to_string(),
                work_item_id: "work-1".to_string(),
                kind: AttemptKind::Write,
                status: AttemptStatus::Complete,
                coder_mapping: CoderMapping::default(),
                tasks: vec![completed_write_task("attempt-1-write-1", "original")],
                review_state: Some(AttemptReviewState::Passed),
                pause_kind: None,
                artifacts: Vec::new(),
                created_at: None,
                completed_at: None,
                ..Default::default()
            }],
            merge_candidates: Vec::new(),
            ..Default::default()
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
            merge_review_state: MergeReviewState::Pending,
            merge_state: MergeCandidateMergeState::default(),
            created_at: None,
            ready_at: None,
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
    fn failed_merge_candidate_preserves_failed_merge_review_state() {
        let mut work_item = WorkItem {
            id: "work-1".to_string(),
            title: "Preserve merge failure state".to_string(),
            planning_context: None,
            instructions: None,
            abandonment: None,
            post_merge_review_fix_depth: None,
            attempts: vec![Attempt {
                id: "attempt-1".to_string(),
                work_item_id: "work-1".to_string(),
                kind: AttemptKind::Write,
                status: AttemptStatus::Reviewing,
                coder_mapping: CoderMapping::default(),
                tasks: vec![completed_write_task("attempt-1-write-1", "original")],
                review_state: Some(AttemptReviewState::Failed),
                pause_kind: None,
                artifacts: Vec::new(),
                created_at: None,
                completed_at: None,
                ..Default::default()
            }],
            merge_candidates: Vec::new(),
            ..Default::default()
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
            merge_review_state: MergeReviewState::Pending,
            merge_state: MergeCandidateMergeState {
                status: MergeCandidateMergeStatus::Failed,
                merged_commit: None,
                failure_reason: Some("Attempt review failed".to_string()),
                check_artifacts: Vec::new(),
                review_artifacts: Vec::new(),
                auto_merge_skipped: None,
                follow_up_failure: None,
                landing_intent: None,
            },
            created_at: None,
            ready_at: None,
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
            post_merge_review_fix_depth: None,
            attempts: vec![Attempt {
                id: "attempt-1".to_string(),
                work_item_id: "work-1".to_string(),
                kind: AttemptKind::Write,
                status: AttemptStatus::Complete,
                coder_mapping: CoderMapping::default(),
                tasks: vec![completed_write_task("attempt-1-write-1", "original")],
                review_state: Some(AttemptReviewState::Passed),
                pause_kind: None,
                artifacts: Vec::new(),
                created_at: None,
                completed_at: None,
                ..Default::default()
            }],
            merge_candidates: Vec::new(),
            ..Default::default()
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
            merge_review_state: MergeReviewState::Pending,
            merge_state: MergeCandidateMergeState {
                status: MergeCandidateMergeStatus::Failed,
                merged_commit: None,
                failure_reason: Some("candidate_commit mismatch".to_string()),
                check_artifacts: Vec::new(),
                review_artifacts: Vec::new(),
                auto_merge_skipped: None,
                follow_up_failure: None,
                landing_intent: None,
            },
            created_at: None,
            ready_at: None,
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
    fn failed_merge_candidate_allows_stale_readiness_timestamp() {
        // A landing failure is durable terminal history. Recovery may happen
        // after a later pause has changed the Attempt timestamp, so reading the
        // failed Candidate must not require its old readiness time to match.
        let mut work_item = work_item_with_completed_write("work-1");
        work_item.attempts[0].review_state = Some(AttemptReviewState::Passed);
        work_item.attempts[0].completed_at = Some("attempt-completed".to_string());
        let mut candidate = candidate_for_completed_write();
        candidate.ready_at = Some("candidate-ready".to_string());
        candidate.merge_state.status = MergeCandidateMergeStatus::Failed;
        candidate.merge_state.failure_reason = Some("landing hook failed".to_string());
        work_item.merge_candidates.push(candidate);

        work_item
            .validate()
            .expect("failed landing history remains readable for recovery");
    }

    #[test]
    fn merge_candidate_merge_review_state_is_separate_from_attempt_review_state() {
        let attempt = Attempt {
            id: "attempt-1".to_string(),
            work_item_id: "work-1".to_string(),
            kind: AttemptKind::Write,
            status: AttemptStatus::Reviewing,
            coder_mapping: CoderMapping::default(),
            tasks: Vec::new(),
            review_state: Some(AttemptReviewState::Uncertain),
            pause_kind: None,
            artifacts: Vec::new(),
            created_at: None,
            completed_at: None,
            ..Default::default()
        };
        let candidate = MergeCandidate {
            id: "candidate-1".to_string(),
            attempt_id: attempt.id.clone(),
            source_workspace: workspace("candidate"),
            target_workspace: workspace("main"),
            source_branch: "main".to_string(),
            target_branch: "main".to_string(),
            candidate_commit: "abc123".to_string(),
            merge_review_state: MergeReviewState::Passed,
            merge_state: MergeCandidateMergeState::default(),
            created_at: None,
            ready_at: None,
            started_at: None,
            completed_at: None,
        };

        assert_eq!(attempt.review_state, Some(AttemptReviewState::Uncertain));
        assert_eq!(candidate.merge_review_state, MergeReviewState::Passed);
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
            post_merge_review_fix_depth: None,
            attempts: Vec::new(),
            merge_candidates: Vec::new(),
            ..Default::default()
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
            message.contains(".fluent/work/tasks/work-1/attempt-1"),
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
            post_merge_review_fix_depth: None,
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
                            path: ".fluent/work/artifacts/work-1/attempt-1/attempt-1-review-tests"
                                .to_string(),
                        }),
                        review_context: Some(ReviewContext {
                            candidate_workspace_id: "candidate".to_string(),
                            candidate_workspace_path: "/workspaces/candidate".to_string(),
                            source_branch: "main".to_string(),
                            candidate_commit: "commit-initial".to_string(),
                            base_commit: None,
                        }),
                        writer_correction_assignment: None,
                        verification_context: None,
                        evidence_review_context: None,
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
                pause_kind: None,
                artifacts: Vec::new(),
                created_at: None,
                completed_at: None,
                ..Default::default()
            }],
            merge_candidates: Vec::new(),
            ..Default::default()
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
            writer_correction_assignment: None,
            verification_context: None,
            evidence_review_context: None,
            input_artifacts: Vec::new(),
            depends_on: None,
            output: Some(TaskOutput {
                workspace_id: "candidate".to_string(),
                workspace_path: format!("../work-6-work-1-attempt-1-{suffix}"),
                source_branch: "main".to_string(),
                base_commit: None,
                commit: format!("commit-{suffix}"),
                no_change: None,
                learner_canonicalization: None,
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
            post_merge_review_fix_depth: None,
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
                pause_kind: None,
                artifacts: Vec::new(),
                created_at: None,
                completed_at: None,
                ..Default::default()
            }],
            merge_candidates: Vec::new(),
            ..Default::default()
        }
    }

    #[test]
    fn writer_correction_assignment_round_trips_and_is_immutable() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = WorkModelStore::new(tmp.path());
        let evidence_path = work_artifact_path("work-1", "attempt-1", "review-tests/review.md");
        let evidence_file = tmp.path().join(&evidence_path);
        fs::create_dir_all(evidence_file.parent().unwrap()).unwrap();
        fs::write(&evidence_file, "- [ ] Preserve the assignment\n").unwrap();

        let mut item = work_item_with_completed_write("work-1");
        let task_id = item
            .add_next_write_round(
                "attempt-1",
                vec![ArtifactRef {
                    producer_id: "review-tests".into(),
                    path: evidence_path.clone(),
                }],
            )
            .unwrap();
        item.attempts[0].tasks[1].writer_correction_assignment = Some(WriterCorrectionAssignment {
            schema_version: WRITER_CORRECTION_ASSIGNMENT_SCHEMA_VERSION,
            work_item_id: "work-1".into(),
            attempt_id: "attempt-1".into(),
            writer_task_id: task_id.clone(),
            source_task_id: "attempt-1-write-1".into(),
            source_candidate_commit: "commit-initial".into(),
            source_task_inputs: Vec::new(),
            evidence: vec![WriterCorrectionAssignmentInput {
                artifact: ArtifactRef {
                    producer_id: "review-tests".into(),
                    path: evidence_path,
                },
                digest: format!("sha256:{}", "0".repeat(64)),
            }],
            findings: vec![WriterCompletionFinding {
                id: "finding:tests:preserve-assignment".into(),
                role: "tests".into(),
                source: evidence_file
                    .strip_prefix(tmp.path())
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
                requirement: "Preserve the assignment".into(),
            }],
            origin: WriterCorrectionAssignmentOrigin::Captured,
        });
        // Keep the finding source equal to the managed relative path used by
        // the evidence binding; the assignment intentionally stores no absolute
        // filesystem paths.
        let source = work_artifact_path("work-1", "attempt-1", "review-tests/review.md");
        item.attempts[0].tasks[1]
            .writer_correction_assignment
            .as_mut()
            .unwrap()
            .findings[0]
            .source = source;
        item.validate().unwrap();
        store.create_work_item(&item).unwrap();

        let stored = store.read_work_item("work-1").unwrap();
        assert_eq!(
            stored.attempts[0].tasks[1]
                .writer_correction_assignment
                .as_ref()
                .unwrap()
                .findings[0]
                .id,
            "finding:tests:preserve-assignment"
        );

        let mut tampered = stored;
        tampered.attempts[0].tasks[1].writer_correction_assignment = None;
        let error = store.write_work_item(&tampered).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("host-owned Writer correction assignment")
        );
    }

    fn persisted_writer_assignment_fixture(root: &Path) -> (WorkModelStore, WorkItem) {
        let store = WorkModelStore::new(root);
        let evidence_path = work_artifact_path("work-1", "attempt-1", "review-tests/review.md");
        let mut item = work_item_with_completed_write("work-1");
        let writer_task_id = item
            .add_next_write_round(
                "attempt-1",
                vec![ArtifactRef {
                    producer_id: "review-tests".into(),
                    path: evidence_path.clone(),
                }],
            )
            .unwrap();
        item.attempts[0].tasks[1].writer_correction_assignment = Some(WriterCorrectionAssignment {
            schema_version: WRITER_CORRECTION_ASSIGNMENT_SCHEMA_VERSION,
            work_item_id: "work-1".into(),
            attempt_id: "attempt-1".into(),
            writer_task_id,
            source_task_id: "attempt-1-write-1".into(),
            source_candidate_commit: "commit-initial".into(),
            source_task_inputs: Vec::new(),
            evidence: vec![WriterCorrectionAssignmentInput {
                artifact: ArtifactRef {
                    producer_id: "review-tests".into(),
                    path: evidence_path.clone(),
                },
                digest: format!("sha256:{}", "1".repeat(64)),
            }],
            findings: vec![WriterCompletionFinding {
                id: "finding:tests:assigned".into(),
                role: "tests".into(),
                source: evidence_path.clone(),
                requirement: "Preserve the assigned finding".into(),
            }],
            origin: WriterCorrectionAssignmentOrigin::Captured,
        });
        item.validate().unwrap();
        store.create_work_item(&item).unwrap();
        let stored = store.read_work_item("work-1").unwrap();
        (store, stored)
    }

    #[test]
    fn writer_assignment_safety_rejects_replacement_and_removal() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (store, stored) = persisted_writer_assignment_fixture(tmp.path());
        let original = stored.attempts[0].tasks[1]
            .writer_correction_assignment
            .clone()
            .unwrap();

        for replacement in [None, Some("commit-replaced")] {
            let mut tampered = stored.clone();
            if let Some(commit) = replacement {
                tampered.attempts[0].tasks[1]
                    .writer_correction_assignment
                    .as_mut()
                    .unwrap()
                    .source_candidate_commit = commit.into();
            } else {
                tampered.attempts[0].tasks[1].writer_correction_assignment = None;
            }
            let before = snapshot_dir(tmp.path());
            let error = store
                .write_work_item(&tampered)
                .expect_err("a published assignment cannot be removed or replaced");
            assert!(
                error
                    .to_string()
                    .contains("host-owned Writer correction assignment"),
                "immutable assignment rejection should identify its owner: {error}"
            );
            assert_eq!(
                snapshot_dir(tmp.path()),
                before,
                "a rejected assignment mutation must not change any persisted bytes"
            );
            assert_eq!(
                store.read_work_item("work-1").unwrap().attempts[0].tasks[1]
                    .writer_correction_assignment
                    .as_ref(),
                Some(&original)
            );
        }
    }

    #[test]
    fn writer_assignment_safety_rejects_unknown_schema_and_bounds() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (_store, stored) = persisted_writer_assignment_fixture(tmp.path());
        let task = &stored.attempts[0].tasks[1];
        let original = task.writer_correction_assignment.as_ref().unwrap();

        let mut unknown = original.clone();
        unknown.schema_version = WRITER_CORRECTION_ASSIGNMENT_SCHEMA_VERSION + 1;
        let error = unknown
            .validate_for_task(task)
            .expect_err("unknown assignment schemas must fail closed");
        assert!(error.contains("unsupported Writer correction assignment schema"));

        let mut oversized_inputs = original.clone();
        oversized_inputs.source_task_inputs = (0..=MAX_WRITER_CORRECTION_ASSIGNMENT_INPUTS)
            .map(|index| WriterCorrectionAssignmentInput {
                artifact: ArtifactRef {
                    producer_id: format!("source-{index}"),
                    path: format!(".fluent/work/artifacts/source-{index}.json"),
                },
                digest: format!("sha256:{}", "2".repeat(64)),
            })
            .collect();
        let error = oversized_inputs
            .validate_for_task(task)
            .expect_err("assignment inputs must stay bounded");
        assert!(error.contains("bounded input or finding limit"));

        let mut oversized_findings = original.clone();
        oversized_findings.findings = (0..=MAX_WRITER_CORRECTION_ASSIGNMENT_FINDINGS)
            .map(|index| WriterCompletionFinding {
                id: format!("finding:tests:{index}"),
                role: "tests".into(),
                source: original.evidence[0].artifact.path.clone(),
                requirement: "Preserve the assigned finding".into(),
            })
            .collect();
        let error = oversized_findings
            .validate_for_task(task)
            .expect_err("assignment findings must stay bounded");
        assert!(error.contains("bounded input or finding limit"));
    }

    #[test]
    fn writer_assignment_safety_retries_interrupted_publication() {
        for publication_point in ["before-root", "after-root"] {
            let tmp = tempfile::TempDir::new().unwrap();
            let (store, stored) = persisted_writer_assignment_fixture(tmp.path());
            let assignment = stored.attempts[0].tasks[1]
                .writer_correction_assignment
                .clone()
                .unwrap();
            let mut updated = stored.clone();
            updated.title = format!("Retry {publication_point}");

            let _fault = if publication_point == "before-root" {
                PersistFaultGuard::Split(persist_fault::arm_split_write("work-1"))
            } else {
                PersistFaultGuard::AfterRoot(persist_fault::arm_creation_after_publish("work-1"))
            };
            assert!(
                store.write_work_item(&updated).is_err(),
                "the fixture must interrupt {publication_point} publication"
            );
            let journal_path = store.work_transaction_path("work-1").unwrap();
            assert!(
                journal_path.exists(),
                "the interrupted transaction is durable"
            );
            let journal = read_json_file::<WorkItemTransaction>(&journal_path).unwrap();
            assert_eq!(
                journal.work_item.attempts[0].tasks[1]
                    .writer_correction_assignment
                    .as_ref(),
                Some(&assignment),
                "the transaction journal must carry the protected assignment"
            );
            drop(_fault);

            store
                .write_work_item(&updated)
                .expect("a retry should recover and publish the same assignment");
            assert!(
                !journal_path.exists(),
                "a successful retry cleans the journal"
            );
            let recovered = store.read_work_item("work-1").unwrap();
            assert_eq!(
                recovered.attempts[0].tasks[1]
                    .writer_correction_assignment
                    .as_ref(),
                Some(&assignment)
            );
            assert_eq!(recovered.title, updated.title);
        }
    }

    enum PersistFaultGuard {
        Split(persist_fault::SplitWriteFaultGuard),
        AfterRoot(persist_fault::CreationAfterPublishFaultGuard),
    }

    fn candidate_for_completed_write() -> MergeCandidate {
        MergeCandidate {
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
            merge_review_state: MergeReviewState::Pending,
            merge_state: MergeCandidateMergeState::default(),
            created_at: None,
            ready_at: None,
            started_at: None,
            completed_at: None,
        }
    }

    #[test]
    fn merge_candidate_readiness_stamps_one_shared_idempotent_time() {
        let mut item = work_item_with_completed_write("work-1");
        item.attempts[0].review_state = Some(AttemptReviewState::Passed);
        item.attempts[0].completed_at = Some("2026-01-01T00:00:00Z".to_string());
        item.attempts[0].learning = Some(AttemptLearning::succeeded(
            1,
            crate::follow_up::ArtifactRef {
                path: ".fluent/work/learner/work-1/attempt-1/handoff.json".to_string(),
                digest: "sha256:test".to_string(),
            },
        ));
        let candidate_id = item.create_or_get_merge_candidate("attempt-1").unwrap();

        assert_eq!(
            item.mark_merge_candidate_ready("attempt-1").unwrap(),
            candidate_id
        );
        let first = item.merge_candidates[0].ready_at.clone().unwrap();
        assert_eq!(
            item.attempts[0].completed_at.as_deref(),
            Some(first.as_str())
        );
        assert_eq!(item.merge_candidates[0].id, candidate_id);

        item.attempts[0].completed_at = Some("2025-01-01T00:00:00Z".to_string());
        item.mark_merge_candidate_ready("attempt-1").unwrap();
        assert_eq!(
            item.merge_candidates[0].ready_at.as_deref(),
            Some(first.as_str())
        );
        assert_eq!(
            item.attempts[0].completed_at.as_deref(),
            Some(first.as_str())
        );
    }

    #[test]
    fn aggregate_reader_waits_until_split_snapshot_is_complete() {
        use std::sync::{Arc, mpsc};
        use std::thread;
        use std::time::Duration;

        let tmp = tempfile::TempDir::new().unwrap();
        let store = Arc::new(WorkModelStore::new(tmp.path()));
        let initial = work_item_with_completed_write("work-1");
        store.create_work_item(&initial).unwrap();
        let mut updated = store.read_work_item("work-1").unwrap();
        updated.title = "Committed snapshot".to_string();
        updated.attempts[0].coder_mapping.write.model = "new-model".to_string();

        let model_lock = store.lock_work_item_model("work-1").unwrap();
        let path = store.work_item_path("work-1").unwrap();
        let mut record = WorkItemRecord::from(&updated);
        record.storage_revision = 1;
        crate::atomic_write::atomic_write(&path, to_json_pretty(&record).unwrap().as_bytes())
            .unwrap();

        let (sender, receiver) = mpsc::channel();
        let reader = Arc::clone(&store);
        let handle = thread::spawn(move || {
            sender.send(reader.read_work_item("work-1")).unwrap();
        });
        assert!(
            receiver.recv_timeout(Duration::from_millis(100)).is_err(),
            "an aggregate reader must not observe a top-level record before its children"
        );

        store.write_attempt_records(&updated).unwrap();
        drop(model_lock);
        let observed = receiver
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .unwrap();
        handle.join().unwrap();
        assert_eq!(observed.title, "Committed snapshot");
        assert_eq!(observed.attempts[0].coder_mapping.write.model, "new-model");
    }

    #[test]
    fn post_publication_creation_error_retains_recoverable_input_snapshot() {
        let tmp = tempfile::TempDir::new().unwrap();
        let source = tmp.path().join(".fluent/work/artifacts/evidence/input.txt");
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        fs::write(&source, b"authoritative\n").unwrap();
        let store = WorkModelStore::new(tmp.path());
        let mut item = WorkItem::planned("work-1", "Retain durable input");
        let _fault = persist_fault::arm_creation_after_publish("work-1");

        store
            .create_work_item_with_inputs(
                &mut item,
                &[".fluent/work/artifacts/evidence/input.txt".to_string()],
            )
            .expect_err("the injected post-publication cleanup fault must surface");

        let snapshot = tmp
            .path()
            .join(".fluent/work/artifacts/work-1/inputs/0000-input.txt");
        assert_eq!(fs::read(&snapshot).unwrap(), b"authoritative\n");
        let recovered = store.read_work_item("work-1").unwrap();
        assert_eq!(
            recovered.input_artifacts[0].snapshot_path,
            snapshot.strip_prefix(tmp.path()).unwrap().to_string_lossy()
        );
        assert_eq!(fs::read(snapshot).unwrap(), b"authoritative\n");
    }

    #[test]
    fn mutate_work_item_commits_a_reducer_and_no_ops_an_unchanged_snapshot() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = WorkModelStore::new(tmp.path());
        store
            .create_work_item(&work_item_with_completed_write("work-1"))
            .unwrap();
        let base_revision = store
            .read_work_item("work-1")
            .unwrap()
            .storage_revision
            .get();

        // A reducer that mutates commits its change and returns its value.
        let returned = store
            .mutate_work_item("work-1", |item| {
                item.title = "Reduced".to_string();
                Ok(item.attempts.len())
            })
            .unwrap();
        assert_eq!(returned, 1);
        let after_change = store.read_work_item("work-1").unwrap();
        assert_eq!(after_change.title, "Reduced");
        assert_ne!(
            after_change.storage_revision.get(),
            base_revision,
            "a committed mutation advances the storage revision"
        );

        // A reducer that leaves the aggregate unchanged commits nothing: the
        // revision is stable, so a rejected transition is a durable no-op.
        let changed_revision = after_change.storage_revision.get();
        store.mutate_work_item("work-1", |_item| Ok(())).unwrap();
        assert_eq!(
            store
                .read_work_item("work-1")
                .unwrap()
                .storage_revision
                .get(),
            changed_revision,
            "an unchanged reducer must not advance the revision"
        );

        // A reducer error propagates and commits nothing.
        let error = store
            .mutate_work_item("work-1", |_item| -> Result<(), WorkModelError> {
                Err(WorkModelError::TaskNotFound {
                    id: "missing".to_string(),
                })
            })
            .unwrap_err();
        assert!(matches!(error, WorkModelStorageError::InvalidModel { .. }));
        assert_eq!(store.read_work_item("work-1").unwrap().title, "Reduced");
    }

    #[test]
    fn snapshot_retries_publication_between_root_and_children() {
        for listing in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let store = WorkModelStore::new(root.path());
            store
                .create_work_item(&work_item_with_completed_write("work-1"))
                .unwrap();
            let writer = store.clone();
            let _interleave = persist_fault::after_next_root_read("work-1", move || {
                writer
                    .mutate_work_item("work-1", |item| {
                        item.title = "Published next revision".into();
                        item.attempts[0].tasks[0].instructions =
                            Some("Published next child".into());
                        Ok(())
                    })
                    .unwrap();
            });
            let read = if listing {
                store.snapshot_work_items().unwrap().items.remove(0)
            } else {
                store.read_work_item_snapshot("work-1").unwrap()
            };
            assert_eq!(read.title, "Published next revision");
            assert_eq!(
                read.attempts[0].tasks[0].instructions.as_deref(),
                Some("Published next child")
            );
            assert_eq!(read.storage_revision.get(), Some(1));
        }
    }

    #[test]
    fn snapshot_rejects_interrupted_child_publication() {
        let root = tempfile::tempdir().unwrap();
        let store = WorkModelStore::new(root.path());
        store
            .create_work_item(&work_item_with_completed_write("work-1"))
            .unwrap();
        store
            .create_work_item(&WorkItem::planned("unrelated", "Still visible"))
            .unwrap();
        let mut update = store.read_work_item_snapshot("work-1").unwrap();
        update.title = "Not yet published".into();
        update.attempts[0].tasks[0].instructions = Some("New child under the old root".into());
        let _fault = persist_fault::arm_split_write("work-1");
        assert!(store.write_work_item(&update).is_err());
        let journal = store.work_transaction_path("work-1").unwrap();
        let before = fs::read(&journal).unwrap();

        assert!(store.read_work_item_snapshot("work-1").is_err());
        let snapshot = store.snapshot_work_items().unwrap();
        assert_eq!(snapshot.items.len(), 1);
        assert_eq!(snapshot.items[0].id, "unrelated");
        assert!(
            snapshot
                .diagnostics
                .iter()
                .any(|error| error.contains("work-1")
                    && error.contains("host-owned recovery is pending"))
        );
        assert!(snapshot.diagnostics.iter().all(|error| error.len() <= 512));
        assert_eq!(fs::read(journal).unwrap(), before);
        assert_eq!(
            read_json_file::<WorkItemRecord>(&store.work_item_path("work-1").unwrap())
                .unwrap()
                .storage_revision,
            0
        );
    }

    #[test]
    #[cfg(unix)]
    fn snapshot_rejects_inaccessible_journal_after_child_publication() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().unwrap();
        let store = WorkModelStore::new(root.path());
        store
            .create_work_item(&work_item_with_completed_write("work-1"))
            .unwrap();
        let mut update = store.read_work_item_snapshot("work-1").unwrap();
        update.title = "Not yet published".into();
        update.attempts[0].tasks[0].instructions = Some("Unpublished child".into());
        let _fault = persist_fault::arm_split_write("work-1");
        assert!(store.write_work_item(&update).is_err());
        let before = snapshot_dir(root.path());

        let transactions = store.work_transactions_dir();
        let permissions = fs::metadata(&transactions).unwrap().permissions();
        fs::set_permissions(&transactions, fs::Permissions::from_mode(0o600)).unwrap();
        let lookup = fs::read(store.work_transaction_path("work-1").unwrap());
        let item = store.read_work_item_snapshot("work-1");
        let collection = store.snapshot_work_items();
        // Restore access before assertions so failure cannot strand the fixture.
        fs::set_permissions(&transactions, permissions).unwrap();

        assert_eq!(lookup.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
        assert!(matches!(
            item,
            Err(WorkModelStorageError::SnapshotUnavailable { .. })
        ));
        let snapshot = collection.unwrap();
        assert!(snapshot.items.is_empty());
        assert!(
            snapshot
                .diagnostics
                .iter()
                .any(|error| error.contains("work-1"))
        );
        assert!(snapshot.diagnostics.iter().all(|error| error.len() <= 512));
        assert_eq!(snapshot_dir(root.path()), before);
    }

    #[test]
    #[cfg(unix)]
    fn snapshot_rejects_split_collection_lookup_errors() {
        use std::os::unix::fs::PermissionsExt;

        for collection in ["attempts", "tasks", "attempt-tasks", "merge-candidates"] {
            let root = tempfile::tempdir().unwrap();
            let store = WorkModelStore::new(root.path());
            let item = if collection == "attempt-tasks" {
                work_item_with_completed_write("work-1")
            } else {
                WorkItem::planned("work-1", "Published")
            };
            store.create_work_item(&item).unwrap();
            let parent = match collection {
                "attempts" => store.work_attempts_dir(),
                "tasks" => store.work_tasks_dir(),
                "attempt-tasks" => store.work_tasks_dir().join("work-1"),
                "merge-candidates" => store.work_merge_candidates_dir(),
                _ => unreachable!(),
            };
            let before = snapshot_dir(root.path());
            let permissions = fs::metadata(&parent).unwrap().permissions();
            fs::set_permissions(&parent, fs::Permissions::from_mode(0o600)).unwrap();
            let item = store.read_work_item_snapshot("work-1");
            let snapshot = store.snapshot_work_items();
            fs::set_permissions(&parent, permissions).unwrap();

            assert!(
                matches!(
                    item,
                    Err(WorkModelStorageError::ReadDirectory { source, .. })
                        if source.kind() == io::ErrorKind::PermissionDenied
                ),
                "{collection} must report a lookup error"
            );
            let snapshot = snapshot.unwrap();
            assert!(snapshot.items.is_empty(), "{collection}");
            assert!(
                snapshot
                    .diagnostics
                    .iter()
                    .any(|error| error.contains("work-1"))
            );
            assert!(snapshot.diagnostics.iter().all(|error| error.len() <= 512));
            assert_eq!(snapshot_dir(root.path()), before, "{collection}");
        }
    }

    #[test]
    #[cfg(unix)]
    fn snapshot_rejects_orphan_task_metadata_errors() {
        for nested in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let store = WorkModelStore::new(root.path());
            store
                .create_work_item(&WorkItem::planned("work-1", "Published"))
                .unwrap();
            store
                .create_work_item(&WorkItem::planned("unrelated", "Still visible"))
                .unwrap();
            let orphan = store.work_tasks_dir().join("work-1").join("orphan");
            let link = if nested {
                fs::create_dir(&orphan).unwrap();
                orphan.join("unreadable")
            } else {
                orphan
            };
            // A symlink loop reliably fails metadata lookup even as root.
            std::os::unix::fs::symlink(&link, &link).unwrap();
            let before = snapshot_dir(root.path());

            assert!(matches!(
                store.read_work_item_snapshot("work-1"),
                Err(WorkModelStorageError::ReadDirectory { .. })
            ));
            let snapshot = store.snapshot_work_items().unwrap();
            assert_eq!(snapshot.items.len(), 1);
            assert_eq!(snapshot.items[0].id, "unrelated");
            assert!(
                snapshot
                    .diagnostics
                    .iter()
                    .any(|error| error.contains("work-1"))
            );
            assert!(snapshot.diagnostics.iter().all(|error| error.len() <= 512));
            assert_eq!(snapshot_dir(root.path()), before);
            assert_eq!(fs::read_link(&link).unwrap(), link);
        }
    }

    #[test]
    fn snapshot_reads_absent_split_collections_and_journal() {
        let root = tempfile::tempdir().unwrap();
        let store = WorkModelStore::new(root.path());
        let item = WorkItem::planned("work-1", "Published");
        store.create_work_item(&item).unwrap();
        for dir in [
            store.work_attempts_dir(),
            store.work_tasks_dir(),
            store.work_merge_candidates_dir(),
            store.work_transactions_dir(),
        ] {
            fs::remove_dir_all(dir).unwrap();
        }
        let before = snapshot_dir(root.path());
        assert_eq!(store.read_work_item_snapshot("work-1").unwrap(), item);
        let snapshot = store.snapshot_work_items().unwrap();
        assert_eq!(snapshot.items, vec![item]);
        assert!(snapshot.diagnostics.is_empty());
        assert_eq!(snapshot_dir(root.path()), before);
    }

    #[test]
    fn snapshot_reads_unchanged_children_with_pending_transaction() {
        let root = tempfile::tempdir().unwrap();
        let store = WorkModelStore::new(root.path());
        store
            .create_work_item(&work_item_with_completed_write("work-1"))
            .unwrap();
        let published = store.read_work_item_snapshot("work-1").unwrap();
        let mut update = published.clone();
        update.title = "Not yet published".into();
        let _fault = persist_fault::arm_split_write("work-1");
        assert!(store.write_work_item(&update).is_err());
        let journal = store.work_transaction_path("work-1").unwrap();
        let before = fs::read(&journal).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&before).unwrap();
        assert!(value["base_children_digest"].is_string());
        assert_eq!(store.read_work_item_snapshot("work-1").unwrap(), published);
        assert_eq!(store.snapshot_work_items().unwrap().items[0], published);
        assert_eq!(fs::read(journal).unwrap(), before);
    }

    #[test]
    fn snapshot_defers_legacy_journal_without_base_proof() {
        let root = tempfile::tempdir().unwrap();
        let store = WorkModelStore::new(root.path());
        store
            .create_work_item(&work_item_with_completed_write("work-1"))
            .unwrap();
        let mut update = store.read_work_item_snapshot("work-1").unwrap();
        update.title = "Legacy pending update".into();
        let _fault = persist_fault::arm_split_write("work-1");
        assert!(store.write_work_item(&update).is_err());
        let journal = store.work_transaction_path("work-1").unwrap();
        let mut value: serde_json::Value = read_json_file(&journal).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .remove("base_children_digest");
        write_json_file(&journal, &value).unwrap();
        let before = fs::read(&journal).unwrap();
        assert!(
            store
                .read_work_item_snapshot("work-1")
                .unwrap_err()
                .to_string()
                .contains("host-owned recovery is pending")
        );
        assert!(store.snapshot_work_items().unwrap().items.is_empty());
        assert_eq!(fs::read(journal).unwrap(), before);
        // Legacy journals remain recoverable at the existing mutation boundary.
        store.mutate_work_item("work-1", |_| Ok(())).unwrap();
        assert_eq!(
            store.read_work_item_snapshot("work-1").unwrap().title,
            "Legacy pending update"
        );
    }

    #[test]
    fn snapshot_reads_published_target_with_pending_cleanup() {
        let root = tempfile::tempdir().unwrap();
        let store = WorkModelStore::new(root.path());
        store
            .create_work_item(&work_item_with_completed_write("work-1"))
            .unwrap();
        let mut update = store.read_work_item_snapshot("work-1").unwrap();
        update.title = "Fully published".into();
        update.attempts[0].tasks[0].instructions = Some("Published child".into());
        let _fault = persist_fault::arm_creation_after_publish("work-1");
        assert!(store.write_work_item(&update).is_err());
        let journal = store.work_transaction_path("work-1").unwrap();
        let before = fs::read(&journal).unwrap();
        let read = store.read_work_item_snapshot("work-1").unwrap();
        assert_eq!(read.title, "Fully published");
        assert_eq!(read.attempts, update.attempts);
        assert_eq!(read.storage_revision.get(), Some(1));
        assert_eq!(fs::read(journal).unwrap(), before);
    }

    #[test]
    fn mutating_boundary_recovers_pending_transaction() {
        let tmp = tempfile::tempdir().unwrap();
        let store = WorkModelStore::new(tmp.path());
        store
            .create_work_item(&WorkItem::planned("work-1", "Published"))
            .unwrap();
        let mut target = store.read_work_item_snapshot("work-1").unwrap();
        target.title = "Recovered".into();
        let journal = store.work_transaction_path("work-1").unwrap();
        write_json_file(
            &journal,
            &WorkItemTransaction {
                schema_version: WorkItemTransaction::SCHEMA_VERSION,
                id: "work-1".into(),
                base_revision: Some(0),
                target_revision: 1,
                base_children_digest: Some(work_item_children_digest(&target).unwrap()),
                managed_files: Vec::new(),
                usage_limit_publications: Vec::new(),
                context_retry_publications: Vec::new(),
                context_retry_base: None,
                assignment_context_base: None,
                work_item: target,
            },
        )
        .unwrap();
        assert_eq!(
            store.read_work_item_snapshot("work-1").unwrap().title,
            "Published"
        );
        store
            .mutate_work_item("work-1", |item| {
                assert_eq!(item.title, "Recovered");
                assert!(!journal.exists());
                let lock =
                    File::open(tmp.path().join(".fluent/work/locks/work-1/model.lock")).unwrap();
                assert_eq!(
                    io::Error::from(
                        flock(&lock, FlockOperation::NonBlockingLockExclusive).unwrap_err()
                    )
                    .kind(),
                    io::ErrorKind::WouldBlock
                );
                item.title.push_str(" then mutated");
                Ok(())
            })
            .unwrap();
        let published = store.read_work_item_snapshot("work-1").unwrap();
        assert_eq!(published.title, "Recovered then mutated");
        assert_eq!(published.storage_revision.get(), Some(2));
    }

    #[test]
    fn failed_split_write_retries_from_durable_transaction() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = WorkModelStore::new(tmp.path());
        store
            .create_work_item(&work_item_with_completed_write("work-1"))
            .unwrap();
        let mut updated = store.read_work_item("work-1").unwrap();
        updated.title = "Recovered transaction".to_string();
        updated.attempts[0].review_state = Some(AttemptReviewState::Passed);
        updated
            .merge_candidates
            .push(candidate_for_completed_write());
        let _fault = persist_fault::arm_split_write("work-1");

        assert!(store.write_work_item(&updated).is_err());
        let transaction_path = tmp.path().join(".fluent/work/transactions/work-1.json");
        assert!(
            transaction_path.is_file(),
            "the retry snapshot must remain durable"
        );
        let top_level: WorkItemRecord =
            read_json_file(&store.work_item_path("work-1").unwrap()).unwrap();
        assert_eq!(top_level.storage_revision, 0, "publish the revision last");

        store.write_work_item(&updated).unwrap();

        let recovered = WorkModelStore::new(tmp.path())
            .read_work_item("work-1")
            .unwrap();
        assert_eq!(recovered.title, "Recovered transaction");
        assert_eq!(recovered.merge_candidates.len(), 1);
        assert!(!transaction_path.exists());
    }

    #[test]
    fn fresh_store_recovers_interrupted_creation_before_listing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = WorkModelStore::new(tmp.path());
        let mut item = work_item_with_completed_write("work-1");
        item.attempts[0].review_state = Some(AttemptReviewState::Passed);
        item.merge_candidates.push(candidate_for_completed_write());
        let candidate_path = store
            .work_merge_candidate_path("work-1", "attempt-1-merge-candidate")
            .unwrap();
        fs::create_dir_all(&candidate_path).unwrap();

        assert!(store.create_work_item(&item).is_err());
        assert!(!store.work_item_path("work-1").unwrap().exists());
        fs::remove_dir(&candidate_path).unwrap();

        let recovered = WorkModelStore::new(tmp.path()).list_work_items().unwrap();
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].merge_candidates.len(), 1);
    }

    #[test]
    fn storage_revision_exhaustion_fails_closed() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = WorkModelStore::new(tmp.path());
        store
            .create_work_item(&work_item_with_completed_write("work-1"))
            .unwrap();
        let path = store.work_item_path("work-1").unwrap();
        let mut record: WorkItemRecord = read_json_file(&path).unwrap();
        record.storage_revision = u64::MAX;
        write_json_file(&path, &record).unwrap();
        let mut item = store.read_work_item("work-1").unwrap();
        item.title = "must not publish".to_string();

        let error = store.write_work_item(&item).unwrap_err();

        assert!(matches!(
            error,
            WorkModelStorageError::StorageRevisionExhausted { .. }
        ));
        assert_ne!(
            store.read_work_item("work-1").unwrap().title,
            "must not publish"
        );
    }

    #[test]
    fn identical_aggregate_write_preserves_bytes_and_revision() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = WorkModelStore::new(tmp.path());
        store
            .create_work_item(&work_item_with_completed_write("work-1"))
            .unwrap();
        let path = store.work_item_path("work-1").unwrap();
        let before = fs::read(&path).unwrap();
        let item = store.read_work_item("work-1").unwrap();

        store.write_work_item(&item).unwrap();

        assert_eq!(fs::read(&path).unwrap(), before);
        assert_eq!(item.storage_revision.get(), Some(0));
    }

    #[test]
    fn missing_work_read_does_not_create_model_lock() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = WorkModelStore::new(tmp.path());

        assert!(store.read_work_item("missing-work").is_err());

        assert!(
            !tmp.path()
                .join(".fluent/work/locks/missing-work/model.lock")
                .exists()
        );
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
                        post_merge_review_fix_depth: None,
                        attempts: Vec::new(),
                        merge_candidates: Vec::new(),
                        ..Default::default()
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
            post_merge_review_fix_depth: None,
            attempts: Vec::new(),
            merge_candidates: Vec::new(),
            ..Default::default()
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
            pause_kind: None,
            artifacts: Vec::new(),
            created_at: None,
            completed_at: None,
            ..Default::default()
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
            id: "work-post-merge-review".to_string(),
            title: "Post-merge review".to_string(),
            planning_context: None,
            instructions: None,
            abandonment: None,
            post_merge_review_fix_depth: None,
            attempts: Vec::new(),
            merge_candidates: Vec::new(),
            ..Default::default()
        };
        let task_ids = item
            .add_post_merge_review_attempt(
                "attempt-1",
                &["skills", "tests"],
                "main",
                "abc123",
                Some("base123".to_string()),
            )
            .unwrap();
        assert_eq!(task_ids.len(), 3, "1 tester + 2 reviewers");
        assert_eq!(task_ids[0], "attempt-1-tester");
        assert_eq!(item.attempts[0].kind, AttemptKind::PostMergeReview);
        assert_eq!(item.attempts[0].tasks[0].kind, TaskKind::Tester);
        assert_eq!(item.attempts[0].tasks[1].kind, TaskKind::Review);
        assert_eq!(
            item.attempts[0].tasks[1].depends_on.as_deref(),
            Some("attempt-1-tester")
        );
        let worktree_path = crate::review_only_worktree::review_only_worktree_path("main");
        for task in &item.attempts[0].tasks {
            assert_eq!(task.workspace_access.reads[0].path, worktree_path);
            assert_eq!(
                task.review_context
                    .as_ref()
                    .and_then(|context| context.base_commit.as_deref()),
                Some("base123")
            );
        }

        store.create_work_item(&item).unwrap();
        let loaded = store.read_work_item("work-post-merge-review").unwrap();
        assert_eq!(loaded.attempts[0].kind, AttemptKind::PostMergeReview);
        assert_eq!(loaded.attempts[0].tasks.len(), 3);
        assert!(loaded.attempts[0].tasks.iter().all(|task| {
            task.review_context
                .as_ref()
                .and_then(|context| context.base_commit.as_deref())
                == Some("base123")
        }));
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
            id: "work-post-merge-review".to_string(),
            title: "Post-merge review".to_string(),
            planning_context: None,
            instructions: None,
            abandonment: None,
            post_merge_review_fix_depth: None,
            attempts: Vec::new(),
            merge_candidates: Vec::new(),
            ..Default::default()
        };
        item.add_post_merge_review_attempt("attempt-1", &["skills"], "main", "abc123", None)
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
            writer_correction_assignment: None,
            verification_context: None,
            evidence_review_context: None,
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
            writer_correction_assignment: None,
            verification_context: None,
            evidence_review_context: None,
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
            pause_kind: None,
            artifacts: Vec::new(),
            created_at: Some("2026-06-12T10:00:00+00:00".to_string()),
            completed_at: Some("2026-06-12T10:05:00+00:00".to_string()),
            ..Default::default()
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
            merge_review_state: MergeReviewState::Pending,
            merge_state: MergeCandidateMergeState::default(),
            created_at: Some("2026-06-12T10:00:00+00:00".to_string()),
            ready_at: Some("2026-06-12T10:04:00+00:00".to_string()),
            started_at: Some("2026-06-12T10:01:00+00:00".to_string()),
            completed_at: Some("2026-06-12T10:05:00+00:00".to_string()),
        };
        let json = serde_json::to_string(&candidate).unwrap();
        let round_tripped: MergeCandidate = serde_json::from_str(&json).unwrap();
        assert_eq!(round_tripped.created_at, candidate.created_at);
        assert_eq!(round_tripped.ready_at, candidate.ready_at);
        assert_eq!(round_tripped.started_at, candidate.started_at);
        assert_eq!(round_tripped.completed_at, candidate.completed_at);
    }

    #[test]
    fn legacy_merge_candidate_without_ready_at_round_trips_without_field() {
        let json = r#"{
            "id": "mc-1",
            "attempt_id": "a-1",
            "source_workspace": {"id": "candidate", "path": "../candidate"},
            "target_workspace": {"id": "target", "path": "."},
            "source_branch": "main",
            "target_branch": "main",
            "candidate_commit": "abc123",
            "merge_review_state": "pending",
            "merge_state": {"status": "pending"}
        }"#;

        let candidate: MergeCandidate = serde_json::from_str(json).unwrap();
        assert_eq!(candidate.ready_at, None);
        let reserialized = serde_json::to_string(&candidate).unwrap();
        assert!(!reserialized.contains("\"ready_at\""));
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
    fn task_output_base_commit_is_backward_compatible() {
        let legacy = r#"{
            "workspace_id": "candidate",
            "workspace_path": "../work-candidate",
            "source_branch": "main",
            "commit": "abc123"
        }"#;
        let output: TaskOutput = serde_json::from_str(legacy).unwrap();
        assert!(output.base_commit.is_none());
        assert!(
            output.no_change.is_none(),
            "legacy output remains an ordinary committed Writer output"
        );
        assert!(output.learner_canonicalization.is_none());
        assert!(
            !serde_json::to_string(&output)
                .unwrap()
                .contains("base_commit")
        );
        assert!(
            !serde_json::to_string(&output)
                .unwrap()
                .contains("no_change")
        );

        let mut persisted = output;
        persisted.base_commit = Some("base123".to_string());
        let json = serde_json::to_string(&persisted).unwrap();
        assert!(json.contains(r#""base_commit":"base123""#));
    }

    fn valid_no_change_task() -> Task {
        Task {
            id: "attempt-1-write-2".to_string(),
            kind: TaskKind::Write,
            status: TaskStatus::Complete,
            role: "author".to_string(),
            instructions: None,
            work_item_id: "work-1".to_string(),
            attempt_id: Some("attempt-1".to_string()),
            workspace_access: WorkspaceAccess {
                reads: Vec::new(),
                writes: vec![workspace("candidate")],
            },
            artifact_area: Some(TaskArtifactArea {
                path: ".fluent/work/artifacts/work-1/attempt-1/attempt-1-write-2".to_string(),
            }),
            review_context: None,
            writer_correction_assignment: None,
            verification_context: None,
            evidence_review_context: None,
            input_artifacts: Vec::new(),
            depends_on: None,
            output: Some(TaskOutput {
                workspace_id: "candidate".to_string(),
                workspace_path: "/workspaces/candidate".to_string(),
                source_branch: "main".to_string(),
                base_commit: Some("abc123".to_string()),
                commit: "abc123".to_string(),
                no_change: Some(NoChangeOutput {
                    schema_version: 1,
                    declaration_path:
                        ".fluent/work/artifacts/work-1/attempt-1/attempt-1-write-2/no-change.json"
                            .to_string(),
                    reason: "The candidate already passes the requested check".to_string(),
                    verification: vec![NoChangeVerification {
                        command: "cargo test focused_test".to_string(),
                        result: NoChangeVerificationResult::Pass,
                    }],
                }),
                learner_canonicalization: None,
            }),
            created_at: None,
            started_at: None,
            completed_at: None,
        }
    }

    #[test]
    fn valid_no_change_task_output_passes_model_validation() {
        valid_no_change_task().validate().unwrap();
    }

    #[test]
    fn no_change_commit_divergence_requires_successful_capture_learning() {
        let mut canonicalized = valid_no_change_task();
        let output = canonicalized.output.as_mut().unwrap();
        output.commit = "def456".to_string();
        output.learner_canonicalization = Some(LearnerCommitCanonicalization {
            from_commit: "abc123".to_string(),
            to_commit: "def456".to_string(),
        });
        canonicalized.validate().unwrap();

        let mut item = WorkItem::planned("work-1", "Canonicalized no-change output");
        item.add_initial_attempt("attempt-1").unwrap();
        item.attempts[0].tasks[0] = canonicalized.clone();
        item.attempts[0].status = AttemptStatus::Complete;
        item.attempts[0].review_state = Some(AttemptReviewState::Passed);
        assert!(matches!(
            item.validate(),
            Err(WorkModelError::InvalidTaskNoChangeOutput { .. })
        ));

        item.attempts[0].learning = Some(AttemptLearning::in_progress(1));
        assert!(matches!(
            item.validate(),
            Err(WorkModelError::InvalidTaskNoChangeOutput { .. })
        ));
        assert!(matches!(
            item.attempts[0].learning_advancement_readiness(),
            Err(WorkModelError::AttemptLearningNotSucceeded {
                state: "in progress",
                ..
            })
        ));

        item.attempts[0].learning = Some(AttemptLearning::handoff_pending(1));
        assert!(matches!(
            item.validate(),
            Err(WorkModelError::InvalidTaskNoChangeOutput { .. })
        ));

        item.merge_candidates.push(MergeCandidate {
            id: "attempt-1-merge-candidate".to_string(),
            attempt_id: "attempt-1".to_string(),
            source_workspace: workspace("candidate"),
            target_workspace: WorkspaceRef {
                id: "target".to_string(),
                path: ".".to_string(),
            },
            source_branch: "main".to_string(),
            target_branch: "main".to_string(),
            candidate_commit: "def456".to_string(),
            merge_review_state: MergeReviewState::Pending,
            merge_state: MergeCandidateMergeState::default(),
            created_at: None,
            ready_at: None,
            started_at: None,
            completed_at: None,
        });
        item.validate().unwrap();

        for kind in [
            LearningFailureKind::Generic,
            LearningFailureKind::TranscriptPump,
            LearningFailureKind::EvidencePending,
        ] {
            item.attempts[0].learning = Some(AttemptLearning::failed_with_kind(
                1,
                "ordinary failure",
                kind,
            ));
            assert!(matches!(
                item.validate(),
                Err(WorkModelError::InvalidTaskNoChangeOutput { .. })
            ));
        }

        let mut publication_failure = AttemptLearning::failed_with_kind(
            1,
            "handoff publication failed",
            LearningFailureKind::Generic,
        );
        publication_failure.failure_stage = Some(LearningFailureStage::HandoffPublication);
        item.attempts[0].learning = Some(publication_failure);
        item.validate().unwrap();

        item.merge_candidates[0].candidate_commit = "other".to_string();
        assert!(matches!(
            item.validate(),
            Err(WorkModelError::InvalidTaskNoChangeOutput { .. })
        ));
        item.merge_candidates[0].candidate_commit = "def456".to_string();

        let mut stale_writer = valid_no_change_task();
        stale_writer.id = "attempt-1-write-3".to_string();
        stale_writer.output.as_mut().unwrap().base_commit = Some("def456".to_string());
        stale_writer.output.as_mut().unwrap().commit = "def456".to_string();
        stale_writer.output.as_mut().unwrap().no_change = None;
        stale_writer.artifact_area = None;
        item.attempts[0].tasks.push(stale_writer);
        assert!(matches!(
            item.validate(),
            Err(WorkModelError::InvalidTaskNoChangeOutput { .. })
        ));
        item.attempts[0].tasks.pop();

        item.learner_mode = LearnerMode::NoExpertise;
        assert!(matches!(
            item.validate(),
            Err(WorkModelError::InvalidTaskNoChangeOutput { .. })
        ));

        let mut cases = Vec::new();

        let mut missing = canonicalized.clone();
        missing.output.as_mut().unwrap().learner_canonicalization = None;
        cases.push(("missing transition", missing));

        let mut reversed = canonicalized.clone();
        reversed
            .output
            .as_mut()
            .unwrap()
            .learner_canonicalization
            .as_mut()
            .unwrap()
            .from_commit = "def456".to_string();
        reversed
            .output
            .as_mut()
            .unwrap()
            .learner_canonicalization
            .as_mut()
            .unwrap()
            .to_commit = "abc123".to_string();
        cases.push(("reversed transition", reversed));

        let mut mismatched_from = canonicalized.clone();
        mismatched_from
            .output
            .as_mut()
            .unwrap()
            .learner_canonicalization
            .as_mut()
            .unwrap()
            .from_commit = "other".to_string();
        cases.push(("mismatched source", mismatched_from));

        let mut mismatched_to = canonicalized;
        mismatched_to
            .output
            .as_mut()
            .unwrap()
            .learner_canonicalization
            .as_mut()
            .unwrap()
            .to_commit = "other".to_string();
        cases.push(("mismatched destination", mismatched_to));

        for (name, task) in cases {
            assert!(
                matches!(
                    task.validate(),
                    Err(WorkModelError::InvalidTaskNoChangeOutput { .. })
                ),
                "{name} must fail no-change validation"
            );
        }
    }

    #[test]
    fn learner_canonicalization_rejects_partial_json_and_omits_none() {
        let mut output = valid_no_change_task().output.unwrap();
        let legacy_json = serde_json::to_string(&output).unwrap();
        assert!(!legacy_json.contains("learner_canonicalization"));

        let partial = legacy_json.replace(
            r#""commit":"abc123""#,
            r#""commit":"def456","learner_canonicalization":{"from_commit":"abc123"}"#,
        );
        assert!(serde_json::from_str::<TaskOutput>(&partial).is_err());

        output.commit = "def456".to_string();
        output.learner_canonicalization = Some(LearnerCommitCanonicalization {
            from_commit: "abc123".to_string(),
            to_commit: "def456".to_string(),
        });
        let round_tripped: TaskOutput =
            serde_json::from_str(&serde_json::to_string(&output).unwrap()).unwrap();
        assert_eq!(round_tripped, output);
    }

    #[test]
    fn ordinary_committed_task_output_may_advance_from_base_commit() {
        let mut task = valid_no_change_task();
        let output = task.output.as_mut().unwrap();
        output.commit = "def456".to_string();
        output.no_change = None;

        task.validate().unwrap();
    }

    #[test]
    fn invalid_no_change_task_output_shapes_fail_model_validation() {
        let mut cases = Vec::new();

        let mut changed_commit = valid_no_change_task();
        changed_commit.output.as_mut().unwrap().commit = "def456".to_string();
        cases.push(("changed commit", changed_commit));

        let mut missing_base = valid_no_change_task();
        missing_base.output.as_mut().unwrap().base_commit = None;
        cases.push(("missing base commit", missing_base));

        let mut wrong_kind = valid_no_change_task();
        wrong_kind.kind = TaskKind::Tester;
        cases.push(("non-Writer task", wrong_kind));

        let mut wrong_path = valid_no_change_task();
        wrong_path
            .output
            .as_mut()
            .unwrap()
            .no_change
            .as_mut()
            .unwrap()
            .declaration_path = ".fluent/work/artifacts/other/no-change.json".to_string();
        cases.push(("wrong declaration path", wrong_path));

        let mut wrong_version = valid_no_change_task();
        wrong_version
            .output
            .as_mut()
            .unwrap()
            .no_change
            .as_mut()
            .unwrap()
            .schema_version = 2;
        cases.push(("wrong schema version", wrong_version));

        let mut empty_reason = valid_no_change_task();
        empty_reason
            .output
            .as_mut()
            .unwrap()
            .no_change
            .as_mut()
            .unwrap()
            .reason = " ".to_string();
        cases.push(("empty reason", empty_reason));

        let mut empty_verification = valid_no_change_task();
        empty_verification
            .output
            .as_mut()
            .unwrap()
            .no_change
            .as_mut()
            .unwrap()
            .verification
            .clear();
        cases.push(("empty verification", empty_verification));

        let mut empty_command = valid_no_change_task();
        empty_command
            .output
            .as_mut()
            .unwrap()
            .no_change
            .as_mut()
            .unwrap()
            .verification[0]
            .command = " ".to_string();
        cases.push(("empty verification command", empty_command));

        for (case, task) in cases {
            assert!(
                matches!(
                    task.validate(),
                    Err(WorkModelError::InvalidTaskNoChangeOutput { .. })
                ),
                "{case} must fail no-change validation"
            );
        }
    }

    #[test]
    fn valid_no_change_output_round_trips_through_storage() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = WorkModelStore::new(tmp.path());
        let mut item = WorkItem::planned("work-1", "No-change output");
        item.add_initial_attempt("attempt-1").unwrap();
        item.attempts[0].tasks[0] = completed_write_task("attempt-1-write-1", "first");
        item.attempts[0].tasks[0].output.as_mut().unwrap().commit = "abc123".to_string();
        item.add_next_write_round("attempt-1", Vec::new()).unwrap();
        item.attempts[0].tasks.pop();
        item.attempts[0].tasks.push(valid_no_change_task());

        store.create_work_item(&item).unwrap();
        let loaded = store.read_work_item("work-1").unwrap();

        assert_eq!(loaded.attempts[0].tasks[1], valid_no_change_task());
    }

    #[test]
    fn canonicalized_no_change_output_round_trips_through_storage() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = WorkModelStore::new(tmp.path());
        let mut item = WorkItem::planned("work-1", "Canonicalized no-change output");
        item.add_initial_attempt("attempt-1").unwrap();
        item.attempts[0].tasks[0] = completed_write_task("attempt-1-write-1", "first");
        item.attempts[0].tasks[0].output.as_mut().unwrap().commit = "abc123".to_string();
        item.add_next_write_round("attempt-1", Vec::new()).unwrap();
        let mut canonicalized = valid_no_change_task();
        let output = canonicalized.output.as_mut().unwrap();
        output.commit = "def456".to_string();
        output.learner_canonicalization = Some(LearnerCommitCanonicalization {
            from_commit: "abc123".to_string(),
            to_commit: "def456".to_string(),
        });
        item.attempts[0].tasks.pop();
        item.attempts[0].tasks.push(canonicalized.clone());
        item.attempts[0].learning = Some(AttemptLearning::handoff_pending(1));
        item.attempts[0].status = AttemptStatus::Complete;
        item.attempts[0].review_state = Some(AttemptReviewState::Passed);
        item.create_or_get_merge_candidate("attempt-1").unwrap();

        store.create_work_item(&item).unwrap();
        let loaded = store.read_work_item("work-1").unwrap();

        assert_eq!(loaded.attempts[0].tasks[1], canonicalized);
    }

    #[test]
    fn storage_rejects_no_change_output_with_changed_commit() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = WorkModelStore::new(tmp.path());
        let mut item = WorkItem::planned("work-1", "Contradictory no-change output");
        item.add_initial_attempt("attempt-1").unwrap();
        item.attempts[0].tasks[0] = completed_write_task("attempt-1-write-1", "first");
        item.attempts[0].tasks[0].output.as_mut().unwrap().commit = "abc123".to_string();
        item.add_next_write_round("attempt-1", Vec::new()).unwrap();
        let mut contradictory = valid_no_change_task();
        contradictory.output.as_mut().unwrap().commit = "def456".to_string();
        item.attempts[0].tasks.pop();
        item.attempts[0].tasks.push(contradictory);

        assert!(store.create_work_item(&item).is_err());
        assert!(!store.work_item_path("work-1").unwrap().exists());
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
            writer_correction_assignment: None,
            verification_context: None,
            evidence_review_context: None,
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
            writer_correction_assignment: None,
            verification_context: None,
            evidence_review_context: None,
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
            writer_correction_assignment: None,
            verification_context: None,
            evidence_review_context: None,
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
            pause_kind: None,
            artifacts: Vec::new(),
            created_at: None,
            completed_at: None,
            ..Default::default()
        };
        set_attempt_terminal(&mut attempt, AttemptStatus::Complete);
        assert_eq!(attempt.status, AttemptStatus::Complete);
        assert!(attempt.completed_at.is_some());
    }

    #[test]
    fn complete_attempt_accepts_superseded_task_history() {
        let mut item = work_item_with_completed_write("work-1");
        let attempt = &mut item.attempts[0];
        let mut superseded = attempt.tasks[0].clone();
        superseded.id = "attempt-1-write-superseded".to_string();
        superseded.status = TaskStatus::Superseded;
        superseded.output = None;
        superseded.artifact_area = Some(TaskArtifactArea {
            path: work_artifact_path("work-1", "attempt-1", &superseded.id),
        });
        attempt.tasks.insert(0, superseded);
        set_attempt_terminal(attempt, AttemptStatus::Complete);

        attempt.validate("work-1").unwrap();
    }

    fn attempt_in(status: AttemptStatus, pause: Option<PauseKind>) -> Attempt {
        Attempt {
            id: "a-1".to_string(),
            work_item_id: "w-1".to_string(),
            kind: AttemptKind::Write,
            status,
            coder_mapping: CoderMapping::default(),
            tasks: Vec::new(),
            review_state: None,
            pause_kind: pause,
            artifacts: Vec::new(),
            created_at: None,
            completed_at: None,
            ..Default::default()
        }
    }

    #[test]
    fn transition_attempt_enforces_monotonic_precedence() {
        // A resumable transcript pause is recorded over an active Attempt.
        let mut a = attempt_in(AttemptStatus::Reviewing, None);
        assert!(transition_attempt(
            &mut a,
            AttemptStatus::NeedsUser,
            Some(PauseKind::TranscriptPump)
        ));
        assert_eq!(a.pause_kind, Some(PauseKind::TranscriptPump));

        // A later peer completion must NOT overwrite the resumable pause.
        assert!(!transition_attempt(&mut a, AttemptStatus::Complete, None));
        assert_eq!(a.status, AttemptStatus::NeedsUser);
        assert_eq!(a.pause_kind, Some(PauseKind::TranscriptPump));

        // A later active transition must NOT overwrite the terminal Attempt.
        assert!(!transition_attempt(&mut a, AttemptStatus::Reviewing, None));
        assert_eq!(a.status, AttemptStatus::NeedsUser);

        // A hard failure DOES dominate a resumable pause.
        assert!(transition_attempt(&mut a, AttemptStatus::Failed, None));
        assert_eq!(a.status, AttemptStatus::Failed);

        // A resumable pause must NOT mask the hard failure.
        assert!(!transition_attempt(
            &mut a,
            AttemptStatus::NeedsUser,
            Some(PauseKind::TranscriptPump)
        ));
        assert_eq!(a.status, AttemptStatus::Failed);
    }

    #[test]
    fn transition_attempt_preserves_first_equal_rank_terminal() {
        // Two resumable pauses of equal rank: the first is preserved.
        let mut a = attempt_in(AttemptStatus::Executing, None);
        assert!(transition_attempt(
            &mut a,
            AttemptStatus::NeedsUser,
            Some(PauseKind::Auth)
        ));
        assert!(!transition_attempt(
            &mut a,
            AttemptStatus::NeedsUser,
            Some(PauseKind::TranscriptPump)
        ));
        assert_eq!(
            a.pause_kind,
            Some(PauseKind::Auth),
            "equal-rank terminal events preserve the first result"
        );
    }

    #[test]
    fn a_new_attempt_state_clears_the_previous_pause_recovery() {
        let mut attempt = attempt_in(AttemptStatus::NeedsUser, Some(PauseKind::TesterHarness));
        attempt.pause_recovery = Some(PauseRecovery::TesterNoSandbox);

        assert!(transition_attempt(
            &mut attempt,
            AttemptStatus::Failed,
            None
        ));

        assert_eq!(attempt.status, AttemptStatus::Failed);
        assert!(attempt.pause_kind.is_none());
        assert!(attempt.pause_recovery.is_none());
    }

    #[test]
    fn reopen_attempt_replans_only_pause_failed_tasks() {
        // Reopen resets only the resumable pause-failed Task (durably NeedsUser),
        // leaving Complete peers, hard Failed Tasks, and still-live Executing
        // Tasks untouched.
        let mut a = attempt_in(AttemptStatus::NeedsUser, Some(PauseKind::TranscriptPump));
        let task = |id: &str, status: TaskStatus| Task {
            id: id.to_string(),
            kind: TaskKind::Write,
            status,
            role: "author".to_string(),
            instructions: None,
            work_item_id: "w-1".to_string(),
            attempt_id: Some("a-1".to_string()),
            workspace_access: WorkspaceAccess {
                reads: Vec::new(),
                writes: Vec::new(),
            },
            artifact_area: None,
            review_context: None,
            writer_correction_assignment: None,
            verification_context: None,
            evidence_review_context: None,
            input_artifacts: Vec::new(),
            depends_on: None,
            output: None,
            created_at: None,
            started_at: None,
            completed_at: Some("2026-07-21T00:00:00Z".to_string()),
        };
        a.tasks = vec![
            task("complete", TaskStatus::Complete),
            task("pause-failed", TaskStatus::NeedsUser),
            task("hard-failed", TaskStatus::Failed),
            task("live", TaskStatus::Executing),
        ];

        reopen_attempt(&mut a);

        assert_eq!(a.status, AttemptStatus::Planned);
        assert!(a.pause_kind.is_none());
        assert_eq!(a.tasks[0].status, TaskStatus::Complete, "complete stays");
        assert_eq!(
            a.tasks[1].status,
            TaskStatus::Planned,
            "the pause-failed task replans"
        );
        assert!(
            a.tasks[1].completed_at.is_none(),
            "the replanned task clears its terminal timestamp"
        );
        assert_eq!(
            a.tasks[2].status,
            TaskStatus::Failed,
            "a hard-failed task is not silently retried"
        );
        assert_eq!(
            a.tasks[3].status,
            TaskStatus::Executing,
            "a still-live task is not replanned"
        );
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
            merge_review_state: MergeReviewState::Pending,
            merge_state: MergeCandidateMergeState::default(),
            created_at: None,
            ready_at: None,
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
            merge_review_state: MergeReviewState::Pending,
            merge_state: MergeCandidateMergeState::default(),
            created_at: None,
            ready_at: None,
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
            post_merge_review_fix_depth: None,
            attempts: Vec::new(),
            merge_candidates: Vec::new(),
            ..Default::default()
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
            follow_up_failure: None,
            landing_intent: None,
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
                model: "qwen3.6-35b-a3b".to_string(),
                effort: None,
            },
            review: CoderModelPair {
                coder: CoderKind::Claude,
                model: "claude-opus-4-6".to_string(),
                effort: None,
            },
        };
        let json = serde_json::to_string(&mapping).unwrap();
        assert!(!json.contains("behavior-tests"));
        let parsed: CoderMapping = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, mapping);
    }

    #[test]
    fn coder_mapping_rejects_retired_behavior_test_role() {
        let json = r#"{
            "write":{"coder":"claude","model":""},
            "review":{"coder":"claude","model":""},
            "behavior-tests":{"coder":"claude","model":""}
        }"#;
        let error = serde_json::from_str::<CoderMapping>(json).unwrap_err();
        assert!(error.to_string().contains("behavior-tests"));
    }

    #[test]
    fn attempt_without_coder_mapping_deserializes_with_default() {
        let json = r#"{"id":"a1","work_item_id":"w1","status":"planned"}"#;
        let record: Attempt = serde_json::from_str(json).unwrap();
        assert_eq!(record.coder_mapping.write.coder, CoderKind::Claude);
        assert_eq!(record.coder_mapping.review.coder, CoderKind::Claude);
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
                    model: "qwen3.6-35b-a3b".to_string(),
                    effort: None,
                },
                review: CoderModelPair {
                    coder: CoderKind::Claude,
                    model: "claude-opus-4-6".to_string(),
                    effort: None,
                },
            },
            tasks: Vec::new(),
            review_state: None,
            pause_kind: None,
            artifacts: Vec::new(),
            created_at: None,
            completed_at: None,
            ..Default::default()
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
                effort: None,
            },
            review: CoderModelPair {
                coder: CoderKind::Claude,
                model: "review-model".to_string(),
                effort: None,
            },
        };
        assert_eq!(mapping.for_task_kind(TaskKind::Write).coder, CoderKind::Pi);
        assert_eq!(
            mapping.for_task_kind(TaskKind::Review).coder,
            CoderKind::Claude
        );
        assert_eq!(mapping.for_task_kind(TaskKind::Tester).coder, CoderKind::Pi);
    }

    #[test]
    fn resolve_coder_mapping_default_when_nothing_set() {
        let inputs = CoderMappingInputs::default();
        let mapping = resolve_coder_mapping(&inputs).unwrap();
        assert_eq!(mapping.write.coder, CoderKind::Claude);
        assert_eq!(mapping.review.coder, CoderKind::Claude);
        assert!(
            mapping.write.model.is_empty(),
            "model should be empty when nothing is configured"
        );
    }

    #[test]
    fn resolve_coder_mapping_precedence_flag_env_config() {
        let config = CoderMappingInputs {
            write_model: Some("config-model".to_string()),
            review_model: Some("config-review-model".to_string()),
            ..Default::default()
        };
        let env = CoderMappingInputs {
            review_model: Some("env-review-model".to_string()),
            ..Default::default()
        };
        let inputs = config
            .merge(env)
            .merge_cli(None, None, None, None, None, None, None, None, None);
        let mapping = resolve_coder_mapping(&inputs).unwrap();
        assert_eq!(
            mapping.write.model, "config-model",
            "config model used when env and CLI are unset"
        );
        assert_eq!(
            mapping.review.model, "env-review-model",
            "env overrides config"
        );
        let config2 = CoderMappingInputs {
            write_model: Some("config-model".to_string()),
            review_model: Some("config-review-model".to_string()),
            ..Default::default()
        };
        let env2 = CoderMappingInputs {
            review_model: Some("env-review-model".to_string()),
            ..Default::default()
        };
        let inputs2 = config2.merge(env2).merge_cli(
            None,
            None,
            None,
            Some("cli-review-model".to_string()),
            None,
            None,
            None,
            None,
            None,
        );
        let mapping2 = resolve_coder_mapping(&inputs2).unwrap();
        assert_eq!(
            mapping2.review.model, "cli-review-model",
            "CLI overrides env and config"
        );
    }

    #[test]
    fn resolve_coder_mapping_fluent_coder_sets_both_roles() {
        let inputs = CoderMappingInputs {
            global_coder: Some("pi".to_string()),
            ..Default::default()
        };
        let mapping = resolve_coder_mapping(&inputs).unwrap();
        assert_eq!(mapping.write.coder, CoderKind::Pi);
        assert_eq!(mapping.review.coder, CoderKind::Pi);
    }

    #[test]
    fn resolve_coder_mapping_per_role_cli_flag_wins() {
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
    fn work_item_fix_depth_round_trips_through_storage() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = WorkModelStore::new(tmp.path());
        let mut item = WorkItem {
            id: "fix-depth-rt".to_string(),
            title: "Round-trip fix depth".to_string(),
            planning_context: None,
            instructions: None,
            abandonment: None,
            post_merge_review_fix_depth: Some(3),
            attempts: Vec::new(),
            merge_candidates: Vec::new(),
            ..Default::default()
        };
        item.add_initial_attempt("attempt-1").unwrap();
        store.create_work_item(&item).unwrap();
        let read = store.read_work_item("fix-depth-rt").unwrap();
        assert_eq!(read.post_merge_review_fix_depth, Some(3));
    }

    #[test]
    fn attempt_pause_kind_round_trips_through_storage() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = WorkModelStore::new(tmp.path());
        let mut item = WorkItem {
            id: "work-pause".to_string(),
            title: "Pause kind round-trip".to_string(),
            planning_context: None,
            instructions: None,
            abandonment: None,
            post_merge_review_fix_depth: None,
            attempts: Vec::new(),
            merge_candidates: Vec::new(),
            ..Default::default()
        };
        item.add_initial_attempt("attempt-1").unwrap();
        suspend_attempt(&mut item.attempts[0], PauseKind::Auth);
        item.attempts[0].tasks[0].status = TaskStatus::NeedsUser;
        store.create_work_item(&item).unwrap();

        let read = store.read_work_item("work-pause").unwrap();
        assert_eq!(
            read.model_writer_version.as_deref(),
            Some(WORK_MODEL_WRITER_VERSION)
        );
        assert_eq!(read.attempts[0].status, AttemptStatus::NeedsUser);
        assert_eq!(read.attempts[0].pause_kind, Some(PauseKind::Auth));
        assert!(read.attempts[0].completed_at.is_some());
    }

    #[test]
    fn host_sandbox_pause_round_trips_through_storage() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = WorkModelStore::new(tmp.path());
        let mut item = WorkItem::planned("host-sandbox-pause", "Host sandbox pause");
        item.add_initial_attempt("attempt-1").unwrap();
        suspend_attempt(&mut item.attempts[0], PauseKind::HostSandbox);
        item.attempts[0].tasks[0].status = TaskStatus::NeedsUser;
        store.create_work_item(&item).unwrap();

        let read = store.read_work_item("host-sandbox-pause").unwrap();
        assert_eq!(
            read.attempts[0].pause_kind,
            Some(PauseKind::HostSandbox),
            "the explicit host-sandbox pause survives durable storage"
        );
    }

    #[test]
    fn tester_harness_pause_round_trips_through_storage() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = WorkModelStore::new(tmp.path());
        let mut item = WorkItem::planned("tester-harness-pause", "Tester harness pause");
        item.add_initial_attempt("attempt-1").unwrap();
        suspend_attempt(&mut item.attempts[0], PauseKind::TesterHarness);
        item.attempts[0].pause_recovery = Some(PauseRecovery::TesterNoSandbox);
        item.attempts[0].tasks[0].status = TaskStatus::NeedsUser;
        store.create_work_item(&item).unwrap();

        let read = store.read_work_item("tester-harness-pause").unwrap();
        assert_eq!(read.attempts[0].pause_kind, Some(PauseKind::TesterHarness));
        assert_eq!(
            read.attempts[0].pause_recovery,
            Some(PauseRecovery::TesterNoSandbox)
        );
    }

    #[test]
    fn known_and_future_pause_kinds_preserve_their_wire_values() {
        for value in [
            "host-sandbox",
            "tester-harness",
            "writer-completion",
            "candidate-gate",
            "future-pause",
        ] {
            let parsed: PauseKind = serde_json::from_str(&format!("\"{value}\"")).unwrap();
            assert_eq!(parsed.as_str(), value);
            assert_eq!(
                serde_json::to_string(&parsed).unwrap(),
                format!("\"{value}\"")
            );
        }
        assert_eq!(
            serde_json::from_str::<PauseKind>("\"future-pause\"").unwrap(),
            PauseKind::Unknown("future-pause".to_string())
        );
    }

    #[test]
    fn candidate_gate_and_legacy_gate_pauses_are_resumable() {
        let mut attempt = Attempt {
            id: "attempt-1".to_string(),
            work_item_id: "work-1".to_string(),
            status: AttemptStatus::NeedsUser,
            pause_kind: Some(PauseKind::CandidateGate),
            review_state: Some(AttemptReviewState::NotReviewed),
            ..Attempt::default()
        };
        assert!(attempt.ensure_resume_ready().is_ok());

        attempt.pause_kind = Some(PauseKind::Uncertain);
        attempt.artifacts = vec![
            ArtifactRef {
                producer_id: "attempt-1-write-2".to_string(),
                path: ".fluent/work/artifacts/work-1/attempt-1/candidate-gates/attempt-1-write-2/candidate-gate.json".to_string(),
            },
            ArtifactRef {
                producer_id: "candidate-gate".to_string(),
                path: ".fluent/work/artifacts/work-1/attempt-1/candidate-gate-needs-user.md".to_string(),
            },
        ];
        assert!(attempt.is_candidate_gate_pause());
        assert!(attempt.ensure_resume_ready().is_ok());

        attempt.review_state = Some(AttemptReviewState::Uncertain);
        assert!(!attempt.is_candidate_gate_pause());
        assert!(attempt.ensure_resume_ready().is_err());
    }

    #[test]
    fn completed_uncertain_review_pause_is_resumable_but_active_work_is_not() {
        let mut tasks = vec![Task {
            work_item_id: "work-1".to_string(),
            ..completed_write_task("attempt-1-write-1", "initial")
        }];
        for role in crate::review::REVIEWERS {
            tasks.push(frozen_context_task(
                &format!("attempt-1-review-1-{role}"),
                TaskKind::Review,
                role,
            ));
        }
        let mut attempt = Attempt {
            id: "attempt-1".to_string(),
            work_item_id: "work-1".to_string(),
            status: AttemptStatus::NeedsUser,
            pause_kind: Some(PauseKind::Uncertain),
            review_state: Some(AttemptReviewState::Uncertain),
            tasks,
            ..Attempt::default()
        };

        assert!(attempt.ensure_resume_ready().is_ok());

        attempt.tasks.last_mut().unwrap().status = TaskStatus::Executing;
        assert!(attempt.ensure_resume_ready().is_err());
    }

    #[test]
    fn failed_candidate_gate_correction_is_narrowly_resumable() {
        let mut item = WorkItem::planned("work-1", "Retry failed gate correction");
        item.add_initial_attempt("attempt-1").unwrap();
        let first = &mut item.attempts[0].tasks[0];
        first.status = TaskStatus::Complete;
        first.output = Some(TaskOutput {
            workspace_id: "candidate".to_string(),
            workspace_path: "../candidate".to_string(),
            source_branch: "main".to_string(),
            base_commit: Some("base".to_string()),
            commit: "candidate".to_string(),
            no_change: None,
            learner_canonicalization: None,
        });
        item.add_next_write_round(
            "attempt-1",
            vec![ArtifactRef {
                producer_id: "attempt-1-write-1".to_string(),
                path: ".fluent/work/artifacts/work-1/attempt-1/candidate-gates/attempt-1-write-1/candidate-gate.json".to_string(),
            }],
        )
        .unwrap();
        let gate = item.attempts[0].tasks[1].input_artifacts[0].clone();
        item.attempts[0].artifacts.push(gate);
        let attempt = &mut item.attempts[0];
        attempt.status = AttemptStatus::Failed;
        attempt.pause_kind = None;
        attempt.review_state = Some(AttemptReviewState::Failed);
        attempt.tasks[1].status = TaskStatus::Failed;

        assert!(attempt.is_failed_candidate_gate_correction());
        assert!(attempt.is_candidate_gate_pause());
        assert!(attempt.ensure_resume_ready().is_ok());

        attempt.tasks[1].input_artifacts.clear();
        assert!(!attempt.is_failed_candidate_gate_correction());
        assert!(attempt.ensure_resume_ready().is_err());
    }

    #[test]
    fn unknown_pause_is_readable_but_mutation_fails_closed() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = WorkModelStore::new(tmp.path());
        let mut item = WorkItem::planned("future-pause-work", "Future pause");
        item.add_initial_attempt("attempt-1").unwrap();
        suspend_attempt(&mut item.attempts[0], PauseKind::HostSandbox);
        item.attempts[0].tasks[0].status = TaskStatus::NeedsUser;
        store.create_work_item(&item).unwrap();

        let attempt_path = store
            .work_attempt_path("future-pause-work", "attempt-1")
            .unwrap();
        let mut record: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&attempt_path).unwrap()).unwrap();
        record["pause_kind"] = serde_json::json!("future-pause");
        fs::write(
            &attempt_path,
            serde_json::to_string_pretty(&record).unwrap(),
        )
        .unwrap();

        let read = store.read_work_item("future-pause-work").unwrap();
        assert_eq!(
            read.attempts[0].pause_kind,
            Some(PauseKind::Unknown("future-pause".to_string()))
        );
        assert!(read.compatibility_warnings()[0].contains("future-pause"));
        let mut reducer_ran = false;
        let error = store
            .mutate_work_item("future-pause-work", |_| {
                reducer_ran = true;
                Ok(())
            })
            .unwrap_err();
        assert!(!reducer_ran);
        assert!(error.to_string().contains("upgrade Fluent before mutation"));
        assert_eq!(
            fs::read_to_string(&attempt_path).unwrap(),
            serde_json::to_string_pretty(&record).unwrap()
        );
    }

    #[test]
    fn newer_model_writer_version_warns_and_blocks_mutation() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = WorkModelStore::new(tmp.path());
        let item = WorkItem::planned("newer-writer", "Newer writer");
        store.create_work_item(&item).unwrap();
        let item_path = store.work_item_path("newer-writer").unwrap();
        let mut record: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&item_path).unwrap()).unwrap();
        record["model_writer_version"] = serde_json::json!("999.0.0");
        fs::write(&item_path, serde_json::to_string_pretty(&record).unwrap()).unwrap();

        let read = store.read_work_item("newer-writer").unwrap();
        assert_eq!(read.model_writer_version.as_deref(), Some("999.0.0"));
        assert!(read.compatibility_warnings()[0].contains("installed Fluent"));
        let error = store
            .mutate_work_item("newer-writer", |_| Ok(()))
            .unwrap_err();
        assert!(error.to_string().contains("999.0.0"));
    }

    #[test]
    fn explicit_model_upgrade_rewrites_legacy_records_and_is_idempotent() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = WorkModelStore::new(tmp.path());
        let mut item = WorkItem::planned("legacy-upgrade", "Legacy upgrade");
        item.add_initial_attempt("attempt-1").unwrap();
        store.create_work_item(&item).unwrap();

        let item_path = store.work_item_path("legacy-upgrade").unwrap();
        let attempt_path = store
            .work_attempt_path("legacy-upgrade", "attempt-1")
            .unwrap();
        let mut item_record: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&item_path).unwrap()).unwrap();
        item_record
            .as_object_mut()
            .unwrap()
            .remove("schema_version");
        item_record["model_writer_version"] = serde_json::json!("0.2.0");
        fs::write(
            &item_path,
            serde_json::to_string_pretty(&item_record).unwrap(),
        )
        .unwrap();
        let mut attempt_record: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&attempt_path).unwrap()).unwrap();
        attempt_record
            .as_object_mut()
            .unwrap()
            .remove("schema_version");
        fs::write(
            &attempt_path,
            serde_json::to_string_pretty(&attempt_record).unwrap(),
        )
        .unwrap();

        let legacy = store.read_work_item("legacy-upgrade").unwrap();
        assert_eq!(legacy.model_writer_version.as_deref(), Some("0.2.0"));
        let report = store.upgrade_work_item("legacy-upgrade").unwrap();
        assert!(!report.already_current);
        assert_eq!(report.from_model_writer_version.as_deref(), Some("0.2.0"));
        assert_eq!(report.from_record_schema_version, 0);
        assert_eq!(report.upgraded_attempts.len(), 1);
        assert!(
            report
                .audit_path
                .as_deref()
                .is_some_and(|path| tmp.path().join(path).is_file())
        );

        let upgraded = store.read_work_item("legacy-upgrade").unwrap();
        assert_eq!(upgraded.id, legacy.id);
        assert_eq!(upgraded.title, legacy.title);
        assert_eq!(upgraded.attempts, legacy.attempts);
        assert_eq!(
            upgraded.model_writer_version.as_deref(),
            Some(WORK_MODEL_WRITER_VERSION)
        );
        let persisted_item: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&item_path).unwrap()).unwrap();
        assert_eq!(
            persisted_item["schema_version"],
            serde_json::json!(WORK_MODEL_RECORD_SCHEMA_VERSION)
        );
        let persisted_attempt: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&attempt_path).unwrap()).unwrap();
        assert_eq!(
            persisted_attempt["schema_version"],
            serde_json::json!(WORK_MODEL_RECORD_SCHEMA_VERSION)
        );

        let repeated = store.upgrade_work_item("legacy-upgrade").unwrap();
        assert!(repeated.already_current);
        assert!(repeated.operation_id.is_none());
        assert!(repeated.audit_path.is_none());
        assert_eq!(store.read_work_item("legacy-upgrade").unwrap(), upgraded);
    }

    #[test]
    fn explicit_model_upgrade_uses_each_attempt_schema() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = WorkModelStore::new(tmp.path());
        let mut item = WorkItem::planned("mixed-upgrade", "Mixed schema upgrade");
        item.add_initial_attempt("attempt-1").unwrap();
        item.attempts.push(Attempt {
            id: "attempt-2".to_string(),
            work_item_id: item.id.clone(),
            status: AttemptStatus::Planned,
            recovery_operations: vec![RecoveryOperationRecord {
                schema_version: RECOVERY_OPERATION_SCHEMA_VERSION,
                operation_id: "completion-authority".to_string(),
                operation_kind: "writer-completion-no-change-approve".to_string(),
                work_item_id: item.id.clone(),
                attempt_id: "attempt-2".to_string(),
                candidate_commit: None,
                task_id: None,
                evidence_digest: None,
                workspace_path: None,
                authorization: None,
                authorization_digest: None,
                input_digest: None,
                result: RecoveryOperationResult::for_kind("writer-completion-no-change-approve"),
                next_status: AttemptStatus::Planned,
                next_pause_kind: None,
                next_action: None,
                created_at: now_iso8601(),
                completed_at: now_iso8601(),
            }],
            ..Attempt::default()
        });
        store.create_work_item(&item).unwrap();

        let attempt_path = store.work_attempt_path(&item.id, "attempt-2").unwrap();
        let mut stored: serde_json::Value =
            serde_json::from_slice(&fs::read(&attempt_path).unwrap()).unwrap();
        stored["schema_version"] = serde_json::json!(WORK_MODEL_RECORD_SCHEMA_VERSION);
        fs::write(&attempt_path, to_json_pretty(&stored).unwrap()).unwrap();

        let first = store.upgrade_work_item(&item.id).unwrap();
        assert!(!first.already_current);
        assert_eq!(first.upgraded_attempts.len(), 1);
        assert_eq!(first.upgraded_attempts[0].attempt_id, "attempt-2");
        assert_eq!(
            first.upgraded_attempts[0].to_record_schema_version,
            NO_CHANGE_COMPLETION_RECORD_SCHEMA_VERSION
        );
        let second = store.upgrade_work_item(&item.id).unwrap();
        assert!(second.already_current);
        assert!(second.operation_id.is_none());
    }

    #[test]
    fn model_upgrade_audit_survives_a_failed_write_and_retry() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = WorkModelStore::new(tmp.path());
        let item = WorkItem::planned("upgrade-retry", "Upgrade retry");
        store.create_work_item(&item).unwrap();
        let item_path = store.work_item_path("upgrade-retry").unwrap();
        let mut record: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&item_path).unwrap()).unwrap();
        record.as_object_mut().unwrap().remove("schema_version");
        record["model_writer_version"] = serde_json::json!("0.2.0");
        fs::write(&item_path, serde_json::to_string_pretty(&record).unwrap()).unwrap();

        let fault = persist_fault::arm_split_write("upgrade-retry");
        let first = store.upgrade_work_item("upgrade-retry").unwrap_err();
        drop(fault);
        assert!(
            first
                .to_string()
                .contains("injected split-record storage fault")
        );
        let audit_dir = tmp
            .path()
            .join(WORK_ARTIFACTS_DIR)
            .join("upgrade-retry")
            .join("model-upgrades");
        assert_eq!(fs::read_dir(audit_dir).unwrap().count(), 1);

        let second = store.upgrade_work_item("upgrade-retry").unwrap();
        assert!(second.already_current);
        assert_eq!(
            second.from_model_writer_version.as_deref(),
            Some(WORK_MODEL_WRITER_VERSION)
        );
        assert!(
            store
                .upgrade_work_item("upgrade-retry")
                .unwrap()
                .already_current
        );
    }

    #[test]
    fn future_model_record_schema_is_rejected_without_guessing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = WorkModelStore::new(tmp.path());
        let item = WorkItem::planned("future-record", "Future record");
        store.create_work_item(&item).unwrap();
        let item_path = store.work_item_path("future-record").unwrap();
        let mut record: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&item_path).unwrap()).unwrap();
        record["schema_version"] = serde_json::json!(99);
        fs::write(&item_path, serde_json::to_string_pretty(&record).unwrap()).unwrap();

        let error = store.read_work_item("future-record").unwrap_err();
        assert!(error.to_string().contains("newer than supported version"));
        assert!(error.to_string().contains("upgrade Fluent before reading"));
    }

    #[test]
    fn malformed_model_writer_version_is_not_upgraded_by_guessing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = WorkModelStore::new(tmp.path());
        let item = WorkItem::planned("invalid-writer", "Invalid writer");
        store.create_work_item(&item).unwrap();
        let item_path = store.work_item_path("invalid-writer").unwrap();
        let mut record: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&item_path).unwrap()).unwrap();
        record["model_writer_version"] = serde_json::json!("unknown-writer");
        fs::write(&item_path, serde_json::to_string_pretty(&record).unwrap()).unwrap();

        let error = store.upgrade_work_item("invalid-writer").unwrap_err();
        assert!(error.to_string().contains("not a valid semantic version"));
        let persisted: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&item_path).unwrap()).unwrap();
        assert_eq!(
            persisted["model_writer_version"],
            serde_json::json!("unknown-writer")
        );
    }

    #[test]
    fn reopen_attempt_resets_incomplete_tasks_and_clears_completed_at() {
        let mut item = WorkItem {
            id: "work-reopen".to_string(),
            title: "Reopen attempt".to_string(),
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
            workspace_path: "../work-1-candidate".to_string(),
            source_branch: "main".to_string(),
            base_commit: None,
            commit: "abc123".to_string(),
            no_change: None,
            learner_canonicalization: None,
        });
        item.add_review_tasks("attempt-1", &["tests"]).unwrap();
        let attempt = &mut item.attempts[0];
        // A resumable auth failure marks the Task NeedsUser, distinct from a hard
        // Failed; reopen replans exactly these.
        attempt.tasks[1].status = TaskStatus::NeedsUser;

        suspend_attempt(attempt, PauseKind::Auth);
        assert_eq!(attempt.status, AttemptStatus::NeedsUser);
        assert_eq!(attempt.pause_kind, Some(PauseKind::Auth));
        assert!(attempt.completed_at.is_some());

        reopen_attempt(attempt);
        assert_eq!(attempt.status, AttemptStatus::Planned);
        assert_eq!(attempt.pause_kind, None);
        assert!(attempt.completed_at.is_none());
        assert_eq!(
            attempt.tasks[0].status,
            TaskStatus::Complete,
            "writer stays complete"
        );
        assert_eq!(
            attempt.tasks[1].status,
            TaskStatus::Planned,
            "the pause-failed review resets to planned"
        );
    }

    #[test]
    fn suspend_attempt_records_pause_kind_and_sets_completed_at() {
        let mut item = WorkItem {
            id: "work-suspend".to_string(),
            title: "Suspend test".to_string(),
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
        assert!(attempt.completed_at.is_none());

        suspend_attempt(attempt, PauseKind::RoundCap);
        assert_eq!(attempt.status, AttemptStatus::NeedsUser);
        assert_eq!(attempt.pause_kind, Some(PauseKind::RoundCap));
        assert!(attempt.completed_at.is_some());
    }

    #[test]
    fn persisted_pause_record_carries_one_safe_operation() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = WorkModelStore::new(tmp.path());
        let mut item = WorkItem::planned("work-pause-record", "Pause record");
        item.add_initial_attempt("attempt-1").unwrap();
        item.attempts[0].tasks[0].status = TaskStatus::NeedsUser;
        suspend_attempt(&mut item.attempts[0], PauseKind::Auth);

        store.create_work_item(&item).unwrap();
        let loaded = store.read_work_item("work-pause-record").unwrap();
        let record = loaded.attempts[0].pause_record.as_ref().unwrap();

        assert_eq!(record.schema_version, PAUSE_RECORD_SCHEMA_VERSION);
        assert_eq!(record.pause_kind, PauseKind::Auth);
        assert!(record.next_operation.is_some());
        assert!(record.no_safe_operation_reason.is_none());
        assert_eq!(
            record.next_operation.as_ref().unwrap().command,
            "fluent queue resume work-pause-record --attempt attempt-1"
        );
        loaded.attempts[0].validate("work-pause-record").unwrap();
    }

    #[test]
    fn writer_completion_without_recovery_evidence_has_no_safe_operation() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = WorkModelStore::new(tmp.path());
        let mut item = WorkItem::planned("work-writer-pause", "Writer pause");
        item.add_initial_attempt("attempt-1").unwrap();
        item.attempts[0].tasks[0].status = TaskStatus::NeedsUser;
        suspend_attempt(&mut item.attempts[0], PauseKind::WriterCompletion);

        store.create_work_item(&item).unwrap();
        let loaded = store.read_work_item("work-writer-pause").unwrap();
        let record = loaded.attempts[0].pause_record.as_ref().unwrap();

        assert!(record.next_operation.is_none());
        assert!(
            record
                .no_safe_operation_reason
                .as_deref()
                .is_some_and(|reason| { reason.contains("writer-completion") })
        );
        loaded.attempts[0].validate("work-writer-pause").unwrap();
    }

    #[test]
    fn persisted_pause_record_explicitly_explains_when_no_operation_is_safe() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = WorkModelStore::new(tmp.path());
        let mut item = WorkItem::planned("work-pause-cap", "Pause cap");
        item.add_initial_attempt("attempt-1").unwrap();
        suspend_attempt(&mut item.attempts[0], PauseKind::RoundCap);

        store.create_work_item(&item).unwrap();
        let loaded = store.read_work_item("work-pause-cap").unwrap();
        let record = loaded.attempts[0].pause_record.as_ref().unwrap();

        assert!(record.next_operation.is_none());
        assert!(
            record
                .no_safe_operation_reason
                .as_deref()
                .is_some_and(|reason| reason.contains("round-cap"))
        );
    }

    #[test]
    fn reopening_an_attempt_clears_its_pause_record() {
        let mut item = WorkItem::planned("work-pause-clear", "Pause clear");
        item.add_initial_attempt("attempt-1").unwrap();
        let attempt = &mut item.attempts[0];
        suspend_attempt(attempt, PauseKind::RoundCap);
        attempt.pause_record = Some(PauseRecord::generated(
            attempt,
            None,
            Some("no operation".to_string()),
        ));

        reopen_attempt(attempt);

        assert!(attempt.pause_record.is_none());
    }

    #[test]
    fn recovery_operation_is_audited_idempotent_and_candidate_bound() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut item = work_item_with_completed_write("work-1");
        let request = RecoveryOperationRequest::new(
            "candidate-repair",
            "work-1",
            "attempt-1",
            Some("commit-initial".to_string()),
            Some("attempt-1-write-1".to_string()),
            None,
            None,
            Some("Approved repair after reviewing the saved gate.".to_string()),
            Some("sha256:input".to_string()),
        );

        assert!(
            item.validate_recovery_operation(tmp.path(), &request)
                .unwrap()
                .is_none()
        );
        let first = item.attempts[0]
            .record_recovery_operation(
                &request,
                Some("fluent queue resume work-1 --attempt attempt-1".to_string()),
            )
            .unwrap();
        let duplicate = item.attempts[0]
            .record_recovery_operation(&request, None)
            .unwrap();
        assert_eq!(first, duplicate);
        assert_eq!(item.attempts[0].recovery_operations.len(), 1);

        let replay = item
            .validate_recovery_operation(tmp.path(), &request)
            .unwrap()
            .expect("an exact repeat returns its saved result");
        assert_eq!(replay.operation_id, request.operation_id());
        assert_eq!(replay.authorization_digest, request.authorization_digest());

        let stale_candidate = RecoveryOperationRequest::new(
            "candidate-repair",
            "work-1",
            "attempt-1",
            Some("commit-no-longer-current".to_string()),
            Some("attempt-1-write-1".to_string()),
            None,
            None,
            Some("Approved repair after reviewing the saved gate.".to_string()),
            Some("sha256:input".to_string()),
        );
        let error = item
            .validate_recovery_operation(tmp.path(), &stale_candidate)
            .unwrap_err();
        assert!(matches!(
            error,
            WorkModelError::RecoveryOperationIneligible { .. }
        ));

        let round_trip: WorkItem =
            serde_json::from_str(&serde_json::to_string(&item).unwrap()).unwrap();
        assert_eq!(
            round_trip.attempts[0].recovery_operations,
            item.attempts[0].recovery_operations
        );
    }

    #[test]
    fn legacy_attempt_without_recovery_operations_remains_readable() {
        let attempt: Attempt = serde_json::from_str(
            r#"{"id":"attempt-1","work_item_id":"work-1","status":"planned"}"#,
        )
        .unwrap();
        assert!(attempt.recovery_operations.is_empty());
        assert!(
            !serde_json::to_string(&attempt)
                .unwrap()
                .contains("recovery_operations")
        );
    }

    #[test]
    fn pause_kind_omitted_from_json_when_none() {
        let mut item = WorkItem {
            id: "work-omit".to_string(),
            title: "Omit test".to_string(),
            planning_context: None,
            instructions: None,
            abandonment: None,
            post_merge_review_fix_depth: None,
            attempts: Vec::new(),
            merge_candidates: Vec::new(),
            ..Default::default()
        };
        item.add_initial_attempt("attempt-1").unwrap();

        let tmp = tempfile::TempDir::new().unwrap();
        let store = WorkModelStore::new(tmp.path());
        store.create_work_item(&item).unwrap();

        let attempt_path = store.work_attempt_path("work-omit", "attempt-1").unwrap();
        let raw = std::fs::read_to_string(attempt_path).unwrap();
        assert!(
            !raw.contains("pause_kind"),
            "pause_kind should be omitted when None: {raw}"
        );
    }

    #[test]
    fn resolve_coder_mapping_stores_resolved_model_at_creation() {
        let inputs = CoderMappingInputs {
            write_model: Some("custom-write-model".to_string()),
            review_model: Some("custom-review-model".to_string()),
            ..Default::default()
        };
        let mapping = resolve_coder_mapping(&inputs).unwrap();
        assert_eq!(mapping.write.model, "custom-write-model");
        assert_eq!(mapping.review.model, "custom-review-model");
    }

    #[test]
    fn merge_candidate_deserializes_old_review_state_key() {
        let json = r#"{
            "id": "mc-1",
            "attempt_id": "a-1",
            "source_workspace": {"id": "src", "path": "work/a"},
            "target_workspace": {"id": "tgt", "path": "."},
            "source_branch": "main",
            "target_branch": "main",
            "candidate_commit": "abc123",
            "review_state": "pending",
            "merge_state": {"status": "pending"}
        }"#;
        let candidate: MergeCandidate = serde_json::from_str(json).unwrap();
        assert_eq!(candidate.merge_review_state, MergeReviewState::Pending);
    }

    #[test]
    fn coder_model_pair_deserializes_without_effort_field() {
        let json = r#"{"coder":"claude","model":"opus-4"}"#;
        let pair: CoderModelPair = serde_json::from_str(json).unwrap();
        assert_eq!(pair.coder, CoderKind::Claude);
        assert_eq!(pair.model, "opus-4");
        assert!(pair.effort.is_none(), "effort defaults to None");
    }

    #[test]
    fn coder_model_pair_round_trips_with_effort() {
        let pair = CoderModelPair {
            coder: CoderKind::Claude,
            model: "opus-4".to_string(),
            effort: Some("high".to_string()),
        };
        let json = serde_json::to_string(&pair).unwrap();
        assert!(json.contains("\"effort\":\"high\""));
        let deserialized: CoderModelPair = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.effort, Some("high".to_string()));
    }

    #[test]
    fn coder_model_pair_omits_effort_when_none() {
        let pair = CoderModelPair {
            coder: CoderKind::Claude,
            model: "opus-4".to_string(),
            effort: None,
        };
        let json = serde_json::to_string(&pair).unwrap();
        assert!(
            !json.contains("effort"),
            "effort field should be omitted from serialized JSON"
        );
    }

    #[test]
    fn resolve_coder_mapping_threads_effort() {
        let inputs = CoderMappingInputs {
            write_effort: Some("high".to_string()),
            review_effort: Some("medium".to_string()),
            ..Default::default()
        };
        let mapping = resolve_coder_mapping(&inputs).unwrap();
        assert_eq!(mapping.write.effort.as_deref(), Some("high"));
        assert_eq!(mapping.review.effort.as_deref(), Some("medium"));
    }

    #[test]
    fn merge_overlays_set_fields_and_preserves_unset() {
        let base = CoderMappingInputs {
            write_coder: Some("claude".to_string()),
            write_model: Some("base-model".to_string()),
            review_model: Some("base-review".to_string()),
            ..Default::default()
        };
        let overlay = CoderMappingInputs {
            write_model: Some("overlay-model".to_string()),
            review_coder: Some("codex".to_string()),
            ..Default::default()
        };
        let merged = base.merge(overlay);
        assert_eq!(
            merged.write_coder.as_deref(),
            Some("claude"),
            "base field preserved when overlay is None"
        );
        assert_eq!(
            merged.write_model.as_deref(),
            Some("overlay-model"),
            "overlay wins when both set"
        );
        assert_eq!(
            merged.review_coder.as_deref(),
            Some("codex"),
            "overlay fills in when base is None"
        );
        assert_eq!(
            merged.review_model.as_deref(),
            Some("base-review"),
            "base field preserved when overlay is None"
        );
    }

    #[test]
    fn overlay_run_inputs_changes_only_explicit_fields() {
        let stored = CoderMapping {
            write: CoderModelPair {
                coder: CoderKind::Codex,
                model: "stored-write".to_string(),
                effort: Some("high".to_string()),
            },
            review: CoderModelPair {
                coder: CoderKind::Pi,
                model: "stored-review".to_string(),
                effort: Some("medium".to_string()),
            },
        };
        let inputs = CoderMappingInputs::default().merge_cli(
            Some("pi".to_string()),
            None,
            None,
            Some("review-override".to_string()),
            Some("claude".to_string()),
            Some("global-override".to_string()),
            None,
            Some("max".to_string()),
            None,
        );

        let overlaid = stored.overlay_run_inputs(&inputs).unwrap();

        assert_eq!(overlaid.write.coder, CoderKind::Pi);
        assert_eq!(overlaid.write.model, "global-override");
        assert_eq!(overlaid.write.effort.as_deref(), Some("high"));
        assert_eq!(overlaid.review.coder, CoderKind::Claude);
        assert_eq!(overlaid.review.model, "review-override");
        assert_eq!(overlaid.review.effort.as_deref(), Some("max"));
    }

    #[test]
    fn overlay_attempt_coder_mapping_preserves_fresh_untouched_fields() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = WorkModelStore::new(tmp.path());
        let mut item = WorkItem {
            id: "work-1".to_string(),
            title: "Fresh coder mapping overlay".to_string(),
            ..Default::default()
        };
        item.add_initial_attempt("attempt-1").unwrap();
        item.attempts[0].coder_mapping.write.effort = Some("before-peer".to_string());
        store.create_work_item(&item).unwrap();

        let overrides = CoderMappingInputs::default().merge_cli(
            None,
            None,
            None,
            Some("explicit-review-model".to_string()),
            None,
            None,
            None,
            None,
            None,
        );

        store
            .mutate_work_item("work-1", |fresh| {
                fresh.attempts[0].coder_mapping.write.effort =
                    Some("concurrent-writer-effort".to_string());
                Ok(())
            })
            .unwrap();

        let effective =
            overlay_attempt_coder_mapping(&store, "work-1", "attempt-1", &overrides).unwrap();

        assert_eq!(
            effective.write.effort.as_deref(),
            Some("concurrent-writer-effort"),
            "the override transaction must preserve an untouched field from fresh state"
        );
        assert_eq!(effective.review.model, "explicit-review-model");
        let persisted = store.read_work_item("work-1").unwrap();
        assert_eq!(persisted.attempts[0].coder_mapping, effective);
    }

    #[test]
    fn merge_cli_global_model_fills_unset_roles() {
        let base = CoderMappingInputs::default();
        let merged = base.merge_cli(
            None,
            Some("per-role-write".to_string()),
            None,
            None,
            None,
            Some("global-model".to_string()),
            None,
            None,
            None,
        );
        assert_eq!(
            merged.write_model.as_deref(),
            Some("per-role-write"),
            "per-role CLI model wins over global"
        );
        assert_eq!(
            merged.review_model.as_deref(),
            Some("global-model"),
            "global fills review when no per-role CLI flag"
        );
    }

    #[test]
    fn merge_cli_global_effort_fills_unset_roles() {
        let base = CoderMappingInputs::default();
        let merged = base.merge_cli(
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            Some("per-role-review".to_string()),
            Some("high".to_string()),
        );
        assert_eq!(
            merged.write_effort.as_deref(),
            Some("high"),
            "global fills write effort when no per-role CLI flag"
        );
        assert_eq!(
            merged.review_effort.as_deref(),
            Some("per-role-review"),
            "per-role CLI effort wins over global"
        );
    }

    #[test]
    #[serial_test::serial]
    fn from_env_maps_all_env_vars_to_fields() {
        let vars = [
            ("FLUENT_WRITE_CODER", "write-c"),
            ("FLUENT_WRITE_MODEL", "write-m"),
            ("FLUENT_WRITE_EFFORT", "write-e"),
            ("FLUENT_REVIEW_CODER", "review-c"),
            ("FLUENT_REVIEW_MODEL", "review-m"),
            ("FLUENT_REVIEW_EFFORT", "review-e"),
            ("FLUENT_CODER", "global-c"),
        ];
        // SAFETY: test is serialized via #[serial_test::serial]; no other
        // threads read these env vars concurrently.
        unsafe {
            for (key, val) in &vars {
                std::env::set_var(key, val);
            }
        }

        let inputs = CoderMappingInputs::from_env();

        unsafe {
            for (key, _) in &vars {
                std::env::remove_var(key);
            }
        }

        assert_eq!(inputs.write_coder.as_deref(), Some("write-c"));
        assert_eq!(inputs.write_model.as_deref(), Some("write-m"));
        assert_eq!(inputs.write_effort.as_deref(), Some("write-e"));
        assert_eq!(inputs.review_coder.as_deref(), Some("review-c"));
        assert_eq!(inputs.review_model.as_deref(), Some("review-m"));
        assert_eq!(inputs.review_effort.as_deref(), Some("review-e"));
        assert_eq!(inputs.global_coder.as_deref(), Some("global-c"));
    }

    #[test]
    #[serial_test::serial]
    fn from_env_returns_none_when_vars_unset() {
        let vars = [
            "FLUENT_WRITE_CODER",
            "FLUENT_WRITE_MODEL",
            "FLUENT_WRITE_EFFORT",
            "FLUENT_REVIEW_CODER",
            "FLUENT_REVIEW_MODEL",
            "FLUENT_REVIEW_EFFORT",
            "FLUENT_CODER",
        ];
        // SAFETY: test is serialized; no concurrent env var readers.
        unsafe {
            for key in &vars {
                std::env::remove_var(key);
            }
        }

        let inputs = CoderMappingInputs::from_env();
        assert!(inputs.write_coder.is_none());
        assert!(inputs.write_model.is_none());
        assert!(inputs.write_effort.is_none());
        assert!(inputs.review_coder.is_none());
        assert!(inputs.review_model.is_none());
        assert!(inputs.review_effort.is_none());
        assert!(inputs.global_coder.is_none());
    }

    #[test]
    fn merge_cli_per_role_effort_wins_over_global() {
        let base = CoderMappingInputs::default();
        let merged = base.merge_cli(
            None,
            None,
            None,
            None,
            None,
            None,
            Some("write-effort".to_string()),
            None,
            Some("global-effort".to_string()),
        );
        assert_eq!(
            merged.write_effort.as_deref(),
            Some("write-effort"),
            "per-role effort wins over global"
        );
        assert_eq!(
            merged.review_effort.as_deref(),
            Some("global-effort"),
            "global fills unset role"
        );
    }

    // -------------------------------------------------------------------------
    // Follow-up contracts: origin, authorization, lineage, corrective context
    // -------------------------------------------------------------------------

    fn complete_corrective_context() -> CorrectiveContext {
        CorrectiveContext {
            objective: "Restore the retry guard".to_string(),
            requirement: "Retries stop after the configured cap".to_string(),
            evidence: "Merged commit abc123 removed the cap check".to_string(),
            included_scope: "src/retry.rs".to_string(),
            excluded_scope: "unrelated backoff tuning".to_string(),
            verification: "cargo test retry::cap_is_enforced".to_string(),
        }
    }

    #[test]
    fn legacy_work_without_authorization_remains_execution_ready() {
        // Work persisted before execution-authorization state carries no
        // `authorization` field. It must deserialize as execution-ready so the
        // new model does not strand it.
        let legacy = r#"{ "id": "work-1", "title": "Legacy work" }"#;
        let record: WorkItemRecord = from_json(legacy).unwrap();
        let item = WorkItem::from(record);

        assert!(item.authorization.is_execution_ready());
        assert!(item.origin.is_planned());
        assert!(item.lineage.is_uncharged_root());
        item.ensure_execution_ready()
            .expect("legacy Work must be executable");
    }

    #[test]
    fn learner_mode_defaults_to_capture_for_legacy_work_items() {
        // Work persisted before the Learner-mode policy carries no `learner_mode`
        // field. It must resolve to the default `capture` so existing Work keeps
        // its ordinary pre-land Learner behavior.
        let legacy = r#"{ "id": "work-1", "title": "Legacy work" }"#;
        let record: WorkItemRecord = from_json(legacy).unwrap();
        let item = WorkItem::from(record);

        assert_eq!(item.learner_mode, LearnerMode::Capture);
        assert!(item.learner_mode.is_capture());
    }

    #[test]
    fn no_expertise_learner_mode_round_trips_through_split_storage() {
        // A no-expertise policy persists the exact split-record field and reads
        // back unchanged through the typed aggregate.
        let item = WorkItem {
            learner_mode: LearnerMode::NoExpertise,
            ..WorkItem::planned("work-1", "Release gate")
        };

        let json = to_json_pretty(&WorkItemRecord::from(&item)).unwrap();
        assert!(
            json.contains("\"learner_mode\": \"no-expertise\""),
            "expected the exact split-record field, got: {json}"
        );

        let record: WorkItemRecord = from_json(&json).unwrap();
        let restored = WorkItem::from(record);
        assert_eq!(restored.learner_mode, LearnerMode::NoExpertise);
    }

    #[test]
    fn corrective_context_is_valid_execution_input_without_fake_planning() {
        let provenance = DerivedProvenance {
            observation_id: Some("obs-1".to_string()),
            work_item_id: Some("root-1".to_string()),
            ..Default::default()
        };
        let item = WorkItem::derived_corrective(
            "work-fix-1",
            "Restore the retry guard",
            provenance,
            complete_corrective_context(),
            WorkLineage::descendant_of("root-1", None),
            Some(ExecutionAuthority::Automatic),
        )
        .expect("a complete corrective context is a valid execution input");

        // The corrective context stands in for planning artifacts: no brief,
        // behaviors, approach, or plan is fabricated.
        assert!(item.planning_context.is_none());
        assert!(item.instructions.is_none());
        let context = item
            .corrective_context
            .as_ref()
            .expect("corrective context is persisted");
        let instructions = item
            .write_task_instructions()
            .expect("corrective Work has execution input");
        assert_eq!(instructions, context.to_execution_context());
        assert!(instructions.contains("Restore the retry guard"));
        assert!(instructions.contains("Retries stop after the configured cap"));

        // An incomplete corrective context is rejected rather than persisted.
        let mut incomplete = complete_corrective_context();
        incomplete.verification = "   ".to_string();
        let error = WorkItem::derived_corrective(
            "work-fix-2",
            "Missing verification",
            DerivedProvenance::default(),
            incomplete,
            WorkLineage::descendant_of("root-1", None),
            Some(ExecutionAuthority::Automatic),
        )
        .unwrap_err();
        assert_eq!(
            error,
            WorkModelError::CorrectiveContextIncomplete {
                field: "verification"
            }
        );
    }

    #[test]
    fn proposed_work_rejects_attempt_creation_at_the_model() {
        let mut item = WorkItem::derived_corrective(
            "work-fix-1",
            "Restore the retry guard",
            DerivedProvenance::default(),
            complete_corrective_context(),
            WorkLineage::descendant_of("root-1", None),
            None,
        )
        .unwrap();
        assert!(item.authorization.is_proposed());

        let error = item.add_initial_attempt("attempt-1").unwrap_err();
        assert_eq!(
            error,
            WorkModelError::WorkNotExecutionReady {
                work_item_id: "work-fix-1".to_string()
            }
        );
        assert!(item.attempts.is_empty());

        // Once authorized, the same Work accepts an Attempt.
        item.authorize_execution(ExecutionAuthority::Human).unwrap();
        item.add_initial_attempt("attempt-1")
            .expect("execution-ready Work accepts an Attempt");
        assert_eq!(item.attempts.len(), 1);
    }

    #[test]
    fn planned_work_is_execution_ready_uncharged_root() {
        let item = WorkItem::planned("work-1", "Ordinary planned work");

        assert!(item.origin.is_planned());
        assert!(item.authorization.is_execution_ready());
        assert_eq!(
            item.authorization.authority(),
            Some(ExecutionAuthority::Human)
        );
        assert!(item.lineage.is_uncharged_root());
        assert!(item.corrective_context.is_none());
    }

    #[test]
    fn derived_work_round_trips_authorization_and_provenance() {
        let provenance = DerivedProvenance {
            observation_id: Some("obs-1".to_string()),
            work_item_id: Some("root-1".to_string()),
            attempt_id: Some("attempt-2".to_string()),
            merge_candidate_id: Some("candidate-1".to_string()),
            merged_commit: Some("abc123".to_string()),
        };
        let item = WorkItem::derived_corrective(
            "work-fix-1",
            "Restore the retry guard",
            provenance.clone(),
            complete_corrective_context(),
            WorkLineage::descendant_of("root-1", Some(10)),
            None,
        )
        .unwrap();

        let json = to_json_pretty(&WorkItemRecord::from(&item)).unwrap();
        let record: WorkItemRecord = from_json(&json).unwrap();
        let restored = WorkItem::from(record);

        assert_eq!(restored.origin.provenance(), Some(&provenance));
        assert!(restored.authorization.is_proposed());
        assert_eq!(restored.lineage.root_id.as_deref(), Some("root-1"));
        assert_eq!(restored.lineage.descendant_limit, Some(10));
    }

    #[test]
    fn lineage_charge_occurs_only_when_work_first_becomes_ready() {
        // A derived Work Item created proposed is not yet charged.
        let mut item = WorkItem::derived_corrective(
            "work-fix-1",
            "Restore the retry guard",
            DerivedProvenance::default(),
            complete_corrective_context(),
            WorkLineage::descendant_of("root-1", None),
            None,
        )
        .unwrap();
        assert!(item.authorization.is_proposed());
        assert!(
            !item.lineage.charged,
            "proposed Work must not charge the lineage"
        );

        // The charge happens exactly when it first becomes execution-ready.
        item.authorize_execution(ExecutionAuthority::Automatic)
            .unwrap();
        assert!(item.authorization.is_execution_ready());
        assert!(item.lineage.charged, "becoming ready charges the lineage");

        // Re-authorizing already-ready Work does not charge again.
        item.authorize_execution(ExecutionAuthority::Automatic)
            .unwrap();
        assert!(item.lineage.charged);

        // Derived Work created ready charges once at creation, not later.
        let created_ready = WorkItem::derived_corrective(
            "work-fix-2",
            "Restore the retry guard",
            DerivedProvenance::default(),
            complete_corrective_context(),
            WorkLineage::descendant_of("root-1", None),
            Some(ExecutionAuthority::Automatic),
        )
        .unwrap();
        assert!(created_ready.lineage.charged);

        // An ordinary planned root is never charged.
        let mut planned = WorkItem::planned("root-1", "Planned root");
        planned
            .authorize_execution(ExecutionAuthority::Human)
            .unwrap();
        assert!(
            !planned.lineage.charged,
            "a planned root never charges the lineage"
        );
    }

    #[test]
    fn default_lineage_limit_allows_ten_automatic_descendants() {
        let limit = crate::config::DEFAULT_DESCENDANT_LIMIT;
        assert_eq!(limit, 10);
        // Ten descendants may each be charged; the eleventh is refused.
        for already_charged in 0..limit {
            assert!(
                WorkLineage::can_authorize_descendant(already_charged, limit),
                "descendant {} of {limit} must be allowed",
                already_charged + 1
            );
        }
        assert!(!WorkLineage::can_authorize_descendant(limit, limit));
    }

    // --- Human authorization and lineage (Step 3) ---

    fn derived_descendant(id: &str, root: &str) -> WorkItem {
        WorkItem::derived_corrective(
            id,
            "Corrective descendant",
            DerivedProvenance {
                work_item_id: Some(root.to_string()),
                ..Default::default()
            },
            complete_corrective_context(),
            WorkLineage::descendant_of(root, Some(3)),
            None,
        )
        .unwrap()
    }

    #[test]
    fn human_authorization_preserves_origin_provenance() {
        let mut item = WorkItem::derived_corrective(
            "child-1",
            "Corrective descendant",
            DerivedProvenance {
                observation_id: Some("obs-1".to_string()),
                work_item_id: Some("root-1".to_string()),
                merged_commit: Some("abc123".to_string()),
                ..Default::default()
            },
            complete_corrective_context(),
            WorkLineage::descendant_of("root-1", Some(3)),
            None,
        )
        .unwrap();
        let provenance_before = item.origin.provenance().cloned();

        item.authorize_execution(ExecutionAuthority::Human).unwrap();

        // The same Work Item transitions in place: id, origin, and provenance are
        // preserved; no replacement is created.
        assert_eq!(item.id, "child-1");
        assert!(item.authorization.is_execution_ready());
        assert_eq!(
            item.authorization.authority(),
            Some(ExecutionAuthority::Human)
        );
        assert_eq!(item.origin.provenance().cloned(), provenance_before);
        assert!(
            item.lineage.charged,
            "authorization charges the lineage once"
        );
    }

    #[test]
    fn learner_and_post_merge_descendants_share_root_lineage() {
        let root = WorkItem::planned("root-1", "Root work");
        assert_eq!(root.lineage.root_id(&root.id), "root-1");

        // A learner-derived and a post-merge-derived descendant both compute the
        // same root from the originating Work Item's lineage.
        let learner_child = derived_descendant("child-learner", root.lineage.root_id(&root.id));
        let post_merge_child =
            derived_descendant("child-post-merge", root.lineage.root_id(&root.id));
        assert_eq!(learner_child.lineage.root_id.as_deref(), Some("root-1"));
        assert_eq!(post_merge_child.lineage.root_id.as_deref(), Some("root-1"));

        // A grandchild derived from a descendant still roots at the same lineage.
        let grandchild = derived_descendant(
            "grandchild",
            learner_child.lineage.root_id(&learner_child.id),
        );
        assert_eq!(grandchild.lineage.root_id.as_deref(), Some("root-1"));
    }

    /// Charge descendants across a shared lineage, refusing any promotion once
    /// the budget is spent. Models the boundary the follow-up processor enforces
    /// under its operation lock, recounting the charged descendants before each
    /// decision.
    fn run_lineage_boundary(pre_charged: &WorkItem, contenders: &mut [WorkItem], limit: u32) {
        for i in 0..contenders.len() {
            let charged = pre_charged.lineage.charged as u32
                + contenders.iter().filter(|c| c.lineage.charged).count() as u32;
            if !contenders[i].lineage.charged
                && WorkLineage::can_authorize_descendant(charged, limit)
            {
                contenders[i]
                    .authorize_execution(ExecutionAuthority::Automatic)
                    .unwrap();
            }
        }
    }

    #[test]
    fn concurrent_lineage_boundary_never_exceeds_limit() {
        let limit = 3;
        // One descendant already holds a slot; three more compete for the rest.
        let mut pre_charged = derived_descendant("d0", "root-1");
        pre_charged
            .authorize_execution(ExecutionAuthority::Automatic)
            .unwrap();
        let mut contenders = vec![
            derived_descendant("d1", "root-1"),
            derived_descendant("d2", "root-1"),
            derived_descendant("d3", "root-1"),
        ];

        run_lineage_boundary(&mut pre_charged, &mut contenders, limit);

        let charged: u32 = 1 + contenders.iter().filter(|i| i.lineage.charged).count() as u32;
        assert_eq!(charged, limit, "charging never exceeds the limit");
        // Exactly the two remaining slots are authorized; the rest stay proposed.
        assert!(contenders[0].authorization.is_execution_ready());
        assert!(contenders[1].authorization.is_execution_ready());
        assert!(contenders[2].authorization.is_proposed());
    }

    #[test]
    fn lineage_boundary_winners_are_stable_on_retry() {
        let limit = 3;
        let mut pre_charged = derived_descendant("d0", "root-1");
        pre_charged
            .authorize_execution(ExecutionAuthority::Automatic)
            .unwrap();
        let mut contenders = vec![
            derived_descendant("d1", "root-1"),
            derived_descendant("d2", "root-1"),
            derived_descendant("d3", "root-1"),
        ];

        run_lineage_boundary(&mut pre_charged, &mut contenders, limit);
        let winners: Vec<String> = contenders
            .iter()
            .filter(|i| i.lineage.charged)
            .map(|i| i.id.clone())
            .collect();

        // Re-running the boundary preserves the same winners and re-charges none.
        run_lineage_boundary(&mut pre_charged, &mut contenders, limit);
        let winners_again: Vec<String> = contenders
            .iter()
            .filter(|i| i.lineage.charged)
            .map(|i| i.id.clone())
            .collect();

        assert_eq!(winners, vec!["d1".to_string(), "d2".to_string()]);
        assert_eq!(
            winners, winners_again,
            "the recorded winners are stable on retry"
        );
        assert!(contenders[2].authorization.is_proposed());
    }

    /// A durable no-expertise aggregate whose Attempt carries a completed reviewed
    /// Write, one final-round built-in reviewer context, a Merge Candidate, and the
    /// given Learning record. Its reviewed Writer SHA is `commit-initial`.
    fn no_expertise_frozen_item(learning: AttemptLearning) -> WorkItem {
        let source = WorkspaceRef {
            id: "candidate".to_string(),
            path: "../work-6-work-1-attempt-1-initial".to_string(),
        };
        let review_task = Task {
            id: "attempt-1-review-1-tests".to_string(),
            kind: TaskKind::Review,
            status: TaskStatus::Complete,
            role: "tests".to_string(),
            instructions: None,
            work_item_id: "work-1".to_string(),
            attempt_id: Some("attempt-1".to_string()),
            workspace_access: WorkspaceAccess::read_only(vec![source.clone()]),
            artifact_area: Some(TaskArtifactArea {
                path: work_artifact_path("work-1", "attempt-1", "attempt-1-review-1-tests"),
            }),
            review_context: Some(ReviewContext {
                candidate_workspace_id: source.id.clone(),
                candidate_workspace_path: source.path.clone(),
                source_branch: "main".to_string(),
                candidate_commit: "commit-initial".to_string(),
                base_commit: None,
            }),
            writer_correction_assignment: None,
            verification_context: None,
            evidence_review_context: None,
            input_artifacts: Vec::new(),
            depends_on: None,
            output: None,
            created_at: None,
            started_at: None,
            completed_at: None,
        };
        let mut item = WorkItem {
            id: "work-1".to_string(),
            title: "Frozen no-expertise".to_string(),
            learner_mode: LearnerMode::NoExpertise,
            attempts: vec![Attempt {
                id: "attempt-1".to_string(),
                work_item_id: "work-1".to_string(),
                kind: AttemptKind::Write,
                status: AttemptStatus::Complete,
                coder_mapping: CoderMapping::default(),
                tasks: vec![
                    Task {
                        work_item_id: "work-1".to_string(),
                        ..completed_write_task("attempt-1-write-1", "initial")
                    },
                    review_task,
                ],
                review_state: Some(AttemptReviewState::Passed),
                pause_kind: None,
                artifacts: Vec::new(),
                created_at: None,
                completed_at: None,
                learning: Some(learning),
                ..Default::default()
            }],
            merge_candidates: Vec::new(),
            ..Default::default()
        };
        item.create_or_get_merge_candidate("attempt-1").unwrap();
        item
    }

    fn frozen_store(learning: AttemptLearning) -> (tempfile::TempDir, WorkModelStore) {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = WorkModelStore::new(tmp.path());
        store
            .create_work_item(&no_expertise_frozen_item(learning))
            .unwrap();
        (tmp, store)
    }

    fn is_frozen_identity_error(err: &WorkModelStorageError) -> bool {
        matches!(
            err,
            WorkModelStorageError::InvalidModel {
                source: WorkModelError::FrozenReviewedIdentity { .. },
                ..
            }
        )
    }

    #[test]
    fn no_expertise_exposed_handoff_freezes_reviewed_identity_across_supported_writes() {
        // B4s: while a no-expertise Attempt's Learning is HandoffPending or Succeeded,
        // a supported write that moves the reviewed Writer SHA, a final-round reviewer
        // context, or the no-expertise policy is rejected and the whole aggregate is
        // preserved. Learning-only transitions, unrelated fields, and any change while
        // Learning is InProgress or Failed stay allowed.
        let handoff = crate::follow_up::ArtifactRef {
            path: ".fluent/handoff.json".to_string(),
            digest: "sha256:frozen".to_string(),
        };

        for learning in [
            AttemptLearning::handoff_pending(1),
            AttemptLearning::succeeded(1, handoff.clone()),
        ] {
            // Reviewed Writer SHA moves (write output and Merge Candidate together, so
            // MergeCandidate provenance still validates): the freeze rejects it.
            let (_tmp, store) = frozen_store(learning.clone());
            let err = store
                .mutate_work_item("work-1", |item| {
                    item.attempts[0].tasks[0].output.as_mut().unwrap().commit =
                        "commit-moved".to_string();
                    item.merge_candidates[0].candidate_commit = "commit-moved".to_string();
                    Ok(())
                })
                .unwrap_err();
            assert!(is_frozen_identity_error(&err), "reviewed SHA move: {err:?}");
            let reread = store.read_work_item("work-1").unwrap();
            assert_eq!(
                reread.attempts[0].tasks[0].output.as_ref().unwrap().commit,
                "commit-initial",
                "a rejected reviewed-SHA move leaves the aggregate unchanged"
            );
            assert_eq!(
                reread.merge_candidates[0].candidate_commit,
                "commit-initial"
            );

            // Final-round reviewer context moves: rejected (validation permits it).
            let err = store
                .mutate_work_item("work-1", |item| {
                    item.attempts[0].tasks[1]
                        .review_context
                        .as_mut()
                        .unwrap()
                        .candidate_commit = "commit-other".to_string();
                    Ok(())
                })
                .unwrap_err();
            assert!(
                is_frozen_identity_error(&err),
                "reviewer context move: {err:?}"
            );

            // The no-expertise policy that protects the pointers may not change.
            let err = store
                .mutate_work_item("work-1", |item| {
                    item.learner_mode = LearnerMode::Capture;
                    Ok(())
                })
                .unwrap_err();
            assert!(is_frozen_identity_error(&err), "policy change: {err:?}");
        }

        // Learning-only transition HandoffPending -> Succeeded is allowed.
        let (_tmp, store) = frozen_store(AttemptLearning::handoff_pending(1));
        store
            .mutate_work_item("work-1", |item| {
                item.attempts[0].learning = Some(AttemptLearning::succeeded(1, handoff.clone()));
                Ok(())
            })
            .unwrap();
        assert!(
            store.read_work_item("work-1").unwrap().attempts[0]
                .learning
                .as_ref()
                .unwrap()
                .is_succeeded()
        );

        // An unrelated field update is allowed under an exposed handoff.
        store
            .mutate_work_item("work-1", |item| {
                item.title = "Retitled".to_string();
                Ok(())
            })
            .unwrap();
        assert_eq!(store.read_work_item("work-1").unwrap().title, "Retitled");

        // A pointer move while Learning is InProgress or Failed remains possible.
        for learning in [
            AttemptLearning::in_progress(1),
            AttemptLearning::failed(1, "retry me"),
        ] {
            let (_tmp, store) = frozen_store(learning);
            store
                .mutate_work_item("work-1", |item| {
                    item.attempts[0].tasks[0].output.as_mut().unwrap().commit =
                        "commit-moved".to_string();
                    item.merge_candidates[0].candidate_commit = "commit-moved".to_string();
                    Ok(())
                })
                .unwrap();
            assert_eq!(
                store.read_work_item("work-1").unwrap().attempts[0].tasks[0]
                    .output
                    .as_ref()
                    .unwrap()
                    .commit,
                "commit-moved",
                "an unexposed Learning record does not freeze the reviewed identity"
            );
        }
    }

    #[test]
    fn no_expertise_exposed_handoff_rejects_mismatched_merged_commit() {
        // B4s: a landing metadata write may persist merged_commit only as the frozen
        // reviewed Writer SHA; any other value is rejected with the aggregate intact.
        let handoff = crate::follow_up::ArtifactRef {
            path: ".fluent/handoff.json".to_string(),
            digest: "sha256:frozen".to_string(),
        };
        let (_tmp, store) = frozen_store(AttemptLearning::succeeded(1, handoff));

        let err = store
            .mutate_work_item("work-1", |item| {
                item.merge_candidates[0].merge_state.merged_commit =
                    Some("commit-other".to_string());
                Ok(())
            })
            .unwrap_err();
        assert!(
            is_frozen_identity_error(&err),
            "mismatched merged_commit: {err:?}"
        );
        assert!(
            store.read_work_item("work-1").unwrap().merge_candidates[0]
                .merge_state
                .merged_commit
                .is_none(),
            "a rejected merged_commit leaves the merge state unchanged"
        );

        // Persisting the exact frozen reviewed SHA as the merged commit is allowed.
        store
            .mutate_work_item("work-1", |item| {
                item.merge_candidates[0].merge_state.merged_commit =
                    Some("commit-initial".to_string());
                Ok(())
            })
            .unwrap();
        assert_eq!(
            store.read_work_item("work-1").unwrap().merge_candidates[0]
                .merge_state
                .merged_commit
                .as_deref(),
            Some("commit-initial")
        );
    }

    /// A completed review-context Task (Tester or built-in reviewer) naming the
    /// reviewed Writer SHA `commit-initial`, for the frozen-identity guard matrix.
    fn frozen_context_task(id: &str, kind: TaskKind, role: &str) -> Task {
        let source = WorkspaceRef {
            id: "candidate".to_string(),
            path: "../work-6-work-1-attempt-1-initial".to_string(),
        };
        Task {
            id: id.to_string(),
            kind,
            status: TaskStatus::Complete,
            role: role.to_string(),
            instructions: None,
            work_item_id: "work-1".to_string(),
            attempt_id: Some("attempt-1".to_string()),
            workspace_access: WorkspaceAccess::read_only(vec![source.clone()]),
            artifact_area: Some(TaskArtifactArea {
                path: work_artifact_path("work-1", "attempt-1", id),
            }),
            review_context: Some(ReviewContext {
                candidate_workspace_id: source.id.clone(),
                candidate_workspace_path: source.path.clone(),
                source_branch: "main".to_string(),
                candidate_commit: "commit-initial".to_string(),
                base_commit: None,
            }),
            writer_correction_assignment: None,
            verification_context: None,
            evidence_review_context: None,
            input_artifacts: Vec::new(),
            depends_on: None,
            output: None,
            created_at: None,
            started_at: None,
            completed_at: None,
        }
    }

    /// A durable no-expertise aggregate whose Attempt carries a completed reviewed
    /// Write, a final-round Tester context, one final-round context for EVERY built-in
    /// reviewer, and a Merge Candidate — the full frozen reviewed identity tuple. Its
    /// reviewed Writer SHA is `commit-initial`.
    fn no_expertise_full_frozen_item(learning: AttemptLearning) -> WorkItem {
        let mut tasks = vec![Task {
            work_item_id: "work-1".to_string(),
            ..completed_write_task("attempt-1-write-1", "initial")
        }];
        tasks.push(frozen_context_task(
            "attempt-1-tester",
            TaskKind::Tester,
            "tester",
        ));
        for role in crate::review::REVIEWERS {
            tasks.push(frozen_context_task(
                &format!("attempt-1-review-1-{role}"),
                TaskKind::Review,
                role,
            ));
        }
        let mut item = WorkItem {
            id: "work-1".to_string(),
            title: "Frozen no-expertise".to_string(),
            learner_mode: LearnerMode::NoExpertise,
            attempts: vec![Attempt {
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
                learning: Some(learning),
                ..Default::default()
            }],
            merge_candidates: Vec::new(),
            ..Default::default()
        };
        item.create_or_get_merge_candidate("attempt-1").unwrap();
        item
    }

    /// A byte-exact snapshot of every file under a directory, for proving a rejected
    /// write left the split records, storage revision, and transaction journal
    /// completely unchanged.
    fn snapshot_dir(root: &Path) -> std::collections::BTreeMap<PathBuf, Vec<u8>> {
        let mut out = std::collections::BTreeMap::new();
        fn walk(dir: &Path, out: &mut std::collections::BTreeMap<PathBuf, Vec<u8>>) {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, out);
                } else if let Ok(bytes) = std::fs::read(&path) {
                    out.insert(path, bytes);
                }
            }
        }
        walk(root, &mut out);
        out
    }

    #[test]
    fn no_expertise_frozen_identity_guard_covers_every_supported_write_and_pointer() {
        // B4af: while a no-expertise Learning record is HandoffPending or Succeeded,
        // the SAME complete frozen-identity mutation is rejected through BOTH
        // mutate_work_item and the public whole-aggregate write_work_item, preserving
        // the entire aggregate, storage revision, split-record bytes, and the absence
        // of a transaction journal. The matrix covers the no-expertise policy; the
        // latest completed Write task id and commit; the selected Merge Candidate id
        // and commit; the final Tester task id and commit context; each built-in
        // reviewer task id and commit context; and a mismatched merged_commit — in
        // both exposed states. InProgress and relaunchable Failed stay writable.
        let handoff = crate::follow_up::ArtifactRef {
            path: ".fluent/handoff.json".to_string(),
            digest: "sha256:frozen".to_string(),
        };

        // Each mutation moves exactly one element of the frozen reviewed identity.
        type Mutation = Box<dyn Fn(&mut WorkItem)>;
        let mut mutations: Vec<(String, Mutation)> = vec![
            (
                "no-expertise policy".to_string(),
                Box::new(|item: &mut WorkItem| item.learner_mode = LearnerMode::Capture),
            ),
            (
                "latest Write commit (with coupled candidate commit)".to_string(),
                Box::new(|item: &mut WorkItem| {
                    item.attempts[0].tasks[0].output.as_mut().unwrap().commit =
                        "commit-moved".to_string();
                    item.merge_candidates[0].candidate_commit = "commit-moved".to_string();
                }),
            ),
            (
                "latest Write task id".to_string(),
                Box::new(|item: &mut WorkItem| {
                    item.attempts[0].tasks[0].id = "attempt-1-write-1-moved".to_string();
                }),
            ),
            (
                "selected Merge Candidate id".to_string(),
                Box::new(|item: &mut WorkItem| {
                    item.merge_candidates[0].id = "attempt-1-merge-candidate-moved".to_string();
                }),
            ),
            (
                "mismatched merged_commit".to_string(),
                Box::new(|item: &mut WorkItem| {
                    item.merge_candidates[0].merge_state.merged_commit =
                        Some("commit-other".to_string());
                }),
            ),
            (
                "final Tester context commit".to_string(),
                Box::new(|item: &mut WorkItem| {
                    let task = item.attempts[0]
                        .tasks
                        .iter_mut()
                        .find(|task| task.kind == TaskKind::Tester)
                        .unwrap();
                    task.review_context.as_mut().unwrap().candidate_commit =
                        "commit-other".to_string();
                }),
            ),
            (
                "final Tester task id".to_string(),
                Box::new(|item: &mut WorkItem| {
                    let task = item.attempts[0]
                        .tasks
                        .iter_mut()
                        .find(|task| task.kind == TaskKind::Tester)
                        .unwrap();
                    task.id = "attempt-1-tester-renamed".to_string();
                }),
            ),
        ];
        for role in crate::review::REVIEWERS {
            let role_ctx = (*role).to_string();
            mutations.push((
                format!("reviewer {role} context commit"),
                Box::new(move |item: &mut WorkItem| {
                    let task = item.attempts[0]
                        .tasks
                        .iter_mut()
                        .find(|task| task.role == role_ctx && task.kind == TaskKind::Review)
                        .unwrap();
                    task.review_context.as_mut().unwrap().candidate_commit =
                        "commit-other".to_string();
                }),
            ));
            let role_id = (*role).to_string();
            mutations.push((
                format!("reviewer {role} task id"),
                Box::new(move |item: &mut WorkItem| {
                    let task = item.attempts[0]
                        .tasks
                        .iter_mut()
                        .find(|task| task.role == role_id && task.kind == TaskKind::Review)
                        .unwrap();
                    task.id = format!("attempt-1-review-1-{role_id}-renamed");
                }),
            ));
        }

        for learning in [
            AttemptLearning::handoff_pending(1),
            AttemptLearning::succeeded(1, handoff.clone()),
        ] {
            for (label, mutate) in &mutations {
                // Path 1: mutate_work_item (reducer-based).
                let tmp = tempfile::TempDir::new().unwrap();
                let store = WorkModelStore::new(tmp.path());
                store
                    .create_work_item(&no_expertise_full_frozen_item(learning.clone()))
                    .unwrap();
                let before = snapshot_dir(tmp.path());
                let err = store
                    .mutate_work_item("work-1", |item| {
                        mutate(item);
                        Ok(())
                    })
                    .unwrap_err();
                assert!(
                    is_frozen_identity_error(&err),
                    "[mutate_work_item] {label}: {err:?}"
                );
                assert_eq!(
                    snapshot_dir(tmp.path()),
                    before,
                    "[mutate_work_item] {label}: a rejected write leaves every split record, the \
                     storage revision, and the transaction journal byte-for-byte unchanged"
                );
                assert!(
                    !store.work_transaction_path("work-1").unwrap().exists(),
                    "[mutate_work_item] {label}: a rejected write authors no transaction journal"
                );

                // Path 2: public whole-aggregate write_work_item (imperative).
                let tmp = tempfile::TempDir::new().unwrap();
                let store = WorkModelStore::new(tmp.path());
                store
                    .create_work_item(&no_expertise_full_frozen_item(learning.clone()))
                    .unwrap();
                let before = snapshot_dir(tmp.path());
                let mut proposed = store.read_work_item("work-1").unwrap();
                mutate(&mut proposed);
                let err = store.write_work_item(&proposed).unwrap_err();
                assert!(
                    is_frozen_identity_error(&err),
                    "[write_work_item] {label}: {err:?}"
                );
                assert_eq!(
                    snapshot_dir(tmp.path()),
                    before,
                    "[write_work_item] {label}: a rejected whole-aggregate write leaves every split \
                     record, the storage revision, and the transaction journal unchanged"
                );
                assert!(
                    !store.work_transaction_path("work-1").unwrap().exists(),
                    "[write_work_item] {label}: a rejected write authors no transaction journal"
                );
            }
        }

        // The exposed states still allow a Learning-only transition and unrelated
        // fields (proved for HandoffPending -> Succeeded and a retitle).
        let (_tmp, store) = frozen_store(AttemptLearning::handoff_pending(1));
        store
            .mutate_work_item("work-1", |item| {
                item.attempts[0].learning = Some(AttemptLearning::succeeded(1, handoff.clone()));
                Ok(())
            })
            .unwrap();
        store
            .mutate_work_item("work-1", |item| {
                item.title = "Retitled".to_string();
                Ok(())
            })
            .unwrap();

        // InProgress and relaunchable Failed keep the reviewed identity writable
        // through both write paths.
        for learning in [
            AttemptLearning::in_progress(1),
            AttemptLearning::failed(1, "retry me"),
        ] {
            let tmp = tempfile::TempDir::new().unwrap();
            let store = WorkModelStore::new(tmp.path());
            store
                .create_work_item(&no_expertise_full_frozen_item(learning.clone()))
                .unwrap();
            store
                .mutate_work_item("work-1", |item| {
                    item.attempts[0].tasks[0].output.as_mut().unwrap().commit =
                        "commit-moved".to_string();
                    item.merge_candidates[0].candidate_commit = "commit-moved".to_string();
                    Ok(())
                })
                .unwrap();
            assert_eq!(
                store.read_work_item("work-1").unwrap().attempts[0].tasks[0]
                    .output
                    .as_ref()
                    .unwrap()
                    .commit,
                "commit-moved",
                "an unexposed Learning record does not freeze the reviewed identity"
            );

            let mut proposed = store.read_work_item("work-1").unwrap();
            proposed.title = "Writable while unexposed".to_string();
            store.write_work_item(&proposed).unwrap();
            assert_eq!(
                store.read_work_item("work-1").unwrap().title,
                "Writable while unexposed"
            );
        }
    }

    #[test]
    fn no_expertise_unexposed_identity_repair_uses_both_write_apis() {
        // B4ap: while a no-expertise Learning record is InProgress or relaunchable
        // Failed, a valid coupled repair of the reviewed-identity tuple persists through
        // BOTH mutate_work_item and the public whole-aggregate write_work_item, while
        // Learning and every unrelated field survive exactly. Every cell of
        // {InProgress, Failed} × {mutate_work_item, write_work_item} uses an independent
        // fresh store, so no reducer-mutated state leaks into the whole-aggregate API.
        fn repair(item: &mut WorkItem) {
            // Move the reviewed Write commit and every coupled pointer together to one
            // new, internally consistent reviewed SHA.
            item.attempts[0].tasks[0].output.as_mut().unwrap().commit =
                "commit-repaired".to_string();
            item.merge_candidates[0].candidate_commit = "commit-repaired".to_string();
            for task in item.attempts[0].tasks.iter_mut() {
                if let Some(ctx) = task.review_context.as_mut() {
                    ctx.candidate_commit = "commit-repaired".to_string();
                }
            }
        }

        for learning in [
            AttemptLearning::in_progress(1),
            AttemptLearning::failed(1, "retry me"),
        ] {
            // Cell 1: mutate_work_item (reducer-based) on a fresh store. The expected
            // aggregate is the created baseline with only the identity repair applied.
            let tmp = tempfile::TempDir::new().unwrap();
            let store = WorkModelStore::new(tmp.path());
            store
                .create_work_item(&no_expertise_full_frozen_item(learning.clone()))
                .unwrap();
            let mut expected = store.read_work_item("work-1").unwrap();
            repair(&mut expected);
            store
                .mutate_work_item("work-1", |item| {
                    repair(item);
                    Ok(())
                })
                .unwrap();
            let after = store.read_work_item("work-1").unwrap();
            assert_eq!(
                after.attempts[0].learning,
                Some(learning.clone()),
                "[mutate_work_item] the repair preserves the unexposed Learning record"
            );
            let transitions = after.transitions.clone();
            let mut business_state = after;
            business_state.transitions.clear();
            assert_eq!(
                business_state, expected,
                "[mutate_work_item] a valid identity repair persists with only the identity \
                 fields changed and every unrelated field preserved"
            );
            assert_eq!(transitions.len(), durable_object_states(&expected).len());
            assert!(transitions.iter().all(|transition| {
                transition.old_state.is_none()
                    && transition.origin == DurableTransitionOrigin::Unknown
            }));

            // Cell 2: public whole-aggregate write_work_item on an INDEPENDENT fresh
            // store — never the reducer-mutated store above.
            let tmp = tempfile::TempDir::new().unwrap();
            let store = WorkModelStore::new(tmp.path());
            store
                .create_work_item(&no_expertise_full_frozen_item(learning.clone()))
                .unwrap();
            let mut expected = store.read_work_item("work-1").unwrap();
            repair(&mut expected);
            store.write_work_item(&expected).unwrap();
            let after = store.read_work_item("work-1").unwrap();
            assert_eq!(
                after.attempts[0].learning,
                Some(learning.clone()),
                "[write_work_item] the repair preserves the unexposed Learning record"
            );
            let transitions = after.transitions.clone();
            let mut business_state = after;
            business_state.transitions.clear();
            assert_eq!(
                business_state, expected,
                "[write_work_item] a valid identity repair persists with only the identity \
                 fields changed and every unrelated field preserved"
            );
            assert_eq!(transitions.len(), durable_object_states(&expected).len());
            assert!(transitions.iter().all(|transition| {
                transition.old_state.is_none()
                    && transition.origin == DurableTransitionOrigin::Unknown
            }));
        }
    }

    fn round_cap_request(additional_write_rounds: usize) -> RoundCapExtensionRequest {
        RoundCapExtensionRequest {
            old_cap: 10,
            additional_write_rounds,
            candidate_commit: "candidate-commit".to_string(),
            failed_reviews: vec![RoundCapReviewArtifact {
                artifact: ArtifactRef {
                    producer_id: "attempt-1-review-tests".to_string(),
                    path: ".fluent/work/artifacts/work-1/attempt-1/review/review.md".to_string(),
                },
                digest: "sha256:approved".to_string(),
            }],
            human_resolution: "Preserve the public contract and fix the boundary.".to_string(),
        }
    }

    #[test]
    fn round_cap_extension_is_bounded_idempotent_and_digest_bound() {
        let mut attempt = Attempt::default();
        let first = attempt
            .record_round_cap_extension(10, round_cap_request(2))
            .unwrap()
            .clone();
        let duplicate = attempt
            .record_round_cap_extension(10, round_cap_request(2))
            .unwrap()
            .clone();
        assert_eq!(first, duplicate);
        assert_eq!(attempt.round_cap_extensions.len(), 1);
        assert_eq!(first.authorized_base_cap, Some(10));
        assert_eq!(first.pre_extension_write_rounds, Some(0));
        assert_eq!(attempt.effective_write_round_cap(10), 12);
        assert_eq!(first.failed_reviews[0].digest, "sha256:approved");
        assert_eq!(
            first.human_resolution,
            "Preserve the public contract and fix the boundary."
        );
        assert_eq!(
            first.human_resolution_digest,
            round_cap_resolution_digest(&first.human_resolution)
        );
        assert!(
            !serde_json::to_string(&first)
                .unwrap()
                .contains("evidence_write_rounds")
        );

        let error = Attempt::default()
            .record_round_cap_extension(10, round_cap_request(4))
            .unwrap_err();
        assert!(matches!(
            error,
            WorkModelError::RoundCapExtensionIncrementOutOfRange { requested: 4 }
        ));

        let mut empty = round_cap_request(1);
        empty.human_resolution = "  \n".to_string();
        let error = Attempt::default()
            .record_round_cap_extension(10, empty)
            .unwrap_err();
        assert!(matches!(
            error,
            WorkModelError::RoundCapResolutionInvalid { .. }
        ));

        let mut altered = first;
        altered.human_resolution.push_str(" Changed.");
        let error = altered.validate_human_resolution().unwrap_err();
        assert!(error.to_string().contains("no longer matches"));
    }

    #[test]
    fn materialized_writer_overrun_becomes_the_extension_floor() {
        let mut attempt = Attempt {
            tasks: (0..11).map(|_| task(TaskKind::Write, Vec::new())).collect(),
            ..Attempt::default()
        };

        assert_eq!(attempt.effective_write_round_cap(10), 11);
        let mut request = round_cap_request(3);
        request.old_cap = 11;
        let extension = attempt
            .record_round_cap_extension(10, request)
            .unwrap()
            .clone();

        assert_eq!(extension.old_cap, 11);
        assert_eq!(extension.new_cap, 14);
        assert_eq!(attempt.effective_write_round_cap(10), 14);
    }

    #[test]
    fn evidence_code_changes_advance_the_effective_write_cap() {
        let recovery = |id: &str, state: EvidenceRecoveryState| EvidenceRecovery {
            carried_finding: None,
            required_check: None,
            id: id.to_string(),
            candidate_commit: format!("candidate-{id}"),
            attachment: EvidenceAttachment {
                snapshot_path: format!(".fluent/work/artifacts/work-1/attempt-1/{id}.json"),
                digest: format!("sha256:{id}"),
            },
            targets: vec![EvidenceReviewTarget {
                role: "tests".to_string(),
                prior_review_artifact: format!(
                    ".fluent/work/artifacts/work-1/attempt-1/{id}/review.md"
                ),
                review_task_id: Some(format!("attempt-1-{id}-tests")),
                review_digest: None,
            }],
            state,
            created_at: "2026-08-11T00:00:00Z".to_string(),
            code_change_classification: None,
        };
        let mut attempt = Attempt {
            evidence_recoveries: vec![
                recovery("evidence-1", EvidenceRecoveryState::CodeChange),
                recovery("evidence-2", EvidenceRecoveryState::CodeChange),
                recovery("evidence-3", EvidenceRecoveryState::Complete),
                recovery("evidence-4", EvidenceRecoveryState::CodeChange),
            ],
            tasks: ["evidence-1", "evidence-2"]
                .into_iter()
                .map(|id| {
                    let mut writer = task(TaskKind::Write, Vec::new());
                    writer.input_artifacts = vec![ArtifactRef {
                        producer_id: format!("attempt-1-{id}-tests"),
                        path: format!(".fluent/work/artifacts/work-1/attempt-1/{id}/review.md"),
                    }];
                    writer
                })
                .collect(),
            ..Attempt::default()
        };

        assert_eq!(attempt.effective_write_round_cap(10), 12);
        let mut request = round_cap_request(1);
        request.old_cap = 12;
        let extension = attempt
            .record_round_cap_extension(10, request)
            .unwrap()
            .clone();

        assert_eq!(extension.old_cap, 12);
        assert_eq!(extension.new_cap, 13);
        assert_eq!(extension.evidence_write_rounds, 2);
        assert_eq!(
            serde_json::to_value(&extension).unwrap()["evidence_write_rounds"],
            2
        );
        assert_eq!(attempt.effective_write_round_cap(10), 13);
        assert_eq!(attempt.effective_write_round_cap(99), 13);

        attempt.evidence_recoveries[2].state = EvidenceRecoveryState::CodeChange;
        let mut writer = task(TaskKind::Write, Vec::new());
        writer.input_artifacts = vec![ArtifactRef {
            producer_id: "attempt-1-evidence-3-tests".to_string(),
            path: ".fluent/work/artifacts/work-1/attempt-1/evidence-3/review.md".to_string(),
        }];
        attempt.tasks.push(writer);
        assert_eq!(attempt.effective_write_round_cap(99), 14);
    }

    #[test]
    fn round_cap_extension_chain_uses_recorded_evidence_counts() {
        let recovery = |id: &str| EvidenceRecovery {
            carried_finding: None,
            required_check: None,
            id: id.to_string(),
            candidate_commit: format!("candidate-{id}"),
            attachment: EvidenceAttachment {
                snapshot_path: format!(".fluent/work/artifacts/work-1/attempt-1/{id}.json"),
                digest: format!("sha256:{id}"),
            },
            targets: vec![EvidenceReviewTarget {
                role: "tests".to_string(),
                prior_review_artifact: format!(
                    ".fluent/work/artifacts/work-1/attempt-1/{id}/review.md"
                ),
                review_task_id: Some(format!("attempt-1-{id}-tests")),
                review_digest: None,
            }],
            state: EvidenceRecoveryState::CodeChange,
            created_at: "2026-08-11T00:00:00Z".to_string(),
            code_change_classification: None,
        };
        let mut tasks = Vec::new();
        for id in ["evidence-1", "evidence-2"] {
            let mut writer = task(TaskKind::Write, Vec::new());
            writer.input_artifacts = vec![ArtifactRef {
                producer_id: format!("attempt-1-{id}-tests"),
                path: format!(".fluent/work/artifacts/work-1/attempt-1/{id}/review.md"),
            }];
            tasks.push(writer);
        }
        let mut attempt = Attempt {
            evidence_recoveries: vec![recovery("evidence-1"), recovery("evidence-2")],
            tasks,
            round_cap_extensions: vec![
                RoundCapExtension {
                    id: "round-cap-10-11".to_string(),
                    old_cap: 10,
                    new_cap: 11,
                    additional_write_rounds: 1,
                    evidence_write_rounds: 0,
                    authorized_base_cap: None,
                    pre_extension_write_rounds: None,
                    candidate_commit: "candidate-commit".to_string(),
                    failed_reviews: Vec::new(),
                    human_resolution: String::new(),
                    human_resolution_digest: String::new(),
                    approved_at: "2026-08-11T00:00:00Z".to_string(),
                },
                RoundCapExtension {
                    id: "round-cap-12-13".to_string(),
                    old_cap: 12,
                    new_cap: 13,
                    additional_write_rounds: 1,
                    evidence_write_rounds: 1,
                    authorized_base_cap: None,
                    pre_extension_write_rounds: None,
                    candidate_commit: "candidate-commit".to_string(),
                    failed_reviews: Vec::new(),
                    human_resolution: String::new(),
                    human_resolution_digest: String::new(),
                    approved_at: "2026-08-12T00:00:00Z".to_string(),
                },
            ],
            ..Attempt::default()
        };

        assert_eq!(attempt.evidence_write_rounds(), 2);
        attempt
            .validate_round_cap_extensions()
            .expect("later evidence must not rewrite historical extension continuity");
    }

    fn historical_round_cap_attempt(
        previous_cap: usize,
        evidence: usize,
        writers: usize,
        new_cap: usize,
    ) -> Attempt {
        let start = chrono::DateTime::parse_from_rfc3339("2026-10-01T00:00:00Z").unwrap();
        let mut attempt = Attempt::default();
        // Materialize independent Task history, including a later Writer which
        // must never contribute to the earlier approval's count.
        for index in 1..=writers + 1 {
            let mut writer = task(TaskKind::Write, Vec::new());
            writer.id = format!("attempt-1-write-{index}");
            writer.created_at =
                Some((start + chrono::Duration::seconds(index as i64 * 2)).to_rfc3339());
            writer.completed_at =
                Some((start + chrono::Duration::seconds(index as i64 * 2 + 1)).to_rfc3339());
            writer.status = TaskStatus::Complete;
            writer.output = Some(TaskOutput {
                workspace_id: "candidate".to_string(),
                workspace_path: "/workspaces/candidate".to_string(),
                source_branch: "main".to_string(),
                base_commit: None,
                commit: format!("candidate-{index}"),
                no_change: None,
                learner_canonicalization: None,
            });
            if index <= evidence {
                let review_id = format!("evidence-{index}-tests");
                let review_path =
                    format!(".fluent/work/artifacts/work-1/attempt-1/{review_id}/review.md");
                writer.input_artifacts.push(ArtifactRef {
                    producer_id: review_id.clone(),
                    path: review_path.clone(),
                });
                attempt.evidence_recoveries.push(EvidenceRecovery {
                    id: format!("evidence-{index}"),
                    candidate_commit: format!("candidate-{index}"),
                    attachment: EvidenceAttachment {
                        snapshot_path: format!("evidence-{index}.json"),
                        digest: "sha256:evidence".to_string(),
                    },
                    targets: vec![EvidenceReviewTarget {
                        role: "tests".to_string(),
                        prior_review_artifact: review_path,
                        review_task_id: Some(review_id),
                        review_digest: None,
                    }],
                    state: EvidenceRecoveryState::CodeChange,
                    created_at: writer.created_at.clone().unwrap(),
                    carried_finding: None,
                    required_check: None,
                    code_change_classification: None,
                });
            }
            attempt.tasks.push(writer);
        }
        let extension =
            |old_cap, new_cap, evidence_write_rounds, candidate, approved_at| RoundCapExtension {
                id: format!("round-cap-{old_cap}-{new_cap}"),
                old_cap,
                new_cap,
                additional_write_rounds: new_cap - old_cap,
                evidence_write_rounds,
                authorized_base_cap: None,
                pre_extension_write_rounds: None,
                candidate_commit: candidate,
                failed_reviews: Vec::new(),
                human_resolution: String::new(),
                human_resolution_digest: String::new(),
                approved_at,
            };
        attempt.round_cap_extensions = vec![
            extension(
                previous_cap - 1,
                previous_cap,
                0,
                "candidate-1".to_string(),
                (start + chrono::Duration::seconds(3)).to_rfc3339(),
            ),
            extension(
                writers,
                new_cap,
                evidence,
                format!("candidate-{writers}"),
                (start + chrono::Duration::seconds(writers as i64 * 2 + 1)).to_rfc3339(),
            ),
        ];
        attempt
    }

    #[test]
    fn round_cap_chain_uses_historical_writer_count() {
        for (previous_cap, evidence, writers, new_cap) in [
            (16, 0, 18, 19),
            (25, 0, 26, 27),
            (11, 0, 12, 15),
            (12, 0, 13, 16),
            (16, 0, 17, 19),
            (12, 8, 22, 25),
            (44, 0, 45, 47),
            (48, 0, 49, 51),
            (52, 0, 53, 54),
            (54, 0, 55, 56),
            (56, 3, 67, 68),
            (72, 21, 94, 95),
            (15, 1, 17, 18),
            (27, 0, 28, 29),
            (38, 0, 39, 40),
            (45, 2, 48, 49),
            (50, 0, 51, 52),
            (14, 0, 15, 16),
        ] {
            let attempt = historical_round_cap_attempt(previous_cap, evidence, writers, new_cap);
            attempt
                .validate_round_cap_extensions()
                .unwrap_or_else(|error| {
                    panic!("{previous_cap}/{evidence}/{writers}/{new_cap}: {error}")
                });
            // Equivalent offsets describe the same ordering, even when their
            // textual date or clock time sorts differently.
            let mut offset = attempt.clone();
            offset.tasks[writers - 1].created_at = Some(
                (chrono::DateTime::parse_from_rfc3339(
                    offset.tasks[writers - 1].created_at.as_ref().unwrap(),
                )
                .unwrap())
                .with_timezone(&chrono::FixedOffset::west_opt(7 * 3600).unwrap())
                .to_rfc3339(),
            );
            offset.validate_round_cap_extensions().unwrap();
        }
    }

    #[test]
    fn round_cap_chain_rejects_unproven_writer_count() {
        let original = historical_round_cap_attempt(2, 0, 3, 4);
        for case in [
            "later",
            "at-approval",
            "missing-time",
            "invalid-time",
            "invalid-approval",
            "tied",
            "reordered",
            "candidate",
            "incomplete",
            "missing-output",
            "missing-completion",
            "late-completion",
            "early-completion",
            "partial-base",
            "partial-count",
            "too-few",
            "too-many",
        ] {
            let mut attempt = original.clone();
            let approval = attempt.round_cap_extensions[1].approved_at.clone();
            match case {
                "later" => attempt.tasks[2].created_at = Some("2026-10-01T00:00:07.5Z".to_string()),
                "at-approval" => attempt.tasks[2].created_at = Some(approval),
                "missing-time" => attempt.tasks[0].created_at = None,
                "invalid-time" => attempt.tasks[0].created_at = Some("yesterday".to_string()),
                "invalid-approval" => {
                    attempt.round_cap_extensions[1].approved_at = "yesterday".to_string()
                }
                "tied" => attempt.tasks[2].created_at = attempt.tasks[1].created_at.clone(),
                "reordered" => attempt.tasks.swap(1, 2),
                "candidate" => {
                    attempt.tasks[2].output.as_mut().unwrap().commit = "altered".to_string()
                }
                "incomplete" => attempt.tasks[2].status = TaskStatus::Planned,
                "missing-output" => attempt.tasks[2].output = None,
                "missing-completion" => attempt.tasks[2].completed_at = None,
                "late-completion" => {
                    attempt.tasks[2].completed_at = Some("2026-10-01T00:00:08Z".to_string())
                }
                "early-completion" => {
                    attempt.tasks[2].completed_at = Some("2026-10-01T00:00:05Z".to_string())
                }
                "partial-base" => attempt.round_cap_extensions[1].authorized_base_cap = Some(1),
                "partial-count" => {
                    attempt.round_cap_extensions[1].pre_extension_write_rounds = Some(3)
                }
                "too-few" => {
                    attempt.round_cap_extensions[1].authorized_base_cap = Some(1);
                    attempt.round_cap_extensions[1].pre_extension_write_rounds = Some(2);
                }
                "too-many" => {
                    attempt.round_cap_extensions[1].authorized_base_cap = Some(1);
                    attempt.round_cap_extensions[1].pre_extension_write_rounds = Some(5);
                }
                _ => unreachable!(),
            }
            let error = attempt
                .validate_round_cap_extensions()
                .expect_err(case)
                .to_string();
            assert!(error.contains("round-cap-3-4"), "{case}: {error}");
        }
    }

    #[test]
    fn round_cap_chain_preserves_contiguous_legacy_entries() {
        let mut attempt = historical_round_cap_attempt(3, 0, 3, 4);
        for writer in &mut attempt.tasks {
            writer.created_at = None;
            writer.completed_at = None;
        }
        attempt.validate_round_cap_extensions().unwrap();
        assert!(
            attempt
                .validate_round_cap_extensions_for_approach_revision()
                .unwrap_err()
                .to_string()
                .contains("no independently bound")
        );
    }

    #[test]
    fn round_cap_extension_after_writer_overrun_round_trips() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = WorkModelStore::new(tmp.path());
        let mut item = WorkItem::planned("work-1", "Current round caps");
        item.add_initial_attempt("attempt-1").unwrap();
        store.create_work_item(&item).unwrap();
        // Each stage has more materialized Writers than the preceding cap plus
        // its new evidence allowances. Use the actual extension writer each time.
        for (writers, evidence, new_cap) in [(3, 0, 4), (6, 1, 7), (9, 2, 10)] {
            let history = historical_round_cap_attempt(2, evidence, writers, new_cap);
            let attempt = &mut item.attempts[0];
            attempt.tasks = history.tasks[..writers].to_vec();
            attempt.evidence_recoveries = history.evidence_recoveries;
            let mut request = round_cap_request(1);
            request.old_cap = writers;
            request.candidate_commit = format!("candidate-{writers}");
            let extension = attempt.record_round_cap_extension(2, request).unwrap();
            assert_eq!(extension.new_cap, new_cap);
            assert_eq!(extension.pre_extension_write_rounds, Some(writers));
            assert_eq!(extension.evidence_write_rounds, evidence);
            let expected = attempt.clone();
            store.write_work_item(&item).unwrap();
            item = store.read_work_item("work-1").unwrap();
            assert_eq!(item.attempts[0], expected);
            assert_eq!(item.attempts[0].effective_write_round_cap(99), new_cap);
        }
    }

    #[test]
    fn legacy_attempt_omits_empty_round_cap_extensions() {
        let attempt: Attempt = serde_json::from_str(
            r#"{"id":"attempt-1","work_item_id":"work-1","status":"planned"}"#,
        )
        .unwrap();
        assert!(attempt.round_cap_extensions.is_empty());
        assert!(
            !serde_json::to_string(&attempt)
                .unwrap()
                .contains("round_cap_extensions")
        );
    }

    #[test]
    fn legacy_round_cap_extension_has_no_human_resolution() {
        let extension: RoundCapExtension = serde_json::from_str(
            r#"{"id":"round-cap-1-2","old_cap":1,"new_cap":2,"additional_write_rounds":1,"candidate_commit":"abc123","failed_reviews":[],"approved_at":"2026-08-18T00:00:00Z"}"#,
        )
        .unwrap();

        assert!(extension.human_resolution.is_empty());
        assert!(extension.human_resolution_digest.is_empty());
        extension.validate_human_resolution().unwrap();
    }
}

pub(crate) fn publish_recovery_snapshot(
    project_root: &Path,
    relative: &str,
    bytes: &[u8],
) -> anyhow::Result<()> {
    use anyhow::Context as _;
    use std::io::Write as _;
    let relative_path = Path::new(relative);
    let managed_root = Path::new(".fluent/work/artifacts");
    if relative_path.is_absolute() || !relative_path.starts_with(managed_root) {
        anyhow::bail!("evidence snapshot path must stay under the managed artifact root");
    }
    let mut current = project_root.to_path_buf();
    for component in relative_path.components() {
        let Component::Normal(part) = component else {
            anyhow::bail!("evidence snapshot path must use normal managed components");
        };
        current.push(part);
        if current == project_root.join(relative_path) {
            break;
        }
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                anyhow::bail!("evidence snapshot path must not traverse a symlink")
            }
            Ok(metadata) if !metadata.is_dir() => {
                anyhow::bail!("evidence snapshot parent must be a directory")
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir(&current).with_context(|| {
                    format!("create managed evidence directory {}", current.display())
                })?;
            }
            Err(error) => return Err(error.into()),
        }
    }
    let path = project_root.join(relative_path);
    match tempfile::NamedTempFile::new_in(path.parent().expect("snapshot has a parent")) {
        Ok(mut file) => {
            file.write_all(bytes)?;
            file.as_file().sync_all()?;
            match file.persist_noclobber(&path) {
                Ok(_) => {}
                Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
                    if fs::symlink_metadata(&path)?.file_type().is_symlink() {
                        anyhow::bail!("evidence snapshot path must not be a symlink");
                    }
                    if fs::read(&path)? != bytes {
                        anyhow::bail!("evidence snapshot digest collision at {}", path.display());
                    }
                }
                Err(error) => return Err(error.error.into()),
            }
        }
        Err(error) => return Err(error.into()),
    }
    // The Work-model write that follows may survive a power loss, so make the
    // snapshot directory entry durable before that model can reference it.
    fs::File::open(path.parent().expect("snapshot has a parent"))?.sync_all()?;
    Ok(())
}

/// A single passing test identity observed by the host rerun.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeTestObservation {
    pub id: String,
    pub harness: String,
}

/// Version-one operator evidence document. The parser below is the single
/// validation boundary for host-evidence attachment and source correction.
/// Callers retain the exact serialized bytes separately as the audit artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceDocument {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub environment: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub comparison_commits: BTreeMap<String, String>,
    pub schema_version: u32,
    pub producer: String,
    pub check: String,
    pub working_directory: String,
    pub result: EvidenceResult,
    pub run_at: String,
    pub output: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub native_observations: Vec<NativeTestObservation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EvidenceResult {
    Pass,
    Fail,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BoundRecheckArtifact {
    pub(crate) path: String,
    pub(crate) digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub(crate) struct PlanHostEvidenceBinding {
    #[serde(
        default,
        skip_serializing_if = "ResolvedCheckApplicability::is_default"
    )]
    pub(crate) applicability: ResolvedCheckApplicability,
    pub(crate) schema_version: u32,
    pub(crate) work_item_id: String,
    pub(crate) attempt_id: String,
    pub(crate) writer_task_id: String,
    pub(crate) candidate_commit: String,
    pub(crate) plan_row: String,
    pub(crate) requirement: String,
    pub(crate) command: String,
    /// `structured:<row>` for current matrices; old matrices record the text
    /// fields used by the one compatibility parser.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) command_source: Option<String>,
    pub(crate) working_directory: String,
    pub(crate) source_matrix: BoundRecheckArtifact,
    pub(crate) evidence: BoundRecheckArtifact,
    pub(crate) result: EvidenceResult,
    pub(crate) run_at: String,
}
