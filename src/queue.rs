//! Durable dispatch ledger for the regular Work Queue.
//!
//! Each Work Item owns one ledger file under `.fluent/work/queue/`. A ledger
//! keeps terminal dispatch history and at most one active dispatch, so a Work
//! Item can be queued, run, reach a terminal disposition, and be explicitly
//! re-queued without losing the record of earlier dispatches. Legacy
//! single-entry queue files read as the first dispatch and migrate lazily.
//!
//! Mutations that must be atomic across processes take a short project-wide
//! queue lock. That lock is never held while acquiring the Work model,
//! lineage, candidate, or follow-up locks, so the queue can never invert a
//! lock order and deadlock.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};

use rustix::fs::{FlockOperation, flock};

use crate::work_model::{
    ArtifactRef, AttemptStatus, DurableTransitionMetadata, DurableTransitionOrigin, PauseKind,
    PauseRecovery, TaskKind, TaskStatus, WorkItem, WorkModelError, WorkModelStore,
};

const SUPERSEDED_QUEUE_ADMISSION_ROLE: &str = "superseded-queue-admission";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct QueueResumeReconciliation {
    schema_version: u32,
    resumed_attempt_id: String,
    superseded_attempt_ids: Vec<String>,
    reason: String,
}

/// Execution status of a single dispatch.
///
/// `Queued`, `Claimed`, and `Running` are active states; the remainder are
/// terminal dispositions that survive replayed automatic enqueue attempts.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum DispatchStatus {
    Queued,
    Claimed,
    Running,
    CandidateReady,
    Failed,
    NeedsUser,
    Canceled,
    Blocked,
}

impl DispatchStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Claimed => "claimed",
            Self::Running => "running",
            Self::CandidateReady => "candidate-ready",
            Self::Failed => "failed",
            Self::NeedsUser => "needs-user",
            Self::Canceled => "canceled",
            Self::Blocked => "blocked",
        }
    }

    /// Whether the dispatch has reached a terminal disposition. A terminal
    /// dispatch is preserved as history and never resumed.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::CandidateReady | Self::Failed | Self::NeedsUser | Self::Canceled | Self::Blocked
        )
    }

    /// Whether the dispatch is still active — awaiting a claim or under
    /// execution. At most one dispatch per ledger is active.
    pub fn is_active(&self) -> bool {
        !self.is_terminal()
    }
}

impl std::fmt::Display for DispatchStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Why this dispatch was admitted to the queue.
///
/// Ordinary dispatches run execution-ready Work. A resume dispatch is a
/// separate, explicit authorization to advance one exact suspended Attempt.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum DispatchIntent {
    #[default]
    Run,
    ResumeAttempt,
}

impl DispatchIntent {
    fn is_run(intent: &Self) -> bool {
        *intent == Self::Run
    }
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// One dispatch of a Work Item onto the queue: a single trip from `queued`
/// through a bound Attempt to a terminal disposition.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Dispatch {
    /// Stable id of this dispatch within its ledger.
    pub dispatch_id: String,
    /// The Work Item this dispatch schedules.
    pub work_item_id: String,
    /// When the dispatch entered the queue.
    pub queued_at: String,
    /// Scheduling priority; higher runs sooner.
    pub priority: i64,
    /// Execution status.
    pub status: DispatchStatus,
    /// Monotonic generation, bumped on every state mutation so a claim,
    /// launch, or reconcile can detect a concurrent change and fail closed.
    #[serde(default)]
    pub generation: u64,
    /// Explicit admission intent. Omitted legacy records are ordinary runs.
    #[serde(default, skip_serializing_if = "DispatchIntent::is_run")]
    pub intent: DispatchIntent,
    /// The single Attempt durably bound to this dispatch, persisted before the
    /// Attempt is ensured so recovery advances that exact Attempt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bound_attempt_id: Option<String>,
    /// When the dispatch was claimed, for reference.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claimed_at: Option<String>,
    /// The originating operation that enqueued this dispatch automatically.
    /// Lets replayed materialization or promotion recognize its own dispatch
    /// and stay idempotent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_operation_id: Option<String>,
    /// The earlier terminal needs-user dispatch this explicit resume follows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_of_dispatch_id: Option<String>,
    /// Reason a dispatch was blocked, when `status` is `Blocked`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_reason: Option<String>,
    /// One-dispatch approval to rerun a Tester-harness recovery without the
    /// Fluent Tester sandbox. Only explicit resume admission may set this.
    #[serde(default, skip_serializing_if = "is_false")]
    pub tester_no_sandbox: bool,
    /// Exact cancellation an operator authorized this dispatch to resume.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_cancellation_requested_at: Option<String>,
}

impl Dispatch {
    fn new_queued(
        work_item_id: &str,
        priority: i64,
        origin_operation_id: Option<String>,
        sequence: u64,
    ) -> Self {
        Self {
            dispatch_id: format!("dispatch-{sequence}"),
            work_item_id: work_item_id.to_string(),
            queued_at: crate::work_model::now_iso8601(),
            priority,
            status: DispatchStatus::Queued,
            generation: 0,
            intent: DispatchIntent::Run,
            bound_attempt_id: None,
            claimed_at: None,
            origin_operation_id,
            resume_of_dispatch_id: None,
            blocked_reason: None,
            tester_no_sandbox: false,
            resume_cancellation_requested_at: None,
        }
    }

    fn new_resume(
        work_item_id: &str,
        attempt_id: &str,
        priority: i64,
        resume_of_dispatch_id: Option<String>,
        tester_no_sandbox: bool,
        resume_cancellation_requested_at: Option<String>,
        sequence: u64,
    ) -> Self {
        Self {
            dispatch_id: format!("dispatch-{sequence}"),
            work_item_id: work_item_id.to_string(),
            queued_at: crate::work_model::now_iso8601(),
            priority,
            status: DispatchStatus::Queued,
            generation: 0,
            intent: DispatchIntent::ResumeAttempt,
            bound_attempt_id: Some(attempt_id.to_string()),
            claimed_at: None,
            origin_operation_id: None,
            resume_of_dispatch_id,
            blocked_reason: None,
            tester_no_sandbox,
            resume_cancellation_requested_at,
        }
    }
}

/// A durable pointer to a claimed dispatch. Carries the generation and bound
/// Attempt id so a launch or reconcile can verify it is still acting on the
/// same dispatch it claimed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DispatchToken {
    pub work_item_id: String,
    pub dispatch_id: String,
    pub generation: u64,
    pub bound_attempt_id: String,
    pub priority: i64,
    pub tester_no_sandbox: bool,
}

/// Result of reconciling an automatic enqueue intent with the durable ledger.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnsureDispatchOutcome {
    /// This call created the operation's dispatch.
    Created,
    /// The operation's matching dispatch was already durable.
    ExistingMatching,
}

/// Result of explicitly authorizing scheduler recovery for one Attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResumeDispatchOutcome {
    Created,
    ExistingMatching,
}

/// A per-Work-Item ledger: terminal dispatch history plus at most one active
/// dispatch. Dispatches are ordered oldest first; the active dispatch, when
/// present, is always the last entry.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct DispatchLedger {
    pub work_item_id: String,
    #[serde(default)]
    pub dispatches: Vec<Dispatch>,
}

impl DispatchLedger {
    fn empty(work_item_id: &str) -> Self {
        Self {
            work_item_id: work_item_id.to_string(),
            dispatches: Vec::new(),
        }
    }

    /// The active dispatch, if the most recent one has not reached a terminal
    /// disposition.
    pub fn active(&self) -> Option<&Dispatch> {
        self.dispatches.last().filter(|d| d.status.is_active())
    }

    fn active_mut(&mut self) -> Option<&mut Dispatch> {
        match self.dispatches.last_mut() {
            Some(d) if d.status.is_active() => Some(d),
            _ => None,
        }
    }

    /// The most recent dispatch regardless of disposition — the ledger's
    /// current state for display.
    pub fn latest(&self) -> Option<&Dispatch> {
        self.dispatches.last()
    }

    /// The next dispatch sequence number, so ids stay unique and ordered.
    fn next_sequence(&self) -> u64 {
        self.dispatches.len() as u64 + 1
    }
}

fn queue_dir(project_root: &Path) -> PathBuf {
    project_root.join(".fluent").join("work").join("queue")
}

fn queue_file(project_root: &Path, work_item_id: &str) -> PathBuf {
    queue_dir(project_root).join(format!("{work_item_id}.json"))
}

fn queue_lock_path(project_root: &Path) -> PathBuf {
    queue_dir(project_root).join(".lock")
}

/// A held project-wide queue lock. Dropping it releases the lock.
pub struct QueueLock {
    _file: fs::File,
}

/// Acquire the short project-wide queue lock, blocking until it is free. The
/// caller must not acquire any Work model, lineage, candidate, or follow-up
/// lock while holding it.
pub fn lock_queue(project_root: &Path) -> Result<QueueLock> {
    let dir = queue_dir(project_root);
    fs::create_dir_all(&dir)?;
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .open(queue_lock_path(project_root))?;
    flock(&file, FlockOperation::LockExclusive)?;
    Ok(QueueLock { _file: file })
}

fn work_item_exists(project_root: &Path, id: &str) -> bool {
    project_root
        .join(".fluent")
        .join("work")
        .join("items")
        .join(format!("{id}.json"))
        .is_file()
}

// -------------------------------------------------------------------------
// Ledger read / write, including lazy legacy migration
// -------------------------------------------------------------------------

/// Legacy single-entry queue schema, kept so previously persisted queue files
/// still load. Its `done` status maps to `candidate-ready`.
#[derive(Deserialize)]
struct LegacyQueueEntry {
    work_item_id: String,
    queued_at: String,
    priority: i64,
    status: String,
}

fn legacy_status_to_dispatch(status: &str) -> DispatchStatus {
    match status {
        "done" => DispatchStatus::CandidateReady,
        "running" => DispatchStatus::Running,
        "failed" => DispatchStatus::Failed,
        "needs-user" => DispatchStatus::NeedsUser,
        _ => DispatchStatus::Queued,
    }
}

/// Read the ledger for a Work Item. Returns `None` when no queue file exists.
/// A legacy single-entry file reads as a one-dispatch ledger, preserving its
/// priority, queue time, and outcome. A malformed file is an error so callers
/// can surface it for operator inspection.
pub fn read_ledger(project_root: &Path, work_item_id: &str) -> Result<Option<DispatchLedger>> {
    #[cfg(feature = "snapshot-profile")]
    crate::snapshot_profile::operation("read_ledger");
    let path = queue_file(project_root, work_item_id);
    let content = match fs::read_to_string(&path) {
        Ok(content) => content,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err.into()),
    };
    parse_ledger(&content).map(Some)
}

fn parse_ledger(content: &str) -> Result<DispatchLedger> {
    let value: serde_json::Value = serde_json::from_str(content)?;
    if value.get("dispatches").is_some() {
        let ledger: DispatchLedger = serde_json::from_value(value)?;
        Ok(ledger)
    } else {
        // Legacy single-entry schema: migrate it into the first dispatch.
        let legacy: LegacyQueueEntry = serde_json::from_value(value)?;
        let dispatch = Dispatch {
            dispatch_id: "dispatch-1".to_string(),
            work_item_id: legacy.work_item_id.clone(),
            queued_at: legacy.queued_at,
            priority: legacy.priority,
            status: legacy_status_to_dispatch(&legacy.status),
            generation: 0,
            intent: DispatchIntent::Run,
            bound_attempt_id: None,
            claimed_at: None,
            origin_operation_id: None,
            resume_of_dispatch_id: None,
            blocked_reason: None,
            tester_no_sandbox: false,
            resume_cancellation_requested_at: None,
        };
        Ok(DispatchLedger {
            work_item_id: legacy.work_item_id,
            dispatches: vec![dispatch],
        })
    }
}

fn write_ledger(project_root: &Path, ledger: &DispatchLedger) -> Result<()> {
    fs::create_dir_all(queue_dir(project_root))?;
    let path = queue_file(project_root, &ledger.work_item_id);
    crate::atomic_write::atomic_write(&path, serde_json::to_string_pretty(ledger)?.as_bytes())?;
    Ok(())
}

// -------------------------------------------------------------------------
// Queue commands: add, show, list, remove
// -------------------------------------------------------------------------

/// Add an execution-ready, lifecycle-eligible Work Item to the queue.
///
/// Creates one `queued` dispatch when the Work Item has no active dispatch,
/// preserving any earlier terminal history. When an active dispatch already
/// exists, preserves its queue time and changes its priority only when an
/// explicit priority is supplied.
pub fn add(project_root: &Path, id: &str, priority: Option<i64>) -> Result<()> {
    if !work_item_exists(project_root, id) {
        bail!("Work Item {id:?} not found");
    }
    let store = WorkModelStore::new(project_root);
    let _model_lock = store.lock_work_item_model(id)?;
    let item = store.read_work_item_under_model_lock(id)?;
    ensure_lifecycle_eligible(&item, id)?;

    let _lock = lock_queue(project_root)?;
    let mut ledger = read_ledger(project_root, id)?.unwrap_or_else(|| DispatchLedger::empty(id));

    if let Some(active) = ledger.active_mut() {
        // A dispatch is already active: keep its queue time, and only an
        // explicit priority changes it.
        if let Some(priority) = priority {
            active.priority = priority;
        }
        write_ledger(project_root, &ledger)?;
        return Ok(());
    }

    let sequence = ledger.next_sequence();
    ledger.dispatches.push(Dispatch::new_queued(
        id,
        priority.unwrap_or(0),
        None,
        sequence,
    ));
    write_ledger(project_root, &ledger)?;
    Ok(())
}

/// Explicitly authorize the scheduler to resume one exact suspended Attempt.
///
/// A new resume dispatch preserves the earlier terminal dispatch as history
/// and carries both the authorization intent and Attempt binding before any
/// scheduler claim. Repeating the command while that same resume is active is
/// idempotent; a later needs-user outcome requires another explicit resume.
fn reconcile_interrupted_evidence_queue_admissions(
    project_root: &Path,
    id: &str,
    attempt_id: &str,
) -> Result<bool> {
    let store = WorkModelStore::new(project_root);
    let snapshot = store.read_work_item(id)?;
    let Some(target_index) = snapshot
        .attempts
        .iter()
        .position(|attempt| attempt.id == attempt_id)
    else {
        return Ok(false);
    };
    let target = &snapshot.attempts[target_index];
    let later = &snapshot.attempts[target_index + 1..];
    if later.is_empty() {
        return Ok(false);
    }
    let evidence_path = format!(
        "{}/superseded-attempts.json",
        crate::work_model::work_artifact_path(id, attempt_id, "queue-resume-reconciliation")
    );
    let evidence_ref = ArtifactRef {
        producer_id: "queue-resume-reconciliation".to_string(),
        path: evidence_path.clone(),
    };
    if has_reconciled_interrupted_evidence_queue_admission(project_root, &snapshot, attempt_id) {
        return Ok(true);
    }
    if !target.has_pending_evidence_review_tasks() {
        return Ok(false);
    }
    if later.iter().any(|attempt| {
        attempt.status != AttemptStatus::NeedsUser
            || attempt.pause_kind != Some(PauseKind::Interrupted)
            || attempt.tasks.len() != 1
            || attempt.tasks[0].kind != TaskKind::Write
            || attempt.tasks[0].status != TaskStatus::NeedsUser
            || attempt.tasks[0].output.is_some()
    }) {
        return Ok(false);
    }

    // Own every interrupted Task while checking its worktree and publishing
    // the reconciliation. A live or concurrently resumed Writer therefore
    // fails before any Work state changes.
    let mut task_leases = Vec::with_capacity(later.len());
    for attempt in later {
        task_leases.push(crate::lease::acquire(&crate::lease::task_lock_path(
            project_root,
            id,
            &attempt.tasks[0].id,
        ))?);
    }
    let _land_lock = crate::land_lock::acquire(&crate::land_lock::lock_path(project_root))?;
    let project_head = crate::git::run_stdout(
        project_root,
        &["rev-parse", "HEAD"],
        "resolve queue-resume project HEAD",
    )?;
    for attempt in later {
        let task = &attempt.tasks[0];
        let [workspace] = task.workspace_access.writes.as_slice() else {
            bail!(
                "interrupted queue admission {:?} does not own one exact Writer workspace",
                attempt.id
            );
        };
        let workspace_path = PathBuf::from(&workspace.path);
        let workspace_path = if workspace_path.is_absolute() {
            workspace_path
        } else {
            project_root.join(workspace_path)
        };
        let workspace_path = workspace_path.canonicalize()?;
        let workspace_head = crate::git::run_stdout(
            &workspace_path,
            &["rev-parse", "HEAD"],
            "resolve interrupted queue-admission HEAD",
        )?;
        let dirty = crate::git::run_stdout(
            &workspace_path,
            &["status", "--porcelain", "--untracked-files=all", "--", "."],
            "inspect interrupted queue-admission workspace",
        )?;
        if workspace_head != project_head || !dirty.is_empty() {
            bail!(
                "interrupted queue admission {:?} contains candidate work and cannot be superseded",
                attempt.id
            );
        }
    }

    let superseded_attempt_ids = later
        .iter()
        .map(|attempt| attempt.id.clone())
        .collect::<Vec<_>>();
    let evidence = QueueResumeReconciliation {
        schema_version: 1,
        resumed_attempt_id: attempt_id.to_string(),
        superseded_attempt_ids: superseded_attempt_ids.clone(),
        reason: "resume an existing evidence Review instead of an accidental new Attempt"
            .to_string(),
    };
    let evidence_bytes = serde_json::to_vec_pretty(&evidence)?;
    let evidence_file = project_root.join(&evidence_path);
    if let Some(parent) = evidence_file.parent() {
        fs::create_dir_all(parent)?;
    }
    if evidence_file.exists() {
        if fs::read(&evidence_file)? != evidence_bytes {
            bail!("queue-resume reconciliation evidence changed at a stable path");
        }
    } else {
        crate::atomic_write::atomic_write(&evidence_file, &evidence_bytes)?;
    }

    let later_snapshot = later.to_vec();
    let transition = DurableTransitionMetadata {
        origin: DurableTransitionOrigin::Fluent,
        reason: Some("supersede accidental queue admission during exact Attempt resume".into()),
        evidence_ref: Some(evidence_path),
    };
    store.mutate_work_item_with_transition(id, &transition, |item| {
        let fresh_target_index = item
            .attempts
            .iter()
            .position(|attempt| attempt.id == attempt_id)
            .ok_or_else(|| WorkModelError::AttemptNotFound {
                id: attempt_id.to_string(),
            })?;
        if !item.attempts[fresh_target_index].has_pending_evidence_review_tasks()
            || item.attempts[fresh_target_index + 1..] != later_snapshot
        {
            return Err(WorkModelError::AttemptResumeIneligible {
                attempt_id: attempt_id.to_string(),
                reason: "the evidence-review or later Attempt frontier changed during queue reconciliation"
                    .to_string(),
            });
        }
        if !item.attempts[fresh_target_index]
            .artifacts
            .contains(&evidence_ref)
        {
            item.attempts[fresh_target_index]
                .artifacts
                .push(evidence_ref.clone());
        }
        for attempt in &mut item.attempts[fresh_target_index + 1..] {
            let task = &mut attempt.tasks[0];
            crate::work_model::set_task_terminal(task, TaskStatus::Superseded);
            task.role = SUPERSEDED_QUEUE_ADMISSION_ROLE.to_string();
            if !task.input_artifacts.contains(&evidence_ref) {
                task.input_artifacts.push(evidence_ref.clone());
            }
            attempt.status = AttemptStatus::Failed;
            attempt.pause_kind = None;
            attempt.pause_recovery = None;
            if !attempt.artifacts.contains(&evidence_ref) {
                attempt.artifacts.push(evidence_ref.clone());
            }
        }
        Ok(())
    })?;
    drop(task_leases);
    Ok(true)
}

fn has_reconciled_interrupted_evidence_queue_admission(
    project_root: &Path,
    item: &WorkItem,
    attempt_id: &str,
) -> bool {
    let Some(target_index) = item
        .attempts
        .iter()
        .position(|attempt| attempt.id == attempt_id)
    else {
        return false;
    };
    let target = &item.attempts[target_index];
    let later = &item.attempts[target_index + 1..];
    if later.is_empty() {
        return false;
    }
    let evidence_ref = ArtifactRef {
        producer_id: "queue-resume-reconciliation".to_string(),
        path: format!(
            "{}/superseded-attempts.json",
            crate::work_model::work_artifact_path(
                &item.id,
                attempt_id,
                "queue-resume-reconciliation"
            )
        ),
    };
    let expected_evidence = QueueResumeReconciliation {
        schema_version: 1,
        resumed_attempt_id: attempt_id.to_string(),
        superseded_attempt_ids: later.iter().map(|attempt| attempt.id.clone()).collect(),
        reason: "resume an existing evidence Review instead of an accidental new Attempt"
            .to_string(),
    };
    fs::read(project_root.join(&evidence_ref.path))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<QueueResumeReconciliation>(&bytes).ok())
        .as_ref()
        == Some(&expected_evidence)
        && target.artifacts.contains(&evidence_ref)
        && later.iter().all(|attempt| {
            attempt.status == AttemptStatus::Failed
                && attempt.pause_kind.is_none()
                && attempt.tasks.len() == 1
                && attempt.tasks[0].kind == TaskKind::Write
                && attempt.tasks[0].status == TaskStatus::Superseded
                && attempt.tasks[0].role == SUPERSEDED_QUEUE_ADMISSION_ROLE
                && attempt.tasks[0].output.is_none()
                && attempt.tasks[0].input_artifacts.contains(&evidence_ref)
                && attempt.artifacts.contains(&evidence_ref)
        })
}

pub fn resume(
    project_root: &Path,
    id: &str,
    attempt_id: &str,
    priority: Option<i64>,
) -> Result<ResumeDispatchOutcome> {
    resume_with_tester_no_sandbox(project_root, id, attempt_id, priority, false)
}

/// Explicitly authorize one scheduler recovery, optionally approving the
/// already-paused Tester Task to run without Fluent's outer Tester sandbox.
pub fn resume_with_tester_no_sandbox(
    project_root: &Path,
    id: &str,
    attempt_id: &str,
    priority: Option<i64>,
    tester_no_sandbox: bool,
) -> Result<ResumeDispatchOutcome> {
    resume_checked(
        project_root,
        id,
        attempt_id,
        priority,
        tester_no_sandbox,
        None,
    )
}

pub fn resume_with_approval(
    project_root: &Path,
    id: &str,
    attempt_id: &str,
    approval: &crate::work_approval::WorkApproval,
    expected_dispatch: &str,
) -> Result<ResumeDispatchOutcome> {
    resume_checked(
        project_root,
        id,
        attempt_id,
        None,
        false,
        Some((approval, expected_dispatch)),
    )
}
fn resume_checked(
    project_root: &Path,
    id: &str,
    attempt_id: &str,
    priority: Option<i64>,
    tester_no_sandbox: bool,
    approval: Option<(&crate::work_approval::WorkApproval, &str)>,
) -> Result<ResumeDispatchOutcome> {
    if let Some((approval, _)) = approval {
        // A durable dispatch proves admission even if it has since stopped again.
        // Never let replay of that operation authorize a later suspension.
        let _lock = lock_queue(project_root)?;
        if read_ledger(project_root, id)?.is_some_and(|ledger| {
            ledger
                .dispatches
                .iter()
                .any(|d| d.origin_operation_id.as_deref() == Some(&approval.operation))
        }) {
            return Ok(ResumeDispatchOutcome::ExistingMatching);
        }
    }
    if !work_item_exists(project_root, id) {
        bail!("Work Item {id:?} not found");
    }
    let snapshot = WorkModelStore::new(project_root).read_work_item_snapshot(id)?;
    let context_retry_ready = crate::recovery_contracts::context_retry::resume_ready(
        project_root,
        &snapshot,
        attempt_id,
    )?;
    let usage_limit_ready = crate::lifecycle_recovery::usage_limit_writer_resume_ready(
        project_root,
        &snapshot,
        attempt_id,
    )?;
    if !context_retry_ready && !usage_limit_ready {
        ensure_writer_assignment_resume_ready(project_root, &snapshot, attempt_id)?;
    }
    let _ = reconcile_interrupted_evidence_queue_admissions(project_root, id, attempt_id)?;
    let store = WorkModelStore::new(project_root);
    // Older Fluent versions could persist a direct Codex authentication failure
    // as a generic round-cap pause. Reclassify that exact, transcript-proven
    // frontier before queue admission so the public scheduler recovery remains
    // usable after the operator re-authenticates. The migration is idempotent;
    // non-matching failures remain hard and are still rejected below.
    let _ = crate::work_attempt_loop::reclassify_codex_auth_pause(
        &store,
        project_root,
        id,
        attempt_id,
    )?;
    // Older classified recoveries omitted the target Review digest. Reconcile
    // that field from the exact candidate-bound Review before queue admission;
    // the migration validates the complete replay contract and is idempotent.
    let _ = store.mutate_work_item(id, |item| {
        crate::recovery_contracts::migrate_legacy_code_change_review_digest(
            project_root,
            item,
            attempt_id,
            None,
        )
        .map_err(|reason| WorkModelError::HostEvidenceRecoveryIneligible {
            attempt_id: attempt_id.to_string(),
            reason,
        })
    })?;
    // An older paused correction may omit the protected assignment. Rebuild it
    // only after this explicit resume request has reached the lifecycle owner;
    // the inspector itself remains read-only and repeats under the model lock.
    let _ = crate::lifecycle_recovery::recover_legacy_writer_correction_assignment(
        project_root,
        &store,
        id,
        attempt_id,
    )?;
    let _model_lock = store.lock_work_item_model(id)?;
    let item = store.read_work_item_under_model_lock(id)?;
    if let Some((approval, _)) = approval {
        approval.check(&item)?;
    }

    // Queue admission and foreground execution recognize the same narrow
    // legacy presentation-only pause. A candidate-gate-shaped mismatch fails
    // before this command changes the dispatch ledger.
    let _ = crate::lifecycle_recovery::inspect_retired_commit_style_recovery(
        project_root,
        &item,
        attempt_id,
    )?;
    let failed_candidate_gate_correction = item
        .attempts
        .iter()
        .find(|attempt| attempt.id == attempt_id)
        .is_some_and(crate::work_model::Attempt::is_failed_candidate_gate_correction);
    let reconciled_tester_harness_repair = item
        .attempts
        .iter()
        .find(|attempt| attempt.id == attempt_id)
        .is_some_and(crate::work_model::Attempt::has_reconciled_tester_harness_repair);
    let reconciled_candidate_ancestry_repair =
        crate::candidate_ancestry_recovery::is_resume_ready(project_root, &item, attempt_id)?;
    let reconciled_candidate_metadata_repair =
        crate::candidate_metadata_recovery::is_resume_ready(project_root, &item, attempt_id)?;
    let learner_retry = crate::lifecycle_recovery::is_learner_retry_recovery(&item, attempt_id);
    let pending_evidence_review = item
        .attempts
        .iter()
        .find(|attempt| attempt.id == attempt_id)
        .is_some_and(crate::work_model::Attempt::has_pending_evidence_review_tasks);
    let pending_code_change_classification = item
        .attempts
        .iter()
        .find(|attempt| attempt.id == attempt_id)
        .is_some_and(crate::work_model::Attempt::has_pending_code_change_classification);
    let reconciled_evidence_queue_admission =
        has_reconciled_interrupted_evidence_queue_admission(project_root, &item, attempt_id);
    let legacy_learner_tester_evidence =
        crate::lifecycle_recovery::is_legacy_learner_tester_evidence_failure(
            project_root,
            &item,
            attempt_id,
        )?;

    // Read-only recovery validation may inspect revision audits, completion
    // matrices, worktrees, and downstream evidence. Do it before taking the
    // project-wide queue lock. An already-matching active resume is the one
    // idempotent receipt-loss path that intentionally skips this full replay;
    // its model and cancellation identity are checked again below. Re-read
    // the ledger under the queue lock and restart admission if it changed
    // while the lock-free validation ran.
    let mut preflight_ledger =
        read_ledger(project_root, id)?.unwrap_or_else(|| DispatchLedger::empty(id));
    loop {
        let already_queued = preflight_ledger.active().is_some_and(|active| {
            active.intent == DispatchIntent::ResumeAttempt
                && active.bound_attempt_id.as_deref() == Some(attempt_id)
        });
        if already_queued {
            if let Some(attempt) = item
                .attempts
                .iter()
                .find(|attempt| attempt.id == attempt_id)
            {
                crate::host_evidence::ensure_owned_host_resume_ready_for_item(
                    project_root,
                    &item,
                    attempt,
                )?;
            }
        } else {
            ensure_tester_no_sandbox_eligible(&item, id, attempt_id, tester_no_sandbox)?;
            ensure_resume_eligible(
                project_root,
                &item,
                id,
                attempt_id,
                legacy_learner_tester_evidence,
                reconciled_candidate_ancestry_repair,
                reconciled_candidate_metadata_repair,
                pending_code_change_classification,
            )?;
        }

        #[cfg(test)]
        crate::test_lock_probe::reach("queue-lifecycle", id, "resume", "PREFLIGHT");

        let queue_lock = lock_queue(project_root)?;
        let mut ledger =
            read_ledger(project_root, id)?.unwrap_or_else(|| DispatchLedger::empty(id));
        if preflight_ledger != ledger {
            drop(queue_lock);
            preflight_ledger = ledger;
            continue;
        }
        let _lock = queue_lock;
        if let Some((approval, expected)) = approval {
            use sha2::{Digest, Sha256};
            if ledger
                .dispatches
                .iter()
                .any(|d| d.origin_operation_id.as_deref() == Some(&approval.operation))
            {
                return Ok(ResumeDispatchOutcome::ExistingMatching);
            }
            let current = read_ledger(project_root, id)?;
            anyhow::ensure!(
                format!("{:x}", Sha256::digest(serde_json::to_vec(&current)?)) == expected,
                "Work dispatch changed; request fresh approval"
            );
        }

        if let Some(active) = ledger.active_mut() {
            if active.intent == DispatchIntent::ResumeAttempt
                && active.bound_attempt_id.as_deref() == Some(attempt_id)
            {
                if let Some(attempt) = item
                    .attempts
                    .iter()
                    .find(|attempt| attempt.id == attempt_id)
                {
                    if attempt.cancellation_requested()
                        && active.resume_cancellation_requested_at.as_deref()
                            != attempt
                                .cancellation
                                .as_ref()
                                .map(|stop| stop.requested_at.as_str())
                    {
                        bail!(
                            "active dispatch was admitted before the current cancellation; wait for it to settle or remove it before authorizing a new resume"
                        );
                    }
                }
                if active.tester_no_sandbox != tester_no_sandbox {
                    bail!(
                        "resume of Attempt {attempt_id:?} is already queued with tester_no_sandbox={}; remove it before changing that approval",
                        active.tester_no_sandbox
                    );
                }
                if let Some(priority) = priority {
                    active.priority = priority;
                    write_ledger(project_root, &ledger)?;
                }
                return Ok(ResumeDispatchOutcome::ExistingMatching);
            }
            bail!(
                "Work Item {id:?} already has active dispatch {:?} ({})",
                active.dispatch_id,
                active.status
            );
        }

        let resume_of_dispatch_id = match ledger.latest() {
            Some(previous) if previous.status == DispatchStatus::Canceled => {
                // A canceled dispatch is immutable history, not the suspension this
                // new command authorizes. The current Work-model frontier above is
                // the authority, so append a fresh exact resume without linking it
                // to an older (and possibly differently bound) canceled dispatch.
                None
            }
            Some(previous) => {
                if previous.status != DispatchStatus::NeedsUser
                    && !(previous.status == DispatchStatus::Failed
                        && failed_candidate_gate_correction)
                    && !(previous.status == DispatchStatus::Failed
                        && reconciled_candidate_ancestry_repair)
                    && !(previous.status == DispatchStatus::Failed
                        && reconciled_candidate_metadata_repair)
                    && !(previous.status == DispatchStatus::Blocked
                        && reconciled_candidate_metadata_repair)
                    && !(previous.status == DispatchStatus::Blocked && learner_retry)
                    && !(previous.status == DispatchStatus::Blocked
                        && reconciled_tester_harness_repair)
                    && !(previous.status == DispatchStatus::Blocked && pending_evidence_review)
                    && !(pending_code_change_classification
                        && matches!(
                            previous.status,
                            DispatchStatus::Failed | DispatchStatus::Blocked
                        ))
                    && !(legacy_learner_tester_evidence
                        && matches!(
                            previous.status,
                            DispatchStatus::CandidateReady | DispatchStatus::Blocked
                        ))
                {
                    bail!(
                        "Work Item {id:?} latest dispatch {:?} is {}, not a resumable suspension",
                        previous.dispatch_id,
                        previous.status
                    );
                }
                if previous
                    .bound_attempt_id
                    .as_deref()
                    .is_some_and(|bound| bound != attempt_id)
                    && !reconciled_evidence_queue_admission
                {
                    bail!(
                        "Work Item {id:?} latest resumable dispatch is bound to Attempt {:?}, not {attempt_id:?}",
                        previous.bound_attempt_id.as_deref().unwrap_or_default()
                    );
                }
                Some(previous.dispatch_id.clone())
            }
            None => None,
        };

        let sequence = ledger.next_sequence();
        ledger.dispatches.push(Dispatch::new_resume(
            id,
            attempt_id,
            priority.unwrap_or(0),
            resume_of_dispatch_id,
            tester_no_sandbox,
            item.attempts
                .iter()
                .find(|attempt| attempt.id == attempt_id)
                .filter(|attempt| attempt.cancellation_requested())
                .and_then(|attempt| attempt.cancellation.as_ref())
                .map(|stop| stop.requested_at.clone()),
            sequence,
        ));
        if let Some((approval, _)) = approval {
            ledger.dispatches.last_mut().unwrap().origin_operation_id =
                Some(approval.operation.clone());
        }
        write_ledger(project_root, &ledger)?;
        return Ok(ResumeDispatchOutcome::Created);
    }
}

/// Read the recovery authority captured by the exact running scheduler dispatch.
/// A delayed child must not infer authorization from a newer Attempt stop.
pub fn cancellation_resume_for_dispatch(
    project_root: &Path,
    work_item_id: &str,
    attempt_id: &str,
    dispatch_id: &str,
) -> Result<Option<String>> {
    let ledger = read_ledger(project_root, work_item_id)?
        .ok_or_else(|| anyhow::anyhow!("scheduler dispatch ledger is missing"))?;
    let dispatch = ledger
        .active()
        .filter(|dispatch| {
            dispatch.dispatch_id == dispatch_id
                && dispatch.status == DispatchStatus::Running
                && dispatch.bound_attempt_id.as_deref() == Some(attempt_id)
        })
        .ok_or_else(|| {
            anyhow::anyhow!("scheduler launch does not match the running dispatch and Attempt")
        })?;
    Ok(if dispatch.intent == DispatchIntent::ResumeAttempt {
        dispatch.resume_cancellation_requested_at.clone()
    } else {
        None
    })
}

/// Idempotently enqueue an execution-ready Work Item on behalf of automatic
/// promotion. Recognizes its own earlier dispatch by `origin_operation_id` for
/// receipt-loss replay. An unrelated active or terminal dispatch fails closed
/// because it cannot prove this operation's required queue effect.
pub fn ensure_dispatch(
    project_root: &Path,
    id: &str,
    origin_operation_id: &str,
    priority: i64,
) -> Result<EnsureDispatchOutcome> {
    if !work_item_exists(project_root, id) {
        bail!("Work Item {id:?} not found");
    }
    let store = WorkModelStore::new(project_root);
    let _model_lock = store.lock_work_item_model(id)?;
    let item = store.read_work_item_under_model_lock(id)?;
    let eligibility = ensure_lifecycle_eligible(&item, id);

    let _lock = lock_queue(project_root)?;
    let mut ledger = read_ledger(project_root, id)?.unwrap_or_else(|| DispatchLedger::empty(id));

    // This exact automatic operation already produced a dispatch: reuse it,
    // whatever its current disposition, so replays stay idempotent.
    if ledger
        .dispatches
        .iter()
        .any(|d| d.origin_operation_id.as_deref() == Some(origin_operation_id))
    {
        return Ok(EnsureDispatchOutcome::ExistingMatching);
    }

    eligibility?;

    // Another dispatch cannot prove this automatic operation's required
    // effect, regardless of whether that dispatch is active or terminal.
    if let Some(existing) = ledger.latest() {
        bail!(
            "Work Item {id:?} dispatch {:?} ({}) does not match automatic operation {origin_operation_id:?}",
            existing.dispatch_id,
            existing.status
        );
    }

    let sequence = ledger.next_sequence();
    ledger.dispatches.push(Dispatch::new_queued(
        id,
        priority,
        Some(origin_operation_id.to_string()),
        sequence,
    ));
    write_ledger(project_root, &ledger)?;
    Ok(EnsureDispatchOutcome::Created)
}

/// Idempotently enqueue an approver-authorized correction after a reviewed
/// Merge Candidate becomes terminal. Unlike first-time automatic promotion,
/// this boundary permits earlier terminal dispatch history while rejecting an
/// unrelated active dispatch.
pub fn ensure_candidate_rework_dispatch(
    project_root: &Path,
    id: &str,
    origin_operation_id: &str,
    priority: i64,
) -> Result<EnsureDispatchOutcome> {
    if !work_item_exists(project_root, id) {
        bail!("Work Item {id:?} not found");
    }
    let store = WorkModelStore::new(project_root);
    let _model_lock = store.lock_work_item_model(id)?;
    let item = store.read_work_item_under_model_lock(id)?;
    let eligibility = ensure_lifecycle_eligible(&item, id);

    let _lock = lock_queue(project_root)?;
    let mut ledger = read_ledger(project_root, id)?.unwrap_or_else(|| DispatchLedger::empty(id));
    if ledger
        .dispatches
        .iter()
        .any(|dispatch| dispatch.origin_operation_id.as_deref() == Some(origin_operation_id))
    {
        return Ok(EnsureDispatchOutcome::ExistingMatching);
    }
    eligibility?;
    if let Some(active) = ledger.active() {
        bail!(
            "Work Item {id:?} already has active dispatch {:?} ({})",
            active.dispatch_id,
            active.status
        );
    }
    let sequence = ledger.next_sequence();
    ledger.dispatches.push(Dispatch::new_queued(
        id,
        priority,
        Some(origin_operation_id.to_string()),
        sequence,
    ));
    write_ledger(project_root, &ledger)?;
    Ok(EnsureDispatchOutcome::Created)
}

/// List each Work Item's active dispatch, ordered by priority descending then
/// queue time ascending. Terminal dispositions stay in their ledger as history
/// and are not listed. Skips malformed ledger files with a warning and
/// preserves them for operator inspection.
pub fn list(project_root: &Path) -> Result<Vec<Dispatch>> {
    let mut dispatches: Vec<Dispatch> = list_ledgers(project_root)?
        .into_iter()
        .filter_map(|ledger| ledger.active().cloned())
        .collect();

    dispatches.sort_by(|a, b| {
        b.priority
            .cmp(&a.priority)
            .then_with(|| a.queued_at.cmp(&b.queued_at))
    });

    Ok(dispatches)
}

/// Return the Work Item's latest dispatch, including terminal history.
pub fn show(project_root: &Path, work_item_id: &str) -> Result<Dispatch> {
    read_ledger(project_root, work_item_id)?
        .and_then(|ledger| ledger.latest().cloned())
        .ok_or_else(|| anyhow::anyhow!("Work Item {work_item_id:?} has no queue dispatch"))
}

/// Read every parseable ledger in the queue directory, skipping malformed
/// files with a warning so one bad entry never stalls the scheduler.
pub fn list_ledgers(project_root: &Path) -> Result<Vec<DispatchLedger>> {
    let dir = queue_dir(project_root);
    if !dir.is_dir() {
        return Ok(Vec::new());
    }

    let mut ledgers = Vec::new();
    for dir_entry in fs::read_dir(&dir)? {
        let dir_entry = dir_entry?;
        let path = dir_entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let content = match fs::read_to_string(&path) {
            Ok(content) => content,
            Err(_) => {
                eprintln!("warning: could not read queue file {}", path.display());
                continue;
            }
        };
        match parse_ledger(&content) {
            Ok(ledger) => ledgers.push(ledger),
            Err(e) => {
                eprintln!(
                    "warning: skipping malformed queue file {}: {e}",
                    path.display()
                );
                continue;
            }
        }
    }
    Ok(ledgers)
}

/// Remove an unclaimed queue entry, recording a cancellation disposition that
/// replayed automatic promotion will not restore. Rejects removal when the
/// entry has a live claim, and errors when no active entry exists.
pub fn remove(project_root: &Path, id: &str) -> Result<()> {
    let _lock = lock_queue(project_root)?;
    let mut ledger = match read_ledger(project_root, id)? {
        Some(ledger) => ledger,
        None => bail!("Work Item {id:?} is not queued"),
    };

    let Some(active) = ledger.active_mut() else {
        bail!("Work Item {id:?} is not queued");
    };

    if matches!(
        active.status,
        DispatchStatus::Claimed | DispatchStatus::Running
    ) && claim_is_live(project_root, id)
    {
        bail!("Work Item {id:?} is active and cannot be removed");
    }

    active.status = DispatchStatus::Canceled;
    active.generation += 1;
    write_ledger(project_root, &ledger)?;
    Ok(())
}

// -------------------------------------------------------------------------
// Lifecycle eligibility for explicit queue add
// -------------------------------------------------------------------------

fn ensure_lifecycle_eligible(item: &WorkItem, id: &str) -> Result<()> {
    item.ensure_not_abandoned()?;
    item.ensure_execution_ready()?;

    // A suspended Attempt or a Merge Candidate pending land is recovered
    // directly, not by dispatching new Work.
    if let Some(attempt) = item
        .attempts
        .last()
        .filter(|attempt| attempt.status == AttemptStatus::NeedsUser)
    {
        bail!(
            "Work Item {id:?} has current Attempt {:?} suspended at needs-user; run `fluent queue resume {id} --attempt {}` instead of queueing",
            attempt.id,
            attempt.id
        );
    }
    if let Some(attempt) = item
        .attempts
        .last()
        .filter(|attempt| attempt.has_pending_evidence_review_tasks())
    {
        bail!(
            "Work Item {id:?} has pending evidence Review work on Attempt {:?}; run `fluent queue resume {id} --attempt {}` instead of creating another Attempt",
            attempt.id,
            attempt.id
        );
    }
    if let Some(candidate) = item.merge_candidates.iter().find(|candidate| {
        candidate.merge_state.status == crate::work_model::MergeCandidateMergeStatus::Pending
    }) {
        bail!(
            "Work Item {id:?} has Merge Candidate {:?} pending land; land it instead of queueing",
            candidate.id
        );
    }

    Ok(())
}

/// Reject stale completed Writer authority before recovery can refresh stored
/// pause guidance. Queue admission repeats this check under the model lock.
fn ensure_writer_assignment_resume_ready(
    project_root: &Path,
    item: &WorkItem,
    attempt_id: &str,
) -> Result<()> {
    let Some(attempt) = item
        .attempts
        .iter()
        .find(|attempt| attempt.id == attempt_id)
    else {
        return Ok(());
    };
    if attempt.pause_kind == Some(crate::work_model::PauseKind::WriterCompletion)
        && let Some(writer) = attempt.tasks.iter().rev().find(|task| {
            task.kind == TaskKind::Write
                && task.status == TaskStatus::Complete
                && task.output.is_some()
                && task.writer_correction_assignment.is_some()
        })
    {
        let assignment = writer
            .writer_correction_assignment
            .as_ref()
            .expect("the assignment was checked above");
        crate::recovery_contracts::validate_writer_correction_assignment(
            project_root,
            item,
            writer,
            assignment,
        )
        .with_context(|| {
            format!(
                "Writer correction assignment is unverifiable before resume for Task {}",
                writer.id
            )
        })?;
    }
    Ok(())
}

fn ensure_resume_eligible(
    project_root: &Path,
    item: &WorkItem,
    id: &str,
    attempt_id: &str,
    known_legacy_learner_tester_evidence: bool,
    known_candidate_ancestry_repair: bool,
    known_candidate_metadata_repair: bool,
    known_code_change_classification: bool,
) -> Result<()> {
    item.ensure_not_abandoned()?;
    item.ensure_execution_ready()?;
    let attempt = item
        .attempts
        .iter()
        .find(|attempt| attempt.id == attempt_id)
        .ok_or_else(|| anyhow::anyhow!("Work Item {id:?} has no Attempt to resume"))?;
    if let Some(task) = attempt
        .tasks
        .last()
        .filter(|task| task.kind == TaskKind::Write && task.status == TaskStatus::Planned)
    {
        crate::recovery_contracts::writer_completion::validated_committed_repair_start(
            project_root,
            item,
            task,
        )?;
    }
    if crate::recovery_contracts::context_retry::resume_ready(project_root, item, attempt_id)? {
        return Ok(());
    }
    if crate::lifecycle_recovery::usage_limit_writer_resume_ready(project_root, item, attempt_id)? {
        return Ok(());
    }
    ensure_writer_assignment_resume_ready(project_root, item, attempt_id)?;
    crate::host_evidence::ensure_owned_host_resume_ready_for_item(project_root, item, attempt)?;
    #[cfg(test)]
    crate::test_lock_probe::reach("queue-lifecycle", id, "resume", "EVIDENCE");
    let allows_planned_approach_correction =
        crate::recovery_contracts::validate_planning_resume_for_queue(project_root, item, attempt)?;
    let candidate_ancestry_repair = known_candidate_ancestry_repair
        || crate::candidate_ancestry_recovery::is_resume_ready(project_root, item, attempt_id)?;
    let candidate_metadata_repair = known_candidate_metadata_repair
        || crate::candidate_metadata_recovery::is_resume_ready(project_root, item, attempt_id)?;
    let code_change_classification =
        known_code_change_classification || attempt.has_pending_code_change_classification();
    let replayable_code_change_classification = attempt.has_replayable_code_change_classification();
    let evidence_queue_reconciliation =
        has_reconciled_interrupted_evidence_queue_admission(project_root, item, attempt_id);
    if item
        .attempts
        .last()
        .is_some_and(|current| current.id != attempt_id)
        && !candidate_ancestry_repair
        && !candidate_metadata_repair
        && !code_change_classification
        && !replayable_code_change_classification
        && !evidence_queue_reconciliation
    {
        bail!(
            "Work Item {id:?} current Attempt is {:?}, not {attempt_id:?}",
            item.attempts
                .last()
                .map(|attempt| attempt.id.as_str())
                .unwrap_or_default()
        );
    }
    if crate::writer_completion_recovery::planned_successor_resume_ready(
        project_root,
        item,
        attempt,
    )? {
        return Ok(());
    }
    if candidate_ancestry_repair {
        return Ok(());
    }
    if candidate_metadata_repair {
        return Ok(());
    }
    if code_change_classification {
        ensure_code_change_classification_resume_ready(project_root, item, attempt)?;
        return Ok(());
    }
    if replayable_code_change_classification {
        ensure_code_change_classification_resume_ready(project_root, item, attempt)?;
        return Ok(());
    }
    let legacy_learner_tester_evidence = known_legacy_learner_tester_evidence
        || crate::lifecycle_recovery::is_legacy_learner_tester_evidence_failure(
            project_root,
            item,
            attempt_id,
        )?;
    if !legacy_learner_tester_evidence
        && crate::lifecycle_recovery::is_learner_retry_recovery(item, attempt_id)
    {
        crate::lifecycle_recovery::inspect_learner_retry_recovery(project_root, item, attempt_id)?;
        return Ok(());
    }
    if !legacy_learner_tester_evidence {
        let legacy_review_evidence_round_cap =
            crate::work_attempt_loop::review_evidence_covered_round_cap_resume_is_eligible(
                project_root,
                attempt,
            );
        let completed_recheck_evidence_round_cap =
            crate::work_attempt_loop::completed_recheck_evidence_review_round_cap_is_eligible(
                project_root,
                attempt,
            );
        ensure_attempt_resume_ready(
            attempt,
            legacy_review_evidence_round_cap || completed_recheck_evidence_round_cap,
            allows_planned_approach_correction,
        )?;
        if attempt.is_uncertain_review_pause() {
            crate::lifecycle_recovery::inspect_uncertain_review_recovery(
                project_root,
                item,
                attempt_id,
            )?;
        }
    }
    Ok(())
}

fn ensure_code_change_classification_resume_ready(
    project_root: &Path,
    item: &WorkItem,
    attempt: &crate::work_model::Attempt,
) -> Result<()> {
    let ineligible = |reason: &str| {
        anyhow::anyhow!(
            "Attempt {:?} host-evidence source correction is not resumable: {reason}",
            attempt.id
        )
    };
    if item.attempts.last().map(|current| current.id.as_str()) != Some(attempt.id.as_str()) {
        return Err(ineligible("the classified Attempt is no longer current"));
    }
    let recovery = attempt
        .evidence_recoveries
        .iter()
        .rev()
        .find(|recovery| {
            recovery.state == crate::work_model::EvidenceRecoveryState::CodeChange
                && recovery.code_change_classification.is_some()
        })
        .ok_or_else(|| ineligible("no classified recovery remains"))?;
    crate::recovery_contracts::validate_classification_recovery_scope(attempt, &recovery.id)
        .map_err(|reason| ineligible(&reason))?;
    let classification = recovery
        .code_change_classification
        .as_ref()
        .expect("classified recovery checked above");
    let (writer_index, writer) = attempt
        .tasks
        .iter()
        .enumerate()
        .find(|(_, task)| task.id == classification.writer_task_id && task.kind == TaskKind::Write)
        .ok_or_else(|| ineligible("the classified Writer is missing"))?;
    if writer.status == TaskStatus::Failed {
        return Err(ineligible(
            "the classified Writer is hard-failed without a validated retry transition",
        ));
    }
    if attempt.status == AttemptStatus::NeedsUser
        && writer.output.is_none()
        && matches!(writer.status, TaskStatus::Planned | TaskStatus::NeedsUser)
    {
        let target = crate::recovery_contracts::unique_tests_review_target(
            attempt,
            recovery,
            &classification.review_artifact,
        )
        .map_err(|reason| ineligible(&reason))?;
        let review_task_id =
            target.target.review_task_id.as_deref().ok_or_else(|| {
                ineligible("classified recovery target has no review Task identity")
            })?;
        crate::recovery_contracts::validate_code_change_classification_replay(
            project_root,
            item,
            attempt,
            recovery,
            classification,
            review_task_id,
        )
        .map_err(|reason| ineligible(&reason))?;
        attempt
            .ensure_resume_ready()
            .map_err(|error| anyhow::anyhow!("{error}"))?;
        return Ok(());
    }
    if attempt.tasks.iter().enumerate().any(|(index, task)| {
        task.status == TaskStatus::Executing
            || (task.status == TaskStatus::NeedsUser)
            || (task.status == TaskStatus::Failed
                && (task.kind != TaskKind::Write || task.output.is_some()))
            || (task.status == TaskStatus::Planned && index != writer_index)
    }) {
        return Err(ineligible(
            "the Attempt contains another live or unresolved Task",
        ));
    }
    if item.merge_candidates.iter().any(|candidate| {
        candidate.attempt_id == attempt.id
            && candidate.candidate_commit == recovery.candidate_commit
    }) {
        return Err(ineligible("a Merge Candidate already owns this candidate"));
    }
    let completed = attempt
        .tasks
        .iter()
        .take(writer_index)
        .rev()
        .find(|task| task.kind == TaskKind::Write && task.status == TaskStatus::Complete)
        .and_then(|task| task.output.as_ref())
        .ok_or_else(|| ineligible("the source candidate Writer is missing"))?;
    if completed.commit != recovery.candidate_commit {
        return Err(ineligible("the source candidate commit changed"));
    }
    if !crate::recovery_contracts::candidate_workspace_is_clean_at_managed(
        project_root,
        completed,
        &recovery.candidate_commit,
    ) {
        return Err(ineligible(
            "the candidate workspace is not the current clean registered worktree",
        ));
    }
    if !writer
        .input_artifacts
        .contains(&classification.audit_artifact)
    {
        return Err(ineligible(
            "the planned Writer no longer names its audit artifact",
        ));
    }
    crate::recovery_contracts::validate_code_change_writer_start(
        project_root,
        item,
        &attempt.id,
        &classification.writer_task_id,
    )
    .map_err(|error| anyhow::anyhow!("{error}"))?;
    Ok(())
}

fn ensure_attempt_resume_ready(
    attempt: &crate::work_model::Attempt,
    legacy_review_evidence_round_cap: bool,
    allows_planned_approach_correction: bool,
) -> Result<(), WorkModelError> {
    if legacy_review_evidence_round_cap || allows_planned_approach_correction {
        Ok(())
    } else {
        attempt.ensure_resume_ready()
    }
}

fn ensure_tester_no_sandbox_eligible(
    item: &WorkItem,
    id: &str,
    attempt_id: &str,
    tester_no_sandbox: bool,
) -> Result<()> {
    if !tester_no_sandbox {
        return Ok(());
    }
    let attempt = item
        .attempts
        .iter()
        .find(|attempt| attempt.id == attempt_id)
        .ok_or_else(|| anyhow::anyhow!("Attempt {attempt_id:?} not found for Work Item {id:?}"))?;
    if attempt.status != AttemptStatus::NeedsUser
        || attempt.pause_kind.as_ref() != Some(&crate::work_model::PauseKind::TesterHarness)
        || attempt.pause_recovery != Some(PauseRecovery::TesterNoSandbox)
    {
        bail!(
            "--no-sandbox is allowed only when Attempt {attempt_id:?} records a Tester no-sandbox recovery"
        );
    }
    Ok(())
}

// -------------------------------------------------------------------------
// Generation-checked claim / launch / reconcile
// -------------------------------------------------------------------------

/// The lock path a coordinator worker holds while a dispatch's bound Attempt
/// executes. Its liveness is the durable signal that a claim is live.
pub fn dispatch_lease_path(project_root: &Path, work_item_id: &str) -> PathBuf {
    project_root
        .join(".fluent/work/scheduler/leases")
        .join(format!("{work_item_id}.lock"))
}

fn claim_is_live(project_root: &Path, work_item_id: &str) -> bool {
    crate::lease::is_leased(&dispatch_lease_path(project_root, work_item_id))
}

/// Atomically claim the active queued dispatch, binding `bound_attempt_id` and
/// bumping the generation before its Attempt is ensured. Returns `None` when
/// no queued dispatch is available.
pub fn claim(
    project_root: &Path,
    work_item_id: &str,
    bound_attempt_id: &str,
) -> Result<Option<DispatchToken>> {
    if !work_item_exists(project_root, work_item_id) {
        bail!("Work Item {work_item_id:?} not found");
    }
    let store = WorkModelStore::new(project_root);
    let _model_lock = store.lock_work_item_model(work_item_id)?;
    let item = store.read_work_item_under_model_lock(work_item_id)?;
    // Claim must apply the same narrow legacy recognition as queue admission
    // and foreground execution before it changes dispatch state. A fresh
    // dispatch has no Attempt until the claim is durably bound, so there is no
    // legacy recovery state to inspect in that case.
    if item
        .attempts
        .iter()
        .any(|attempt| attempt.id == bound_attempt_id)
    {
        let _ = crate::recovery_contracts::inspect_legacy_writer_correction_assignment(
            project_root,
            &item,
            bound_attempt_id,
        )?;
    }
    #[cfg(test)]
    crate::test_lock_probe::reach("queue-lifecycle", work_item_id, "claim", "ELIGIBLE");
    // The ledger file is atomically replaced, so this read is a safe admission
    // hint. The locked reread below proves that the queue did not change while
    // the evidence checks ran.
    let initial_ledger = read_ledger(project_root, work_item_id)?;
    if let Some(active) = initial_ledger.as_ref().and_then(DispatchLedger::active)
        && active.status == DispatchStatus::Queued
        && active.intent == DispatchIntent::ResumeAttempt
    {
        let persisted_attempt_id = active.bound_attempt_id.as_deref().ok_or_else(|| {
            anyhow::anyhow!(
                "resume dispatch {:?} has no bound Attempt",
                active.dispatch_id
            )
        })?;
        ensure_resume_eligible(
            project_root,
            &item,
            work_item_id,
            persisted_attempt_id,
            false,
            false,
            false,
            false,
        )?;
    }
    let _lock = lock_queue(project_root)?;
    let mut ledger = match read_ledger(project_root, work_item_id)? {
        Some(ledger) => ledger,
        None => return Ok(None),
    };
    if initial_ledger.as_ref() != Some(&ledger) {
        bail!("Work Item {work_item_id:?} queue changed during resume admission; retry the claim");
    }

    let Some(active) = ledger.active_mut() else {
        return Ok(None);
    };
    if active.status != DispatchStatus::Queued {
        return Ok(None);
    }

    if let Some(attempt) = item
        .attempts
        .iter()
        .find(|attempt| attempt.id == bound_attempt_id)
    {
        if attempt.cancellation_requested()
            && (active.intent != DispatchIntent::ResumeAttempt
                || active.resume_cancellation_requested_at.as_deref()
                    != attempt
                        .cancellation
                        .as_ref()
                        .map(|stop| stop.requested_at.as_str()))
        {
            bail!(
                "dispatch {} predates the current cancellation; remove this unclaimed dispatch and authorize a new queue resume",
                active.dispatch_id
            );
        }
    }

    match active.intent {
        DispatchIntent::Run => {
            if active.tester_no_sandbox {
                bail!(
                    "run dispatch {:?} cannot carry Tester no-sandbox approval",
                    active.dispatch_id
                );
            }
            ensure_lifecycle_eligible(&item, work_item_id)?;
        }
        DispatchIntent::ResumeAttempt => {
            let persisted_attempt_id = active.bound_attempt_id.as_deref().ok_or_else(|| {
                anyhow::anyhow!(
                    "resume dispatch {:?} has no bound Attempt",
                    active.dispatch_id
                )
            })?;
            if persisted_attempt_id != bound_attempt_id {
                bail!(
                    "resume dispatch {:?} is bound to Attempt {persisted_attempt_id:?}, not {bound_attempt_id:?}",
                    active.dispatch_id
                );
            }
            ensure_tester_no_sandbox_eligible(
                &item,
                work_item_id,
                persisted_attempt_id,
                active.tester_no_sandbox,
            )?;
        }
    }
    if let Some(persisted) = active.bound_attempt_id.as_deref() {
        if persisted != bound_attempt_id {
            bail!(
                "dispatch {:?} is bound to Attempt {persisted:?}, not {bound_attempt_id:?}",
                active.dispatch_id
            );
        }
    } else {
        active.bound_attempt_id = Some(bound_attempt_id.to_string());
    }

    active.status = DispatchStatus::Claimed;
    active.claimed_at = Some(crate::work_model::now_iso8601());
    active.generation += 1;

    let persisted_attempt_id = active
        .bound_attempt_id
        .clone()
        .expect("claim persisted an Attempt binding");

    let token = DispatchToken {
        work_item_id: work_item_id.to_string(),
        dispatch_id: active.dispatch_id.clone(),
        generation: active.generation,
        bound_attempt_id: persisted_attempt_id,
        priority: active.priority,
        tester_no_sandbox: active.tester_no_sandbox,
    };
    write_ledger(project_root, &ledger)?;
    Ok(Some(token))
}

/// Transition a claimed dispatch to `running` on launch, verifying the token's
/// generation still matches so a concurrent change fails closed. Returns a
/// refreshed token carrying the new generation.
pub fn mark_running(project_root: &Path, token: &DispatchToken) -> Result<DispatchToken> {
    with_matching_active(project_root, token, |dispatch| {
        if dispatch.status != DispatchStatus::Claimed {
            bail!(
                "dispatch {:?} is {} and cannot transition to running",
                dispatch.dispatch_id,
                dispatch.status
            );
        }
        dispatch.status = DispatchStatus::Running;
        dispatch.generation += 1;
        Ok(())
    })?;
    Ok(DispatchToken {
        generation: token.generation + 1,
        ..token.clone()
    })
}

/// Reconcile a dispatch to a terminal disposition from its Attempt's outcome,
/// verifying the token's generation still matches.
pub fn reconcile(project_root: &Path, token: &DispatchToken, status: DispatchStatus) -> Result<()> {
    debug_assert!(status.is_terminal(), "reconcile expects a terminal status");
    with_matching_active(project_root, token, |dispatch| {
        dispatch.status = status;
        dispatch.generation += 1;
        Ok(())
    })
}

// -------------------------------------------------------------------------
// Recovery transitions on the latest dispatch
// -------------------------------------------------------------------------
//
// Recovery acts on the ledger's latest dispatch (which may already read as a
// terminal disposition, e.g. a resumed needs-user Attempt) rather than only on
// an active dispatch. Each transition is guarded by the queue lock and bumps
// the generation so an in-flight token fails closed.

/// Return a stale claimed or running dispatch to `queued`. When `clear_binding`
/// is set the bound Attempt is dropped so the next claim allocates a fresh one;
/// otherwise the binding is preserved so the next claim resumes the same
/// Attempt.
pub fn requeue_active(project_root: &Path, work_item_id: &str, clear_binding: bool) -> Result<()> {
    mutate_latest(project_root, work_item_id, |dispatch| {
        dispatch.status = DispatchStatus::Queued;
        dispatch.claimed_at = None;
        if clear_binding {
            dispatch.bound_attempt_id = None;
        }
        dispatch.generation += 1;
    })
}

/// Return the named dispatch to `queued` only if it is still the same latest
/// dispatch the caller inspected. A concurrent queue operation makes the
/// operation a no-op, so recovery cannot rewrite a newer authorization.
pub fn requeue_if_current(
    project_root: &Path,
    work_item_id: &str,
    dispatch_id: &str,
    generation: u64,
    clear_binding: bool,
) -> Result<bool> {
    mutate_latest_if_current(
        project_root,
        work_item_id,
        dispatch_id,
        generation,
        |dispatch| {
            let changed = dispatch.status != DispatchStatus::Queued
                || dispatch.claimed_at.is_some()
                || (clear_binding && dispatch.bound_attempt_id.is_some());
            if !changed {
                return Ok(false);
            }
            dispatch.status = DispatchStatus::Queued;
            dispatch.claimed_at = None;
            if clear_binding {
                dispatch.bound_attempt_id = None;
            }
            dispatch.generation += 1;
            Ok(true)
        },
    )
}

/// Force the latest dispatch to a terminal disposition during recovery, without
/// a matching token. Used to reconcile a stale claim from its Attempt's outcome
/// or to reflect a human-resumed suspension.
pub fn reconcile_active(
    project_root: &Path,
    work_item_id: &str,
    status: DispatchStatus,
) -> Result<()> {
    debug_assert!(status.is_terminal(), "reconcile expects a terminal status");
    mutate_latest(project_root, work_item_id, |dispatch| {
        dispatch.status = status;
        dispatch.generation += 1;
    })
}

/// Reconcile the named dispatch only if it is still the same latest dispatch
/// the caller inspected. This is the generation-checked form used by
/// scheduler recovery after it reads Work-model state without holding the
/// queue lock.
pub fn reconcile_if_current(
    project_root: &Path,
    work_item_id: &str,
    dispatch_id: &str,
    generation: u64,
    status: DispatchStatus,
) -> Result<bool> {
    mutate_latest_if_current(
        project_root,
        work_item_id,
        dispatch_id,
        generation,
        |dispatch| {
            if dispatch.status == status {
                return Ok(false);
            }
            dispatch.status = status;
            dispatch.generation += 1;
            Ok(true)
        },
    )
}

/// Block the latest dispatch with a reason, leaving it as durable evidence for
/// operator inspection.
pub fn block_active(project_root: &Path, work_item_id: &str, reason: &str) -> Result<()> {
    mutate_latest(project_root, work_item_id, |dispatch| {
        dispatch.status = DispatchStatus::Blocked;
        dispatch.blocked_reason = Some(reason.to_string());
        dispatch.generation += 1;
    })
}

/// Block the named dispatch only if it is still the same latest dispatch the
/// caller inspected. A concurrent queue operation makes the operation a
/// no-op, preserving the newer dispatch for its own reconciliation.
pub fn block_if_current(
    project_root: &Path,
    work_item_id: &str,
    dispatch_id: &str,
    generation: u64,
    reason: &str,
) -> Result<bool> {
    mutate_latest_if_current(
        project_root,
        work_item_id,
        dispatch_id,
        generation,
        |dispatch| {
            if dispatch.status == DispatchStatus::Blocked
                && dispatch.blocked_reason.as_deref() == Some(reason)
            {
                return Ok(false);
            }
            dispatch.status = DispatchStatus::Blocked;
            dispatch.blocked_reason = Some(reason.to_string());
            dispatch.generation += 1;
            Ok(true)
        },
    )
}

/// Cancel the latest dispatch — used when queued Work becomes abandoned before
/// any Attempt is created.
pub fn cancel_active(project_root: &Path, work_item_id: &str) -> Result<()> {
    mutate_latest(project_root, work_item_id, |dispatch| {
        dispatch.status = DispatchStatus::Canceled;
        dispatch.generation += 1;
    })
}

/// Cancel the named dispatch only if it is still the same latest dispatch the
/// caller inspected. A concurrent queue operation makes the operation a
/// no-op, preserving the newer dispatch for its own reconciliation.
pub fn cancel_if_current(
    project_root: &Path,
    work_item_id: &str,
    dispatch_id: &str,
    generation: u64,
) -> Result<bool> {
    mutate_latest_if_current(
        project_root,
        work_item_id,
        dispatch_id,
        generation,
        |dispatch| {
            if dispatch.status == DispatchStatus::Canceled {
                return Ok(false);
            }
            dispatch.status = DispatchStatus::Canceled;
            dispatch.generation += 1;
            Ok(true)
        },
    )
}

fn mutate_latest(
    project_root: &Path,
    work_item_id: &str,
    mutate: impl FnOnce(&mut Dispatch),
) -> Result<()> {
    let _lock = lock_queue(project_root)?;
    let mut ledger = match read_ledger(project_root, work_item_id)? {
        Some(ledger) => ledger,
        None => bail!("Work Item {work_item_id:?} has no queue ledger"),
    };
    let Some(dispatch) = ledger.dispatches.last_mut() else {
        bail!("Work Item {work_item_id:?} has no dispatch");
    };
    mutate(dispatch);
    write_ledger(project_root, &ledger)?;
    Ok(())
}

fn mutate_latest_if_current(
    project_root: &Path,
    work_item_id: &str,
    dispatch_id: &str,
    generation: u64,
    mutate: impl FnOnce(&mut Dispatch) -> Result<bool>,
) -> Result<bool> {
    let _lock = lock_queue(project_root)?;
    let mut ledger = match read_ledger(project_root, work_item_id)? {
        Some(ledger) => ledger,
        None => return Ok(false),
    };
    let Some(dispatch) = ledger.dispatches.last_mut() else {
        return Ok(false);
    };
    if dispatch.dispatch_id != dispatch_id || dispatch.generation != generation {
        return Ok(false);
    }
    let changed = mutate(dispatch)?;
    if changed {
        write_ledger(project_root, &ledger)?;
    }
    Ok(changed)
}

fn with_matching_active(
    project_root: &Path,
    token: &DispatchToken,
    mutate: impl FnOnce(&mut Dispatch) -> Result<()>,
) -> Result<()> {
    let _lock = lock_queue(project_root)?;
    let mut ledger = match read_ledger(project_root, &token.work_item_id)? {
        Some(ledger) => ledger,
        None => bail!("Work Item {:?} has no queue ledger", token.work_item_id),
    };
    let Some(active) = ledger.active_mut() else {
        bail!("Work Item {:?} has no active dispatch", token.work_item_id);
    };
    if active.dispatch_id != token.dispatch_id {
        bail!(
            "dispatch changed under token: expected {:?}, found {:?}",
            token.dispatch_id,
            active.dispatch_id
        );
    }
    if active.generation != token.generation {
        bail!(
            "dispatch {:?} generation changed: expected {}, found {}",
            token.dispatch_id,
            token.generation,
            active.generation
        );
    }
    if active.tester_no_sandbox != token.tester_no_sandbox {
        bail!(
            "dispatch {:?} Tester no-sandbox approval changed under token",
            token.dispatch_id
        );
    }
    mutate(active)?;
    write_ledger(project_root, &ledger)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::work_model::{
        ArtifactRef, AttemptLearning, AttemptReviewState, EvidenceAttachment, EvidenceRecovery,
        EvidenceRecoveryState, EvidenceReviewContext, EvidenceReviewTarget, LearningFailureKind,
        MergeCandidateMergeStatus, MergeReviewState, PauseKind, TaskKind, TaskOutput, TaskStatus,
    };

    fn setup_project(tmp: &Path) {
        fs::create_dir_all(tmp.join(".fluent/work/items")).unwrap();
    }

    fn write_ready_work_item(tmp: &Path, id: &str) {
        fs::write(
            tmp.join(format!(".fluent/work/items/{id}.json")),
            format!(r#"{{"id": "{id}", "title": "Test"}}"#),
        )
        .unwrap();
    }

    fn write_legacy_entry(tmp: &Path, id: &str, priority: i64, status: &str, queued_at: &str) {
        let dir = queue_dir(tmp);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join(format!("{id}.json")),
            format!(
                r#"{{"work_item_id":"{id}","queued_at":"{queued_at}","priority":{priority},"status":"{status}"}}"#
            ),
        )
        .unwrap();
    }

    fn write_work_item_json(tmp: &Path, id: &str, extra_fields: &str) {
        let json = format!(r#"{{"id": "{id}", "title": "Test"{extra_fields}}}"#);
        serde_json::from_str::<WorkItem>(&json).expect("fixture must be a valid Work record");
        fs::write(tmp.join(format!(".fluent/work/items/{id}.json")), json).unwrap();
    }

    fn write_planned_attempt(tmp: &Path, id: &str, attempt_id: &str) {
        let store = WorkModelStore::new(tmp);
        let mut item = WorkItem::planned(id, "Test");
        item.add_initial_attempt(attempt_id).unwrap();
        store.create_work_item(&item).unwrap();
    }

    fn write_pending_evidence_review(tmp: &Path, id: &str, attempt_id: &str) {
        let store = WorkModelStore::new(tmp);
        let candidate =
            crate::git::run_stdout(tmp, &["rev-parse", "HEAD"], "read candidate").unwrap();
        let mut item = WorkItem::planned(id, "Resume pending evidence Review");
        item.add_initial_attempt(attempt_id).unwrap();
        let writer = &mut item.attempts[0].tasks[0];
        writer.status = TaskStatus::Complete;
        writer.output = Some(TaskOutput {
            workspace_id: "candidate".to_string(),
            workspace_path: tmp.display().to_string(),
            source_branch: "main".to_string(),
            base_commit: Some(candidate.clone()),
            commit: candidate.clone(),
            no_change: None,
            learner_canonicalization: None,
        });
        item.add_review_tasks(attempt_id, &["tests"]).unwrap();
        let prior = item.attempts[0].tasks.last_mut().unwrap();
        prior.status = TaskStatus::Complete;
        let prior_path = format!("{}/review.md", prior.artifact_area.as_ref().unwrap().path);
        let mut targeted = prior.clone();
        targeted.id = format!("{attempt_id}-evidence-proof-tests");
        targeted.status = TaskStatus::Planned;
        targeted.artifact_area = Some(crate::work_model::TaskArtifactArea {
            path: crate::work_model::work_artifact_path(id, attempt_id, &targeted.id),
        });
        targeted.started_at = None;
        targeted.completed_at = None;
        let attachment = EvidenceAttachment {
            snapshot_path: format!(
                ".fluent/work/artifacts/{id}/{attempt_id}/host-evidence/proof.json"
            ),
            digest: "sha256:proof".to_string(),
        };
        let recovery_id = "host-evidence-proof".to_string();
        targeted.evidence_review_context = Some(EvidenceReviewContext {
            recovery_id: recovery_id.clone(),
            candidate_commit: candidate.clone(),
            attachment: attachment.clone(),
            prior_review_artifact: prior_path.clone(),
        });
        let attempt = &mut item.attempts[0];
        attempt.tasks.push(targeted.clone());
        attempt.status = AttemptStatus::Reviewing;
        attempt.review_state = Some(AttemptReviewState::NotReviewed);
        attempt.evidence_recoveries.push(EvidenceRecovery {
            carried_finding: None,
            required_check: None,
            id: recovery_id,
            candidate_commit: candidate,
            attachment,
            targets: vec![EvidenceReviewTarget {
                role: "tests".to_string(),
                prior_review_artifact: prior_path,
                review_task_id: Some(targeted.id),
                review_digest: None,
            }],
            state: EvidenceRecoveryState::Reviewing,
            created_at: "2026-08-25T10:00:00Z".to_string(),
            code_change_classification: None,
        });
        store.create_work_item(&item).unwrap();
    }

    fn suspend_attempt(tmp: &Path, id: &str, attempt_id: &str, pause_kind: PauseKind) {
        let store = WorkModelStore::new(tmp);
        store
            .mutate_work_item(id, |item| {
                let attempt = item
                    .attempts
                    .iter_mut()
                    .find(|attempt| attempt.id == attempt_id)
                    .unwrap();
                attempt.status = AttemptStatus::NeedsUser;
                attempt.pause_kind = Some(pause_kind);
                attempt.pause_recovery = None;
                attempt.tasks[0].status = TaskStatus::NeedsUser;
                Ok(())
            })
            .unwrap();
    }

    fn write_scheduler_suspension(tmp: &Path, id: &str, attempt_id: &str) {
        write_planned_attempt(tmp, id, attempt_id);
        add(tmp, id, Some(3)).unwrap();
        claim(tmp, id, attempt_id).unwrap().unwrap();
        suspend_attempt(tmp, id, attempt_id, PauseKind::WriterCompletion);
        reconcile_active(tmp, id, DispatchStatus::NeedsUser).unwrap();
    }

    fn write_tester_suspension(tmp: &Path, id: &str, attempt_id: &str) {
        write_scheduler_suspension(tmp, id, attempt_id);
        suspend_attempt(tmp, id, attempt_id, PauseKind::TesterHarness);
        WorkModelStore::new(tmp)
            .mutate_work_item(id, |item| {
                item.attempts
                    .iter_mut()
                    .find(|attempt| attempt.id == attempt_id)
                    .unwrap()
                    .pause_recovery = Some(PauseRecovery::TesterNoSandbox);
                Ok(())
            })
            .unwrap();
    }

    fn write_blocked_relaunchable_learner(tmp: &Path, id: &str, attempt_id: &str) -> PathBuf {
        crate::git::run(tmp, &["init", "-q"], "initialize learner recovery project").unwrap();
        crate::git::run(
            tmp,
            &["config", "user.name", "Test Author"],
            "configure author",
        )
        .unwrap();
        crate::git::run(
            tmp,
            &["config", "user.email", "test@example.com"],
            "configure email",
        )
        .unwrap();
        fs::write(tmp.join("README.md"), "base\n").unwrap();
        crate::git::run(tmp, &["add", "README.md"], "stage base").unwrap();
        crate::git::run(tmp, &["commit", "-q", "-m", "Create base"], "commit base").unwrap();

        let workspace_path = crate::work_model::initial_candidate_workspace_path(id, attempt_id);
        let workspace = tmp.parent().unwrap().join(
            Path::new(&workspace_path)
                .file_name()
                .expect("candidate workspace name"),
        );
        crate::git::run(
            tmp,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "work/learner-retry",
                workspace.to_str().unwrap(),
                "HEAD",
            ],
            "create learner recovery candidate",
        )
        .unwrap();
        let candidate =
            crate::git::run_stdout(&workspace, &["rev-parse", "HEAD"], "candidate").unwrap();

        write_planned_attempt(tmp, id, attempt_id);
        add(tmp, id, Some(3)).unwrap();
        claim(tmp, id, attempt_id).unwrap().unwrap();
        WorkModelStore::new(tmp)
            .mutate_work_item(id, |item| {
                let attempt = &mut item.attempts[0];
                let writer = &mut attempt.tasks[0];
                writer.status = TaskStatus::Complete;
                writer.output = Some(TaskOutput {
                    workspace_id: "candidate".to_string(),
                    workspace_path: workspace_path.clone(),
                    source_branch: "work/learner-retry".to_string(),
                    base_commit: None,
                    commit: candidate.clone(),
                    no_change: None,
                    learner_canonicalization: None,
                });
                attempt.status = AttemptStatus::Complete;
                attempt.review_state = Some(AttemptReviewState::Passed);
                attempt.learning = Some(AttemptLearning::failed(1, "retry learner"));
                item.create_or_get_merge_candidate(attempt_id)?;
                Ok(())
            })
            .unwrap();
        reconcile_active(tmp, id, DispatchStatus::Blocked).unwrap();
        workspace
    }

    fn write_uncertain_review_suspension(tmp: &Path, id: &str, attempt_id: &str) {
        crate::git::run(tmp, &["init", "-q"], "initialize queue recovery project").unwrap();
        crate::git::run(
            tmp,
            &["config", "user.name", "Test Author"],
            "configure author",
        )
        .unwrap();
        crate::git::run(
            tmp,
            &["config", "user.email", "test@example.com"],
            "configure email",
        )
        .unwrap();
        fs::write(tmp.join("README.md"), "base\n").unwrap();
        crate::git::run(tmp, &["add", "README.md"], "stage base").unwrap();
        crate::git::run(tmp, &["commit", "-q", "-m", "Create base"], "commit base").unwrap();
        let workspace_path = crate::work_model::initial_candidate_workspace_path(id, attempt_id);
        let workspace = tmp.parent().unwrap().join(
            Path::new(&workspace_path)
                .file_name()
                .expect("candidate workspace name"),
        );
        crate::git::run(
            tmp,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "work/attempt-1",
                workspace.to_str().unwrap(),
                "HEAD",
            ],
            "create candidate worktree",
        )
        .unwrap();
        let candidate =
            crate::git::run_stdout(&workspace, &["rev-parse", "HEAD"], "candidate").unwrap();

        write_scheduler_suspension(tmp, id, attempt_id);
        let store = WorkModelStore::new(tmp);
        store
            .mutate_work_item(id, |item| {
                let writer = &mut item.attempts[0].tasks[0];
                writer.status = TaskStatus::Complete;
                writer.output = Some(TaskOutput {
                    workspace_id: "candidate".to_string(),
                    workspace_path: workspace_path.clone(),
                    source_branch: "work/attempt-1".to_string(),
                    base_commit: None,
                    commit: candidate.clone(),
                    no_change: None,
                    learner_canonicalization: None,
                });
                item.add_review_tasks(attempt_id, crate::review::REVIEWERS)?;
                Ok(())
            })
            .unwrap();
        store
            .mutate_work_item(id, |item| {
                let attempt = &mut item.attempts[0];
                for task in &mut attempt.tasks {
                    if task.kind == TaskKind::Review {
                        task.status = TaskStatus::Complete;
                    }
                }
                attempt.review_state = Some(AttemptReviewState::Uncertain);
                crate::work_model::suspend_attempt(attempt, PauseKind::Uncertain);
                Ok(())
            })
            .unwrap();
        let item = store.read_work_item(id).unwrap();
        for task in item.attempts[0]
            .tasks
            .iter()
            .filter(|task| task.kind == TaskKind::Review)
        {
            let directory = tmp.join(&task.artifact_area.as_ref().unwrap().path);
            fs::create_dir_all(&directory).unwrap();
            let verdict = if task.role == "skills" {
                "UNCERTAIN"
            } else {
                "PASS"
            };
            fs::write(directory.join("review.md"), format!("Verdict: {verdict}\n")).unwrap();
        }
    }

    /// Overwrite a ledger directly so a test can start from an arbitrary
    /// disposition without driving the full scheduler pipeline.
    fn put_ledger(tmp: &Path, ledger: &DispatchLedger) {
        write_ledger(tmp, ledger).unwrap();
    }

    #[test]
    fn legacy_queue_entry_migrates_to_dispatch_history() {
        let dir = tempfile::tempdir().unwrap();
        setup_project(dir.path());
        write_ready_work_item(dir.path(), "wi-legacy");
        write_legacy_entry(dir.path(), "wi-legacy", 7, "done", "2026-06-13T10:00:00Z");

        let ledger = read_ledger(dir.path(), "wi-legacy").unwrap().unwrap();
        assert_eq!(ledger.dispatches.len(), 1);
        let first = &ledger.dispatches[0];
        // Priority and queue time are preserved.
        assert_eq!(first.priority, 7);
        assert_eq!(first.queued_at, "2026-06-13T10:00:00Z");
        // Legacy `done` maps to `candidate-ready`.
        assert_eq!(first.status, DispatchStatus::CandidateReady);
        // The migrated dispatch is terminal history — no active dispatch, and
        // no Attempt was created by reading it.
        assert!(ledger.active().is_none());
    }

    #[test]
    fn ordinary_dispatch_schema_omits_default_resume_fields() {
        let dispatch = Dispatch::new_queued("wi-1", 0, None, 1);
        let value = serde_json::to_value(&dispatch).unwrap();
        assert!(value.get("intent").is_none());
        assert!(value.get("resume_of_dispatch_id").is_none());
        assert!(value.get("tester_no_sandbox").is_none());
        assert!(value.get("resume_cancellation_requested_at").is_none());

        let decoded: Dispatch = serde_json::from_value(value).unwrap();
        assert_eq!(decoded.intent, DispatchIntent::Run);
        assert_eq!(decoded.resume_of_dispatch_id, None);
        assert!(!decoded.tester_no_sandbox);
        assert_eq!(decoded.resume_cancellation_requested_at, None);
    }

    #[test]
    fn tester_no_sandbox_resume_rejects_non_tester_pause_without_mutation() {
        let dir = tempfile::tempdir().unwrap();
        setup_project(dir.path());
        write_scheduler_suspension(dir.path(), "wi-1", "attempt-1");
        let before = fs::read(queue_file(dir.path(), "wi-1")).unwrap();

        let error =
            resume_with_tester_no_sandbox(dir.path(), "wi-1", "attempt-1", None, true).unwrap_err();
        assert!(error.to_string().contains("Tester no-sandbox recovery"));
        assert_eq!(fs::read(queue_file(dir.path(), "wi-1")).unwrap(), before);
    }

    #[test]
    fn tester_no_sandbox_resume_rejects_an_unapproved_tester_pause() {
        let dir = tempfile::tempdir().unwrap();
        setup_project(dir.path());
        write_scheduler_suspension(dir.path(), "wi-1", "attempt-1");
        suspend_attempt(dir.path(), "wi-1", "attempt-1", PauseKind::TesterHarness);
        let before = fs::read(queue_file(dir.path(), "wi-1")).unwrap();

        let error =
            resume_with_tester_no_sandbox(dir.path(), "wi-1", "attempt-1", None, true).unwrap_err();

        assert!(error.to_string().contains("Tester no-sandbox recovery"));
        assert_eq!(fs::read(queue_file(dir.path(), "wi-1")).unwrap(), before);
    }

    #[test]
    fn tester_no_sandbox_approval_is_durable_and_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        setup_project(dir.path());
        write_tester_suspension(dir.path(), "wi-1", "attempt-1");

        assert_eq!(
            resume_with_tester_no_sandbox(dir.path(), "wi-1", "attempt-1", Some(6), true).unwrap(),
            ResumeDispatchOutcome::Created
        );
        let ledger = read_ledger(dir.path(), "wi-1").unwrap().unwrap();
        let resumed = ledger.active().unwrap();
        assert!(resumed.tester_no_sandbox);
        assert_eq!(
            serde_json::to_value(resumed).unwrap()["tester_no_sandbox"],
            true
        );

        assert_eq!(
            resume_with_tester_no_sandbox(dir.path(), "wi-1", "attempt-1", None, true).unwrap(),
            ResumeDispatchOutcome::ExistingMatching
        );
        let mismatch = resume(dir.path(), "wi-1", "attempt-1", None).unwrap_err();
        assert!(mismatch.to_string().contains("already queued"));
    }

    #[test]
    fn tester_no_sandbox_claim_revalidates_the_tester_pause() {
        let dir = tempfile::tempdir().unwrap();
        setup_project(dir.path());
        write_tester_suspension(dir.path(), "wi-1", "attempt-1");
        resume_with_tester_no_sandbox(dir.path(), "wi-1", "attempt-1", None, true).unwrap();

        suspend_attempt(dir.path(), "wi-1", "attempt-1", PauseKind::WriterCompletion);
        let error = claim(dir.path(), "wi-1", "attempt-1").unwrap_err();
        assert!(error.to_string().contains("Tester no-sandbox recovery"));
        assert_eq!(
            read_ledger(dir.path(), "wi-1")
                .unwrap()
                .unwrap()
                .active()
                .unwrap()
                .status,
            DispatchStatus::Queued
        );

        suspend_attempt(dir.path(), "wi-1", "attempt-1", PauseKind::TesterHarness);
        WorkModelStore::new(dir.path())
            .mutate_work_item("wi-1", |item| {
                item.attempts[0].pause_recovery = Some(PauseRecovery::TesterNoSandbox);
                Ok(())
            })
            .unwrap();
        let token = claim(dir.path(), "wi-1", "attempt-1").unwrap().unwrap();
        assert!(token.tester_no_sandbox);
    }

    #[test]
    fn claim_rejects_no_sandbox_approval_on_ordinary_dispatch() {
        let dir = tempfile::tempdir().unwrap();
        setup_project(dir.path());
        write_planned_attempt(dir.path(), "wi-1", "attempt-1");
        add(dir.path(), "wi-1", None).unwrap();

        let mut ledger = read_ledger(dir.path(), "wi-1").unwrap().unwrap();
        ledger.active_mut().unwrap().tester_no_sandbox = true;
        put_ledger(dir.path(), &ledger);

        let error = claim(dir.path(), "wi-1", "attempt-1").unwrap_err();
        assert!(
            error
                .to_string()
                .contains("cannot carry Tester no-sandbox approval")
        );
        assert_eq!(
            read_ledger(dir.path(), "wi-1")
                .unwrap()
                .unwrap()
                .active()
                .unwrap()
                .status,
            DispatchStatus::Queued
        );
    }

    #[test]
    fn resume_records_exact_cancellation_authority() {
        let dir = tempfile::tempdir().unwrap();
        setup_project(dir.path());
        write_scheduler_suspension(dir.path(), "wi-1", "attempt-1");
        let store = WorkModelStore::new(dir.path());
        crate::work_attempt_loop::cancel_local_attempt(dir.path(), &store, "wi-1", "attempt-1")
            .unwrap();
        let stopped = store.read_work_item("wi-1").unwrap();
        let stop = stopped.attempts[0].cancellation.as_ref().unwrap();
        resume(dir.path(), "wi-1", "attempt-1", None).unwrap();
        let ledger = read_ledger(dir.path(), "wi-1").unwrap().unwrap();
        let dispatch = serde_json::to_value(ledger.active().unwrap()).unwrap();
        assert_eq!(
            dispatch["resume_cancellation_requested_at"],
            stop.requested_at
        );
        assert_eq!(
            store.read_work_item("wi-1").unwrap(),
            stopped,
            "admission records authorization without reopening the Attempt"
        );
        let dispatch_id = ledger.active().unwrap().dispatch_id.clone();
        assert!(
            cancellation_resume_for_dispatch(dir.path(), "wi-1", "attempt-1", &dispatch_id)
                .is_err()
        );
        let token = claim(dir.path(), "wi-1", "attempt-1").unwrap().unwrap();
        mark_running(dir.path(), &token).unwrap();
        assert_eq!(
            cancellation_resume_for_dispatch(dir.path(), "wi-1", "attempt-1", &dispatch_id)
                .unwrap(),
            Some(stop.requested_at.clone())
        );
        assert!(
            cancellation_resume_for_dispatch(dir.path(), "wi-1", "attempt-2", &dispatch_id)
                .is_err()
        );
        assert!(
            cancellation_resume_for_dispatch(dir.path(), "wi-1", "attempt-1", "dispatch-1")
                .is_err()
        );
        store
            .mutate_work_item("wi-1", |item| {
                let attempt = &mut item.attempts[0];
                attempt.cancellation.as_mut().unwrap().resumed_at =
                    Some(crate::work_model::now_iso8601());
                crate::work_model::reopen_attempt(attempt);
                Ok(())
            })
            .unwrap();
        crate::work_attempt_loop::cancel_local_attempt(dir.path(), &store, "wi-1", "attempt-1")
            .unwrap();
        let before = read_ledger(dir.path(), "wi-1").unwrap();
        assert!(resume(dir.path(), "wi-1", "attempt-1", None).is_err());
        assert_eq!(
            read_ledger(dir.path(), "wi-1").unwrap(),
            before,
            "repeating resume cannot refresh an existing dispatch's authority"
        );
    }

    #[test]
    fn resume_preserves_history_binds_exact_attempt_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        setup_project(dir.path());
        write_scheduler_suspension(dir.path(), "wi-1", "attempt-1");

        assert_eq!(
            resume(dir.path(), "wi-1", "attempt-1", Some(8)).unwrap(),
            ResumeDispatchOutcome::Created
        );
        let first = read_ledger(dir.path(), "wi-1").unwrap().unwrap();
        assert_eq!(first.dispatches.len(), 2);
        assert_eq!(first.dispatches[0].status, DispatchStatus::NeedsUser);
        let resumed = first.active().unwrap();
        assert_eq!(resumed.intent, DispatchIntent::ResumeAttempt);
        assert_eq!(resumed.bound_attempt_id.as_deref(), Some("attempt-1"));
        assert_eq!(resumed.resume_of_dispatch_id.as_deref(), Some("dispatch-1"));
        assert_eq!(resumed.priority, 8);

        assert_eq!(
            resume(dir.path(), "wi-1", "attempt-1", None).unwrap(),
            ResumeDispatchOutcome::ExistingMatching
        );
        assert_eq!(
            resume(dir.path(), "wi-1", "attempt-1", Some(11)).unwrap(),
            ResumeDispatchOutcome::ExistingMatching
        );
        WorkModelStore::new(dir.path())
            .mutate_work_item("wi-1", |item| {
                item.attempts[0].status = AttemptStatus::Executing;
                item.attempts[0].pause_kind = None;
                item.attempts[0].tasks[0].status = TaskStatus::Planned;
                Ok(())
            })
            .unwrap();
        assert_eq!(
            resume(dir.path(), "wi-1", "attempt-1", None).unwrap(),
            ResumeDispatchOutcome::ExistingMatching,
            "receipt-loss replay recognizes its active dispatch after lifecycle advances"
        );
        let replayed = read_ledger(dir.path(), "wi-1").unwrap().unwrap();
        assert_eq!(replayed.dispatches.len(), 2);
        assert_eq!(replayed.active().unwrap().priority, 11);
    }

    #[test]
    fn resume_reclassifies_legacy_codex_auth_pause_before_admission() {
        let dir = tempfile::tempdir().unwrap();
        setup_project(dir.path());
        write_planned_attempt(dir.path(), "wi-auth", "attempt-1");
        add(dir.path(), "wi-auth", Some(3)).unwrap();
        claim(dir.path(), "wi-auth", "attempt-1").unwrap().unwrap();

        let store = WorkModelStore::new(dir.path());
        store
            .mutate_work_item("wi-auth", |item| {
                let attempt = &mut item.attempts[0];
                attempt.coder_mapping.write.coder = crate::coder::CoderKind::Codex;
                attempt.status = AttemptStatus::NeedsUser;
                attempt.pause_kind = Some(PauseKind::RoundCap);
                attempt.review_state = Some(AttemptReviewState::Failed);
                let task = &mut attempt.tasks[0];
                task.status = TaskStatus::Failed;
                task.artifact_area = Some(crate::work_model::TaskArtifactArea {
                    path: crate::work_model::work_artifact_path(
                        "wi-auth",
                        "attempt-1",
                        "attempt-1-write-1",
                    ),
                });
                Ok(())
            })
            .unwrap();
        reconcile_active(dir.path(), "wi-auth", DispatchStatus::NeedsUser).unwrap();

        let transcript = dir.path().join(crate::work_model::work_artifact_path(
            "wi-auth",
            "attempt-1",
            "attempt-1-write-1/transcript.jsonl",
        ));
        fs::create_dir_all(transcript.parent().unwrap()).unwrap();
        fs::write(
            &transcript,
            concat!(
                "{\"type\":\"thread.started\"}\n",
                "{\"type\":\"turn.started\"}\n",
                "{\"type\":\"error\",\"message\":\"Your access token could not be refreshed because your refresh token was already used. Please log out and sign in again.\"}\n",
                "{\"type\":\"turn.failed\",\"error\":{\"message\":\"Your access token could not be refreshed because your refresh token was already used. Please log out and sign in again.\"}}\n",
            ),
        )
        .unwrap();

        assert_eq!(
            resume(dir.path(), "wi-auth", "attempt-1", None).unwrap(),
            ResumeDispatchOutcome::Created
        );
        let migrated = store.read_work_item("wi-auth").unwrap();
        assert_eq!(migrated.attempts[0].pause_kind, Some(PauseKind::Auth));
        assert_eq!(migrated.attempts[0].tasks[0].status, TaskStatus::NeedsUser);
        assert_eq!(
            resume(dir.path(), "wi-auth", "attempt-1", None).unwrap(),
            ResumeDispatchOutcome::ExistingMatching
        );
        assert_eq!(store.read_work_item("wi-auth").unwrap(), migrated);
    }

    fn initialize_queue_resume_git_project(project: &Path) {
        fs::create_dir_all(project).unwrap();
        crate::git::run(project, &["init", "-q"], "initialize queue resume fixture").unwrap();
        crate::git::run(
            project,
            &["config", "user.name", "Test Author"],
            "configure author",
        )
        .unwrap();
        crate::git::run(
            project,
            &["config", "user.email", "test@example.com"],
            "configure email",
        )
        .unwrap();
        fs::write(project.join("tracked.txt"), "base\n").unwrap();
        crate::git::run(project, &["add", "tracked.txt"], "stage fixture").unwrap();
        crate::git::run(project, &["commit", "-m", "Base"], "commit fixture").unwrap();
    }

    #[test]
    fn resume_accepts_pending_evidence_review_after_blocked_dispatch() {
        let dir = tempfile::tempdir().unwrap();
        initialize_queue_resume_git_project(dir.path());
        write_pending_evidence_review(dir.path(), "wi-evidence", "attempt-1");
        let mut blocked = Dispatch::new_queued("wi-evidence", 0, None, 1);
        blocked.status = DispatchStatus::Blocked;
        blocked.bound_attempt_id = Some("attempt-1".to_string());
        put_ledger(
            dir.path(),
            &DispatchLedger {
                work_item_id: "wi-evidence".to_string(),
                dispatches: vec![blocked],
            },
        );

        assert_eq!(
            resume(dir.path(), "wi-evidence", "attempt-1", None).unwrap(),
            ResumeDispatchOutcome::Created
        );
        let ledger = read_ledger(dir.path(), "wi-evidence").unwrap().unwrap();
        assert_eq!(ledger.dispatches.len(), 2);
        assert_eq!(ledger.dispatches[0].status, DispatchStatus::Blocked);
        assert_eq!(
            ledger.active().unwrap().bound_attempt_id.as_deref(),
            Some("attempt-1")
        );
    }

    #[test]
    fn resume_accepts_completed_unsettled_evidence_review_after_blocked_dispatch() {
        let dir = tempfile::tempdir().unwrap();
        initialize_queue_resume_git_project(dir.path());
        write_pending_evidence_review(dir.path(), "wi-evidence", "attempt-1");
        WorkModelStore::new(dir.path())
            .mutate_work_item("wi-evidence", |item| {
                item.attempts[0].tasks.last_mut().unwrap().status = TaskStatus::Complete;
                Ok(())
            })
            .unwrap();
        let mut blocked = Dispatch::new_queued("wi-evidence", 0, None, 1);
        blocked.status = DispatchStatus::Blocked;
        blocked.bound_attempt_id = Some("attempt-1".to_string());
        put_ledger(
            dir.path(),
            &DispatchLedger {
                work_item_id: "wi-evidence".to_string(),
                dispatches: vec![blocked],
            },
        );

        assert_eq!(
            resume(dir.path(), "wi-evidence", "attempt-1", None).unwrap(),
            ResumeDispatchOutcome::Created
        );
        let ledger = read_ledger(dir.path(), "wi-evidence").unwrap().unwrap();
        assert_eq!(ledger.dispatches.len(), 2);
        assert_eq!(ledger.dispatches[0].status, DispatchStatus::Blocked);
        assert_eq!(
            ledger.active().unwrap().bound_attempt_id.as_deref(),
            Some("attempt-1")
        );
    }

    #[test]
    fn add_rejects_pending_evidence_review_instead_of_creating_new_attempt() {
        let dir = tempfile::tempdir().unwrap();
        initialize_queue_resume_git_project(dir.path());
        write_pending_evidence_review(dir.path(), "wi-evidence", "attempt-1");

        let error = add(dir.path(), "wi-evidence", None).unwrap_err();
        assert!(error.to_string().contains("queue resume"));
        assert!(read_ledger(dir.path(), "wi-evidence").unwrap().is_none());
        WorkModelStore::new(dir.path())
            .mutate_work_item("wi-evidence", |item| {
                item.attempts[0].tasks.last_mut().unwrap().status = TaskStatus::Complete;
                Ok(())
            })
            .unwrap();
        let error = add(dir.path(), "wi-evidence", None).unwrap_err();
        assert!(error.to_string().contains("queue resume"));
        assert!(read_ledger(dir.path(), "wi-evidence").unwrap().is_none());
        let item = WorkModelStore::new(dir.path())
            .read_work_item("wi-evidence")
            .unwrap();
        assert_eq!(item.attempts.len(), 1);
        assert!(item.attempts[0].has_pending_evidence_review_tasks());
    }

    #[test]
    fn resume_supersedes_clean_interrupted_accidental_queue_admission() {
        let dir = tempfile::tempdir().unwrap();
        initialize_queue_resume_git_project(dir.path());
        write_pending_evidence_review(dir.path(), "wi-evidence", "attempt-1");
        let accidental_workspace = dir.path().join("accidental-attempt");
        crate::git::run(
            dir.path(),
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "accidental-attempt",
                accidental_workspace.to_str().unwrap(),
                "HEAD",
            ],
            "create accidental queue workspace",
        )
        .unwrap();
        let store = WorkModelStore::new(dir.path());
        store
            .mutate_work_item("wi-evidence", |item| {
                item.add_initial_attempt("attempt-2")?;
                let attempt = item.attempts.last_mut().unwrap();
                attempt.status = AttemptStatus::NeedsUser;
                attempt.pause_kind = Some(PauseKind::Interrupted);
                let writer = &mut attempt.tasks[0];
                writer.status = TaskStatus::NeedsUser;
                writer.workspace_access.writes = vec![crate::work_model::WorkspaceRef {
                    id: "candidate".to_string(),
                    path: accidental_workspace.display().to_string(),
                }];
                Ok(())
            })
            .unwrap();
        let mut accidental = Dispatch::new_queued("wi-evidence", 0, None, 1);
        accidental.status = DispatchStatus::NeedsUser;
        accidental.bound_attempt_id = Some("attempt-2".to_string());
        put_ledger(
            dir.path(),
            &DispatchLedger {
                work_item_id: "wi-evidence".to_string(),
                dispatches: vec![accidental],
            },
        );

        fs::write(accidental_workspace.join("tracked.txt"), "candidate work\n").unwrap();
        assert!(resume(dir.path(), "wi-evidence", "attempt-1", None).is_err());
        assert_eq!(
            store.read_work_item("wi-evidence").unwrap().attempts[1].status,
            AttemptStatus::NeedsUser
        );
        crate::git::run(
            &accidental_workspace,
            &["checkout", "--", "tracked.txt"],
            "restore accidental workspace fixture",
        )
        .unwrap();
        let task_lease = crate::lease::acquire(&crate::lease::task_lock_path(
            dir.path(),
            "wi-evidence",
            "attempt-2-write-1",
        ))
        .unwrap();
        assert!(resume(dir.path(), "wi-evidence", "attempt-1", None).is_err());
        drop(task_lease);

        assert_eq!(
            resume(dir.path(), "wi-evidence", "attempt-1", None).unwrap(),
            ResumeDispatchOutcome::Created
        );
        let item = store.read_work_item("wi-evidence").unwrap();
        assert!(item.attempts[0].has_pending_evidence_review_tasks());
        assert_eq!(item.attempts[1].status, AttemptStatus::Failed);
        assert_eq!(item.attempts[1].pause_kind, None);
        assert_eq!(item.attempts[1].tasks[0].status, TaskStatus::Superseded);
        assert_eq!(
            item.attempts[1].tasks[0].role,
            SUPERSEDED_QUEUE_ADMISSION_ROLE
        );
        let ledger = read_ledger(dir.path(), "wi-evidence").unwrap().unwrap();
        assert_eq!(ledger.dispatches[0].status, DispatchStatus::NeedsUser);
        assert_eq!(
            ledger.active().unwrap().bound_attempt_id.as_deref(),
            Some("attempt-1")
        );
        assert_eq!(
            resume(dir.path(), "wi-evidence", "attempt-1", None).unwrap(),
            ResumeDispatchOutcome::ExistingMatching
        );
        let evidence_path = dir.path().join(
            item.attempts[0]
                .artifacts
                .iter()
                .find(|artifact| artifact.producer_id == "queue-resume-reconciliation")
                .unwrap()
                .path
                .clone(),
        );
        let evidence_bytes = fs::read(&evidence_path).unwrap();
        fs::write(&evidence_path, b"{}").unwrap();
        assert!(claim(dir.path(), "wi-evidence", "attempt-1").is_err());
        fs::write(&evidence_path, evidence_bytes).unwrap();
        let claimed = claim(dir.path(), "wi-evidence", "attempt-1")
            .unwrap()
            .expect("reconciled evidence resume remains claimable");
        assert_eq!(claimed.bound_attempt_id, "attempt-1");
        reconcile_active(dir.path(), "wi-evidence", DispatchStatus::NeedsUser).unwrap();
        store
            .mutate_work_item("wi-evidence", |item| {
                let target = &mut item.attempts[0];
                target
                    .tasks
                    .iter_mut()
                    .find(|task| task.id == "attempt-1-evidence-proof-tests")
                    .unwrap()
                    .status = TaskStatus::Complete;
                target.status = AttemptStatus::NeedsUser;
                target.pause_kind = Some(PauseKind::WriterCompletion);
                Ok(())
            })
            .unwrap();
        assert!(has_reconciled_interrupted_evidence_queue_admission(
            dir.path(),
            &store.read_work_item("wi-evidence").unwrap(),
            "attempt-1"
        ));
        assert_eq!(
            resume(dir.path(), "wi-evidence", "attempt-1", None).unwrap(),
            ResumeDispatchOutcome::Created
        );
        assert_eq!(
            resume(dir.path(), "wi-evidence", "attempt-1", None).unwrap(),
            ResumeDispatchOutcome::ExistingMatching
        );
    }

    #[test]
    fn resume_after_canceled_history_uses_the_current_exact_attempt() {
        let dir = tempfile::tempdir().unwrap();
        setup_project(dir.path());
        write_planned_attempt(dir.path(), "wi-1", "attempt-2");
        suspend_attempt(dir.path(), "wi-1", "attempt-2", PauseKind::WriterCompletion);
        let mut canceled = Dispatch::new_resume("wi-1", "attempt-1", 0, None, false, None, 1);
        canceled.status = DispatchStatus::Canceled;
        put_ledger(
            dir.path(),
            &DispatchLedger {
                work_item_id: "wi-1".to_string(),
                dispatches: vec![canceled],
            },
        );

        assert_eq!(
            resume(dir.path(), "wi-1", "attempt-2", None).unwrap(),
            ResumeDispatchOutcome::Created
        );
        let ledger = read_ledger(dir.path(), "wi-1").unwrap().unwrap();
        assert_eq!(ledger.dispatches[0].status, DispatchStatus::Canceled);
        let active = ledger.active().unwrap();
        assert_eq!(active.bound_attempt_id.as_deref(), Some("attempt-2"));
        assert_eq!(active.resume_of_dispatch_id, None);

        let token = claim(dir.path(), "wi-1", "attempt-2")
            .unwrap()
            .expect("the fresh exact resume remains claimable");
        assert_eq!(token.bound_attempt_id, "attempt-2");
    }

    #[test]
    fn resume_queues_and_claims_only_the_retryable_failed_learner() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("main");
        setup_project(&project);
        write_blocked_relaunchable_learner(&project, "wi-1", "attempt-1");

        assert_eq!(
            resume(&project, "wi-1", "attempt-1", Some(8)).unwrap(),
            ResumeDispatchOutcome::Created
        );
        let ledger = read_ledger(&project, "wi-1").unwrap().unwrap();
        assert_eq!(ledger.dispatches.len(), 2);
        assert_eq!(ledger.dispatches[0].status, DispatchStatus::Blocked);
        assert_eq!(
            ledger.active().unwrap().resume_of_dispatch_id.as_deref(),
            Some("dispatch-1")
        );

        let token = claim(&project, "wi-1", "attempt-1")
            .unwrap()
            .expect("the unchanged Learner retry frontier remains claimable");
        assert_eq!(token.bound_attempt_id, "attempt-1");
        let item = WorkModelStore::new(&project)
            .read_work_item("wi-1")
            .unwrap();
        assert_eq!(item.attempts[0].tasks.len(), 1);
        assert!(item.attempts[0].learning.as_ref().unwrap().is_failed());
    }

    #[test]
    fn resume_rejects_a_dirty_learner_retry_candidate_without_queue_mutation() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("main");
        setup_project(&project);
        let workspace = write_blocked_relaunchable_learner(&project, "wi-1", "attempt-1");
        fs::write(workspace.join("unreviewed.txt"), "dirty\n").unwrap();
        let before = fs::read(queue_file(&project, "wi-1")).unwrap();

        let error = resume(&project, "wi-1", "attempt-1", None).unwrap_err();
        assert!(error.to_string().contains("candidate workspace is dirty"));
        assert_eq!(fs::read(queue_file(&project, "wi-1")).unwrap(), before);
    }

    #[test]
    fn claim_rejects_a_learner_candidate_dirtied_after_resume() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("main");
        setup_project(&project);
        let workspace = write_blocked_relaunchable_learner(&project, "wi-1", "attempt-1");
        resume(&project, "wi-1", "attempt-1", None).unwrap();
        fs::write(workspace.join("unreviewed.txt"), "dirty\n").unwrap();

        let error = claim(&project, "wi-1", "attempt-1").unwrap_err();
        assert!(error.to_string().contains("candidate workspace is dirty"));
        let ledger = read_ledger(&project, "wi-1").unwrap().unwrap();
        assert_eq!(ledger.dispatches.len(), 2);
        assert_eq!(ledger.active().unwrap().status, DispatchStatus::Queued);
    }

    #[test]
    fn resume_rejects_a_non_relaunchable_learner_without_queue_mutation() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("main");
        setup_project(&project);
        write_blocked_relaunchable_learner(&project, "wi-1", "attempt-1");
        WorkModelStore::new(&project)
            .mutate_work_item("wi-1", |item| {
                item.attempts[0].learning = Some(AttemptLearning::failed_with_kind(
                    1,
                    "evidence pending",
                    LearningFailureKind::EvidencePending,
                ));
                Ok(())
            })
            .unwrap();
        let before = fs::read(queue_file(&project, "wi-1")).unwrap();

        let error = resume(&project, "wi-1", "attempt-1", None).unwrap_err();
        assert!(error.to_string().contains("not relaunchable"));
        assert_eq!(fs::read(queue_file(&project, "wi-1")).unwrap(), before);
    }

    #[test]
    fn resume_rejects_a_live_learner_without_queue_mutation() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("main");
        setup_project(&project);
        write_blocked_relaunchable_learner(&project, "wi-1", "attempt-1");
        let _learner = crate::lease::acquire(&crate::lease::learner_lock_path(
            &project,
            "wi-1",
            "attempt-1",
        ))
        .unwrap();
        let before = fs::read(queue_file(&project, "wi-1")).unwrap();

        let error = resume(&project, "wi-1", "attempt-1", None).unwrap_err();
        assert!(error.to_string().contains("live Learner run"));
        assert_eq!(fs::read(queue_file(&project, "wi-1")).unwrap(), before);
    }

    #[test]
    fn resume_rejects_an_incomplete_task_in_a_learner_retry() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("main");
        setup_project(&project);
        write_blocked_relaunchable_learner(&project, "wi-1", "attempt-1");
        let store = WorkModelStore::new(&project);
        let task_path = store
            .work_task_path("wi-1", "attempt-1", "attempt-1-write-1")
            .unwrap();
        let mut task: serde_json::Value =
            serde_json::from_slice(&fs::read(&task_path).unwrap()).unwrap();
        task["status"] = serde_json::json!("planned");
        fs::write(&task_path, serde_json::to_vec_pretty(&task).unwrap()).unwrap();
        let before = fs::read(queue_file(&project, "wi-1")).unwrap();

        resume(&project, "wi-1", "attempt-1", None).unwrap_err();
        assert_eq!(fs::read(queue_file(&project, "wi-1")).unwrap(), before);
    }

    #[test]
    fn resume_rejects_a_moved_learner_retry_candidate() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("main");
        setup_project(&project);
        let workspace = write_blocked_relaunchable_learner(&project, "wi-1", "attempt-1");
        fs::write(workspace.join("later.txt"), "later\n").unwrap();
        crate::git::run(&workspace, &["add", "later.txt"], "stage later commit").unwrap();
        crate::git::run(
            &workspace,
            &["commit", "-q", "-m", "Move candidate"],
            "move candidate",
        )
        .unwrap();
        let before = fs::read(queue_file(&project, "wi-1")).unwrap();

        let error = resume(&project, "wi-1", "attempt-1", None).unwrap_err();
        assert!(error.to_string().contains("HEAD no longer matches"));
        assert_eq!(fs::read(queue_file(&project, "wi-1")).unwrap(), before);
    }

    #[test]
    fn resume_rejects_a_reviewed_or_landed_learner_candidate() {
        for state in ["reviewed", "landed"] {
            let dir = tempfile::tempdir().unwrap();
            let project = dir.path().join("main");
            setup_project(&project);
            write_blocked_relaunchable_learner(&project, "wi-1", "attempt-1");
            WorkModelStore::new(&project)
                .mutate_work_item("wi-1", |item| {
                    if state == "reviewed" {
                        item.merge_candidates[0].merge_review_state = MergeReviewState::Passed;
                    } else {
                        item.merge_candidates[0].merge_state.status =
                            MergeCandidateMergeStatus::Merged;
                    }
                    Ok(())
                })
                .unwrap();
            let before = fs::read(queue_file(&project, "wi-1")).unwrap();

            assert!(resume(&project, "wi-1", "attempt-1", None).is_err());
            assert_eq!(
                fs::read(queue_file(&project, "wi-1")).unwrap(),
                before,
                "{state} candidate changed the queue"
            );
        }
    }

    #[test]
    fn resume_rejects_a_missing_or_ambiguous_learner_candidate() {
        for state in ["missing", "ambiguous"] {
            let dir = tempfile::tempdir().unwrap();
            let project = dir.path().join("main");
            setup_project(&project);
            write_blocked_relaunchable_learner(&project, "wi-1", "attempt-1");
            let store = WorkModelStore::new(&project);
            let candidate_path = store
                .work_merge_candidate_path("wi-1", "attempt-1-merge-candidate")
                .unwrap();
            if state == "missing" {
                fs::remove_file(&candidate_path).unwrap();
            } else {
                let mut duplicate: serde_json::Value =
                    serde_json::from_slice(&fs::read(&candidate_path).unwrap()).unwrap();
                duplicate["id"] = serde_json::json!("attempt-1-second-candidate");
                let duplicate_path = store
                    .work_merge_candidate_path("wi-1", "attempt-1-second-candidate")
                    .unwrap();
                fs::write(
                    duplicate_path,
                    serde_json::to_vec_pretty(&duplicate).unwrap(),
                )
                .unwrap();
            }
            let before = fs::read(queue_file(&project, "wi-1")).unwrap();

            assert!(resume(&project, "wi-1", "attempt-1", None).is_err());
            assert_eq!(
                fs::read(queue_file(&project, "wi-1")).unwrap(),
                before,
                "{state} candidate changed the queue"
            );
        }
    }

    #[test]
    fn resume_rejects_wrong_or_unsupported_attempt_without_mutating_queue() {
        let dir = tempfile::tempdir().unwrap();
        setup_project(dir.path());
        write_scheduler_suspension(dir.path(), "wi-1", "attempt-1");
        let before = fs::read(queue_file(dir.path(), "wi-1")).unwrap();

        assert!(resume(dir.path(), "wi-1", "attempt-2", None).is_err());
        assert_eq!(fs::read(queue_file(dir.path(), "wi-1")).unwrap(), before);

        let store = WorkModelStore::new(dir.path());
        store
            .mutate_work_item("wi-1", |item| {
                item.attempts[0].pause_kind = Some(PauseKind::Uncertain);
                Ok(())
            })
            .unwrap();
        assert!(resume(dir.path(), "wi-1", "attempt-1", None).is_err());
        assert_eq!(fs::read(queue_file(dir.path(), "wi-1")).unwrap(), before);
    }

    #[test]
    fn accepted_host_evidence_admits_legacy_round_cap_resume() {
        let mut item = WorkItem::planned("wi-1", "Legacy evidence recovery");
        item.add_initial_attempt("attempt-1").unwrap();
        let attempt = &mut item.attempts[0];
        attempt.status = AttemptStatus::NeedsUser;
        attempt.pause_kind = Some(PauseKind::RoundCap);
        attempt.review_state = Some(AttemptReviewState::Failed);
        attempt.tasks[0].status = TaskStatus::Complete;

        assert!(
            ensure_attempt_resume_ready(attempt, false, false).is_err(),
            "an ordinary exhausted round-cap pause still requires an extension"
        );
        ensure_attempt_resume_ready(attempt, true, false)
            .expect("validated exact host evidence admits scheduler resume");
    }

    #[test]
    fn resume_queues_a_valid_uncertain_review_recovery() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("main");
        setup_project(&project);
        write_uncertain_review_suspension(&project, "wi-1", "attempt-1");

        assert_eq!(
            resume(&project, "wi-1", "attempt-1", None).unwrap(),
            ResumeDispatchOutcome::Created
        );
        let ledger = read_ledger(&project, "wi-1").unwrap().unwrap();
        assert_eq!(ledger.dispatches.len(), 2);
        assert_eq!(
            ledger.active().unwrap().bound_attempt_id.as_deref(),
            Some("attempt-1")
        );
        assert_eq!(
            ledger.active().unwrap().intent,
            DispatchIntent::ResumeAttempt
        );
    }

    #[test]
    fn resume_accepts_typed_and_legacy_candidate_gate_pauses() {
        for legacy in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            setup_project(dir.path());
            write_scheduler_suspension(dir.path(), "wi-1", "attempt-1");
            let store = WorkModelStore::new(dir.path());
            store
                .mutate_work_item("wi-1", |item| {
                    let attempt = &mut item.attempts[0];
                    attempt.pause_kind = Some(if legacy {
                        PauseKind::Uncertain
                    } else {
                        PauseKind::CandidateGate
                    });
                    attempt.review_state = Some(AttemptReviewState::NotReviewed);
                    attempt.artifacts = vec![
                        ArtifactRef {
                            producer_id: "attempt-1-write-1".to_string(),
                            path: ".fluent/work/artifacts/wi-1/attempt-1/candidate-gates/attempt-1-write-1/candidate-gate.json".to_string(),
                        },
                        ArtifactRef {
                            producer_id: "candidate-gate".to_string(),
                            path: ".fluent/work/artifacts/wi-1/attempt-1/candidate-gate-needs-user.md".to_string(),
                        },
                    ];
                    Ok(())
                })
                .unwrap();

            assert_eq!(
                resume(dir.path(), "wi-1", "attempt-1", None).unwrap(),
                ResumeDispatchOutcome::Created
            );
            let ledger = read_ledger(dir.path(), "wi-1").unwrap().unwrap();
            assert_eq!(ledger.dispatches.len(), 2);
            assert_eq!(
                ledger.active().unwrap().intent,
                DispatchIntent::ResumeAttempt
            );
        }
    }

    #[test]
    fn resume_accepts_legacy_failed_candidate_gate_correction_dispatch() {
        let dir = tempfile::tempdir().unwrap();
        setup_project(dir.path());
        let store = WorkModelStore::new(dir.path());
        let mut item = WorkItem::planned("wi-1", "Retry gate correction");
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
                path: ".fluent/work/artifacts/wi-1/attempt-1/candidate-gates/attempt-1-write-1/candidate-gate.json".to_string(),
            }],
        )
        .unwrap();
        let gate = item.attempts[0].tasks[1].input_artifacts[0].clone();
        item.attempts[0].artifacts.push(gate);
        store.create_work_item(&item).unwrap();
        add(dir.path(), "wi-1", Some(0)).unwrap();
        claim(dir.path(), "wi-1", "attempt-1").unwrap().unwrap();
        store
            .mutate_work_item("wi-1", |item| {
                let attempt = &mut item.attempts[0];
                attempt.status = AttemptStatus::Failed;
                attempt.pause_kind = None;
                attempt.review_state = Some(AttemptReviewState::Failed);
                attempt.tasks[1].status = TaskStatus::Failed;
                Ok(())
            })
            .unwrap();
        reconcile_active(dir.path(), "wi-1", DispatchStatus::Failed).unwrap();

        assert_eq!(
            resume(dir.path(), "wi-1", "attempt-1", None).unwrap(),
            ResumeDispatchOutcome::Created
        );
        let ledger = read_ledger(dir.path(), "wi-1").unwrap().unwrap();
        assert_eq!(ledger.dispatches.len(), 2);
        assert_eq!(ledger.dispatches[0].status, DispatchStatus::Failed);
        assert_eq!(
            ledger.active().unwrap().resume_of_dispatch_id.as_deref(),
            Some("dispatch-1")
        );
    }

    #[test]
    fn resume_rejects_unrelated_active_and_contradictory_terminal_dispatches() {
        let dir = tempfile::tempdir().unwrap();
        setup_project(dir.path());

        for (id, status) in [
            ("wi-live", DispatchStatus::Running),
            ("wi-terminal", DispatchStatus::Failed),
            ("wi-blocked", DispatchStatus::Blocked),
        ] {
            write_planned_attempt(dir.path(), id, "attempt-1");
            suspend_attempt(dir.path(), id, "attempt-1", PauseKind::WriterCompletion);
            let mut dispatch = Dispatch::new_queued(id, 0, None, 1);
            dispatch.status = status;
            if status == DispatchStatus::Running {
                dispatch.bound_attempt_id = Some("attempt-1".to_string());
            }
            put_ledger(
                dir.path(),
                &DispatchLedger {
                    work_item_id: id.to_string(),
                    dispatches: vec![dispatch],
                },
            );
            let _live_lease = (status == DispatchStatus::Running)
                .then(|| crate::lease::acquire(&dispatch_lease_path(dir.path(), id)).unwrap());
            let before = fs::read(queue_file(dir.path(), id)).unwrap();

            assert!(resume(dir.path(), id, "attempt-1", None).is_err());
            assert_eq!(fs::read(queue_file(dir.path(), id)).unwrap(), before);
        }
    }

    #[test]
    fn claim_accepts_explicit_resume_and_repeated_pause_needs_new_dispatch() {
        let dir = tempfile::tempdir().unwrap();
        setup_project(dir.path());
        write_scheduler_suspension(dir.path(), "wi-1", "attempt-1");
        resume(dir.path(), "wi-1", "attempt-1", None).unwrap();

        let token = claim(dir.path(), "wi-1", "attempt-1").unwrap().unwrap();
        assert_eq!(token.bound_attempt_id, "attempt-1");
        reconcile_active(dir.path(), "wi-1", DispatchStatus::NeedsUser).unwrap();

        assert_eq!(
            resume(dir.path(), "wi-1", "attempt-1", None).unwrap(),
            ResumeDispatchOutcome::Created
        );
        let ledger = read_ledger(dir.path(), "wi-1").unwrap().unwrap();
        assert_eq!(ledger.dispatches.len(), 3);
        assert_eq!(ledger.dispatches[1].status, DispatchStatus::NeedsUser);
        assert_eq!(
            ledger.active().unwrap().resume_of_dispatch_id.as_deref(),
            Some("dispatch-2")
        );
    }

    #[test]
    fn add_ready_work_creates_one_active_entry() {
        let dir = tempfile::tempdir().unwrap();
        setup_project(dir.path());
        write_ready_work_item(dir.path(), "wi-1");
        // A prior terminal dispatch must survive an explicit re-add.
        write_legacy_entry(dir.path(), "wi-1", 3, "failed", "2026-06-13T08:00:00Z");

        add(dir.path(), "wi-1", Some(5)).unwrap();

        let ledger = read_ledger(dir.path(), "wi-1").unwrap().unwrap();
        assert_eq!(ledger.dispatches.len(), 2, "prior history is preserved");
        assert_eq!(ledger.dispatches[0].status, DispatchStatus::Failed);
        let active = ledger.active().expect("one active dispatch");
        assert_eq!(active.status, DispatchStatus::Queued);
        assert_eq!(active.priority, 5);
    }

    #[test]
    fn add_fails_when_work_item_does_not_exist() {
        let dir = tempfile::tempdir().unwrap();
        setup_project(dir.path());

        let result = add(dir.path(), "nonexistent", None);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[test]
    fn claim_binds_attempt_and_bumps_generation() {
        let dir = tempfile::tempdir().unwrap();
        setup_project(dir.path());
        write_ready_work_item(dir.path(), "wi-1");
        add(dir.path(), "wi-1", None).unwrap();

        let token = claim(dir.path(), "wi-1", "attempt-1").unwrap().unwrap();
        assert_eq!(token.bound_attempt_id, "attempt-1");
        assert_eq!(token.generation, 1);

        let ledger = read_ledger(dir.path(), "wi-1").unwrap().unwrap();
        let active = ledger.active().unwrap();
        assert_eq!(active.status, DispatchStatus::Claimed);
        assert_eq!(active.bound_attempt_id.as_deref(), Some("attempt-1"));
    }

    #[test]
    fn mark_running_then_reconcile_advance_the_dispatch() {
        let dir = tempfile::tempdir().unwrap();
        setup_project(dir.path());
        write_ready_work_item(dir.path(), "wi-1");
        add(dir.path(), "wi-1", None).unwrap();

        let token = claim(dir.path(), "wi-1", "attempt-1").unwrap().unwrap();
        let running_token = mark_running(dir.path(), &token).unwrap();
        let ledger = read_ledger(dir.path(), "wi-1").unwrap().unwrap();
        assert_eq!(ledger.active().unwrap().status, DispatchStatus::Running);

        reconcile(dir.path(), &running_token, DispatchStatus::CandidateReady).unwrap();
        let ledger = read_ledger(dir.path(), "wi-1").unwrap().unwrap();
        assert_eq!(
            ledger.latest().unwrap().status,
            DispatchStatus::CandidateReady
        );
        assert!(ledger.active().is_none(), "terminal dispatch is not active");
    }

    #[test]
    fn reconcile_rejects_stale_generation() {
        let dir = tempfile::tempdir().unwrap();
        setup_project(dir.path());
        write_ready_work_item(dir.path(), "wi-1");
        add(dir.path(), "wi-1", None).unwrap();
        let token = claim(dir.path(), "wi-1", "attempt-1").unwrap().unwrap();
        // Advance the dispatch so the claim token is stale.
        mark_running(dir.path(), &token).unwrap();

        let result = reconcile(dir.path(), &token, DispatchStatus::Failed);
        assert!(result.is_err(), "stale generation must fail closed");
    }

    #[test]
    fn recovery_update_ignores_a_replaced_dispatch_generation() {
        let dir = tempfile::tempdir().unwrap();
        setup_project(dir.path());
        write_ready_work_item(dir.path(), "wi-1");
        add(dir.path(), "wi-1", None).unwrap();
        let first = read_ledger(dir.path(), "wi-1").unwrap().unwrap();
        let first = first.latest().unwrap().clone();

        // A different recovery operation changes the same dispatch before the
        // first reader attempts to apply its older observation.
        reconcile_active(dir.path(), "wi-1", DispatchStatus::NeedsUser).unwrap();

        assert!(
            !reconcile_if_current(
                dir.path(),
                "wi-1",
                &first.dispatch_id,
                first.generation,
                DispatchStatus::Failed,
            )
            .unwrap()
        );
        assert_eq!(
            read_ledger(dir.path(), "wi-1")
                .unwrap()
                .unwrap()
                .latest()
                .unwrap()
                .status,
            DispatchStatus::NeedsUser
        );
    }

    #[test]
    fn repeated_add_preserves_time_and_updates_only_explicit_priority() {
        let dir = tempfile::tempdir().unwrap();
        setup_project(dir.path());
        write_ready_work_item(dir.path(), "wi-1");

        add(dir.path(), "wi-1", Some(5)).unwrap();
        let first = read_ledger(dir.path(), "wi-1").unwrap().unwrap();
        let queued_at = first.active().unwrap().queued_at.clone();
        assert_eq!(first.active().unwrap().priority, 5);

        // Repeating without a priority preserves both time and priority.
        add(dir.path(), "wi-1", None).unwrap();
        let second = read_ledger(dir.path(), "wi-1").unwrap().unwrap();
        assert_eq!(second.dispatches.len(), 1, "no new dispatch is created");
        assert_eq!(second.active().unwrap().queued_at, queued_at);
        assert_eq!(second.active().unwrap().priority, 5);

        // An explicit priority updates only the priority, keeping the time.
        add(dir.path(), "wi-1", Some(10)).unwrap();
        let third = read_ledger(dir.path(), "wi-1").unwrap().unwrap();
        assert_eq!(third.active().unwrap().queued_at, queued_at);
        assert_eq!(third.active().unwrap().priority, 10);
    }

    #[test]
    fn add_rejects_proposed_and_abandoned_work() {
        let dir = tempfile::tempdir().unwrap();
        setup_project(dir.path());
        write_work_item_json(
            dir.path(),
            "wi-proposed",
            r#","authorization":{"state":"proposed"}"#,
        );
        write_work_item_json(dir.path(), "wi-abandoned", r#","abandonment":{}"#);

        let proposed = add(dir.path(), "wi-proposed", None);
        assert!(proposed.is_err(), "proposed Work is rejected");
        assert!(
            read_ledger(dir.path(), "wi-proposed").unwrap().is_none(),
            "no queue entry is created for proposed Work"
        );

        let abandoned = add(dir.path(), "wi-abandoned", None);
        assert!(abandoned.is_err(), "abandoned Work is rejected");
        assert!(
            read_ledger(dir.path(), "wi-abandoned").unwrap().is_none(),
            "no queue entry is created for abandoned Work"
        );
    }

    #[test]
    fn automatic_dispatch_rejects_ineligible_work() {
        let dir = tempfile::tempdir().unwrap();
        setup_project(dir.path());
        write_work_item_json(
            dir.path(),
            "wi-proposed",
            r#","authorization":{"state":"proposed"}"#,
        );
        write_work_item_json(dir.path(), "wi-abandoned", r#","abandonment":{}"#);

        let proposed = ensure_dispatch(dir.path(), "wi-proposed", "op-proposed", 1)
            .unwrap_err()
            .to_string();
        let abandoned = ensure_dispatch(dir.path(), "wi-abandoned", "op-abandoned", 1)
            .unwrap_err()
            .to_string();
        assert!(proposed.contains("is proposed; human authorization is required"));
        assert_eq!(abandoned, "Work Item \"wi-abandoned\" is abandoned");
        assert!(read_ledger(dir.path(), "wi-proposed").unwrap().is_none());
        assert!(read_ledger(dir.path(), "wi-abandoned").unwrap().is_none());
    }

    #[test]
    fn claim_rechecks_work_lifecycle() {
        let dir = tempfile::tempdir().unwrap();
        setup_project(dir.path());
        write_ready_work_item(dir.path(), "wi-abandoned");
        add(dir.path(), "wi-abandoned", None).unwrap();
        write_work_item_json(dir.path(), "wi-abandoned", r#","abandonment":{}"#);

        assert_eq!(
            claim(dir.path(), "wi-abandoned", "attempt-1")
                .unwrap_err()
                .to_string(),
            "Work Item \"wi-abandoned\" is abandoned"
        );
        let ledger = read_ledger(dir.path(), "wi-abandoned").unwrap().unwrap();
        assert_eq!(ledger.active().unwrap().status, DispatchStatus::Queued);
    }

    #[test]
    fn matching_automatic_dispatch_survives_lifecycle_changes_on_receipt_replay() {
        let dir = tempfile::tempdir().unwrap();
        setup_project(dir.path());
        write_ready_work_item(dir.path(), "wi-1");
        ensure_dispatch(dir.path(), "wi-1", "operation-1", 7).unwrap();
        let ledger_before = fs::read(queue_file(dir.path(), "wi-1")).unwrap();
        write_work_item_json(dir.path(), "wi-1", r#","abandonment":{}"#);

        ensure_dispatch(dir.path(), "wi-1", "operation-1", 99).unwrap();

        assert_eq!(
            fs::read(queue_file(dir.path(), "wi-1")).unwrap(),
            ledger_before
        );
    }

    #[test]
    fn unrelated_active_and_terminal_dispatches_do_not_satisfy_automatic_enqueue() {
        let dir = tempfile::tempdir().unwrap();
        setup_project(dir.path());
        for (id, status) in [
            ("wi-active", DispatchStatus::Queued),
            ("wi-terminal", DispatchStatus::Canceled),
        ] {
            write_ready_work_item(dir.path(), id);
            let mut dispatch = Dispatch::new_queued(id, 3, None, 1);
            dispatch.status = status;
            put_ledger(
                dir.path(),
                &DispatchLedger {
                    work_item_id: id.to_string(),
                    dispatches: vec![dispatch],
                },
            );
            let before = fs::read(queue_file(dir.path(), id)).unwrap();

            let error = ensure_dispatch(dir.path(), id, "automatic-operation", 8)
                .unwrap_err()
                .to_string();

            assert!(error.contains("does not match automatic operation"));
            assert_eq!(fs::read(queue_file(dir.path(), id)).unwrap(), before);
        }
    }

    #[test]
    fn claim_holds_the_work_boundary_until_its_queue_mutation_finishes() {
        use crate::work_model::WorkItemAbandonment;
        use std::sync::mpsc;
        use std::time::Duration;

        let dir = tempfile::tempdir().unwrap();
        setup_project(dir.path());
        write_ready_work_item(dir.path(), "wi-race");
        add(dir.path(), "wi-race", None).unwrap();
        let store = WorkModelStore::new(dir.path());
        let mut abandoned = store.read_work_item("wi-race").unwrap();
        abandoned.abandonment = Some(WorkItemAbandonment {
            reason: Some("race winner".to_string()),
        });
        let queue_lock = lock_queue(dir.path()).unwrap();
        let probe =
            crate::test_lock_probe::ScopedLockProbe::install("queue-lifecycle", "wi-race", None);
        let claim_root = dir.path().to_path_buf();
        let claim_thread = std::thread::spawn(move || claim(&claim_root, "wi-race", "attempt-1"));
        assert!(probe.wait_for("claim", "ELIGIBLE"));

        let abandon_root = dir.path().to_path_buf();
        let (abandoned_tx, abandoned_rx) = mpsc::channel();
        let abandon_thread = std::thread::spawn(move || {
            let result = WorkModelStore::new(abandon_root).write_work_item(&abandoned);
            abandoned_tx.send(result).unwrap();
        });

        let early_abandonment = abandoned_rx.recv_timeout(Duration::from_millis(150));
        let abandonment_blocked = matches!(early_abandonment, Err(mpsc::RecvTimeoutError::Timeout));
        drop(queue_lock);
        assert!(claim_thread.join().unwrap().unwrap().is_some());
        let abandonment = match early_abandonment {
            Ok(result) => result,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                abandoned_rx.recv_timeout(Duration::from_secs(2)).unwrap()
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                panic!("abandonment writer disconnected")
            }
        };
        assert!(abandonment.is_ok());
        abandon_thread.join().unwrap();

        assert!(
            abandonment_blocked,
            "abandonment changed lifecycle after claim validated but before it mutated the queue"
        );
        let ledger = read_ledger(dir.path(), "wi-race").unwrap().unwrap();
        assert_eq!(ledger.latest().unwrap().status, DispatchStatus::Claimed);
        assert!(
            WorkModelStore::new(dir.path())
                .read_work_item("wi-race")
                .unwrap()
                .abandonment
                .is_some()
        );
    }

    #[test]
    fn resume_validates_planning_evidence_before_queue_lock() {
        use std::sync::mpsc;
        use std::time::Duration;

        let dir = tempfile::tempdir().unwrap();
        setup_project(dir.path());
        write_scheduler_suspension(dir.path(), "wi-evidence-lock", "attempt-1");
        let probe = crate::test_lock_probe::ScopedLockProbe::install(
            "queue-lifecycle",
            "wi-evidence-lock",
            Some(("resume", "EVIDENCE")),
        );
        let resume_root = dir.path().to_path_buf();
        let (resume_tx, resume_rx) = mpsc::channel();
        let resume_thread = std::thread::spawn(move || {
            resume_tx
                .send(resume(&resume_root, "wi-evidence-lock", "attempt-1", None))
                .unwrap();
        });

        assert!(probe.wait_for("resume", "EVIDENCE"));
        let lock_root = dir.path().to_path_buf();
        let (lock_tx, lock_rx) = mpsc::channel();
        let lock_thread = std::thread::spawn(move || {
            let lock = lock_queue(&lock_root).unwrap();
            lock_tx.send(()).unwrap();
            drop(lock);
        });
        assert!(
            lock_rx.recv_timeout(Duration::from_millis(250)).is_ok(),
            "queue evidence validation must not hold the queue lock"
        );
        probe.release();
        assert!(
            resume_rx
                .recv_timeout(Duration::from_secs(2))
                .unwrap()
                .is_ok()
        );
        resume_thread.join().unwrap();
        lock_thread.join().unwrap();
    }

    #[test]
    fn resume_rechecks_full_admission_when_matching_dispatch_changes() {
        use std::sync::mpsc;
        use std::time::Duration;

        let dir = tempfile::tempdir().unwrap();
        setup_project(dir.path());
        write_scheduler_suspension(dir.path(), "wi-resume-race", "attempt-1");
        assert_eq!(
            resume(dir.path(), "wi-resume-race", "attempt-1", None).unwrap(),
            ResumeDispatchOutcome::Created
        );

        let probe = crate::test_lock_probe::ScopedLockProbe::install(
            "queue-lifecycle",
            "wi-resume-race",
            Some(("resume", "PREFLIGHT")),
        );
        let resume_root = dir.path().to_path_buf();
        let (resume_tx, resume_rx) = mpsc::channel();
        let resume_thread = std::thread::spawn(move || {
            resume_tx
                .send(resume_with_tester_no_sandbox(
                    &resume_root,
                    "wi-resume-race",
                    "attempt-1",
                    None,
                    true,
                ))
                .unwrap();
        });

        assert!(probe.wait_for("resume", "PREFLIGHT"));
        remove(dir.path(), "wi-resume-race").unwrap();
        probe.release();

        let error = resume_rx
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("Tester no-sandbox recovery"),
            "changed resume must rerun the no-sandbox admission check: {error}"
        );
        resume_thread.join().unwrap();

        let ledger = read_ledger(dir.path(), "wi-resume-race").unwrap().unwrap();
        assert_eq!(ledger.dispatches.len(), 2);
        assert_eq!(ledger.dispatches[0].status, DispatchStatus::NeedsUser);
        assert_eq!(ledger.latest().unwrap().status, DispatchStatus::Canceled);
    }

    #[test]
    fn list_sorts_by_priority_then_queue_time() {
        let dir = tempfile::tempdir().unwrap();
        setup_project(dir.path());
        for id in ["wi-a", "wi-b", "wi-c"] {
            write_ready_work_item(dir.path(), id);
        }
        put_ledger(
            dir.path(),
            &DispatchLedger {
                work_item_id: "wi-a".to_string(),
                dispatches: vec![Dispatch {
                    queued_at: "2026-06-13T10:00:00Z".to_string(),
                    priority: 5,
                    ..Dispatch::new_queued("wi-a", 5, None, 1)
                }],
            },
        );
        put_ledger(
            dir.path(),
            &DispatchLedger {
                work_item_id: "wi-b".to_string(),
                dispatches: vec![Dispatch {
                    queued_at: "2026-06-13T09:00:00Z".to_string(),
                    priority: 10,
                    ..Dispatch::new_queued("wi-b", 10, None, 1)
                }],
            },
        );
        put_ledger(
            dir.path(),
            &DispatchLedger {
                work_item_id: "wi-c".to_string(),
                dispatches: vec![Dispatch {
                    queued_at: "2026-06-13T08:00:00Z".to_string(),
                    priority: 5,
                    ..Dispatch::new_queued("wi-c", 5, None, 1)
                }],
            },
        );

        let entries = list(dir.path()).unwrap();
        let ids: Vec<&str> = entries.iter().map(|d| d.work_item_id.as_str()).collect();
        // Highest priority first, then oldest queue time.
        assert_eq!(ids, vec!["wi-b", "wi-c", "wi-a"]);
    }

    #[test]
    fn show_returns_latest_terminal_dispatch_with_attempt_binding() {
        let dir = tempfile::tempdir().unwrap();
        setup_project(dir.path());
        let first = Dispatch {
            status: DispatchStatus::Failed,
            bound_attempt_id: Some("attempt-1".to_string()),
            ..Dispatch::new_queued("wi-1", 0, None, 1)
        };
        let latest = Dispatch {
            status: DispatchStatus::NeedsUser,
            bound_attempt_id: Some("attempt-2".to_string()),
            ..Dispatch::new_queued("wi-1", 0, None, 2)
        };
        put_ledger(
            dir.path(),
            &DispatchLedger {
                work_item_id: "wi-1".to_string(),
                dispatches: vec![first, latest.clone()],
            },
        );

        assert_eq!(show(dir.path(), "wi-1").unwrap(), latest);
        assert!(list(dir.path()).unwrap().is_empty());
    }

    #[test]
    fn show_reports_missing_dispatch() {
        let dir = tempfile::tempdir().unwrap();
        setup_project(dir.path());

        let error = show(dir.path(), "wi-missing").unwrap_err().to_string();
        assert!(error.contains("has no queue dispatch"));
    }

    #[test]
    fn remove_unclaimed_entry_preserves_cancellation_disposition() {
        let dir = tempfile::tempdir().unwrap();
        setup_project(dir.path());
        write_ready_work_item(dir.path(), "wi-1");
        add(dir.path(), "wi-1", None).unwrap();

        remove(dir.path(), "wi-1").unwrap();

        let ledger = read_ledger(dir.path(), "wi-1").unwrap().unwrap();
        assert_eq!(ledger.latest().unwrap().status, DispatchStatus::Canceled);
        assert!(ledger.active().is_none(), "canceled entry is not active");

        // Replayed automatic promotion must not restore the canceled dispatch.
        assert!(ensure_dispatch(dir.path(), "wi-1", "op-1", 100).is_err());
        let after = read_ledger(dir.path(), "wi-1").unwrap().unwrap();
        assert_eq!(after.dispatches.len(), 1);
        assert_eq!(after.latest().unwrap().status, DispatchStatus::Canceled);
    }

    #[test]
    fn remove_missing_entry_reports_not_queued() {
        let dir = tempfile::tempdir().unwrap();
        setup_project(dir.path());
        write_ready_work_item(dir.path(), "wi-1");

        let result = remove(dir.path(), "wi-1");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not queued"));
    }

    #[test]
    fn remove_rejects_live_claim() {
        let dir = tempfile::tempdir().unwrap();
        setup_project(dir.path());
        write_ready_work_item(dir.path(), "wi-1");
        add(dir.path(), "wi-1", None).unwrap();
        claim(dir.path(), "wi-1", "attempt-1").unwrap().unwrap();

        // Hold the dispatch lease so the claim reads as live.
        let lease = crate::lease::acquire(&dispatch_lease_path(dir.path(), "wi-1")).unwrap();

        let result = remove(dir.path(), "wi-1");
        assert!(result.is_err(), "a live claim blocks removal");
        assert!(result.unwrap_err().to_string().contains("active"));

        drop(lease);
    }

    #[test]
    fn automatic_enqueue_retry_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        setup_project(dir.path());
        write_ready_work_item(dir.path(), "wi-1");

        ensure_dispatch(dir.path(), "wi-1", "op-1", 100).unwrap();
        // A replay of the same origin operation, even with a different priority,
        // must not create a second dispatch or change the first's priority.
        ensure_dispatch(dir.path(), "wi-1", "op-1", 50).unwrap();

        let ledger = read_ledger(dir.path(), "wi-1").unwrap().unwrap();
        assert_eq!(ledger.dispatches.len(), 1);
        assert_eq!(ledger.active().unwrap().priority, 100);
    }

    #[test]
    fn scheduler_skips_malformed_entry_without_stalling() {
        let dir = tempfile::tempdir().unwrap();
        setup_project(dir.path());
        write_ready_work_item(dir.path(), "wi-good");
        add(dir.path(), "wi-good", Some(1)).unwrap();

        // A malformed queue file must not abort listing the good entries.
        let qdir = queue_dir(dir.path());
        fs::write(qdir.join("wi-bad.json"), "{ this is not json").unwrap();

        let entries = list(dir.path()).unwrap();
        assert_eq!(entries.len(), 1, "the good entry is still listed");
        assert_eq!(entries[0].work_item_id, "wi-good");
    }

    #[test]
    fn scheduler_warns_and_preserves_malformed_entry() {
        let dir = tempfile::tempdir().unwrap();
        setup_project(dir.path());
        let qdir = queue_dir(dir.path());
        fs::create_dir_all(&qdir).unwrap();
        let bad = qdir.join("wi-bad.json");
        fs::write(&bad, "{ not valid").unwrap();

        // Listing skips the malformed file rather than erroring, and leaves it
        // on disk for operator inspection.
        let entries = list(dir.path()).unwrap();
        assert!(entries.is_empty());
        assert!(bad.exists(), "the malformed entry is preserved");
        // list_ledgers, the scheduler's read path, also skips it without error.
        assert!(list_ledgers(dir.path()).unwrap().is_empty());
        assert!(bad.exists());
    }

    #[test]
    fn terminal_and_blocked_dispositions_survive_automatic_replay() {
        let dir = tempfile::tempdir().unwrap();
        setup_project(dir.path());

        for (id, status) in [
            ("wi-failed", DispatchStatus::Failed),
            ("wi-ready", DispatchStatus::CandidateReady),
            ("wi-blocked", DispatchStatus::Blocked),
        ] {
            write_ready_work_item(dir.path(), id);
            put_ledger(
                dir.path(),
                &DispatchLedger {
                    work_item_id: id.to_string(),
                    dispatches: vec![Dispatch {
                        status,
                        ..Dispatch::new_queued(id, 0, None, 1)
                    }],
                },
            );

            assert!(ensure_dispatch(dir.path(), id, "op-replay", 100).is_err());

            let ledger = read_ledger(dir.path(), id).unwrap().unwrap();
            assert_eq!(ledger.dispatches.len(), 1, "no new dispatch for {id}");
            assert_eq!(
                ledger.latest().unwrap().status,
                status,
                "{id} disposition survives replay"
            );
        }
    }
}
