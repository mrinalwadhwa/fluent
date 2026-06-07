use anyhow::Result;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use crate::coder::{Coder, CoderKind};
use crate::content::ContentResolver;
use crate::report;
use crate::review;
use crate::run::{ReviewScope, Run, RunMode, RunStatus};

const MAX_SESSIONS: u32 = 50;
const MAX_CONSECUTIVE_FAILURES: u32 = 3;
const MAX_REVIEW_ROUNDS: u32 = 10;

/// Configuration for the session loop.
pub struct SessionConfig {
    pub run: Run,
    pub system_prompt: String,
    pub working_dir: PathBuf,
    pub extra_args: Vec<String>,
    pub resolver: ContentResolver,
}

/// Hooks called during the session loop. Override for testing.
pub trait SessionHooks {
    fn pre_session(&self) -> Result<()> {
        Ok(())
    }
    fn sleep(&self, duration: Duration) {
        thread::sleep(duration);
    }
}

/// Default hooks (no-op pre_session, real sleep).
pub struct DefaultHooks;

impl SessionHooks for DefaultHooks {}

/// Hooks for sandboxed runs that refresh credentials before each session.
///
/// The sandbox blocks Keychain access, so the factory must refresh
/// OAuth tokens outside the sandbox between sessions.
pub struct SandboxedHooks;

impl SessionHooks for SandboxedHooks {
    fn pre_session(&self) -> Result<()> {
        crate::credential::refresh_credentials()
    }
}

/// Run the session loop.
pub fn run_session_loop(
    author: &dyn Coder,
    config: &SessionConfig,
    hooks: &dyn SessionHooks,
    coder_kind: CoderKind,
) -> Result<()> {
    let run = &config.run;
    let run_dir = &run.dir;

    // Detect run mode
    let run_mode = run.mode();
    let reviewer_filter = run.reviewer_filter();

    // Set status to executing if starting fresh
    let current_status = run.status()?;
    if current_status == RunStatus::Planned {
        run.set_status(&RunStatus::Executing)?;
    }

    // For review runs, run reviewers and stop.
    let mut review_round: u32 = 0;
    let initial_prompt = if run_mode == RunMode::Review {
        eprintln!("  Mode: review (reviewers only)");
        run.set_status(&RunStatus::Reviewing)?;
        review_round += 1;
        review::run_reviews(
            run_dir,
            &run.id,
            &reviewer_filter,
            ReviewScope::Full,
            &config.resolver,
            review_round,
            coder_kind,
        )?;
        run.set_status(&RunStatus::Complete)?;
        report::generate_report(run_dir, &run.id, 0)?;
        eprintln!("\n  Run {} completed (review only).", run.id);
        return Ok(());
    } else if run.has_handoff() {
        format!(
            "Read the handoff at .factory/runs/{}/handoff.md and continue working.",
            run.id
        )
    } else {
        format!(
            "Read the brief at .factory/runs/{}/brief.md and begin working.",
            run.id
        )
    };

    let mut session_count = run.session_count() as u32;
    let mut consecutive_failures: u32 = 0;
    let mut prompt = initial_prompt;

    loop {
        session_count += 1;
        eprintln!("\n  === Session {} (run: {}) ===\n", session_count, run.id);

        if session_count > MAX_SESSIONS {
            eprintln!("  Max sessions ({}) reached — stopping.", MAX_SESSIONS);
            run.set_status(&RunStatus::Failed)?;
            break;
        }

        // Per-session hook
        hooks.pre_session()?;

        // Set up transcript capture
        let session_dir = run_dir.join(format!("sessions/session-{session_count}"));
        fs::create_dir_all(&session_dir)?;
        let transcript_file = session_dir.join("transcript.jsonl");

        let session_start = std::time::Instant::now();

        // Launch author
        let exit_code = author.run(
            &prompt,
            &config.system_prompt,
            &config.working_dir,
            &config.extra_args,
            Some(&transcript_file),
        )?;

        let session_elapsed = session_start.elapsed().as_secs();
        eprintln!("\n  Author exited (code: {exit_code}, {session_elapsed}s)");

        // Write session metadata to sessions.log
        {
            let status_after = run
                .status()
                .map(|s| s.to_string())
                .unwrap_or_else(|_| "unknown".into());
            let timestamp = now_iso();
            let mut log_file = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(run_dir.join("sessions.log"))?;
            use std::io::Write;
            writeln!(
                log_file,
                "{timestamp} session={session_count} exit={exit_code} duration={session_elapsed}s status={status_after}"
            )?;
        }

        // Track consecutive failures
        if exit_code != 0 {
            consecutive_failures += 1;
            if consecutive_failures >= MAX_CONSECUTIVE_FAILURES {
                eprintln!(
                    "  {} consecutive failures — stopping.",
                    consecutive_failures
                );
                run.set_status(&RunStatus::Failed)?;
                break;
            }
        } else {
            consecutive_failures = 0;
        }

        // Read status and decide
        let status = run.status()?;
        eprintln!("  Run status: {status}");

        match status {
            RunStatus::Complete => {
                let review_scope = ReviewScope::Changes;

                // Skip run-scoped reviews when no code changed and no
                // explicit scope was requested by the user.
                if config.run.scope().is_none() && !has_changes(&config.working_dir, run_dir) {
                    eprintln!("  No code changes — skipping reviews.");
                    report::generate_report(run_dir, &run.id, session_count)?;
                    eprintln!("\n  Run {} completed.", run.id);
                    break;
                }

                run.set_status(&RunStatus::Reviewing)?;
                review_round += 1;
                if review_round > MAX_REVIEW_ROUNDS {
                    eprintln!(
                        "  Max review rounds ({}) reached — accepting current state.",
                        MAX_REVIEW_ROUNDS
                    );
                    if complete_or_continue_dirty(
                        run,
                        &config.working_dir,
                        "Review limit reached, but the worktree still has uncommitted changes.",
                    )? {
                        report::generate_report(run_dir, &run.id, session_count)?;
                        eprintln!("\n  Run {} completed (review limit).", run.id);
                        break;
                    }
                    prompt = format!(
                        "Review limit reached, but the worktree still has uncommitted changes. Read the handoff at .factory/runs/{}/handoff.md and commit or resolve them before completing.",
                        run.id
                    );
                    hooks.sleep(Duration::from_secs(2));
                    continue;
                }
                let review_start = std::time::Instant::now();
                let all_pass = review::run_reviews(
                    run_dir,
                    &run.id,
                    &reviewer_filter,
                    review_scope,
                    &config.resolver,
                    review_round,
                    coder_kind,
                )?;
                // Log review phase
                {
                    let review_elapsed = review_start.elapsed().as_secs();
                    let timestamp = now_iso();
                    let verdict = if all_pass { "pass" } else { "fail" };
                    let mut log_file = fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(run_dir.join("sessions.log"))?;
                    use std::io::Write;
                    writeln!(
                        log_file,
                        "{timestamp} review={review_round} duration={review_elapsed}s verdict={verdict}"
                    )?;
                }
                if all_pass {
                    if complete_or_continue_dirty(
                        run,
                        &config.working_dir,
                        "Reviews passed, but the worktree still has uncommitted changes.",
                    )? {
                        report::generate_report(run_dir, &run.id, session_count)?;
                        eprintln!("\n  Run {} completed.", run.id);
                        break;
                    }
                    eprintln!(
                        "  Reviews passed, but uncommitted changes remain — restarting author..."
                    );
                    prompt = format!(
                        "Reviews passed, but the worktree still has uncommitted changes. Read the handoff at .factory/runs/{}/handoff.md and commit or resolve them before completing.",
                        run.id
                    );
                    hooks.sleep(Duration::from_secs(2));
                } else {
                    eprintln!("  Review returned findings — restarting author...");
                    run.set_status(&RunStatus::Executing)?;
                    prompt = format!(
                        "Reviewers found issues. Read the review artifacts at .factory/runs/{}/reviews/ and address the findings.",
                        run.id
                    );
                    hooks.sleep(Duration::from_secs(2));
                }
            }
            RunStatus::NeedsUser => {
                eprintln!("\n  Run {} needs your input.", run.id);
                if run.has_handoff() {
                    eprintln!("  See: {}/handoff.md", run_dir.display());
                }
                break;
            }
            RunStatus::Failed => {
                eprintln!("\n  Run {} failed.", run.id);
                if run.has_handoff() {
                    eprintln!("  See: {}/handoff.md", run_dir.display());
                }
                break;
            }
            RunStatus::Executing => {
                eprintln!("  Restarting session...");
                prompt = format!(
                    "Continue from the handoff at .factory/runs/{}/handoff.md",
                    run.id
                );
                hooks.sleep(Duration::from_secs(2));
            }
            RunStatus::RateLimited => {
                eprintln!("  Rate limited — waiting 5 minutes...");
                prompt = format!(
                    "Continue from the handoff at .factory/runs/{}/handoff.md",
                    run.id
                );
                hooks.sleep(Duration::from_secs(300));
            }
            _ => {
                eprintln!("  Unexpected status \"{}\" — stopping.", status.as_str());
                break;
            }
        }
    }

    eprintln!("\n  Total sessions: {session_count}");
    Ok(())
}

fn complete_or_continue_dirty(run: &Run, working_dir: &Path, reason: &str) -> Result<bool> {
    if has_dirty_worktree(working_dir) {
        write_dirty_handoff(run, working_dir, reason)?;
        run.set_status(&RunStatus::Executing)?;
        Ok(false)
    } else {
        run.set_status(&RunStatus::Complete)?;
        Ok(true)
    }
}

fn write_dirty_handoff(run: &Run, working_dir: &Path, reason: &str) -> Result<()> {
    let status = git_status_porcelain(working_dir).unwrap_or_else(|| {
        "Unable to read git status; assume uncommitted changes remain.".to_string()
    });
    let mut file = fs::File::create(run.dir.join("handoff.md"))?;
    writeln!(file, "## Run {}", run.id)?;
    writeln!(
        file,
        "Brief: Resolve uncommitted completed work before landing."
    )?;
    writeln!(file, "Status: executing")?;
    writeln!(file)?;
    writeln!(file, "### Completed")?;
    writeln!(file, "- {reason}")?;
    writeln!(
        file,
        "- Reviews have already run for the dirty worktree state."
    )?;
    writeln!(file)?;
    writeln!(file, "### In progress")?;
    writeln!(
        file,
        "- Commit, revert, or intentionally ignore the remaining worktree changes."
    )?;
    writeln!(file)?;
    writeln!(file, "### Open questions")?;
    writeln!(file, "- None.")?;
    writeln!(file)?;
    writeln!(file, "### Next steps")?;
    writeln!(file, "- Run `git status --short` in the worktree.")?;
    writeln!(
        file,
        "- Make the worktree clean before writing `complete` again."
    )?;
    if !status.trim().is_empty() {
        writeln!(file)?;
        writeln!(file, "Current git status:")?;
        writeln!(file)?;
        writeln!(file, "```")?;
        write!(file, "{status}")?;
        if !status.ends_with('\n') {
            writeln!(file)?;
        }
        writeln!(file, "```")?;
    }
    Ok(())
}

/// ISO 8601 UTC timestamp for log entries.
fn now_iso() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// Check whether the worktree has committed or uncommitted changes.
///
/// Dirty worktrees must count as changed so completed author work cannot
/// bypass review just because it has not been committed yet.
fn has_changes(working_dir: &Path, run_dir: &Path) -> bool {
    let source_branch = match fs::read_to_string(run_dir.join("source-branch")) {
        Ok(b) => b.trim().to_string(),
        Err(_) => return true, // assume changes if we can't tell
    };
    let committed_diff = std::process::Command::new("git")
        .args(["diff", "--quiet", &format!("{source_branch}..HEAD")])
        .current_dir(working_dir)
        .status();

    match committed_diff {
        Ok(status) if !status.success() => return true,
        Ok(_) => {}
        Err(_) => return true, // assume changes if git fails
    }

    has_dirty_worktree(working_dir)
}

fn has_dirty_worktree(working_dir: &Path) -> bool {
    match git_status_porcelain(working_dir) {
        Some(status) => !status.is_empty(),
        None => true,
    }
}

fn git_status_porcelain(working_dir: &Path) -> Option<String> {
    let worktree_status = std::process::Command::new("git")
        .args([
            "status",
            "--porcelain",
            "--untracked-files=normal",
            "--",
            ".",
            ":(exclude).factory",
        ])
        .current_dir(working_dir)
        .output();

    match worktree_status {
        Ok(output) if output.status.success() => {
            Some(String::from_utf8_lossy(&output.stdout).to_string())
        }
        Ok(_) | Err(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::ffi::OsString;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::{Arc, Mutex, OnceLock};
    use tempfile::TempDir;

    /// Mock author that calls a handler to determine exit code and writes status.
    struct TestAgent<F>
    where
        F: Fn(&str, u32, &Path) -> i32 + Send + Sync,
    {
        handler: F,
        call_count: AtomicU32,
        transcript_paths: std::sync::Mutex<Vec<Option<PathBuf>>>,
    }

    impl<F> TestAgent<F>
    where
        F: Fn(&str, u32, &Path) -> i32 + Send + Sync,
    {
        fn new(handler: F) -> Self {
            Self {
                handler,
                call_count: AtomicU32::new(0),
                transcript_paths: std::sync::Mutex::new(Vec::new()),
            }
        }
    }

    impl<F> Coder for TestAgent<F>
    where
        F: Fn(&str, u32, &Path) -> i32 + Send + Sync,
    {
        fn run(
            &self,
            prompt: &str,
            _system_prompt: &str,
            _working_dir: &Path,
            _extra_args: &[String],
            transcript_file: Option<&Path>,
        ) -> Result<i32> {
            let n = self.call_count.fetch_add(1, Ordering::SeqCst) + 1;
            self.transcript_paths
                .lock()
                .unwrap()
                .push(transcript_file.map(|p| p.to_path_buf()));
            Ok((self.handler)(prompt, n, Path::new("")))
        }

        fn run_interactive(
            &self,
            _system_prompt: &str,
            _working_dir: &Path,
            _extra_args: &[String],
        ) -> Result<i32> {
            Ok(0)
        }
    }

    struct NoopHooks;
    impl SessionHooks for NoopHooks {
        fn sleep(&self, _duration: Duration) {}
    }

    static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    struct PathGuard {
        previous: Option<OsString>,
    }

    impl Drop for PathGuard {
        fn drop(&mut self) {
            unsafe {
                match self.previous.take() {
                    Some(path) => env::set_var("PATH", path),
                    None => env::remove_var("PATH"),
                }
            }
        }
    }

    fn prepend_path(path: &Path) -> PathGuard {
        let previous = env::var_os("PATH");
        let mut paths = vec![path.to_path_buf()];
        if let Some(existing) = previous.clone() {
            paths.extend(env::split_paths(&existing));
        }
        let joined = env::join_paths(paths).unwrap();
        unsafe {
            env::set_var("PATH", joined);
        }
        PathGuard { previous }
    }

    /// No-op review runner for tests that don't need real reviews.
    fn setup_test_run() -> (TempDir, Run) {
        let tmp = TempDir::new().unwrap();
        let run_id = "test-loop";
        let run_dir = tmp.path().join(format!("project/.factory/runs/{run_id}"));
        fs::create_dir_all(&run_dir).unwrap();
        fs::write(run_dir.join("brief.md"), "Test brief").unwrap();
        fs::write(run_dir.join("status"), "planned").unwrap();

        let run = Run {
            id: run_id.to_string(),
            dir: run_dir,
        };
        (tmp, run)
    }

    fn make_config(run: &Run) -> SessionConfig {
        SessionConfig {
            run: run.clone(),
            system_prompt: "test".to_string(),
            working_dir: PathBuf::from("/tmp"),
            extra_args: vec![],
            resolver: ContentResolver::new(None),
        }
    }

    fn make_project_config(run: &Run, project: &Path) -> SessionConfig {
        SessionConfig {
            run: run.clone(),
            system_prompt: "test".to_string(),
            working_dir: project.to_path_buf(),
            extra_args: vec![],
            resolver: ContentResolver::new(Some(project)),
        }
    }

    fn write_executable(path: &Path, content: &str) {
        fs::write(path, content).unwrap();
        let mut permissions = fs::metadata(path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions).unwrap();
    }

    fn setup_review_mode_run(verdict: &str) -> (TempDir, PathBuf, Run, PathBuf) {
        let tmp = TempDir::new().unwrap();
        let project = tmp.path().join("project");
        let run_id = "test-loop";
        let run_dir = project.join(format!(".factory/runs/{run_id}"));
        let prompts_dir = project.join(".factory/prompts");
        fs::create_dir_all(&run_dir).unwrap();
        fs::create_dir_all(&prompts_dir).unwrap();
        fs::write(run_dir.join("brief.md"), "Review the codebase.").unwrap();
        fs::write(run_dir.join("status"), "planned").unwrap();
        fs::write(run_dir.join("mode"), "review").unwrap();
        fs::write(run_dir.join("reviewers"), "tests").unwrap();
        fs::write(
            prompts_dir.join("review-tests.md"),
            "[system]\nTest reviewer.\n[changes]\nChanges scope marker.\n[full]\nFull scope marker.\n",
        )
        .unwrap();

        let bin_dir = tmp.path().join("bin");
        fs::create_dir_all(&bin_dir).unwrap();
        write_executable(
            &bin_dir.join("claude"),
            &format!(
                r#"#!/usr/bin/env bash
set -euo pipefail
RUN_DIR=".factory/runs/{run_id}"
mkdir -p "$RUN_DIR/reviews"
cat "$RUN_DIR/status" > "$RUN_DIR/review-status-seen"
printf '%s\n' "$*" > "$RUN_DIR/review-args"
case "$*" in
  *"Full scope marker"*) ;;
  *) exit 42 ;;
esac
printf 'Verdict: {verdict}\n\nReview findings.\n' > "$RUN_DIR/reviews/review-tests.md"
printf '{{"type":"result"}}\n'
"#
            ),
        );

        let run = Run {
            id: run_id.to_string(),
            dir: run_dir,
        };
        (tmp, project, run, bin_dir)
    }

    fn run_git(dir: &Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {:?} failed:\nstdout: {}\nstderr: {}",
            args,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn setup_change_detection_repo() -> (TempDir, PathBuf, PathBuf) {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        fs::create_dir_all(&repo).unwrap();
        run_git(&repo, &["init", "-b", "main"]);
        run_git(&repo, &["config", "commit.gpgsign", "false"]);
        run_git(&repo, &["config", "user.email", "test@test"]);
        run_git(&repo, &["config", "user.name", "test"]);

        fs::write(repo.join(".gitignore"), "ignored.txt\n").unwrap();
        fs::write(repo.join("tracked.txt"), "base\n").unwrap();
        run_git(&repo, &["add", "."]);
        run_git(&repo, &["commit", "-m", "init"]);

        let run_dir = repo.join(".factory/runs/test-run");
        fs::create_dir_all(&run_dir).unwrap();
        fs::write(run_dir.join("source-branch"), "main").unwrap();
        run_git(&repo, &["checkout", "-b", "test-run"]);

        (tmp, repo, run_dir)
    }

    #[test]
    fn has_changes_returns_false_for_clean_worktree() {
        let (_tmp, repo, run_dir) = setup_change_detection_repo();

        assert!(!has_changes(&repo, &run_dir));
    }

    #[test]
    fn has_changes_detects_committed_diff() {
        let (_tmp, repo, run_dir) = setup_change_detection_repo();
        fs::write(repo.join("committed.txt"), "change\n").unwrap();
        run_git(&repo, &["add", "."]);
        run_git(&repo, &["commit", "-m", "change"]);

        assert!(has_changes(&repo, &run_dir));
    }

    #[test]
    fn has_changes_detects_unstaged_tracked_change() {
        let (_tmp, repo, run_dir) = setup_change_detection_repo();
        fs::write(repo.join("tracked.txt"), "dirty\n").unwrap();

        assert!(has_changes(&repo, &run_dir));
    }

    #[test]
    fn has_changes_detects_staged_change() {
        let (_tmp, repo, run_dir) = setup_change_detection_repo();
        fs::write(repo.join("staged.txt"), "staged\n").unwrap();
        run_git(&repo, &["add", "staged.txt"]);

        assert!(has_changes(&repo, &run_dir));
    }

    #[test]
    fn has_changes_detects_untracked_file() {
        let (_tmp, repo, run_dir) = setup_change_detection_repo();
        fs::write(repo.join("untracked.txt"), "untracked\n").unwrap();

        assert!(has_changes(&repo, &run_dir));
    }

    #[test]
    fn has_changes_ignores_ignored_files() {
        let (_tmp, repo, run_dir) = setup_change_detection_repo();
        fs::write(repo.join("ignored.txt"), "ignored\n").unwrap();

        assert!(!has_changes(&repo, &run_dir));
    }

    #[test]
    fn has_changes_ignores_factory_run_state() {
        let (_tmp, repo, run_dir) = setup_change_detection_repo();
        fs::write(run_dir.join("report.md"), "run state\n").unwrap();

        assert!(!has_changes(&repo, &run_dir));
    }

    #[test]
    fn complete_or_continue_dirty_keeps_review_limit_dirty_run_executing() {
        let (_tmp, repo, run_dir) = setup_change_detection_repo();
        let run = Run {
            id: "test-run".to_string(),
            dir: run_dir,
        };
        fs::write(repo.join("tracked.txt"), "dirty\n").unwrap();

        let completed = complete_or_continue_dirty(
            &run,
            &repo,
            "Review limit reached, but the worktree still has uncommitted changes.",
        )
        .unwrap();

        assert!(!completed);
        assert_eq!(run.status().unwrap(), RunStatus::Executing);
        let handoff = fs::read_to_string(run.dir.join("handoff.md")).unwrap();
        assert!(handoff.contains("Review limit reached"));
        assert!(handoff.contains("tracked.txt"));
    }

    #[test]
    fn complete_or_continue_dirty_completes_review_limit_clean_run() {
        let (_tmp, repo, run_dir) = setup_change_detection_repo();
        let run = Run {
            id: "test-run".to_string(),
            dir: run_dir,
        };

        let completed = complete_or_continue_dirty(
            &run,
            &repo,
            "Review limit reached, but the worktree still has uncommitted changes.",
        )
        .unwrap();

        assert!(completed);
        assert_eq!(run.status().unwrap(), RunStatus::Complete);
        assert!(
            !run.dir.join("handoff.md").exists(),
            "clean review-limit completion should not write a dirty handoff"
        );
    }

    #[test]
    fn review_mode_runs_full_scope_review_without_author() {
        let _env_guard = ENV_LOCK.get_or_init(|| Mutex::new(())).lock().unwrap();
        let (_tmp, project, run, bin_dir) = setup_review_mode_run("pass");
        let _path_guard = prepend_path(&bin_dir);
        let author = TestAgent::new(move |_prompt: &str, _n: u32, _: &Path| {
            panic!("review-only runs must not launch the author")
        });

        let config = make_project_config(&run, &project);
        run_session_loop(&author, &config, &NoopHooks, CoderKind::Claude).unwrap();

        assert_eq!(author.call_count.load(Ordering::SeqCst), 0);
        assert_eq!(run.status().unwrap(), RunStatus::Complete);
        assert_eq!(
            fs::read_to_string(run.dir.join("review-status-seen")).unwrap(),
            "reviewing"
        );
        let review = fs::read_to_string(run.dir.join("reviews/review-tests.md")).unwrap();
        assert!(review.contains("Verdict: pass"));
        let args = fs::read_to_string(run.dir.join("review-args")).unwrap();
        assert!(args.contains("Full scope marker"));
        assert!(!args.contains("Changes scope marker"));
        assert!(run.dir.join("report.md").exists());
    }

    #[test]
    fn review_mode_completes_after_failing_findings() {
        let _env_guard = ENV_LOCK.get_or_init(|| Mutex::new(())).lock().unwrap();
        let (_tmp, project, run, bin_dir) = setup_review_mode_run("fail");
        let _path_guard = prepend_path(&bin_dir);
        let author = TestAgent::new(move |_prompt: &str, _n: u32, _: &Path| {
            panic!("review-only runs must not launch the author")
        });

        let config = make_project_config(&run, &project);
        run_session_loop(&author, &config, &NoopHooks, CoderKind::Claude).unwrap();

        assert_eq!(author.call_count.load(Ordering::SeqCst), 0);
        assert_eq!(run.status().unwrap(), RunStatus::Complete);
        let review = fs::read_to_string(run.dir.join("reviews/review-tests.md")).unwrap();
        assert!(review.contains("Verdict: fail"));
    }

    #[test]
    fn test_loop_review_limit_dirty_worktree_restarts_author() {
        let _env_guard = ENV_LOCK.get_or_init(|| Mutex::new(())).lock().unwrap();
        let (_tmp, repo, run_dir) = setup_change_detection_repo();
        fs::write(run_dir.join("brief.md"), "Test brief").unwrap();
        fs::write(run_dir.join("status"), "planned").unwrap();
        fs::write(run_dir.join("reviewers"), "tests").unwrap();
        let run = Run {
            id: "test-run".to_string(),
            dir: run_dir,
        };

        let bin_dir = repo.join("bin");
        fs::create_dir_all(&bin_dir).unwrap();
        let fake_claude = bin_dir.join("claude");
        write_executable(
            &fake_claude,
            "#!/usr/bin/env bash\nset -euo pipefail\nmkdir -p .factory/runs/test-run/reviews\nprintf 'Verdict: fail\\n\\nAlways failing.\\n' > .factory/runs/test-run/reviews/review-tests.md\nprintf '{\"type\":\"result\"}\\n'\n",
        );
        let _path_guard = prepend_path(&bin_dir);

        let run_dir = run.dir.clone();
        let repo_for_author = repo.clone();
        let saw_limit_prompt = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let saw_limit_prompt_for_author = saw_limit_prompt.clone();
        let author = TestAgent::new(move |prompt: &str, n: u32, _: &Path| {
            if prompt.contains("Review limit reached") {
                saw_limit_prompt_for_author.store(true, Ordering::SeqCst);
                assert_eq!(
                    fs::read_to_string(run_dir.join("status")).unwrap(),
                    "executing"
                );
                assert!(run_dir.join("handoff.md").exists());
                fs::write(run_dir.join("status"), "needs-user").unwrap();
                return 0;
            }

            if n == 1 {
                fs::write(repo_for_author.join("tracked.txt"), "dirty\n").unwrap();
            }
            fs::write(run_dir.join("status"), "complete").unwrap();
            0
        });

        let config = SessionConfig {
            run: run.clone(),
            system_prompt: "test".to_string(),
            working_dir: repo,
            extra_args: vec![],
            resolver: ContentResolver::new(None),
        };
        run_session_loop(&author, &config, &NoopHooks, CoderKind::Claude).unwrap();

        assert!(saw_limit_prompt.load(Ordering::SeqCst));
        assert_eq!(
            author.call_count.load(Ordering::SeqCst),
            MAX_REVIEW_ROUNDS + 2
        );
        let handoff = fs::read_to_string(run.dir.join("handoff.md")).unwrap();
        assert!(handoff.contains("Review limit reached"));
        assert!(handoff.contains("tracked.txt"));
    }

    #[test]
    fn test_loop_initial_prompt_uses_brief() {
        let (_tmp, run) = setup_test_run();
        let run_dir = run.dir.clone();
        let captured_prompts: Arc<std::sync::Mutex<Vec<String>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let prompts = captured_prompts.clone();

        let author = TestAgent::new(move |prompt: &str, _n: u32, _: &Path| {
            prompts.lock().unwrap().push(prompt.to_string());
            fs::write(run_dir.join("status"), "needs-user").unwrap();
            0
        });

        let config = make_config(&run);
        run_session_loop(&author, &config, &NoopHooks, CoderKind::Claude).unwrap();

        let prompts = captured_prompts.lock().unwrap();
        assert!(prompts[0].contains("brief"));
        assert!(prompts[0].contains(&run.id));
    }

    #[test]
    fn test_loop_initial_prompt_uses_handoff() {
        let (_tmp, run) = setup_test_run();
        fs::write(run.dir.join("handoff.md"), "Previous work handoff").unwrap();
        let run_dir = run.dir.clone();
        let captured_prompts: Arc<std::sync::Mutex<Vec<String>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let prompts = captured_prompts.clone();

        let author = TestAgent::new(move |prompt: &str, _n: u32, _: &Path| {
            prompts.lock().unwrap().push(prompt.to_string());
            fs::write(run_dir.join("status"), "needs-user").unwrap();
            0
        });

        let config = make_config(&run);
        run_session_loop(&author, &config, &NoopHooks, CoderKind::Claude).unwrap();

        let prompts = captured_prompts.lock().unwrap();
        assert!(prompts[0].contains("handoff"));
    }

    #[test]
    fn test_loop_stops_on_needs_user() {
        let (_tmp, run) = setup_test_run();
        let run_dir = run.dir.clone();

        let author = TestAgent::new(move |_prompt: &str, _n: u32, _: &Path| {
            fs::write(run_dir.join("status"), "needs-user").unwrap();
            0
        });

        let config = make_config(&run);
        run_session_loop(&author, &config, &NoopHooks, CoderKind::Claude).unwrap();

        assert_eq!(author.call_count.load(Ordering::SeqCst), 1);
        assert_eq!(run.status().unwrap(), RunStatus::NeedsUser);
    }

    #[test]
    fn test_loop_stops_on_failed() {
        let (_tmp, run) = setup_test_run();
        let run_dir = run.dir.clone();

        let author = TestAgent::new(move |_prompt: &str, _n: u32, _: &Path| {
            fs::write(run_dir.join("status"), "failed").unwrap();
            0
        });

        let config = make_config(&run);
        run_session_loop(&author, &config, &NoopHooks, CoderKind::Claude).unwrap();

        assert_eq!(author.call_count.load(Ordering::SeqCst), 1);
        assert_eq!(run.status().unwrap(), RunStatus::Failed);
    }

    #[test]
    fn test_loop_restarts_on_executing() {
        let (_tmp, run) = setup_test_run();
        let run_dir = run.dir.clone();

        let author = TestAgent::new(move |_prompt: &str, n: u32, _: &Path| {
            if n < 3 {
                fs::write(run_dir.join("status"), "executing").unwrap();
            } else {
                fs::write(run_dir.join("status"), "needs-user").unwrap();
            }
            0
        });

        let config = make_config(&run);
        run_session_loop(&author, &config, &NoopHooks, CoderKind::Claude).unwrap();

        assert_eq!(author.call_count.load(Ordering::SeqCst), 3);
        assert_eq!(run.status().unwrap(), RunStatus::NeedsUser);
    }

    #[test]
    fn test_loop_restarts_on_rate_limited() {
        let (_tmp, run) = setup_test_run();
        let run_dir = run.dir.clone();

        let author = TestAgent::new(move |_prompt: &str, n: u32, _: &Path| {
            if n == 1 {
                fs::write(run_dir.join("status"), "rate-limited").unwrap();
            } else {
                fs::write(run_dir.join("status"), "needs-user").unwrap();
            }
            0
        });

        let config = make_config(&run);
        run_session_loop(&author, &config, &NoopHooks, CoderKind::Claude).unwrap();

        assert_eq!(author.call_count.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn test_loop_consecutive_failures_set_failed() {
        let (_tmp, run) = setup_test_run();
        // Set initial status to executing so the loop continues
        fs::write(run.dir.join("status"), "executing").unwrap();

        let author = TestAgent::new(move |_prompt: &str, _n: u32, _: &Path| {
            1 // non-zero exit
        });

        let config = make_config(&run);
        run_session_loop(&author, &config, &NoopHooks, CoderKind::Claude).unwrap();

        assert_eq!(author.call_count.load(Ordering::SeqCst), 3);
        assert_eq!(run.status().unwrap(), RunStatus::Failed);
    }

    #[test]
    fn test_loop_success_resets_failure_counter() {
        let (_tmp, run) = setup_test_run();
        fs::write(run.dir.join("status"), "executing").unwrap();
        let run_dir = run.dir.clone();

        let author = TestAgent::new(move |_prompt: &str, n: u32, _: &Path| {
            match n {
                1 | 2 => 1, // Two failures
                3 => 0,     // Success — resets counter
                4 | 5 => 1, // Two more failures (not three)
                _ => {
                    fs::write(run_dir.join("status"), "needs-user").unwrap();
                    0
                }
            }
        });

        let config = make_config(&run);
        run_session_loop(&author, &config, &NoopHooks, CoderKind::Claude).unwrap();

        // Should have run more than 5 sessions (counter was reset)
        assert!(author.call_count.load(Ordering::SeqCst) > 5);
    }

    #[test]
    fn test_loop_max_sessions_sets_failed() {
        let (_tmp, run) = setup_test_run();
        let run_dir = run.dir.clone();

        let author = TestAgent::new(move |_prompt: &str, _n: u32, _: &Path| {
            fs::write(run_dir.join("status"), "executing").unwrap();
            0
        });

        let config = make_config(&run);
        run_session_loop(&author, &config, &NoopHooks, CoderKind::Claude).unwrap();

        assert_eq!(author.call_count.load(Ordering::SeqCst), MAX_SESSIONS);
        assert_eq!(run.status().unwrap(), RunStatus::Failed);
    }

    #[test]
    fn test_loop_writes_sessions_log() {
        let (_tmp, run) = setup_test_run();
        let run_dir = run.dir.clone();

        let author = TestAgent::new(move |_prompt: &str, n: u32, _: &Path| {
            if n < 3 {
                fs::write(run_dir.join("status"), "executing").unwrap();
            } else {
                fs::write(run_dir.join("status"), "needs-user").unwrap();
            }
            0
        });

        let config = make_config(&run);
        run_session_loop(&author, &config, &NoopHooks, CoderKind::Claude).unwrap();

        let log = fs::read_to_string(run.dir.join("sessions.log")).unwrap();
        let lines: Vec<&str> = log.lines().collect();
        assert_eq!(lines.len(), 3);
        assert!(lines[0].contains("session=1 exit=0 duration="));
        assert!(lines[0].contains("status=executing"));
        assert!(lines[2].contains("session=3 exit=0 duration="));
        assert!(lines[2].contains("status=needs-user"));
    }

    #[test]
    fn test_loop_creates_session_transcript_dir() {
        let (_tmp, run) = setup_test_run();
        let run_dir = run.dir.clone();
        let expected_path = run.dir.join("sessions/session-1/transcript.jsonl");

        let author = TestAgent::new(move |_prompt: &str, _n: u32, _: &Path| {
            fs::write(run_dir.join("status"), "needs-user").unwrap();
            0
        });

        let config = make_config(&run);
        run_session_loop(&author, &config, &NoopHooks, CoderKind::Claude).unwrap();

        // Session directory should exist
        assert!(run.dir.join("sessions/session-1").is_dir());
        // Transcript path should be passed to the author
        let paths = author.transcript_paths.lock().unwrap();
        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0].as_deref(), Some(expected_path.as_path()));
    }

    #[test]
    fn test_loop_writes_nonzero_exit_to_sessions_log() {
        let (_tmp, run) = setup_test_run();
        fs::write(run.dir.join("status"), "executing").unwrap();

        let author = TestAgent::new(move |_prompt: &str, _n: u32, _: &Path| {
            1 // non-zero exit
        });

        let config = make_config(&run);
        run_session_loop(&author, &config, &NoopHooks, CoderKind::Claude).unwrap();

        let log = fs::read_to_string(run.dir.join("sessions.log")).unwrap();
        let lines: Vec<&str> = log.lines().collect();
        assert_eq!(lines.len(), 3);
        assert!(lines[0].contains("session=1 exit=1 duration="));
    }

    /// Hooks that record pre_session calls for testing.
    struct RecordingHooks {
        call_count: AtomicU32,
    }

    impl RecordingHooks {
        fn new() -> Self {
            Self {
                call_count: AtomicU32::new(0),
            }
        }

        fn calls(&self) -> u32 {
            self.call_count.load(Ordering::SeqCst)
        }
    }

    impl SessionHooks for RecordingHooks {
        fn pre_session(&self) -> Result<()> {
            self.call_count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        fn sleep(&self, _duration: Duration) {}
    }

    #[test]
    fn test_loop_calls_pre_session_before_each_session() {
        let (_tmp, run) = setup_test_run();
        let run_dir = run.dir.clone();

        let author = TestAgent::new(move |_prompt: &str, n: u32, _: &Path| {
            if n < 3 {
                fs::write(run_dir.join("status"), "executing").unwrap();
            } else {
                fs::write(run_dir.join("status"), "needs-user").unwrap();
            }
            0
        });

        let hooks = RecordingHooks::new();
        let config = make_config(&run);
        run_session_loop(&author, &config, &hooks, CoderKind::Claude).unwrap();

        assert_eq!(author.call_count.load(Ordering::SeqCst), 3);
        assert_eq!(hooks.calls(), 3);
    }

    /// Hooks that fail on pre_session to test error propagation.
    struct FailingHooks;

    impl SessionHooks for FailingHooks {
        fn pre_session(&self) -> Result<()> {
            anyhow::bail!("credential refresh failed")
        }
        fn sleep(&self, _duration: Duration) {}
    }

    #[test]
    fn test_loop_stops_when_pre_session_returns_error() {
        let (_tmp, run) = setup_test_run();

        let author = TestAgent::new(move |_prompt: &str, _n: u32, _: &Path| 0);

        let config = make_config(&run);
        let result = run_session_loop(&author, &config, &FailingHooks, CoderKind::Claude);

        assert!(result.is_err());
        assert_eq!(author.call_count.load(Ordering::SeqCst), 0);
    }
}
