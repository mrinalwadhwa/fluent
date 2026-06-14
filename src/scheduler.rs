use anyhow::Result;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::queue::{self, QueueEntry, QueueStatus};

static SHUTDOWN: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttemptOutcome {
    Complete,
    Failed,
    NeedsUser,
}

pub trait AttemptInvoker: Send + Sync {
    fn invoke(&self, project_root: &Path, work_item_id: &str) -> Result<AttemptOutcome>;
}

pub struct CliAttemptInvoker;

impl AttemptInvoker for CliAttemptInvoker {
    fn invoke(&self, project_root: &Path, work_item_id: &str) -> Result<AttemptOutcome> {
        let factory_bin = std::env::current_exe()?;

        let create_status = Command::new(&factory_bin)
            .current_dir(project_root)
            .args(["work", "attempt", work_item_id])
            .status()?;

        if !create_status.success() {
            return Ok(AttemptOutcome::Failed);
        }

        let run_status = Command::new(&factory_bin)
            .current_dir(project_root)
            .args(["work", "attempt", "run", work_item_id, "--no-sandbox"])
            .status()?;

        if run_status.success() {
            classify_attempt_outcome(project_root, work_item_id)
        } else {
            Ok(AttemptOutcome::Failed)
        }
    }
}

fn classify_attempt_outcome(project_root: &Path, work_item_id: &str) -> Result<AttemptOutcome> {
    let store = crate::work_model::WorkModelStore::new(project_root);
    let item = store.read_work_item(work_item_id)?;

    if let Some(attempt) = item.attempts.last() {
        match attempt.status {
            crate::work_model::AttemptStatus::Complete => Ok(AttemptOutcome::Complete),
            crate::work_model::AttemptStatus::Failed => Ok(AttemptOutcome::Failed),
            crate::work_model::AttemptStatus::NeedsUser => Ok(AttemptOutcome::NeedsUser),
            _ => Ok(AttemptOutcome::Complete),
        }
    } else {
        Ok(AttemptOutcome::Failed)
    }
}

pub fn run(
    project_root: &Path,
    poll_seconds: u64,
    invoker: &dyn AttemptInvoker,
) -> Result<()> {
    install_signal_handler();

    loop {
        if shutdown_requested() {
            return Ok(());
        }

        let next = pick_next_queued(project_root)?;
        match next {
            Some(entry) => {
                run_one(project_root, &entry, invoker)?;
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

pub fn pick_next_queued(project_root: &Path) -> Result<Option<QueueEntry>> {
    let entries = queue::list(project_root)?;
    Ok(entries.into_iter().find(|e| e.status == QueueStatus::Queued))
}

pub fn run_one(
    project_root: &Path,
    entry: &QueueEntry,
    invoker: &dyn AttemptInvoker,
) -> Result<()> {
    queue::update_status(project_root, &entry.work_item_id, QueueStatus::Running)?;
    eprintln!("[scheduler] starting {}", entry.work_item_id);

    let outcome = invoker.invoke(project_root, &entry.work_item_id)?;

    let new_status = match outcome {
        AttemptOutcome::Complete => QueueStatus::Done,
        AttemptOutcome::Failed => QueueStatus::Failed,
        AttemptOutcome::NeedsUser => QueueStatus::NeedsUser,
    };
    queue::update_status(project_root, &entry.work_item_id, new_status.clone())?;
    eprintln!("[scheduler] finished {} -> {}", entry.work_item_id, new_status);
    Ok(())
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
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::Mutex;

    struct MockInvoker {
        outcome: AttemptOutcome,
        invocations: Mutex<Vec<String>>,
    }

    impl MockInvoker {
        fn new(outcome: AttemptOutcome) -> Self {
            Self {
                outcome,
                invocations: Mutex::new(Vec::new()),
            }
        }

        fn invoked_ids(&self) -> Vec<String> {
            self.invocations.lock().unwrap().clone()
        }
    }

    impl AttemptInvoker for MockInvoker {
        fn invoke(&self, _project_root: &Path, work_item_id: &str) -> Result<AttemptOutcome> {
            self.invocations
                .lock()
                .unwrap()
                .push(work_item_id.to_string());
            Ok(self.outcome.clone())
        }
    }

    fn setup_project(tmp: &Path) {
        fs::create_dir_all(tmp.join(".factory/work/items")).unwrap();
    }

    fn write_work_item(tmp: &Path, id: &str) {
        fs::write(
            tmp.join(format!(".factory/work/items/{id}.json")),
            format!(r#"{{"id": "{id}", "title": "Test"}}"#),
        )
        .unwrap();
    }

    fn write_queue_entry(tmp: &Path, id: &str, priority: i64, status: &str, queued_at: &str) {
        let dir = tmp.join(".factory/work/queue");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join(format!("{id}.json")),
            format!(
                r#"{{"work_item_id":"{id}","queued_at":"{queued_at}","priority":{priority},"status":"{status}"}}"#
            ),
        )
        .unwrap();
    }

    #[test]
    fn pick_next_queued_returns_highest_priority_queued() {
        let dir = tempfile::tempdir().unwrap();
        setup_project(dir.path());
        write_work_item(dir.path(), "wi-low");
        write_work_item(dir.path(), "wi-high");
        write_queue_entry(dir.path(), "wi-low", 1, "queued", "2026-06-13T10:00:00Z");
        write_queue_entry(dir.path(), "wi-high", 10, "queued", "2026-06-13T10:00:00Z");

        let next = pick_next_queued(dir.path()).unwrap().unwrap();
        assert_eq!(next.work_item_id, "wi-high");
    }

    #[test]
    fn pick_next_queued_breaks_priority_ties_by_queued_at() {
        let dir = tempfile::tempdir().unwrap();
        setup_project(dir.path());
        write_work_item(dir.path(), "wi-later");
        write_work_item(dir.path(), "wi-earlier");
        write_queue_entry(dir.path(), "wi-later", 5, "queued", "2026-06-13T12:00:00Z");
        write_queue_entry(
            dir.path(),
            "wi-earlier",
            5,
            "queued",
            "2026-06-13T09:00:00Z",
        );

        let next = pick_next_queued(dir.path()).unwrap().unwrap();
        assert_eq!(next.work_item_id, "wi-earlier");
    }

    #[test]
    fn pick_next_queued_skips_non_queued_statuses() {
        let dir = tempfile::tempdir().unwrap();
        setup_project(dir.path());
        write_work_item(dir.path(), "wi-done");
        write_work_item(dir.path(), "wi-queued");
        write_queue_entry(dir.path(), "wi-done", 10, "done", "2026-06-13T10:00:00Z");
        write_queue_entry(dir.path(), "wi-queued", 1, "queued", "2026-06-13T10:00:00Z");

        let next = pick_next_queued(dir.path()).unwrap().unwrap();
        assert_eq!(next.work_item_id, "wi-queued");
    }

    #[test]
    fn pick_next_queued_returns_none_when_empty() {
        let dir = tempfile::tempdir().unwrap();
        setup_project(dir.path());
        assert!(pick_next_queued(dir.path()).unwrap().is_none());
    }

    #[test]
    fn run_one_updates_status_to_done_on_complete_outcome() {
        let dir = tempfile::tempdir().unwrap();
        setup_project(dir.path());
        write_work_item(dir.path(), "wi-1");
        queue::add(dir.path(), "wi-1", None).unwrap();

        let invoker = MockInvoker::new(AttemptOutcome::Complete);
        let entry = pick_next_queued(dir.path()).unwrap().unwrap();
        run_one(dir.path(), &entry, &invoker).unwrap();

        let entries = queue::list(dir.path()).unwrap();
        assert_eq!(entries[0].status, QueueStatus::Done);
        assert_eq!(invoker.invoked_ids(), vec!["wi-1"]);
    }

    #[test]
    fn run_one_updates_status_to_failed_on_failed_outcome() {
        let dir = tempfile::tempdir().unwrap();
        setup_project(dir.path());
        write_work_item(dir.path(), "wi-1");
        queue::add(dir.path(), "wi-1", None).unwrap();

        let invoker = MockInvoker::new(AttemptOutcome::Failed);
        let entry = pick_next_queued(dir.path()).unwrap().unwrap();
        run_one(dir.path(), &entry, &invoker).unwrap();

        let entries = queue::list(dir.path()).unwrap();
        assert_eq!(entries[0].status, QueueStatus::Failed);
    }

    #[test]
    fn run_one_updates_status_to_needs_user_on_needs_user_outcome() {
        let dir = tempfile::tempdir().unwrap();
        setup_project(dir.path());
        write_work_item(dir.path(), "wi-1");
        queue::add(dir.path(), "wi-1", None).unwrap();

        let invoker = MockInvoker::new(AttemptOutcome::NeedsUser);
        let entry = pick_next_queued(dir.path()).unwrap().unwrap();
        run_one(dir.path(), &entry, &invoker).unwrap();

        let entries = queue::list(dir.path()).unwrap();
        assert_eq!(entries[0].status, QueueStatus::NeedsUser);
    }

    #[test]
    fn run_one_marks_running_before_invoking_attempt() {
        let dir = tempfile::tempdir().unwrap();
        setup_project(dir.path());
        write_work_item(dir.path(), "wi-1");
        queue::add(dir.path(), "wi-1", None).unwrap();

        struct StatusCheckInvoker {
            project_root: PathBuf,
        }

        impl AttemptInvoker for StatusCheckInvoker {
            fn invoke(
                &self,
                _project_root: &Path,
                work_item_id: &str,
            ) -> Result<AttemptOutcome> {
                let entries = queue::list(&self.project_root).unwrap();
                let entry = entries
                    .iter()
                    .find(|e| e.work_item_id == work_item_id)
                    .unwrap();
                assert_eq!(
                    entry.status,
                    QueueStatus::Running,
                    "status should be running during invocation"
                );
                Ok(AttemptOutcome::Complete)
            }
        }

        let invoker = StatusCheckInvoker {
            project_root: dir.path().to_path_buf(),
        };
        let entry = pick_next_queued(dir.path()).unwrap().unwrap();
        run_one(dir.path(), &entry, &invoker).unwrap();
    }
}
