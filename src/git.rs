use anyhow::{Context, Result, bail};
use std::path::Path;
use std::process::{Command, Output};

fn build_command(cwd: &Path, args: &[&str]) -> Command {
    let mut cmd = Command::new("git");
    cmd.current_dir(cwd);
    cmd.env("GIT_EDITOR", "true");
    cmd.env("GIT_SEQUENCE_EDITOR", "true");
    cmd.env("GIT_TERMINAL_PROMPT", "0");
    cmd.args(["-c", "commit.gpgsign=false"]);
    cmd.args(["-c", "core.editor=true"]);
    cmd.args(args);
    cmd
}

/// Run `git <args>` in `cwd` and check the exit status.
pub fn run(cwd: &Path, args: &[&str], action: &str) -> Result<()> {
    let output = build_command(cwd, args)
        .output()
        .with_context(|| format!("Failed to {action}"))?;
    if output.status.success() {
        return Ok(());
    }
    bail!(
        "git {} failed (exit {}) while {action}\n  cwd: {}\n{}",
        args.join(" "),
        exit_code_display(&output),
        cwd.display(),
        format_output(&output)
    )
}

/// Run `git <args>` in `cwd`, return trimmed stdout on success.
pub fn run_stdout(cwd: &Path, args: &[&str], action: &str) -> Result<String> {
    let output = build_command(cwd, args)
        .output()
        .with_context(|| format!("Failed to {action}"))?;
    if !output.status.success() {
        bail!(
            "git {} failed (exit {}) while {action}\n  cwd: {}\n{}",
            args.join(" "),
            exit_code_display(&output),
            cwd.display(),
            format_output(&output)
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Run `git <args>` in `cwd`, return the raw `Output`.
pub fn run_raw(cwd: &Path, args: &[&str]) -> Result<Output> {
    build_command(cwd, args)
        .output()
        .context("Failed to run git")
}

fn exit_code_display(output: &Output) -> String {
    output
        .status
        .code()
        .map_or("signal".to_string(), |c| c.to_string())
}

fn format_output(output: &Output) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let mut combined = String::new();
    if !stdout.trim().is_empty() {
        combined.push_str("  stdout:\n  ");
        combined.push_str(&stdout.trim_end().replace('\n', "\n  "));
        combined.push('\n');
    }
    if !stderr.trim().is_empty() {
        combined.push_str("  stderr:\n  ");
        combined.push_str(&stderr.trim_end().replace('\n', "\n  "));
        combined.push('\n');
    }
    if combined.is_empty() {
        combined.push_str("  (no output)\n");
    }
    combined
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn build_command_sets_non_interactive_env() {
        let tmp = TempDir::new().unwrap();
        let cmd = build_command(tmp.path(), &["status"]);
        let envs: Vec<_> = cmd.get_envs().collect();
        assert!(envs
            .iter()
            .any(|(k, v)| *k == "GIT_EDITOR"
                && *v == Some(std::ffi::OsStr::new("true"))));
        assert!(envs
            .iter()
            .any(|(k, v)| *k == "GIT_SEQUENCE_EDITOR"
                && *v == Some(std::ffi::OsStr::new("true"))));
        assert!(envs
            .iter()
            .any(|(k, v)| *k == "GIT_TERMINAL_PROMPT"
                && *v == Some(std::ffi::OsStr::new("0"))));
    }

    #[test]
    fn build_command_passes_gpgsign_false() {
        let tmp = TempDir::new().unwrap();
        let cmd = build_command(tmp.path(), &["commit", "-m", "test"]);
        let args: Vec<_> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().to_string())
            .collect();
        let pos = args.iter().position(|a| a == "commit.gpgsign=false");
        assert!(
            pos.is_some(),
            "Expected commit.gpgsign=false in args: {args:?}"
        );
        assert_eq!(args[pos.unwrap() - 1], "-c");
    }

    #[test]
    fn build_command_passes_core_editor_true() {
        let tmp = TempDir::new().unwrap();
        let cmd = build_command(tmp.path(), &["commit", "-m", "test"]);
        let args: Vec<_> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().to_string())
            .collect();
        let pos = args.iter().position(|a| a == "core.editor=true");
        assert!(
            pos.is_some(),
            "Expected core.editor=true in args: {args:?}"
        );
        assert_eq!(args[pos.unwrap() - 1], "-c");
    }

    #[test]
    fn run_returns_error_with_full_context() {
        let tmp = TempDir::new().unwrap();
        build_command(tmp.path(), &["init"]).output().unwrap();

        let result = run(
            tmp.path(),
            &["checkout", "nonexistent-branch"],
            "switch to branch",
        );
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("switch to branch"),
            "Error should contain action: {err}"
        );
        assert!(
            err.contains(&tmp.path().display().to_string()),
            "Error should contain cwd: {err}"
        );
    }
}
