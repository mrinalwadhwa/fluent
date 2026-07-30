use anyhow::Result;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const DEFAULT_CLAUDE_REFRESH_DEADLINE_SECS: u64 = 30;
const CLAUDE_REFRESH_DEADLINE_ENV: &str = "FLUENT_CLAUDE_REFRESH_DEADLINE_SECS";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialRefreshError {
    InvalidDeadline(String),
    Spawn(String),
    Timeout(Duration),
    Cleanup(String),
    Failed(i32),
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
            Self::Failed(status) => write!(
                f,
                "Claude credential refresh probe exited unsuccessfully with status {status}"
            ),
        }
    }
}

impl std::error::Error for CredentialRefreshError {}

/// Safety: We only call set_env_var from the main thread when no child
/// processes are running — both during initial setup and between sessions.
fn set_env_var(key: &str, value: &str) {
    // SAFETY: Called from the main thread when no child processes are running.
    unsafe { std::env::set_var(key, value) };
}

/// Inject credentials from macOS Keychain into environment variables.
/// This runs OUTSIDE the sandbox.
pub fn inject_credentials() -> Result<()> {
    inject_oauth_token()?;
    inject_brave_search_key()?;
    inject_aws_credentials()?;
    Ok(())
}

/// Refresh credentials before a new session.
///
/// Runs a safe-mode host-side Claude probe outside the sandbox to trigger OAuth
/// token refresh, then re-reads credentials from Keychain after a successful exit.
/// Called between sessions in sandboxed mode because the sandbox blocks
/// Keychain access — the agent cannot refresh tokens itself.
pub fn refresh_credentials() -> Result<()> {
    eprintln!("  Refreshing credentials...");

    let deadline = configured_refresh_deadline()?;
    refresh_credentials_with(refresh_probe_command(), deadline, refresh_oauth_token)
}

fn refresh_credentials_with<F>(
    command: Command,
    deadline: Duration,
    reread_oauth_token: F,
) -> Result<()>
where
    F: FnOnce() -> Result<()>,
{
    run_refresh_probe(command, deadline)?;
    reread_oauth_token()
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
    command
        .args(["--safe-mode", "-p", "ok"])
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command
}

fn run_refresh_probe(
    mut command: Command,
    deadline: Duration,
) -> std::result::Result<(), CredentialRefreshError> {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // The probe owns its process group. On timeout, the group is signalled while
        // its leader remains waitable, so descendants cannot outlive the refresh.
        command.process_group(0);
    }

    let mut child = command
        .spawn()
        .map_err(|error| CredentialRefreshError::Spawn(error.to_string()))?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => {
                return Err(CredentialRefreshError::Failed(status.code().unwrap_or(1)));
            }
            Ok(None) if started.elapsed() >= deadline => {
                return crate::coder::terminate_owned_process_tree(child)
                    .map_err(CredentialRefreshError::Cleanup)
                    .and(Err(CredentialRefreshError::Timeout(deadline)));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(error) => return Err(CredentialRefreshError::Spawn(error.to_string())),
        }
    }
}

/// Read the OAuth token from Keychain via `security find-generic-password`.
fn read_oauth_from_keychain() -> Option<String> {
    let output = Command::new("security")
        .args([
            "find-generic-password",
            "-s",
            "Claude Code-credentials",
            "-w",
        ])
        .output()
        .ok()?;

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
        command.args(["-c", "exit 23"]);

        assert_eq!(
            run_refresh_probe(command, Duration::from_secs(1)).unwrap_err(),
            CredentialRefreshError::Failed(23)
        );
    }

    #[test]
    fn refresh_probe_timeout_terminates_process_tree() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("descendant-survived");
        let mut command = Command::new("/bin/sh");
        command.args([
            "-c",
            &format!("(sleep 1; touch '{}') & sleep 60", marker.display()),
        ]);

        assert!(matches!(
            run_refresh_probe(command, Duration::from_millis(30)),
            Err(CredentialRefreshError::Timeout(_))
        ));
        std::thread::sleep(Duration::from_millis(1100));
        assert!(
            !marker.exists(),
            "the refresh probe descendant survived the timeout cleanup"
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

    #[test]
    fn refresh_probe_uses_safe_mode_without_max_turns() {
        let command = refresh_probe_command();
        let args = command.get_args().collect::<Vec<_>>();
        assert!(args.contains(&OsStr::new("--safe-mode")));
        assert!(!args.contains(&OsStr::new("--max-turns")));
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
