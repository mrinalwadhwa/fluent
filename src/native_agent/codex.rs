//! Launch one assigned Codex conversation behind the Task's terminal owner.

use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::net::Shutdown;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
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
    let result = match session {
        CoderSession::Resume(thread_id) => {
            ensure!(
                !thread_id.is_empty(),
                "native Codex resume identity is empty"
            );
            request(
                socket,
                2,
                "thread/resume",
                json!({
                    "threadId": thread_id, "cwd": workspace,
                }),
            )?
        }
        CoderSession::FreshPersistent | CoderSession::Ephemeral => request(
            socket,
            2,
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
    values.push(("HOME".into(), task.private_home.as_os_str().to_owned()));
    values.push((
        "CODEX_HOME".into(),
        task.private_home.as_os_str().to_owned(),
    ));
    values.push(("PWD".into(), task.workspace.as_os_str().to_owned()));
    values.push(("GIT_EDITOR".into(), "false".into()));
    values.push(("GIT_SEQUENCE_EDITOR".into(), "false".into()));
    values.extend(
        task.launch_env
            .iter()
            .map(|(key, value)| (key.into(), value.into())),
    );
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
    let (display_tx, display_rx) = sync_channel::<String>(32);
    std::thread::scope(|scope| {
        let display_client = Arc::clone(&client);
        let display = scope.spawn(move || {
            while let Ok(text) = display_rx.recv() {
                let Ok(mut client) = display_client.lock() else {
                    break;
                };
                if client.send(Message::Text(text)).is_err() {
                    break;
                }
            }
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
                    let admitted = request_gate.lock()
                        .map_err(|_| anyhow::anyhow!("native Codex relay gate poisoned"))?
                        .admit_request(&request);
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
                let mut display = Some(display_tx);
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
                    if let Some(sender) = display.as_ref()
                        && matches!(
                            sender.try_send(line),
                            Err(TrySendError::Full(_) | TrySendError::Disconnected(_))
                        )
                    {
                        display = None;
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
        display
            .join()
            .map_err(|_| anyhow::anyhow!("Codex WebSocket display relay panicked"))?;
        upstream.and(downstream)
    })
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
    let sockets = tempfile::Builder::new()
        .prefix("fluent-na-")
        .tempdir_in("/private/tmp")?;
    let app_socket = sockets.path().join("app.sock");
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
    let mut terminal = TerminalSession::start_managed_with_sockets(
        task.identity.clone(),
        task.worker_profile,
        task.workspace,
        &shell_command,
        &environment(&task),
        &[sockets.path().to_path_buf()],
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
        stop?;
        joined.map_err(|_| anyhow::anyhow!("native Codex relay panicked"))?;
        result?;
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
    use std::os::unix::fs::PermissionsExt;

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
    fn bootstrap_binds_one_assigned_thread_before_tui_starts() {
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
            let start: Value =
                serde_json::from_str(&socket.read().unwrap().into_text().unwrap()).unwrap();
            assert_eq!(start["method"], "thread/start");
            assert_eq!(start["params"]["model"], "gpt-assigned");
            assert_eq!(start["params"]["approvalPolicy"], "never");
            socket
                .send(Message::Text(
                    "{\"id\":2,\"result\":{\"thread\":{\"id\":\"assigned-thread\"}}}".into(),
                ))
                .unwrap();
        });
        let (mut owner, _) = tungstenite::client("ws://localhost/", owner).unwrap();
        let (thread, _) = bootstrap_protocol(
            &mut owner,
            Path::new("/private/work"),
            "gpt-assigned",
            CoderSession::FreshPersistent,
        )
        .unwrap();
        assert_eq!(thread, "assigned-thread");
        fake.join().unwrap();
    }

    #[test]
    fn bootstrap_starts_ephemeral_reviewer_thread() {
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
            let start: Value =
                serde_json::from_str(&socket.read().unwrap().into_text().unwrap()).unwrap();
            assert_eq!(start["method"], "thread/start");
            assert_eq!(start["params"]["ephemeral"], true);
            socket
                .send(Message::Text(
                    "{\"id\":2,\"result\":{\"thread\":{\"id\":\"review-thread\"}}}".into(),
                ))
                .unwrap();
        });
        let (mut owner, _) = tungstenite::client("ws://localhost/", owner).unwrap();
        let (thread, _) = bootstrap_protocol(
            &mut owner,
            Path::new("/private/review"),
            "gpt-review",
            CoderSession::Ephemeral,
        )
        .unwrap();
        assert_eq!(thread, "review-thread");
        fake.join().unwrap();
    }

    #[test]
    fn bootstrap_rejects_a_different_resumed_thread() {
        let (owner, provider) = UnixStream::pair().unwrap();
        let fake = std::thread::spawn(move || {
            let mut socket = tungstenite::accept(provider).unwrap();
            for _ in 0..3 {
                let message: Value =
                    serde_json::from_str(&socket.read().unwrap().into_text().unwrap()).unwrap();
                match message["method"].as_str().unwrap() {
                    "initialize" => socket
                        .send(Message::Text("{\"id\":1,\"result\":{}}".into()))
                        .unwrap(),
                    "initialized" => {}
                    "thread/resume" => {
                        assert_eq!(message["params"]["threadId"], "assigned-thread");
                        socket
                            .send(Message::Text(
                                "{\"id\":2,\"result\":{\"thread\":{\"id\":\"another-thread\"}}}"
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
                Path::new("/private/work"),
                "gpt-assigned",
                CoderSession::Resume("assigned-thread"),
            )
            .is_err()
        );
        fake.join().unwrap();
    }
}
