use anyhow::Result;
use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Command, Stdio};

const DEFAULT_MODEL: &str = "claude-opus-4-6";

fn model() -> String {
    std::env::var("FACTORY_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_string())
}

/// Trait abstracting the coding agent (currently Claude Code).
pub trait Coder {
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
            cmd.args(["--model", &model()]);
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
        cmd.args(["--model", &model()]);
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
    pub call_count: std::cell::Cell<u32>,
}

#[cfg(test)]
impl<F> Coder for MockCoder<F>
where
    F: Fn(&str, u32) -> (i32, Option<String>),
{
    fn run(
        &self,
        prompt: &str,
        _system_prompt: &str,
        _working_dir: &Path,
        _extra_args: &[String],
        _transcript_file: Option<&Path>,
    ) -> Result<i32> {
        let n = self.call_count.get() + 1;
        self.call_count.set(n);
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
