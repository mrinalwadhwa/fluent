//! Shared conversation contracts. Platform payloads and persistence stay in their adapters.
use anyhow::Result;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// Host checkout configuration resolved by persistence and provider adapters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checkout {
    pub label: String,
    pub path: PathBuf,
    pub identity: String,
    pub branch: String,
    pub commit: String,
}

/// Filesystem authority resolved at the provider interface boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderScope {
    pub checkout: PathBuf,
    pub slice_draft: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderEvent {
    Message(String),
    Interrupted,
}

/// Structured result returned by a provider turn. Plain-text provider replies
/// remain compatible and are interpreted as messages by the coordinator.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ProviderOutput {
    Message {
        text: String,
        references: Vec<CodeReference>,
    },
    PlanningArtifact {
        kind: String,
        markdown: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderTurnRecovery {
    Missing,
    Accepted { turn_id: String },
    Completed { turn_id: String, reply: String },
}

/// A provider-reported source location. The coordinator validates and renders
/// it at the delivery boundary so providers never expose host checkout paths.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeReference {
    pub path: PathBuf,
    pub line: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProviderFailure {
    Authentication,
    HostCredentialDenied,
    IncompatibleProtocol,
    Unavailable,
    RateLimited {
        retry_after_seconds: u64,
    },
    /// The adapter could not complete its host-side bookkeeping. This is not
    /// a provider recovery state and remains an operational runner error.
    Internal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderError {
    pub failure: ProviderFailure,
    diagnostic: String,
}

impl ProviderError {
    pub fn new(failure: ProviderFailure, diagnostic: impl Into<String>) -> Self {
        Self {
            failure,
            diagnostic: diagnostic.into(),
        }
    }

    pub fn diagnostic(&self) -> &str {
        &self.diagnostic
    }

    pub fn from_diagnostic(diagnostic: impl Into<String>) -> Self {
        let diagnostic = diagnostic.into();
        Self::new(classify_provider_failure(&diagnostic), diagnostic)
    }

    pub fn internal(diagnostic: impl Into<String>) -> Self {
        Self::new(ProviderFailure::Internal, diagnostic)
    }
}

impl std::fmt::Display for ProviderError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.diagnostic)
    }
}

impl std::error::Error for ProviderError {}

impl From<anyhow::Error> for ProviderError {
    fn from(error: anyhow::Error) -> Self {
        Self::from_diagnostic(format!("{error:#}"))
    }
}

pub type ProviderResult<T> = std::result::Result<T, ProviderError>;

pub fn classify_provider_failure(message: &str) -> ProviderFailure {
    let lowered = message.to_ascii_lowercase();
    if lowered.contains("rate limit") || lowered.contains("429") {
        ProviderFailure::RateLimited {
            retry_after_seconds: retry_after_seconds(&lowered).unwrap_or(60),
        }
    } else if lowered.contains("host credential")
        || lowered.contains("credential access denied")
        || lowered.contains("credentials access denied")
        || lowered.contains("permission denied")
    {
        ProviderFailure::HostCredentialDenied
    } else if (lowered.contains("incompatible") && lowered.contains("protocol"))
        || lowered.contains("protocol version")
        || lowered.contains("unsupported protocol")
    {
        ProviderFailure::IncompatibleProtocol
    } else if lowered.contains("auth")
        || lowered.contains("login")
        || lowered.contains("credential")
    {
        ProviderFailure::Authentication
    } else {
        ProviderFailure::Unavailable
    }
}

fn retry_after_seconds(message: &str) -> Option<u64> {
    let (_, suffix) = message.split_once("retry-after=")?;
    suffix
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .parse()
        .ok()
}

/// Produce a safe recovery status without exposing provider diagnostics.
pub fn provider_recovery_message(failure: &ProviderFailure, can_switch: bool) -> String {
    let action = can_switch
        .then_some(" A project approver may switch providers.")
        .unwrap_or("");
    match failure {
        ProviderFailure::Authentication => format!("Provider authentication is required.{action}"),
        ProviderFailure::HostCredentialDenied => format!(
            "Provider credentials are unavailable to the runner host; restore host credential access.{action}"
        ),
        ProviderFailure::IncompatibleProtocol => format!(
            "Provider protocol is incompatible with this Fluent build; update the provider or Fluent.{action}"
        ),
        ProviderFailure::Unavailable => {
            format!("Provider is unavailable; queued replies are preserved.{action}")
        }
        ProviderFailure::RateLimited {
            retry_after_seconds,
        } => {
            format!("Provider is rate-limited; retrying in {retry_after_seconds} seconds.{action}")
        }
        ProviderFailure::Internal => "The provider runner encountered an internal error.".into(),
    }
}

/// Provider-neutral turn boundary with an explicit filesystem authority scope.
pub trait ConversationProvider: Send {
    /// Reports the provider implementation that owns persisted session ids.
    fn name(&self) -> &str {
        "codex"
    }
    fn select(&mut self, provider: &str) -> ProviderResult<()> {
        if self.name() != provider {
            return Err(ProviderError::new(
                ProviderFailure::Unavailable,
                format!("conversation provider {provider} is not available"),
            ));
        }
        Ok(())
    }
    fn start(
        &mut self,
        action_id: &str,
        scope: &ProviderScope,
        contribution: &str,
        actor: &str,
    ) -> ProviderResult<(String, String)>;

    fn resume(
        &mut self,
        action_id: &str,
        session: &str,
        scope: &ProviderScope,
        contribution: &str,
        actor: &str,
    ) -> ProviderResult<String>;

    fn prepare_session(&mut self, _scope: &ProviderScope) -> ProviderResult<Option<String>> {
        Ok(None)
    }

    fn recover_turn(
        &mut self,
        _action_id: &str,
        _session: &str,
        _scope: &ProviderScope,
    ) -> ProviderResult<ProviderTurnRecovery> {
        Ok(ProviderTurnRecovery::Missing)
    }

    fn run_turn(
        &mut self,
        action_id: &str,
        session: Option<&str>,
        scope: &ProviderScope,
        contribution: &str,
        actor: &str,
        accepted: &mut dyn FnMut(&str) -> Result<()>,
    ) -> ProviderResult<(Option<String>, String)> {
        let (new_session, reply) = if let Some(session) = session {
            (
                None,
                self.resume(action_id, session, scope, contribution, actor)?,
            )
        } else {
            let (session, reply) = self.start(action_id, scope, contribution, actor)?;
            (Some(session), reply)
        };
        accepted(action_id).map_err(|error| ProviderError::internal(format!("{error:#}")))?;
        Ok((new_session, reply))
    }

    fn send(&mut self, _session: &str, _message: &str) -> ProviderResult<()> {
        Err(ProviderError::new(
            ProviderFailure::Unavailable,
            "conversation provider does not support direct messages",
        ))
    }

    fn interrupt(&mut self, _session: &str) -> ProviderResult<()> {
        Err(ProviderError::new(
            ProviderFailure::Unavailable,
            "conversation provider does not support interruption",
        ))
    }

    fn structured_events(&mut self, _session: &str) -> ProviderResult<Vec<ProviderEvent>> {
        Ok(Vec::new())
    }

    fn ready(&self) -> ProviderResult<()> {
        Ok(())
    }
}

/// A Work Item approved as part of one conversation Slice plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlannedWork {
    pub id: String,
    pub title: String,
}

impl PlannedWork {
    pub fn new(id: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
        }
    }
}

/// Stable execution request submitted to the scheduler-owned boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkExecutionRequest {
    pub version: u32,
    pub id: String,
    pub slice_id: String,
    pub work_item_id: String,
    pub authorized_by: String,
    pub created_at: String,
}

/// Scheduler acknowledgement that is safe to report in a conversation thread.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkQueueOutcome {
    Queued { request_id: String },
    AlreadyQueued { request_id: String },
}

/// Typed scheduler boundary. Implementations own queue storage and capacity;
/// Conversation coordination only submits a frozen request.
pub trait WorkScheduler {
    fn submit(&self, request: &WorkExecutionRequest) -> Result<WorkQueueOutcome>;
}

/// Compare complete opaque action and revision identities at the approval boundary.
pub fn approval_matches_revision(
    action: &str,
    revision: &str,
    offered_action: &str,
    offered_revision: &str,
) -> bool {
    action == offered_action && revision == offered_revision
}

/// An immutable planning artifact accepted for review in a Fluent thread.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactRevision {
    pub id: String,
    pub kind: String,
    pub markdown: String,
    pub sha256: String,
}

impl ArtifactRevision {
    pub fn new(kind: impl Into<String>, markdown: impl Into<String>) -> Self {
        let kind = kind.into();
        let markdown = markdown.into();
        let sha256 = hex_digest(markdown.as_bytes());
        Self {
            id: format!("{kind}:{sha256}"),
            kind,
            markdown,
            sha256,
        }
    }
}

fn hex_digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub fn redact_secrets(message: &str) -> String {
    let mut redacted = String::with_capacity(message.len());
    let mut remainder = message;
    while let Some(start) = [remainder.find("xapp-"), remainder.find("xoxb-")]
        .into_iter()
        .flatten()
        .min()
    {
        redacted.push_str(&remainder[..start]);
        redacted.push_str("[REDACTED]");
        let token = &remainder[start..];
        let end = token
            .char_indices()
            .find(|(_, character)| {
                !character.is_ascii_alphanumeric() && *character != '-' && *character != '_'
            })
            .map(|(index, _)| index)
            .unwrap_or(token.len());
        remainder = &token[end..];
    }
    redacted.push_str(remainder);
    redacted
}

pub fn sanitize_provider_reply(reply: &str, checkout: &Path) -> String {
    let reply = redact_secrets(reply);
    let configured_root = checkout.to_string_lossy().trim_end_matches('/').to_owned();
    let canonical_root = checkout
        .canonicalize()
        .ok()
        .map(|path| path.to_string_lossy().trim_end_matches('/').to_owned());
    let mut roots = vec![configured_root];
    if let Some(root) = canonical_root
        && !roots.contains(&root)
    {
        roots.push(root);
    }
    roots.sort_by_key(|root| std::cmp::Reverse(root.len()));

    reply
        .split_inclusive(char::is_whitespace)
        .map(|piece| {
            let body_len = piece.trim_end_matches(char::is_whitespace).len();
            let (body, whitespace) = piece.split_at(body_len);
            let Some(slash) = body.char_indices().find_map(|(index, character)| {
                if character != '/' {
                    return None;
                }
                let begins_absolute_path = index == 0
                    || body[..index].chars().next_back().is_some_and(|character| {
                        !character.is_alphanumeric() && !"/\\._-".contains(character)
                    });
                begins_absolute_path.then_some(index)
            }) else {
                return piece.to_owned();
            };
            let prefix = &body[..slash];
            if prefix.ends_with("http:") || prefix.ends_with("https:") {
                return piece.to_owned();
            }
            let candidate_with_punctuation = &body[slash..];
            let candidate = candidate_with_punctuation
                .trim_end_matches([')', ']', '}', '>', ',', ';', '\'', '"', '`']);
            let punctuation = &candidate_with_punctuation[candidate.len()..];
            let replacement = roots
                .iter()
                .find_map(|root| {
                    candidate.strip_prefix(root).and_then(|suffix| {
                        if suffix.is_empty() {
                            Some(".".to_owned())
                        } else {
                            suffix.strip_prefix('/').map(ToOwned::to_owned)
                        }
                    })
                })
                .unwrap_or_else(|| "[REDACTED_PATH]".to_owned());
            format!("{prefix}{replacement}{punctuation}{whitespace}")
        })
        .collect()
}
