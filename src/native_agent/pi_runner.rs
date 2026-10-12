//! Run Pi's foreground interface with one owned extension and session file.

use anyhow::{Result, ensure};
use serde_json::{Value, json};
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use crate::coder::{CoderSession, PiCode};
use crate::native_agent::Completion;
use crate::native_agent::pi::{PiEventGate, receive_event, select_local_model_config};
use crate::terminal_sessions::{TerminalIdentity, TerminalSession};

pub struct PiNativeTask<'a> {
    pub identity: TerminalIdentity,
    pub worker_profile: &'a crate::os::SandboxProfile,
    pub workspace: &'a Path,
    pub model: &'a str,
    pub thinking: &'a str,
    pub prompt: &'a str,
    pub system_prompt: &'a str,
    pub session: CoderSession<'a>,
    pub previous_session: Option<&'a Path>,
    pub launch_env: &'a [(String, String)],
}

fn source_model_config() -> Option<PathBuf> {
    std::env::var_os("PI_CODING_AGENT_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".pi/agent")))
        .map(|dir| dir.join("models.json"))
}

fn selected_model_config(model: &str) -> Result<Value> {
    if let Ok(endpoint) = std::env::var("FLUENT_NATIVE_PI_ENDPOINT") {
        ensure!(
            [
                "http://localhost:",
                "http://localhost/",
                "http://127.0.0.1:",
                "http://127.0.0.1/"
            ]
            .iter()
            .any(|prefix| endpoint.starts_with(prefix)),
            "native Pi endpoint must be a local HTTP server"
        );
        return Ok(json!({"providers":{"local-openai":{
            "baseUrl":endpoint,"api":"openai-completions",
            "apiKey":std::env::var("FLUENT_NATIVE_PI_API_KEY").unwrap_or_else(|_| "local".into()),
            "models":[{"id":model}]
        }}}));
    }
    if let Some(source) = source_model_config()
        && source.is_file()
    {
        let config: Value = serde_json::from_slice(&fs::read(source)?)?;
        return select_local_model_config(&config, model);
    }
    anyhow::bail!(
        "native Pi requires readable local-openai configuration or FLUENT_NATIVE_PI_ENDPOINT"
    )
}

pub fn check_capability(model: &str, thinking: Option<&str>) -> Result<()> {
    ensure!(!model.is_empty(), "native Pi model is unresolved");
    ensure!(
        matches!(
            thinking,
            Some("off" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max")
        ),
        "native Pi thinking level is unresolved or unsupported"
    );
    let output = Command::new("pi").arg("--help").output()?;
    ensure!(output.status.success(), "native Pi capability query failed");
    let help = String::from_utf8_lossy(&output.stdout);
    for flag in ["--no-extensions", "--extension", "--session", "--thinking"] {
        ensure!(help.contains(flag), "native Pi lacks {flag}");
    }
    ensure!(
        Command::new("tmux").arg("-V").output()?.status.success(),
        "native Pi requires tmux"
    );
    selected_model_config(model)?;
    Ok(())
}

fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn command_line(command: &Command) -> Result<String> {
    let mut words =
        vec![quote(command.get_program().to_str().ok_or_else(|| {
            anyhow::anyhow!("Pi executable is not UTF-8")
        })?)];
    for arg in command.get_args() {
        words.push(quote(
            arg.to_str()
                .ok_or_else(|| anyhow::anyhow!("Pi argument is not UTF-8"))?,
        ));
    }
    Ok(words.join(" "))
}

fn environment(
    task: &PiNativeTask<'_>,
    private_home: &Path,
    socket: &Path,
) -> Vec<(OsString, OsString)> {
    let mut values: Vec<_> = [
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
        "SSL_CERT_FILE",
    ]
    .iter()
    .filter_map(|name| std::env::var_os(name).map(|value| (OsString::from(name), value)))
    .collect();
    values.push(("HOME".into(), private_home.as_os_str().to_owned()));
    values.push((
        "PI_CODING_AGENT_DIR".into(),
        private_home.as_os_str().to_owned(),
    ));
    values.push((
        "FLUENT_NATIVE_PI_SOCKET".into(),
        socket.as_os_str().to_owned(),
    ));
    values.push(("PWD".into(), task.workspace.as_os_str().to_owned()));
    values.extend(
        task.launch_env
            .iter()
            .map(|(key, value)| (key.into(), value.into())),
    );
    values
}

fn session_id(path: &Path) -> Result<String> {
    let file = File::open(path)?;
    let mut line = String::new();
    BufReader::new(file).read_line(&mut line)?;
    let header: Value = serde_json::from_str(&line)?;
    ensure!(header["type"] == "session", "Pi session has no header");
    Ok(header["id"]
        .as_str()
        .filter(|id| !id.is_empty())
        .ok_or_else(|| anyhow::anyhow!("Pi session has no identity"))?
        .into())
}

pub fn run(task: PiNativeTask<'_>) -> Result<()> {
    let artifact = &task.identity.artifact_dir;
    fs::create_dir_all(artifact)?;
    let private_home_guard = tempfile::Builder::new()
        .prefix("native-pi-home-")
        .tempdir_in(artifact)?;
    let private_home = private_home_guard.path().to_path_buf();
    let extension = private_home.join("fluent-pi-extension.mjs");
    fs::write(&extension, include_str!("pi_extension.mjs"))?;
    fs::write(
        private_home.join("models.json"),
        serde_json::to_vec(&selected_model_config(task.model)?)?,
    )?;
    let session_file = private_home.join("session.jsonl");
    if let CoderSession::Resume(expected) = task.session {
        let previous = task.previous_session.ok_or_else(|| {
            anyhow::anyhow!("native Pi continuation has no prior session snapshot")
        })?;
        ensure!(
            session_id(previous)? == expected,
            "native Pi prior session has another conversation identity"
        );
        fs::copy(previous, &session_file)?;
    }
    let sockets = tempfile::Builder::new()
        .prefix("fluent-na-")
        .tempdir_in("/private/tmp")?;
    let socket = sockets.path().join("pi.sock");
    let listener = UnixListener::bind(&socket)?;
    listener.set_nonblocking(true)?;
    let generation_file = sockets.path().join("generation");
    let coder = PiCode {
        sandbox_profile: None,
        trusted_sandbox: false,
        model_override: Some(task.model.into()),
    };
    let command = coder.native_task_command_in_terminal(
        task.workspace,
        &private_home,
        &extension,
        &session_file,
        task.thinking,
        task.system_prompt,
        task.prompt,
    )?;
    let pane = artifact.join("native-pi-pane.sh");
    fs::write(
        &pane,
        format!(
            "#!/bin/sh\nset -eu\nfor n in $(seq 1 1200); do\n  [ -f {} ] && break\n  sleep 0.05\ndone\n[ -f {} ] || exit 1\nexport FLUENT_NATIVE_PI_GENERATION=$(cat {})\nexec {}\n",
            quote(&generation_file.display().to_string()),
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
        &environment(&task, &private_home, &socket),
        &[sockets.path().to_path_buf()],
    )?;
    let outcome = (|| -> Result<()> {
        let generation = terminal.descriptor().execution_generation.clone();
        let mut gate = PiEventGate::new(&generation, &session_file);
        let transcript = artifact.join("transcript.jsonl");
        let mut evidence = OpenOptions::new()
            .append(true)
            .create(true)
            .open(&transcript)?;
        fs::write(&generation_file, &generation)?;
        let deadline = Instant::now() + Duration::from_secs(60);
        let result = loop {
            match listener.accept() {
                Ok((stream, _)) => match receive_event(stream, &mut gate, &mut evidence) {
                    Ok(Completion::Accepted { .. }) => break Ok(()),
                    Ok(Completion::Ignored) => {}
                    Err(error) => break Err(error),
                },
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if !terminal.is_live()? {
                        break Err(anyhow::anyhow!(
                            "native Pi terminal ended without completion"
                        ));
                    }
                    if gate.session_id().is_none() && Instant::now() > deadline {
                        break Err(anyhow::anyhow!("native Pi emitted no session_start event"));
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(error) => break Err(error.into()),
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
        ensure!(
            session_id(&session_file)? == gate.session_id().unwrap_or_default(),
            "Pi session identity changed before evidence copy"
        );
        fs::copy(&session_file, artifact.join("pi-session.jsonl"))?;
        File::open(artifact.join("pi-session.jsonl"))?.sync_all()?;
        Ok(())
    })();
    if outcome.is_err() {
        let _ = terminal.stop();
    }
    outcome?;
    Ok(())
}
