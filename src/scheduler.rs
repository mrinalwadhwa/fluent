//! Local scheduler that dispatches execution-ready queued Work through the
//! durable dispatch ledger.
//!
//! The scheduler claims a queued dispatch, durably binds exactly one Attempt to
//! it before launching, transitions it to `running`, drives the Attempt to a
//! terminal outcome, and reconciles the dispatch. It never lands a Merge
//! Candidate: a passing Attempt reaches `candidate-ready` and its Merge
//! Candidate stays pending for an explicit land or the separately authorized
//! auto-merge policy.

use anyhow::Result;
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use rustix::fs::{FlockOperation, flock};

use crate::lease::{self, TaskLease};
use crate::queue::{self, Dispatch, DispatchStatus, DispatchToken};
use crate::work_model::{AttemptStatus, WorkModelStore};

static SHUTDOWN: AtomicBool = AtomicBool::new(false);

/// The terminal outcome of running a bound Attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttemptOutcome {
    Complete,
    Failed,
    NeedsUser,
}

impl AttemptOutcome {
    fn to_dispatch_status(self) -> DispatchStatus {
        match self {
            Self::Complete => DispatchStatus::CandidateReady,
            Self::Failed => DispatchStatus::Failed,
            Self::NeedsUser => DispatchStatus::NeedsUser,
        }
    }
}

/// Runs a bound Attempt to a terminal outcome. The scheduler owns claiming,
/// binding, launching, and reconciling; a runner only advances the Attempt.
pub trait AttemptRunner: Send + Sync {
    fn run(
        &self,
        project_root: &Path,
        work_item_id: &str,
        attempt_id: &str,
    ) -> Result<AttemptOutcome>;
}

/// Production runner that drives an Attempt by invoking `fluent attempt run`.
pub struct CliAttemptRunner;

impl AttemptRunner for CliAttemptRunner {
    fn run(
        &self,
        project_root: &Path,
        work_item_id: &str,
        attempt_id: &str,
    ) -> Result<AttemptOutcome> {
        let fluent_bin = std::env::current_exe()?;
        let run_status = Command::new(&fluent_bin)
            .current_dir(project_root)
            .args(["attempt", "run", work_item_id, attempt_id, "--no-sandbox"])
            .status()?;

        if run_status.success() {
            classify_attempt_outcome(project_root, work_item_id, attempt_id)
        } else {
            Ok(AttemptOutcome::Failed)
        }
    }
}

/// Classify a bound Attempt's terminal outcome from its persisted status.
pub fn classify_attempt_outcome(
    project_root: &Path,
    work_item_id: &str,
    attempt_id: &str,
) -> Result<AttemptOutcome> {
    let store = WorkModelStore::new(project_root);
    let item = store.read_work_item(work_item_id)?;
    let outcome = item
        .attempts
        .iter()
        .find(|attempt| attempt.id == attempt_id)
        .map(|attempt| match attempt.status {
            AttemptStatus::Failed => AttemptOutcome::Failed,
            AttemptStatus::NeedsUser => AttemptOutcome::NeedsUser,
            _ => AttemptOutcome::Complete,
        })
        .unwrap_or(AttemptOutcome::Failed);
    Ok(outcome)
}

/// Elect one coordinator per project and, when elected, run its capacity-limited
/// worker pool. A start that finds another live coordinator reports reuse and
/// returns successfully without claiming Work.
pub fn run(project_root: &Path, poll_seconds: u64, runner: &dyn AttemptRunner) -> Result<()> {
    install_signal_handler();

    match start_or_reuse(project_root)? {
        CoordinatorStart::Reused => {
            eprintln!("[scheduler] reusing live coordinator for this project");
            Ok(())
        }
        CoordinatorStart::Elected(lease) => {
            let capacity = resolve_capacity(project_root)?;
            let result = run_coordinator(project_root, poll_seconds, capacity, runner);
            drop(lease);
            result
        }
    }
}

/// Run the elected coordinator's worker pool: each poll fills free capacity with
/// newly claimed dispatches, launching each on its own worker thread. Running
/// Work is never interrupted; on shutdown the scope drains live workers.
fn run_coordinator(
    project_root: &Path,
    poll_seconds: u64,
    capacity: usize,
    runner: &dyn AttemptRunner,
) -> Result<()> {
    std::thread::scope(|scope| -> Result<()> {
        let mut workers: Vec<std::thread::ScopedJoinHandle<()>> = Vec::new();
        loop {
            workers.retain(|handle| !handle.is_finished());

            if shutdown_requested() {
                break;
            }

            let mut spawned = false;
            for (token, lease) in claim_available(project_root, capacity)? {
                let handle = scope.spawn(move || {
                    if let Err(error) = run_claimed(project_root, token, lease, runner) {
                        eprintln!("[scheduler] worker error: {error}");
                    }
                });
                workers.push(handle);
                spawned = true;
            }

            if shutdown_requested() {
                break;
            }

            if spawned {
                // Re-evaluate promptly so freed capacity refills without waiting
                // a full poll interval.
                sleep_with_shutdown_check(Duration::from_millis(50));
            } else {
                sleep_with_shutdown_check(Duration::from_secs(poll_seconds));
            }
        }
        Ok(())
    })?;
    Ok(())
}

fn resolve_capacity(project_root: &Path) -> Result<usize> {
    let config = crate::config::resolve_scheduler_config(project_root)?;
    Ok(config.max_local_concurrency.value as usize)
}

/// The highest-priority queued dispatch, breaking ties by oldest queue time.
pub fn pick_next_queued(project_root: &Path) -> Result<Option<Dispatch>> {
    Ok(queue::list(project_root)?
        .into_iter()
        .find(|dispatch| dispatch.status == DispatchStatus::Queued))
}

/// Claim queued dispatches up to the project's remaining local capacity,
/// binding one Attempt and holding a whole-Attempt lease for each. Each returned
/// claim reads as live, so capacity is never exceeded across polls.
pub fn claim_available(
    project_root: &Path,
    capacity: usize,
) -> Result<Vec<(DispatchToken, TaskLease)>> {
    let mut claims = Vec::new();
    while project_local_capacity_used(project_root)? < capacity {
        match claim_next(project_root)? {
            Some(claim) => claims.push(claim),
            None => break,
        }
    }
    Ok(claims)
}

/// Claim the next eligible queued dispatch, binding an Attempt id and acquiring
/// its whole-Attempt lease before returning.
fn claim_next(project_root: &Path) -> Result<Option<(DispatchToken, TaskLease)>> {
    let Some(dispatch) = pick_next_queued(project_root)? else {
        return Ok(None);
    };
    let bound_attempt_id = resolve_bound_attempt_id(project_root, &dispatch.work_item_id)?;
    let Some(token) = queue::claim(project_root, &dispatch.work_item_id, &bound_attempt_id)? else {
        return Ok(None);
    };
    let lease = acquire_dispatch_lease(project_root, &dispatch.work_item_id)?;
    Ok(Some((token, lease)))
}

/// Count the project's local scheduler concurrency: every Work Item whose active
/// dispatch is claimed or running with a still-live bound-Attempt lease. Direct
/// and Fargate Attempts have no dispatch and never count; a Work Item with
/// nested reviewers is one dispatch and therefore one slot.
pub fn project_local_capacity_used(project_root: &Path) -> Result<usize> {
    let mut used = 0;
    for ledger in queue::list_ledgers(project_root)? {
        let Some(active) = ledger.active() else {
            continue;
        };
        if matches!(
            active.status,
            DispatchStatus::Claimed | DispatchStatus::Running
        ) && lease::is_leased(&queue::dispatch_lease_path(project_root, &ledger.work_item_id))
        {
            used += 1;
        }
    }
    Ok(used)
}

/// Claim a queued dispatch, durably bind exactly one Attempt, launch it, and
/// reconcile the dispatch from the Attempt's outcome. Never invokes merge
/// logic.
pub fn dispatch_one(
    project_root: &Path,
    dispatch: &Dispatch,
    runner: &dyn AttemptRunner,
) -> Result<()> {
    let work_item_id = &dispatch.work_item_id;

    // Decide the bound Attempt id before claiming so the ledger records the
    // exact Attempt the claim owns, then ensure that Attempt under the Work
    // model. A crash between the two reconciles by the persisted binding.
    let bound_attempt_id = resolve_bound_attempt_id(project_root, work_item_id)?;

    let Some(token) = queue::claim(project_root, work_item_id, &bound_attempt_id)? else {
        // Another worker claimed it first; nothing to do.
        return Ok(());
    };

    // Hold the whole-Attempt lease for the life of this dispatch so the claim
    // reads as live to capacity counting and duplicate-dispatch prevention.
    let lease = acquire_dispatch_lease(project_root, work_item_id)?;
    run_claimed(project_root, token, lease, runner)
}

/// Run an already-claimed dispatch on the current thread: ensure its bound
/// Attempt, transition to running, drive it, and reconcile. Holds the passed
/// lease for the full lifetime of the dispatch.
fn run_claimed(
    project_root: &Path,
    token: DispatchToken,
    lease: TaskLease,
    runner: &dyn AttemptRunner,
) -> Result<()> {
    let _lease = lease;
    let work_item_id = token.work_item_id.clone();

    ensure_bound_attempt(project_root, &work_item_id, &token.bound_attempt_id)?;

    let token = queue::mark_running(project_root, &token)?;
    eprintln!("[scheduler] starting {work_item_id}");

    let outcome = runner.run(project_root, &work_item_id, &token.bound_attempt_id)?;

    let status = outcome.to_dispatch_status();
    queue::reconcile(project_root, &token, status)?;
    eprintln!("[scheduler] finished {work_item_id} -> {status}");
    Ok(())
}

// -------------------------------------------------------------------------
// Coordinator election
// -------------------------------------------------------------------------

/// The outcome of a scheduler start: this process became the coordinator, or a
/// live coordinator already owns the project.
pub enum CoordinatorStart {
    Elected(CoordinatorLease),
    Reused,
}

/// The lifetime lease of an elected coordinator. Held for the coordinator's run
/// and released when it exits, so the next start can be elected.
pub struct CoordinatorLease {
    _file: File,
}

fn coordinator_lock_path(project_root: &Path) -> PathBuf {
    project_root.join(".fluent/work/scheduler/coordinator.lock")
}

/// Try to become the project's coordinator. Elects this process when the
/// coordinator lease is free; otherwise reports reuse of the live coordinator.
pub fn start_or_reuse(project_root: &Path) -> Result<CoordinatorStart> {
    let path = coordinator_lock_path(project_root);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .open(&path)?;
    match flock(&file, FlockOperation::NonBlockingLockExclusive) {
        Ok(()) => Ok(CoordinatorStart::Elected(CoordinatorLease { _file: file })),
        Err(_) => Ok(CoordinatorStart::Reused),
    }
}

/// The Attempt id a fresh dispatch will bind. Reuses a non-terminal Attempt for
/// recovery; otherwise allocates the next id.
fn resolve_bound_attempt_id(project_root: &Path, work_item_id: &str) -> Result<String> {
    let store = WorkModelStore::new(project_root);
    let item = store.read_work_item(work_item_id)?;
    Ok(item.next_attempt_id())
}

/// Ensure exactly the bound Attempt exists, creating it once when absent.
fn ensure_bound_attempt(project_root: &Path, work_item_id: &str, attempt_id: &str) -> Result<()> {
    let store = WorkModelStore::new(project_root);
    let mut item = store.read_work_item(work_item_id)?;
    if item.attempts.iter().any(|a| a.id == attempt_id) {
        return Ok(());
    }
    item.add_initial_attempt(attempt_id)?;
    store.write_work_item(&item)?;
    Ok(())
}

fn acquire_dispatch_lease(project_root: &Path, work_item_id: &str) -> Result<TaskLease> {
    let path = queue::dispatch_lease_path(project_root, work_item_id);
    lease::acquire(&path).map_err(|source| {
        anyhow::anyhow!("could not acquire dispatch lease for {work_item_id:?}: {source}")
    })
}

fn install_signal_handler() {
    let _ = ctrlc::set_handler(|| {
        SHUTDOWN.store(true, Ordering::Release);
    });
}

fn shutdown_requested() -> bool {
    SHUTDOWN.load(Ordering::Acquire)
}

fn sleep_with_shutdown_check(duration: Duration) {
    let one_second = Duration::from_secs(1);
    let mut remaining = duration;
    while remaining > Duration::ZERO {
        if shutdown_requested() {
            return;
        }
        let sleep_for = remaining.min(one_second);
        std::thread::sleep(sleep_for);
        remaining = remaining.saturating_sub(sleep_for);
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use crate::work_model::{
        AttemptReviewState, TaskOutput, TaskStatus, WorkspaceAccess, WorkspaceRef,
    };
    use std::fs;
    use std::sync::Mutex;

    /// A runner that returns a canned outcome and records its invocations.
    #[allow(dead_code)] // used by concurrency and outcome tests in later slices
    pub struct MockRunner {
        outcome: AttemptOutcome,
        pub invocations: Mutex<Vec<String>>,
    }

    #[allow(dead_code)] // used by concurrency and outcome tests in later slices
    impl MockRunner {
        pub fn new(outcome: AttemptOutcome) -> Self {
            Self {
                outcome,
                invocations: Mutex::new(Vec::new()),
            }
        }

        pub fn invoked_ids(&self) -> Vec<String> {
            self.invocations.lock().unwrap().clone()
        }
    }

    impl AttemptRunner for MockRunner {
        fn run(
            &self,
            _project_root: &Path,
            work_item_id: &str,
            _attempt_id: &str,
        ) -> Result<AttemptOutcome> {
            self.invocations
                .lock()
                .unwrap()
                .push(work_item_id.to_string());
            Ok(self.outcome)
        }
    }

    /// A runner that drives the bound Attempt to a passing Merge Candidate
    /// before returning `Complete`, so reconcile can produce `candidate-ready`.
    pub struct PassingRunner;

    impl AttemptRunner for PassingRunner {
        fn run(
            &self,
            project_root: &Path,
            work_item_id: &str,
            attempt_id: &str,
        ) -> Result<AttemptOutcome> {
            make_attempt_pass_with_candidate(project_root, work_item_id, attempt_id);
            Ok(AttemptOutcome::Complete)
        }
    }

    pub fn setup_project(tmp: &Path) {
        fs::create_dir_all(tmp.join(".fluent/work/items")).unwrap();
    }

    pub fn write_ready_work_item(tmp: &Path, id: &str) {
        fs::write(
            tmp.join(format!(".fluent/work/items/{id}.json")),
            format!(r#"{{"id": "{id}", "title": "Test"}}"#),
        )
        .unwrap();
    }

    /// Drive a bound write Attempt to `Complete` + passing review with a Merge
    /// Candidate, mirroring the shape a real passing Attempt produces.
    pub fn make_attempt_pass_with_candidate(
        project_root: &Path,
        work_item_id: &str,
        attempt_id: &str,
    ) {
        let store = WorkModelStore::new(project_root);
        let mut item = store.read_work_item(work_item_id).unwrap();
        let attempt = item
            .attempts
            .iter_mut()
            .find(|a| a.id == attempt_id)
            .expect("bound attempt exists");
        let task = attempt.tasks.last_mut().unwrap();
        task.status = TaskStatus::Complete;
        task.workspace_access = WorkspaceAccess {
            reads: vec![WorkspaceRef {
                id: "target".to_string(),
                path: ".".to_string(),
            }],
            writes: vec![WorkspaceRef {
                id: "candidate".to_string(),
                path: format!("../work-{work_item_id}-{attempt_id}"),
            }],
        };
        task.output = Some(TaskOutput {
            workspace_id: "candidate".to_string(),
            workspace_path: format!("../work-{work_item_id}-{attempt_id}"),
            source_branch: "main".to_string(),
            commit: "abc123".to_string(),
        });
        attempt.status = AttemptStatus::Complete;
        attempt.review_state = Some(AttemptReviewState::Passed);
        item.create_or_get_merge_candidate(attempt_id).unwrap();
        store.write_work_item(&item).unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;
    use crate::work_model::MergeCandidateMergeStatus;

    fn add_queued(project_root: &Path, id: &str, priority: i64) {
        queue::add(project_root, id, Some(priority)).unwrap();
    }

    /// Acquire and return the whole-Attempt lease for a Work Item so a test can
    /// hold a claim live across capacity assertions.
    fn hold_lease(project_root: &Path, work_item_id: &str) -> TaskLease {
        lease::acquire(&queue::dispatch_lease_path(project_root, work_item_id)).unwrap()
    }

    /// Drive a Work Item to a running dispatch with a held lease, returning the
    /// lease so the caller keeps the claim live.
    fn make_running(project_root: &Path, id: &str) -> TaskLease {
        add_queued(project_root, id, 0);
        let token = queue::claim(project_root, id, "attempt-1").unwrap().unwrap();
        let lease = hold_lease(project_root, id);
        queue::mark_running(project_root, &token).unwrap();
        lease
    }

    #[test]
    fn project_capacity_counts_all_claimed_and_running_work() {
        let dir = tempfile::tempdir().unwrap();
        setup_project(dir.path());
        for id in ["wi-claimed", "wi-running", "wi-queued", "wi-stale"] {
            write_ready_work_item(dir.path(), id);
        }

        // A claimed dispatch with a live lease.
        add_queued(dir.path(), "wi-claimed", 0);
        queue::claim(dir.path(), "wi-claimed", "attempt-1")
            .unwrap()
            .unwrap();
        let _claimed_lease = hold_lease(dir.path(), "wi-claimed");

        // A running dispatch with a live lease.
        let _running_lease = make_running(dir.path(), "wi-running");

        // A merely-queued dispatch does not count.
        add_queued(dir.path(), "wi-queued", 0);

        // A stale claim whose lease is dead does not count toward live capacity.
        add_queued(dir.path(), "wi-stale", 0);
        queue::claim(dir.path(), "wi-stale", "attempt-1")
            .unwrap()
            .unwrap();

        assert_eq!(project_local_capacity_used(dir.path()).unwrap(), 2);
    }

    #[test]
    fn direct_and_fargate_attempts_do_not_consume_local_scheduler_capacity() {
        let dir = tempfile::tempdir().unwrap();
        setup_project(dir.path());

        // A directly started Attempt exists in the Work model but has no queue
        // dispatch, so it never counts toward local scheduler capacity.
        let store = WorkModelStore::new(dir.path());
        let mut item = crate::work_model::WorkItem::planned("wi-direct", "Direct run");
        item.add_initial_attempt("attempt-1").unwrap();
        item.attempts[0].status = AttemptStatus::Executing;
        store.create_work_item(&item).unwrap();

        assert_eq!(project_local_capacity_used(dir.path()).unwrap(), 0);
    }

    #[test]
    fn nested_reviewers_use_one_work_slot_and_separate_reviewer_limit() {
        let dir = tempfile::tempdir().unwrap();
        setup_project(dir.path());
        write_ready_work_item(dir.path(), "wi-review");

        // A single running dispatch counts as exactly one Work slot regardless
        // of how many reviewers run inside its Attempt.
        let _lease = make_running(dir.path(), "wi-review");
        assert_eq!(project_local_capacity_used(dir.path()).unwrap(), 1);

        // The reviewer-parallelism limit is applied independently of the Work
        // slot count.
        assert!(crate::work_attempt_loop::max_parallel_reviewers() >= 1);
    }

    #[test]
    fn scheduler_fills_available_project_capacity() {
        let dir = tempfile::tempdir().unwrap();
        setup_project(dir.path());
        for n in 0..6 {
            let id = format!("wi-{n}");
            write_ready_work_item(dir.path(), &id);
            add_queued(dir.path(), &id, 0);
        }

        // With capacity 4 and six queued, exactly four are claimed at once.
        let claims = claim_available(dir.path(), 4).unwrap();
        assert_eq!(claims.len(), 4);
        assert_eq!(project_local_capacity_used(dir.path()).unwrap(), 4);

        // At capacity, a further fill claims nothing while leases stay held.
        let more = claim_available(dir.path(), 4).unwrap();
        assert!(more.is_empty(), "capacity is not exceeded");

        drop(claims);
    }

    #[test]
    fn scheduler_orders_priority_then_fifo_without_preemption() {
        let dir = tempfile::tempdir().unwrap();
        setup_project(dir.path());
        for id in ["wi-run", "wi-hi", "wi-b", "wi-a"] {
            write_ready_work_item(dir.path(), id);
        }

        // An already-running dispatch that must not be interrupted.
        let _running = make_running(dir.path(), "wi-run");

        // Queue three: one high priority, two equal priority added oldest first.
        add_queued(dir.path(), "wi-hi", 10);
        add_queued(dir.path(), "wi-b", 5);
        add_queued(dir.path(), "wi-a", 5);

        // Capacity 3 leaves two free slots after the running dispatch.
        let claims = claim_available(dir.path(), 3).unwrap();
        let claimed: Vec<String> = claims
            .iter()
            .map(|(token, _)| token.work_item_id.clone())
            .collect();
        // Higher priority first, then the older equal-priority entry.
        assert_eq!(claimed, vec!["wi-hi", "wi-b"]);

        // The lowest-priority entry stays queued; the running one is untouched.
        let ledger_a = queue::read_ledger(dir.path(), "wi-a").unwrap().unwrap();
        assert_eq!(ledger_a.active().unwrap().status, DispatchStatus::Queued);
        let ledger_run = queue::read_ledger(dir.path(), "wi-run").unwrap().unwrap();
        assert_eq!(ledger_run.active().unwrap().status, DispatchStatus::Running);

        drop(claims);
    }

    #[test]
    fn concurrent_starts_elect_one_coordinator() {
        let dir = tempfile::tempdir().unwrap();
        setup_project(dir.path());
        let root = dir.path().to_path_buf();

        let elected = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));

        std::thread::scope(|scope| {
            let mut handles = Vec::new();
            for _ in 0..8 {
                let root = root.clone();
                let elected = elected.clone();
                let barrier = barrier.clone();
                handles.push(scope.spawn(move || {
                    barrier.wait();
                    let start = start_or_reuse(&root).unwrap();
                    match start {
                        CoordinatorStart::Elected(lease) => {
                            elected.fetch_add(1, Ordering::SeqCst);
                            // Hold the lease until every start has resolved so no
                            // other start can be elected in the meantime.
                            std::thread::sleep(Duration::from_millis(100));
                            drop(lease);
                        }
                        CoordinatorStart::Reused => {}
                    }
                }));
            }
        });

        assert_eq!(
            elected.load(Ordering::SeqCst),
            1,
            "exactly one coordinator is elected among concurrent starts"
        );
    }

    #[test]
    fn live_claim_or_execution_prevents_duplicate_dispatch() {
        let dir = tempfile::tempdir().unwrap();
        setup_project(dir.path());
        write_ready_work_item(dir.path(), "wi-1");
        add_queued(dir.path(), "wi-1", 0);

        // Claim it and hold the lease so the claim reads as live.
        queue::claim(dir.path(), "wi-1", "attempt-1")
            .unwrap()
            .unwrap();
        let _lease = hold_lease(dir.path(), "wi-1");

        // A second claim finds no queued dispatch and does not bind another
        // Attempt.
        assert!(
            queue::claim(dir.path(), "wi-1", "attempt-2")
                .unwrap()
                .is_none()
        );
        assert!(pick_next_queued(dir.path()).unwrap().is_none());
        assert_eq!(project_local_capacity_used(dir.path()).unwrap(), 1);
        assert!(
            claim_available(dir.path(), 4).unwrap().is_empty(),
            "a live claim is not re-dispatched"
        );
    }

    #[test]
    fn claim_persists_bound_attempt_before_launch() {
        let dir = tempfile::tempdir().unwrap();
        setup_project(dir.path());
        write_ready_work_item(dir.path(), "wi-1");
        add_queued(dir.path(), "wi-1", 1);

        // A runner that inspects the ledger and Work model at launch time:
        // the dispatch must already be bound to exactly one created Attempt.
        struct InspectRunner {
            root: std::path::PathBuf,
        }
        impl AttemptRunner for InspectRunner {
            fn run(&self, _p: &Path, wi: &str, attempt_id: &str) -> Result<AttemptOutcome> {
                let ledger = queue::read_ledger(&self.root, wi).unwrap().unwrap();
                let active = ledger.active().unwrap();
                assert_eq!(active.bound_attempt_id.as_deref(), Some(attempt_id));
                let store = WorkModelStore::new(&self.root);
                let item = store.read_work_item(wi).unwrap();
                assert_eq!(
                    item.attempts.iter().filter(|a| a.id == attempt_id).count(),
                    1,
                    "exactly one bound Attempt exists at launch"
                );
                Ok(AttemptOutcome::Complete)
            }
        }

        let dispatch = pick_next_queued(dir.path()).unwrap().unwrap();
        let runner = InspectRunner {
            root: dir.path().to_path_buf(),
        };
        dispatch_one(dir.path(), &dispatch, &runner).unwrap();
    }

    #[test]
    fn bound_claim_transitions_to_running_on_launch() {
        let dir = tempfile::tempdir().unwrap();
        setup_project(dir.path());
        write_ready_work_item(dir.path(), "wi-1");
        add_queued(dir.path(), "wi-1", 1);

        struct RunningCheckRunner {
            root: std::path::PathBuf,
        }
        impl AttemptRunner for RunningCheckRunner {
            fn run(&self, _p: &Path, wi: &str, _a: &str) -> Result<AttemptOutcome> {
                let ledger = queue::read_ledger(&self.root, wi).unwrap().unwrap();
                assert_eq!(
                    ledger.active().unwrap().status,
                    DispatchStatus::Running,
                    "dispatch is running during launch"
                );
                Ok(AttemptOutcome::Complete)
            }
        }

        let dispatch = pick_next_queued(dir.path()).unwrap().unwrap();
        let runner = RunningCheckRunner {
            root: dir.path().to_path_buf(),
        };
        dispatch_one(dir.path(), &dispatch, &runner).unwrap();
    }

    #[test]
    fn passing_attempt_becomes_candidate_ready_without_land() {
        let dir = tempfile::tempdir().unwrap();
        setup_project(dir.path());
        write_ready_work_item(dir.path(), "wi-1");
        add_queued(dir.path(), "wi-1", 1);

        let dispatch = pick_next_queued(dir.path()).unwrap().unwrap();
        dispatch_one(dir.path(), &dispatch, &PassingRunner).unwrap();

        let ledger = queue::read_ledger(dir.path(), "wi-1").unwrap().unwrap();
        assert_eq!(
            ledger.latest().unwrap().status,
            DispatchStatus::CandidateReady
        );
    }

    #[test]
    fn scheduler_never_invokes_merge() {
        let dir = tempfile::tempdir().unwrap();
        setup_project(dir.path());
        write_ready_work_item(dir.path(), "wi-1");
        add_queued(dir.path(), "wi-1", 1);

        let dispatch = pick_next_queued(dir.path()).unwrap().unwrap();
        dispatch_one(dir.path(), &dispatch, &PassingRunner).unwrap();

        // The Merge Candidate is created but left pending: the scheduler never
        // lands it.
        let store = WorkModelStore::new(dir.path());
        let item = store.read_work_item("wi-1").unwrap();
        let candidate = item.merge_candidates.last().expect("candidate created");
        assert_eq!(
            candidate.merge_state.status,
            MergeCandidateMergeStatus::Pending,
            "scheduler must not land the Merge Candidate"
        );
    }
}
