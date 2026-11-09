use anyhow::Result;
#[cfg(target_os = "macos")]
use std::ffi::CStr;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const DEFAULT_CLAUDE_REFRESH_DEADLINE_SECS: u64 = 30;
const CLAUDE_REFRESH_DEADLINE_ENV: &str = "FLUENT_CLAUDE_REFRESH_DEADLINE_SECS";
#[cfg(any(test, feature = "test-support"))]
const TEST_CREDENTIAL_FIXTURES_ENV: &str = "FLUENT_TEST_ALLOW_CREDENTIAL_FIXTURES";

/// Keep test-support provider launches independent from operator credentials.
#[cfg(feature = "test-support")]
pub(crate) fn hermetic_provider_test_mode() -> bool {
    std::env::var_os("FLUENT_TEST_HERMETIC_PROVIDERS").is_some()
        && std::env::var(TEST_CREDENTIAL_FIXTURES_ENV).as_deref() != Ok("1")
}

#[cfg(not(feature = "test-support"))]
pub(crate) fn hermetic_provider_test_mode() -> bool {
    false
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialRefreshError {
    InvalidDeadline(String),
    Spawn(String),
    Timeout(Duration),
    Cleanup(String),
    Reread(String),
    Failed {
        status: i32,
        failure: crate::claude_diagnostics::ClaudeProbeFailure,
    },
}

impl std::fmt::Display for CredentialRefreshError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidDeadline(value) => write!(
                f,
                "invalid {CLAUDE_REFRESH_DEADLINE_ENV} value {value:?}; use a positive number of seconds"
            ),
            Self::Spawn(message) => {
                write!(f, "could not start Claude credential refresh: {message}")
            }
            Self::Timeout(deadline) => write!(
                f,
                "Claude credential refresh timed out after {} seconds",
                deadline.as_secs_f64()
            ),
            Self::Cleanup(message) => write!(
                f,
                "could not terminate Claude credential refresh: {message}"
            ),
            Self::Reread(message) => {
                write!(
                    f,
                    "could not reread Claude credentials after refresh: {message}"
                )
            }
            Self::Failed { status, failure } => write!(
                f,
                "Claude credential refresh probe exited with status {status}: {}: {}",
                failure.code(),
                failure.diagnostic()
            ),
        }
    }
}

impl std::error::Error for CredentialRefreshError {}

impl CredentialRefreshError {
    pub fn probe_failure(&self) -> crate::claude_diagnostics::ClaudeProbeFailure {
        use crate::claude_diagnostics::{ClaudeProbeFailure, ClaudeProbeFailureKind};
        match self {
            Self::Failed { failure, .. } => failure.clone(),
            Self::Timeout(deadline) => ClaudeProbeFailure::new(
                ClaudeProbeFailureKind::Timeout,
                format!(
                    "probe exceeded its {} second deadline",
                    deadline.as_secs_f64()
                ),
            ),
            Self::Cleanup(message) => {
                let classified = ClaudeProbeFailure::from_output(&[], message.as_bytes());
                if classified.kind() == ClaudeProbeFailureKind::Unknown {
                    ClaudeProbeFailure::new(ClaudeProbeFailureKind::Unknown, message)
                } else {
                    classified
                }
            }
            Self::Spawn(message) => ClaudeProbeFailure::new(
                ClaudeProbeFailureKind::Unknown,
                format!("could not start the probe: {message}"),
            ),
            Self::Reread(message) => ClaudeProbeFailure::new(
                ClaudeProbeFailureKind::Unknown,
                format!("could not reread credentials after refresh: {message}"),
            ),
            Self::InvalidDeadline(value) => ClaudeProbeFailure::new(
                ClaudeProbeFailureKind::Unknown,
                format!("invalid {CLAUDE_REFRESH_DEADLINE_ENV} value {value:?}"),
            ),
        }
    }
}

/// Safety: We only call set_env_var from the main thread when no child
/// processes are running — both during initial setup and between sessions.
fn set_env_var(key: &str, value: &str) {
    // SAFETY: Called from the main thread when no child processes are running.
    unsafe { std::env::set_var(key, value) };
}

/// Inject credentials from macOS Keychain into environment variables.
/// This runs OUTSIDE the sandbox.
pub fn inject_credentials() -> Result<()> {
    if hermetic_provider_test_mode() {
        return Ok(());
    }
    inject_claude_credentials()?;
    inject_brave_search_key()?;
    inject_aws_credentials()?;
    Ok(())
}

/// Inject only the supported Claude launch credential.
///
/// Autonomous Claude workers use a managed `HOME`, including when the caller
/// deliberately disables Fluent's Seatbelt profile. In that route the
/// interactive Claude credential store is intentionally hidden, so the host
/// must bridge the credential through the process environment without copying
/// the rest of the interactive home.
pub fn inject_claude_credentials() -> Result<()> {
    if hermetic_provider_test_mode() {
        return Ok(());
    }
    inject_oauth_token()
}

/// Refresh credentials before a new session.
///
/// Runs a safe-mode host-side Claude probe outside the sandbox to trigger OAuth
/// token refresh, then re-reads credentials from Keychain after a successful exit.
/// Called between sessions in sandboxed mode because the sandbox blocks
/// Keychain access — the agent cannot refresh tokens itself.
pub fn refresh_credentials() -> std::result::Result<(), CredentialRefreshError> {
    if hermetic_provider_test_mode() {
        return Ok(());
    }
    eprintln!("  Refreshing credentials...");

    let deadline = configured_refresh_deadline()?;
    refresh_credentials_with(refresh_probe_command(), deadline, refresh_oauth_token)
}

fn refresh_credentials_with<F>(
    command: Command,
    deadline: Duration,
    reread_oauth_token: F,
) -> std::result::Result<(), CredentialRefreshError>
where
    F: FnOnce() -> Result<()>,
{
    run_refresh_probe(command, deadline)?;
    reread_oauth_token().map_err(|error| CredentialRefreshError::Reread(error.to_string()))
}

fn configured_refresh_deadline() -> std::result::Result<Duration, CredentialRefreshError> {
    configured_refresh_deadline_from(
        match std::env::var(CLAUDE_REFRESH_DEADLINE_ENV) {
            Ok(value) => Some(value),
            Err(std::env::VarError::NotPresent) => None,
            Err(std::env::VarError::NotUnicode(_)) => {
                return Err(CredentialRefreshError::InvalidDeadline(
                    "non-Unicode value".to_string(),
                ));
            }
        }
        .as_deref(),
    )
}

fn configured_refresh_deadline_from(
    value: Option<&str>,
) -> std::result::Result<Duration, CredentialRefreshError> {
    let Some(value) = value else {
        return Ok(Duration::from_secs(DEFAULT_CLAUDE_REFRESH_DEADLINE_SECS));
    };
    let seconds = value
        .parse::<u64>()
        .ok()
        .filter(|seconds| *seconds > 0)
        .ok_or_else(|| CredentialRefreshError::InvalidDeadline(value.to_string()))?;
    Ok(Duration::from_secs(seconds))
}

fn refresh_probe_command() -> Command {
    let mut command = Command::new("claude");
    command.args(["--safe-mode", "-p", "ok"]);
    command
}

fn run_refresh_probe(
    command: Command,
    deadline: Duration,
) -> std::result::Result<(), CredentialRefreshError> {
    run_bounded_claude_probe(command, deadline).map(|_| ())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BoundedClaudeProbeOutput {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

pub(crate) fn run_bounded_claude_probe(
    command: Command,
    deadline: Duration,
) -> std::result::Result<BoundedClaudeProbeOutput, CredentialRefreshError> {
    run_bounded_claude_probe_with_cleanup(
        command,
        deadline,
        crate::coder::terminate_owned_process_tree,
    )
}

fn run_bounded_claude_probe_with_cleanup<F>(
    mut command: Command,
    deadline: Duration,
    cleanup: F,
) -> std::result::Result<BoundedClaudeProbeOutput, CredentialRefreshError>
where
    F: FnOnce(
        std::process::Child,
    ) -> std::result::Result<crate::coder::GroupSweepDisposition, String>,
{
    let mut stdout = tempfile::NamedTempFile::new()
        .map_err(|error| CredentialRefreshError::Spawn(error.to_string()))?;
    let mut stderr = tempfile::NamedTempFile::new()
        .map_err(|error| CredentialRefreshError::Spawn(error.to_string()))?;
    command
        .stdout(Stdio::from(stdout.reopen().map_err(|error| {
            CredentialRefreshError::Spawn(error.to_string())
        })?))
        .stderr(Stdio::from(stderr.reopen().map_err(|error| {
            CredentialRefreshError::Spawn(error.to_string())
        })?));
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // The probe owns its process group. On timeout, cleanup signals the group while
        // its leader remains waitable, then reports a cleanup failure if it cannot
        // confirm the sweep.
        command.process_group(0);
    }

    let mut child = command
        .spawn()
        .map_err(|error| CredentialRefreshError::Spawn(error.to_string()))?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => {
                return Ok(BoundedClaudeProbeOutput {
                    stdout: read_bounded_probe_output(&mut stdout)?,
                    stderr: read_bounded_probe_output(&mut stderr)?,
                });
            }
            Ok(Some(status)) => {
                let stdout = read_bounded_probe_output(&mut stdout)?;
                let stderr = read_bounded_probe_output(&mut stderr)?;
                return Err(CredentialRefreshError::Failed {
                    status: status.code().unwrap_or(1),
                    failure: crate::claude_diagnostics::ClaudeProbeFailure::from_output(
                        &stdout, &stderr,
                    ),
                });
            }
            Ok(None) if started.elapsed() >= deadline => {
                let disposition = cleanup(child).map_err(CredentialRefreshError::Cleanup)?;
                if disposition.is_verified_swept() {
                    return Err(CredentialRefreshError::Timeout(deadline));
                }
                return Err(CredentialRefreshError::Cleanup(format!(
                    "process group cleanup was not confirmed: {disposition:?}"
                )));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(error) => return Err(CredentialRefreshError::Spawn(error.to_string())),
        }
    }
}

fn read_bounded_probe_output(
    file: &mut tempfile::NamedTempFile,
) -> std::result::Result<Vec<u8>, CredentialRefreshError> {
    const CAPTURE_LIMIT_BYTES: u64 = 16 * 1024;
    file.as_file_mut()
        .seek(SeekFrom::Start(0))
        .map_err(|error| CredentialRefreshError::Spawn(error.to_string()))?;
    let mut output = Vec::new();
    file.as_file_mut()
        .take(CAPTURE_LIMIT_BYTES)
        .read_to_end(&mut output)
        .map_err(|error| CredentialRefreshError::Spawn(error.to_string()))?;
    Ok(output)
}

/// Read the OAuth token from Keychain via `security find-generic-password`.
#[cfg(target_os = "macos")]
fn account_home_from_user_database() -> Option<PathBuf> {
    let uid = unsafe { libc::geteuid() };
    let mut record = unsafe { std::mem::zeroed::<libc::passwd>() };
    let mut result = std::ptr::null_mut();
    let mut buffer = vec![0_u8; 16 * 1024];
    // SAFETY: getpwuid_r receives valid pointers to an owned record and buffer,
    // and result is checked before pw_dir is read and copied.
    let status = unsafe {
        libc::getpwuid_r(
            uid,
            &mut record,
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            &mut result,
        )
    };
    if status != 0 || result.is_null() || record.pw_dir.is_null() {
        return None;
    }
    // SAFETY: a successful getpwuid_r returns a NUL-terminated pw_dir pointer
    // into buffer, which remains alive until the value is copied below.
    let home = unsafe { CStr::from_ptr(record.pw_dir) };
    Some(PathBuf::from(home.to_string_lossy().into_owned()))
}

#[cfg(not(target_os = "macos"))]
fn account_home_from_user_database() -> Option<PathBuf> {
    None
}

fn claude_keychain_command_for(account_home: Option<&Path>) -> Command {
    let mut command = Command::new("security");
    command.args([
        "find-generic-password",
        "-s",
        "Claude Code-credentials",
        "-w",
    ]);
    #[cfg(target_os = "macos")]
    if let Some(home) = account_home {
        command.arg(home.join("Library/Keychains/login.keychain-db"));
    }
    command
}

pub(crate) fn claude_keychain_command() -> Command {
    claude_keychain_command_for(account_home_from_user_database().as_deref())
}

fn read_oauth_from_keychain() -> Option<String> {
    let output = claude_keychain_command().output().ok()?;

    if !output.status.success() {
        return None;
    }

    let cred_json = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if cred_json.is_empty() {
        return None;
    }

    extract_oauth_token(&cred_json)
}

/// Inject OAuth token from Keychain if not already set.
fn inject_oauth_token() -> Result<()> {
    if std::env::var("CLAUDE_CODE_OAUTH_TOKEN").is_ok() {
        return Ok(());
    }

    if let Some(token) = read_oauth_from_keychain() {
        set_env_var("CLAUDE_CODE_OAUTH_TOKEN", &token);
        eprintln!("  OAuth token injected from Keychain");
        return Ok(());
    }

    // API key fallback (skip if OAuth available)
    if std::env::var("CLAUDE_CODE_OAUTH_TOKEN").is_err()
        && std::env::var("ANTHROPIC_API_KEY").is_err()
    {
        let output = Command::new("security")
            .args([
                "find-internet-password",
                "-s",
                "https://api.anthropic.com",
                "-a",
                "Bearer",
                "-w",
            ])
            .output();

        if let Ok(output) = output
            && output.status.success()
        {
            let key = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !key.is_empty() {
                set_env_var("ANTHROPIC_API_KEY", &key);
                eprintln!("  Anthropic key injected from Keychain");
            }
        }
    }

    Ok(())
}

/// Re-read the OAuth token from Keychain, replacing any existing value.
fn refresh_oauth_token() -> Result<()> {
    if let Some(token) = read_oauth_from_keychain() {
        set_env_var("CLAUDE_CODE_OAUTH_TOKEN", &token);
    }
    Ok(())
}

/// Force a fresh OAuth token read from the Keychain, bypassing the
/// inject guard that skips when a token is already set.
pub fn force_refresh_oauth_token() -> Result<()> {
    if hermetic_provider_test_mode() {
        return Ok(());
    }
    refresh_oauth_token()
}

fn extract_oauth_token(json_str: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(json_str).ok()?;
    v.get("claudeAiOauth")?
        .get("accessToken")?
        .as_str()
        .map(|s| s.to_string())
}

fn inject_brave_search_key() -> Result<()> {
    if std::env::var("BRAVE_SEARCH_API_KEY").is_ok() {
        return Ok(());
    }

    let output = Command::new("security")
        .args([
            "find-generic-password",
            "-s",
            "zed-sandbox",
            "-a",
            "brave_api_key",
            "-w",
        ])
        .output();

    if let Ok(output) = output
        && output.status.success()
    {
        let key = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !key.is_empty() {
            set_env_var("BRAVE_SEARCH_API_KEY", &key);
            eprintln!("  Brave Search key injected from Keychain");
        }
    }

    Ok(())
}

fn inject_aws_credentials() -> Result<()> {
    if std::env::var("AWS_ACCESS_KEY_ID").is_ok() {
        return Ok(());
    }

    if which("aws").is_none() {
        return Ok(());
    }

    let output = Command::new("aws")
        .args([
            "configure",
            "export-credentials",
            "--format",
            "env-no-export",
        ])
        .output();

    if let Ok(output) = output
        && output.status.success()
    {
        let creds = String::from_utf8_lossy(&output.stdout);
        for line in creds.lines() {
            if let Some((key, value)) = line.split_once('=') {
                let key = key.trim();
                let value = value.trim().trim_matches('"');
                if key.starts_with("AWS_") {
                    set_env_var(key, value);
                }
            }
        }
        // Get region
        let region_output = Command::new("aws")
            .args(["configure", "get", "region"])
            .output();
        if let Ok(output) = region_output
            && output.status.success()
        {
            let region = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !region.is_empty() {
                set_env_var("AWS_DEFAULT_REGION", &region);
            }
        }
        eprintln!("  AWS credentials injected (STS temporary)");
    }

    Ok(())
}

/// Set up git SSH signing if the ssh-sign-agent is available.
pub fn setup_git_signing() {
    let sandbox_dir = std::env::var("SANDBOX_DIR").unwrap_or_else(|_| {
        let home = std::env::var("HOME").unwrap_or_default();
        format!("{home}/.config/sandbox")
    });
    let ssh_sign_agent = format!("{sandbox_dir}/ssh-sign-agent");

    if std::fs::metadata(&ssh_sign_agent).is_ok_and(|m| !m.is_dir()) {
        set_env_var("GIT_CONFIG_COUNT", "1");
        set_env_var("GIT_CONFIG_KEY_0", "gpg.ssh.program");
        set_env_var("GIT_CONFIG_VALUE_0", &ssh_sign_agent);
        eprintln!("  Git SSH signing routed through ssh-agent");
    }
}

fn which(name: &str) -> Option<String> {
    Command::new("which")
        .arg(name)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    struct ScopedEnv {
        key: &'static str,
        previous: Option<std::ffi::OsString>,
    }

    impl ScopedEnv {
        fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
            let previous = std::env::var_os(key);
            // SAFETY: Credential tests serialize environment mutation and restore the
            // previous value when their scoped guard is dropped.
            unsafe { std::env::set_var(key, value) };
            Self { key, previous }
        }

        fn remove(key: &'static str) -> Self {
            let previous = std::env::var_os(key);
            // SAFETY: Credential tests serialize environment mutation and restore the
            // previous value when their scoped guard is dropped.
            unsafe { std::env::remove_var(key) };
            Self { key, previous }
        }
    }

    impl Drop for ScopedEnv {
        fn drop(&mut self) {
            // SAFETY: Credential tests serialize environment mutation and restore the
            // previous value when their scoped guard is dropped.
            unsafe {
                match &self.previous {
                    Some(value) => std::env::set_var(self.key, value),
                    None => std::env::remove_var(self.key),
                }
            }
        }
    }

    #[cfg(feature = "test-support")]
    #[test]
    #[serial_test::serial]
    fn hermetic_provider_mode_requires_explicit_fixture_allowlist() {
        let _hermetic = ScopedEnv::set("FLUENT_TEST_HERMETIC_PROVIDERS", "1");
        let allowlist = ScopedEnv::remove(TEST_CREDENTIAL_FIXTURES_ENV);
        assert!(hermetic_provider_test_mode());

        drop(allowlist);
        let invalid_allowlist = ScopedEnv::set(TEST_CREDENTIAL_FIXTURES_ENV, "yes");
        assert!(hermetic_provider_test_mode());

        drop(invalid_allowlist);
        let _allowlist = ScopedEnv::set(TEST_CREDENTIAL_FIXTURES_ENV, "1");
        assert!(!hermetic_provider_test_mode());
    }

    #[test]
    fn refresh_probe_honors_configured_deadline() {
        assert_eq!(
            configured_refresh_deadline_from(Some("7")).unwrap(),
            Duration::from_secs(7)
        );
        assert!(configured_refresh_deadline_from(Some("0")).is_err());
        assert!(configured_refresh_deadline_from(Some("not-a-duration")).is_err());
    }

    #[test]
    fn refresh_probe_nonzero_exit_is_failure() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "printf 'HTTP 429 rate limit' >&2; exit 23"]);

        let error = run_refresh_probe(command, Duration::from_secs(1)).unwrap_err();
        assert!(matches!(
            error,
            CredentialRefreshError::Failed {
                status: 23,
                ref failure,
            } if failure.kind() == crate::claude_diagnostics::ClaudeProbeFailureKind::RateLimit
        ));
    }

    #[test]
    fn bounded_probe_returns_success_output() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "printf 'ready'; printf 'note' >&2"]);

        let output = run_bounded_claude_probe(command, Duration::from_secs(1)).unwrap();

        assert_eq!(output.stdout, b"ready");
        assert_eq!(output.stderr, b"note");
    }

    #[test]
    fn refresh_probe_retains_bounded_sanitized_diagnostic() {
        let mut command = Command::new("/bin/sh");
        command.args([
            "-c",
            "printf 'HTTP 401 token=sk-ant-this-must-not-leak %0600d' 0 >&2; exit 1",
        ]);

        let error = run_refresh_probe(command, Duration::from_secs(1)).unwrap_err();
        let CredentialRefreshError::Failed { failure, .. } = error else {
            panic!("expected failed probe");
        };
        assert_eq!(
            failure.kind(),
            crate::claude_diagnostics::ClaudeProbeFailureKind::Authentication
        );
        assert!(!failure.diagnostic().contains("sk-ant-this-must-not-leak"));
        assert!(failure.diagnostic().chars().count() < 530);
    }

    #[cfg(unix)]
    #[test]
    fn refresh_probe_timeout_terminates_process_tree() {
        assert_eq!(
            observe_refresh_probe_tree_cleanup(Duration::from_millis(30), true)
                .expect("probe descendant survived cleanup"),
            0
        );
    }

    #[cfg(unix)]
    #[test]
    fn refresh_probe_tree_cleanup_tolerates_late_timeout_poll() {
        // A busy host can delay the timeout poll past the old marker timer.
        assert_eq!(
            observe_refresh_probe_tree_cleanup(Duration::from_millis(1200), true)
                .expect("probe descendant survived late cleanup"),
            0
        );
    }

    #[cfg(unix)]
    #[test]
    fn refresh_probe_tree_observer_detects_surviving_descendant() {
        let error = observe_refresh_probe_tree_cleanup(Duration::from_millis(30), false)
            .expect_err("killing only the leader must leave the descendant's socket open");
        assert!(matches!(
            error.kind(),
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
        ));
    }

    #[cfg(unix)]
    fn observe_refresh_probe_tree_cleanup(
        deadline: Duration,
        sweep_descendants: bool,
    ) -> std::io::Result<usize> {
        use std::net::Shutdown;
        use std::os::fd::OwnedFd;
        use std::os::unix::net::UnixStream;
        use std::process::Child;

        struct Fixture {
            observer: UnixStream,
            retained_leader: Option<Child>,
        }

        impl Drop for Fixture {
            fn drop(&mut self) {
                // Release a surviving descendant even if an assertion panics.
                // The negative control keeps its leader waitable to pin the
                // group identity until this fallback sweep and reap.
                let _ = self.observer.shutdown(Shutdown::Both);
                if let Some(child) = self.retained_leader.take() {
                    let _ = crate::coder::terminate_owned_process_tree(child);
                }
            }
        }

        let (observer, descendant) = UnixStream::pair().unwrap();
        let mut fixture = Fixture {
            observer,
            retained_leader: None,
        };
        fixture
            .observer
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut command = Command::new("/bin/sh");
        command
            .args([
                "-c",
                r#"exec 3<&0
/bin/sh -c 'printf R >&3; IFS= read -r release <&3' &
exec 0<&- 3<&-
wait"#,
            ])
            .stdin(Stdio::from(OwnedFd::from(descendant)));

        let mut ready = [0u8; 1];
        let mut readiness = None;
        let result = run_bounded_claude_probe_with_cleanup(command, deadline, |child| {
            // Wait for the descendant itself, even if the deadline expires
            // before it gets CPU time. Do not assert until cleanup has run.
            readiness = Some(fixture.observer.read_exact(&mut ready));
            if sweep_descendants {
                crate::coder::terminate_owned_process_tree(child)
            } else {
                fixture.retained_leader = Some(child);
                fixture
                    .retained_leader
                    .as_mut()
                    .unwrap()
                    .kill()
                    .map_err(|error| error.to_string())?;
                // Deliberately claim success without sweeping descendants.
                // The socket observation must detect this false claim.
                Ok(crate::coder::GroupSweepDisposition::Delivered)
            }
        });
        assert!(
            matches!(result, Err(CredentialRefreshError::Timeout(_))),
            "expected confirmed timeout cleanup, got {result:?}"
        );
        readiness
            .expect("probe did not reach timeout cleanup")
            .expect("probe descendant did not announce readiness");
        assert_eq!(ready, *b"R");

        // Command has dropped its copy of the peer, and the leader closed its
        // descriptors before waiting. Only the descendant can hold it open.
        // Observe before Drop can release or sweep a survivor. An empty buffer
        // would return zero without observing EOF, so always read one byte.
        // Keep the timeout installed before spawn: macOS can reject changing
        // socket options after the peer has already closed during cleanup.
        fixture.observer.read(&mut [0u8; 1])
    }

    #[test]
    fn refresh_probe_timeout_with_unconfirmed_group_sweep_is_cleanup_failure() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "sleep 60"]);

        let result = run_bounded_claude_probe_with_cleanup(
            command,
            Duration::from_millis(30),
            |mut child| {
                child.kill().unwrap();
                child.wait().unwrap();
                Ok(crate::coder::GroupSweepDisposition::Unconfirmed(
                    crate::coder::ProcessOpDiagnostic {
                        operation: "kill process group".to_string(),
                        kind: Some("PermissionDenied".to_string()),
                        errno: Some(libc::EPERM),
                        message: Some("operation not permitted".to_string()),
                    },
                ))
            },
        );

        assert!(
            matches!(result, Err(CredentialRefreshError::Cleanup(message)) if message.contains("process group cleanup was not confirmed"))
        );
    }

    #[test]
    fn successful_refresh_probe_rereads_credentials() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "exit 0"]);
        let reread = std::cell::Cell::new(false);
        refresh_credentials_with(command, Duration::from_secs(1), || {
            reread.set(true);
            Ok(())
        })
        .unwrap();
        assert!(
            reread.get(),
            "a successful probe must reread Keychain credentials"
        );
    }

    #[cfg(unix)]
    #[test]
    #[serial_test::serial]
    fn successful_refresh_probe_replaces_oauth_token_from_keychain() {
        let bin_dir = tempfile::tempdir().unwrap();
        let claude = bin_dir.path().join("claude");
        let security = bin_dir.path().join("security");
        std::fs::write(&claude, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::write(
            &security,
            "#!/bin/sh\nprintf '%s\\n' '{\"claudeAiOauth\":{\"accessToken\":\"refreshed-token\"}}'\n",
        )
        .unwrap();
        for executable in [&claude, &security] {
            let mut permissions = std::fs::metadata(executable).unwrap().permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(executable, permissions).unwrap();
        }

        let original_path = std::env::var_os("PATH").unwrap_or_default();
        let mut path_entries = vec![bin_dir.path().to_path_buf()];
        path_entries.extend(std::env::split_paths(&original_path));
        let test_path = std::env::join_paths(path_entries).unwrap();
        let _path = ScopedEnv::set("PATH", test_path);
        let _token = ScopedEnv::set("CLAUDE_CODE_OAUTH_TOKEN", "stale-token");
        let _allowlist = ScopedEnv::set(TEST_CREDENTIAL_FIXTURES_ENV, "1");

        refresh_credentials().unwrap();

        assert_eq!(
            std::env::var("CLAUDE_CODE_OAUTH_TOKEN").unwrap(),
            "refreshed-token",
            "a successful probe must replace the process token with the Keychain reread"
        );
    }

    #[cfg(unix)]
    #[test]
    #[serial_test::serial]
    fn claude_credential_bridge_reads_oauth_for_managed_home_launch() {
        let bin_dir = tempfile::tempdir().unwrap();
        let security = bin_dir.path().join("security");
        std::fs::write(
            &security,
            "#!/bin/sh\nprintf '%s\\n' '{\"claudeAiOauth\":{\"accessToken\":\"managed-home-token\"}}'\n",
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&security).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&security, permissions).unwrap();

        let original_path = std::env::var_os("PATH").unwrap_or_default();
        let mut path_entries = vec![bin_dir.path().to_path_buf()];
        path_entries.extend(std::env::split_paths(&original_path));
        let _path = ScopedEnv::set("PATH", std::env::join_paths(path_entries).unwrap());
        let _oauth = ScopedEnv::remove("CLAUDE_CODE_OAUTH_TOKEN");
        let _api_key = ScopedEnv::remove("ANTHROPIC_API_KEY");
        let _allowlist = ScopedEnv::set(TEST_CREDENTIAL_FIXTURES_ENV, "1");

        inject_claude_credentials().unwrap();

        assert_eq!(
            std::env::var("CLAUDE_CODE_OAUTH_TOKEN").unwrap(),
            "managed-home-token"
        );
        assert!(std::env::var_os("ANTHROPIC_API_KEY").is_none());
    }

    #[test]
    fn refresh_probe_uses_safe_mode_without_max_turns() {
        let command = refresh_probe_command();
        let args = command.get_args().collect::<Vec<_>>();
        assert!(args.contains(&OsStr::new("--safe-mode")));
        assert!(!args.contains(&OsStr::new("--max-turns")));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn claude_keychain_lookup_uses_account_home_explicitly() {
        let account_home = Path::new("/Users/account-home");
        let command = claude_keychain_command_for(Some(account_home));
        let args = command.get_args().collect::<Vec<_>>();

        assert!(args.contains(&OsStr::new(
            "/Users/account-home/Library/Keychains/login.keychain-db"
        )));
        assert!(
            !args
                .iter()
                .any(|arg| arg.to_string_lossy().contains("$HOME"))
        );
    }

    #[test]
    fn test_extract_oauth_token_valid() {
        let json = r#"{"claudeAiOauth":{"accessToken":"sk-ant-abc123"}}"#;
        assert_eq!(extract_oauth_token(json), Some("sk-ant-abc123".to_string()));
    }

    #[test]
    fn test_extract_oauth_token_missing_outer_key() {
        let json = r#"{"otherKey":{"accessToken":"sk-ant-abc123"}}"#;
        assert_eq!(extract_oauth_token(json), None);
    }

    #[test]
    fn test_extract_oauth_token_missing_inner_key() {
        let json = r#"{"claudeAiOauth":{"refreshToken":"rt-abc123"}}"#;
        assert_eq!(extract_oauth_token(json), None);
    }

    #[test]
    fn test_extract_oauth_token_invalid_json() {
        assert_eq!(extract_oauth_token("not json"), None);
    }

    #[test]
    fn test_extract_oauth_token_empty_string() {
        assert_eq!(extract_oauth_token(""), None);
    }

    #[test]
    fn test_extract_oauth_token_nested_structure() {
        let json = r#"{"claudeAiOauth":{"accessToken":"tok-123","refreshToken":"rt-456","expiresAt":1234567890}}"#;
        assert_eq!(extract_oauth_token(json), Some("tok-123".to_string()));
    }

    #[test]
    fn test_extract_oauth_token_non_string_value() {
        let json = r#"{"claudeAiOauth":{"accessToken":12345}}"#;
        assert_eq!(extract_oauth_token(json), None);
    }
}
