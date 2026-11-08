#[cfg(not(test))]
use std::process::Command;
use std::time::Duration;

use serde::Deserialize;

use crate::coder::CoderKind;

const CLAUDE_READINESS_MARKER: &str = "FLUENT_CLAUDE_READY";
const DEFAULT_CLAUDE_READINESS_DEADLINE_SECS: u64 = 30;
const CLAUDE_READINESS_DEADLINE_ENV: &str = "FLUENT_CLAUDE_READINESS_DEADLINE_SECS";

/// A provider checked for local launch readiness. Claude and Codex also verify
/// authentication; OpenCode checks only its version and surfaces authentication
/// failures on first launch.
///
/// Codex keeps its private worker home alive so the caller can use the exact
/// authenticated boundary that passed the check for its subsequent launch.
pub struct ProviderReadiness {
    codex_worker: Option<crate::codex_worker::CodexWorkerEnvironment>,
}

#[derive(Debug)]
pub enum ProviderReadinessError {
    Claude(crate::claude_diagnostics::ClaudeProbeFailure),
    Provider {
        provider: CoderKind,
        condition: String,
    },
    Codex(crate::codex_worker::CodexWorkerPreparationError),
}

impl std::fmt::Display for ProviderReadinessError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Claude(failure) => write!(formatter, "{}", failure.user_message("readiness")),
            Self::Provider {
                provider,
                condition,
            } => {
                write!(formatter, "{}: {condition}", provider.as_str())
            }
            Self::Codex(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for ProviderReadinessError {}

impl ProviderReadiness {
    /// Verify readiness without sending a model prompt.
    pub fn prepare(provider: CoderKind) -> Result<Self, ProviderReadinessError> {
        Self::prepare_with_project(provider, None)
    }

    /// Verify readiness and freeze the project's configured Codex skills into
    /// the same private worker home retained for the subsequent launch.
    pub fn prepare_for_project(
        provider: CoderKind,
        project_root: &std::path::Path,
    ) -> Result<Self, ProviderReadinessError> {
        Self::prepare_with_project(provider, Some(project_root))
    }

    fn prepare_with_project(
        provider: CoderKind,
        project_root: Option<&std::path::Path>,
    ) -> Result<Self, ProviderReadinessError> {
        let prepare_codex = || {
            match project_root {
                Some(project_root) => {
                    crate::codex_worker::CodexWorkerEnvironment::prepare_for_project(project_root)
                }
                None => crate::codex_worker::CodexWorkerEnvironment::prepare(),
            }
            .map_err(ProviderReadinessError::Codex)
        };
        #[cfg(test)]
        {
            let codex_worker = if provider == CoderKind::Codex {
                Some(prepare_codex()?)
            } else {
                None
            };
            return Ok(Self { codex_worker });
        }

        #[cfg(not(test))]
        match provider {
            CoderKind::Codex => {
                let worker = prepare_codex()?;
                worker.preflight().map_err(|error| {
                    ProviderReadinessError::Codex(
                        crate::codex_worker::CodexWorkerPreparationError::Authentication(error),
                    )
                })?;
                Ok(Self {
                    codex_worker: Some(worker),
                })
            }
            CoderKind::Claude => {
                // Autonomous Claude workers run with a Fluent-managed HOME, so
                // bridge the host credential before asking that isolated CLI
                // whether it is logged in. The later launch preparation remains
                // idempotent and reuses the same process environment value.
                crate::credential::inject_claude_credentials().map_err(|error| {
                    ProviderReadinessError::Claude(
                        crate::claude_diagnostics::ClaudeProbeFailure::new(
                            crate::claude_diagnostics::ClaudeProbeFailureKind::Unknown,
                            format!("cannot prepare Claude credentials: {error}"),
                        ),
                    )
                })?;
                let output = Command::new("claude")
                    .args(["auth", "status", "--json"])
                    .output()
                    .map_err(|error| {
                        ProviderReadinessError::Claude(
                            crate::claude_diagnostics::ClaudeProbeFailure::new(
                                crate::claude_diagnostics::ClaudeProbeFailureKind::Unknown,
                                format!("cannot run `claude auth status --json`: {error}"),
                            ),
                        )
                    })?;
                // Claude exits with status 1 for a valid logged-out response.
                // Decode structured status before classifying diagnostic text.
                let status: ClaudeAuthStatus = serde_json::from_slice(&output.stdout).map_err(
                    |error| {
                        let output_failure =
                            crate::claude_diagnostics::ClaudeProbeFailure::from_output(
                                &output.stdout,
                                &output.stderr,
                            );
                        let failure = if output_failure.kind()
                            == crate::claude_diagnostics::ClaudeProbeFailureKind::Unknown
                        {
                            crate::claude_diagnostics::ClaudeProbeFailure::new(
                                crate::claude_diagnostics::ClaudeProbeFailureKind::Unknown,
                                format!(
                                    "`claude auth status --json` returned invalid JSON: {error}; {}",
                                    output_failure.diagnostic()
                                ),
                            )
                        } else {
                            output_failure
                        };
                        ProviderReadinessError::Claude(failure)
                    },
                )?;
                if !status.logged_in {
                    let failure = claude_keychain_boundary_failure().unwrap_or_else(|| {
                        crate::claude_diagnostics::ClaudeProbeFailure::new(
                            crate::claude_diagnostics::ClaudeProbeFailureKind::Authentication,
                            "`claude auth status --json` reports no active login",
                        )
                    });
                    return Err(ProviderReadinessError::Claude(failure));
                }
                warn_for_old_claude_version();
                Ok(Self { codex_worker: None })
            }
            CoderKind::Pi => {
                let status = Command::new("pi")
                    .arg("--version")
                    .status()
                    .map_err(|error| ProviderReadinessError::Provider {
                        provider,
                        condition: format!("command is not installed: {error}"),
                    })?;
                if !status.success() {
                    return Err(ProviderReadinessError::Provider {
                        provider,
                        condition: "command readiness check failed".to_string(),
                    });
                }
                Ok(Self { codex_worker: None })
            }
            CoderKind::Opencode => {
                let output =
                    Command::new("opencode")
                        .arg("--version")
                        .output()
                        .map_err(|error| ProviderReadinessError::Provider {
                            provider,
                            condition: format!("command is not installed: {error}"),
                        })?;
                if !output.status.success() {
                    return Err(ProviderReadinessError::Provider {
                        provider,
                        condition: "command readiness check failed".to_string(),
                    });
                }
                let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
                let major = opencode_major_version(&version);
                if major.is_none_or(|major| major < crate::coder::MINIMUM_OPENCODE_MAJOR_VERSION) {
                    return Err(ProviderReadinessError::Provider {
                        provider,
                        condition: format!(
                            "OpenCode 2 or newer is required (found {version:?}); run `opencode upgrade --major`"
                        ),
                    });
                }
                // `opencode auth list` lists configured provider connections even
                // when their environment variables are absent, so it cannot prove
                // authentication. Let the first launch surface a real auth failure.
                Ok(Self { codex_worker: None })
            }
        }
    }

    pub fn codex_worker(&self) -> Option<&crate::codex_worker::CodexWorkerEnvironment> {
        self.codex_worker.as_ref()
    }

    /// Require one real, tool-free Claude response for the selected model.
    pub fn verify_claude_inference(model: &str) -> Result<(), ProviderReadinessError> {
        #[cfg(test)]
        {
            let _ = model;
            return Ok(());
        }

        #[cfg(not(test))]
        {
            let deadline = configured_claude_readiness_deadline().map_err(|diagnostic| {
                ProviderReadinessError::Claude(crate::claude_diagnostics::ClaudeProbeFailure::new(
                    crate::claude_diagnostics::ClaudeProbeFailureKind::Unknown,
                    diagnostic,
                ))
            })?;
            let mut command = Command::new("claude");
            command.args([
                "--safe-mode",
                "--tools",
                "",
                "--effort",
                "low",
                "--output-format",
                "json",
                "--no-session-persistence",
            ]);
            if !model.is_empty() {
                command.args(["--model", model]);
            }
            command.args([
                "-p",
                &format!("Reply with exactly: {CLAUDE_READINESS_MARKER}"),
            ]);
            let output = crate::credential::run_bounded_claude_probe(command, deadline)
                .map_err(|error| ProviderReadinessError::Claude(error.probe_failure()))?;
            if claude_live_response_is_ready(&output.stdout) {
                return Ok(());
            }
            let classified = crate::claude_diagnostics::ClaudeProbeFailure::from_output(
                &output.stdout,
                &output.stderr,
            );
            let failure = if classified.kind()
                == crate::claude_diagnostics::ClaudeProbeFailureKind::Unknown
            {
                crate::claude_diagnostics::ClaudeProbeFailure::new(
                    crate::claude_diagnostics::ClaudeProbeFailureKind::Unknown,
                    "Claude live readiness did not return the expected response",
                )
            } else {
                classified
            };
            Err(ProviderReadinessError::Claude(failure))
        }
    }
}

#[cfg(not(test))]
fn configured_claude_readiness_deadline() -> Result<Duration, String> {
    let value = match std::env::var(CLAUDE_READINESS_DEADLINE_ENV) {
        Ok(value) => Some(value),
        Err(std::env::VarError::NotPresent) => None,
        Err(std::env::VarError::NotUnicode(_)) => {
            return Err(format!(
                "invalid {CLAUDE_READINESS_DEADLINE_ENV} value: use a positive number of seconds"
            ));
        }
    };
    configured_claude_readiness_deadline_from(value.as_deref())
}

fn configured_claude_readiness_deadline_from(value: Option<&str>) -> Result<Duration, String> {
    let Some(value) = value else {
        return Ok(Duration::from_secs(DEFAULT_CLAUDE_READINESS_DEADLINE_SECS));
    };
    let seconds = value
        .parse::<u64>()
        .ok()
        .filter(|seconds| *seconds > 0)
        .ok_or_else(|| {
            format!(
                "invalid {CLAUDE_READINESS_DEADLINE_ENV} value {value:?}; use a positive number of seconds"
            )
        })?;
    Ok(Duration::from_secs(seconds))
}

fn claude_live_response_is_ready(output: &[u8]) -> bool {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(output) else {
        return false;
    };
    let terminal = match &value {
        serde_json::Value::Object(_) => &value,
        serde_json::Value::Array(events) => match events.last() {
            Some(terminal) => terminal,
            None => return false,
        },
        _ => return false,
    };
    terminal["type"].as_str() == Some("result")
        && terminal["subtype"].as_str() == Some("success")
        && terminal["is_error"].as_bool() == Some(false)
        && terminal["result"].as_str().map(str::trim) == Some(CLAUDE_READINESS_MARKER)
}

#[derive(Deserialize)]
struct ClaudeAuthStatus {
    #[serde(rename = "loggedIn")]
    logged_in: bool,
}

impl ProviderReadinessError {
    pub fn is_authentication_error(&self) -> bool {
        matches!(
            self,
            Self::Codex(crate::codex_worker::CodexWorkerPreparationError::Authentication(_))
        ) || matches!(self, Self::Claude(failure) if failure.is_authentication())
    }

    pub fn claude_failure(&self) -> Option<&crate::claude_diagnostics::ClaudeProbeFailure> {
        match self {
            Self::Claude(failure) => Some(failure),
            _ => None,
        }
    }
}

#[cfg(not(test))]
fn claude_keychain_boundary_failure() -> Option<crate::claude_diagnostics::ClaudeProbeFailure> {
    let output = Command::new("security")
        .args(["find-generic-password", "-s", "Claude Code-credentials"])
        .output()
        .ok()?;
    if output.status.success() {
        return None;
    }
    let failure = crate::claude_diagnostics::ClaudeProbeFailure::from_output(&[], &output.stderr);
    failure.is_host_boundary().then_some(failure)
}

#[cfg(not(test))]
fn warn_for_old_claude_version() {
    let Some(warning) = Command::new("claude")
        .arg("--version")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| claude_version_warning(&output.stdout))
    else {
        return;
    };
    eprintln!("  Warning: {warning}");
}

fn claude_version_warning(output: &[u8]) -> Option<String> {
    let installed = String::from_utf8_lossy(output)
        .split_whitespace()
        .find_map(|part| {
            crate::update::Version::parse(part.trim_matches(|character: char| {
                !character.is_ascii_alphanumeric() && character != '.'
            }))
        })?;
    let minimum = crate::update::Version::parse("2.1.211").expect("valid Claude minimum version");
    installed.is_behind(&minimum).then(|| {
        format!(
            "Claude Code {installed} predates 2.1.211, which fixes parallel session token renewal; upgrade Claude Code before parallel Fluent runs"
        )
    })
}

fn opencode_major_version(version: &str) -> Option<u32> {
    version.split_whitespace().find_map(|token| {
        let token = token.trim_start_matches('v');
        let major = token.split('.').next()?;
        major.parse::<u32>().ok()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_status_requires_an_active_login() {
        let status: ClaudeAuthStatus = serde_json::from_str(r#"{"loggedIn":false}"#).unwrap();
        assert!(!status.logged_in);
    }

    #[test]
    fn old_claude_version_warns_but_current_version_does_not() {
        let warning = claude_version_warning(b"2.1.191 (Claude Code)").unwrap();
        assert!(warning.contains("2.1.211"));
        assert!(warning.contains("parallel session token renewal"));
        assert!(claude_version_warning(b"2.1.211 (Claude Code)").is_none());
        assert!(claude_version_warning(b"2.2.0 (Claude Code)").is_none());
    }

    #[test]
    fn unknown_claude_version_does_not_block_readiness() {
        assert!(claude_version_warning(b"Claude Code development build").is_none());
    }

    #[test]
    fn claude_live_response_requires_exact_success_marker() {
        assert!(claude_live_response_is_ready(
            br#"{"type":"result","subtype":"success","is_error":false,"result":"FLUENT_CLAUDE_READY"}"#
        ));
        assert!(claude_live_response_is_ready(
            br#"[{"type":"system","subtype":"init"},{"type":"result","subtype":"success","is_error":false,"result":"FLUENT_CLAUDE_READY"}]"#
        ));
        assert!(!claude_live_response_is_ready(
            br#"{"type":"result","subtype":"success","is_error":false,"result":"almost"}"#
        ));
        assert!(!claude_live_response_is_ready(
            br#"[{"type":"result","subtype":"success","is_error":false,"result":"FLUENT_CLAUDE_READY"},{"type":"result","subtype":"error","is_error":true,"result":"failed"}]"#
        ));
        assert!(!claude_live_response_is_ready(b"not json"));
    }

    #[test]
    fn claude_live_readiness_deadline_is_positive() {
        assert_eq!(
            configured_claude_readiness_deadline_from(None).unwrap(),
            Duration::from_secs(30)
        );
        assert_eq!(
            configured_claude_readiness_deadline_from(Some("7")).unwrap(),
            Duration::from_secs(7)
        );
        assert!(configured_claude_readiness_deadline_from(Some("0")).is_err());
        assert!(configured_claude_readiness_deadline_from(Some("bad")).is_err());
    }

    #[test]
    fn opencode_version_major_parses_supported_release() {
        assert_eq!(opencode_major_version("2.0.26"), Some(2));
        assert_eq!(opencode_major_version("opencode 2.0.26"), Some(2));
        assert_eq!(opencode_major_version("v2.1.0"), Some(2));
        assert_eq!(opencode_major_version("26.0.0"), Some(26));
        assert_eq!(opencode_major_version("development build"), None);
    }

    #[test]
    fn opencode_rejects_v1_version() {
        let major = opencode_major_version("1.19.0");
        assert!(major.is_some_and(|major| major < crate::coder::MINIMUM_OPENCODE_MAJOR_VERSION));
    }
}
