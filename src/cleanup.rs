use anyhow::{Context, Result, bail};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::run::{self, Run, RunStatus};

#[derive(Debug, Clone)]
pub struct CleanupOptions {
    pub run_id: Option<String>,
    pub apply: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorktreeCleanup {
    None,
    WouldRemove(PathBuf),
    Removed(PathBuf),
    SkippedUnregistered(PathBuf),
    Missing(PathBuf),
}

#[derive(Debug, Clone)]
pub struct CleanupResult {
    pub run_id: String,
    pub status: RunStatus,
    pub applied: bool,
    pub worktree: WorktreeCleanup,
}

pub fn cleanup_runs(search_root: &Path, options: &CleanupOptions) -> Result<Vec<CleanupResult>> {
    let source_root = cleanup_source_root(search_root)?;
    let candidates = cleanup_candidates(&source_root, options.run_id.as_deref())?;
    let registered = registered_worktrees(&source_root)?;
    let mut results = Vec::new();

    for run in candidates {
        let status = run.status()?;
        let worktree = cleanup_worktree(&source_root, &run, &registered, options.apply)?;
        if options.apply {
            write_cleaned_marker(&run, &status, &worktree)?;
        }
        results.push(CleanupResult {
            run_id: run.id,
            status,
            applied: options.apply,
            worktree,
        });
    }

    Ok(results)
}

pub fn run_is_cleaned(run: &Run) -> bool {
    run.dir.join("cleaned.md").exists()
}

fn cleanup_source_root(search_root: &Path) -> Result<PathBuf> {
    let search_root = fs::canonicalize(search_root).unwrap_or_else(|_| search_root.to_path_buf());
    let current_worktree = git_worktree_root(&search_root).unwrap_or_else(|| search_root.clone());
    let registered = registered_worktrees(&search_root)?;

    for candidate in registered {
        if candidate == current_worktree {
            continue;
        }
        if registry_points_to_worktree(&candidate, &current_worktree)? {
            return Ok(candidate);
        }
    }

    Ok(search_root)
}

fn git_worktree_root(search_root: &Path) -> Option<PathBuf> {
    let output = Command::new("git")
        .args(["-C", &search_root.to_string_lossy()])
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if path.is_empty() {
        None
    } else {
        Some(PathBuf::from(path))
    }
}

fn registry_points_to_worktree(registry_root: &Path, worktree_root: &Path) -> Result<bool> {
    let runs_dir = registry_root.join(".factory/runs");
    if !runs_dir.is_dir() {
        return Ok(false);
    }

    let canonical_worktree = worktree_root.canonicalize().ok();
    for entry in fs::read_dir(runs_dir)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let recorded = match fs::read_to_string(path.join("worktree")) {
            Ok(content) => content.trim().to_string(),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(err) => return Err(err).context("Failed to read run worktree path"),
        };
        if recorded.is_empty() {
            continue;
        }
        let recorded_path = PathBuf::from(recorded);
        if recorded_path == worktree_root {
            return Ok(true);
        }
        if let (Some(worktree), Ok(recorded)) = (&canonical_worktree, recorded_path.canonicalize())
        {
            if recorded == *worktree {
                return Ok(true);
            }
        }
    }

    Ok(false)
}

fn cleanup_candidates(search_root: &Path, run_id: Option<&str>) -> Result<Vec<Run>> {
    if let Some(id) = run_id {
        let run = run::resolve_run_by_id(search_root, id)?;
        if run_is_cleaned(&run) {
            return Ok(Vec::new());
        }
        ensure_cleanable(&run)?;
        return Ok(vec![run]);
    }

    let mut candidates = Vec::new();
    for run in run::list_runs(search_root)? {
        if run_is_cleaned(&run) {
            continue;
        }
        if is_cleanable_status(&run.status()?) {
            candidates.push(run);
        }
    }
    Ok(candidates)
}

fn ensure_cleanable(run: &Run) -> Result<()> {
    let status = run.status()?;
    if !is_cleanable_status(&status) {
        bail!(
            "Run {} has status '{}', expected complete or landed",
            run.id,
            status
        );
    }
    Ok(())
}

fn is_cleanable_status(status: &RunStatus) -> bool {
    matches!(status, RunStatus::Complete | RunStatus::Landed)
}

fn cleanup_worktree(
    search_root: &Path,
    run: &Run,
    registered: &[PathBuf],
    apply: bool,
) -> Result<WorktreeCleanup> {
    let Some(path) = recorded_worktree_path(run)? else {
        return Ok(WorktreeCleanup::None);
    };

    if !path.exists() {
        return Ok(WorktreeCleanup::Missing(path));
    }

    if !path_is_registered(&path, registered) {
        return Ok(WorktreeCleanup::SkippedUnregistered(path));
    }

    if !apply {
        return Ok(WorktreeCleanup::WouldRemove(path));
    }

    let output = Command::new("git")
        .args(["-C", &search_root.to_string_lossy()])
        .args(["worktree", "remove", "--force", &path.to_string_lossy()])
        .output()
        .context("Failed to remove registered worktree")?;

    if !output.status.success() {
        bail!(
            "Failed to remove worktree {}:\n{}",
            path.display(),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    Ok(WorktreeCleanup::Removed(path))
}

fn recorded_worktree_path(run: &Run) -> Result<Option<PathBuf>> {
    match fs::read_to_string(run.dir.join("worktree")) {
        Ok(content) => {
            let trimmed = content.trim();
            if trimmed.is_empty() {
                Ok(None)
            } else {
                Ok(Some(PathBuf::from(trimmed)))
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err).context("Failed to read run worktree path"),
    }
}

fn registered_worktrees(search_root: &Path) -> Result<Vec<PathBuf>> {
    let output = Command::new("git")
        .args(["-C", &search_root.to_string_lossy()])
        .args(["worktree", "list", "--porcelain"])
        .output();

    let Ok(output) = output else {
        return Ok(Vec::new());
    };
    if !output.status.success() {
        return Ok(Vec::new());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(stdout
        .lines()
        .filter_map(|line| line.strip_prefix("worktree "))
        .map(PathBuf::from)
        .collect())
}

fn path_is_registered(path: &Path, registered: &[PathBuf]) -> bool {
    let canonical_path = path.canonicalize().ok();
    registered.iter().any(|registered_path| {
        if registered_path == path {
            return true;
        }
        match (&canonical_path, registered_path.canonicalize().ok()) {
            (Some(path), Some(registered)) => path == &registered,
            _ => false,
        }
    })
}

fn write_cleaned_marker(run: &Run, status: &RunStatus, worktree: &WorktreeCleanup) -> Result<()> {
    let cleaned_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    let worktree_line = match worktree {
        WorktreeCleanup::None => "Worktree: none recorded".to_string(),
        WorktreeCleanup::WouldRemove(path) => format!("Worktree: would remove {}", path.display()),
        WorktreeCleanup::Removed(path) => format!("Worktree: removed {}", path.display()),
        WorktreeCleanup::SkippedUnregistered(path) => {
            format!("Worktree: skipped unregistered {}", path.display())
        }
        WorktreeCleanup::Missing(path) => format!("Worktree: missing {}", path.display()),
    };
    let content = format!(
        "# Cleaned\n\nRun: {}\nStatus: {}\nCleaned at: unix-{cleaned_at}\nReason: stale terminal run cleanup\n{worktree_line}\n",
        run.id, status
    );
    fs::write(run.dir.join("cleaned.md"), content).context("Failed to write cleanup marker")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn create_run(root: &Path, id: &str, status: &str) -> PathBuf {
        let run_dir = root.join(format!(".factory/runs/{id}"));
        fs::create_dir_all(&run_dir).unwrap();
        fs::write(run_dir.join("status"), status).unwrap();
        run_dir
    }

    #[test]
    fn dry_run_does_not_write_marker() {
        let tmp = TempDir::new().unwrap();
        create_run(tmp.path(), "done", "complete");

        let results = cleanup_runs(
            tmp.path(),
            &CleanupOptions {
                run_id: None,
                apply: false,
            },
        )
        .unwrap();

        assert_eq!(results.len(), 1);
        assert!(!tmp.path().join(".factory/runs/done/cleaned.md").exists());
    }

    #[test]
    fn apply_writes_marker_without_status_change() {
        let tmp = TempDir::new().unwrap();
        create_run(tmp.path(), "done", "landed");

        cleanup_runs(
            tmp.path(),
            &CleanupOptions {
                run_id: None,
                apply: true,
            },
        )
        .unwrap();

        let run_dir = tmp.path().join(".factory/runs/done");
        assert_eq!(
            fs::read_to_string(run_dir.join("status")).unwrap(),
            "landed"
        );
        let marker = fs::read_to_string(run_dir.join("cleaned.md")).unwrap();
        assert!(marker.contains("Reason: stale terminal run cleanup"));
    }

    #[test]
    fn cleanup_skips_active_statuses() {
        let tmp = TempDir::new().unwrap();
        create_run(tmp.path(), "planned-run", "planned");
        create_run(tmp.path(), "needs-user-run", "needs-user");
        create_run(tmp.path(), "failed-run", "failed");

        let results = cleanup_runs(
            tmp.path(),
            &CleanupOptions {
                run_id: None,
                apply: false,
            },
        )
        .unwrap();

        assert!(results.is_empty());
    }

    #[test]
    fn unregistered_worktree_path_is_not_removed() {
        let tmp = TempDir::new().unwrap();
        let run_dir = create_run(tmp.path(), "done", "complete");
        let path = tmp.path().join("not-a-worktree");
        fs::create_dir_all(&path).unwrap();
        fs::write(run_dir.join("worktree"), path.to_str().unwrap()).unwrap();

        let results = cleanup_runs(
            tmp.path(),
            &CleanupOptions {
                run_id: None,
                apply: true,
            },
        )
        .unwrap();

        assert!(path.is_dir());
        assert_eq!(
            results[0].worktree,
            WorktreeCleanup::SkippedUnregistered(path)
        );
    }
}
