#[cfg(not(test))]
use std::process::Command;

use serde::Deserialize;

use crate::coder::CoderKind;

/// A provider checked for local command and authentication readiness.
///
/// Codex keeps its private worker home alive so the caller can use the exact
/// authenticated boundary that passed the check for its subsequent launch.
pub struct ProviderReadiness {
    codex_worker: Option<crate::codex_worker::CodexWorkerEnvironment>,
}

#[derive(Debug)]
pub enum ProviderReadinessError {
    ClaudeAuthentication(String),
    Provider {
        provider: CoderKind,
        condition: String,
    },
    Codex(crate::codex_worker::CodexWorkerPreparationError),
}

impl std::fmt::Display for ProviderReadinessError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ClaudeAuthentication(condition) => {
                write!(
                    formatter,
                    "claude: authentication is unavailable ({condition})"
                )
            }
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
                let output = Command::new("claude")
                    .args(["auth", "status", "--json"])
                    .output()
                    .map_err(|error| {
                        ProviderReadinessError::ClaudeAuthentication(format!(
                            "cannot run `claude auth status --json`: {error}"
                        ))
                    })?;
                if !output.status.success() {
                    return Err(ProviderReadinessError::ClaudeAuthentication(
                        "`claude auth status --json` failed".to_string(),
                    ));
                }
                let status: ClaudeAuthStatus =
                    serde_json::from_slice(&output.stdout).map_err(|error| {
                        ProviderReadinessError::ClaudeAuthentication(format!(
                            "`claude auth status --json` returned invalid JSON: {error}"
                        ))
                    })?;
                if !status.logged_in {
                    return Err(ProviderReadinessError::ClaudeAuthentication(
                        "`claude auth status --json` reports no active login".to_string(),
                    ));
                }
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
        }
    }

    pub fn codex_worker(&self) -> Option<&crate::codex_worker::CodexWorkerEnvironment> {
        self.codex_worker.as_ref()
    }
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
            Self::ClaudeAuthentication(_)
                | Self::Codex(crate::codex_worker::CodexWorkerPreparationError::Authentication(_))
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_status_requires_an_active_login() {
        let status: ClaudeAuthStatus = serde_json::from_str(r#"{"loggedIn":false}"#).unwrap();
        assert!(!status.logged_in);
    }
}
