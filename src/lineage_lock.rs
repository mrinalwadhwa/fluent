//! Serialize mutations that charge one corrective Work lineage.
//!
//! Lock order is: follow-up operation (when present), root lineage, then a
//! specific Work Item. Callers must release all model locks before taking the
//! queue lock.

use rustix::fs::{FlockOperation, flock};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

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
    let path = lock_path(project_root, root_id);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let file = OpenOptions::new().create(true).write(true).open(path)?;
    flock(&file, FlockOperation::LockExclusive).map_err(io::Error::from)?;
    Ok(LineageLock { _file: file })
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
