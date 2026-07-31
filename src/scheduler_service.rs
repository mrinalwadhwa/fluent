//! Persistent scheduler service foundation: checkout identity, user registry,
//! service-manager boundary, versioned socket health, immutable Attempt
//! execution requests, sequenced events, and a private executor contract.

use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, bail};
use rustix::fs::{FlockOperation, flock};
use serde::{Deserialize, Serialize};

use crate::atomic_write::atomic_write;

/// Integer protocol generation. Bump when the request/response schema changes
/// incompatibly.
pub const PROTOCOL_GENERATION: u32 = 1;

// ─────────────────────────────────────────────────
// Filesystem layout
// ─────────────────────────────────────────────────

/// Private user-state root: `~/.config/fluent/scheduler/`.
pub fn service_state_root(home_dir: &Path) -> PathBuf {
    home_dir.join(".config/fluent/scheduler")
}

fn registry_path(home_dir: &Path) -> PathBuf {
    service_state_root(home_dir).join("registry.json")
}

/// Unix socket path for the scheduler service.
pub fn socket_path(home_dir: &Path) -> PathBuf {
    service_state_root(home_dir).join("service.sock")
}

/// Path of the managed (staged) service executable.
pub fn managed_executable_path(home_dir: &Path) -> PathBuf {
    service_state_root(home_dir).join("service")
}

fn desired_build_path(home_dir: &Path) -> PathBuf {
    service_state_root(home_dir).join("build/desired.json")
}

fn observed_build_path(home_dir: &Path) -> PathBuf {
    service_state_root(home_dir).join("build/observed.json")
}

fn checkout_identity_path(project_root: &Path) -> PathBuf {
    project_root.join(".fluent/work/scheduler/identity")
}

fn requests_dir(project_root: &Path) -> PathBuf {
    project_root.join(".fluent/work/scheduler/requests")
}

fn events_dir(project_root: &Path) -> PathBuf {
    project_root.join(".fluent/work/scheduler/events")
}

fn dispatch_tokens_dir(project_root: &Path) -> PathBuf {
    project_root.join(".fluent/work/scheduler/dispatches")
}

fn registry_lock_path(home_dir: &Path) -> PathBuf {
    service_state_root(home_dir).join("registry.lock")
}

// ─────────────────────────────────────────────────
// Advisory lock helpers
// ─────────────────────────────────────────────────

struct RegistryLock {
    _file: fs::File,
}

fn acquire_registry_lock(home_dir: &Path) -> Result<RegistryLock> {
    let state_root = service_state_root(home_dir);
    fs::create_dir_all(&state_root)?;
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .open(registry_lock_path(home_dir))?;
    flock(&file, FlockOperation::LockExclusive)?;
    Ok(RegistryLock { _file: file })
}

// ─────────────────────────────────────────────────
// Checkout identity
// ─────────────────────────────────────────────────

/// A stable, random identifier for one checkout of a project. Stored at
/// `.fluent/work/scheduler/identity` and never changes after creation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckoutIdentity(pub String);

/// Assign a checkout identity to the project, creating one if absent. Returns
/// the existing identity on repeated calls. The file lives under a directory
/// that should be git-ignored.
pub fn assign_checkout_identity(project_root: &Path) -> Result<CheckoutIdentity> {
    let path = checkout_identity_path(project_root);
    if path.exists() {
        let raw = fs::read_to_string(&path)
            .with_context(|| format!("read checkout identity {}", path.display()))?;
        let s = raw.trim().to_string();
        if !s.is_empty() {
            return Ok(CheckoutIdentity(s));
        }
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let identity = generate_random_hex(16)?;
    atomic_write(&path, identity.as_bytes())?;
    Ok(CheckoutIdentity(identity))
}

// ─────────────────────────────────────────────────
// User registry
// ─────────────────────────────────────────────────

#[derive(Debug, Default, Serialize, Deserialize)]
struct RegistryFile {
    /// canonical checkout path → checkout identity hex string
    checkouts: HashMap<String, String>,
}

/// Atomically pair this checkout's canonical path with its identity in the
/// user-private registry at `~/.config/fluent/scheduler/registry.json`.
pub fn register_checkout(
    home_dir: &Path,
    project_root: &Path,
    identity: &CheckoutIdentity,
) -> Result<()> {
    let canonical = project_root
        .canonicalize()
        .with_context(|| format!("canonicalize {}", project_root.display()))?;
    let state_root = service_state_root(home_dir);
    fs::create_dir_all(&state_root)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&state_root, fs::Permissions::from_mode(0o700))?;
    }
    let _lock = acquire_registry_lock(home_dir)?;
    let mut reg = read_registry_raw(home_dir).unwrap_or_default();
    reg.checkouts
        .insert(canonical.to_string_lossy().into_owned(), identity.0.clone());
    atomic_write(&registry_path(home_dir), &serde_json::to_vec_pretty(&reg)?)?;
    Ok(())
}

/// Remove a checkout from the user registry.
pub fn unregister_checkout(home_dir: &Path, project_root: &Path) -> Result<()> {
    let canonical = project_root
        .canonicalize()
        .with_context(|| format!("canonicalize {}", project_root.display()))?;
    let _lock = acquire_registry_lock(home_dir)?;
    let mut reg = read_registry_raw(home_dir).unwrap_or_default();
    reg.checkouts
        .remove(&canonical.to_string_lossy().into_owned());
    let reg_path = registry_path(home_dir);
    if let Some(parent) = reg_path.parent() {
        fs::create_dir_all(parent)?;
    }
    atomic_write(&reg_path, &serde_json::to_vec_pretty(&reg)?)?;
    Ok(())
}

/// Read all registered checkouts. Returns an empty map if the registry is
/// absent.
pub fn read_registry(home_dir: &Path) -> Result<HashMap<PathBuf, CheckoutIdentity>> {
    let reg = read_registry_raw(home_dir).unwrap_or_default();
    Ok(reg
        .checkouts
        .into_iter()
        .map(|(path, id)| (PathBuf::from(path), CheckoutIdentity(id)))
        .collect())
}

fn read_registry_raw(home_dir: &Path) -> Result<RegistryFile> {
    let path = registry_path(home_dir);
    let bytes = fs::read(&path).with_context(|| format!("read registry {}", path.display()))?;
    serde_json::from_slice(&bytes).with_context(|| format!("parse registry {}", path.display()))
}

// ─────────────────────────────────────────────────
// Build identity
// ─────────────────────────────────────────────────

/// Build identity: version string + hex-encoded SHA-256 of the binary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildIdentity {
    pub version: String,
    pub hash: String,
}

/// Copy the source executable to the managed service path atomically. Returns
/// the build identity of the staged binary. An interrupted copy leaves no
/// partial target because it writes through a temp file.
pub fn stage_executable(source_exe: &Path, home_dir: &Path) -> Result<BuildIdentity> {
    use sha2::{Digest, Sha256};
    let bytes = fs::read(source_exe)
        .with_context(|| format!("read source executable {}", source_exe.display()))?;
    let hash = format!("{:x}", Sha256::digest(&bytes));
    let version = crate::version::version_tag();
    let dest = managed_executable_path(home_dir);
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    atomic_write(&dest, &bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&dest, fs::Permissions::from_mode(0o700))?;
    }
    Ok(BuildIdentity { version, hash })
}

/// Record the desired build identity (what we intend the service to run).
pub fn record_desired_build(home_dir: &Path, build: &BuildIdentity) -> Result<()> {
    let path = desired_build_path(home_dir);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    atomic_write(&path, &serde_json::to_vec_pretty(build)?)?;
    Ok(())
}

/// Record the observed build identity (the last known running version).
pub fn record_observed_build(home_dir: &Path, build: &BuildIdentity) -> Result<()> {
    let path = observed_build_path(home_dir);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    atomic_write(&path, &serde_json::to_vec_pretty(build)?)?;
    Ok(())
}

pub fn read_desired_build(home_dir: &Path) -> Result<Option<BuildIdentity>> {
    read_optional_build(desired_build_path(home_dir))
}

pub fn read_observed_build(home_dir: &Path) -> Result<Option<BuildIdentity>> {
    read_optional_build(observed_build_path(home_dir))
}

fn read_optional_build(path: PathBuf) -> Result<Option<BuildIdentity>> {
    match fs::read(&path) {
        Ok(bytes) => {
            Ok(Some(serde_json::from_slice(&bytes).with_context(|| {
                format!("parse build state {}", path.display())
            })?))
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("read build state {}", path.display())),
    }
}

// ─────────────────────────────────────────────────
// ServiceManager trait
// ─────────────────────────────────────────────────

/// Platform-neutral interface for managing the scheduler service process.
/// The production LaunchAgent implementation belongs to the `init-status-release`
/// Work Item. Only a `FakeServiceManager` is provided here.
pub trait ServiceManager: Send + Sync {
    fn install_or_update(
        &self,
        executable: &Path,
        build: &BuildIdentity,
        sock_path: &Path,
    ) -> Result<()>;
    fn enable(&self) -> Result<()>;
    fn disable(&self) -> Result<()>;
    fn start(&self) -> Result<()>;
    fn stop(&self) -> Result<()>;
    /// Drain: stop accepting new work and wait for in-flight work before
    /// stopping. The fake drains immediately (no in-flight work).
    fn drain(&self) -> Result<()>;
    fn is_installed(&self) -> bool;
    fn is_enabled(&self) -> bool;
    fn is_running(&self) -> bool;
}

// ─────────────────────────────────────────────────
// FakeServiceManager
// ─────────────────────────────────────────────────

struct FakeInner {
    installed: bool,
    enabled: bool,
    running: bool,
    build: Option<BuildIdentity>,
    sock_path: Option<PathBuf>,
}

/// In-process fake for tests. Tracks lifecycle state without binding real
/// sockets or spawning platform daemons. Use `FakeSocketListener` when the
/// test needs an actual health exchange over a Unix socket.
pub struct FakeServiceManager {
    inner: Mutex<FakeInner>,
}

impl FakeServiceManager {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(FakeInner {
                installed: false,
                enabled: false,
                running: false,
                build: None,
                sock_path: None,
            }),
        }
    }

    pub fn installed_build(&self) -> Option<BuildIdentity> {
        self.inner.lock().unwrap().build.clone()
    }

    pub fn installed_sock_path(&self) -> Option<PathBuf> {
        self.inner.lock().unwrap().sock_path.clone()
    }
}

impl Default for FakeServiceManager {
    fn default() -> Self {
        Self::new()
    }
}

impl ServiceManager for FakeServiceManager {
    fn install_or_update(
        &self,
        _executable: &Path,
        build: &BuildIdentity,
        sock_path: &Path,
    ) -> Result<()> {
        let mut s = self.inner.lock().unwrap();
        s.installed = true;
        s.build = Some(build.clone());
        s.sock_path = Some(sock_path.to_path_buf());
        Ok(())
    }

    fn enable(&self) -> Result<()> {
        let mut s = self.inner.lock().unwrap();
        if !s.installed {
            bail!("cannot enable: service not installed");
        }
        s.enabled = true;
        Ok(())
    }

    fn disable(&self) -> Result<()> {
        self.inner.lock().unwrap().enabled = false;
        Ok(())
    }

    fn start(&self) -> Result<()> {
        let mut s = self.inner.lock().unwrap();
        if !s.installed {
            bail!("cannot start: service not installed");
        }
        s.running = true;
        Ok(())
    }

    fn stop(&self) -> Result<()> {
        self.inner.lock().unwrap().running = false;
        Ok(())
    }

    fn drain(&self) -> Result<()> {
        self.stop()
    }

    fn is_installed(&self) -> bool {
        self.inner.lock().unwrap().installed
    }
    fn is_enabled(&self) -> bool {
        self.inner.lock().unwrap().enabled
    }
    fn is_running(&self) -> bool {
        self.inner.lock().unwrap().running
    }
}

// ─────────────────────────────────────────────────
// FakeSocketListener
// ─────────────────────────────────────────────────

/// A no-op socket listener for testing the health exchange protocol in
/// environments that permit Unix domain socket creation.
pub struct FakeSocketListener {
    sock_path: PathBuf,
    stop_flag: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl FakeSocketListener {
    /// Bind a Unix socket at `sock_path` and start serving health responses.
    pub fn start(sock_path: &Path, build: BuildIdentity) -> Result<Self> {
        let _ = fs::remove_file(sock_path);
        if let Some(parent) = sock_path.parent() {
            fs::create_dir_all(parent)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
            }
        }
        let listener = UnixListener::bind(sock_path)
            .with_context(|| format!("bind socket {}", sock_path.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(sock_path, fs::Permissions::from_mode(0o600))?;
        }
        let stop_flag = Arc::new(AtomicBool::new(false));
        let flag = stop_flag.clone();
        let handle = std::thread::spawn(move || {
            run_fake_listener(listener, build, flag);
        });
        Ok(Self {
            sock_path: sock_path.to_path_buf(),
            stop_flag,
            handle: Some(handle),
        })
    }
}

impl Drop for FakeSocketListener {
    fn drop(&mut self) {
        self.stop_flag.store(true, Ordering::Release);
        let _ = UnixStream::connect(&self.sock_path);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

// ─────────────────────────────────────────────────
// Socket protocol
// ─────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum SocketRequest {
    Health { generation: u32 },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum SocketResponse {
    HealthOk {
        generation: u32,
        build: BuildIdentity,
    },
}

/// The payload of a successful health exchange.
#[derive(Debug, Clone)]
pub struct HealthResponse {
    pub generation: u32,
    pub build: BuildIdentity,
}

/// Send a health request over the Unix socket and return the response.
pub fn send_health_request(sock_path: &Path) -> Result<HealthResponse> {
    let mut stream = UnixStream::connect(sock_path)
        .with_context(|| format!("connect to {}", sock_path.display()))?;
    let req = SocketRequest::Health {
        generation: PROTOCOL_GENERATION,
    };
    frame_write(&mut stream, &serde_json::to_vec(&req)?)?;
    let resp_bytes = frame_read(&mut stream)?;
    match serde_json::from_slice::<SocketResponse>(&resp_bytes).context("parse health response")? {
        SocketResponse::HealthOk { generation, build } => Ok(HealthResponse { generation, build }),
    }
}

fn frame_write(stream: &mut UnixStream, payload: &[u8]) -> io::Result<()> {
    let len = payload.len() as u32;
    stream.write_all(&len.to_le_bytes())?;
    stream.write_all(payload)
}

fn frame_read(stream: &mut UnixStream) -> io::Result<Vec<u8>> {
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf)?;
    let len = u32::from_le_bytes(len_buf) as usize;
    let mut payload = vec![0u8; len];
    stream.read_exact(&mut payload)?;
    Ok(payload)
}

fn run_fake_listener(listener: UnixListener, build: BuildIdentity, stop_flag: Arc<AtomicBool>) {
    for stream_result in listener.incoming() {
        if stop_flag.load(Ordering::Acquire) {
            break;
        }
        let Ok(mut stream) = stream_result else {
            continue;
        };
        if stop_flag.load(Ordering::Acquire) {
            break;
        }
        let Ok(payload) = frame_read(&mut stream) else {
            continue;
        };
        let resp = match serde_json::from_slice::<SocketRequest>(&payload) {
            Ok(SocketRequest::Health { .. }) => SocketResponse::HealthOk {
                generation: PROTOCOL_GENERATION,
                build: build.clone(),
            },
            Err(_) => continue,
        };
        if let Ok(bytes) = serde_json::to_vec(&resp) {
            let _ = frame_write(&mut stream, &bytes);
        }
    }
}

// ─────────────────────────────────────────────────
// Service lifecycle
// ─────────────────────────────────────────────────

/// The outcome of `start_or_reuse_service`.
pub enum ServiceStartOutcome {
    /// A new service instance was started.
    Started(HealthResponse),
    /// An existing healthy instance answered the health check.
    Reused(HealthResponse),
}

/// Set up the service for a project: assign a checkout identity, register it
/// in the user registry, stage the current executable, record the desired
/// build, and install through the service manager. Returns the staged build
/// identity.
pub fn setup_service(
    project_root: &Path,
    home_dir: &Path,
    source_exe: &Path,
    manager: &dyn ServiceManager,
) -> Result<BuildIdentity> {
    let identity = assign_checkout_identity(project_root)?;
    register_checkout(home_dir, project_root, &identity)?;
    let build = stage_executable(source_exe, home_dir)?;
    record_desired_build(home_dir, &build)?;
    let sock = socket_path(home_dir);
    manager.install_or_update(&managed_executable_path(home_dir), &build, &sock)?;
    Ok(build)
}

/// Start the service if not already running, or reuse a healthy existing
/// instance. On a fresh start, records the observed build identity.
pub fn start_or_reuse_service(
    home_dir: &Path,
    build: &BuildIdentity,
    manager: &dyn ServiceManager,
) -> Result<ServiceStartOutcome> {
    let sock = socket_path(home_dir);
    if let Ok(health) = send_health_request(&sock) {
        return Ok(ServiceStartOutcome::Reused(health));
    }
    manager.start()?;
    let health = wait_for_health(&sock, std::time::Duration::from_secs(5))?;
    record_observed_build(home_dir, build)?;
    Ok(ServiceStartOutcome::Started(health))
}

fn wait_for_health(sock_path: &Path, timeout: std::time::Duration) -> Result<HealthResponse> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if let Ok(health) = send_health_request(sock_path) {
            return Ok(health);
        }
        if std::time::Instant::now() >= deadline {
            bail!("service did not become healthy within {:?}", timeout);
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

// ─────────────────────────────────────────────────
// AttemptExecutionRequest
// ─────────────────────────────────────────────────

/// A frozen, immutable record of one Attempt execution to dispatch. Must be
/// persisted before claim so a crash between persist and launch is recoverable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttemptExecutionRequest {
    pub id: String,
    pub work_item_id: String,
    pub attempt_id: String,
    pub created_at: String,
}

impl AttemptExecutionRequest {
    pub fn new(work_item_id: impl Into<String>, attempt_id: impl Into<String>) -> Result<Self> {
        Ok(Self {
            id: generate_random_hex(16)?,
            work_item_id: work_item_id.into(),
            attempt_id: attempt_id.into(),
            created_at: chrono::Utc::now().to_rfc3339(),
        })
    }
}

fn request_path(project_root: &Path, request_id: &str) -> PathBuf {
    requests_dir(project_root).join(format!("{request_id}.json"))
}

/// Persist a frozen `AttemptExecutionRequest`. Idempotent: writing the same id
/// twice with identical content is safe.
pub fn persist_request(project_root: &Path, request: &AttemptExecutionRequest) -> Result<()> {
    let path = request_path(project_root, &request.id);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    atomic_write(&path, &serde_json::to_vec_pretty(request)?)?;
    Ok(())
}

/// Load a previously persisted `AttemptExecutionRequest` by id.
pub fn load_request(project_root: &Path, request_id: &str) -> Result<AttemptExecutionRequest> {
    let path = request_path(project_root, request_id);
    let bytes = fs::read(&path).with_context(|| format!("read request {}", path.display()))?;
    serde_json::from_slice(&bytes).with_context(|| format!("parse request {}", path.display()))
}

// ─────────────────────────────────────────────────
// Dispatch boundary
// ─────────────────────────────────────────────────

/// Token returned by a successful idempotent dispatch submission.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DispatchToken {
    pub request_id: String,
    pub work_item_id: String,
    pub attempt_id: String,
}

fn dispatch_token_path(project_root: &Path, request_id: &str) -> PathBuf {
    dispatch_tokens_dir(project_root).join(format!("{request_id}.json"))
}

/// Submit an `AttemptExecutionRequest` through an exact-bound idempotent
/// dispatch boundary. The same request id always returns the same token.
/// The request must be persisted before calling this function.
pub fn submit_dispatch(
    project_root: &Path,
    request: &AttemptExecutionRequest,
) -> Result<DispatchToken> {
    let token_path = dispatch_token_path(project_root, &request.id);
    if token_path.exists() {
        let bytes = fs::read(&token_path)
            .with_context(|| format!("read dispatch token {}", token_path.display()))?;
        return serde_json::from_slice(&bytes)
            .with_context(|| format!("parse dispatch token {}", token_path.display()));
    }
    let persisted = request_path(project_root, &request.id);
    if !persisted.exists() {
        bail!(
            "request {} must be persisted before dispatch; {} not found",
            request.id,
            persisted.display()
        );
    }
    let token = DispatchToken {
        request_id: request.id.clone(),
        work_item_id: request.work_item_id.clone(),
        attempt_id: request.attempt_id.clone(),
    };
    if let Some(parent) = token_path.parent() {
        fs::create_dir_all(parent)?;
    }
    atomic_write(&token_path, &serde_json::to_vec_pretty(&token)?)?;
    Ok(token)
}

// ─────────────────────────────────────────────────
// AttemptEvent
// ─────────────────────────────────────────────────

/// A bounded, sequenced event in the lifecycle of one Attempt execution.
/// Events are appended as NDJSON and replayed in sequence order. Raw coder
/// transcripts are never stored here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttemptEvent {
    /// Monotonically increasing sequence number within this Attempt's log.
    pub seq: u64,
    /// Event kind string (e.g. `"started"`, `"completed"`, `"failed"`).
    pub kind: String,
    pub attempt_id: String,
    pub timestamp: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

fn events_path(project_root: &Path, attempt_id: &str) -> PathBuf {
    events_dir(project_root).join(format!("{attempt_id}.ndjson"))
}

/// Append one event to the Attempt's append-only event log. The caller is
/// responsible for supplying a monotonically increasing `seq` field.
pub fn append_event(project_root: &Path, event: &AttemptEvent) -> Result<()> {
    let path = events_path(project_root, &event.attempt_id);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut line = serde_json::to_string(event)?;
    line.push('\n');
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("open event log {}", path.display()))?;
    file.write_all(line.as_bytes())
        .with_context(|| format!("append to event log {}", path.display()))?;
    Ok(())
}

/// Replay all persisted events for an Attempt, sorted by sequence number.
pub fn replay_events(project_root: &Path, attempt_id: &str) -> Result<Vec<AttemptEvent>> {
    let path = events_path(project_root, attempt_id);
    match fs::read_to_string(&path) {
        Ok(content) => {
            let mut events: Vec<AttemptEvent> = content
                .lines()
                .filter(|l| !l.trim().is_empty())
                .map(|line| {
                    serde_json::from_str(line)
                        .with_context(|| format!("parse event in {}", path.display()))
                })
                .collect::<Result<_>>()?;
            events.sort_by_key(|e| e.seq);
            Ok(events)
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(vec![]),
        Err(e) => Err(e).with_context(|| format!("read event log {}", path.display())),
    }
}

// ─────────────────────────────────────────────────
// Executor contract
// ─────────────────────────────────────────────────

/// A validated execution request. The request is well-formed, durably
/// persisted, and its ids are consistent.
pub struct ValidatedExecutionRequest {
    pub request: AttemptExecutionRequest,
}

/// Validate an `AttemptExecutionRequest` for private executor invocation
/// without launching a coder. Returns `Err` if the request is missing,
/// malformed, or structurally invalid.
pub fn validate_executor_request(
    project_root: &Path,
    request_id: &str,
) -> Result<ValidatedExecutionRequest> {
    let request = load_request(project_root, request_id)?;
    if request.id != request_id {
        bail!(
            "request id mismatch: file contains {:?}, expected {:?}",
            request.id,
            request_id
        );
    }
    if request.work_item_id.is_empty() {
        bail!("work_item_id is empty in request {request_id}");
    }
    if request.attempt_id.is_empty() {
        bail!("attempt_id is empty in request {request_id}");
    }
    if request.created_at.is_empty() {
        bail!("created_at is empty in request {request_id}");
    }
    Ok(ValidatedExecutionRequest { request })
}

// ─────────────────────────────────────────────────
// Utilities
// ─────────────────────────────────────────────────

fn generate_random_hex(n_bytes: usize) -> Result<String> {
    let mut buf = vec![0u8; n_bytes];
    let mut f = fs::File::open("/dev/urandom").context("open /dev/urandom")?;
    f.read_exact(&mut buf).context("read random bytes")?;
    Ok(buf.iter().map(|b| format!("{b:02x}")).collect())
}
