//! Local Discord collaboration with an independent identity and storage namespace.
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::atomic_write::atomic_write;
use crate::conversation::{Checkout, ConversationProvider, ProviderScope};
use crate::slack_collaboration::checkout_is_available;

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
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Delivery {
    pub id: String,
    pub channel: String,
    pub text: String,
    pub message_id: Option<String>,
}
pub trait DiscordTransport {
    /// Reconcile the command's root message before creating its unique public thread.
    fn ensure_thread(&mut self, command: &str, channel: &str, project: &str) -> Result<String>;
    /// Reconcile an existing delivery identity before posting again.
    fn deliver(&mut self, delivery: &Delivery) -> Result<String>;
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
}
impl<P: ConversationProvider, T: DiscordTransport> DiscordRunner<P, T> {
    pub fn new(home: &Path, config: DiscordConfig, provider: P, transport: T) -> Result<Self> {
        config.validate()?;
        Ok(Self {
            home: home.into(),
            config,
            provider,
            transport,
        })
    }
    pub fn provider(&self) -> &P {
        &self.provider
    }
    pub fn transport(&self) -> &T {
        &self.transport
    }
    pub fn ingest(&mut self, input: Input) -> Result<()> {
        self.config.validate()?;
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
            InputKind::Message { .. } => index
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
        if !thread.inputs.iter().any(|e| e.id == input.id) {
            self.provider.select(&binding.provider)?;
            let scope = ProviderScope {
                checkout: project.checkout.path.clone(),
                slice_draft: project
                    .checkout
                    .path
                    .join(".fluent/work/discord/drafts")
                    .join(&binding.thread),
            };
            let text = match &input.kind {
                InputKind::Command { text, .. } | InputKind::Message { text } => text,
            };
            if thread.session.is_none() {
                thread.session = self.provider.prepare_session(&scope)?;
                write(&path, &thread)?;
            }
            let (session, response) = self.provider.run_turn(
                &format!("discord:{}:{}", input.guild, input.id),
                thread.session.as_deref(),
                &scope,
                text,
                &format!("discord:{}:{}", input.guild, input.actor),
                &mut |_| Ok(()),
            )?;
            if session.is_some() {
                thread.session = session;
            }
            thread.deliveries.push(Delivery {
                id: format!("{}:reply", input.id),
                channel: binding.thread.clone(),
                text: format!(
                    "Project: {}\n\n{}",
                    binding.project,
                    response.replace(
                        project.checkout.path.to_string_lossy().as_ref(),
                        "[checkout]"
                    )
                ),
                message_id: None,
            });
            thread.inputs.push(input);
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
