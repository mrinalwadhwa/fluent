use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};

pub fn task_lock_path(project_root: &Path, work_item_id: &str, task_id: &str) -> PathBuf {
    project_root
        .join(".fluent/work/locks")
        .join(work_item_id)
        .join(format!("{task_id}.lock"))
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
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(TaskLease { _file: file })
}

pub fn is_leased(lock_path: &Path) -> bool {
    let file = match OpenOptions::new().read(true).open(lock_path) {
        Ok(f) => f,
        Err(_) => return false,
    };
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc == 0 {
        unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
        false
    } else {
        true
    }
}
