//! Bind a host-local terminal socket to one supervised worker invocation.

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

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
        Self {
            source_root: root.to_path_buf(),
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
            && record.launch_generation.as_deref() == Some(&requested.execution_generation)
            && execution::execution_is_live(&stored.execution_record)?,
        "terminal execution generation is no longer running"
    );
    Ok(())
}

impl TerminalSession {
    fn start_command(
        identity: TerminalIdentity,
        mut command: Command,
        socket_dir: Option<PathBuf>,
    ) -> Result<Self> {
        std::fs::create_dir_all(&identity.artifact_dir)?;
        let transcript = identity.artifact_dir.join("transcript.jsonl");
        execution::prepare_launch(&transcript, None)?;
        command.stdout(Stdio::null()).stderr(Stdio::null());
        let child = match execution::spawn(&mut command, &transcript) {
            Ok(child) => child,
            Err(error) => {
                if let Some(dir) = &socket_dir {
                    let _ = std::fs::remove_dir_all(dir);
                }
                return Err(error).context("starting managed terminal server");
            }
        };
        let record_path = record_for(&identity);
        let record = read_record(&record_path)?;
        let generation = record
            .launch_generation
            .context("terminal has no launch generation")?;
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
        let mut session = Self {
            stored,
            child: Some(child),
            socket_dir,
        };
        if let Err(error) = crate::atomic_write::atomic_write(
            &descriptor_path(&record_path),
            &serde_json::to_vec_pretty(&session.stored)?,
        ) {
            let cleanup = session.stop();
            return Err(error).context(format!(
                "publishing managed terminal descriptor; cleanup: {cleanup:?}"
            ));
        }
        Ok(session)
    }

    #[cfg(feature = "test-support")]
    pub fn start_fixture(identity: TerminalIdentity, command: &str) -> Result<Self> {
        let mut child = Command::new("/bin/sh");
        child.arg("-c").arg(command);
        Self::start_command(identity, child, None)
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
    let root = source_root.join(".fluent/work/artifacts");
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut pending = vec![root];
    let mut found = Vec::new();
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                pending.push(entry.path());
            } else if entry.file_name() == DESCRIPTOR {
                found.push(serde_json::from_slice(&std::fs::read(entry.path())?)?);
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
