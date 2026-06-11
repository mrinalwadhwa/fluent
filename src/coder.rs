use anyhow::{Result, bail};
use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const DEFAULT_CLAUDE_MODEL: &str = "claude-opus-4-6";

fn claude_model() -> String {
    std::env::var("FACTORY_CLAUDE_MODEL")
        .or_else(|_| std::env::var("FACTORY_MODEL"))
        .unwrap_or_else(|_| DEFAULT_CLAUDE_MODEL.to_string())
}

fn codex_model() -> Option<String> {
    std::env::var("FACTORY_CODEX_MODEL").ok()
}

fn codex_ca_bundle() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("FACTORY_CODEX_CA_BUNDLE") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Some(path);
        }
    }

    [
        "/opt/homebrew/etc/ca-certificates/cert.pem",
        "/opt/homebrew/etc/openssl@3/cert.pem",
        "/usr/local/etc/ca-certificates/cert.pem",
        "/usr/local/etc/openssl@3/cert.pem",
        "/etc/ssl/cert.pem",
    ]
    .iter()
    .map(PathBuf::from)
    .find(|path| path.is_file())
}

/// Which coding agent the factory should launch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoderKind {
    Claude,
    Codex,
}

/// Sandbox mode requested for the coder launch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoderSandbox {
    None,
    SeatbeltProfile(String),
    SeatbeltRoots { writable_roots: Vec<PathBuf> },
}

impl CoderKind {
    pub fn resolve(value: Option<&str>) -> Result<Self> {
        let value = value
            .map(str::to_string)
            .or_else(|| std::env::var("FACTORY_CODER").ok())
            .unwrap_or_else(|| "claude".to_string());

        match value.trim().to_lowercase().as_str() {
            "claude" | "claude-code" => Ok(Self::Claude),
            "codex" => Ok(Self::Codex),
            other => bail!("Unknown coder '{other}'. Available: claude, codex."),
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }

    pub fn boxed(&self, sandbox: CoderSandbox) -> Box<dyn Coder> {
        match self {
            Self::Claude => match sandbox {
                CoderSandbox::SeatbeltProfile(profile) => Box::new(SandboxedClaudeCode {
                    sandbox_profile: Some(profile),
                }),
                _ => Box::new(BareClaudeCode),
            },
            Self::Codex => Box::new(CodexCode {
                sandbox_profile: match &sandbox {
                    CoderSandbox::SeatbeltProfile(profile) => Some(profile.clone()),
                    _ => None,
                },
            }),
        }
    }
}

/// Trait abstracting the coding agent.
pub trait Coder: Send + Sync {
    /// Launch the coder with a prompt, system prompt, and working directory.
    /// When `transcript_file` is provided, add `--verbose --output-format
    /// stream-json` and pipe stdout to the file (like `tee`).
    /// Returns the exit code.
    fn run(
        &self,
        prompt: &str,
        system_prompt: &str,
        working_dir: &Path,
        extra_args: &[String],
        extra_env: &[(String, String)],
        transcript_file: Option<&Path>,
    ) -> Result<i32>;

    /// Launch an interactive session (no -p flag).
    fn run_interactive(
        &self,
        system_prompt: &str,
        working_dir: &Path,
        extra_args: &[String],
        extra_env: &[(String, String)],
    ) -> Result<i32>;
}

/// Claude Code invoked via sandbox-exec.
pub struct SandboxedClaudeCode {
    pub sandbox_profile: Option<String>,
}

impl Coder for SandboxedClaudeCode {
    fn run(
        &self,
        prompt: &str,
        system_prompt: &str,
        working_dir: &Path,
        extra_args: &[String],
        extra_env: &[(String, String)],
        transcript_file: Option<&Path>,
    ) -> Result<i32> {
        let want_transcript = transcript_file.is_some();
        run_with_transcript_retrying(
            || {
                let mut cmd = self.build_command(working_dir);
                for (key, value) in extra_env {
                    cmd.env(key, value);
                }
                if want_transcript {
                    cmd.args(["--verbose", "--output-format", "stream-json"]);
                }
                cmd.args(["--append-system-prompt", system_prompt]);
                cmd.args(["-p", prompt]);
                cmd.args(extra_args);
                cmd
            },
            transcript_file,
        )
    }

    fn run_interactive(
        &self,
        system_prompt: &str,
        working_dir: &Path,
        extra_args: &[String],
        extra_env: &[(String, String)],
    ) -> Result<i32> {
        let mut cmd = self.build_command(working_dir);
        for (key, value) in extra_env {
            cmd.env(key, value);
        }
        cmd.args(["--append-system-prompt", system_prompt]);
        cmd.args(extra_args);

        let status = cmd.status()?;
        Ok(status.code().unwrap_or(1))
    }
}

impl SandboxedClaudeCode {
    fn build_command(&self, working_dir: &Path) -> Command {
        if let Some(ref profile) = self.sandbox_profile {
            let mut cmd = Command::new("sandbox-exec");
            cmd.args(["-f", profile]);
            cmd.arg("claude");
            cmd.arg("--dangerously-skip-permissions");
            cmd.args(["--model", &claude_model()]);
            cmd.current_dir(working_dir);
            cmd
        } else {
            let mut cmd = Command::new("claude");
            cmd.current_dir(working_dir);
            cmd
        }
    }
}

/// Bare Claude Code (no sandbox, for Fargate/Linux/--no-sandbox).
pub struct BareClaudeCode;

impl Coder for BareClaudeCode {
    fn run(
        &self,
        prompt: &str,
        system_prompt: &str,
        working_dir: &Path,
        extra_args: &[String],
        extra_env: &[(String, String)],
        transcript_file: Option<&Path>,
    ) -> Result<i32> {
        let want_transcript = transcript_file.is_some();
        run_with_transcript_retrying(
            || {
                let mut cmd = Command::new("claude");
                cmd.current_dir(working_dir);
                for (key, value) in extra_env {
                    cmd.env(key, value);
                }
                cmd.args(["--dangerously-skip-permissions"]);
                cmd.args(["--model", &claude_model()]);
                if want_transcript {
                    cmd.args(["--verbose", "--output-format", "stream-json"]);
                }
                cmd.args(["--append-system-prompt", system_prompt]);
                cmd.args(["-p", prompt]);
                cmd.args(extra_args);
                cmd
            },
            transcript_file,
        )
    }

    fn run_interactive(
        &self,
        system_prompt: &str,
        working_dir: &Path,
        extra_args: &[String],
        extra_env: &[(String, String)],
    ) -> Result<i32> {
        let mut cmd = Command::new("claude");
        cmd.current_dir(working_dir);
        for (key, value) in extra_env {
            cmd.env(key, value);
        }
        cmd.args(["--dangerously-skip-permissions"]);
        cmd.args(["--append-system-prompt", system_prompt]);
        cmd.args(extra_args);

        let status = cmd.status()?;
        Ok(status.code().unwrap_or(1))
    }
}

/// OpenAI Codex CLI.
pub struct CodexCode {
    pub sandbox_profile: Option<String>,
}

impl Coder for CodexCode {
    fn run(
        &self,
        prompt: &str,
        system_prompt: &str,
        working_dir: &Path,
        extra_args: &[String],
        extra_env: &[(String, String)],
        transcript_file: Option<&Path>,
    ) -> Result<i32> {
        let want_transcript = transcript_file.is_some();
        let combined_prompt = format!("{system_prompt}\n\n---\n\n{prompt}");
        run_with_transcript_retrying(
            || {
                let mut cmd = self.build_command(working_dir, true);
                for (key, value) in extra_env {
                    cmd.env(key, value);
                }
                if want_transcript {
                    cmd.arg("--json");
                }
                cmd.arg(&combined_prompt);
                cmd.args(extra_args);
                cmd
            },
            transcript_file,
        )
    }

    fn run_interactive(
        &self,
        system_prompt: &str,
        working_dir: &Path,
        extra_args: &[String],
        extra_env: &[(String, String)],
    ) -> Result<i32> {
        let mut cmd = self.build_command(working_dir, false);
        for (key, value) in extra_env {
            cmd.env(key, value);
        }
        cmd.arg(system_prompt);
        cmd.args(extra_args);

        let status = cmd.status()?;
        Ok(status.code().unwrap_or(1))
    }
}

impl CodexCode {
    fn build_command(&self, working_dir: &Path, exec_mode: bool) -> Command {
        let mut cmd = if let Some(profile) = &self.sandbox_profile {
            let mut cmd = Command::new("sandbox-exec");
            cmd.args(["-f", profile]);
            cmd.arg("codex");
            if let Some(ca_bundle) = codex_ca_bundle() {
                cmd.env("SSL_CERT_FILE", ca_bundle);
            }
            cmd
        } else {
            Command::new("codex")
        };

        // --ask-for-approval is a top-level option, not an exec subcommand
        // option, so it must appear before the `exec` subcommand.
        if self.sandbox_profile.is_some() && exec_mode {
            cmd.args(["--ask-for-approval", "never"]);
        }
        if exec_mode {
            cmd.arg("exec");
        }
        cmd.args(["--cd", &working_dir.to_string_lossy()]);
        cmd.args(["--dangerously-bypass-approvals-and-sandbox"]);
        if let Some(model) = codex_model() {
            cmd.args(["--model", &model]);
        }
        cmd.current_dir(working_dir);
        cmd
    }
}

/// Run a command, optionally piping stdout to a transcript file (like `tee`).
/// When `transcript_file` is `None`, stdout inherits from the parent process.
fn run_with_transcript(mut cmd: Command, transcript_file: Option<&Path>) -> Result<i32> {
    match transcript_file {
        Some(path) => {
            cmd.stdout(Stdio::piped());
            let mut child = cmd.spawn()?;
            let stdout = child.stdout.take().expect("stdout was piped");
            let mut file = File::create(path)?;
            let reader = BufReader::new(stdout);
            for line in reader.lines() {
                let line = line?;
                writeln!(file, "{}", line)?;
                eprintln!("{}", line);
            }
            let status = child.wait()?;
            Ok(status.code().unwrap_or(1))
        }
        None => {
            let status = cmd.status()?;
            Ok(status.code().unwrap_or(1))
        }
    }
}

const DEFAULT_RATE_LIMIT_RETRY_AFTER_SECS: u64 = 1800;
const RATE_LIMIT_MAX_RETRIES: u32 = 2;

fn rate_limit_retry_after() -> std::time::Duration {
    let secs = std::env::var("FACTORY_RATE_LIMIT_RETRY_AFTER_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(DEFAULT_RATE_LIMIT_RETRY_AFTER_SECS);
    std::time::Duration::from_secs(secs)
}

/// Scan a transcript for a Claude session-limit marker. Returns true if the
/// session was rate-limited (i.e. the non-zero exit is a transient capacity
/// failure, not a real Task failure).
pub fn transcript_indicates_rate_limit(transcript_path: &Path) -> bool {
    let Ok(file) = File::open(transcript_path) else {
        return false;
    };
    let reader = BufReader::new(file);
    for line in reader.lines().map_while(Result::ok) {
        let l = line.to_lowercase();
        if l.contains("session limit") || l.contains("rate limit") || l.contains("rate-limit") {
            return true;
        }
    }
    false
}

/// Run a Coder command with rate-limit-aware retry. After a non-zero exit
/// whose transcript contains a session-limit marker, sleep for the
/// configured retry-after window and try again (up to a small bounded
/// number of attempts). If no transcript is available the function behaves
/// the same as `run_with_transcript`.
fn run_with_transcript_retrying<F>(build_cmd: F, transcript_file: Option<&Path>) -> Result<i32>
where
    F: Fn() -> Command,
{
    let mut attempt: u32 = 0;
    loop {
        let exit = run_with_transcript(build_cmd(), transcript_file)?;
        if exit == 0 {
            return Ok(exit);
        }
        let Some(path) = transcript_file else {
            return Ok(exit);
        };
        if !transcript_indicates_rate_limit(path) {
            return Ok(exit);
        }
        if attempt >= RATE_LIMIT_MAX_RETRIES {
            eprintln!(
                "  Rate-limit detected on attempt {}; retry budget exhausted, propagating exit code {exit}.",
                attempt + 1
            );
            return Ok(exit);
        }
        let wait = rate_limit_retry_after();
        eprintln!(
            "  Rate-limit detected on attempt {}; sleeping {}s before retry.",
            attempt + 1,
            wait.as_secs()
        );
        std::thread::sleep(wait);
        attempt += 1;
    }
}

/// Mock coder for testing. Calls a closure to determine behavior.
#[cfg(test)]
pub struct MockCoder<F>
where
    F: Fn(&str, u32) -> (i32, Option<String>),
{
    pub handler: F,
    pub call_count: std::sync::atomic::AtomicU32,
}

#[cfg(test)]
impl<F> Coder for MockCoder<F>
where
    F: Fn(&str, u32) -> (i32, Option<String>) + Send + Sync,
{
    fn run(
        &self,
        prompt: &str,
        _system_prompt: &str,
        _working_dir: &Path,
        _extra_args: &[String],
        _extra_env: &[(String, String)],
        _transcript_file: Option<&Path>,
    ) -> Result<i32> {
        let n = self
            .call_count
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            + 1;
        let (exit_code, status_to_write) = (self.handler)(prompt, n);
        // The mock doesn't write status — the test setup handles it
        let _ = status_to_write;
        Ok(exit_code)
    }

    fn run_interactive(
        &self,
        _system_prompt: &str,
        _working_dir: &Path,
        _extra_args: &[String],
        _extra_env: &[(String, String)],
    ) -> Result<i32> {
        Ok(0)
    }
}

#[cfg(test)]
mod transcript_rate_limit_tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn detects_session_limit_marker() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript.txt");
        let mut f = File::create(&path).unwrap();
        writeln!(f, "Some normal output line").unwrap();
        writeln!(
            f,
            "You've hit your session limit · resets 7:10pm (America/Los_Angeles)"
        )
        .unwrap();
        writeln!(f, "Error: Coder exited with code 1").unwrap();
        drop(f);
        assert!(transcript_indicates_rate_limit(&path));
    }

    #[test]
    fn detects_generic_rate_limit_phrase() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript.txt");
        let mut f = File::create(&path).unwrap();
        writeln!(f, "Some normal output line").unwrap();
        writeln!(f, "rate-limit exceeded").unwrap();
        drop(f);
        assert!(transcript_indicates_rate_limit(&path));
    }

    #[test]
    fn no_marker_returns_false() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript.txt");
        let mut f = File::create(&path).unwrap();
        writeln!(f, "All good, no limit hit here").unwrap();
        writeln!(f, "Some other text").unwrap();
        drop(f);
        assert!(!transcript_indicates_rate_limit(&path));
    }

    #[test]
    fn missing_file_returns_false() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nonexistent.txt");
        assert!(!transcript_indicates_rate_limit(&path));
    }
}
