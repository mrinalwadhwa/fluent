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
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::lease::{self, TaskLease};
use crate::queue::{self, Dispatch, DispatchStatus};
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

/// Poll the queue and dispatch queued Work sequentially. This walking skeleton
/// runs one dispatch at a time; concurrency and coordinator election arrive in
/// a later slice.
pub fn run(project_root: &Path, poll_seconds: u64, runner: &dyn AttemptRunner) -> Result<()> {
    install_signal_handler();

    loop {
        if shutdown_requested() {
            return Ok(());
        }

        match pick_next_queued(project_root)? {
            Some(dispatch) => {
                dispatch_one(project_root, &dispatch, runner)?;
            }
            None => {
                sleep_with_shutdown_check(Duration::from_secs(poll_seconds));
            }
        }

        if shutdown_requested() {
            return Ok(());
        }
    }
}

/// The highest-priority queued dispatch, breaking ties by oldest queue time.
pub fn pick_next_queued(project_root: &Path) -> Result<Option<Dispatch>> {
    Ok(queue::list(project_root)?
        .into_iter()
        .find(|dispatch| dispatch.status == DispatchStatus::Queued))
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
    let _lease = acquire_dispatch_lease(project_root, work_item_id)?;

    ensure_bound_attempt(project_root, work_item_id, &token.bound_attempt_id)?;

    let token = queue::mark_running(project_root, &token)?;
    eprintln!("[scheduler] starting {work_item_id}");

    let outcome = runner.run(project_root, work_item_id, &token.bound_attempt_id)?;

    let status = outcome.to_dispatch_status();
    queue::reconcile(project_root, &token, status)?;
    eprintln!("[scheduler] finished {work_item_id} -> {status}");
    Ok(())
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
