use anyhow::{Context, Result, bail};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, SystemTime};

// Only the synchronous local snapshot reader activates this scope. Ordinary
// status and mutation callers always execute Git and make their own decisions.
thread_local! {
    static SNAPSHOT_OBJECTS: std::cell::RefCell<Option<SnapshotObjects>> = const {
        std::cell::RefCell::new(None)
    };
}

#[derive(Default)]
struct SnapshotObjects {
    outputs: std::collections::BTreeMap<(std::path::PathBuf, Vec<String>), Output>,
    bytes: usize,
}

pub(crate) struct SnapshotObjectReads {
    previous: Option<SnapshotObjects>,
    // A scope must be dropped on the thread that owns its cache.
    _thread: std::marker::PhantomData<std::rc::Rc<()>>,
}

/// Reuse successful, fully addressed blob reads until this snapshot finishes.
pub(crate) fn snapshot_object_reads() -> SnapshotObjectReads {
    let previous = SNAPSHOT_OBJECTS.with(|slot| slot.replace(Some(SnapshotObjects::default())));
    SnapshotObjectReads {
        previous,
        _thread: std::marker::PhantomData,
    }
}

impl Drop for SnapshotObjectReads {
    fn drop(&mut self) {
        SNAPSHOT_OBJECTS.with(|slot| slot.replace(self.previous.take()));
    }
}

fn snapshot_object_key(cwd: &Path, args: &[&str]) -> Option<(std::path::PathBuf, Vec<String>)> {
    if !SNAPSHOT_OBJECTS.with(|slot| slot.borrow().is_some()) {
        return None;
    }
    // No options, revision expressions, abbreviated IDs, mutable refs, index
    // reads, or environment overrides qualify. Retain the complete arguments.
    let ["show", spec] = args else { return None };
    let (object, path) = spec.split_once(':')?;
    if !matches!(object.len(), 40 | 64)
        || !object.bytes().all(|byte| byte.is_ascii_hexdigit())
        || path.is_empty()
    {
        return None;
    }
    Some((
        cwd.canonicalize().ok()?,
        args.iter().map(|arg| (*arg).into()).collect(),
    ))
}

const LOCK_RETRY_MAX_ATTEMPTS: usize = 8;
const LOCK_RETRY_BASE_MS: u64 = 20;
const LOCK_RETRY_CAP_MS: u64 = 320;

pub(crate) fn build_command(cwd: &Path, args: &[&str]) -> Command {
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
        let mut command = cmd_builder();
        #[cfg(feature = "snapshot-profile")]
        let started = std::time::Instant::now();
        let output = command.output();
        #[cfg(feature = "snapshot-profile")]
        crate::snapshot_profile::git(&command, started);
        let output = output.context("Failed to invoke git")?;

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

/// Fast-forward a target branch only while it still names the verified base.
pub fn fast_forward_checked(cwd: &Path, branch: &str, base: &str, commit: &str) -> Result<()> {
    let current = run_stdout(cwd, &["rev-parse", branch], "resolve target branch")?;
    anyhow::ensure!(
        current == base,
        "target branch {branch} moved from {base} to {current}"
    );
    run(cwd, &["checkout", branch], "checkout target branch")?;
    let checked_out = run_stdout(cwd, &["rev-parse", "HEAD"], "resolve checked-out target")?;
    anyhow::ensure!(
        checked_out == base,
        "target checkout moved from {base} to {checked_out}"
    );
    run(
        cwd,
        &["merge", "--ff-only", commit],
        "fast-forward target branch",
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

/// Run Git with one host-owned environment override through the central
/// non-interactive command boundary.
pub fn run_stdout_with_env(
    cwd: &Path,
    args: &[&str],
    environment: (&str, &Path),
    action: &str,
) -> Result<String> {
    let output = run_with_lock_retry(|| {
        let mut command = build_command(cwd, args);
        command.env(environment.0, environment.1);
        command
    })?;
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

pub fn run_stdout_with_stdin_and_env(
    cwd: &Path,
    args: &[&str],
    input: &[u8],
    environment: (&str, &Path),
    action: &str,
) -> Result<String> {
    let mut command = build_command(cwd, args);
    command
        .env(environment.0, environment.1)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(feature = "snapshot-profile")]
    let started = std::time::Instant::now();
    let mut child = command.spawn().context("Failed to invoke git")?;
    child
        .stdin
        .take()
        .context("Failed to open git stdin")?
        .write_all(input)?;
    let output = child.wait_with_output();
    #[cfg(feature = "snapshot-profile")]
    crate::snapshot_profile::git(&command, started);
    let output = output.context("Failed to wait for git")?;
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
    let key = snapshot_object_key(cwd, args);
    if let Some(key) = &key {
        if let Some(output) = SNAPSHOT_OBJECTS.with(|slot| {
            slot.borrow()
                .as_ref()
                .and_then(|cache| cache.outputs.get(key).cloned())
        }) {
            return Ok(output);
        }
    }
    let output = run_with_lock_retry(|| build_command(cwd, args))?;
    if output.status.success() {
        if let Some(key) = key {
            SNAPSHOT_OBJECTS.with(|slot| {
                if let Some(cache) = slot.borrow_mut().as_mut() {
                    let bytes = output.stdout.len() + output.stderr.len();
                    // Bound retained source bytes as well as the scope lifetime.
                    if cache.bytes + bytes <= 64 * 1024 * 1024 {
                        cache.bytes += bytes;
                        cache.outputs.insert(key, output.clone());
                    }
                }
            });
        }
    }
    Ok(output)
}

/// Read a blob as Git would write it to the given checkout path.
/// Retain the path's original bytes when selecting attributes and filters.
pub fn read_checkout_blob(cwd: &Path, object: &str, path: &Path) -> Result<Output> {
    run_with_lock_retry(|| {
        let mut command = build_command(cwd, &["cat-file", "--filters"]);
        command.arg("--path").arg(path).arg(object);
        command
    })
}

/// The effective ignore rule Git reports for one path.
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct IgnoreMatch {
    pub source: String,
    pub line: String,
    pub pattern: String,
    pub path: String,
}

/// Return effective ignore rules for `paths`, including repository, info,
/// global, nested, and command-line exclude sources.
pub(crate) fn ignored_paths(cwd: &Path, paths: &[&str]) -> Result<Vec<IgnoreMatch>> {
    let mut input = Vec::new();
    for path in paths {
        input.extend_from_slice(path.as_bytes());
        input.push(0);
    }
    let output = run_raw_with_stdin(
        cwd,
        &["check-ignore", "-z", "-v", "--no-index", "--stdin"],
        &input,
    )?;
    if !output.status.success() && output.status.code() != Some(1) {
        bail!(
            "git check-ignore failed (exit {}) while checking Fluent durable paths\n  cwd: {}\n{}",
            exit_code_display(&output),
            cwd.display(),
            format_output(&output)
        );
    }
    parse_ignore_matches(&output.stdout)
}

fn parse_ignore_matches(output: &[u8]) -> Result<Vec<IgnoreMatch>> {
    let mut fields = output.split(|byte| *byte == 0).collect::<Vec<_>>();
    if fields.last() == Some(&&[][..]) {
        fields.pop();
    }
    if fields.len() % 4 != 0 {
        bail!(
            "git check-ignore returned a malformed verbose record with {} fields",
            fields.len()
        );
    }
    Ok(fields
        .chunks_exact(4)
        .filter_map(|fields| {
            let pattern = String::from_utf8_lossy(fields[2]).into_owned();
            // In verbose mode Git also reports the effective negation that made
            // a path trackable. Only positive patterns describe ignored paths.
            if pattern.starts_with('!') {
                return None;
            }
            Some(IgnoreMatch {
                source: String::from_utf8_lossy(fields[0]).into_owned(),
                line: String::from_utf8_lossy(fields[1]).into_owned(),
                pattern,
                path: String::from_utf8_lossy(fields[3]).into_owned(),
            })
        })
        .collect())
}

/// Return staged, unstaged, and untracked changes outside `.fluent/`.
///
/// Keep Git's porcelain diagnostic intact so callers can name every path without
/// maintaining a second parser for quoted path syntax.
pub fn non_fluent_worktree_status(cwd: &Path) -> Result<String> {
    let output = run_raw(
        cwd,
        &[
            "status",
            "--porcelain",
            "--untracked-files=all",
            "--",
            ".",
            ":(exclude).fluent",
        ],
    )?;
    if !output.status.success() {
        bail!(
            "git status failed (exit {}) while checking source checkout status\n  cwd: {}\n{}",
            exit_code_display(&output),
            cwd.display(),
            format_output(&output)
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .trim_end()
        .to_string())
}

/// Run `git <args>` with byte input on stdin and check the exit status.
pub fn run_with_stdin(cwd: &Path, args: &[&str], input: &[u8], action: &str) -> Result<()> {
    let output = run_raw_with_stdin(cwd, args, input)?;
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

fn run_raw_with_stdin(cwd: &Path, args: &[&str], input: &[u8]) -> Result<Output> {
    let mut command = build_command(cwd, args);
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(feature = "snapshot-profile")]
    let started = std::time::Instant::now();
    let mut child = command.spawn().context("Failed to invoke git")?;
    child
        .stdin
        .take()
        .context("Failed to open git stdin")?
        .write_all(input)?;
    let output = child.wait_with_output();
    #[cfg(feature = "snapshot-profile")]
    crate::snapshot_profile::git(&command, started);
    output.context("Failed to wait for git")
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
    #[cfg(feature = "snapshot-profile")]
    fn snapshot_object_reads_reuse_only_successful_immutable_queries() {
        let root = TempDir::new().unwrap();
        run(root.path(), &["init"], "init").unwrap();
        run(
            root.path(),
            &["config", "user.name", "Fixture"],
            "configure",
        )
        .unwrap();
        run(
            root.path(),
            &["config", "user.email", "fixture@example.com"],
            "configure",
        )
        .unwrap();
        std::fs::write(root.path().join("file"), "first").unwrap();
        run(root.path(), &["add", "."], "stage").unwrap();
        run(root.path(), &["commit", "-m", "Initial"], "commit").unwrap();
        let commit = run_stdout(root.path(), &["rev-parse", "HEAD"], "resolve").unwrap();
        let other = TempDir::new().unwrap();
        run(other.path(), &["init"], "init second repository").unwrap();
        let spec = format!("{commit}:file");
        let missing = format!("{commit}:missing");
        let (_, profile) = crate::snapshot_profile::capture(|| {
            let _scope = snapshot_object_reads();
            for _ in 0..2 {
                assert_eq!(
                    run_raw(root.path(), &["show", &spec]).unwrap().stdout,
                    b"first"
                );
                assert!(
                    !run_raw(root.path(), &["show", &missing])
                        .unwrap()
                        .status
                        .success()
                );
                assert_eq!(
                    run_raw(root.path(), &["show", "HEAD:file"]).unwrap().stdout,
                    b"first"
                );
            }
            assert!(
                !run_raw(other.path(), &["show", &spec])
                    .unwrap()
                    .status
                    .success(),
                "an identical object query in another repository must execute"
            );
            let abbreviated = format!("{}:file", &commit[..12]);
            for _ in 0..2 {
                assert_eq!(
                    run_raw(root.path(), &["show", &abbreviated])
                        .unwrap()
                        .stdout,
                    b"first"
                );
            }
        });
        assert_eq!(
            profile.git.len(),
            8,
            "only the full object read may be reused"
        );
        let (_, refresh) = crate::snapshot_profile::capture(|| {
            let _scope = snapshot_object_reads();
            run_raw(root.path(), &["show", &spec]).unwrap();
        });
        assert_eq!(refresh.git.len(), 1, "each snapshot must start empty");
        let _ = std::panic::catch_unwind(|| {
            let _scope = snapshot_object_reads();
            panic!("release scope on unwind");
        });
        let (_, after) = crate::snapshot_profile::capture(|| {
            run_raw(root.path(), &["show", &spec]).unwrap();
            run_raw(root.path(), &["show", &spec]).unwrap();
        });
        assert_eq!(
            after.git.len(),
            2,
            "ordinary Git callers must remain uncached"
        );
    }

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

    #[test]
    fn parse_ignore_matches_keeps_positive_rules_and_discards_negations() {
        let output = [
            b".gitignore\0".as_slice(),
            b"4\0.fluent/\0.fluent/tester.yaml\0".as_slice(),
            b".gitignore\0".as_slice(),
            b"5\0!.fluent/hooks/\0.fluent/hooks/check-pre-merge\0".as_slice(),
        ]
        .concat();

        assert_eq!(
            parse_ignore_matches(&output).unwrap(),
            vec![IgnoreMatch {
                source: ".gitignore".into(),
                line: "4".into(),
                pattern: ".fluent/".into(),
                path: ".fluent/tester.yaml".into(),
            }]
        );
    }

    #[test]
    fn parse_ignore_matches_rejects_incomplete_records() {
        let error = parse_ignore_matches(b".gitignore\0").unwrap_err();
        assert!(error.to_string().contains("malformed verbose record"));
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
