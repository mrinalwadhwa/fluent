//! Durable, local-only Slack collaboration primitives.
//!
//! This module deliberately keeps Slack and provider protocol details at the
//! edge. The coordinator accepts durable envelopes, makes pure binding
//! decisions, and records stable outbound deliveries before an adapter sends
//! them.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::atomic_write::atomic_write;

mod adapters;
mod coordination;
mod domain;
mod github_review;
mod interfaces;
mod policy;
mod project_enablement;
mod store;
mod work_control;

pub use adapters::*;
pub use coordination::*;
pub use domain::*;
pub use github_review::*;
pub use interfaces::*;
pub use project_enablement::*;
use store::*;
pub use store::{FileCollaborationStore, ThreadCommitCrashPoint, config_path, state_root};
pub use work_control::*;

/// Slack app manifest for a local Socket Mode installation. It intentionally
/// has no request URL because the runner never accepts public HTTP ingress.
pub const SOCKET_MODE_MANIFEST: &str = r#"display_information:
  name: Fluent
features:
  bot_user:
    display_name: Fluent
    always_online: false
settings:
  socket_mode_enabled: true
  interactivity:
    is_enabled: true
  token_rotation_enabled: false
  event_subscriptions:
    bot_events:
      - app_mention
      - message.channels
      - message.im
oauth_config:
  scopes:
    bot:
      - app_mentions:read
      - channels:history
      - im:history
      - chat:write
"#;

/// Names used for the two Slack secrets in the macOS Keychain. Token values
/// are never represented in runner configuration or diagnostic structures.
pub const KEYCHAIN_SERVICE: &str = "dev.fluent.slack";
pub const APP_TOKEN_ACCOUNT: &str = "app-token";
pub const BOT_TOKEN_ACCOUNT: &str = "bot-token";

/// Host-owned secret boundary. Implement this with the macOS Keychain; tests
/// can use an in-memory implementation without ever handling diagnostics.
pub trait SecretStore {
    fn store(&mut self, service: &str, account: &str, secret: &str) -> Result<()>;
    fn read(&self, service: &str, account: &str) -> Result<String>;
}

/// Stores credentials in the runner owner's login Keychain.
pub struct KeychainSecretStore {
    executable: PathBuf,
}

impl Default for KeychainSecretStore {
    fn default() -> Self {
        Self {
            executable: PathBuf::from("security"),
        }
    }
}

impl KeychainSecretStore {
    #[doc(hidden)]
    pub fn with_executable(executable: impl Into<PathBuf>) -> Self {
        Self {
            executable: executable.into(),
        }
    }
}

impl SecretStore for KeychainSecretStore {
    fn store(&mut self, service: &str, account: &str, secret: &str) -> Result<()> {
        let output = Command::new(&self.executable)
            .args([
                "add-generic-password",
                "-U",
                "-a",
                account,
                "-s",
                service,
                "-w",
                secret,
            ])
            .output()
            .context("invoke macOS Keychain")?;
        if !output.status.success() {
            bail!(
                "macOS Keychain rejected Slack credential: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(())
    }
    fn read(&self, service: &str, account: &str) -> Result<String> {
        let output = Command::new(&self.executable)
            .args(["find-generic-password", "-a", account, "-s", service, "-w"])
            .output()
            .context("read macOS Keychain")?;
        if !output.status.success() {
            bail!(
                "macOS Keychain has no Slack credential: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(String::from_utf8(output.stdout)?.trim().to_owned())
    }
}

/// Store Slack credentials exclusively at the host secret boundary.
pub fn store_slack_credentials<S: SecretStore>(
    secrets: &mut S,
    app_token: &str,
    bot_token: &str,
) -> Result<()> {
    if !app_token.starts_with("xapp-") || !bot_token.starts_with("xoxb-") {
        bail!("Slack credentials have an unexpected token type");
    }
    secrets.store(KEYCHAIN_SERVICE, APP_TOKEN_ACCOUNT, app_token)?;
    secrets.store(KEYCHAIN_SERVICE, BOT_TOKEN_ACCOUNT, bot_token)
}

/// Remove token-shaped strings before writing a diagnostic or Slack message.
pub use crate::conversation::redact_secrets;

/// Report a host action that the current sandbox cannot perform. This is not a
/// Slack authentication failure: the owner must run the exact command outside
/// the enclosing sandbox.
pub fn host_handoff(command: &str) -> String {
    format!("Host access is required. Run outside the agent sandbox: {command}")
}

/// Preserve permission-denial classification at owner-only boundaries while
/// leaving ordinary Slack authentication and configuration failures intact.
pub fn classify_host_boundary(error: anyhow::Error, command: &str) -> anyhow::Error {
    let diagnostic = format!("{error:#}");
    let lowered = diagnostic.to_ascii_lowercase();
    if lowered.contains("operation not permitted")
        || lowered.contains("permission denied")
        || lowered.contains("sandbox")
    {
        anyhow::anyhow!(
            "{}\nCause: {}",
            host_handoff(command),
            redact_secrets(&diagnostic)
        )
    } else {
        error
    }
}

/// Background-service boundary for the per-user LaunchAgent. A production
/// adapter owns `launchctl`; the coordinator never shells out to it directly.
pub trait ServiceManager {
    fn install(&mut self, executable: &Path) -> Result<()>;
    fn start(&mut self) -> Result<()>;
    fn restart(&mut self) -> Result<()>;
    fn stop(&mut self) -> Result<()>;
    fn enabled(&self) -> bool;
}

impl<T: ServiceManager> HostActionExecutor for T {
    fn execute_host_action(&mut self, action: &HostAction) -> Result<()> {
        if action.artifact_revision.is_empty()
            || action.actor.is_empty()
            || action.idempotency_identity.is_empty()
        {
            bail!("host action identity is incomplete");
        }
        match &action.action {
            HostActionKind::Enable { executable } => {
                self.install(executable)?;
                self.start()
            }
            HostActionKind::Restart => self.restart(),
            HostActionKind::Stop => self.stop(),
        }
    }
}

/// Manages the owner-scoped LaunchAgent used by the local runner.
pub struct LaunchAgentServiceManager {
    pub label: String,
    pub plist: PathBuf,
}
impl LaunchAgentServiceManager {
    pub fn new(home: &Path) -> Self {
        Self {
            label: "dev.fluent.slack-runner".into(),
            plist: home.join("Library/LaunchAgents/dev.fluent.slack-runner.plist"),
        }
    }
    fn domain(&self) -> String {
        format!("gui/{}", unsafe { libc::getuid() })
    }
    fn run(&self, verb: &str) -> Result<()> {
        let output = Command::new("launchctl")
            .args([verb, &self.domain(), &self.plist.display().to_string()])
            .output()?;
        if !output.status.success() {
            bail!(
                "launchctl {verb} failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(())
    }

    pub fn program_arguments(executable: &Path) -> Vec<String> {
        vec![
            executable.display().to_string(),
            "slack".into(),
            "run".into(),
        ]
    }
}
impl ServiceManager for LaunchAgentServiceManager {
    fn install(&mut self, executable: &Path) -> Result<()> {
        let parent = self
            .plist
            .parent()
            .context("LaunchAgent path has no parent")?;
        fs::create_dir_all(parent)?;
        let arguments = Self::program_arguments(executable);
        let xml = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?><plist version=\"1.0\"><dict><key>Label</key><string>{}</string><key>ProgramArguments</key><array><string>{}</string><string>{}</string><string>{}</string></array><key>RunAtLoad</key><true/><key>KeepAlive</key><true/></dict></plist>",
            self.label, arguments[0], arguments[1], arguments[2]
        );
        atomic_write(&self.plist, xml.as_bytes())?;
        Ok(())
    }
    fn start(&mut self) -> Result<()> {
        self.run("bootstrap")
    }
    fn restart(&mut self) -> Result<()> {
        self.stop()?;
        self.start()
    }
    fn stop(&mut self) -> Result<()> {
        let output = Command::new("launchctl")
            .args(["bootout", &self.domain(), &self.plist.display().to_string()])
            .output()?;
        if !output.status.success() {
            bail!(
                "launchctl bootout failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(())
    }
    fn enabled(&self) -> bool {
        self.plist.exists()
    }
}

/// Deterministic service-manager double for lifecycle tests.
#[derive(Debug, Default)]
pub struct FakeServiceManager {
    pub installed: bool,
    pub running: bool,
    pub executable: Option<PathBuf>,
}
impl ServiceManager for FakeServiceManager {
    fn install(&mut self, executable: &Path) -> Result<()> {
        self.installed = true;
        self.executable = Some(executable.into());
        Ok(())
    }
    fn start(&mut self) -> Result<()> {
        if !self.installed {
            bail!("Slack runner is not installed");
        }
        self.running = true;
        Ok(())
    }
    fn restart(&mut self) -> Result<()> {
        self.stop()?;
        self.start()
    }
    fn stop(&mut self) -> Result<()> {
        self.running = false;
        Ok(())
    }
    fn enabled(&self) -> bool {
        self.installed
    }
}

/// Install the exact current executable, then start it under the owner account.
pub fn enable_runner(manager: &mut impl ServiceManager, executable: &Path) -> Result<()> {
    manager.execute_host_action(&HostAction {
        action: HostActionKind::Enable {
            executable: executable.to_owned(),
        },
        artifact_revision: crate::version::version_string(),
        actor: "runner-owner".into(),
        idempotency_identity: format!("enable:{}", executable.display()),
    })
}

/// Apply an owner-requested service lifecycle action.
pub fn control_runner(manager: &mut impl ServiceManager, action: RunnerControl) -> Result<()> {
    let action_name = match action {
        RunnerControl::Restart => "restart",
        RunnerControl::Stop => "stop",
    };
    manager.execute_host_action(&HostAction {
        action: match action {
            RunnerControl::Restart => HostActionKind::Restart,
            RunnerControl::Stop => HostActionKind::Stop,
        },
        artifact_revision: crate::version::version_string(),
        actor: "runner-owner".into(),
        idempotency_identity: format!("{action_name}:dev.fluent.slack-runner"),
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunnerControl {
    Restart,
    Stop,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunnerStatus {
    pub workspace: String,
    pub connection: String,
    pub last_successful_connection: Option<String>,
    pub build: String,
    pub failure: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunnerOperationalState {
    pub connected: bool,
    pub last_successful_connection: Option<String>,
    pub failure: Option<String>,
}

pub fn read_runner_operational_state(home: &Path) -> Result<RunnerOperationalState> {
    read_json(operational_status_path(home))
}

fn save_runner_operational_state(home: &Path, state: &RunnerOperationalState) -> Result<()> {
    ensure_root(home)?;
    atomic_write(
        &operational_status_path(home),
        &serde_json::to_vec_pretty(state)?,
    )?;
    Ok(())
}

#[doc(hidden)]
pub fn record_runner_failure(home: &Path, error: &anyhow::Error) -> Result<()> {
    let previous = read_runner_operational_state(home).unwrap_or_default();
    save_runner_operational_state(
        home,
        &RunnerOperationalState {
            connected: false,
            last_successful_connection: previous.last_successful_connection,
            failure: Some(redact_secrets(&error.to_string())),
        },
    )
}

impl std::fmt::Display for RunnerStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "workspace: {}; connection: {}; build: {}",
            self.workspace, self.connection, self.build
        )?;
        if let Some(last) = &self.last_successful_connection {
            write!(f, "; last successful connection: {last}")?;
        }
        if let Some(failure) = &self.failure {
            write!(f, "; failure: {}", redact_secrets(failure))?;
        }
        Ok(())
    }
}

pub fn runner_status(
    config: &RunnerConfig,
    connected: bool,
    last_connection: Option<String>,
    failure: Option<String>,
) -> RunnerStatus {
    RunnerStatus {
        workspace: config.workspace.clone(),
        connection: if connected {
            "connected"
        } else {
            "disconnected"
        }
        .into(),
        last_successful_connection: last_connection,
        build: crate::version::version_string(),
        failure: failure.map(|text| redact_secrets(&text)),
    }
}

#[derive(Debug, Default)]
pub struct InMemoryProvider {
    pub calls: Vec<(Option<String>, PathBuf, String, String)>,
    completed_actions: BTreeMap<String, (String, String)>,
}
impl ConversationProvider for InMemoryProvider {
    fn name(&self) -> &str {
        "codex"
    }
    fn select(&mut self, provider: &str) -> ProviderResult<()> {
        if matches!(provider, "codex" | "claude") {
            Ok(())
        } else {
            Err(ProviderError::new(
                ProviderFailure::Unavailable,
                "provider unavailable",
            ))
        }
    }
    fn start(
        &mut self,
        action_id: &str,
        scope: &ProviderScope,
        contribution: &str,
        actor: &str,
    ) -> ProviderResult<(String, String)> {
        if let Some((session, reply)) = self.completed_actions.get(action_id) {
            return Ok((session.clone(), reply.clone()));
        }
        self.calls.push((
            None,
            scope.checkout.clone(),
            contribution.into(),
            actor.into(),
        ));
        let result = (
            format!("session-{}", self.calls.len()),
            format!("Fluent received: {contribution}"),
        );
        self.completed_actions
            .insert(action_id.into(), result.clone());
        Ok(result)
    }
    fn resume(
        &mut self,
        action_id: &str,
        session: &str,
        scope: &ProviderScope,
        contribution: &str,
        actor: &str,
    ) -> ProviderResult<String> {
        if let Some((_, reply)) = self.completed_actions.get(action_id) {
            return Ok(reply.clone());
        }
        self.calls.push((
            Some(session.into()),
            scope.checkout.clone(),
            contribution.into(),
            actor.into(),
        ));
        let reply = format!("Fluent received: {contribution}");
        self.completed_actions
            .insert(action_id.into(), (session.into(), reply.clone()));
        Ok(reply)
    }
}

#[derive(Debug, Default)]
pub struct InMemorySlack {
    pub acknowledgements: Vec<String>,
    pub deliveries: Vec<OutboundDelivery>,
    pub private_responses: Vec<(String, String, String)>,
}
impl SlackTransport for InMemorySlack {
    fn acknowledge(&mut self, event_id: &str) -> Result<()> {
        self.acknowledgements.push(event_id.into());
        Ok(())
    }
    fn post(&mut self, delivery: &OutboundDelivery) -> Result<()> {
        self.deliveries.push(delivery.clone());
        Ok(())
    }
    fn respond_private(&mut self, channel: &str, user: &str, text: &str) -> Result<()> {
        self.private_responses
            .push((channel.into(), user.into(), text.into()));
        Ok(())
    }
    fn receive_timeout(
        &mut self,
        _timeout: std::time::Duration,
    ) -> Result<Option<InboundEnvelope>> {
        Ok(None)
    }
}

pub fn safe_identity(project: &ConfiguredProject) -> String {
    format!(
        "Project: {}; checkout: {}; branch: {}; commit: {}",
        project.label, project.checkout.label, project.checkout.branch, project.checkout.commit
    )
}

/// Derive an opaque identity for this checkout and Git directory instance.
pub fn checkout_identity(path: &Path) -> Result<String> {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::MetadataExt;

    let checkout = path
        .canonicalize()
        .with_context(|| format!("canonicalize checkout {}", path.display()))?;
    let checkout_metadata = fs::metadata(&checkout).context("inspect canonical checkout")?;
    let git_dir = git_output(&checkout, &["rev-parse", "--absolute-git-dir"])?;
    let git_dir = PathBuf::from(git_dir)
        .canonicalize()
        .context("canonicalize Git administrative directory")?;
    let metadata = fs::metadata(&git_dir).context("inspect Git administrative directory")?;
    let mut digest = Sha256::new();
    digest.update(b"fluent-checkout-v1\0");
    digest.update(checkout.as_os_str().as_bytes());
    digest.update(b"\0");
    digest.update(checkout_metadata.dev().to_le_bytes());
    digest.update(checkout_metadata.ino().to_le_bytes());
    digest.update(b"\0");
    digest.update(git_dir.as_os_str().as_bytes());
    digest.update(b"\0");
    digest.update(metadata.dev().to_le_bytes());
    digest.update(metadata.ino().to_le_bytes());
    Ok(format!("checkout-v1:{:x}", digest.finalize()))
}

fn git_output(path: &Path, args: &[&str]) -> Result<String> {
    let output =
        crate::git::run_raw(path, args).with_context(|| format!("run git {}", args.join(" ")))?;
    if !output.status.success() {
        bail!("git {} failed", args.join(" "));
    }
    String::from_utf8(output.stdout)
        .context("Git returned non-UTF-8 output")
        .map(|value| value.trim().to_owned())
}

pub fn checkout_is_available(checkout: &Checkout) -> bool {
    if !checkout.path.is_dir() {
        return false;
    }
    let Ok(identity) = checkout_identity(&checkout.path) else {
        return false;
    };
    if identity != checkout.identity {
        return false;
    }
    let Ok(branch) = git_output(&checkout.path, &["branch", "--show-current"]) else {
        return false;
    };
    let Ok(commit) = git_output(&checkout.path, &["rev-parse", "HEAD"]) else {
        return false;
    };
    branch == checkout.branch && commit == checkout.commit
}

/// Format a code reference without disclosing the runner's host path. A
/// committed GitHub checkout gets an immutable permalink; local state receives
/// a clearly labelled, bounded excerpt instead.
pub fn format_code_reference(checkout: &Checkout, path: &Path, line: usize) -> Result<String> {
    let checkout_root = checkout
        .path
        .canonicalize()
        .context("canonicalize bound checkout")?;
    let path = path
        .canonicalize()
        .context("canonicalize referenced code")?;
    if !path.is_file() {
        bail!("referenced code is not a regular file");
    }
    let relative = path
        .strip_prefix(&checkout_root)
        .context("code reference is outside the bound checkout")?;
    let relative = relative.to_string_lossy().replace('\\', "/");
    let committed = git_output(&checkout.path, &["status", "--porcelain", "--", &relative])
        .map_or(false, |value| value.is_empty());
    let remote = git_output(&checkout.path, &["remote", "get-url", "origin"]).ok();
    if committed {
        if let Some(remote) = remote.and_then(|remote| github_https_remote(&remote)) {
            return Ok(format!(
                "{relative}:{line} — {remote}/blob/{}/{}#L{line}",
                checkout.commit, relative
            ));
        }
    }
    let source = fs::read_to_string(path).context("read referenced source")?;
    let excerpt = source.lines().nth(line.saturating_sub(1)).unwrap_or("");
    let excerpt = if excerpt.chars().count() > 240 {
        format!(
            "{}… [truncated]",
            excerpt.chars().take(240).collect::<String>()
        )
    } else {
        excerpt.to_owned()
    };
    Ok(format!(
        "{relative}:{line} — local-only reference (no durable GitHub link): {excerpt}"
    ))
}

fn github_https_remote(remote: &str) -> Option<String> {
    let remote = remote.trim_end_matches('/').trim_end_matches(".git");
    if let Some(path) = remote.strip_prefix("git@github.com:") {
        return Some(format!("https://github.com/{path}"));
    }
    remote
        .starts_with("https://github.com/")
        .then_some(remote.to_owned())
}

pub fn write_config(home: &Path, config: &RunnerConfig) -> Result<()> {
    config.validate()?;
    ensure_root(home)?;
    atomic_write(&config_path(home), &serde_json::to_vec_pretty(config)?).map_err(Into::into)
}
pub fn read_config(home: &Path) -> Result<RunnerConfig> {
    let value: RunnerConfig = serde_json::from_slice(&fs::read(config_path(home))?)?;
    value.validate()?;
    Ok(value)
}
pub fn preflight<P: ConversationProvider, S: SlackTransport>(
    config: &RunnerConfig,
    provider: &P,
    slack: &S,
) -> Result<()> {
    struct ReachableScheduler;
    impl SchedulerClient for ReachableScheduler {
        fn status(&self, _request: &SchedulerRequest) -> Result<SchedulerStatus> {
            Ok(SchedulerStatus { reachable: true })
        }
    }
    preflight_with_scheduler(config, provider, slack, &ReachableScheduler)
}

pub fn preflight_with_scheduler<P: ConversationProvider, S: SlackTransport, C: SchedulerClient>(
    config: &RunnerConfig,
    provider: &P,
    slack: &S,
    scheduler: &C,
) -> Result<()> {
    config.validate()?;
    for project in config.projects.values() {
        if !checkout_is_available(&project.checkout) {
            bail!(
                "configured checkout {} is unavailable or changed",
                project.label
            );
        }
    }
    provider.ready()?;
    slack.check()?;
    let status = scheduler.status(&SchedulerRequest {
        version: 1,
        operation: "status".into(),
    })?;
    if !status.reachable {
        bail!("scheduler boundary is unavailable");
    }
    Ok(())
}

fn connection_time() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// Replay durable ingress and bounded Slack history through the same
/// idempotent coordinator path after startup or reconnect.
pub fn reconcile_connected_runner<P: ConversationProvider, S: SlackTransport>(
    runner: &mut CollaborationRunner<'_, P, S>,
) -> Result<()> {
    reconcile_runner_connection(runner, false)
}

fn reconcile_runner_connection<P: ConversationProvider, S: SlackTransport>(
    runner: &mut CollaborationRunner<'_, P, S>,
    report_interruption: bool,
) -> Result<()> {
    let targets = runner.recovery_targets()?;
    let history = runner.slack_mut().bounded_history(&targets)?;
    let connected_at = connection_time();
    runner.restore_connection(history, connected_at.clone(), report_interruption)?;
    save_runner_operational_state(
        runner.home,
        &RunnerOperationalState {
            connected: true,
            last_successful_connection: Some(connected_at),
            failure: None,
        },
    )
}

/// Validate owner-scoped configuration and read both Keychain credentials
/// before the foreground runner enters its transport loop.
pub fn run_configured_runner(home: &Path) -> Result<()> {
    let config = read_config(home)?;
    let secrets = KeychainSecretStore::default();
    let app_token = secrets.read(KEYCHAIN_SERVICE, APP_TOKEN_ACCOUNT)?;
    let bot_token = secrets.read(KEYCHAIN_SERVICE, BOT_TOKEN_ACCOUNT)?;
    let api_base =
        std::env::var("FLUENT_SLACK_API_BASE").unwrap_or_else(|_| "https://slack.com/api".into());
    let codex = std::env::var_os("FLUENT_CODEX_APP_SERVER")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("codex"));
    let claude = std::env::var_os("FLUENT_CLAUDE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("claude"));
    let worker_codex = codex.clone();
    let worker_claude = claude.clone();
    let provider = ProviderRegistry::new([
        Box::new(CodexAppServerProvider::new_scoped(codex)) as Box<dyn ConversationProvider>,
        Box::new(ClaudeStructuredSessionProvider::new_scoped(claude))
            as Box<dyn ConversationProvider>,
    ])?;
    let slack = SocketModeTransport::connect(api_base, app_token, bot_token)?;
    let scheduler = LocalSchedulerClient::new(
        home,
        config
            .projects
            .values()
            .map(|project| project.checkout.path.clone()),
    );
    preflight_with_scheduler(&config, &provider, &slack, &scheduler)?;
    let turn_capacity = std::env::var("FLUENT_SLACK_TURN_CAPACITY")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(4);
    let mut runner = AsyncCollaborationRunner::new_with_provider_factory(
        home,
        config,
        provider,
        slack,
        turn_capacity,
        move || {
            ProviderRegistry::new([
                Box::new(CodexAppServerProvider::new_scoped(worker_codex.clone()))
                    as Box<dyn ConversationProvider>,
                Box::new(ClaudeStructuredSessionProvider::new_scoped(
                    worker_claude.clone(),
                )) as Box<dyn ConversationProvider>,
            ])
            .expect("static provider registry is valid")
        },
    );
    let discovery_roots = read_project_discovery_roots(home)?;
    if !discovery_roots.is_empty() {
        runner.configure_project_enablement(
            discovery_roots,
            Box::new(ExistingConfiguredInitBoundary::new(std::env::current_exe()?)),
        );
    }
    let targets = runner.recovery_targets()?;
    let history = runner.slack_mut().bounded_history(&targets)?;
    let connected_at = connection_time();
    runner.restore_connection(history, connected_at.clone(), false)?;
    save_runner_operational_state(
        home,
        &RunnerOperationalState {
            connected: true,
            last_successful_connection: Some(connected_at),
            failure: None,
        },
    )?;
    loop {
        match runner.drive_once(std::time::Duration::from_secs(1)) {
            Ok(()) => continue,
            Err(error) => {
                record_runner_failure(home, &error)?;
                runner.slack_mut().reconnect()?;
                let targets = runner.recovery_targets()?;
                let history = runner.slack_mut().bounded_history(&targets)?;
                runner.restore_connection(history, connection_time(), true)?;
            }
        }
    }
}

/// Launch an interactive provider login with the same private state paths
/// used by the selected Slack thread.
pub fn authenticate_provider_for_thread(
    home: &Path,
    project: &str,
    thread: &str,
    provider: &str,
) -> Result<()> {
    let executable = match provider {
        "codex" => std::env::var_os("FLUENT_CODEX_APP_SERVER")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("codex")),
        "claude" => std::env::var_os("FLUENT_CLAUDE")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("claude")),
        _ => bail!("conversation provider must be codex or claude"),
    };
    authenticate_provider_for_thread_with_executable(home, project, thread, provider, &executable)
}

#[doc(hidden)]
pub fn authenticate_provider_for_thread_with_executable(
    home: &Path,
    project: &str,
    thread: &str,
    provider: &str,
    executable: &Path,
) -> Result<()> {
    let config = read_config(home)?;
    validate_provider_login_recovery(home, &config, project, thread, provider)?;
    authenticate_provider_with_executable(&config, project, thread, provider, executable)?;
    let current = read_config(home)?;
    validate_provider_login_recovery(home, &current, project, thread, provider)?;
    enqueue_recovery_action(
        home,
        OperatorRecoveryAction {
            id: format!("{thread}:provider-login"),
            project: project.into(),
            thread: thread.into(),
            kind: OperatorRecoveryKind::ProviderLogin {
                provider: provider.into(),
            },
        },
    )
}

fn validate_provider_login_recovery(
    home: &Path,
    config: &RunnerConfig,
    project: &str,
    thread: &str,
    provider: &str,
) -> Result<()> {
    let bound = FileCollaborationStore::new(home)
        .load_thread(config, thread)?
        .context("Fluent Slack thread is unavailable")?;
    if bound.project_label.as_deref() != Some(project) {
        bail!("Fluent Slack thread is not bound to the selected project");
    }
    if bound.provider.as_deref() != Some(provider) {
        bail!("Fluent Slack thread is not using the selected provider");
    }
    if !matches!(
        bound.provider_failure,
        Some(
            ProviderFailure::Authentication
                | ProviderFailure::HostCredentialDenied
                | ProviderFailure::Unavailable
        )
    ) || !bound
        .pending_turn
        .as_ref()
        .is_some_and(|turn| turn.dispatch_state == PendingTurnDispatchState::PausedForRecovery)
    {
        bail!("Fluent Slack thread is not paused for provider login recovery");
    }
    Ok(())
}

/// Queue a checkout recovery only after the owner restores the exact binding.
pub fn request_checkout_reconfirmation(home: &Path, project: &str, thread: &str) -> Result<()> {
    validate_thread_identity(thread)?;
    let config = read_config(home)?;
    let configured = config
        .projects
        .get(project)
        .context("configured Slack project is unavailable")?;
    if !checkout_is_available(&configured.checkout) {
        bail!("bound checkout is unavailable");
    }
    let bound = FileCollaborationStore::new(home)
        .load_thread(&config, thread)?
        .context("Fluent Slack thread is unavailable")?;
    if bound.project_label.as_deref() != Some(project) {
        bail!("Fluent Slack thread is not bound to the selected project");
    }
    if bound.provider_failure.is_some()
        || !bound
            .pending_turn
            .as_ref()
            .is_some_and(|turn| turn.dispatch_state == PendingTurnDispatchState::PausedForRecovery)
    {
        bail!("Fluent Slack thread is not paused for checkout recovery");
    }
    enqueue_recovery_action(
        home,
        OperatorRecoveryAction {
            id: format!("{thread}:checkout-reconfirmation"),
            project: project.into(),
            thread: thread.into(),
            kind: OperatorRecoveryKind::CheckoutReconfirmation,
        },
    )
}

#[doc(hidden)]
pub fn authenticate_provider_with_executable(
    config: &RunnerConfig,
    project: &str,
    thread: &str,
    provider: &str,
    executable: &Path,
) -> Result<()> {
    validate_thread_identity(thread)?;
    let project = config
        .projects
        .get(project)
        .context("configured Slack project is unavailable")?;
    if !project.enabled_providers.contains(provider) {
        bail!("conversation provider is not enabled for this project");
    }
    if !checkout_is_available(&project.checkout) {
        bail!("bound checkout is unavailable");
    }
    let draft = project
        .checkout
        .path
        .join(".fluent/work/slack/drafts")
        .join(thread);
    let private_home = draft.join(".provider-home");
    fs::create_dir_all(&private_home)?;
    let mut command = Command::new(executable);
    command
        .current_dir(&project.checkout.path)
        .env("HOME", private_home);
    match provider {
        "codex" => {
            let provider_home = draft.join(".codex-home");
            fs::create_dir_all(&provider_home)?;
            command
                .env("CODEX_HOME", provider_home)
                .env_remove("CLAUDE_CONFIG_DIR")
                .arg("login");
        }
        "claude" => {
            let provider_home = draft.join(".claude-home");
            fs::create_dir_all(&provider_home)?;
            command
                .env("CLAUDE_CONFIG_DIR", provider_home)
                .env_remove("CODEX_HOME");
        }
        _ => bail!("conversation provider must be codex or claude"),
    }
    command
        .status()
        .with_context(|| format!("launch {provider} authentication"))?
        .success()
        .then_some(())
        .with_context(|| format!("{provider} authentication did not complete"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Default)]
    struct MemorySecrets(BTreeMap<(String, String), String>);
    impl SecretStore for MemorySecrets {
        fn store(&mut self, service: &str, account: &str, secret: &str) -> Result<()> {
            self.0
                .insert((service.into(), account.into()), secret.into());
            Ok(())
        }
        fn read(&self, service: &str, account: &str) -> Result<String> {
            self.0
                .get(&(service.into(), account.into()))
                .cloned()
                .context("missing secret")
        }
    }
    use tempfile::TempDir;
    fn initialize_checkout(path: &Path) -> (String, String) {
        for args in [
            &["init", "-b", "main"][..],
            &["config", "user.email", "test@example.invalid"][..],
            &["config", "user.name", "Fluent test"][..],
        ] {
            crate::git::run(path, args, "initialize Slack checkout fixture").unwrap();
        }
        fs::write(path.join("README"), "fixture").unwrap();
        for args in [&["add", "README"][..], &["commit", "-m", "fixture"][..]] {
            crate::git::run(path, args, "commit Slack checkout fixture").unwrap();
        }
        (
            checkout_identity(path).unwrap(),
            crate::git::run_stdout(path, &["rev-parse", "HEAD"], "read fixture HEAD").unwrap(),
        )
    }
    fn setup() -> (TempDir, TempDir, RunnerConfig) {
        let home = TempDir::new().unwrap();
        let checkout = TempDir::new().unwrap();
        let (identity, commit) = initialize_checkout(checkout.path());
        let project = ConfiguredProject {
            label: "app".into(),
            checkout: Checkout {
                label: "main".into(),
                path: checkout.path().into(),
                identity,
                branch: "main".into(),
                commit,
            },
            allowed_participants: ["U1".into()].into_iter().collect(),
            default_provider: "codex".into(),
            enabled_providers: ["codex".into()].into_iter().collect(),
            project_approvers: ["U1".into()].into_iter().collect(),
            github_remote: None,
        };
        let mut config = RunnerConfig {
            workspace: "workspace".into(),
            projects: BTreeMap::new(),
            channel_defaults: BTreeMap::new(),
            direct_message_defaults: BTreeMap::new(),
            enabled_providers: ["codex".into()].into_iter().collect(),
        };
        config.projects.insert("app".into(), project);
        config.channel_defaults.insert("C1".into(), "app".into());
        (home, checkout, config)
    }
    fn event(id: &str, thread: Option<&str>, text: &str) -> InboundEnvelope {
        InboundEnvelope {
            id: id.into(),
            acknowledgement_id: id.into(),
            channel: "C1".into(),
            timestamp: "1.0".into(),
            thread_timestamp: thread.map(str::to_owned),
            user: "U1".into(),
            text: text.into(),
            change: InboundChange::Message,
            approval_action_id: None,
            approval_artifact_revision: None,
        }
    }
    #[test]
    fn top_level_mention_uses_available_channel_default() {
        let (home, _checkout, config) = setup();
        let mut r = CollaborationRunner::new(
            home.path(),
            config,
            InMemoryProvider::default(),
            InMemorySlack::default(),
        );
        r.ingest(event("E1", None, "@Fluent build this")).unwrap();
        assert_eq!(r.provider().calls.len(), 1);
        assert!(r.slack().deliveries[0].text.contains("Project: app"));
        assert!(
            !r.slack().deliveries[0]
                .text
                .contains(&home.path().display().to_string())
        );
    }
    #[test]
    fn redelivered_slack_event_has_no_duplicate_effect() {
        let (home, _checkout, config) = setup();
        let mut r = CollaborationRunner::new(
            home.path(),
            config,
            InMemoryProvider::default(),
            InMemorySlack::default(),
        );
        let e = event("E1", None, "@Fluent hi");
        r.ingest(e.clone()).unwrap();
        r.ingest(e).unwrap();
        assert_eq!(r.provider().calls.len(), 1);
        assert_eq!(r.slack().deliveries.len(), 1);
    }
    #[test]
    fn missing_default_denies_provider_access() {
        let (home, _checkout, mut config) = setup();
        config.channel_defaults.clear();
        let mut r = CollaborationRunner::new(
            home.path(),
            config,
            InMemoryProvider::default(),
            InMemorySlack::default(),
        );
        r.ingest(event("E1", None, "@Fluent hi")).unwrap();
        assert!(r.provider().calls.is_empty());
        assert!(
            r.slack().deliveries[0]
                .text
                .contains("Available projects: app")
        );
    }

    #[test]
    fn unavailable_default_offers_available_projects_without_provider_access() {
        let (home, checkout, mut config) = setup();
        fs::remove_dir_all(checkout.path().join(".git")).unwrap();
        let available = TempDir::new().unwrap();
        let (identity, commit) = initialize_checkout(available.path());
        config.projects.insert(
            "other".into(),
            ConfiguredProject {
                label: "other".into(),
                checkout: Checkout {
                    label: "other-main".into(),
                    path: available.path().into(),
                    identity,
                    branch: "main".into(),
                    commit,
                },
                allowed_participants: ["U1".into()].into_iter().collect(),
                default_provider: "codex".into(),
                enabled_providers: ["codex".into()].into_iter().collect(),
                project_approvers: ["U1".into()].into_iter().collect(),
                github_remote: None,
            },
        );
        let mut r = CollaborationRunner::new(
            home.path(),
            config,
            InMemoryProvider::default(),
            InMemorySlack::default(),
        );
        r.ingest(event("E1", None, "@Fluent hi")).unwrap();
        assert!(r.provider().calls.is_empty());
        assert!(r.slack().deliveries[0].text.contains("other"));
        assert!(!r.slack().deliveries[0].text.contains("app,"));
    }

    #[test]
    fn credentials_stay_in_secret_store_and_redact_output() {
        let mut secrets = MemorySecrets::default();
        store_slack_credentials(&mut secrets, "xapp-secret", "xoxb-secret").unwrap();
        assert_eq!(
            secrets.read(KEYCHAIN_SERVICE, APP_TOKEN_ACCOUNT).unwrap(),
            "xapp-secret"
        );
        assert_eq!(
            redact_secrets("failed xapp-secret xoxb-secret"),
            "failed [REDACTED] [REDACTED]"
        );
    }

    #[test]
    fn status_reports_safe_operational_identity() {
        let (_, _checkout, config) = setup();
        let status = runner_status(
            &config,
            true,
            Some("2026-08-17T00:00:00Z".into()),
            Some("xapp-secret disconnected".into()),
        );
        let rendered = status.to_string();
        assert!(rendered.contains("workspace: workspace"));
        assert!(rendered.contains("last successful connection"));
        assert!(!rendered.contains("xapp-secret"));
    }

    #[test]
    fn setup_writes_socket_mode_manifest_and_validates_configuration() {
        let (home, _checkout, config) = setup();
        assert!(SOCKET_MODE_MANIFEST.contains("socket_mode_enabled: true"));
        assert!(!SOCKET_MODE_MANIFEST.contains("request_url"));
        write_config(home.path(), &config).unwrap();
        assert_eq!(read_config(home.path()).unwrap().workspace, "workspace");
        preflight(
            &config,
            &InMemoryProvider::default(),
            &InMemorySlack::default(),
        )
        .unwrap();
    }

    #[test]
    fn malformed_configuration_fails_closed() {
        let (home, _checkout, mut config) = setup();
        config.workspace.clear();
        assert!(write_config(home.path(), &config).is_err());
    }

    #[test]
    fn enable_installs_controllable_login_service() {
        let mut service = FakeServiceManager::default();
        enable_runner(&mut service, Path::new("/Applications/fluent")).unwrap();
        assert!(service.enabled() && service.running);
        service.restart().unwrap();
        assert!(service.running);
        service.stop().unwrap();
        assert!(!service.running);
    }

    #[test]
    fn launch_agent_installs_the_runner_command() {
        let home = TempDir::new().unwrap();
        let mut service = LaunchAgentServiceManager::new(home.path());
        let executable = Path::new("/Applications/fluent");
        service.install(executable).unwrap();
        let plist = fs::read_to_string(&service.plist).unwrap();
        assert!(plist.contains("/Applications/fluent"));
        assert!(plist.contains("<string>slack</string><string>run</string>"));
        assert!(plist.contains("<key>RunAtLoad</key><true/>"));
        assert_eq!(
            LaunchAgentServiceManager::program_arguments(executable),
            ["/Applications/fluent", "slack", "run"]
        );
    }

    #[test]
    fn runner_restart_recovers_thread_and_reconnect_deduplicates_history() {
        let (home, _checkout, config) = setup();
        let start = event("E1", None, "@Fluent plan this");
        let mut first = CollaborationRunner::new(
            home.path(),
            config.clone(),
            InMemoryProvider::default(),
            InMemorySlack::default(),
        );
        first.ingest(start.clone()).unwrap();

        let mut recovered = CollaborationRunner::new(
            home.path(),
            config.clone(),
            InMemoryProvider::default(),
            InMemorySlack::default(),
        );
        recovered
            .reconnect(
                vec![start, event("E2", Some("1.0"), "continue")],
                "2026-08-17T00:00:00Z".into(),
            )
            .unwrap();
        assert_eq!(
            recovered.provider().calls.len(),
            1,
            "only the unprocessed reply reaches the recovered provider"
        );
        assert_eq!(
            recovered.provider().calls[0].0.as_deref(),
            Some("session-1")
        );
        let recovered_thread = load_thread(home.path(), &config, "1.0").unwrap().unwrap();
        assert_eq!(
            recovered_thread.last_connection.as_deref(),
            Some("2026-08-17T00:00:00Z")
        );
    }
}
