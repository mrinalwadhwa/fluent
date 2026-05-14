use anyhow::Result;
use std::process::Command;

/// Safety: We only call set_env_var from the main thread before spawning
/// child processes. The factory is single-threaded during credential setup.
fn set_env_var(key: &str, value: &str) {
    // SAFETY: Called during single-threaded initialization before spawning agents.
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
/// Runs `claude -p "ok" --max-turns 1` outside the sandbox to trigger
/// OAuth token refresh, then re-reads credentials from Keychain.
/// Called between sessions in sandboxed mode because the sandbox blocks
/// Keychain access — the agent cannot refresh tokens itself.
pub fn refresh_credentials() -> Result<()> {
    eprintln!("  Refreshing credentials...");

    // Trigger Claude Code's internal token refresh
    Command::new("claude")
        .args(["-p", "ok", "--max-turns", "1"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .ok();

    // Re-read OAuth token from Keychain (force refresh)
    refresh_oauth_token()?;
    Ok(())
}

/// Inject OAuth token from Keychain if not already set.
fn inject_oauth_token() -> Result<()> {
    if std::env::var("CLAUDE_CODE_OAUTH_TOKEN").is_ok() {
        return Ok(());
    }

    let output = Command::new("security")
        .args(["find-generic-password", "-s", "Claude Code-credentials", "-w"])
        .output();

    if let Ok(output) = output {
        if output.status.success() {
            let cred_json = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !cred_json.is_empty() {
                if let Some(token) = extract_oauth_token(&cred_json) {
                    set_env_var("CLAUDE_CODE_OAUTH_TOKEN", &token);
                    eprintln!("  OAuth token injected from Keychain");
                    return Ok(());
                }
            }
        }
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

        if let Ok(output) = output {
            if output.status.success() {
                let key = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if !key.is_empty() {
                    set_env_var("ANTHROPIC_API_KEY", &key);
                    eprintln!("  Anthropic key injected from Keychain");
                }
            }
        }
    }

    Ok(())
}

/// Re-read the OAuth token from Keychain, replacing any existing value.
fn refresh_oauth_token() -> Result<()> {
    let output = Command::new("security")
        .args(["find-generic-password", "-s", "Claude Code-credentials", "-w"])
        .output();

    if let Ok(output) = output {
        if output.status.success() {
            let cred_json = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !cred_json.is_empty() {
                if let Some(token) = extract_oauth_token(&cred_json) {
                    set_env_var("CLAUDE_CODE_OAUTH_TOKEN", &token);
                }
            }
        }
    }

    Ok(())
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

    if let Ok(output) = output {
        if output.status.success() {
            let key = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !key.is_empty() {
                set_env_var("BRAVE_SEARCH_API_KEY", &key);
                eprintln!("  Brave Search key injected from Keychain");
            }
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
        .args(["configure", "export-credentials", "--format", "env-no-export"])
        .output();

    if let Ok(output) = output {
        if output.status.success() {
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
            if let Ok(output) = region_output {
                if output.status.success() {
                    let region = String::from_utf8_lossy(&output.stdout).trim().to_string();
                    if !region.is_empty() {
                        set_env_var("AWS_DEFAULT_REGION", &region);
                    }
                }
            }
            eprintln!("  AWS credentials injected (STS temporary)");
        }
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
