//! Launch one assigned Codex conversation behind the Task's terminal owner.

use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::net::Shutdown;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{TrySendError, sync_channel};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};
use tungstenite::{Message, WebSocket};

use crate::coder::{CoderSession, CodexCode};
use crate::native_agent::{CodexEvidenceRecord, CodexRelayGate, Completion, NativeAdmission};
use crate::terminal_sessions::{TerminalIdentity, TerminalSession};

pub struct CodexNativeTask<'a> {
    pub identity: TerminalIdentity,
    pub worker_profile: &'a crate::os::SandboxProfile,
    pub workspace: &'a Path,
    pub private_home: &'a Path,
    pub executable: &'a Path,
    pub model: &'a str,
    pub effort: Option<&'a str>,
    pub prompt: &'a str,
    pub session: CoderSession<'a>,
    pub launch_env: &'a [(String, String)],
}

/// Give this invocation exactly one trusted project in its private Codex
/// configuration. A source-checkout entry can preexist in that home, so never
/// carry project entries into the Task.
fn prepare_native_config(private_home: &Path, workspace: &Path) -> Result<PathBuf> {
    ensure!(
        private_home.is_absolute() && private_home.is_dir(),
        "Codex home must be private and absolute"
    );
    let home_metadata = fs::symlink_metadata(private_home)?;
    ensure!(
        home_metadata.file_type().is_dir()
            && home_metadata.uid() == unsafe { libc::geteuid() }
            && home_metadata.mode() & 0o077 == 0,
        "Codex home is not private to the current user"
    );
    let workspace = workspace
        .canonicalize()
        .context("resolve assigned Codex worktree")?;
    ensure!(
        workspace.is_dir(),
        "assigned Codex worktree is not a directory"
    );
    let path = private_home.join("config.toml");
    let existing = match fs::symlink_metadata(&path) {
        Ok(metadata) => Some(metadata),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error).context("inspect private Codex config"),
    };
    let mut config = if let Some(metadata) = existing {
        ensure!(
            metadata.file_type().is_file() && metadata.mode() & 0o077 == 0,
            "private Codex config is not a private regular file"
        );
        let source = fs::read_to_string(&path)?;
        ensure!(
            source.len() <= 1024 * 1024,
            "private Codex config is oversized"
        );
        source
            .parse::<toml::Table>()
            .context("parse private Codex config")?
    } else {
        toml::Table::new()
    };
    // The worker's authentication, sessions and skills are separate files.
    // Keep only TUI preferences from a generated config: model, hooks, MCP,
    // and project trust from earlier commands must not govern this Task.
    config.retain(|key, _| key == "tui");
    let mut project = toml::Table::new();
    project.insert("trust_level".into(), toml::Value::String("trusted".into()));
    let mut projects = toml::Table::new();
    projects.insert(
        workspace
            .to_str()
            .context("assigned Codex worktree is not UTF-8")?
            .into(),
        toml::Value::Table(project),
    );
    config.insert("projects".into(), toml::Value::Table(projects));
    let mut temporary = tempfile::Builder::new()
        .prefix(".native-config-")
        .tempfile_in(private_home)?;
    temporary
        .as_file()
        .set_permissions(fs::Permissions::from_mode(0o600))?;
    write!(temporary, "{}", toml::to_string(&config)?)?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(&path)
        .context("install private Codex config")?;
    Ok(workspace)
}

struct CodexDaemonResources {
    socket: PathBuf,
    lock: PathBuf,
    owned: bool,
}

impl CodexDaemonResources {
    fn paths(requested_socket: &Path) -> Result<Self> {
        ensure!(
            requested_socket.is_absolute(),
            "Codex socket must be absolute"
        );
        // Codex 0.162.0 hashes the literal --listen path, then binds beside
        // its lock in a per-user daemon directory under /private/tmp.
        let uid = unsafe { libc::geteuid() };
        let parent = PathBuf::from(format!("/private/tmp/codex-daemon-{uid}"));
        let digest = format!(
            "{:x}",
            Sha256::digest(requested_socket.as_os_str().as_encoded_bytes())
        );
        Ok(Self {
            socket: parent.join(&digest),
            lock: parent.join(format!("{digest}.lock")),
            owned: false,
        })
    }

    fn prepare(requested_socket: &Path) -> Result<Self> {
        let mut resources = Self::paths(requested_socket)?;
        let parent = resources.socket.parent().unwrap();
        match fs::DirBuilder::new().mode(0o700).create(parent) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error).context("creating Codex daemon directory"),
        }
        let metadata = fs::symlink_metadata(parent)?;
        ensure!(
            metadata.file_type().is_dir()
                && metadata.uid() == unsafe { libc::geteuid() }
                && metadata.mode() & 0o077 == 0,
            "Codex daemon directory is not private to the current user"
        );
        for path in [&resources.socket, &resources.lock] {
            ensure!(
                fs::symlink_metadata(path)
                    .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound),
                "Codex daemon resource already exists: {}",
                path.display()
            );
        }
        resources.owned = true;
        Ok(resources)
    }
}

impl Drop for CodexDaemonResources {
    fn drop(&mut self) {
        if !self.owned {
            return;
        }
        for path in [&self.socket, &self.lock] {
            if let Ok(metadata) = fs::symlink_metadata(path)
                && metadata.uid() == unsafe { libc::geteuid() }
                && (metadata.file_type().is_socket() || metadata.file_type().is_file())
            {
                let _ = fs::remove_file(path);
            }
        }
    }
}

/// Check the selected executable's remote TUI and private Unix app-server
/// features before the Task takes an Executing reservation.
pub fn check_capability(executable: &Path) -> Result<()> {
    for (arguments, required) in [
        (&["app-server", "--help"][..], &["--listen", "unix://"][..]),
        (&["--help"][..], &["--remote", "unix://"][..]),
    ] {
        let output = Command::new(executable)
            .args(arguments)
            .output()
            .with_context(|| {
                format!(
                    "checking native Codex capability of {}",
                    executable.display()
                )
            })?;
        ensure!(
            output.status.success(),
            "native Codex rejected capability query {:?}",
            arguments
        );
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        for feature in required {
            ensure!(
                text.contains(feature),
                "native Codex lacks required {feature} capability"
            );
        }
    }
    let tmux = Command::new("tmux")
        .arg("-V")
        .output()
        .context("native Codex requires tmux")?;
    ensure!(tmux.status.success(), "native Codex requires working tmux");
    Ok(())
}

fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn command_line(command: &std::process::Command) -> Result<String> {
    let mut words = Vec::new();
    words.push(quote(
        command
            .get_program()
            .to_str()
            .context("native Codex executable is not UTF-8")?,
    ));
    for arg in command.get_args() {
        words.push(quote(
            arg.to_str().context("native Codex argument is not UTF-8")?,
        ));
    }
    Ok(words.join(" "))
}

/// Build the pane script from the same command builder that checks the private
/// home, assigned model, effort, disabled hooks, and exact resume identity.
pub fn pane_script(
    coder: &CodexCode,
    workspace: &Path,
    private_home: &Path,
    app_socket: &Path,
    relay_socket: &Path,
    go_marker: &Path,
    thread_file: &Path,
    provider_log: &Path,
    prompt: &str,
) -> Result<String> {
    let placeholder = "__FLUENT_ASSIGNED_THREAD__";
    let command = coder.native_task_command_with_remote(
        workspace,
        private_home,
        CoderSession::Resume(placeholder),
        prompt,
        Some(relay_socket),
    )?;
    let mut server = Command::new(&coder.executable);
    server.args(["--disable", "hooks"]);
    if let Some(model) = &coder.model_override {
        server.args(["--model", model]);
    }
    if let Some(effort) = &coder.effort {
        server.args(["-c", &format!("model_reasoning_effort={effort}")]);
    }
    server.args([
        "app-server",
        "--listen",
        &format!("unix://{}", app_socket.display()),
    ]);
    Ok(format!(
        "#!/bin/sh\nset -eu\n{} >{} 2>&1 &\nserver=$!\nfor n in $(seq 1 200); do\n  [ -S {} ] && break\n  kill -0 \"$server\" 2>/dev/null || exit 1\n  sleep 0.05\ndone\n[ -S {} ] || exit 1\nfor n in $(seq 1 1200); do\n  [ -f {} ] && break\n  kill -0 \"$server\" 2>/dev/null || exit 1\n  sleep 0.05\ndone\n[ -f {} ] || exit 1\nassigned_thread=$(cat {})\n[ -n \"$assigned_thread\" ] || exit 1\nexec {}\n",
        command_line(&server)?,
        quote(&provider_log.display().to_string()),
        quote(&app_socket.display().to_string()),
        quote(&app_socket.display().to_string()),
        quote(&go_marker.display().to_string()),
        quote(&go_marker.display().to_string()),
        quote(&thread_file.display().to_string()),
        command_line(&command)?.replace(&quote(placeholder), "\"$assigned_thread\""),
    ))
}

fn request(
    socket: &mut WebSocket<UnixStream>,
    id: u64,
    method: &str,
    params: Value,
) -> Result<Value> {
    let payload = json!({"id": id, "method": method, "params": params});
    socket.send(Message::Text(payload.to_string()))?;
    loop {
        let frame = socket.read().context("reading Codex app-server response")?;
        let Message::Text(line) = frame else {
            ensure!(!frame.is_close(), "Codex app-server closed during {method}");
            continue;
        };
        ensure!(
            line.len() <= 1024 * 1024,
            "Codex app-server response is oversized"
        );
        let message: Value = serde_json::from_str(&line)?;
        if message["id"].as_u64() != Some(id) {
            continue;
        }
        ensure!(
            message.get("error").is_none(),
            "Codex app-server {method} failed: {}",
            message["error"]
        );
        return Ok(message);
    }
}

fn bootstrap_protocol(
    socket: &mut WebSocket<UnixStream>,
    workspace: &Path,
    model: &str,
    session: CoderSession<'_>,
) -> Result<(String, Value)> {
    request(
        socket,
        1,
        "initialize",
        json!({
            "clientInfo": {"name": "fluent-native-task", "version": env!("CARGO_PKG_VERSION")}
        }),
    )?;
    socket.send(Message::Text("{\"method\":\"initialized\"}".into()))?;
    let config = request(socket, 2, "config/read", json!({"cwd": workspace}))?;
    let canonical = workspace
        .canonicalize()
        .context("resolve assigned Codex worktree")?;
    let projects = config["result"]["config"]["projects"]
        .as_object()
        .context("Codex config/read omitted project trust")?;
    ensure!(
        projects.len() == 1
            && projects
                .get(
                    canonical
                        .to_str()
                        .context("assigned Codex worktree is not UTF-8")?
                )
                .is_some_and(|entry| entry["trust_level"] == "trusted"),
        "Codex app-server did not trust only the assigned worktree"
    );
    let result = match session {
        CoderSession::Resume(thread_id) => {
            ensure!(
                !thread_id.is_empty(),
                "native Codex resume identity is empty"
            );
            request(
                socket,
                3,
                "thread/resume",
                json!({
                    "threadId": thread_id, "cwd": workspace,
                }),
            )?
        }
        CoderSession::FreshPersistent | CoderSession::Ephemeral => request(
            socket,
            3,
            "thread/start",
            json!({
                "cwd": workspace,
                "model": model,
                "ephemeral": session == CoderSession::Ephemeral,
                "approvalPolicy": "never",
                "sandbox": "danger-full-access",
            }),
        )?,
    };
    let thread_id = result["result"]["thread"]["id"]
        .as_str()
        .context("Codex app-server omitted the assigned thread id")?;
    if let CoderSession::Resume(expected) = session {
        ensure!(
            thread_id == expected,
            "Codex resumed another Task conversation"
        );
    }
    Ok((thread_id.to_owned(), result))
}

fn bootstrap(app_socket: &Path, task: &CodexNativeTask<'_>) -> Result<(String, Value)> {
    let deadline = Instant::now() + Duration::from_secs(12);
    while !app_socket.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    ensure!(
        app_socket.exists(),
        "Codex app-server did not create its private socket"
    );
    let stream = UnixStream::connect(app_socket).context("connect to private Codex app-server")?;
    stream.set_read_timeout(Some(Duration::from_secs(12)))?;
    stream.set_write_timeout(Some(Duration::from_secs(12)))?;
    let (mut socket, _) = tungstenite::client("ws://localhost/", stream)
        .map_err(|error| anyhow::anyhow!("Codex app-server WebSocket handshake failed: {error}"))?;
    bootstrap_protocol(&mut socket, task.workspace, task.model, task.session)
}

fn environment(task: &CodexNativeTask<'_>) -> Vec<(OsString, OsString)> {
    let mut values: Vec<(OsString, OsString)> = [
        "PATH",
        "USER",
        "LOGNAME",
        "SHELL",
        "LANG",
        "LC_ALL",
        "TERM",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "NO_PROXY",
        "OPENAI_API_KEY",
        "CODEX_API_KEY",
        "CODEX_ACCESS_TOKEN",
        "CODEX_AUTH_JSON",
        "SSL_CERT_FILE",
    ]
    .iter()
    .filter_map(|name| std::env::var_os(name).map(|value| (OsString::from(name), value)))
    .collect();
    values.push(("GIT_EDITOR".into(), "false".into()));
    values.push(("GIT_SEQUENCE_EDITOR".into(), "false".into()));
    values.extend(
        task.launch_env
            .iter()
            .filter(|(key, _)| !matches!(key.as_str(), "HOME" | "CODEX_HOME" | "PWD"))
            .map(|(key, value)| (key.into(), value.into())),
    );
    values.push(("HOME".into(), task.private_home.as_os_str().to_owned()));
    values.push((
        "CODEX_HOME".into(),
        task.private_home.as_os_str().to_owned(),
    ));
    values.push(("PWD".into(), task.workspace.as_os_str().to_owned()));
    values
}

fn write_bootstrap_evidence(
    path: &Path,
    generation: &str,
    thread_id: &str,
    event: &Value,
) -> Result<()> {
    let mut file = OpenOptions::new().append(true).create(true).open(path)?;
    serde_json::to_writer(
        &mut file,
        &json!({
            "format": "fluent-native-agent-event",
            "schema_version": 1,
            "provider": "codex",
            "execution_generation": generation,
            "assigned_conversation_id": thread_id,
            "conversation_id": thread_id,
            "turn_id": null,
            "input_sequence": 0,
            "event": event,
        }),
    )?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(())
}

enum SocketRead {
    Pending,
    Closed,
    Text(String),
}

fn read_socket(socket: &Mutex<WebSocket<UnixStream>>) -> Result<SocketRead> {
    let mut socket = socket
        .lock()
        .map_err(|_| anyhow::anyhow!("Codex WebSocket lock poisoned"))?;
    match socket.read() {
        Ok(Message::Text(text)) => Ok(SocketRead::Text(text)),
        Ok(Message::Close(_)) | Err(tungstenite::Error::ConnectionClosed) => Ok(SocketRead::Closed),
        Ok(Message::Ping(_) | Message::Pong(_) | Message::Frame(_)) => Ok(SocketRead::Pending),
        Err(tungstenite::Error::Io(error))
            if matches!(
                error.kind(),
                std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
            ) =>
        {
            Ok(SocketRead::Pending)
        }
        Ok(Message::Binary(_)) => anyhow::bail!("Codex app-server sent a binary protocol frame"),
        Err(error) => Err(error.into()),
    }
}

fn send_required_tui_frame(client: &mut WebSocket<UnixStream>, text: String) -> Result<()> {
    const DRAIN_LIMIT: Duration = Duration::from_secs(3);
    let mut result = client.send(Message::Text(text));
    let deadline = Instant::now() + DRAIN_LIMIT;
    loop {
        match result {
            Ok(()) => return Ok(()),
            Err(tungstenite::Error::Io(ref error))
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                if Instant::now() >= deadline {
                    anyhow::bail!(
                        "Codex TUI WebSocket protocol send failed: required frame did not drain within {DRAIN_LIMIT:?}: {error}"
                    );
                }
                // tungstenite retains the frame after a stream write failure.
                // Flush its buffered bytes; sending the message again would
                // duplicate a partly delivered protocol response.
                std::thread::sleep(Duration::from_millis(10));
                result = client.flush();
            }
            Err(error) => {
                return Err(error).context("Codex TUI WebSocket protocol send failed");
            }
        }
    }
}

/// Relay framed app-server messages. Unix app-server sockets use WebSocket
/// HTTP Upgrade, while the older stream fixture intentionally covers JSONL
/// framing only. This path is the one used by managed Task launch.
pub fn relay_codex_websockets<W, F>(
    client: WebSocket<UnixStream>,
    server: WebSocket<UnixStream>,
    generation: &str,
    thread_id: &str,
    admission: Arc<NativeAdmission>,
    mut evidence: W,
    mut on_completion: F,
) -> Result<()>
where
    W: Write + Send,
    F: FnMut(Completion) -> Result<()> + Send,
{
    let client_abort = client.get_ref().try_clone()?;
    let server_abort = server.get_ref().try_clone()?;
    let downstream_abort_client = client.get_ref().try_clone()?;
    let downstream_abort_server = server.get_ref().try_clone()?;
    let display_abort_client = client.get_ref().try_clone()?;
    let display_abort_server = server.get_ref().try_clone()?;
    client
        .get_ref()
        .set_read_timeout(Some(Duration::from_millis(50)))?;
    client
        .get_ref()
        .set_write_timeout(Some(Duration::from_millis(250)))?;
    server
        .get_ref()
        .set_read_timeout(Some(Duration::from_millis(50)))?;
    server
        .get_ref()
        .set_write_timeout(Some(Duration::from_secs(2)))?;
    let client = Arc::new(Mutex::new(client));
    let server = Arc::new(Mutex::new(server));
    let gate = Arc::new(Mutex::new(CodexRelayGate::with_admission(
        generation, thread_id, admission,
    )));
    // These frames include responses to TUI protocol requests. Losing one can
    // leave the TUI waiting forever before it submits the assigned prompt.
    let (display_tx, display_rx) = sync_channel::<String>(32);
    std::thread::scope(|scope| {
        let display_client = Arc::clone(&client);
        let display = scope.spawn(move || -> Result<()> {
            let result = (|| {
                while let Ok(text) = display_rx.recv() {
                    let mut client = display_client
                        .lock()
                        .map_err(|_| anyhow::anyhow!("Codex TUI WebSocket lock poisoned"))?;
                    send_required_tui_frame(&mut client, text)?;
                }
                Ok(())
            })();
            if result.is_err() {
                let _ = display_abort_client.shutdown(Shutdown::Both);
                let _ = display_abort_server.shutdown(Shutdown::Both);
            }
            result
        });
        let upstream_client = Arc::clone(&client);
        let upstream_server = Arc::clone(&server);
        let request_gate = Arc::clone(&gate);
        let upstream = scope.spawn(move || -> Result<()> {
            let result = (|| {
                loop {
                    let line = match read_socket(&upstream_client)? {
                        SocketRead::Pending => {
                            std::thread::sleep(Duration::from_millis(1));
                            continue;
                        }
                        SocketRead::Closed => return Ok(()),
                        SocketRead::Text(line) => line,
                    };
                    let request: Value = serde_json::from_str(&line)?;
                    let method = request["method"].as_str().unwrap_or("<missing>");
                    let admitted = request_gate.lock()
                        .map_err(|_| anyhow::anyhow!("native Codex relay gate poisoned"))?
                        .admit_request(&request);
                    if admitted.as_ref().is_ok_and(Option::is_none) {
                        eprintln!("native Codex TUI request: {method}");
                    }
                    if admitted.as_ref().is_ok_and(Option::is_some) {
                        eprintln!("native Codex TUI admitted input: {method}");
                    }
                    if let Err(error) = admitted {
                        if let Some(id) = request.get("id") {
                            let response = json!({"id": id, "error": {"code": -32000, "message": error.to_string()}});
                            upstream_client.lock()
                                .map_err(|_| anyhow::anyhow!("Codex WebSocket lock poisoned"))?
                                .send(Message::Text(response.to_string()))?;
                            continue;
                        }
                        return Err(error);
                    }
                    upstream_server.lock()
                        .map_err(|_| anyhow::anyhow!("Codex WebSocket lock poisoned"))?
                        .send(Message::Text(line))?;
                }
            })();
            let _ = client_abort.shutdown(Shutdown::Both);
            let _ = server_abort.shutdown(Shutdown::Both);
            result
        });
        let downstream_server = Arc::clone(&server);
        let response_gate = Arc::clone(&gate);
        let downstream = scope.spawn(move || -> Result<()> {
            let result = (|| {
                loop {
                    let line = match read_socket(&downstream_server)? {
                        SocketRead::Pending => {
                            std::thread::sleep(Duration::from_millis(1));
                            continue;
                        }
                        SocketRead::Closed => return Ok(()),
                        SocketRead::Text(line) => line,
                    };
                    let event: Value = serde_json::from_str(&line)?;
                    let (completion, input_sequence) = {
                        let mut gate = response_gate
                            .lock()
                            .map_err(|_| anyhow::anyhow!("native Codex relay gate poisoned"))?;
                        (
                            gate.observe_event_for_generation(generation, &event)?,
                            gate.input_sequence,
                        )
                    };
                    if input_sequence == 0 {
                        eprintln!(
                            "native Codex app-server message before turn: {}",
                            event["method"].as_str().unwrap_or("response")
                        );
                    }
                    let params = &event["params"];
                    let conversation_id = params["threadId"]
                        .as_str()
                        .or_else(|| params["thread"]["id"].as_str())
                        .unwrap_or(thread_id);
                    let record = CodexEvidenceRecord {
                        format: "fluent-native-agent-event",
                        schema_version: 1,
                        provider: "codex",
                        execution_generation: generation,
                        assigned_conversation_id: thread_id,
                        conversation_id,
                        turn_id: params["turn"]["id"]
                            .as_str()
                            .or_else(|| params["turnId"].as_str()),
                        input_sequence,
                        event: &event,
                    };
                    serde_json::to_writer(&mut evidence, &record)?;
                    evidence.write_all(b"\n")?;
                    evidence.flush()?;
                    if completion != Completion::Ignored {
                        on_completion(completion)?;
                    }
                    match display_tx.try_send(line) {
                        Ok(()) => {}
                        Err(TrySendError::Full(_)) => {
                            anyhow::bail!("Codex TUI WebSocket protocol backpressure");
                        }
                        Err(TrySendError::Disconnected(_)) => {
                            anyhow::bail!("Codex TUI WebSocket protocol sender disconnected");
                        }
                    }
                }
            })();
            let _ = downstream_abort_client.shutdown(Shutdown::Both);
            let _ = downstream_abort_server.shutdown(Shutdown::Both);
            result
        });
        let upstream = upstream
            .join()
            .map_err(|_| anyhow::anyhow!("Codex WebSocket request relay panicked"))?;
        let downstream = downstream
            .join()
            .map_err(|_| anyhow::anyhow!("Codex WebSocket event relay panicked"))?;
        let display = display
            .join()
            .map_err(|_| anyhow::anyhow!("Codex WebSocket display relay panicked"))?;
        display.and(downstream).and(upstream)
    })
}

#[derive(Debug)]
struct NativeFailureWithSettlement {
    primary: anyhow::Error,
    secondary: String,
}

impl std::fmt::Display for NativeFailureWithSettlement {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:#}; {}", self.primary, self.secondary)
    }
}

impl std::error::Error for NativeFailureWithSettlement {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.primary.as_ref())
    }
}

fn settle_native_result<T>(
    result: Result<T>,
    stop: Result<()>,
    joined: std::thread::Result<()>,
) -> Result<T> {
    let mut result = result;
    if let Err(error) = stop {
        result = match result {
            Err(primary) => Err(NativeFailureWithSettlement {
                primary,
                secondary: format!("terminal settlement also failed: {error:#}"),
            }
            .into()),
            Ok(_) => Err(error.context("native Codex terminal settlement failed")),
        };
    }
    if joined.is_err() {
        result = match result {
            Err(primary) => Err(NativeFailureWithSettlement {
                primary,
                secondary: "native Codex relay also panicked".into(),
            }
            .into()),
            Ok(_) => Err(anyhow::anyhow!("native Codex relay panicked")),
        };
    }
    result
}

/// Run the assigned Task through a private app-server, one relay, and one
/// managed tmux pane. A provider notification only nominates completion; the
/// caller still runs its normal Writer validation and publishes the Task result.
pub fn run(task: CodexNativeTask<'_>) -> Result<()> {
    ensure!(
        task.private_home.is_dir(),
        "native Codex private home is absent"
    );
    ensure!(!task.model.is_empty(), "native Codex model is unresolved");
    let assigned_workspace = prepare_native_config(task.private_home, task.workspace)?;
    ensure!(
        assigned_workspace == task.workspace,
        "native Codex workspace must use its canonical assigned path"
    );
    let sockets = tempfile::Builder::new()
        .prefix("fluent-na-")
        .tempdir_in("/private/tmp")?;
    let app_socket = sockets.path().join("app.sock");
    let daemon_resources = CodexDaemonResources::prepare(&app_socket)?;
    let relay_socket = sockets.path().join("relay.sock");
    let listener = UnixListener::bind(&relay_socket).context("bind native Codex relay")?;
    listener.set_nonblocking(true)?;
    let artifact = &task.identity.artifact_dir;
    fs::create_dir_all(artifact)?;
    let go_marker = sockets.path().join("start-tui");
    let thread_file = sockets.path().join("assigned-thread");
    let script_path = artifact.join("native-codex-pane.sh");
    let provider_log = artifact.join("native-provider-stderr.log");
    // The pane starts its private server and waits until the owner has loaded
    // one assigned thread through the same app-server process.
    let coder = CodexCode {
        sandbox_profile: None,
        trusted_sandbox: false,
        model_override: Some(task.model.to_owned()),
        effort: task.effort.map(str::to_owned),
        executable: task.executable.to_path_buf(),
    };
    let script = pane_script(
        &coder,
        task.workspace,
        task.private_home,
        &app_socket,
        &relay_socket,
        &go_marker,
        &thread_file,
        &provider_log,
        task.prompt,
    )?;
    fs::write(&script_path, script)?;
    let shell_command = format!("/bin/sh {}", quote(&script_path.display().to_string()));
    let mut terminal = TerminalSession::start_managed_with_resources(
        task.identity.clone(),
        task.worker_profile,
        task.workspace,
        &shell_command,
        &environment(&task),
        &[sockets.path().to_path_buf()],
        &[daemon_resources.socket.clone()],
        &[daemon_resources.lock.clone()],
    )?;
    let outcome = (|| -> Result<()> {
        let (thread_id, start_event) = bootstrap(&app_socket, &task)?;
        let generation = terminal.descriptor().execution_generation.clone();
        let transcript = artifact.join("transcript.jsonl");
        write_bootstrap_evidence(&transcript, &generation, &thread_id, &start_event)?;
        fs::write(&thread_file, &thread_id)?;
        let evidence = OpenOptions::new().append(true).open(&transcript)?;
        let admission = Arc::new(NativeAdmission::new());
        let settlement = Arc::clone(&admission);
        let stopping = Arc::new(AtomicBool::new(false));
        let relay_stopping = Arc::clone(&stopping);
        let (tx, rx) = mpsc::channel::<Result<Completion>>();
        let thread_id_for_relay = thread_id.clone();
        let relay = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(60);
            let client = loop {
                match listener.accept() {
                    Ok((client, _)) => break Ok(client),
                    Err(error)
                        if error.kind() == std::io::ErrorKind::WouldBlock
                            && Instant::now() < deadline
                            && !relay_stopping.load(Ordering::Acquire) =>
                    {
                        std::thread::sleep(Duration::from_millis(20));
                    }
                    Err(error) => {
                        break Err(anyhow::Error::new(error).context("accept Codex native TUI"));
                    }
                }
            };
            let result = client.and_then(|client| {
                client.set_read_timeout(Some(Duration::from_secs(12)))?;
                let client = tungstenite::accept(client).map_err(|error| {
                    anyhow::anyhow!("Codex native TUI WebSocket handshake failed: {error}")
                })?;
                let server = UnixStream::connect(&app_socket)
                    .context("connect relay to private app-server")?;
                server.set_read_timeout(Some(Duration::from_secs(12)))?;
                let (server, _) =
                    tungstenite::client("ws://localhost/", server).map_err(|error| {
                        anyhow::anyhow!(
                            "private Codex app-server WebSocket handshake failed: {error}"
                        )
                    })?;
                relay_codex_websockets(
                    client,
                    server,
                    &generation,
                    &thread_id_for_relay,
                    admission,
                    evidence,
                    |completion| {
                        if let Completion::Accepted { input_sequence, .. } = &completion
                            && settlement.settle_if_current(*input_sequence)?
                        {
                            tx.send(Ok(completion)).map_err(anyhow::Error::new)?;
                        }
                        Ok(())
                    },
                )
            });
            if let Err(error) = result {
                eprintln!("native Codex relay failed: {error:#}");
                let _ = tx.send(Err(error));
            }
        });
        File::create(&go_marker)?;
        let result = loop {
            match rx.recv_timeout(Duration::from_secs(1)) {
                Ok(result) => break result,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    break Err(anyhow::anyhow!(
                        "native Codex relay ended without completion"
                    ));
                }
                Err(mpsc::RecvTimeoutError::Timeout) => match terminal.is_live() {
                    Ok(true) => {}
                    Ok(false) => {
                        break Err(anyhow::anyhow!(
                            "native Codex terminal ended without completion"
                        ));
                    }
                    Err(error) => break Err(error),
                },
            }
        };
        stopping.store(true, Ordering::Release);
        let stop = if result.is_ok() {
            terminal.finish()
        } else {
            terminal.stop()
        };
        let joined = relay.join();
        settle_native_result(result, stop, joined)?;
        File::open(&transcript)?.sync_all()?;
        Ok(())
    })();
    if outcome.is_err() {
        let _ = terminal.stop();
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;

    fn answer_config_read(socket: &mut WebSocket<UnixStream>) {
        let read: Value =
            serde_json::from_str(&socket.read().unwrap().into_text().unwrap()).unwrap();
        assert_eq!(read["method"], "config/read");
        let workspace = read["params"]["cwd"].as_str().unwrap();
        let response = json!({"id":2,"result":{"config":{"projects":{
            (workspace): {"trust_level":"trusted"}
        }}}});
        socket.send(Message::Text(response.to_string())).unwrap();
    }
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn native_failure_survives_terminal_settlement_failure() {
        let result: Result<()> = Err(anyhow::anyhow!("TUI protocol send failed"));
        let stop = Err(anyhow::anyhow!(
            "terminal pane does not have its generation-specific sandbox grant"
        ));
        let error = settle_native_result(result, stop, Ok(())).unwrap_err();
        let text = format!("{error:#}");
        assert!(text.starts_with("TUI protocol send failed"), "{text}");
        assert!(text.contains("TUI protocol send failed"), "{text}");
        assert!(text.contains("generation-specific sandbox grant"), "{text}");
    }

    #[test]
    fn required_tui_websocket_send_reports_transport_failure() {
        let (tui_stream, relay_stream) = UnixStream::pair().unwrap();
        let relay = std::thread::spawn(move || {
            let mut socket = tungstenite::accept(relay_stream).unwrap();
            socket.get_ref().shutdown(Shutdown::Write).unwrap();
            send_required_tui_frame(&mut socket, "required-response".into())
                .unwrap_err()
                .to_string()
        });
        let _tui = tungstenite::client("ws://localhost/", tui_stream).unwrap();
        assert!(
            relay
                .join()
                .unwrap()
                .contains("Codex TUI WebSocket protocol send failed")
        );
    }

    fn neighboring_daemon_socket(daemon: &CodexDaemonResources) -> PathBuf {
        daemon.socket.with_extension("s")
    }

    struct NeighboringDaemonFixture {
        file: PathBuf,
        socket: PathBuf,
        file_owned: bool,
        listener: Option<UnixListener>,
    }

    impl NeighboringDaemonFixture {
        fn new(daemon: &CodexDaemonResources) -> Self {
            Self {
                file: daemon.socket.with_extension("unrelated-file"),
                socket: neighboring_daemon_socket(daemon),
                file_owned: false,
                listener: None,
            }
        }

        fn create(&mut self) -> io::Result<()> {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&self.file)?;
            self.file_owned = true;
            file.write_all(b"unrelated")?;
            self.listener = Some(UnixListener::bind(&self.socket)?);
            Ok(())
        }
    }

    impl Drop for NeighboringDaemonFixture {
        fn drop(&mut self) {
            if self.listener.take().is_some() {
                let _ = fs::remove_file(&self.socket);
            }
            if self.file_owned {
                let _ = fs::remove_file(&self.file);
            }
        }
    }

    #[test]
    fn daemon_resources_follow_requested_socket_and_stay_distinct() {
        let first = Path::new("/private/tmp/fluent-na-first/app.sock");
        let second = Path::new("/private/tmp/fluent-na-second/app.sock");
        let first_resources = CodexDaemonResources::paths(first).unwrap();
        let second_resources = CodexDaemonResources::paths(second).unwrap();
        assert_ne!(first_resources.socket, second_resources.socket);
        assert_ne!(first_resources.lock, second_resources.lock);
        assert_eq!(
            first_resources.socket.parent(),
            first_resources.lock.parent()
        );
        assert_eq!(first_resources.lock.extension().unwrap(), "lock");
        assert!(first_resources.socket.file_name().unwrap().len() == 64);
        assert_eq!(
            first_resources.socket.file_name().unwrap(),
            "c1ecc951864a9fdaacf032ec1712231b8be21dd9167980d4a65bcae3247f40af"
        );
    }

    #[test]
    fn daemon_cleanup_removes_only_this_invocation() {
        let requested = tempfile::tempdir().unwrap();
        let resources = CodexDaemonResources {
            socket: requested.path().join("socket"),
            lock: requested.path().join("socket.lock"),
            owned: true,
        };
        let socket = resources.socket.clone();
        let lock = resources.lock.clone();
        let other = resources.socket.with_extension("other");
        fs::write(&resources.socket, "owned").unwrap();
        fs::write(&resources.lock, "owned").unwrap();
        fs::write(&other, "unrelated").unwrap();
        drop(resources);
        assert!(!socket.exists());
        assert!(!lock.exists());
        assert_eq!(fs::read(&other).unwrap(), b"unrelated");
        fs::remove_file(&other).unwrap();
    }

    #[test]
    fn neighboring_daemon_socket_fits_unix_address() {
        let private = tempfile::Builder::new()
            .prefix("fluent-na-regression-")
            .tempdir_in("/private/tmp")
            .unwrap();
        let daemon = CodexDaemonResources::paths(&private.path().join("app.sock")).unwrap();
        let sibling_socket = neighboring_daemon_socket(&daemon);
        let address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
        assert!(
            sibling_socket.as_os_str().as_encoded_bytes().len() < address.sun_path.len(),
            "neighboring daemon socket exceeds the Unix address limit: {}",
            sibling_socket.display()
        );
        assert_eq!(sibling_socket.parent(), daemon.socket.parent());
        assert_ne!(sibling_socket, daemon.socket);
        assert_ne!(sibling_socket, daemon.lock);
        assert!(
            sibling_socket
                .file_name()
                .unwrap()
                .as_encoded_bytes()
                .starts_with(daemon.socket.file_name().unwrap().as_encoded_bytes())
        );
    }

    #[test]
    fn neighboring_daemon_fixture_cleans_up_after_setup_failure() {
        let directory = tempfile::tempdir().unwrap();
        let daemon = CodexDaemonResources {
            socket: directory.path().join("daemon"),
            lock: directory.path().join("daemon.lock"),
            owned: false,
        };
        let mut siblings = NeighboringDaemonFixture::new(&daemon);
        fs::write(&siblings.file, "existing file").unwrap();
        assert_eq!(
            siblings.create().unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        drop(siblings);
        assert_eq!(
            fs::read(directory.path().join("daemon.unrelated-file")).unwrap(),
            b"existing file"
        );

        let daemon = CodexDaemonResources {
            socket: directory.path().join("other"),
            lock: directory.path().join("other.lock"),
            owned: false,
        };
        let siblings = NeighboringDaemonFixture::new(&daemon);
        let file = siblings.file.clone();
        let socket = siblings.socket.clone();
        fs::write(&socket, "existing socket path").unwrap();
        let result = std::panic::catch_unwind(|| {
            let mut siblings = siblings;
            siblings.create().unwrap();
        });
        assert!(result.is_err());
        assert!(!file.exists());
        assert_eq!(fs::read(&socket).unwrap(), b"existing socket path");
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "requires the installed Codex binary and an unsandboxed macOS host"]
    fn installed_codex_starts_with_exact_daemon_grants() {
        use crate::content::ContentResolver;
        use std::os::unix::process::CommandExt;

        let executable = PathBuf::from(std::env::var_os("FLUENT_TEST_INSTALLED_CODEX").unwrap());
        let private = tempfile::Builder::new()
            .prefix("fluent-na-regression-")
            .tempdir_in("/private/tmp")
            .unwrap();
        fs::set_permissions(private.path(), fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(
            fs::metadata(private.path()).unwrap().permissions().mode() & 0o777,
            0o700,
            "fixture Codex home must be private before writing config"
        );
        let terminal = tempfile::Builder::new()
            .prefix("fluent-t-regression-")
            .tempdir_in("/private/tmp")
            .unwrap();
        let workspace = private.path().join("assigned-worktree");
        fs::create_dir(&workspace).unwrap();
        let project_config = workspace.join(".codex");
        fs::create_dir(&project_config).unwrap();
        fs::write(project_config.join("config.toml"),
            "model = \"project-unrelated\"\nmodel_reasoning_effort = \"low\"\n[features]\nhooks = true\n").unwrap();
        let private_config = private.path().join("config.toml");
        fs::write(
            &private_config,
            "[projects.\"/unrelated/source\"]\ntrust_level = \"trusted\"\n",
        )
        .unwrap();
        fs::set_permissions(&private_config, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(
            prepare_native_config(private.path(), &workspace).unwrap(),
            workspace
        );
        let app_socket = private.path().join("app.sock");
        let daemon = CodexDaemonResources::prepare(&app_socket).unwrap();
        let mut siblings = NeighboringDaemonFixture::new(&daemon);
        siblings.create().unwrap();
        let resolver = ContentResolver::new(None);
        let installed_package = executable.canonicalize().unwrap();
        let installed_package = installed_package.parent().unwrap().parent().unwrap();
        let launcher_dir = executable.parent().unwrap();
        let node = Command::new("/usr/bin/which").arg("node").output().unwrap();
        assert!(node.status.success());
        let node = PathBuf::from(String::from_utf8(node.stdout).unwrap().trim());
        let node_dir = node.parent().unwrap();
        let base = crate::os::render_profile_for_scoped_codex_provider_with_tools(
            &resolver,
            private.path(),
            &[private.path().to_path_buf()],
            &[],
            private.path(),
            &[
                installed_package.to_path_buf(),
                launcher_dir.to_path_buf(),
                node_dir.to_path_buf(),
            ],
        )
        .unwrap();
        let profile = crate::os::profile_with_terminal_resources(
            &base,
            terminal.path(),
            &[private.path().to_path_buf()],
            &[daemon.socket.clone()],
            &[daemon.lock.clone()],
        )
        .unwrap();
        let mut server = crate::os::command_under_profile(&profile, &executable);
        server.args([
            "--disable",
            "hooks",
            "--model",
            &std::env::var("FLUENT_NATIVE_CODEX_MODEL").unwrap(),
            "-c",
            &format!(
                "model_reasoning_effort={}",
                std::env::var("FLUENT_NATIVE_CODEX_EFFORT").unwrap()
            ),
            "app-server",
            "--listen",
            &format!("unix://{}", app_socket.display()),
        ]);
        server.env_clear();
        server.env("HOME", private.path());
        server.env("CODEX_HOME", private.path());
        server.env("PATH", std::env::var_os("PATH").unwrap());
        server.current_dir(&workspace);
        server.process_group(0);
        let mut child = server
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let deadline = Instant::now() + Duration::from_secs(12);
            while !app_socket.exists() && Instant::now() < deadline {
                assert!(
                    child.try_wait().unwrap().is_none(),
                    "installed Codex exited before binding"
                );
                std::thread::sleep(Duration::from_millis(25));
            }
            assert!(
                app_socket.exists(),
                "installed Codex did not bind requested socket"
            );
            assert!(
                daemon.socket.exists(),
                "installed Codex did not bind daemon socket"
            );
            assert!(
                daemon.lock.exists(),
                "installed Codex did not create daemon lock"
            );
            assert_eq!(
                fs::metadata(&app_socket).unwrap().permissions().mode() & 0o777,
                0o600
            );
            UnixStream::connect(&app_socket).unwrap();
            assert!(child.try_wait().unwrap().is_none());
            let stream = UnixStream::connect(&app_socket).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let (mut client, _) = tungstenite::client("ws://localhost/", stream).unwrap();
            request(
                &mut client,
                1,
                "initialize",
                json!({"clientInfo":{"name":"fluent-native-config-check","version":"1"}}),
            )
            .unwrap();
            client
                .send(Message::Text("{\"method\":\"initialized\"}".into()))
                .unwrap();
            let effective =
                request(&mut client, 2, "config/read", json!({"cwd": workspace})).unwrap();
            let config = &effective["result"]["config"];
            assert_eq!(config["projects"].as_object().unwrap().len(), 1);
            assert_eq!(
                config["projects"][workspace.to_str().unwrap()]["trust_level"],
                "trusted"
            );
            assert_eq!(
                config["model"],
                std::env::var("FLUENT_NATIVE_CODEX_MODEL").unwrap()
            );
            assert_eq!(
                config["model_reasoning_effort"],
                std::env::var("FLUENT_NATIVE_CODEX_EFFORT").unwrap()
            );
            assert_ne!(config["features"]["hooks"], true);
            assert_eq!(fs::read(&siblings.file).unwrap(), b"unrelated");
            UnixStream::connect(&siblings.socket).unwrap();

            let check = include_str!("../../tests/fixtures/codex-confinement-probe.mjs");
            for (operation, path) in [
                ("read", siblings.file.as_path()),
                ("write", siblings.file.as_path()),
                ("connect", siblings.socket.as_path()),
            ] {
                let mut probe = crate::os::command_under_profile(&profile, &node);
                probe
                    .args(["--input-type=module", "-e", check, operation])
                    .arg(path)
                    .env_clear()
                    .env("HOME", private.path())
                    .env("TMPDIR", private.path())
                    .current_dir(private.path())
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped());
                let mut probe = probe.spawn().unwrap();
                let deadline = Instant::now() + Duration::from_secs(5);
                loop {
                    match probe.try_wait() {
                        Ok(Some(_)) => break,
                        Ok(None) if Instant::now() < deadline => {
                            std::thread::sleep(Duration::from_millis(25));
                        }
                        Ok(None) => {
                            let _ = probe.kill();
                            let _ = probe.wait();
                            panic!("{operation} confinement probe timed out");
                        }
                        Err(error) => {
                            let _ = probe.kill();
                            let _ = probe.wait();
                            panic!("{operation} confinement probe wait failed: {error}");
                        }
                    }
                }
                let output = probe.wait_with_output().unwrap();
                assert!(
                    output.status.success()
                        && String::from_utf8_lossy(&output.stdout)
                            .contains(&format!("{operation}: denied ")),
                    "{operation} confinement probe failed: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
        }));
        unsafe { libc::kill(-(child.id() as i32), libc::SIGTERM) };
        let _ = child.kill();
        let output = child.wait_with_output().unwrap();
        if let Err(error) = result {
            eprintln!(
                "installed Codex stderr: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            std::panic::resume_unwind(error);
        }
    }

    #[test]
    fn capability_check_requires_both_native_protocol_surfaces() {
        let fixture = tempfile::tempdir().unwrap();
        let executable = fixture.path().join("codex");
        fs::write(&executable, "#!/bin/sh\ncase \"$*\" in\n  'app-server --help') echo '--listen unix://' ;;\n  '--help') echo '--remote unix://' ;;\n  *) exit 2 ;;\nesac\n").unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(check_capability(&executable).is_ok());
        fs::write(&executable, "#!/bin/sh\necho '--remote'\n").unwrap();
        assert!(check_capability(&executable).is_err());
    }

    #[test]
    fn native_config_trusts_only_canonical_assigned_worktree() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("worker");
        let workspace = root.path().join("work \"assigned\"");
        fs::create_dir(&home).unwrap();
        fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).unwrap();
        fs::create_dir(&workspace).unwrap();
        let source = root.path().join("source");
        fs::create_dir(&source).unwrap();
        let original = toml::to_string(&toml::toml! {
            [tui]
            notifications = ["task-complete"]
            [projects."/unrelated/source"]
            trust_level = "trusted"
            [features]
            hooks = true
            model = "unrelated-model"
        })
        .unwrap();
        let path = home.join("config.toml");
        fs::write(&path, original).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let canonical = prepare_native_config(&home, &workspace).unwrap();
        assert_eq!(canonical, workspace.canonicalize().unwrap());
        let parsed: toml::Table = fs::read_to_string(&path).unwrap().parse().unwrap();
        assert_eq!(parsed["projects"].as_table().unwrap().len(), 1);
        assert_eq!(
            parsed["projects"][canonical.to_str().unwrap()]["trust_level"].as_str(),
            Some("trusted")
        );
        assert_eq!(
            parsed["tui"]["notifications"][0].as_str(),
            Some("task-complete")
        );
        assert!(parsed.get("features").is_none());
        assert!(parsed["projects"].get(source.to_str().unwrap()).is_none());
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn native_config_rejects_an_existing_symlink() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("worker");
        let workspace = root.path().join("assigned");
        fs::create_dir(&home).unwrap();
        fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).unwrap();
        fs::create_dir(&workspace).unwrap();
        let unrelated = root.path().join("unrelated.toml");
        fs::write(&unrelated, "[tui]\nnotifications = []\n").unwrap();
        std::os::unix::fs::symlink(&unrelated, home.join("config.toml")).unwrap();
        assert!(prepare_native_config(&home, &workspace).is_err());
        assert_eq!(
            fs::read_to_string(unrelated).unwrap(),
            "[tui]\nnotifications = []\n"
        );
    }

    #[test]
    fn bootstrap_binds_one_assigned_thread_before_tui_starts() {
        let workspace = tempfile::tempdir().unwrap();
        let (owner, provider) = UnixStream::pair().unwrap();
        let fake = std::thread::spawn(move || {
            let mut socket = tungstenite::accept(provider).unwrap();
            let initialize: Value =
                serde_json::from_str(&socket.read().unwrap().into_text().unwrap()).unwrap();
            assert_eq!(initialize["method"], "initialize");
            socket
                .send(Message::Text("{\"id\":1,\"result\":{}}".into()))
                .unwrap();
            assert_eq!(
                serde_json::from_str::<Value>(&socket.read().unwrap().into_text().unwrap())
                    .unwrap()["method"],
                "initialized"
            );
            answer_config_read(&mut socket);
            let start: Value =
                serde_json::from_str(&socket.read().unwrap().into_text().unwrap()).unwrap();
            assert_eq!(start["method"], "thread/start");
            assert_eq!(start["params"]["model"], "gpt-assigned");
            assert_eq!(start["params"]["approvalPolicy"], "never");
            socket
                .send(Message::Text(
                    "{\"id\":3,\"result\":{\"thread\":{\"id\":\"assigned-thread\"}}}".into(),
                ))
                .unwrap();
        });
        let (mut owner, _) = tungstenite::client("ws://localhost/", owner).unwrap();
        let (thread, _) = bootstrap_protocol(
            &mut owner,
            &workspace.path().canonicalize().unwrap(),
            "gpt-assigned",
            CoderSession::FreshPersistent,
        )
        .unwrap();
        assert_eq!(thread, "assigned-thread");
        fake.join().unwrap();
    }

    #[test]
    fn bootstrap_starts_ephemeral_reviewer_thread() {
        let workspace = tempfile::tempdir().unwrap();
        let (owner, provider) = UnixStream::pair().unwrap();
        let fake = std::thread::spawn(move || {
            let mut socket = tungstenite::accept(provider).unwrap();
            let _: Value =
                serde_json::from_str(&socket.read().unwrap().into_text().unwrap()).unwrap();
            socket
                .send(Message::Text("{\"id\":1,\"result\":{}}".into()))
                .unwrap();
            let _: Value =
                serde_json::from_str(&socket.read().unwrap().into_text().unwrap()).unwrap();
            answer_config_read(&mut socket);
            let start: Value =
                serde_json::from_str(&socket.read().unwrap().into_text().unwrap()).unwrap();
            assert_eq!(start["method"], "thread/start");
            assert_eq!(start["params"]["ephemeral"], true);
            socket
                .send(Message::Text(
                    "{\"id\":3,\"result\":{\"thread\":{\"id\":\"review-thread\"}}}".into(),
                ))
                .unwrap();
        });
        let (mut owner, _) = tungstenite::client("ws://localhost/", owner).unwrap();
        let (thread, _) = bootstrap_protocol(
            &mut owner,
            &workspace.path().canonicalize().unwrap(),
            "gpt-review",
            CoderSession::Ephemeral,
        )
        .unwrap();
        assert_eq!(thread, "review-thread");
        fake.join().unwrap();
    }

    #[test]
    fn bootstrap_rejects_source_trust_before_assigned_thread() {
        let workspace = tempfile::tempdir().unwrap();
        let (owner, provider) = UnixStream::pair().unwrap();
        let fake = std::thread::spawn(move || {
            let mut socket = tungstenite::accept(provider).unwrap();
            let initialize: Value =
                serde_json::from_str(&socket.read().unwrap().into_text().unwrap()).unwrap();
            assert_eq!(initialize["method"], "initialize");
            socket
                .send(Message::Text(json!({"id":1,"result":{}}).to_string()))
                .unwrap();
            let initialized: Value =
                serde_json::from_str(&socket.read().unwrap().into_text().unwrap()).unwrap();
            assert_eq!(initialized["method"], "initialized");
            let read: Value =
                serde_json::from_str(&socket.read().unwrap().into_text().unwrap()).unwrap();
            assert_eq!(read["method"], "config/read");
            socket
                .send(Message::Text(
                    json!({"id":2,"result":{"config":{"projects":{
                        "/unrelated/source": {"trust_level":"trusted"}
                    }}}})
                    .to_string(),
                ))
                .unwrap();
            socket
                .get_mut()
                .set_read_timeout(Some(Duration::from_millis(100)))
                .unwrap();
            assert!(socket.read().is_err(), "unexpected assigned thread launch");
        });
        let (mut owner, _) = tungstenite::client("ws://localhost/", owner).unwrap();
        assert!(
            bootstrap_protocol(
                &mut owner,
                &workspace.path().canonicalize().unwrap(),
                "gpt-assigned",
                CoderSession::FreshPersistent
            )
            .unwrap_err()
            .to_string()
            .contains("did not trust only")
        );
        fake.join().unwrap();
    }

    #[test]
    fn bootstrap_rejects_a_different_resumed_thread() {
        let workspace = tempfile::tempdir().unwrap();
        let (owner, provider) = UnixStream::pair().unwrap();
        let fake = std::thread::spawn(move || {
            let mut socket = tungstenite::accept(provider).unwrap();
            for _ in 0..4 {
                let message: Value =
                    serde_json::from_str(&socket.read().unwrap().into_text().unwrap()).unwrap();
                match message["method"].as_str().unwrap() {
                    "initialize" => socket
                        .send(Message::Text("{\"id\":1,\"result\":{}}".into()))
                        .unwrap(),
                    "initialized" => {}
                    "config/read" => {
                        let path = message["params"]["cwd"].as_str().unwrap();
                        socket
                            .send(Message::Text(
                                json!({"id":2,"result":{"config":{"projects":{
                                    (path): {"trust_level":"trusted"}
                                }}}})
                                .to_string(),
                            ))
                            .unwrap();
                    }
                    "thread/resume" => {
                        assert_eq!(message["params"]["threadId"], "assigned-thread");
                        socket
                            .send(Message::Text(
                                "{\"id\":3,\"result\":{\"thread\":{\"id\":\"another-thread\"}}}"
                                    .into(),
                            ))
                            .unwrap();
                    }
                    method => panic!("unexpected method {method}"),
                }
            }
        });
        let (mut owner, _) = tungstenite::client("ws://localhost/", owner).unwrap();
        assert!(
            bootstrap_protocol(
                &mut owner,
                &workspace.path().canonicalize().unwrap(),
                "gpt-assigned",
                CoderSession::Resume("assigned-thread"),
            )
            .is_err()
        );
        fake.join().unwrap();
    }
}
