use anyhow::{Context, Result, bail};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Result of setting up a worktree for a run.
pub struct WorktreeResult {
    pub worktree_dir: PathBuf,
    pub source_branch: String,
}

/// Create a git worktree for a run and copy run state into it.
pub fn setup_run_worktree(
    source_root: &Path,
    run_id: &str,
    run_dir: &Path,
) -> Result<WorktreeResult> {
    // Record source branch
    let source_branch = git_current_branch(source_root)?;
    fs::write(run_dir.join("source-branch"), &source_branch)?;

    // Compute worktree path as sibling of source worktree
    let project_root = source_root.parent().context("source_root has no parent")?;
    let worktree_dir = project_root.join(run_id);

    if worktree_dir.is_dir() {
        eprintln!("  Worktree already exists: {}", worktree_dir.display());
    } else {
        eprintln!("  Creating worktree {} from {}...", run_id, source_branch);
        // Try creating a new branch, fall back to existing branch
        let result = Command::new("git")
            .args(["-C", &source_root.to_string_lossy()])
            .args([
                "worktree",
                "add",
                &worktree_dir.to_string_lossy(),
                "-b",
                run_id,
            ])
            .output()?;

        if !result.status.success() {
            // Branch exists from a previous run — reset it to current HEAD
            let reset = Command::new("git")
                .args(["-C", &source_root.to_string_lossy()])
                .args(["branch", "-f", run_id, "HEAD"])
                .output()?;

            if !reset.status.success() {
                bail!(
                    "Failed to reset branch to HEAD: {}",
                    String::from_utf8_lossy(&reset.stderr)
                );
            }

            let result2 = Command::new("git")
                .args(["-C", &source_root.to_string_lossy()])
                .args(["worktree", "add", &worktree_dir.to_string_lossy(), run_id])
                .output()?;

            if !result2.status.success() {
                bail!(
                    "Failed to create worktree: {}",
                    String::from_utf8_lossy(&result2.stderr)
                );
            }
        }
    }

    // Copy run state into worktree
    let wt_run_dir = worktree_dir.join(format!(".factory/runs/{run_id}"));
    fs::create_dir_all(&wt_run_dir)?;
    copy_dir_contents(run_dir, &wt_run_dir)?;

    // Write active-run pointer in the worktree
    fs::write(worktree_dir.join(".factory/active-run"), run_id)?;

    // Record worktree path in source run dir
    fs::write(
        run_dir.join("worktree"),
        worktree_dir.to_string_lossy().as_ref(),
    )?;

    eprintln!("  Worktree ready: {}", worktree_dir.display());

    Ok(WorktreeResult {
        worktree_dir,
        source_branch,
    })
}

/// Disable commit signing in a worktree so agents can commit without hardware key.
pub fn disable_commit_signing(worktree_dir: &Path) -> Result<()> {
    Command::new("git")
        .args(["-C", &worktree_dir.to_string_lossy()])
        .args(["config", "commit.gpgsign", "false"])
        .output()?;
    Ok(())
}

/// Check if a directory is a git repository.
pub fn is_git_repo(dir: &Path) -> bool {
    Command::new("git")
        .args(["-C", &dir.to_string_lossy()])
        .args(["rev-parse", "--git-dir"])
        .output()
        .is_ok_and(|o| o.status.success())
}

fn git_current_branch(dir: &Path) -> Result<String> {
    let output = Command::new("git")
        .args(["-C", &dir.to_string_lossy()])
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        Ok("main".to_string())
    }
}

/// Artifacts to copy back from the worktree run directory before cleanup.
const RUN_ARTIFACTS: &[&str] = &["sessions", "sessions.log", "reviews", "report.md", "status"];

/// Land a completed run: copy artifacts back, remove the worktree,
/// rebase onto the source branch, fast-forward merge, and delete the
/// branch. The caller sets the run status to `landed` after this
/// returns.
pub fn land_run(source_root: &Path, run_id: &str, run_dir: &Path) -> Result<()> {
    // Read worktree path
    let wt_path_str = fs::read_to_string(run_dir.join("worktree"))
        .context("No worktree path recorded for this run")?;
    let worktree_dir = PathBuf::from(wt_path_str.trim());
    if !worktree_dir.is_dir() {
        bail!(
            "Worktree directory does not exist: {}",
            worktree_dir.display()
        );
    }

    // Copy artifacts from worktree run dir back to source run dir
    let wt_run_dir = worktree_dir.join(format!(".factory/runs/{run_id}"));
    if wt_run_dir.is_dir() {
        copy_run_artifacts(&wt_run_dir, run_dir)?;
    }

    if worktree_is_dirty(&worktree_dir)? {
        bail!(
            "Cannot land run with uncommitted worktree changes. Commit, revert, or ignore them before landing."
        );
    }

    // Read source branch before removing the worktree
    let main_branch = fs::read_to_string(run_dir.join("source-branch"))
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "main".to_string());

    // Remove the worktree first — the branch can't be rebased while
    // it's checked out in a worktree
    let wt_remove = Command::new("git")
        .args(["-C", &source_root.to_string_lossy()])
        .args([
            "worktree",
            "remove",
            "--force",
            &worktree_dir.to_string_lossy(),
        ])
        .output()?;

    if !wt_remove.status.success() {
        bail!(
            "Failed to remove worktree {}:\n{}",
            worktree_dir.display(),
            String::from_utf8_lossy(&wt_remove.stderr)
        );
    }

    // Rebase the run branch onto the source branch
    let rebase = Command::new("git")
        .args(["-C", &source_root.to_string_lossy()])
        .args(["rebase", &main_branch, run_id])
        .output()?;

    if !rebase.status.success() {
        // Abort the failed rebase so the repo is not left in a broken state
        Command::new("git")
            .args(["-C", &source_root.to_string_lossy()])
            .args(["rebase", "--abort"])
            .output()
            .ok();
        bail!(
            "Rebase failed — resolve conflicts manually:\n{}",
            String::from_utf8_lossy(&rebase.stderr)
        );
    }

    // Checkout the source branch
    let checkout = Command::new("git")
        .args(["-C", &source_root.to_string_lossy()])
        .args(["checkout", &main_branch])
        .output()?;

    if !checkout.status.success() {
        bail!(
            "Failed to checkout {}: {}",
            main_branch,
            String::from_utf8_lossy(&checkout.stderr)
        );
    }

    // Fast-forward merge
    let merge = Command::new("git")
        .args(["-C", &source_root.to_string_lossy()])
        .args(["merge", "--ff-only", run_id])
        .output()?;

    if !merge.status.success() {
        bail!(
            "Fast-forward merge failed:\n{}",
            String::from_utf8_lossy(&merge.stderr)
        );
    }

    // Delete the branch
    Command::new("git")
        .args(["-C", &source_root.to_string_lossy()])
        .args(["branch", "-d", run_id])
        .output()?;

    Ok(())
}

/// Copy run artifacts from the worktree run directory back to the
/// source run directory.
pub fn copy_run_artifacts(wt_run_dir: &Path, source_run_dir: &Path) -> Result<()> {
    for name in RUN_ARTIFACTS {
        let src = wt_run_dir.join(name);
        let dst = source_run_dir.join(name);
        if src.is_dir() {
            copy_dir_contents(&src, &dst)?;
        } else if src.is_file() {
            fs::copy(&src, &dst)?;
        }
    }
    Ok(())
}

fn copy_dir_contents(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if src_path.is_dir() {
            copy_dir_contents(&src_path, &dst_path)?;
        } else {
            fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

fn worktree_is_dirty(worktree_dir: &Path) -> Result<bool> {
    let output = Command::new("git")
        .args(["-C", &worktree_dir.to_string_lossy()])
        .args([
            "status",
            "--porcelain",
            "--untracked-files=normal",
            "--",
            ".",
            ":(exclude).factory",
        ])
        .output()
        .context("Failed to check worktree status before landing")?;

    if !output.status.success() {
        bail!(
            "Failed to check worktree status before landing:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    Ok(!output.stdout.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn setup_git_project() -> TempDir {
        let tmp = TempDir::new().unwrap();
        let main_dir = tmp.path().join("main");
        fs::create_dir_all(&main_dir).unwrap();

        Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(&main_dir)
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "commit.gpgsign", "false"])
            .current_dir(&main_dir)
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.email", "test@test"])
            .current_dir(&main_dir)
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.name", "test"])
            .current_dir(&main_dir)
            .output()
            .unwrap();
        fs::write(main_dir.join("README.md"), "test").unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(&main_dir)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "init"])
            .current_dir(&main_dir)
            .output()
            .unwrap();

        tmp
    }

    #[test]
    fn test_worktree_copies_all_run_state() {
        let tmp = setup_git_project();
        let main_dir = tmp.path().join("main");

        let run_id = "test-full-state";
        let run_dir = main_dir.join(format!(".factory/runs/{run_id}"));
        fs::create_dir_all(&run_dir).unwrap();
        fs::write(run_dir.join("brief.md"), "Test brief content").unwrap();
        fs::write(
            run_dir.join("behaviors.diff.md"),
            "## New behaviors\nWHEN x THE SYSTEM SHALL y",
        )
        .unwrap();
        fs::write(run_dir.join("approach.md"), "## Approach\nDo the thing").unwrap();
        fs::write(run_dir.join("plan.md"), "## Plan\n1. Step one").unwrap();
        fs::write(run_dir.join("status"), "planned").unwrap();

        let result = setup_run_worktree(&main_dir, run_id, &run_dir).unwrap();
        let wt = result.worktree_dir;

        assert!(wt.join(format!(".factory/runs/{run_id}/brief.md")).exists());
        assert!(
            wt.join(format!(".factory/runs/{run_id}/behaviors.diff.md"))
                .exists()
        );
        assert!(
            wt.join(format!(".factory/runs/{run_id}/approach.md"))
                .exists()
        );
        assert!(wt.join(format!(".factory/runs/{run_id}/plan.md")).exists());
        assert!(wt.join(format!(".factory/runs/{run_id}/status")).exists());
        assert_eq!(
            fs::read_to_string(wt.join(".factory/active-run")).unwrap(),
            run_id
        );

        // Cleanup worktree
        Command::new("git")
            .args(["-C", &main_dir.to_string_lossy()])
            .args(["worktree", "remove", "--force", &wt.to_string_lossy()])
            .output()
            .ok();
    }

    #[test]
    fn test_worktree_records_source_branch_and_path() {
        let tmp = setup_git_project();
        let main_dir = tmp.path().join("main");

        let run_id = "test-branch-record";
        let run_dir = main_dir.join(format!(".factory/runs/{run_id}"));
        fs::create_dir_all(&run_dir).unwrap();
        fs::write(run_dir.join("brief.md"), "Brief").unwrap();
        fs::write(run_dir.join("status"), "planned").unwrap();

        let result = setup_run_worktree(&main_dir, run_id, &run_dir).unwrap();

        assert_eq!(result.source_branch, "main");
        assert!(run_dir.join("source-branch").exists());
        assert_eq!(
            fs::read_to_string(run_dir.join("source-branch")).unwrap(),
            "main"
        );
        assert!(run_dir.join("worktree").exists());
        let wt_path = fs::read_to_string(run_dir.join("worktree")).unwrap();
        assert!(Path::new(&wt_path).is_dir());

        // Cleanup
        Command::new("git")
            .args(["-C", &main_dir.to_string_lossy()])
            .args([
                "worktree",
                "remove",
                "--force",
                &result.worktree_dir.to_string_lossy(),
            ])
            .output()
            .ok();
    }

    #[test]
    fn test_worktree_copies_scope_file() {
        let tmp = setup_git_project();
        let main_dir = tmp.path().join("main");

        let run_id = "test-scope-copy";
        let run_dir = main_dir.join(format!(".factory/runs/{run_id}"));
        fs::create_dir_all(&run_dir).unwrap();
        fs::write(run_dir.join("brief.md"), "Scope brief").unwrap();
        fs::write(run_dir.join("status"), "planned").unwrap();
        fs::write(run_dir.join("mode"), "review").unwrap();
        fs::write(run_dir.join("scope"), "documentation/").unwrap();

        let result = setup_run_worktree(&main_dir, run_id, &run_dir).unwrap();
        let wt = result.worktree_dir;

        assert!(wt.join(format!(".factory/runs/{run_id}/scope")).exists());
        assert_eq!(
            fs::read_to_string(wt.join(format!(".factory/runs/{run_id}/scope"))).unwrap(),
            "documentation/"
        );

        // Cleanup
        Command::new("git")
            .args(["-C", &main_dir.to_string_lossy()])
            .args(["worktree", "remove", "--force", &wt.to_string_lossy()])
            .output()
            .ok();
    }

    #[test]
    fn test_worktree_copies_mode_file() {
        let tmp = setup_git_project();
        let main_dir = tmp.path().join("main");

        let run_id = "test-review-mode";
        let run_dir = main_dir.join(format!(".factory/runs/{run_id}"));
        fs::create_dir_all(&run_dir).unwrap();
        fs::write(run_dir.join("brief.md"), "Review brief").unwrap();
        fs::write(run_dir.join("status"), "planned").unwrap();
        fs::write(run_dir.join("mode"), "review").unwrap();

        let result = setup_run_worktree(&main_dir, run_id, &run_dir).unwrap();
        let wt = result.worktree_dir;

        assert!(wt.join(format!(".factory/runs/{run_id}/mode")).exists());
        assert_eq!(
            fs::read_to_string(wt.join(format!(".factory/runs/{run_id}/mode"))).unwrap(),
            "review"
        );

        // Cleanup
        Command::new("git")
            .args(["-C", &main_dir.to_string_lossy()])
            .args(["worktree", "remove", "--force", &wt.to_string_lossy()])
            .output()
            .ok();
    }

    #[test]
    fn test_worktree_copies_reviewers_file() {
        let tmp = setup_git_project();
        let main_dir = tmp.path().join("main");

        let run_id = "test-reviewers-file";
        let run_dir = main_dir.join(format!(".factory/runs/{run_id}"));
        fs::create_dir_all(&run_dir).unwrap();
        fs::write(run_dir.join("brief.md"), "Review brief").unwrap();
        fs::write(run_dir.join("status"), "planned").unwrap();
        fs::write(run_dir.join("mode"), "review").unwrap();
        fs::write(
            run_dir.join("reviewers"),
            "review-documentation,review-behaviors",
        )
        .unwrap();

        let result = setup_run_worktree(&main_dir, run_id, &run_dir).unwrap();
        let wt = result.worktree_dir;

        assert!(
            wt.join(format!(".factory/runs/{run_id}/reviewers"))
                .exists()
        );
        assert_eq!(
            fs::read_to_string(wt.join(format!(".factory/runs/{run_id}/reviewers"))).unwrap(),
            "review-documentation,review-behaviors"
        );

        // Cleanup
        Command::new("git")
            .args(["-C", &main_dir.to_string_lossy()])
            .args(["worktree", "remove", "--force", &wt.to_string_lossy()])
            .output()
            .ok();
    }

    #[test]
    fn test_worktree_reuse_gets_current_head() {
        let tmp = setup_git_project();
        let main_dir = tmp.path().join("main");

        let run_id = "test-reuse";
        let run_dir = main_dir.join(format!(".factory/runs/{run_id}"));
        fs::create_dir_all(&run_dir).unwrap();
        fs::write(run_dir.join("brief.md"), "Brief").unwrap();
        fs::write(run_dir.join("status"), "planned").unwrap();

        // First worktree creation — creates the branch
        let result1 = setup_run_worktree(&main_dir, run_id, &run_dir).unwrap();
        let wt1 = result1.worktree_dir;
        let old_head = String::from_utf8_lossy(
            &Command::new("git")
                .args(["-C", &wt1.to_string_lossy()])
                .args(["rev-parse", "HEAD"])
                .output()
                .unwrap()
                .stdout,
        )
        .trim()
        .to_string();

        // Remove the worktree but keep the branch
        Command::new("git")
            .args(["-C", &main_dir.to_string_lossy()])
            .args(["worktree", "remove", "--force", &wt1.to_string_lossy()])
            .output()
            .unwrap();

        // Advance HEAD on main with a new commit
        fs::write(main_dir.join("new-file.txt"), "new content").unwrap();
        Command::new("git")
            .args(["add", "new-file.txt"])
            .current_dir(&main_dir)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "second commit"])
            .current_dir(&main_dir)
            .output()
            .unwrap();

        let new_head = String::from_utf8_lossy(
            &Command::new("git")
                .args(["-C", &main_dir.to_string_lossy()])
                .args(["rev-parse", "HEAD"])
                .output()
                .unwrap()
                .stdout,
        )
        .trim()
        .to_string();
        assert_ne!(old_head, new_head);

        // Re-create worktree with the same run_id — should be at new HEAD
        let result2 = setup_run_worktree(&main_dir, run_id, &run_dir).unwrap();
        let wt2 = result2.worktree_dir;
        let wt_head = String::from_utf8_lossy(
            &Command::new("git")
                .args(["-C", &wt2.to_string_lossy()])
                .args(["rev-parse", "HEAD"])
                .output()
                .unwrap()
                .stdout,
        )
        .trim()
        .to_string();

        assert_eq!(
            wt_head, new_head,
            "Reused worktree should be at current HEAD, not old branch point"
        );

        // Cleanup
        Command::new("git")
            .args(["-C", &main_dir.to_string_lossy()])
            .args(["worktree", "remove", "--force", &wt2.to_string_lossy()])
            .output()
            .ok();
    }

    #[test]
    fn test_is_git_repo() {
        let tmp = setup_git_project();
        assert!(is_git_repo(&tmp.path().join("main")));
        assert!(!is_git_repo(tmp.path()));
    }
}
