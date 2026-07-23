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

/// The outcome of a non-blocking lease acquisition that distinguishes a live peer
/// already holding the lock from a genuine infrastructure failure on the lock path.
pub enum LeaseAttempt {
    /// This runner holds the lease.
    Acquired(TaskLease),
    /// A live peer already holds the lease; nothing was acquired.
    Contended,
}

/// Try to acquire the lease without blocking, distinguishing contention from
/// infrastructure failure. A `create_dir_all`/`open` error or any flock error other
/// than contention propagates as a real IO error; only a non-blocking exclusive
/// flock that would block — a live peer already holds the lease — yields
/// `LeaseAttempt::Contended`.
pub fn try_acquire(lock_path: &Path) -> io::Result<LeaseAttempt> {
    if let Some(parent) = lock_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .open(lock_path)?;
    match flock(&file, FlockOperation::NonBlockingLockExclusive) {
        Ok(()) => Ok(LeaseAttempt::Acquired(TaskLease { _file: file })),
        Err(errno) => {
            let err = io::Error::from(errno);
            if err.kind() == io::ErrorKind::WouldBlock {
                Ok(LeaseAttempt::Contended)
            } else {
                Err(err)
            }
        }
    }
}

pub fn acquire(lock_path: &Path) -> io::Result<TaskLease> {
    match try_acquire(lock_path)? {
        LeaseAttempt::Acquired(lease) => Ok(lease),
        LeaseAttempt::Contended => Err(io::Error::from(io::ErrorKind::WouldBlock)),
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn try_acquire_acquires_a_free_lock() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("locks/wi-1/attempt-1-learner.lock");
        match try_acquire(&path).unwrap() {
            LeaseAttempt::Acquired(_) => {}
            LeaseAttempt::Contended => panic!("a free lock must be acquired, not contended"),
        }
    }

    #[test]
    fn try_acquire_reports_contention_when_a_peer_holds_the_lock() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("locks/wi-1/attempt-1-learner.lock");
        let _held = match try_acquire(&path).unwrap() {
            LeaseAttempt::Acquired(lease) => lease,
            LeaseAttempt::Contended => panic!("the first acquisition must succeed"),
        };
        // A second non-blocking acquisition sees a live peer, not an error.
        match try_acquire(&path).unwrap() {
            LeaseAttempt::Contended => {}
            LeaseAttempt::Acquired(_) => panic!("a held lock must report contention"),
        }
    }

    #[test]
    fn try_acquire_propagates_infrastructure_failure_on_the_lock_path() {
        // Obstruct the lock parent with a regular file so `create_dir_all` cannot
        // create the lock directory: an infrastructure fault must propagate as an
        // error, never masquerade as a busy peer.
        let dir = tempfile::tempdir().unwrap();
        let obstruction = dir.path().join("locks");
        fs::write(&obstruction, b"not a directory").unwrap();
        let path = obstruction.join("wi-1/attempt-1-learner.lock");
        let err = match try_acquire(&path) {
            Err(err) => err,
            Ok(_) => panic!("an obstructed lock parent must surface an error"),
        };
        assert_ne!(
            err.kind(),
            io::ErrorKind::WouldBlock,
            "an infrastructure failure must not be reported as contention"
        );
    }
}
