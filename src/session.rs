use anyhow::Result;
use std::fs;
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use crate::coder::Coder;
use crate::content::ContentResolver;
use crate::report;
use crate::review;
use crate::run::{Run, RunStatus};

const MAX_SESSIONS: u32 = 50;
const MAX_CONSECUTIVE_FAILURES: u32 = 3;

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
    agent: &dyn Coder,
    config: &SessionConfig,
    hooks: &dyn SessionHooks,
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

    // For review runs, start by running reviewers
    let mut review_round: u32 = 0;
    let initial_prompt = if run_mode == "review" {
        eprintln!("  Mode: review (reviewers run first)");
        let review_scope = "full-codebase";
        review_round += 1;
        if !review::run_reviews(
            run_dir,
            &run.id,
            &reviewer_filter,
            review_scope,
            &config.resolver,
            review_round,
        )? {
            format!(
                "This is a review run. Reviewers have produced findings. Read the review artifacts at .factory/runs/{}/reviews/ and address the findings. When done, write status 'complete'.",
                run.id
            )
        } else {
            eprintln!("\n  All reviewers passed — nothing to fix.");
            run.set_status(&RunStatus::Complete)?;
            report::generate_report(run_dir, &run.id, 0)?;
            eprintln!("\n  Run {} completed.", run.id);
            return Ok(());
        }
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

    let mut session_count: u32 = 0;
    let mut consecutive_failures: u32 = 0;
    let mut prompt = initial_prompt;

    loop {
        session_count += 1;
        eprintln!(
            "\n  === Session {} (run: {}) ===\n",
            session_count, run.id
        );

        if session_count > MAX_SESSIONS {
            eprintln!(
                "  Max sessions ({}) reached — stopping.",
                MAX_SESSIONS
            );
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

        // Launch agent
        let exit_code = agent.run(
            &prompt,
            &config.system_prompt,
            &config.working_dir,
            &config.extra_args,
            Some(&transcript_file),
        )?;

        let session_elapsed = session_start.elapsed().as_secs();
        eprintln!("\n  Agent exited (code: {exit_code}, {session_elapsed}s)");

        // Write session metadata to sessions.log
        {
            let status_after = run.status().map(|s| s.to_string()).unwrap_or_else(|_| "unknown".into());
            let mut log_file = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(run_dir.join("sessions.log"))?;
            use std::io::Write;
            writeln!(
                log_file,
                "session={session_count} exit={exit_code} duration={session_elapsed}s status={status_after}"
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
                let review_scope = if run_mode == "review" {
                    "full-codebase"
                } else {
                    "run-scoped"
                };
                review_round += 1;
                if review::run_reviews(
                    run_dir,
                    &run.id,
                    &reviewer_filter,
                    review_scope,
                    &config.resolver,
                    review_round,
                )? {
                    report::generate_report(run_dir, &run.id, session_count)?;
                    eprintln!("\n  Run {} completed.", run.id);
                    break;
                } else {
                    eprintln!(
                        "  Review returned findings — restarting author..."
                    );
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
                eprintln!(
                    "  Unexpected status \"{}\" — stopping.",
                    status.as_str()
                );
                break;
            }
        }
    }

    eprintln!("\n  Total sessions: {session_count}");
    Ok(())
}


#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;
    use tempfile::TempDir;

    /// Mock agent that calls a handler to determine exit code and writes status.
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

    #[test]
    fn test_loop_initial_prompt_uses_brief() {
        let (_tmp, run) = setup_test_run();
        let run_dir = run.dir.clone();
        let captured_prompts: Arc<std::sync::Mutex<Vec<String>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let prompts = captured_prompts.clone();

        let agent = TestAgent::new(move |prompt: &str, _n: u32, _: &Path| {
                prompts.lock().unwrap().push(prompt.to_string());
                fs::write(run_dir.join("status"), "needs-user").unwrap();
                0
            }
        );

        let config = make_config(&run);
        run_session_loop(&agent, &config, &NoopHooks).unwrap();

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

        let agent = TestAgent::new(move |prompt: &str, _n: u32, _: &Path| {
                prompts.lock().unwrap().push(prompt.to_string());
                fs::write(run_dir.join("status"), "needs-user").unwrap();
                0
            }
        );

        let config = make_config(&run);
        run_session_loop(&agent, &config, &NoopHooks).unwrap();

        let prompts = captured_prompts.lock().unwrap();
        assert!(prompts[0].contains("handoff"));
    }

    #[test]
    fn test_loop_stops_on_needs_user() {
        let (_tmp, run) = setup_test_run();
        let run_dir = run.dir.clone();

        let agent = TestAgent::new(move |_prompt: &str, _n: u32, _: &Path| {
                fs::write(run_dir.join("status"), "needs-user").unwrap();
                0
            }
        );

        let config = make_config(&run);
        run_session_loop(&agent, &config, &NoopHooks).unwrap();

        assert_eq!(agent.call_count.load(Ordering::SeqCst), 1);
        assert_eq!(run.status().unwrap(), RunStatus::NeedsUser);
    }

    #[test]
    fn test_loop_stops_on_failed() {
        let (_tmp, run) = setup_test_run();
        let run_dir = run.dir.clone();

        let agent = TestAgent::new(move |_prompt: &str, _n: u32, _: &Path| {
                fs::write(run_dir.join("status"), "failed").unwrap();
                0
            }
        );

        let config = make_config(&run);
        run_session_loop(&agent, &config, &NoopHooks).unwrap();

        assert_eq!(agent.call_count.load(Ordering::SeqCst), 1);
        assert_eq!(run.status().unwrap(), RunStatus::Failed);
    }

    #[test]
    fn test_loop_restarts_on_executing() {
        let (_tmp, run) = setup_test_run();
        let run_dir = run.dir.clone();

        let agent = TestAgent::new(move |_prompt: &str, n: u32, _: &Path| {
                if n < 3 {
                    fs::write(run_dir.join("status"), "executing").unwrap();
                } else {
                    fs::write(run_dir.join("status"), "needs-user").unwrap();
                }
                0
            }
        );

        let config = make_config(&run);
        run_session_loop(&agent, &config, &NoopHooks).unwrap();

        assert_eq!(agent.call_count.load(Ordering::SeqCst), 3);
        assert_eq!(run.status().unwrap(), RunStatus::NeedsUser);
    }

    #[test]
    fn test_loop_restarts_on_rate_limited() {
        let (_tmp, run) = setup_test_run();
        let run_dir = run.dir.clone();

        let agent = TestAgent::new(move |_prompt: &str, n: u32, _: &Path| {
                if n == 1 {
                    fs::write(run_dir.join("status"), "rate-limited").unwrap();
                } else {
                    fs::write(run_dir.join("status"), "needs-user").unwrap();
                }
                0
            }
        );

        let config = make_config(&run);
        run_session_loop(&agent, &config, &NoopHooks).unwrap();

        assert_eq!(agent.call_count.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn test_loop_consecutive_failures_set_failed() {
        let (_tmp, run) = setup_test_run();
        // Set initial status to executing so the loop continues
        fs::write(run.dir.join("status"), "executing").unwrap();

        let agent = TestAgent::new(move |_prompt: &str, _n: u32, _: &Path| {
                1 // non-zero exit
            }
        );

        let config = make_config(&run);
        run_session_loop(&agent, &config, &NoopHooks).unwrap();

        assert_eq!(agent.call_count.load(Ordering::SeqCst), 3);
        assert_eq!(run.status().unwrap(), RunStatus::Failed);
    }

    #[test]
    fn test_loop_success_resets_failure_counter() {
        let (_tmp, run) = setup_test_run();
        fs::write(run.dir.join("status"), "executing").unwrap();
        let run_dir = run.dir.clone();

        let agent = TestAgent::new(move |_prompt: &str, n: u32, _: &Path| {
                match n {
                    1 | 2 => 1,    // Two failures
                    3 => 0,         // Success — resets counter
                    4 | 5 => 1,    // Two more failures (not three)
                    _ => {
                        fs::write(run_dir.join("status"), "needs-user").unwrap();
                        0
                    }
                }
            }
        );

        let config = make_config(&run);
        run_session_loop(&agent, &config, &NoopHooks).unwrap();

        // Should have run more than 5 sessions (counter was reset)
        assert!(agent.call_count.load(Ordering::SeqCst) > 5);
    }

    #[test]
    fn test_loop_max_sessions_sets_failed() {
        let (_tmp, run) = setup_test_run();
        let run_dir = run.dir.clone();

        let agent = TestAgent::new(move |_prompt: &str, _n: u32, _: &Path| {
                fs::write(run_dir.join("status"), "executing").unwrap();
                0
            }
        );

        let config = make_config(&run);
        run_session_loop(&agent, &config, &NoopHooks).unwrap();

        assert_eq!(agent.call_count.load(Ordering::SeqCst), MAX_SESSIONS);
        assert_eq!(run.status().unwrap(), RunStatus::Failed);
    }

    #[test]
    fn test_loop_writes_sessions_log() {
        let (_tmp, run) = setup_test_run();
        let run_dir = run.dir.clone();

        let agent = TestAgent::new(move |_prompt: &str, n: u32, _: &Path| {
                if n < 3 {
                    fs::write(run_dir.join("status"), "executing").unwrap();
                } else {
                    fs::write(run_dir.join("status"), "needs-user").unwrap();
                }
                0
            }
        );

        let config = make_config(&run);
        run_session_loop(&agent, &config, &NoopHooks).unwrap();

        let log = fs::read_to_string(run.dir.join("sessions.log")).unwrap();
        let lines: Vec<&str> = log.lines().collect();
        assert_eq!(lines.len(), 3);
        assert!(lines[0].starts_with("session=1 exit=0 duration="));
        assert!(lines[0].contains("status=executing"));
        assert!(lines[2].starts_with("session=3 exit=0 duration="));
        assert!(lines[2].contains("status=needs-user"));
    }

    #[test]
    fn test_loop_creates_session_transcript_dir() {
        let (_tmp, run) = setup_test_run();
        let run_dir = run.dir.clone();
        let expected_path = run.dir.join("sessions/session-1/transcript.jsonl");

        let agent = TestAgent::new(move |_prompt: &str, _n: u32, _: &Path| {
                fs::write(run_dir.join("status"), "needs-user").unwrap();
                0
            }
        );

        let config = make_config(&run);
        run_session_loop(&agent, &config, &NoopHooks).unwrap();

        // Session directory should exist
        assert!(run.dir.join("sessions/session-1").is_dir());
        // Transcript path should be passed to the agent
        let paths = agent.transcript_paths.lock().unwrap();
        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0].as_deref(), Some(expected_path.as_path()));
    }

    #[test]
    fn test_loop_writes_nonzero_exit_to_sessions_log() {
        let (_tmp, run) = setup_test_run();
        fs::write(run.dir.join("status"), "executing").unwrap();

        let agent = TestAgent::new(move |_prompt: &str, _n: u32, _: &Path| {
                1 // non-zero exit
            }
        );

        let config = make_config(&run);
        run_session_loop(&agent, &config, &NoopHooks).unwrap();

        let log = fs::read_to_string(run.dir.join("sessions.log")).unwrap();
        let lines: Vec<&str> = log.lines().collect();
        assert_eq!(lines.len(), 3);
        assert!(lines[0].starts_with("session=1 exit=1 duration="));
    }

    /// Hooks that record pre_session calls for testing.
    struct RecordingHooks {
        call_count: AtomicU32,
    }

    impl RecordingHooks {
        fn new() -> Self {
            Self { call_count: AtomicU32::new(0) }
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

        let agent = TestAgent::new(move |_prompt: &str, n: u32, _: &Path| {
            if n < 3 {
                fs::write(run_dir.join("status"), "executing").unwrap();
            } else {
                fs::write(run_dir.join("status"), "needs-user").unwrap();
            }
            0
        });

        let hooks = RecordingHooks::new();
        let config = make_config(&run);
        run_session_loop(&agent, &config, &hooks).unwrap();

        assert_eq!(agent.call_count.load(Ordering::SeqCst), 3);
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

        let agent = TestAgent::new(move |_prompt: &str, _n: u32, _: &Path| {
            0
        });

        let config = make_config(&run);
        let result = run_session_loop(&agent, &config, &FailingHooks);

        assert!(result.is_err());
        assert_eq!(agent.call_count.load(Ordering::SeqCst), 0);
    }
}
