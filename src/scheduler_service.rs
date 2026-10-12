//! Persistent scheduler service foundation: checkout identity, user registry,
//! service-manager boundary, versioned socket health, immutable Attempt
//! execution requests, sequenced events, and a private executor contract.

use std::collections::{HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::io::{Seek, SeekFrom};
use std::os::unix::fs::MetadataExt;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use rustix::fs::{FlockOperation, flock};
use serde::{Deserialize, Serialize};

use crate::atomic_write::atomic_write;

/// Integer protocol generation. Bump when the request/response schema changes
/// incompatibly.
pub const PROTOCOL_GENERATION: u32 = 1;
const SERVICE_METADATA_SCHEMA: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceState {
    Running,
    Draining,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceMetadata {
    pub schema: u32,
    pub instance_id: String,
    pub state: ServiceState,
    pub build: BuildIdentity,
}

fn service_lock_path(home: &Path) -> PathBuf {
    service_state_root(home).join("service.lock")
}
fn service_metadata_path(home: &Path) -> PathBuf {
    service_state_root(home).join("service.json")
}
fn service_stop_path(home: &Path) -> PathBuf {
    service_state_root(home).join("stop-request.json")
}
fn service_instance_stop_path(home: &Path, instance_id: &str) -> PathBuf {
    service_state_root(home)
        .join("stop-requests")
        .join(format!("{instance_id}.json"))
}
fn service_result_path(home: &Path, instance_id: &str) -> PathBuf {
    service_state_root(home)
        .join("stop-results")
        .join(format!("{instance_id}.json"))
}

struct ServiceLease {
    file: fs::File,
    home: PathBuf,
    metadata: ServiceMetadata,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceStopRecord {
    pub schema: u32,
    pub instance_id: String,
    pub build: BuildIdentity,
    #[serde(default)]
    pub failures: Vec<String>,
}

fn read_stop_record(path: &Path) -> Result<Option<ServiceStopRecord>> {
    match fs::read(path) {
        Ok(bytes) => {
            let record: ServiceStopRecord =
                serde_json::from_slice(&bytes).context("parse service stop record")?;
            if record.schema != SERVICE_METADATA_SCHEMA || record.instance_id.len() != 32 {
                bail!("invalid service stop record");
            }
            Ok(Some(record))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).context("read service stop record"),
    }
}

fn write_stop_record(path: &Path, record: &ServiceStopRecord) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    atomic_write(path, &serde_json::to_vec_pretty(record)?).context("publish service stop record")
}

fn read_service_metadata(home: &Path) -> Result<Option<ServiceMetadata>> {
    match fs::read(service_metadata_path(home)) {
        Ok(bytes) => {
            let metadata: ServiceMetadata =
                serde_json::from_slice(&bytes).context("parse service identity")?;
            if metadata.schema != SERVICE_METADATA_SCHEMA
                || metadata.instance_id.len() != 32
                || !metadata.instance_id.bytes().all(|b| b.is_ascii_hexdigit())
            {
                bail!("invalid scheduler service identity metadata");
            }
            Ok(Some(metadata))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).context("read service identity"),
    }
}

fn publish_service_metadata(home: &Path, metadata: &ServiceMetadata) -> Result<()> {
    fs::create_dir_all(service_state_root(home))?;
    atomic_write(
        &service_metadata_path(home),
        &serde_json::to_vec_pretty(metadata)?,
    )
    .context("publish service identity")
}

fn lock_identity(home: &Path) -> Result<String> {
    Ok(fs::read_to_string(service_lock_path(home))?
        .trim()
        .to_owned())
}

fn service_lease_live(home: &Path) -> Result<bool> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(service_lock_path(home))?;
    match flock(&file, FlockOperation::NonBlockingLockExclusive) {
        Ok(()) => {
            flock(&file, FlockOperation::Unlock)?;
            Ok(false)
        }
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(true),
        Err(error) => Err(error).context("inspect service lease"),
    }
}

fn try_service_lease(home: &Path, build: &BuildIdentity) -> Result<Option<ServiceLease>> {
    fs::create_dir_all(service_state_root(home))?;
    let mut file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(service_lock_path(home))?;
    match flock(&file, FlockOperation::NonBlockingLockExclusive) {
        Ok(()) => {
            let metadata = ServiceMetadata {
                schema: SERVICE_METADATA_SCHEMA,
                instance_id: generate_random_hex(16)?,
                state: ServiceState::Running,
                build: build.clone(),
            };
            file.set_len(0)?;
            file.seek(SeekFrom::Start(0))?;
            file.write_all(metadata.instance_id.as_bytes())?;
            file.sync_data()?;
            Ok(Some(ServiceLease {
                file,
                home: home.to_owned(),
                metadata,
            }))
        }
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(None),
        Err(error) => Err(error).context("elect scheduler service"),
    }
}

impl Drop for ServiceLease {
    fn drop(&mut self) {
        if read_service_metadata(&self.home)
            .ok()
            .flatten()
            .is_some_and(|m| m.instance_id == self.metadata.instance_id)
        {
            let _ = fs::remove_file(service_metadata_path(&self.home));
        }
        let _ = flock(&self.file, FlockOperation::Unlock);
    }
}

/// Maximum size of a single protocol frame in bytes (64 KiB). Frames that
/// declare or carry more than this many bytes are rejected before allocation.
pub const MAX_FRAME_SIZE: usize = 64 * 1024;

static SERVICE_SHUTDOWN: AtomicBool = AtomicBool::new(false);

/// Install the scheduler service's graceful signal boundary before identity,
/// registry, or executable hashing can delay entry into the service loop.
pub fn prepare_run_signal_handler() {
    SERVICE_SHUTDOWN.store(false, Ordering::Release);
    let _ = ctrlc::set_handler(|| {
        SERVICE_SHUTDOWN.store(true, Ordering::Release);
    });
}

// ─────────────────────────────────────────────────
// Filesystem layout
// ─────────────────────────────────────────────────

/// Private user-state root: `~/.config/fluent/scheduler/`.
pub fn service_state_root(home_dir: &Path) -> PathBuf {
    home_dir.join(".config/fluent/scheduler")
}

/// Resolve the scheduler service's writable home for this process.
///
/// Production always uses the operator's home. A test-support binary with the
/// explicit host-preflight seam runs inside the configured Tester sandbox, which
/// grants the fixture checkout but intentionally withholds the operator's home.
/// Keep that service state inside the fixture so parallel scheduler tests neither
/// escape their sandbox nor share a singleton socket.
pub fn runtime_home(_project_root: &Path) -> Result<PathBuf> {
    #[cfg(feature = "test-support")]
    if std::env::var_os("FLUENT_TEST_HOST_SANDBOX_PREFLIGHT").is_some() {
        // Keep state relative to the fixture checkout so parallel tests remain
        // isolated without writing the operator's home.
        return Ok(PathBuf::from(".fluent/work/scheduler/service-home"));
    }
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .context("scheduler service requires a home directory")
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

struct RequestLock {
    _file: fs::File,
}

fn request_lock_path(project_root: &Path, request_id: &str) -> PathBuf {
    requests_dir(project_root).join(format!("{request_id}.lock"))
}

fn acquire_request_lock(project_root: &Path, request_id: &str) -> Result<RequestLock> {
    let dir = requests_dir(project_root);
    fs::create_dir_all(&dir)?;
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .open(request_lock_path(project_root, request_id))?;
    flock(&file, FlockOperation::LockExclusive)?;
    Ok(RequestLock { _file: file })
}

struct DispatchLock {
    _file: fs::File,
}

fn dispatch_lock_path(project_root: &Path, request_id: &str) -> PathBuf {
    dispatch_tokens_dir(project_root).join(format!("{request_id}.lock"))
}

fn acquire_dispatch_lock(project_root: &Path, request_id: &str) -> Result<DispatchLock> {
    let dir = dispatch_tokens_dir(project_root);
    fs::create_dir_all(&dir)?;
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .open(dispatch_lock_path(project_root, request_id))?;
    flock(&file, FlockOperation::LockExclusive)?;
    Ok(DispatchLock { _file: file })
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

/// Read the registry for observation without masking a corrupt or unreadable file.
pub fn read_registry_for_dashboard(home_dir: &Path) -> Result<HashMap<PathBuf, CheckoutIdentity>> {
    match fs::metadata(registry_path(home_dir)) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(HashMap::new()),
        Err(error) => return Err(error).context("read scheduler registry metadata"),
    }
    Ok(read_registry_raw(home_dir)?
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

/// Compute the immutable build identity of an executable without staging it.
pub fn executable_build_identity(executable: &Path) -> Result<BuildIdentity> {
    let bytes = fs::read(executable)
        .with_context(|| format!("read executable {}", executable.display()))?;
    Ok(build_identity_for_bytes(&bytes))
}

fn build_identity_for_bytes(bytes: &[u8]) -> BuildIdentity {
    use sha2::{Digest, Sha256};
    BuildIdentity {
        version: crate::version::version_tag(),
        hash: format!("{:x}", Sha256::digest(&bytes)),
    }
}

/// Copy the source executable to the managed service path atomically. Returns
/// the build identity of the staged binary. An interrupted copy leaves no
/// partial target because it writes through a temp file.
pub fn stage_executable(source_exe: &Path, home_dir: &Path) -> Result<BuildIdentity> {
    let bytes = fs::read(source_exe)
        .with_context(|| format!("read source executable {}", source_exe.display()))?;
    let build = build_identity_for_bytes(&bytes);
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
    Ok(build)
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
    Health {
        generation: u32,
    },
    Drain {
        generation: u32,
        instance_id: String,
    },
    SubmitExecution {
        generation: u32,
        build: BuildIdentity,
        project_root: PathBuf,
        request: AttemptExecutionRequest,
    },
    ExecutionStatus {
        generation: u32,
        build: BuildIdentity,
        project_root: PathBuf,
        request_id: String,
    },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum SocketResponse {
    HealthOk {
        generation: u32,
        build: BuildIdentity,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        instance_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        state: Option<ServiceState>,
    },
    DrainAccepted {
        generation: u32,
        instance_id: String,
        state: ServiceState,
    },
    ExecutionSubmitted {
        generation: u32,
        token: DispatchToken,
    },
    ExecutionStatus {
        generation: u32,
        request_id: String,
        work_item_id: String,
        dispatch_id: String,
        status: String,
        attempt_id: Option<String>,
    },
    Error {
        generation: u32,
        message: String,
    },
}

/// The payload of a successful health exchange.
#[derive(Debug, Clone)]
pub struct HealthResponse {
    pub generation: u32,
    pub build: BuildIdentity,
    pub instance_id: Option<String>,
    pub state: Option<ServiceState>,
}

/// Authoritative state of the real queue dispatch created for one service
/// request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionStatus {
    pub request_id: String,
    pub work_item_id: String,
    pub dispatch_id: String,
    pub status: String,
    pub attempt_id: Option<String>,
}

/// Send a health request over the Unix socket and return the response.
pub fn send_health_request(sock_path: &Path) -> Result<HealthResponse> {
    retry_health(|| {
        let mut stream = UnixStream::connect(sock_path)
            .with_context(|| format!("connect to {}", sock_path.display()))?;
        health_on_stream(&mut stream)
    })
}

fn retry_health(
    mut attempt_health: impl FnMut() -> Result<HealthResponse>,
) -> Result<HealthResponse> {
    // A peer can close one exchange while the service stays live. Reconnect
    // briefly, but never retry a malformed or unverifiable health response.
    for attempt in 0..3 {
        match attempt_health() {
            Ok(health) => return Ok(health),
            Err(error)
                if attempt < 2
                    && error.downcast_ref::<io::Error>().is_some_and(|error| {
                        matches!(
                            error.kind(),
                            io::ErrorKind::BrokenPipe
                                | io::ErrorKind::ConnectionReset
                                | io::ErrorKind::UnexpectedEof
                        )
                    }) =>
            {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(error) => return Err(error),
        }
    }
    unreachable!("the final health attempt returns its response or error")
}

#[cfg(test)]
mod health_retry_tests {
    use super::*;

    #[test]
    fn retries_closed_health_connection_without_accepting_unverified_reply() {
        let mut calls = 0;
        let health = retry_health(|| {
            calls += 1;
            if calls == 1 {
                return Err(io::Error::from(io::ErrorKind::BrokenPipe).into());
            }
            Ok(HealthResponse {
                generation: PROTOCOL_GENERATION,
                build: BuildIdentity {
                    version: "test".into(),
                    hash: "hash".into(),
                },
                instance_id: Some("0123456789abcdef0123456789abcdef".into()),
                state: Some(ServiceState::Running),
            })
        })
        .unwrap();
        assert_eq!(calls, 2);
        assert_eq!(health.build.version, "test");
        let mut invalid_calls = 0;
        assert!(
            retry_health(|| {
                invalid_calls += 1;
                bail!("unverified health response")
            })
            .is_err()
        );
        assert_eq!(invalid_calls, 1);
    }
}

fn health_on_stream(stream: &mut UnixStream) -> Result<HealthResponse> {
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    let req = SocketRequest::Health {
        generation: PROTOCOL_GENERATION,
    };
    frame_write(stream, &serde_json::to_vec(&req)?)?;
    let resp_bytes = frame_read(stream)?;
    match serde_json::from_slice::<SocketResponse>(&resp_bytes).context("parse health response")? {
        SocketResponse::HealthOk {
            generation,
            build,
            instance_id,
            state,
        } => Ok(HealthResponse {
            generation,
            build,
            instance_id,
            state,
        }),
        SocketResponse::Error { message, .. } => bail!("scheduler service: {message}"),
        _ => bail!("scheduler service returned an unexpected health response"),
    }
}

fn inspect_service(home: &Path, requested: &BuildIdentity) -> Result<Option<HealthResponse>> {
    inspect_service_with_state(home, requested, false)
}

fn inspect_service_with_state(
    home: &Path,
    requested: &BuildIdentity,
    allow_draining: bool,
) -> Result<Option<HealthResponse>> {
    let path = socket_path(home);
    let mut stream = match UnixStream::connect(&path) {
        Ok(stream) => stream,
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
            ) =>
        {
            return Ok(None);
        }
        Err(error) => return Err(error).context("inspect scheduler service socket"),
    };
    // Once connected, a malformed reply or timeout is an unverified live owner.
    let health = health_on_stream(&mut stream).context(
        "reachable scheduler service cannot be inspected; stop its owning process manually",
    )?;
    if health.generation != PROTOCOL_GENERATION
        || health.instance_id.is_none()
        || health.state.is_none()
    {
        bail!(
            "live scheduler service running {} ({}) lacks verifiable drain control; stop its original supervised process, wait for active work to finish, then run `fluent scheduler run`",
            health.build.version,
            health.build.hash
        );
    }
    let metadata = read_service_metadata(home)?.context(
        "live scheduler service lacks identity metadata; stop its owning process manually",
    )?;
    if health.instance_id.as_deref() != Some(metadata.instance_id.as_str())
        || lock_identity(home)? != metadata.instance_id
        || health.build != metadata.build
        || health.state != Some(metadata.state)
    {
        bail!(
            "live scheduler service ownership cannot be validated; stop its owning process manually"
        );
    }
    if !service_lease_live(home)? {
        bail!("live scheduler service has no ownership lease; stop its owning process manually");
    }
    validate_service_reuse(&health, &metadata, requested, allow_draining)?;
    Ok(Some(health))
}

/// Apply the shared build and lifecycle decision after ownership has been checked.
pub fn validate_service_reuse(
    health: &HealthResponse,
    metadata: &ServiceMetadata,
    requested: &BuildIdentity,
    allow_draining: bool,
) -> Result<()> {
    if health.instance_id.is_none() || health.state.is_none() {
        bail!(
            "live scheduler service running {} ({}) lacks verifiable drain control; stop its original supervised process, wait for active work to finish, then run `fluent scheduler run`",
            health.build.version,
            health.build.hash
        );
    }
    if health.generation != PROTOCOL_GENERATION
        || health.instance_id.as_deref() != Some(&metadata.instance_id)
        || health.build != metadata.build
        || health.state != Some(metadata.state)
    {
        bail!("scheduler service identity or protocol does not match its lease");
    }
    if metadata.state == ServiceState::Draining && !allow_draining {
        bail!(
            "scheduler service {} is draining; wait for `fluent scheduler stop --all` to finish",
            metadata.instance_id
        );
    }
    if health.build != *requested {
        bail!(
            "a live scheduler service is running {} ({}), but this Fluent build is {} ({}); run `fluent scheduler stop --all`, then `fluent scheduler run`",
            health.build.version,
            health.build.hash,
            requested.version,
            requested.hash
        );
    }
    Ok(())
}

/// Submit one immutable execution request through the versioned scheduler
/// service boundary. Only the service writes request and dispatch storage.
pub fn send_execution_request(
    sock_path: &Path,
    project_root: &Path,
    request: &AttemptExecutionRequest,
) -> Result<DispatchToken> {
    let build = send_health_request(sock_path)?.build;
    let response = exchange(
        sock_path,
        &SocketRequest::SubmitExecution {
            generation: PROTOCOL_GENERATION,
            build,
            project_root: project_root.to_path_buf(),
            request: request.clone(),
        },
    )?;
    match response {
        SocketResponse::ExecutionSubmitted { generation, token }
            if generation == PROTOCOL_GENERATION =>
        {
            Ok(token)
        }
        SocketResponse::Error { message, .. } => bail!("scheduler service: {message}"),
        _ => bail!("scheduler service returned an unexpected execution response"),
    }
}

/// Read scheduler-owned execution status through the versioned service API.
pub fn send_execution_status(
    sock_path: &Path,
    project_root: &Path,
    request_id: &str,
) -> Result<ExecutionStatus> {
    let build = send_health_request(sock_path)?.build;
    let response = exchange(
        sock_path,
        &SocketRequest::ExecutionStatus {
            generation: PROTOCOL_GENERATION,
            build,
            project_root: project_root.to_path_buf(),
            request_id: request_id.to_owned(),
        },
    )?;
    match response {
        SocketResponse::ExecutionStatus {
            generation,
            request_id,
            work_item_id,
            dispatch_id,
            status,
            attempt_id,
        } if generation == PROTOCOL_GENERATION => Ok(ExecutionStatus {
            request_id,
            work_item_id,
            dispatch_id,
            status,
            attempt_id,
        }),
        SocketResponse::Error { message, .. } => bail!("scheduler service: {message}"),
        _ => bail!("scheduler service returned an unexpected status response"),
    }
}

fn exchange(sock_path: &Path, request: &SocketRequest) -> Result<SocketResponse> {
    let mut stream = UnixStream::connect(sock_path)
        .with_context(|| format!("connect to {}", sock_path.display()))?;
    exchange_stream(&mut stream, request)
}

fn exchange_stream(stream: &mut UnixStream, request: &SocketRequest) -> Result<SocketResponse> {
    frame_write(stream, &serde_json::to_vec(request)?)?;
    serde_json::from_slice(&frame_read(stream)?).context("parse scheduler response")
}

fn frame_write(stream: &mut UnixStream, payload: &[u8]) -> io::Result<()> {
    if payload.len() > MAX_FRAME_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "frame payload {} bytes exceeds maximum {} bytes",
                payload.len(),
                MAX_FRAME_SIZE
            ),
        ));
    }
    let len = payload.len() as u32;
    stream.write_all(&len.to_le_bytes())?;
    stream.write_all(payload)
}

fn frame_read(stream: &mut UnixStream) -> io::Result<Vec<u8>> {
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf)?;
    let len = u32::from_le_bytes(len_buf) as usize;
    if len > MAX_FRAME_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "peer declared frame length {} bytes exceeds maximum {} bytes",
                len, MAX_FRAME_SIZE
            ),
        ));
    }
    let mut payload = vec![0u8; len];
    stream.read_exact(&mut payload)?;
    Ok(payload)
}

fn configure_accepted_stream(stream: &UnixStream) -> io::Result<()> {
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))
}

#[cfg(test)]
mod accepted_stream_tests {
    use super::*;

    #[test]
    fn fragmented_frame_waits_for_payload() {
        let (mut accepted, mut client) = UnixStream::pair().unwrap();
        accepted.set_nonblocking(true).unwrap();
        configure_accepted_stream(&accepted).unwrap();
        let payload = b"fragmented request";
        client
            .write_all(&(payload.len() as u32).to_le_bytes())
            .unwrap();
        let sender = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            client.write_all(payload).unwrap();
        });
        let received = frame_read(&mut accepted);
        sender.join().unwrap();
        assert_eq!(received.unwrap(), payload);
    }
}

/// Read a raw protocol frame from a Unix stream.
///
/// Exposed only under the `test-support` feature so integration tests can
/// drive oversized-frame rejection through a `UnixStream::pair()` without
/// binding a socket path.
#[cfg(feature = "test-support")]
pub fn frame_read_test_support(stream: &mut UnixStream) -> io::Result<Vec<u8>> {
    frame_read(stream)
}

/// Write a raw protocol frame to a Unix stream.
///
/// Exposed only under the `test-support` feature for the same reason as
/// `frame_read_test_support`.
#[cfg(feature = "test-support")]
pub fn frame_write_test_support(stream: &mut UnixStream, payload: &[u8]) -> io::Result<()> {
    frame_write(stream, payload)
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
        let _ = serve_health_connection(&mut stream, &build);
    }
}

fn serve_health_connection(stream: &mut UnixStream, build: &BuildIdentity) -> Result<()> {
    let payload = frame_read(stream)?;
    let resp = match serde_json::from_slice::<SocketRequest>(&payload) {
        Ok(SocketRequest::Health { generation }) if generation == PROTOCOL_GENERATION => {
            SocketResponse::HealthOk {
                generation: PROTOCOL_GENERATION,
                build: build.clone(),
                instance_id: None,
                state: None,
            }
        }
        Ok(_) => SocketResponse::Error {
            generation: PROTOCOL_GENERATION,
            message: "fake scheduler listener supports health requests only".into(),
        },
        Err(error) => SocketResponse::Error {
            generation: PROTOCOL_GENERATION,
            message: format!("invalid scheduler request: {error}"),
        },
    };
    frame_write(stream, &serde_json::to_vec(&resp)?)?;
    Ok(())
}

/// Production scheduler service state. It validates every project and build,
/// admits requests to the real durable queue, and owns capacity-limited
/// Attempt workers for every registered checkout.
pub struct SchedulerService {
    home_dir: PathBuf,
    build: BuildIdentity,
    runner: Arc<dyn crate::scheduler::AttemptRunner>,
    workers: Vec<ServiceWorker>,
    coordinators: HashMap<PathBuf, crate::scheduler::CoordinatorLease>,
    prepared_projects: HashSet<PathBuf>,
    stopping_projects: HashSet<PathBuf>,
    failed_stop_projects: HashSet<PathBuf>,
    #[cfg(feature = "test-support")]
    recovery_observer: Option<Arc<dyn Fn(&Path, &crate::queue::Dispatch) + Send + Sync>>,
    #[cfg(feature = "test-support")]
    stop_check: Option<Arc<dyn Fn(&Path) -> Result<()> + Send + Sync>>,
    prepare_host_execution: bool,
    reused_coordinator: Option<crate::scheduler::CoordinatorMetadata>,
    poll_failures: Vec<String>,
    worker_failures: Vec<String>,
    service_identity: Option<ServiceMetadata>,
    draining: bool,
}

struct ServiceWorker {
    project: PathBuf,
    token: crate::queue::DispatchToken,
    handle: JoinHandle<Result<()>>,
}

impl SchedulerService {
    fn accept_drain(&mut self, instance_id: &str) -> Result<()> {
        let identity = self
            .service_identity
            .as_mut()
            .context("service has no lifecycle identity")?;
        if identity.instance_id != instance_id {
            bail!("service instance changed; stale drain request rejected");
        }
        if !self.draining {
            let mut draining = identity.clone();
            draining.state = ServiceState::Draining;
            publish_service_metadata(&self.home_dir, &draining)?;
            *identity = draining;
            self.draining = true;
        }
        Ok(())
    }

    fn read_drain_request(&mut self) -> Result<()> {
        if let Some(identity) = &self.service_identity {
            if let Some(record) = read_stop_record(&service_instance_stop_path(
                &self.home_dir,
                &identity.instance_id,
            ))? {
                #[cfg(feature = "test-support")]
                if let Some(marker) =
                    std::env::var_os("FLUENT_SCHEDULER_TEST_PAUSE_BEFORE_DRAIN_ACK")
                {
                    fs::write(marker, b"requested")?;
                    loop {
                        std::thread::sleep(Duration::from_millis(50));
                    }
                }
                self.accept_drain(&record.instance_id)?;
            }
        }
        Ok(())
    }
    pub fn new(
        home_dir: &Path,
        build: BuildIdentity,
        runner: Arc<dyn crate::scheduler::AttemptRunner>,
    ) -> Self {
        Self {
            home_dir: home_dir.to_path_buf(),
            build,
            runner,
            workers: Vec::new(),
            coordinators: HashMap::new(),
            prepared_projects: HashSet::new(),
            stopping_projects: HashSet::new(),
            failed_stop_projects: HashSet::new(),
            #[cfg(feature = "test-support")]
            recovery_observer: None,
            #[cfg(feature = "test-support")]
            stop_check: None,
            prepare_host_execution: false,
            reused_coordinator: None,
            poll_failures: Vec::new(),
            worker_failures: Vec::new(),
            service_identity: None,
            draining: false,
        }
    }

    fn new_production(
        home_dir: &Path,
        build: BuildIdentity,
        runner: Arc<dyn crate::scheduler::AttemptRunner>,
    ) -> Self {
        let mut service = Self::new(home_dir, build, runner);
        service.prepare_host_execution = true;
        service
    }

    /// Serve one bounded protocol exchange.
    pub fn serve_connection(&mut self, stream: &mut UnixStream) -> Result<()> {
        let payload = frame_read(stream)?;
        let response = match serde_json::from_slice::<SocketRequest>(&payload) {
            Ok(SocketRequest::Health { generation }) if generation == PROTOCOL_GENERATION => {
                SocketResponse::HealthOk {
                    generation: PROTOCOL_GENERATION,
                    build: self.build.clone(),
                    instance_id: self
                        .service_identity
                        .as_ref()
                        .map(|m| m.instance_id.clone()),
                    state: self.service_identity.as_ref().map(|m| {
                        if self.draining {
                            ServiceState::Draining
                        } else {
                            m.state
                        }
                    }),
                }
            }
            Ok(SocketRequest::Drain {
                generation,
                instance_id,
            }) if generation == PROTOCOL_GENERATION => match self.accept_drain(&instance_id) {
                Ok(()) => SocketResponse::DrainAccepted {
                    generation,
                    instance_id,
                    state: ServiceState::Draining,
                },
                Err(error) => SocketResponse::Error {
                    generation,
                    message: error.to_string(),
                },
            },
            Ok(SocketRequest::SubmitExecution {
                generation,
                build,
                project_root,
                request,
            }) if generation == PROTOCOL_GENERATION => {
                let submission = (|| {
                    if self.draining {
                        bail!("scheduler service is draining");
                    }
                    let project = self.validate_project(&project_root, &build)?;
                    persist_request(&project, &request)?;
                    let token = submit_dispatch(&project, &request)?;
                    Ok::<_, anyhow::Error>((project, token))
                })();
                match submission {
                    Ok((project, token)) => {
                        self.stopping_projects.remove(&project);
                        SocketResponse::ExecutionSubmitted {
                            generation: PROTOCOL_GENERATION,
                            token,
                        }
                    }
                    Err(error) => SocketResponse::Error {
                        generation: PROTOCOL_GENERATION,
                        message: error.to_string(),
                    },
                }
            }
            Ok(SocketRequest::ExecutionStatus {
                generation,
                build,
                project_root,
                request_id,
            }) if generation == PROTOCOL_GENERATION => {
                match self
                    .validate_project(&project_root, &build)
                    .and_then(|project| execution_status(&project, &request_id))
                {
                    Ok(status) => SocketResponse::ExecutionStatus {
                        generation: PROTOCOL_GENERATION,
                        request_id: status.request_id,
                        work_item_id: status.work_item_id,
                        dispatch_id: status.dispatch_id,
                        status: status.status,
                        attempt_id: status.attempt_id,
                    },
                    Err(error) => SocketResponse::Error {
                        generation: PROTOCOL_GENERATION,
                        message: error.to_string(),
                    },
                }
            }
            Ok(_) => SocketResponse::Error {
                generation: PROTOCOL_GENERATION,
                message: "unsupported scheduler protocol generation".into(),
            },
            Err(error) => SocketResponse::Error {
                generation: PROTOCOL_GENERATION,
                message: format!("invalid scheduler request: {error}"),
            },
        };
        frame_write(stream, &serde_json::to_vec(&response)?)?;
        Ok(())
    }

    fn validate_project(&self, project_root: &Path, build: &BuildIdentity) -> Result<PathBuf> {
        if build != &self.build {
            bail!(
                "scheduler request build {} ({}) does not match service build {} ({})",
                build.version,
                build.hash,
                self.build.version,
                self.build.hash
            );
        }
        if let Some(desired) = read_desired_build(&self.home_dir)? {
            if desired != self.build {
                bail!(
                    "scheduler service build {} ({}) is stale; desired build is {} ({})",
                    self.build.version,
                    self.build.hash,
                    desired.version,
                    desired.hash
                );
            }
        }
        let canonical = project_root.canonicalize().with_context(|| {
            format!("canonicalize scheduler project {}", project_root.display())
        })?;
        let registry = read_registry(&self.home_dir)?;
        let expected = registry.get(&canonical).with_context(|| {
            format!(
                "scheduler project {} is not registered",
                canonical.display()
            )
        })?;
        let actual = fs::read_to_string(checkout_identity_path(&canonical))
            .with_context(|| format!("read checkout identity for {}", canonical.display()))?;
        if actual.trim() != expected.0 {
            bail!(
                "scheduler project {} has stale checkout identity",
                canonical.display()
            );
        }
        Ok(canonical)
    }

    /// Fill every registered project's configured capacity with claims from
    /// the real queue and launch those claims on service-owned workers.
    pub fn poll_once(&mut self) -> Result<()> {
        self.poll_failures.clear();
        self.failed_stop_projects.clear();
        self.reconcile_stops()?;
        if self.draining {
            return Ok(());
        }
        let mut projects = read_registry(&self.home_dir)?
            .into_keys()
            .collect::<Vec<_>>();
        projects.sort();
        for project in projects {
            if self.stopping_projects.contains(&project)
                || self.failed_stop_projects.contains(&project)
            {
                continue;
            }
            if let Err(error) = self.poll_project(&project) {
                if error.is::<crate::os::HostSandboxPreflightError>() {
                    return Err(error).with_context(|| {
                        format!("project {} prepare host execution", project.display())
                    });
                }
                let diagnostic = format!("project {} poll: {error:#}", project.display());
                eprintln!("[scheduler] {diagnostic}");
                self.poll_failures.push(diagnostic);
            }
        }
        Ok(())
    }

    /// Diagnostics from the most recent poll, including its project context.
    pub fn last_poll_failures(&self) -> &[String] {
        &self.poll_failures
    }

    fn poll_project(&mut self, project: &Path) -> Result<()> {
        if !self.coordinators.contains_key(project) {
            match crate::scheduler::start_or_reuse(&project, &self.build)
                .context("start coordinator")?
            {
                crate::scheduler::CoordinatorStart::Elected(coordinator) => {
                    if self.prepare_host_execution {
                        crate::scheduler::prepare_service_execution(&project)
                            .context("prepare execution")?;
                        self.prepared_projects.insert(project.to_path_buf());
                    }
                    self.coordinators.insert(project.to_path_buf(), coordinator);
                }
                crate::scheduler::CoordinatorStart::Reused(metadata) => {
                    self.reused_coordinator = Some(metadata);
                    return Ok(());
                }
            }
        }
        #[cfg(feature = "test-support")]
        if let Some(observer) = &self.recovery_observer {
            crate::scheduler::recover_and_reconcile_with_observer(project, observer.as_ref())
                .with_context(|| format!("recover project {}", project.display()))?;
        } else {
            crate::scheduler::recover_and_reconcile(project)
                .with_context(|| format!("recover project {}", project.display()))?;
        }
        #[cfg(not(feature = "test-support"))]
        crate::scheduler::recover_and_reconcile(project)
            .with_context(|| format!("recover project {}", project.display()))?;
        let capacity =
            crate::scheduler::configured_capacity(&project).context("read scheduler capacity")?;
        while crate::scheduler::project_local_capacity_used(project).context("count live claims")?
            < capacity
        {
            let Some((token, lease)) =
                crate::scheduler::claim_next(project).context("claim next dispatch")?
            else {
                break;
            };
            let runner = self.runner.clone();
            let worker_project = project.to_path_buf();
            let worker_token = token.clone();
            let worker = std::thread::spawn(move || {
                crate::scheduler::run_claimed_dispatch(
                    &worker_project,
                    worker_token,
                    lease,
                    runner.as_ref(),
                )
            });
            self.workers.push(ServiceWorker {
                project: project.to_path_buf(),
                token,
                handle: worker,
            });
        }
        Ok(())
    }

    fn reap_finished(&mut self) -> Result<()> {
        let mut pending = Vec::new();
        for worker in self.workers.drain(..) {
            if worker.handle.is_finished() {
                if let Err(error) = settle_worker(worker) {
                    eprintln!("[scheduler] {error:#}");
                    self.worker_failures.push(format!("{error:#}"));
                }
            } else {
                pending.push(worker);
            }
        }
        self.workers = pending;
        Ok(())
    }

    fn reconcile_stops(&mut self) -> Result<()> {
        self.reap_finished()?;
        let projects = self.coordinators.keys().cloned().collect::<Vec<_>>();
        for project in projects {
            if self.failed_stop_projects.contains(&project) {
                continue;
            }
            #[cfg(feature = "test-support")]
            if let Some(check) = &self.stop_check {
                if let Err(error) = check(&project) {
                    eprintln!(
                        "[scheduler] project {} check stop: {error:#}",
                        project.display()
                    );
                    self.failed_stop_projects.insert(project.clone());
                    continue;
                }
            }
            let stop_requested = match crate::scheduler::service_stop_requested(
                &project,
                self.coordinators
                    .get_mut(&project)
                    .expect("known coordinator"),
            ) {
                Ok(requested) => requested,
                Err(error) => {
                    eprintln!(
                        "[scheduler] project {} check stop: {error:#}",
                        project.display()
                    );
                    self.failed_stop_projects.insert(project.clone());
                    continue;
                }
            };
            let has_workers = self.workers.iter().any(|worker| worker.project == project);
            if stop_requested {
                self.stopping_projects.insert(project.clone());
            }
            if self.stopping_projects.contains(&project) && !has_workers {
                if self.prepared_projects.contains(&project) {
                    if let Err(error) = crate::scheduler::finish_service_execution(&project) {
                        eprintln!(
                            "[scheduler] project {} finish execution: {error:#}",
                            project.display()
                        );
                        self.failed_stop_projects.insert(project.clone());
                        continue;
                    }
                    self.prepared_projects.remove(&project);
                }
                self.coordinators.remove(&project);
            }
        }
        Ok(())
    }

    fn tick(
        &mut self,
        now: Instant,
        next_poll: &mut Instant,
        poll_interval: Duration,
    ) -> Result<()> {
        if now >= *next_poll {
            self.poll_once()?;
            *next_poll = now + poll_interval;
        } else {
            self.reconcile_stops()?;
        }
        Ok(())
    }

    /// Join all current workers. Integration tests use this to observe the
    /// durable terminal dispatch after a deterministic fake runner returns.
    pub fn join_workers(&mut self) -> Result<()> {
        let mut failures = Vec::new();
        for worker in self.workers.drain(..) {
            if let Err(error) = settle_worker(worker) {
                failures.push(format!("{error:#}"));
            }
        }
        if !failures.is_empty() {
            bail!("{}", failures.join("; "));
        }
        Ok(())
    }

    fn finish_prepared_projects(&mut self) {
        for project in self.prepared_projects.drain() {
            if let Err(error) = crate::scheduler::finish_service_execution(&project) {
                self.worker_failures.push(format!(
                    "project {} finish execution: {error:#}",
                    project.display()
                ));
            }
        }
    }

    #[cfg(feature = "test-support")]
    fn take_reused_coordinator(&mut self) -> Option<crate::scheduler::CoordinatorMetadata> {
        self.reused_coordinator.take()
    }

    #[cfg(feature = "test-support")]
    pub fn set_recovery_observer(
        &mut self,
        observer: Arc<dyn Fn(&Path, &crate::queue::Dispatch) + Send + Sync>,
    ) {
        self.recovery_observer = Some(observer);
    }

    #[cfg(feature = "test-support")]
    pub fn set_stop_check(&mut self, check: Arc<dyn Fn(&Path) -> Result<()> + Send + Sync>) {
        self.stop_check = Some(check);
    }

    #[cfg(feature = "test-support")]
    pub fn tick_for_test(
        &mut self,
        now: Instant,
        next_poll: &mut Instant,
        poll_interval: Duration,
    ) -> Result<()> {
        self.tick(now, next_poll, poll_interval)
    }

    #[cfg(feature = "test-support")]
    pub fn worker_count_for_test(&self) -> usize {
        self.workers.len()
    }

    #[cfg(feature = "test-support")]
    pub fn worker_failures_for_test(&self) -> &[String] {
        &self.worker_failures
    }

    #[cfg(feature = "test-support")]
    pub fn set_service_identity_for_test(&mut self, instance_id: &str) {
        self.service_identity = Some(ServiceMetadata {
            schema: SERVICE_METADATA_SCHEMA,
            instance_id: instance_id.to_owned(),
            state: ServiceState::Running,
            build: self.build.clone(),
        });
    }

    #[cfg(feature = "test-support")]
    fn owns_coordinator(&self) -> bool {
        !self.coordinators.is_empty()
    }
}

fn settle_worker(worker: ServiceWorker) -> Result<()> {
    let context = format!(
        "project {} work item {} dispatch {} generation {} attempt {} worker",
        worker.project.display(),
        worker.token.work_item_id,
        worker.token.dispatch_id,
        worker.token.generation,
        worker.token.bound_attempt_id,
    );
    worker
        .handle
        .join()
        .map_err(|_| anyhow::anyhow!("{context} panicked"))?
        .with_context(|| format!("{context} failed"))
}

impl Drop for SchedulerService {
    fn drop(&mut self) {
        if let Err(error) = self.join_workers() {
            eprintln!("[scheduler] shutdown workers: {error:#}");
        }
        for project in self.prepared_projects.drain() {
            let _ = crate::scheduler::finish_service_execution(&project);
        }
    }
}

/// Serve the production scheduler socket until the owning process exits.
/// The scheduler command runs this listener alongside its queue coordinator.
pub fn run_service(home_dir: &Path, build: BuildIdentity, poll_seconds: u64) -> Result<()> {
    run_service_with_runner(
        home_dir,
        build,
        poll_seconds,
        Arc::new(crate::scheduler::CliAttemptRunner),
        true,
        false,
    )
}

/// Run the production listener with an injected external Attempt runner in
/// host tests. The registry, poll loop, claims, and worker settlement are shared.
#[cfg(feature = "test-support")]
pub fn run_service_with_test_runner(
    home_dir: &Path,
    build: BuildIdentity,
    poll_seconds: u64,
    runner: Arc<dyn crate::scheduler::AttemptRunner>,
) -> Result<()> {
    run_service_with_runner(home_dir, build, poll_seconds, runner, false, false)
}

#[cfg(feature = "test-support")]
pub fn run_service_with_socket_test_runner(
    home_dir: &Path,
    build: BuildIdentity,
    poll_seconds: u64,
    runner: Arc<dyn crate::scheduler::AttemptRunner>,
) -> Result<()> {
    run_service_with_runner(home_dir, build, poll_seconds, runner, false, true)
}

fn run_service_with_runner(
    home_dir: &Path,
    build: BuildIdentity,
    poll_seconds: u64,
    runner: Arc<dyn crate::scheduler::AttemptRunner>,
    prepare_host_execution: bool,
    force_socket: bool,
) -> Result<()> {
    if SERVICE_SHUTDOWN.load(Ordering::Acquire) {
        return Ok(());
    }
    #[cfg(feature = "test-support")]
    if !force_socket && std::env::var_os("FLUENT_TEST_HOST_SANDBOX_PREFLIGHT").is_some() {
        return run_transportless_service(
            home_dir,
            build,
            poll_seconds,
            runner,
            prepare_host_execution,
        );
    }
    let path = socket_path(home_dir);
    if let Some(health) = inspect_service(home_dir, &build)? {
        eprintln!(
            "[scheduler] reusing live service {} running {} ({})",
            health.instance_id.as_deref().unwrap_or("unknown"),
            health.build.version,
            health.build.hash
        );
        return Ok(());
    }
    let lease = loop {
        if let Some(lease) = try_service_lease(home_dir, &build)? {
            break lease;
        }
        if let Some(health) = inspect_service(home_dir, &build)? {
            eprintln!(
                "[scheduler] reusing live service {} running {} ({})",
                health.instance_id.as_deref().unwrap_or("unknown"),
                health.build.version,
                health.build.hash
            );
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    // A reachable socket without this lease is a legacy or unverified owner.
    match UnixStream::connect(&path) {
        Ok(_) => bail!(
            "scheduler socket became reachable during election; stop its owning process manually"
        ),
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
            ) => {}
        Err(error) => return Err(error).context("inspect socket after service election"),
    }
    publish_service_metadata(home_dir, &lease.metadata)?;
    if path.exists() {
        fs::remove_file(&path).context("remove inactive service socket")?;
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
        #[cfg(unix)]
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
    }
    let listener = UnixListener::bind(&path)
        .with_context(|| format!("bind scheduler service {}", path.display()))?;
    let bound_inode = fs::metadata(&path)?.ino();
    listener.set_nonblocking(true)?;
    #[cfg(unix)]
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    let mut service = SchedulerService::new_production(home_dir, build, runner);
    service.service_identity = Some(lease.metadata.clone());
    service.prepare_host_execution = prepare_host_execution;
    let poll_interval = Duration::from_secs(poll_seconds.max(1));
    let mut next_poll = Instant::now();
    loop {
        if SERVICE_SHUTDOWN.load(Ordering::Acquire) {
            service.accept_drain(&lease.metadata.instance_id)?;
        }
        service.read_drain_request()?;
        match listener.accept() {
            Ok((mut stream, _)) => {
                configure_accepted_stream(&stream)?;
                let _ = service.serve_connection(&mut stream);
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
            Err(error) => return Err(error).context("accept scheduler service connection"),
        }
        service.tick(Instant::now(), &mut next_poll, poll_interval)?;
        if service.draining && service.workers.is_empty() {
            #[cfg(feature = "test-support")]
            if let Some(marker) = std::env::var_os("FLUENT_SCHEDULER_TEST_PAUSE_AFTER_SETTLEMENT") {
                fs::write(marker, b"settled")?;
                loop {
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
            service.finish_prepared_projects();
            let result = ServiceStopRecord {
                schema: SERVICE_METADATA_SCHEMA,
                instance_id: lease.metadata.instance_id.clone(),
                build: lease.metadata.build.clone(),
                failures: service.worker_failures.clone(),
            };
            write_stop_record(&service_result_path(home_dir, &result.instance_id), &result)?;
            #[cfg(feature = "test-support")]
            if let Some(marker) = std::env::var_os("FLUENT_SCHEDULER_TEST_PAUSE_AFTER_RESULT") {
                fs::write(marker, b"published")?;
                loop {
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
            drop(listener);
            if fs::metadata(&path).is_ok_and(|m| m.ino() == bound_inode) {
                fs::remove_file(&path)?;
            }
            drop(service);
            drop(lease);
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Run the production service coordinator without its Unix socket when the
/// configured test-support sandbox explicitly controls host preflight. The
/// outer Tester sandbox forbids `bind(2)`, while the scheduler behavior under
/// test needs the real registry, host gate, claims, capacity, workers, stop
/// requests, and durable queue reconciliation.
#[cfg(feature = "test-support")]
fn run_transportless_service(
    home_dir: &Path,
    build: BuildIdentity,
    poll_seconds: u64,
    runner: Arc<dyn crate::scheduler::AttemptRunner>,
    prepare_host_execution: bool,
) -> Result<()> {
    let mut service = SchedulerService::new_production(home_dir, build, runner);
    service.prepare_host_execution = prepare_host_execution;
    let poll_interval = Duration::from_secs(poll_seconds.max(1));
    let mut next_poll = Instant::now();
    let mut owned_coordinator = false;
    loop {
        if SERVICE_SHUTDOWN.load(Ordering::Acquire) {
            return service.join_workers().context("shutdown scheduler workers");
        }
        service.tick(Instant::now(), &mut next_poll, poll_interval)?;
        if owned_coordinator && !service.owns_coordinator() {
            return Ok(());
        }
        if let Some(metadata) = service.take_reused_coordinator() {
            eprintln!(
                "[scheduler] reusing live coordinator {} running {} ({})",
                metadata.instance_id, metadata.build.version, metadata.build.hash
            );
            return Ok(());
        }
        owned_coordinator |= service.owns_coordinator();
        std::thread::sleep(Duration::from_millis(50));
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

/// Drain the current shared owner and wait until every owned worker settles.
pub fn stop_service(home: &Path) -> Result<ServiceStopRecord> {
    let health = match send_health_request(&socket_path(home)) {
        Ok(health) => health,
        Err(error) if !socket_path(home).exists() => {
            if let Some(request) = read_stop_record(&service_stop_path(home))? {
                if lock_identity(home)?.as_str() != request.instance_id && service_lease_live(home)?
                {
                    bail!(
                        "a replacement scheduler service is starting; retry stop after its health becomes available"
                    );
                }
                return stop_service_instance(home, &request.instance_id);
            }
            return Err(error).context("shared scheduler is not running");
        }
        Err(error) => return Err(error).context("inspect shared scheduler before drain"),
    };
    let verified = inspect_service_with_state(home, &health.build, true)?
        .context("shared scheduler is not running")?;
    stop_service_instance(
        home,
        verified
            .instance_id
            .as_deref()
            .context("missing service instance")?,
    )
}

/// Replay a drain for exactly one service instance, including after its exit.
pub fn stop_service_instance(home: &Path, instance_id: &str) -> Result<ServiceStopRecord> {
    if instance_id.len() != 32 || !instance_id.bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!("invalid scheduler service instance identity");
    }
    if let Some(result) = read_stop_record(&service_result_path(home, instance_id))? {
        if result.instance_id == instance_id {
            return wait_for_stop_result(home, instance_id);
        }
    }
    let health = match send_health_request(&socket_path(home)) {
        Ok(health) => health,
        Err(error) => {
            if read_stop_record(&service_instance_stop_path(home, instance_id))?.is_some()
                && lock_identity(home)?.as_str() == instance_id
                && service_lease_live(home)?
            {
                return wait_for_stop_result(home, instance_id);
            }
            return Err(error).context("inspect shared scheduler before drain");
        }
    };
    let observed = inspect_service_with_state(home, &health.build, true)?
        .context("shared scheduler is not running")?;
    if observed.instance_id.as_deref() != Some(instance_id) {
        bail!("service instance changed; stale drain request for {instance_id} rejected");
    }
    let record = ServiceStopRecord {
        schema: SERVICE_METADATA_SCHEMA,
        instance_id: instance_id.to_owned(),
        build: observed.build,
        failures: Vec::new(),
    };
    write_stop_record(&service_instance_stop_path(home, instance_id), &record)?;
    write_stop_record(&service_stop_path(home), &record)?;
    // Revalidate after publication. A replacement never accepts the old id.
    if let Ok(current) = send_health_request(&socket_path(home)) {
        if current.instance_id.as_deref() != Some(instance_id) {
            bail!("service instance changed while requesting drain");
        }
        let _ = exchange(
            &socket_path(home),
            &SocketRequest::Drain {
                generation: PROTOCOL_GENERATION,
                instance_id: instance_id.to_owned(),
            },
        );
    }
    wait_for_stop_result(home, instance_id)
}

fn wait_for_stop_result(home: &Path, instance_id: &str) -> Result<ServiceStopRecord> {
    wait_for_stop_result_with_interleave(home, instance_id, || Ok(()))
}

fn wait_for_stop_result_with_interleave(
    home: &Path,
    instance_id: &str,
    mut after_result_read: impl FnMut() -> Result<()>,
) -> Result<ServiceStopRecord> {
    loop {
        if let Some(result) = released_stop_result(home, instance_id)? {
            return settled_stop_result(result);
        }
        after_result_read()?;
        let exit_error = if let Ok(current) = send_health_request(&socket_path(home)) {
            if current.instance_id.as_deref() != Some(instance_id) {
                Some(format!(
                    "service instance changed while draining; {} is now running",
                    current.instance_id.unwrap_or_default()
                ))
            } else {
                None
            }
        } else if !service_lease_live(home)? || lock_identity(home)?.as_str() != instance_id {
            Some(format!(
                "scheduler service {instance_id} exited without a settled drain result; inspect its work before replacement"
            ))
        } else {
            None
        };
        if let Some(error) = exit_error {
            // The owner can publish its result and release its lease between
            // the first result read and the liveness check.
            if let Some(result) = released_stop_result(home, instance_id)? {
                return settled_stop_result(result);
            }
            bail!("{error}");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(test)]
mod stop_result_tests {
    use super::*;

    #[test]
    fn rejects_published_result_when_lease_identity_is_unreadable() {
        let home = tempfile::tempdir().unwrap();
        let instance_id = "0123456789abcdef0123456789abcdef";
        fs::create_dir_all(service_state_root(home.path())).unwrap();
        let result = ServiceStopRecord {
            schema: SERVICE_METADATA_SCHEMA,
            instance_id: instance_id.to_owned(),
            build: BuildIdentity {
                version: "test".to_owned(),
                hash: "test".to_owned(),
            },
            failures: Vec::new(),
        };
        write_stop_record(&service_result_path(home.path(), instance_id), &result).unwrap();
        fs::create_dir(service_lock_path(home.path())).unwrap();

        assert!(released_stop_result(home.path(), instance_id).is_err());
        assert!(stop_service_instance(home.path(), instance_id).is_err());
    }

    #[test]
    fn accepts_result_published_before_owner_exit_check() {
        let home = tempfile::tempdir().unwrap();
        let instance_id = "0123456789abcdef0123456789abcdef";
        fs::create_dir_all(service_state_root(home.path())).unwrap();
        fs::write(service_lock_path(home.path()), instance_id).unwrap();
        let result = ServiceStopRecord {
            schema: SERVICE_METADATA_SCHEMA,
            instance_id: instance_id.to_owned(),
            build: BuildIdentity {
                version: "test".to_owned(),
                hash: "test".to_owned(),
            },
            failures: Vec::new(),
        };
        let observed = wait_for_stop_result_with_interleave(home.path(), instance_id, || {
            write_stop_record(&service_result_path(home.path(), instance_id), &result)
        })
        .unwrap();
        assert_eq!(observed.instance_id, instance_id);
    }
}

fn released_stop_result(home: &Path, instance_id: &str) -> Result<Option<ServiceStopRecord>> {
    let Some(result) = read_stop_record(&service_result_path(home, instance_id))? else {
        return Ok(None);
    };
    if result.instance_id == instance_id && stop_result_released(home, instance_id)? {
        Ok(Some(result))
    } else {
        Ok(None)
    }
}

fn settled_stop_result(result: ServiceStopRecord) -> Result<ServiceStopRecord> {
    if !result.failures.is_empty() {
        bail!(
            "scheduler service {} drained with worker failures: {}",
            result.instance_id,
            result.failures.join("; ")
        );
    }
    Ok(result)
}

fn stop_result_released(home: &Path, instance_id: &str) -> Result<bool> {
    if lock_identity(home)? != instance_id {
        return Ok(true);
    }
    Ok(!service_lease_live(home)?)
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
    if let Some(health) = inspect_service(home_dir, build)? {
        return Ok(ServiceStartOutcome::Reused(health));
    }
    manager.start()?;
    wait_for_health(&sock, std::time::Duration::from_secs(5))?;
    let health =
        inspect_service(home_dir, build)?.context("new service disappeared after health")?;
    record_observed_build(home_dir, &health.build)?;
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
/// twice with identical content succeeds. Rejects a write that would change
/// any field of an already-persisted request.
pub fn persist_request(project_root: &Path, request: &AttemptExecutionRequest) -> Result<()> {
    let path = request_path(project_root, &request.id);
    let _lock = acquire_request_lock(project_root, &request.id)?;
    if path.exists() {
        let bytes =
            fs::read(&path).with_context(|| format!("read existing request {}", path.display()))?;
        let existing: AttemptExecutionRequest = serde_json::from_slice(&bytes)
            .with_context(|| format!("parse existing request {}", path.display()))?;
        if existing == *request {
            return Ok(());
        }
        bail!(
            "request {} is already persisted with different content",
            request.id
        );
    }
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
    /// Identity of the real durable queue dispatch admitted for this request.
    pub dispatch_id: String,
    /// True when an identical request replay found its existing dispatch.
    #[serde(default)]
    pub already_queued: bool,
}

fn dispatch_token_path(project_root: &Path, request_id: &str) -> PathBuf {
    dispatch_tokens_dir(project_root).join(format!("{request_id}.json"))
}

/// Submit an `AttemptExecutionRequest` through an exact-bound idempotent
/// dispatch boundary. The same request id always returns the same token.
/// The request must be persisted before calling this function, and the
/// submitted request's id, work_item_id, and attempt_id must exactly match
/// the persisted record. Any existing token is also verified against the
/// persisted request before being returned.
pub fn submit_dispatch(
    project_root: &Path,
    request: &AttemptExecutionRequest,
) -> Result<DispatchToken> {
    let _lock = acquire_dispatch_lock(project_root, &request.id)?;

    // Load and verify the persisted request.
    let persisted_path = request_path(project_root, &request.id);
    if !persisted_path.exists() {
        bail!(
            "request {} must be persisted before dispatch; {} not found",
            request.id,
            persisted_path.display()
        );
    }
    let persisted_bytes = fs::read(&persisted_path)
        .with_context(|| format!("read persisted request {}", persisted_path.display()))?;
    let persisted: AttemptExecutionRequest = serde_json::from_slice(&persisted_bytes)
        .with_context(|| format!("parse persisted request {}", persisted_path.display()))?;

    // Submitted request must match the persisted record on all identity fields.
    if request.id != persisted.id
        || request.work_item_id != persisted.work_item_id
        || request.attempt_id != persisted.attempt_id
    {
        bail!(
            "submitted request does not match persisted request {} \
             (work_item_id or attempt_id mismatch)",
            persisted.id
        );
    }

    let token_path = dispatch_token_path(project_root, &request.id);
    if token_path.exists() {
        let bytes = fs::read(&token_path)
            .with_context(|| format!("read dispatch token {}", token_path.display()))?;
        let token: DispatchToken = serde_json::from_slice(&bytes)
            .with_context(|| format!("parse dispatch token {}", token_path.display()))?;
        // Existing token must be exactly bound to the persisted request.
        if token.request_id != persisted.id
            || token.work_item_id != persisted.work_item_id
            || token.attempt_id != persisted.attempt_id
            || token.dispatch_id.is_empty()
        {
            bail!(
                "existing dispatch token does not match persisted request {} \
                 (token identity mismatch)",
                persisted.id
            );
        }
        let ledger = crate::queue::read_ledger(project_root, &persisted.work_item_id)?
            .context("real queue ledger is missing for persisted scheduler request")?;
        if !ledger
            .dispatches
            .iter()
            .any(|dispatch| dispatch.dispatch_id == token.dispatch_id)
        {
            bail!(
                "real queue dispatch {} is missing for scheduler request {}",
                token.dispatch_id,
                persisted.id
            );
        }
        return Ok(DispatchToken {
            already_queued: true,
            ..token
        });
    }

    crate::queue::add(project_root, &persisted.work_item_id, None)?;
    let dispatch = crate::queue::show(project_root, &persisted.work_item_id)?;
    let token = DispatchToken {
        request_id: persisted.id.clone(),
        work_item_id: persisted.work_item_id.clone(),
        attempt_id: persisted.attempt_id.clone(),
        dispatch_id: dispatch.dispatch_id,
        already_queued: false,
    };
    if let Some(parent) = token_path.parent() {
        fs::create_dir_all(parent)?;
    }
    atomic_write(&token_path, &serde_json::to_vec_pretty(&token)?)?;
    Ok(token)
}

fn execution_status(project_root: &Path, request_id: &str) -> Result<ExecutionStatus> {
    let request = load_request(project_root, request_id)?;
    let token_path = dispatch_token_path(project_root, request_id);
    let token: DispatchToken = serde_json::from_slice(
        &fs::read(&token_path)
            .with_context(|| format!("read dispatch token {}", token_path.display()))?,
    )
    .with_context(|| format!("parse dispatch token {}", token_path.display()))?;
    let ledger = crate::queue::read_ledger(project_root, &request.work_item_id)?
        .context("real queue ledger is missing for scheduler request")?;
    let dispatch = ledger
        .dispatches
        .iter()
        .find(|dispatch| dispatch.dispatch_id == token.dispatch_id)
        .with_context(|| {
            format!(
                "real queue dispatch {} is missing for scheduler request {request_id}",
                token.dispatch_id
            )
        })?;
    Ok(ExecutionStatus {
        request_id: request.id,
        work_item_id: request.work_item_id,
        dispatch_id: dispatch.dispatch_id.clone(),
        status: dispatch.status.as_str().into(),
        attempt_id: dispatch.bound_attempt_id.clone(),
    })
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

pub(crate) fn generate_random_hex(n_bytes: usize) -> Result<String> {
    let mut buf = vec![0u8; n_bytes];
    let mut f = fs::File::open("/dev/urandom").context("open /dev/urandom")?;
    f.read_exact(&mut buf).context("read random bytes")?;
    Ok(buf.iter().map(|b| format!("{b:02x}")).collect())
}
