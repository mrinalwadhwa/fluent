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

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};

use rustix::fs::{FlockOperation, flock};

use crate::work_model::{AttemptStatus, WorkItem, WorkModelStore};

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
    /// Reason a dispatch was blocked, when `status` is `Blocked`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_reason: Option<String>,
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
            bound_attempt_id: None,
            claimed_at: None,
            origin_operation_id,
            blocked_reason: None,
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
}

/// Result of reconciling an automatic enqueue intent with the durable ledger.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnsureDispatchOutcome {
    /// This call created the operation's dispatch.
    Created,
    /// The operation's matching dispatch was already durable.
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
            bound_attempt_id: None,
            claimed_at: None,
            origin_operation_id: None,
            blocked_reason: None,
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
// Queue commands: add, list, remove
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
    let item = store.read_work_item(id)?;
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
    let item = store.read_work_item(id)?;
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
        .iter()
        .find(|a| a.status == AttemptStatus::NeedsUser)
    {
        bail!(
            "Work Item {id:?} has Attempt {:?} suspended at needs-user; resume it instead of queueing",
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
    let item = store.read_work_item(work_item_id)?;
    ensure_lifecycle_eligible(&item, work_item_id)?;
    #[cfg(test)]
    crate::test_lock_probe::reach(
        "queue-lifecycle",
        work_item_id,
        "claim",
        "ELIGIBLE",
    );
    let _lock = lock_queue(project_root)?;
    let mut ledger = match read_ledger(project_root, work_item_id)? {
        Some(ledger) => ledger,
        None => return Ok(None),
    };

    let Some(active) = ledger.active_mut() else {
        return Ok(None);
    };
    if active.status != DispatchStatus::Queued {
        return Ok(None);
    }

    active.status = DispatchStatus::Claimed;
    active.bound_attempt_id = Some(bound_attempt_id.to_string());
    active.claimed_at = Some(crate::work_model::now_iso8601());
    active.generation += 1;

    let token = DispatchToken {
        work_item_id: work_item_id.to_string(),
        dispatch_id: active.dispatch_id.clone(),
        generation: active.generation,
        bound_attempt_id: bound_attempt_id.to_string(),
        priority: active.priority,
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

/// Block the latest dispatch with a reason, leaving it as durable evidence for
/// operator inspection.
pub fn block_active(project_root: &Path, work_item_id: &str, reason: &str) -> Result<()> {
    mutate_latest(project_root, work_item_id, |dispatch| {
        dispatch.status = DispatchStatus::Blocked;
        dispatch.blocked_reason = Some(reason.to_string());
        dispatch.generation += 1;
    })
}

/// Cancel the latest dispatch — used when queued Work becomes abandoned before
/// any Attempt is created.
pub fn cancel_active(project_root: &Path, work_item_id: &str) -> Result<()> {
    mutate_latest(project_root, work_item_id, |dispatch| {
        dispatch.status = DispatchStatus::Canceled;
        dispatch.generation += 1;
    })
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
    mutate(active)?;
    write_ledger(project_root, &ledger)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
        fs::write(
            tmp.join(format!(".fluent/work/items/{id}.json")),
            json,
        )
        .unwrap();
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

        assert_eq!(fs::read(queue_file(dir.path(), "wi-1")).unwrap(), ledger_before);
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
        let queue_lock = lock_queue(dir.path()).unwrap();
        let probe = crate::test_lock_probe::ScopedLockProbe::install(
            "queue-lifecycle",
            "wi-race",
            None,
        );
        let claim_root = dir.path().to_path_buf();
        let claim_thread = std::thread::spawn(move || {
            claim(&claim_root, "wi-race", "attempt-1")
        });
        assert!(probe.wait_for("claim", "ELIGIBLE"));

        let store = WorkModelStore::new(dir.path());
        let mut abandoned = store.read_work_item("wi-race").unwrap();
        abandoned.abandonment = Some(WorkItemAbandonment {
            reason: Some("race winner".to_string()),
        });
        let abandon_root = dir.path().to_path_buf();
        let (abandoned_tx, abandoned_rx) = mpsc::channel();
        let abandon_thread = std::thread::spawn(move || {
            let result = WorkModelStore::new(abandon_root).write_work_item(&abandoned);
            abandoned_tx.send(result).unwrap();
        });

        let early_abandonment = abandoned_rx.recv_timeout(Duration::from_millis(150));
        let abandonment_blocked = matches!(
            early_abandonment,
            Err(mpsc::RecvTimeoutError::Timeout)
        );
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
