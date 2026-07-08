//! Persistent per-branch worktree for review-only Attempts.
//!
//! PostMergeReview Attempts (and, with step 2, ReviewCodebase
//! Attempts without `--from-working-tree`) execute inside a sibling
//! Git worktree at `../work-review-<sanitized-branch>/`. The worktree
//! persists across Attempts on the same branch so that the Tester Task
//! at the start of each Attempt incrementally refreshes the workspace
//! and its build outputs instead of paying a cold start every time.
//!
//! Step 1 exposes path computation and an ensure-or-sync entry point.
//! Concurrency detection, pruning, and auto-cleanup arrive in later
//! steps.

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

use crate::git;
use crate::work_model::{AttemptStatus, WorkModelStorageError, WorkModelStore};

/// Workspace path stored on a review-only Attempt that targets the
/// per-branch worktree. Sibling of `project_root` so it satisfies
/// `resolve_managed_sibling_workspace_path`'s `work-` prefix rule.
pub fn review_only_worktree_path(branch: &str) -> String {
    format!("../work-review-{}", sanitize_branch(branch))
}

/// True if the workspace path looks like a review-only worktree path
/// (the shape `review_only_worktree_path` produces).
pub fn is_review_only_worktree_workspace_path(path: &str) -> bool {
    path.strip_prefix("../work-review-")
        .is_some_and(|rest| !rest.is_empty() && !rest.contains('/'))
}

/// Make sure the review-only worktree for `branch` exists at
/// `target_commit`. Creates the worktree if absent; syncs it to the
/// target commit if present (resetting any uncommitted state first).
/// Returns the absolute worktree path.
pub fn ensure(project_root: &Path, branch: &str, target_commit: &str) -> Result<PathBuf> {
    let worktree_path = absolute_worktree_path(project_root, branch);
    if worktree_path.exists() {
        sync(project_root, &worktree_path, target_commit)?;
    } else {
        create(project_root, &worktree_path, target_commit)?;
    }
    Ok(worktree_path)
}

fn absolute_worktree_path(project_root: &Path, branch: &str) -> PathBuf {
    project_root
        .parent()
        .unwrap_or(project_root)
        .join(format!("work-review-{}", sanitize_branch(branch)))
}

fn sanitize_branch(branch: &str) -> String {
    branch.replace('/', "-")
}

fn create(project_root: &Path, worktree_path: &Path, target_commit: &str) -> Result<()> {
    if let Some(parent) = worktree_path.parent() {
        std::fs::create_dir_all(parent).with_context(|| {
            format!(
                "Failed to create parent directory for review-only worktree {}",
                worktree_path.display()
            )
        })?;
    }
    let path_str = worktree_path.to_string_lossy();
    git::run(
        project_root,
        &["worktree", "add", "--detach", &path_str, target_commit],
        "create review-only worktree",
    )
}

fn sync(project_root: &Path, worktree_path: &Path, target_commit: &str) -> Result<()> {
    if !worktree_path.join(".git").exists() {
        bail!(
            "Review-only worktree path {} exists but is not a registered worktree; \
             remove it manually before re-running",
            worktree_path.display()
        );
    }
    if is_dirty(worktree_path)? {
        git::run(
            worktree_path,
            &["reset", "--hard"],
            "reset review-only worktree before sync",
        )?;
        git::run(
            worktree_path,
            &["clean", "-fdx"],
            "clean review-only worktree before sync",
        )?;
    }
    git::run(
        worktree_path,
        &["checkout", "--detach", target_commit],
        "sync review-only worktree to target commit",
    )?;
    // Also reset working tree to the checked-out commit to drop any
    // residual state the previous run left behind (the previous
    // Tester or reviewer may have produced untracked build outputs
    // we want to keep as cache, so do NOT `git clean` here).
    git::run(
        worktree_path,
        &["reset", "--hard", target_commit],
        "reset review-only worktree to target commit",
    )?;
    let _ = project_root;
    Ok(())
}

/// In-flight review-only Attempt against a specific worktree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InFlightAttempt {
    pub work_item_id: String,
    pub attempt_id: String,
    pub branch: String,
}

/// Find a review-only-like Attempt that is currently active against the
/// review-only worktree for `branch`. `exclude` lets the caller skip
/// itself when checking before starting its own Attempt.
///
/// "Active" means the Attempt's status is `Executing` or `Reviewing`;
/// terminal statuses (`Complete`, `Failed`, `NeedsUser`) don't block.
/// State-based detection has a small race window between the check and
/// the next run; the post-merge queue and explicit `attempt run`
/// rejection bound it in practice.
pub fn detect_in_flight(
    store: &WorkModelStore,
    branch: &str,
    exclude: Option<(&str, &str)>,
) -> Result<Option<InFlightAttempt>, WorkModelStorageError> {
    let target_path = review_only_worktree_path(branch);
    detect_in_flight_for_workspace_path(store, &target_path, exclude)
}

/// Path-based variant of `detect_in_flight`. Useful for prune, which
/// already knows the worktree's relative path and may not be able to
/// recover the original branch unambiguously after `/` → `-`
/// sanitization.
pub fn detect_in_flight_for_workspace_path(
    store: &WorkModelStore,
    workspace_path: &str,
    exclude: Option<(&str, &str)>,
) -> Result<Option<InFlightAttempt>, WorkModelStorageError> {
    for item in store.list_work_items()? {
        for attempt in &item.attempts {
            if !attempt.kind.is_review_only_like() {
                continue;
            }
            if !matches!(
                attempt.status,
                AttemptStatus::Executing | AttemptStatus::Reviewing
            ) {
                continue;
            }
            let matches_worktree = attempt
                .tasks
                .first()
                .and_then(|task| task.workspace_access.reads.first())
                .map(|workspace| workspace.path == workspace_path)
                .unwrap_or(false);
            if !matches_worktree {
                continue;
            }
            if let Some((excluded_wi, excluded_attempt)) = exclude
                && excluded_wi == item.id
                && excluded_attempt == attempt.id
            {
                continue;
            }
            let attempt_branch = attempt
                .tasks
                .first()
                .and_then(|task| task.review_context.as_ref())
                .map(|context| context.source_branch.clone())
                .unwrap_or_default();
            return Ok(Some(InFlightAttempt {
                work_item_id: item.id.clone(),
                attempt_id: attempt.id.clone(),
                branch: attempt_branch,
            }));
        }
    }
    Ok(None)
}

/// Outcome of one entry processed by `prune`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PruneEntry {
    Removed {
        path: PathBuf,
    },
    SkippedInUse {
        path: PathBuf,
        in_flight: InFlightAttempt,
    },
    SkippedNotOrphan {
        path: PathBuf,
    },
    WouldRemove {
        path: PathBuf,
    },
    WouldSkipInUse {
        path: PathBuf,
        in_flight: InFlightAttempt,
    },
}

/// Summary of one `prune` invocation.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PruneReport {
    pub entries: Vec<PruneEntry>,
}

#[derive(Debug, Clone, Copy)]
pub struct PruneOptions {
    /// When true, evaluate every registered review-only worktree, not just orphans.
    pub all: bool,
    /// When true, report decisions without actually removing anything.
    pub dry_run: bool,
}

/// Walk every registered review-only worktree (paths matching
/// `<project_parent>/work-review-*`) and remove the orphans (or all of
/// them with `all=true`). Worktrees with an in-flight review-only
/// Attempt are always skipped, even with `--all`. With `dry_run=true`,
/// nothing is removed; the report records the decisions only.
pub fn prune(
    store: &WorkModelStore,
    project_root: &Path,
    options: PruneOptions,
) -> Result<PruneReport> {
    let worktrees = list_registered_review_only_worktrees(project_root)?;
    let live_paths = live_review_only_worktree_paths(project_root)?;
    let mut report = PruneReport::default();
    for worktree_path in worktrees {
        let rel_path = worktree_relative_path(project_root, &worktree_path);
        let is_orphan = !live_paths.contains(&rel_path);
        let should_consider = options.all || is_orphan;
        if !should_consider {
            report.entries.push(PruneEntry::SkippedNotOrphan {
                path: worktree_path,
            });
            continue;
        }
        let in_flight = detect_in_flight_for_workspace_path(store, &rel_path, None)
            .with_context(|| format!("in-flight check for {}", worktree_path.display()))?;
        if let Some(in_flight) = in_flight {
            let entry = if options.dry_run {
                PruneEntry::WouldSkipInUse {
                    path: worktree_path,
                    in_flight,
                }
            } else {
                PruneEntry::SkippedInUse {
                    path: worktree_path,
                    in_flight,
                }
            };
            report.entries.push(entry);
            continue;
        }
        if options.dry_run {
            report.entries.push(PruneEntry::WouldRemove {
                path: worktree_path,
            });
            continue;
        }
        remove(project_root, &worktree_path)
            .with_context(|| format!("remove review-only worktree {}", worktree_path.display()))?;
        report.entries.push(PruneEntry::Removed {
            path: worktree_path,
        });
    }
    Ok(report)
}

/// Hard-remove a review-only worktree via `git worktree remove --force`.
pub fn remove(project_root: &Path, worktree_path: &Path) -> Result<()> {
    let path_str = worktree_path.to_string_lossy();
    git::run(
        project_root,
        &["worktree", "remove", "--force", &path_str],
        "remove review-only worktree",
    )
}

/// Registered review-only worktree paths (the absolute paths git
/// reports through `git worktree list --porcelain` that look like
/// `<project_parent>/work-review-*`).
fn list_registered_review_only_worktrees(project_root: &Path) -> Result<Vec<PathBuf>> {
    let output = git::run_raw(project_root, &["worktree", "list", "--porcelain"])
        .context("list git worktrees for review-only prune")?;
    if !output.status.success() {
        bail!(
            "git worktree list failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let parent = project_root.parent().unwrap_or(project_root).to_path_buf();
    let mut paths = Vec::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let Some(path_str) = line.strip_prefix("worktree ") else {
            continue;
        };
        let path = PathBuf::from(path_str);
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.starts_with("work-review-") || name.len() <= "work-review-".len() {
            continue;
        }
        if path.parent().map(|p| p == parent).unwrap_or(false) {
            paths.push(path);
        }
    }
    Ok(paths)
}

/// Sanitized review-only worktree paths for every existing local
/// branch. Used to recognize "non-orphan" worktrees without needing to
/// reverse-engineer slash-to-dash sanitization.
fn live_review_only_worktree_paths(
    project_root: &Path,
) -> Result<std::collections::HashSet<String>> {
    let output = git::run_raw(
        project_root,
        &["for-each-ref", "--format=%(refname:short)", "refs/heads/"],
    )
    .context("list git branches for review-only prune")?;
    if !output.status.success() {
        bail!(
            "git for-each-ref failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| !line.is_empty())
        .map(review_only_worktree_path)
        .collect())
}

fn worktree_relative_path(project_root: &Path, worktree_abs: &Path) -> String {
    let parent = project_root.parent().unwrap_or(project_root);
    match worktree_abs.strip_prefix(parent) {
        Ok(suffix) => format!("../{}", suffix.display()),
        Err(_) => worktree_abs.display().to_string(),
    }
}

fn is_dirty(worktree_path: &Path) -> Result<bool> {
    let output = git::run_raw(worktree_path, &["status", "--porcelain"])
        .with_context(|| format!("read status of {}", worktree_path.display()))?;
    if !output.status.success() {
        bail!(
            "git status failed in {}: {}",
            worktree_path.display(),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let any_tracked_change = String::from_utf8_lossy(&output.stdout)
        .lines()
        .any(|line| !line.starts_with("?? "));
    Ok(any_tracked_change)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_handles_simple_branch_name() {
        assert_eq!(review_only_worktree_path("main"), "../work-review-main");
    }

    #[test]
    fn path_sanitizes_slashes_to_dashes() {
        assert_eq!(
            review_only_worktree_path("feature/widget"),
            "../work-review-feature-widget"
        );
        assert_eq!(
            review_only_worktree_path("origin/main"),
            "../work-review-origin-main"
        );
    }

    #[test]
    fn path_handles_nested_slashes() {
        assert_eq!(
            review_only_worktree_path("user/topic/sub"),
            "../work-review-user-topic-sub"
        );
    }

    #[test]
    fn workspace_path_recognizer_accepts_canonical_shape() {
        assert!(is_review_only_worktree_workspace_path(
            "../work-review-main"
        ));
        assert!(is_review_only_worktree_workspace_path(
            "../work-review-feature-widget"
        ));
    }

    #[test]
    fn workspace_path_recognizer_rejects_other_shapes() {
        assert!(!is_review_only_worktree_workspace_path("."));
        assert!(!is_review_only_worktree_workspace_path("../work-1-wi-att"));
        assert!(!is_review_only_worktree_workspace_path("../work-review-"));
        assert!(!is_review_only_worktree_workspace_path(
            "../work-review-x/y"
        ));
    }
}
