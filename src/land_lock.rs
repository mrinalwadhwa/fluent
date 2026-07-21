use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

use rustix::fs::{FlockOperation, flock};

pub fn lock_path(project_root: &Path) -> PathBuf {
    project_root.join(".fluent/work/locks/land.lock")
}

pub struct LandLock {
    _file: File,
}

pub fn acquire(lock_path: &Path) -> io::Result<LandLock> {
    acquire_for(lock_path, None)
}

fn acquire_for(lock_path: &Path, _actor: Option<&str>) -> io::Result<LandLock> {
    if let Some(parent) = lock_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .open(lock_path)?;
    match flock(&file, FlockOperation::NonBlockingLockExclusive) {
        Ok(()) => {}
        Err(error) => {
            let error = io::Error::from(error);
            if error.kind() != io::ErrorKind::WouldBlock {
                return Err(error);
            }
            #[cfg(test)]
            if let Some(actor) = _actor {
                crate::test_lock_probe::reach(
                    "land",
                    &lock_path.display().to_string(),
                    actor,
                    "BLOCKED",
                );
            }
            flock(&file, FlockOperation::LockExclusive).map_err(io::Error::from)?;
        }
    }
    #[cfg(test)]
    if let Some(actor) = _actor {
        crate::test_lock_probe::reach("land", &lock_path.display().to_string(), actor, "ACQUIRED");
    }
    Ok(LandLock { _file: file })
}

pub fn is_locked(lock_path: &Path) -> bool {
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
    fn contended_acquire_reports_through_scoped_probe() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = lock_path(tmp.path());
        let held = acquire_for(&path, Some("FIRST")).unwrap();

        std::thread::scope(|scope| {
            let target = path.display().to_string();
            let probe = crate::test_lock_probe::ScopedLockProbe::install("land", &target, None);
            let waiter = scope.spawn(|| acquire_for(&path, Some("SECOND")));
            assert!(probe.wait_for("SECOND", "BLOCKED"));
            drop(held);
            assert!(probe.wait_for("SECOND", "ACQUIRED"));
            waiter.join().unwrap().unwrap();
        });
    }
}
