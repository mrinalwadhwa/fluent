use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

use rustix::fs::{FlockOperation, flock};

pub fn task_lock_path(project_root: &Path, work_item_id: &str, task_id: &str) -> PathBuf {
    project_root
        .join(".fluent/work/locks")
        .join(work_item_id)
        .join(format!("{task_id}.lock"))
}

/// The per-Attempt Learner lease path. A live runner holds this across its whole
/// Learner run, so a concurrent runner that cannot acquire it knows a peer is
/// already running and must not launch a second coder, while a crash releases the
/// OS-held lease and leaves the run recoverable.
pub fn learner_lock_path(project_root: &Path, work_item_id: &str, attempt_id: &str) -> PathBuf {
    project_root
        .join(".fluent/work/locks")
        .join(work_item_id)
        .join(format!("{attempt_id}-learner.lock"))
}

pub struct TaskLease {
    _file: File,
}

pub fn acquire(lock_path: &Path) -> io::Result<TaskLease> {
    if let Some(parent) = lock_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .open(lock_path)?;
    flock(&file, FlockOperation::NonBlockingLockExclusive).map_err(io::Error::from)?;
    Ok(TaskLease { _file: file })
}

pub fn is_leased(lock_path: &Path) -> bool {
    let file = match OpenOptions::new().read(true).open(lock_path) {
        Ok(f) => f,
        Err(_) => return false,
    };
    match flock(&file, FlockOperation::NonBlockingLockExclusive) {
        Ok(()) => {
            let _ = flock(&file, FlockOperation::Unlock);
            false
        }
        Err(_) => true,
    }
}
