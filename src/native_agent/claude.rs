//! Correlate native Claude hooks with one managed Task invocation.

use anyhow::{Result, bail, ensure};
use serde_json::Value;
use std::collections::HashMap;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use crate::coder::{CoderSession, SandboxedClaudeCode};
use crate::terminal_sessions::{TerminalIdentity, TerminalSession};

use super::{Completion, NativeAdmission};

pub struct ClaudeHookGate {
    generation: String,
    expected_session: Option<String>,
    session_id: Option<String>,
    admission: NativeAdmission,
    prompts: HashMap<String, u64>,
    last_sequence: u64,
    settled: bool,
    transcript_path: Option<PathBuf>,
}

impl ClaudeHookGate {
    pub fn new(generation: impl Into<String>, expected_session: Option<&str>) -> Self {
        Self {
            generation: generation.into(),
            expected_session: expected_session.map(str::to_owned),
            session_id: None,
            admission: NativeAdmission::new(),
            prompts: HashMap::new(),
            last_sequence: 0,
            settled: false,
            transcript_path: None,
        }
    }

    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    pub fn last_sequence(&self) -> u64 {
        self.last_sequence
    }

    pub fn transcript_path(&self) -> Option<&Path> {
        self.transcript_path.as_deref()
    }

    pub fn observe(&mut self, generation: &str, event: &Value) -> Result<Completion> {
        ensure!(
            generation == self.generation,
            "Claude hook belongs to another Task generation"
        );
        let session_id = event["session_id"]
            .as_str()
            .filter(|id| !id.is_empty())
            .ok_or_else(|| anyhow::anyhow!("Claude hook has no session identity"))?;
        if let Some(expected) = &self.expected_session {
            ensure!(
                session_id == expected,
                "Claude hook belongs to another conversation"
            );
        }
        if let Some(assigned) = &self.session_id {
            ensure!(session_id == assigned, "Claude switched Task conversations");
        }
        if let Some(path) = event["transcript_path"].as_str() {
            let path = Path::new(path);
            ensure!(
                path.is_absolute(),
                "Claude hook transcript path is not absolute"
            );
            if let Some(assigned) = &self.transcript_path {
                ensure!(path == assigned, "Claude switched Task transcripts");
            }
            self.transcript_path = Some(path.to_path_buf());
        }
        let kind = event["hook_event_name"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Claude hook has no event kind"))?;
        match kind {
            "SessionStart" => {
                let source = event["source"].as_str().unwrap_or_default();
                ensure!(
                    matches!(source, "startup" | "resume" | "compact"),
                    "Claude Task session changed through {source}"
                );
                self.session_id = Some(session_id.into());
            }
            "UserPromptSubmit" => {
                ensure!(
                    self.session_id.is_some(),
                    "Claude submitted input before SessionStart"
                );
                let prompt_id = event["prompt_id"]
                    .as_str()
                    .filter(|id| !id.is_empty())
                    .ok_or_else(|| anyhow::anyhow!("Claude input has no prompt identity"))?;
                ensure!(
                    !self.prompts.contains_key(prompt_id),
                    "Claude reused a prompt identity"
                );
                let sequence = self.admission.admit()?;
                self.prompts.insert(prompt_id.into(), sequence);
                self.last_sequence = sequence;
            }
            "Stop" => {
                ensure!(
                    self.session_id.is_some(),
                    "Claude stopped before SessionStart"
                );
                let prompt_id = event["prompt_id"]
                    .as_str()
                    .filter(|id| !id.is_empty())
                    .ok_or_else(|| anyhow::anyhow!("Claude Stop has no prompt identity"))?;
                let Some(&sequence) = self.prompts.get(prompt_id) else {
                    return Ok(Completion::Ignored);
                };
                if event["background_tasks"]
                    .as_array()
                    .is_some_and(|items| !items.is_empty())
                    || event["session_crons"]
                        .as_array()
                        .is_some_and(|items| !items.is_empty())
                {
                    return Ok(Completion::Ignored);
                }
                ensure!(
                    event["last_assistant_message"]
                        .as_str()
                        .is_some_and(|message| !message.trim().is_empty()),
                    "Claude Stop omitted its final assistant message"
                );
                if self.admission.settle_if_current(sequence)? {
                    self.settled = true;
                    return Ok(Completion::Accepted {
                        generation: self.generation.clone(),
                        turn_id: prompt_id.into(),
                        input_sequence: sequence,
                    });
                }
            }
            "StopFailure" => {
                self.admission.begin_settlement()?;
                self.settled = true;
                bail!("Claude turn failed: {}", event["error"]);
            }
            "SessionEnd" => {
                if !self.settled {
                    bail!("Claude session ended without a completed Task turn");
                }
            }
            "PreToolUse" | "PostToolUse" | "PostToolUseFailure" => {
                ensure!(
                    self.session_id.is_some(),
                    "Claude tool ran before SessionStart"
                );
            }
            _ => bail!("unexpected Claude native hook {kind}"),
        }
        Ok(Completion::Ignored)
    }
}

pub struct ClaudeNativeTask<'a> {
    pub identity: TerminalIdentity,
    pub worker_profile: &'a crate::os::SandboxProfile,
    pub workspace: &'a Path,
    pub private_home: &'a Path,
    pub model: &'a str,
    pub effort: &'a str,
    pub prompt: &'a str,
    pub system_prompt: &'a str,
    pub session: CoderSession<'a>,
    pub launch_env: &'a [(String, String)],
}

pub fn check_capability() -> Result<()> {
    let version = Command::new("claude").arg("--version").output()?;
    ensure!(
        version.status.success(),
        "native Claude version query failed"
    );
    let version = String::from_utf8_lossy(&version.stdout);
    ensure!(
        supports_blocking_hook_failure(&version),
        "native Claude requires Code 2.1.295 or later for fail-closed hook errors; installed: {}",
        version.trim()
    );
    let output = Command::new("claude").arg("--help").output()?;
    ensure!(
        output.status.success(),
        "native Claude capability query failed"
    );
    let help = String::from_utf8_lossy(&output.stdout);
    for flag in ["--settings", "--setting-sources", "--resume", "--effort"] {
        ensure!(help.contains(flag), "native Claude lacks {flag}");
    }
    ensure!(
        Command::new("tmux").arg("-V").status()?.success(),
        "native Claude requires tmux"
    );
    ensure!(
        Command::new("python3")
            .arg("--version")
            .output()?
            .status
            .success(),
        "native Claude requires Python hook transport"
    );
    Ok(())
}

pub fn supports_blocking_hook_failure(version: &str) -> bool {
    version
        .split_whitespace()
        .find_map(|word| {
            let numbers: Vec<_> = word
                .split('.')
                .take(3)
                .map(str::parse::<u32>)
                .collect::<std::result::Result<_, _>>()
                .ok()?;
            (numbers.len() == 3).then_some((numbers[0], numbers[1], numbers[2]))
        })
        .is_some_and(|version| version >= (2, 1, 295))
}

fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn command_line(command: &Command) -> Result<String> {
    let mut words =
        vec![quote(command.get_program().to_str().ok_or_else(|| {
            anyhow::anyhow!("Claude executable is not UTF-8")
        })?)];
    for arg in command.get_args() {
        words
            .push(quote(arg.to_str().ok_or_else(|| {
                anyhow::anyhow!("Claude argument is not UTF-8")
            })?));
    }
    Ok(words.join(" "))
}

/// Hook commands block the provider action when the owner cannot admit or
/// record it. Every event carries the terminal's owned generation.
fn install_hooks(home: &Path, socket: &Path, generation_file: &Path) -> Result<std::path::PathBuf> {
    let sender = home.join("fluent-native-hook.py");
    fs::write(
        &sender,
        r#"import json, socket, sys
try:
    event = json.load(sys.stdin)
    with open(sys.argv[2], encoding='utf-8') as source:
        generation = source.read().strip()
    with socket.socket(socket.AF_UNIX) as stream:
        stream.settimeout(8)
        stream.connect(sys.argv[1])
        stream.sendall((json.dumps({'generation': generation, 'event': event}) + '\n').encode())
        reply = stream.makefile('r', encoding='utf-8').readline()
    if reply.strip() != 'ok':
        sys.exit(2)
except Exception as error:
    print('Fluent native hook failed: ' + str(error), file=sys.stderr)
    sys.exit(2)
"#,
    )?;
    let command = format!(
        "python3 {} {} {}",
        quote(&sender.display().to_string()),
        quote(&socket.display().to_string()),
        quote(&generation_file.display().to_string())
    );
    let mut hooks = serde_json::Map::new();
    for event in [
        "SessionStart",
        "UserPromptSubmit",
        "PreToolUse",
        "PostToolUse",
        "PostToolUseFailure",
        "Stop",
        "StopFailure",
        "SessionEnd",
    ] {
        hooks.insert(event.into(), serde_json::json!([{"hooks":[{"type":"command","command":command,"timeout":10,"onFailure":"block"}]}]));
    }
    let settings = home.join("fluent-native-settings.json");
    fs::write(
        &settings,
        serde_json::to_vec(&serde_json::json!({"hooks":hooks}))?,
    )?;
    Ok(settings)
}

fn environment(task: &ClaudeNativeTask<'_>) -> Vec<(OsString, OsString)> {
    let mut values: Vec<_> = [
        "PATH",
        "USER",
        "LOGNAME",
        "SHELL",
        "LANG",
        "LC_ALL",
        "TERM",
        "ANTHROPIC_API_KEY",
        "CLAUDE_CODE_OAUTH_TOKEN",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "NO_PROXY",
        "SSL_CERT_FILE",
    ]
    .iter()
    .filter_map(|name| std::env::var_os(name).map(|value| (OsString::from(name), value)))
    .collect();
    values.push(("HOME".into(), task.private_home.as_os_str().to_owned()));
    values.push((
        "CLAUDE_CONFIG_DIR".into(),
        task.private_home.as_os_str().to_owned(),
    ));
    values.push(("PWD".into(), task.workspace.as_os_str().to_owned()));
    values.extend(
        task.launch_env
            .iter()
            .map(|(key, value)| (key.into(), value.into())),
    );
    values
}

/// Read one hook, validate it against the assigned invocation, durably append
/// its original payload, then acknowledge the provider's blocked action.
pub fn receive_hook(
    mut stream: UnixStream,
    gate: &mut ClaudeHookGate,
    generation: &str,
    evidence: &mut File,
) -> Result<Completion> {
    stream.set_read_timeout(Some(Duration::from_secs(8)))?;
    stream.set_write_timeout(Some(Duration::from_secs(8)))?;
    let mut line = String::new();
    BufReader::new(stream.try_clone()?)
        .take(1024 * 1024)
        .read_line(&mut line)?;
    ensure!(
        !line.is_empty() && line.len() < 1024 * 1024,
        "Claude hook event is empty or oversized"
    );
    let envelope: Value = serde_json::from_str(&line)?;
    let owned = envelope["generation"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("Claude hook has no generation"))?;
    let event = &envelope["event"];
    let completion = gate.observe(owned, event)?;
    ensure!(owned == generation, "Claude hook generation changed");
    serde_json::to_writer(
        &mut *evidence,
        &serde_json::json!({
            "format":"fluent-native-agent-event", "schema_version":1,
            "provider":"claude", "execution_generation":generation,
            "assigned_conversation_id":gate.session_id(),
            "conversation_id":event["session_id"],
            "turn_id":event["prompt_id"],
            "input_sequence":gate.last_sequence(), "event":event,
        }),
    )?;
    evidence.write_all(b"\n")?;
    evidence.sync_all()?;
    stream.write_all(b"ok\n")?;
    Ok(completion)
}

pub fn run(task: ClaudeNativeTask<'_>) -> Result<()> {
    ensure!(
        task.private_home.is_dir(),
        "native Claude private home is absent"
    );
    ensure!(
        !task.model.is_empty() && !task.effort.is_empty(),
        "native Claude model or effort is unresolved"
    );
    let sockets = tempfile::Builder::new()
        .prefix("fluent-na-")
        .tempdir_in("/private/tmp")?;
    let socket = sockets.path().join("hooks.sock");
    let listener = UnixListener::bind(&socket)?;
    listener.set_nonblocking(true)?;
    let artifact = &task.identity.artifact_dir;
    fs::create_dir_all(artifact)?;
    let generation_file = sockets.path().join("generation");
    let settings = install_hooks(task.private_home, &socket, &generation_file)?;
    let coder = SandboxedClaudeCode {
        sandbox_profile: None,
        trusted_sandbox: false,
        model_override: Some(task.model.into()),
        effort: Some(task.effort.into()),
    };
    let command = coder.native_task_command_in_terminal(
        task.workspace,
        task.private_home,
        &settings,
        task.session,
        task.system_prompt,
        task.prompt,
    )?;
    let pane = artifact.join("native-claude-pane.sh");
    fs::write(
        &pane,
        format!(
            "#!/bin/sh\nset -eu\nfor n in $(seq 1 1200); do\n  [ -f {} ] && break\n  sleep 0.05\ndone\n[ -f {} ] || exit 1\nexec {}\n",
            quote(&generation_file.display().to_string()),
            quote(&generation_file.display().to_string()),
            command_line(&command)?
        ),
    )?;
    let shell_command = format!("/bin/sh {}", quote(&pane.display().to_string()));
    let mut terminal = TerminalSession::start_managed_with_sockets(
        task.identity.clone(),
        task.worker_profile,
        task.workspace,
        &shell_command,
        &environment(&task),
        &[sockets.path().to_path_buf()],
    )?;
    let outcome = (|| -> Result<()> {
        let generation = terminal.descriptor().execution_generation.clone();
        let mut gate = ClaudeHookGate::new(
            &generation,
            match task.session {
                CoderSession::Resume(id) => Some(id),
                _ => None,
            },
        );
        let transcript = artifact.join("transcript.jsonl");
        let mut evidence = OpenOptions::new()
            .append(true)
            .create(true)
            .open(&transcript)?;
        fs::write(&generation_file, &generation)?;
        let deadline = Instant::now() + Duration::from_secs(60);
        let result = loop {
            match listener.accept() {
                Ok((stream, _)) => {
                    let observed = receive_hook(stream, &mut gate, &generation, &mut evidence);
                    match observed {
                        Ok(Completion::Accepted { .. }) => {
                            break Ok(());
                        }
                        Ok(Completion::Ignored) => {}
                        Err(error) => {
                            break Err(error);
                        }
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if !terminal.is_live()? {
                        break Err(anyhow::anyhow!(
                            "native Claude terminal ended without completion"
                        ));
                    }
                    if gate.session_id().is_none() && Instant::now() > deadline {
                        break Err(anyhow::anyhow!(
                            "native Claude emitted no SessionStart hook"
                        ));
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(error) => {
                    break Err(error.into());
                }
            }
        };
        let stop = if result.is_ok() {
            terminal.finish()
        } else {
            terminal.stop()
        };
        stop?;
        result?;
        evidence.sync_all()?;
        let native_transcript = gate
            .transcript_path()
            .ok_or_else(|| anyhow::anyhow!("native Claude omitted its transcript path"))?
            .canonicalize()?;
        ensure!(
            native_transcript.starts_with(task.private_home.canonicalize()?),
            "native Claude transcript escaped its private home"
        );
        fs::copy(native_transcript, artifact.join("claude-session.jsonl"))?;
        File::open(artifact.join("claude-session.jsonl"))?.sync_all()?;
        Ok(())
    })();
    if outcome.is_err() {
        let _ = terminal.stop();
    }
    outcome
}
