//! Bind a host-local terminal socket to one supervised worker invocation.

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crate::execution::{self, ExecutionRecord, ExecutionState};
use crate::lease::{self, LeaseAttempt, TaskLease};

const DESCRIPTOR: &str = "terminal-session.json";

#[derive(Debug, Clone)]
pub struct TerminalIdentity {
    pub source_root: PathBuf,
    pub work_item_id: String,
    pub attempt_id: String,
    pub task_id: String,
    pub role: String,
    pub provider: String,
    pub artifact_dir: PathBuf,
}

impl TerminalIdentity {
    #[cfg(feature = "test-support")]
    pub fn fixture(root: &Path, work: &str, attempt: &str, task: &str) -> Self {
        let root = root.canonicalize().unwrap();
        Self {
            source_root: root.clone(),
            work_item_id: work.into(),
            attempt_id: attempt.into(),
            task_id: task.into(),
            role: "writer".into(),
            provider: "fixture".into(),
            artifact_dir: root
                .join(".fluent/work/artifacts")
                .join(work)
                .join(attempt)
                .join(task),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TerminalLifecycle {
    Starting,
    Running,
    Settling,
    Ended,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalDescriptor {
    pub schema_version: u32,
    pub source_id: String,
    pub project_identity: String,
    pub work_item_id: String,
    pub attempt_id: String,
    pub task_id: String,
    pub execution_generation: String,
    pub session_id: String,
    pub role: String,
    pub provider: String,
    pub lifecycle: TerminalLifecycle,
    pub can_observe: bool,
    pub can_control: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unavailable_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredTerminal {
    descriptor: TerminalDescriptor,
    execution_record: PathBuf,
    socket: Option<PathBuf>,
}

pub struct TerminalSession {
    stored: StoredTerminal,
    child: Option<Child>,
    socket_dir: Option<PathBuf>,
    _profile: Option<crate::os::SandboxProfile>,
}

pub struct Attachment {
    stored: StoredTerminal,
    control: Option<TaskLease>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlOutcome {
    Occupied,
    Acquired,
}

fn record_for(identity: &TerminalIdentity) -> PathBuf {
    identity.artifact_dir.join("execution.json")
}

fn descriptor_path(record: &Path) -> PathBuf {
    record.with_file_name(DESCRIPTOR)
}

fn read_record(path: &Path) -> Result<ExecutionRecord> {
    serde_json::from_slice(&std::fs::read(path).context("reading terminal execution owner")?)
        .context("parsing terminal execution owner")
}

fn find_tmux() -> Result<PathBuf> {
    for dir in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
        let candidate = dir.join("tmux");
        if candidate.is_file() {
            return candidate
                .canonicalize()
                .context("resolving tmux executable");
        }
    }
    bail!("tmux is required for managed terminal sessions")
}

fn validate(stored: &StoredTerminal, requested: &TerminalDescriptor) -> Result<()> {
    ensure!(stored.descriptor == *requested, "terminal identity changed");
    let published: StoredTerminal =
        serde_json::from_slice(&std::fs::read(descriptor_path(&stored.execution_record))?)?;
    ensure!(
        published.descriptor == *requested,
        "terminal descriptor changed"
    );
    ensure!(
        published.execution_record == stored.execution_record,
        "terminal owner path changed"
    );
    ensure!(published.socket == stored.socket, "terminal socket changed");
    let record = read_record(&stored.execution_record)?;
    ensure!(
        record.state == ExecutionState::Running
            && execution::verified_running_generation(
                &stored.execution_record,
                &requested.execution_generation
            )?,
        "terminal execution generation is no longer running"
    );
    Ok(())
}

impl TerminalSession {
    fn start_command(
        identity: TerminalIdentity,
        mut command: Command,
        socket_dir: Option<PathBuf>,
        attempt_owner: bool,
        ready: impl FnOnce(Option<&Path>) -> Result<()>,
    ) -> Result<Self> {
        std::fs::create_dir_all(&identity.artifact_dir)?;
        let transcript = identity.artifact_dir.join("transcript.jsonl");
        let owner = attempt_owner.then(|| execution::AttemptExecutionOwner {
            project_root: identity.source_root.clone(),
            work_item_id: identity.work_item_id.clone(),
            attempt_id: identity.attempt_id.clone(),
        });
        execution::prepare_launch(&transcript, owner)?;
        command.stdout(Stdio::null()).stderr(Stdio::null());
        let mut child = match execution::spawn(&mut command, &transcript) {
            Ok(child) => child,
            Err(error) => {
                if let Some(dir) = &socket_dir {
                    let _ = std::fs::remove_dir_all(dir);
                }
                return Err(error).context("starting managed terminal server");
            }
        };
        let record_path = record_for(&identity);
        let setup = (|| -> Result<StoredTerminal> {
            ready(socket_dir.as_deref())?;
            let record = read_record(&record_path)?;
            let generation = record
                .launch_generation
                .context("terminal has no launch generation")?;
            ensure!(
                execution::verified_running_generation(&record_path, &generation)?,
                "terminal server stopped during setup"
            );
            let session_id = format!("{}-{}", identity.task_id, generation);
            let root = identity.source_root.canonicalize()?;
            let descriptor = TerminalDescriptor {
                schema_version: 1,
                source_id: root.to_string_lossy().into_owned(),
                project_identity: root.to_string_lossy().into_owned(),
                work_item_id: identity.work_item_id,
                attempt_id: identity.attempt_id,
                task_id: identity.task_id,
                execution_generation: generation,
                session_id,
                role: identity.role,
                provider: identity.provider,
                lifecycle: TerminalLifecycle::Running,
                can_observe: true,
                can_control: true,
                unavailable_reason: None,
            };
            let stored = StoredTerminal {
                descriptor,
                execution_record: record_path.clone(),
                socket: socket_dir.as_ref().map(|dir| dir.join("socket")),
            };
            crate::atomic_write::atomic_write(
                &descriptor_path(&record_path),
                &serde_json::to_vec_pretty(&stored)?,
            )?;
            Ok(stored)
        })();
        let stored = match setup {
            Ok(stored) => stored,
            Err(error) => {
                let cleanup = execution::cancel(&record_path)
                    .and_then(|_| child.wait().map(|_| ()).map_err(Into::into));
                if let Some(dir) = &socket_dir {
                    let _ = std::fs::remove_dir_all(dir);
                }
                return Err(error)
                    .context(format!("setting up managed terminal; cleanup: {cleanup:?}"));
            }
        };
        let session = Self {
            stored,
            child: Some(child),
            socket_dir,
            _profile: None,
        };
        Ok(session)
    }

    #[cfg(feature = "test-support")]
    pub fn start_fixture(identity: TerminalIdentity, command: &str) -> Result<Self> {
        let mut child = Command::new("/bin/sh");
        child.arg("-c").arg(command);
        Self::start_command(identity, child, None, false, |_| Ok(()))
    }

    /// Start a foreground tmux server inside the supplied worker profile.
    /// Pane commands inherit that server's sandbox and private environment.
    pub fn start_managed(
        identity: TerminalIdentity,
        worker_profile: &crate::os::SandboxProfile,
        shell_command: &str,
        environment: &[(std::ffi::OsString, std::ffi::OsString)],
    ) -> Result<Self> {
        let socket_dir = tempfile::Builder::new()
            .prefix("fluent-t-")
            .tempdir_in("/private/tmp")?
            .keep();
        let socket = socket_dir.join("socket");
        let profile = crate::os::profile_with_terminal_socket(worker_profile, &socket_dir)?;
        let tmux = find_tmux()?;
        let mut command = crate::os::command_under_profile(&profile, &tmux);
        command
            .arg("-S")
            .arg(&socket)
            .arg("-f")
            .arg("/dev/null")
            .arg("-D");
        command.env_clear();
        for (key, value) in environment {
            command.env(key, value);
        }
        let mut session = Self::start_command(identity, command, Some(socket_dir), true, |_| {
            let deadline = Instant::now() + Duration::from_secs(2);
            while !socket.exists() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(20));
            }
            ensure!(socket.exists(), "tmux server did not create its socket");
            let output = Command::new(&tmux)
                .arg("-S")
                .arg(&socket)
                .arg("new-session")
                .arg("-d")
                .arg("-s")
                .arg("fluent")
                .arg(shell_command)
                .output()
                .context("creating managed tmux session")?;
            ensure!(
                output.status.success(),
                "creating managed tmux session: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            Ok(())
        })?;
        session._profile = Some(profile);
        Ok(session)
    }

    pub fn descriptor(&self) -> &TerminalDescriptor {
        &self.stored.descriptor
    }

    pub fn is_live(&self) -> Result<bool> {
        execution::execution_is_live(&self.stored.execution_record)
    }

    pub fn observe(&self, requested: &TerminalDescriptor) -> Result<Attachment> {
        validate(&self.stored, requested)?;
        Ok(Attachment {
            stored: self.stored.clone(),
            control: None,
        })
    }

    pub fn stop(&mut self) -> Result<()> {
        if self.child.is_some() {
            execution::cancel(&self.stored.execution_record)?;
            if let Some(mut child) = self.child.take() {
                child.wait()?;
            }
        }
        if let Some(dir) = self.socket_dir.take() {
            std::fs::remove_dir_all(dir)?;
        }
        Ok(())
    }
}

impl Attachment {
    pub fn descriptor(&self) -> &TerminalDescriptor {
        &self.stored.descriptor
    }

    pub fn acquire_control(&mut self) -> Result<ControlOutcome> {
        validate(&self.stored, &self.stored.descriptor)?;
        if self.control.is_some() {
            return Ok(ControlOutcome::Acquired);
        }
        let path = self
            .stored
            .execution_record
            .with_file_name("terminal-control.lock");
        match lease::try_acquire(&path)? {
            LeaseAttempt::Acquired(lease) => {
                validate(&self.stored, &self.stored.descriptor)?;
                self.control = Some(lease);
                Ok(ControlOutcome::Acquired)
            }
            LeaseAttempt::Contended => Ok(ControlOutcome::Occupied),
        }
    }

    pub fn resize(&self, _columns: u16, _rows: u16) -> Result<()> {
        validate(&self.stored, &self.stored.descriptor)
    }

    pub fn run(&self) -> Result<i32> {
        validate(&self.stored, &self.stored.descriptor)?;
        let socket = self
            .stored
            .socket
            .as_ref()
            .context("fixture has no tmux socket")?;
        let mut command = Command::new("tmux");
        command.arg("-S").arg(socket).arg("attach-session");
        if self.control.is_none() {
            command.arg("-r");
        }
        let status = command.status().context("attaching managed terminal")?;
        Ok(status.code().unwrap_or(1))
    }

    pub fn detach(self) {}
}

pub fn resolve(source_root: &Path, session_id: &str) -> Result<Attachment> {
    for descriptor in list_stored(source_root)? {
        if descriptor.descriptor.session_id == session_id {
            validate(&descriptor, &descriptor.descriptor)?;
            return Ok(Attachment {
                stored: descriptor,
                control: None,
            });
        }
    }
    bail!("managed terminal {session_id} is unavailable for this source")
}

fn list_stored(source_root: &Path) -> Result<Vec<StoredTerminal>> {
    let source_root = source_root.canonicalize()?;
    let root = source_root.join(".fluent/work/artifacts");
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut pending = vec![root.clone()];
    let mut found = Vec::new();
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                pending.push(entry.path());
            } else if entry.file_name() == DESCRIPTOR {
                ensure!(
                    entry.file_type()?.is_file(),
                    "terminal descriptor is not a regular file"
                );
                let stored: StoredTerminal = serde_json::from_slice(&std::fs::read(entry.path())?)?;
                for id in [
                    &stored.descriptor.work_item_id,
                    &stored.descriptor.attempt_id,
                    &stored.descriptor.task_id,
                ] {
                    ensure!(
                        Path::new(id).components().count() == 1
                            && Path::new(id)
                                .components()
                                .all(|part| matches!(part, std::path::Component::Normal(_))),
                        "terminal identity contains an invalid path component"
                    );
                }
                let expected = root
                    .join(&stored.descriptor.work_item_id)
                    .join(&stored.descriptor.attempt_id)
                    .join(&stored.descriptor.task_id)
                    .join("execution.json");
                ensure!(
                    stored.execution_record == expected,
                    "terminal owner path escapes its source"
                );
                ensure!(
                    stored.descriptor.source_id == source_root.to_string_lossy(),
                    "terminal source identity changed"
                );
                found.push(stored);
            }
        }
    }
    Ok(found)
}

pub fn list(source_root: &Path) -> Result<Vec<TerminalDescriptor>> {
    let mut descriptors = Vec::new();
    for stored in list_stored(source_root)? {
        let mut descriptor = stored.descriptor.clone();
        if validate(&stored, &descriptor).is_err() {
            descriptor.lifecycle = TerminalLifecycle::Ended;
            descriptor.can_observe = false;
            descriptor.can_control = false;
            descriptor.unavailable_reason =
                Some("worker invocation has ended or changed generation".into());
        }
        descriptors.push(descriptor);
    }
    Ok(descriptors)
}
