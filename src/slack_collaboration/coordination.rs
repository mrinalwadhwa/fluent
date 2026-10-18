use super::*;
use crate::work_model::WorkModelStore;
use std::collections::{BTreeSet, VecDeque};
use std::path::Path;
use std::sync::Arc;

use crate::conversation::sanitize_provider_reply;

fn project_enablement_request(text: &str) -> bool {
    let normalized = text.to_ascii_lowercase();
    normalized.contains("enable project") || normalized.contains("enable an existing project")
}

fn enablement_goal(text: &str) -> Option<String> {
    text.split_once("goal:")
        .map(|(_, goal)| goal.trim().to_owned())
        .filter(|goal| !goal.is_empty())
}

fn parse_checkout_selection(text: &str) -> Option<(&str, SlackInitChoices)> {
    let fields = text.split_whitespace().collect::<Vec<_>>();
    let [
        "select",
        "checkout",
        selection,
        "profile",
        profile,
        "mode",
        mode,
        role_fields @ ..,
    ] = fields.as_slice()
    else {
        return None;
    };
    if role_fields.len() % 2 != 0 {
        return None;
    }
    let mut choices = SlackInitChoices {
        coder_profile: (*profile).into(),
        follow_up_mode: (*mode).into(),
        write_coder: None,
        write_model: None,
        write_effort: None,
        review_coder: None,
        review_model: None,
        review_effort: None,
    };
    for pair in role_fields.chunks_exact(2) {
        let field = match pair[0] {
            "write-coder" => &mut choices.write_coder,
            "write-model" => &mut choices.write_model,
            "write-effort" => &mut choices.write_effort,
            "review-coder" => &mut choices.review_coder,
            "review-model" => &mut choices.review_model,
            "review-effort" => &mut choices.review_effort,
            _ => return None,
        };
        if field.replace(pair[1].into()).is_some() {
            return None;
        }
    }
    if choices.coder_profile == "custom"
        && [
            &choices.write_coder,
            &choices.write_model,
            &choices.write_effort,
            &choices.review_coder,
            &choices.review_model,
            &choices.review_effort,
        ]
        .iter()
        .any(|field| field.is_none())
    {
        return None;
    }
    Some((selection, choices))
}

fn render_setup_choices(choices: &SlackInitChoices) -> String {
    let mut rendered = format!(
        "profile {}; follow-up mode {}",
        choices.coder_profile, choices.follow_up_mode
    );
    if let (Some(coder), Some(model), Some(effort)) = (
        &choices.write_coder,
        &choices.write_model,
        &choices.write_effort,
    ) {
        rendered.push_str(&format!(
            "; Writer: coder {coder}, model {model}, effort {effort}"
        ));
    }
    if let (Some(coder), Some(model), Some(effort)) = (
        &choices.review_coder,
        &choices.review_model,
        &choices.review_effort,
    ) {
        rendered.push_str(&format!(
            "; Reviewer: coder {coder}, model {model}, effort {effort}"
        ));
    }
    rendered
}

fn render_initialization_review(review: &InitializationReview) -> String {
    review
        .generated_changes
        .iter()
        .map(|change| {
            let before = change
                .before
                .as_deref()
                .map(String::from_utf8_lossy)
                .unwrap_or_else(|| "<absent>".into());
            let after = change
                .after
                .as_deref()
                .map(String::from_utf8_lossy)
                .unwrap_or_else(|| "<deleted>".into());
            format!(
                "{} ({} -> {})\n--- before\n{}\n+++ after\n{}",
                change.path,
                change.before_mode.as_deref().unwrap_or("absent"),
                change.after_mode.as_deref().unwrap_or("deleted"),
                before,
                after
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Fair, bounded admission for provider turns. A thread may hold at most one
/// slot, and waiting threads are admitted in first-ready order.
#[derive(Debug)]
pub struct FairTurnPool {
    capacity: usize,
    active: BTreeSet<String>,
    waiting: VecDeque<String>,
}

impl FairTurnPool {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            active: BTreeSet::new(),
            waiting: VecDeque::new(),
        }
    }

    pub fn request(&mut self, thread: impl Into<String>) -> bool {
        let thread = thread.into();
        if self.active.contains(&thread) || self.waiting.contains(&thread) {
            return false;
        }
        if self.active.len() < self.capacity {
            self.active.insert(thread);
            true
        } else {
            self.waiting.push_back(thread);
            false
        }
    }

    pub fn finish(&mut self, thread: &str) -> Option<String> {
        self.active.remove(thread);
        if let Some(next) = self.waiting.pop_front() {
            self.active.insert(next.clone());
            Some(next)
        } else {
            None
        }
    }
}

/// Preserve Slack ordering and attribution when several replies form one turn.
pub fn coalesce_contributions(replies: impl IntoIterator<Item = (String, String)>) -> String {
    replies
        .into_iter()
        .map(|(actor, text)| format!("Slack user {actor}: {text}"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[derive(Deserialize)]
struct PlanApprovalCommand {
    slice_id: String,
    planned_work: Vec<PlannedWork>,
}

struct GitHubIntegrations<'a> {
    github: &'a mut dyn GitHubReviewClient,
    rework: &'a dyn CandidateReworkBoundary,
}

/// Coordinates one local runner. File state is written before external effects.
pub struct CollaborationRunner<'a, P, S, C = FileCollaborationStore> {
    pub(super) home: &'a Path,
    pub config: RunnerConfig,
    provider: P,
    slack: S,
    store: C,
    recovering_event: bool,
    clock: Arc<dyn Fn() -> u64 + Send + Sync>,
    turn_pool: FairTurnPool,
    project_discovery_roots: Vec<ProjectDiscoveryRoot>,
    configured_init: Option<Box<dyn ConfiguredInitBoundary + 'a>>,
}

impl<'a, P: ConversationProvider, S: SlackTransport>
    CollaborationRunner<'a, P, S, FileCollaborationStore>
{
    pub fn new(home: &'a Path, config: RunnerConfig, provider: P, slack: S) -> Self {
        Self::new_with_turn_capacity(home, config, provider, slack, 4)
    }

    pub fn new_with_turn_capacity(
        home: &'a Path,
        config: RunnerConfig,
        provider: P,
        slack: S,
        capacity: usize,
    ) -> Self {
        Self::with_store_and_turn_capacity(
            home,
            config,
            provider,
            slack,
            FileCollaborationStore::new(home),
            capacity,
        )
    }
}

impl<'a, P: ConversationProvider, S: SlackTransport, C: CollaborationStore>
    CollaborationRunner<'a, P, S, C>
{
    pub fn with_store(
        home: &'a Path,
        config: RunnerConfig,
        provider: P,
        slack: S,
        store: C,
    ) -> Self {
        Self::with_store_and_turn_capacity(home, config, provider, slack, store, 4)
    }

    pub fn with_store_and_turn_capacity(
        home: &'a Path,
        config: RunnerConfig,
        provider: P,
        slack: S,
        store: C,
        capacity: usize,
    ) -> Self {
        Self {
            home,
            config,
            provider,
            slack,
            store,
            recovering_event: false,
            clock: Arc::new(|| {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs()
            }),
            turn_pool: FairTurnPool::new(capacity),
            project_discovery_roots: Vec::new(),
            configured_init: None,
        }
    }

    pub fn configure_project_enablement(
        &mut self,
        roots: Vec<ProjectDiscoveryRoot>,
        boundary: Box<dyn ConfiguredInitBoundary + 'a>,
    ) {
        self.project_discovery_roots = roots;
        self.configured_init = Some(boundary);
    }
    pub fn provider(&self) -> &P {
        &self.provider
    }
    pub fn provider_mut(&mut self) -> &mut P {
        &mut self.provider
    }
    pub fn slack(&self) -> &S {
        &self.slack
    }
    pub fn slack_mut(&mut self) -> &mut S {
        &mut self.slack
    }

    #[doc(hidden)]
    pub fn set_clock_for_test(&mut self, clock: impl Fn() -> u64 + Send + Sync + 'static) {
        self.clock = Arc::new(clock);
    }

    /// Store and present an immutable planning revision before it can advance.
    pub fn present_planning_artifact(
        &mut self,
        thread_id: &str,
        kind: &str,
        markdown: &str,
    ) -> Result<ArtifactRevision> {
        let mut thread = self
            .store
            .load_thread(&self.config, thread_id)?
            .context("Fluent thread is unavailable")?;
        self.present_planning_artifact_for_thread(&mut thread, kind, markdown)
    }

    fn present_planning_artifact_for_thread(
        &mut self,
        thread: &mut FluentThread,
        kind: &str,
        markdown: &str,
    ) -> Result<ArtifactRevision> {
        if let Some(work_item_id) = thread.linked_work.as_deref() {
            WorkModelStore::new(&self.bound_project(thread)?.checkout.path)
                .read_work_item(work_item_id)?
                .ensure_planning_revision_can_advance(kind)?;
        }
        let revision = ArtifactRevision::new(kind, markdown);
        if !thread
            .artifact_revisions
            .iter()
            .any(|existing| existing.id == revision.id)
        {
            thread.artifact_revisions.push(revision.clone());
        }
        thread.pending_artifact_revision = Some(revision.id.clone());
        thread.planning_stage = Some(revision.kind.clone());
        thread.pending_question = Some(format!(
            "approve {} revision {}",
            revision.kind, revision.id
        ));
        self.store.save_thread(&self.config, thread)?;
        let parts = Self::artifact_delivery_parts(&revision);
        let part_count = parts.len();
        let part_width = part_count.to_string().len();
        for (index, part) in parts.into_iter().enumerate() {
            self.store.persist_outbox(OutboundDelivery {
                id: format!(
                    "{}:artifact:{}:{:0part_width$}",
                    thread.id, revision.id, index
                ),
                channel: thread.channel.clone(),
                thread_timestamp: thread.id.clone(),
                text: format!(
                    "Planning revision {} ({}/{part_count})\n\n{part}",
                    revision.id,
                    index + 1
                ),
                delivered: false,
                approval_control: None,
                update_timestamp: None,
            })?;
        }
        self.deliver_pending()?;
        Ok(revision)
    }

    /// Advance only the exact revision currently waiting for approval.
    pub fn approve_planning_revision(
        &mut self,
        thread_id: &str,
        revision_id: &str,
        actor: &str,
    ) -> Result<()> {
        let mut thread = self
            .store
            .load_thread(&self.config, thread_id)?
            .context("Fluent thread is unavailable")?;
        if !self
            .bound_project(&thread)?
            .project_approvers
            .contains(actor)
        {
            bail!("participant is not a project approver for this Fluent thread");
        }
        if thread.pending_artifact_revision.as_deref() != Some(revision_id) {
            bail!("approval must identify the revision waiting at the planning gate");
        }
        let revision = thread
            .artifact_revisions
            .iter()
            .find(|revision| revision.id == revision_id)
            .context("pending planning revision is unavailable")?;
        if thread.planning_stage.as_deref() != Some(revision.kind.as_str())
            || thread.pending_question.as_deref()
                != Some(&format!("approve {} revision {revision_id}", revision.kind))
        {
            bail!("planning lifecycle is not waiting for this artifact revision");
        }
        self.approve_work_item_planning_revision(&thread, revision)?;
        thread.pending_artifact_revision = None;
        thread.pending_question = None;
        self.store.save_thread(&self.config, &thread)
    }

    fn approve_work_item_planning_revision(
        &self,
        thread: &FluentThread,
        revision: &ArtifactRevision,
    ) -> Result<()> {
        let work_item_id = thread
            .linked_work
            .as_deref()
            .context("planning artifact is not bound to a Fluent Work Item")?;
        let checkout = &self.bound_project(thread)?.checkout.path;
        WorkModelStore::new(checkout)
            .mutate_work_item(work_item_id, |work_item| {
                work_item.approve_planning_revision(
                    &revision.kind,
                    &revision.id,
                    revision.markdown.clone(),
                )
            })
            .with_context(|| format!("approve planning Work Item {work_item_id}"))
    }

    /// Re-admit every durable provider turn whose retry deadline has passed.
    pub fn retry_due_turns(&mut self) -> Result<()> {
        self.config = self.current_authority_config()?;
        let now = (self.clock)();
        let thread_ids: Vec<_> = self.store.load_threads(&self.config)?.into_keys().collect();
        for thread_id in thread_ids {
            let Some(mut thread) = self.store.load_thread(&self.config, &thread_id)? else {
                continue;
            };
            let due = thread
                .pending_turn
                .as_ref()
                .and_then(|turn| turn.retry_at_unix_seconds)
                .is_some_and(|deadline| deadline <= now);
            if !due {
                continue;
            }
            if let Some(turn) = thread.pending_turn.as_mut() {
                turn.retry_at_unix_seconds = None;
                turn.dispatch_state = PendingTurnDispatchState::Runnable;
            }
            self.store.save_thread(&self.config, &thread)?;
            let event = InboundEnvelope {
                id: format!("{}:provider-retry:{now}", thread.id),
                acknowledgement_id: String::new(),
                channel: thread.channel.clone(),
                timestamp: thread.id.clone(),
                thread_timestamp: Some(thread.id.clone()),
                user: thread
                    .pending_turn
                    .as_ref()
                    .map(|turn| turn.actor.clone())
                    .unwrap_or_default(),
                text: String::new(),
                change: InboundChange::Message,
                approval_action_id: None,
                approval_artifact_revision: None,
            };
            self.complete_pending_turn(&event, &mut thread)?;
        }
        self.deliver_pending()
    }

    /// Switch an interrupted conversation and run its preserved turn through
    /// the selected provider. Scheduler Work is not represented or mutated at
    /// this boundary.
    pub fn switch_provider(&mut self, thread_id: &str, actor: &str, provider: &str) -> Result<()> {
        self.config = self.current_authority_config()?;
        let mut thread = self
            .store
            .load_thread(&self.config, thread_id)?
            .context("Fluent thread is unavailable")?;
        self.switch_provider_metadata(&mut thread, actor, provider)?;
        self.store.save_thread(&self.config, &thread)?;
        let event = InboundEnvelope {
            id: format!("{}:provider-switch:{}", thread.id, provider),
            acknowledgement_id: String::new(),
            channel: thread.channel.clone(),
            timestamp: thread.id.clone(),
            thread_timestamp: Some(thread.id.clone()),
            user: actor.into(),
            text: format!("[provider={provider}]"),
            change: InboundChange::Message,
            approval_action_id: None,
            approval_artifact_revision: None,
        };
        self.complete_pending_turn(&event, &mut thread)?;
        self.store.save_thread(&self.config, &thread)
    }

    /// Recheck the selected provider after the runner owner completes its
    /// scoped login flow, then resume the preserved turn.
    pub fn resume_after_provider_login(&mut self, thread_id: &str) -> Result<()> {
        self.config = self.current_authority_config()?;
        let mut thread = self
            .store
            .load_thread(&self.config, thread_id)?
            .context("Fluent thread is unavailable")?;
        self.resume_after_provider_login_metadata(&mut thread)?;
        let event = recovery_event(&thread, "provider-login");
        self.complete_pending_turn(&event, &mut thread)?;
        self.store.save_thread(&self.config, &thread)?;
        self.deliver_pending()
    }

    fn switch_provider_metadata(
        &self,
        thread: &mut FluentThread,
        actor: &str,
        provider: &str,
    ) -> Result<()> {
        let project = self.bound_project(&thread)?;
        if !project.project_approvers.contains(actor) {
            bail!("only a project approver may switch providers");
        }
        if thread.pending_turn.is_none() {
            bail!("only an interrupted Fluent thread may switch providers");
        }
        if !project.enabled_providers.contains(provider) {
            bail!("conversation provider is not enabled");
        }
        thread.provider = Some(provider.into());
        thread.provider_session = None;
        thread.provider_failure = None;
        if let Some(turn) = thread.pending_turn.as_mut() {
            turn.dispatch_state = PendingTurnDispatchState::Runnable;
            turn.retry_at_unix_seconds = None;
            turn.delivery_attempt = turn.delivery_attempt.saturating_add(1);
        }
        self.store.persist_outbox(OutboundDelivery {
            id: format!("{}:provider-switch:{}", thread.id, provider),
            channel: thread.channel.clone(),
            thread_timestamp: thread.id.clone(),
            text: format!(
                "Provider switched to {provider}; Fluent will resume from the durable Slice conversation."
            ),
            delivered: false,
            approval_control: None,
            update_timestamp: None,
        })
    }

    fn resume_after_provider_login_metadata(&mut self, thread: &mut FluentThread) -> Result<()> {
        let failure = thread
            .provider_failure
            .as_ref()
            .context("thread is not paused for provider recovery")?;
        if !matches!(
            failure,
            ProviderFailure::Authentication
                | ProviderFailure::HostCredentialDenied
                | ProviderFailure::Unavailable
        ) {
            bail!("provider failure requires a different recovery action");
        }
        let selected = self.authorized_thread_provider(thread)?;
        self.provider.select(&selected)?;
        let checkout = self.bound_project(thread)?.checkout.path.clone();
        let scope = ProviderScope {
            slice_draft: checkout.join(".fluent/work/slack/drafts").join(&thread.id),
            checkout,
        };
        if let Some(session) = self.provider.prepare_session(&scope)? {
            thread.provider_session = Some(session);
        }
        thread.provider_failure = None;
        let turn = thread
            .pending_turn
            .as_mut()
            .context("thread has no preserved provider turn")?;
        turn.dispatch_state = PendingTurnDispatchState::Runnable;
        turn.retry_at_unix_seconds = None;
        turn.delivery_attempt = turn.delivery_attempt.saturating_add(1);
        self.store.save_thread(&self.config, thread)
    }

    fn reconfirm_checkout_metadata(&self, thread: &mut FluentThread) -> Result<()> {
        if thread.provider_failure.is_some() {
            bail!("provider recovery must complete before checkout reconfirmation");
        }
        self.bound_project(thread)?;
        let turn = thread
            .pending_turn
            .as_mut()
            .context("thread has no preserved provider turn")?;
        if turn.dispatch_state != PendingTurnDispatchState::PausedForRecovery {
            bail!("thread is not paused for checkout recovery");
        }
        turn.dispatch_state = PendingTurnDispatchState::Runnable;
        turn.retry_at_unix_seconds = None;
        turn.delivery_attempt = turn.delivery_attempt.saturating_add(1);
        self.store.save_thread(&self.config, thread)
    }

    /// Persist first, then acknowledge exactly once, then process and send.
    pub fn ingest(&mut self, event: InboundEnvelope) -> Result<()> {
        self.ingest_at_scheduler_boundary(event, true, None, None)
    }

    /// Process Slack ingress through an explicitly supplied scheduler boundary.
    /// Production uses the local scheduler; embedders and tests can supply the
    /// same typed boundary directly.
    pub fn ingest_with_scheduler(
        &mut self,
        event: InboundEnvelope,
        scheduler: &dyn WorkScheduler,
    ) -> Result<()> {
        self.ingest_at_scheduler_boundary(event, true, Some(scheduler), None)
    }

    pub fn ingest_with_github(
        &mut self,
        event: InboundEnvelope,
        scheduler: Option<&dyn WorkScheduler>,
        github: &mut dyn GitHubReviewClient,
        rework: &dyn CandidateReworkBoundary,
    ) -> Result<()> {
        self.ingest_at_scheduler_boundary(
            event,
            true,
            scheduler,
            Some(GitHubIntegrations { github, rework }),
        )
    }

    fn ingest_at_scheduler_boundary(
        &mut self,
        event: InboundEnvelope,
        run_now: bool,
        scheduler: Option<&dyn WorkScheduler>,
        mut integrations: Option<GitHubIntegrations<'_>>,
    ) -> Result<()> {
        event.validate_thread_identity()?;
        self.config = self.current_authority_config()?;
        if let Some(thread) = self.store.load_thread(&self.config, &event.thread_id())?
            && thread.state == ThreadState::Active
            && !policy::participant_can_contribute(&self.config, &thread, &event)
        {
            if !event.acknowledgement_id.is_empty() {
                self.slack.acknowledge(&event.acknowledgement_id)?;
            }
            self.slack.respond_private(
                &event.channel,
                &event.user,
                "You cannot participate in this Fluent thread.",
            )?;
            return Ok(());
        }
        match self.store.persist_inbox(&self.config, &event)? {
            InboxState::Processed => {
                self.replay_processed_github_request(&event)?;
                return Ok(());
            }
            InboxState::Pending if !event.acknowledgement_id.is_empty() => {
                self.slack.acknowledge(&event.acknowledgement_id)?
            }
            InboxState::Pending => {}
        }
        self.process(
            event.clone(),
            run_now,
            scheduler,
            integrations.as_mut().map(|value| GitHubIntegrations {
                github: &mut *value.github,
                rework: value.rework,
            }),
        )?;
        self.store.mark_inbox_processed(&event.id)?;
        if self
            .store
            .load_thread(&self.config, &event.thread_id())?
            .is_some_and(|thread| thread.slice.is_some())
        {
            self.report_thread_work(&event.thread_id())?;
            if let Some(integrations) = integrations.as_mut() {
                self.reconcile_thread_github(&event.thread_id(), &mut *integrations.github)?;
            }
        }
        self.deliver_pending()
    }

    fn replay_processed_github_request(&mut self, event: &InboundEnvelope) -> Result<()> {
        let Some(candidate_id) = github_pull_request_candidate(event.text.trim()) else {
            return Ok(());
        };
        let Some(thread) = self.store.load_thread(&self.config, &event.thread_id())? else {
            return Ok(());
        };
        let Some(slice_id) = thread.slice.as_deref() else {
            return Ok(());
        };
        let checkout = self
            .configured_bound_project(&thread)?
            .checkout
            .path
            .clone();
        let Some(publication) = WorkControl::new(checkout)
            .slice(slice_id)?
            .and_then(|slice| slice.github_publication)
        else {
            return Ok(());
        };
        let candidate_was_published = publication.candidate.candidate_id == candidate_id
            || publication
                .candidate_history
                .iter()
                .any(|candidate| candidate.candidate_id == candidate_id);
        let Some(url) = publication
            .pull_request_url
            .filter(|_| candidate_was_published)
        else {
            return Ok(());
        };

        if !event.acknowledgement_id.is_empty() {
            self.slack.acknowledge(&event.acknowledgement_id)?;
        }
        self.store.persist_outbox(OutboundDelivery {
            id: format!("{}:github-pr-existing", event.id),
            channel: thread.channel,
            thread_timestamp: thread.id,
            text: format!(
                "GitHub pull request for Merge Candidate {candidate_id} already exists: {url}"
            ),
            delivered: false,
            approval_control: None,
            update_timestamp: None,
        })?;
        self.deliver_pending()
    }

    fn artifact_delivery_parts(revision: &ArtifactRevision) -> Vec<String> {
        let mut part_count = 1;
        loop {
            let header = format!(
                "Planning revision {} ({part_count}/{part_count})\n\n",
                revision.id
            );
            let payload_limit = SLACK_MESSAGE_CONTENT_LIMIT
                .checked_sub(header.len())
                .expect("artifact revision header must fit in Slack delivery");
            let parts = split_artifact_markdown(&revision.markdown, payload_limit);
            if parts.len() == part_count {
                return parts;
            }
            part_count = parts.len();
        }
    }
    pub fn reconnect(
        &mut self,
        history: impl IntoIterator<Item = InboundEnvelope>,
        connected_at: String,
    ) -> Result<()> {
        self.restore_connection(history, connected_at, true)
    }

    pub(super) fn restore_connection(
        &mut self,
        history: impl IntoIterator<Item = InboundEnvelope>,
        connected_at: String,
        report_interruption: bool,
    ) -> Result<()> {
        self.restore_connection_with_completion(history, connected_at, report_interruption, true)
    }

    fn restore_connection_with_completion(
        &mut self,
        history: impl IntoIterator<Item = InboundEnvelope>,
        connected_at: String,
        report_interruption: bool,
        run_now: bool,
    ) -> Result<()> {
        let mut events: Vec<_> = self
            .store
            .load_pending_inbox(&self.config)?
            .into_iter()
            .map(|event| (true, event))
            .collect();
        events.extend(history.into_iter().map(|event| (true, event)));
        events.sort_by(|(left_history, left), (right_history, right)| {
            left.timestamp
                .cmp(&right.timestamp)
                .then(left_history.cmp(right_history).then(left.id.cmp(&right.id)))
        });
        for (recovered, event) in events {
            self.recovering_event = recovered;
            self.ingest_at_scheduler_boundary(event.clone(), run_now, None, None)?;
            self.recovering_event = false;
            if recovered {
                self.store.save_recovery_cursor(
                    &event.channel,
                    &event.thread_id(),
                    &event.timestamp,
                )?;
            }
        }
        let thread_ids: Vec<_> = self.store.load_threads(&self.config)?.into_keys().collect();
        for id in thread_ids {
            let Some(mut thread) = self.store.load_thread(&self.config, &id)? else {
                continue;
            };
            thread.last_connection = Some(connected_at.clone());
            self.store.save_thread(&self.config, &thread)?;
            if thread.slice.is_some() {
                self.report_thread_work(&id)?;
                thread = self
                    .store
                    .load_thread(&self.config, &id)?
                    .context("Fluent thread disappeared during Work reconciliation")?;
            }
            self.queue_thread_status(
                &thread,
                report_interruption.then_some("Slack connection was interrupted."),
            )?;
        }
        self.deliver_pending()
    }

    pub fn recovery_targets(&self) -> Result<Vec<RecoveryTarget>> {
        self.store
            .load_threads(&self.config)?
            .into_values()
            .filter(|thread| thread.state == ThreadState::Active)
            .map(|thread| {
                let oldest = self
                    .store
                    .recovery_cursor(&thread.channel, &thread.id)?
                    .unwrap_or_else(|| thread.id.clone());
                Ok(RecoveryTarget {
                    channel: thread.channel,
                    thread_timestamp: thread.id,
                    oldest,
                })
            })
            .collect()
    }

    /// Reconcile every bound Slice during a healthy idle connection tick.
    pub fn poll_linked_work(&mut self) -> Result<()> {
        let thread_ids = self
            .store
            .load_threads(&self.config)?
            .into_values()
            .filter(|thread| thread.state == ThreadState::Active)
            .map(|thread| thread.id)
            .collect::<Vec<_>>();
        for thread_id in thread_ids {
            if self
                .store
                .load_thread(&self.config, &thread_id)?
                .is_some_and(|thread| thread.slice.is_some())
            {
                self.report_thread_work(&thread_id)?;
            }
        }
        Ok(())
    }

    pub fn poll_linked_work_with_github(
        &mut self,
        github: &mut dyn GitHubReviewClient,
    ) -> Result<()> {
        let thread_ids = self
            .store
            .load_threads(&self.config)?
            .into_values()
            .filter(|thread| thread.state == ThreadState::Active)
            .map(|thread| thread.id)
            .collect::<Vec<_>>();
        for thread_id in thread_ids {
            if self
                .store
                .load_thread(&self.config, &thread_id)?
                .is_none_or(|thread| thread.slice.is_none())
            {
                continue;
            }
            self.report_thread_work(&thread_id)?;
            self.reconcile_thread_github(&thread_id, github)?;
        }
        Ok(())
    }

    fn reconcile_thread_github(
        &mut self,
        thread_id: &str,
        github: &mut dyn GitHubReviewClient,
    ) -> Result<()> {
        let thread = self
            .store
            .load_thread(&self.config, thread_id)?
            .context("Fluent thread is unavailable")?;
        let project = self.bound_project(&thread)?;
        let slice_id = thread
            .slice
            .as_deref()
            .context("thread has no approved Slice")?;
        if WorkControl::new(&project.checkout.path)
            .slice(slice_id)?
            .and_then(|slice| slice.github_publication)
            .is_none_or(|publication| publication.pull_request_url.is_none())
        {
            return Ok(());
        }
        self.advance_github_review(thread_id, github)?;
        self.report_github_feedback(thread_id, github)?;
        Ok(())
    }
    pub fn deliver_pending(&mut self) -> Result<()> {
        let mut deliveries = self.store.load_outbox()?;
        for index in 0..deliveries.len() {
            if !deliveries[index].delivered {
                let posted_timestamp = if deliveries[index].update_timestamp.is_some() {
                    self.slack.update(&deliveries[index])?;
                    None
                } else {
                    Some(self.slack.post_with_identity(&deliveries[index])?)
                };
                deliveries[index].delivered = true;
                self.store.save_outbox(&deliveries)?;
                if let Some(timestamp) = posted_timestamp {
                    if let Some(mut thread) = self
                        .store
                        .load_thread(&self.config, &deliveries[index].thread_timestamp)?
                    {
                        if thread.first_reply_timestamp.is_none() {
                            thread.first_reply_timestamp = Some(timestamp);
                            self.store.save_thread(&self.config, &thread)?;
                        }
                    }
                }
            }
        }
        Ok(())
    }
    fn run_pending_thread(&mut self, thread_id: &str) -> Result<()> {
        let mut thread = self
            .store
            .load_thread(&self.config, thread_id)?
            .context("Fluent thread is unavailable")?;
        let turn = thread
            .pending_turn
            .as_ref()
            .context("thread has no pending provider turn")?;
        let event = InboundEnvelope {
            id: if turn.delivery_attempt == 0 {
                turn.action_id.clone()
            } else {
                format!("{}:retry:{}", turn.action_id, turn.delivery_attempt)
            },
            acknowledgement_id: String::new(),
            channel: thread.channel.clone(),
            timestamp: turn.source_timestamp.clone(),
            thread_timestamp: Some(thread.id.clone()),
            user: turn.actor.clone(),
            text: turn.contribution.clone(),
            change: InboundChange::Message,
            approval_action_id: None,
            approval_artifact_revision: None,
        };
        self.complete_pending_turn(&event, &mut thread)
    }
    fn process(
        &mut self,
        event: InboundEnvelope,
        run_now: bool,
        scheduler: Option<&dyn WorkScheduler>,
        mut integrations: Option<GitHubIntegrations<'_>>,
    ) -> Result<()> {
        let existing = self.store.load_thread(&self.config, &event.thread_id())?;
        if existing.is_none()
            && event.is_top_level_mention()
            && !policy::participant_can_contribute(
                &self.current_authority_config()?,
                &FluentThread::new(&event),
                &event,
            )
            && !(project_enablement_request(&event.text)
                && self.participant_can_setup_project(&event.user))
        {
            return self.slack.respond_private(
                &event.channel,
                &event.user,
                "You are not allowed to start Fluent work.",
            );
        }
        let mut thread = existing.unwrap_or_else(|| FluentThread::new(&event));
        if let Some(project_label) = &thread.project_label {
            self.store
                .bind_inbox_event(&self.config, &event.id, project_label)?;
        }
        let setup_event = project_enablement_request(&event.text)
            || decode_project_setup_state(thread.pending_question.as_deref())?.is_some();
        if !policy::participant_can_contribute(&self.config, &thread, &event)
            && !(setup_event && self.participant_can_setup_project(&event.user))
        {
            return self.slack.respond_private(
                &event.channel,
                &event.user,
                "You cannot participate in this Fluent thread.",
            );
        }
        match event.change {
            InboundChange::Edit => {
                if !thread.processed_events.insert(event.id.clone()) {
                    return Ok(());
                }
                return self.apply_edit(event, thread);
            }
            InboundChange::Delete => {
                if !thread.processed_events.insert(event.id.clone()) {
                    return Ok(());
                }
                return self.apply_deletion(event, thread);
            }
            InboundChange::Message => {}
        }
        if thread.state == ThreadState::Active
            && thread.pending_action.is_some()
            && policy::is_approval_response(&thread, &event)
        {
            match self.approve_thread_action(&mut thread, &event)? {
                ApprovalOutcome::WaitingForApprover => self.slack.respond_private(
                    &event.channel,
                    &event.user,
                    "Only a project approver may authorize this action.",
                )?,
                ApprovalOutcome::FreshConfirmationRequired => self.queue_reply(
                    &event,
                    "Fresh confirmation is required before Fluent can perform this action.",
                )?,
                ApprovalOutcome::Stale { current_action_id } => self.queue_reply(
                    &event,
                    &format!(
                        "That approval is stale. Current approval request: {current_action_id}."
                    ),
                )?,
                _ => {}
            }
            return self.store.save_thread(&self.config, &thread);
        }
        if thread.state == ThreadState::Active
            && event.thread_timestamp.is_some()
            && let Some(provider) = selected_provider(&event.text)
        {
            if run_now {
                return self.switch_provider(&thread.id, &event.user, provider);
            }
            self.switch_provider_metadata(&mut thread, &event.user, provider)?;
            return self.store.save_thread(&self.config, &thread);
        }
        if thread
            .pending_turn
            .as_ref()
            .is_some_and(|turn| turn.action_id == event.id)
        {
            return if run_now {
                self.complete_pending_turn(&event, &mut thread)
            } else {
                Ok(())
            };
        }
        if thread.state == ThreadState::WaitingForProject
            && (project_enablement_request(&event.text)
                || decode_project_setup_state(thread.pending_question.as_deref())?.is_some())
        {
            if !thread.processed_events.insert(event.id.clone()) {
                return Ok(());
            }
            if self.handle_project_enablement(&event, &mut thread)? {
                return self.store.save_thread(&self.config, &thread);
            }
        }
        if !thread.processed_events.insert(event.id.clone()) {
            return Ok(());
        }
        if thread.state == ThreadState::Active
            && event.thread_timestamp.is_some()
            && self.approve_revision_if_requested(&event, &mut thread)?
        {
        } else if thread.state == ThreadState::Active
            && event.thread_timestamp.is_some()
            && self.offer_linked_thread_if_requested(&event, &mut thread)?
        {
        } else if thread.state == ThreadState::Active
            && event.thread_timestamp.is_some()
            && thread.pending_artifact_revision.is_some()
        {
            self.queue_reply(
                &event,
                "Planning is waiting for approval of the presented revision.",
            )?;
        } else if thread.state == ThreadState::WaitingForProject && event.is_top_level_mention() {
            self.start_or_offer(&event, &mut thread, run_now)?;
        } else if thread.state == ThreadState::Active
            && event.thread_timestamp.is_some()
            && thread.pending_turn.is_some()
        {
            thread.queued_contributions.push(QueuedContribution {
                action_id: event.id.clone(),
                source_timestamp: event.timestamp.clone(),
                contribution: event.text.clone(),
                actor: event.user.clone(),
            });
            self.store.save_thread(&self.config, &thread)?;
            self.queue_queued_acknowledgement(&event)?;
        } else if thread.state == ThreadState::Active && event.thread_timestamp.is_some() {
            let command = event.text.trim();
            if let Some(payload) = command.strip_prefix("approve plan ") {
                let approval: PlanApprovalCommand = serde_json::from_str(payload)
                    .context("approve plan requires a valid JSON plan payload")?;
                self.approve_thread_plan(
                    &thread.id,
                    &event.user,
                    &approval.slice_id,
                    &approval.planned_work,
                )?;
                let mut refreshed = self
                    .store
                    .load_thread(&self.config, &thread.id)?
                    .context("Fluent thread disappeared after plan approval")?;
                refreshed.processed_events.insert(event.id);
                return self.store.save_thread(&self.config, &refreshed);
            }
            if command.eq_ignore_ascii_case("authorize execution") {
                if let Some(scheduler) = scheduler {
                    self.authorize_thread_execution(&thread.id, &event.user, scheduler)?;
                } else {
                    self.authorize_thread_execution_locally(&thread.id, &event.user)?;
                }
                let mut refreshed = self
                    .store
                    .load_thread(&self.config, &thread.id)?
                    .context("Fluent thread disappeared after execution approval")?;
                refreshed.processed_events.insert(event.id);
                return self.store.save_thread(&self.config, &refreshed);
            }
            if let Some(candidate_id) = github_pull_request_candidate(command) {
                let github = integrations
                    .as_mut()
                    .context("GitHub integration is unavailable")?;
                self.request_github_review(
                    &thread.id,
                    &event.user,
                    candidate_id,
                    &mut *github.github,
                )?;
                let mut refreshed = self
                    .store
                    .load_thread(&self.config, &thread.id)?
                    .context("Fluent thread disappeared after GitHub publication")?;
                refreshed.processed_events.insert(event.id);
                return self.store.save_thread(&self.config, &refreshed);
            }
            if let Some(feedback_ids) = selected_github_feedback(command) {
                let integrations = integrations
                    .as_mut()
                    .context("GitHub integration is unavailable")?;
                self.authorize_github_feedback_rework(
                    &thread.id,
                    &event.user,
                    &feedback_ids,
                    integrations.rework,
                )?;
                let mut refreshed = self
                    .store
                    .load_thread(&self.config, &thread.id)?
                    .context("Fluent thread disappeared after GitHub rework request")?;
                refreshed.processed_events.insert(event.id);
                return self.store.save_thread(&self.config, &refreshed);
            }
            if matches!(
                command.to_ascii_lowercase().as_str(),
                "status" | "work status"
            ) {
                self.store.save_thread(&self.config, &thread)?;
                let status = self.render_thread_status(&thread, None)?;
                return self.queue_reply(&event, &status);
            }
            if let Some(requested_work) = new_product_work_request(command) {
                let completed = if let Some(slice_id) = thread.slice.as_deref() {
                    let project = self.bound_project(&thread)?;
                    WorkControl::new(&project.checkout.path).slice_stage(slice_id)?
                        == SliceStage::Complete
                } else {
                    false
                };
                if completed {
                    self.store.save_thread(&self.config, &thread)?;
                    self.offer_linked_slice_for_new_work(&thread.id, &event.user, requested_work)?;
                    let mut refreshed = self
                        .store
                        .load_thread(&self.config, &thread.id)?
                        .context("Fluent thread disappeared after linked Slice offer")?;
                    refreshed.processed_events.insert(event.id);
                    return self.store.save_thread(&self.config, &refreshed);
                }
            }
            thread.pending_turn = Some(PendingTurn {
                action_id: event.id.clone(),
                source_timestamp: event.timestamp.clone(),
                contribution: event.text.clone(),
                actor: event.user.clone(),
                starts_session: false,
                provider_turn_id: None,
                completed_reply: None,
                dispatch_state: PendingTurnDispatchState::Runnable,
                retry_at_unix_seconds: None,
                delivery_attempt: 0,
            });
            self.store.save_thread(&self.config, &thread)?;
            if run_now {
                self.complete_pending_turn(&event, &mut thread)?;
            }
        }
        self.store.save_thread(&self.config, &thread)
    }

    /// Present exactly one revision-bound lifecycle action for approval.
    pub fn request_approval(&mut self, thread_id: &str, action: PendingAction) -> Result<()> {
        let mut thread = self
            .store
            .load_thread(&self.config, thread_id)?
            .context("Fluent thread is unavailable")?;
        if thread.state != ThreadState::Active
            || action.id.is_empty()
            || action.artifact_revision.is_empty()
        {
            bail!("pending action identity is incomplete");
        }
        thread.pending_question = Some(action.id.clone());
        let question = format!(
            "Waiting for a project approver to authorize {} at revision {}.",
            action.id, action.artifact_revision
        );
        thread.pending_action = Some(action);
        self.store.save_thread(&self.config, &thread)?;
        self.queue_thread_waiting(&thread, &question)?;
        self.deliver_pending()
    }

    /// Evaluate a direct approval attempt against current durable authority.
    pub fn approve_pending_action(
        &mut self,
        thread_id: &str,
        actor: &str,
        action_id: &str,
        artifact_revision: &str,
        event_id: &str,
        accepted_at: &str,
    ) -> Result<ApprovalOutcome> {
        let mut thread = self
            .store
            .load_thread(&self.config, thread_id)?
            .context("Fluent thread is unavailable")?;
        if let Some(record) = thread
            .approvals
            .iter()
            .find(|record| record.event_id == event_id)
        {
            return Ok(ApprovalOutcome::Accepted(record.clone()));
        }
        let Some(action) = thread.pending_action.as_ref() else {
            return Ok(ApprovalOutcome::NoPendingAction);
        };
        if action.id != action_id || action.artifact_revision != artifact_revision {
            return Ok(ApprovalOutcome::Stale {
                current_action_id: action.id.clone(),
            });
        }
        let authority = policy::project_authority(&self.current_authority_config()?, &thread);
        let outcome = policy::evaluate_approval(
            &mut thread,
            authority.as_ref(),
            actor,
            event_id,
            accepted_at,
            false,
            None,
        );
        self.store.save_thread(&self.config, &thread)?;
        Ok(outcome)
    }

    fn approve_thread_action(
        &mut self,
        thread: &mut FluentThread,
        event: &InboundEnvelope,
    ) -> Result<ApprovalOutcome> {
        let accepted_at = chrono::Utc::now().to_rfc3339();
        let authority = policy::project_authority(&self.current_authority_config()?, thread);
        Ok(policy::evaluate_approval(
            thread,
            authority.as_ref(),
            &event.user,
            &event.id,
            &accepted_at,
            self.recovering_event,
            Some(event),
        ))
    }

    fn apply_edit(&mut self, event: InboundEnvelope, mut thread: FluentThread) -> Result<()> {
        if let Some(queued) = thread
            .queued_contributions
            .iter_mut()
            .find(|queued| queued.source_timestamp == event.timestamp)
        {
            queued.contribution = event.text;
            return self.store.save_thread(&self.config, &thread);
        }
        if let Some(turn) = thread.pending_turn.as_mut().filter(|turn| {
            turn.source_timestamp == event.timestamp
                && turn.dispatch_state == PendingTurnDispatchState::Runnable
        }) {
            turn.contribution = event.text;
            return self.store.save_thread(&self.config, &thread);
        }
        if let Some(input) = thread
            .conversation_inputs
            .iter_mut()
            .find(|input| input.source_timestamp == event.timestamp)
        {
            input.corrections.push(AttributedCorrection {
                event_id: event.id,
                actor: event.user,
                content: event.text,
            });
        }
        self.store.save_thread(&self.config, &thread)
    }

    fn apply_deletion(&mut self, event: InboundEnvelope, mut thread: FluentThread) -> Result<()> {
        if let Some(input) = thread
            .conversation_inputs
            .iter_mut()
            .find(|input| input.source_timestamp == event.timestamp)
        {
            input.source_deleted = true;
        }
        self.store.save_thread(&self.config, &thread)
    }

    fn current_authority_config(&self) -> Result<RunnerConfig> {
        let path = config_path(self.home);
        let config = if path.exists() {
            read_config(self.home)
        } else {
            Ok(self.config.clone())
        }?;
        config.validate()?;
        Ok(config)
    }
    fn start_or_offer(
        &mut self,
        event: &InboundEnvelope,
        thread: &mut FluentThread,
        run_now: bool,
    ) -> Result<()> {
        let default = policy::default_project(&self.config, &event.channel)
            .and_then(|project| self.config.projects.get(project));
        let available = default.filter(|p| {
            p.allowed_participants.contains(&event.user) && checkout_is_available(&p.checkout)
        });
        if let Some(project) = available {
            let provider = selected_provider(&event.text).unwrap_or(&project.default_provider);
            if !project.enabled_providers.contains(provider) {
                self.queue_reply(
                    event,
                    "The selected conversation provider is not enabled for this project.",
                )?;
                return Ok(());
            }
            self.store
                .bind_inbox_event(&self.config, &event.id, &project.label)?;
            thread.state = ThreadState::Active;
            thread.project_label = Some(project.label.clone());
            thread.provider = Some(provider.to_owned());
            if let Some(work_item_id) = work_item_id_from_request(&event.text) {
                WorkModelStore::new(&project.checkout.path)
                    .read_work_item(work_item_id)
                    .with_context(|| format!("read linked Work Item {work_item_id}"))?;
                thread.linked_work = Some(work_item_id.to_owned());
            }
            thread.pending_turn = Some(PendingTurn {
                action_id: event.id.clone(),
                source_timestamp: event.timestamp.clone(),
                contribution: event.text.clone(),
                actor: event.user.clone(),
                starts_session: true,
                provider_turn_id: None,
                completed_reply: None,
                dispatch_state: PendingTurnDispatchState::Runnable,
                retry_at_unix_seconds: None,
                delivery_attempt: 0,
            });
            self.store.save_thread(&self.config, thread)?;
            if let Some(connected_at) =
                read_runner_operational_state(self.home)?.last_successful_connection
            {
                thread.last_connection = Some(connected_at);
                self.store.save_thread(&self.config, thread)?;
                self.queue_thread_status(thread, None)?;
            }
            if run_now {
                self.complete_pending_turn(event, thread)?;
            }
        } else {
            let projects =
                policy::available_projects(&self.config, &event.user, checkout_is_available);
            let reason = if default.is_some() {
                "The channel default is unavailable."
            } else {
                "Select a configured project before Fluent can access a checkout."
            };
            thread.pending_question = Some("project selection".into());
            self.queue_reply(
                event,
                &format!("{reason} Available projects: {}", projects.join(", ")),
            )?;
        }
        Ok(())
    }

    fn participant_can_setup_project(&self, participant: &str) -> bool {
        self.project_discovery_roots
            .iter()
            .any(|root| root.allowed_participants.contains(participant))
    }

    fn handle_project_enablement(
        &mut self,
        event: &InboundEnvelope,
        thread: &mut FluentThread,
    ) -> Result<bool> {
        let state = decode_project_setup_state(thread.pending_question.as_deref())?;
        if project_enablement_request(&event.text) {
            let index = discover_project_checkouts(&self.project_discovery_roots, &event.user)?;
            let offers = index.offers();
            if offers.is_empty() {
                self.slack.respond_private(
                    &event.channel,
                    &event.user,
                    "No project checkout is available to you.",
                )?;
                return Ok(true);
            }
            let goal = enablement_goal(&event.text);
            thread.pending_question = Some(encode_project_setup_state(
                &SlackProjectSetupState::Selecting { goal },
            )?);
            self.store.save_thread(&self.config, thread)?;
            let labels = offers
                .iter()
                .map(|offer| {
                    format!(
                        "{} — project {}; checkout {}",
                        offer.selection_id, offer.project_label, offer.checkout_label
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            self.queue_reply(
                event,
                &format!(
                    "Available project checkouts:\n{labels}\nReply `select checkout <id> profile <profile> mode <propose|execute>`. For profile `custom`, append `write-coder <coder> write-model <model> write-effort <effort> review-coder <coder> review-model <model> review-effort <effort>`."
                ),
            )?;
            return Ok(true);
        }
        let Some(state) = state else {
            return Ok(false);
        };
        match state {
            SlackProjectSetupState::Selecting { goal } => {
                let Some((selection, choices)) = parse_checkout_selection(&event.text) else {
                    self.queue_reply(
                        event,
                        "Choose a checkout with `select checkout <id> profile <profile> mode <propose|execute>`. A `custom` profile also requires all Writer and reviewer coder, model, and effort fields.",
                    )?;
                    return Ok(true);
                };
                let index = discover_project_checkouts(&self.project_discovery_roots, &event.user)?;
                let proposal = prepare_slack_project_setup(&index, selection, choices)?;
                thread.pending_question = Some(encode_project_setup_state(
                    &SlackProjectSetupState::AwaitingSetupApproval {
                        proposal: proposal.clone(),
                        goal,
                    },
                )?);
                self.store.save_thread(&self.config, thread)?;
                self.queue_reply(
                    event,
                    &format!(
                        "Proposed setup: project {}; checkout {}; {}. No checkout changes have been made. A project approver may reply `approve setup {}`.",
                        proposal.project_label,
                        proposal.checkout_label,
                        render_setup_choices(&proposal.choices),
                        proposal.revision
                    ),
                )?;
            }
            SlackProjectSetupState::AwaitingSetupApproval { proposal, goal } => {
                let Some(revision) = event.text.trim().strip_prefix("approve setup ") else {
                    self.queue_reply(
                        event,
                        "Project setup is waiting for its exact approval revision.",
                    )?;
                    return Ok(true);
                };
                if self.recovering_event {
                    self.queue_reply(
                        event,
                        "Fresh confirmation is required before Fluent can initialize this checkout.",
                    )?;
                    return Ok(true);
                }
                let index = discover_project_checkouts(&self.project_discovery_roots, &event.user)?;
                let boundary = self
                    .configured_init
                    .as_deref_mut()
                    .context("Slack project setup has no configured initialization boundary")?;
                match approve_slack_project_setup(
                    &index,
                    &proposal,
                    revision.trim(),
                    &event.user,
                    boundary,
                )? {
                    SlackSetupOutcome::AlreadyInitialized { configuration } => {
                        let checkout = index
                            .entry(&proposal.selection_id)
                            .context("selected checkout is unavailable")?;
                        let commit = crate::git::run_stdout(
                            &checkout.path,
                            &["rev-parse", "HEAD"],
                            "record existing Fluent checkout",
                        )?;
                        complete_project_enablement(
                            self.home,
                            &mut self.config,
                            &index,
                            &proposal.selection_id,
                            &InitializationCommit {
                                commit,
                                slack_approver: event.user.clone(),
                                reviewed_revision: proposal.revision.clone(),
                            },
                            thread,
                            goal.as_deref(),
                        )?;
                        self.queue_reply(
                            event,
                            &format!(
                                "The existing Fluent configuration is ready:\n{configuration}"
                            ),
                        )?;
                    }
                    SlackSetupOutcome::Initialized(review) => {
                        let rendered = render_initialization_review(&review);
                        thread.pending_question = Some(encode_project_setup_state(
                            &SlackProjectSetupState::AwaitingCommitApproval {
                                selection_id: proposal.selection_id,
                                review: review.clone(),
                                goal,
                            },
                        )?);
                        self.store.save_thread(&self.config, thread)?;
                        self.queue_reply(
                            event,
                            &format!(
                                "Initialization produced these separate changes:\n{rendered}\nA project approver may reply `commit initialization {}`.",
                                review.revision
                            ),
                        )?;
                    }
                    SlackSetupOutcome::PreflightFailed { boundary, recovery } => {
                        self.queue_reply(
                            event,
                            &format!(
                                "Configured initialization failed at the {boundary} boundary. {recovery}"
                            ),
                        )?;
                    }
                }
            }
            SlackProjectSetupState::AwaitingCommitApproval {
                selection_id,
                review,
                goal,
            } => {
                let Some(revision) = event.text.trim().strip_prefix("commit initialization ")
                else {
                    self.queue_reply(
                        event,
                        "Initialization changes are waiting for a distinct commit approval.",
                    )?;
                    return Ok(true);
                };
                if self.recovering_event {
                    self.queue_reply(
                        event,
                        "Fresh confirmation is required before Fluent can commit initialization changes.",
                    )?;
                    return Ok(true);
                }
                let index = discover_project_checkouts(&self.project_discovery_roots, &event.user)?;
                let committed = commit_reviewed_initialization(
                    &index,
                    &selection_id,
                    &review,
                    revision.trim(),
                    &event.user,
                )?;
                complete_project_enablement(
                    self.home,
                    &mut self.config,
                    &index,
                    &selection_id,
                    &committed,
                    thread,
                    goal.as_deref(),
                )?;
                self.queue_reply(
                    event,
                    "Project enablement is complete. The checkout is ready.",
                )?;
            }
        }
        Ok(true)
    }

    fn approve_revision_if_requested(
        &mut self,
        event: &InboundEnvelope,
        thread: &mut FluentThread,
    ) -> Result<bool> {
        let command = event.text.trim();
        if command.starts_with("approve plan ") {
            return Ok(false);
        }
        let Some(revision_id) = command.strip_prefix("approve ") else {
            return Ok(false);
        };
        if revision_id.trim().is_empty() {
            return Ok(false);
        }
        self.approve_planning_revision(&thread.id, revision_id.trim(), &event.user)?;
        thread.pending_artifact_revision = None;
        thread.pending_question = None;
        self.queue_reply(
            event,
            &format!("Approved planning revision {}.", revision_id.trim()),
        )?;
        Ok(true)
    }

    fn offer_linked_thread_if_requested(
        &mut self,
        event: &InboundEnvelope,
        thread: &mut FluentThread,
    ) -> Result<bool> {
        let Some(project) = event.text.strip_prefix("project: ").map(str::trim) else {
            return Ok(false);
        };
        if !self.config.projects.contains_key(project)
            || thread.project_label.as_deref() == Some(project)
        {
            return Ok(false);
        }
        thread.linked_thread_intent = Some(project.into());
        self.store.save_thread(&self.config, thread)?;
        self.queue_reply(
            event,
            &format!("This thread remains bound to its original project. Start a linked Fluent thread for project {project}."),
        )?;
        Ok(true)
    }
    fn complete_pending_turn(
        &mut self,
        event: &InboundEnvelope,
        thread: &mut FluentThread,
    ) -> Result<()> {
        let paused = thread.pending_turn.as_ref().is_some_and(|turn| {
            matches!(
                turn.dispatch_state,
                PendingTurnDispatchState::PausedForRecovery
                    | PendingTurnDispatchState::RateLimitedUntilDeadline
            ) || turn.retry_at_unix_seconds.is_some()
        });
        if paused {
            return Ok(());
        }
        if thread
            .provider_failure
            .as_ref()
            .is_some_and(|failure| !matches!(failure, ProviderFailure::RateLimited { .. }))
            && let Some(turn) = thread.pending_turn.as_mut()
        {
            turn.dispatch_state = PendingTurnDispatchState::PausedForRecovery;
            return self.store.save_thread(&self.config, thread);
        }
        if !self.turn_pool.request(&thread.id) {
            self.queue_reply(event, "Turn queued for fair provider admission.")?;
            return Ok(());
        }
        if let Some(turn) = thread.pending_turn.as_mut() {
            turn.dispatch_state = PendingTurnDispatchState::InFlight;
        }
        self.store.save_thread(&self.config, thread)?;
        let mut current_event = event.clone();
        loop {
            let action_id = thread
                .pending_turn
                .as_ref()
                .map(|turn| turn.action_id.clone());
            let result = self.execute_pending_turn(&current_event, thread);
            self.turn_pool.finish(&thread.id);
            result?;
            let ready = thread.pending_turn.as_ref().is_some_and(|turn| {
                turn.dispatch_state == PendingTurnDispatchState::Runnable
                    && turn.retry_at_unix_seconds.is_none()
                    && Some(&turn.action_id) != action_id.as_ref()
            });
            if !ready || !self.turn_pool.request(&thread.id) {
                return Ok(());
            }
            if let Some(turn) = thread.pending_turn.as_mut() {
                turn.dispatch_state = PendingTurnDispatchState::InFlight;
                current_event.id = turn.action_id.clone();
                current_event.acknowledgement_id.clear();
                current_event.user = turn.actor.clone();
                current_event.text.clear();
            }
            self.store.save_thread(&self.config, thread)?;
        }
    }

    fn execute_pending_turn(
        &mut self,
        event: &InboundEnvelope,
        thread: &mut FluentThread,
    ) -> Result<()> {
        let mut turn = thread
            .pending_turn
            .clone()
            .context("thread has no pending provider turn")?;
        let original_turn = turn.clone();
        let selected_provider = self.authorized_thread_provider(thread)?;
        if let Err(error) = self.provider.select(&selected_provider) {
            return self.preserve_provider_failure(event, thread, error);
        }
        let previously_completed = turn.completed_reply.is_some();
        let checkout = match self.bound_project(thread) {
            Ok(project) => project.checkout.path.clone(),
            Err(error) if error.to_string().contains("bound checkout is unavailable") => {
                if let Some(pending) = thread.pending_turn.as_mut() {
                    pending.dispatch_state = PendingTurnDispatchState::PausedForRecovery;
                }
                self.queue_reply(
                    event,
                    "The bound checkout is unavailable. Fluent stopped provider access and will not rebind this thread automatically.",
                )?;
                return self.store.save_thread(&self.config, thread);
            }
            Err(error) => return Err(error),
        };
        let scope = ProviderScope {
            slice_draft: checkout.join(".fluent/work/slack/drafts").join(&thread.id),
            checkout,
        };
        if turn.starts_session && thread.provider_session.is_none() {
            match self.provider.prepare_session(&scope) {
                Ok(Some(session)) => {
                    thread.provider_session = Some(session);
                    self.store.save_thread(&self.config, thread)?;
                }
                Ok(None) => {}
                Err(error) => return self.preserve_provider_failure(event, thread, error),
            }
        }
        if thread.provider_session.is_none() && !thread.conversation.is_empty() {
            let mut context = thread
                .conversation
                .iter()
                .map(|message| (message.actor.clone(), message.contribution.clone()))
                .collect::<Vec<_>>();
            context.push((turn.actor.clone(), turn.contribution.clone()));
            turn.contribution = format!(
                "Durable Slice conversation:\n{}",
                coalesce_contributions(context)
            );
            turn.actor = "participants".into();
        }
        if turn.completed_reply.is_none() {
            if let Some(session) = thread.provider_session.as_deref() {
                let recovery = match self.provider.recover_turn(&turn.action_id, session, &scope) {
                    Ok(recovery) => recovery,
                    Err(error) => return self.preserve_provider_failure(event, thread, error),
                };
                match recovery {
                    ProviderTurnRecovery::Missing if turn.provider_turn_id.is_some() => {
                        return self.preserve_provider_failure(
                            event,
                            thread,
                            ProviderError::new(
                                ProviderFailure::IncompatibleProtocol,
                                "accepted provider turn is missing from provider history",
                            ),
                        );
                    }
                    ProviderTurnRecovery::Missing => {}
                    ProviderTurnRecovery::Accepted { turn_id } => {
                        turn.provider_turn_id = Some(turn_id);
                        thread.pending_turn = Some(turn);
                        self.store.save_thread(&self.config, thread)?;
                        bail!("provider turn is accepted but not complete");
                    }
                    ProviderTurnRecovery::Completed { turn_id, reply } => {
                        turn.provider_turn_id = Some(turn_id);
                        turn.completed_reply =
                            Some(sanitize_provider_reply(&reply, &scope.checkout));
                    }
                }
            }
        }
        if turn.completed_reply.is_none() {
            let store = &self.store;
            let config = &self.config;
            let checkpoint = std::cell::RefCell::new(thread.clone());
            let mut accepted = |turn_id: &str| {
                let mut checkpoint = checkpoint.borrow_mut();
                let current = checkpoint
                    .pending_turn
                    .as_mut()
                    .context("thread lost its pending provider turn")?;
                current.provider_turn_id = Some(turn_id.to_owned());
                if let Some(latest) = store.load_thread(config, &checkpoint.id)? {
                    if latest.provider != checkpoint.provider {
                        return Ok(());
                    }
                    checkpoint.queued_contributions = latest.queued_contributions;
                    checkpoint.processed_events = latest.processed_events;
                    checkpoint.conversation = latest.conversation;
                    checkpoint.conversation_inputs = latest.conversation_inputs;
                }
                store.save_thread(config, &checkpoint)
            };
            let run = self.provider.run_turn(
                &turn.action_id,
                thread.provider_session.as_deref(),
                &scope,
                &turn.contribution,
                &turn.actor,
                &mut accepted,
            );
            let (session, reply) = match run {
                Ok(result) => result,
                Err(error) => {
                    *thread = checkpoint.into_inner();
                    return self.preserve_provider_failure(event, thread, error);
                }
            };
            *thread = checkpoint.into_inner();
            if let Some(session) = session {
                thread.provider_session = Some(session);
            }
            turn = thread.pending_turn.clone().unwrap_or(turn);
            turn.completed_reply = Some(sanitize_provider_reply(&reply, &scope.checkout));
        }
        let raw_reply = turn
            .completed_reply
            .clone()
            .context("provider turn completed without a reply")?;
        let raw_reply = sanitize_provider_reply(&raw_reply, &scope.checkout);
        let output = serde_json::from_str::<ProviderOutput>(&raw_reply).unwrap_or_else(|_| {
            ProviderOutput::Message {
                text: raw_reply.clone(),
                references: Vec::new(),
            }
        });
        let (raw_reply, provider_references) = match output {
            ProviderOutput::Message { text, references } => (text, references),
            ProviderOutput::PlanningArtifact { kind, markdown } => {
                turn.completed_reply = Some(raw_reply);
                thread.pending_turn = Some(turn);
                self.store.save_thread(&self.config, thread)?;
                self.present_planning_artifact_for_thread(thread, &kind, &markdown)?;
                thread.pending_turn = None;
                self.store.save_thread(&self.config, thread)?;
                return Ok(());
            }
        };
        let provider_reply = raw_reply.clone();
        let mut references = Vec::with_capacity(provider_references.len());
        for reference in provider_references {
            if reference.path.is_absolute()
                || reference
                    .path
                    .components()
                    .any(|component| matches!(component, std::path::Component::ParentDir))
            {
                return self.preserve_provider_failure(
                    event,
                    thread,
                    ProviderError::new(
                        ProviderFailure::IncompatibleProtocol,
                        "provider returned a code reference outside the bound checkout",
                    ),
                );
            }
            let formatted = match crate::slack_collaboration::format_code_reference(
                &self.bound_project(thread)?.checkout,
                &scope.checkout.join(reference.path),
                reference.line,
            ) {
                Ok(formatted) => formatted,
                Err(error) => {
                    return self.preserve_provider_failure(
                        event,
                        thread,
                        ProviderError::new(
                            ProviderFailure::IncompatibleProtocol,
                            format!("provider returned an invalid code reference: {error}"),
                        ),
                    );
                }
            };
            references.push(formatted);
        }
        let reply = if previously_completed {
            raw_reply
        } else if turn.starts_session {
            format!(
                "Bound to {}; provider: {}.\n{}",
                safe_identity(self.bound_project(thread)?),
                thread.provider.as_deref().unwrap_or("unknown"),
                raw_reply
            )
        } else {
            raw_reply
        };
        let reply = if references.is_empty() {
            reply
        } else {
            format!("{reply}\n\n{}", references.join("\n"))
        };
        if let Some(latest) = self.store.load_thread(&self.config, &thread.id)? {
            if latest.provider != thread.provider {
                *thread = latest;
                return Ok(());
            }
            thread.queued_contributions = latest.queued_contributions;
            thread.processed_events = latest.processed_events;
            thread.conversation = latest.conversation;
            thread.conversation_inputs = latest.conversation_inputs;
        }
        if !thread
            .conversation_inputs
            .iter()
            .any(|input| input.event_id == original_turn.action_id)
        {
            thread.conversation_inputs.push(ConversationInput {
                event_id: original_turn.action_id.clone(),
                source_timestamp: original_turn.source_timestamp.clone(),
                actor: original_turn.actor.clone(),
                content: original_turn.contribution.clone(),
                corrections: Vec::new(),
                source_deleted: false,
            });
        }
        if !thread
            .conversation
            .iter()
            .any(|message| message.action_id == original_turn.action_id)
        {
            thread.conversation.push(QueuedContribution {
                action_id: original_turn.action_id.clone(),
                source_timestamp: original_turn.source_timestamp.clone(),
                contribution: original_turn.contribution,
                actor: original_turn.actor,
            });
            thread.conversation.push(QueuedContribution {
                action_id: format!("{}:reply", original_turn.action_id),
                source_timestamp: String::new(),
                contribution: provider_reply,
                actor: "Fluent".into(),
            });
        }
        turn.completed_reply = Some(reply.clone());
        thread.provider_failure = None;
        thread.pending_turn = Some(turn);
        self.store.save_thread(&self.config, thread)?;
        self.queue_reply(event, &reply)?;
        thread.pending_turn = None;
        self.store.save_thread(&self.config, thread)?;
        if !thread.queued_contributions.is_empty() {
            let queued = std::mem::take(&mut thread.queued_contributions);
            let action_id = queued
                .iter()
                .map(|item| item.action_id.as_str())
                .collect::<Vec<_>>()
                .join("+");
            let contribution = coalesce_contributions(
                queued
                    .iter()
                    .map(|item| (item.actor.clone(), item.contribution.clone())),
            );
            thread.pending_turn = Some(PendingTurn {
                action_id: action_id.clone(),
                source_timestamp: String::new(),
                contribution,
                actor: "participants".into(),
                starts_session: false,
                provider_turn_id: None,
                completed_reply: None,
                dispatch_state: PendingTurnDispatchState::Runnable,
                retry_at_unix_seconds: None,
                delivery_attempt: 0,
            });
            self.store.save_thread(&self.config, thread)?;
        }
        Ok(())
    }

    fn preserve_provider_failure(
        &mut self,
        event: &InboundEnvelope,
        thread: &mut FluentThread,
        error: ProviderError,
    ) -> Result<()> {
        if error.failure == ProviderFailure::Internal {
            return Err(error.into());
        }
        let failure = error.failure;
        let can_switch = self
            .bound_project(thread)
            .is_ok_and(|project| !project.project_approvers.is_empty());
        self.queue_reply(event, &provider_recovery_message(&failure, can_switch))?;
        if let Some(turn) = thread.pending_turn.as_mut() {
            match &failure {
                ProviderFailure::RateLimited {
                    retry_after_seconds,
                } => {
                    turn.retry_at_unix_seconds =
                        Some((self.clock)().saturating_add(*retry_after_seconds));
                    turn.dispatch_state = PendingTurnDispatchState::RateLimitedUntilDeadline;
                }
                _ => {
                    turn.retry_at_unix_seconds = None;
                    turn.dispatch_state = PendingTurnDispatchState::PausedForRecovery;
                }
            }
        }
        thread.provider_failure = Some(failure);
        self.store.save_thread(&self.config, thread)
    }

    /// Materialize an authorized thread plan and durably queue its identifiers
    /// for Slack delivery without starting execution.
    pub fn approve_thread_plan(
        &mut self,
        thread_id: &str,
        actor: &str,
        slice_id: &str,
        planned_work: &[PlannedWork],
    ) -> Result<ApprovedPlanReport> {
        let mut thread = self
            .store
            .load_thread(&self.config, thread_id)?
            .context("Fluent thread is unavailable")?;
        let project = self.bound_project(&thread)?;
        if !project.project_approvers.contains(actor) {
            bail!("participant cannot approve Work for this project");
        }
        let control = WorkControl::new(&project.checkout.path);
        let report = control.approve_plan(slice_id, actor, planned_work)?;
        thread.slice = Some(slice_id.to_owned());
        thread.linked_work = report.work_item_ids.first().cloned();
        self.store.save_thread(&self.config, &thread)?;
        self.store.persist_outbox(OutboundDelivery {
            id: format!("{thread_id}:work-plan:{slice_id}"),
            channel: thread.channel.clone(),
            thread_timestamp: thread.id.clone(),
            text: format!(
                "Approved Slice {slice_id}. Planned Work Items: {}. Execution has not started.",
                report.work_item_ids.join(", ")
            ),
            delivered: false,
            approval_control: None,
            update_timestamp: None,
        })?;
        self.queue_thread_status(&thread, None)?;
        self.deliver_pending()?;
        Ok(report)
    }

    /// Submit separately approved Work through the scheduler boundary and
    /// durably report each real acknowledgement in the bound thread.
    pub fn authorize_thread_execution(
        &mut self,
        thread_id: &str,
        actor: &str,
        scheduler: &dyn WorkScheduler,
    ) -> Result<ExecutionApprovalReport> {
        let thread = self
            .store
            .load_thread(&self.config, thread_id)?
            .context("Fluent thread is unavailable")?;
        let project = self.bound_project(&thread)?;
        if !project.project_approvers.contains(actor) {
            bail!("participant cannot authorize Work for this project");
        }
        let slice_id = thread
            .slice
            .as_deref()
            .context("thread has no approved Slice")?;
        let report = WorkControl::new(&project.checkout.path)
            .authorize_execution(slice_id, actor, scheduler)?;
        for (work_item_id, outcome) in &report.outcomes {
            self.store.persist_outbox(OutboundDelivery {
                id: format!("{thread_id}:work-queue:{work_item_id}"),
                channel: thread.channel.clone(),
                thread_timestamp: thread.id.clone(),
                text: format!("Work Item {work_item_id} queue outcome: {outcome:?}."),
                delivered: false,
                approval_control: None,
                update_timestamp: None,
            })?;
        }
        self.deliver_pending()?;
        Ok(report)
    }

    /// Authorize Work through the configured checkout's production scheduler
    /// submission boundary.
    pub fn authorize_thread_execution_locally(
        &mut self,
        thread_id: &str,
        actor: &str,
    ) -> Result<ExecutionApprovalReport> {
        let thread = self
            .store
            .load_thread(&self.config, thread_id)?
            .context("Fluent thread is unavailable")?;
        let project_root = self.bound_project(&thread)?.checkout.path.clone();
        let scheduler = LocalSchedulerClient::new(self.home, [project_root]);
        self.authorize_thread_execution(thread_id, actor, &scheduler)
    }

    /// Project authoritative Work state and schedule unseen lifecycle replies
    /// through the collaboration outbox before advancing Slack cursors.
    pub fn report_thread_work(&mut self, thread_id: &str) -> Result<Vec<WorkTransition>> {
        let thread = self
            .store
            .load_thread(&self.config, thread_id)?
            .context("Fluent thread is unavailable")?;
        let project = self.bound_project(&thread)?;
        self.reconcile_thread_post_land_effects(&thread, project)?;
        let slice_id = thread
            .slice
            .as_deref()
            .context("thread has no approved Slice")?;
        let control = WorkControl::new(&project.checkout.path);
        let store = &self.store;
        let transitions = control.schedule_durable_transitions(slice_id, |transition| {
            let evidence = transition.evidence_ref.iter().cloned().collect::<Vec<_>>();
            store.persist_outbox(OutboundDelivery {
                id: format!(
                    "{thread_id}:work-transition:{}:{}",
                    transition.work_item_id, transition.sequence
                ),
                channel: thread.channel.clone(),
                thread_timestamp: thread.id.clone(),
                text: control.render_actionable_transition(
                    transition,
                    &evidence,
                    project.github_remote.as_deref(),
                ),
                delivered: false,
                approval_control: None,
                update_timestamp: None,
            })
        })?;
        let refreshed = self
            .store
            .load_thread(&self.config, thread_id)?
            .context("Fluent thread is unavailable after Work reconciliation")?;
        self.queue_thread_status(&refreshed, None)?;
        self.deliver_pending()?;
        Ok(transitions)
    }

    /// Publish one explicitly requested exact ready candidate. GitHub failures
    /// become recoverable Slack reports and never modify Work-model readiness.
    pub fn request_github_review(
        &mut self,
        thread_id: &str,
        actor: &str,
        candidate_id: &str,
        github: &mut dyn GitHubReviewClient,
    ) -> Result<GitHubReviewRequestOutcome> {
        let thread = self
            .store
            .load_thread(&self.config, thread_id)?
            .context("Fluent thread is unavailable")?;
        let project = self.bound_project(&thread)?;
        if !project.project_approvers.contains(actor) {
            bail!("participant cannot publish GitHub review for this project");
        }
        let remote = project
            .github_remote
            .as_deref()
            .context("GitHub pull-request integration is not configured")?;
        let slice_id = thread
            .slice
            .as_deref()
            .context("thread has no approved Slice")?;
        let control = WorkControl::new(&project.checkout.path);
        match control.publish_ready_candidate(slice_id, candidate_id, remote, github) {
            Ok(publication) => {
                let url = publication
                    .pull_request_url
                    .as_deref()
                    .expect("successful publication has a URL");
                self.store.persist_outbox(OutboundDelivery {
                    id: format!(
                        "{thread_id}:github-pr:{}",
                        publication.candidate.candidate_id
                    ),
                    channel: thread.channel.clone(),
                    thread_timestamp: thread.id.clone(),
                    text: format!(
                        "Published exact ready Merge Candidate {} ({}) for GitHub review: {url}",
                        publication.candidate.candidate_id, publication.candidate.commit
                    ),
                    delivered: false,
                    approval_control: None,
                    update_timestamp: None,
                })?;
                self.deliver_pending()?;
                Ok(GitHubReviewRequestOutcome::Published(publication))
            }
            Err(error) => {
                let message = redact_secrets(&format!("{error:#}"));
                let digest = format!("{:x}", Sha256::digest(message.as_bytes()));
                self.store.persist_outbox(OutboundDelivery {
                    id: format!(
                        "{thread_id}:github-pr-failure:{candidate_id}:{}",
                        &digest[..16]
                    ),
                    channel: thread.channel.clone(),
                    thread_timestamp: thread.id.clone(),
                    text: format!(
                        "GitHub review publication failed for Merge Candidate {candidate_id}: {message}. The ready candidate is unchanged; retry this pull-request request after correcting GitHub authentication, host, or remote access."
                    ),
                    delivered: false,
                    approval_control: None,
                    update_timestamp: None,
                })?;
                self.deliver_pending()?;
                Ok(GitHubReviewRequestOutcome::Failed {
                    candidate_id: candidate_id.to_owned(),
                    message,
                })
            }
        }
    }

    /// Advance the one open pull request to a newly ready candidate. Callers
    /// invoke this after Work reconciliation; unready commits remain invisible.
    pub fn advance_github_review(
        &mut self,
        thread_id: &str,
        github: &mut dyn GitHubReviewClient,
    ) -> Result<Option<GitHubPublication>> {
        let thread = self
            .store
            .load_thread(&self.config, thread_id)?
            .context("Fluent thread is unavailable")?;
        let project = self.bound_project(&thread)?;
        let slice_id = thread
            .slice
            .as_deref()
            .context("thread has no approved Slice")?;
        let publication = WorkControl::new(&project.checkout.path)
            .advance_published_candidate(slice_id, github)?;
        if let Some(publication) = publication.as_ref() {
            let url = publication
                .pull_request_url
                .as_deref()
                .context("advanced GitHub publication lacks a URL")?;
            self.store.persist_outbox(OutboundDelivery {
                id: format!(
                    "{thread_id}:github-pr-advance:{}",
                    publication.candidate.candidate_id
                ),
                channel: thread.channel.clone(),
                thread_timestamp: thread.id.clone(),
                text: format!(
                    "Updated GitHub pull request {url} to newly ready Merge Candidate {} ({}).",
                    publication.candidate.candidate_id, publication.candidate.commit
                ),
                delivered: false,
                approval_control: None,
                update_timestamp: None,
            })?;
            self.deliver_pending()?;
        }
        Ok(publication)
    }

    /// Report unseen GitHub feedback with stable links. This path never calls
    /// the candidate-rework boundary.
    pub fn report_github_feedback(
        &mut self,
        thread_id: &str,
        github: &mut dyn GitHubReviewClient,
    ) -> Result<Vec<GitHubReviewFeedback>> {
        let thread = self
            .store
            .load_thread(&self.config, thread_id)?
            .context("Fluent thread is unavailable")?;
        let project = self.bound_project(&thread)?;
        let slice_id = thread
            .slice
            .as_deref()
            .context("thread has no approved Slice")?;
        let store = &self.store;
        let feedback = WorkControl::new(&project.checkout.path).schedule_github_feedback(
            slice_id,
            github,
            |item| {
                let summary = item.body.split_whitespace().collect::<Vec<_>>().join(" ");
                let summary = if summary.len() > 240 {
                    format!("{}…", &summary[..summary.floor_char_boundary(240)])
                } else {
                    summary
                };
                store.persist_outbox(OutboundDelivery {
                    id: format!("{thread_id}:github-feedback:{}", item.id),
                    channel: thread.channel.clone(),
                    thread_timestamp: thread.id.clone(),
                    text: format!(
                        "GitHub {} on candidate {}: {} {}. No corrective Work started.",
                        item.kind, item.candidate_commit, summary, item.url
                    ),
                    delivered: false,
                    approval_control: None,
                    update_timestamp: None,
                })
            },
        )?;
        self.deliver_pending()?;
        Ok(feedback)
    }

    pub fn authorize_github_feedback_rework(
        &mut self,
        thread_id: &str,
        actor: &str,
        feedback_ids: &[String],
        boundary: &dyn CandidateReworkBoundary,
    ) -> Result<CandidateReworkRequest> {
        let thread = self
            .store
            .load_thread(&self.config, thread_id)?
            .context("Fluent thread is unavailable")?;
        let project = self.bound_project(&thread)?;
        if !project.project_approvers.contains(actor) {
            bail!("participant cannot authorize candidate rework for this project");
        }
        let slice_id = thread
            .slice
            .as_deref()
            .context("thread has no approved Slice")?;
        let request = WorkControl::new(&project.checkout.path).authorize_feedback_rework(
            slice_id,
            &project.label,
            actor,
            feedback_ids,
            boundary,
        )?;
        self.store.persist_outbox(OutboundDelivery {
            id: format!("{thread_id}:{}", request.id),
            channel: thread.channel.clone(),
            thread_timestamp: thread.id.clone(),
            text: format!(
                "Submitted selected GitHub feedback through Merge Candidate rework request {} for candidate {}.",
                request.id, request.candidate_id
            ),
            delivered: false,
            approval_control: None,
            update_timestamp: None,
        })?;
        self.deliver_pending()?;
        Ok(request)
    }

    fn reconcile_thread_post_land_effects(
        &self,
        thread: &FluentThread,
        project: &ConfiguredProject,
    ) -> Result<()> {
        let slice_id = thread
            .slice
            .as_deref()
            .context("thread has no approved Slice")?;
        let control = WorkControl::new(&project.checkout.path);
        let slice = control
            .slice(slice_id)?
            .with_context(|| format!("approved Slice {slice_id} is unavailable"))?;
        let effects = crate::follow_up::completed_projection_effects(
            &project.checkout.path,
            &slice.work_item_ids,
        )?;
        if effects.is_empty() {
            return Ok(());
        }
        let existing = self
            .store
            .load_outbox()?
            .into_iter()
            .map(|delivery| delivery.id)
            .collect::<std::collections::HashSet<_>>();
        let delivery_id = |identity: &str| {
            let digest = format!("{:x}", Sha256::digest(identity.as_bytes()));
            format!("{}:post-land:{}", thread.id, &digest[..16])
        };
        let stage = control.slice_stage(slice_id)?;
        let mut corrective_work = Vec::new();
        let mut deliveries = Vec::new();
        for effect in effects {
            let id = delivery_id(&effect.identity);
            match effect.kind {
                crate::follow_up::PostLandProjectionKind::CorrectiveWork { work_item_id } => {
                    corrective_work.push(work_item_id.clone());
                    if !existing.contains(&id) {
                        deliveries.push((
                            id,
                            format!(
                                "Linked corrective Work Item {work_item_id} to Slice {slice_id}; the Slice is active until the correction reaches its terminal outcome."
                            ),
                        ));
                    }
                }
                crate::follow_up::PostLandProjectionKind::Observation { observation_id }
                    if !existing.contains(&id) =>
                {
                    let text = if stage == SliceStage::Complete {
                        control.render_optional_observation(slice_id, &observation_id)?
                    } else {
                        format!("Slice {slice_id} is active. Observation: {observation_id}.")
                    };
                    deliveries.push((id, redact_secrets(&text)));
                }
                crate::follow_up::PostLandProjectionKind::Observation { .. } => {}
            }
        }
        control.link_post_land_corrective_work_items(slice_id, &corrective_work)?;
        for (id, text) in deliveries {
            self.store.persist_outbox(OutboundDelivery {
                id,
                channel: thread.channel.clone(),
                thread_timestamp: thread.id.clone(),
                text,
                delivered: false,
                approval_control: None,
                update_timestamp: None,
            })?;
        }
        Ok(())
    }

    /// Offer a linked Slice for new product Work without reopening a completed
    /// Slice or authorizing the proposed Work.
    pub fn offer_linked_slice_for_new_work(
        &mut self,
        thread_id: &str,
        actor: &str,
        requested_work: &str,
    ) -> Result<String> {
        let thread = self
            .store
            .load_thread(&self.config, thread_id)?
            .context("Fluent thread is unavailable")?;
        let project = self.bound_project(&thread)?;
        if !project.allowed_participants.contains(actor) {
            bail!("participant is not allowed to contribute to this project");
        }
        let slice_id = thread
            .slice
            .as_deref()
            .context("thread has no approved Slice")?;
        let reply = WorkControl::new(&project.checkout.path)
            .linked_slice_offer(slice_id, requested_work)?;
        let digest = format!("{:x}", Sha256::digest(requested_work.as_bytes()));
        self.store.persist_outbox(OutboundDelivery {
            id: format!("{thread_id}:linked-slice-offer:{}", &digest[..16]),
            channel: thread.channel.clone(),
            thread_timestamp: thread.id.clone(),
            text: reply.clone(),
            delivered: false,
            approval_control: None,
            update_timestamp: None,
        })?;
        self.deliver_pending()?;
        Ok(reply)
    }

    /// Report an optional post-land Observation in the originating thread
    /// without changing the completed Slice.
    pub fn report_thread_observation(
        &mut self,
        thread_id: &str,
        observation_ref: &str,
    ) -> Result<String> {
        let thread = self
            .store
            .load_thread(&self.config, thread_id)?
            .context("Fluent thread is unavailable")?;
        let project = self.bound_project(&thread)?;
        let slice_id = thread
            .slice
            .as_deref()
            .context("thread has no approved Slice")?;
        let reply = redact_secrets(
            &WorkControl::new(&project.checkout.path)
                .render_optional_observation(slice_id, observation_ref)?,
        );
        let digest = format!("{:x}", Sha256::digest(observation_ref.as_bytes()));
        self.store.persist_outbox(OutboundDelivery {
            id: format!("{thread_id}:observation:{}", &digest[..16]),
            channel: thread.channel.clone(),
            thread_timestamp: thread.id.clone(),
            text: reply.clone(),
            delivered: false,
            approval_control: None,
            update_timestamp: None,
        })?;
        self.deliver_pending()?;
        Ok(reply)
    }

    /// Link policy-created corrective Work to the originating Slice and report
    /// its durable initial state through the original thread.
    pub fn link_thread_corrective_work(
        &mut self,
        thread_id: &str,
        work_item_id: &str,
    ) -> Result<()> {
        let thread = self
            .store
            .load_thread(&self.config, thread_id)?
            .context("Fluent thread is unavailable")?;
        let project = self.bound_project(&thread)?;
        let slice_id = thread
            .slice
            .as_deref()
            .context("thread has no approved Slice")?;
        WorkControl::new(&project.checkout.path).link_corrective_work(slice_id, work_item_id)?;
        self.store.persist_outbox(OutboundDelivery {
            id: format!("{thread_id}:corrective-work:{work_item_id}"),
            channel: thread.channel.clone(),
            thread_timestamp: thread.id.clone(),
            text: format!(
                "Linked corrective Work Item {work_item_id} to Slice {slice_id}; the Slice is active until the correction reaches its terminal outcome."
            ),
            delivered: false,
            approval_control: None,
            update_timestamp: None,
        })?;
        self.report_thread_work(thread_id)?;
        Ok(())
    }

    fn bound_project(&self, thread: &FluentThread) -> Result<&ConfiguredProject> {
        let project = self.configured_bound_project(thread)?;
        if !checkout_is_available(&project.checkout) {
            bail!("bound checkout is unavailable; provider access stopped without rebinding");
        }
        Ok(project)
    }
    fn configured_bound_project(&self, thread: &FluentThread) -> Result<&ConfiguredProject> {
        let label = thread
            .project_label
            .as_deref()
            .context("thread is unbound")?;
        self.config
            .projects
            .get(label)
            .context("thread project no longer configured")
    }
    fn authorized_thread_provider(&self, thread: &mut FluentThread) -> Result<String> {
        let provider = thread
            .provider
            .as_deref()
            .context("thread has no selected provider")?
            .to_owned();
        if !self
            .configured_bound_project(thread)?
            .enabled_providers
            .contains(&provider)
        {
            if let Some(turn) = thread.pending_turn.as_mut() {
                turn.dispatch_state = PendingTurnDispatchState::PausedForRecovery;
                turn.retry_at_unix_seconds = None;
            }
            self.store.save_thread(&self.config, thread)?;
            bail!("thread provider is no longer enabled for the bound project");
        }
        Ok(provider)
    }
    fn queue_reply(&self, event: &InboundEnvelope, text: &str) -> Result<()> {
        let delivery = OutboundDelivery {
            id: format!("{}:reply", event.id),
            channel: event.channel.clone(),
            thread_timestamp: event.thread_id(),
            text: redact_secrets(text),
            delivered: false,
            approval_control: None,
            update_timestamp: None,
        };
        self.store.persist_outbox(delivery)
    }

    fn queue_queued_acknowledgement(&self, event: &InboundEnvelope) -> Result<()> {
        self.store.persist_outbox(OutboundDelivery {
            id: format!("{}:queued", event.id),
            channel: event.channel.clone(),
            thread_timestamp: event.thread_id(),
            text: "Reply queued for the next conversation turn.".into(),
            delivered: false,
            approval_control: None,
            update_timestamp: None,
        })
    }

    fn queue_thread_waiting(&self, thread: &FluentThread, text: &str) -> Result<()> {
        let action = thread
            .pending_action
            .as_ref()
            .context("waiting thread lacks a pending action")?;
        self.store.persist_outbox(OutboundDelivery {
            id: format!(
                "{}:approval-waiting:{}:{}",
                thread.id, action.id, action.artifact_revision
            ),
            channel: thread.channel.clone(),
            thread_timestamp: thread.id.clone(),
            text: text.into(),
            delivered: false,
            approval_control: Some(ApprovalControl {
                action_id: action.id.clone(),
                artifact_revision: action.artifact_revision.clone(),
                label: "Approve".into(),
            }),
            update_timestamp: None,
        })
    }

    fn queue_thread_status(&self, thread: &FluentThread, interruption: Option<&str>) -> Result<()> {
        let text = self.render_thread_status(thread, interruption)?;
        let digest = format!("{:x}", Sha256::digest(text.as_bytes()));
        self.store.persist_outbox(OutboundDelivery {
            id: format!("{}:status:{}", thread.id, &digest[..16]),
            channel: thread.channel.clone(),
            thread_timestamp: thread.id.clone(),
            text,
            delivered: false,
            approval_control: None,
            update_timestamp: thread.first_reply_timestamp.clone(),
        })
    }

    fn render_thread_status(
        &self,
        thread: &FluentThread,
        interruption: Option<&str>,
    ) -> Result<String> {
        let interruption = interruption
            .map(|message| format!(" {message}"))
            .unwrap_or_default();
        let provider_state = if thread.provider_session.is_some() {
            "connected"
        } else {
            "idle"
        };
        let project = thread
            .project_label
            .as_deref()
            .and_then(|label| self.config.projects.get(label));
        let mut text = match (project, thread.slice.as_deref()) {
            (Some(project), Some(slice_id)) => WorkControl::new(&project.checkout.path)
                .render_status_message(
                    slice_id,
                    &project.label,
                    &project.checkout.label,
                    provider_state,
                )?,
            (Some(project), None) => format!(
                "Project: {}; checkout: {}; Slice: planning; provider: {provider_state}. Next action: complete the Implementation Plan.",
                project.label, project.checkout.label
            ),
            _ => format!("Fluent thread status; provider: {provider_state}."),
        };
        text.push_str(&interruption);
        if let Some(connected_at) = &thread.last_connection {
            text.push_str(&format!(" Runner last connected at {connected_at}."));
        }
        Ok(text)
    }
}

struct ProviderCompletion {
    thread_id: String,
    result: Result<()>,
}

/// Keeps Slack ingress responsive while provider turns run on bounded workers.
pub struct AsyncCollaborationRunner<'a, P, S> {
    runner: CollaborationRunner<'a, P, S>,
    provider_factory: Arc<dyn Fn() -> P + Send + Sync>,
    completions_tx: std::sync::mpsc::Sender<ProviderCompletion>,
    completions_rx: std::sync::mpsc::Receiver<ProviderCompletion>,
    pool: FairTurnPool,
    active: BTreeSet<String>,
    github: Box<dyn GitHubReviewClient + Send>,
    rework: Box<dyn CandidateReworkBoundary + Send + Sync>,
}

impl<'a, P, S> AsyncCollaborationRunner<'a, P, S>
where
    P: ConversationProvider + Clone + Send + Sync + 'static,
    S: SlackTransport,
{
    pub fn new(
        home: &'a Path,
        config: RunnerConfig,
        provider: P,
        slack: S,
        capacity: usize,
    ) -> Self {
        let template = provider.clone();
        Self::new_with_provider_factory(home, config, provider, slack, capacity, move || {
            template.clone()
        })
    }
}

impl<'a, P, S> AsyncCollaborationRunner<'a, P, S>
where
    P: ConversationProvider + Send + 'static,
    S: SlackTransport,
{
    pub fn new_with_provider_factory(
        home: &'a Path,
        config: RunnerConfig,
        provider: P,
        slack: S,
        capacity: usize,
        provider_factory: impl Fn() -> P + Send + Sync + 'static,
    ) -> Self {
        let rework = LocalCandidateReworkBoundary::new(
            config
                .projects
                .iter()
                .map(|(label, project)| (label.clone(), project.checkout.path.clone())),
        );
        Self::new_with_provider_factory_and_integrations(
            home,
            config,
            provider,
            slack,
            capacity,
            provider_factory,
            Box::new(GhGitHubClient::default()),
            Box::new(rework),
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new_with_provider_factory_and_integrations(
        home: &'a Path,
        config: RunnerConfig,
        provider: P,
        slack: S,
        capacity: usize,
        provider_factory: impl Fn() -> P + Send + Sync + 'static,
        github: Box<dyn GitHubReviewClient + Send>,
        rework: Box<dyn CandidateReworkBoundary + Send + Sync>,
    ) -> Self {
        let (completions_tx, completions_rx) = std::sync::mpsc::channel();
        Self {
            runner: CollaborationRunner::new(home, config, provider, slack),
            provider_factory: Arc::new(provider_factory),
            completions_tx,
            completions_rx,
            pool: FairTurnPool::new(capacity),
            active: BTreeSet::new(),
            github,
            rework,
        }
    }

    pub fn slack(&self) -> &S {
        self.runner.slack()
    }

    pub fn slack_mut(&mut self) -> &mut S {
        self.runner.slack_mut()
    }

    pub fn configure_project_enablement(
        &mut self,
        roots: Vec<ProjectDiscoveryRoot>,
        boundary: Box<dyn ConfiguredInitBoundary + 'a>,
    ) {
        self.runner.configure_project_enablement(roots, boundary);
    }

    #[doc(hidden)]
    pub fn set_clock_for_test(&mut self, clock: impl Fn() -> u64 + Send + Sync + 'static) {
        self.runner.set_clock_for_test(clock);
    }

    pub fn ingest(&mut self, event: InboundEnvelope) -> Result<()> {
        let thread_id = event.thread_id();
        self.runner.ingest_at_scheduler_boundary(
            event,
            false,
            None,
            Some(GitHubIntegrations {
                github: &mut *self.github,
                rework: &*self.rework,
            }),
        )?;
        self.dispatch_if_ready(&thread_id)
    }

    pub fn resume_after_provider_login(&mut self, thread_id: &str) -> Result<()> {
        self.runner.config = self.runner.current_authority_config()?;
        let mut thread = self
            .runner
            .store
            .load_thread(&self.runner.config, thread_id)?
            .context("Fluent thread is unavailable")?;
        self.runner
            .resume_after_provider_login_metadata(&mut thread)?;
        self.dispatch_if_ready(thread_id)
    }

    pub fn reconfirm_checkout(&mut self, thread_id: &str) -> Result<()> {
        let mut thread = self
            .runner
            .store
            .load_thread(&self.runner.config, thread_id)?
            .context("Fluent thread is unavailable")?;
        self.runner.reconfirm_checkout_metadata(&mut thread)?;
        self.dispatch_if_ready(thread_id)
    }

    /// Consume owner-authenticated recovery actions from the durable local
    /// inbox and apply them to this live runner instance.
    pub fn process_operator_recovery_actions(&mut self) -> Result<()> {
        for action in load_recovery_actions(self.runner.home)? {
            validate_thread_identity(&action.thread)?;
            let Some(thread) = self
                .runner
                .store
                .load_thread(&self.runner.config, &action.thread)?
            else {
                remove_recovery_action(self.runner.home, &action.id)?;
                continue;
            };
            if thread.project_label.as_deref() != Some(&action.project) {
                remove_recovery_action(self.runner.home, &action.id)?;
                continue;
            }
            let still_paused = thread.pending_turn.as_ref().is_some_and(|turn| {
                turn.dispatch_state == PendingTurnDispatchState::PausedForRecovery
            });
            if !still_paused {
                remove_recovery_action(self.runner.home, &action.id)?;
                continue;
            }
            match &action.kind {
                OperatorRecoveryKind::ProviderLogin { provider } => {
                    if thread.provider.as_deref() != Some(provider) {
                        remove_recovery_action(self.runner.home, &action.id)?;
                        continue;
                    }
                    if !matches!(
                        thread.provider_failure,
                        Some(
                            ProviderFailure::Authentication
                                | ProviderFailure::HostCredentialDenied
                                | ProviderFailure::Unavailable
                        )
                    ) {
                        remove_recovery_action(self.runner.home, &action.id)?;
                        continue;
                    }
                    self.resume_after_provider_login(&action.thread)?;
                }
                OperatorRecoveryKind::CheckoutReconfirmation => {
                    if thread.provider_failure.is_some() {
                        remove_recovery_action(self.runner.home, &action.id)?;
                        continue;
                    }
                    self.reconfirm_checkout(&action.thread)?;
                }
            }
            remove_recovery_action(self.runner.home, &action.id)?;
        }
        Ok(())
    }

    pub fn recovery_targets(&self) -> Result<Vec<RecoveryTarget>> {
        self.runner.recovery_targets()
    }

    pub fn restore_connection(
        &mut self,
        history: impl IntoIterator<Item = InboundEnvelope>,
        connected_at: String,
        report_interruption: bool,
    ) -> Result<()> {
        self.runner.restore_connection_with_completion(
            history,
            connected_at,
            report_interruption,
            false,
        )?;
        self.runner
            .poll_linked_work_with_github(&mut *self.github)?;
        let thread_ids: Vec<_> = self
            .runner
            .store
            .load_threads(&self.runner.config)?
            .into_keys()
            .collect();
        for thread_id in thread_ids {
            let Some(mut thread) = self
                .runner
                .store
                .load_thread(&self.runner.config, &thread_id)?
            else {
                continue;
            };
            if thread
                .pending_turn
                .as_ref()
                .is_some_and(|turn| turn.dispatch_state == PendingTurnDispatchState::InFlight)
            {
                thread.pending_turn.as_mut().unwrap().dispatch_state =
                    PendingTurnDispatchState::Runnable;
                self.runner
                    .store
                    .save_thread(&self.runner.config, &thread)?;
            }
            self.dispatch_if_ready(&thread_id)?;
        }
        Ok(())
    }

    fn dispatch_if_ready(&mut self, thread_id: &str) -> Result<()> {
        self.runner.config = self.runner.current_authority_config()?;
        let Some(mut thread) = self
            .runner
            .store
            .load_thread(&self.runner.config, thread_id)?
        else {
            return Ok(());
        };
        let ready = thread.pending_turn.as_ref().is_some_and(|turn| {
            turn.dispatch_state == PendingTurnDispatchState::Runnable
                && turn.retry_at_unix_seconds.is_none()
        }) && !thread
            .provider_failure
            .as_ref()
            .is_some_and(|failure| !matches!(failure, ProviderFailure::RateLimited { .. }));
        if ready {
            self.runner.authorized_thread_provider(&mut thread)?;
        }
        if ready && self.pool.request(thread_id) {
            if let Err(error) = self.mark_in_flight(thread) {
                self.pool.finish(thread_id);
                return Err(error);
            }
            self.spawn_admitted(thread_id.to_owned());
        }
        Ok(())
    }

    fn mark_in_flight(&self, mut thread: FluentThread) -> Result<()> {
        thread
            .pending_turn
            .as_mut()
            .context("thread has no pending provider turn")?
            .dispatch_state = PendingTurnDispatchState::InFlight;
        self.runner.store.save_thread(&self.runner.config, &thread)
    }

    fn spawn_admitted(&mut self, thread_id: String) {
        self.active.insert(thread_id.clone());
        let provider = (self.provider_factory)();
        let config = self.runner.config.clone();
        let home = self.runner.home.to_owned();
        let clock = self.runner.clock.clone();
        let sender = self.completions_tx.clone();
        std::thread::spawn(move || {
            let mut worker = CollaborationRunner::new(
                &home,
                config,
                provider,
                crate::slack_collaboration::InMemorySlack::default(),
            );
            worker.clock = clock;
            let result = worker.run_pending_thread(&thread_id);
            let _ = sender.send(ProviderCompletion { thread_id, result });
        });
    }

    fn handle_completion(&mut self, completion: ProviderCompletion) -> Result<()> {
        self.active.remove(&completion.thread_id);
        let next = self.pool.finish(&completion.thread_id);
        self.runner.deliver_pending()?;
        if let Some(next) = next {
            let thread = self
                .runner
                .store
                .load_thread(&self.runner.config, &next)?
                .context("fairly admitted thread is unavailable")?;
            self.mark_in_flight(thread)?;
            self.spawn_admitted(next);
            if completion.result.is_ok() {
                self.dispatch_if_ready(&completion.thread_id)?;
            }
        } else if completion.result.is_ok() {
            self.dispatch_if_ready(&completion.thread_id)?;
        }
        completion.result
    }

    pub fn wait_for_completion(&mut self) -> Result<()> {
        let completion = self
            .completions_rx
            .recv()
            .context("provider worker stopped")?;
        self.handle_completion(completion)
    }

    pub fn poll_completions(&mut self) -> Result<()> {
        while let Ok(completion) = self.completions_rx.try_recv() {
            self.handle_completion(completion)?;
        }
        Ok(())
    }

    pub fn next_retry_deadline(&self) -> Result<Option<u64>> {
        Ok(self
            .runner
            .store
            .load_threads(&self.runner.config)?
            .into_values()
            .filter_map(|thread| thread.pending_turn?.retry_at_unix_seconds)
            .min())
    }

    pub fn wake_due_retries(&mut self, now: u64) -> Result<()> {
        let thread_ids: Vec<_> = self
            .runner
            .store
            .load_threads(&self.runner.config)?
            .into_keys()
            .collect();
        for thread_id in thread_ids {
            let Some(mut thread) = self
                .runner
                .store
                .load_thread(&self.runner.config, &thread_id)?
            else {
                continue;
            };
            let due = thread
                .pending_turn
                .as_ref()
                .and_then(|turn| turn.retry_at_unix_seconds)
                .is_some_and(|deadline| deadline <= now);
            if due {
                if let Some(turn) = thread.pending_turn.as_mut() {
                    turn.retry_at_unix_seconds = None;
                    turn.dispatch_state = PendingTurnDispatchState::Runnable;
                    turn.delivery_attempt = turn.delivery_attempt.saturating_add(1);
                }
                self.runner
                    .store
                    .save_thread(&self.runner.config, &thread)?;
                self.dispatch_if_ready(&thread_id)?;
            }
        }
        Ok(())
    }

    /// Drive completions, Slack ingress, and the earliest durable retry timer.
    pub fn drive_once(&mut self, maximum_wait: std::time::Duration) -> Result<()> {
        self.poll_completions()?;
        self.process_operator_recovery_actions()?;
        let now = (self.runner.clock)();
        self.wake_due_retries(now)?;
        let retry_wait = self
            .next_retry_deadline()?
            .map(|deadline| std::time::Duration::from_secs(deadline.saturating_sub(now)))
            .unwrap_or(maximum_wait);
        let wait = retry_wait
            .min(maximum_wait)
            .max(std::time::Duration::from_millis(1));
        if let Some(event) = self.runner.slack_mut().receive_timeout(wait)? {
            self.ingest(event)?;
        } else {
            self.runner
                .poll_linked_work_with_github(&mut *self.github)?;
        }
        self.wake_due_retries((self.runner.clock)())?;
        self.poll_completions()
    }
}

fn selected_provider(text: &str) -> Option<&str> {
    let marker = "[provider=";
    let (_, suffix) = text.split_once(marker)?;
    let suffix = suffix.split_once(']')?.0;
    (!suffix.is_empty()).then_some(suffix)
}

fn recovery_event(thread: &FluentThread, suffix: &str) -> InboundEnvelope {
    InboundEnvelope {
        id: format!("{}:{suffix}", thread.id),
        acknowledgement_id: String::new(),
        channel: thread.channel.clone(),
        timestamp: thread.id.clone(),
        thread_timestamp: Some(thread.id.clone()),
        user: thread
            .pending_turn
            .as_ref()
            .map(|turn| turn.actor.clone())
            .unwrap_or_default(),
        text: String::new(),
        change: InboundChange::Message,
        approval_action_id: None,
        approval_artifact_revision: None,
    }
}

fn work_item_id_from_request(text: &str) -> Option<&str> {
    text.split_once("work-item:")
        .and_then(|(_, suffix)| suffix.split_whitespace().next())
        .filter(|id| !id.is_empty())
}

fn new_product_work_request(command: &str) -> Option<&str> {
    ["new work ", "new product work "]
        .into_iter()
        .find_map(|prefix| {
            let candidate = command.get(..prefix.len())?;
            candidate
                .eq_ignore_ascii_case(prefix)
                .then(|| command[prefix.len()..].trim())
        })
        .filter(|request| !request.is_empty())
}

fn github_pull_request_candidate(command: &str) -> Option<&str> {
    ["create pull request ", "create github pull request "]
        .into_iter()
        .find_map(|prefix| {
            command
                .to_ascii_lowercase()
                .strip_prefix(prefix)
                .map(|_| prefix)
        })
        .and_then(|prefix| command.get(prefix.len()..))
        .map(str::trim)
        .filter(|candidate| !candidate.is_empty() && !candidate.contains(char::is_whitespace))
}

fn selected_github_feedback(command: &str) -> Option<Vec<String>> {
    let prefix = "address github feedback ";
    if !command.to_ascii_lowercase().starts_with(prefix) {
        return None;
    }
    let ids = command[prefix.len()..]
        .split([',', ' '])
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    (!ids.is_empty()).then_some(ids)
}
