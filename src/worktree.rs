use anyhow::Result;
use std::path::{Path, PathBuf};

use crate::git;

/// Disable commit signing in a worktree so external coders can commit
/// without hardware key. Fluent's own git calls get this via the
/// wrapper's `-c commit.gpgsign=false`; this sets the persistent
/// repo-level config for processes outside our control.
pub fn disable_commit_signing(worktree_dir: &Path) -> Result<()> {
    git::run(
        worktree_dir,
        &["config", "commit.gpgsign", "false"],
        "disable commit signing",
    )
}

/// Check if a directory is a git repository.
pub fn is_git_repo(dir: &Path) -> bool {
    git::run_raw(dir, &["rev-parse", "--git-dir"]).is_ok_and(|o| o.status.success())
}

/// Return the repository's common git directory as an absolute path.
pub fn git_common_dir(dir: &Path) -> Result<PathBuf> {
    let path = git::run_stdout(
        dir,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        "resolve common git directory",
    )?;
    Ok(PathBuf::from(path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn setup_git_project() -> TempDir {
        let tmp = TempDir::new().unwrap();
        let main_dir = tmp.path().join("main");
        fs::create_dir_all(&main_dir).unwrap();

        git::run(&main_dir, &["init", "-b", "main"], "init").unwrap();
        git::run(&main_dir, &["config", "user.email", "test@test"], "config").unwrap();
        git::run(&main_dir, &["config", "user.name", "test"], "config").unwrap();
        fs::write(main_dir.join("README.md"), "test").unwrap();
        git::run(&main_dir, &["add", "."], "stage").unwrap();
        git::run(&main_dir, &["commit", "-m", "init"], "commit").unwrap();

        tmp
    }

    #[test]
    fn test_is_git_repo() {
        let tmp = setup_git_project();
        assert!(is_git_repo(&tmp.path().join("main")));
        assert!(!is_git_repo(tmp.path()));
    }
}
