//! Persist and control process ownership for local workers.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

const EXECUTION_FILE: &str = "execution.json";
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(1);
const TERMINATION_GRACE: Duration = Duration::from_secs(2);
const LAUNCH_PROTOCOL_VERSION: u32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ExecutionState {
    Launching,
    Running,
    Settled,
    Canceled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionRecord {
    pub schema_version: u32,
    pub state: ExecutionState,
    pub owner_pid: u32,
    pub leader_pid: u32,
    pub process_group_id: u32,
    pub started_at: String,
    pub heartbeat_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settled_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt_owner: Option<AttemptExecutionOwner>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch_generation: Option<String>,
}

fn check_protocol(record: &ExecutionRecord) -> Result<()> {
    anyhow::ensure!(
        record.schema_version == 1 || record.schema_version == LAUNCH_PROTOCOL_VERSION,
        "unsupported local execution protocol {}",
        record.schema_version
    );
    anyhow::ensure!(
        record.schema_version == 1 || record.launch_generation.is_some(),
        "launch identity is missing its generation"
    );
    Ok(())
}

fn receipt_path(path: &Path) -> PathBuf {
    path.with_file_name("execution-receipt")
}

pub(crate) fn receipt_absent(path: &Path) -> Result<bool> {
    match std::fs::symlink_metadata(receipt_path(path)) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Ok(_) => Ok(false),
        Err(error) => Err(error.into()),
    }
}

/// A prior child may leave its receipt in the Task area when preparation
/// writes a new launch generation and fails before spawning that child.
/// Accept only a well-formed receipt for a different generation; an invalid
/// receipt cannot establish which launch it belongs to.
pub(crate) fn receipt_absent_or_prior_generation(path: &Path, current: &str) -> Result<bool> {
    if !valid_launch_generation(current) {
        return Ok(false);
    }
    let receipt = receipt_path(path);
    let metadata = match std::fs::symlink_metadata(&receipt) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(true),
        Ok(metadata) => metadata,
        Err(error) => return Err(error.into()),
    };
    if !metadata.file_type().is_file() {
        return Ok(false);
    }
    let bytes = std::fs::read(&receipt)?;
    let Ok(text) = std::str::from_utf8(&bytes) else {
        return Ok(false);
    };
    let Some(rest) = text.strip_prefix("2 ") else {
        return Ok(false);
    };
    let Some((generation, pid_line)) = rest.split_once(' ') else {
        return Ok(false);
    };
    let Some(pid) = pid_line.strip_suffix('\n') else {
        return Ok(false);
    };
    Ok(generation != current
        && valid_launch_generation(generation)
        && pid.len() == 10
        && pid.bytes().all(|byte| byte.is_ascii_digit())
        && pid.parse::<u32>().is_ok_and(|pid| pid > 0))
}

fn valid_launch_generation(generation: &str) -> bool {
    generation.len() == 32
        && generation
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn new_generation() -> Result<String> {
    let mut bytes = [0u8; 16];
    File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn receipt_identity(path: &Path, record: &ExecutionRecord) -> Result<Option<u32>> {
    if record.schema_version == 1 {
        return Ok(None);
    }
    let bytes = match std::fs::read(receipt_path(path)) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let Some(generation) = record.launch_generation.as_deref() else {
        return Ok(None);
    };
    let Ok(text) = std::str::from_utf8(&bytes) else {
        return Ok(None);
    };
    let Some(rest) = text.strip_prefix(&format!("2 {generation} ")) else {
        return Ok(None);
    };
    let Some(pid_text) = rest.strip_suffix('\n') else {
        return Ok(None);
    };
    if pid_text.len() != 10 || !pid_text.bytes().all(|byte| byte.is_ascii_digit()) {
        return Ok(None);
    }
    let pid: u32 = pid_text.parse()?;
    Ok((pid > 0).then_some(pid))
}

fn effective_identity(path: &Path, record: &ExecutionRecord) -> Result<Option<(u32, u32)>> {
    check_protocol(record)?;
    if record.schema_version == 1 {
        return Ok((record.state == ExecutionState::Running
            && record.leader_pid > 0
            && record.process_group_id > 0)
            .then_some((record.leader_pid, record.process_group_id)));
    }
    let Some(pid) = receipt_identity(path, record)? else {
        return Ok(None);
    };
    if record.leader_pid != 0 && record.leader_pid != pid
        || record.process_group_id != 0 && record.process_group_id != pid
    {
        return Ok(None);
    }
    Ok(Some((pid, pid)))
}

/// Bind every child in this execution to the Attempt's durable stop boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttemptExecutionOwner {
    pub project_root: PathBuf,
    pub work_item_id: String,
    pub attempt_id: String,
}

fn lock_path(path: &Path) -> PathBuf {
    path.with_extension("lock")
}

fn mutate_record<T>(
    path: &Path,
    mutate: impl FnOnce(&mut ExecutionRecord) -> Result<T>,
) -> Result<T> {
    let _lock = crate::lease::acquire_blocking(&lock_path(path))
        .with_context(|| format!("locking local execution record {}", path.display()))?;
    let bytes = std::fs::read(path)
        .with_context(|| format!("reading local execution record {}", path.display()))?;
    let mut record: ExecutionRecord = serde_json::from_slice(&bytes)
        .with_context(|| format!("parsing local execution record {}", path.display()))?;
    check_protocol(&record)?;
    let result = mutate(&mut record)?;
    crate::atomic_write::atomic_write(path, &serde_json::to_vec_pretty(&record)?)
        .with_context(|| format!("writing local execution record {}", path.display()))?;
    Ok(result)
}

fn identity_lock_path(path: &Path) -> PathBuf {
    path.with_file_name("execution-owner.lock")
}

fn identity_is_held(path: &Path) -> Result<bool> {
    let file = match File::open(identity_lock_path(path)) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error).context("opening execution identity lock"),
    };
    match rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive) {
        Ok(()) => {
            let _ = rustix::fs::flock(&file, rustix::fs::FlockOperation::Unlock);
            Ok(false)
        }
        Err(error) if std::io::Error::from(error).kind() == std::io::ErrorKind::WouldBlock => {
            Ok(true)
        }
        Err(error) => Err(std::io::Error::from(error)).context("checking execution identity lock"),
    }
}

#[cfg(unix)]
fn process_group_exists(process_group_id: u32) -> Result<bool> {
    let group = i32::try_from(process_group_id).context("process group id is out of range")?;
    let result = unsafe { libc::kill(-group, 0) };
    if result == 0 {
        return Ok(true);
    }
    let error = std::io::Error::last_os_error();
    match error.raw_os_error() {
        Some(libc::ESRCH) => Ok(false),
        Some(libc::EPERM) => Ok(true),
        _ => Err(error).context("checking local coder process group"),
    }
}

#[cfg(unix)]
fn leader_matches_process_group(leader_pid: u32, process_group_id: u32) -> Result<bool> {
    let leader = i32::try_from(leader_pid).context("leader pid is out of range")?;
    let expected_group =
        i32::try_from(process_group_id).context("process group id is out of range")?;
    let actual_group = unsafe { libc::getpgid(leader) };
    if actual_group >= 0 {
        return Ok(actual_group == expected_group);
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        // A group leader may exit while its descendants remain in the group.
        // The inherited identity lock plus the still-live group remains the
        // durable binding in that case.
        return Ok(true);
    }
    Err(error).context("checking local coder group leader")
}

#[cfg(not(unix))]
fn process_group_exists(_process_group_id: u32) -> Result<bool> {
    bail!("local execution cancellation requires Unix process groups")
}

#[cfg(not(unix))]
fn leader_matches_process_group(_leader_pid: u32, _process_group_id: u32) -> Result<bool> {
    bail!("local execution cancellation requires Unix process groups")
}

fn recorded_identity_is_signalable(path: &Path, record: &ExecutionRecord) -> Result<bool> {
    let Some((leader, group)) = effective_identity(path, record)? else {
        return Ok(false);
    };
    Ok(identity_is_held(path)?
        && process_group_exists(group)?
        && leader_matches_process_group(leader, group)?)
}

#[cfg(unix)]
fn signal_process_group(process_group_id: u32, signal: i32) -> Result<()> {
    let group = i32::try_from(process_group_id).context("process group id is out of range")?;
    let result = unsafe { libc::kill(-group, signal) };
    if result == 0 {
        return Ok(());
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        return Ok(());
    }
    Err(error).context("signaling local coder process group")
}

#[cfg(not(unix))]
fn signal_process_group(_process_group_id: u32, _signal: i32) -> Result<()> {
    bail!("local execution cancellation requires Unix process groups")
}

fn wait_for_identity_release(path: &Path, timeout: Duration) -> Result<bool> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if !identity_is_held(path)? {
            return Ok(true);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(!identity_is_held(path)?)
}

pub fn record_path_for_transcript(transcript: &Path) -> PathBuf {
    transcript.with_file_name(EXECUTION_FILE)
}

pub fn record_path_for_artifact(project_root: &Path, artifact_area: &str) -> PathBuf {
    project_root.join(artifact_area).join(EXECUTION_FILE)
}

pub struct ExecutionHeartbeat {
    path: PathBuf,
    last_write: Instant,
}

/// Configure a lock that the child opens before exec and its descendants inherit.
/// Contention then proves that the original process tree still owns the recorded
/// identity even if the Fluent owner has exited.
#[cfg(unix)]
pub fn configure_identity_lock(command: &mut Command, transcript: &Path) -> Result<()> {
    configure_launch_identity(command, transcript, None)
}

#[cfg(unix)]
fn configure_launch_identity(
    command: &mut Command,
    transcript: &Path,
    generation: Option<&str>,
) -> Result<()> {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::process::CommandExt;

    let record_path = record_path_for_transcript(transcript);
    if let Some(parent) = record_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let path = std::ffi::CString::new(identity_lock_path(&record_path).as_os_str().as_bytes())
        .context("execution identity path contains a NUL byte")?;
    let receipt = std::ffi::CString::new(receipt_path(&record_path).as_os_str().as_bytes())
        .context("execution receipt path contains a NUL byte")?;
    let mut receipt_bytes = [0u8; 46];
    if let Some(generation) = generation {
        anyhow::ensure!(
            generation.len() == 32 && generation.bytes().all(|b| b.is_ascii_hexdigit()),
            "invalid launch generation"
        );
        receipt_bytes[..2].copy_from_slice(b"2 ");
        receipt_bytes[2..34].copy_from_slice(generation.as_bytes());
        receipt_bytes[34] = b' ';
        receipt_bytes[45] = b'\n';
    }
    let publish = generation.is_some();
    #[cfg(feature = "test-support")]
    let child_barrier = std::env::var_os("FLUENT_TEST_LAUNCH_CHILD_BARRIER")
        .filter(|_| {
            std::env::var_os("FLUENT_TEST_LAUNCH_BARRIER_PROGRAM").is_none_or(|program| {
                Path::new(command.get_program()).file_name() == Some(program.as_os_str())
            })
        })
        .map(|path| std::ffi::CString::new(Path::new(&path).as_os_str().as_bytes()))
        .transpose()
        .context("test launch barrier contains a NUL byte")?;
    #[cfg(feature = "test-support")]
    let receipt_ready = child_barrier.as_ref().map(|barrier| {
        let mut bytes = barrier.as_bytes().to_vec();
        bytes.extend_from_slice(b".ready");
        std::ffi::CString::new(bytes).expect("barrier was checked for NUL bytes")
    });
    #[cfg(feature = "test-support")]
    let start_marker = std::env::var_os("FLUENT_TEST_LAUNCH_START_MARKER")
        .map(|path| std::ffi::CString::new(Path::new(&path).as_os_str().as_bytes()))
        .transpose()
        .context("test launch start marker contains a NUL byte")?;
    unsafe {
        command.pre_exec(move || {
            let descriptor = libc::open(path.as_ptr(), libc::O_CREAT | libc::O_RDWR, 0o600);
            if descriptor < 0 {
                return Err(std::io::Error::last_os_error());
            }
            // Never let Command::spawn hang inside pre_exec behind a stale
            // descendant from an earlier launch in this artifact area.
            if libc::flock(descriptor, libc::LOCK_EX | libc::LOCK_NB) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            if publish {
                let pid = libc::getpid();
                if pid <= 0 || libc::getpgrp() != pid {
                    return Err(std::io::Error::from_raw_os_error(libc::EINVAL));
                }
                let mut digits = pid as u32;
                for index in (35..45).rev() {
                    receipt_bytes[index] = b'0' + (digits % 10) as u8;
                    digits /= 10;
                }
                let fd = libc::open(
                    receipt.as_ptr(),
                    libc::O_CREAT | libc::O_TRUNC | libc::O_WRONLY,
                    0o600,
                );
                if fd < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                let mut written = 0;
                while written < receipt_bytes.len() {
                    let count = libc::write(
                        fd,
                        receipt_bytes[written..].as_ptr().cast(),
                        receipt_bytes.len() - written,
                    );
                    if count <= 0 {
                        let error = std::io::Error::last_os_error();
                        libc::close(fd);
                        return Err(error);
                    }
                    written += count as usize;
                }
                if libc::close(fd) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            #[cfg(feature = "test-support")]
            if let (Some(barrier), Some(ready)) = (&child_barrier, &receipt_ready) {
                let fd = libc::open(ready.as_ptr(), libc::O_CREAT | libc::O_WRONLY, 0o600);
                if fd < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::close(fd) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                while libc::access(barrier.as_ptr(), libc::F_OK) != 0 {
                    libc::usleep(10_000);
                }
            }
            let flags = libc::fcntl(descriptor, libc::F_GETFD);
            if flags < 0 || libc::fcntl(descriptor, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            #[cfg(feature = "test-support")]
            if let Some(marker) = &start_marker {
                let fd = libc::open(marker.as_ptr(), libc::O_CREAT | libc::O_WRONLY, 0o600);
                if fd < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::close(fd) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
    Ok(())
}

#[cfg(not(unix))]
pub fn configure_identity_lock(_command: &mut Command, _transcript: &Path) -> Result<()> {
    bail!("local execution identity requires Unix process groups")
}

/// Publish a cancellable owner before the Task becomes visibly executing.
/// The caller holds its Work Item mutation boundary through reservation.
pub(crate) fn prepare_launch(
    transcript: &Path,
    attempt_owner: Option<AttemptExecutionOwner>,
) -> Result<LaunchReservation> {
    let path = record_path_for_transcript(transcript);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let _lock = crate::lease::acquire_blocking(&lock_path(&path))?;
    anyhow::ensure!(
        !identity_is_held(&path)?,
        "prior local execution still owns its identity"
    );
    if path.exists() {
        let old: ExecutionRecord = serde_json::from_slice(&std::fs::read(&path)?)?;
        check_protocol(&old)?;
    }
    let now = crate::work_model::now_iso8601();
    let record = ExecutionRecord {
        schema_version: LAUNCH_PROTOCOL_VERSION,
        state: ExecutionState::Launching,
        owner_pid: std::process::id(),
        leader_pid: 0,
        process_group_id: 0,
        started_at: now.clone(),
        heartbeat_at: now,
        settled_at: None,
        attempt_owner,
        launch_generation: Some(new_generation()?),
    };
    crate::atomic_write::atomic_write(&path, &serde_json::to_vec_pretty(&record)?)?;
    Ok(LaunchReservation { path, record })
}

/// Retain the exact unlaunched identity published by this caller. Spawning a
/// child replaces its generation; that child's supervisor owns its settlement.
pub(crate) struct LaunchReservation {
    path: PathBuf,
    record: ExecutionRecord,
}

impl LaunchReservation {
    pub(crate) fn settle_unused(&self) -> Result<()> {
        let _lock = crate::lease::acquire_blocking(&lock_path(&self.path))?;
        let mut current: ExecutionRecord = serde_json::from_slice(&std::fs::read(&self.path)?)?;
        check_protocol(&current)?;
        // Never rewrite another owner/generation, a launched child's evidence,
        // or a concurrent cancellation. Even a terminal no-op retains its bytes.
        if current != self.record {
            return Ok(());
        }
        anyhow::ensure!(
            !identity_is_held(&self.path)?
                && receipt_absent_or_prior_generation(
                    &self.path,
                    current.launch_generation.as_deref().unwrap_or_default(),
                )?,
            "reserved execution acquired a child identity; refusing unused settlement: {}",
            self.path.display()
        );
        current.state = ExecutionState::Settled;
        current.settled_at = Some(crate::work_model::now_iso8601());
        crate::atomic_write::atomic_write(&self.path, &serde_json::to_vec_pretty(&current)?)?;
        Ok(())
    }
}

/// Reconcile only a snapshot proven to be an unused post-stop reservation.
/// The caller holds the Work model and every setup-owner lease, and has
/// revalidated the same unresumed stop. This never signals or rewrites a child.
pub(crate) fn reconcile_unused_after_stop(
    path: &Path,
    expected: &ExecutionRecord,
    owner: &AttemptExecutionOwner,
    stop: &crate::work_model::AttemptCancellation,
) -> Result<()> {
    let _lock = crate::lease::acquire_blocking(&lock_path(path))?;
    let mut current: ExecutionRecord = serde_json::from_slice(&std::fs::read(path)?)?;
    check_protocol(&current)?;
    anyhow::ensure!(
        current == *expected,
        "execution generation or evidence changed during cancellation: {}",
        path.display()
    );
    anyhow::ensure!(
        stop.resumed_at.is_none(),
        "cancellation was resumed before settlement"
    );
    anyhow::ensure!(
        !identity_is_held(path)?,
        "execution identity is still held; refusing unused settlement: {}",
        path.display()
    );
    anyhow::ensure!(
        current
            .attempt_owner
            .as_ref()
            .is_none_or(|actual| actual == owner),
        "execution belongs to another Attempt; refusing settlement: {}",
        path.display()
    );
    // A retained terminal resource can name pane descendants outside the server
    // group. Leave it for verified terminal cleanup, even with zero recorded PIDs.
    let task = path
        .parent()
        .and_then(Path::file_name)
        .context("execution has no Task directory")?;
    let resource = owner
        .project_root
        .join(".fluent/work/terminal-sessions")
        .join(&owner.work_item_id)
        .join(&owner.attempt_id)
        .join(task)
        .join("resource.json");
    match std::fs::symlink_metadata(&resource) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
        Ok(_) => bail!(
            "unused reservation retains terminal resources; verify cleanup separately: {}",
            resource.display()
        ),
    }
    if matches!(
        current.state,
        ExecutionState::Canceled | ExecutionState::Settled
    ) && current.settled_at.is_some()
    {
        return Ok(()); // Preserve terminal evidence, including legacy records.
    }
    anyhow::ensure!(
        current.schema_version == LAUNCH_PROTOCOL_VERSION
            && current.state == ExecutionState::Launching
            && current.attempt_owner.as_ref() == Some(owner)
            && current.owner_pid > 0
            && current.leader_pid == 0
            && current.process_group_id == 0
            && current.settled_at.is_none()
            && current.heartbeat_at == current.started_at,
        "execution is not a proven owned unused reservation: {}",
        path.display()
    );
    let started = chrono::DateTime::parse_from_rfc3339(&current.started_at)?;
    let requested = chrono::DateTime::parse_from_rfc3339(&stop.requested_at)?;
    let settled = stop
        .settled_at
        .as_deref()
        .map(chrono::DateTime::parse_from_rfc3339)
        .transpose()?;
    anyhow::ensure!(
        started >= requested
            && started <= chrono::Utc::now()
            && settled.is_none_or(|settled| started <= settled),
        "unused reservation is outside the original cancellation interval: {}",
        path.display()
    );
    anyhow::ensure!(
        receipt_absent_or_prior_generation(
            path,
            current.launch_generation.as_deref().unwrap_or_default()
        )?,
        "unused reservation has current or ambiguous child handoff evidence: {}",
        path.display()
    );
    current.state = ExecutionState::Canceled;
    current.settled_at = Some(crate::work_model::now_iso8601());
    crate::atomic_write::atomic_write(path, &serde_json::to_vec_pretty(&current)?)?;
    Ok(())
}

thread_local! {
    static TASK_OWNER: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
    static HANDOFF_CORRECTION: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Mark only the Writer's in-process handoff correction as a permitted second launch.
pub(crate) struct HandoffCorrection(bool);
impl HandoffCorrection {
    pub(crate) fn enter(enabled: bool) -> Self {
        Self(HANDOFF_CORRECTION.with(|flag| flag.replace(enabled)))
    }
}
impl Drop for HandoffCorrection {
    fn drop(&mut self) {
        HANDOFF_CORRECTION.with(|flag| flag.set(self.0));
    }
}

/// Attribute nested deterministic commands, including baseline checks, to their Task.
pub(crate) struct TaskOwner(Option<PathBuf>);
impl TaskOwner {
    pub(crate) fn enter(transcript: PathBuf) -> Self {
        Self(TASK_OWNER.with(|owner| owner.replace(Some(transcript))))
    }
}
impl Drop for TaskOwner {
    fn drop(&mut self) {
        TASK_OWNER.with(|owner| owner.replace(self.0.take()));
    }
}
fn owned_transcript(transcript: &Path) -> PathBuf {
    TASK_OWNER
        .with(|owner| owner.borrow().clone())
        .unwrap_or_else(|| transcript.to_path_buf())
}

/// Serialize the stop check, child creation, and child identity publication.
/// Cancellation observes either a stopped launch or the fully registered child.
pub fn spawn(command: &mut Command, transcript: &Path) -> Result<std::process::Child> {
    use std::os::unix::process::CommandExt;
    let owned = owned_transcript(transcript);
    let transcript = owned.as_path();
    command.process_group(0);
    let path = record_path_for_transcript(transcript);
    let attempt_owner = match std::fs::read(&path) {
        Ok(bytes) => {
            let record: ExecutionRecord = serde_json::from_slice(&bytes)?;
            check_protocol(&record)?;
            record.attempt_owner
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    let store = attempt_owner
        .as_ref()
        .map(|owner| crate::work_model::WorkModelStore::new(&owner.project_root));
    // Follow reservation's lock order: Work model, then execution. Hold both
    // through spawn and identity publication so cancellation cannot commit its
    // shared stop between the check and this child becoming registered.
    let model_lock = match (&store, &attempt_owner) {
        (Some(store), Some(owner)) => {
            let lock = store.lock_work_item_model(&owner.work_item_id)?;
            let item = store.read_work_item_under_model_lock(&owner.work_item_id)?;
            let attempt = item
                .attempts
                .iter()
                .find(|attempt| attempt.id == owner.attempt_id)
                .ok_or_else(|| anyhow::anyhow!("execution's owning Attempt is missing"))?;
            attempt.ensure_not_canceled()?;
            crate::writer_completion_recovery::before_context_retry_spawn(
                &owner.project_root,
                &item,
                &owner.attempt_id,
                transcript,
                HANDOFF_CORRECTION.with(|flag| flag.get()),
            )?;
            crate::usage_limit_recovery::before_spawn(
                &owner.project_root,
                &item,
                &owner.attempt_id,
                transcript,
            )?;
            Some(lock)
        }
        _ => None,
    };
    let _lock = crate::lease::acquire_blocking(&lock_path(&path))?;
    let generation = new_generation()?;
    let reserved_record = match std::fs::read(&path) {
        Ok(bytes) => {
            let mut record: ExecutionRecord = serde_json::from_slice(&bytes)?;
            check_protocol(&record)?;
            anyhow::ensure!(
                record.attempt_owner == attempt_owner,
                "execution ownership changed before launch"
            );
            anyhow::ensure!(
                record.state != ExecutionState::Canceled,
                "local execution was canceled before launch"
            );
            anyhow::ensure!(
                !identity_is_held(&path)?,
                "prior local execution still owns its identity"
            );
            record.schema_version = LAUNCH_PROTOCOL_VERSION;
            record.state = ExecutionState::Launching;
            record.leader_pid = 0;
            record.process_group_id = 0;
            record.launch_generation = Some(generation.clone());
            record.settled_at = None;
            crate::atomic_write::atomic_write(&path, &serde_json::to_vec_pretty(&record)?)?;
            record
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            anyhow::ensure!(
                attempt_owner.is_none(),
                "owned execution record disappeared before launch"
            );
            let now = crate::work_model::now_iso8601();
            let record = ExecutionRecord {
                schema_version: LAUNCH_PROTOCOL_VERSION,
                state: ExecutionState::Launching,
                owner_pid: std::process::id(),
                leader_pid: 0,
                process_group_id: 0,
                started_at: now.clone(),
                heartbeat_at: now,
                settled_at: None,
                attempt_owner: None,
                launch_generation: Some(generation.clone()),
            };
            crate::atomic_write::atomic_write(&path, &serde_json::to_vec_pretty(&record)?)?;
            record
        }
        Err(error) => return Err(error.into()),
    };
    let reservation = LaunchReservation {
        path: path.clone(),
        record: reserved_record,
    };
    let launch = configure_launch_identity(command, transcript, Some(&generation))
        .and_then(|()| command.spawn().map_err(Into::into));
    let child = match launch {
        Ok(child) => child,
        Err(error) => {
            // Spawn has replaced the caller's reservation generation. Retain
            // this exact token too, and release both locks before settlement.
            // A child-owned receipt makes unused settlement fail closed.
            drop(_lock);
            drop(model_lock);
            return match reservation.settle_unused() {
                Ok(()) => Err(error),
                Err(settlement) => Err(error.context(format!(
                    "failed launch has unproven settlement: {settlement:#}"
                ))),
            };
        }
    };
    #[cfg(feature = "test-support")]
    if let Ok(barrier) = std::env::var("FLUENT_TEST_LAUNCH_PARENT_BARRIER") {
        std::fs::write(&barrier, b"spawned")?;
        std::process::abort();
    }
    let publication = write_running_record(&path, child.id());
    // Worker execution and failed-launch settlement must not hold the model lock.
    drop(_lock);
    drop(model_lock);
    if let Err(error) = publication {
        return match settle_failed_launch(child, transcript) {
            Ok(()) => Err(error),
            Err(cleanup) => Err(error.context(format!("launch cleanup failed: {cleanup:#}"))),
        };
    }
    Ok(child)
}

/// Sweep and reap a spawned child when setup fails before normal supervision.
pub(crate) fn settle_failed_launch(child: std::process::Child, transcript: &Path) -> Result<()> {
    let (_, _, disposition) =
        crate::coder::wait_owned_process_tree(child, Duration::ZERO).map_err(anyhow::Error::msg)?;
    anyhow::ensure!(
        disposition.is_verified_swept(),
        "failed launch process cleanup was not confirmed: {disposition:?}"
    );
    anyhow::ensure!(
        wait_for_identity_release(
            &record_path_for_transcript(&owned_transcript(transcript)),
            TERMINATION_GRACE,
        )?,
        "failed launch still owns its execution identity"
    );
    Ok(())
}

/// Capture a deterministic Task command under the same cancellation identity.
/// Keep ordinary Tester leak detection authoritative after a natural exit.
pub(crate) fn output(command: &mut Command, transcript: &Path) -> Result<std::process::Output> {
    command
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let child = spawn(command, transcript)?;
    let result = child.wait_with_output();
    let path = record_path_for_transcript(&owned_transcript(transcript));
    if result.is_err() {
        cancel(&path)?;
    }
    if !identity_is_held(&path)? {
        mutate_record(&path, |record| {
            if record.state == ExecutionState::Running {
                record.state = ExecutionState::Settled;
                record.settled_at = Some(crate::work_model::now_iso8601());
            }
            Ok(())
        })?;
    }
    result.context("capture local Task command")
}

fn write_running_record(path: &Path, leader_pid: u32) -> Result<()> {
    let mut record: ExecutionRecord = match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => ExecutionRecord {
            schema_version: 1,
            state: ExecutionState::Launching,
            owner_pid: std::process::id(),
            leader_pid: 0,
            process_group_id: 0,
            started_at: String::new(),
            heartbeat_at: String::new(),
            settled_at: None,
            attempt_owner: None,
            launch_generation: None,
        },
        Err(error) => return Err(error.into()),
    };
    check_protocol(&record)?;
    anyhow::ensure!(
        record.schema_version == 1
            || effective_identity(path, &record)? == Some((leader_pid, leader_pid)),
        "child launch receipt does not match the spawned process"
    );
    let now = crate::work_model::now_iso8601();
    record.state = ExecutionState::Running;
    record.owner_pid = std::process::id();
    record.leader_pid = leader_pid;
    record.process_group_id = leader_pid;
    record.started_at = now.clone();
    record.heartbeat_at = now;
    record.settled_at = None;
    crate::atomic_write::atomic_write(path, &serde_json::to_vec_pretty(&record)?)?;
    Ok(())
}

impl ExecutionHeartbeat {
    pub fn start(transcript: &Path, leader_pid: u32) -> Result<Self> {
        let path = record_path_for_transcript(transcript);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let _lock = crate::lease::acquire_blocking(&lock_path(&path))?;
        if path.exists() {
            let record: ExecutionRecord = serde_json::from_slice(&std::fs::read(&path)?)?;
            check_protocol(&record)?;
            anyhow::ensure!(
                record.state != ExecutionState::Canceled,
                "local execution was canceled after launch"
            );
        }
        write_running_record(&path, leader_pid)?;
        Ok(Self {
            path,
            last_write: Instant::now(),
        })
    }

    pub fn heartbeat(&mut self) -> Result<()> {
        if self.last_write.elapsed() < HEARTBEAT_INTERVAL {
            return Ok(());
        }
        mutate_record(&self.path, |record| {
            if record.state == ExecutionState::Running {
                record.heartbeat_at = crate::work_model::now_iso8601();
            }
            Ok(())
        })?;
        self.last_write = Instant::now();
        Ok(())
    }

    pub fn settle(&mut self) -> Result<()> {
        mutate_record(&self.path, |record| {
            if record.state == ExecutionState::Running {
                record.state = ExecutionState::Settled;
                record.settled_at = Some(crate::work_model::now_iso8601());
            }
            Ok(())
        })
    }
}

pub fn execution_is_live(path: &Path) -> Result<bool> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error).context("reading local execution record"),
    };
    let record: ExecutionRecord = serde_json::from_slice(&bytes)
        .with_context(|| format!("parsing local execution record {}", path.display()))?;
    check_protocol(&record)?;
    // The inherited lock proves that part of the original launch still exists,
    // even when a parent settled the record after its leader exited. A damaged
    // group field must keep recovery blocked rather than make the Task reclaimable.
    identity_is_held(path)
}

/// Confirm the generation and child-owned receipt before another client uses an
/// execution as attachment authority.
pub fn verified_running_generation(path: &Path, generation: &str) -> Result<bool> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    let record: ExecutionRecord = serde_json::from_slice(&bytes)?;
    check_protocol(&record)?;
    Ok(record.state == ExecutionState::Running
        && record.launch_generation.as_deref() == Some(generation)
        && recorded_identity_is_signalable(path, &record)?)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelOutcome {
    Canceled,
    AlreadyStopped,
}

fn stop_owned_process_group(
    path: &Path,
    record: &ExecutionRecord,
    terminal_closed: bool,
) -> Result<()> {
    let (_, group) = effective_identity(path, record)?
        .ok_or_else(|| anyhow::anyhow!("local execution has no process group"))?;
    // Closing an owned tmux server normally releases the identity itself.
    // Wait for that exit before signaling its former process group: on macOS,
    // signaling the departed server group can fail with EPERM.
    if terminal_closed && wait_for_identity_release(path, TERMINATION_GRACE)? {
        return Ok(());
    }
    signal_process_group(group, libc::SIGTERM)?;
    if !wait_for_identity_release(path, TERMINATION_GRACE)? {
        signal_process_group(group, libc::SIGKILL)?;
        anyhow::ensure!(
            wait_for_identity_release(path, TERMINATION_GRACE)?,
            "local execution process group {group} did not stop"
        );
    }
    Ok(())
}

/// Settle a completed native terminal while preserving the Task owner's
/// ability to launch its later validation commands. A concurrent Attempt stop
/// wins through the same execution lock and cannot be relabeled successful.
pub(crate) fn settle_managed_terminal(path: &Path) -> Result<()> {
    let _lock = crate::lease::acquire_blocking(&lock_path(path))
        .with_context(|| format!("locking local execution record {}", path.display()))?;
    let mut record: ExecutionRecord = serde_json::from_slice(&std::fs::read(path)?)?;
    check_protocol(&record)?;
    anyhow::ensure!(
        record.state == ExecutionState::Running,
        "native terminal is not running"
    );
    anyhow::ensure!(
        recorded_identity_is_signalable(path, &record)?,
        "native terminal lost its process identity before settlement"
    );
    let terminal_closed = crate::terminal_sessions::settle_for_execution(path, &record)?;
    stop_owned_process_group(path, &record, terminal_closed)?;
    record.state = ExecutionState::Settled;
    record.settled_at = Some(crate::work_model::now_iso8601());
    crate::atomic_write::atomic_write(path, &serde_json::to_vec_pretty(&record)?)?;
    crate::terminal_sessions::retire_for_execution(path, &record)?;
    Ok(())
}

pub fn cancel(path: &Path) -> Result<CancelOutcome> {
    cancel_execution(path, false)
}

/// Stop owned resources without rewriting a completed Task's settled history.
pub(crate) fn cancel_completed_task(path: &Path) -> Result<CancelOutcome> {
    cancel_execution(path, true)
}

fn cancel_execution(path: &Path, completed_task: bool) -> Result<CancelOutcome> {
    let _lock = crate::lease::acquire_blocking(&lock_path(path))
        .with_context(|| format!("locking local execution record {}", path.display()))?;
    let bytes = std::fs::read(path)
        .with_context(|| format!("reading local execution record {}", path.display()))?;
    let mut record: ExecutionRecord = serde_json::from_slice(&bytes)
        .with_context(|| format!("parsing local execution record {}", path.display()))?;
    check_protocol(&record)?;
    if record.state == ExecutionState::Canceled {
        crate::terminal_sessions::retire_for_execution(path, &record)?;
        return Ok(CancelOutcome::AlreadyStopped);
    }

    // Only the Work model can establish that no later command belongs to
    // this Task. A settled command in unfinished work still needs a stop.
    let preserve_history =
        completed_task && record.state == ExecutionState::Settled && record.settled_at.is_some();

    // A settled child may be followed by another command in the same Task.
    // Preserve the stop so that command cannot launch without a new reservation.
    if !identity_is_held(path)? {
        crate::terminal_sessions::settle_for_execution(path, &record)?;
        if !preserve_history {
            record.state = ExecutionState::Canceled;
            record.settled_at = Some(crate::work_model::now_iso8601());
            crate::atomic_write::atomic_write(path, &serde_json::to_vec_pretty(&record)?)?;
        }
        crate::terminal_sessions::retire_for_execution(path, &record)?;
        return Ok(CancelOutcome::AlreadyStopped);
    }
    if !recorded_identity_is_signalable(path, &record)? {
        bail!(
            "local execution identity is still held, but leader {} and process group {} do not match; refusing to signal or recover the Task",
            record.leader_pid,
            record.process_group_id
        );
    }

    let terminal_closed = crate::terminal_sessions::settle_for_execution(path, &record)?;
    stop_owned_process_group(path, &record, terminal_closed)?;
    if !preserve_history {
        record.state = ExecutionState::Canceled;
        record.settled_at = Some(crate::work_model::now_iso8601());
        crate::atomic_write::atomic_write(path, &serde_json::to_vec_pretty(&record)?)?;
    }
    crate::terminal_sessions::retire_for_execution(path, &record)?;
    Ok(CancelOutcome::Canceled)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::CommandExt;

    #[test]
    fn rejected_spawn_settles_its_unmaterialized_generation() {
        let dir = tempfile::tempdir().unwrap();
        let transcript = dir.path().join("transcript.jsonl");
        output(&mut Command::new("true"), &transcript).unwrap();
        let path = record_path_for_transcript(&transcript);
        let prior_receipt = std::fs::read(receipt_path(&path)).unwrap();
        prepare_launch(&transcript, None).unwrap();
        let mut invalid = Command::new("true");
        invalid.arg("invalid\0argument");
        assert!(spawn(&mut invalid, &transcript).is_err());
        let record: ExecutionRecord =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(record.state, ExecutionState::Settled);
        assert!(record.settled_at.is_some());
        assert_eq!((record.leader_pid, record.process_group_id), (0, 0));
        assert_eq!(std::fs::read(receipt_path(&path)).unwrap(), prior_receipt);
    }

    #[test]
    fn stopped_reservation_reconciliation_refuses_changed_snapshot_and_stop() {
        for change in ["generation", "owner", "resumed"] {
            let dir = tempfile::tempdir().unwrap();
            let owner = AttemptExecutionOwner {
                project_root: dir.path().into(),
                work_item_id: "work-1".into(),
                attempt_id: "attempt-1".into(),
            };
            let transcript = dir.path().join("transcript.jsonl");
            let reservation = prepare_launch(&transcript, Some(owner.clone())).unwrap();
            let mut stop = crate::work_model::AttemptCancellation {
                requested_at: reservation.record.started_at.clone(),
                settled_at: None,
                resumed_at: None,
            };
            match change {
                "generation" => {
                    prepare_launch(&transcript, Some(owner.clone())).unwrap();
                }
                "owner" => {
                    mutate_record(&reservation.path, |r| {
                        r.owner_pid += 1;
                        Ok(())
                    })
                    .unwrap();
                }
                "resumed" => stop.resumed_at = Some(crate::work_model::now_iso8601()),
                _ => unreachable!(),
            }
            let before = std::fs::read(&reservation.path).unwrap();
            assert!(
                reconcile_unused_after_stop(&reservation.path, &reservation.record, &owner, &stop)
                    .is_err(),
                "{change}"
            );
            assert_eq!(
                std::fs::read(&reservation.path).unwrap(),
                before,
                "{change}"
            );
        }
    }

    #[test]
    fn unused_reservation_settlement_retains_generation_and_prior_receipt() {
        let dir = tempfile::tempdir().unwrap();
        let transcript = dir.path().join("transcript.jsonl");
        assert!(
            output(&mut Command::new("true"), &transcript)
                .unwrap()
                .status
                .success()
        );
        let path = record_path_for_transcript(&transcript);
        let prior_receipt = std::fs::read(receipt_path(&path)).unwrap();
        let reservation = prepare_launch(&transcript, None).unwrap();
        reservation.settle_unused().unwrap();
        let settled: ExecutionRecord =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(settled.state, ExecutionState::Settled);
        assert!(settled.settled_at.is_some());
        assert_eq!(
            settled.launch_generation,
            reservation.record.launch_generation
        );
        assert_eq!(settled.owner_pid, reservation.record.owner_pid);
        assert_eq!(settled.leader_pid, 0);
        assert_eq!(settled.process_group_id, 0);
        assert_eq!(std::fs::read(receipt_path(&path)).unwrap(), prior_receipt);
        let bytes = std::fs::read(&path).unwrap();
        reservation.settle_unused().unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }

    #[test]
    fn unused_reservation_settlement_refuses_changed_or_active_identity() {
        for change in [
            "generation",
            "owner",
            "owner-pid",
            "cancel",
            "lock",
            "receipt",
            "malformed-receipt",
        ] {
            let dir = tempfile::tempdir().unwrap();
            let transcript = dir.path().join("transcript.jsonl");
            let reservation = prepare_launch(&transcript, None).unwrap();
            let path = &reservation.path;
            let mut held = None;
            match change {
                "generation" => {
                    prepare_launch(&transcript, None).unwrap();
                }
                "owner" => {
                    mutate_record(path, |record| {
                        record.attempt_owner = Some(AttemptExecutionOwner {
                            project_root: dir.path().into(),
                            work_item_id: "other".into(),
                            attempt_id: "attempt-2".into(),
                        });
                        Ok(())
                    })
                    .unwrap();
                }
                "owner-pid" => {
                    mutate_record(path, |record| {
                        record.owner_pid += 1;
                        Ok(())
                    })
                    .unwrap();
                }
                "cancel" => {
                    cancel(path).unwrap();
                }
                "lock" => {
                    held = Some(crate::lease::acquire_blocking(&identity_lock_path(path)).unwrap());
                }
                "receipt" => {
                    std::fs::write(
                        receipt_path(path),
                        format!(
                            "2 {} {:010}\n",
                            reservation.record.launch_generation.as_deref().unwrap(),
                            std::process::id()
                        ),
                    )
                    .unwrap();
                }
                "malformed-receipt" => {
                    std::fs::write(receipt_path(path), "unknown child").unwrap();
                }
                _ => unreachable!(),
            }
            let bytes = std::fs::read(path).unwrap();
            let result = reservation.settle_unused();
            assert_eq!(
                result.is_err(),
                matches!(change, "lock" | "receipt" | "malformed-receipt"),
                "{change}"
            );
            assert_eq!(std::fs::read(path).unwrap(), bytes, "{change}");
            drop(held);
        }
    }

    #[test]
    fn unused_reservation_settlement_preserves_launched_child_evidence() {
        let dir = tempfile::tempdir().unwrap();
        let transcript = dir.path().join("transcript.jsonl");
        let reservation = prepare_launch(&transcript, None).unwrap();
        let mut command = Command::new("/bin/cat");
        command
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null());
        let mut child = spawn(&mut command, &transcript).unwrap();
        let mut heartbeat = ExecutionHeartbeat::start(&transcript, child.id()).unwrap();
        let running = std::fs::read(&reservation.path).unwrap();
        reservation.settle_unused().unwrap();
        assert_eq!(std::fs::read(&reservation.path).unwrap(), running);
        assert!(execution_is_live(&reservation.path).unwrap());
        drop(child.stdin.take());
        assert!(child.wait().unwrap().success());
        heartbeat.settle().unwrap();
        let settled = std::fs::read(&reservation.path).unwrap();
        reservation.settle_unused().unwrap();
        assert_eq!(std::fs::read(&reservation.path).unwrap(), settled);
    }

    #[test]
    fn attempt_owner_survives_commands_and_heartbeat() {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::work_model::WorkModelStore::new(dir.path());
        let mut item = crate::work_model::WorkItem::planned("work-1", "Preserve launch ownership");
        item.add_initial_attempt("attempt-1").unwrap();
        store.create_work_item(&item).unwrap();
        let transcript = dir.path().join("transcript.jsonl");
        let path = record_path_for_transcript(&transcript);
        let owner = AttemptExecutionOwner {
            project_root: dir.path().into(),
            work_item_id: "work-1".into(),
            attempt_id: "attempt-1".into(),
        };
        prepare_launch(&transcript, Some(owner.clone())).unwrap();
        let mut first = Command::new("true");
        // Observe the model lock from the forked child before exec. Merely
        // reading the stop and dropping its lock before spawn leaves a race.
        use std::os::unix::ffi::OsStrExt;
        let model_lock = std::ffi::CString::new(
            dir.path()
                .join(".fluent/work/locks/work-1/model.lock")
                .as_os_str()
                .as_bytes(),
        )
        .unwrap();
        unsafe {
            first.pre_exec(move || {
                let fd = libc::open(model_lock.as_ptr(), libc::O_RDONLY);
                if fd < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                let locked = libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB);
                let error = std::io::Error::last_os_error();
                libc::close(fd);
                if locked < 0 && error.kind() == std::io::ErrorKind::WouldBlock {
                    Ok(())
                } else {
                    Err(std::io::Error::from_raw_os_error(libc::EINVAL))
                }
            });
        }
        assert!(output(&mut first, &transcript).unwrap().status.success());
        let mut second = Command::new("/bin/cat");
        second
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null());
        let mut child = spawn(&mut second, &transcript).unwrap();
        let heartbeat = ExecutionHeartbeat::start(&transcript, child.id());
        child.kill().unwrap();
        child.wait().unwrap();
        heartbeat.unwrap().settle().unwrap();
        let record: ExecutionRecord =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(record.attempt_owner, Some(owner));
        assert_eq!(record.state, ExecutionState::Settled);
        // Persist only the shared intent, before cancellation visits this record.
        store
            .mutate_work_item("work-1", |item| {
                let attempt = &mut item.attempts[0];
                attempt.cancellation = Some(crate::work_model::AttemptCancellation {
                    requested_at: crate::work_model::now_iso8601(),
                    settled_at: None,
                    resumed_at: None,
                });
                attempt.status = crate::work_model::AttemptStatus::NeedsUser;
                attempt.pause_kind = Some(crate::work_model::PauseKind::Interrupted);
                Ok(())
            })
            .unwrap();
        let marker = dir.path().join("later-child");
        let mut later = Command::new("touch");
        later.arg(&marker);
        assert!(output(&mut later, &transcript).is_err());
        assert!(!marker.exists());
        assert_eq!(
            serde_json::from_slice::<ExecutionRecord>(&std::fs::read(&path).unwrap()).unwrap(),
            record
        );
    }

    #[test]
    fn owned_launch_rejects_a_missing_attempt() {
        let dir = tempfile::tempdir().unwrap();
        let transcript = dir.path().join("transcript.jsonl");
        prepare_launch(
            &transcript,
            Some(AttemptExecutionOwner {
                project_root: dir.path().into(),
                work_item_id: "missing".into(),
                attempt_id: "attempt-1".into(),
            }),
        )
        .unwrap();
        let marker = dir.path().join("launched");
        let mut command = Command::new("touch");
        command.arg(&marker);
        assert!(spawn(&mut command, &transcript).is_err());
        assert!(!marker.exists());
    }

    #[test]
    fn cancel_settled_execution_blocks_later_children_until_reservation() {
        let dir = tempfile::tempdir().unwrap();
        let transcript = dir.path().join("transcript.jsonl");
        let path = record_path_for_transcript(&transcript);
        prepare_launch(&transcript, None).unwrap();
        let mut first = Command::new("/bin/sh");
        first.args(["-c", "exit 0"]);
        assert!(output(&mut first, &transcript).unwrap().status.success());
        let settled: ExecutionRecord =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(settled.state, ExecutionState::Settled);

        assert_eq!(cancel(&path).unwrap(), CancelOutcome::AlreadyStopped);
        let marker = dir.path().join("next-started");
        let mut next = Command::new("touch");
        next.arg(&marker);
        let launched = output(&mut next, &transcript);
        assert!(
            launched.is_err(),
            "canceling a settled child must block the next launch"
        );
        assert!(!marker.exists());
        let stopped = std::fs::read(&path).unwrap();
        assert_eq!(
            serde_json::from_slice::<ExecutionRecord>(&stopped)
                .unwrap()
                .state,
            ExecutionState::Canceled
        );
        assert_eq!(cancel(&path).unwrap(), CancelOutcome::AlreadyStopped);
        assert_eq!(std::fs::read(&path).unwrap(), stopped);

        // Task reservation calls prepare_launch only after the Work model admits
        // explicit resume; completed commands within one reservation do not.
        prepare_launch(&transcript, None).unwrap();
        let mut resumed = Command::new("touch");
        resumed.arg(&marker);
        assert!(output(&mut resumed, &transcript).unwrap().status.success());
        assert!(marker.exists());
    }

    #[test]
    fn failed_launch_settles_owned_process_tree() {
        use std::io::BufRead;
        let dir = tempfile::tempdir().unwrap();
        let transcript = dir.path().join("stdout.log");
        prepare_launch(&transcript, None).unwrap();
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", "sleep 30 & echo ready; wait"])
            .stdout(std::process::Stdio::piped());
        let mut child = spawn(&mut command, &transcript).unwrap();
        let pid = child.id();
        let mut ready = String::new();
        std::io::BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut ready)
            .unwrap();
        assert_eq!(ready.trim(), "ready");
        assert!(identity_is_held(&record_path_for_transcript(&transcript)).unwrap());
        settle_failed_launch(child, &transcript).unwrap();
        assert!(!identity_is_held(&record_path_for_transcript(&transcript)).unwrap());
        assert_eq!(
            unsafe { libc::waitpid(pid as i32, std::ptr::null_mut(), libc::WNOHANG) },
            -1
        );
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ECHILD)
        );
    }

    #[test]
    fn liveness_probe_does_not_create_identity_lock() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("execution.json");
        let record = ExecutionRecord {
            schema_version: 1,
            state: ExecutionState::Running,
            owner_pid: std::process::id(),
            leader_pid: std::process::id(),
            process_group_id: std::process::id(),
            started_at: "started".into(),
            heartbeat_at: "heartbeat".into(),
            settled_at: None,
            attempt_owner: None,
            launch_generation: None,
        };
        let bytes = serde_json::to_vec_pretty(&record).unwrap();
        assert!(
            serde_json::from_slice::<serde_json::Value>(&bytes)
                .unwrap()
                .get("attempt_owner")
                .is_none()
        );
        assert_eq!(
            serde_json::from_slice::<ExecutionRecord>(&bytes)
                .unwrap()
                .attempt_owner,
            None
        );
        std::fs::write(&path, &bytes).unwrap();
        assert!(!execution_is_live(&path).unwrap());
        assert!(!identity_lock_path(&path).exists());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        let missing = dir.path().join("missing/execution.json");
        assert!(!identity_is_held(&missing).unwrap());
        assert!(!execution_is_live(&missing).unwrap());
        assert!(!missing.parent().unwrap().exists());
    }

    #[test]
    fn liveness_probe_observes_existing_held_identity_lock() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let transcript = dir.path().join("transcript.jsonl");
        let path = record_path_for_transcript(&transcript);
        let lock = identity_lock_path(&path);
        std::fs::write(&lock, b"preserve identity bytes").unwrap();
        let mut command = Command::new("/bin/cat");
        command
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .process_group(0);
        configure_identity_lock(&mut command, &transcript).unwrap();
        let mut child = command.spawn().unwrap();
        let record = ExecutionRecord {
            schema_version: 1,
            state: ExecutionState::Running,
            owner_pid: std::process::id(),
            leader_pid: child.id(),
            process_group_id: child.id(),
            started_at: "started".into(),
            heartbeat_at: "heartbeat".into(),
            settled_at: None,
            attempt_owner: None,
            launch_generation: None,
        };
        std::fs::write(&path, serde_json::to_vec_pretty(&record).unwrap()).unwrap();
        std::fs::set_permissions(&lock, std::fs::Permissions::from_mode(0o444)).unwrap();
        let live = execution_is_live(&path);
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(live.unwrap());
        assert_eq!(std::fs::read(&lock).unwrap(), b"preserve identity bytes");
        assert!(!execution_is_live(&path).unwrap());
    }

    #[test]
    fn cancel_terminates_owned_process_group() {
        let dir = tempfile::tempdir().unwrap();
        let transcript = dir.path().join("transcript.jsonl");
        let mut command = Command::new("/bin/sh");
        command.arg("-c").arg("sleep 30");
        command.process_group(0);
        let path = record_path_for_transcript(&transcript);
        configure_identity_lock(&mut command, &transcript).unwrap();
        let mut child = command.spawn().unwrap();
        let _heartbeat = ExecutionHeartbeat::start(&transcript, child.id()).unwrap();

        assert!(
            identity_is_held(&path).unwrap(),
            "child must inherit identity"
        );
        assert!(
            process_group_exists(child.id()).unwrap(),
            "child group must exist"
        );
        assert_eq!(cancel(&path).unwrap(), CancelOutcome::Canceled);
        assert!(!execution_is_live(&path).unwrap());
        let _ = child.wait();
    }

    #[test]
    fn stale_record_never_signals_reused_identity() {
        let dir = tempfile::tempdir().unwrap();
        let transcript = dir.path().join("transcript.jsonl");
        let mut command = Command::new("/bin/sh");
        command.arg("-c").arg("sleep 30");
        command.process_group(0);
        let path = record_path_for_transcript(&transcript);
        configure_identity_lock(&mut command, &transcript).unwrap();
        let mut child = command.spawn().unwrap();
        let heartbeat = ExecutionHeartbeat::start(&transcript, child.id()).unwrap();
        // Model a stale or corrupted record by pointing it at this test process's
        // group while the original child continues to hold the identity lock. The
        // two independent identity checks no longer agree, so cancellation must not
        // signal either process group.
        mutate_record(&path, |record| {
            record.process_group_id = std::process::id();
            Ok(())
        })
        .unwrap();

        assert!(execution_is_live(&path).unwrap());
        let error = cancel(&path).unwrap_err();
        assert!(error.to_string().contains("refusing to signal"));
        assert!(child.try_wait().unwrap().is_none());
        drop(heartbeat);
        unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL) };
        let _ = child.wait();
    }

    #[test]
    fn identity_lock_contention_fails_a_new_spawn_without_waiting() {
        let dir = tempfile::tempdir().unwrap();
        let transcript = dir.path().join("transcript.jsonl");
        let mut first = Command::new("/bin/sh");
        first.arg("-c").arg("sleep 30");
        first.process_group(0);
        configure_identity_lock(&mut first, &transcript).unwrap();
        let mut first = first.spawn().unwrap();

        let mut second = Command::new("/usr/bin/true");
        second.process_group(0);
        configure_identity_lock(&mut second, &transcript).unwrap();
        let started = Instant::now();
        assert!(second.spawn().is_err());
        assert!(started.elapsed() < Duration::from_secs(1));

        unsafe { libc::kill(-(first.id() as i32), libc::SIGKILL) };
        let _ = first.wait();
    }
}
