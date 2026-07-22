use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime};

const DEFAULT_PI_MODEL: &str = "qwen3.6-35b-a3b";

fn trusted_sandbox_executable() -> &'static str {
    "/usr/bin/sandbox-exec"
}

fn claude_model() -> Option<String> {
    std::env::var("FLUENT_CLAUDE_MODEL")
        .or_else(|_| std::env::var("FLUENT_MODEL"))
        .ok()
}

fn codex_model() -> Option<String> {
    std::env::var("FLUENT_CODEX_MODEL").ok()
}

fn pi_model() -> String {
    std::env::var("FLUENT_PI_MODEL")
        .or_else(|_| std::env::var("FLUENT_MODEL"))
        .unwrap_or_else(|_| DEFAULT_PI_MODEL.to_string())
}

/// Apply Fluent's env defaults plus caller-provided extras to a Coder command.
/// `GIT_EDITOR` and `GIT_SEQUENCE_EDITOR` default to `false` so interactive editor
/// prompts (commit messages, `rebase -i` reword, broken commit messages during
/// `rebase --continue`) fail cleanly instead of hanging the non-interactive Coder.
/// Callers can override either by including it in `extra_env`.
fn apply_coder_env(cmd: &mut Command, extra_env: &[(String, String)]) {
    cmd.env("GIT_EDITOR", "false");
    cmd.env("GIT_SEQUENCE_EDITOR", "false");
    if let Some(working_dir) = cmd.get_current_dir().map(Path::to_path_buf) {
        cmd.env("PWD", working_dir);
    }
    cmd.env_remove("OLDPWD");
    for (key, value) in extra_env {
        cmd.env(key, value);
    }
}

fn restrict_trusted_coder_env(cmd: &mut Command) {
    const ALLOWED: &[&str] = &[
        "PATH",
        "HOME",
        "USER",
        "LOGNAME",
        "SHELL",
        "LANG",
        "LC_ALL",
        "TERM",
        "CLAUDE_CODE_OAUTH_TOKEN",
        "ANTHROPIC_API_KEY",
        "OPENAI_API_KEY",
        "CODEX_API_KEY",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "NO_PROXY",
    ];
    let retained = ALLOWED
        .iter()
        .filter_map(|key| std::env::var_os(key).map(|value| (*key, value)))
        .collect::<Vec<_>>();
    cmd.env_clear();
    cmd.envs(retained);
}

fn codex_ca_bundle() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("FLUENT_CODEX_CA_BUNDLE") {
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

/// Which coding agent the fluent should launch.
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
    TrustedSeatbeltProfile(String),
    SeatbeltRoots { writable_roots: Vec<PathBuf> },
}

impl CoderKind {
    pub fn resolve(value: Option<&str>) -> Result<Self> {
        let value = value
            .map(str::to_string)
            .or_else(|| std::env::var("FLUENT_CODER").ok())
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
            Self::Claude => claude_model().unwrap_or_default(),
            Self::Codex => codex_model().unwrap_or_default(),
            Self::Pi => pi_model(),
        }
    }

    pub fn boxed(&self, sandbox: CoderSandbox) -> Box<dyn Coder> {
        self.boxed_with_model(sandbox, None, None)
    }

    pub fn boxed_with_model(
        &self,
        sandbox: CoderSandbox,
        model: Option<&str>,
        effort: Option<&str>,
    ) -> Box<dyn Coder> {
        match self {
            Self::Claude => match sandbox {
                CoderSandbox::SeatbeltProfile(profile) => Box::new(SandboxedClaudeCode {
                    sandbox_profile: Some(profile),
                    trusted_sandbox: false,
                    model_override: model.map(str::to_string),
                    effort: effort.map(str::to_string),
                }),
                CoderSandbox::TrustedSeatbeltProfile(profile) => Box::new(SandboxedClaudeCode {
                    sandbox_profile: Some(profile),
                    trusted_sandbox: true,
                    model_override: model.map(str::to_string),
                    effort: effort.map(str::to_string),
                }),
                _ => Box::new(BareClaudeCode {
                    model_override: model.map(str::to_string),
                    effort: effort.map(str::to_string),
                }),
            },
            Self::Codex => Box::new(CodexCode {
                sandbox_profile: match &sandbox {
                    CoderSandbox::SeatbeltProfile(profile)
                    | CoderSandbox::TrustedSeatbeltProfile(profile) => Some(profile.clone()),
                    _ => None,
                },
                trusted_sandbox: matches!(sandbox, CoderSandbox::TrustedSeatbeltProfile(_)),
                model_override: model.map(str::to_string),
                effort: effort.map(str::to_string),
            }),
            Self::Pi => Box::new(PiCode {
                sandbox_profile: match &sandbox {
                    CoderSandbox::SeatbeltProfile(profile)
                    | CoderSandbox::TrustedSeatbeltProfile(profile) => Some(profile.clone()),
                    _ => None,
                },
                trusted_sandbox: matches!(sandbox, CoderSandbox::TrustedSeatbeltProfile(_)),
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
    pub trusted_sandbox: bool,
    pub model_override: Option<String>,
    pub effort: Option<String>,
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
        ensure_not_expired_with_refresh()?;
        let want_transcript = transcript_file.is_some();
        run_with_transcript_retrying(
            || {
                let mut cmd = self.build_command(working_dir);
                apply_coder_env(&mut cmd, extra_env);
                if want_transcript {
                    cmd.args(["--verbose", "--output-format", "stream-json"]);
                }
                cmd.args(["--append-system-prompt", system_prompt]);
                cmd.args(["-p", prompt]);
                cmd.args(extra_args);
                cmd
            },
            transcript_file,
            &crate::notify::notify,
            &real_credential_refresh,
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
        apply_coder_env(&mut cmd, extra_env);
        cmd.args(["--append-system-prompt", system_prompt]);
        cmd.args(extra_args);

        let status = cmd.status()?;
        Ok(status.code().unwrap_or(1))
    }
}

impl SandboxedClaudeCode {
    fn effective_model(&self) -> Option<String> {
        self.model_override.clone().or_else(claude_model)
    }

    fn build_command(&self, working_dir: &Path) -> Command {
        let model = self.effective_model();
        if let Some(ref profile) = self.sandbox_profile {
            let mut cmd = Command::new(if self.trusted_sandbox {
                trusted_sandbox_executable()
            } else {
                "sandbox-exec"
            });
            if self.trusted_sandbox {
                restrict_trusted_coder_env(&mut cmd);
            }
            cmd.args(["-f", profile]);
            cmd.arg("claude");
            cmd.arg("--dangerously-skip-permissions");
            if let Some(ref m) = model {
                cmd.args(["--model", m]);
            }
            if let Some(ref e) = self.effort {
                cmd.args(["--effort", e]);
            }
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
    pub effort: Option<String>,
}

impl BareClaudeCode {
    fn effective_model(&self) -> Option<String> {
        self.model_override.clone().or_else(claude_model)
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
        ensure_not_expired_with_refresh()?;
        let want_transcript = transcript_file.is_some();
        let model = self.effective_model();
        let effort = self.effort.clone();
        run_with_transcript_retrying(
            || {
                let mut cmd = Command::new("claude");
                cmd.current_dir(working_dir);
                apply_coder_env(&mut cmd, extra_env);
                cmd.args(["--dangerously-skip-permissions"]);
                if let Some(ref m) = model {
                    cmd.args(["--model", m]);
                }
                if let Some(ref e) = effort {
                    cmd.args(["--effort", e]);
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
            &crate::notify::notify,
            &real_credential_refresh,
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
        apply_coder_env(&mut cmd, extra_env);
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
    pub trusted_sandbox: bool,
    pub model_override: Option<String>,
    pub effort: Option<String>,
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
                apply_coder_env(&mut cmd, extra_env);
                if want_transcript {
                    cmd.arg("--json");
                }
                cmd.arg(&combined_prompt);
                cmd.args(extra_args);
                cmd
            },
            transcript_file,
            &crate::notify::notify,
            &real_credential_refresh,
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
        apply_coder_env(&mut cmd, extra_env);
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
            let mut cmd = Command::new(if self.trusted_sandbox {
                trusted_sandbox_executable()
            } else {
                "sandbox-exec"
            });
            if self.trusted_sandbox {
                restrict_trusted_coder_env(&mut cmd);
            }
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
        if let Some(ref effort) = self.effort {
            cmd.args(["-c", &format!("model_reasoning_effort={effort}")]);
        }
        cmd.current_dir(working_dir);
        cmd
    }
}

/// Pi (pi.dev) coding agent backed by a local vllm-mlx model.
pub struct PiCode {
    pub sandbox_profile: Option<String>,
    pub trusted_sandbox: bool,
    pub model_override: Option<String>,
}

impl PiCode {
    fn effective_model(&self) -> String {
        self.model_override.clone().unwrap_or_else(pi_model)
    }

    fn build_command(&self, working_dir: &Path) -> Command {
        let model = self.effective_model();
        if let Some(ref profile) = self.sandbox_profile {
            let mut cmd = Command::new(if self.trusted_sandbox {
                trusted_sandbox_executable()
            } else {
                "sandbox-exec"
            });
            if self.trusted_sandbox {
                restrict_trusted_coder_env(&mut cmd);
            }
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
                apply_coder_env(&mut cmd, extra_env);
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
            &crate::notify::notify,
            &real_credential_refresh,
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
        apply_coder_env(&mut cmd, extra_env);
        cmd.args(["--thinking", "off"]);
        cmd.args(["--append-system-prompt", system_prompt]);
        cmd.args(extra_args);

        let status = cmd.status()?;
        Ok(status.code().unwrap_or(1))
    }
}

/// How often the supervisor polls the child and pump for a terminal outcome.
/// It is a correctness poll for capture failure, not a stale-session timer.
const SUPERVISOR_POLL_INTERVAL: Duration = Duration::from_millis(20);

/// Run a command, optionally draining stdout into a transcript file through the
/// byte-oriented pump. When `transcript_file` is `None`, stdout inherits from
/// the parent process.
fn run_with_transcript(mut cmd: Command, transcript_file: Option<&Path>) -> Result<i32> {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    match transcript_file {
        Some(path) => {
            cmd.stdout(Stdio::piped());
            let mut child = cmd.spawn()?;
            let child_id = child.id();
            let stdout = child.stdout.take().expect("stdout was piped");

            let config = crate::transcript_pump::active_config();
            let status_path = crate::transcript_pump::status_path_for(path);
            let mut pump = crate::transcript_pump::spawn_pump(
                stdout,
                path.to_path_buf(),
                Some(status_path),
                crate::transcript_pump::console_preview_sink(),
                config,
            );

            // Supervise the child and pump together. A pump that fails while the
            // coder is still alive terminates and reaps the process group at
            // once, rather than letting the coder run on invisibly until it
            // exits on its own.
            loop {
                if let Some(terminal) = pump.try_terminal() {
                    match terminal {
                        Ok(_summary) => {
                            let status = child.wait()?;
                            terminate_process_group(child_id);
                            pump.join();
                            return Ok(status.code().unwrap_or(1));
                        }
                        Err(pump_err) => {
                            terminate_process_group(child_id);
                            let _ = child.wait();
                            pump.join();
                            return Err(anyhow::Error::new(pump_err));
                        }
                    }
                }
                if let Some(status) = child.try_wait()? {
                    // The leader exited. Terminate any surviving descendants in
                    // the group before waiting for the pump: a backgrounded
                    // descendant that inherited stdout would otherwise hold the
                    // pipe's write end open and the pump would wait for EOF
                    // forever. The leader's bytes are already buffered in the
                    // pipe, so closing the group lets the pump drain them to EOF
                    // and finish. Killing the group here reaps the descendants
                    // rather than racing a managed import against them.
                    terminate_process_group(child_id);
                    let terminal = pump.wait_terminal();
                    pump.join();
                    return match terminal {
                        Ok(_summary) => Ok(status.code().unwrap_or(1)),
                        Err(pump_err) => Err(anyhow::Error::new(pump_err)),
                    };
                }
                std::thread::sleep(SUPERVISOR_POLL_INTERVAL);
            }
        }
        None => {
            let mut child = cmd.spawn()?;
            let child_id = child.id();
            let status = child.wait()?;
            terminate_process_group(child_id);
            Ok(status.code().unwrap_or(1))
        }
    }
}

#[cfg(unix)]
fn terminate_process_group(leader: u32) {
    if let Ok(process_group) = i32::try_from(leader) {
        // The child was launched as its own process-group leader. Kill the
        // group before returning so descendants cannot race a managed import.
        unsafe {
            libc::kill(-process_group, libc::SIGKILL);
        }
    }
}

#[cfg(not(unix))]
fn terminate_process_group(_leader: u32) {}

// ---------------------------------------------------------------------------
// Rate-limit parsing, jitter, and state tracking
// ---------------------------------------------------------------------------

const DEFAULT_RATE_LIMIT_RETRY_AFTER_SECS: u64 = 1800;
const RATE_LIMIT_MAX_RETRIES: u32 = 2;
const DEFAULT_JITTER_MAX_SECS: u64 = 30;

fn ensure_not_expired_with_refresh() -> Result<(), crate::claude_auth::AuthError> {
    if crate::claude_auth::ensure_not_expired().is_err() {
        eprintln!("  Token expired — refreshing credentials before launch.");
        let _ = crate::credential::refresh_credentials();
        if let Err(err) = crate::claude_auth::ensure_not_expired() {
            return Err(err);
        }
        eprintln!("  Credential refresh resolved the expiry — proceeding.");
    }
    Ok(())
}

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
    RateLimited,
}

fn rate_limit_retry_after() -> Duration {
    let secs = std::env::var("FLUENT_RATE_LIMIT_RETRY_AFTER_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(DEFAULT_RATE_LIMIT_RETRY_AFTER_SECS);
    Duration::from_secs(secs)
}

fn jitter_max_secs() -> u64 {
    std::env::var("FLUENT_RATE_LIMIT_JITTER_MAX_SECONDS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(DEFAULT_JITTER_MAX_SECS)
}

/// Per-process randomized jitter to stagger concurrent Fluent runs.
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
                "Fluent",
                &format!("Fluent paused: {reason}. Will retry at {retry_time}."),
            );
            RateLimitState::RateLimited
        }
        RateLimitState::RateLimited => RateLimitState::RateLimited,
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

fn real_credential_refresh() {
    let _ = crate::credential::refresh_credentials();
}

/// Preserve the transcript of a just-finished attempt as an immutable sibling
/// before the next attempt truncates the live path. Each retried phase (a 401
/// refresh or a rate-limit wait) leaves its own durable `.<n>.jsonl` artifact,
/// so a session-ending 401 is not overwritten by the attempt that recovers it.
///
/// The sibling is opened create-new so an existing per-phase artifact is never
/// overwritten, and every failure propagates: a lost transcript record must not
/// pass silently, since it is the only durable evidence of the recovered phase.
/// Preserve the live transcript as an immutable per-phase sibling before a 401
/// or rate-limit retry replaces it. A failure here is a transcript-pump
/// infrastructure failure — the coder may already have produced side effects, so
/// it must never be mistaken for an ordinary retryable coder error and relaunch
/// a coder; a supported resume retries after the operator fixes the transport.
fn preserve_transcript_phase(
    transcript_file: Option<&Path>,
    phase: &mut u32,
) -> std::result::Result<(), crate::transcript_pump::TranscriptPumpError> {
    use crate::transcript_pump::TranscriptPumpError;
    let Some(path) = transcript_file else {
        return Ok(());
    };
    let preserved = phase_transcript_path(path, *phase);
    let contents = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(err) => {
            return Err(TranscriptPumpError::new(format!(
                "read live transcript at {} before phase preservation: {err}",
                path.display()
            )));
        }
    };
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&preserved)
        .map_err(|err| {
            TranscriptPumpError::new(format!(
                "preserve transcript phase to {}: {err}",
                preserved.display()
            ))
        })?;
    file.write_all(&contents).map_err(|err| {
        TranscriptPumpError::new(format!(
            "write preserved transcript phase to {}: {err}",
            preserved.display()
        ))
    })?;
    *phase += 1;
    Ok(())
}

/// The immutable per-phase transcript path derived from a live transcript path:
/// `run.jsonl` becomes `run.<phase>.jsonl`.
/// The next safe per-phase transcript number for a Task, derived from existing
/// `<stem>.N.<ext>` siblings on disk. Returns one past the highest existing
/// phase, or `0` when none exist, so a resumed Task continues numbering rather
/// than restarting a process-local counter and colliding with preserved
/// evidence.
fn next_transcript_phase(transcript_file: Option<&Path>) -> u32 {
    let Some(path) = transcript_file else {
        return 0;
    };
    let Some(dir) = path.parent() else {
        return 0;
    };
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let ext = path.extension().map(|e| e.to_string_lossy().to_string());
    let prefix = format!("{stem}.");

    let mut max_phase: Option<u32> = None;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            // Only regular files are preserved phase evidence; ignore anything
            // else so a stray directory cannot inflate the counter.
            if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
                continue;
            }
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let Some(rest) = name.strip_prefix(&prefix) else {
                continue;
            };
            let number = match &ext {
                Some(ext) => rest.strip_suffix(&format!(".{ext}")),
                None => Some(rest),
            };
            if let Some(number) = number
                && let Ok(parsed) = number.parse::<u32>()
            {
                max_phase = Some(max_phase.map_or(parsed, |m| m.max(parsed)));
            }
        }
    }
    max_phase.map_or(0, |m| m + 1)
}

fn phase_transcript_path(path: &Path, phase: u32) -> PathBuf {
    let mut name = path
        .file_stem()
        .map(|stem| stem.to_string_lossy().to_string())
        .unwrap_or_default();
    name.push_str(&format!(".{phase}"));
    if let Some(ext) = path.extension() {
        name.push('.');
        name.push_str(&ext.to_string_lossy());
    }
    path.with_file_name(name)
}

/// Run a Coder command with rate-limit-aware retry. After a non-zero exit
/// whose transcript contains a rate-limit marker, parse the retry-after
/// timing from the transcript, apply per-run jitter, and sleep before
/// retrying. Falls back to the configured fixed wait when no structured
/// timing is available. Fires notifications on rate-limit state transitions.
fn run_with_transcript_retrying<F>(
    build_cmd: F,
    transcript_file: Option<&Path>,
    notify_fn: &dyn Fn(&str, &str),
    refresh_fn: &dyn Fn(),
) -> Result<i32>
where
    F: Fn() -> Command,
{
    let mut attempt: u32 = 0;
    let mut rl_state = RateLimitState::Normal;
    let mut auth_refreshed = false;
    // Derive the starting phase from existing per-phase siblings so a resumed
    // Task never overwrites or collides with `.N.jsonl` evidence a prior run
    // preserved; the process-local counter alone would restart at 0.
    let mut phase: u32 = next_transcript_phase(transcript_file);

    loop {
        let exit = run_with_transcript(build_cmd(), transcript_file)?;
        if exit == 0 {
            if auth_refreshed {
                notify_fn("Fluent", "Recovered after credential refresh.");
                eprintln!("  Credential refresh resolved the auth issue — continuing.");
            }
            if matches!(rl_state, RateLimitState::RateLimited) {
                notify_fn("Fluent", "Fluent resumed after rate-limit pause.");
                eprintln!("  Rate-limit cleared — resuming.");
            }
            return Ok(exit);
        }

        let Some(path) = transcript_file else {
            return Ok(exit);
        };

        if let Some(auth_err) = crate::claude_auth::classify_transcript_401(path) {
            if !auth_refreshed {
                auth_refreshed = true;
                eprintln!("  Auth 401 detected — refreshing credentials and retrying.");
                preserve_transcript_phase(transcript_file, &mut phase)?;
                refresh_fn();
                continue;
            }
            return Err(anyhow::Error::new(auth_err));
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
        rl_state = transition_rate_limit_state(&rl_state, &reason, retry_at, notify_fn);

        eprintln!(
            "  Rate-limit detected on attempt {} ({reason}); sleeping {}s before retry.",
            attempt + 1,
            wait.as_secs()
        );
        preserve_transcript_phase(transcript_file, &mut phase)?;
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
mod pump_supervision_tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn pump_failure_terminates_and_reaps_live_coder() {
        // The pump cannot open its transcript because the path is a directory,
        // so it fails immediately while the coder is still sleeping. Supervision
        // must terminate and reap the live coder at once, persist the specific
        // pump error, and return a typed infrastructure failure — not wait out
        // the coder's 30-second sleep.
        let dir = tempfile::tempdir().unwrap();
        let transcript = dir.path().join("transcript.jsonl");
        std::fs::create_dir(&transcript).unwrap();

        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c").arg("printf 'starting\\n'; sleep 30");

        let started = Instant::now();
        let result = run_with_transcript(cmd, Some(&transcript));
        let elapsed = started.elapsed();

        let err = result.expect_err("a pump failure must surface as an error");
        assert!(
            err.downcast_ref::<crate::transcript_pump::TranscriptPumpError>()
                .is_some(),
            "the failure must be a typed transcript-pump infrastructure error: {err}"
        );
        assert!(
            elapsed < Duration::from_secs(10),
            "the live coder must be terminated promptly, not waited out; took {elapsed:?}"
        );

        let status_path = crate::transcript_pump::status_path_for(&transcript);
        let status: crate::transcript_pump::PumpStatus =
            serde_json::from_slice(&std::fs::read(&status_path).unwrap()).unwrap();
        assert_eq!(status.state, crate::transcript_pump::PumpState::Failed);
        assert!(
            status.error.is_some(),
            "the persisted status must name the specific pump error"
        );
    }

    #[test]
    fn successful_capture_returns_coder_exit_and_persists_bytes() {
        // The ordinary path: the coder writes records and exits 0. Every byte is
        // captured and the coder's exit code is returned.
        let dir = tempfile::tempdir().unwrap();
        let transcript = dir.path().join("transcript.jsonl");

        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c")
            .arg("printf '{\"type\":\"a\"}\\n{\"type\":\"b\"}\\n'; exit 0");

        let exit = run_with_transcript(cmd, Some(&transcript)).unwrap();
        assert_eq!(exit, 0);
        let body = std::fs::read_to_string(&transcript).unwrap();
        assert!(body.contains("\"type\":\"a\""));
        assert!(body.contains("\"type\":\"b\""));
    }

    #[cfg(unix)]
    #[test]
    fn transcript_capture_returns_when_descendant_holds_stdout_open() {
        // The leader emits a record and exits while a same-group descendant
        // inherits stdout and sleeps, holding the pipe's write end open. The
        // pump would wait for EOF forever unless supervision terminates the
        // surviving descendant first. Fluent must return promptly, reap the
        // descendant, preserve the leader's output, and leave terminal pump
        // status — not wait out the descendant's 30-second sleep.
        let dir = tempfile::tempdir().unwrap();
        let transcript = dir.path().join("transcript.jsonl");
        let pid_path = dir.path().join("descendant.pid");

        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c")
            .arg("sleep 30 & echo $! > descendant.pid; printf '{\"type\":\"leader\"}\\n'; exit 0")
            .current_dir(dir.path());

        let started = Instant::now();
        let exit = run_with_transcript(cmd, Some(&transcript)).unwrap();
        let elapsed = started.elapsed();

        assert_eq!(exit, 0);
        assert!(
            elapsed < Duration::from_secs(10),
            "supervision must not wait out the descendant's sleep; took {elapsed:?}"
        );

        let body = std::fs::read_to_string(&transcript).unwrap();
        assert!(
            body.contains("\"type\":\"leader\""),
            "the leader's output must be preserved: {body:?}"
        );

        let pid = std::fs::read_to_string(&pid_path).unwrap();
        let alive = Command::new("/bin/kill")
            .args(["-0", pid.trim()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert!(
            !alive.success(),
            "the same-group descendant must be reaped before returning"
        );

        let status_path = crate::transcript_pump::status_path_for(&transcript);
        let status: crate::transcript_pump::PumpStatus =
            serde_json::from_slice(&std::fs::read(&status_path).unwrap()).unwrap();
        assert_eq!(
            status.state,
            crate::transcript_pump::PumpState::Complete,
            "terminal pump status must record completed capture"
        );
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

        assert!(matches!(new_state, RateLimitState::RateLimited));
        let notifications = calls.lock().unwrap();
        assert_eq!(notifications.len(), 1);
        assert_eq!(notifications[0].0, "Fluent");
        assert!(notifications[0].1.contains("Fluent paused: Rate limited"));
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

        let _retry_at = SystemTime::now() + Duration::from_secs(300);
        let state = RateLimitState::RateLimited;
        let new_retry_at = SystemTime::now() + Duration::from_secs(600);
        let new_state =
            transition_rate_limit_state(&state, "Rate limited again", new_retry_at, &notify);

        assert!(matches!(new_state, RateLimitState::RateLimited));
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

#[cfg(test)]
mod model_default_tests {
    use super::*;
    use std::ffi::OsStr;

    #[test]
    fn pi_default_matches_local_vllm() {
        // Pi's local vllm serves this exact name; drift silently 404s Pi launches.
        assert_eq!(DEFAULT_PI_MODEL, "qwen3.6-35b-a3b");
    }

    #[test]
    fn apply_coder_env_sets_git_editor_defaults() {
        let mut cmd = Command::new("/bin/true");
        let dir = tempfile::tempdir().unwrap();
        cmd.current_dir(dir.path());
        apply_coder_env(&mut cmd, &[]);
        let envs: Vec<_> = cmd.get_envs().collect();
        assert!(
            envs.iter()
                .any(|(k, v)| *k == OsStr::new("GIT_EDITOR") && *v == Some(OsStr::new("false"))),
            "GIT_EDITOR default missing"
        );
        assert!(
            envs.iter()
                .any(|(k, v)| *k == OsStr::new("GIT_SEQUENCE_EDITOR")
                    && *v == Some(OsStr::new("false"))),
            "GIT_SEQUENCE_EDITOR default missing"
        );
        assert!(
            envs.iter()
                .any(|(k, v)| { *k == OsStr::new("PWD") && *v == Some(dir.path().as_os_str()) })
        );
        assert!(
            envs.iter()
                .any(|(k, v)| *k == OsStr::new("OLDPWD") && v.is_none())
        );
    }

    #[test]
    fn trusted_sandbox_always_uses_the_system_launcher() {
        assert_eq!(trusted_sandbox_executable(), "/usr/bin/sandbox-exec");
    }

    #[test]
    fn trusted_coder_environment_drops_unapproved_parent_paths() {
        let mut command = Command::new("/usr/bin/env");
        command.env("FLUENT_LIVE_PATH_SENTINEL", "/live/project");
        restrict_trusted_coder_env(&mut command);
        let output = command.output().unwrap();
        assert!(output.status.success());
        let environment = String::from_utf8(output.stdout).unwrap();
        assert!(!environment.contains("FLUENT_LIVE_PATH_SENTINEL"));
        assert!(!environment.lines().any(|line| line.starts_with("PWD=")));
        assert!(!environment.lines().any(|line| line.starts_with("OLDPWD=")));
    }

    #[cfg(unix)]
    #[test]
    fn coder_run_terminates_background_descendants_before_returning() {
        let dir = tempfile::tempdir().unwrap();
        let pid_path = dir.path().join("descendant.pid");
        let launched_path = dir.path().join("descendant-launched");
        let denied_path = dir.path().join("descendant-write");
        let mut command = Command::new("/bin/sh");
        command
            .arg("-c")
            .arg(
                "(echo launched > descendant-launched; sleep 0.2; echo escaped > descendant-write) & pid=$!; while [ ! -f descendant-launched ]; do :; done; echo $pid > descendant.pid",
            )
            .current_dir(dir.path());

        assert_eq!(run_with_transcript(command, None).unwrap(), 0);
        assert!(
            launched_path.exists(),
            "hostile descendant actually executed"
        );
        let pid = std::fs::read_to_string(pid_path).unwrap();
        let status = Command::new("/bin/kill")
            .args(["-0", pid.trim()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert!(
            !status.success(),
            "background descendant survived coder return"
        );
        std::thread::sleep(Duration::from_millis(300));
        assert!(
            !denied_path.exists(),
            "terminated descendant wrote after the coder boundary returned"
        );
    }

    #[test]
    fn apply_coder_env_lets_caller_override() {
        let mut cmd = Command::new("/bin/true");
        apply_coder_env(&mut cmd, &[("GIT_EDITOR".to_string(), "vim".to_string())]);
        let envs: Vec<_> = cmd.get_envs().collect();
        assert!(
            envs.iter()
                .any(|(k, v)| *k == OsStr::new("GIT_EDITOR") && *v == Some(OsStr::new("vim"))),
            "caller override of GIT_EDITOR should win"
        );
    }

    fn cmd_has_arg(cmd: &Command, arg: &str) -> bool {
        cmd.get_args().any(|a| a == OsStr::new(arg))
    }

    #[test]
    fn claude_command_omits_model_when_unset() {
        let coder = SandboxedClaudeCode {
            sandbox_profile: Some("/tmp/profile".to_string()),
            trusted_sandbox: false,
            model_override: None,
            effort: None,
        };
        let dir = tempfile::tempdir().unwrap();
        let cmd = coder.build_command(dir.path());
        assert!(
            !cmd_has_arg(&cmd, "--model"),
            "should not pass --model when no model is configured"
        );
    }

    #[test]
    fn claude_command_passes_model_when_set() {
        let coder = SandboxedClaudeCode {
            sandbox_profile: Some("/tmp/profile".to_string()),
            trusted_sandbox: false,
            model_override: Some("claude-sonnet-4-6".to_string()),
            effort: None,
        };
        let dir = tempfile::tempdir().unwrap();
        let cmd = coder.build_command(dir.path());
        assert!(
            cmd_has_arg(&cmd, "--model"),
            "should pass --model when model is configured"
        );
        assert!(
            cmd_has_arg(&cmd, "claude-sonnet-4-6"),
            "should pass the configured model"
        );
    }

    #[test]
    fn trusted_claude_sandbox_uses_the_system_launcher() {
        let coder = SandboxedClaudeCode {
            sandbox_profile: Some("/tmp/profile".to_string()),
            trusted_sandbox: true,
            model_override: None,
            effort: None,
        };
        let dir = tempfile::tempdir().unwrap();

        let cmd = coder.build_command(dir.path());

        assert_eq!(cmd.get_program(), OsStr::new("/usr/bin/sandbox-exec"));
    }

    #[test]
    fn bare_claude_command_omits_model_when_unset() {
        let coder = BareClaudeCode {
            model_override: None,
            effort: None,
        };
        assert!(
            coder.effective_model().is_none(),
            "effective_model should be None when no model is configured"
        );
    }

    #[test]
    fn codex_command_omits_model_when_unset() {
        let coder = CodexCode {
            sandbox_profile: None,
            trusted_sandbox: false,
            model_override: None,
            effort: None,
        };
        let dir = tempfile::tempdir().unwrap();
        let cmd = coder.build_command(dir.path(), true);
        assert!(
            !cmd_has_arg(&cmd, "--model"),
            "codex should not pass --model when no model is configured"
        );
    }

    #[test]
    fn codex_command_passes_model_when_set() {
        let coder = CodexCode {
            sandbox_profile: None,
            trusted_sandbox: false,
            model_override: Some("gpt-4o".to_string()),
            effort: None,
        };
        let dir = tempfile::tempdir().unwrap();
        let cmd = coder.build_command(dir.path(), true);
        assert!(
            cmd_has_arg(&cmd, "--model"),
            "codex should pass --model when model is configured"
        );
        assert!(
            cmd_has_arg(&cmd, "gpt-4o"),
            "codex should pass the configured model"
        );
    }

    #[test]
    fn claude_effort_passed_when_set() {
        let coder = SandboxedClaudeCode {
            sandbox_profile: Some("/tmp/profile".to_string()),
            trusted_sandbox: false,
            model_override: None,
            effort: Some("high".to_string()),
        };
        let dir = tempfile::tempdir().unwrap();
        let cmd = coder.build_command(dir.path());
        assert!(cmd_has_arg(&cmd, "--effort"), "should pass --effort flag");
        assert!(cmd_has_arg(&cmd, "high"), "should pass effort value");
    }

    #[test]
    fn claude_effort_omitted_when_unset() {
        let coder = SandboxedClaudeCode {
            sandbox_profile: Some("/tmp/profile".to_string()),
            trusted_sandbox: false,
            model_override: None,
            effort: None,
        };
        let dir = tempfile::tempdir().unwrap();
        let cmd = coder.build_command(dir.path());
        assert!(
            !cmd_has_arg(&cmd, "--effort"),
            "should not pass --effort when unset"
        );
    }

    #[test]
    fn codex_effort_passed_as_config_flag() {
        let coder = CodexCode {
            sandbox_profile: None,
            trusted_sandbox: false,
            model_override: None,
            effort: Some("medium".to_string()),
        };
        let dir = tempfile::tempdir().unwrap();
        let cmd = coder.build_command(dir.path(), true);
        assert!(
            cmd_has_arg(&cmd, "model_reasoning_effort=medium"),
            "codex should pass effort via -c flag"
        );
    }

    #[test]
    fn codex_effort_omitted_when_unset() {
        let coder = CodexCode {
            sandbox_profile: None,
            trusted_sandbox: false,
            model_override: None,
            effort: None,
        };
        let dir = tempfile::tempdir().unwrap();
        let cmd = coder.build_command(dir.path(), true);
        let args: Vec<_> = cmd.get_args().collect();
        let has_effort = args
            .iter()
            .any(|a| a.to_string_lossy().contains("model_reasoning_effort"));
        assert!(!has_effort, "codex should not pass effort when unset");
    }
}

#[cfg(test)]
mod auth_refresh_tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn make_401_script(counter_path: &Path, succeed_on_call: Option<u32>) -> String {
        let counter = counter_path.display();
        let success_check = match succeed_on_call {
            Some(n) => format!(r#"if [ "$count" -ge {n} ]; then exit 0; fi"#),
            None => String::new(),
        };
        format!(
            r#"count=0
if [ -f "{counter}" ]; then count=$(cat "{counter}"); fi
count=$((count + 1))
printf '%s' "$count" > "{counter}"
{success_check}
echo '{{"type":"result","api_error_status":401,"request_id":"req-test"}}'
exit 1"#
        )
    }

    #[test]
    fn coder_retries_once_after_credential_refresh_on_401() {
        let dir = tempfile::tempdir().unwrap();
        let transcript = dir.path().join("transcript.jsonl");
        let counter = dir.path().join("counter");
        let script = make_401_script(&counter, None);

        let refresh_count = Arc::new(Mutex::new(0u32));
        let refresh_clone = Arc::clone(&refresh_count);
        let refresh = move || {
            *refresh_clone.lock().unwrap() += 1;
        };

        let _ = run_with_transcript_retrying(
            move || {
                let mut cmd = Command::new("/bin/sh");
                cmd.arg("-c").arg(&script);
                cmd
            },
            Some(&transcript),
            &|_, _| {},
            &refresh,
        );

        let count: u32 = std::fs::read_to_string(&counter)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert_eq!(
            count, 2,
            "should invoke command exactly twice (original + one retry)"
        );
        assert_eq!(
            *refresh_count.lock().unwrap(),
            1,
            "should invoke refresh exactly once"
        );
    }

    #[test]
    fn coder_succeeds_on_retry_when_refresh_fixes_auth() {
        let dir = tempfile::tempdir().unwrap();
        let transcript = dir.path().join("transcript.jsonl");
        let counter = dir.path().join("counter");
        let script = make_401_script(&counter, Some(2));

        let result = run_with_transcript_retrying(
            move || {
                let mut cmd = Command::new("/bin/sh");
                cmd.arg("-c").arg(&script);
                cmd
            },
            Some(&transcript),
            &|_, _| {},
            &|| {},
        );

        assert_eq!(result.unwrap(), 0, "should succeed after retry");
    }

    #[test]
    fn coder_surfaces_auth_error_when_refresh_does_not_help() {
        let dir = tempfile::tempdir().unwrap();
        let transcript = dir.path().join("transcript.jsonl");
        let counter = dir.path().join("counter");
        let script = make_401_script(&counter, None);

        let result = run_with_transcript_retrying(
            move || {
                let mut cmd = Command::new("/bin/sh");
                cmd.arg("-c").arg(&script);
                cmd
            },
            Some(&transcript),
            &|_, _| {},
            &|| {},
        );

        let err = result.unwrap_err();
        assert!(
            err.downcast_ref::<crate::claude_auth::AuthError>()
                .is_some(),
            "should return AuthError, got: {err}"
        );
    }

    #[test]
    fn phase_transcript_path_inserts_phase_before_extension() {
        let base = Path::new("/tmp/learner/transcript.jsonl");
        assert_eq!(
            phase_transcript_path(base, 0),
            Path::new("/tmp/learner/transcript.0.jsonl")
        );
        assert_eq!(
            phase_transcript_path(base, 3),
            Path::new("/tmp/learner/transcript.3.jsonl")
        );
        let no_ext = Path::new("/tmp/learner/transcript");
        assert_eq!(
            phase_transcript_path(no_ext, 1),
            Path::new("/tmp/learner/transcript.1")
        );
    }

    #[test]
    fn auth_refresh_preserves_prior_transcript_phase() {
        let dir = tempfile::tempdir().unwrap();
        let transcript = dir.path().join("transcript.jsonl");
        let counter = dir.path().join("counter");
        // First call emits a session-ending 401; the refreshed retry exits 0
        // with no output, truncating the live transcript.
        let script = make_401_script(&counter, Some(2));

        let result = run_with_transcript_retrying(
            move || {
                let mut cmd = Command::new("/bin/sh");
                cmd.arg("-c").arg(&script);
                cmd
            },
            Some(&transcript),
            &|_, _| {},
            &|| {},
        );
        assert_eq!(result.unwrap(), 0, "should recover after refresh");

        // The recovering run truncates the live transcript, but the
        // session-ending 401 stays captured in an immutable per-phase sibling.
        let preserved = dir.path().join("transcript.0.jsonl");
        assert!(
            preserved.exists(),
            "the pre-refresh transcript phase must be preserved"
        );
        let body = std::fs::read_to_string(&preserved).unwrap();
        assert!(
            body.contains("\"api_error_status\":401"),
            "the preserved phase must capture the session-ending 401: {body}"
        );
    }

    #[test]
    fn preserve_transcript_phase_refuses_to_overwrite_an_existing_sibling() {
        let dir = tempfile::tempdir().unwrap();
        let transcript = dir.path().join("transcript.jsonl");
        std::fs::write(&transcript, "live phase\n").unwrap();
        // A per-phase sibling already occupies phase 0's slot.
        let occupied = dir.path().join("transcript.0.jsonl");
        std::fs::write(&occupied, "earlier immutable record\n").unwrap();

        let mut phase = 0;
        let err = preserve_transcript_phase(Some(&transcript), &mut phase)
            .expect_err("create-new must refuse to overwrite an existing artifact");
        assert!(
            err.to_string().contains("preserve transcript phase"),
            "the failure must surface, not pass silently: {err}"
        );
        assert_eq!(phase, 0, "a failed preservation must not advance the phase");
        assert_eq!(
            std::fs::read_to_string(&occupied).unwrap(),
            "earlier immutable record\n",
            "the earlier immutable record must be left intact"
        );
    }

    #[test]
    fn next_transcript_phase_continues_past_existing_siblings() {
        // On resume the phase counter must continue past preserved evidence, not
        // restart at 0 and collide with an existing `.N.jsonl` sibling.
        let dir = tempfile::tempdir().unwrap();
        let transcript = dir.path().join("transcript.jsonl");

        // No siblings yet.
        assert_eq!(next_transcript_phase(Some(&transcript)), 0);

        std::fs::write(dir.path().join("transcript.0.jsonl"), "p0\n").unwrap();
        std::fs::write(dir.path().join("transcript.1.jsonl"), "p1\n").unwrap();
        // The live transcript and adjacent status file must be ignored.
        std::fs::write(&transcript, "live\n").unwrap();
        std::fs::write(dir.path().join("transcript-pump.json"), "{}").unwrap();

        assert_eq!(
            next_transcript_phase(Some(&transcript)),
            2,
            "the next phase must be one past the highest preserved sibling"
        );
    }

    #[test]
    fn resumed_retry_preserves_a_new_phase_without_collision() {
        // With phases 0 and 1 already preserved, a resumed retry that hits a 401
        // must preserve the live transcript as phase 2 rather than colliding.
        let dir = tempfile::tempdir().unwrap();
        let transcript = dir.path().join("transcript.jsonl");
        std::fs::write(dir.path().join("transcript.0.jsonl"), "earlier 0\n").unwrap();
        std::fs::write(dir.path().join("transcript.1.jsonl"), "earlier 1\n").unwrap();

        let counter = dir.path().join("counter");
        let script = make_401_script(&counter, Some(2));
        let result = run_with_transcript_retrying(
            move || {
                let mut cmd = Command::new("/bin/sh");
                cmd.arg("-c").arg(&script);
                cmd
            },
            Some(&transcript),
            &|_, _| {},
            &|| {},
        );

        assert_eq!(result.unwrap(), 0, "should recover after refresh");
        assert!(
            dir.path().join("transcript.2.jsonl").exists(),
            "the resumed retry must preserve a fresh phase 2 sibling"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("transcript.0.jsonl")).unwrap(),
            "earlier 0\n",
            "preserved evidence from a prior run must be untouched"
        );
    }

    #[test]
    fn preserve_transcript_phase_copies_live_bytes_and_advances() {
        let dir = tempfile::tempdir().unwrap();
        let transcript = dir.path().join("transcript.jsonl");
        std::fs::write(&transcript, "phase zero body\n").unwrap();

        let mut phase = 0;
        preserve_transcript_phase(Some(&transcript), &mut phase).unwrap();
        assert_eq!(phase, 1, "a successful preservation advances the phase");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("transcript.0.jsonl")).unwrap(),
            "phase zero body\n"
        );

        // The next phase writes a distinct immutable sibling.
        std::fs::write(&transcript, "phase one body\n").unwrap();
        preserve_transcript_phase(Some(&transcript), &mut phase).unwrap();
        assert_eq!(phase, 2);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("transcript.1.jsonl")).unwrap(),
            "phase one body\n"
        );
    }

    #[test]
    fn phase_preservation_failure_surfaces_as_a_transcript_pump_error() {
        // A 401 refresh preserves the prior transcript phase before replacing it.
        // When that preservation fails — here phase 0's immutable sibling slot is
        // already occupied — the retry path must surface a typed transcript-pump
        // infrastructure error, not an ordinary coder error. The classifier then
        // keeps it out of the generic retry budget, so a coder is never relaunched
        // after possible side effects.
        let dir = tempfile::tempdir().unwrap();
        let transcript = dir.path().join("transcript.jsonl");
        // Occupy phase 0's slot with a directory: it is not a regular file, so
        // phase derivation ignores it and still targets phase 0, where
        // create-new then fails — exercising the typed phase-preservation error.
        std::fs::create_dir(dir.path().join("transcript.0.jsonl")).unwrap();

        let script = "echo '{\"type\":\"result\",\"api_error_status\":401,\"request_id\":\"req-test\"}'; exit 1";
        let result = run_with_transcript_retrying(
            move || {
                let mut cmd = Command::new("/bin/sh");
                cmd.arg("-c").arg(script);
                cmd
            },
            Some(&transcript),
            &|_, _| {},
            &|| {},
        );

        let err = result.expect_err("a failed phase preservation must surface as an error");
        assert!(
            err.downcast_ref::<crate::transcript_pump::TranscriptPumpError>()
                .is_some(),
            "phase-preservation failure must be a typed transcript-pump error: {err:#}"
        );
    }

    #[test]
    fn recovered_after_refresh_posts_notification() {
        let dir = tempfile::tempdir().unwrap();
        let transcript = dir.path().join("transcript.jsonl");
        let counter = dir.path().join("counter");
        let script = make_401_script(&counter, Some(2));

        let calls: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
        let calls_clone = Arc::clone(&calls);
        let notify = move |title: &str, body: &str| {
            calls_clone
                .lock()
                .unwrap()
                .push((title.to_string(), body.to_string()));
        };

        let refresh_count = Arc::new(Mutex::new(0u32));
        let refresh_clone = Arc::clone(&refresh_count);
        let refresh = move || {
            *refresh_clone.lock().unwrap() += 1;
        };

        let result = run_with_transcript_retrying(
            move || {
                let mut cmd = Command::new("/bin/sh");
                cmd.arg("-c").arg(&script);
                cmd
            },
            Some(&transcript),
            &notify,
            &refresh,
        );

        assert_eq!(result.unwrap(), 0);
        let notifications = calls.lock().unwrap();
        assert_eq!(
            notifications.len(),
            1,
            "should post exactly one notification"
        );
        assert_eq!(notifications[0].0, "Fluent");
        assert!(
            notifications[0].1.contains("credential refresh"),
            "notification should mention credential refresh: {}",
            notifications[0].1
        );
        assert_eq!(
            *refresh_count.lock().unwrap(),
            1,
            "should invoke refresh exactly once"
        );
    }

    /// Exercise the real credential-refresh path end-to-end.
    /// Requires a valid Anthropic session; run with:
    ///   cargo nextest run --run-ignored -E 'test(real_credential_refresh)'
    #[test]
    #[ignore]
    fn real_credential_refresh_through_retry_path() {
        let dir = tempfile::tempdir().unwrap();
        let transcript = dir.path().join("transcript.jsonl");
        let counter = dir.path().join("counter");
        let script = make_401_script(&counter, Some(2));

        let result = run_with_transcript_retrying(
            move || {
                let mut cmd = Command::new("/bin/sh");
                cmd.arg("-c").arg(&script);
                cmd
            },
            Some(&transcript),
            &|_, _| {},
            &real_credential_refresh,
        );

        assert_eq!(result.unwrap(), 0, "should succeed after real refresh");
    }
}
