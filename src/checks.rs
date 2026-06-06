use anyhow::{Context, Result, bail};
use std::path::Path;
use std::process::{Command, Output};

use crate::config::ProjectCheck;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FixOutcome {
    NoChanges,
    Committed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckRunResult {
    pub check: ProjectCheck,
    pub passed: bool,
    pub output: String,
}

pub fn run_pre_land_checks(
    worktree_dir: &Path,
    checks: &[ProjectCheck],
) -> Result<Vec<CheckRunResult>> {
    let mut results = Vec::new();
    for check in checks.iter().filter(|check| check.run_before_land) {
        let output = run_shell_command(worktree_dir, &check.command)
            .with_context(|| format!("Failed to run check '{}'", check.name))?;
        let result = CheckRunResult {
            check: check.clone(),
            passed: output.status.success(),
            output: command_output(&output),
        };
        if !result.passed {
            results.push(result);
            return Ok(results);
        }
        results.push(result);
    }
    Ok(results)
}

pub fn apply_autofix(worktree_dir: &Path, check: &ProjectCheck) -> Result<FixOutcome> {
    let Some(fix_command) = check.fix_command.as_deref() else {
        bail!("Check '{}' has no fix command configured", check.name);
    };

    if worktree_is_dirty(worktree_dir)? {
        bail!(
            "Cannot autofix check '{}': worktree has uncommitted changes",
            check.name
        );
    }

    let output = run_shell_command(worktree_dir, fix_command)
        .with_context(|| format!("Failed to run fix command for check '{}'", check.name))?;
    if !output.status.success() {
        bail!(
            "Autofix failed for check '{}'\nCommand: {}\n{}",
            check.name,
            fix_command,
            command_output(&output)
        );
    }

    if !worktree_is_dirty(worktree_dir)? {
        return Ok(FixOutcome::NoChanges);
    }

    git(worktree_dir, &["add", "--", ".", ":(exclude).factory"])?;
    git(
        worktree_dir,
        &[
            "commit",
            "-m",
            "Apply project check autofix",
            "-m",
            "- Apply configured autofix command before landing.",
        ],
    )?;
    Ok(FixOutcome::Committed)
}

pub fn format_check_failure(result: &CheckRunResult) -> String {
    let mut message = format!(
        "Pre-land check '{}' failed\nCommand: {}\n{}",
        result.check.name, result.check.command, result.output
    );
    if let Some(fix_command) = &result.check.fix_command {
        message.push_str(&format!("\nConfigured fix command: {fix_command}"));
    }
    message
}

fn run_shell_command(worktree_dir: &Path, command: &str) -> Result<Output> {
    Command::new("/bin/sh")
        .args(["-c", command])
        .current_dir(worktree_dir)
        .output()
        .context("Failed to launch shell command")
}

fn command_output(output: &Output) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let mut combined = String::new();
    if !stdout.trim().is_empty() {
        combined.push_str("stdout:\n");
        combined.push_str(stdout.trim_end());
        combined.push('\n');
    }
    if !stderr.trim().is_empty() {
        combined.push_str("stderr:\n");
        combined.push_str(stderr.trim_end());
        combined.push('\n');
    }
    if combined.is_empty() {
        combined.push_str("(no output)\n");
    }
    combined
}

fn worktree_is_dirty(worktree_dir: &Path) -> Result<bool> {
    let output = git_output(
        worktree_dir,
        &[
            "status",
            "--porcelain",
            "--untracked-files=normal",
            "--",
            ".",
            ":(exclude).factory",
        ],
    )?;
    Ok(!output.stdout.is_empty())
}

fn git(worktree_dir: &Path, args: &[&str]) -> Result<()> {
    let output = git_output(worktree_dir, args)?;
    if output.status.success() {
        return Ok(());
    }
    bail!(
        "git {} failed:\n{}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git_output(worktree_dir: &Path, args: &[&str]) -> Result<Output> {
    Command::new("git")
        .args(["-C", &worktree_dir.to_string_lossy()])
        .args(args)
        .output()
        .context("Failed to run git")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn check(command: &str) -> ProjectCheck {
        ProjectCheck {
            name: "format".into(),
            command: command.into(),
            fix_command: None,
            autofix: false,
            run_before_land: true,
        }
    }

    #[test]
    fn runs_pre_land_checks() {
        let tmp = TempDir::new().unwrap();

        let results = run_pre_land_checks(tmp.path(), &[check("printf ok")]).unwrap();

        assert_eq!(results.len(), 1);
        assert!(results[0].passed);
        assert!(results[0].output.contains("ok"));
    }

    #[test]
    fn stops_at_first_failing_check() {
        let tmp = TempDir::new().unwrap();

        let results = run_pre_land_checks(
            tmp.path(),
            &[check("printf bad >&2; exit 1"), check("printf skipped")],
        )
        .unwrap();

        assert_eq!(results.len(), 1);
        assert!(!results[0].passed);
        assert!(format_check_failure(&results[0]).contains("bad"));
    }

    #[test]
    fn ignores_factory_changes_when_checking_dirty_state() {
        let tmp = TempDir::new().unwrap();
        Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "commit.gpgsign", "false"])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.email", "test@test"])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.name", "test"])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        fs::create_dir_all(tmp.path().join(".factory/runs/run")).unwrap();
        fs::write(tmp.path().join(".factory/runs/run/status"), "complete").unwrap();

        let mut project_check = check("true");
        project_check.fix_command = Some("true".into());
        project_check.autofix = true;

        assert_eq!(
            apply_autofix(tmp.path(), &project_check).unwrap(),
            FixOutcome::NoChanges
        );
    }
}
