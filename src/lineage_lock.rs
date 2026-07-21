//! Serialize mutations that charge one corrective Work lineage.
//!
//! Lock order is: follow-up operation (when present), root lineage, then a
//! specific Work Item, then queue reconciliation. Callers release lineage and
//! Work locks before the queue; an outer follow-up operation lock may remain
//! held through its queue stage.

use rustix::fs::{FlockOperation, flock};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub struct LineageLock {
    _file: File,
}

pub fn lock_path(project_root: &Path, root_id: &str) -> PathBuf {
    let key = format!("{:x}", Sha256::digest(root_id.as_bytes()));
    project_root
        .join(".fluent/work/locks/lineages")
        .join(format!("{key}.lock"))
}

pub fn acquire(project_root: &Path, root_id: &str) -> io::Result<LineageLock> {
    acquire_for(project_root, root_id, None)
}

pub fn acquire_automatic(project_root: &Path, root_id: &str) -> io::Result<LineageLock> {
    acquire_for(project_root, root_id, Some("AUTOMATIC"))
}

pub fn acquire_human(project_root: &Path, root_id: &str) -> io::Result<LineageLock> {
    acquire_for(project_root, root_id, Some("HUMAN"))
}

fn acquire_for(project_root: &Path, root_id: &str, actor: Option<&str>) -> io::Result<LineageLock> {
    let path = lock_path(project_root, root_id);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let file = OpenOptions::new().create(true).write(true).open(path)?;
    match flock(&file, FlockOperation::NonBlockingLockExclusive) {
        Ok(()) => {}
        Err(error) => {
            let error = io::Error::from(error);
            if error.kind() != io::ErrorKind::WouldBlock {
                return Err(error);
            }
            test_phase(actor, root_id, "BLOCKED")?;
            flock(&file, FlockOperation::LockExclusive).map_err(io::Error::from)?;
        }
    }
    test_phase(actor, root_id, "ACQUIRED")?;
    Ok(LineageLock { _file: file })
}

fn test_phase(actor: Option<&str>, root_id: &str, phase: &str) -> io::Result<()> {
    let Some(actor) = actor else {
        return Ok(());
    };
    if std::env::var("FLUENT_TEST_LINEAGE_ROOT").as_deref() != Ok(root_id) {
        return Ok(());
    }
    let path_key = format!("FLUENT_TEST_{actor}_LINEAGE_{phase}_PATH");
    if let Ok(path) = std::env::var(path_key) {
        fs::write(path, format!("{phase}\n"))?;
    }
    if phase == "ACQUIRED" {
        let release_key = format!("FLUENT_TEST_{actor}_LINEAGE_RELEASE_PATH");
        if let Ok(path) = std::env::var(release_key) {
            let path = PathBuf::from(path);
            while !path.exists() {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_path_is_stable_per_root_and_filename_safe() {
        let root = Path::new("/tmp/project");
        let first = lock_path(root, "root/with unsafe characters");
        let same = lock_path(root, "root/with unsafe characters");
        let other = lock_path(root, "other-root");
        let expected_parent = root.join(".fluent/work/locks/lineages");

        assert_eq!(first, same);
        assert_ne!(first, other);
        assert_eq!(first.parent(), Some(expected_parent.as_path()));
    }
}
