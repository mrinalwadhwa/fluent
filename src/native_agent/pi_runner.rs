//! Run Pi's foreground interface with one owned extension and session file.

use anyhow::{Result, ensure};
use serde_json::Value;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use crate::coder::{CoderSession, PiCode};
use crate::native_agent::Completion;
use crate::native_agent::pi::{
    PiEventGate, receive_event, select_local_model_config, select_local_model_config_with_endpoint,
};
use crate::terminal_sessions::{TerminalIdentity, TerminalSession};

/// A native Pi invocation may have produced side effects before its owner
/// loses the receiver, liveness signal, or durable evidence. The Task must
/// pause for inspection instead of launching another generation in-process.
#[derive(Debug)]
pub struct PiNativeTaskError(anyhow::Error);

impl PiNativeTaskError {
    pub fn new(error: anyhow::Error) -> Self {
        Self(error)
    }
}

impl std::fmt::Display for PiNativeTaskError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{:#}", self.0)
    }
}

impl std::error::Error for PiNativeTaskError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.0.as_ref())
    }
}

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
    /// Place Pi's writable private files in an already confined staging root.
    pub private_root: Option<&'a Path>,
    pub launch_env: &'a [(String, String)],
}

fn source_model_config() -> Option<PathBuf> {
    std::env::var_os("PI_CODING_AGENT_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".pi/agent")))
        .map(|dir| dir.join("models.json"))
}

fn selected_model_config(model: &str) -> Result<Value> {
    let source = source_model_config()
        .filter(|path| path.is_file())
        .ok_or_else(|| anyhow::anyhow!("native Pi requires readable local-openai models.json"))?;
    let config: Value = serde_json::from_slice(&fs::read(source)?)?;
    match std::env::var("FLUENT_NATIVE_PI_ENDPOINT") {
        Ok(endpoint) => select_local_model_config_with_endpoint(
            &config,
            model,
            &endpoint,
            std::env::var("FLUENT_NATIVE_PI_API_KEY").ok().as_deref(),
        ),
        Err(_) => select_local_model_config(&config, model),
    }
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
    values.push((
        "FLUENT_NATIVE_PI_REJECTION_LOG".into(),
        private_home
            .join("native-pi-rejected-input.jsonl")
            .into_os_string(),
    ));
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

// The extension reports once per second while its callback path is healthy.
// Pi's installed runner swallows callback exceptions, and a failed shutdown
// request may leave its pane alive, so terminal liveness alone is insufficient.
const OWNER_HEARTBEAT_DEADLINE: Duration = Duration::from_secs(12);

struct PiOwnerLiveness {
    last_event: Instant,
    deadline: Duration,
}

impl PiOwnerLiveness {
    fn new(now: Instant, deadline: Duration) -> Self {
        Self {
            last_event: now,
            deadline,
        }
    }

    fn observe(&mut self, now: Instant) {
        self.last_event = now;
    }

    fn expired(&self, now: Instant) -> bool {
        now.duration_since(self.last_event) > self.deadline
    }
}

fn receive_until_settled(
    listener: &UnixListener,
    gate: &mut PiEventGate,
    evidence: &mut File,
    is_live: impl FnMut() -> Result<bool>,
    startup_timeout: Duration,
    owner_timeout: Duration,
) -> Result<()> {
    wait_for_pi_completion(
        || match listener.accept() {
            Ok((stream, _)) => {
                let completion = receive_event(stream, gate, evidence)?;
                Ok(Some((completion, gate.session_id().is_some())))
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
            Err(error) => Err(error.into()),
        },
        is_live,
        startup_timeout,
        owner_timeout,
    )
}

fn wait_for_pi_completion(
    mut poll_event: impl FnMut() -> Result<Option<(Completion, bool)>>,
    mut is_live: impl FnMut() -> Result<bool>,
    startup_timeout: Duration,
    owner_timeout: Duration,
) -> Result<()> {
    let startup_deadline = Instant::now() + startup_timeout;
    let mut owner_liveness = PiOwnerLiveness::new(Instant::now(), owner_timeout);
    let mut session_started = false;
    loop {
        match poll_event()? {
            Some((Completion::Accepted { .. }, _)) => return Ok(()),
            Some((Completion::Ignored, started)) => {
                session_started |= started;
                owner_liveness.observe(Instant::now());
            }
            None => {
                if !is_live()? {
                    return Err(anyhow::anyhow!(
                        "native Pi terminal ended without completion"
                    ));
                }
                if !session_started && Instant::now() > startup_deadline {
                    return Err(anyhow::anyhow!("native Pi emitted no session_start event"));
                }
                if session_started && owner_liveness.expired(Instant::now()) {
                    return Err(anyhow::anyhow!(
                        "native Pi extension stopped reporting to its Task owner"
                    ));
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }
}

fn settle_pi_result(run: Result<()>, stop: Result<()>) -> Result<()> {
    match (run, stop) {
        (Err(run_error), Err(stop_error)) => Err(anyhow::anyhow!(
            "{run_error:#}; native Pi terminal settlement also failed: {stop_error:#}"
        )),
        (Err(error), _) | (_, Err(error)) => Err(error),
        (Ok(()), Ok(())) => Ok(()),
    }
}

pub fn run(task: PiNativeTask<'_>) -> Result<()> {
    run_inner(task).map_err(|error| anyhow::Error::new(PiNativeTaskError::new(error)))
}

fn run_inner(task: PiNativeTask<'_>) -> Result<()> {
    let artifact = &task.identity.artifact_dir;
    fs::create_dir_all(artifact)?;
    let private_home_guard = tempfile::Builder::new()
        .prefix("native-pi-home-")
        .tempdir_in(task.private_root.unwrap_or(artifact))?;
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
        let result = receive_until_settled(
            &listener,
            &mut gate,
            &mut evidence,
            || terminal.is_live(),
            Duration::from_secs(60),
            OWNER_HEARTBEAT_DEADLINE,
        );
        let stop = if result.is_ok() {
            terminal.finish()
        } else {
            terminal.stop()
        };
        settle_pi_result(result, stop)?;
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
    let rejected = private_home.join("native-pi-rejected-input.jsonl");
    if rejected.is_file() {
        fs::copy(&rejected, artifact.join("native-pi-rejected-input.jsonl"))?;
        File::open(artifact.join("native-pi-rejected-input.jsonl"))?.sync_all()?;
    }
    outcome?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pi_run_failure_survives_terminal_settlement_failure() {
        let error = settle_pi_result(
            Err(anyhow::anyhow!("Pi extension stopped reporting")),
            Err(anyhow::anyhow!("pane grant query failed")),
        )
        .unwrap_err()
        .to_string();
        assert!(error.starts_with("Pi extension stopped reporting"));
        assert!(error.contains("pane grant query failed"));
    }

    #[test]
    fn owner_expires_after_extension_stops_reporting() {
        let start = Instant::now();
        let mut owner = PiOwnerLiveness::new(start, OWNER_HEARTBEAT_DEADLINE);
        owner.observe(start + Duration::from_secs(1));
        assert!(!owner.expired(start + Duration::from_secs(12)));
        assert!(owner.expired(start + Duration::from_secs(14)));
        owner.observe(start + Duration::from_secs(14));
        assert!(!owner.expired(start + Duration::from_secs(15)));
    }

    #[test]
    fn owner_fails_a_live_pane_after_callback_silence() {
        let mut startup = true;
        let error = wait_for_pi_completion(
            || {
                if startup {
                    startup = false;
                    Ok(Some((Completion::Ignored, true)))
                } else {
                    Ok(None)
                }
            },
            || Ok(true),
            Duration::from_secs(1),
            Duration::from_millis(100),
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("extension stopped reporting"));
    }
}
