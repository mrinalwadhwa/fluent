use anyhow::{Context, Result, bail};
use std::path::Path;
use std::process::Command;

use crate::coder::CoderKind;
use crate::content::ContentResolver;
use crate::hooks::{self, HookContext};
use crate::review;
use crate::run::{self, Run};
use crate::worktree;

pub fn merge_worktree_run(source_root: &Path, run: &Run) -> Result<()> {
    run_pre_merge_hooks_for_run(source_root, run)?;
    worktree::merge_run(source_root, &run.id, &run.dir)
}

/// Run the `check-pre-merge` hook (and `fix-pre-merge` if it exists)
/// against the legacy run's worktree before landing. Skips silently
/// if no hook is configured.
fn run_pre_merge_hooks_for_run(source_root: &Path, run: &Run) -> Result<()> {
    if hooks::find_hook(source_root, "check-pre-merge").is_none() {
        return Ok(());
    }

    let worktree_dir = run
        .worktree_dir()
        .context("No worktree path recorded for this run")?;
    eprintln!("  Running check-pre-merge hook...");

    let context = HookContext {
        artifact_dir: Some(run.dir.clone()),
        log_dir: run.dir.join("hooks"),
        ..Default::default()
    };

    let check_outcome = hooks::run_hook(source_root, "check-pre-merge", &worktree_dir, &context)?
        .expect("check-pre-merge presence confirmed above");
    if check_outcome.passed {
        return Ok(());
    }

    if hooks::find_hook(source_root, "fix-pre-merge").is_none() {
        bail!(
            "check-pre-merge failed (exit {}). Log: {}",
            check_outcome.exit_code,
            check_outcome.log_path.display()
        );
    }

    if worktree_is_dirty(&worktree_dir)? {
        bail!(
            "check-pre-merge failed and fix-pre-merge cannot run: worktree has uncommitted changes"
        );
    }

    eprintln!("  check-pre-merge failed; running fix-pre-merge...");
    let fix_outcome = hooks::run_hook(source_root, "fix-pre-merge", &worktree_dir, &context)?
        .expect("fix-pre-merge presence confirmed above");
    if !fix_outcome.passed {
        bail!(
            "fix-pre-merge failed (exit {}). Log: {}",
            fix_outcome.exit_code,
            fix_outcome.log_path.display()
        );
    }
    if worktree_is_dirty(&worktree_dir)? {
        commit_autofix(&worktree_dir)?;
        rerun_reviews_after_autofix(source_root, run, &worktree_dir)?;
    }

    let recheck = hooks::run_hook(source_root, "check-pre-merge", &worktree_dir, &context)?
        .expect("check-pre-merge presence already confirmed");
    if !recheck.passed {
        bail!(
            "check-pre-merge failed after fix-pre-merge (exit {}). Log: {}",
            recheck.exit_code,
            recheck.log_path.display()
        );
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
        .context("Failed to run git status")?;
    Ok(!output.stdout.is_empty())
}

fn commit_autofix(worktree_dir: &Path) -> Result<()> {
    git(
        worktree_dir,
        &["add", "--", ".", ":(exclude).factory"],
        "stage fix-pre-merge changes",
    )?;
    git(
        worktree_dir,
        &[
            "commit",
            "-m",
            "Apply fix-pre-merge changes",
            "-m",
            "- Apply changes produced by the project's fix-pre-merge hook before landing.",
        ],
        "commit fix-pre-merge changes",
    )
}

fn git(worktree_dir: &Path, args: &[&str], subject: &str) -> Result<()> {
    let output = Command::new("git")
        .args(["-C", &worktree_dir.to_string_lossy()])
        .args(args)
        .output()
        .with_context(|| format!("Failed to {subject}"))?;
    if !output.status.success() {
        bail!(
            "git {} failed:\n{}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}

fn rerun_reviews_after_autofix(source_root: &Path, run: &Run, worktree_dir: &Path) -> Result<()> {
    let wt_run_dir = worktree_dir.join(format!(".factory/runs/{}", run.id));
    let reviewer_filter = std::fs::read_to_string(wt_run_dir.join("reviewers"))
        .or_else(|_| std::fs::read_to_string(run.dir.join("reviewers")))
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    let coder_name = std::fs::read_to_string(wt_run_dir.join("coder"))
        .or_else(|_| std::fs::read_to_string(run.dir.join("coder")))
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "claude".to_string());
    let coder_kind = CoderKind::resolve(Some(&coder_name))?;
    let resolver = ContentResolver::new(Some(worktree_dir));

    eprintln!("  Rerunning reviewers after fix-pre-merge autofix...");
    let reviews_passed = review::run_reviews(
        &wt_run_dir,
        &run.id,
        &reviewer_filter,
        run::ReviewScope::Changes,
        &resolver,
        2,
        coder_kind,
    )?;
    worktree::copy_run_artifacts(&wt_run_dir, &run.dir)?;

    if !reviews_passed {
        bail!(
            "Cannot land run {}: reviewers did not pass after fix-pre-merge",
            run.id
        );
    }

    let source_status = source_root.join(format!(".factory/runs/{}/status", run.id));
    std::fs::write(source_status, run::RunStatus::Complete.as_str())?;

    Ok(())
}
