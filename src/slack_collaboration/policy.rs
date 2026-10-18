use super::{
    ApprovalOutcome, ApprovalRecord, Checkout, FluentThread, InboundEnvelope, ParticipantAuthority,
    PendingAction, PendingActionKind, RunnerConfig,
};

pub(super) fn participant_can_contribute(
    config: &RunnerConfig,
    thread: &FluentThread,
    event: &InboundEnvelope,
) -> bool {
    match thread.project_label.as_deref() {
        Some(project_label) => config
            .projects
            .get(project_label)
            .is_some_and(|project| project.allowed_participants.contains(&event.user)),
        None => match default_project(config, &event.channel) {
            Some(project_label) => config
                .projects
                .get(project_label)
                .is_some_and(|project| project.allowed_participants.contains(&event.user)),
            None => config
                .projects
                .values()
                .any(|project| project.allowed_participants.contains(&event.user)),
        },
    }
}

pub(super) fn available_projects(
    config: &RunnerConfig,
    participant: &str,
    checkout_is_available: impl Fn(&Checkout) -> bool,
) -> Vec<String> {
    config
        .projects
        .values()
        .filter(|project| {
            project.allowed_participants.contains(participant)
                && checkout_is_available(&project.checkout)
        })
        .map(|project| project.label.clone())
        .collect()
}

pub(super) fn project_authority(
    config: &RunnerConfig,
    thread: &FluentThread,
) -> Option<ParticipantAuthority> {
    thread
        .project_label
        .as_deref()
        .and_then(|label| config.projects.get(label))
        .map(|project| ParticipantAuthority {
            project_label: project.label.clone(),
            approvers: project.project_approvers.clone(),
        })
}

/// Identify whether an inbound event answers the pending approval question.
pub(super) fn is_approval_response(thread: &FluentThread, event: &InboundEnvelope) -> bool {
    let Some(action) = &thread.pending_action else {
        return false;
    };
    let answer = event.text.trim().to_ascii_lowercase();
    ((event.approval_action_id.is_some() || event.approval_artifact_revision.is_some())
        && event.text == "fluent_approval")
        || answer == "y"
        || answer == "yes"
        || answer == action.id.to_ascii_lowercase()
        || action
            .labels
            .iter()
            .any(|label| label.eq_ignore_ascii_case(&answer))
        || is_explicit_action_request(&action.kind, &answer)
}

fn is_explicit_action_request(kind: &PendingActionKind, answer: &str) -> bool {
    let verb = match kind {
        PendingActionKind::Planning => "plan",
        PendingActionKind::Execution => "execute",
        PendingActionKind::Resume => "resume",
        PendingActionKind::Landing => "land",
    };
    let answer = answer
        .trim_end_matches(|character| character == '.' || character == '!')
        .trim();
    [
        verb.to_owned(),
        format!("{verb} it"),
        format!("{verb} this"),
        format!("{verb} this work"),
        format!("please {verb}"),
        format!("please {verb} it"),
        format!("please {verb} this"),
        format!("please {verb} this work"),
    ]
    .iter()
    .any(|request| request == answer)
}

/// Evaluate a pending approval using only the thread, an authority snapshot,
/// and the approval attempt. The caller owns configuration loading and effects.
pub(super) fn evaluate_approval(
    thread: &mut FluentThread,
    authority: Option<&ParticipantAuthority>,
    actor: &str,
    event_id: &str,
    accepted_at: &str,
    recovered: bool,
    approval_event: Option<&InboundEnvelope>,
) -> ApprovalOutcome {
    let Some(action) = thread.pending_action.clone() else {
        return ApprovalOutcome::NoPendingAction;
    };
    if let Some(event) = approval_event
        && !approval_matches_presented_action(&action, event, recovered)
    {
        return ApprovalOutcome::Stale {
            current_action_id: action.id.clone(),
        };
    }
    if let Some(record) = thread
        .approvals
        .iter()
        .find(|record| record.event_id == event_id)
    {
        return ApprovalOutcome::Accepted(record.clone());
    }
    let Some(authority) = authority.filter(|authority| {
        authority.project_label == thread.project_label.as_deref().unwrap_or_default()
    }) else {
        return ApprovalOutcome::WaitingForApprover;
    };
    if !authority.approvers.contains(actor) {
        return ApprovalOutcome::WaitingForApprover;
    }
    if recovered && !action.kind.is_delayed_safe() {
        thread.pending_question = Some(format!("fresh confirmation required: {}", action.id));
        return ApprovalOutcome::FreshConfirmationRequired;
    }
    let record = ApprovalRecord {
        project_label: authority.project_label.clone(),
        thread_id: thread.id.clone(),
        pending_action_id: action.id,
        action_kind: action.kind,
        artifact_revision: action.artifact_revision,
        presented_at: action.presented_at,
        actor: actor.into(),
        accepted_at: accepted_at.into(),
        event_id: event_id.into(),
    };
    thread.approvals.push(record.clone());
    thread.pending_action = None;
    thread.pending_question = None;
    ApprovalOutcome::Accepted(record)
}

pub(super) fn approval_matches_presented_action(
    action: &PendingAction,
    event: &InboundEnvelope,
    recovered: bool,
) -> bool {
    match (&event.approval_action_id, &event.approval_artifact_revision) {
        (Some(id), Some(revision)) => crate::conversation::approval_matches_revision(
            &action.id,
            &action.artifact_revision,
            id,
            revision,
        ),
        (None, None) if recovered => {
            slack_timestamp_is_at_or_after(&event.timestamp, &action.presented_at)
        }
        (None, None) => true,
        _ => false,
    }
}

fn slack_timestamp_is_at_or_after(value: &str, threshold: &str) -> bool {
    fn parts(value: &str) -> Option<(&str, &str)> {
        let (seconds, fraction) = value.split_once('.')?;
        if seconds.is_empty()
            || fraction.is_empty()
            || !seconds.bytes().all(|byte| byte.is_ascii_digit())
            || !fraction.bytes().all(|byte| byte.is_ascii_digit())
        {
            return None;
        }
        Some((seconds, fraction))
    }
    let Some((value_seconds, value_fraction)) = parts(value) else {
        return false;
    };
    let Some((threshold_seconds, threshold_fraction)) = parts(threshold) else {
        return false;
    };
    let Ok(value_seconds) = value_seconds.parse::<u64>() else {
        return false;
    };
    let Ok(threshold_seconds) = threshold_seconds.parse::<u64>() else {
        return false;
    };
    if value_seconds != threshold_seconds {
        return value_seconds > threshold_seconds;
    }
    let width = value_fraction.len().max(threshold_fraction.len());
    format!("{value_fraction:0<width$}") >= format!("{threshold_fraction:0<width$}")
}

pub(super) fn default_project<'a>(config: &'a RunnerConfig, destination: &str) -> Option<&'a str> {
    config
        .channel_defaults
        .get(destination)
        .or_else(|| config.direct_message_defaults.get(destination))
        .map(String::as_str)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::slack_collaboration::ThreadState;

    #[test]
    fn recovered_consequential_approval_requires_fresh_confirmation() {
        let action = PendingAction {
            id: "approve-landing".into(),
            kind: PendingActionKind::Landing,
            artifact_revision: "revision-1".into(),
            presented_at: "1.0".into(),
            labels: BTreeSet::from(["approve".into()]),
        };
        let mut thread = FluentThread {
            id: "1.0".into(),
            channel: "C1".into(),
            state: ThreadState::Active,
            project_label: Some("app".into()),
            provider: Some("codex".into()),
            provider_failure: None,
            provider_session: None,
            slice: None,
            planning_stage: None,
            linked_work: None,
            pending_question: Some(action.id.clone()),
            artifact_revisions: Vec::new(),
            pending_artifact_revision: None,
            linked_thread_intent: None,
            conversation_inputs: Vec::new(),
            pending_action: Some(action.clone()),
            approvals: Vec::new(),
            project_initialization_approvals: Vec::new(),
            processed_events: BTreeSet::new(),
            last_connection: None,
            first_reply_timestamp: None,
            pending_turn: None,
            queued_contributions: Vec::new(),
            conversation: Vec::new(),
        };
        let event = InboundEnvelope {
            id: "E2".into(),
            acknowledgement_id: "E2".into(),
            channel: "C1".into(),
            timestamp: "2.0".into(),
            thread_timestamp: Some("1.0".into()),
            user: "U1".into(),
            text: "fluent_approval".into(),
            change: Default::default(),
            approval_action_id: Some(action.id.clone()),
            approval_artifact_revision: Some(action.artifact_revision.clone()),
        };
        let authority = ParticipantAuthority {
            project_label: "app".into(),
            approvers: BTreeSet::from(["U1".into()]),
        };

        let outcome = evaluate_approval(
            &mut thread,
            Some(&authority),
            &event.user,
            &event.id,
            "3.0",
            true,
            Some(&event),
        );

        assert_eq!(outcome, ApprovalOutcome::FreshConfirmationRequired);
        assert!(thread.approvals.is_empty());
        assert_eq!(thread.pending_action, Some(action));
        assert!(
            thread
                .pending_question
                .as_deref()
                .is_some_and(|question| question.contains("fresh confirmation"))
        );
    }
}
