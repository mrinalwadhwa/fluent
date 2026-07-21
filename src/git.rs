use anyhow::{Context, Result, bail};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, SystemTime};

const LOCK_RETRY_MAX_ATTEMPTS: usize = 8;
const LOCK_RETRY_BASE_MS: u64 = 20;
const LOCK_RETRY_CAP_MS: u64 = 320;

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

fn is_lock_error(stderr: &str) -> bool {
    let s = stderr.to_lowercase();
    s.contains("could not lock")
        || s.contains("lock failed")
        || (s.contains(": file exists")
            && (s.contains(".lock") || s.contains("/index") || s.contains("/head")))
        || (s.contains("resource temporarily unavailable") && s.contains(".lock"))
}

fn lock_jitter_factor() -> f64 {
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos() as u64;
    let mixed = nanos ^ (std::process::id() as u64);
    let bucket = (mixed % 100) as f64;
    (bucket - 50.0) / 200.0
}

fn backoff_duration(attempt: usize) -> Duration {
    let exp = LOCK_RETRY_BASE_MS << (attempt - 1).min(4);
    let base = exp.min(LOCK_RETRY_CAP_MS);
    let jitter = (base as f64) * lock_jitter_factor();
    Duration::from_millis((base as f64 + jitter) as u64)
}

fn run_with_lock_retry(cmd_builder: impl Fn() -> Command) -> Result<Output> {
    let mut last_output: Option<Output> = None;
    for attempt in 1..=LOCK_RETRY_MAX_ATTEMPTS {
        let output = cmd_builder().output().context("Failed to invoke git")?;

        if output.status.success() {
            return Ok(output);
        }

        let stderr = String::from_utf8_lossy(&output.stderr);
        if !is_lock_error(&stderr) {
            return Ok(output);
        }

        if attempt == LOCK_RETRY_MAX_ATTEMPTS {
            eprintln!(
                "git lock retry budget exhausted after {LOCK_RETRY_MAX_ATTEMPTS} attempts: {}",
                stderr.lines().next().unwrap_or("(empty)")
            );
            last_output = Some(output);
            break;
        }

        std::thread::sleep(backoff_duration(attempt));
    }

    Ok(last_output.expect("loop exits via return or last_output set"))
}

/// Run `git <args>` in `cwd` and check the exit status.
pub fn run(cwd: &Path, args: &[&str], action: &str) -> Result<()> {
    let output = run_with_lock_retry(|| build_command(cwd, args))?;
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
    let output = run_with_lock_retry(|| build_command(cwd, args))?;
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
    run_with_lock_retry(|| build_command(cwd, args))
}

/// Run `git <args>` with byte input on stdin and check the exit status.
pub fn run_with_stdin(cwd: &Path, args: &[&str], input: &[u8], action: &str) -> Result<()> {
    let mut child = build_command(cwd, args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .context("Failed to invoke git")?;
    child
        .stdin
        .take()
        .context("Failed to open git stdin")?
        .write_all(input)?;
    let output = child.wait_with_output()?;
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
        assert!(
            envs.iter()
                .any(|(k, v)| *k == "GIT_EDITOR" && *v == Some(std::ffi::OsStr::new("true")))
        );
        assert!(
            envs.iter()
                .any(|(k, v)| *k == "GIT_SEQUENCE_EDITOR"
                    && *v == Some(std::ffi::OsStr::new("true")))
        );
        assert!(
            envs.iter()
                .any(|(k, v)| *k == "GIT_TERMINAL_PROMPT" && *v == Some(std::ffi::OsStr::new("0")))
        );
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
        assert!(pos.is_some(), "Expected core.editor=true in args: {args:?}");
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

    #[test]
    fn run_with_stdin_applies_patch_input() {
        let tmp = TempDir::new().unwrap();
        run(tmp.path(), &["init"], "initialize repository").unwrap();
        std::fs::write(tmp.path().join("sample.txt"), "before\n").unwrap();
        let patch = b"diff --git a/sample.txt b/sample.txt\n\
index 90be1a7..38f8e88 100644\n\
--- a/sample.txt\n\
+++ b/sample.txt\n\
@@ -1 +1 @@\n\
-before\n\
+after\n";

        run_with_stdin(tmp.path(), &["apply"], patch, "apply test patch").unwrap();

        assert_eq!(
            std::fs::read_to_string(tmp.path().join("sample.txt")).unwrap(),
            "after\n"
        );
    }

    // Lock-error detection tests

    #[test]
    fn is_lock_error_recognizes_could_not_lock_config() {
        assert!(is_lock_error(
            "error: could not lock config file /path/.git/config: File exists"
        ));
    }

    #[test]
    fn is_lock_error_recognizes_index_lock_file_exists() {
        assert!(is_lock_error(
            "fatal: Unable to create '/path/.git/index.lock': File exists."
        ));
    }

    #[test]
    fn is_lock_error_recognizes_head_lock_resource_temporarily_unavailable() {
        assert!(is_lock_error(
            "error: Unable to create '/path/.git/HEAD.lock': Resource temporarily unavailable"
        ));
    }

    #[test]
    fn is_lock_error_recognizes_refs_lock_file_exists() {
        assert!(is_lock_error(
            "error: Unable to create '/path/.git/refs/heads/main.lock': File exists"
        ));
    }

    #[test]
    fn is_lock_error_recognizes_lock_failed() {
        assert!(is_lock_error("error: lock failed on refs/heads/main"));
    }

    #[test]
    fn is_lock_error_does_not_match_authentication_failure() {
        assert!(!is_lock_error(
            "fatal: Authentication failed for 'https://github.com/repo.git/'"
        ));
    }

    #[test]
    fn is_lock_error_does_not_match_network_error() {
        assert!(!is_lock_error(
            "fatal: unable to access 'https://github.com/repo.git/': Could not resolve host"
        ));
    }

    #[test]
    fn is_lock_error_does_not_match_unrelated_file_exists_error() {
        assert!(!is_lock_error(
            "fatal: destination path '/tmp/repo' already exists and is not an empty directory."
        ));
        assert!(!is_lock_error("error: '/some/path/data.txt': File exists"));
    }

    #[test]
    fn is_lock_error_case_insensitive() {
        assert!(is_lock_error("ERROR: COULD NOT LOCK config file"));
        assert!(is_lock_error("Fatal: LOCK FAILED on refs/heads/main"));
    }

    // Backoff duration tests

    #[test]
    fn backoff_duration_doubles_for_first_5_attempts() {
        let expected_bases: [u64; 5] = [20, 40, 80, 160, 320];
        for (i, &expected_base) in expected_bases.iter().enumerate() {
            let attempt = i + 1;
            let dur = backoff_duration(attempt);
            let ms = dur.as_millis() as u64;
            let lower = (expected_base as f64 * 0.75) as u64;
            let upper = (expected_base as f64 * 1.25) as u64;
            assert!(
                ms >= lower && ms <= upper,
                "attempt {attempt}: expected {lower}..={upper}ms, got {ms}ms"
            );
        }
    }

    #[test]
    fn backoff_duration_caps_at_320ms_after_5th_attempt() {
        for attempt in 6..=8 {
            let dur = backoff_duration(attempt);
            let ms = dur.as_millis() as u64;
            let lower = (320_f64 * 0.75) as u64;
            let upper = (320_f64 * 1.25) as u64;
            assert!(
                ms >= lower && ms <= upper,
                "attempt {attempt}: expected {lower}..={upper}ms (capped), got {ms}ms"
            );
        }
    }

    #[test]
    fn backoff_duration_applies_jitter_within_25_percent() {
        for attempt in 1..=8 {
            let dur = backoff_duration(attempt);
            let ms = dur.as_millis() as u64;
            let exp = LOCK_RETRY_BASE_MS << (attempt - 1).min(4);
            let base = exp.min(LOCK_RETRY_CAP_MS);
            let lower = (base as f64 * 0.75) as u64;
            let upper = (base as f64 * 1.25) as u64;
            assert!(
                ms >= lower && ms <= upper,
                "attempt {attempt}: {ms}ms outside [{lower}, {upper}]"
            );
        }
    }

    #[test]
    fn lock_jitter_factor_within_range() {
        for _ in 0..20 {
            let f = lock_jitter_factor();
            assert!(
                f >= -0.25 && f <= 0.25,
                "jitter factor {f} outside [-0.25, 0.25]"
            );
        }
    }
}
