//! Launch one assigned Codex conversation behind the Task's terminal owner.

use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::net::Shutdown;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, mpsc};
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

fn app_server_command(
    executable: &Path,
    model: &str,
    effort: Option<&str>,
    app_socket: &Path,
) -> Result<Command> {
    fn config_string(value: &str) -> Result<String> {
        let config = toml::to_string(&toml::Table::from_iter([(
            "value".to_owned(),
            toml::Value::String(value.to_owned()),
        )]))?;
        Ok(config
            .strip_prefix("value = ")
            .context("encode native Codex config string")?
            .trim_end()
            .to_owned())
    }

    ensure!(!model.is_empty(), "native Codex model is unresolved");
    let mut server = Command::new(executable);
    server.args(["--disable", "hooks"]);
    // App-server reads trusted project config; CLI config overrides must win.
    server.args(["-c", &format!("model={}", config_string(model)?)]);
    if let Some(effort) = effort {
        server.args([
            "-c",
            &format!("model_reasoning_effort={}", config_string(effort)?),
        ]);
    }
    server.args(["-c", "features.hooks=false"]);
    server.args([
        "app-server",
        "--listen",
        &format!("unix://{}", app_socket.display()),
    ]);
    Ok(server)
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
    generation_file: &Path,
    client_exit: &Path,
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
    let server = app_server_command(
        &coder.executable,
        coder
            .model_override
            .as_deref()
            .context("native Codex model is unresolved")?,
        coder.effort.as_deref(),
        app_socket,
    )?;
    let client_exit_tmp = client_exit.with_extension("tmp");
    Ok(format!(
        "#!/bin/sh\nset -eu\numask 077\n{} >{} 2>&1 &\nserver=$!\nfor n in $(seq 1 200); do\n  [ -S {} ] && break\n  kill -0 \"$server\" 2>/dev/null || exit 1\n  sleep 0.05\ndone\n[ -S {} ] || exit 1\nfor n in $(seq 1 1200); do\n  [ -f {} ] && break\n  kill -0 \"$server\" 2>/dev/null || exit 1\n  sleep 0.05\ndone\n[ -f {} ] || exit 1\nassigned_thread=$(cat {})\nassigned_generation=$(cat {})\n[ -n \"$assigned_thread\" ] && [ -n \"$assigned_generation\" ] || exit 1\nif {}; then client_status=0; else client_status=$?; fi\nprintf '%s\\t%s\\t%d\\n' \"$assigned_generation\" \"$assigned_thread\" \"$client_status\" >{}\nmv {} {}\n# Keep the pane briefly so its final native display can be classified.\nsleep 5\nexit \"$client_status\"\n",
        command_line(&server)?,
        quote(&provider_log.display().to_string()),
        quote(&app_socket.display().to_string()),
        quote(&app_socket.display().to_string()),
        quote(&go_marker.display().to_string()),
        quote(&go_marker.display().to_string()),
        quote(&thread_file.display().to_string()),
        quote(&generation_file.display().to_string()),
        command_line(&command)?.replace(&quote(placeholder), "\"$assigned_thread\""),
        quote(&client_exit_tmp.display().to_string()),
        quote(&client_exit_tmp.display().to_string()),
        quote(&client_exit.display().to_string()),
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
    effort: Option<&str>,
    session: CoderSession<'_>,
) -> Result<(String, Value, Value)> {
    // The native TUI uses this same connection after bootstrap, including its
    // experimental thread and turn requests.
    let initialized = request(
        socket,
        1,
        "initialize",
        json!({
            "clientInfo": {"name": "fluent-native-task", "version": env!("CARGO_PKG_VERSION")},
            "capabilities": {"experimentalApi": true}
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
    let effective = &config["result"]["config"];
    ensure!(
        effective["model"].as_str() == Some(model),
        "Codex app-server did not select the assigned model"
    );
    if let Some(effort) = effort {
        ensure!(
            effective["model_reasoning_effort"].as_str() == Some(effort),
            "Codex app-server did not select the assigned effort"
        );
    }
    ensure!(
        effective["features"]["hooks"] == false,
        "Codex app-server did not disable hooks"
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
                // An empty thread has no rollout to page yet. The installed
                // TUI can display a legacy empty history and then own its
                // first real user turn on this same app-server connection.
                "historyMode": "legacy",
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
    Ok((thread_id.to_owned(), result, initialized))
}

fn bootstrap(
    app_socket: &Path,
    workspace: &Path,
    model: &str,
    effort: Option<&str>,
    session: CoderSession<'_>,
) -> Result<(String, Value, Value, WebSocket<UnixStream>)> {
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
    let (thread_id, result, initialized) =
        bootstrap_protocol(&mut socket, workspace, model, effort, session)?;
    Ok((thread_id, result, initialized, socket))
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

fn provider_startup_log(socket_dir: &Path) -> PathBuf {
    socket_dir.join("native-provider-stderr.log")
}

fn archive_provider_startup_log(log: &Path, artifact: &Path) -> Result<PathBuf> {
    const MAX_DIAGNOSTIC_BYTES: u64 = 1024 * 1024;
    let archived = artifact.join("native-provider-stderr.log");
    let source = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(log)
    {
        Ok(source) => Some(source),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error).context("open native Codex provider stderr"),
    };
    let mut destination = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&archived)
        .context("create archived native Codex provider stderr")?;
    if let Some(source) = source {
        ensure!(
            source.metadata()?.file_type().is_file(),
            "native Codex provider stderr is not a regular file"
        );
        let copied = std::io::copy(&mut source.take(MAX_DIAGNOSTIC_BYTES + 1), &mut destination)
            .context("archive native Codex provider stderr")?;
        if copied > MAX_DIAGNOSTIC_BYTES {
            destination.set_len(MAX_DIAGNOSTIC_BYTES)?;
            destination.seek(SeekFrom::Start(MAX_DIAGNOSTIC_BYTES))?;
            destination.write_all(b"\n[provider stderr truncated]\n")?;
        }
    } else {
        destination.write_all(b"native Codex pane produced no provider output\n")?;
    }
    destination.sync_all()?;
    Ok(archived)
}

fn write_bootstrap_evidence(
    path: &Path,
    generation: &str,
    thread_id: &str,
    event: &Value,
    session: CoderSession<'_>,
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
            "launch_method": if matches!(session, CoderSession::Resume(_)) {
                "thread/resume"
            } else {
                "thread/start"
            },
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

struct DisplaySpool {
    state: Mutex<DisplaySpoolState>,
    ready: Condvar,
}

struct DisplaySpoolState {
    file: File,
    read_offset: u64,
    write_offset: u64,
    closed: bool,
}

impl DisplaySpool {
    fn new() -> Result<Self> {
        Ok(Self {
            state: Mutex::new(DisplaySpoolState {
                file: tempfile::tempfile().context("create native Codex display spool")?,
                read_offset: 0,
                write_offset: 0,
                closed: false,
            }),
            ready: Condvar::new(),
        })
    }

    fn push(&self, text: String) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("Codex display spool poisoned"))?;
        ensure!(!state.closed, "Codex display spool closed");
        let bytes = text.as_bytes();
        let length = u64::try_from(bytes.len()).context("Codex display frame too large")?;
        let offset = state.write_offset;
        state.file.seek(SeekFrom::Start(offset))?;
        state.file.write_all(&length.to_le_bytes())?;
        state.file.write_all(bytes)?;
        state.write_offset = offset + 8 + length;
        self.ready.notify_one();
        Ok(())
    }

    fn pop(&self) -> Result<Option<String>> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("Codex display spool poisoned"))?;
        loop {
            if state.read_offset < state.write_offset {
                let offset = state.read_offset;
                state.file.seek(SeekFrom::Start(offset))?;
                let mut length = [0; 8];
                state.file.read_exact(&mut length)?;
                let length = usize::try_from(u64::from_le_bytes(length))
                    .context("Codex display frame exceeds address space")?;
                let mut bytes = vec![0; length];
                state.file.read_exact(&mut bytes)?;
                state.read_offset = offset + 8 + length as u64;
                return Ok(Some(String::from_utf8(bytes)?));
            }
            if state.closed {
                return Ok(None);
            }
            state = self
                .ready
                .wait(state)
                .map_err(|_| anyhow::anyhow!("Codex display spool poisoned"))?;
        }
    }

    fn close(&self) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("Codex display spool poisoned"))?;
        state.closed = true;
        self.ready.notify_all();
        Ok(())
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
    evidence: W,
    on_completion: F,
) -> Result<()>
where
    W: Write + Send,
    F: FnMut(Completion) -> Result<()> + Send,
{
    relay_codex_websockets_with_trace(
        client,
        server,
        generation,
        thread_id,
        admission,
        evidence,
        on_completion,
        std::io::sink(),
    )
}

// Keep only protocol direction and method names. Request bodies, response
// bodies, pane bytes, and provider errors can contain private credentials.
fn trace_relay<T: Write>(
    trace: &Mutex<(T, usize)>,
    generation: &str,
    thread_id: &str,
    direction: &str,
    outcome: &str,
    method: Option<&str>,
    input_sequence: u64,
) -> Result<()> {
    trace_relay_with_read_flag(
        trace,
        generation,
        thread_id,
        direction,
        outcome,
        method,
        input_sequence,
        None,
    )
}

fn trace_relay_with_read_flag<T: Write>(
    trace: &Mutex<(T, usize)>,
    generation: &str,
    thread_id: &str,
    direction: &str,
    outcome: &str,
    method: Option<&str>,
    input_sequence: u64,
    include_turns: Option<bool>,
) -> Result<()> {
    let mut trace = trace
        .lock()
        .map_err(|_| anyhow::anyhow!("Codex relay trace lock poisoned"))?;
    if trace.1 >= 272 || (trace.1 >= 256 && matches!(outcome, "received" | "forwarded")) {
        return Ok(());
    }
    let safe_method = method.filter(|method| {
        method.len() <= 64
            && method
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'_' | b'-'))
    });
    serde_json::to_writer(
        &mut trace.0,
        &json!({
            "execution_generation": generation,
            "assigned_conversation_id": thread_id,
            "direction": direction,
            "outcome": outcome,
            "method": safe_method.unwrap_or("<other>"),
            "input_sequence": input_sequence,
            "include_turns": include_turns,
        }),
    )?;
    trace.0.write_all(b"\n")?;
    trace.0.flush()?;
    trace.1 += 1;
    Ok(())
}

/// Retain bounded, payload-free diagnostics for the production relay.
pub fn relay_codex_websockets_with_trace<W, F, T>(
    client: WebSocket<UnixStream>,
    server: WebSocket<UnixStream>,
    generation: &str,
    thread_id: &str,
    admission: Arc<NativeAdmission>,
    evidence: W,
    on_completion: F,
    trace: T,
) -> Result<()>
where
    W: Write + Send,
    F: FnMut(Completion) -> Result<()> + Send,
    T: Write + Send,
{
    relay_codex_websockets_with_handoff(
        client,
        server,
        generation,
        thread_id,
        admission,
        evidence,
        on_completion,
        trace,
        None,
    )
}

struct CodexHandoff {
    initialized: Value,
    thread: Value,
    fresh: bool,
}

// The installed TUI reads full history immediately after attaching. A new
// app-server thread has no rollout yet, so includeTurns fails until its first
// real prompt. Return only the provider's empty thread object to the display;
// the canonical transcript still contains only provider messages.
fn fresh_thread_display_read(start: &Value, request: &Value, assigned: &str) -> Result<Value> {
    ensure!(
        request["params"]["threadId"] == assigned,
        "Codex TUI read another thread"
    );
    ensure!(
        request["params"]["includeTurns"] == true,
        "Codex TUI display read did not request thread history"
    );
    let thread = &start["result"]["thread"];
    ensure!(
        thread["id"] == assigned,
        "Codex start response changed thread identity"
    );
    ensure!(
        thread["turns"].as_array().is_some_and(Vec::is_empty),
        "Codex fresh thread has nonempty history"
    );
    Ok(json!({"id": request["id"], "result": {"thread": thread}}))
}

// A paginated TUI asks for the first page even when the provider has only
// created an in-memory thread. It has no rollout to page through yet.
fn fresh_thread_display_page(start: &Value, request: &Value, assigned: &str) -> Result<Value> {
    ensure!(
        request["params"]["threadId"] == assigned,
        "Codex TUI paged another thread"
    );
    ensure!(
        request["params"]["cursor"].is_null(),
        "Codex TUI requested a later page of an empty thread"
    );
    let thread = &start["result"]["thread"];
    ensure!(
        thread["id"] == assigned,
        "Codex start response changed thread identity"
    );
    ensure!(
        thread["turns"].as_array().is_some_and(Vec::is_empty),
        "Codex fresh thread has nonempty history"
    );
    Ok(json!({"id": request["id"], "result": {
        "data": [], "nextCursor": null, "backwardsCursor": null
    }}))
}

/// The TUI opens a new connection to Fluent, but the app-server connection
/// that started its empty thread must remain loaded. Codex cannot resume that
/// thread by id until its first real turn creates a rollout. Complete the
/// TUI's handshake from the provider's original responses, then forward its
/// first prompt and later requests over the subscribed bootstrap connection.
fn relay_codex_websockets_with_handoff<W, F, T>(
    client: WebSocket<UnixStream>,
    server: WebSocket<UnixStream>,
    generation: &str,
    thread_id: &str,
    admission: Arc<NativeAdmission>,
    evidence: W,
    mut on_completion: F,
    trace: T,
    handoff: Option<CodexHandoff>,
) -> Result<()>
where
    W: Write + Send,
    F: FnMut(Completion) -> Result<()> + Send,
    T: Write + Send,
{
    let trace = Arc::new(Mutex::new((trace, 0usize)));
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
    let evidence = Arc::new(Mutex::new(evidence));
    // Keep protocol frames in order without making evidence capture wait for
    // the TUI. The temporary file bounds the relay's memory under backpressure.
    let display_spool = Arc::new(DisplaySpool::new()?);
    std::thread::scope(|scope| {
        let display_client = Arc::clone(&client);
        let display_trace = Arc::clone(&trace);
        let display_gate = Arc::clone(&gate);
        let display_queue = Arc::clone(&display_spool);
        let display = scope.spawn(move || -> Result<()> {
            let result = (|| {
                while let Some(text) = display_queue.pop()? {
                    let mut client = display_client
                        .lock()
                        .map_err(|_| anyhow::anyhow!("Codex TUI WebSocket lock poisoned"))?;
                    if let Err(error) = send_required_tui_frame(&mut client, text) {
                        let sequence = display_gate
                            .lock()
                            .map_err(|_| anyhow::anyhow!("native Codex relay gate poisoned"))?
                            .input_sequence;
                        trace_relay(
                            &display_trace,
                            generation,
                            thread_id,
                            "app-to-tui",
                            "send-error",
                            None,
                            sequence,
                        )?;
                        return Err(error);
                    }
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
        let request_evidence = Arc::clone(&evidence);
        let request_trace = Arc::clone(&trace);
        let bootstrap_display = Arc::clone(&display_spool);
        let upstream = scope.spawn(move || -> Result<()> {
            let mut handoff = handoff.map(|handoff| (handoff, false, false, false));
            let result = (|| {
                loop {
                    let line = match read_socket(&upstream_client) {
                        Err(error) => {
                            let sequence = request_gate.lock().map_err(|_| anyhow::anyhow!("native Codex relay gate poisoned"))?.input_sequence;
                            trace_relay(&request_trace, generation, thread_id, "tui-to-app", "read-error", None, sequence)?;
                            return Err(error).context("reading native Codex TUI requests");
                        }
                        Ok(SocketRead::Closed) => {
                            let sequence = request_gate.lock().map_err(|_| anyhow::anyhow!("native Codex relay gate poisoned"))?.input_sequence;
                            trace_relay(&request_trace, generation, thread_id, "tui-to-app", "closed", None, sequence)?;
                            return Ok(());
                        }
                        Ok(SocketRead::Pending) => {
                            std::thread::sleep(Duration::from_millis(1));
                            continue;
                        }
                        Ok(SocketRead::Text(line)) => line,
                    };
                    let request: Value = match serde_json::from_str(&line) {
                        Ok(request) => request,
                        Err(error) => {
                            let sequence = request_gate.lock().map_err(|_| anyhow::anyhow!("native Codex relay gate poisoned"))?.input_sequence;
                            trace_relay(&request_trace, generation, thread_id, "tui-to-app", "decode-error", None, sequence)?;
                            return Err(error.into());
                        }
                    };
                    let method = request["method"].as_str().unwrap_or("<missing>");
                    let sequence = request_gate.lock()
                        .map_err(|_| anyhow::anyhow!("native Codex relay gate poisoned"))?
                        .input_sequence;
                    trace_relay(&request_trace, generation, thread_id, "tui-to-app", "received", Some(method), sequence)?;
                    let (admitted, next_sequence) = {
                        let mut gate = request_gate.lock()
                            .map_err(|_| anyhow::anyhow!("native Codex relay gate poisoned"))?;
                        let admitted = gate.admit_request(&request);
                        (admitted, gate.input_sequence)
                    };
                    if admitted.as_ref().is_ok_and(Option::is_none) {
                        eprintln!("native Codex TUI request: {method}");
                    }
                    if admitted.as_ref().is_ok_and(Option::is_some) {
                        eprintln!("native Codex TUI admitted input: {method}");
                    }
                    if let Err(error) = admitted {
                        trace_relay(&request_trace, generation, thread_id, "tui-to-app", "rejected", Some(method), next_sequence)?;
                        if let Some(id) = request.get("id") {
                            let response = json!({"id": id, "error": {"code": -32000, "message": error.to_string()}});
                            upstream_client.lock()
                                .map_err(|_| anyhow::anyhow!("Codex WebSocket lock poisoned"))?
                                .send(Message::Text(response.to_string()))?;
                            continue;
                        }
                        return Err(error);
                    }
                    if let Some((responses, initialized, acknowledged, resumed)) = &mut handoff {
                        let reply = match method {
                            "initialize" => {
                                ensure!(!*initialized, "Codex TUI initialized twice");
                                *initialized = true;
                                Some(&responses.initialized)
                            }
                            "initialized" => {
                                ensure!(*initialized && !*acknowledged, "Codex TUI initialization order changed");
                                *acknowledged = true;
                                None
                            }
                            "thread/resume" => {
                                ensure!(*acknowledged && !*resumed, "Codex TUI resumed outside its bootstrap handoff");
                                *resumed = true;
                                Some(&responses.thread)
                            }
                            _ => {
                                ensure!(*acknowledged, "Codex TUI sent a request before initialization");
                                if method.starts_with("turn/") {
                                    ensure!(*resumed, "Codex TUI sent input before its assigned thread was attached");
                                }
                                None
                            }
                        };
                        if method == "initialized" {
                            trace_relay(&request_trace, generation, thread_id, "tui-to-app", "bootstrap-acknowledged", Some(method), next_sequence)?;
                            continue;
                        }
                        if let Some(reply) = reply {
                            let id = request.get("id").filter(|id| !id.is_null())
                                .context("Codex TUI bootstrap request omitted its id")?;
                            let mut reply = reply.clone();
                            reply["id"] = id.clone();
                            bootstrap_display.push(reply.to_string())
                                .context("queue Codex TUI bootstrap response")?;
                            trace_relay(&request_trace, generation, thread_id, "tui-to-app", "bootstrap-replied", Some(method), next_sequence)?;
                            continue;
                        }
                        if responses.fresh && *resumed && method == "thread/read"
                            && request["params"]["includeTurns"] == true
                            && next_sequence == 0 {
                            let reply = fresh_thread_display_read(&responses.thread, &request, thread_id)?;
                            bootstrap_display.push(reply.to_string())
                                .context("queue Codex TUI display-only read")?;
                            trace_relay(&request_trace, generation, thread_id, "tui-to-app", "ui-only-replied", Some(method), next_sequence)?;
                            continue;
                        }
                        if responses.fresh && *resumed && method == "thread/turns/list"
                            && next_sequence == 0 {
                            let reply = fresh_thread_display_page(&responses.thread, &request, thread_id)?;
                            bootstrap_display.push(reply.to_string())
                                .context("queue Codex TUI display-only page")?;
                            trace_relay(&request_trace, generation, thread_id, "tui-to-app", "ui-only-replied", Some(method), next_sequence)?;
                            continue;
                        }
                    }
                    if let Ok(Some(input_sequence)) = admitted {
                        let record = CodexEvidenceRecord {
                            format: "fluent-native-agent-event",
                            schema_version: 1,
                            provider: "codex",
                            execution_generation: generation,
                            assigned_conversation_id: thread_id,
                            conversation_id: thread_id,
                            turn_id: request["params"]["turnId"].as_str(),
                            input_sequence,
                            event: &request,
                        };
                        let mut evidence = request_evidence.lock()
                            .map_err(|_| anyhow::anyhow!("native Codex evidence lock poisoned"))?;
                        serde_json::to_writer(&mut *evidence, &record)?;
                        evidence.write_all(b"\n")?;
                        evidence.flush()?;
                    }
                    let forwarded = upstream_server.lock()
                        .map_err(|_| anyhow::anyhow!("Codex WebSocket lock poisoned"))?
                        .send(Message::Text(line));
                    if let Err(error) = forwarded {
                        trace_relay(&request_trace, generation, thread_id, "tui-to-app", "send-error", Some(method), next_sequence)?;
                        return Err(error).with_context(|| format!("forwarding native Codex TUI {method} to app-server"));
                    }
                    trace_relay_with_read_flag(
                        &request_trace, generation, thread_id, "tui-to-app", "forwarded",
                        Some(method), next_sequence,
                        if method == "thread/read" { request["params"]["includeTurns"].as_bool() } else { None },
                    )?;
                }
            })();
            let _ = client_abort.shutdown(Shutdown::Both);
            let _ = server_abort.shutdown(Shutdown::Both);
            result
        });
        let downstream_server = Arc::clone(&server);
        let response_gate = Arc::clone(&gate);
        let response_evidence = Arc::clone(&evidence);
        let response_trace = Arc::clone(&trace);
        let response_display = Arc::clone(&display_spool);
        let downstream = scope.spawn(move || -> Result<()> {
            let result = (|| {
                loop {
                    let line = match read_socket(&downstream_server) {
                        Err(error) => {
                            let sequence = response_gate
                                .lock()
                                .map_err(|_| anyhow::anyhow!("native Codex relay gate poisoned"))?
                                .input_sequence;
                            trace_relay(
                                &response_trace,
                                generation,
                                thread_id,
                                "app-to-tui",
                                "read-error",
                                None,
                                sequence,
                            )?;
                            return Err(error).context("reading native Codex app-server events");
                        }
                        Ok(SocketRead::Closed) => {
                            let sequence = response_gate
                                .lock()
                                .map_err(|_| anyhow::anyhow!("native Codex relay gate poisoned"))?
                                .input_sequence;
                            trace_relay(
                                &response_trace,
                                generation,
                                thread_id,
                                "app-to-tui",
                                "closed",
                                None,
                                sequence,
                            )?;
                            return Ok(());
                        }
                        Ok(SocketRead::Pending) => {
                            std::thread::sleep(Duration::from_millis(1));
                            continue;
                        }
                        Ok(SocketRead::Text(line)) => line,
                    };
                    let event: Value = match serde_json::from_str(&line) {
                        Ok(event) => event,
                        Err(error) => {
                            let sequence = response_gate
                                .lock()
                                .map_err(|_| anyhow::anyhow!("native Codex relay gate poisoned"))?
                                .input_sequence;
                            trace_relay(
                                &response_trace,
                                generation,
                                thread_id,
                                "app-to-tui",
                                "decode-error",
                                None,
                                sequence,
                            )?;
                            return Err(error.into());
                        }
                    };
                    let (observed, input_sequence) = {
                        let mut gate = response_gate
                            .lock()
                            .map_err(|_| anyhow::anyhow!("native Codex relay gate poisoned"))?;
                        (
                            gate.observe_event_for_generation(generation, &event),
                            gate.input_sequence,
                        )
                    };
                    let completion = match observed {
                        Ok(completion) => completion,
                        Err(error) => {
                            trace_relay(
                                &response_trace,
                                generation,
                                thread_id,
                                "app-to-tui",
                                "rejected",
                                event["method"].as_str(),
                                input_sequence,
                            )?;
                            return Err(error);
                        }
                    };
                    if input_sequence == 0 {
                        eprintln!(
                            "native Codex app-server message before turn: {}",
                            event["method"].as_str().unwrap_or("response")
                        );
                    }
                    trace_relay(
                        &response_trace,
                        generation,
                        thread_id,
                        "app-to-tui",
                        "received",
                        event["method"].as_str(),
                        input_sequence,
                    )?;
                    // JSON-RPC responses can include large UI catalogs such as
                    // plugin/list. They do not supply turn evidence; the gate
                    // has already checked input errors and completion above.
                    if event.get("id").is_none() {
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
                        let mut evidence = response_evidence
                            .lock()
                            .map_err(|_| anyhow::anyhow!("native Codex evidence lock poisoned"))?;
                        serde_json::to_writer(&mut *evidence, &record)?;
                        evidence.write_all(b"\n")?;
                        evidence.flush()?;
                    }
                    response_display
                        .push(line)
                        .context("queue Codex TUI protocol frame")?;
                    if completion != Completion::Ignored {
                        on_completion(completion)?;
                    }
                }
            })();
            let _ = downstream_abort_client.shutdown(Shutdown::Both);
            let _ = downstream_abort_server.shutdown(Shutdown::Both);
            result
        });
        let upstream = upstream.join();
        let downstream = downstream.join();
        display_spool.close()?;
        let display = display
            .join()
            .map_err(|_| anyhow::anyhow!("Codex WebSocket display relay panicked"))?;
        let upstream =
            upstream.map_err(|_| anyhow::anyhow!("Codex WebSocket request relay panicked"))?;
        let downstream =
            downstream.map_err(|_| anyhow::anyhow!("Codex WebSocket event relay panicked"))?;
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

fn add_fixture_cleanup<T>(result: Result<T>, label: &str, cleanup: Result<()>) -> Result<T> {
    match (result, cleanup) {
        (Err(primary), Err(error)) => Err(NativeFailureWithSettlement {
            primary,
            secondary: format!("{label} also failed: {error:#}"),
        }
        .into()),
        (Ok(_), Err(error)) => Err(error.context(format!("{label} failed"))),
        (result, Ok(())) => result,
    }
}

fn parse_client_exit(status: &str, generation: &str, thread_id: &str) -> Result<i32> {
    let fields: Vec<_> = status.trim_end().split('\t').collect();
    ensure!(
        fields.len() == 3 && fields[0] == generation && fields[1] == thread_id,
        "native Codex client exit belongs to another invocation"
    );
    fields[2]
        .parse::<i32>()
        .context("parse native Codex client exit status")
}

fn recognized_pane_output(snapshot: Option<&str>) -> Vec<&'static str> {
    [
        "Connection reset without closing handshake",
        "Failed to connect",
        "Authentication failed",
        "Unauthorized",
        "Permission denied",
        "Model not found",
        "Unknown model",
        "Error: Permission overrides are not supported when resuming a remote task.",
    ]
    .into_iter()
    .filter(|phrase| snapshot.is_some_and(|text| text.contains(phrase)))
    .collect()
}

fn record_client_diagnostic(
    artifact: &Path,
    status_path: &Path,
    generation: &str,
    thread_id: &str,
    terminal: &TerminalSession,
) -> Result<Option<i32>> {
    let exit_status = match fs::read_to_string(status_path) {
        Ok(status) => Some(parse_client_exit(&status, generation, thread_id)?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error).context("read native Codex client exit"),
    };
    let snapshot = terminal.capture_pane_snapshot().ok();
    let output_signals = recognized_pane_output(snapshot.as_deref());
    let mut file = OpenOptions::new()
        .append(true)
        .create(true)
        .open(artifact.join("native-codex-client-diagnostics.jsonl"))?;
    serde_json::to_writer(
        &mut file,
        &json!({
            "execution_generation": generation,
            "assigned_conversation_id": thread_id,
            "exit_status": exit_status,
            "pane_snapshot_available": snapshot.is_some(),
            "recognized_output": output_signals,
        }),
    )?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(exit_status)
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
    let generation_file = sockets.path().join("assigned-generation");
    let script_path = artifact.join("native-codex-pane.sh");
    // Learner artifacts are host-owned and read-only to the provider. Redirect
    // before app-server starts into the invocation's writable socket directory;
    // the owner archives the log after the terminal has settled.
    let provider_log = provider_startup_log(sockets.path());
    let client_exit = sockets.path().join("client-exit");
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
        &generation_file,
        &client_exit,
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
    )
    .map_err(
        |error| match archive_provider_startup_log(&provider_log, artifact) {
            Ok(path) => error.context(format!("native Codex provider stderr: {}", path.display())),
            Err(archive_error) => error.context(format!(
                "native Codex provider stderr could not be archived: {archive_error:#}"
            )),
        },
    )?;
    let outcome = (|| -> Result<()> {
        let (thread_id, start_event, initialized, bootstrap_socket) = bootstrap(
            &app_socket,
            task.workspace,
            task.model,
            task.effort,
            task.session,
        )?;
        let generation = terminal.descriptor().execution_generation.clone();
        fs::write(&generation_file, &generation)?;
        let transcript = artifact.join("transcript.jsonl");
        write_bootstrap_evidence(
            &transcript,
            &generation,
            &thread_id,
            &start_event,
            task.session,
        )?;
        fs::write(&thread_file, &thread_id)?;
        let evidence = OpenOptions::new().append(true).open(&transcript)?;
        let admission = Arc::new(NativeAdmission::new());
        let settlement = Arc::clone(&admission);
        let stopping = Arc::new(AtomicBool::new(false));
        let relay_stopping = Arc::clone(&stopping);
        let (tx, rx) = mpsc::channel::<Result<Completion>>();
        let thread_id_for_relay = thread_id.clone();
        let generation_for_relay = generation.clone();
        let fresh_thread = !matches!(task.session, CoderSession::Resume(_));
        let relay_trace = artifact.join("native-codex-relay-diagnostics.jsonl");
        let relay_trace_for_worker = relay_trace.clone();
        let relay = std::thread::spawn(move || {
            let trace = OpenOptions::new()
                .append(true)
                .create(true)
                .open(&relay_trace_for_worker);
            let trace = match trace {
                Ok(trace) => trace,
                Err(error) => {
                    let _ = tx.send(Err(error.into()));
                    return;
                }
            };
            let boundary = match trace.try_clone() {
                Ok(file) => Mutex::new((file, 0)),
                Err(error) => {
                    let _ = tx.send(Err(error.into()));
                    return;
                }
            };
            if let Err(error) = trace_relay(
                &boundary,
                &generation_for_relay,
                &thread_id_for_relay,
                "tui-to-app",
                "awaiting-connection",
                None,
                0,
            ) {
                let _ = tx.send(Err(error));
                return;
            }
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
                trace_relay(
                    &boundary,
                    &generation_for_relay,
                    &thread_id_for_relay,
                    "tui-to-app",
                    "connected",
                    None,
                    0,
                )?;
                client.set_read_timeout(Some(Duration::from_secs(12)))?;
                let client = tungstenite::accept(client).map_err(|error| {
                    anyhow::anyhow!("Codex native TUI WebSocket handshake failed: {error}")
                })?;
                trace_relay(
                    &boundary,
                    &generation_for_relay,
                    &thread_id_for_relay,
                    "tui-to-app",
                    "handshake-complete",
                    None,
                    0,
                )?;
                trace_relay(
                    &boundary,
                    &generation_for_relay,
                    &thread_id_for_relay,
                    "app-to-tui",
                    "connected",
                    None,
                    0,
                )?;
                trace_relay(
                    &boundary,
                    &generation_for_relay,
                    &thread_id_for_relay,
                    "app-to-tui",
                    "handshake-complete",
                    None,
                    0,
                )?;
                relay_codex_websockets_with_handoff(
                    client,
                    bootstrap_socket,
                    &generation_for_relay,
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
                    trace,
                    Some(CodexHandoff {
                        initialized,
                        thread: start_event,
                        fresh: fresh_thread,
                    }),
                )
            });
            if let Err(error) = result {
                let _ = trace_relay(
                    &boundary,
                    &generation_for_relay,
                    &thread_id_for_relay,
                    "relay",
                    "failed",
                    None,
                    0,
                );
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
            if client_exit.exists() {
                break Err(anyhow::anyhow!(
                    "native Codex client exited before the assigned turn completed"
                ));
            }
        };
        if result.is_err() && !client_exit.exists() {
            let deadline = Instant::now() + Duration::from_millis(500);
            while !client_exit.exists() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        let diagnostic =
            record_client_diagnostic(artifact, &client_exit, &generation, &thread_id, &terminal);
        stopping.store(true, Ordering::Release);
        let stop = if result.is_ok() {
            terminal.finish()
        } else {
            terminal.stop()
        };
        let joined = relay.join();
        let trace_sync = if relay_trace.exists() {
            OpenOptions::new()
                .read(true)
                .open(&relay_trace)
                .and_then(|file| file.sync_all())
        } else {
            Ok(())
        };
        let result = settle_native_result(result, stop, joined);
        if let Err(error) = trace_sync {
            return result.and_then(|_| Err(error).context("sync native Codex relay diagnostics"));
        }
        if let Err(error) = diagnostic {
            return result
                .and_then(|_| Err(error.context("record native Codex client diagnostics")));
        }
        result?;
        File::open(&transcript)?.sync_all()?;
        Ok(())
    })();
    if outcome.is_err() {
        let _ = terminal.stop();
    }
    let archived = archive_provider_startup_log(&provider_log, artifact);
    match (outcome, archived) {
        (Ok(()), Ok(_)) => Ok(()),
        (Ok(()), Err(error)) => Err(error),
        (Err(error), Ok(path)) => {
            Err(error.context(format!("native Codex provider stderr: {}", path.display())))
        }
        (Err(error), Err(archive_error)) => Err(error.context(format!(
            "native Codex provider stderr could not be archived: {archive_error:#}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{self, Read};

    #[test]
    fn bootstrap_evidence_records_selected_native_launch_method() {
        let output = tempfile::NamedTempFile::new().unwrap();
        for (session, expected) in [
            (CoderSession::FreshPersistent, "thread/start"),
            (CoderSession::Resume("saved-thread"), "thread/resume"),
        ] {
            write_bootstrap_evidence(
                output.path(),
                "new-generation",
                "saved-thread",
                &json!({"result":{"thread":{"id":"saved-thread"}}}),
                session,
            )
            .unwrap();
            let last: Value = serde_json::from_str(
                fs::read_to_string(output.path())
                    .unwrap()
                    .lines()
                    .last()
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(last["launch_method"], expected);
        }
    }

    #[test]
    fn provider_startup_log_uses_invocation_writable_directory() {
        let invocation = tempfile::tempdir().unwrap();
        let artifact = tempfile::tempdir().unwrap();
        let log = provider_startup_log(invocation.path());
        assert!(log.starts_with(invocation.path()));
        assert!(!log.starts_with(artifact.path()));
        fs::write(&log, "app-server startup failed\n").unwrap();
        let archived = archive_provider_startup_log(&log, artifact.path()).unwrap();
        assert_eq!(
            fs::read_to_string(archived).unwrap(),
            "app-server startup failed\n"
        );

        let linked_log = invocation.path().join("linked-provider.log");
        std::os::unix::fs::symlink(&log, &linked_log).unwrap();
        let second_artifact = tempfile::tempdir().unwrap();
        assert!(archive_provider_startup_log(&linked_log, second_artifact.path()).is_err());
    }

    #[cfg(unix)]
    struct OwnedAppServerFixture {
        child: Option<std::process::Child>,
        group: i32,
        verified_group: bool,
    }

    #[cfg(unix)]
    fn read_fixture_stderr_bounded(mut stderr: std::process::ChildStderr) -> Result<String> {
        let fd = std::os::fd::AsRawFd::as_raw_fd(&stderr);
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        ensure!(flags >= 0, "inspect app-server fixture stderr flags");
        ensure!(
            unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } >= 0,
            "make app-server fixture stderr nonblocking"
        );
        let mut bytes = Vec::new();
        let deadline = Instant::now() + Duration::from_millis(200);
        loop {
            let mut chunk = [0u8; 4096];
            match stderr.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => bytes.extend_from_slice(&chunk[..n]),
                Err(error)
                    if error.kind() == io::ErrorKind::WouldBlock && Instant::now() < deadline =>
                {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) => return Err(error).context("read app-server fixture stderr"),
            }
            if bytes.len() >= 64 * 1024 {
                break;
            }
        }
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    #[cfg(unix)]
    impl OwnedAppServerFixture {
        fn spawn(command: &mut Command) -> Result<Self> {
            use std::os::unix::process::CommandExt;

            command.process_group(0);
            let child = command.spawn()?;
            let group = i32::try_from(child.id())?;
            let mut fixture = Self {
                child: Some(child),
                group,
                verified_group: false,
            };
            // Keep the leader unreaped until both signals are sent. Its PID
            // cannot be reused as another process group's ID in that time.
            let actual = unsafe { libc::getpgid(group) };
            if actual != group || group == unsafe { libc::getpgrp() } {
                let cleanup = fixture.finish();
                anyhow::bail!(
                    "app-server fixture did not own a separate process group: getpgid={actual}; cleanup: {cleanup:?}"
                );
            }
            fixture.verified_group = true;
            Ok(fixture)
        }

        fn finish(&mut self) -> Result<String> {
            self.finish_with(
                |group, signal| {
                    if unsafe { libc::kill(-group, signal) } == -1 {
                        Err(io::Error::last_os_error())
                    } else {
                        Ok(())
                    }
                },
                |child| child.kill(),
            )
        }

        fn finish_with(
            &mut self,
            mut signal_group: impl FnMut(i32, i32) -> io::Result<()>,
            mut kill_leader: impl FnMut(&mut std::process::Child) -> io::Result<()>,
        ) -> Result<String> {
            let Some(child) = self.child.as_mut() else {
                return Ok(String::new());
            };
            // The constructor verified this Child's private group. Keep its
            // exact PID unreaped until all group signals and probes finish;
            // getpgid may return ESRCH for an unreaped zombie on macOS.
            let mut errors = Vec::new();
            let mut permission_errors = Vec::new();
            let mut denied_live_term = false;
            let owned_group = self.verified_group
                && self.group > 0
                && self.group == child.id() as i32
                && self.group != unsafe { libc::getpgrp() };
            let actual = if owned_group {
                unsafe { libc::getpgid(self.group) }
            } else {
                -1
            };
            let group_identity_ok = owned_group
                && (actual == self.group
                    || (actual == -1
                        && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)));
            if !group_identity_ok {
                errors.push(format!("fixture group identity changed: {actual}"));
                if let Err(error) = kill_leader(child)
                    && error.kind() != io::ErrorKind::InvalidInput
                {
                    errors.push(format!("kill fixture leader after identity loss: {error}"));
                }
            }
            // The launcher may have exited while a reparented process still
            // holds stderr. Send both signals before reaping the leader.
            for signal in [libc::SIGTERM, libc::SIGKILL] {
                if group_identity_ok && let Err(error) = signal_group(self.group, signal) {
                    if error.raw_os_error() != Some(libc::ESRCH) {
                        let diagnostic =
                            format!("signal owned app-server fixture group with {signal}: {error}");
                        if error.raw_os_error() == Some(libc::EPERM) {
                            denied_live_term |= signal == libc::SIGTERM && actual == self.group;
                            permission_errors.push(diagnostic);
                        } else {
                            errors.push(diagnostic);
                        }
                    }
                }
                if signal == libc::SIGTERM {
                    std::thread::sleep(Duration::from_millis(150));
                }
            }
            let deadline = Instant::now() + Duration::from_secs(2);
            let mut reaped = false;
            let mut needed_leader_kill = false;
            loop {
                match child.try_wait() {
                    Ok(Some(_)) => {
                        reaped = true;
                        break;
                    }
                    Ok(None) => {}
                    Err(error) => {
                        errors.push(format!("reap app-server fixture leader: {error}"));
                        break;
                    }
                }
                if Instant::now() >= deadline {
                    errors.push("app-server fixture leader did not exit".into());
                    needed_leader_kill = true;
                    if let Err(error) = kill_leader(child) {
                        errors.push(format!("kill app-server fixture leader: {error}"));
                    }
                    let kill_deadline = Instant::now() + Duration::from_millis(250);
                    while Instant::now() < kill_deadline {
                        match child.try_wait() {
                            Ok(Some(_)) => {
                                reaped = true;
                                break;
                            }
                            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
                            Err(error) => {
                                errors.push(format!(
                                    "reap app-server fixture leader after kill: {error}"
                                ));
                                break;
                            }
                        }
                    }
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            if !reaped {
                errors.push("app-server fixture leader remains owned and unreaped".into());
                errors.extend(permission_errors);
                anyhow::bail!("{}", errors.join("; "));
            }
            let stderr = child.stderr.take();
            self.child = None;
            // All group signals ran while the leader reserved its group ID.
            // A zero probe is read-only; after reaping, give reparented
            // descendants time to leave before deciding the group survived.
            let group_probe = if group_identity_ok {
                let deadline = Instant::now() + Duration::from_secs(2);
                loop {
                    let probe = signal_group(self.group, 0);
                    if probe.is_err() || Instant::now() >= deadline {
                        break Some(probe);
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
            } else {
                None
            };
            if matches!(group_probe, Some(Ok(()))) {
                errors.push("owned app-server fixture group survived cleanup".into());
            }
            // macOS may return EPERM when TERM has already left only an
            // unreaped zombie. Retain failures if the group survived or the
            // leader needed a separate kill after the group signals.
            if denied_live_term || needed_leader_kill || matches!(group_probe, Some(Ok(()))) {
                errors.extend(permission_errors);
            }
            if let Some(Err(error)) = group_probe
                && error.raw_os_error() != Some(libc::ESRCH)
                && error.raw_os_error() != Some(libc::EPERM)
            {
                errors.push(format!("inspect owned app-server fixture group: {error}"));
            }
            let stderr = match stderr {
                Some(stderr) => match read_fixture_stderr_bounded(stderr) {
                    Ok(stderr) => stderr,
                    Err(error) => {
                        errors.push(format!("read app-server fixture stderr: {error:#}"));
                        String::new()
                    }
                },
                None => String::new(),
            };
            ensure!(
                errors.is_empty(),
                "{}; app-server stderr: {stderr}",
                errors.join("; ")
            );
            Ok(stderr)
        }
    }

    #[cfg(unix)]
    impl Drop for OwnedAppServerFixture {
        fn drop(&mut self) {
            if self.child.is_some() {
                let _ = self.finish();
            }
        }
    }

    #[cfg(unix)]
    fn read_published_fixture_pid(path: &Path, deadline: Instant) -> Result<i32> {
        loop {
            if let Ok(contents) = fs::read_to_string(path)
                && let Ok(pid) = contents.trim().parse::<i32>()
                && pid > 0
            {
                return Ok(pid);
            }
            ensure!(
                Instant::now() < deadline,
                "fixture did not publish a valid positive daemon PID"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[cfg(unix)]
    fn stop_private_tmux_fixture(socket: &Path) -> Result<()> {
        let mut client = Command::new("tmux")
            .arg("-S")
            .arg(socket)
            .arg("kill-server")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()?;
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if client.try_wait()?.is_some() {
                return Ok(());
            }
            if Instant::now() >= deadline {
                let _ = client.kill();
                let _ = client.try_wait();
                anyhow::bail!("private tmux cleanup exceeded its deadline");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[cfg(unix)]
    fn join_fixture_relay_bounded(
        handle: std::thread::JoinHandle<Result<()>>,
        abort: Option<UnixStream>,
    ) -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(13);
        while !handle.is_finished() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        if !handle.is_finished() {
            if let Some(abort) = abort {
                let _ = abort.shutdown(Shutdown::Both);
            }
            let deadline = Instant::now() + Duration::from_secs(1);
            while !handle.is_finished() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        ensure!(
            handle.is_finished(),
            "installed Codex relay did not stop after cleanup"
        );
        let _ = handle
            .join()
            .map_err(|_| anyhow::anyhow!("installed Codex relay panicked"))?;
        Ok(())
    }

    #[cfg(unix)]
    fn installed_handoff_trace_ready(trace: &str) -> Result<bool> {
        // The relay may still be writing the last record when the preflight polls.
        let complete = trace.rsplit_once('\n').map_or("", |(complete, _)| complete);
        let events = complete
            .lines()
            .map(serde_json::from_str::<Value>)
            .collect::<serde_json::Result<Vec<_>>>()?;
        let resume = events.iter().position(|event| {
            event["method"] == "thread/resume" && event["outcome"] == "bootstrap-replied"
        });
        let Some(resume) = resume else {
            return Ok(false);
        };
        let read = events.iter().enumerate().any(|(index, event)| {
            index > resume
                && event["method"] == "thread/read"
                && event["outcome"] == "ui-only-replied"
                && event["input_sequence"] == 0
        });
        ensure!(
            !events.iter().any(|event| {
                event["method"] == "thread/resume" && event["outcome"] == "forwarded"
            }),
            "empty Codex thread was resumed through the app-server"
        );
        ensure!(
            !events.iter().any(|event| {
                event["method"] == "turn/start" && event["outcome"] == "forwarded"
            }),
            "credential-free TUI submitted a model turn"
        );
        ensure!(
            !events.iter().skip(resume + 1).any(|event| {
                event["method"] == "thread/turns/list"
                    && event["outcome"] == "forwarded"
                    && event["input_sequence"] == 0
            }),
            "empty Codex thread attempted provider history pagination"
        );
        ensure!(
            !events.iter().skip(resume + 1).any(|event| {
                event["method"] == "thread/read"
                    && event["outcome"] == "forwarded"
                    && event["include_turns"] == true
                    && event["input_sequence"] == 0
            }),
            "empty Codex thread attempted provider history lookup"
        );
        Ok(read)
    }

    #[cfg(unix)]
    #[test]
    fn app_server_fixture_bounds_stderr_held_by_exited_launcher() {
        let mut command = Command::new("/bin/sh");
        command
            .arg("-c")
            .arg("sleep 1 >&2 & exit 0")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped());
        let mut child = command.spawn().unwrap();
        let stderr = child.stderr.take().unwrap();
        let launcher_started = Instant::now();
        child.wait().unwrap();
        assert!(launcher_started.elapsed() < Duration::from_millis(750));
        let start = Instant::now();
        read_fixture_stderr_bounded(stderr).unwrap();
        assert!(start.elapsed() < Duration::from_millis(600));
    }

    #[cfg(unix)]
    #[test]
    fn fixture_pid_reader_waits_for_complete_positive_publication() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("daemon.pid");
        fs::write(&path, "").unwrap();
        let writer_path = path.clone();
        let writer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            fs::write(writer_path, "12345\n").unwrap();
        });
        assert_eq!(
            read_published_fixture_pid(&path, Instant::now() + Duration::from_secs(1)).unwrap(),
            12345
        );
        writer.join().unwrap();
        fs::write(&path, "0\n").unwrap();
        assert!(
            read_published_fixture_pid(&path, Instant::now() + Duration::from_millis(30)).is_err()
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "requires unsandboxed process-group signals"]
    fn app_server_fixture_cleans_reparented_stderr_holder() {
        let root = tempfile::tempdir().unwrap();
        let pid_path = root.path().join("daemon.pid");
        let mut command = Command::new("/bin/sh");
        command
            .arg("-c")
            .arg("trap '' TERM; sleep 60 >&2 & echo $! > \"$1\"; exit 0")
            .arg("fixture")
            .arg(&pid_path)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped());
        let mut fixture = OwnedAppServerFixture::spawn(&mut command).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        let daemon_pid = read_published_fixture_pid(&pid_path, deadline);
        let daemon_pid = match daemon_pid {
            Ok(pid) => pid,
            Err(error) => {
                let cleanup = fixture.finish();
                panic!("{error:#}; cleanup: {cleanup:?}");
            }
        };
        assert_eq!(unsafe { libc::getpgid(daemon_pid) }, fixture.group);
        let start = Instant::now();
        fixture.finish().unwrap();
        assert!(start.elapsed() < Duration::from_secs(3));
        let deadline = Instant::now() + Duration::from_secs(2);
        while unsafe { libc::kill(daemon_pid, 0) } == 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(unsafe { libc::kill(daemon_pid, 0) }, -1);
        assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::ESRCH));
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "requires unsandboxed process-group signals"]
    fn app_server_fixture_reaps_zombie_only_group() {
        let mut command = Command::new("/bin/sh");
        command
            .arg("-c")
            .arg("exit 0")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped());
        let mut fixture = OwnedAppServerFixture::spawn(&mut command).unwrap();
        std::thread::sleep(Duration::from_millis(100));
        fixture.finish().unwrap();
        assert!(fixture.child.is_none());
    }

    #[cfg(unix)]
    #[test]
    fn app_server_fixture_waits_for_group_probe_to_clear() {
        let mut command = Command::new("/bin/sh");
        command
            .arg("-c")
            .arg("exit 0")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped());
        let mut fixture = OwnedAppServerFixture::spawn(&mut command).unwrap();
        let mut probes = 0;
        fixture
            .finish_with(
                |_, signal| {
                    if signal == 0 {
                        probes += 1;
                        if probes == 1 {
                            Ok(())
                        } else {
                            Err(io::Error::from_raw_os_error(libc::ESRCH))
                        }
                    } else {
                        Ok(())
                    }
                },
                |child| child.kill(),
            )
            .unwrap();
        assert_eq!(probes, 2);
        assert!(fixture.child.is_none());
    }

    #[cfg(unix)]
    #[test]
    fn app_server_fixture_reports_changed_group_without_signalling_it() {
        let mut owner_command = Command::new("/bin/sleep");
        owner_command
            .arg("60")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped());
        let mut fixture = OwnedAppServerFixture::spawn(&mut owner_command).unwrap();
        let mut sentinel_command = Command::new("/bin/sleep");
        sentinel_command
            .arg("60")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped());
        let mut sentinel = OwnedAppServerFixture::spawn(&mut sentinel_command).unwrap();
        fixture.group = sentinel.group;
        let error = fixture.finish().unwrap_err().to_string();
        assert!(error.contains("fixture group identity changed"), "{error}");
        assert!(fixture.child.is_none());
        assert!(
            sentinel
                .child
                .as_mut()
                .unwrap()
                .try_wait()
                .unwrap()
                .is_none()
        );
        let sentinel_child = sentinel.child.as_mut().unwrap();
        sentinel_child.kill().unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while sentinel_child.try_wait().unwrap().is_none() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(sentinel_child.try_wait().unwrap().is_some());
        sentinel.child = None;
    }

    #[cfg(unix)]
    #[test]
    fn app_server_fixture_denied_kill_is_bounded_and_retains_child() {
        let mut command = Command::new("/bin/sleep");
        command
            .arg("60")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped());
        let mut fixture = OwnedAppServerFixture::spawn(&mut command).unwrap();
        let start = Instant::now();
        let error = fixture
            .finish_with(
                |_, _| Err(io::Error::from_raw_os_error(libc::EPERM)),
                |_| Err(io::Error::from_raw_os_error(libc::EPERM)),
            )
            .unwrap_err()
            .to_string();
        assert!(start.elapsed() < Duration::from_secs(3), "{error}");
        assert!(
            error.contains("Permission denied") || error.contains("Operation not permitted"),
            "{error}"
        );
        assert!(error.contains("remains owned and unreaped"), "{error}");
        let child = fixture.child.as_mut().unwrap();
        assert!(child.try_wait().unwrap().is_none());
        child.kill().unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while child.try_wait().unwrap().is_none() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(child.try_wait().unwrap().is_some());
        fixture.child = None;
    }

    #[cfg(unix)]
    #[test]
    fn app_server_fixture_reports_denied_term_on_live_leader() {
        let mut command = Command::new("/bin/sleep");
        command
            .arg("1")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped());
        let mut fixture = OwnedAppServerFixture::spawn(&mut command).unwrap();
        let error = fixture
            .finish_with(
                |_, signal| {
                    if signal == libc::SIGTERM {
                        Err(io::Error::from_raw_os_error(libc::EPERM))
                    } else if signal == 0 {
                        Err(io::Error::from_raw_os_error(libc::ESRCH))
                    } else {
                        Ok(())
                    }
                },
                |child| child.kill(),
            )
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("signal owned app-server fixture group"),
            "{error}"
        );
        assert!(
            error.contains("Operation not permitted") || error.contains("Permission denied"),
            "{error}"
        );
        assert!(fixture.child.is_none());
    }

    #[cfg(unix)]
    #[test]
    fn installed_handoff_allows_display_history_but_rejects_provider_lookup() {
        let event = |method, outcome, include_turns: Option<bool>| {
            json!({"method":method,"outcome":outcome,"include_turns":include_turns,"input_sequence":0}).to_string()
        };
        let trace = [
            event("thread/read", "forwarded", Some(false)),
            event("thread/resume", "bootstrap-replied", None),
            event("thread/read", "forwarded", Some(false)),
            event("thread/read", "ui-only-replied", None),
            event("thread/turns/list", "ui-only-replied", None),
        ]
        .join("\n")
            + "\n";
        assert!(installed_handoff_trace_ready(&trace).unwrap());
        let bad = format!("{trace}{}\n", event("thread/read", "forwarded", Some(true)));
        assert!(
            installed_handoff_trace_ready(&bad)
                .unwrap_err()
                .to_string()
                .contains("provider history lookup")
        );
        let bad = format!("{trace}{}\n", event("thread/turns/list", "forwarded", None));
        assert!(
            installed_handoff_trace_ready(&bad)
                .unwrap_err()
                .to_string()
                .contains("provider history pagination")
        );
    }

    #[cfg(unix)]
    #[test]
    fn installed_handoff_ignores_split_record_but_rejects_malformed_complete_record() {
        let resume = json!({"method":"thread/resume","outcome":"bootstrap-replied"});
        let read = json!({"method":"thread/read","outcome":"ui-only-replied","input_sequence":0});
        let trace = format!("{resume}\n{{\"method\":\"thread/read\"");
        assert!(!installed_handoff_trace_ready(&trace).unwrap());

        let trace = format!("{resume}\n{read}\n{{\"method\":");
        assert!(installed_handoff_trace_ready(&trace).unwrap());

        let malformed = format!("{resume}\n{{\"method\":\n{read}\n");
        assert!(installed_handoff_trace_ready(&malformed).is_err());
    }

    #[test]
    fn installed_preflight_keeps_primary_error_with_all_cleanup_failures() {
        let outcome = add_fixture_cleanup::<()>(
            Err(anyhow::anyhow!("TUI thread/read failed")),
            "private tmux cleanup",
            Err(anyhow::anyhow!("tmux timed out")),
        );
        let outcome = add_fixture_cleanup(
            outcome,
            "app-server fixture cleanup",
            Err(anyhow::anyhow!("permission denied")),
        );
        let text = outcome.unwrap_err().to_string();
        assert!(text.contains("TUI thread/read failed"), "{text}");
        assert!(text.contains("tmux timed out"), "{text}");
        assert!(text.contains("permission denied"), "{text}");
    }

    #[test]
    fn empty_thread_display_read_requires_provider_identity_and_empty_history() {
        let start =
            json!({"result":{"thread":{"id":"assigned","turns":[],"historyMode":"legacy"}}});
        let read = json!({"id":7,"params":{"threadId":"assigned","includeTurns":true}});
        assert_eq!(
            fresh_thread_display_read(&start, &read, "assigned").unwrap(),
            json!({"id":7,"result":{"thread":{"id":"assigned","turns":[],"historyMode":"legacy"}}})
        );
        assert!(fresh_thread_display_read(&start, &read, "other").is_err());
        let metadata = json!({"id":8,"params":{"threadId":"assigned","includeTurns":false}});
        assert!(fresh_thread_display_read(&start, &metadata, "assigned").is_err());
        let mut materialized = start;
        materialized["result"]["thread"]["turns"] = json!([{"id":"real-turn"}]);
        assert!(fresh_thread_display_read(&materialized, &read, "assigned").is_err());
    }

    #[test]
    fn empty_thread_display_page_requires_first_page_of_assigned_thread() {
        let start =
            json!({"result":{"thread":{"id":"assigned","turns":[],"historyMode":"legacy"}}});
        let page = json!({"id":9,"params":{"threadId":"assigned","limit":20}});
        assert_eq!(
            fresh_thread_display_page(&start, &page, "assigned").unwrap(),
            json!({"id":9,"result":{"data":[],"nextCursor":null,"backwardsCursor":null}})
        );
        assert!(fresh_thread_display_page(&start, &page, "other").is_err());
        let mut later = page.clone();
        later["params"]["cursor"] = json!("older-page");
        assert!(fresh_thread_display_page(&start, &later, "assigned").is_err());
        let mut materialized = start;
        materialized["result"]["thread"]["turns"] = json!([{"id":"real-turn"}]);
        assert!(fresh_thread_display_page(&materialized, &page, "assigned").is_err());
    }

    #[test]
    fn client_exit_diagnostic_keeps_identity_and_hides_other_pane_text() {
        assert_eq!(
            parse_client_exit(
                "generation-1\tassigned-thread\t23\n",
                "generation-1",
                "assigned-thread"
            )
            .unwrap(),
            23
        );
        assert!(
            parse_client_exit(
                "generation-2\tassigned-thread\t23\n",
                "generation-1",
                "assigned-thread"
            )
            .is_err()
        );
        assert_eq!(
            recognized_pane_output(Some(
                "private-token Connection reset without closing handshake"
            )),
            vec!["Connection reset without closing handshake"]
        );
        assert_eq!(
            recognized_pane_output(Some(
                "private-token Error: Permission overrides are not supported when resuming a remote task. another-secret"
            )),
            vec!["Error: Permission overrides are not supported when resuming a remote task."]
        );
        assert!(recognized_pane_output(Some("private-token another-secret")).is_empty());
    }

    fn answer_config_read(socket: &mut WebSocket<UnixStream>, model: &str) {
        let read: Value =
            serde_json::from_str(&socket.read().unwrap().into_text().unwrap()).unwrap();
        assert_eq!(read["method"], "config/read");
        let workspace = read["params"]["cwd"].as_str().unwrap();
        let response = json!({"id":2,"result":{"config":{
            "projects": {(workspace): {"trust_level":"trusted"}},
            "model": model,
            "model_reasoning_effort": "medium",
            "features": {"hooks": false}
        }}});
        socket.send(Message::Text(response.to_string())).unwrap();
    }

    fn configure_no_auth_tui_fixture(home: &Path) -> Result<()> {
        let path = home.join("config.toml");
        let mut config: toml::Table = fs::read_to_string(&path)?.parse()?;
        config.insert(
            "model_provider".into(),
            toml::Value::String("fixture-local".into()),
        );
        let provider: toml::Table = toml::from_str(
            "name = 'Fixture local'\nbase_url = 'http://127.0.0.1:9/v1'\nwire_api = 'responses'\nrequires_openai_auth = false\n",
        )?;
        config.insert(
            "model_providers".into(),
            toml::Value::Table(toml::Table::from_iter([(
                "fixture-local".into(),
                toml::Value::Table(provider),
            )])),
        );
        fs::write(path, toml::to_string(&config)?)?;
        Ok(())
    }

    #[test]
    fn installed_tui_fixture_selects_private_no_auth_provider() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let workspace = root.path().join("workspace");
        fs::create_dir(&home).unwrap();
        fs::create_dir(&workspace).unwrap();
        fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).unwrap();
        prepare_native_config(&home, &workspace).unwrap();
        configure_no_auth_tui_fixture(&home).unwrap();
        let config: toml::Table = fs::read_to_string(home.join("config.toml"))
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(config["model_provider"].as_str(), Some("fixture-local"));
        assert_eq!(
            config["model_providers"]["fixture-local"]["requires_openai_auth"].as_bool(),
            Some(false)
        );
        assert_eq!(
            config["model_providers"]["fixture-local"]["base_url"].as_str(),
            Some("http://127.0.0.1:9/v1")
        );
    }
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn pane_and_installed_fixture_share_encoded_app_server_launch() {
        let root = tempfile::tempdir().unwrap();
        let model = "gpt-\"quote\"'$(touch forbidden)";
        let executable = root.path().join("codex");
        let socket = root.path().join("app.sock");
        let server = app_server_command(&executable, model, Some("medium"), &socket).unwrap();
        let args: Vec<_> = server.get_args().map(|arg| arg.to_str().unwrap()).collect();
        let encoded = args.iter().find(|arg| arg.starts_with("model=")).unwrap();
        let parsed: toml::Table = format!("value = {}", encoded.strip_prefix("model=").unwrap())
            .parse()
            .unwrap();
        assert_eq!(parsed["value"], toml::Value::String(model.to_owned()));
        assert!(args.contains(&"model_reasoning_effort=\"medium\""));
        assert!(args.contains(&"features.hooks=false"));
        let coder = CodexCode {
            sandbox_profile: None,
            trusted_sandbox: false,
            model_override: Some(model.to_owned()),
            effort: Some("medium".into()),
            executable,
        };
        let script = pane_script(
            &coder,
            root.path(),
            root.path(),
            &socket,
            &root.path().join("relay.sock"),
            &root.path().join("go"),
            &root.path().join("thread"),
            &root.path().join("server.log"),
            &root.path().join("generation"),
            &root.path().join("client-exit"),
            "assigned prompt",
        )
        .unwrap();
        assert!(script.contains(&command_line(&server).unwrap()));
    }

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

    #[test]
    fn websocket_relay_retains_frames_while_tui_temporarily_stalls() {
        let workspace = tempfile::tempdir().unwrap();
        let (tui_stream, relay_client) = UnixStream::pair().unwrap();
        let (relay_server, provider_stream) = UnixStream::pair().unwrap();
        let (completed, received) = mpsc::channel();
        let evidence_path = workspace.path().join("evidence.jsonl");
        let evidence = File::create(&evidence_path).unwrap();
        let relay = std::thread::spawn(move || {
            let client = tungstenite::accept(relay_client).unwrap();
            let server = tungstenite::client("ws://localhost/", relay_server)
                .unwrap()
                .0;
            relay_codex_websockets(
                client,
                server,
                "generation-1",
                "assigned-thread",
                Arc::new(NativeAdmission::new()),
                evidence,
                |completion| completed.send(completion).map_err(anyhow::Error::new),
            )
        });
        let provider = std::thread::spawn(move || {
            let mut socket = tungstenite::accept(provider_stream).unwrap();
            let request: Value =
                serde_json::from_str(&socket.read().unwrap().into_text().unwrap()).unwrap();
            assert_eq!(request["method"], "turn/start");
            socket
                .send(Message::Text(
                    json!({"id":1,"result":{"turn":{"id":"turn-1"}}}).to_string(),
                ))
                .unwrap();
            socket.send(Message::Text(json!({"method":"turn/started","params":{"threadId":"assigned-thread","turn":{"id":"turn-1"}}}).to_string())).unwrap();
            for index in 0..96 {
                socket.send(Message::Text(json!({"method":"item/updated","params":{"threadId":"assigned-thread","turnId":"turn-1","index":index,"text":"x".repeat(16384)}}).to_string())).unwrap();
            }
            socket.send(Message::Text(json!({"method":"turn/completed","params":{"threadId":"assigned-thread","turn":{"id":"turn-1","status":"completed"}}}).to_string())).unwrap();
            socket
        });
        let mut tui = tungstenite::client("ws://localhost/", tui_stream)
            .unwrap()
            .0;
        tui.send(Message::Text(json!({"id":1,"method":"turn/start","params":{"threadId":"assigned-thread","input":[]}}).to_string())).unwrap();
        // Do not read display frames until the canonical completion arrives.
        assert!(matches!(
            received.recv_timeout(Duration::from_secs(5)).unwrap(),
            Completion::Accepted {
                input_sequence: 1,
                ..
            }
        ));
        let methods: Vec<String> = (0..99)
            .map(|_| {
                let event: Value =
                    serde_json::from_str(&tui.read().unwrap().into_text().unwrap()).unwrap();
                event["method"].as_str().unwrap_or("response").to_owned()
            })
            .collect();
        assert_eq!(methods[1], "turn/started");
        assert_eq!(
            methods
                .iter()
                .filter(|method| *method == "item/updated")
                .count(),
            96
        );
        assert_eq!(methods.last().unwrap(), "turn/completed");
        assert_eq!(
            fs::read_to_string(evidence_path).unwrap().lines().count(),
            99
        );
        drop(tui);
        drop(provider.join().unwrap());
        let _ = relay.join().unwrap();
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
        let model = std::env::var("FLUENT_NATIVE_CODEX_MODEL").unwrap();
        let effort = std::env::var("FLUENT_NATIVE_CODEX_EFFORT").unwrap();
        let launch = app_server_command(&executable, &model, Some(&effort), &app_socket).unwrap();
        let mut server =
            crate::os::command_under_profile(&profile, Path::new(launch.get_program()));
        server.args(launch.get_args());
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
            assert_eq!(config["features"]["hooks"], false);
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

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "requires the installed Codex binary and an unsandboxed macOS host"]
    fn installed_codex_fresh_thread_survives_bootstrap_handoff() {
        let executable = PathBuf::from(std::env::var_os("FLUENT_TEST_INSTALLED_CODEX").unwrap());
        let private = tempfile::Builder::new()
            .prefix("fluent-na-handoff-")
            .tempdir_in("/private/tmp")
            .unwrap();
        fs::set_permissions(private.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let workspace = private.path().join("assigned-worktree");
        fs::create_dir(&workspace).unwrap();
        let workspace = prepare_native_config(private.path(), &workspace).unwrap();
        // Keep this installed TUI test credential-free, including provider
        // model prewarm. The endpoint is loopback only and admission below
        // rejects any model turn before it can reach the provider.
        configure_no_auth_tui_fixture(private.path()).unwrap();
        let app_socket = private.path().join("app.sock");
        let daemon = CodexDaemonResources::prepare(&app_socket).unwrap();
        let model = std::env::var("FLUENT_NATIVE_CODEX_MODEL").unwrap();
        let effort = std::env::var("FLUENT_NATIVE_CODEX_EFFORT").unwrap();
        let mut server =
            app_server_command(&executable, &model, Some(&effort), &app_socket).unwrap();
        server
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap())
            .env("HOME", private.path())
            .env("CODEX_HOME", private.path());
        server.current_dir(&workspace);
        let mut child = OwnedAppServerFixture::spawn(
            server
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::piped()),
        )
        .unwrap();
        let tmux_socket = private.path().join("tmux.sock");
        let relay_socket = private.path().join("relay.sock");
        let trace_path = private.path().join("relay-trace.jsonl");
        let mut relay_thread = None;
        let mut relay_abort = None;
        let outcome = (|| -> Result<()> {
            let listener = UnixListener::bind(&relay_socket)
                .context("bind installed Codex TUI relay socket")?;
            listener.set_nonblocking(true)?;
            let deadline = Instant::now() + Duration::from_secs(12);
            while !app_socket.exists() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(25));
            }
            ensure!(
                app_socket.exists() && daemon.socket.exists(),
                "Codex socket absent"
            );
            let (thread_id, start, initialized, mut owner) = bootstrap(
                &app_socket,
                &workspace,
                &model,
                Some(&effort),
                CoderSession::FreshPersistent,
            )?;
            let account = request(&mut owner, 4, "account/read", json!({}))?;
            ensure!(
                account["result"]["requiresOpenaiAuth"] == false,
                "credential-free fixture still requires OpenAI authentication"
            );
            relay_abort = Some(owner.get_ref().try_clone()?);
            let coder = CodexCode {
                sandbox_profile: None,
                trusted_sandbox: false,
                model_override: Some(model.clone()),
                effort: Some(effort.clone()),
                executable: executable.clone(),
            };
            let tui_command = coder.native_task_command_with_remote(
                &workspace,
                private.path(),
                CoderSession::Resume(&thread_id),
                "",
                Some(&relay_socket),
            )?;
            let script = private.path().join("native-tui.sh");
            fs::write(
                &script,
                format!(
                    "#!/bin/sh\nexec /usr/bin/env -i HOME={} CODEX_HOME={} PATH={} TERM=xterm-256color {}\n",
                    quote(&private.path().display().to_string()),
                    quote(&private.path().display().to_string()),
                    quote(&std::env::var("PATH")?),
                    command_line(&tui_command)?,
                ),
            )?;
            let admission = Arc::new(NativeAdmission::new());
            // This regression must never submit a paid model turn.
            admission.begin_settlement()?;
            let relay_trace = trace_path.clone();
            relay_thread = Some(std::thread::spawn(move || -> Result<()> {
                let deadline = Instant::now() + Duration::from_secs(12);
                let client = loop {
                    match listener.accept() {
                        Ok((client, _)) => break client,
                        Err(error)
                            if error.kind() == std::io::ErrorKind::WouldBlock
                                && Instant::now() < deadline =>
                        {
                            std::thread::sleep(Duration::from_millis(20));
                        }
                        Err(error) => return Err(error).context("accept installed Codex TUI"),
                    }
                };
                let client = tungstenite::accept(client)
                    .map_err(|error| anyhow::anyhow!("installed Codex TUI WebSocket: {error}"))?;
                relay_codex_websockets_with_handoff(
                    client,
                    owner,
                    "installed-handoff",
                    &thread_id,
                    admission,
                    File::create(relay_trace.with_extension("evidence.jsonl"))?,
                    |_| Ok(()),
                    File::create(relay_trace)?,
                    Some(CodexHandoff {
                        initialized,
                        thread: start,
                        fresh: true,
                    }),
                )
            }));
            let started = Command::new("tmux")
                .arg("-S")
                .arg(&tmux_socket)
                .args(["new-session", "-d", "-s", "handoff", "-c"])
                .arg(&workspace)
                .arg(format!("/bin/sh {}", quote(&script.display().to_string())))
                .status()?;
            ensure!(
                started.success(),
                "tmux could not launch the installed Codex TUI"
            );
            let deadline = Instant::now() + Duration::from_secs(12);
            loop {
                let trace = fs::read_to_string(&trace_path).unwrap_or_default();
                if installed_handoff_trace_ready(&trace)? {
                    break;
                }
                ensure!(
                    Instant::now() < deadline,
                    "installed TUI did not complete its assigned fresh-thread history handoff"
                );
                std::thread::sleep(Duration::from_millis(25));
            }
            std::thread::sleep(Duration::from_millis(500));
            ensure!(
                installed_handoff_trace_ready(&fs::read_to_string(&trace_path)?)?,
                "installed Codex TUI lost its fresh-thread handoff"
            );
            ensure!(
                Command::new("tmux")
                    .arg("-S")
                    .arg(&tmux_socket)
                    .args(["has-session", "-t", "handoff"])
                    .status()?
                    .success(),
                "installed Codex TUI exited after its resume handoff"
            );
            let pane = Command::new("tmux")
                .arg("-S")
                .arg(&tmux_socket)
                .args(["capture-pane", "-p", "-t", "handoff"])
                .output()?;
            ensure!(
                pane.status.success(),
                "installed Codex TUI ended during handoff"
            );
            ensure!(
                !String::from_utf8_lossy(&pane.stdout).contains("Failed to resume task"),
                "installed Codex TUI rejected its assigned thread"
            );
            ensure!(
                !String::from_utf8_lossy(&pane.stdout)
                    .contains("failed to load a bounded thread history page"),
                "installed Codex TUI could not load its empty thread"
            );
            ensure!(
                !relay_thread.as_ref().unwrap().is_finished(),
                "installed Codex relay stopped before TUI handoff settled"
            );
            Ok(())
        })();
        let tmux_cleanup = stop_private_tmux_fixture(&tmux_socket);
        let daemon_cleanup = child.finish();
        let relay_cleanup = relay_thread
            .map(|relay_thread| join_fixture_relay_bounded(relay_thread, relay_abort))
            .unwrap_or(Ok(()));
        if outcome.is_err() {
            if let Ok(stderr) = &daemon_cleanup {
                eprintln!("installed Codex stderr: {stderr}");
            }
        }
        let outcome = add_fixture_cleanup(outcome, "private tmux cleanup", tmux_cleanup);
        let outcome = add_fixture_cleanup(
            outcome,
            "app-server fixture cleanup",
            daemon_cleanup.map(|_| ()),
        );
        let outcome = add_fixture_cleanup(outcome, "Codex relay cleanup", relay_cleanup);
        outcome.unwrap();
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
            answer_config_read(&mut socket, "gpt-assigned");
            let start: Value =
                serde_json::from_str(&socket.read().unwrap().into_text().unwrap()).unwrap();
            assert_eq!(start["method"], "thread/start");
            assert_eq!(start["params"]["model"], "gpt-assigned");
            assert_eq!(start["params"]["approvalPolicy"], "never");
            assert_eq!(start["params"]["historyMode"], "legacy");
            socket
                .send(Message::Text(
                    "{\"id\":3,\"result\":{\"thread\":{\"id\":\"assigned-thread\"}}}".into(),
                ))
                .unwrap();
        });
        let (mut owner, _) = tungstenite::client("ws://localhost/", owner).unwrap();
        let (thread, _, _) = bootstrap_protocol(
            &mut owner,
            &workspace.path().canonicalize().unwrap(),
            "gpt-assigned",
            Some("medium"),
            CoderSession::FreshPersistent,
        )
        .unwrap();
        assert_eq!(thread, "assigned-thread");
        fake.join().unwrap();
    }

    #[test]
    fn bootstrap_hands_its_live_thread_to_native_resume() {
        let workspace = tempfile::tempdir().unwrap();
        let canonical = workspace.path().canonicalize().unwrap();
        let (owner, provider) = UnixStream::pair().unwrap();
        let (fake_release, fake_wait) = mpsc::channel();
        let fake = std::thread::spawn(move || {
            let mut socket = tungstenite::accept(provider).unwrap();
            let initialize: Value =
                serde_json::from_str(&socket.read().unwrap().into_text().unwrap()).unwrap();
            assert_eq!(initialize["method"], "initialize");
            socket
                .send(Message::Text(
                    json!({"id":1,"result":{"userAgent":"installed-provider"}}).to_string(),
                ))
                .unwrap();
            assert_eq!(
                serde_json::from_str::<Value>(&socket.read().unwrap().into_text().unwrap())
                    .unwrap()["method"],
                "initialized"
            );
            answer_config_read(&mut socket, "gpt-assigned");
            let start: Value =
                serde_json::from_str(&socket.read().unwrap().into_text().unwrap()).unwrap();
            assert_eq!(start["method"], "thread/start");
            assert_eq!(start["params"]["historyMode"], "legacy");
            socket.send(Message::Text(json!({"id":3,"result":{"thread":{"id":"assigned-thread","historyMode":"legacy","turns":[]},"model":"gpt-assigned"}}).to_string())).unwrap();
            let turn: Value =
                serde_json::from_str(&socket.read().unwrap().into_text().unwrap()).unwrap();
            assert_eq!(
                turn["method"], "turn/start",
                "empty thread was resumed through the app-server"
            );
            assert_eq!(turn["params"]["threadId"], "assigned-thread");
            socket
                .send(Message::Text(
                    json!({"id":5,"result":{"turn":{"id":"turn-1"}}}).to_string(),
                ))
                .unwrap();
            socket.send(Message::Text(json!({"method":"turn/started","params":{"threadId":"assigned-thread","turn":{"id":"turn-1"}}}).to_string())).unwrap();
            socket.send(Message::Text(json!({"method":"turn/completed","params":{"threadId":"assigned-thread","turn":{"id":"turn-1","status":"completed"}}}).to_string())).unwrap();
            fake_wait.recv_timeout(Duration::from_secs(2)).unwrap();
            let _ = socket.close(None);
        });
        let (mut owner, _) = tungstenite::client("ws://localhost/", owner).unwrap();
        let (thread_id, start, initialized) = bootstrap_protocol(
            &mut owner,
            &canonical,
            "gpt-assigned",
            Some("medium"),
            CoderSession::FreshPersistent,
        )
        .unwrap();
        let (tui, relay) = UnixStream::pair().unwrap();
        let (completions, received) = mpsc::channel();
        let evidence_path = workspace.path().join("canonical.jsonl");
        let trace_path = workspace.path().join("relay.jsonl");
        let evidence = File::create(&evidence_path).unwrap();
        let trace = File::create(&trace_path).unwrap();
        let relay_thread = std::thread::spawn(move || {
            let relay = tungstenite::accept(relay).unwrap();
            relay_codex_websockets_with_handoff(
                relay,
                owner,
                "generation-1",
                &thread_id,
                Arc::new(NativeAdmission::new()),
                evidence,
                |completion| {
                    completions.send(completion).unwrap();
                    Ok(())
                },
                trace,
                Some(CodexHandoff {
                    initialized,
                    thread: start,
                    fresh: true,
                }),
            )
        });
        let (mut tui, _) = tungstenite::client("ws://localhost/", tui).unwrap();
        assert_eq!(
            request(
                &mut tui,
                1,
                "initialize",
                json!({"clientInfo":{"name":"native-tui","version":"1"}})
            )
            .unwrap()["result"]["userAgent"],
            "installed-provider"
        );
        tui.send(Message::Text("{\"method\":\"initialized\"}".into()))
            .unwrap();
        assert_eq!(
            request(
                &mut tui,
                2,
                "thread/resume",
                json!({"threadId":"assigned-thread"})
            )
            .unwrap()["result"]["thread"]["historyMode"],
            "legacy"
        );
        assert_eq!(
            request(
                &mut tui,
                3,
                "thread/read",
                json!({"threadId":"assigned-thread","includeTurns":true})
            )
            .unwrap()["result"]["thread"]["turns"],
            json!([])
        );
        assert_eq!(
            request(
                &mut tui,
                4,
                "thread/turns/list",
                json!({"threadId":"assigned-thread","limit":20})
            )
            .unwrap()["result"],
            json!({"data":[],"nextCursor":null,"backwardsCursor":null})
        );
        assert_eq!(
            request(
                &mut tui,
                5,
                "turn/start",
                json!({"threadId":"assigned-thread","input":[]})
            )
            .unwrap()["result"]["turn"]["id"],
            "turn-1"
        );
        assert!(matches!(
            received.recv_timeout(Duration::from_secs(2)).unwrap(),
            Completion::Accepted {
                input_sequence: 1,
                ..
            }
        ));
        let _ = tui.close(None);
        let relay_result = relay_thread.join().unwrap();
        fake_release.send(()).unwrap();
        drop(tui);
        if let Err(error) = relay_result {
            // Closing one side of the paired fixture sockets can reset the
            // other after the accepted turn has already been observed.
            assert!(
                format!("{error:#}").contains("Connection reset without closing handshake"),
                "{error:#}"
            );
        }
        fake.join().unwrap();
        assert!(
            fs::read_to_string(trace_path)
                .unwrap()
                .contains("ui-only-replied")
        );
        let evidence = fs::read_to_string(evidence_path).unwrap();
        assert!(!evidence.contains("thread/read"));
        assert!(!evidence.contains("thread/turns/list"));
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
            answer_config_read(&mut socket, "gpt-review");
            let start: Value =
                serde_json::from_str(&socket.read().unwrap().into_text().unwrap()).unwrap();
            assert_eq!(start["method"], "thread/start");
            assert_eq!(start["params"]["ephemeral"], true);
            assert_eq!(start["params"]["historyMode"], "legacy");
            socket
                .send(Message::Text(
                    "{\"id\":3,\"result\":{\"thread\":{\"id\":\"review-thread\"}}}".into(),
                ))
                .unwrap();
        });
        let (mut owner, _) = tungstenite::client("ws://localhost/", owner).unwrap();
        let (thread, _, _) = bootstrap_protocol(
            &mut owner,
            &workspace.path().canonicalize().unwrap(),
            "gpt-review",
            Some("medium"),
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
                Some("medium"),
                CoderSession::FreshPersistent
            )
            .unwrap_err()
            .to_string()
            .contains("did not trust only")
        );
        fake.join().unwrap();
    }

    #[test]
    fn bootstrap_rejects_conflicting_effective_settings_before_any_thread() {
        for (field, value) in [
            ("model", json!("project-unrelated")),
            ("model_reasoning_effort", json!("low")),
            ("hooks", json!(true)),
        ] {
            for resume in [false, true] {
                let value = value.clone();
                let workspace = tempfile::tempdir().unwrap();
                let canonical = workspace.path().canonicalize().unwrap();
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
                    let path = read["params"]["cwd"].as_str().unwrap();
                    let mut config = json!({
                        "projects": {(path): {"trust_level":"trusted"}},
                        "model": "gpt-assigned",
                        "model_reasoning_effort": "medium",
                        "features": {"hooks": false}
                    });
                    if field == "hooks" {
                        config["features"]["hooks"] = value.clone();
                    } else {
                        config[field] = value.clone();
                    }
                    socket
                        .send(Message::Text(
                            json!({"id":2,"result":{"config":config}}).to_string(),
                        ))
                        .unwrap();
                    socket
                        .get_mut()
                        .set_read_timeout(Some(Duration::from_millis(100)))
                        .unwrap();
                    assert!(
                        socket.read().is_err(),
                        "unexpected thread call after {field} mismatch"
                    );
                });
                let (mut owner, _) = tungstenite::client("ws://localhost/", owner).unwrap();
                let session = if resume {
                    CoderSession::Resume("assigned-thread")
                } else {
                    CoderSession::FreshPersistent
                };
                assert!(
                    bootstrap_protocol(
                        &mut owner,
                        &canonical,
                        "gpt-assigned",
                        Some("medium"),
                        session
                    )
                    .is_err(),
                    "{field} mismatch allowed thread launch"
                );
                fake.join().unwrap();
            }
        }
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
                                json!({"id":2,"result":{"config":{
                                    "projects": {(path): {"trust_level":"trusted"}},
                                    "model":"gpt-assigned",
                                    "model_reasoning_effort":"medium",
                                    "features":{"hooks":false}
                                }}})
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
                Some("medium"),
                CoderSession::Resume("assigned-thread"),
            )
            .is_err()
        );
        fake.join().unwrap();
    }
}
