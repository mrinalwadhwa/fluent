//! Bounded, read-only projection of each selected checkout.
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::terminal_sessions::{self, TerminalDescriptor};
use crate::work_model::{TaskKind, WorkModelStore};
use crate::{queue, work_status};
use sha2::{Digest, Sha256};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Category {
    NeedsYou,
    Running,
    ReadyToLand,
    Queued,
    Ready,
    Planned,
    History,
    Unknown,
}
impl Category {
    pub fn label(self) -> &'static str {
        match self {
            Self::NeedsYou => "Needs you",
            Self::Running => "Running",
            Self::ReadyToLand => "Ready to land",
            Self::Queued => "Queued",
            Self::Ready => "Ready",
            Self::Planned => "Planned",
            Self::History => "History",
            Self::Unknown => "Unknown",
        }
    }
    pub fn from_published_action(action: &str, queued: bool) -> Self {
        match action {
            "complete" | "merged" | "abandoned" => Self::History,
            "needs-user" | "failed" | "evidence-needed" | "learner-blocked" | "merge-failed"
            | "landing-pending" => Self::NeedsYou,
            "executing" | "reviewing" | "merging" => Self::Running,
            "merge-ready" => Self::ReadyToLand,
            _ if queued => Self::Queued,
            "task-ready" | "learner-not-ready" => Self::Ready,
            "planned" | "not-started" => Self::Planned,
            _ => Self::Unknown,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RowKey {
    pub source_id: String,
    pub work_item_id: String,
    pub attempt_id: Option<String>,
    pub task_id: Option<String>,
    pub execution_generation: Option<String>,
}
impl RowKey {
    pub fn work(source_id: &str, work_item_id: &str) -> Self {
        Self {
            source_id: source_id.into(),
            work_item_id: work_item_id.into(),
            attempt_id: None,
            task_id: None,
            execution_generation: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Worker {
    pub key: RowKey,
    pub role: String,
    pub state: String,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub elapsed_ms: Option<u64>,
    pub terminal: Option<TerminalDescriptor>,
    pub unavailable_reason: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectWork {
    pub key: RowKey,
    pub status: work_status::WorkItemStatus,
    pub category: Category,
    pub phase: Option<String>,
    pub workers: Vec<Worker>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceSnapshot {
    pub schema_version: u32,
    pub id: String,
    pub host: String,
    pub path: PathBuf,
    pub project: String,
    pub work: Vec<ProjectWork>,
    pub queued_work_item_ids: Vec<String>,
    pub diagnostics: Vec<String>,
    pub stale: Option<String>,
}
impl SourceSnapshot {
    pub fn short_id(&self) -> String {
        format!("{:x}", Sha256::digest(self.id.as_bytes()))[..8].to_string()
    }

    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let path = path.canonicalize()?;
        let id = path.to_string_lossy().into_owned();
        let host = std::env::var("HOSTNAME").unwrap_or_else(|_| "local".into());
        let project = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| id.clone());
        let snapshot = WorkModelStore::new(&path).snapshot_work_items()?;
        let mut diagnostics = snapshot.diagnostics.clone();
        let descriptors = match terminal_sessions::list(&path) {
            Ok(value) => value,
            Err(error) => {
                diagnostics.push(format!("Terminal descriptors unavailable: {error:#}"));
                Vec::new()
            }
        };
        let mut queued_work_item_ids = Vec::new();
        let mut queue_error_ids = BTreeSet::new();
        for item in &snapshot.items {
            match queue::read_ledger(&path, &item.id) {
                Ok(Some(ledger))
                    if ledger.active().is_some_and(|dispatch| {
                        dispatch.status == queue::DispatchStatus::Queued
                    }) =>
                {
                    queued_work_item_ids.push(item.id.clone())
                }
                Ok(_) => {}
                Err(error) => {
                    diagnostics.push(format!(
                        "Queue state for {} unavailable: {error:#}",
                        item.id
                    ));
                    queue_error_ids.insert(item.id.clone());
                }
            }
        }
        let work = snapshot
            .items
            .iter()
            .map(|item| {
                let status = work_status::summarize_work_item(item, Some(&path));
                let queued = queued_work_item_ids.contains(&item.id);
                let category =
                    if queue_error_ids.contains(&item.id) && status.action == "task-ready" {
                        Category::Unknown
                    } else {
                        Category::from_published_action(&status.action, queued)
                    };
                let attempt = work_status::display_attempt(item, Some(&path));
                let phase = match status.action.as_str() {
                    "merging" | "landing-pending" => Some("Landing".into()),
                    "reviewing" => Some("Reviewing".into()),
                    _ => attempt
                        .and_then(|attempt| {
                            attempt.tasks.iter().find(|task| {
                                task.status == crate::work_model::TaskStatus::Executing
                            })
                        })
                        .map(|task| {
                            match task.kind {
                                TaskKind::Write => "Writing",
                                TaskKind::Review => "Reviewing",
                                TaskKind::Tester => "Testing",
                                TaskKind::Verification => "Verifying",
                                TaskKind::Learn => "Learning",
                                TaskKind::Merge => "Landing",
                                _ => "Running",
                            }
                            .into()
                        }),
                };
                let workers = attempt
                    .map(|attempt| {
                        attempt
                            .tasks
                            .iter()
                            .map(|task| {
                                let terminal = descriptors
                                    .iter()
                                    .find(|descriptor| {
                                        descriptor.work_item_id == item.id
                                            && descriptor.attempt_id == attempt.id
                                            && descriptor.task_id == task.id
                                    })
                                    .cloned();
                                let pair = attempt.coder_mapping.for_task_kind(task.kind);
                                let provider =
                                    terminal.as_ref().map(|d| d.provider.clone()).or_else(|| {
                                        matches!(
                                            task.kind,
                                            TaskKind::Write
                                                | TaskKind::Review
                                                | TaskKind::Learn
                                                | TaskKind::Rebase
                                        )
                                        .then(|| format!("{:?}", pair.coder))
                                    });
                                let elapsed_ms = task
                                    .started_at
                                    .as_deref()
                                    .and_then(|value| {
                                        chrono::DateTime::parse_from_rfc3339(value).ok()
                                    })
                                    .map(|start| {
                                        let end = task
                                            .completed_at
                                            .as_deref()
                                            .and_then(|value| {
                                                chrono::DateTime::parse_from_rfc3339(value).ok()
                                            })
                                            .unwrap_or_else(|| chrono::Utc::now().fixed_offset());
                                        end.signed_duration_since(start).num_milliseconds().max(0)
                                            as u64
                                    });
                                Worker {
                                    key: RowKey {
                                        source_id: id.clone(),
                                        work_item_id: item.id.clone(),
                                        attempt_id: Some(attempt.id.clone()),
                                        task_id: Some(task.id.clone()),
                                        execution_generation: terminal
                                            .as_ref()
                                            .map(|d| d.execution_generation.clone()),
                                    },
                                    role: task.role.clone(),
                                    state: work_status::effective_task_status_label(
                                        task,
                                        item,
                                        Some(&path),
                                    )
                                    .into(),
                                    provider,
                                    model: (!pair.model.is_empty()).then(|| pair.model.clone()),
                                    effort: pair.effort.clone(),
                                    elapsed_ms,
                                    unavailable_reason: terminal
                                        .as_ref()
                                        .and_then(|d| d.unavailable_reason.clone())
                                        .unwrap_or_else(|| {
                                            terminal_sessions::unavailable_reason(
                                                task.kind,
                                                task.status.clone(),
                                                task.started_at.is_some(),
                                            )
                                        }),
                                    terminal,
                                }
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                ProjectWork {
                    key: RowKey::work(&id, &item.id),
                    status,
                    category,
                    phase,
                    workers,
                }
            })
            .collect();
        Ok(Self {
            schema_version: 1,
            id,
            host,
            path,
            project,
            work,
            queued_work_item_ids,
            diagnostics,
            stale: None,
        })
    }
}

pub fn detail_values(sources: &[SourceSnapshot], key: &RowKey) -> Vec<String> {
    let Some((source, work)) = find_work(sources, key) else {
        return vec!["Selection unavailable".into()];
    };
    let next_action = match work.category {
        Category::Queued => "Awaiting queue dispatch".into(),
        Category::Running => "Workflow running".into(),
        Category::History => "No operator action".into(),
        _ => crate::guidance::next_action_for_status_row(&work.status)
            .map(|action| {
                action
                    .trim()
                    .trim_start_matches("→ Next:")
                    .trim()
                    .to_string()
            })
            .unwrap_or_else(|| "unavailable".into()),
    };
    let mut values = vec![
        work.status.title.clone(),
        format!("Source: {} · {}", source.host, source.path.display()),
        format!("Work Item: {}", work.status.id),
        format!("Status: {}", work.category.label()),
        format!("Phase: {}", work.phase.as_deref().unwrap_or("unavailable")),
        format!("Attempt: {}", work.status.attempt),
        format!("Next action: {next_action}"),
    ];
    if let Some(worker) = find_worker(sources, key) {
        values.extend([
            format!("Worker: {}", worker.role),
            format!(
                "Task: {}",
                worker.key.task_id.as_deref().unwrap_or("unavailable")
            ),
            format!("State: {}", worker.state),
            format!(
                "Invocation: {}",
                worker
                    .key
                    .execution_generation
                    .as_deref()
                    .unwrap_or("unavailable")
            ),
            format!(
                "Provider: {}",
                worker.provider.as_deref().unwrap_or("unavailable")
            ),
            format!(
                "Model: {}",
                worker.model.as_deref().unwrap_or("unavailable")
            ),
            format!(
                "Effort: {}",
                worker.effort.as_deref().unwrap_or("unavailable")
            ),
            format!(
                "Elapsed: {}",
                worker
                    .elapsed_ms
                    .map(|ms| format!("{}s", ms / 1000))
                    .unwrap_or_else(|| "unavailable".into())
            ),
            format!(
                "Work provider wait: {}",
                if work.status.metrics.provider_wait_ms > 0 {
                    format!("{}s", work.status.metrics.provider_wait_ms / 1000)
                } else {
                    "unavailable".into()
                }
            ),
            format!(
                "Agent terminal: {}",
                worker
                    .terminal
                    .as_ref()
                    .map(|terminal| if terminal.can_observe {
                        "Observe available".into()
                    } else {
                        worker.unavailable_reason.clone()
                    })
                    .unwrap_or_else(|| worker.unavailable_reason.clone())
            ),
        ]);
    }
    if let Some(stale) = &source.stale {
        values.push(format!("Stale source: {stale}"));
    }
    values.extend(
        source
            .diagnostics
            .iter()
            .map(|diagnostic| format!("Source diagnostic: {diagnostic}")),
    );
    values
}

pub fn discover_paths(
    current: &Path,
    explicit: &[PathBuf],
    home: &Path,
) -> (Vec<PathBuf>, Vec<String>) {
    let mut paths = BTreeSet::new();
    let mut errors = Vec::new();
    for path in std::iter::once(current).chain(explicit.iter().map(PathBuf::as_path)) {
        match path.canonicalize() {
            Ok(path) => {
                paths.insert(path);
            }
            Err(error) => errors.push(format!("{}: {error}", path.display())),
        }
    }
    match crate::scheduler_service::read_registry_for_dashboard(home) {
        Ok(entries) => {
            for path in entries.into_keys() {
                match path.canonicalize() {
                    Ok(path) => {
                        paths.insert(path);
                    }
                    Err(error) => errors.push(format!("{}: {error}", path.display())),
                }
            }
        }
        Err(error) => errors.push(format!("Scheduler registry: {error:#}")),
    }
    (paths.into_iter().collect(), errors)
}

pub fn refresh_sources(previous: &[SourceSnapshot], paths: &[PathBuf]) -> Vec<SourceSnapshot> {
    let old = previous
        .iter()
        .map(|source| (source.path.clone(), source))
        .collect::<BTreeMap<_, _>>();
    paths
        .iter()
        .map(|path| {
            let path = path.canonicalize().unwrap_or_else(|_| path.clone());
            match SourceSnapshot::load(&path) {
                Ok(source) => source,
                Err(error) => {
                    let mut source = old
                        .get(&path)
                        .map(|source| (*source).clone())
                        .unwrap_or_else(|| SourceSnapshot {
                            schema_version: 1,
                            id: path.to_string_lossy().into_owned(),
                            host: "local".into(),
                            path: path.clone(),
                            project: path
                                .file_name()
                                .map(|name| name.to_string_lossy().into_owned())
                                .unwrap_or_default(),
                            work: Vec::new(),
                            queued_work_item_ids: Vec::new(),
                            diagnostics: Vec::new(),
                            stale: None,
                        });
                    source.stale = Some(error.to_string());
                    source
                }
            }
        })
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Filter {
    All,
    NeedsYou,
    Running,
    Ready,
    Waiting,
    History,
}
impl Filter {
    pub const VIEWS: [Self; 6] = [
        Self::All,
        Self::NeedsYou,
        Self::Running,
        Self::Ready,
        Self::Waiting,
        Self::History,
    ];
    pub fn matches(self, category: Category) -> bool {
        match self {
            Self::All => true,
            Self::NeedsYou => category == Category::NeedsYou,
            Self::Running => category == Category::Running,
            Self::Ready => matches!(category, Category::Ready | Category::ReadyToLand),
            Self::Waiting => matches!(category, Category::Queued | Category::Planned),
            Self::History => category == Category::History,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::NeedsYou => "Needs you",
            Self::Running => "Running",
            Self::Ready => "Ready",
            Self::Waiting => "Waiting",
            Self::History => "History",
        }
    }
    pub fn next(self) -> Self {
        Self::VIEWS
            [(Self::VIEWS.iter().position(|view| *view == self).unwrap() + 1) % Self::VIEWS.len()]
    }
}

pub fn visible_keys(
    sources: &[SourceSnapshot],
    filter: Filter,
    expanded: &BTreeSet<RowKey>,
) -> Vec<RowKey> {
    let mut keys = Vec::new();
    for source in sources {
        for work in &source.work {
            if filter.matches(work.category) {
                keys.push(work.key.clone());
                if expanded.contains(&work.key) {
                    keys.extend(work.workers.iter().map(|worker| worker.key.clone()));
                }
            }
        }
    }
    keys
}

pub fn initial_key(sources: &[SourceSnapshot], filter: Filter) -> Option<RowKey> {
    let mut best: Option<(u8, RowKey)> = None;
    for source in sources {
        for work in &source.work {
            if !filter.matches(work.category) {
                continue;
            }
            let rank = match work.category {
                Category::NeedsYou => 0,
                Category::Running => 1,
                Category::Ready | Category::ReadyToLand => 2,
                _ => 3,
            };
            if best.as_ref().is_none_or(|(old, _)| rank < *old) {
                best = Some((rank, work.key.clone()));
            }
        }
    }
    best.map(|(_, key)| key)
}

pub fn find_work<'a>(
    sources: &'a [SourceSnapshot],
    key: &RowKey,
) -> Option<(&'a SourceSnapshot, &'a ProjectWork)> {
    sources
        .iter()
        .find(|source| source.id == key.source_id)
        .and_then(|source| {
            source
                .work
                .iter()
                .find(|work| work.key.work_item_id == key.work_item_id)
                .map(|work| (source, work))
        })
}
pub fn find_worker<'a>(sources: &'a [SourceSnapshot], key: &RowKey) -> Option<&'a Worker> {
    find_work(sources, key)?
        .1
        .workers
        .iter()
        .find(|worker| &worker.key == key)
}
