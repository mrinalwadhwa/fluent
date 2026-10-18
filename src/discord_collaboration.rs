//! Local Discord collaboration with an independent identity and storage namespace.
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::atomic_write::atomic_write;
use crate::conversation::{Checkout, ConversationProvider, ProviderScope, ProviderTurnRecovery};
use crate::slack_collaboration::checkout_is_available;
use sha2::{Digest, Sha256};
mod gateway;
mod service;
mod transport;
mod work;
pub use gateway::*;
pub use service::*;
pub use transport::*;
pub use work::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Project {
    pub checkout: Checkout,
    pub participants: BTreeSet<String>,
    pub approvers: BTreeSet<String>,
    pub provider: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiscordConfig {
    pub guild_id: String,
    pub channels: BTreeMap<String, BTreeSet<String>>,
    pub projects: BTreeMap<String, Project>,
}
impl DiscordConfig {
    pub fn validate(&self) -> Result<()> {
        snowflake(&self.guild_id)?;
        if self.channels.is_empty() || self.projects.is_empty() {
            bail!("configure a Discord server, text channel, and project");
        }
        for (label, project) in &self.projects {
            if label.is_empty()
                || label.len() > 64
                || !label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
            {
                bail!("project labels must use letters, digits, hyphens, or underscores");
            }
            if project.participants.is_empty()
                || !project.approvers.is_subset(&project.participants)
            {
                bail!("configure participants and a subset of project approvers");
            }
            for actor in &project.participants {
                snowflake(actor)?;
            }
            if !matches!(project.provider.as_str(), "codex" | "claude") {
                bail!("conversation provider must be codex or claude");
            }
        }
        for (channel, projects) in &self.channels {
            snowflake(channel)?;
            if projects.is_empty() || projects.iter().any(|p| !self.projects.contains_key(p)) {
                bail!("Discord channel references an unavailable project");
            }
        }
        Ok(())
    }
}
fn snowflake(value: &str) -> Result<()> {
    if value.starts_with('0')
        || value.parse::<u64>().is_err()
        || value.is_empty()
        || !value.bytes().all(|b| b.is_ascii_digit())
    {
        bail!("Discord identity must be a canonical positive snowflake");
    }
    Ok(())
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Input {
    pub id: String,
    pub guild: String,
    pub channel: String,
    pub actor: String,
    pub kind: InputKind,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum InputKind {
    Command {
        project: String,
        provider: Option<String>,
        text: String,
    },
    Message {
        text: String,
    },
    Edit {
        message: String,
        revision: String,
        text: String,
    },
    Delete {
        message: String,
    },
    Approval {
        action: String,
        revision: String,
    },
}
impl Input {
    fn key(&self) -> String {
        match &self.kind {
            InputKind::Edit {
                message, revision, ..
            } => format!("edit:{message}:{revision}"),
            InputKind::Delete { message } => format!("delete:{message}"),
            _ => self.id.clone(),
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Delivery {
    pub id: String,
    pub channel: String,
    pub text: String,
    pub message_id: Option<String>,
    #[serde(default)]
    pub approval: Option<ApprovalButton>,
}
pub trait DiscordTransport {
    /// Reconcile the command's root message before creating its unique public thread.
    fn ensure_thread(&mut self, command: &str, channel: &str, project: &str) -> Result<String>;
    /// Reconcile an existing delivery identity before posting again.
    fn deliver(&mut self, delivery: &Delivery) -> Result<String>;
    fn history(&mut self, _guild: &str, _thread: &str, _after: &str) -> Result<HistoryPage> {
        Ok(HistoryPage::default())
    }
    fn corrections(
        &mut self,
        _guild: &str,
        _thread: &str,
        _inputs: &[Input],
    ) -> Result<Vec<Input>> {
        Ok(vec![])
    }
}
#[derive(Default)]
pub struct HistoryPage {
    pub inputs: Vec<Input>,
    pub last_message: Option<String>,
    pub has_more: bool,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct PendingTurn {
    pub input: Input,
    pub dispatched: bool,
    pub accepted: Option<String>,
}
impl PendingTurn {
    fn new(input: Input) -> Self {
        Self {
            input,
            dispatched: false,
            accepted: None,
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Binding {
    guild: String,
    parent: String,
    project: String,
    checkout: String,
    provider: String,
    thread: String,
}
#[derive(Default, Serialize, Deserialize)]
struct Index {
    commands: BTreeMap<String, Binding>,
    threads: BTreeMap<String, Binding>,
}
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Thread {
    pub session: Option<String>,
    pub inputs: Vec<Input>,
    pub deliveries: Vec<Delivery>,
    #[serde(default)]
    pub pending: Option<PendingTurn>,
    #[serde(default)]
    pub history_cursor: String,
    #[serde(default)]
    pub approval: Option<PendingApproval>,
    #[serde(default)]
    pub approvals: Vec<AcceptedApproval>,
}
pub fn state_root(home: &Path) -> PathBuf {
    home.join(".config/fluent/discord")
}
fn read<T: serde::de::DeserializeOwned + Default>(path: &Path) -> Result<T> {
    match fs::read(path) {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
        Err(e) => Err(e.into()),
    }
}
fn write(path: &Path, value: &impl Serialize) -> Result<()> {
    fs::create_dir_all(path.parent().context("state requires a parent directory")?)?;
    atomic_write(path, &serde_json::to_vec_pretty(value)?)?;
    Ok(())
}
pub struct DiscordRunner<P, T> {
    home: PathBuf,
    config: DiscordConfig,
    provider: P,
    transport: T,
    recovered: bool,
}
impl<P: ConversationProvider, T: DiscordTransport> DiscordRunner<P, T> {
    pub fn new(home: &Path, config: DiscordConfig, provider: P, transport: T) -> Result<Self> {
        config.validate()?;
        Ok(Self {
            home: home.into(),
            config,
            provider,
            transport,
            recovered: false,
        })
    }
    pub fn provider(&self) -> &P {
        &self.provider
    }
    pub fn transport(&self) -> &T {
        &self.transport
    }
    pub fn ingest_recovered(&mut self, input: Input) -> Result<()> {
        self.recovered = true;
        let result = self.ingest(input);
        self.recovered = false;
        result
    }
    pub fn ingest(&mut self, input: Input) -> Result<()> {
        if config_path(&self.home).exists() {
            self.config = read_config(&self.home)?;
        }
        self.config.validate()?;
        persist_ingress(&self.home, &self.config, &input)?;
        for id in [&input.id, &input.guild, &input.channel, &input.actor] {
            snowflake(id)?;
        }
        if input.guild != self.config.guild_id {
            bail!("Discord server is not authorized");
        }
        let root = state_root(&self.home);
        let _lock = crate::lease::acquire_blocking(&root.join("state.lock"))?;
        let index_path = root.join("index.json");
        let mut index: Index = read(&index_path)?;
        let binding = match &input.kind {
            InputKind::Command {
                project, provider, ..
            } => {
                self.authorize(&input.channel, project, &input.actor)?;
                if let Some(existing) = index.commands.get(&input.id) {
                    existing.clone()
                } else {
                    let configured = &self.config.projects[project];
                    let provider = provider.as_deref().unwrap_or(&configured.provider);
                    if !matches!(provider, "codex" | "claude") {
                        bail!("provider is not available");
                    }
                    let thread =
                        self.transport
                            .ensure_thread(&input.id, &input.channel, project)?;
                    snowflake(&thread)?;
                    let binding = Binding {
                        guild: input.guild.clone(),
                        parent: input.channel.clone(),
                        project: project.clone(),
                        checkout: configured.checkout.identity.clone(),
                        provider: provider.into(),
                        thread: thread.clone(),
                    };
                    index.commands.insert(input.id.clone(), binding.clone());
                    index.threads.insert(thread, binding.clone());
                    write(&index_path, &index)?;
                    binding
                }
            }
            _ => index
                .threads
                .get(&input.channel)
                .context("Discord thread is not bound")?
                .clone(),
        };
        self.authorize(&binding.parent, &binding.project, &input.actor)?;
        let project = &self.config.projects[&binding.project];
        if binding.guild != self.config.guild_id || binding.checkout != project.checkout.identity {
            bail!("bound checkout authority changed");
        }
        let path = project
            .checkout
            .path
            .join(".fluent/work/discord/threads")
            .join(format!("{}.json", binding.thread));
        let mut thread: Thread = read(&path)?;
        if work::process_work(
            &self.home,
            project,
            &binding,
            &mut thread,
            &input,
            &path,
            self.recovered,
        )? {
            for i in 0..thread.deliveries.len() {
                if thread.deliveries[i].message_id.is_none() {
                    thread.deliveries[i].message_id =
                        Some(self.transport.deliver(&thread.deliveries[i])?);
                    write(&path, &thread)?;
                }
            }
            return Ok(());
        }
        let key = input.key();
        if thread
            .pending
            .as_ref()
            .is_some_and(|p| p.input.key() != key)
        {
            bail!("an earlier Discord turn needs recovery; this input remains queued");
        }
        if !thread.inputs.iter().any(|e| e.key() == key) || thread.pending.is_some() {
            let contribution = match &input.kind {
                InputKind::Approval { .. } => unreachable!("approvals cross the Work boundary"),
                InputKind::Command { text, .. } | InputKind::Message { text } => text.clone(),
                InputKind::Edit { message, text, .. } => {
                    let original = thread
                        .inputs
                        .iter()
                        .find(|e| e.id == *message && matches!(e.kind, InputKind::Message { .. }))
                        .context("edited message was not accepted")?;
                    if original.actor != input.actor {
                        bail!("edit author differs from accepted message author");
                    }
                    format!(
                        "Correction to accepted message {message} by {}: {text}",
                        input.actor
                    )
                }
                InputKind::Delete { message } => {
                    let original = thread
                        .inputs
                        .iter()
                        .find(|e| e.id == *message && matches!(e.kind, InputKind::Message { .. }))
                        .context("deleted message was not accepted")?;
                    if original.actor != input.actor {
                        bail!("deletion attribution differs from original author");
                    }
                    format!(
                        "Accepted message {message} by {} was deleted. Discord did not identify who deleted it; retain the accepted history.",
                        input.actor
                    )
                }
            };
            self.provider.select(&binding.provider)?;
            let scope = ProviderScope {
                checkout: project.checkout.path.clone(),
                slice_draft: project
                    .checkout
                    .path
                    .join(".fluent/work/discord/drafts")
                    .join(&binding.thread),
            };
            let action_id = format!("discord:{}:{key}", input.guild);
            if thread.pending.is_none() {
                thread.pending = Some(PendingTurn::new(input.clone()));
                thread.inputs.push(input.clone());
                write(&path, &thread)?;
            }
            let response = if thread.pending.as_ref().unwrap().dispatched {
                let session = thread.session.as_deref().context("Provider acceptance is uncertain. The preserved turn needs owner inspection; it will not be repeated.")?;
                match self.provider.recover_turn(&action_id, session, &scope)? {
                    ProviderTurnRecovery::Completed { reply, .. } => reply,
                    _ => bail!(
                        "Provider turn is accepted or uncertain; wait for recovery. Fluent will not repeat it."
                    ),
                }
            } else {
                if thread.session.is_none() {
                    thread.session = self.provider.prepare_session(&scope)?;
                    write(&path, &thread)?;
                }
                let session = thread.session.clone();
                thread.pending.as_mut().unwrap().dispatched = true;
                write(&path, &thread)?;
                let (new_session, response) = self.provider.run_turn(
                    &action_id,
                    session.as_deref(),
                    &scope,
                    &contribution,
                    &format!("discord:{}:{}", input.guild, input.actor),
                    &mut |turn| {
                        thread.pending.as_mut().unwrap().accepted = Some(turn.into());
                        write(&path, &thread)
                    },
                )?;
                if new_session.is_some() {
                    thread.session = new_session;
                }
                response
            };
            match serde_json::from_str::<crate::conversation::ProviderOutput>(&response) {
                Ok(crate::conversation::ProviderOutput::PlanningArtifact { kind, markdown }) => {
                    let artifact = crate::conversation::ArtifactRevision::new(&kind, &markdown);
                    work::offer_action(
                        &project.checkout.path,
                        &binding,
                        &mut thread,
                        WorkAction::Planning { artifact },
                        &markdown,
                    )?;
                }
                output => {
                    let text = match output {
                        Ok(crate::conversation::ProviderOutput::Message { text, .. }) => text,
                        _ => response,
                    };
                    thread.deliveries.push(Delivery {
                        id: format!("{key}:reply"),
                        channel: binding.thread.clone(),
                        text: format!(
                            "Project: {}\n\n{}",
                            binding.project,
                            text.replace(
                                project.checkout.path.to_string_lossy().as_ref(),
                                "[checkout]"
                            )
                        ),
                        message_id: None,
                        approval: None,
                    });
                }
            }
            thread.pending = None;
            write(&path, &thread)?;
        }
        for i in 0..thread.deliveries.len() {
            if thread.deliveries[i].message_id.is_none() {
                thread.deliveries[i].message_id =
                    Some(self.transport.deliver(&thread.deliveries[i])?);
                write(&path, &thread)?;
            }
        }
        Ok(())
    }
    pub fn recover(&mut self) -> Result<()> {
        if config_path(&self.home).exists() {
            self.config = read_config(&self.home)?;
        }
        let index: Index = read(&state_root(&self.home).join("index.json"))?;
        for binding in index.threads.values() {
            let Some(project) = self.config.projects.get(&binding.project) else {
                continue;
            };
            if binding.checkout != project.checkout.identity
                || !checkout_is_available(&project.checkout)
            {
                continue;
            }
            let path = project
                .checkout
                .path
                .join(".fluent/work/discord/threads")
                .join(format!("{}.json", binding.thread));
            let thread: Thread = read(&path)?;
            // An accepted turn precedes subsequent history. Ingest also reconciles its outbox.
            if let Some(pending) = thread.pending {
                self.ingest_recovered(pending.input)?;
            } else if let Some(last) = thread.inputs.last() {
                self.ingest_recovered(last.clone())?;
            }
            for _ in 0..100 {
                let current: Thread = read(&path)?;
                let page = self.transport.history(
                    &binding.guild,
                    &binding.thread,
                    &current.history_cursor,
                )?;
                for input in page.inputs {
                    self.ingest_recovered(input)?;
                }
                let mut current: Thread = read(&path)?;
                if let Some(cursor) = page.last_message {
                    current.history_cursor = cursor;
                    write(&path, &current)?;
                }
                if !page.has_more {
                    break;
                }
            }
            let current: Thread = read(&path)?;
            for input in
                self.transport
                    .corrections(&binding.guild, &binding.thread, &current.inputs)?
            {
                self.ingest_recovered(input)?;
            }
        }
        Ok(())
    }
    fn authorize(&self, channel: &str, label: &str, actor: &str) -> Result<()> {
        if !self
            .config
            .channels
            .get(channel)
            .is_some_and(|p| p.contains(label))
        {
            bail!("Discord channel is not authorized for this project");
        }
        let project = self
            .config
            .projects
            .get(label)
            .context("project is not configured")?;
        if !project.participants.contains(actor) {
            bail!("Discord participant is not authorized");
        }
        if !checkout_is_available(&project.checkout) {
            bail!("bound checkout is unavailable; reconfirm its identity locally");
        }
        Ok(())
    }
}
