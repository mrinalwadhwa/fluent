use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::fd::{AsRawFd, RawFd};
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

impl TaskLease {
    pub(crate) fn raw_fd(&self) -> RawFd {
        self._file.as_raw_fd()
    }
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

/// Acquire an exclusive lease, waiting for a peer that is currently using the
/// same resource. Reviewer cache admission uses this to make accounting and
/// copying one project-wide transaction.
pub fn acquire_blocking(lock_path: &Path) -> io::Result<TaskLease> {
    if let Some(parent) = lock_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .open(lock_path)?;
    flock(&file, FlockOperation::LockExclusive).map_err(io::Error::from)?;
    Ok(TaskLease { _file: file })
}

/// Let concurrent Tasks use one build output while an exclusive reclamation
/// check waits for every user to finish.
pub fn acquire_shared_blocking(lock_path: &Path) -> io::Result<TaskLease> {
    if let Some(parent) = lock_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(lock_path)?;
    flock(&file, FlockOperation::LockShared).map_err(io::Error::from)?;
    Ok(TaskLease { _file: file })
}

pub fn is_leased(lock_path: &Path) -> bool {
    // Infrastructure failures cannot establish that ownership has settled.
    is_leased_checked(lock_path).unwrap_or(true)
}

/// Check ownership without treating an unreadable lease as an absent owner.
pub fn is_leased_checked(lock_path: &Path) -> io::Result<bool> {
    let file = match OpenOptions::new().read(true).open(lock_path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    // Serialize observers across processes. Each observer still probes with an
    // exclusive lock to detect both exclusive and shared owners, but must not
    // mistake another observer's transient probe for an owner. Owners continue
    // to acquire the original lease directly. Lock the existing directory so
    // observing a lease never creates files or changes persisted Work state.
    let parent = lock_path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let probe_guard = File::open(parent)?;
    flock(&probe_guard, FlockOperation::LockExclusive).map_err(io::Error::from)?;
    match flock(&file, FlockOperation::NonBlockingLockExclusive) {
        Ok(()) => {
            flock(&file, FlockOperation::Unlock).map_err(io::Error::from)?;
            Ok(false)
        }
        Err(errno) => {
            let error = io::Error::from(errno);
            if error.kind() == io::ErrorKind::WouldBlock {
                Ok(true)
            } else {
                Err(error)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concurrent_observer_waits_for_probe_without_reporting_an_owner() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("task.lock");
        let file = File::create(&path).unwrap();
        // Hold the exact locks used by an observer, pausing its exclusive probe
        // so another observer deterministically overlaps that interval.
        let probe = File::open(dir.path()).unwrap();
        flock(&probe, FlockOperation::LockExclusive).unwrap();
        flock(&file, FlockOperation::LockExclusive).unwrap();
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (result_tx, result_rx) = std::sync::mpsc::channel();
        let observer = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            result_tx.send(is_leased_checked(&path).unwrap()).unwrap();
        });
        started_rx.recv().unwrap();
        let early = result_rx.recv_timeout(std::time::Duration::from_millis(100));
        flock(&file, FlockOperation::Unlock).unwrap();
        drop(probe);
        observer.join().unwrap();
        assert!(
            matches!(early, Err(std::sync::mpsc::RecvTimeoutError::Timeout)),
            "an observer must wait for a peer probe, got {early:?}"
        );
        assert!(!result_rx.recv().unwrap());
    }

    #[test]
    fn observing_a_free_lease_keeps_the_directory_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("task.lock");
        fs::write(&path, b"retained lease bytes").unwrap();
        assert!(!is_leased_checked(&path).unwrap());
        assert!(!is_leased(&path));
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
        assert_eq!(fs::read(&path).unwrap(), b"retained lease bytes");
    }

    #[test]
    fn observers_detect_exclusive_and_shared_owners() {
        let dir = tempfile::tempdir().unwrap();
        for shared in [false, true] {
            let path = dir.path().join(format!("task-{shared}.lock"));
            let _owner = if shared {
                acquire_shared_blocking(&path).unwrap()
            } else {
                acquire(&path).unwrap()
            };
            assert!(is_leased_checked(&path).unwrap());
            assert!(is_leased(&path));
        }
    }

    #[test]
    fn observers_fail_conservatively_on_infrastructure_errors() {
        let dir = tempfile::tempdir().unwrap();
        let obstruction = dir.path().join("file");
        fs::write(&obstruction, b"not a directory").unwrap();
        let path = obstruction.join("task.lock");
        assert!(is_leased_checked(&path).is_err());
        assert!(is_leased(&path));
        assert!(!is_leased(&dir.path().join("missing.lock")));
    }

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
    fn shared_task_leases_allow_peers_and_block_reclamation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("build-output.lock");
        let first = acquire_shared_blocking(&path).unwrap();
        let second = acquire_shared_blocking(&path).unwrap();
        assert!(matches!(
            try_acquire(&path).unwrap(),
            LeaseAttempt::Contended
        ));
        drop(first);
        assert!(matches!(
            try_acquire(&path).unwrap(),
            LeaseAttempt::Contended
        ));
        drop(second);
        assert!(matches!(
            try_acquire(&path).unwrap(),
            LeaseAttempt::Acquired(_)
        ));
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
