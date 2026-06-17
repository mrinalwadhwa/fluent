use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime};

const DEFAULT_CLAUDE_MODEL: &str = "claude-opus-4-6";
const DEFAULT_PI_MODEL: &str = "qwen3-30b-a3b";

fn claude_model() -> String {
    std::env::var("FACTORY_CLAUDE_MODEL")
        .or_else(|_| std::env::var("FACTORY_MODEL"))
        .unwrap_or_else(|_| DEFAULT_CLAUDE_MODEL.to_string())
}

fn codex_model() -> Option<String> {
    std::env::var("FACTORY_CODEX_MODEL").ok()
}

fn pi_model() -> String {
    std::env::var("FACTORY_PI_MODEL")
        .or_else(|_| std::env::var("FACTORY_MODEL"))
        .unwrap_or_else(|_| DEFAULT_PI_MODEL.to_string())
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CoderKind {
    Claude,
    Codex,
    Pi,
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
            "pi" => Ok(Self::Pi),
            other => bail!("Unknown coder '{other}'. Available: claude, codex, pi."),
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Pi => "pi",
        }
    }

    pub fn default_model(&self) -> String {
        match self {
            Self::Claude => claude_model(),
            Self::Codex => codex_model().unwrap_or_else(|| "o3".to_string()),
            Self::Pi => pi_model(),
        }
    }

    pub fn boxed(&self, sandbox: CoderSandbox) -> Box<dyn Coder> {
        self.boxed_with_model(sandbox, None)
    }

    pub fn boxed_with_model(&self, sandbox: CoderSandbox, model: Option<&str>) -> Box<dyn Coder> {
        match self {
            Self::Claude => match sandbox {
                CoderSandbox::SeatbeltProfile(profile) => Box::new(SandboxedClaudeCode {
                    sandbox_profile: Some(profile),
                    model_override: model.map(str::to_string),
                }),
                _ => Box::new(BareClaudeCode {
                    model_override: model.map(str::to_string),
                }),
            },
            Self::Codex => Box::new(CodexCode {
                sandbox_profile: match &sandbox {
                    CoderSandbox::SeatbeltProfile(profile) => Some(profile.clone()),
                    _ => None,
                },
                model_override: model.map(str::to_string),
            }),
            Self::Pi => Box::new(PiCode {
                sandbox_profile: match &sandbox {
                    CoderSandbox::SeatbeltProfile(profile) => Some(profile.clone()),
                    _ => None,
                },
                model_override: model.map(str::to_string),
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
    pub model_override: Option<String>,
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
        if let Err(auth_err) = crate::claude_auth::ensure_not_expired() {
            bail!(auth_err.user_message());
        }
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
    fn effective_model(&self) -> String {
        self.model_override.clone().unwrap_or_else(claude_model)
    }

    fn build_command(&self, working_dir: &Path) -> Command {
        let model = self.effective_model();
        if let Some(ref profile) = self.sandbox_profile {
            let mut cmd = Command::new("sandbox-exec");
            cmd.args(["-f", profile]);
            cmd.arg("claude");
            cmd.arg("--dangerously-skip-permissions");
            cmd.args(["--model", &model]);
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
pub struct BareClaudeCode {
    pub model_override: Option<String>,
}

impl BareClaudeCode {
    fn effective_model(&self) -> String {
        self.model_override.clone().unwrap_or_else(claude_model)
    }
}

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
        if let Err(auth_err) = crate::claude_auth::ensure_not_expired() {
            bail!(auth_err.user_message());
        }
        let want_transcript = transcript_file.is_some();
        let model = self.effective_model();
        run_with_transcript_retrying(
            || {
                let mut cmd = Command::new("claude");
                cmd.current_dir(working_dir);
                for (key, value) in extra_env {
                    cmd.env(key, value);
                }
                cmd.args(["--dangerously-skip-permissions"]);
                cmd.args(["--model", &model]);
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
    pub model_override: Option<String>,
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
    fn effective_model(&self) -> Option<String> {
        self.model_override.clone().or_else(codex_model)
    }

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
        if let Some(model) = self.effective_model() {
            cmd.args(["--model", &model]);
        }
        cmd.current_dir(working_dir);
        cmd
    }
}

/// Pi (pi.dev) coding agent backed by a local vllm-mlx model.
pub struct PiCode {
    pub sandbox_profile: Option<String>,
    pub model_override: Option<String>,
}

impl PiCode {
    fn effective_model(&self) -> String {
        self.model_override.clone().unwrap_or_else(pi_model)
    }

    fn build_command(&self, working_dir: &Path) -> Command {
        let model = self.effective_model();
        if let Some(ref profile) = self.sandbox_profile {
            let mut cmd = Command::new("sandbox-exec");
            cmd.args(["-f", profile]);
            cmd.arg("pi");
            cmd.args(["--provider", "local-openai"]);
            cmd.args(["--model", &model]);
            cmd.current_dir(working_dir);
            cmd
        } else {
            let mut cmd = Command::new("pi");
            cmd.args(["--provider", "local-openai"]);
            cmd.args(["--model", &model]);
            cmd.current_dir(working_dir);
            cmd
        }
    }
}

impl Coder for PiCode {
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
                    cmd.args(["--mode", "json"]);
                }
                cmd.args(["--thinking", "off"]);
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
        cmd.args(["--thinking", "off"]);
        cmd.args(["--append-system-prompt", system_prompt]);
        cmd.args(extra_args);

        let status = cmd.status()?;
        Ok(status.code().unwrap_or(1))
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

// ---------------------------------------------------------------------------
// Rate-limit parsing, jitter, and state tracking
// ---------------------------------------------------------------------------

const DEFAULT_RATE_LIMIT_RETRY_AFTER_SECS: u64 = 1800;
const RATE_LIMIT_MAX_RETRIES: u32 = 2;
const DEFAULT_JITTER_MAX_SECS: u64 = 30;

/// Parsed rate-limit info from a coder transcript.
#[derive(Debug, Clone)]
pub struct RateLimitInfo {
    pub retry_at: SystemTime,
    pub reason: String,
}

/// Track whether the retry loop is in a rate-limited state, so
/// notifications fire on state transitions rather than on every retry.
#[derive(Debug, Clone)]
enum RateLimitState {
    Normal,
    RateLimited { retry_at: SystemTime },
}

fn rate_limit_retry_after() -> Duration {
    let secs = std::env::var("FACTORY_RATE_LIMIT_RETRY_AFTER_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(DEFAULT_RATE_LIMIT_RETRY_AFTER_SECS);
    Duration::from_secs(secs)
}

fn jitter_max_secs() -> u64 {
    std::env::var("FACTORY_RATE_LIMIT_JITTER_MAX_SECONDS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(DEFAULT_JITTER_MAX_SECS)
}

/// Per-process randomized jitter to stagger concurrent Factory runs.
pub fn rate_limit_jitter() -> Duration {
    rate_limit_jitter_with_max(jitter_max_secs())
}

fn rate_limit_jitter_with_max(max: u64) -> Duration {
    if max == 0 {
        return Duration::ZERO;
    }
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos() as u64;
    let jitter_secs = (nanos ^ (std::process::id() as u64)) % (max + 1);
    Duration::from_secs(jitter_secs)
}

/// Parse structured rate-limit info from a transcript JSONL file.
///
/// Walks all lines and returns the last (most recent) rate-limit event
/// that contains parseable timing information. Returns `None` when no
/// such event is found.
///
/// Handles two provider event shapes:
/// - Claude Code: `{"type":"rate_limit_event","retry_after":N,...}` or
///   `{"type":"rate_limit_event","reset_at":"ISO-8601",...}`
/// - Codex: `{"type":"error","code":"rate_limit","retry_after":N,...}`
pub fn parse_rate_limit_from_transcript(transcript_path: &Path) -> Option<RateLimitInfo> {
    let file = File::open(transcript_path).ok()?;
    let reader = BufReader::new(file);
    let mut last: Option<RateLimitInfo> = None;

    for line in reader.lines().map_while(Result::ok) {
        let val: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let event_type = val["type"].as_str().unwrap_or("");

        let info = match event_type {
            "rate_limit_event" => parse_claude_rate_limit_event(&val),
            "error" => parse_codex_error_event(&val),
            _ => None,
        };

        if info.is_some() {
            last = info;
        }
    }

    last
}

/// Parse a Claude Code `rate_limit_event` into `RateLimitInfo`.
///
/// Accepted fields (checked in order):
/// - `retry_after`: seconds until retry (integer)
/// - `retry_after_ms`: milliseconds until retry (integer)
/// - `reset_at`: ISO-8601 timestamp when the limit resets
fn parse_claude_rate_limit_event(val: &serde_json::Value) -> Option<RateLimitInfo> {
    let reason = val["message"]
        .as_str()
        .unwrap_or("Rate limited")
        .to_string();

    let retry_at = if let Some(secs) = val["retry_after"].as_u64() {
        SystemTime::now() + Duration::from_secs(secs)
    } else if let Some(ms) = val["retry_after_ms"].as_u64() {
        SystemTime::now() + Duration::from_millis(ms)
    } else if let Some(reset_str) = val["reset_at"].as_str() {
        parse_iso8601_to_system_time(reset_str)?
    } else {
        return None;
    };

    Some(RateLimitInfo { retry_at, reason })
}

/// Parse a Codex `error` event with `code: "rate_limit"` into `RateLimitInfo`.
fn parse_codex_error_event(val: &serde_json::Value) -> Option<RateLimitInfo> {
    if val["code"].as_str() != Some("rate_limit") {
        return None;
    }

    let reason = val["message"]
        .as_str()
        .unwrap_or("Rate limited")
        .to_string();

    let retry_at = if let Some(secs) = val["retry_after"].as_u64() {
        SystemTime::now() + Duration::from_secs(secs)
    } else if let Some(reset_str) = val["reset_at"].as_str() {
        parse_iso8601_to_system_time(reset_str)?
    } else {
        return None;
    };

    Some(RateLimitInfo { retry_at, reason })
}

/// Parse an ISO-8601 UTC timestamp into `SystemTime`.
fn parse_iso8601_to_system_time(s: &str) -> Option<SystemTime> {
    let dt = chrono::DateTime::parse_from_rfc3339(s).ok()?;
    let epoch_secs = dt.timestamp();
    if epoch_secs < 0 {
        return None;
    }
    let nanos = dt.timestamp_subsec_nanos();
    Some(
        SystemTime::UNIX_EPOCH
            + Duration::from_secs(epoch_secs as u64)
            + Duration::from_nanos(nanos as u64),
    )
}

/// Format a `SystemTime` as a human-readable local time string.
fn format_retry_time(t: SystemTime) -> String {
    let dt: chrono::DateTime<chrono::Local> = t.into();
    dt.format("%H:%M:%S").to_string()
}

/// Advance the rate-limit state machine. Fires notifications on
/// Normal→RateLimited only; RateLimited→RateLimited updates the
/// retry_at without re-notifying.
fn transition_rate_limit_state(
    current: &RateLimitState,
    reason: &str,
    retry_at: SystemTime,
    notify: &dyn Fn(&str, &str),
) -> RateLimitState {
    match current {
        RateLimitState::Normal => {
            let retry_time = format_retry_time(retry_at);
            notify(
                "Factory",
                &format!("Factory paused: {reason}. Will retry at {retry_time}."),
            );
            RateLimitState::RateLimited { retry_at }
        }
        RateLimitState::RateLimited { .. } => RateLimitState::RateLimited { retry_at },
    }
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
/// whose transcript contains a rate-limit marker, parse the retry-after
/// timing from the transcript, apply per-run jitter, and sleep before
/// retrying. Falls back to the configured fixed wait when no structured
/// timing is available. Fires notifications on rate-limit state transitions.
fn run_with_transcript_retrying<F>(build_cmd: F, transcript_file: Option<&Path>) -> Result<i32>
where
    F: Fn() -> Command,
{
    let mut attempt: u32 = 0;
    let mut rl_state = RateLimitState::Normal;

    loop {
        let exit = run_with_transcript(build_cmd(), transcript_file)?;
        if exit == 0 {
            if matches!(rl_state, RateLimitState::RateLimited { .. }) {
                crate::notify::notify("Factory", "Factory resumed after rate-limit pause.");
                eprintln!("  Rate-limit cleared — resuming.");
            }
            return Ok(exit);
        }

        let Some(path) = transcript_file else {
            return Ok(exit);
        };

        // 401 auth failure short-circuits before rate-limit detection.
        if let Some(auth_err) = crate::claude_auth::classify_transcript_401(path) {
            bail!(auth_err.user_message());
        }

        // Try structured parsing first, then fall back to text detection.
        let parsed = parse_rate_limit_from_transcript(path);
        let is_rate_limited = parsed.is_some() || transcript_indicates_rate_limit(path);

        if !is_rate_limited {
            return Ok(exit);
        }

        if attempt >= RATE_LIMIT_MAX_RETRIES {
            eprintln!(
                "  Rate-limit detected on attempt {}; retry budget exhausted, propagating exit code {exit}.",
                attempt + 1
            );
            return Ok(exit);
        }

        let jitter = rate_limit_jitter();

        let (wait, reason) = if let Some(ref info) = parsed {
            let now = SystemTime::now();
            let base_wait = info.retry_at.duration_since(now).unwrap_or(Duration::ZERO);
            (base_wait + jitter, info.reason.clone())
        } else {
            (
                rate_limit_retry_after() + jitter,
                "Rate limited".to_string(),
            )
        };

        let retry_at = SystemTime::now() + wait;
        rl_state =
            transition_rate_limit_state(&rl_state, &reason, retry_at, &crate::notify::notify);

        eprintln!(
            "  Rate-limit detected on attempt {} ({reason}); sleeping {}s before retry.",
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

#[cfg(test)]
mod rate_limit_parsing_tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn claude_code_parses_retry_after_seconds() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript.jsonl");
        let mut f = File::create(&path).unwrap();
        writeln!(
            f,
            r#"{{"type":"system","subtype":"init","session_id":"s1","model":"claude-opus-4-6"}}"#
        )
        .unwrap();
        writeln!(f, r#"{{"type":"rate_limit_event","retry_after":300,"message":"Rate limited for 5 minutes"}}"#).unwrap();
        drop(f);

        let info = parse_rate_limit_from_transcript(&path).expect("should parse");
        assert_eq!(info.reason, "Rate limited for 5 minutes");
        let until_retry = info.retry_at.duration_since(SystemTime::now()).unwrap();
        assert!(until_retry.as_secs() <= 300);
        assert!(until_retry.as_secs() >= 298);
    }

    #[test]
    fn claude_code_parses_retry_after_ms() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript.jsonl");
        let mut f = File::create(&path).unwrap();
        writeln!(
            f,
            r#"{{"type":"rate_limit_event","retry_after_ms":60000,"message":"Wait 60s"}}"#
        )
        .unwrap();
        drop(f);

        let info = parse_rate_limit_from_transcript(&path).expect("should parse");
        assert_eq!(info.reason, "Wait 60s");
        let until_retry = info.retry_at.duration_since(SystemTime::now()).unwrap();
        assert!(until_retry.as_secs() <= 60);
        assert!(until_retry.as_secs() >= 58);
    }

    #[test]
    fn claude_code_parses_reset_at_iso8601() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript.jsonl");
        let mut f = File::create(&path).unwrap();
        writeln!(f, r#"{{"type":"rate_limit_event","reset_at":"2099-01-01T00:00:00Z","message":"Resets Jan 1"}}"#).unwrap();
        drop(f);

        let info = parse_rate_limit_from_transcript(&path).expect("should parse");
        assert_eq!(info.reason, "Resets Jan 1");
        assert!(info.retry_at > SystemTime::now());
    }

    #[test]
    fn claude_code_returns_none_for_no_timing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript.jsonl");
        let mut f = File::create(&path).unwrap();
        writeln!(f, r#"{{"type":"rate_limit_event"}}"#).unwrap();
        drop(f);

        assert!(parse_rate_limit_from_transcript(&path).is_none());
    }

    #[test]
    fn claude_code_returns_none_for_unstructured_transcript() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript.txt");
        let mut f = File::create(&path).unwrap();
        writeln!(f, "You've hit your session limit · resets 7:10pm").unwrap();
        drop(f);

        assert!(parse_rate_limit_from_transcript(&path).is_none());
    }

    #[test]
    fn claude_code_returns_latest_event_when_multiple_present() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript.jsonl");
        let mut f = File::create(&path).unwrap();
        writeln!(
            f,
            r#"{{"type":"rate_limit_event","retry_after":60,"message":"First"}}"#
        )
        .unwrap();
        writeln!(
            f,
            r#"{{"type":"rate_limit_event","retry_after":300,"message":"Second"}}"#
        )
        .unwrap();
        drop(f);

        let info = parse_rate_limit_from_transcript(&path).expect("should parse");
        assert_eq!(info.reason, "Second");
        let until_retry = info.retry_at.duration_since(SystemTime::now()).unwrap();
        assert!(until_retry.as_secs() >= 298);
    }

    #[test]
    fn codex_parses_rate_limit_error_event() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript.jsonl");
        let mut f = File::create(&path).unwrap();
        writeln!(f, r#"{{"type":"thread.started","thread_id":"t1"}}"#).unwrap();
        writeln!(f, r#"{{"type":"error","code":"rate_limit","retry_after":120,"message":"Rate limit exceeded"}}"#).unwrap();
        drop(f);

        let info = parse_rate_limit_from_transcript(&path).expect("should parse");
        assert_eq!(info.reason, "Rate limit exceeded");
        let until_retry = info.retry_at.duration_since(SystemTime::now()).unwrap();
        assert!(until_retry.as_secs() <= 120);
        assert!(until_retry.as_secs() >= 118);
    }

    #[test]
    fn codex_returns_none_for_non_rate_limit_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript.jsonl");
        let mut f = File::create(&path).unwrap();
        writeln!(
            f,
            r#"{{"type":"error","code":"internal","message":"Something broke"}}"#
        )
        .unwrap();
        drop(f);

        assert!(parse_rate_limit_from_transcript(&path).is_none());
    }

    #[test]
    fn codex_returns_none_for_no_rate_limit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript.jsonl");
        let mut f = File::create(&path).unwrap();
        writeln!(f, r#"{{"type":"thread.started","thread_id":"t1"}}"#).unwrap();
        writeln!(f, r#"{{"type":"turn.completed"}}"#).unwrap();
        drop(f);

        assert!(parse_rate_limit_from_transcript(&path).is_none());
    }

    #[test]
    fn returns_none_for_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nonexistent.jsonl");
        assert!(parse_rate_limit_from_transcript(&path).is_none());
    }

    #[test]
    fn returns_none_for_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.jsonl");
        File::create(&path).unwrap();
        assert!(parse_rate_limit_from_transcript(&path).is_none());
    }

    #[test]
    fn fixture_claude_code_retry_after() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(
            "tests/fixtures/rate-limit-transcripts/claude-code/rate-limit-with-retry-after.jsonl",
        );
        let info = parse_rate_limit_from_transcript(&path).expect("should parse fixture");
        assert_eq!(
            info.reason,
            "You've hit your rate limit. Retry after 300 seconds."
        );
    }

    #[test]
    fn fixture_claude_code_reset_at() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(
            "tests/fixtures/rate-limit-transcripts/claude-code/rate-limit-with-reset-at.jsonl",
        );
        let info = parse_rate_limit_from_transcript(&path).expect("should parse fixture");
        assert!(info.reason.contains("session limit"));
    }

    #[test]
    fn fixture_claude_code_no_timing() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/rate-limit-transcripts/claude-code/rate-limit-no-timing.jsonl");
        assert!(parse_rate_limit_from_transcript(&path).is_none());
    }

    #[test]
    fn fixture_claude_code_no_rate_limit() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/rate-limit-transcripts/claude-code/no-rate-limit.jsonl");
        assert!(parse_rate_limit_from_transcript(&path).is_none());
    }

    #[test]
    fn fixture_claude_code_multiple_events() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/rate-limit-transcripts/claude-code/multiple-rate-limits.jsonl");
        let info = parse_rate_limit_from_transcript(&path).expect("should parse fixture");
        assert!(info.reason.contains("Second"));
    }

    #[test]
    fn codex_parses_reset_at_iso8601() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript.jsonl");
        let mut f = File::create(&path).unwrap();
        writeln!(f, r#"{{"type":"error","code":"rate_limit","reset_at":"2099-01-01T00:00:00Z","message":"Resets Jan 1"}}"#).unwrap();
        drop(f);

        let info = parse_rate_limit_from_transcript(&path).expect("should parse");
        assert_eq!(info.reason, "Resets Jan 1");
        assert!(info.retry_at > SystemTime::now());
    }

    #[test]
    fn fixture_codex_retry_after() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/rate-limit-transcripts/codex/rate-limit-with-retry-after.jsonl");
        let info = parse_rate_limit_from_transcript(&path).expect("should parse fixture");
        assert_eq!(info.reason, "Rate limit exceeded. Retry after 120 seconds.");
    }

    #[test]
    fn fixture_codex_reset_at() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/rate-limit-transcripts/codex/rate-limit-with-reset-at.jsonl");
        let info = parse_rate_limit_from_transcript(&path).expect("should parse fixture");
        assert!(info.reason.contains("Resets at"));
        assert!(info.retry_at > SystemTime::now());
    }

    #[test]
    fn fixture_codex_no_rate_limit() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/rate-limit-transcripts/codex/no-rate-limit.jsonl");
        assert!(parse_rate_limit_from_transcript(&path).is_none());
    }
}

#[cfg(test)]
mod jitter_tests {
    use super::*;

    #[test]
    fn jitter_respects_max() {
        let max = DEFAULT_JITTER_MAX_SECS;
        for _ in 0..100 {
            let j = rate_limit_jitter_with_max(max);
            assert!(j.as_secs() <= max);
        }
    }

    #[test]
    fn jitter_returns_zero_when_max_is_zero() {
        let j = rate_limit_jitter_with_max(0);
        assert_eq!(j, Duration::ZERO);
    }

    #[test]
    fn jitter_respects_custom_max() {
        let max = 10;
        for _ in 0..100 {
            let j = rate_limit_jitter_with_max(max);
            assert!(j.as_secs() <= max);
        }
    }
}

#[cfg(test)]
mod rate_limit_state_tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[test]
    fn normal_to_rate_limited_fires_enter_notification() {
        let calls: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
        let calls_clone = Arc::clone(&calls);
        let notify = move |title: &str, body: &str| {
            calls_clone
                .lock()
                .unwrap()
                .push((title.to_string(), body.to_string()));
        };

        let state = RateLimitState::Normal;
        let retry_at = SystemTime::now() + Duration::from_secs(300);
        let new_state = transition_rate_limit_state(&state, "Rate limited", retry_at, &notify);

        assert!(matches!(new_state, RateLimitState::RateLimited { .. }));
        let notifications = calls.lock().unwrap();
        assert_eq!(notifications.len(), 1);
        assert_eq!(notifications[0].0, "Factory");
        assert!(notifications[0].1.contains("Factory paused: Rate limited"));
    }

    #[test]
    fn rate_limited_to_rate_limited_does_not_refire_notification() {
        let calls: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
        let calls_clone = Arc::clone(&calls);
        let notify = move |title: &str, body: &str| {
            calls_clone
                .lock()
                .unwrap()
                .push((title.to_string(), body.to_string()));
        };

        let retry_at = SystemTime::now() + Duration::from_secs(300);
        let state = RateLimitState::RateLimited { retry_at };
        let new_retry_at = SystemTime::now() + Duration::from_secs(600);
        let new_state =
            transition_rate_limit_state(&state, "Rate limited again", new_retry_at, &notify);

        assert!(matches!(new_state, RateLimitState::RateLimited { .. }));
        if let RateLimitState::RateLimited { retry_at } = new_state {
            assert_eq!(retry_at, new_retry_at);
        }
        let notifications = calls.lock().unwrap();
        assert_eq!(notifications.len(), 0);
    }

    #[test]
    fn full_cycle_fires_enter_once_and_leave_once() {
        let calls: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
        let calls_clone = Arc::clone(&calls);
        let notify = move |title: &str, body: &str| {
            calls_clone
                .lock()
                .unwrap()
                .push((title.to_string(), body.to_string()));
        };

        // Normal → RateLimited (enter notification)
        let state = RateLimitState::Normal;
        let retry_at = SystemTime::now() + Duration::from_secs(300);
        let state = transition_rate_limit_state(&state, "Rate limited", retry_at, &notify);
        assert_eq!(calls.lock().unwrap().len(), 1);

        // RateLimited → RateLimited (no notification)
        let new_retry = SystemTime::now() + Duration::from_secs(600);
        let state = transition_rate_limit_state(&state, "Still limited", new_retry, &notify);
        assert_eq!(calls.lock().unwrap().len(), 1);

        // RateLimited → RateLimited again (no notification)
        let newer_retry = SystemTime::now() + Duration::from_secs(900);
        let _state = transition_rate_limit_state(&state, "Still limited", newer_retry, &notify);
        assert_eq!(calls.lock().unwrap().len(), 1);

        // The leave notification is checked separately in run_with_transcript_retrying
        // via the `if matches!(rl_state, RateLimited)` guard on exit-0.
    }
}

#[cfg(test)]
mod coder_kind_tests {
    use super::*;

    #[test]
    fn coder_kind_resolves_pi() {
        let kind = CoderKind::resolve(Some("pi")).unwrap();
        assert_eq!(kind, CoderKind::Pi);
    }

    #[test]
    fn coder_kind_resolves_claude() {
        let kind = CoderKind::resolve(Some("claude")).unwrap();
        assert_eq!(kind, CoderKind::Claude);
    }

    #[test]
    fn coder_kind_resolves_codex() {
        let kind = CoderKind::resolve(Some("codex")).unwrap();
        assert_eq!(kind, CoderKind::Codex);
    }

    #[test]
    fn coder_kind_rejects_unknown() {
        let result = CoderKind::resolve(Some("unknown"));
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("pi"), "error should list pi: {err}");
    }

    #[test]
    fn coder_kind_serializes_pi_as_kebab_case() {
        let json = serde_json::to_string(&CoderKind::Pi).unwrap();
        assert_eq!(json, "\"pi\"");
    }

    #[test]
    fn coder_kind_serializes_claude_as_kebab_case() {
        let json = serde_json::to_string(&CoderKind::Claude).unwrap();
        assert_eq!(json, "\"claude\"");
    }

    #[test]
    fn coder_kind_round_trips_all_variants() {
        for kind in [CoderKind::Claude, CoderKind::Codex, CoderKind::Pi] {
            let json = serde_json::to_string(&kind).unwrap();
            let parsed: CoderKind = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, kind);
        }
    }

    #[test]
    fn pi_as_str_returns_pi() {
        assert_eq!(CoderKind::Pi.as_str(), "pi");
    }
}
