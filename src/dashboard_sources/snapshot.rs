use super::{MAX_SNAPSHOT_BYTES, SshSource, identifier};
use crate::dashboard::project::{Category, ProjectWork, RowKey, SourceSnapshot, Worker};
use crate::terminal_sessions::TerminalDescriptor;
use crate::work_status::{WorkItemStatus, WorkMetrics};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

// Keep host-local evidence and exceptional command text out of the wire schema.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    schema_version: u32,
    source_id: String,
    project_root: PathBuf,
    project: String,
    work: Vec<Work>,
    queued_work_item_ids: Vec<String>,
    diagnostics: Vec<String>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Work {
    id: String,
    title: String,
    attempt: String,
    task: String,
    review: String,
    merge_candidate: String,
    merge: String,
    action: String,
    phase: Option<String>,
    workers: Vec<Worker>,
}

pub fn encode_snapshot(source: &SourceSnapshot) -> Result<Vec<u8>> {
    let snapshot = Snapshot {
        schema_version: 1,
        source_id: source.id.clone(),
        project_root: source.path.clone(),
        project: source.project.clone(),
        queued_work_item_ids: source.queued_work_item_ids.clone(),
        diagnostics: source.diagnostics.clone(),
        work: source
            .work
            .iter()
            .map(|w| Work {
                id: w.status.id.clone(),
                title: w.status.title.clone(),
                attempt: w.status.attempt.clone(),
                task: w.status.task.clone(),
                review: w.status.review.clone(),
                merge_candidate: w.status.merge_candidate.clone(),
                merge: w.status.merge.clone(),
                action: w.status.action.clone(),
                phase: w.phase.clone(),
                workers: w.workers.clone(),
            })
            .collect(),
    };
    let bytes = serde_json::to_vec(&snapshot)?;
    ensure!(
        bytes.len() <= MAX_SNAPSHOT_BYTES,
        "remote snapshot exceeds 2 MiB; reduce published Work metadata"
    );
    Ok(bytes)
}

pub fn validate_descriptor(source: &SshSource, d: &TerminalDescriptor) -> Result<()> {
    source.validate()?;
    ensure!(
        d.schema_version == 1,
        "unsupported terminal protocol version"
    );
    ensure!(
        d.source_id == source.project_root.to_string_lossy()
            && d.project_identity == source.project_root.to_string_lossy(),
        "remote terminal project identity changed"
    );
    for id in [
        &d.work_item_id,
        &d.attempt_id,
        &d.task_id,
        &d.execution_generation,
        &d.session_id,
        &d.role,
        &d.provider,
    ] {
        identifier(id)?;
    }
    ensure!(
        !d.can_control || d.can_observe,
        "invalid terminal capabilities"
    );
    Ok(())
}

pub fn decode_snapshot(source: &SshSource, bytes: &[u8]) -> Result<SourceSnapshot> {
    source.validate()?;
    ensure!(
        bytes.len() <= MAX_SNAPSHOT_BYTES,
        "remote snapshot exceeds 2 MiB"
    );
    let value: serde_json::Value = serde_json::from_slice(bytes)?;
    validate_text(&value)?;
    let snapshot: Snapshot = serde_json::from_value(value)?;
    ensure!(
        snapshot.schema_version == 1,
        "unsupported remote snapshot protocol version"
    );
    ensure!(
        snapshot.project_root == source.project_root
            && snapshot.source_id == source.project_root.to_string_lossy(),
        "remote snapshot project identity changed"
    );
    let id = source.id();
    let mut work = Vec::new();
    let mut keys = std::collections::BTreeSet::new();
    for mut row in snapshot.work {
        identifier(&row.id)?;
        ensure!(
            keys.insert(row.id.clone()),
            "duplicate remote Work identity"
        );
        let mut workers = std::collections::BTreeSet::new();
        for worker in &mut row.workers {
            ensure!(
                worker.key.source_id == snapshot.source_id && worker.key.work_item_id == row.id,
                "remote worker source mismatch"
            );
            let attempt = worker
                .key
                .attempt_id
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("missing remote attempt"))?;
            let task = worker
                .key
                .task_id
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("missing remote task"))?;
            identifier(attempt)?;
            identifier(task)?;
            ensure!(
                workers.insert((attempt.to_string(), task.to_string())),
                "duplicate remote worker identity"
            );
            if let Some(generation) = &worker.key.execution_generation {
                identifier(generation)?;
            }
            if let Some(d) = &worker.terminal {
                validate_descriptor(source, d)?;
                ensure!(
                    d.work_item_id == row.id
                        && d.attempt_id == attempt
                        && d.task_id == task
                        && Some(&d.execution_generation)
                            == worker.key.execution_generation.as_ref(),
                    "remote terminal worker mismatch"
                );
            }
            worker.key.source_id = id.clone();
        }
        work.push(ProjectWork {
            key: RowKey::work(&id, &row.id),
            category: Category::from_published_action(
                &row.action,
                snapshot.queued_work_item_ids.contains(&row.id),
            ),
            phase: row.phase,
            // Preserve the version-1 wire contract; worker keys already carry
            // exact Attempt identity. Sources without workers copy the CLI default.
            attempt_id: row
                .workers
                .first()
                .and_then(|worker| worker.key.attempt_id.clone()),
            candidate_commit: None,
            updated_at: None,
            workers: row.workers,
            status: WorkItemStatus {
                id: row.id,
                title: row.title,
                attempt: row.attempt,
                task: row.task,
                review: row.review,
                merge_candidate: row.merge_candidate,
                merge: row.merge,
                action: row.action,
                next_action: None,
                recovery_blocker: None,
                verification: None,
                evidence_history: None,
                metrics: WorkMetrics::default(),
                compatibility_warnings: Vec::new(),
                release: None,
            },
        });
    }
    for queued in &snapshot.queued_work_item_ids {
        identifier(queued)?;
    }
    Ok(SourceSnapshot {
        schema_version: 1,
        id,
        host: source.host.clone(),
        path: source.project_root.clone(),
        project: snapshot.project,
        work,
        queued_work_item_ids: snapshot.queued_work_item_ids,
        diagnostics: snapshot.diagnostics,
        stale: None,
    })
}

fn validate_text(value: &serde_json::Value) -> Result<()> {
    match value {
        serde_json::Value::String(text) => ensure!(
            !text.chars().any(char::is_control),
            "remote metadata contains terminal control characters"
        ),
        serde_json::Value::Array(items) => {
            for item in items {
                validate_text(item)?;
            }
        }
        serde_json::Value::Object(fields) => {
            for (key, value) in fields {
                ensure!(
                    !key.chars().any(char::is_control),
                    "remote metadata field contains terminal control characters"
                );
                validate_text(value)?;
            }
        }
        _ => {}
    }
    Ok(())
}
