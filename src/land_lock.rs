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
            if let Ok(path) = std::env::var("FLUENT_TEST_LAND_LOCK_BLOCKED_PATH") {
                fs::write(path, b"blocked\n")?;
            }
            flock(&file, FlockOperation::LockExclusive).map_err(io::Error::from)?;
        }
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
