use super::*;
use crate::conversation::{ArtifactRevision, PlannedWork};
use crate::slack_collaboration::{LocalSchedulerClient, WorkControl};
use crate::work_model::{MergeCandidateMergeStatus, WorkItem, WorkModelStore};

/// Host execution effects. Conversation routing and approval checks stay in the runner.
pub trait WorkEffects: Send {
    fn submit(
        &self,
        home: &Path,
        checkout: &Path,
        request: &crate::conversation::WorkExecutionRequest,
    ) -> Result<crate::conversation::WorkQueueOutcome>;
    fn land(
        &self,
        config: crate::work_merge_executor::WorkMergeConfig<'_>,
        approval: &crate::work_approval::WorkApproval,
    ) -> Result<crate::work_merge_executor::WorkMergeOutcome>;
}
pub(super) struct LocalWorkEffects;
impl WorkEffects for LocalWorkEffects {
    fn submit(
        &self,
        home: &Path,
        checkout: &Path,
        request: &crate::conversation::WorkExecutionRequest,
    ) -> Result<crate::conversation::WorkQueueOutcome> {
        use crate::conversation::WorkScheduler;
        LocalSchedulerClient::new(home, [checkout.to_path_buf()]).submit(request)
    }
    fn land(
        &self,
        config: crate::work_merge_executor::WorkMergeConfig<'_>,
        approval: &crate::work_approval::WorkApproval,
    ) -> Result<crate::work_merge_executor::WorkMergeOutcome> {
        crate::work_merge_executor::merge_candidate_with_approval(config, approval)
    }
}
struct SchedulerEffect<'a> {
    home: &'a Path,
    checkout: &'a Path,
    effects: &'a dyn WorkEffects,
}
impl crate::conversation::WorkScheduler for SchedulerEffect<'_> {
    fn submit(
        &self,
        request: &crate::conversation::WorkExecutionRequest,
    ) -> Result<crate::conversation::WorkQueueOutcome> {
        self.effects.submit(self.home, self.checkout, request)
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalButton {
    pub action: String,
    pub revision: String,
    pub label: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WorkAction {
    Planning { artifact: ArtifactRevision },
    Create,
    Execute,
    Resume { attempt: String },
    Land { candidate: String, commit: String },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingApproval {
    pub button: ApprovalButton,
    pub action: WorkAction,
    pub work_id: String,
    pub expected_state: String,
    #[serde(default)]
    pub expected_dispatch: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcceptedApproval {
    pub input: String,
    pub actor: String,
    pub pending: PendingApproval,
    pub completed: bool,
}
fn fingerprint(item: &WorkItem) -> Result<String> {
    Ok(crate::work_approval::WorkApproval::fingerprint(item))
}
fn current_state(checkout: &Path, id: &str) -> Result<String> {
    let store = WorkModelStore::new(checkout);
    if !store.work_item_path(id)?.exists() {
        return Ok("new".into());
    }
    fingerprint(&store.read_work_item(id)?)
}
fn binding(home: &Path, config: &DiscordConfig, thread_id: &str) -> Result<Binding> {
    snowflake(thread_id)?;
    let index: Index = read(&state_root(home).join("index.json"))?;
    let binding = index
        .threads
        .get(thread_id)
        .context("Discord thread is not bound")?
        .clone();
    let project = config
        .projects
        .get(&binding.project)
        .context("project authority was revoked")?;
    if !checkout_is_available(&project.checkout) || binding.checkout != project.checkout.identity {
        bail!("bound checkout is unavailable");
    }
    Ok(binding)
}
impl<P: ConversationProvider, T: DiscordTransport> DiscordRunner<P, T> {
    pub fn present_planning(
        &mut self,
        thread_id: &str,
        kind: &str,
        markdown: &str,
    ) -> Result<ApprovalButton> {
        if config_path(&self.home).exists() {
            self.config = read_config(&self.home)?;
        }
        let _lock = crate::lease::acquire_blocking(&state_root(&self.home).join("state.lock"))?;
        let binding = binding(&self.home, &self.config, thread_id)?;
        let checkout = self.config.projects[&binding.project].checkout.path.clone();
        let path = checkout
            .join(".fluent/work/discord/threads")
            .join(format!("{thread_id}.json"));
        let mut thread = read_thread(&path)?;
        let markdown = crate::conversation::sanitize_provider_reply(markdown, &checkout);
        let artifact = ArtifactRevision::new(kind, &markdown);
        let button = offer_action(
            &checkout,
            &binding,
            &mut thread,
            WorkAction::Planning { artifact },
            &markdown,
        )?;
        super::rendering::prepare_deliveries(&mut thread, &checkout);
        write(&path, &thread)?;
        for i in 0..thread.deliveries.len() {
            if thread.deliveries[i].message_id.is_none() {
                thread.deliveries[i].message_id =
                    Some(self.transport.deliver(&thread.deliveries[i])?);
                write(&path, &thread)?;
            }
        }
        Ok(button)
    }
}
pub(super) fn offer_action(
    checkout: &Path,
    binding: &Binding,
    thread: &mut Thread,
    action: WorkAction,
    text: &str,
) -> Result<ApprovalButton> {
    let work_id = format!("discord-{}", binding.thread);
    let expected_state = current_state(checkout, &work_id)?;
    if let WorkAction::Planning { artifact } = &action {
        let store = WorkModelStore::new(checkout);
        let item = if expected_state == "new" {
            WorkItem::default()
        } else {
            store.read_work_item(&work_id)?
        };
        item.ensure_planning_revision_can_advance(&artifact.kind)?;
    }
    let revision = format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&(&action, &expected_state))?)
    );
    let id = format!(
        "{:x}",
        Sha256::digest(
            format!("{}:{}:{revision}", binding.thread, thread.deliveries.len()).as_bytes()
        )
    )[..16]
        .to_owned();
    let button = ApprovalButton {
        action: id.clone(),
        revision,
        label: match action {
            WorkAction::Planning { .. } => "Approve revision",
            WorkAction::Create => "Create Work",
            WorkAction::Execute => "Execute Work",
            WorkAction::Resume { .. } => "Resume Work",
            WorkAction::Land { .. } => "Land candidate",
        }
        .into(),
    };
    thread.approval = Some(PendingApproval {
        button: button.clone(),
        action,
        work_id,
        expected_state,
        expected_dispatch: Some(format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&crate::queue::read_ledger(
                checkout,
                &format!("discord-{}", binding.thread)
            )?)?)
        )),
    });
    thread.deliveries.push(Delivery {
        id: format!("approval:{id}"),
        channel: binding.thread.clone(),
        text: text.into(),
        message_id: None,
        approval: Some(button.clone()),
    });
    Ok(button)
}
pub(super) fn process_work(
    home: &Path,
    project: &Project,
    binding: &Binding,
    thread: &mut Thread,
    input: &Input,
    path: &Path,
    recovered: bool,
    effects: &dyn WorkEffects,
) -> Result<bool> {
    let work_id = format!("discord-{}", binding.thread);
    let control = WorkControl::for_discord(&project.checkout.path);
    if let InputKind::Approval { action, revision } = &input.kind {
        if !project.approvers.contains(&input.actor) {
            bail!(RejectedInput(
                "Discord approval was revoked or actor is not a project approver"
            ));
        }
        if let Some(accepted) = thread
            .approvals
            .iter()
            .find(|approval| approval.input == input.key())
            .cloned()
        {
            if accepted.completed {
                return Ok(true);
            }
            let approval = work_approval(&accepted.pending);
            let applied = approval.applied(&project.checkout.path, &accepted.pending.work_id)?;
            if !applied
                && !thread.approval.as_ref().is_some_and(|p| {
                    p.button.action == accepted.pending.button.action
                        && p.button.revision == accepted.pending.button.revision
                })
            {
                bail!(RejectedInput(
                    "approval is stale; a newer action is pending"
                ));
            }
            apply_action(
                home,
                &project.checkout.path,
                &accepted.pending,
                &accepted.actor,
                effects,
            )?;
            thread
                .approvals
                .iter_mut()
                .find(|approval| approval.input == input.key())
                .unwrap()
                .completed = true;
        } else {
            let pending = thread
                .approval
                .clone()
                .ok_or(RejectedInput("approval is stale; no action is pending"))?;
            if recovered && !matches!(pending.action, WorkAction::Planning { .. }) {
                bail!(RejectedInput(
                    "fresh confirmation is required for recovered Work approval"
                ));
            }
            if !crate::conversation::approval_matches_revision(
                &pending.button.action,
                &pending.button.revision,
                action,
                revision,
            ) {
                bail!(RejectedInput(
                    "approval is stale; review the current action and revision"
                ));
            }
            if current_state(&project.checkout.path, &work_id)? != pending.expected_state {
                bail!(RejectedInput("Work changed; request a fresh approval"));
            }
            let actor = format!("discord:{}:{}", input.guild, input.actor);
            thread.approvals.push(AcceptedApproval {
                input: input.key(),
                actor: actor.clone(),
                pending: pending.clone(),
                completed: false,
            });
            write(path, thread)?;
            apply_action(home, &project.checkout.path, &pending, &actor, effects)?;
            thread.approvals.last_mut().unwrap().completed = true;
        }
        if thread
            .approval
            .as_ref()
            .is_some_and(|p| p.button.action == *action && p.button.revision == *revision)
        {
            thread.approval = None;
        }
        if !thread.inputs.iter().any(|i| i.key() == input.key()) {
            thread.inputs.push(input.clone());
        }
        if !thread
            .deliveries
            .iter()
            .any(|d| d.id == format!("{}:approved", input.id))
        {
            thread.deliveries.push(Delivery {
                id: format!("{}:approved", input.id),
                channel: binding.thread.clone(),
                text: format!(
                    "Approved by Discord user {}. {}",
                    input.actor,
                    work_status(&control, &project.checkout.path, &work_id)?
                ),
                message_id: None,
                approval: None,
            });
        }
        write(path, thread)?;
        return Ok(true);
    }
    let InputKind::Message { text } = &input.kind else {
        return Ok(false);
    };
    let command = text.trim();
    if !matches!(
        command,
        "work status" | "work create" | "work execute" | "work resume" | "work land"
    ) {
        return Ok(false);
    }
    if thread.inputs.iter().any(|i| i.key() == input.key()) {
        return Ok(true);
    }
    if command == "work status" {
        thread.deliveries.push(Delivery {
            id: format!("{}:status", input.id),
            channel: binding.thread.clone(),
            text: work_status(&control, &project.checkout.path, &work_id)?,
            message_id: None,
            approval: None,
        });
    } else {
        if !project.approvers.contains(&input.actor) {
            bail!(RejectedInput(
                "only project approvers may request Work actions"
            ));
        }
        let store = WorkModelStore::new(&project.checkout.path);
        let action = match command {
            "work create" => WorkAction::Create,
            "work execute" => {
                let item = store.read_work_item(&work_id)?;
                if item
                    .planning_context
                    .as_ref()
                    .and_then(|p| p.plan.as_ref())
                    .is_none()
                {
                    bail!("approve all planning stages before execution");
                }
                WorkAction::Execute
            }
            "work resume" => {
                let item = store.read_work_item(&work_id)?;
                let attempt = item.attempts.last().context("no Attempt to resume")?;
                WorkAction::Resume {
                    attempt: attempt.id.clone(),
                }
            }
            _ => {
                let item = store.read_work_item(&work_id)?;
                let candidate = item
                    .merge_candidates
                    .last()
                    .context("no Merge Candidate to land")?;
                if candidate.merge_state.status == MergeCandidateMergeStatus::Merged {
                    bail!("candidate already landed");
                }
                WorkAction::Land {
                    candidate: candidate.id.clone(),
                    commit: candidate.candidate_commit.clone(),
                }
            }
        };
        let description = format!(
            "Project: {}. Confirm {command} for {work_id}. Current state revision: {}",
            binding.project,
            current_state(&project.checkout.path, &work_id)?
        );
        offer_action(
            &project.checkout.path,
            binding,
            thread,
            action,
            &description,
        )?;
    }
    thread.inputs.push(input.clone());
    write(path, thread)?;
    Ok(true)
}
fn work_status(control: &WorkControl, checkout: &Path, id: &str) -> Result<String> {
    if control.slice(id)?.is_none() {
        return Ok(format!("Work {id} has not been created."));
    }
    let item = WorkModelStore::new(checkout).read_work_item(id)?;
    if let Some(candidate) = item.merge_candidates.last()
        && candidate.merge_state.status == MergeCandidateMergeStatus::Merged
    {
        return Ok(format!(
            "Work {id}: merged candidate {} at {}.",
            candidate.id,
            candidate
                .merge_state
                .merged_commit
                .as_deref()
                .unwrap_or(&candidate.candidate_commit)
        ));
    }
    Ok(control
        .status(id)?
        .iter()
        .map(|s| format!("{}: {}. {}", s.work_item_id, s.state, s.next_action))
        .collect::<Vec<_>>()
        .join("\n"))
}
fn work_approval(pending: &PendingApproval) -> crate::work_approval::WorkApproval {
    crate::work_approval::WorkApproval {
        operation: format!(
            "discord:{}:{}",
            pending.button.action, pending.button.revision
        ),
        expected_state: pending.expected_state.clone(),
    }
}
fn apply_action(
    home: &Path,
    checkout: &Path,
    pending: &PendingApproval,
    actor: &str,
    effects: &dyn WorkEffects,
) -> Result<()> {
    let control = WorkControl::for_discord(checkout);
    let store = WorkModelStore::new(checkout);
    let id = &pending.work_id;
    let approval = work_approval(pending);
    match &pending.action {
        WorkAction::Create | WorkAction::Planning { .. } => {
            if control.slice(id)?.is_none() {
                control.approve_plan(
                    id,
                    actor,
                    &[PlannedWork::new(
                        id,
                        format!("Discord project discussion {id}"),
                    )],
                )?;
            }
            if let WorkAction::Planning { artifact } = &pending.action {
                approval.mutate(checkout, id, |item| {
                    item.approve_planning_revision(
                        &artifact.kind,
                        &artifact.id,
                        artifact.markdown.clone(),
                    )
                })?;
            }
        }
        WorkAction::Execute => {
            let scheduler = SchedulerEffect {
                home,
                checkout,
                effects,
            };
            control.authorize_execution_with_approval(id, actor, &scheduler, Some(&approval))?;
        }
        WorkAction::Resume { attempt } => {
            crate::queue::resume_with_approval(
                checkout,
                id,
                attempt,
                &approval,
                pending
                    .expected_dispatch
                    .as_deref()
                    .context("request fresh resume approval")?,
            )?;
        }
        WorkAction::Land { candidate, commit } => {
            let item = store.read_work_item(id)?;
            let current = item
                .merge_candidates
                .iter()
                .find(|c| c.id == *candidate)
                .context("approved candidate is unavailable")?;
            if current.candidate_commit != *commit {
                bail!("approved candidate commit changed; request fresh approval");
            }
            if current.merge_state.status == MergeCandidateMergeStatus::Merged {
                return Ok(());
            }
            let resolver = crate::content::ContentResolver::new(Some(checkout));
            approval.admit(checkout, id)?;
            effects.land(
                crate::work_merge_executor::WorkMergeConfig {
                    project_root: checkout,
                    store: &store,
                    work_item_id: id,
                    merge_candidate_id: candidate,
                    resolver: &resolver,
                    extra_args: &[],
                    coder_kind: crate::coder::CoderKind::Codex,
                    coder_override: None,
                    model: None,
                    effort: None,
                    use_attempt_mapping: true,
                    no_sandbox: false,
                    run_post_merge_review: false,
                },
                &approval,
            )?;
        }
    }
    Ok(())
}
