//! Correlate native Claude hooks with one managed Task invocation.

use anyhow::{Result, bail, ensure};
use serde_json::Value;
use std::collections::{HashMap, VecDeque};
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
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
    prompts: HashMap<String, VecDeque<u64>>,
    last_sequence: u64,
    event_sequence: u64,
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
            event_sequence: 0,
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

    pub fn event_sequence(&self) -> u64 {
        self.event_sequence
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
        self.event_sequence = self.last_sequence;
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
                let sequence = self.admission.admit()?;
                self.prompts
                    .entry(prompt_id.into())
                    .or_default()
                    .push_back(sequence);
                self.last_sequence = sequence;
                self.event_sequence = sequence;
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
                let Some(&sequence) = self
                    .prompts
                    .get(prompt_id)
                    .and_then(|pending| pending.front())
                else {
                    self.event_sequence = 0;
                    return Ok(Completion::Ignored);
                };
                self.event_sequence = sequence;
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
                self.prompts
                    .get_mut(prompt_id)
                    .expect("prompt has a pending completion")
                    .pop_front();
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

/// Trust only the workspace Fluent has already confined for this invocation.
fn prepare_private_home(home: &Path, workspace: &Path, api_key: Option<&str>) -> Result<()> {
    let temp = home.join("tmp");
    match fs::symlink_metadata(&temp) {
        Ok(metadata) => ensure!(metadata.is_dir(), "native Claude temp is not a directory"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => fs::create_dir(&temp)?,
        Err(error) => return Err(error.into()),
    }
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&temp, fs::Permissions::from_mode(0o700))?;
    let config = home.join(".claude.json");
    let mut value: Value = match fs::symlink_metadata(&config) {
        Ok(metadata) => {
            ensure!(
                metadata.is_file(),
                "native Claude config is not a regular file"
            );
            serde_json::from_slice(&fs::read(&config)?)?
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => serde_json::json!({}),
        Err(error) => return Err(error.into()),
    };
    let root = value
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("native Claude config is not an object"))?;
    root.insert("hasCompletedOnboarding".into(), Value::Bool(true));
    let projects = root
        .entry("projects")
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("native Claude projects are not an object"))?;
    let canonical = workspace.canonicalize()?;
    for path in [workspace, canonical.as_path()] {
        let project = projects
            .entry(path.to_string_lossy().into_owned())
            .or_insert_with(|| serde_json::json!({}))
            .as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("native Claude project is not an object"))?;
        project.insert("hasTrustDialogAccepted".into(), Value::Bool(true));
    }
    if let Some(key) = api_key.filter(|key| !key.is_empty()) {
        // Claude asks for confirmation before it starts a session when a new
        // API key arrives through the environment. Approve only the credential
        // already assigned to this private Task home.
        let responses = root
            .entry("customApiKeyResponses")
            .or_insert_with(|| serde_json::json!({"approved": [], "rejected": []}))
            .as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("native Claude API key responses are not an object"))?;
        let approved = responses
            .entry("approved")
            .or_insert_with(|| serde_json::json!([]))
            .as_array_mut()
            .ok_or_else(|| anyhow::anyhow!("native Claude approved keys are not an array"))?;
        approved.clear();
        approved.push(Value::String(key.into()));
        let rejected = responses
            .entry("rejected")
            .or_insert_with(|| serde_json::json!([]))
            .as_array_mut()
            .ok_or_else(|| anyhow::anyhow!("native Claude rejected keys are not an array"))?;
        rejected.retain(|value| value.as_str() != Some(key));
    }
    crate::atomic_write::atomic_write(&config, &serde_json::to_vec(&value)?)?;
    fs::set_permissions(&config, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

fn startup_signals(snapshot: Option<&str>) -> Vec<&'static str> {
    let Some(snapshot) = snapshot else {
        return Vec::new();
    };
    let mut signals = Vec::new();
    if snapshot.contains("Let's get started") {
        signals.push("onboarding");
    }
    if snapshot.contains("Quick safety check") {
        signals.push("workspace_trust");
    }
    if snapshot.contains("Bypass Permissions mode") {
        signals.push("bypass_confirmation");
    }
    if snapshot.contains("API key") && snapshot.contains("want to use") {
        signals.push("api_key_confirmation");
    }
    signals
}

fn failure_signals(output: Option<&str>) -> Vec<&'static str> {
    let Some(output) = output else {
        return Vec::new();
    };
    let mut signals = Vec::new();
    if output.contains("Resource temporarily unavailable") || output.contains("os error 35") {
        signals.push("process_resource_unavailable");
    }
    signals
}

fn session_tail(path: &Path, private_home: &Path) -> Option<String> {
    if !path
        .canonicalize()
        .ok()?
        .starts_with(private_home.canonicalize().ok()?)
    {
        return None;
    }
    let mut file = File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    file.seek(SeekFrom::Start(len.saturating_sub(64 * 1024)))
        .ok()?;
    let mut tail = Vec::new();
    file.read_to_end(&mut tail).ok()?;
    Some(String::from_utf8_lossy(&tail).into_owned())
}

fn record_runtime_diagnostic(
    artifact: &Path,
    generation: &str,
    terminal: &TerminalSession,
    gate: &ClaudeHookGate,
    private_home: &Path,
    reason: &str,
) -> Result<Vec<&'static str>> {
    let snapshot = terminal.capture_pane_snapshot().ok();
    let tail = gate
        .transcript_path()
        .and_then(|path| session_tail(path, private_home));
    let mut signals = failure_signals(snapshot.as_deref());
    for signal in failure_signals(tail.as_deref()) {
        if !signals.contains(&signal) {
            signals.push(signal);
        }
    }
    let mut file = File::create(artifact.join("native-claude-runtime-diagnostic.json"))?;
    serde_json::to_writer(
        &mut file,
        &serde_json::json!({
            "execution_generation": generation,
            "reason": reason,
            "session_started": gate.session_id().is_some(),
            "last_input_sequence": gate.last_sequence(),
            "pane_snapshot_available": snapshot.is_some(),
            "session_tail_available": tail.is_some(),
            "recognized_output": startup_signals(snapshot.as_deref()),
            "failure_signals": signals,
        }),
    )?;
    file.sync_all()?;
    Ok(signals)
}

fn record_startup_diagnostic(
    artifact: &Path,
    generation: &str,
    terminal: &TerminalSession,
    reason: &str,
) -> Result<()> {
    let snapshot = terminal.capture_pane_snapshot().ok();
    let mut file = File::create(artifact.join("native-claude-startup-diagnostic.json"))?;
    serde_json::to_writer(
        &mut file,
        &serde_json::json!({
            "execution_generation": generation,
            "reason": reason,
            "pane_snapshot_available": snapshot.is_some(),
            "recognized_output": startup_signals(snapshot.as_deref()),
        }),
    )?;
    file.sync_all()?;
    Ok(())
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
        serde_json::to_vec(&serde_json::json!({
            "skipDangerousModePermissionPrompt": true,
            "hooks": hooks,
        }))?,
    )?;
    Ok(settings)
}

fn environment(task: &ClaudeNativeTask<'_>) -> Vec<(OsString, OsString)> {
    managed_environment(task.private_home, task.workspace, task.launch_env)
}

fn managed_environment(
    private_home: &Path,
    workspace: &Path,
    launch_env: &[(String, String)],
) -> Vec<(OsString, OsString)> {
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
    values.push(("HOME".into(), private_home.as_os_str().to_owned()));
    values.push((
        "CLAUDE_CONFIG_DIR".into(),
        private_home.as_os_str().to_owned(),
    ));
    values.push(("PWD".into(), workspace.as_os_str().to_owned()));
    values.extend(
        launch_env
            .iter()
            .map(|(key, value)| (key.into(), value.into())),
    );
    let temp = private_home.join("tmp");
    values.push(("TMPDIR".into(), temp.as_os_str().to_owned()));
    // Claude uses its own internal temp setting on macOS and otherwise
    // creates /tmp/claude-<uid> before its SessionStart hook can run.
    values.push(("CLAUDE_CODE_TMPDIR".into(), temp.into_os_string()));
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
    stream.set_nonblocking(false)?;
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
    let completion = match gate.observe(owned, event) {
        Ok(completion) => completion,
        Err(error) => {
            if event["hook_event_name"] == "UserPromptSubmit" {
                serde_json::to_writer(
                    &mut *evidence,
                    &serde_json::json!({
                        "format":"fluent-native-agent-event", "schema_version":1,
                        "provider":"claude", "execution_generation":generation,
                        "assigned_conversation_id":gate.session_id(),
                        "conversation_id":event["session_id"], "input_sequence":0,
                        "event":{"type":"input_rejected", "request":event,
                            "reason":error.to_string()},
                    }),
                )?;
                evidence.write_all(b"\n")?;
                evidence.sync_all()?;
            }
            return Err(error);
        }
    };
    ensure!(owned == generation, "Claude hook generation changed");
    serde_json::to_writer(
        &mut *evidence,
        &serde_json::json!({
            "format":"fluent-native-agent-event", "schema_version":1,
            "provider":"claude", "execution_generation":generation,
            "assigned_conversation_id":gate.session_id(),
            "conversation_id":event["session_id"],
            "turn_id":event["prompt_id"],
            "input_sequence":gate.event_sequence(), "event":event,
        }),
    )?;
    evidence.write_all(b"\n")?;
    evidence.sync_all()?;
    stream.write_all(b"ok\n")?;
    Ok(completion)
}

fn copy_session_tail(source: &Path, output: &Path, start: u64) -> Result<()> {
    let mut source = File::open(source)?;
    ensure!(
        start <= source.metadata()?.len(),
        "native Claude session shrank during Task execution"
    );
    source.seek(SeekFrom::Start(start))?;
    let mut current_usage = File::create(output)?;
    std::io::copy(&mut source, &mut current_usage)?;
    current_usage.sync_all()?;
    Ok(())
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
    prepare_private_home(
        task.private_home,
        task.workspace,
        std::env::var("ANTHROPIC_API_KEY").ok().as_deref(),
    )?;
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
        let mut session_start_bytes = None;
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
                    if session_start_bytes.is_none()
                        && let Some(path) = gate.transcript_path()
                    {
                        ensure!(
                            path.starts_with(task.private_home),
                            "native Claude transcript escaped its private home"
                        );
                        session_start_bytes = Some(match fs::metadata(path) {
                            Ok(meta) => meta.len(),
                            Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
                            Err(error) => return Err(error.into()),
                        });
                    }
                    match observed {
                        Ok(Completion::Accepted { .. }) => {
                            break Ok(());
                        }
                        Ok(Completion::Ignored) => {}
                        Err(error) => {
                            let signals = record_runtime_diagnostic(
                                artifact,
                                &generation,
                                &terminal,
                                &gate,
                                task.private_home,
                                "hook_rejected",
                            )?;
                            break Err(error.context(format!(
                                "native Claude hook rejected; signals={signals:?}; inspect {}",
                                artifact
                                    .join("native-claude-runtime-diagnostic.json")
                                    .display()
                            )));
                        }
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if !terminal.is_live()? {
                        let (diagnostic, signals) = if gate.session_id().is_none() {
                            record_startup_diagnostic(
                                artifact,
                                &generation,
                                &terminal,
                                "terminal_exit_before_session_start",
                            )?;
                            (
                                artifact.join("native-claude-startup-diagnostic.json"),
                                Vec::new(),
                            )
                        } else {
                            let signals = record_runtime_diagnostic(
                                artifact,
                                &generation,
                                &terminal,
                                &gate,
                                task.private_home,
                                "terminal_exit_after_session_start",
                            )?;
                            (
                                artifact.join("native-claude-runtime-diagnostic.json"),
                                signals,
                            )
                        };
                        break Err(anyhow::anyhow!(
                            "native Claude terminal ended without completion; signals={signals:?}; inspect {}",
                            diagnostic.display()
                        ));
                    }
                    if gate.session_id().is_none() && Instant::now() > deadline {
                        record_startup_diagnostic(
                            artifact,
                            &generation,
                            &terminal,
                            "session_start_timeout",
                        )?;
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
        copy_session_tail(
            &artifact.join("claude-session.jsonl"),
            &artifact.join("claude-task-usage.jsonl"),
            session_start_bytes.unwrap_or(0),
        )?;
        Ok(())
    })();
    if outcome.is_err() {
        let _ = terminal.stop();
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::{
        ClaudeHookGate, copy_session_tail, install_hooks, managed_environment,
        prepare_private_home, receive_hook, startup_signals,
    };
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::UnixStream;
    use std::time::Duration;

    #[test]
    fn claude_hook_waits_for_payload_on_nonblocking_accepted_socket() {
        let (mut sender, receiver) = UnixStream::pair().unwrap();
        // Accepted sockets inherit the listener's nonblocking flag on macOS.
        // Set it explicitly to test that receiver state without binding a socket.
        receiver.set_nonblocking(true).unwrap();
        let hook = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            sender
                .write_all(b"{\"generation\":\"generation-1\",\"event\":{\"hook_event_name\":\"SessionStart\",\"session_id\":\"assigned-session\",\"source\":\"startup\"}}\n")
                .unwrap();
            let mut ack = String::new();
            BufReader::new(sender).read_line(&mut ack).unwrap();
            ack
        });
        let evidence = tempfile::NamedTempFile::new().unwrap();
        let mut writer = evidence.reopen().unwrap();
        let mut gate = ClaudeHookGate::new("generation-1", Some("assigned-session"));
        receive_hook(receiver, &mut gate, "generation-1", &mut writer).unwrap();
        assert_eq!(hook.join().unwrap(), "ok\n");
        assert_eq!(gate.session_id(), Some("assigned-session"));
        assert_eq!(
            std::fs::read_to_string(evidence.path())
                .unwrap()
                .lines()
                .count(),
            1
        );
    }

    #[test]
    fn private_claude_startup_skips_only_owned_workspace_prompts() {
        let home = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let config = home.path().join(".claude.json");
        std::fs::write(
            &config,
            serde_json::json!({"projects": {other.path().to_str().unwrap(): {"hasTrustDialogAccepted": false}}, "existing": true}).to_string(),
        )
        .unwrap();
        prepare_private_home(home.path(), workspace.path(), None).unwrap();
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&config).unwrap()).unwrap();
        assert_eq!(value["hasCompletedOnboarding"], true);
        assert_eq!(value["existing"], true);
        assert_eq!(
            value["projects"][workspace.path().canonicalize().unwrap().to_str().unwrap()]["hasTrustDialogAccepted"],
            true
        );
        assert_eq!(
            value["projects"][other.path().to_str().unwrap()]["hasTrustDialogAccepted"],
            false
        );

        let socket = home.path().join("hooks.sock");
        let generation = home.path().join("generation");
        let settings = install_hooks(home.path(), &socket, &generation).unwrap();
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(settings).unwrap()).unwrap();
        assert_eq!(value["skipDangerousModePermissionPrompt"], true);
        assert!(value["hooks"]["SessionStart"].is_array());
    }

    #[cfg(unix)]
    #[test]
    fn private_claude_trusts_the_workspace_path_given_to_the_terminal() {
        let home = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let logical = home.path().join("logical-workspace");
        std::os::unix::fs::symlink(project.path(), &logical).unwrap();
        prepare_private_home(home.path(), &logical, None).unwrap();
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(home.path().join(".claude.json")).unwrap())
                .unwrap();
        assert_eq!(
            value["projects"][logical.to_str().unwrap()]["hasTrustDialogAccepted"],
            true
        );
        assert_eq!(
            value["projects"][project.path().canonicalize().unwrap().to_str().unwrap()]["hasTrustDialogAccepted"],
            true
        );
    }

    #[test]
    fn private_claude_approves_only_the_assigned_api_key() {
        let home = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let config = home.path().join(".claude.json");
        std::fs::write(
            &config,
            serde_json::json!({"customApiKeyResponses": {
                "approved": ["previous-key"], "rejected": ["other-key"]
            }})
            .to_string(),
        )
        .unwrap();
        prepare_private_home(home.path(), workspace.path(), Some("assigned-key")).unwrap();
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&config).unwrap()).unwrap();
        assert_eq!(
            value["customApiKeyResponses"],
            serde_json::json!({
                "approved": ["assigned-key"],
                "rejected": ["other-key"]
            })
        );
        assert_eq!(
            std::fs::metadata(&config).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn native_claude_redirects_internal_temp_away_from_shared_tmp() {
        let home = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        prepare_private_home(home.path(), workspace.path(), None).unwrap();
        let env = managed_environment(
            home.path(),
            workspace.path(),
            &[
                ("TMPDIR".into(), "/private/tmp".into()),
                ("CLAUDE_CODE_TMPDIR".into(), "/tmp".into()),
            ],
        );
        let temp = home.path().join("tmp");
        assert!(temp.is_dir());
        assert_eq!(
            env.iter().rev().find(|(key, _)| key == "TMPDIR").unwrap().1,
            temp
        );
        // The installed CLI otherwise tries to create /tmp/claude-<uid>
        // before SessionStart, even when TMPDIR points to the private home.
        assert_eq!(
            env.iter()
                .rev()
                .find(|(key, _)| key == "CLAUDE_CODE_TMPDIR")
                .unwrap()
                .1,
            temp
        );
    }

    #[test]
    fn claude_startup_diagnostic_keeps_only_known_signals() {
        assert_eq!(
            startup_signals(Some("Quick safety check: Is this a project you trust?")),
            vec!["workspace_trust"]
        );
        assert_eq!(
            startup_signals(Some(
                "WARNING: Claude Code running in Bypass Permissions mode"
            )),
            vec!["bypass_confirmation"]
        );
        assert_eq!(
            startup_signals(Some("Do you want to use this API key?")),
            vec!["api_key_confirmation"]
        );
        assert!(startup_signals(Some("private prompt and credential text")).is_empty());
    }

    #[test]
    fn claude_failure_diagnostic_identifies_resource_exhaustion_without_private_text() {
        assert_eq!(
            super::failure_signals(Some(
                "Could not launch shell: Resource temporarily unavailable (os error 35)\nprivate prompt"
            )),
            vec!["process_resource_unavailable"]
        );
        assert!(super::failure_signals(Some("private prompt and credential text")).is_empty());

        let home = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let session = home.path().join("session.jsonl");
        std::fs::write(&session, "tool failed: os error 35\nprivate prompt").unwrap();
        let tail = super::session_tail(&session, home.path()).unwrap();
        assert_eq!(
            super::failure_signals(Some(&tail)),
            vec!["process_resource_unavailable"]
        );
        assert!(super::session_tail(&session, outside.path()).is_none());
    }

    #[test]
    fn claude_usage_keeps_only_current_invocation_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("session.jsonl");
        let output = dir.path().join("task-usage.jsonl");
        let older = b"{\"type\":\"assistant\",\"message\":{\"id\":\"old\"}}\n";
        let current = b"{\"type\":\"assistant\",\"message\":{\"id\":\"new\"}}\n";
        std::fs::write(&source, [older.as_slice(), current.as_slice()].concat()).unwrap();
        copy_session_tail(&source, &output, older.len() as u64).unwrap();
        assert_eq!(std::fs::read(&output).unwrap(), current);
        assert!(copy_session_tail(&source, &output, 10_000).is_err());
    }
}
