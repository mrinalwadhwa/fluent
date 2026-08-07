//! Bound concurrent provider use with process-held advisory leases.

use std::io;
use std::path::{Path, PathBuf};

use crate::coder::CoderKind;
use crate::lease::{self, LeaseAttempt, TaskLease};

/// Resolve the user-owned Fluent state root for cross-project capacity locks.
#[cfg(not(test))]
fn user_root() -> io::Result<PathBuf> {
    let home = std::env::var_os("HOME")
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "HOME is not set"))?;
    Ok(PathBuf::from(home).join(".config/fluent"))
}

#[cfg(test)]
pub(crate) fn effective_user_root(project_root: &Path) -> io::Result<PathBuf> {
    Ok(project_root.join(".fluent/work/test-user-provider-admission"))
}

#[cfg(all(not(test), feature = "test-support"))]
pub(crate) fn effective_user_root(project_root: &Path) -> io::Result<PathBuf> {
    // Provider credential fixtures and provider-admission storage are separate
    // test boundaries. A test may opt into a fake credential while its global
    // filesystem effects must remain hermetic.
    if std::env::var_os("FLUENT_TEST_HERMETIC_PROVIDERS").is_some() {
        Ok(project_root.join(".fluent/work/test-user-provider-admission"))
    } else {
        user_root()
    }
}

#[cfg(all(not(test), not(feature = "test-support")))]
pub(crate) fn effective_user_root(_project_root: &Path) -> io::Result<PathBuf> {
    user_root()
}

pub fn provider_name(coder: CoderKind) -> &'static str {
    coder.as_str()
}

fn user_slot_path(user_root: &Path, provider: &str, slot: u32) -> PathBuf {
    user_root
        .join("provider-admission")
        .join(provider)
        .join(format!("{slot}.lock"))
}

fn project_slot_path(project_root: &Path, provider: &str, slot: u32) -> PathBuf {
    project_root
        .join(".fluent/work/provider-admission")
        .join(provider)
        .join(format!("{slot}.lock"))
}

/// Hold one user-wide slot and one project slot for a logical provider run.
pub struct ProviderLease {
    _user: TaskLease,
    _project: TaskLease,
}

pub enum AdmissionAttempt {
    Acquired(ProviderLease),
    Contended,
}

/// Try every project/user slot pair without retaining a partial acquisition.
pub fn try_acquire(
    user_root: &Path,
    project_root: &Path,
    provider: &str,
    user_limit: u32,
    project_limit: u32,
) -> io::Result<AdmissionAttempt> {
    let user_limit = user_limit.max(1);
    let project_limit = project_limit.min(user_limit).max(1);
    for project_slot in 0..project_limit {
        let project_lease =
            match lease::try_acquire(&project_slot_path(project_root, provider, project_slot))? {
                LeaseAttempt::Acquired(lease) => lease,
                LeaseAttempt::Contended => continue,
            };
        for user_slot in 0..user_limit {
            match lease::try_acquire(&user_slot_path(user_root, provider, user_slot))? {
                LeaseAttempt::Acquired(user_lease) => {
                    return Ok(AdmissionAttempt::Acquired(ProviderLease {
                        _user: user_lease,
                        _project: project_lease,
                    }));
                }
                LeaseAttempt::Contended => continue,
            }
        }
        drop(project_lease);
    }
    Ok(AdmissionAttempt::Contended)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn cancelled_run_drop_releases_shared_slot() {
        let root = tempfile::tempdir().unwrap();
        let user = root.path().join("user");
        let project_a = root.path().join("a");
        let project_b = root.path().join("b");
        let held = match try_acquire(&user, &project_a, "claude", 1, 1).unwrap() {
            AdmissionAttempt::Acquired(lease) => lease,
            AdmissionAttempt::Contended => panic!("first slot must be available"),
        };
        assert!(matches!(
            try_acquire(&user, &project_b, "claude", 1, 1).unwrap(),
            AdmissionAttempt::Contended
        ));
        drop(held);
        assert!(matches!(
            try_acquire(&user, &project_b, "claude", 1, 1).unwrap(),
            AdmissionAttempt::Acquired(_)
        ));
    }

    #[test]
    fn shared_ceiling_bounds_total_in_flight_runs() {
        let root = tempfile::tempdir().unwrap();
        let user = root.path().join("user");
        let project = root.path().join("project");
        let _first = match try_acquire(&user, &project, "claude", 2, 2).unwrap() {
            AdmissionAttempt::Acquired(lease) => lease,
            AdmissionAttempt::Contended => panic!("first slot must be available"),
        };
        let _second = match try_acquire(&user, &project, "claude", 2, 2).unwrap() {
            AdmissionAttempt::Acquired(lease) => lease,
            AdmissionAttempt::Contended => panic!("second slot must be available"),
        };
        assert!(matches!(
            try_acquire(&user, &project, "claude", 2, 2).unwrap(),
            AdmissionAttempt::Contended
        ));
    }

    #[test]
    fn providers_use_independent_pools() {
        let root = tempfile::tempdir().unwrap();
        let user = root.path().join("user");
        let project = root.path().join("project");
        let _claude = match try_acquire(&user, &project, "claude", 1, 1).unwrap() {
            AdmissionAttempt::Acquired(lease) => lease,
            AdmissionAttempt::Contended => panic!("claude slot must be available"),
        };
        assert!(matches!(
            try_acquire(&user, &project, "codex", 1, 1).unwrap(),
            AdmissionAttempt::Acquired(_)
        ));
    }

    #[test]
    fn project_limit_can_be_lower_than_shared_limit() {
        let root = tempfile::tempdir().unwrap();
        let user = root.path().join("user");
        let project = root.path().join("project");
        let _first = match try_acquire(&user, &project, "codex", 3, 1).unwrap() {
            AdmissionAttempt::Acquired(lease) => lease,
            AdmissionAttempt::Contended => panic!("first slot must be available"),
        };
        assert!(matches!(
            try_acquire(&user, &project, "codex", 3, 1).unwrap(),
            AdmissionAttempt::Contended
        ));
    }

    #[test]
    fn cross_process_ceiling_and_exit_release_slot() {
        let root = tempfile::tempdir().unwrap();
        let user = root.path().join("user");
        let project = root.path().join("project");
        let ready = root.path().join("ready");
        let release = root.path().join("release");
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "provider_admission::tests::__provider_slot_child",
                "--nocapture",
            ])
            .env("FLUENT_PROVIDER_SLOT_CHILD", "1")
            .env("FLUENT_PROVIDER_SLOT_USER", &user)
            .env("FLUENT_PROVIDER_SLOT_PROJECT", &project)
            .env("FLUENT_PROVIDER_SLOT_READY", &ready)
            .env("FLUENT_PROVIDER_SLOT_RELEASE", &release)
            .spawn()
            .unwrap();

        for _ in 0..100 {
            if ready.is_file() {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert!(ready.is_file(), "child must report its held provider slot");
        assert!(matches!(
            try_acquire(&user, &project, "claude", 1, 1).unwrap(),
            AdmissionAttempt::Contended
        ));

        std::fs::write(&release, b"release").unwrap();
        assert!(child.wait().unwrap().success());
        assert!(matches!(
            try_acquire(&user, &project, "claude", 1, 1).unwrap(),
            AdmissionAttempt::Acquired(_)
        ));
    }

    #[test]
    fn __provider_slot_child() {
        if std::env::var_os("FLUENT_PROVIDER_SLOT_CHILD").is_none() {
            return;
        }
        let user = PathBuf::from(std::env::var_os("FLUENT_PROVIDER_SLOT_USER").unwrap());
        let project = PathBuf::from(std::env::var_os("FLUENT_PROVIDER_SLOT_PROJECT").unwrap());
        let ready = PathBuf::from(std::env::var_os("FLUENT_PROVIDER_SLOT_READY").unwrap());
        let release = PathBuf::from(std::env::var_os("FLUENT_PROVIDER_SLOT_RELEASE").unwrap());
        let _lease = match try_acquire(&user, &project, "claude", 1, 1).unwrap() {
            AdmissionAttempt::Acquired(lease) => lease,
            AdmissionAttempt::Contended => panic!("child slot must be available"),
        };
        std::fs::write(ready, b"ready").unwrap();
        for _ in 0..500 {
            if release.is_file() {
                return;
            }
            thread::sleep(Duration::from_millis(10));
        }
        panic!("parent did not release child provider slot fixture");
    }
}
