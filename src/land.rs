use anyhow::{Context, Result, bail};
use std::fs;
use std::path::Path;

use crate::checks::{self, FixOutcome};
use crate::coder::CoderKind;
use crate::config::{self, ProjectCheck};
use crate::content::ContentResolver;
use crate::review;
use crate::run::{self, Run};
use crate::worktree;

#[derive(Debug, Clone)]
pub struct PreLandCheckRun {
    pub results: Vec<checks::CheckRunResult>,
    pub after_autofix: Option<Vec<checks::CheckRunResult>>,
    pub fix_outcome: Option<FixOutcome>,
}

pub fn land_worktree_run(source_root: &Path, run: &Run) -> Result<()> {
    run_pre_land_checks(source_root, run)?;
    worktree::land_run(source_root, &run.id, &run.dir)
}

fn run_pre_land_checks(source_root: &Path, run: &Run) -> Result<()> {
    let Some(config) = config::load_factory_config(source_root)? else {
        return Ok(());
    };
    let checks: Vec<ProjectCheck> = config
        .checks
        .into_iter()
        .filter(|check| check.run_before_land)
        .collect();
    if checks.is_empty() {
        return Ok(());
    }

    let worktree_dir = run
        .worktree_dir()
        .context("No worktree path recorded for this run")?;
    eprintln!("  Running pre-land checks...");

    let outcome = run_pre_land_checks_for_worktree(&worktree_dir, &checks)?;
    match outcome.fix_outcome {
        Some(FixOutcome::Committed) => eprintln!("  Autofix changes committed."),
        Some(FixOutcome::NoChanges) => eprintln!("  Autofix produced no changes."),
        None => {}
    }

    if outcome.fix_outcome == Some(FixOutcome::Committed) {
        rerun_reviews_after_autofix(source_root, run, &worktree_dir)?;
    }

    Ok(())
}

pub fn run_pre_land_checks_for_worktree(
    worktree_dir: &Path,
    checks: &[ProjectCheck],
) -> Result<PreLandCheckRun> {
    let results = checks::run_pre_land_checks(worktree_dir, checks)?;
    let Some(failed) = results.iter().find(|result| !result.passed) else {
        return Ok(PreLandCheckRun {
            results,
            after_autofix: None,
            fix_outcome: None,
        });
    };

    if !failed.check.autofix || failed.check.fix_command.is_none() {
        bail!("{}", checks::format_check_failure(failed));
    }

    eprintln!(
        "  Check '{}' failed; running configured autofix...",
        failed.check.name
    );
    let fix_outcome = checks::apply_autofix(worktree_dir, &failed.check)?;
    let rerun_results = checks::run_pre_land_checks(worktree_dir, checks)?;
    if let Some(still_failed) = rerun_results.iter().find(|result| !result.passed) {
        bail!("{}", checks::format_check_failure(still_failed));
    }

    Ok(PreLandCheckRun {
        results,
        after_autofix: Some(rerun_results),
        fix_outcome: Some(fix_outcome),
    })
}

fn rerun_reviews_after_autofix(source_root: &Path, run: &Run, worktree_dir: &Path) -> Result<()> {
    let wt_run_dir = worktree_dir.join(format!(".factory/runs/{}", run.id));
    let reviewer_filter = fs::read_to_string(wt_run_dir.join("reviewers"))
        .or_else(|_| fs::read_to_string(run.dir.join("reviewers")))
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    let coder_name = fs::read_to_string(wt_run_dir.join("coder"))
        .or_else(|_| fs::read_to_string(run.dir.join("coder")))
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "claude".to_string());
    let coder_kind = CoderKind::resolve(Some(&coder_name))?;
    let resolver = ContentResolver::new(Some(worktree_dir));

    eprintln!("  Rerunning reviewers after autofix...");
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
            "Cannot land run {}: reviewers did not pass after autofix",
            run.id
        );
    }

    let source_status = source_root.join(format!(".factory/runs/{}/status", run.id));
    fs::write(source_status, run::RunStatus::Complete.as_str())?;

    Ok(())
}
