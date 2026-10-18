//! Durable Slack Slice bindings and the narrow scheduler submission boundary.
//!
//! Plan approval writes proposed Work and its Slice binding under
//! `.fluent/work/slack/slices/`; execution approval is a separate action that
//! submits stable requests to the scheduler. Read methods project persisted
//! Work state, while approval and reconciliation methods mutate only their
//! durable bindings or the Work model.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{
    CandidateReworkBoundary, CandidateReworkRequest, GitHubCandidate, GitHubPublication,
    GitHubPublishRequest, GitHubReviewClient, GitHubReviewFeedback,
};
use crate::atomic_write::atomic_write;
use crate::work_model::{
    DurableTransitionMetadata, DurableTransitionOrigin, DurableWorkTransition, ExecutionAuthority,
    ExecutionAuthorization, WorkItem, WorkModelStore, validate_work_item_id,
};

pub use crate::conversation::{PlannedWork, WorkExecutionRequest, WorkQueueOutcome, WorkScheduler};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovedPlanReport {
    pub slice_id: String,
    pub work_item_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionApprovalReport {
    pub slice_id: String,
    pub outcomes: Vec<(String, WorkQueueOutcome)>,
}

/// Public, durable Work status rendered for a Slack thread.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SliceWorkStatus {
    pub work_item_id: String,
    pub state: String,
    pub next_action: String,
}

/// Projected Slice state. A completed Slice remains readable and can be linked
/// to future corrective Work without losing its original record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SliceStage {
    Active,
    Complete,
}

/// Durable progress for one approved plan's Work materialization.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PlanApprovalState {
    Materializing,
    Complete,
}

impl Default for PlanApprovalState {
    fn default() -> Self {
        Self::Complete
    }
}

/// Durable Slice-to-Work binding, stored beside the bound project's Work model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SliceWork {
    pub id: String,
    pub approved_by: String,
    pub work_item_ids: Vec<String>,
    #[serde(default)]
    pub planned_work: Vec<PlannedWork>,
    #[serde(default)]
    pub approval_state: PlanApprovalState,
    #[serde(default)]
    pub execution_requests: Vec<WorkExecutionRequest>,
    #[serde(default)]
    acknowledged_request_ids: Vec<String>,
    #[serde(default)]
    transition_cursors: BTreeMap<String, u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github_publication: Option<GitHubPublication>,
}

/// A bounded, ordered status change observed from the authoritative Work
/// aggregate. The originating actor stays optional because direct Fluent
/// mutations do not always carry one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkTransition {
    pub sequence: u64,
    pub work_item_id: String,
    #[serde(default)]
    pub object_id: String,
    pub old_state: Option<String>,
    pub new_state: String,
    #[serde(default)]
    pub origin: DurableTransitionOrigin,
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_ref: Option<String>,
    #[serde(default = "default_work_item_kind")]
    pub object_kind: String,
    #[serde(default)]
    pub occurred_at: Option<String>,
    #[serde(default)]
    pub elapsed_seconds: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate_commit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comparison_base: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub completed_tests: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub completed_reviews: Vec<String>,
}

fn default_work_item_kind() -> String {
    "work-item".into()
}

/// Render one bounded, attributed transition reply for a Slack thread. The
/// caller supplies links only after it has verified that they are safe public
/// evidence references.
pub fn render_transition_reply(transition: &WorkTransition, evidence: &[String]) -> String {
    let object_id = if transition.object_id.is_empty() {
        &transition.work_item_id
    } else {
        &transition.object_id
    };
    let mut reply = format!(
        "{} {} for Work Item {} is now {}.",
        transition.object_kind, object_id, transition.work_item_id, transition.new_state
    );
    if transition.origin != DurableTransitionOrigin::Unknown {
        let origin = transition.origin;
        reply.push_str(&format!(" Origin: {origin}."));
    }
    if let Some(reason) = &transition.reason {
        reply.push_str(&format!(" Next action: {reason}."));
    }
    reply.push_str(&format!(
        " Elapsed: {}s.",
        transition.elapsed_seconds.unwrap_or(0)
    ));
    if !evidence.is_empty() {
        reply.push_str(&format!(" Evidence: {}.", evidence.join(", ")));
    }
    reply
}

/// Materializes approved plans without starting execution, then independently
/// authorizes their stable scheduler requests.
pub struct WorkControl {
    project_root: PathBuf,
    namespace: &'static str,
}

impl WorkControl {
    pub const REQUEST_VERSION: u32 = 1;

    pub fn new(project_root: impl Into<PathBuf>) -> Self {
        Self {
            project_root: project_root.into(),
            namespace: "slack",
        }
    }

    /// Keep Discord Work bindings separate while sharing the existing Work policy.
    pub fn for_discord(project_root: impl Into<PathBuf>) -> Self {
        Self {
            project_root: project_root.into(),
            namespace: "discord",
        }
    }

    /// Materialize one approved plan as proposed Work without starting it.
    pub fn approve_plan(
        &self,
        slice_id: &str,
        approver: &str,
        planned_work: &[PlannedWork],
    ) -> Result<ApprovedPlanReport> {
        if slice_id.trim().is_empty() || approver.trim().is_empty() || planned_work.is_empty() {
            bail!("an approved plan requires a Slice, approver, and Work Items");
        }
        validate_slice_id(slice_id)?;
        let ids: Vec<_> = planned_work.iter().map(|work| work.id.clone()).collect();
        if ids.iter().any(|id| id.trim().is_empty())
            || planned_work.iter().any(|work| work.title.trim().is_empty())
            || ids.len() != ids.iter().collect::<std::collections::BTreeSet<_>>().len()
        {
            bail!("approved plan Work Items must have unique non-empty identifiers and titles");
        }
        for id in &ids {
            validate_work_item_id(id)?;
        }
        let _slice_lock = crate::lease::acquire_blocking(&self.slice_lock_path(slice_id)?)
            .with_context(|| format!("lock approved Slice {slice_id}"))?;
        let mut slice = if let Some(existing) = self.slice(slice_id)? {
            let same_plan = existing.work_item_ids == ids
                && existing.approved_by == approver
                && (existing.planned_work.is_empty() || existing.planned_work == planned_work);
            if !same_plan {
                bail!("Slice {slice_id:?} already has a different approved plan");
            }
            if existing.approval_state == PlanApprovalState::Complete {
                return Ok(ApprovedPlanReport {
                    slice_id: existing.id,
                    work_item_ids: existing.work_item_ids,
                });
            }
            existing
        } else {
            let staged = SliceWork {
                id: slice_id.into(),
                approved_by: approver.into(),
                work_item_ids: ids.clone(),
                planned_work: planned_work.to_vec(),
                approval_state: PlanApprovalState::Materializing,
                execution_requests: Vec::new(),
                acknowledged_request_ids: Vec::new(),
                transition_cursors: BTreeMap::new(),
                github_publication: None,
            };
            self.save_slice(&staged)?;
            staged
        };

        let store = WorkModelStore::new(&self.project_root);
        let metadata = DurableTransitionMetadata {
            origin: if self.namespace == "slack" {
                DurableTransitionOrigin::SlackThread
            } else {
                DurableTransitionOrigin::External
            },
            reason: Some(format!("Plan approved by {approver}")),
            evidence_ref: Some(format!("{}-slice:{slice_id}", self.namespace)),
        };
        for work in planned_work {
            let path = store.work_item_path(&work.id)?;
            if path.exists() {
                let existing = store.read_work_item(&work.id).with_context(|| {
                    format!("inspect planned Work Item {} for Slice {slice_id}", work.id)
                })?;
                if !approved_work_matches(&existing, work, &metadata) {
                    bail!(
                        "planned Work Item {} already exists outside Slice {slice_id}",
                        work.id
                    );
                }
            }
        }
        for work in planned_work {
            let item = WorkItem {
                id: work.id.clone(),
                title: work.title.clone(),
                authorization: ExecutionAuthorization::Proposed,
                ..WorkItem::default()
            };
            if let Err(create_error) = store.create_work_item_with_transition(&item, &metadata) {
                match store.read_work_item(&work.id) {
                    Ok(existing) if approved_work_matches(&existing, work, &metadata) => {}
                    _ => {
                        return Err(create_error).with_context(|| {
                            format!("create planned Work Item {} for Slice {slice_id}", work.id)
                        });
                    }
                }
            }
        }
        slice.approval_state = PlanApprovalState::Complete;
        self.save_slice(&slice)?;
        Ok(ApprovedPlanReport {
            slice_id: slice_id.into(),
            work_item_ids: ids,
        })
    }

    pub fn authorize_execution(
        &self,
        slice_id: &str,
        approver: &str,
        scheduler: &dyn WorkScheduler,
    ) -> Result<ExecutionApprovalReport> {
        let _slice_lock = crate::lease::acquire_blocking(&self.slice_lock_path(slice_id)?)
            .with_context(|| format!("lock approved Slice {slice_id}"))?;
        let mut slice = self
            .slice(slice_id)?
            .with_context(|| format!("approved Slice {slice_id} is unavailable"))?;
        if slice.approval_state != PlanApprovalState::Complete {
            bail!("approved Slice {slice_id} is still materializing its planned Work");
        }
        let store = WorkModelStore::new(&self.project_root);
        let mut outcomes = Vec::with_capacity(slice.work_item_ids.len());
        for work_item_id in &slice.work_item_ids {
            let existing_request = slice
                .execution_requests
                .iter()
                .find(|request| request.work_item_id == *work_item_id)
                .cloned();
            let request = existing_request
                .clone()
                .unwrap_or_else(|| WorkExecutionRequest {
                    version: Self::REQUEST_VERSION,
                    id: request_id(self.namespace, slice_id, work_item_id),
                    slice_id: slice_id.into(),
                    work_item_id: work_item_id.clone(),
                    authorized_by: approver.into(),
                    created_at: chrono::Utc::now().to_rfc3339(),
                });
            if existing_request.is_none() {
                slice.execution_requests.push(request.clone());
                self.save_slice(&slice)?;
            }
            let metadata = DurableTransitionMetadata {
                origin: if self.namespace == "slack" {
                    DurableTransitionOrigin::SlackThread
                } else {
                    DurableTransitionOrigin::External
                },
                reason: Some(format!("Execution approved by {approver}")),
                evidence_ref: Some(format!("{}-slice:{slice_id}", self.namespace)),
            };
            store.mutate_work_item_with_transition(work_item_id, &metadata, |item| {
                item.authorization =
                    ExecutionAuthorization::execution_ready(ExecutionAuthority::Human);
                Ok(())
            })?;
            let outcome = if slice.acknowledged_request_ids.contains(&request.id) {
                WorkQueueOutcome::Queued {
                    request_id: request.id.clone(),
                }
            } else {
                // The scheduler owns idempotency. A persisted request is intent,
                // not proof of delivery: retry the same identity after a lost or
                // failed acknowledgement instead of reporting a synthetic queue.
                let outcome = scheduler.submit(&request)?;
                slice.acknowledged_request_ids.push(request.id.clone());
                self.save_slice(&slice)?;
                outcome
            };
            outcomes.push((work_item_id.clone(), outcome));
        }
        self.save_slice(&slice)?;
        Ok(ExecutionApprovalReport {
            slice_id: slice.id,
            outcomes,
        })
    }

    /// Publish only the named current ready candidate. The Slice record stores
    /// intent before the external call so a retry can reconcile partial success.
    pub fn publish_ready_candidate(
        &self,
        slice_id: &str,
        candidate_id: &str,
        remote: &str,
        github: &mut dyn GitHubReviewClient,
    ) -> Result<GitHubPublication> {
        let _slice_lock = crate::lease::acquire_blocking(&self.slice_lock_path(slice_id)?)
            .with_context(|| format!("lock GitHub publication for Slice {slice_id}"))?;
        let mut slice = self
            .slice(slice_id)?
            .with_context(|| format!("approved Slice {slice_id} is unavailable"))?;
        let candidate = self.current_ready_candidate(&slice)?;
        if candidate.candidate_id != candidate_id {
            bail!(
                "Merge Candidate {candidate_id} is stale; current ready candidate is {}",
                candidate.candidate_id
            );
        }
        if let Some(existing) = slice.github_publication.as_ref()
            && existing.candidate == candidate
            && existing.pull_request_url.is_some()
        {
            return Ok(existing.clone());
        }
        let existing = slice.github_publication.clone();
        let mut publication = GitHubPublication {
            slice_id: slice_id.to_owned(),
            remote: remote.to_owned(),
            head_branch: format!("fluent/{slice_id}"),
            candidate,
            candidate_history: existing
                .as_ref()
                .map(|record| {
                    let mut history = record.candidate_history.clone();
                    if !history.contains(&record.candidate) {
                        history.push(record.candidate.clone());
                    }
                    history
                })
                .unwrap_or_default(),
            pull_request_url: existing
                .as_ref()
                .and_then(|record| record.pull_request_url.clone()),
            feedback: existing
                .as_ref()
                .map(|record| record.feedback.clone())
                .unwrap_or_default(),
            routed_feedback_ids: existing
                .as_ref()
                .map(|record| record.routed_feedback_ids.clone())
                .unwrap_or_default(),
        };
        if !publication
            .candidate_history
            .contains(&publication.candidate)
        {
            publication
                .candidate_history
                .push(publication.candidate.clone());
        }
        slice.github_publication = Some(publication.clone());
        self.save_slice(&slice)?;
        let url = github.publish(
            &GitHubPublishRequest {
                checkout: self.project_root.clone(),
                publication: publication.clone(),
            },
            existing.as_ref(),
        )?;
        if url.trim().is_empty() {
            bail!("GitHub returned an empty pull-request URL");
        }
        publication.pull_request_url = Some(url);
        slice.github_publication = Some(publication.clone());
        self.save_slice(&slice)?;
        Ok(publication)
    }

    /// Advance an existing Fluent-created pull request only after a different
    /// candidate independently reaches the authoritative ready state.
    pub fn advance_published_candidate(
        &self,
        slice_id: &str,
        github: &mut dyn GitHubReviewClient,
    ) -> Result<Option<GitHubPublication>> {
        let slice = self
            .slice(slice_id)?
            .with_context(|| format!("approved Slice {slice_id} is unavailable"))?;
        let Some(existing) = slice.github_publication.as_ref() else {
            return Ok(None);
        };
        let current = self.current_ready_candidate(&slice)?;
        if current == existing.candidate {
            return Ok(None);
        }
        self.publish_ready_candidate(slice_id, &current.candidate_id, &existing.remote, github)
            .map(Some)
    }

    /// Schedule unseen feedback before advancing the durable seen set.
    pub fn schedule_github_feedback(
        &self,
        slice_id: &str,
        github: &mut dyn GitHubReviewClient,
        mut schedule: impl FnMut(&GitHubReviewFeedback) -> Result<()>,
    ) -> Result<Vec<GitHubReviewFeedback>> {
        let _slice_lock = crate::lease::acquire_blocking(&self.slice_lock_path(slice_id)?)
            .with_context(|| format!("lock GitHub feedback for Slice {slice_id}"))?;
        let mut slice = self
            .slice(slice_id)?
            .with_context(|| format!("approved Slice {slice_id} is unavailable"))?;
        let publication = slice
            .github_publication
            .as_mut()
            .context("Slice has no Fluent-created pull request")?;
        let received = github.review_feedback(&self.project_root, publication)?;
        let mut unseen = Vec::new();
        let mut publication_changed = false;
        for feedback in received {
            if let Some(seen) = publication
                .feedback
                .iter_mut()
                .find(|seen| seen.id == feedback.id)
            {
                if seen.thread_id != feedback.thread_id
                    || seen.comment_id != feedback.comment_id
                    || seen.url != feedback.url
                    || seen.candidate_commit != feedback.candidate_commit
                {
                    bail!("GitHub feedback identity changed for {}", feedback.id);
                }
                if seen.kind != feedback.kind
                    || seen.body != feedback.body
                    || seen.resolved != feedback.resolved
                {
                    seen.kind = feedback.kind;
                    seen.body = feedback.body;
                    seen.resolved = feedback.resolved;
                    publication_changed = true;
                }
                continue;
            }
            schedule(&feedback)?;
            publication.feedback.push(feedback.clone());
            unseen.push(feedback);
            publication_changed = true;
        }
        if publication_changed {
            self.save_slice(&slice)?;
        }
        Ok(unseen)
    }

    /// Route an approver-selected subset through the candidate-rework owner.
    /// Merely ingesting feedback never calls this boundary.
    pub fn authorize_feedback_rework(
        &self,
        slice_id: &str,
        project_label: &str,
        actor: &str,
        feedback_ids: &[String],
        boundary: &dyn CandidateReworkBoundary,
    ) -> Result<CandidateReworkRequest> {
        if feedback_ids.is_empty() {
            bail!("candidate rework requires selected GitHub feedback");
        }
        let _slice_lock = crate::lease::acquire_blocking(&self.slice_lock_path(slice_id)?)
            .with_context(|| format!("lock GitHub rework for Slice {slice_id}"))?;
        let mut slice = self
            .slice(slice_id)?
            .with_context(|| format!("approved Slice {slice_id} is unavailable"))?;
        let publication = slice
            .github_publication
            .as_mut()
            .context("Slice has no Fluent-created pull request")?;
        let selected = feedback_ids
            .iter()
            .map(|id| {
                publication
                    .feedback
                    .iter()
                    .find(|feedback| feedback.id == *id && !feedback.resolved)
                    .cloned()
                    .with_context(|| format!("GitHub feedback {id} is unavailable or resolved"))
            })
            .collect::<Result<Vec<_>>>()?;
        let candidate_commit = selected
            .first()
            .map(|feedback| feedback.candidate_commit.as_str())
            .context("candidate rework requires selected GitHub feedback")?;
        if selected
            .iter()
            .any(|feedback| feedback.candidate_commit != candidate_commit)
        {
            bail!("selected GitHub feedback spans multiple candidate commits");
        }
        let candidate = publication
            .candidate_history
            .iter()
            .chain(std::iter::once(&publication.candidate))
            .find(|candidate| candidate.commit == candidate_commit)
            .cloned()
            .with_context(|| {
                format!("GitHub feedback targets unknown candidate commit {candidate_commit}")
            })?;
        let mut hasher = Sha256::new();
        hasher.update(b"fluent-github-rework-v1\0");
        hasher.update(slice_id.as_bytes());
        hasher.update([0]);
        for id in feedback_ids {
            hasher.update(id.as_bytes());
            hasher.update([0]);
        }
        let request = CandidateReworkRequest {
            id: format!("github-rework-{:x}", hasher.finalize()),
            slice_id: slice_id.to_owned(),
            project_label: project_label.to_owned(),
            work_item_id: candidate.work_item_id,
            candidate_id: candidate.candidate_id,
            candidate_commit: candidate.commit,
            authorized_by: actor.to_owned(),
            feedback: selected,
        };
        let already_routed = feedback_ids
            .iter()
            .all(|id| publication.routed_feedback_ids.contains(id));
        if !already_routed {
            boundary.submit_candidate_rework(&request)?;
            for id in feedback_ids {
                if !publication.routed_feedback_ids.contains(id) {
                    publication.routed_feedback_ids.push(id.clone());
                }
            }
            self.save_slice(&slice)?;
        }
        Ok(request)
    }

    fn current_ready_candidate(&self, slice: &SliceWork) -> Result<GitHubCandidate> {
        let store = WorkModelStore::new(&self.project_root);
        let mut ready = Vec::new();
        for work_item_id in &slice.work_item_ids {
            let item = store.read_work_item(work_item_id)?;
            for candidate in &item.merge_candidates {
                if candidate.ready_at.is_some()
                    && candidate.merge_state.status
                        == crate::work_model::MergeCandidateMergeStatus::Pending
                    && candidate.validate_advancement(&item).is_ok()
                {
                    ready.push((
                        candidate.ready_at.clone().unwrap_or_default(),
                        GitHubCandidate {
                            work_item_id: work_item_id.clone(),
                            candidate_id: candidate.id.clone(),
                            commit: candidate.candidate_commit.clone(),
                            target_branch: candidate.target_branch.clone(),
                        },
                    ));
                }
            }
        }
        ready.sort_by(|left, right| {
            left.0
                .cmp(&right.0)
                .then(left.1.candidate_id.cmp(&right.1.candidate_id))
        });
        ready
            .pop()
            .map(|(_, candidate)| candidate)
            .context("Slice has no ready Merge Candidate")
    }

    /// Read the persisted Slice binding without changing it.
    pub fn slice(&self, slice_id: &str) -> Result<Option<SliceWork>> {
        let path = self.slice_path(slice_id)?;
        match fs::read(&path) {
            Ok(bytes) => {
                Ok(Some(serde_json::from_slice(&bytes).with_context(|| {
                    format!("parse Slice Work {}", path.display())
                })?))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error).with_context(|| format!("read Slice Work {}", path.display())),
        }
    }

    /// Read the authoritative persisted aggregate instead of inferring status
    /// from scheduler queues or prior Slack replies.
    pub fn status(&self, slice_id: &str) -> Result<Vec<SliceWorkStatus>> {
        let slice = self
            .slice(slice_id)?
            .with_context(|| format!("approved Slice {slice_id} is unavailable"))?;
        let store = WorkModelStore::new(&self.project_root);
        slice
            .work_item_ids
            .iter()
            .map(|work_item_id| {
                let item = store.read_work_item(work_item_id)?;
                let (state, next_action) = if item.authorization.is_proposed() {
                    ("planned", "Await execution approval")
                } else if let Some(attempt) = item.attempts.last() {
                    match attempt.status {
                        crate::work_model::AttemptStatus::NeedsUser => {
                            ("needs-user", "Respond to the current recovery request")
                        }
                        crate::work_model::AttemptStatus::Failed => {
                            ("failed", "Inspect durable evidence and permitted recovery")
                        }
                        crate::work_model::AttemptStatus::Complete => {
                            ("complete", "Await candidate or landing outcome")
                        }
                        crate::work_model::AttemptStatus::Planned => {
                            ("queued", "Await scheduler capacity")
                        }
                        crate::work_model::AttemptStatus::Executing => {
                            ("executing", "Await Attempt progress")
                        }
                        crate::work_model::AttemptStatus::Reviewing => {
                            ("reviewing", "Await review and Tester results")
                        }
                    }
                } else {
                    ("authorized", "Await scheduler dispatch")
                };
                Ok(SliceWorkStatus {
                    work_item_id: work_item_id.clone(),
                    state: state.into(),
                    next_action: next_action.into(),
                })
            })
            .collect()
    }

    /// Read the model-owned ledger after this Slack consumer's independent
    /// cursor without acknowledging delivery. Callers that perform an external
    /// effect must use [`Self::schedule_durable_transitions`] so a durable
    /// outbox entry exists before the cursor advances.
    pub fn reconcile_durable_transitions(&self, slice_id: &str) -> Result<Vec<WorkTransition>> {
        let slice = self
            .slice(slice_id)?
            .with_context(|| format!("approved Slice {slice_id} is unavailable"))?;
        let store = WorkModelStore::new(&self.project_root);
        let mut reported = Vec::new();
        for work_item_id in &slice.work_item_ids {
            let item = match store.read_work_item(work_item_id) {
                Ok(item) => item,
                Err(_) if slice.approval_state == PlanApprovalState::Materializing => continue,
                Err(error) => return Err(error.into()),
            };
            let cursor = slice
                .transition_cursors
                .get(work_item_id)
                .copied()
                .unwrap_or(0);
            for entry in item
                .transitions
                .iter()
                .filter(|entry| entry.sequence > cursor)
            {
                reported.push(project_durable_transition(work_item_id, &item, entry));
            }
        }
        reported.sort_by(|left, right| {
            left.sequence
                .cmp(&right.sequence)
                .then_with(|| left.work_item_id.cmp(&right.work_item_id))
        });
        Ok(reported)
    }

    /// Schedule each unseen transition before advancing its durable cursor.
    /// Repeating after a crash is safe when `schedule` uses the transition's
    /// stable identity at an idempotent outbox boundary.
    pub fn schedule_durable_transitions(
        &self,
        slice_id: &str,
        mut schedule: impl FnMut(&WorkTransition) -> Result<()>,
    ) -> Result<Vec<WorkTransition>> {
        let mut slice = self
            .slice(slice_id)?
            .with_context(|| format!("approved Slice {slice_id} is unavailable"))?;
        let store = WorkModelStore::new(&self.project_root);
        let mut pending = Vec::new();
        for work_item_id in &slice.work_item_ids {
            let item = match store.read_work_item(work_item_id) {
                Ok(item) => item,
                Err(_) if slice.approval_state == PlanApprovalState::Materializing => continue,
                Err(error) => return Err(error.into()),
            };
            let cursor = slice
                .transition_cursors
                .get(work_item_id)
                .copied()
                .unwrap_or(0);
            pending.extend(
                item.transitions
                    .iter()
                    .filter(|entry| entry.sequence > cursor)
                    .map(|entry| project_durable_transition(work_item_id, &item, entry)),
            );
        }
        pending.sort_by(|left, right| {
            left.occurred_at
                .cmp(&right.occurred_at)
                .then_with(|| left.work_item_id.cmp(&right.work_item_id))
                .then_with(|| left.sequence.cmp(&right.sequence))
        });
        for transition in &pending {
            schedule(transition)?;
            slice
                .transition_cursors
                .insert(transition.work_item_id.clone(), transition.sequence);
        }
        if !pending.is_empty() {
            self.save_slice(&slice)?;
        }
        Ok(pending)
    }

    /// Complete a Slice only when every Work Item's latest required Attempt is
    /// complete and every candidate has landed without a pending post-land
    /// failure. Failed and needs-user Attempts keep the Slice active.
    pub fn slice_stage(&self, slice_id: &str) -> Result<SliceStage> {
        let slice = self
            .slice(slice_id)?
            .with_context(|| format!("approved Slice {slice_id} is unavailable"))?;
        let store = WorkModelStore::new(&self.project_root);
        let mut completed = !slice.work_item_ids.is_empty();
        for work_item_id in &slice.work_item_ids {
            let item = store.read_work_item(work_item_id)?;
            let attempt_complete = matches!(
                item.attempts.last().map(|attempt| &attempt.status),
                Some(crate::work_model::AttemptStatus::Complete)
            );
            let candidates_landed = item.merge_candidates.iter().all(|candidate| {
                candidate.merge_state.status == crate::work_model::MergeCandidateMergeStatus::Merged
                    && candidate.merge_state.follow_up_failure.is_none()
            });
            completed &= attempt_complete && candidates_landed;
        }
        Ok(if completed {
            SliceStage::Complete
        } else {
            SliceStage::Active
        })
    }

    /// Link separately created corrective Work to its completed originating
    /// Slice. The new non-terminal Work immediately reactivates the Slice.
    pub fn link_corrective_work(&self, slice_id: &str, work_item_id: &str) -> Result<()> {
        if self.slice_stage(slice_id)? != SliceStage::Complete {
            bail!("corrective Work can only extend a completed Slice");
        }
        let store = WorkModelStore::new(&self.project_root);
        store.read_work_item(work_item_id)?;
        let mut slice = self
            .slice(slice_id)?
            .with_context(|| format!("approved Slice {slice_id} is unavailable"))?;
        if !slice.work_item_ids.iter().any(|id| id == work_item_id) {
            slice.work_item_ids.push(work_item_id.to_owned());
            self.save_slice(&slice)?;
        }
        Ok(())
    }

    /// Reconcile host-validated post-land Work even after an earlier correction
    /// has already reactivated the Slice.
    pub(crate) fn link_post_land_corrective_work_items(
        &self,
        slice_id: &str,
        work_item_ids: &[String],
    ) -> Result<()> {
        let mut slice = self
            .slice(slice_id)?
            .with_context(|| format!("approved Slice {slice_id} is unavailable"))?;
        let store = WorkModelStore::new(&self.project_root);
        let mut unlinked = Vec::new();
        for work_item_id in work_item_ids {
            if slice.work_item_ids.contains(work_item_id) {
                continue;
            }
            let item = store.read_work_item(work_item_id)?;
            let origin = item
                .origin
                .provenance()
                .and_then(|provenance| provenance.work_item_id.as_deref());
            if !origin.is_some_and(|origin| slice.work_item_ids.iter().any(|id| id == origin)) {
                bail!("post-land corrective Work is unrelated to Slice {slice_id}");
            }
            unlinked.push(work_item_id.clone());
        }
        if !unlinked.is_empty() {
            slice.work_item_ids.extend(unlinked);
            self.save_slice(&slice)?;
        }
        Ok(())
    }

    /// Render the pinned thread status from durable model state. The caller
    /// supplies provider state because it is runner-local rather than Work
    /// model authority.
    pub fn render_status_message(
        &self,
        slice_id: &str,
        project_label: &str,
        checkout_label: &str,
        provider_state: &str,
    ) -> Result<String> {
        let statuses = self.status(slice_id)?;
        let stage = self.slice_stage(slice_id)?;
        let work = statuses
            .iter()
            .map(|status| format!("{}: {}", status.work_item_id, status.state))
            .collect::<Vec<_>>()
            .join(", ");
        let next = statuses
            .first()
            .map(|status| status.next_action.as_str())
            .unwrap_or("No Work is planned");
        Ok(format!(
            "Project: {project_label}; checkout: {checkout_label}; Slice: {stage:?}; provider: {provider_state}; Work: {work}. Next action: {next}."
        ))
    }

    /// Explain a durable exceptional transition without publishing externally.
    pub fn render_actionable_transition(
        &self,
        transition: &WorkTransition,
        evidence: &[String],
        github_remote: Option<&str>,
    ) -> String {
        let mut reply = render_transition_reply(transition, evidence);
        match transition.new_state.as_str() {
            "needs-user" => reply.push_str(" Reply with the requested recovery decision."),
            "failed" => {
                reply.push_str(" Inspect the evidence and use only the permitted recovery action.")
            }
            "ready" => {
                reply.push_str(" Candidate is ready for review.");
                if let Some(commit) = &transition.candidate_commit {
                    reply.push_str(&format!(" Commit: {commit}."));
                    if !transition.completed_tests.is_empty() {
                        reply.push_str(&format!(
                            " Tests: {}.",
                            transition.completed_tests.join(", ")
                        ));
                    }
                    if !transition.completed_reviews.is_empty() {
                        reply.push_str(&format!(
                            " Reviews: {}.",
                            transition.completed_reviews.join(", ")
                        ));
                    }
                    if let Some(remote) = github_remote {
                        reply.push_str(&format!(" Review {remote}/commit/{commit}."));
                        if let Some(base) = &transition.comparison_base {
                            reply
                                .push_str(&format!(" Compare {remote}/compare/{base}...{commit}."));
                        }
                        reply.push_str(" Optionally create a pull request after review.");
                    }
                }
            }
            _ => {}
        }
        reply
    }

    /// Preserve a completed Slice and offer an explicit linked conversation for
    /// newly requested product work. This does not create or authorize Work.
    pub fn linked_slice_offer(
        &self,
        completed_slice_id: &str,
        requested_work: &str,
    ) -> Result<String> {
        if self.slice_stage(completed_slice_id)? != SliceStage::Complete {
            bail!("Slice {completed_slice_id:?} is not complete");
        }
        let mut hasher = Sha256::new();
        hasher.update(b"fluent-slack-linked-slice-v1\0");
        hasher.update(completed_slice_id.as_bytes());
        hasher.update([0]);
        hasher.update(requested_work.as_bytes());
        Ok(format!(
            "Slice {completed_slice_id} remains complete. Start linked Slice slice-{:x} for the requested product Work?",
            hasher.finalize()
        ))
    }

    /// Report an optional post-land Observation without changing Slice stage.
    pub fn render_optional_observation(
        &self,
        slice_id: &str,
        observation_ref: &str,
    ) -> Result<String> {
        if self.slice_stage(slice_id)? != SliceStage::Complete {
            bail!("optional Observations do not close an active Slice");
        }
        Ok(format!(
            "Slice {slice_id} remains complete. Observation: {observation_ref}."
        ))
    }

    fn save_slice(&self, slice: &SliceWork) -> Result<()> {
        let path = self.slice_path(&slice.id)?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        atomic_write(&path, &serde_json::to_vec_pretty(slice)?)?;
        Ok(())
    }

    fn slice_path(&self, slice_id: &str) -> Result<PathBuf> {
        validate_slice_id(slice_id)?;
        Ok(self
            .project_root
            .join(format!(".fluent/work/{}/slices", self.namespace))
            .join(format!("{slice_id}.json")))
    }

    fn slice_lock_path(&self, slice_id: &str) -> Result<PathBuf> {
        validate_slice_id(slice_id)?;
        Ok(self
            .project_root
            .join(format!(".fluent/work/{}/locks", self.namespace))
            .join(format!("{slice_id}.lock")))
    }
}

fn project_durable_transition(
    work_item_id: &str,
    item: &WorkItem,
    entry: &DurableWorkTransition,
) -> WorkTransition {
    let candidate = (entry.object_kind == "merge-candidate")
        .then(|| {
            item.merge_candidates
                .iter()
                .find(|candidate| candidate.id == entry.object_id)
        })
        .flatten();
    WorkTransition {
        sequence: entry.sequence,
        work_item_id: work_item_id.to_owned(),
        object_id: entry.object_id.clone(),
        old_state: entry.old_state.clone(),
        new_state: entry.new_state.clone(),
        origin: entry.origin.clone(),
        reason: entry.reason.clone(),
        evidence_ref: entry.evidence_ref.clone(),
        object_kind: entry.object_kind.clone(),
        occurred_at: Some(entry.occurred_at.clone()),
        elapsed_seconds: entry.elapsed_seconds,
        candidate_commit: candidate.map(|value| value.candidate_commit.clone()),
        comparison_base: candidate.map(|value| value.target_branch.clone()),
        completed_tests: candidate
            .map(|value| {
                value
                    .merge_state
                    .check_artifacts
                    .iter()
                    .map(|artifact| artifact.path.clone())
                    .collect()
            })
            .unwrap_or_default(),
        completed_reviews: candidate
            .map(|value| {
                value
                    .merge_state
                    .review_artifacts
                    .iter()
                    .map(|artifact| artifact.path.clone())
                    .collect()
            })
            .unwrap_or_default(),
    }
}

fn approved_work_matches(
    item: &WorkItem,
    planned: &PlannedWork,
    metadata: &DurableTransitionMetadata,
) -> bool {
    item.id == planned.id
        && item.title == planned.title
        && item.authorization.is_proposed()
        && item.transitions.iter().any(|transition| {
            transition.object_kind == "work-item"
                && transition.object_id == planned.id
                && transition.old_state.is_none()
                && transition.new_state == "planned"
                && transition.origin == metadata.origin
                && transition.reason == metadata.reason
                && transition.evidence_ref == metadata.evidence_ref
        })
}

fn validate_slice_id(slice_id: &str) -> Result<()> {
    validate_work_item_id(slice_id).map_err(Into::into)
}

fn request_id(namespace: &str, slice_id: &str, work_item_id: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(format!("fluent-{namespace}-work-request-v1\0").as_bytes());
    hasher.update(slice_id.as_bytes());
    hasher.update([0]);
    hasher.update(work_item_id.as_bytes());
    format!("{namespace}-work-{:x}", hasher.finalize())
}
