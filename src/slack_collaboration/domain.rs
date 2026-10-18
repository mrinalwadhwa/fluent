use std::collections::BTreeSet;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

/// Largest Markdown payload sent in one Slack artifact delivery.
pub const SLACK_MESSAGE_CONTENT_LIMIT: usize = 4_000;

pub use crate::conversation::ArtifactRevision;

pub fn split_artifact_markdown(markdown: &str, limit: usize) -> Vec<String> {
    assert!(limit > 0, "Slack artifact content limit must be positive");
    if markdown.is_empty() {
        return vec![String::new()];
    }
    let mut parts = Vec::new();
    let mut start = 0;
    while start < markdown.len() {
        let mut end = (start + limit).min(markdown.len());
        while end < markdown.len() && !markdown.is_char_boundary(end) {
            end -= 1;
        }
        parts.push(markdown[start..end].to_owned());
        start = end;
    }
    parts
}

pub fn reassemble_artifact_parts(parts: &[&OutboundDelivery]) -> Result<String> {
    let mut ordered: Vec<_> = parts.iter().collect();
    ordered.sort_by(|left, right| left.id.cmp(&right.id));
    ordered
        .into_iter()
        .map(|part| {
            part.text
                .split_once("\n\n")
                .map(|(_, markdown)| markdown)
                .context("artifact delivery lacks a revision header")
        })
        .collect()
}

/// A persisted, idempotent Slack event. `id` is Slack's event id, never a
/// generated retry id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboundEnvelope {
    pub id: String,
    #[serde(default)]
    pub acknowledgement_id: String,
    pub channel: String,
    pub timestamp: String,
    pub thread_timestamp: Option<String>,
    pub user: String,
    pub text: String,
    #[serde(default)]
    pub change: InboundChange,
    /// Immutable identity supplied by an approval interaction control.
    #[serde(default)]
    pub approval_action_id: Option<String>,
    #[serde(default)]
    pub approval_artifact_revision: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum InboundChange {
    #[default]
    Message,
    Edit,
    Delete,
}

impl InboundEnvelope {
    pub fn thread_id(&self) -> String {
        self.thread_timestamp
            .clone()
            .unwrap_or_else(|| self.timestamp.clone())
    }

    pub fn is_top_level_mention(&self) -> bool {
        self.thread_timestamp.is_none() && self.text.contains("@Fluent")
    }

    pub fn validate_thread_identity(&self) -> Result<()> {
        validate_thread_identity(&self.thread_id())
    }
}

pub(super) fn validate_thread_identity(value: &str) -> Result<()> {
    if value.is_empty()
        || value == "."
        || value == ".."
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b'.')
    {
        bail!("Slack thread identity is not a canonical timestamp")
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutboundDelivery {
    pub id: String,
    pub channel: String,
    pub thread_timestamp: String,
    pub text: String,
    pub delivered: bool,
    #[serde(default)]
    pub approval_control: Option<ApprovalControl>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update_timestamp: Option<String>,
}

/// A revision-bound Slack button that authorizes one pending action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalControl {
    pub action_id: String,
    pub artifact_revision: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ThreadState {
    WaitingForProject,
    Active,
}

/// A lifecycle action that needs a project approver's current authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PendingActionKind {
    Planning,
    Execution,
    Resume,
    Landing,
}

impl PendingActionKind {
    pub fn is_delayed_safe(&self) -> bool {
        matches!(self, Self::Planning)
    }
}

/// The one approval question currently presented in a Fluent thread.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingAction {
    pub id: String,
    pub kind: PendingActionKind,
    pub artifact_revision: String,
    pub presented_at: String,
    #[serde(default)]
    pub labels: BTreeSet<String>,
}

/// A point-in-time project authority snapshot used to evaluate an approval.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParticipantAuthority {
    pub project_label: String,
    pub approvers: BTreeSet<String>,
}

/// A durable authorization record. Consumers, rather than this domain, perform
/// any filesystem, Work, publication, initialization, or landing effect.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalRecord {
    pub project_label: String,
    pub thread_id: String,
    pub pending_action_id: String,
    pub action_kind: PendingActionKind,
    pub artifact_revision: String,
    pub presented_at: String,
    pub actor: String,
    pub accepted_at: String,
    pub event_id: String,
}

/// Result of evaluating an approval without performing its host-side effect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalOutcome {
    Accepted(ApprovalRecord),
    WaitingForApprover,
    FreshConfirmationRequired,
    Stale { current_action_id: String },
    NoPendingAction,
}

/// Records who approved a reviewed initialization commit independently from
/// the Git author identity used to create that commit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectInitializationApproval {
    pub commit: String,
    pub reviewed_revision: String,
    pub slack_approver: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FluentThread {
    pub id: String,
    pub channel: String,
    pub state: ThreadState,
    pub project_label: Option<String>,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub provider_failure: Option<super::ProviderFailure>,
    pub provider_session: Option<String>,
    pub slice: Option<String>,
    pub planning_stage: Option<String>,
    pub linked_work: Option<String>,
    pub pending_question: Option<String>,
    #[serde(default)]
    pub artifact_revisions: Vec<ArtifactRevision>,
    #[serde(default)]
    pub pending_artifact_revision: Option<String>,
    #[serde(default)]
    pub linked_thread_intent: Option<String>,
    #[serde(default)]
    pub conversation_inputs: Vec<ConversationInput>,
    pub pending_action: Option<PendingAction>,
    #[serde(default)]
    pub approvals: Vec<ApprovalRecord>,
    #[serde(default)]
    pub project_initialization_approvals: Vec<ProjectInitializationApproval>,
    pub processed_events: BTreeSet<String>,
    pub last_connection: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_reply_timestamp: Option<String>,
    #[serde(default)]
    pub pending_turn: Option<PendingTurn>,
    #[serde(default)]
    pub queued_contributions: Vec<QueuedContribution>,
    #[serde(default)]
    pub conversation: Vec<QueuedContribution>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueuedContribution {
    pub action_id: String,
    #[serde(default)]
    pub source_timestamp: String,
    pub contribution: String,
    pub actor: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PendingTurnDispatchState {
    Runnable,
    InFlight,
    RateLimitedUntilDeadline,
    PausedForRecovery,
}

impl Default for PendingTurnDispatchState {
    fn default() -> Self {
        Self::Runnable
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingTurn {
    pub action_id: String,
    #[serde(default)]
    pub source_timestamp: String,
    pub contribution: String,
    pub actor: String,
    pub starts_session: bool,
    #[serde(default)]
    pub provider_turn_id: Option<String>,
    #[serde(default)]
    pub completed_reply: Option<String>,
    #[serde(default)]
    pub dispatch_state: PendingTurnDispatchState,
    #[serde(default)]
    pub retry_at_unix_seconds: Option<u64>,
    #[serde(default)]
    pub delivery_attempt: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum OperatorRecoveryKind {
    ProviderLogin { provider: String },
    CheckoutReconfirmation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct OperatorRecoveryAction {
    pub id: String,
    pub project: String,
    pub thread: String,
    pub kind: OperatorRecoveryKind,
}

impl FluentThread {
    pub(super) fn new(event: &InboundEnvelope) -> Self {
        Self {
            id: event.thread_id(),
            channel: event.channel.clone(),
            state: ThreadState::WaitingForProject,
            project_label: None,
            provider: None,
            provider_failure: None,
            provider_session: None,
            slice: None,
            planning_stage: None,
            linked_work: None,
            pending_question: None,
            artifact_revisions: Vec::new(),
            pending_artifact_revision: None,
            linked_thread_intent: None,
            conversation_inputs: Vec::new(),
            pending_action: None,
            approvals: Vec::new(),
            project_initialization_approvals: Vec::new(),
            processed_events: BTreeSet::new(),
            last_connection: None,
            first_reply_timestamp: None,
            pending_turn: None,
            queued_contributions: Vec::new(),
            conversation: Vec::new(),
        }
    }
}

/// Accepted provider context remains immutable even when Slack changes later.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversationInput {
    pub event_id: String,
    pub source_timestamp: String,
    pub actor: String,
    pub content: String,
    #[serde(default)]
    pub corrections: Vec<AttributedCorrection>,
    #[serde(default)]
    pub source_deleted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttributedCorrection {
    pub event_id: String,
    pub actor: String,
    pub content: String,
}
