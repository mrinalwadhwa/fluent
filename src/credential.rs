use anyhow::Result;
use std::process::Command;

/// Safety: We only call set_env_var from the main thread when no child
/// processes are running — both during initial setup and between sessions.
fn set_env_var(key: &str, value: &str) {
    // SAFETY: Called from the main thread when no child processes are running.
    unsafe { std::env::set_var(key, value) };
}

/// Keychain item class. A generic password and an internet password are
/// separate classes with separate lookup commands, and `find-generic-password`
/// does not find an item stored as the other. Linux has no equivalent split:
/// `secret-tool` looks up by attribute.
#[derive(Clone, Copy, PartialEq, Eq)]
enum KeychainItem {
    Generic,
    Internet,
}

/// Configuration for a named secret: service identifier, optional account,
/// Keychain item class, and environment variable fallback name.
struct CredentialConfig {
    service: &'static str,
    account: Option<&'static str>,
    /// Chooses the Keychain lookup. Linux resolves by attribute and has no
    /// equivalent class to pick.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    keychain_item: KeychainItem,
    /// Names the variable the Linux fallback reads. The Keychain holds every
    /// secret Fluent needs, so macOS never consults it.
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    env_var: &'static str,
}

const OAUTH_TOKEN: CredentialConfig = CredentialConfig {
    service: "Claude Code-credentials",
    account: None,
    keychain_item: KeychainItem::Generic,
    env_var: "CLAUDE_CODE_OAUTH_TOKEN",
};

const ANTHROPIC_API_KEY: CredentialConfig = CredentialConfig {
    service: "https://api.anthropic.com",
    account: Some("Bearer"),
    keychain_item: KeychainItem::Internet,
    env_var: "ANTHROPIC_API_KEY",
};

const BRAVE_SEARCH_KEY: CredentialConfig = CredentialConfig {
    service: "zed-sandbox",
    account: Some("brave_api_key"),
    keychain_item: KeychainItem::Generic,
    env_var: "BRAVE_SEARCH_API_KEY",
};

/// Whether a lookup may fall back to the environment when the store has no
/// entry.
///
/// Injection may: a headless Linux host often runs no keyring daemon, and the
/// environment is where the secret arrives instead. A refresh may not — the
/// variable it would read is the stale token it was called to replace, so
/// falling back would report success while changing nothing.
#[derive(Clone, Copy, PartialEq, Eq)]
enum EnvFallback {
    Allowed,
    Denied,
}

/// Name the `security` subcommand that reads this item class.
///
/// Kept outside the `cfg` split so a Linux builder still fails when the two
/// classes are collapsed into one lookup.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn keychain_lookup_command(item: KeychainItem) -> &'static str {
    match item {
        KeychainItem::Generic => "find-generic-password",
        KeychainItem::Internet => "find-internet-password",
    }
}

/// Read a named secret from the platform credential store.
///
/// On macOS, queries the Keychain, which holds every secret Fluent needs and
/// takes no environment fallback. On Linux, queries `secret-tool` and then the
/// configured environment variable when `env_fallback` allows it.
#[cfg(target_os = "macos")]
fn read_secret(config: &CredentialConfig, _env_fallback: EnvFallback) -> Option<String> {
    let lookup = keychain_lookup_command(config.keychain_item);
    let mut args = vec![lookup, "-s", config.service, "-w"];
    if let Some(account) = config.account {
        args.push("-a");
        args.push(account);
    }

    let output = Command::new("security").args(args).output().ok()?;

    if !output.status.success() {
        return None;
    }

    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if value.is_empty() {
        return None;
    }

    // OAuth token is stored as JSON; extract the access token
    if config.service == OAUTH_TOKEN.service {
        return extract_oauth_token(&value);
    }

    Some(value)
}

#[cfg(not(target_os = "macos"))]
fn read_secret(config: &CredentialConfig, env_fallback: EnvFallback) -> Option<String> {
    // Try secret-tool (libsecret CLI)
    let mut args = vec!["lookup", "service", config.service];
    if let Some(account) = config.account {
        args.push("username");
        args.push(account);
    }
    let output = Command::new("secret-tool").args(args).output();

    if let Ok(output) = output
        && output.status.success()
    {
        let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !value.is_empty() {
            // OAuth token is stored as JSON; extract the access token
            if config.service == OAUTH_TOKEN.service {
                return extract_oauth_token(&value);
            }
            return Some(value);
        }
    }

    if env_fallback == EnvFallback::Denied {
        return None;
    }
    std::env::var(config.env_var).ok()
}

/// Inject credentials from the platform credential store into environment variables.
/// This runs OUTSIDE the sandbox.
pub fn inject_credentials() -> Result<()> {
    inject_oauth_token()?;
    inject_brave_search_key()?;
    inject_aws_credentials()?;
    Ok(())
}

/// Refresh credentials before a new session.
///
/// Runs `claude -p "ok" --max-turns 1` outside the sandbox to trigger
/// OAuth token refresh, then re-reads credentials from the credential store.
/// Called between sessions in sandboxed mode because the sandbox blocks
/// credential store access — the agent cannot refresh tokens itself.
pub fn refresh_credentials() -> Result<()> {
    eprintln!("  Refreshing credentials...");

    // Trigger Claude Code's internal token refresh
    Command::new("claude")
        .args(["-p", "ok", "--max-turns", "1"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .ok();

    // Re-read OAuth token from credential store (force refresh)
    refresh_oauth_token()?;
    Ok(())
}

/// Inject OAuth token from credential store if not already set.
fn inject_oauth_token() -> Result<()> {
    if std::env::var("CLAUDE_CODE_OAUTH_TOKEN").is_ok() {
        return Ok(());
    }

    if let Some(token) = read_secret(&OAUTH_TOKEN, EnvFallback::Allowed) {
        set_env_var("CLAUDE_CODE_OAUTH_TOKEN", &token);
        eprintln!("  OAuth token injected from credential store");
        return Ok(());
    }

    // API key fallback (skip if OAuth available)
    if std::env::var("CLAUDE_CODE_OAUTH_TOKEN").is_err()
        && std::env::var("ANTHROPIC_API_KEY").is_err()
    {
        if let Some(key) = read_secret(&ANTHROPIC_API_KEY, EnvFallback::Allowed) {
            set_env_var("ANTHROPIC_API_KEY", &key);
            eprintln!("  Anthropic key injected from credential store");
        }
    }

    Ok(())
}

/// Re-read the OAuth token from the credential store, replacing any existing value.
fn refresh_oauth_token() -> Result<()> {
    if let Some(token) = read_secret(&OAUTH_TOKEN, EnvFallback::Denied) {
        set_env_var("CLAUDE_CODE_OAUTH_TOKEN", &token);
    }
    Ok(())
}

/// Force a fresh OAuth token read from the credential store, bypassing the
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

    if let Some(key) = read_secret(&BRAVE_SEARCH_KEY, EnvFallback::Allowed) {
        set_env_var("BRAVE_SEARCH_API_KEY", &key);
        eprintln!("  Brave Search key injected from credential store");
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

    #[test]
    fn test_extract_oauth_token_valid() {
        let json = r#"{"claudeAiOauth":{"accessToken":"«redacted:sk-…»"}}"#;
        assert_eq!(extract_oauth_token(json), Some("«redacted:sk-…»".to_string()));
    }

    #[test]
    fn test_extract_oauth_token_missing_outer_key() {
        let json = r#"{"otherKey":{"accessToken":"«redacted:sk-…»"}}"#;
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

    // -- Linux credential lookup tests --

    #[test]
    #[cfg(not(target_os = "macos"))]
    fn test_read_secret_env_fallback() {
        // secret-tool is absent on this host; the env var should be used
        const TEST_SECRET: &str = "CREDENTIAL_TEST_SECRET";
        // SAFETY: test runs single-threaded, no child processes exist
        unsafe { std::env::set_var(TEST_SECRET, "env-secret-value") };
        let config = CredentialConfig {
            service: "test-service",
            account: None,
            keychain_item: KeychainItem::Generic,
            env_var: TEST_SECRET,
        };
        let result = read_secret(&config, EnvFallback::Allowed);
        // SAFETY: test runs single-threaded, no child processes exist
        unsafe { std::env::remove_var(TEST_SECRET) };
        assert_eq!(result, Some("env-secret-value".to_string()));
    }

    #[test]
    fn test_read_secret_absent_secret_tool_and_env() {
        // Neither secret-tool nor env var available -> None
        let config = CredentialConfig {
            service: "nonexistent-service",
            account: None,
            keychain_item: KeychainItem::Generic,
            env_var: "CREDENTIAL_NONEXISTENT_VAR",
        };
        assert!(std::env::var("CREDENTIAL_NONEXISTENT_VAR").is_err());
        assert_eq!(read_secret(&config, EnvFallback::Allowed), None);
    }

    #[test]
    fn an_internet_password_is_read_with_its_own_lookup() {
        // The Anthropic key is stored as an internet password, which
        // find-generic-password does not return.
        assert_eq!(
            keychain_lookup_command(ANTHROPIC_API_KEY.keychain_item),
            "find-internet-password"
        );
        assert_eq!(
            keychain_lookup_command(OAUTH_TOKEN.keychain_item),
            "find-generic-password"
        );
        assert_eq!(
            keychain_lookup_command(BRAVE_SEARCH_KEY.keychain_item),
            "find-generic-password"
        );
    }

    #[test]
    #[cfg(not(target_os = "macos"))]
    fn a_refusing_lookup_ignores_the_environment_it_would_otherwise_read() {
        // A refresh reads past the variable it is about to replace; falling
        // back to it would re-set the stale token and report success.
        const TEST_SECRET: &str = "CREDENTIAL_TEST_NO_FALLBACK";
        // SAFETY: test runs single-threaded, no child processes exist
        unsafe { std::env::set_var(TEST_SECRET, "stale-token") };
        let config = CredentialConfig {
            service: "nonexistent-service",
            account: None,
            keychain_item: KeychainItem::Generic,
            env_var: TEST_SECRET,
        };

        let denied = read_secret(&config, EnvFallback::Denied);
        let allowed = read_secret(&config, EnvFallback::Allowed);
        // SAFETY: test runs single-threaded, no child processes exist
        unsafe { std::env::remove_var(TEST_SECRET) };

        assert_eq!(denied, None);
        assert_eq!(allowed, Some("stale-token".to_string()));
    }

    #[test]
    fn test_inject_oauth_token_skips_when_already_set() {
        const TEST_TOKEN: &str = "CREDENTIAL_TEST_TOKEN";
        // SAFETY: test runs single-threaded, no child processes exist
        unsafe { std::env::set_var("CLAUDE_CODE_OAUTH_TOKEN", TEST_TOKEN) };
        inject_oauth_token().unwrap();
        // Token should remain unchanged
        assert_eq!(std::env::var("CLAUDE_CODE_OAUTH_TOKEN").unwrap(), TEST_TOKEN);
        // SAFETY: test runs single-threaded, no child processes exist
        unsafe { std::env::remove_var("CLAUDE_CODE_OAUTH_TOKEN") };
    }
}
