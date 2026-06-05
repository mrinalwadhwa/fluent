use anyhow::{bail, Result};
use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
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

/// Which coding agent the factory should launch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoderKind {
    Claude,
    Codex,
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

    pub fn boxed(&self, sandbox_profile: Option<String>) -> Box<dyn Coder> {
        match self {
            Self::Claude => match sandbox_profile {
                Some(profile) => Box::new(SandboxedClaudeCode {
                    sandbox_profile: Some(profile),
                }),
                None => Box::new(BareClaudeCode),
            },
            Self::Codex => Box::new(CodexCode { sandbox_profile }),
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
        transcript_file: Option<&Path>,
    ) -> Result<i32>;

    /// Launch an interactive session (no -p flag).
    fn run_interactive(
        &self,
        system_prompt: &str,
        working_dir: &Path,
        extra_args: &[String],
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
        transcript_file: Option<&Path>,
    ) -> Result<i32> {
        let mut cmd = self.build_command(working_dir);
        if transcript_file.is_some() {
            cmd.args(["--verbose", "--output-format", "stream-json"]);
        }
        cmd.args(["--append-system-prompt", system_prompt]);
        cmd.args(["-p", prompt]);
        cmd.args(extra_args);

        run_with_transcript(cmd, transcript_file)
    }

    fn run_interactive(
        &self,
        system_prompt: &str,
        working_dir: &Path,
        extra_args: &[String],
    ) -> Result<i32> {
        let mut cmd = self.build_command(working_dir);
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
        transcript_file: Option<&Path>,
    ) -> Result<i32> {
        let mut cmd = Command::new("claude");
        cmd.current_dir(working_dir);
        cmd.args(["--dangerously-skip-permissions"]);
        cmd.args(["--model", &claude_model()]);
        if transcript_file.is_some() {
            cmd.args(["--verbose", "--output-format", "stream-json"]);
        }
        cmd.args(["--append-system-prompt", system_prompt]);
        cmd.args(["-p", prompt]);
        cmd.args(extra_args);

        run_with_transcript(cmd, transcript_file)
    }

    fn run_interactive(
        &self,
        system_prompt: &str,
        working_dir: &Path,
        extra_args: &[String],
    ) -> Result<i32> {
        let mut cmd = Command::new("claude");
        cmd.current_dir(working_dir);
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
        transcript_file: Option<&Path>,
    ) -> Result<i32> {
        let mut cmd = self.build_command(working_dir, true);
        if transcript_file.is_some() {
            cmd.arg("--json");
        }
        let combined_prompt = format!("{system_prompt}\n\n---\n\n{prompt}");
        cmd.arg(combined_prompt);
        cmd.args(extra_args);

        run_with_transcript(cmd, transcript_file)
    }

    fn run_interactive(
        &self,
        system_prompt: &str,
        working_dir: &Path,
        extra_args: &[String],
    ) -> Result<i32> {
        let mut cmd = self.build_command(working_dir, false);
        cmd.arg(system_prompt);
        cmd.args(extra_args);

        let status = cmd.status()?;
        Ok(status.code().unwrap_or(1))
    }
}

impl CodexCode {
    fn build_command(&self, working_dir: &Path, exec_mode: bool) -> Command {
        let mut cmd = if let Some(ref profile) = self.sandbox_profile {
            let mut cmd = Command::new("sandbox-exec");
            cmd.args(["-f", profile]);
            cmd.arg("codex");
            cmd
        } else {
            Command::new("codex")
        };

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
    ) -> Result<i32> {
        Ok(0)
    }
}
