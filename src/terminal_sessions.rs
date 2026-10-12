//! Bind a host-local terminal socket to one supervised worker invocation.

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::os::unix::process::CommandExt;
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
    registry_path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TerminalResource {
    execution_record: PathBuf,
    socket: PathBuf,
    #[serde(default)]
    pane_sessions: Vec<i32>,
    #[serde(default)]
    owner_marker: String,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TerminalAvailability {
    pub descriptor: Option<TerminalDescriptor>,
    pub unavailable_reason: Option<String>,
}

pub fn unavailable_reason(
    kind: crate::work_model::TaskKind,
    status: crate::work_model::TaskStatus,
    has_execution: bool,
) -> String {
    use crate::work_model::{TaskKind, TaskStatus};
    if matches!(
        status,
        TaskStatus::Complete | TaskStatus::Failed | TaskStatus::NeedsUser | TaskStatus::Superseded
    ) {
        return "This worker has completed; its invocation cannot receive input.".into();
    }
    if !matches!(
        kind,
        TaskKind::Write | TaskKind::Review | TaskKind::Rebase | TaskKind::Learn
    ) {
        return if matches!(kind, TaskKind::Tester | TaskKind::Verification) {
            "This Tester or verification worker has no agent conversation to attach to.".into()
        } else {
            "This deterministic worker has no agent conversation to attach to.".into()
        };
    }
    if has_execution {
        "This worker uses headless execution and has no live agent terminal.".into()
    } else {
        "This legacy or not-yet-started worker has no managed terminal session.".into()
    }
}

/// Explain availability for one selected Task using the source's published
/// Work model and the generation-bound terminal descriptor.
pub fn availability(
    source_root: &Path,
    work_id: &str,
    attempt_id: &str,
    task_id: &str,
) -> Result<TerminalAvailability> {
    let source_root = source_root.canonicalize()?;
    let store = crate::work_model::WorkModelStore::new(&source_root);
    let item = store.read_work_item_snapshot(work_id)?;
    let attempt = item
        .attempts
        .iter()
        .find(|attempt| attempt.id == attempt_id)
        .context("selected Attempt does not exist")?;
    let task = attempt
        .tasks
        .iter()
        .find(|task| task.id == task_id)
        .context("selected Task does not exist")?;
    if !matches!(
        task.kind,
        crate::work_model::TaskKind::Write
            | crate::work_model::TaskKind::Review
            | crate::work_model::TaskKind::Rebase
            | crate::work_model::TaskKind::Learn
    ) || matches!(
        task.status,
        crate::work_model::TaskStatus::Complete
            | crate::work_model::TaskStatus::Failed
            | crate::work_model::TaskStatus::NeedsUser
            | crate::work_model::TaskStatus::Superseded
    ) {
        return Ok(TerminalAvailability {
            descriptor: None,
            unavailable_reason: Some(unavailable_reason(task.kind, task.status.clone(), true)),
        });
    }
    if let Some(descriptor) = list(&source_root)?.into_iter().find(|descriptor| {
        descriptor.work_item_id == work_id
            && descriptor.attempt_id == attempt_id
            && descriptor.task_id == task_id
    }) {
        if descriptor.lifecycle == TerminalLifecycle::Running {
            return Ok(TerminalAvailability {
                descriptor: Some(descriptor),
                unavailable_reason: None,
            });
        }
        return Ok(TerminalAvailability {
            descriptor: None,
            unavailable_reason: descriptor.unavailable_reason,
        });
    }
    let execution = source_root
        .join(".fluent/work/artifacts")
        .join(work_id)
        .join(attempt_id)
        .join(task_id)
        .join("execution.json");
    Ok(TerminalAvailability {
        descriptor: None,
        unavailable_reason: Some(unavailable_reason(
            task.kind,
            task.status.clone(),
            execution.exists(),
        )),
    })
}

fn record_for(identity: &TerminalIdentity) -> PathBuf {
    identity.artifact_dir.join("execution.json")
}

fn registry_dir(identity: &TerminalIdentity) -> PathBuf {
    identity
        .source_root
        .join(".fluent/work/terminal-sessions")
        .join(&identity.work_item_id)
        .join(&identity.attempt_id)
        .join(&identity.task_id)
}

fn descriptor_path(identity: &TerminalIdentity) -> PathBuf {
    registry_dir(identity).join(DESCRIPTOR)
}

fn resource_path(identity: &TerminalIdentity) -> PathBuf {
    registry_dir(identity).join("resource.json")
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
    let published: StoredTerminal = serde_json::from_slice(&std::fs::read(&stored.registry_path)?)?;
    ensure!(
        published.descriptor == *requested,
        "terminal descriptor changed"
    );
    ensure!(
        published.execution_record == stored.execution_record,
        "terminal owner path changed"
    );
    ensure!(published.socket == stored.socket, "terminal socket changed");
    ensure!(
        published.registry_path == stored.registry_path,
        "terminal registry changed"
    );
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
        ensure!(
            identity.artifact_dir.canonicalize()? == identity.artifact_dir,
            "terminal artifact area resolves outside its Task"
        );
        std::fs::create_dir_all(registry_dir(&identity))?;
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
            let registry_path = descriptor_path(&identity);
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
                registry_path,
            };
            crate::atomic_write::atomic_write(
                &stored.registry_path,
                &serde_json::to_vec_pretty(&stored)?,
            )?;
            Ok(stored)
        })();
        let stored = match setup {
            Ok(stored) => stored,
            Err(error) => {
                let cleanup = execution::cancel(&record_path)
                    .and_then(|_| child.wait().map(|_| ()).map_err(Into::into));
                if cleanup.is_ok() {
                    if let Some(dir) = &socket_dir {
                        let _ = std::fs::remove_dir_all(dir);
                    }
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

    #[cfg(feature = "test-support")]
    pub fn start_tmux_fixture(identity: TerminalIdentity) -> Result<Self> {
        let socket_dir = tempfile::Builder::new()
            .prefix("fluent-t-")
            .tempdir_in("/private/tmp")?
            .keep();
        let socket = socket_dir.join("socket");
        let mut command = Command::new(find_tmux()?);
        command.args(["-S", socket.to_str().unwrap(), "-f", "/dev/null", "-D"]);
        Self::start_command(identity, command, Some(socket_dir), false, |_| {
            let deadline = Instant::now() + Duration::from_secs(5);
            while !socket.exists() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(20));
            }
            ensure!(socket.exists(), "fixture tmux server did not start");
            let output = Command::new(find_tmux()?)
                .arg("-S")
                .arg(&socket)
                .args(["new-session", "-d", "-s", "fluent", "/bin/sh"])
                .output()?;
            ensure!(
                output.status.success(),
                "fixture tmux session failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            Ok(())
        })
    }

    #[cfg(feature = "test-support")]
    pub fn start_fixture_failed_setup(identity: TerminalIdentity) -> Result<Self> {
        let mut child = Command::new("/bin/sh");
        child.arg("-c").arg("sleep 30");
        Self::start_command(identity, child, None, false, |_| {
            bail!("injected terminal setup failure")
        })
    }

    /// Start a foreground tmux server inside the supplied worker profile.
    /// Pane commands inherit that server's sandbox and private environment.
    pub fn start_managed(
        identity: TerminalIdentity,
        worker_profile: &crate::os::SandboxProfile,
        working_dir: &Path,
        shell_command: &str,
        environment: &[(std::ffi::OsString, std::ffi::OsString)],
    ) -> Result<Self> {
        Self::start_managed_with_sockets(
            identity,
            worker_profile,
            working_dir,
            shell_command,
            environment,
            &[],
        )
    }

    pub fn start_managed_with_sockets(
        mut identity: TerminalIdentity,
        worker_profile: &crate::os::SandboxProfile,
        working_dir: &Path,
        shell_command: &str,
        environment: &[(std::ffi::OsString, std::ffi::OsString)],
        provider_socket_dirs: &[PathBuf],
    ) -> Result<Self> {
        identity.source_root = identity.source_root.canonicalize()?;
        for id in [
            &identity.work_item_id,
            &identity.attempt_id,
            &identity.task_id,
        ] {
            ensure!(
                Path::new(id).components().count() == 1
                    && Path::new(id)
                        .components()
                        .all(|part| matches!(part, std::path::Component::Normal(_))),
                "terminal identity contains an invalid path component"
            );
        }
        let expected_artifact = identity
            .source_root
            .join(".fluent/work/artifacts")
            .join(&identity.work_item_id)
            .join(&identity.attempt_id)
            .join(&identity.task_id);
        ensure!(
            identity.artifact_dir == expected_artifact,
            "terminal artifact area is outside its Task"
        );
        let store = crate::work_model::WorkModelStore::new(&identity.source_root);
        let item = store.read_work_item_snapshot(&identity.work_item_id)?;
        let task = item
            .attempts
            .iter()
            .find(|attempt| attempt.id == identity.attempt_id)
            .and_then(|attempt| {
                attempt
                    .tasks
                    .iter()
                    .find(|task| task.id == identity.task_id)
            })
            .context("managed terminal Task is absent from its source")?;
        ensure!(
            matches!(
                task.kind,
                crate::work_model::TaskKind::Write
                    | crate::work_model::TaskKind::Review
                    | crate::work_model::TaskKind::Rebase
                    | crate::work_model::TaskKind::Learn
            ),
            "this worker has no agent conversation to steer"
        );
        let socket_guard = tempfile::Builder::new()
            .prefix("fluent-t-")
            .tempdir_in("/private/tmp")?;
        let socket_dir = socket_guard.path().to_path_buf();
        let socket = socket_dir.join("socket");
        let owner_handle = socket_dir.join("owner");
        std::fs::File::create(&owner_handle).context("creating terminal owner handle")?;
        let profile = crate::os::profile_with_terminal_sockets(
            worker_profile,
            &socket_dir,
            provider_socket_dirs,
        )?;
        let tmux = find_tmux()?;
        std::fs::create_dir_all(registry_dir(&identity))?;
        let resource = TerminalResource {
            execution_record: record_for(&identity),
            socket: socket.clone(),
            pane_sessions: Vec::new(),
            owner_marker: socket_dir
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
        };
        let resource_path = resource_path(&identity);
        let mut resource_file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&resource_path)
            .context("reserving managed terminal resource")?;
        if let Err(error) = resource_file
            .write_all(&serde_json::to_vec_pretty(&resource)?)
            .and_then(|_| resource_file.sync_all())
        {
            drop(resource_file);
            let _ = std::fs::remove_file(&resource_path);
            return Err(error).context("writing managed terminal resource");
        }
        let mut command = crate::os::command_under_profile(&profile, &tmux);
        command
            .arg("-S")
            .arg(&socket)
            .arg("-f")
            .arg("/dev/null")
            .arg("-D");
        command.env_clear();
        command.current_dir(working_dir);
        for (key, value) in environment {
            command.env(key, value);
        }
        command.env_remove("TMUX");
        let result = Self::start_command(identity, command, Some(socket_dir), true, |_| {
            let deadline = Instant::now() + Duration::from_secs(2);
            while !socket.exists() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(20));
            }
            ensure!(socket.exists(), "tmux server did not create its socket");
            #[cfg(feature = "test-support")]
            if let Some(barrier) = std::env::var_os("FLUENT_TEST_TERMINAL_SETUP_BARRIER") {
                let barrier = PathBuf::from(barrier);
                std::fs::write(barrier.with_extension("ready"), b"server-ready")?;
                while !barrier.exists() {
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
            #[cfg(feature = "test-support")]
            if std::env::var_os("FLUENT_TEST_TERMINAL_FAIL_AFTER_SERVER").is_some() {
                bail!("injected terminal failure after server startup");
            }
            // The open handle survives exec, environment replacement, setsid,
            // and reparenting. Its private inode identifies this generation.
            let quoted_handle = format!(
                "'{}'",
                owner_handle.display().to_string().replace('\'', "'\\''")
            );
            let pane_command = format!("exec 9<{quoted_handle}; exec {shell_command}");
            let output = Command::new(&tmux)
                .env_remove("TMUX")
                .arg("-S")
                .arg(&socket)
                .arg("new-session")
                .arg("-d")
                .arg("-E")
                .arg("-s")
                .arg("fluent")
                .arg(&pane_command)
                .output()
                .context("creating managed tmux session")?;
            ensure!(
                output.status.success(),
                "creating managed tmux session: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            let panes = tmux_pane_pids(&socket)?;
            ensure!(!panes.is_empty(), "managed session has no pane owner");
            let mut owned = resource.clone();
            for pane in panes {
                owned.pane_sessions.push(process_session(pane)?);
            }
            owned.pane_sessions.sort_unstable();
            owned.pane_sessions.dedup();
            crate::atomic_write::atomic_write(&resource_path, &serde_json::to_vec_pretty(&owned)?)?;
            Ok(())
        });
        let mut session = match result {
            Ok(session) => session,
            Err(error) => {
                let record_path = resource.execution_record;
                let settled = read_record(&record_path)
                    .map(|record| record.state == ExecutionState::Canceled)
                    .unwrap_or(false);
                // Spawn may fail before an execution owner exists. In that
                // case, inspect the receipt's handle and sandbox grant before
                // discarding it; server exit alone proves nothing about panes.
                let fully_settled = settled
                    || (!execution::execution_is_live(&record_path).unwrap_or(false)
                        && settle_panes(&socket, 0, &[], &resource.owner_marker).is_ok());
                if fully_settled {
                    let _ = std::fs::remove_file(resource_path);
                } else {
                    // The server may exit while a pane descendant still owns
                    // this generation. Keep its receipt and owner handle so
                    // a later cancellation can complete the inventory.
                    let _ = socket_guard.keep();
                }
                return Err(error);
            }
        };
        let _ = socket_guard.keep();
        session._profile = Some(profile);
        Ok(session)
    }

    pub fn descriptor(&self) -> &TerminalDescriptor {
        &self.stored.descriptor
    }

    #[cfg(feature = "test-support")]
    pub fn socket_path(&self) -> Option<&Path> {
        self.stored.socket.as_deref()
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
            let record = read_record(&self.stored.execution_record)?;
            ensure!(
                record.launch_generation.as_deref()
                    == Some(&self.stored.descriptor.execution_generation),
                "managed terminal generation changed before settlement"
            );
            execution::cancel(&self.stored.execution_record)?;
            if let Some(mut child) = self.child.take() {
                child.wait()?;
            }
        }
        self.socket_dir.take();
        Ok(())
    }

    /// End a completed native invocation without canceling the owning Task's
    /// remaining validation and artifact commands.
    pub fn finish(&mut self) -> Result<()> {
        if self.child.is_some() {
            let record = read_record(&self.stored.execution_record)?;
            ensure!(
                record.launch_generation.as_deref()
                    == Some(&self.stored.descriptor.execution_generation),
                "managed terminal generation changed before settlement"
            );
            execution::settle_managed_terminal(&self.stored.execution_record)?;
            if let Some(mut child) = self.child.take() {
                child.wait()?;
            }
        }
        self.socket_dir.take();
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
            .registry_path
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

    pub fn resize(&self, columns: u16, rows: u16) -> Result<()> {
        validate(&self.stored, &self.stored.descriptor)?;
        ensure!(
            self.control.is_some(),
            "Control is required to resize the shared terminal"
        );
        ensure!(
            columns > 0 && rows > 0,
            "terminal dimensions must be positive"
        );
        let socket = self
            .stored
            .socket
            .as_ref()
            .context("fixture has no tmux socket")?;
        let status = Command::new(find_tmux()?)
            .env_remove("TMUX")
            .arg("-S")
            .arg(socket)
            .args([
                "resize-window",
                "-t",
                "fluent",
                "-x",
                &columns.to_string(),
                "-y",
                &rows.to_string(),
            ])
            .status()?;
        ensure!(status.success(), "resizing managed terminal failed");
        Ok(())
    }

    pub fn run(&self) -> Result<i32> {
        validate(&self.stored, &self.stored.descriptor)?;
        let socket = self
            .stored
            .socket
            .as_ref()
            .context("fixture has no tmux socket")?;
        let mut command = Command::new(find_tmux()?);
        command.env_remove("TMUX");
        command
            .arg("-S")
            .arg(socket)
            .arg("attach-session")
            .arg("-E");
        if self.control.is_none() {
            command.arg("-r");
        }
        let status = spawn_attachment_client(&mut command, self.control.as_ref())
            .context("attaching managed terminal")?
            .wait()?;
        Ok(status.code().unwrap_or(1))
    }

    pub fn detach(self) {}
}

fn spawn_attachment_client(command: &mut Command, control: Option<&TaskLease>) -> Result<Child> {
    let Some(control) = control else {
        return command.spawn().context("starting observation client");
    };
    let fd = control.raw_fd();
    // The tmux client keeps the lease across Fluent's exit. A shell wrapper
    // cannot own it: the shell can die while the writable tmux client lives.
    unsafe {
        command.pre_exec(move || {
            let flags = libc::fcntl(fd, libc::F_GETFD);
            if flags < 0 || libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    command
        .spawn()
        .context("starting writable attachment client")
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
    let root = source_root.join(".fluent/work/terminal-sessions");
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
                    .join(DESCRIPTOR);
                ensure!(
                    stored.registry_path == expected && entry.path() == expected,
                    "terminal registry path escapes its source"
                );
                let execution = source_root
                    .join(".fluent/work/artifacts")
                    .join(&stored.descriptor.work_item_id)
                    .join(&stored.descriptor.attempt_id)
                    .join(&stored.descriptor.task_id)
                    .join("execution.json");
                ensure!(
                    stored.execution_record == execution,
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

/// Follow parent links from tmux pane PIDs, including children in other
/// process groups. A server PID or socket alone cannot prove pane settlement.
pub fn descendants_from_ps(table: &str, pane_pids: &[u32]) -> Result<Vec<u32>> {
    let mut parents = BTreeMap::new();
    for line in table.lines() {
        let parts: Vec<_> = line.split_whitespace().collect();
        ensure!(parts.len() == 3, "invalid process inventory row");
        let pid: u32 = parts[0].parse()?;
        let ppid: u32 = parts[1].parse()?;
        let _group: u32 = parts[2].parse()?;
        parents.insert(pid, ppid);
    }
    let mut owned: BTreeSet<u32> = pane_pids.iter().copied().collect();
    loop {
        let before = owned.len();
        for (&pid, &ppid) in &parents {
            if owned.contains(&ppid) {
                owned.insert(pid);
            }
        }
        if owned.len() == before {
            break;
        }
    }
    Ok(owned.into_iter().collect())
}

fn process_table() -> Result<String> {
    let output = Command::new("/bin/ps")
        .args(["-axo", "pid=,ppid=,pgid="])
        .output()?;
    ensure!(
        output.status.success(),
        "reading terminal process inventory failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).context("terminal process inventory is not UTF-8")
}

fn process_session(pid: u32) -> Result<i32> {
    let sid = unsafe { libc::getsid(i32::try_from(pid)?) };
    if sid < 0 {
        return Err(std::io::Error::last_os_error()).context("reading terminal process session");
    }
    Ok(sid)
}

fn session_members(sessions: &[i32]) -> Result<Vec<u32>> {
    if sessions.is_empty() {
        return Ok(Vec::new());
    }
    let output = Command::new("/bin/ps").args(["-axo", "pid="]).output()?;
    ensure!(
        output.status.success(),
        "reading terminal process IDs failed"
    );
    let mut members = Vec::new();
    for line in String::from_utf8(output.stdout)?.lines() {
        let pid: u32 = line.trim().parse()?;
        let sid = unsafe { libc::getsid(i32::try_from(pid)?) };
        if sid < 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ESRCH) {
                continue; // The process exited after ps listed it.
            }
            return Err(error).context("reading terminal process session inventory");
        }
        if sessions.contains(&sid) {
            members.push(pid);
        }
    }
    Ok(members)
}

/// Parse the kernel's open-file inventory for the private owner handle.
pub fn owner_members_from_lsof(table: &str) -> Result<Vec<u32>> {
    let mut members = BTreeSet::new();
    for line in table.lines() {
        if let Some(pid) = line.strip_prefix('p') {
            members.insert(pid.parse()?);
        }
    }
    Ok(members.into_iter().collect())
}

fn owner_members(handle: &Path) -> Result<Vec<u32>> {
    ensure!(handle.is_file(), "terminal owner handle is missing");
    let output = Command::new("/usr/sbin/lsof")
        .args(["-F", "p", "--"])
        .arg(handle)
        .output()?;
    ensure!(
        output.status.success() || (output.status.code() == Some(1) && output.stdout.is_empty()),
        "reading terminal owner handles failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    owner_members_from_lsof(&String::from_utf8(output.stdout)?)
}

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn sandbox_check(
        pid: libc::pid_t,
        operation: *const libc::c_char,
        filter: libc::c_int,
        ...
    ) -> libc::c_int;
}

/// The private socket grant remains in a descendant's Seatbelt policy after
/// it closes descriptors, clears its environment, or changes POSIX session.
/// Compare it with the existing temporary root so an unrestricted process is
/// not adopted. A nonexistent path gives unreliable policy query results.
#[cfg(target_os = "macos")]
fn policy_members(socket: &Path, panes: &[u32]) -> Result<Vec<u32>> {
    use std::ffi::CString;

    let dir = socket
        .parent()
        .context("terminal socket has no directory")?;
    let owner = dir.join("owner");
    ensure!(owner.is_file(), "terminal policy owner probe is missing");
    ensure!(
        Path::new("/private/tmp").is_dir(),
        "terminal policy outside probe is missing"
    );
    let owned = CString::new(owner.as_os_str().as_encoded_bytes())?;
    let outside = CString::new(b"/private/tmp".as_slice())?;
    let operation = c"file-write-data";
    let matches = |pid: u32| -> Result<bool> {
        let pid = i32::try_from(pid)?;
        // SANDBOX_FILTER_PATH is 1. sandbox_check returns zero for an allow.
        let own = unsafe { sandbox_check(pid, operation.as_ptr(), 1, owned.as_ptr()) };
        let peer = unsafe { sandbox_check(pid, operation.as_ptr(), 1, outside.as_ptr()) };
        Ok(policy_grants_owner_only(own, peer))
    };
    for pane in panes {
        ensure!(
            matches(*pane)?,
            "terminal pane does not have its generation-specific sandbox grant"
        );
    }
    let output = Command::new("/bin/ps").args(["-axo", "pid="]).output()?;
    ensure!(
        output.status.success(),
        "reading terminal policy process inventory failed"
    );
    let mut members = Vec::new();
    for line in String::from_utf8(output.stdout)?.lines() {
        let pid: u32 = line.trim().parse()?;
        if matches(pid)? {
            members.push(pid);
        }
    }
    Ok(members)
}

/// Require an explicit allow for the generation handle and an explicit deny
/// for a real path outside its private grant. Negative query results are errors.
#[cfg(target_os = "macos")]
#[doc(hidden)]
pub fn policy_grants_owner_only(own: i32, outside: i32) -> bool {
    own == 0 && outside == 1
}

#[cfg(all(target_os = "macos", feature = "test-support"))]
#[doc(hidden)]
pub fn policy_members_for_test(socket: &Path, panes: &[u32]) -> Result<Vec<u32>> {
    policy_members(socket, panes)
}

fn tmux_pane_pids(socket: &Path) -> Result<Vec<u32>> {
    let output = Command::new(find_tmux()?)
        .env_remove("TMUX")
        .arg("-S")
        .arg(socket)
        .args(["list-panes", "-a", "-F", "#{pane_pid}"])
        .output()?;
    ensure!(
        output.status.success(),
        "reading managed pane owners failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)?
        .lines()
        .map(|line| line.parse().map_err(Into::into))
        .collect()
}

fn signal_pid(pid: u32, signal: i32) -> Result<()> {
    let pid = i32::try_from(pid)?;
    let result = unsafe { libc::kill(pid, signal) };
    if result == 0 {
        return Ok(());
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        return Ok(());
    }
    Err(error).context("signaling terminal pane descendant")
}

fn process_start(pid: u32) -> Result<Option<String>> {
    let output = Command::new("/bin/ps")
        .args(["-o", "lstart=", "-p", &pid.to_string()])
        .output()?;
    if !output.status.success() && output.stdout.is_empty() {
        return Ok(None);
    }
    ensure!(
        output.status.success(),
        "reading terminal process start time failed"
    );
    let value = String::from_utf8(output.stdout)?;
    Ok((!value.trim().is_empty()).then(|| value.trim().to_string()))
}

fn signal_same_process(pid: u32, started: &str, signal: i32) -> Result<()> {
    match process_start(pid)? {
        None => Ok(()),
        Some(actual) if actual == started => signal_pid(pid, signal),
        Some(_) => bail!("terminal descendant PID {pid} was reused before settlement"),
    }
}

fn live_pids(pids: &[u32]) -> Result<Vec<u32>> {
    let output = Command::new("/bin/ps")
        .args(["-axo", "pid=,stat="])
        .output()?;
    ensure!(
        output.status.success(),
        "reading terminal process states failed"
    );
    let table = String::from_utf8(output.stdout)?;
    let live: BTreeSet<u32> = table
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let pid = fields.next()?.parse().ok()?;
            let state = fields.next()?;
            (!state.starts_with('Z')).then_some(pid)
        })
        .collect();
    Ok(pids
        .iter()
        .copied()
        .filter(|pid| live.contains(pid))
        .collect())
}

struct StoppedPanes(Vec<u32>);

impl StoppedPanes {
    fn stop(&mut self, pid: u32) -> Result<()> {
        signal_pid(pid, libc::SIGSTOP)?;
        self.0.push(pid);
        Ok(())
    }

    fn resume(&mut self) -> Result<()> {
        while let Some(pid) = self.0.pop() {
            if let Err(error) = signal_pid(pid, libc::SIGCONT) {
                self.0.push(pid);
                return Err(error);
            }
        }
        Ok(())
    }
}

impl Drop for StoppedPanes {
    fn drop(&mut self) {
        for pid in self.0.drain(..) {
            let _ = signal_pid(pid, libc::SIGCONT);
        }
    }
}

fn settle_panes(socket: &Path, leader_pid: u32, sessions: &[i32], marker: &str) -> Result<()> {
    let output = if socket.exists() {
        Some(
            Command::new(find_tmux()?)
                .env_remove("TMUX")
                .arg("-S")
                .arg(socket)
                .args(["list-panes", "-a", "-F", "#{pane_pid}"])
                .output()?,
        )
    } else {
        None
    };
    if let Some(output) = &output
        && !output.status.success()
    {
        let error = String::from_utf8_lossy(&output.stderr);
        let has_sessions = if error.contains("no current target") {
            Some(tmux_server_has_sessions(socket)?)
        } else {
            None
        };
        check_pane_query_failure(&error, has_sessions)?;
    }
    let panes: Vec<u32> =
        String::from_utf8(output.map(|output| output.stdout).unwrap_or_default())?
            .lines()
            .map(str::parse)
            .collect::<std::result::Result<_, _>>()?;
    let server_tree = descendants_from_ps(&process_table()?, &[leader_pid])?;
    ensure!(
        panes.iter().all(|pane| server_tree.contains(pane)),
        "tmux pane owner is outside the verified execution tree"
    );
    let mut stopped = StoppedPanes(Vec::new());
    for pid in &panes {
        stopped.stop(*pid)?;
    }
    let mut owned = BTreeSet::new();
    let mut stable = false;
    for _ in 0..8 {
        let mut now = descendants_from_ps(&process_table()?, &panes)?;
        now.extend(session_members(sessions)?);
        if !marker.is_empty() {
            now.extend(owner_members(&socket.with_file_name("owner"))?);
            #[cfg(target_os = "macos")]
            now.extend(policy_members(socket, &panes)?);
        }
        now.retain(|pid| *pid != leader_pid);
        let before = owned.len();
        for pid in now {
            if owned.insert(pid) {
                stopped.stop(pid)?;
            }
        }
        if before == owned.len() {
            stable = true;
            break;
        }
    }
    ensure!(
        stable,
        "terminal descendants changed throughout settlement inventory"
    );
    let pids: Vec<u32> = owned.into_iter().collect();
    let mut starts = BTreeMap::new();
    for pid in &pids {
        if let Some(start) = process_start(*pid)? {
            starts.insert(*pid, start);
        }
    }
    for pid in pids.iter().rev() {
        if let Some(start) = starts.get(pid) {
            signal_same_process(*pid, start, libc::SIGTERM)?;
        }
    }
    stopped.resume()?;
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        if live_pids(&pids)?.is_empty() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    for pid in live_pids(&pids)? {
        let start = starts
            .get(&pid)
            .context("terminal descendant lacked a start identity")?;
        signal_same_process(pid, start, libc::SIGKILL)?;
    }
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        if live_pids(&pids)?.is_empty() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    bail!("managed terminal pane descendants did not settle")
}

fn tmux_server_has_sessions(socket: &Path) -> Result<bool> {
    let output = Command::new(find_tmux()?)
        .env_remove("TMUX")
        .arg("-S")
        .arg(socket)
        .arg("list-sessions")
        .output()?;
    if output.status.success() {
        return Ok(!output.stdout.is_empty());
    }
    let error = String::from_utf8_lossy(&output.stderr);
    if error.contains("no sessions") {
        return Ok(false);
    }
    bail!("checking managed terminal sessions failed: {error}")
}

fn check_pane_query_failure(error: &str, has_sessions: Option<bool>) -> Result<()> {
    if error.contains("no sessions")
        || (error.contains("no current target") && has_sessions == Some(false))
    {
        return Ok(());
    }
    bail!("reading managed pane owners failed: {error}")
}

fn descriptor_publishes_resource(
    stored: &StoredTerminal,
    resource: &TerminalResource,
    generation: Option<&str>,
) -> bool {
    generation.is_some_and(|generation| stored.descriptor.execution_generation == generation)
        && stored.execution_record == resource.execution_record
        && stored.socket.as_ref() == Some(&resource.socket)
}

/// Settle pane descendants before the existing execution owner signals its
/// server group. The resource receipt lives outside worker-writable artifacts.
pub(crate) fn settle_for_execution(path: &Path, record: &ExecutionRecord) -> Result<()> {
    let Some(owner) = &record.attempt_owner else {
        return Ok(());
    };
    let task = path
        .parent()
        .and_then(Path::file_name)
        .context("terminal execution has no Task directory")?;
    for id in [&owner.work_item_id, &owner.attempt_id] {
        ensure!(
            Path::new(id).components().count() == 1
                && Path::new(id)
                    .components()
                    .all(|part| matches!(part, std::path::Component::Normal(_))),
            "terminal execution owner contains an invalid identifier"
        );
    }
    let registry = owner
        .project_root
        .join(".fluent/work/terminal-sessions")
        .join(&owner.work_item_id)
        .join(&owner.attempt_id)
        .join(task);
    let resource_path = registry.join("resource.json");
    let metadata = match std::fs::symlink_metadata(&resource_path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let expected = owner
        .project_root
        .join(".fluent/work/artifacts")
        .join(&owner.work_item_id)
        .join(&owner.attempt_id)
        .join(task)
        .join("execution.json");
    ensure!(
        path == expected,
        "terminal execution record is outside its owner Task"
    );
    ensure!(
        metadata.is_file(),
        "terminal resource receipt is not a regular file"
    );
    let resource: TerminalResource = serde_json::from_slice(&std::fs::read(&resource_path)?)?;
    ensure!(
        resource.execution_record == path,
        "terminal resource owner changed"
    );
    let socket_dir = resource
        .socket
        .parent()
        .context("terminal socket has no directory")?;
    ensure!(
        socket_dir.parent() == Some(Path::new("/private/tmp"))
            && socket_dir
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("fluent-t-"))
            && resource
                .socket
                .file_name()
                .is_some_and(|name| name == "socket"),
        "terminal socket path is outside its private namespace"
    );
    ensure!(
        resource.owner_marker.is_empty()
            || socket_dir
                .file_name()
                .is_some_and(|name| name == resource.owner_marker.as_str()),
        "terminal owner marker changed"
    );
    // The resource receipt exists before descriptor publication. It remains
    // authoritative when setup fails or the server exits first.
    if !resource.socket.exists()
        && resource.pane_sessions.is_empty()
        && resource.owner_marker.is_empty()
    {
        bail!("managed terminal socket disappeared before pane descendants could be verified");
    }
    settle_panes(
        &resource.socket,
        record.leader_pid,
        &resource.pane_sessions,
        &resource.owner_marker,
    )
}

/// Release only the resource receipt owned by the settled execution generation.
pub(crate) fn retire_for_execution(path: &Path, record: &ExecutionRecord) -> Result<()> {
    let Some(owner) = &record.attempt_owner else {
        return Ok(());
    };
    let task = path
        .parent()
        .and_then(Path::file_name)
        .context("terminal Task path is absent")?;
    let registry = owner
        .project_root
        .join(".fluent/work/terminal-sessions")
        .join(&owner.work_item_id)
        .join(&owner.attempt_id)
        .join(task);
    let receipt = registry.join("resource.json");
    match std::fs::symlink_metadata(&receipt) {
        Ok(metadata) => ensure!(
            metadata.is_file(),
            "terminal resource receipt is not a regular file"
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    }
    let expected = owner
        .project_root
        .join(".fluent/work/artifacts")
        .join(&owner.work_item_id)
        .join(&owner.attempt_id)
        .join(task)
        .join("execution.json");
    ensure!(
        path == expected,
        "terminal execution owner changed before retirement"
    );
    let bytes = match std::fs::read(&receipt) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let resource: TerminalResource = serde_json::from_slice(&bytes)?;
    ensure!(
        resource.execution_record == path,
        "terminal resource owner changed"
    );
    match std::fs::read(registry.join(DESCRIPTOR)) {
        Ok(bytes) => {
            let stored: StoredTerminal = serde_json::from_slice(&bytes)?;
            if !descriptor_publishes_resource(
                &stored,
                &resource,
                record.launch_generation.as_deref(),
            ) {
                return Ok(());
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let dir = resource
        .socket
        .parent()
        .context("terminal socket directory is absent")?;
    ensure!(
        dir.parent() == Some(Path::new("/private/tmp"))
            && dir
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("fluent-t-"))
            && resource
                .socket
                .file_name()
                .is_some_and(|name| name == "socket"),
        "terminal socket path is outside its private namespace"
    );
    if dir.exists() {
        std::fs::remove_dir_all(dir)?;
    }
    std::fs::remove_file(receipt)?;
    Ok(())
}

#[cfg(test)]
mod control_client_tests {
    use super::*;

    #[test]
    fn writable_client_itself_holds_control_after_parent_releases_lease() {
        let root = tempfile::tempdir().unwrap();
        let occupied_descriptors: Vec<_> = (0..16)
            .map(|index| std::fs::File::create(root.path().join(format!("open-{index}"))).unwrap())
            .collect();
        let path = root.path().join("control.lock");
        let lease = match lease::try_acquire(&path).unwrap() {
            LeaseAttempt::Acquired(lease) => lease,
            LeaseAttempt::Contended => panic!("new control lock was occupied"),
        };
        let pid_file = root.path().join("client.pid");
        let mut command = Command::new("/bin/sh");
        command
            .arg("-c")
            .arg(format!("echo $$ > '{}'; exec sleep 30", pid_file.display()));
        let mut client = spawn_attachment_client(&mut command, Some(&lease)).unwrap();
        drop(lease);
        assert!(matches!(
            lease::try_acquire(&path).unwrap(),
            LeaseAttempt::Contended
        ));
        let deadline = Instant::now() + Duration::from_secs(5);
        while !pid_file.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        let pid: i32 = std::fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert_eq!(
            client.id() as i32,
            pid,
            "the lease holder must be the writable client"
        );
        client.kill().unwrap();
        client.wait().unwrap();
        assert!(matches!(
            lease::try_acquire(&path).unwrap(),
            LeaseAttempt::Acquired(_)
        ));
        drop(occupied_descriptors);
    }

    #[test]
    fn writable_client_keeps_control_after_parent_process_exits() {
        if let Some(root) = std::env::var_os("FLUENT_TEST_CONTROL_PARENT") {
            let root = PathBuf::from(root);
            let lease = match lease::try_acquire(&root.join("control.lock")).unwrap() {
                LeaseAttempt::Acquired(lease) => lease,
                LeaseAttempt::Contended => panic!("new control lock was occupied"),
            };
            let mut command = Command::new("/bin/sh");
            command.args(["-c", "sleep 2"]);
            let client = spawn_attachment_client(&mut command, Some(&lease)).unwrap();
            std::fs::write(root.join("client.pid"), client.id().to_string()).unwrap();
            std::process::exit(0);
        }

        let root = tempfile::tempdir().unwrap();
        let status = Command::new(std::env::current_exe().unwrap())
            .arg("writable_client_keeps_control_after_parent_process_exits")
            .env("FLUENT_TEST_CONTROL_PARENT", root.path())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert!(status.success());
        let pid: i32 = std::fs::read_to_string(root.path().join("client.pid"))
            .unwrap()
            .parse()
            .unwrap();
        let path = root.path().join("control.lock");
        assert!(matches!(
            lease::try_acquire(&path).unwrap(),
            LeaseAttempt::Contended
        ));
        assert_eq!(unsafe { libc::kill(pid, libc::SIGKILL) }, 0);
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match lease::try_acquire(&path).unwrap() {
                LeaseAttempt::Acquired(_) => break,
                LeaseAttempt::Contended if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                LeaseAttempt::Contended => panic!("Control stayed occupied after client exit"),
            }
        }
    }
}

#[cfg(test)]
mod setup_cleanup_tests {
    use super::*;

    #[test]
    fn unpublished_exited_server_still_settles_owner_handle_descendant() {
        let source = tempfile::tempdir().unwrap();
        let socket_dir = tempfile::Builder::new()
            .prefix("fluent-t-")
            .tempdir_in("/private/tmp")
            .unwrap();
        let owner_handle = socket_dir.path().join("owner");
        std::fs::write(&owner_handle, b"").unwrap();
        let registry = source
            .path()
            .join(".fluent/work/terminal-sessions/work-1/attempt-1/task-1");
        std::fs::create_dir_all(&registry).unwrap();
        let record_path = source
            .path()
            .join(".fluent/work/artifacts/work-1/attempt-1/task-1/execution.json");
        std::fs::create_dir_all(record_path.parent().unwrap()).unwrap();
        let resource = TerminalResource {
            execution_record: record_path.clone(),
            socket: socket_dir.path().join("socket"),
            pane_sessions: vec![],
            owner_marker: socket_dir
                .path()
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
        };
        std::fs::write(
            registry.join("resource.json"),
            serde_json::to_vec(&resource).unwrap(),
        )
        .unwrap();
        let record = ExecutionRecord {
            schema_version: 2,
            state: ExecutionState::Settled,
            owner_pid: 0,
            leader_pid: 0,
            process_group_id: 0,
            started_at: String::new(),
            heartbeat_at: String::new(),
            settled_at: None,
            attempt_owner: Some(execution::AttemptExecutionOwner {
                project_root: source.path().into(),
                work_item_id: "work-1".into(),
                attempt_id: "attempt-1".into(),
            }),
            launch_generation: Some("generation-1".into()),
        };
        std::fs::write(&record_path, serde_json::to_vec(&record).unwrap()).unwrap();
        let mut child = Command::new("/bin/sh")
            .arg("-c")
            .arg(format!(
                "exec 9< '{}'; exec /bin/sleep 30",
                owner_handle.display()
            ))
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while !owner_members(&owner_handle).unwrap().contains(&child.id())
            && Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(owner_members(&owner_handle).unwrap().contains(&child.id()));
        let outcome = execution::cancel(&record_path);
        let still_running = child.try_wait().unwrap().is_none();
        if still_running {
            child.kill().unwrap();
        }
        child.wait().unwrap();
        match outcome {
            Ok(_) => {
                assert!(
                    !still_running,
                    "unpublished terminal descendant survived cancellation"
                );
                assert!(!registry.join("resource.json").exists());
            }
            Err(error) => {
                // Some Writer sandboxes deny even a child-process signal.
                // Cancellation must retain the receipt and report that denial.
                assert!(
                    error.to_string().contains("Operation not permitted"),
                    "unexpected settlement error: {error:#}"
                );
                assert!(registry.join("resource.json").exists());
            }
        }
    }

    #[test]
    fn open_owner_handle_survives_environment_clear_without_adopting_arguments() {
        let dir = tempfile::tempdir_in("/private/tmp").unwrap();
        let handle = dir.path().join("owner");
        std::fs::File::create(&handle).unwrap();
        let command = format!(
            "exec 9<'{}'; exec /usr/bin/env -i /bin/sleep 30",
            handle.display()
        );
        let mut owned = Command::new("/bin/sh")
            .arg("-c")
            .arg(command)
            .spawn()
            .unwrap();
        let marker = format!("FLUENT_TERMINAL_OWNER={}", dir.path().display());
        let mut unrelated = Command::new("/usr/bin/perl")
            .args(["-e", "sleep 30", &marker])
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let members = owner_members(&handle).unwrap();
            if members.contains(&owned.id()) {
                assert!(!members.contains(&unrelated.id()));
                break;
            }
            assert!(Instant::now() < deadline, "open owner handle was not found");
            std::thread::sleep(Duration::from_millis(20));
        }
        owned.kill().unwrap();
        unrelated.kill().unwrap();
        owned.wait().unwrap();
        unrelated.wait().unwrap();
    }

    #[test]
    fn empty_server_pane_query_allows_partial_setup_cleanup() {
        assert!(check_pane_query_failure("no current target", Some(false)).is_ok());
        assert!(check_pane_query_failure("no sessions", None).is_ok());
        assert!(check_pane_query_failure("no current target", Some(true)).is_err());
        assert!(check_pane_query_failure("permission denied", None).is_err());
    }

    #[test]
    fn stale_descriptor_does_not_publish_a_new_generation() {
        let mut descriptor: TerminalDescriptor = serde_json::from_str(include_str!(
            "../tests/fixtures/terminal-descriptor-v1.json"
        ))
        .unwrap();
        descriptor.execution_generation = "old-generation".into();
        let stored = StoredTerminal {
            descriptor,
            execution_record: PathBuf::from("/tmp/execution.json"),
            socket: Some(PathBuf::from("/private/tmp/fluent-t-test/socket")),
            registry_path: PathBuf::from("/tmp/terminal-session.json"),
        };
        let resource = TerminalResource {
            execution_record: stored.execution_record.clone(),
            socket: stored.socket.clone().unwrap(),
            pane_sessions: vec![],
            owner_marker: String::new(),
        };
        assert!(!descriptor_publishes_resource(
            &stored,
            &resource,
            Some("new-generation")
        ));
        assert!(descriptor_publishes_resource(
            &stored,
            &resource,
            Some("old-generation")
        ));
    }

    #[test]
    fn retirement_ignores_unmanaged_execution_without_a_receipt() {
        let source = tempfile::tempdir().unwrap();
        let path = source
            .path()
            .join(".fluent/work/artifacts/work-1/attempt-1/learner/runs/run-1/execution.json");
        let record = ExecutionRecord {
            schema_version: 2,
            state: ExecutionState::Canceled,
            owner_pid: 0,
            leader_pid: 0,
            process_group_id: 0,
            started_at: String::new(),
            heartbeat_at: String::new(),
            settled_at: None,
            attempt_owner: Some(execution::AttemptExecutionOwner {
                project_root: source.path().into(),
                work_item_id: "work-1".into(),
                attempt_id: "attempt-1".into(),
            }),
            launch_generation: None,
        };
        retire_for_execution(&path, &record).unwrap();
    }

    #[test]
    fn retirement_removes_only_the_settled_generation_resource() {
        let source = tempfile::tempdir().unwrap();
        let socket_dir = tempfile::Builder::new()
            .prefix("fluent-t-")
            .tempdir_in("/private/tmp")
            .unwrap();
        let socket = socket_dir.path().join("socket");
        std::fs::write(&socket, b"socket fixture").unwrap();
        let registry = source
            .path()
            .join(".fluent/work/terminal-sessions/work-1/attempt-1/task-1");
        std::fs::create_dir_all(&registry).unwrap();
        let record_path = source
            .path()
            .join(".fluent/work/artifacts/work-1/attempt-1/task-1/execution.json");
        let resource = TerminalResource {
            execution_record: record_path.clone(),
            socket: socket.clone(),
            pane_sessions: vec![],
            owner_marker: String::new(),
        };
        std::fs::write(
            registry.join("resource.json"),
            serde_json::to_vec(&resource).unwrap(),
        )
        .unwrap();
        let record = ExecutionRecord {
            schema_version: 2,
            state: ExecutionState::Canceled,
            owner_pid: 0,
            leader_pid: 0,
            process_group_id: 0,
            started_at: String::new(),
            heartbeat_at: String::new(),
            settled_at: None,
            attempt_owner: Some(execution::AttemptExecutionOwner {
                project_root: source.path().into(),
                work_item_id: "work-1".into(),
                attempt_id: "attempt-1".into(),
            }),
            launch_generation: Some("old".into()),
        };
        let substituted_path = source.path().join("other/task-1/execution.json");
        assert!(retire_for_execution(&substituted_path, &record).is_err());
        assert!(registry.join("resource.json").exists());
        let stored = StoredTerminal {
            descriptor: TerminalDescriptor {
                schema_version: 1,
                source_id: String::new(),
                project_identity: String::new(),
                work_item_id: "work-1".into(),
                attempt_id: "attempt-1".into(),
                task_id: "task-1".into(),
                execution_generation: "new".into(),
                session_id: String::new(),
                role: String::new(),
                provider: String::new(),
                lifecycle: TerminalLifecycle::Running,
                can_observe: true,
                can_control: true,
                unavailable_reason: None,
            },
            execution_record: record_path.clone(),
            socket: Some(socket.clone()),
            registry_path: registry.join(DESCRIPTOR),
        };
        std::fs::write(
            registry.join(DESCRIPTOR),
            serde_json::to_vec(&stored).unwrap(),
        )
        .unwrap();
        retire_for_execution(&record_path, &record).unwrap();
        assert!(registry.join("resource.json").exists());
        assert!(socket.exists());
        let mut current = record;
        current.launch_generation = Some("new".into());
        retire_for_execution(&record_path, &current).unwrap();
        assert!(!registry.join("resource.json").exists());
        assert!(!socket.exists());
    }
}
