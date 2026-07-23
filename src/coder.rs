use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
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
/// A resolved, immutable per-launch transcript capture: where to persist the
/// canonical byte stream and the pump thresholds to use for it.
///
/// The config travels WITH the launch rather than through mutable process-global
/// state, so a concurrent launch (for example a parallel reviewer) can never
/// overwrite another capture's resolved thresholds between resolution and pump
/// spawn. The one value is retained across a launch's auth/rate-limit phases.
pub struct TranscriptCapture<'a> {
    pub(crate) path: &'a Path,
    pub(crate) config: crate::transcript_pump::TranscriptPumpConfig,
}

impl<'a> TranscriptCapture<'a> {
    /// Construct a capture boundary for a launch: persist the canonical transcript
    /// at `transcript_path`, resolving this project's layered pump thresholds from
    /// `project_root`. This is the intentional public constructor for external
    /// [`Coder`] implementations — it never requires the caller to name the private
    /// pump configuration. Internal callers that have already resolved the config
    /// once (to retain it across retry phases) use the crate-private `with_config`.
    pub fn new(transcript_path: &'a Path, project_root: &Path) -> Self {
        Self {
            path: transcript_path,
            config: crate::transcript_pump::resolve_config(project_root),
        }
    }

    /// The transcript path this capture persists to, for external `Coder`
    /// implementations that pipe stdout themselves.
    pub fn path(&self) -> &Path {
        self.path
    }

    /// Construct from an already-resolved configuration. Crate-private: the resolved
    /// config is threaded through a launch's retry phases so a concurrent launch can
    /// never overwrite it, and the config type is not part of the public API.
    pub(crate) fn with_config(
        path: &'a Path,
        config: crate::transcript_pump::TranscriptPumpConfig,
    ) -> Self {
        Self { path, config }
    }
}

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

    /// Launch the coder draining stdout into a byte pump configured by an
    /// immutable per-launch [`TranscriptCapture`].
    ///
    /// This is the production entry point: it threads the resolved config into
    /// the pump at spawn, so capture never depends on a mutable process-global
    /// value a concurrent launch could replace. The default implementation is for
    /// coders without a byte pump (mocks, interactive shims): it drops the config
    /// and runs the legacy transcript path.
    fn run_captured(
        &self,
        prompt: &str,
        system_prompt: &str,
        working_dir: &Path,
        extra_args: &[String],
        extra_env: &[(String, String)],
        capture: Option<&TranscriptCapture<'_>>,
    ) -> Result<i32> {
        self.run(
            prompt,
            system_prompt,
            working_dir,
            extra_args,
            extra_env,
            capture.map(|c| c.path),
        )
    }

    /// Like [`run_captured`], but also surfaces the per-launch supervision report so a
    /// role artifact boundary can persist a group-sweep diagnostic instead of dropping
    /// it with the `ManagedChild`. The default wraps the legacy `run_captured` with an
    /// empty report — enough for external coders and mocks that run no byte pump;
    /// built-in coders override it to carry the real report out of the supervisor.
    ///
    /// [`run_captured`]: Coder::run_captured
    fn run_captured_reported(
        &self,
        prompt: &str,
        system_prompt: &str,
        working_dir: &Path,
        extra_args: &[String],
        extra_env: &[(String, String)],
        capture: Option<&TranscriptCapture<'_>>,
    ) -> CoderRunCompletion {
        CoderRunCompletion::terminal_only(self.run_captured(
            prompt,
            system_prompt,
            working_dir,
            extra_args,
            extra_env,
            capture,
        ))
    }

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
        let capture = transcript_file.map(|path| TranscriptCapture::with_config(path, Default::default()));
        self.run_captured(
            prompt,
            system_prompt,
            working_dir,
            extra_args,
            extra_env,
            capture.as_ref(),
        )
    }

    fn run_captured(
        &self,
        prompt: &str,
        system_prompt: &str,
        working_dir: &Path,
        extra_args: &[String],
        extra_env: &[(String, String)],
        capture: Option<&TranscriptCapture<'_>>,
    ) -> Result<i32> {
        self.run_captured_reported(
            prompt,
            system_prompt,
            working_dir,
            extra_args,
            extra_env,
            capture,
        )
        .into_result()
    }

    fn run_captured_reported(
        &self,
        prompt: &str,
        system_prompt: &str,
        working_dir: &Path,
        extra_args: &[String],
        extra_env: &[(String, String)],
        capture: Option<&TranscriptCapture<'_>>,
    ) -> CoderRunCompletion {
        if let Err(err) = ensure_not_expired_with_refresh() {
            return CoderRunCompletion::terminal_only(Err(err.into()));
        }
        let want_transcript = capture.is_some();
        let transcript_file = capture.map(|c| c.path);
        let config = capture.map(|c| c.config.clone()).unwrap_or_default();
        run_with_transcript_retrying_reported(
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
            &config,
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
        let capture = transcript_file.map(|path| TranscriptCapture::with_config(path, Default::default()));
        self.run_captured(
            prompt,
            system_prompt,
            working_dir,
            extra_args,
            extra_env,
            capture.as_ref(),
        )
    }

    fn run_captured(
        &self,
        prompt: &str,
        system_prompt: &str,
        working_dir: &Path,
        extra_args: &[String],
        extra_env: &[(String, String)],
        capture: Option<&TranscriptCapture<'_>>,
    ) -> Result<i32> {
        self.run_captured_reported(
            prompt,
            system_prompt,
            working_dir,
            extra_args,
            extra_env,
            capture,
        )
        .into_result()
    }

    fn run_captured_reported(
        &self,
        prompt: &str,
        system_prompt: &str,
        working_dir: &Path,
        extra_args: &[String],
        extra_env: &[(String, String)],
        capture: Option<&TranscriptCapture<'_>>,
    ) -> CoderRunCompletion {
        if let Err(err) = ensure_not_expired_with_refresh() {
            return CoderRunCompletion::terminal_only(Err(err.into()));
        }
        let want_transcript = capture.is_some();
        let transcript_file = capture.map(|c| c.path);
        let config = capture.map(|c| c.config.clone()).unwrap_or_default();
        let model = self.effective_model();
        let effort = self.effort.clone();
        run_with_transcript_retrying_reported(
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
            &config,
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
        let capture = transcript_file.map(|path| TranscriptCapture::with_config(path, Default::default()));
        self.run_captured(
            prompt,
            system_prompt,
            working_dir,
            extra_args,
            extra_env,
            capture.as_ref(),
        )
    }

    fn run_captured(
        &self,
        prompt: &str,
        system_prompt: &str,
        working_dir: &Path,
        extra_args: &[String],
        extra_env: &[(String, String)],
        capture: Option<&TranscriptCapture<'_>>,
    ) -> Result<i32> {
        self.run_captured_reported(
            prompt,
            system_prompt,
            working_dir,
            extra_args,
            extra_env,
            capture,
        )
        .into_result()
    }

    fn run_captured_reported(
        &self,
        prompt: &str,
        system_prompt: &str,
        working_dir: &Path,
        extra_args: &[String],
        extra_env: &[(String, String)],
        capture: Option<&TranscriptCapture<'_>>,
    ) -> CoderRunCompletion {
        let want_transcript = capture.is_some();
        let transcript_file = capture.map(|c| c.path);
        let config = capture.map(|c| c.config.clone()).unwrap_or_default();
        let combined_prompt = format!("{system_prompt}\n\n---\n\n{prompt}");
        run_with_transcript_retrying_reported(
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
            &config,
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
        let capture = transcript_file.map(|path| TranscriptCapture::with_config(path, Default::default()));
        self.run_captured(
            prompt,
            system_prompt,
            working_dir,
            extra_args,
            extra_env,
            capture.as_ref(),
        )
    }

    fn run_captured(
        &self,
        prompt: &str,
        system_prompt: &str,
        working_dir: &Path,
        extra_args: &[String],
        extra_env: &[(String, String)],
        capture: Option<&TranscriptCapture<'_>>,
    ) -> Result<i32> {
        self.run_captured_reported(
            prompt,
            system_prompt,
            working_dir,
            extra_args,
            extra_env,
            capture,
        )
        .into_result()
    }

    fn run_captured_reported(
        &self,
        prompt: &str,
        system_prompt: &str,
        working_dir: &Path,
        extra_args: &[String],
        extra_env: &[(String, String)],
        capture: Option<&TranscriptCapture<'_>>,
    ) -> CoderRunCompletion {
        let want_transcript = capture.is_some();
        let transcript_file = capture.map(|c| c.path);
        let config = capture.map(|c| c.config.clone()).unwrap_or_default();
        run_with_transcript_retrying_reported(
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
            &config,
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
/// byte-oriented pump configured by `config`. When `transcript_file` is `None`,
/// stdout inherits from the parent process.
///
/// The child is owned by a [`CoderSupervisor`] guard from the instant it is
/// spawned — before stdout is taken and before the pump thread is spawned — so
/// any failure or panic in that window still terminates and reaps the coder
/// process group rather than leaking a live child. Both branches route through
/// the same guard, so a `wait`/`try_wait` error can never bypass cleanup.
/// A serializable projection of a [`ProcessOpError`] for the durable supervision
/// sidecar: the failed operation, its OS `ErrorKind`, errno, and original message, so
/// the diagnostic persisted at a role artifact boundary is lossless.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProcessOpDiagnostic {
    pub operation: String,
    pub kind: Option<String>,
    pub errno: Option<i32>,
    pub message: Option<String>,
}

impl ProcessOpDiagnostic {
    fn from_process_op_error(err: &ProcessOpError) -> Self {
        Self {
            operation: err.op.to_string(),
            kind: err.kind.map(|k| format!("{k:?}")),
            errno: err.errno,
            message: err.message.clone(),
        }
    }
}

/// A per-launch coder supervision report surfaced from the supervisor so its
/// group-sweep diagnostic — an unconfirmed sweep after a reaped leader — is carried out
/// of the coder and persisted at the role's artifact boundary rather than dropped when
/// the `ManagedChild` is dropped.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct CoderSupervisionReport {
    /// The retained process-group sweep failure when the leader was reaped but its
    /// group could not be verifiably swept (`killpg` returned `EPERM`, or another
    /// non-`ESRCH` error). `None` when the group was verifiably swept — or no
    /// pump/sweep ran — which is the clean, nothing-to-persist case.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub group_sweep_unconfirmed: Option<ProcessOpDiagnostic>,
}

impl CoderSupervisionReport {
    /// Whether the report carries no diagnostic worth persisting.
    pub fn is_clean(&self) -> bool {
        self.group_sweep_unconfirmed.is_none()
    }
}

/// A coder run's terminal outcome paired with its per-launch supervision report. The
/// legacy `run_captured -> Result<i32>` entry points delegate through `into_result`,
/// discarding the report; the reported entry points persist it at the role boundary.
pub struct CoderRunCompletion {
    pub terminal: Result<i32>,
    pub report: CoderSupervisionReport,
}

impl CoderRunCompletion {
    /// A completion carrying a terminal outcome and no supervision diagnostic — used by
    /// external coders and the spawn/setup-error paths that never supervised a pump.
    fn terminal_only(terminal: Result<i32>) -> Self {
        Self {
            terminal,
            report: CoderSupervisionReport::default(),
        }
    }

    /// Discard the report and return the terminal outcome for the legacy callers.
    pub(crate) fn into_result(self) -> Result<i32> {
        self.terminal
    }
}

/// The durable per-launch supervision sidecar filename written beside a role's other
/// transcript artifacts.
pub(crate) const CODER_SUPERVISION_SIDECAR: &str = "coder-supervision.json";

/// A failure to persist the coder supervision sidecar at a role artifact boundary. It
/// is a NON-retryable infrastructure fault: the coder already ran, so relaunching it
/// through the generic retry budget would repeat its side effects. It stays a
/// downcastable typed error so a role boundary composes it without masking the primary.
#[derive(Debug)]
pub struct SupervisionSidecarError(anyhow::Error);

impl std::fmt::Display for SupervisionSidecarError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "failed to persist coder supervision sidecar: {:#}", self.0)
    }
}

impl std::error::Error for SupervisionSidecarError {}

/// Persist a non-clean supervision report as `coder-supervision.json` in the role
/// artifact directory, written atomically via a temp file and rename. A clean report
/// has nothing to persist. A write obstruction returns a typed non-retryable error.
fn persist_coder_supervision(
    report: &CoderSupervisionReport,
    artifact_dir: &Path,
) -> Result<(), SupervisionSidecarError> {
    if report.is_clean() {
        return Ok(());
    }
    let write = || -> Result<()> {
        std::fs::create_dir_all(artifact_dir)
            .with_context(|| format!("create artifact dir {}", artifact_dir.display()))?;
        let json = serde_json::to_vec_pretty(report)?;
        let tmp = artifact_dir.join(format!("{CODER_SUPERVISION_SIDECAR}.tmp"));
        std::fs::write(&tmp, &json).with_context(|| format!("write {}", tmp.display()))?;
        std::fs::rename(&tmp, artifact_dir.join(CODER_SUPERVISION_SIDECAR))
            .with_context(|| "rename supervision sidecar into place".to_string())?;
        Ok(())
    };
    write().map_err(SupervisionSidecarError)
}

/// Persist the coder's supervision report at the role artifact boundary, then return
/// its terminal outcome. A sidecar obstruction is composed as a typed non-retryable
/// secondary — onto the run error if the run also failed, or returned as the error if
/// the run succeeded — so the group-sweep diagnostic is never silently dropped and the
/// already-run coder is never relaunched.
pub(crate) fn finish_supervised_coder_run(
    completion: CoderRunCompletion,
    artifact_dir: &Path,
) -> Result<i32> {
    let sidecar = persist_coder_supervision(&completion.report, artifact_dir);
    match (completion.terminal, sidecar) {
        (Ok(code), Ok(())) => Ok(code),
        (Ok(_), Err(sidecar_err)) => Err(anyhow::Error::new(sidecar_err)),
        (Err(run_err), Ok(())) => Err(run_err),
        // Keep the typed primary as the base of the context chain (still downcastable)
        // and attach the typed sidecar error as a downcastable secondary.
        (Err(run_err), Err(sidecar_err)) => Err(run_err.context(sidecar_err)),
    }
}

#[cfg(test)]
fn run_with_transcript(
    cmd: Command,
    transcript_file: Option<&Path>,
    config: &crate::transcript_pump::TranscriptPumpConfig,
) -> Result<i32> {
    run_with_transcript_reported(cmd, transcript_file, config).into_result()
}

/// Launch a coder, supervise its pump, and return both the terminal outcome AND the
/// per-launch supervision report, so the group-sweep diagnostic reaches the role
/// artifact boundary instead of being dropped with the `ManagedChild`.
fn run_with_transcript_reported(
    mut cmd: Command,
    transcript_file: Option<&Path>,
    config: &crate::transcript_pump::TranscriptPumpConfig,
) -> CoderRunCompletion {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    match transcript_file {
        Some(path) => {
            cmd.stdout(Stdio::piped());
            let child = match cmd.spawn() {
                Ok(child) => child,
                Err(err) => return CoderRunCompletion::terminal_only(Err(err.into())),
            };
            let child_id = child.id();
            // Own the child immediately: from here every exit path — a missing
            // stdout, a failed pump-thread spawn, or a `?` return — terminates and
            // reaps the coder group through the guard's cleanup.
            let mut supervisor = CoderSupervisor::new(child, child_id);

            // Finalize any stdout/pump SETUP failure explicitly through the managed
            // child — composing the setup error with any cleanup failure — rather
            // than escaping via `?` and leaving the sweep/reap (and its outcome) to
            // Drop. The supervision report is read AFTER the outcome settles, so even a
            // setup-error path surfaces any retained group-sweep diagnostic.
            let stdout = match supervisor.take_stdout() {
                Some(stdout) => stdout,
                None => {
                    let terminal = Err(supervisor
                        .finalize_setup_error(anyhow::anyhow!("coder stdout was not piped")));
                    return CoderRunCompletion {
                        report: supervisor.report(),
                        terminal,
                    };
                }
            };
            let status_path = crate::transcript_pump::status_path_for(path);
            let pump = match crate::transcript_pump::spawn_pump(
                stdout,
                path.to_path_buf(),
                Some(status_path),
                crate::transcript_pump::console_preview_sink(),
                config.clone(),
            ) {
                Ok(pump) => pump,
                Err(err) => {
                    let terminal = Err(supervisor.finalize_setup_error(anyhow::Error::new(err)));
                    return CoderRunCompletion {
                        report: supervisor.report(),
                        terminal,
                    };
                }
            };
            supervisor.attach_pump(pump);
            let terminal = supervisor.supervise();
            CoderRunCompletion {
                report: supervisor.report(),
                terminal,
            }
        }
        None => {
            let child = match cmd.spawn() {
                Ok(child) => child,
                Err(err) => return CoderRunCompletion::terminal_only(Err(err.into())),
            };
            let child_id = child.id();
            let mut supervisor = CoderSupervisor::new(child, child_id);
            let terminal = supervisor.wait_no_pump();
            CoderRunCompletion {
                report: supervisor.report(),
                terminal,
            }
        }
    }
}

/// A failed process operation, retaining WHICH syscall failed, the OS [`ErrorKind`],
/// the errno, and the ORIGINAL OS message, so a cleanup diagnostic is lossless — it
/// names the operation and its structured category and message rather than collapsing
/// every failure into a bare `false`/`None` or an opaque code. It OWNS the message, so
/// it is `Clone` but not `Copy`; the outcome enums that embed it (`GroupSweep`,
/// `CleanupError`) are `Clone` too. The kind/message are recoverable structurally from
/// the returned/durable diagnostic, not only from the rendered text.
///
/// [`ErrorKind`]: std::io::ErrorKind
#[derive(Debug, Clone)]
struct ProcessOpError {
    op: &'static str,
    errno: Option<i32>,
    kind: Option<std::io::ErrorKind>,
    message: Option<String>,
}

impl ProcessOpError {
    /// Retain the full structured payload of a failed syscall from its `io::Error`:
    /// the errno, the `ErrorKind`, and the original OS message.
    fn from_io(op: &'static str, err: &std::io::Error) -> Self {
        Self {
            op,
            errno: err.raw_os_error(),
            kind: Some(err.kind()),
            message: Some(err.to_string()),
        }
    }

    /// A structured outcome for a state with no underlying OS error: an id out of
    /// range, a signal not attempted, or a group that was never swept.
    fn without_os_error(op: &'static str) -> Self {
        Self {
            op,
            errno: None,
            kind: None,
            message: None,
        }
    }

    /// The structured OS `ErrorKind`, recoverable from the returned/durable diagnostic
    /// without parsing the rendered message.
    #[cfg(test)]
    fn kind(&self) -> Option<std::io::ErrorKind> {
        self.kind
    }

    /// The original OS message, recoverable from the returned/durable diagnostic.
    #[cfg(test)]
    fn message(&self) -> Option<&str> {
        self.message.as_deref()
    }
}

impl std::fmt::Display for ProcessOpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.op)?;
        match &self.message {
            // Prefer the retained original OS message; it already includes the errno.
            Some(message) => write!(f, ": {message}")?,
            None => {
                if let Some(errno) = self.errno {
                    write!(f, " (os error {errno})")?;
                }
            }
        }
        // Append the structured error category when known, so the durable diagnostic
        // carries the OS `ErrorKind` itself, not only its rendered message.
        if let Some(kind) = self.kind {
            write!(f, " [{kind:?}]")?;
        }
        Ok(())
    }
}

/// The result of sweeping the leader's process group. Both success arms prove the
/// group has no live members left to signal; `Failed` retains the OS error, so a
/// group that was not verifiably swept is a lossless structured outcome rather than
/// a heuristic guess derived from the leader's observed exit.
#[derive(Debug, Clone)]
enum GroupSweep {
    /// SIGKILL was delivered to the group.
    Delivered,
    /// The group was already gone (`ESRCH`): nothing remained to sweep.
    AlreadyGone,
    /// The signal failed for another reason; the group is not verifiably swept.
    Failed(ProcessOpError),
}

impl GroupSweep {
    /// Whether the group is verifiably swept — the signal landed, or the group was
    /// already gone. This is authoritative, never inferred from the leader's exit.
    fn swept(&self) -> bool {
        matches!(self, GroupSweep::Delivered | GroupSweep::AlreadyGone)
    }
}

/// Kill the coder's process group. The child was launched as its own process-group
/// leader, so killing the group sweeps descendants that could otherwise race a
/// managed import. Returns a structured [`GroupSweep`] retaining the OS error on a
/// genuine failure.
#[cfg(unix)]
fn terminate_process_group(leader: u32) -> GroupSweep {
    let Ok(process_group) = i32::try_from(leader) else {
        return GroupSweep::Failed(ProcessOpError::without_os_error(
            "process group id out of range",
        ));
    };
    let rc = unsafe { libc::kill(-process_group, libc::SIGKILL) };
    if rc == 0 {
        return GroupSweep::Delivered;
    }
    let os_error = std::io::Error::last_os_error();
    if os_error.raw_os_error() == Some(libc::ESRCH) {
        GroupSweep::AlreadyGone
    } else {
        GroupSweep::Failed(ProcessOpError::from_io("kill process group", &os_error))
    }
}

#[cfg(not(unix))]
fn terminate_process_group(_leader: u32) -> GroupSweep {
    GroupSweep::Delivered
}

/// Read the originating pid from a `siginfo_t` filled by `waitid`. The field is a
/// union accessor on Linux and a plain field on the BSD-derived platforms.
#[cfg(any(target_os = "linux", target_os = "android"))]
unsafe fn siginfo_pid(info: &libc::siginfo_t) -> libc::pid_t {
    unsafe { info.si_pid() }
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
unsafe fn siginfo_pid(info: &libc::siginfo_t) -> libc::pid_t {
    info.si_pid
}

/// Observe whether the leader has exited WITHOUT reaping it, so its PID keeps
/// pinning the process-group identity until the group has been swept. `block`
/// waits for the exit; otherwise it polls. `WNOWAIT` leaves the zombie waitable so
/// a later `Child::wait` still reaps it exactly once.
#[cfg(unix)]
fn observe_exit(pid: u32, block: bool) -> ExitObservation {
    let Ok(pid_t) = i32::try_from(pid) else {
        return ExitObservation::Unknown;
    };
    let mut flags = libc::WEXITED | libc::WNOWAIT;
    if !block {
        flags |= libc::WNOHANG;
    }
    loop {
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let rc = unsafe { libc::waitid(libc::P_PID, pid_t as libc::id_t, &mut info, flags) };
        if rc != 0 {
            let errno = std::io::Error::last_os_error().raw_os_error();
            // Retry an interrupted syscall rather than losing the observation.
            if errno == Some(libc::EINTR) {
                continue;
            }
            // ECHILD means the leader was already reaped: its PID/PGID identity is
            // gone, distinct from an unobservable or unsupported state.
            if errno == Some(libc::ECHILD) {
                return ExitObservation::IdentityLost;
            }
            return ExitObservation::Unknown;
        }
        // Under WNOHANG with no state change, `si_pid` stays zero.
        let si_pid = unsafe { siginfo_pid(&info) };
        return if si_pid == 0 {
            ExitObservation::Running
        } else {
            ExitObservation::Exited
        };
    }
}

#[cfg(not(unix))]
fn observe_exit(_pid: u32, _block: bool) -> ExitObservation {
    ExitObservation::Unknown
}

/// Whether the leader process has been observed to exit, without reaping it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ExitObservation {
    /// The leader is still running.
    Running,
    /// The leader has exited and is waitable, but has not been reaped, so its PID
    /// still pins the process-group identity.
    Exited,
    /// The leader's identity was lost (`ECHILD`: already reaped), so it can no longer
    /// be observed or reaped — kept distinct from an unobservable/unsupported state.
    IdentityLost,
    /// The exit state could not be observed (no platform support, or a transient error).
    Unknown,
}

/// The process operations a [`ManagedChild`] performs, behind a narrow seam so
/// cleanup ordering and failure paths are deterministically testable without real
/// processes.
trait LeaderProcess: Send {
    fn id(&self) -> u32;
    fn take_stdout(&mut self) -> Option<std::process::ChildStdout>;
    /// Non-blocking: observe leader exit without reaping (identity stays pinned).
    fn poll_exit(&mut self) -> ExitObservation;
    /// Block until the leader exits, without reaping it.
    fn wait_exit(&mut self) -> ExitObservation;
    /// SIGKILL the leader's whole process group, returning a structured sweep result
    /// that retains the OS error on a genuine failure.
    fn signal_group(&mut self) -> GroupSweep;
    /// SIGKILL the direct leader — the fallback when a verified group signal fails.
    /// `Err` retains the operation and OS error.
    fn kill_leader(&mut self) -> Result<(), ProcessOpError>;
    /// Reap the leader exactly once, returning its exit code, or a structured error
    /// retaining the failed operation and OS error.
    fn reap(&mut self) -> Result<i32, ProcessOpError>;
}

/// The production leader: a real child process launched as its own group leader.
struct SystemLeader {
    child: Child,
    id: u32,
    /// A cached exit code from a non-Unix poll (which must reap to observe), so the
    /// later `reap` returns it without a double wait. Unused on Unix, where
    /// `observe_exit` never reaps.
    cached_code: Option<i32>,
}

impl SystemLeader {
    fn new(child: Child, id: u32) -> Self {
        Self {
            child,
            id,
            cached_code: None,
        }
    }
}

impl LeaderProcess for SystemLeader {
    fn id(&self) -> u32 {
        self.id
    }

    fn take_stdout(&mut self) -> Option<std::process::ChildStdout> {
        self.child.stdout.take()
    }

    fn poll_exit(&mut self) -> ExitObservation {
        #[cfg(unix)]
        {
            observe_exit(self.id, false)
        }
        #[cfg(not(unix))]
        {
            // Without `waitid`, observation must reap; cache the code for `reap`.
            match self.child.try_wait() {
                Ok(Some(status)) => {
                    self.cached_code = Some(status.code().unwrap_or(1));
                    ExitObservation::Exited
                }
                Ok(None) => ExitObservation::Running,
                Err(_) => ExitObservation::Unknown,
            }
        }
    }

    fn wait_exit(&mut self) -> ExitObservation {
        #[cfg(unix)]
        {
            observe_exit(self.id, true)
        }
        #[cfg(not(unix))]
        {
            match self.child.wait() {
                Ok(status) => {
                    self.cached_code = Some(status.code().unwrap_or(1));
                    ExitObservation::Exited
                }
                Err(_) => ExitObservation::Unknown,
            }
        }
    }

    fn signal_group(&mut self) -> GroupSweep {
        terminate_process_group(self.id)
    }

    fn kill_leader(&mut self) -> Result<(), ProcessOpError> {
        self.child
            .kill()
            .map_err(|err| ProcessOpError::from_io("kill leader", &err))
    }

    fn reap(&mut self) -> Result<i32, ProcessOpError> {
        if let Some(code) = self.cached_code {
            return Ok(code);
        }
        self.child
            .wait()
            .map(|status| status.code().unwrap_or(1))
            .map_err(|err| ProcessOpError::from_io("reap leader", &err))
    }
}

/// A cleanup step that could not be completed. It is a diagnostic outcome the
/// caller composes with any primary pump failure rather than discards, so a coder
/// that could not be terminated or reaped is never silently reported as clean.
#[derive(Debug, Clone)]
enum CleanupError {
    /// Neither the verified group signal nor the direct-child kill settled the
    /// group, so the coder cannot be guaranteed terminated. Retains both OS errors.
    NotTerminated {
        id: u32,
        signal: ProcessOpError,
        kill: ProcessOpError,
    },
    /// The leader could not be reaped; retains the reap operation's OS error.
    NotReaped { id: u32, source: ProcessOpError },
    /// The leader's identity was already lost (`ECHILD`: reaped out from under the
    /// managed child). Its PID/PGID may now be recycled, so it was deliberately NOT
    /// signaled or reaped — doing so could hit an unrelated process.
    IdentityLost { id: u32 },
}

impl std::fmt::Display for CleanupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CleanupError::NotTerminated { id, signal, kill } => {
                write!(f, "coder process {id} could not be terminated (group signal {signal} and direct kill {kill} both failed)")
            }
            CleanupError::NotReaped { id, source } => {
                write!(f, "coder process {id} could not be reaped ({source})")
            }
            CleanupError::IdentityLost { id } => {
                write!(f, "coder process {id} identity was already lost (ECHILD); not signaled or reaped to avoid a recycled PID")
            }
        }
    }
}

impl std::error::Error for CleanupError {}

/// Records each cleanup outcome so the terminal path stays truthful: an unsuccessful
/// reap remains incomplete (and can be retried), while a successful one makes
/// repeated cleanup a no-op.
#[derive(Default)]
struct CleanupOutcome {
    /// The structured group-sweep result, once attempted. A swept sweep is the only
    /// state that proves the group and its descendants are gone; a `Failed` sweep
    /// retains its OS error.
    group_signal: Option<GroupSweep>,
    /// The direct-child kill result, once attempted as a fallback. It terminates the
    /// leader but never proves the group was swept; `Err` retains its OS error.
    direct_kill: Option<Result<(), ProcessOpError>>,
    /// The leader's cached exit code, once reaped. `Some` marks the leader reaped.
    exit_code: Option<i32>,
    /// The finalized cleanup result, computed exactly once. Once set, every repeated
    /// cleanup (explicit or `Drop`) returns it verbatim without any further
    /// signal/kill/reap — success AND failure are cached, so a stale or recycled PID
    /// is never signaled or reaped twice.
    finalized: Option<Result<i32, CleanupError>>,
}

impl CleanupOutcome {
    /// Whether the leader is terminated (the group was swept, or the direct kill
    /// succeeded). Only a swept group also guarantees descendants are gone.
    fn leader_terminated(&self) -> bool {
        matches!(&self.group_signal, Some(sweep) if sweep.swept())
            || matches!(&self.direct_kill, Some(Ok(())))
    }

    /// Whether the group and its descendants were verifiably swept.
    fn group_swept(&self) -> bool {
        matches!(&self.group_signal, Some(sweep) if sweep.swept())
    }

    /// The group-sweep OS error, when the group was not swept, for a cleanup error.
    fn group_sweep_error(&self) -> ProcessOpError {
        match &self.group_signal {
            Some(GroupSweep::Failed(err)) => err.clone(),
            _ => ProcessOpError::without_os_error("process group not swept"),
        }
    }

    /// The direct-kill OS error, when the fallback kill failed, for a cleanup error.
    fn direct_kill_error(&self) -> ProcessOpError {
        match &self.direct_kill {
            Some(Err(err)) => err.clone(),
            _ => ProcessOpError::without_os_error("direct child kill not attempted"),
        }
    }
}

/// The stateful owner of a coder leader, its process group, cleanup attempts, and
/// cached exit status. It preserves the leader's PID/PGID identity until the group
/// is swept, reaps the leader exactly once, and caches every outcome so repeated
/// explicit cleanup or a later `Drop` is idempotent — without falsely recording a
/// failed reap as complete or permanently latching a failed termination.
struct ManagedChild {
    leader: Box<dyn LeaderProcess>,
    outcome: CleanupOutcome,
}

impl ManagedChild {
    fn new(leader: Box<dyn LeaderProcess>) -> Self {
        Self {
            leader,
            outcome: CleanupOutcome::default(),
        }
    }

    fn id(&self) -> u32 {
        self.leader.id()
    }

    fn take_stdout(&mut self) -> Option<std::process::ChildStdout> {
        self.leader.take_stdout()
    }

    fn poll_exit(&mut self) -> ExitObservation {
        self.leader.poll_exit()
    }

    fn exit_code(&self) -> Option<i32> {
        self.outcome.exit_code
    }

    /// Whether the leader's process group is verifiably swept, so any pump writers
    /// are proven gone and a pump join cannot block on a surviving descendant.
    fn group_swept(&self) -> bool {
        self.outcome.group_swept()
    }

    /// The retained process-group sweep failure once a sweep was attempted but could
    /// not be confirmed (a reaped leader whose `killpg` returned `EPERM`, or another
    /// non-`ESRCH` error). `None` when the group was verifiably swept or no signal was
    /// attempted. This is a non-blocking, structured diagnostic so an unconfirmed
    /// sweep stays inspectable without writing to a possibly-saturated stderr during
    /// finalization.
    fn unconfirmed_group_sweep(&self) -> Option<ProcessOpError> {
        match &self.outcome.group_signal {
            Some(GroupSweep::Failed(err)) => Some(err.clone()),
            _ => None,
        }
    }

    /// Sweep the leader's process group while its PID still pins the group identity,
    /// then reap the leader exactly once. Idempotent and truthful: once reaped, a
    /// repeat is a no-op; a termination that both group-signals and direct-kills
    /// fails returns without blocking on a reap that may never complete, and leaves
    /// the state unsettled so a retry can re-attempt; a failed reap likewise stays
    /// incomplete and retryable. Its diagnostics survive on the outcome.
    fn terminate_and_reap(&mut self) -> Result<i32, CleanupError> {
        // One-shot: the first call computes and caches the terminal outcome; every
        // later call (an explicit retry or Drop) returns it verbatim WITHOUT any
        // further signal/kill/reap. Caching failure too is what guarantees a stale or
        // recycled PID is never signaled or reaped twice.
        if let Some(finalized) = &self.outcome.finalized {
            return finalized.clone();
        }
        let result = self.finalize_cleanup();
        self.outcome.finalized = Some(result.clone());
        result
    }

    /// Compute the cleanup outcome exactly once. Sweeps the group while the leader
    /// still pins its PID/PGID identity, then reaps. A leader whose identity was
    /// already lost (`ECHILD`) is NEVER signaled or reaped — its PID may be recycled.
    fn finalize_cleanup(&mut self) -> Result<i32, CleanupError> {
        let id = self.leader.id();
        // Observe the leader's exit without reaping so the group is swept while the
        // leader still pins PID/PGID identity. Pump EOF or a leader exit never means
        // descendants are already gone. The observation is not used to infer the
        // sweep — only the structured signal result decides that.
        let observed = self.leader.poll_exit();
        if observed == ExitObservation::IdentityLost {
            // The leader was reaped out from under us; its PID/PGID may already be
            // recycled. Signaling or reaping it could hit an unrelated process, so do
            // NEITHER. Identity loss does NOT prove the process group or its
            // descendants are gone, so the group stays unswept (`group_signal`
            // remains None → `group_swept()` is false) and Drop will not join a pump
            // whose descendant may still hold stdout open.
            return Err(CleanupError::IdentityLost { id });
        }
        let sweep = self.leader.signal_group();
        let swept = sweep.swept();
        self.outcome.group_signal = Some(sweep);
        if !swept {
            // The verified group signal failed; fall back to killing the direct
            // leader while it is still owned. This terminates the leader but does NOT
            // sweep the group or its descendants.
            self.outcome.direct_kill = Some(self.leader.kill_leader());
        }
        if !self.outcome.leader_terminated() {
            // Both the group signal and the direct kill failed; surface a cleanup
            // failure retaining both OS errors, without blocking on a reap.
            return Err(CleanupError::NotTerminated {
                id,
                signal: self.outcome.group_sweep_error(),
                kill: self.outcome.direct_kill_error(),
            });
        }
        // The leader is terminated. Reap it exactly once, retaining the reap's OS
        // error on failure.
        let code = match self.leader.reap() {
            Ok(code) => code,
            Err(source) => return Err(CleanupError::NotReaped { id, source }),
        };
        self.outcome.exit_code = Some(code);
        // The leader is terminated and reaped: a successful cleanup for the run.
        // Whether the process group was *verifiably* swept is a separate diagnostic
        // that `group_swept()` reports from the structured signal result. That gate
        // governs only whether Drop may join the pump — not the run's outcome. A
        // reaped leader whose group signal could not confirm the sweep (a normal path
        // once the leader has exited: the group is already gone and `killpg` returns
        // `EPERM`/`ESRCH`) is not a run fault, so it must not abort the task. Drop
        // stays conservative and detaches rather than joins the pump when the sweep
        // was not confirmed.
        // When the sweep was not confirmed, the structured `GroupSweep::Failed`
        // (with its OS error) stays retained on the outcome and is exposed by
        // `unconfirmed_group_sweep()` — a non-blocking, inspectable diagnostic. It is
        // deliberately NOT written to stderr here: this runs during coder
        // finalization, where a saturated fd 2 could block the write and reintroduce
        // the very deadlock class this pump design closes. The group is still never
        // recorded as swept (the failure stays distinct from an `ESRCH` `AlreadyGone`).
        Ok(code)
    }

    /// Block until the leader exits (without reaping), then sweep the group and reap.
    /// Used when the leader is expected to finish on its own — the no-transcript
    /// path and the EOF-first success path — so a healthy leader's natural exit code
    /// is preserved rather than the leader being killed merely because stdout closed.
    fn await_exit_then_cleanup(&mut self) -> Result<i32, CleanupError> {
        if self.outcome.exit_code.is_none() {
            let _ = self.leader.wait_exit();
        }
        self.terminate_and_reap()
    }
}

/// Owns a [`ManagedChild`] and (once spawned) its transcript pump for the duration
/// of supervision. Its `Drop` is the single structured-cleanup point: it sweeps and
/// reaps the coder through the managed child and settles the pump thread on every
/// exit path, so an error propagated by `?` — or a panic while wiring up the pump —
/// cannot leak a live coder, a surviving descendant, or a stuck pump thread.
struct CoderSupervisor {
    managed: ManagedChild,
    /// `None` until a pump is attached (the no-transcript branch never attaches
    /// one, and the transcript branch attaches it only after a successful spawn).
    pump: Option<crate::transcript_pump::PumpHandle>,
}

impl CoderSupervisor {
    fn new(child: Child, child_id: u32) -> Self {
        Self {
            managed: ManagedChild::new(Box::new(SystemLeader::new(child, child_id))),
            pump: None,
        }
    }

    #[cfg(test)]
    fn with_leader(leader: Box<dyn LeaderProcess>) -> Self {
        Self {
            managed: ManagedChild::new(leader),
            pump: None,
        }
    }

    fn take_stdout(&mut self) -> Option<std::process::ChildStdout> {
        self.managed.take_stdout()
    }

    /// The per-launch supervision report read from the managed child after its outcome
    /// settled: an unconfirmed group sweep (a reaped leader whose group could not be
    /// verifiably swept) is projected into a serializable diagnostic so the role
    /// artifact boundary can persist it instead of dropping it with the child.
    fn report(&self) -> CoderSupervisionReport {
        CoderSupervisionReport {
            group_sweep_unconfirmed: self
                .managed
                .unconfirmed_group_sweep()
                .as_ref()
                .map(ProcessOpDiagnostic::from_process_op_error),
        }
    }

    fn attach_pump(&mut self, pump: crate::transcript_pump::PumpHandle) {
        self.pump = Some(pump);
    }

    fn pump_mut(&mut self) -> &mut crate::transcript_pump::PumpHandle {
        self.pump
            .as_mut()
            .expect("supervise runs only after a pump is attached")
    }

    /// Poll the pump and leader together until one reaches a terminal outcome. A
    /// first fault or a pump failure while the coder is alive sweeps and reaps the
    /// coder at once; an EOF-first success waits for the healthy leader's own exit
    /// (EOF alone is never leader completion); a leader exit sweeps any surviving
    /// descendants before reaping. Every cleanup failure is composed with the pump
    /// outcome rather than discarded, and an unobservable leader is terminal rather
    /// than an infinite poll loop.
    fn supervise(&mut self) -> Result<i32> {
        loop {
            // First-fault fast path: the pump published its immutable first fault
            // (capture, preview, phase-preservation, or status persistence) before
            // attempting terminal settlement. Sweep and reap the coder NOW — only
            // then wait for the pump's terminal outcome, which a blocked or slow
            // status store may still be delaying. This stops a delayed status write
            // from extending invisible coder work.
            if self
                .pump
                .as_ref()
                .expect("supervise runs only after a pump is attached")
                .first_fault_observed()
            {
                let cleanup = self.managed.terminate_and_reap();
                let outcome = self.pump_mut().wait_terminal();
                return compose_terminal(outcome, cleanup);
            }
            if let Some(terminal) = self.pump_mut().try_terminal() {
                return match terminal {
                    Ok(_summary) => {
                        // The pump drained to EOF. EOF is NOT leader completion: wait
                        // for the healthy leader's own exit — preserving its natural
                        // exit code — then sweep the group and reap it exactly once.
                        self.managed
                            .await_exit_then_cleanup()
                            .map_err(anyhow::Error::new)
                    }
                    Err(pump_err) => {
                        // Sweep and reap the still-live coder now; compose any cleanup
                        // failure with the primary pump error rather than masking it.
                        let cleanup = self.managed.terminate_and_reap();
                        Err(compose_pump_error(pump_err, cleanup))
                    }
                };
            }
            match self.managed.poll_exit() {
                // A leader that exited, or whose identity was already lost (ECHILD),
                // is done producing output: sweep any surviving descendants in the
                // group BEFORE reaping, while the leader's PID still pins the group
                // identity — a backgrounded descendant that inherited stdout would
                // otherwise hold the pipe's write end open and the pump would wait for
                // EOF forever. Then let the pump drain to EOF.
                ExitObservation::Exited | ExitObservation::IdentityLost => {
                    let cleanup = self.managed.terminate_and_reap();
                    let outcome = self.pump_mut().wait_terminal();
                    return compose_terminal(outcome, cleanup);
                }
                ExitObservation::Unknown => {
                    // The leader's exit state cannot be observed; do not spin forever.
                    // Sweep and reap, and surface a terminal error composing any
                    // cleanup failure as typed context rather than a flattened string.
                    let cleanup = self.managed.terminate_and_reap();
                    let base = anyhow::anyhow!(
                        "coder process {} exit state could not be observed",
                        self.managed.id()
                    );
                    return Err(match cleanup {
                        Ok(_) => base,
                        Err(c) => base.context(c),
                    });
                }
                ExitObservation::Running => {}
            }
            std::thread::sleep(SUPERVISOR_POLL_INTERVAL);
        }
    }

    /// Wait a coder launched without a transcript pump. Blocking for the leader's
    /// exit, sweeping any descendants that inherited stdio, and reaping all route
    /// through the managed child, so a reap error still runs cleanup rather than
    /// leaking the group.
    fn wait_no_pump(&mut self) -> Result<i32> {
        self.managed
            .await_exit_then_cleanup()
            .map_err(anyhow::Error::new)
    }

    /// Finalize a stdout/pump SETUP failure explicitly: sweep and reap the coder
    /// through the managed child NOW and compose the typed setup error with any
    /// structured cleanup failure. Setup failures thus never rely on `Drop` to clean
    /// up (which cannot surface a cleanup failure) and never discard the cleanup
    /// outcome. `Drop` remains a safe idempotent no-op afterward.
    fn finalize_setup_error(&mut self, setup: anyhow::Error) -> anyhow::Error {
        match self.managed.terminate_and_reap() {
            Ok(_) => setup,
            Err(cleanup) => setup.context(cleanup),
        }
    }
}

/// Compose a pump terminal outcome with a cleanup outcome. A pump failure is the
/// primary cause; a cleanup failure that follows it is attached as context. When
/// the pump succeeded, a cleanup failure is itself the terminal error — a coder that
/// could not be terminated or reaped is never reported as clean success.
fn compose_terminal(
    pump: Result<crate::transcript_pump::PumpSummary, crate::transcript_pump::TranscriptPumpError>,
    cleanup: Result<i32, CleanupError>,
) -> Result<i32> {
    match pump {
        Ok(_summary) => cleanup.map_err(anyhow::Error::new),
        Err(pump_err) => Err(compose_pump_error(pump_err, cleanup)),
    }
}

/// Return the pump error as the primary cause, attaching a cleanup failure as
/// context so cleanup never masks the primary pump error.
fn compose_pump_error(
    pump_err: crate::transcript_pump::TranscriptPumpError,
    cleanup: Result<i32, CleanupError>,
) -> anyhow::Error {
    let err = anyhow::Error::new(pump_err);
    match cleanup {
        Ok(_) => err,
        // Attach the typed CleanupError as context (not a flattened string) so both
        // the primary pump fault and the structured cleanup failure stay downcastable.
        Err(c) => err.context(c),
    }
}

impl Drop for CoderSupervisor {
    fn drop(&mut self) {
        // Best-effort last-resort cleanup after a `?` early return or a panic; a
        // happy path already reaped, so this is then a no-op. There is no channel to
        // propagate a Drop-time failure, but supervise composes and surfaces cleanup
        // failures on every non-Drop path.
        let _ = self.managed.terminate_and_reap();
        // Join the pump ONLY when cleanup proved the group swept — writers are then
        // gone and the pump reaches EOF promptly. If the group could not be
        // verifiably swept, a surviving descendant may still hold the pipe's write
        // end open, so joining would block Drop forever; detach the pump thread
        // instead. The canonical transcript already holds every byte it persisted.
        if self.managed.group_swept() {
            if let Some(pump) = self.pump.as_mut() {
                pump.join();
            }
        }
    }
}

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
#[cfg(test)]
fn run_with_transcript_retrying<F>(
    build_cmd: F,
    transcript_file: Option<&Path>,
    config: &crate::transcript_pump::TranscriptPumpConfig,
    notify_fn: &dyn Fn(&str, &str),
    refresh_fn: &dyn Fn(),
) -> Result<i32>
where
    F: Fn() -> Command,
{
    run_with_transcript_retrying_reported(build_cmd, transcript_file, config, notify_fn, refresh_fn)
        .into_result()
}

/// The reported retry loop: identical retry behavior, but it returns the terminal
/// outcome AND the final attempt's supervision report, so an unconfirmed group sweep
/// survives auth/rate-limit retries and reaches the role artifact boundary.
fn run_with_transcript_retrying_reported<F>(
    build_cmd: F,
    transcript_file: Option<&Path>,
    config: &crate::transcript_pump::TranscriptPumpConfig,
    notify_fn: &dyn Fn(&str, &str),
    refresh_fn: &dyn Fn(),
) -> CoderRunCompletion
where
    F: Fn() -> Command,
{
    run_with_transcript_retrying_reported_using(
        build_cmd,
        transcript_file,
        config,
        notify_fn,
        refresh_fn,
        &|cmd, transcript, cfg| run_with_transcript_reported(cmd, transcript, cfg),
    )
}

/// The retry loop returning a `Result<i32>`, parameterized by a per-attempt run
/// function. Retained for tests that observe the exact config threaded into each
/// auth/rate-limit retry phase without spawning a real pump.
#[cfg(test)]
fn run_with_transcript_retrying_using<F>(
    build_cmd: F,
    transcript_file: Option<&Path>,
    config: &crate::transcript_pump::TranscriptPumpConfig,
    notify_fn: &dyn Fn(&str, &str),
    refresh_fn: &dyn Fn(),
    run_fn: &dyn Fn(
        Command,
        Option<&Path>,
        &crate::transcript_pump::TranscriptPumpConfig,
    ) -> Result<i32>,
) -> Result<i32>
where
    F: Fn() -> Command,
{
    run_with_transcript_retrying_reported_using(
        build_cmd,
        transcript_file,
        config,
        notify_fn,
        refresh_fn,
        &|cmd, transcript, cfg| CoderRunCompletion::terminal_only(run_fn(cmd, transcript, cfg)),
    )
    .into_result()
}

/// The retry loop core, parameterized by a per-attempt run function that returns a
/// [`CoderRunCompletion`]. The final attempt's supervision report is retained across
/// every auth/rate-limit retry and returned alongside the terminal outcome.
fn run_with_transcript_retrying_reported_using<F>(
    build_cmd: F,
    transcript_file: Option<&Path>,
    config: &crate::transcript_pump::TranscriptPumpConfig,
    notify_fn: &dyn Fn(&str, &str),
    refresh_fn: &dyn Fn(),
    run_fn: &dyn Fn(
        Command,
        Option<&Path>,
        &crate::transcript_pump::TranscriptPumpConfig,
    ) -> CoderRunCompletion,
) -> CoderRunCompletion
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
        // The one resolved config value is retained across every auth/rate-limit
        // phase of this launch, so a mid-launch retry never re-resolves or picks
        // up a different value.
        let completion = run_fn(build_cmd(), transcript_file, config);
        // Retain this attempt's supervision report; the loop returns the final
        // attempt's report so a group-sweep diagnostic is never dropped by a retry.
        let report = completion.report;
        let exit = match completion.terminal {
            Ok(exit) => exit,
            Err(err) => return CoderRunCompletion {
                terminal: Err(err),
                report,
            },
        };
        if exit == 0 {
            if auth_refreshed {
                notify_fn("Fluent", "Recovered after credential refresh.");
                eprintln!("  Credential refresh resolved the auth issue — continuing.");
            }
            if matches!(rl_state, RateLimitState::RateLimited) {
                notify_fn("Fluent", "Fluent resumed after rate-limit pause.");
                eprintln!("  Rate-limit cleared — resuming.");
            }
            return CoderRunCompletion {
                terminal: Ok(exit),
                report,
            };
        }

        let Some(path) = transcript_file else {
            return CoderRunCompletion {
                terminal: Ok(exit),
                report,
            };
        };

        if let Some(auth_err) = crate::claude_auth::classify_transcript_401(path) {
            if !auth_refreshed {
                auth_refreshed = true;
                eprintln!("  Auth 401 detected — refreshing credentials and retrying.");
                if let Err(err) = preserve_transcript_phase(transcript_file, &mut phase) {
                    return CoderRunCompletion {
                        terminal: Err(anyhow::Error::new(err)),
                        report,
                    };
                }
                refresh_fn();
                continue;
            }
            return CoderRunCompletion {
                terminal: Err(anyhow::Error::new(auth_err)),
                report,
            };
        }

        // Try structured parsing first, then fall back to text detection.
        let parsed = parse_rate_limit_from_transcript(path);
        let is_rate_limited = parsed.is_some() || transcript_indicates_rate_limit(path);

        if !is_rate_limited {
            return CoderRunCompletion {
                terminal: Ok(exit),
                report,
            };
        }

        if attempt >= RATE_LIMIT_MAX_RETRIES {
            eprintln!(
                "  Rate-limit detected on attempt {}; retry budget exhausted, propagating exit code {exit}.",
                attempt + 1
            );
            return CoderRunCompletion {
                terminal: Ok(exit),
                report,
            };
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
        if let Err(err) = preserve_transcript_phase(transcript_file, &mut phase) {
            return CoderRunCompletion {
                terminal: Err(anyhow::Error::new(err)),
                report,
            };
        }
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
    use std::sync::{Arc, Mutex};
    use std::time::Instant;

    /// A pump reader that panics on its first read, modelling a pump that crashes
    /// while its coder is still alive.
    struct PanicOnRead;

    impl std::io::Read for PanicOnRead {
        fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
            panic!("simulated pump panic");
        }
    }

    #[cfg(unix)]
    #[test]
    fn pump_panic_recovers_while_console_is_saturated() {
        // B6: a pump that panics while its coder is still alive must recover
        // promptly. The panic never blocks on a saturated console: the production
        // sink declines every preview (never touching fd 2) and the process-wide
        // hook keeps the pump thread's panic off the blocking default stderr path.
        // Supervision returns a typed failure and its guard terminates and reaps
        // the still-live coder.
        let dir = tempfile::tempdir().unwrap();
        let transcript = dir.path().join("transcript.jsonl");
        let status = crate::transcript_pump::status_path_for(&transcript);

        // A live coder that outlives the pump panic.
        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c").arg("sleep 5");
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }
        let child = cmd.spawn().unwrap();
        let child_id = child.id();

        // A pump whose reader panics immediately, while the coder keeps running.
        let pump = crate::transcript_pump::spawn_pump(
            PanicOnRead,
            transcript.clone(),
            Some(status.clone()),
            crate::transcript_pump::console_preview_sink(),
            crate::transcript_pump::TranscriptPumpConfig::default(),
        )
        .unwrap();

        let mut supervisor = CoderSupervisor::new(child, child_id);
        supervisor.attach_pump(pump);

        let started = Instant::now();
        let result = supervisor.supervise();
        let elapsed = started.elapsed();

        let err = result.expect_err("a pump panic must surface as a typed failure");
        assert!(
            err.downcast_ref::<crate::transcript_pump::TranscriptPumpError>()
                .is_some(),
            "the failure must be a typed transcript-pump error: {err}"
        );
        assert!(
            elapsed < Duration::from_secs(3),
            "panic recovery must be prompt, well before the coder's 5s sleep; took {elapsed:?}"
        );

        // The guard terminates and reaps the still-live coder.
        drop(supervisor);
        let alive = Command::new("/bin/kill")
            .args(["-0", &child_id.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        assert!(
            !alive,
            "the live coder must be terminated and reaped after the pump panic"
        );

        // The terminal status names the panic.
        let persisted: crate::transcript_pump::PumpStatus =
            serde_json::from_slice(&std::fs::read(&status).unwrap()).unwrap();
        assert_eq!(
            persisted.state,
            crate::transcript_pump::PumpState::Failed
        );
        assert!(persisted.error.is_some());
    }

    #[cfg(unix)]
    #[test]
    fn pump_failure_terminates_and_reaps_live_coder() {
        // The pump cannot open its transcript because the path is a directory, so
        // it fails immediately while the coder is still alive. A pump failure must
        // terminate and reap the coder's WHOLE process group — the leader and a
        // backgrounded descendant that never touches stdout, so neither can be
        // killed incidentally by SIGPIPE. Both must be gone and neither's delayed
        // side effect may occur after the typed failure returns.
        let dir = tempfile::tempdir().unwrap();
        let transcript = dir.path().join("transcript.jsonl");
        std::fs::create_dir(&transcript).unwrap();
        let d = dir.path().display();

        // The leader records its and the descendant's pids, then both sleep and
        // touch a side-effect file. Nothing reads or writes stdout.
        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c").arg(format!(
            "echo $$ > {d}/leader.pid; \
             ( sleep 1; : > {d}/descendant-ran ) & echo $! > {d}/descendant.pid; \
             sleep 1; : > {d}/leader-ran"
        ));

        let started = Instant::now();
        let result = run_with_transcript(cmd, Some(&transcript), &crate::transcript_pump::TranscriptPumpConfig::default());
        let elapsed = started.elapsed();

        let err = result.expect_err("a pump failure must surface as an error");
        assert!(
            err.downcast_ref::<crate::transcript_pump::TranscriptPumpError>()
                .is_some(),
            "the failure must be a typed transcript-pump infrastructure error: {err}"
        );
        assert!(
            elapsed < Duration::from_secs(1),
            "the live coder group must be terminated promptly; took {elapsed:?}"
        );

        let is_alive = |pid: &str| {
            Command::new("/bin/kill")
                .args(["-0", pid.trim()])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
        };
        for pidfile in ["leader.pid", "descendant.pid"] {
            let pid = std::fs::read_to_string(dir.path().join(pidfile))
                .unwrap_or_else(|_| panic!("{pidfile} must have been recorded"));
            assert!(
                !is_alive(&pid),
                "{pidfile} process survived the pump failure"
            );
        }

        // Wait past the 1s side-effect delay: neither the leader nor the reaped
        // descendant may run its delayed side effect after the boundary returned.
        std::thread::sleep(Duration::from_millis(1500));
        assert!(
            !dir.path().join("leader-ran").exists(),
            "the leader ran a delayed side effect after termination"
        );
        assert!(
            !dir.path().join("descendant-ran").exists(),
            "the descendant ran a delayed side effect after termination"
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

        let exit = run_with_transcript(cmd, Some(&transcript), &crate::transcript_pump::TranscriptPumpConfig::default()).unwrap();
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
        let exit = run_with_transcript(cmd, Some(&transcript), &crate::transcript_pump::TranscriptPumpConfig::default()).unwrap();
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

    /// A programmable leader for deterministic cleanup-ordering and failure tests.
    /// It records every operation the managed child performs, so the sweep-before-
    /// reap order, the direct-kill fallback, and exactly-once reaping are observable
    /// without spawning real processes.
    struct FakeLeader {
        id: u32,
        calls: Arc<Mutex<Vec<&'static str>>>,
        poll: ExitObservation,
        group_settles: bool,
        kill_succeeds: bool,
        /// Successive reap results, popped per call; an empty queue reaps `None`.
        reaps: std::collections::VecDeque<Option<i32>>,
    }

    impl FakeLeader {
        fn new(calls: Arc<Mutex<Vec<&'static str>>>, reap: Option<i32>) -> Self {
            Self {
                id: 4242,
                calls,
                poll: ExitObservation::Running,
                group_settles: true,
                kill_succeeds: true,
                reaps: std::collections::VecDeque::from([reap]),
            }
        }
    }

    impl LeaderProcess for FakeLeader {
        fn id(&self) -> u32 {
            self.id
        }
        fn take_stdout(&mut self) -> Option<std::process::ChildStdout> {
            None
        }
        fn poll_exit(&mut self) -> ExitObservation {
            self.calls.lock().unwrap().push("poll_exit");
            self.poll
        }
        fn wait_exit(&mut self) -> ExitObservation {
            self.calls.lock().unwrap().push("wait_exit");
            ExitObservation::Exited
        }
        fn signal_group(&mut self) -> GroupSweep {
            self.calls.lock().unwrap().push("signal_group");
            if self.group_settles {
                GroupSweep::Delivered
            } else {
                // Inject a real OS error so the retained sweep diagnostic carries a
                // recoverable ErrorKind and message, as a genuine EPERM sweep would.
                GroupSweep::Failed(ProcessOpError::from_io(
                    "fake group signal",
                    &std::io::Error::from_raw_os_error(libc::EPERM),
                ))
            }
        }
        fn kill_leader(&mut self) -> Result<(), ProcessOpError> {
            self.calls.lock().unwrap().push("kill_leader");
            if self.kill_succeeds {
                Ok(())
            } else {
                Err(ProcessOpError::from_io(
                    "fake direct kill",
                    &std::io::Error::from_raw_os_error(libc::EPERM),
                ))
            }
        }
        fn reap(&mut self) -> Result<i32, ProcessOpError> {
            self.calls.lock().unwrap().push("reap");
            self.reaps.pop_front().unwrap_or(None).ok_or_else(|| {
                ProcessOpError::from_io(
                    "fake reap",
                    &std::io::Error::from_raw_os_error(libc::ECHILD),
                )
            })
        }
    }

    #[test]
    fn group_signal_failure_falls_back_to_child_kill_and_reaps_once() {
        // B6: when the verified group signal fails, cleanup falls back to killing the
        // direct child while it is still owned, and reaps the leader exactly once
        // even across a repeated cleanup call.
        let calls = Arc::new(Mutex::new(Vec::new()));
        let leader = FakeLeader {
            group_settles: false,
            ..FakeLeader::new(Arc::clone(&calls), Some(0))
        };
        let mut managed = ManagedChild::new(Box::new(leader));
        // A direct-kill fallback terminates and reaps the leader — a successful
        // cleanup for the run — even though the group could not be verifiably swept.
        // A failed group signal after a reaped leader is a normal path (the group is
        // already gone), not a run-aborting fault; the same successful outcome is
        // cached and returned verbatim on repeat.
        assert_eq!(managed.terminate_and_reap().unwrap(), 0);
        assert_eq!(
            managed.terminate_and_reap().unwrap(),
            0,
            "the reaped outcome is cached on repeat, never re-signaled"
        );
        // The group was not verifiably swept, so Drop must stay conservative and not
        // join a pump that a surviving descendant could keep open.
        assert!(
            !managed.group_swept(),
            "an unconfirmed group sweep is never recorded as swept"
        );
        // The structured sweep failure stays retained and inspectable (a non-blocking
        // diagnostic), rather than being written to a possibly-saturated stderr.
        assert!(
            managed.unconfirmed_group_sweep().is_some(),
            "the unconfirmed sweep's OS error is retained for observation"
        );

        let calls = calls.lock().unwrap();
        assert_eq!(
            calls.iter().filter(|c| **c == "signal_group").count(),
            1,
            "the group is signaled once"
        );
        assert_eq!(
            calls.iter().filter(|c| **c == "kill_leader").count(),
            1,
            "a failed group signal falls back to a direct child kill"
        );
        assert_eq!(
            calls.iter().filter(|c| **c == "reap").count(),
            1,
            "the leader is reaped exactly once"
        );
        // The group is signaled before the leader is reaped.
        let signal_at = calls.iter().position(|c| *c == "signal_group").unwrap();
        let reap_at = calls.iter().position(|c| *c == "reap").unwrap();
        assert!(signal_at < reap_at, "the group is signaled before reaping");
    }

    #[test]
    fn repeated_cleanup_is_a_no_op_after_success() {
        // B6: once cleanup succeeds, a repeated explicit cleanup (or a later Drop) is
        // a no-op — it never re-signals the group or re-reaps the leader.
        let calls = Arc::new(Mutex::new(Vec::new()));
        let leader = FakeLeader {
            poll: ExitObservation::Exited,
            ..FakeLeader::new(Arc::clone(&calls), Some(7))
        };
        let mut managed = ManagedChild::new(Box::new(leader));
        assert_eq!(managed.terminate_and_reap().unwrap(), 7);
        assert_eq!(managed.terminate_and_reap().unwrap(), 7);
        assert_eq!(managed.terminate_and_reap().unwrap(), 7);
        // The reaped code is cached on the finalized outcome, so a later reader (a
        // caller that already reaped, or Drop) can recover it without re-waiting.
        assert_eq!(
            managed.exit_code(),
            Some(7),
            "the finalized outcome caches the reaped exit code"
        );

        let calls = calls.lock().unwrap();
        assert_eq!(calls.iter().filter(|c| **c == "signal_group").count(), 1);
        assert_eq!(calls.iter().filter(|c| **c == "reap").count(), 1);
        assert_eq!(
            calls.iter().filter(|c| **c == "kill_leader").count(),
            0,
            "a settled group signal never falls back to a direct kill"
        );
    }

    #[test]
    fn no_transcript_wait_error_uses_managed_cleanup() {
        // B6: a no-transcript coder whose reap fails still routes through managed
        // cleanup — the group is swept — and the error surfaces rather than leaking.
        let calls = Arc::new(Mutex::new(Vec::new()));
        let leader = FakeLeader {
            poll: ExitObservation::Exited,
            ..FakeLeader::new(Arc::clone(&calls), None)
        };
        let mut supervisor = CoderSupervisor::with_leader(Box::new(leader));
        let result = supervisor.wait_no_pump();
        assert!(result.is_err(), "a failed reap surfaces as an error");
        assert!(
            calls.lock().unwrap().iter().any(|c| *c == "signal_group"),
            "managed cleanup swept the group despite the reap failure"
        );
    }

    #[test]
    fn double_termination_failure_is_cached_not_retried() {
        // B6: when both the group signal and the direct kill fail, cleanup returns a
        // NotTerminated failure without blocking on a reap, and — because cleanup is
        // one-shot — a later call returns the cached failure verbatim without
        // re-signaling a PID that may by then be recycled.
        let calls = Arc::new(Mutex::new(Vec::new()));
        let leader = FakeLeader {
            group_settles: false,
            kill_succeeds: false,
            ..FakeLeader::new(Arc::clone(&calls), Some(0))
        };
        let mut managed = ManagedChild::new(Box::new(leader));
        assert!(
            matches!(
                managed.terminate_and_reap(),
                Err(CleanupError::NotTerminated { .. })
            ),
            "an unterminable coder is a cleanup failure, not a clean reap"
        );
        assert!(matches!(
            managed.terminate_and_reap(),
            Err(CleanupError::NotTerminated { .. })
        ));
        let calls = calls.lock().unwrap();
        assert_eq!(
            calls.iter().filter(|c| **c == "signal_group").count(),
            1,
            "the failed termination is cached, never re-signaled"
        );
        assert_eq!(
            calls.iter().filter(|c| **c == "reap").count(),
            0,
            "an unterminated coder never blocks on a reap"
        );
    }

    #[test]
    fn reap_failure_is_cached_not_retried() {
        // B6: a failed reap is cached, not retried — a later call returns the cached
        // NotReaped failure without a second reap that, on a recycled PID, could reap
        // an unrelated process. One-shot cleanup is the safety contract.
        let calls = Arc::new(Mutex::new(Vec::new()));
        let mut leader = FakeLeader::new(Arc::clone(&calls), None);
        leader.reaps = std::collections::VecDeque::from([None, Some(5)]);
        let mut managed = ManagedChild::new(Box::new(leader));
        assert!(matches!(
            managed.terminate_and_reap(),
            Err(CleanupError::NotReaped { .. })
        ));
        assert!(
            matches!(
                managed.terminate_and_reap(),
                Err(CleanupError::NotReaped { .. })
            ),
            "the failed reap is cached, not retried into a second reap"
        );
        let calls = calls.lock().unwrap();
        assert_eq!(
            calls.iter().filter(|c| **c == "signal_group").count(),
            1,
            "the settled group is not re-signaled"
        );
        assert_eq!(
            calls.iter().filter(|c| **c == "reap").count(),
            1,
            "the leader is never reaped a second time"
        );
    }

    #[test]
    fn identity_lost_leader_is_never_signaled_and_stays_unswept() {
        // B5/B6: an ECHILD identity loss must never signal, kill, or reap the
        // possibly-recycled PID, and it must NOT be recorded as swept — the group and
        // its descendants are not proven gone, so `group_swept()` stays false and a
        // pump join is never attempted on Drop. The result is cached one-shot.
        let calls = Arc::new(Mutex::new(Vec::new()));
        let leader = FakeLeader {
            poll: ExitObservation::IdentityLost,
            ..FakeLeader::new(Arc::clone(&calls), Some(0))
        };
        let mut managed = ManagedChild::new(Box::new(leader));
        assert!(matches!(
            managed.terminate_and_reap(),
            Err(CleanupError::IdentityLost { .. })
        ));
        assert!(
            !managed.group_swept(),
            "identity loss does not prove the group swept"
        );
        // A repeat returns the cached identity loss without re-observing or touching
        // the recycled PID.
        assert!(matches!(
            managed.terminate_and_reap(),
            Err(CleanupError::IdentityLost { .. })
        ));
        let calls = calls.lock().unwrap();
        assert_eq!(
            calls.iter().filter(|c| **c == "poll_exit").count(),
            1,
            "the leader is observed once, then the identity loss is cached"
        );
        assert_eq!(
            calls.iter().filter(|c| **c == "signal_group").count(),
            0,
            "a recycled PID/PGID is never signaled"
        );
        assert_eq!(calls.iter().filter(|c| **c == "kill_leader").count(), 0);
        assert_eq!(
            calls.iter().filter(|c| **c == "reap").count(),
            0,
            "a recycled PID is never reaped"
        );
    }

    #[cfg(unix)]
    #[test]
    fn drop_with_unswept_group_does_not_block_joining_a_live_pump() {
        // B5/B6: this pins the Drop *wall-clock* contract — when the group sweep is
        // unconfirmed (`group_swept()` is false, here via an ECHILD identity loss),
        // Drop detaches the still-running pump instead of joining it, returning
        // within a deadline rather than hanging forever on a pump that never reaches
        // EOF. The pipe here is held open by an independent process standing in for a
        // surviving descendant — this test isolates the non-join timing, NOT real
        // process-group descendant sweeping, which `no_transcript_sweeps_real_descendants`
        // and `real_leader_is_reaped_exactly_once_and_leaves_no_zombie` cover at the
        // OS boundary.
        let dir = tempfile::tempdir().unwrap();
        let transcript = dir.path().join("transcript.jsonl");
        let status = crate::transcript_pump::status_path_for(&transcript);

        // An independent process that holds the pump's read source open without ever
        // writing to it or closing it, so the pump thread blocks on read — a stand-in
        // for a surviving descendant that keeps the transcript pipe open.
        let mut writer = Command::new("/bin/sh");
        writer.arg("-c").arg("sleep 30").stdout(Stdio::piped());
        let mut writer_child = writer.spawn().unwrap();
        let source = writer_child.stdout.take().unwrap();

        let pump = crate::transcript_pump::spawn_pump(
            source,
            transcript.clone(),
            Some(status.clone()),
            crate::transcript_pump::console_preview_sink(),
            crate::transcript_pump::TranscriptPumpConfig::default(),
        )
        .unwrap();

        // A leader whose identity was already lost (ECHILD): the group is never
        // verifiably swept, so `group_swept()` stays false and Drop must not join.
        let calls = Arc::new(Mutex::new(Vec::new()));
        let leader = FakeLeader {
            poll: ExitObservation::IdentityLost,
            ..FakeLeader::new(Arc::clone(&calls), Some(0))
        };
        let mut supervisor = CoderSupervisor::with_leader(Box::new(leader));
        supervisor.attach_pump(pump);

        let started = Instant::now();
        drop(supervisor);
        let elapsed = started.elapsed();
        assert!(
            elapsed < Duration::from_secs(5),
            "Drop must not block joining a pump whose descendant still holds stdout; took {elapsed:?}"
        );

        // Release the still-blocked pump reader so its detached thread can exit.
        let _ = writer_child.kill();
        let _ = writer_child.wait();
    }

    #[test]
    fn cleanup_failure_composes_with_primary_pump_error() {
        // B6/B7: a primary pump error is preserved as the cause while a cleanup
        // failure is attached as context — cleanup never masks the primary fault.
        let err = compose_pump_error(
            crate::transcript_pump::TranscriptPumpError::new("primary pump fault"),
            Err(CleanupError::NotReaped {
                id: 99,
                source: ProcessOpError::from_io(
                    "reap leader",
                    &std::io::Error::from_raw_os_error(libc::ECHILD),
                ),
            }),
        );
        assert!(
            err.downcast_ref::<crate::transcript_pump::TranscriptPumpError>()
                .is_some(),
            "the primary pump fault is preserved as the cause: {err:#}"
        );
        // The typed cleanup failure is attached as downcastable context, not a
        // flattened string, so both faults stay structurally recoverable.
        assert!(
            err.downcast_ref::<CleanupError>().is_some(),
            "the cleanup failure is attached as typed context: {err:#}"
        );
        assert!(
            format!("{err:#}").contains("could not be reaped"),
            "the cleanup failure is attached as context: {err:#}"
        );
    }

    #[test]
    fn process_op_error_retains_structured_kind_and_message() {
        // B5/B6: a failed process operation retains an OWNED structured payload — the
        // operation, the OS ErrorKind, the errno, and the original message — that is
        // recoverable structurally (not by parsing rendered text) and rendered in the
        // durable diagnostic.
        let err = ProcessOpError::from_io(
            "kill process group",
            &std::io::Error::from_raw_os_error(libc::EPERM),
        );
        assert_eq!(
            err.kind(),
            Some(std::io::ErrorKind::PermissionDenied),
            "the structured OS ErrorKind is retained, not reconstructed from errno"
        );
        let message = err.message().expect("the original OS message is retained");
        assert!(
            message.contains("os error"),
            "the retained message is the original OS message: {message}"
        );

        let rendered = err.to_string();
        assert!(
            rendered.contains("kill process group"),
            "the failed operation is named: {rendered}"
        );
        assert!(
            rendered.contains(message),
            "the durable diagnostic renders the retained OS message: {rendered}"
        );
        assert!(
            rendered.contains("PermissionDenied"),
            "the durable diagnostic renders the structured ErrorKind category: {rendered}"
        );
    }

    #[test]
    fn unconfirmed_group_sweep_recovers_structured_kind_and_message() {
        // B5/B6: the retained sweep diagnostic exposes the structured payload of a
        // failed group signal, so a caller recovers the OS ErrorKind and message from
        // the returned/durable diagnostic — not only from the rendered CleanupError.
        let calls = Arc::new(Mutex::new(Vec::new()));
        let leader = FakeLeader {
            group_settles: false,
            ..FakeLeader::new(Arc::clone(&calls), Some(0))
        };
        let mut managed = ManagedChild::new(Box::new(leader));
        assert_eq!(managed.terminate_and_reap().unwrap(), 0);

        let sweep = managed
            .unconfirmed_group_sweep()
            .expect("the unconfirmed group sweep retains its structured OS error");
        assert_eq!(
            sweep.kind(),
            Some(std::io::ErrorKind::PermissionDenied),
            "the ErrorKind is recoverable from the returned diagnostic"
        );
        assert!(
            sweep.message().is_some_and(|m| m.contains("os error")),
            "the original OS message is recoverable from the returned diagnostic: {sweep}"
        );
    }

    #[test]
    fn setup_error_is_finalized_through_managed_cleanup() {
        // B6: a stdout/pump SETUP failure is finalized through the managed child — the
        // group is swept and the leader reaped now — and the typed setup error is
        // composed with any structured cleanup failure rather than left to Drop or
        // discarded.
        let calls = Arc::new(Mutex::new(Vec::new()));
        let leader = FakeLeader {
            group_settles: false,
            kill_succeeds: false,
            ..FakeLeader::new(Arc::clone(&calls), Some(0))
        };
        let mut supervisor = CoderSupervisor::with_leader(Box::new(leader));
        let err =
            supervisor.finalize_setup_error(anyhow::anyhow!("coder stdout was not piped"));

        assert!(
            format!("{err:#}").contains("coder stdout was not piped"),
            "the setup error is preserved as the primary cause: {err:#}"
        );
        assert!(
            err.downcast_ref::<CleanupError>().is_some(),
            "the cleanup failure is composed in as typed context: {err:#}"
        );
        assert!(
            calls.lock().unwrap().iter().any(|c| *c == "signal_group"),
            "setup finalization swept the group rather than leaving it to Drop"
        );
    }

    #[cfg(unix)]
    #[test]
    fn real_leader_is_reaped_exactly_once_and_leaves_no_zombie() {
        // B5/B6: a real leader is observed then reaped exactly once. After cleanup the
        // kernel holds no zombie for its PID (a raw WNOHANG waitid returns ECHILD), and
        // a repeated cleanup returns the cached exit code without a second reap that
        // would itself ECHILD-fail. This is the zombie-sensitive exactly-once evidence
        // a deterministic FakeLeader seam cannot provide.
        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c").arg("exit 7").stdout(Stdio::null());
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }
        let child = cmd.spawn().expect("spawn real leader");
        let pid = child.id();
        let mut managed = ManagedChild::new(Box::new(SystemLeader::new(child, pid)));

        // Wait for the leader's own exit (without reaping), then sweep and reap once;
        // the natural exit code survives the group SIGKILL to the already-exited leader.
        let code = managed
            .await_exit_then_cleanup()
            .expect("the leader is reaped");
        assert_eq!(code, 7, "the natural exit code is preserved through cleanup");

        // Zombie-sensitive: no waitable child remains for this PID. A missed reap
        // would leave a waitable zombie; a double reap is impossible because the code
        // is cached and the leader is never waited twice.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let rc = unsafe {
            libc::waitid(
                libc::P_PID,
                pid as libc::id_t,
                &mut info,
                libc::WEXITED | libc::WNOHANG,
            )
        };
        assert_eq!(rc, -1, "no waitable child remains for the reaped leader");
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ECHILD),
            "the leader left no zombie: it was reaped exactly once"
        );

        // A repeated cleanup returns the cached code without a second reap.
        assert_eq!(
            managed.terminate_and_reap().expect("cached exit code"),
            7,
            "repeated cleanup returns the cached code without reaping again"
        );
    }

    #[cfg(unix)]
    #[test]
    fn no_transcript_sweeps_real_descendants() {
        // B6: a no-transcript coder that backgrounds a descendant routes through the
        // managed child, so the descendant is swept after the leader exits rather
        // than leaking. Uses a real process group, no pump.
        let dir = tempfile::tempdir().unwrap();
        let pid_path = dir.path().join("descendant.pid");

        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c")
            .arg("sleep 5 & echo $! > descendant.pid; exit 0")
            .current_dir(dir.path())
            .stdout(Stdio::null());

        let exit = run_with_transcript(
            cmd,
            None,
            &crate::transcript_pump::TranscriptPumpConfig::default(),
        )
        .unwrap();
        assert_eq!(exit, 0);

        let pid = std::fs::read_to_string(&pid_path).unwrap();
        // Give the group signal a moment to land and the descendant to be reaped.
        std::thread::sleep(Duration::from_millis(200));
        let alive = Command::new("/bin/kill")
            .args(["-0", pid.trim()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        assert!(
            !alive,
            "the no-transcript coder's backgrounded descendant must be swept"
        );
    }

    #[cfg(unix)]
    #[test]
    fn natural_exit_sweeps_group_before_reaping() {
        // B5: the leader exits naturally and the pump reaches EOF, but a same-group
        // descendant (with stdout redirected away, so it does not hold the pipe) is
        // still swept before the boundary returns.
        let dir = tempfile::tempdir().unwrap();
        let transcript = dir.path().join("transcript.jsonl");
        let pid_path = dir.path().join("descendant.pid");

        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c")
            .arg(
                "( sleep 5 >/dev/null 2>&1 ) & echo $! > descendant.pid; \
                 printf '{\"type\":\"leader\"}\\n'; exit 0",
            )
            .current_dir(dir.path());

        let started = Instant::now();
        let exit = run_with_transcript(
            cmd,
            Some(&transcript),
            &crate::transcript_pump::TranscriptPumpConfig::default(),
        )
        .unwrap();
        let elapsed = started.elapsed();

        assert_eq!(exit, 0, "the leader's natural exit code is returned");
        assert!(
            elapsed < Duration::from_secs(4),
            "must not wait out the descendant's sleep; took {elapsed:?}"
        );

        let pid = std::fs::read_to_string(&pid_path).unwrap();
        let alive = Command::new("/bin/kill")
            .args(["-0", pid.trim()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        assert!(
            !alive,
            "the same-group descendant must be swept before the leader is reaped"
        );
    }

    #[cfg(unix)]
    #[test]
    fn pump_eof_does_not_end_child_supervision() {
        // B5: the leader closes its own stdout and keeps working. The pump reaches
        // EOF immediately, but EOF is NOT leader completion: supervision must neither
        // return nor terminate the healthy leader — it must let the leader finish its
        // work and preserve its distinctive natural exit code.
        let dir = tempfile::tempdir().unwrap();
        let transcript = dir.path().join("transcript.jsonl");
        let d = dir.path().display();

        // Close stdout up front, then do work and exit with a distinctive code.
        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c").arg(format!(
            "exec 1>&-; sleep 0.3; : > {d}/leader-finished; exit 42"
        ));

        let started = Instant::now();
        let exit = run_with_transcript(
            cmd,
            Some(&transcript),
            &crate::transcript_pump::TranscriptPumpConfig::default(),
        )
        .unwrap();
        let elapsed = started.elapsed();

        assert_eq!(
            exit, 42,
            "the healthy leader's natural exit code must be preserved, not a kill"
        );
        assert!(
            dir.path().join("leader-finished").exists(),
            "the leader must run to completion; EOF must not terminate it early"
        );
        assert!(
            elapsed >= Duration::from_millis(250),
            "supervision must wait for the leader's own exit, not return at EOF; took {elapsed:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn descendant_that_closes_stdout_is_still_terminated() {
        // B5: a backgrounded descendant closes the inherited stdout (so the pump
        // reaches EOF) but keeps running. It must be swept before the boundary
        // returns, not left to run its delayed side effect.
        let dir = tempfile::tempdir().unwrap();
        let transcript = dir.path().join("transcript.jsonl");
        let d = dir.path().display();

        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c").arg(format!(
            "( exec 1>&-; sleep 5; : > {d}/descendant-ran ) & echo $! > {d}/descendant.pid; \
             printf '{{\"type\":\"leader\"}}\\n'; exit 0"
        ));

        let started = Instant::now();
        let exit = run_with_transcript(
            cmd,
            Some(&transcript),
            &crate::transcript_pump::TranscriptPumpConfig::default(),
        )
        .unwrap();
        let elapsed = started.elapsed();

        assert_eq!(exit, 0);
        assert!(
            elapsed < Duration::from_secs(4),
            "must not wait out the descendant's sleep; took {elapsed:?}"
        );

        let pid = std::fs::read_to_string(dir.path().join("descendant.pid")).unwrap();
        let alive = Command::new("/bin/kill")
            .args(["-0", pid.trim()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        assert!(
            !alive,
            "the descendant that closed stdout must still be swept"
        );
        std::thread::sleep(Duration::from_millis(700));
        assert!(
            !dir.path().join("descendant-ran").exists(),
            "the swept descendant must not run its delayed side effect"
        );
    }

    /// A pump reader that errors on its first read, modelling stdout failing while
    /// the coder is still alive.
    struct ErrorOnFirstRead;

    impl std::io::Read for ErrorOnFirstRead {
        fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("simulated stdout read failure"))
        }
    }

    /// A pump reader that reaches EOF immediately, so capture succeeds and the
    /// coordinator settles a Complete status.
    struct EofReader;

    impl std::io::Read for EofReader {
        fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
            Ok(0)
        }
    }

    /// A status store that persists Running immediately but announces when it enters
    /// a terminal write and then blocks it until the test releases the gate. The
    /// handshake proves the store is blocked inside terminal persistence before the
    /// test observes the coder being reaped. It can also fail the Complete write, so
    /// the blocked write is the Failed fallback.
    struct GateTerminalStore {
        entered: std::sync::mpsc::Sender<crate::transcript_pump::PumpState>,
        gate: std::sync::mpsc::Receiver<()>,
        fail_complete: bool,
    }

    impl crate::transcript_pump::StatusStore for GateTerminalStore {
        fn write(
            &mut self,
            status: &crate::transcript_pump::PumpStatus,
        ) -> Result<(), String> {
            use crate::transcript_pump::PumpState;
            match status.state {
                PumpState::Running => Ok(()),
                PumpState::Complete if self.fail_complete => {
                    Err("simulated complete write failure".to_string())
                }
                _ => {
                    // Announce entry into the terminal write, then block on the gate.
                    let _ = self.entered.send(status.state);
                    let _ = self.gate.recv();
                    Ok(())
                }
            }
        }
    }

    /// Drive supervision against a coder and a gated terminal store, proving the
    /// coder is terminated and reaped while the store is blocked inside terminal
    /// persistence — the first fault reaches supervision before settlement unblocks.
    #[cfg(unix)]
    fn assert_first_fault_reaps_before_settlement<R>(reader: R, fail_complete: bool, coder: &str)
    where
        R: std::io::Read + Send + 'static,
    {
        let dir = tempfile::tempdir().unwrap();
        let transcript = dir.path().join("transcript.jsonl");

        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c").arg(coder);
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }
        let child = cmd.spawn().unwrap();
        let child_id = child.id();

        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (gate_tx, gate_rx) = std::sync::mpsc::channel::<()>();
        let store = GateTerminalStore {
            entered: entered_tx,
            gate: gate_rx,
            fail_complete,
        };
        let pump = crate::transcript_pump::spawn_pump_with_store(
            reader,
            transcript.clone(),
            Some(Box::new(store)),
            crate::transcript_pump::console_preview_sink(),
            crate::transcript_pump::TranscriptPumpConfig::default(),
        )
        .unwrap();

        let mut supervisor = CoderSupervisor::new(child, child_id);
        supervisor.attach_pump(pump);
        let handle = std::thread::spawn(move || supervisor.supervise());

        // Handshake: the store has entered terminal persistence and is now blocked.
        let entered = entered_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("the store must enter a blocked terminal write");
        assert_ne!(entered, crate::transcript_pump::PumpState::Running);

        // While it is blocked, the coder must be terminated and reaped.
        let is_alive = |pid: u32| {
            Command::new("/bin/kill")
                .args(["-0", &pid.to_string()])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
        };
        let deadline = Instant::now() + Duration::from_secs(3);
        while is_alive(child_id) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            !is_alive(child_id),
            "the coder must be reaped while the terminal write is still blocked"
        );

        // Only now release the blocked terminal write; supervision returns typed.
        let _ = gate_tx.send(());
        let result = handle.join().unwrap();
        let err = result.expect_err("the first fault must surface as a typed failure");
        assert!(
            err.downcast_ref::<crate::transcript_pump::TranscriptPumpError>()
                .is_some(),
            "the failure must be a typed transcript-pump error: {err}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn first_fault_reaches_supervisor_before_terminal_status_unblocks() {
        // B2: a capture read fault publishes the first fault before terminal
        // settlement; supervision reaps the still-live coder while the terminal
        // (Failed) write is blocked.
        assert_first_fault_reaps_before_settlement(ErrorOnFirstRead, false, "sleep 5");
    }

    #[cfg(unix)]
    #[test]
    fn capture_panic_first_fault_reaches_supervisor_before_settlement() {
        // B2: a capture PANIC (not just a returned error) is caught, its first fault
        // published before settlement, and supervision reaps the still-live coder
        // while the terminal write is blocked.
        assert_first_fault_reaps_before_settlement(PanicOnRead, false, "sleep 5");
    }

    #[cfg(unix)]
    #[test]
    fn complete_write_failure_first_fault_reaches_supervisor_before_fallback() {
        // Re-audit regression: capture succeeds but the Complete write fails and the
        // Failed fallback blocks. The Complete failure must publish the first fault
        // BEFORE the blocked fallback, so supervision reaps the still-running coder
        // without waiting for the fallback to unblock.
        assert_first_fault_reaps_before_settlement(EofReader, true, "sleep 5");
    }

    #[test]
    fn supervise_report_carries_an_unconfirmed_group_sweep() {
        // B5/B6: the supervisor SURFACES a per-launch report, so a reaped-but-unswept
        // group's diagnostic reaches the role artifact boundary instead of being
        // dropped with the ManagedChild. A run whose group signal fails (EPERM) but
        // whose direct kill and reap succeed is a successful run with an unconfirmed
        // sweep, and the report carries the structured OS error.
        let calls = Arc::new(Mutex::new(Vec::new()));
        let leader = FakeLeader {
            group_settles: false,
            poll: ExitObservation::Exited,
            ..FakeLeader::new(Arc::clone(&calls), Some(0))
        };
        let mut supervisor = CoderSupervisor::with_leader(Box::new(leader));
        let exit = supervisor
            .wait_no_pump()
            .expect("the reaped-but-unswept run still succeeds");
        assert_eq!(exit, 0);
        let report = supervisor.report();
        assert!(
            !report.is_clean(),
            "an unconfirmed group sweep is a non-clean report worth persisting"
        );
        let diag = report
            .group_sweep_unconfirmed
            .expect("the sweep diagnostic is surfaced from the supervisor");
        assert_eq!(
            diag.errno,
            Some(libc::EPERM),
            "the EPERM errno is retained structurally, not only in rendered text"
        );
        assert!(
            diag.message.is_some(),
            "the original OS message is retained on the durable diagnostic"
        );
    }

    #[test]
    fn supervise_report_is_clean_when_the_group_is_swept() {
        // A verifiably swept group leaves nothing to persist: the report is clean.
        let calls = Arc::new(Mutex::new(Vec::new()));
        let leader = FakeLeader {
            poll: ExitObservation::Exited,
            ..FakeLeader::new(Arc::clone(&calls), Some(0))
        };
        let mut supervisor = CoderSupervisor::with_leader(Box::new(leader));
        supervisor.wait_no_pump().unwrap();
        assert!(
            supervisor.report().is_clean(),
            "a swept group produces no supervision diagnostic"
        );
    }

    #[test]
    fn finish_supervised_run_persists_a_non_clean_report() {
        // The role artifact boundary persists a non-clean report as coder-supervision.json.
        let dir = tempfile::tempdir().unwrap();
        let report = CoderSupervisionReport {
            group_sweep_unconfirmed: Some(ProcessOpDiagnostic {
                operation: "fake group signal".to_string(),
                kind: Some("PermissionDenied".to_string()),
                errno: Some(libc::EPERM),
                message: Some("Operation not permitted".to_string()),
            }),
        };
        let completion = CoderRunCompletion {
            terminal: Ok(0),
            report,
        };
        let code = finish_supervised_coder_run(completion, dir.path()).unwrap();
        assert_eq!(code, 0, "the terminal outcome passes through unchanged");
        let path = dir.path().join(CODER_SUPERVISION_SIDECAR);
        assert!(path.exists(), "a non-clean report is persisted as a sidecar");
        let persisted: CoderSupervisionReport =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            persisted.group_sweep_unconfirmed.unwrap().errno,
            Some(libc::EPERM),
            "the persisted sidecar retains the structured OS error"
        );
    }

    #[test]
    fn finish_supervised_run_skips_a_clean_report() {
        // A clean report has nothing to persist, so no sidecar is written.
        let dir = tempfile::tempdir().unwrap();
        finish_supervised_coder_run(CoderRunCompletion::terminal_only(Ok(0)), dir.path()).unwrap();
        assert!(
            !dir.path().join(CODER_SUPERVISION_SIDECAR).exists(),
            "a clean supervision report writes no sidecar"
        );
    }

    #[test]
    fn finish_supervised_run_composes_a_sidecar_obstruction_as_typed_secondary() {
        // A sidecar obstruction (the artifact dir path is a FILE) is a typed
        // non-retryable error. When the run also failed, the typed run primary is
        // preserved as the base of the chain and the sidecar error rides along as a
        // downcastable secondary — neither is flattened, and the coder is never relaunched.
        let dir = tempfile::tempdir().unwrap();
        let obstruction = dir.path().join("not-a-dir");
        std::fs::write(&obstruction, b"x").unwrap();
        let report = CoderSupervisionReport {
            group_sweep_unconfirmed: Some(ProcessOpDiagnostic {
                operation: "s".to_string(),
                kind: None,
                errno: Some(libc::EPERM),
                message: None,
            }),
        };

        // A successful run whose sidecar write fails returns the typed sidecar error.
        let ok = CoderRunCompletion {
            terminal: Ok(0),
            report: report.clone(),
        };
        let err = finish_supervised_coder_run(ok, &obstruction)
            .expect_err("an obstructed sidecar surfaces an error");
        assert!(
            err.downcast_ref::<SupervisionSidecarError>().is_some(),
            "the sidecar obstruction is a downcastable typed error: {err:#}"
        );

        // A failed run whose sidecar ALSO fails preserves the typed run primary and
        // composes the sidecar error as a downcastable secondary.
        let pump = crate::transcript_pump::TranscriptPumpError::new("pump fault".to_string());
        let both = CoderRunCompletion {
            terminal: Err(anyhow::Error::new(pump)),
            report,
        };
        let composed = finish_supervised_coder_run(both, &obstruction)
            .expect_err("the composed failure still surfaces");
        assert!(
            composed
                .downcast_ref::<crate::transcript_pump::TranscriptPumpError>()
                .is_some(),
            "the typed run primary is preserved: {composed:#}"
        );
        assert!(
            composed.downcast_ref::<SupervisionSidecarError>().is_some(),
            "the sidecar error rides as a downcastable secondary: {composed:#}"
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

        assert_eq!(run_with_transcript(command, None, &crate::transcript_pump::TranscriptPumpConfig::default()).unwrap(), 0);
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
    fn capture_config_is_stable_across_retry_phases() {
        // B8: one immutable resolved config is retained across a launch's auth-refresh
        // retry phase — the same distinctive value flows into every attempt, never
        // re-resolved (which a concurrent launch could once perturb).
        let dir = tempfile::tempdir().unwrap();
        let transcript = dir.path().join("transcript.jsonl");
        let counter = dir.path().join("counter");
        let capture = TranscriptCapture::with_config(
            &transcript,
            crate::transcript_pump::TranscriptPumpConfig {
                console_preview_limit: 4321,
                ..Default::default()
            },
        );

        let seen: Arc<Mutex<Vec<crate::transcript_pump::TranscriptPumpConfig>>> =
            Arc::new(Mutex::new(Vec::new()));
        let seen_run = Arc::clone(&seen);
        // 401 on the first call, success on the second.
        let script = make_401_script(&counter, Some(2));
        let run_fn = move |cmd: Command,
                           tf: Option<&Path>,
                           cfg: &crate::transcript_pump::TranscriptPumpConfig|
              -> Result<i32> {
            seen_run.lock().unwrap().push(cfg.clone());
            run_with_transcript(cmd, tf, cfg)
        };

        let result = run_with_transcript_retrying_using(
            move || {
                let mut cmd = Command::new("/bin/sh");
                cmd.arg("-c").arg(&script);
                cmd
            },
            Some(&transcript),
            &capture.config,
            &|_, _| {},
            &|| {},
            &run_fn,
        );
        assert_eq!(result.unwrap(), 0, "the retry recovers");

        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 2, "the original attempt plus one auth-refresh retry");
        assert_eq!(
            seen[0], seen[1],
            "the same resolved config flows into every retry phase"
        );
        assert_eq!(
            seen[0].console_preview_limit, 4321,
            "the distinctive resolved config is retained, not re-resolved to defaults"
        );
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
            &crate::transcript_pump::TranscriptPumpConfig::default(),
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
            &crate::transcript_pump::TranscriptPumpConfig::default(),
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
            &crate::transcript_pump::TranscriptPumpConfig::default(),
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
            &crate::transcript_pump::TranscriptPumpConfig::default(),
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
            &crate::transcript_pump::TranscriptPumpConfig::default(),
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
            &crate::transcript_pump::TranscriptPumpConfig::default(),
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
            &crate::transcript_pump::TranscriptPumpConfig::default(),
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
            &crate::transcript_pump::TranscriptPumpConfig::default(),
            &|_, _| {},
            &real_credential_refresh,
        );

        assert_eq!(result.unwrap(), 0, "should succeed after real refresh");
    }
}
