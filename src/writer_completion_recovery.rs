//! Audited recovery for a Writer that stopped before producing a valid result.
//!
//! A Writer can finish its model session without changing the candidate. In
//! that case it must either write the required `no-change.json` declaration or
//! pause for recovery. Older Fluent versions retried the paused Writer and
//! also appended another Writer, leaving the original task first in the list
//! and creating an endless sequence of duplicate retries. This module records
//! one explicit recovery, retries the same task, and marks only those later
//! duplicate Writer tasks as superseded history.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::ErrorKind;
use std::path::Path;

use crate::follow_up::content_digest;
use crate::git;
use crate::work_model::{
    ArtifactRef, Attempt, AttemptReviewState, AttemptStatus, PauseKind, TaskKind, TaskStatus,
    WorkModelError, WorkModelStore, now_iso8601, resolve_managed_sibling_workspace_path,
    work_artifact_path,
};

mod context_retry;
pub(crate) use crate::recovery_contracts::writer_completion::validated_writer_context;
use crate::recovery_contracts::writer_completion::{
    PRODUCER, RecoveryAuthority, SCHEMA_VERSION, corrective_writer_instructions,
    normalize_resolution, recovery_instructions,
};
pub(crate) use context_retry::{
    before_spawn as before_context_retry_spawn, record_start as record_context_retry_start,
    resume as resume_context_retry,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriterCompletionRecovery {
    pub task_id: String,
    pub candidate_commit: String,
    pub audit_artifact: String,
    pub superseded_task_ids: Vec<String>,
    pub reconciled_task_ids: Vec<String>,
    pub already_applied: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
struct RecoveryReconciliation {
    schema_version: u32,
    work_item_id: String,
    attempt_id: String,
    authority_artifact: String,
    source_writer_task_id: String,
    candidate_commit: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    successor_task_id: Option<String>,
    superseded_task_ids: Vec<String>,
    resolution: String,
    resolution_sha256: String,
    created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct EligibleRecovery {
    task_id: String,
    source_writer_task_id: String,
    candidate_commit: String,
    workspace_path: String,
    protected_base_commit: Option<String>,
    superseded_task_ids: Vec<String>,
    existing_no_change: bool,
    schedule_corrective_writer: bool,
    committed_source_matrix: bool,
    source_matrix_sha256: Option<String>,
    requirement_contract_sha256: Option<String>,
    source_assignment_sha256: Option<String>,
    source_transcript_sha256: Option<String>,
    source_output_sha256: Option<String>,
}

/// Return the command that recovers this exact Writer-completion pause, when
/// the saved Attempt has the narrow supported shape.
pub fn recovery_action(project_root: &Path, attempt: &Attempt) -> Option<String> {
    let eligible = inspect_eligible(project_root, attempt).ok()?;
    validate_prior_recoveries(project_root, attempt, &eligible, None).ok()?;
    Some(format!(
        "fluent attempt repair-writer-completion {} {} --resolution \"<describe the approved retry>\"",
        attempt.work_item_id, attempt.id
    ))
}

/// Report a repair for the latest eligible Writer-completion pause.
pub(crate) fn status_recovery_action(project_root: &Path, attempt: &Attempt) -> Option<String> {
    let eligible = inspect_eligible(project_root, attempt).ok()?;
    if eligible.existing_no_change {
        let source = attempt
            .tasks
            .iter()
            .find(|task| task.id == eligible.source_writer_task_id)?;
        if !writer_matrix_has_pending_writer_work(project_root, attempt, source).ok()? {
            return None;
        }
    }
    let latest = attempt
        .tasks
        .iter()
        .rev()
        .find(|task| task.kind == TaskKind::Write && task.status == TaskStatus::Complete)?;
    if latest.id != eligible.source_writer_task_id {
        return None;
    }
    validate_prior_recoveries(project_root, attempt, &eligible, None).ok()?;
    Some(format!(
        "fluent attempt repair-writer-completion {} {} --resolution \"<describe the approved retry>\"",
        attempt.work_item_id, attempt.id
    ))
}

/// Admit queue resume for the one planned corrective Writer created by an
/// audited completion repair. Ordinary planned Attempts still use queue add.
pub(crate) fn planned_successor_resume_ready(
    project_root: &Path,
    item: &crate::work_model::WorkItem,
    attempt: &Attempt,
) -> Result<bool> {
    if attempt.status != AttemptStatus::Planned || attempt.pause_kind.is_some() {
        return Ok(false);
    }
    let Some(task) = attempt.tasks.last() else {
        return Ok(false);
    };
    if task.kind != TaskKind::Write
        || task.status != TaskStatus::Planned
        || !task
            .input_artifacts
            .iter()
            .any(|artifact| artifact.producer_id == PRODUCER)
    {
        return Ok(false);
    }
    attempt.ensure_not_canceled()?;
    ensure_settled_ownership(project_root, attempt)?;
    Ok(validated_writer_context(project_root, item, task)?.is_some())
}

fn validate_prior_recoveries(
    project_root: &Path,
    attempt: &Attempt,
    eligible: &EligibleRecovery,
    resolution: Option<&str>,
) -> Result<()> {
    let current_index = attempt
        .tasks
        .iter()
        .position(|task| task.id == eligible.source_writer_task_id)
        .context("current recovery source Writer is missing")?;
    for reference in attempt
        .artifacts
        .iter()
        .filter(|artifact| artifact.producer_id == PRODUCER)
    {
        let path =
            crate::recovery_contracts::resolve_input_artifact_path(project_root, &reference.path)?;
        let bytes = fs::read(&path)?;
        let authority: RecoveryAuthority = serde_json::from_slice(&bytes)
            .context("parse prior Writer-completion recovery authority")?;
        anyhow::ensure!(
            bytes == serde_json::to_vec_pretty(&authority)?
                && authority.schema_version == SCHEMA_VERSION
                && authority.work_item_id == attempt.work_item_id
                && authority.attempt_id == attempt.id
                && authority.resolution == normalize_resolution(&authority.resolution)?
                && authority.resolution_sha256 == content_digest(authority.resolution.as_bytes())
                && reference.path
                    == work_artifact_path(
                        &attempt.work_item_id,
                        &attempt.id,
                        &format!(
                            "writer-completion-recoveries/{}/recovery.json",
                            authority.task_id
                        ),
                    ),
            "prior Writer-completion recovery authority changed"
        );
        if authority.task_id == eligible.task_id {
            continue;
        }
        let replacement_index = attempt
            .tasks
            .iter()
            .position(|task| task.id == authority.task_id)
            .context("prior recovery Writer is missing")?;
        let replacement = &attempt.tasks[replacement_index];
        let source_index = attempt
            .tasks
            .iter()
            .position(|task| task.id == authority.source_writer_task_id)
            .context("prior recovery source Writer is missing")?;
        let source = &attempt.tasks[source_index];
        let source_output = source
            .output
            .as_ref()
            .context("prior recovery source output is missing")?;
        // Older releases approved a completed no-change Writer in place but
        // reserved no successor. Retain that operation as history; a distinct
        // decision with current source bindings may authorize the next Task.
        if source_index == replacement_index
            && source_output.no_change.is_some()
            && authority.source_matrix_sha256.is_none()
        {
            anyhow::ensure!(
                source.kind == TaskKind::Write
                    && source.status == TaskStatus::Complete
                    && source_output.commit == authority.candidate_commit
                    && source_output.base_commit == authority.protected_base_commit
                    && source.input_artifacts.contains(reference),
                "prior no-change Writer-completion recovery source changed"
            );
            crate::recovery_contracts::writer_completion::validate_task_recovery_source(
                project_root,
                attempt,
                source,
            )?;
            if let Some(resolution) = resolution {
                anyhow::ensure!(
                    authority.resolution != resolution,
                    "later Writer-completion recovery needs a distinct authorization decision"
                );
            }
            continue;
        }
        anyhow::ensure!(
            source_index <= replacement_index
                && replacement_index < current_index
                && source.kind == TaskKind::Write
                && source.status == TaskStatus::Complete
                && source_output.commit == authority.candidate_commit
                && source_output.base_commit == authority.protected_base_commit
                && replacement.status == TaskStatus::Complete
                && replacement.input_artifacts.contains(reference),
            "prior Writer-completion recovery has not settled"
        );
        crate::recovery_contracts::writer_completion::validate_task_recovery_source(
            project_root,
            attempt,
            replacement,
        )?;
        if let Some(resolution) = resolution {
            anyhow::ensure!(
                authority.resolution != resolution,
                "later Writer-completion recovery needs a distinct authorization decision"
            );
        }
    }
    Ok(())
}

/// Retry the exact paused Writer through one audited, idempotent operation.
/// Existing Task and artifact history is retained. Planned Writer tasks that
/// were appended after the paused task by an older retry are marked
/// `superseded`; they are not silently deleted or counted as valid next work.
pub fn repair(
    project_root: &Path,
    store: &WorkModelStore,
    work_item_id: &str,
    attempt_id: &str,
    resolution: &str,
) -> Result<WriterCompletionRecovery> {
    let _land_lock = crate::land_lock::acquire(&crate::land_lock::lock_path(project_root))
        .context("acquire Writer-completion recovery lock")?;
    let resolution = normalize_resolution(resolution)?;
    if let Some(retry) =
        context_retry::repair(project_root, store, work_item_id, attempt_id, &resolution)?
    {
        return Ok(retry);
    }
    let item = store.read_work_item(work_item_id)?;
    item.ensure_mutation_compatible()?;
    let attempt = item
        .attempts
        .iter()
        .find(|attempt| attempt.id == attempt_id)
        .ok_or_else(|| anyhow::anyhow!("Attempt {attempt_id:?} not found"))?;

    if let Some(existing) = existing_recovery(project_root, &item, attempt, &resolution)? {
        if let Some(reconciled) = reconcile_existing_recovery(
            project_root,
            store,
            work_item_id,
            attempt_id,
            &existing,
            &resolution,
        )? {
            return Ok(reconciled);
        }
        return Ok(existing);
    }
    if item
        .merge_candidates
        .iter()
        .any(|candidate| candidate.attempt_id == attempt_id)
    {
        bail!("Writer-completion recovery is not eligible after a Merge Candidate exists");
    }

    let eligible = inspect_eligible(project_root, attempt)?;
    let created_at = now_iso8601();
    let audit_artifact = work_artifact_path(
        work_item_id,
        attempt_id,
        &format!(
            "writer-completion-recoveries/{}/recovery.json",
            eligible.task_id
        ),
    );
    let authority = RecoveryAuthority {
        schema_version: SCHEMA_VERSION,
        work_item_id: work_item_id.to_string(),
        attempt_id: attempt_id.to_string(),
        task_id: eligible.task_id.clone(),
        source_writer_task_id: eligible.source_writer_task_id.clone(),
        candidate_commit: eligible.candidate_commit.clone(),
        protected_base_commit: eligible.protected_base_commit.clone(),
        superseded_task_ids: eligible.superseded_task_ids.clone(),
        resolution: resolution.clone(),
        resolution_sha256: content_digest(resolution.as_bytes()),
        created_at,
        source_matrix_sha256: eligible.source_matrix_sha256.clone(),
        requirement_contract_sha256: eligible.requirement_contract_sha256.clone(),
        source_assignment_sha256: eligible.source_assignment_sha256.clone(),
        source_transcript_sha256: eligible.source_transcript_sha256.clone(),
        source_output_sha256: eligible.source_output_sha256.clone(),
    };
    let authority_bytes =
        serde_json::to_vec(&authority).context("serialize Writer-completion recovery identity")?;
    let operation = crate::work_model::RecoveryOperationRequest::new(
        "writer-completion-recovery",
        work_item_id,
        attempt_id,
        Some(eligible.candidate_commit.clone()),
        Some(if eligible.source_matrix_sha256.is_some() {
            eligible.source_writer_task_id.clone()
        } else {
            eligible.task_id.clone()
        }),
        None,
        Some(eligible.workspace_path.clone()),
        Some(resolution.clone()),
        Some(format!("sha256:{:x}", Sha256::digest(&authority_bytes))),
    );
    let _ = item.validate_recovery_operation(project_root, &operation)?;
    let audit_path = project_root.join(&audit_artifact);
    write_authority_once(&audit_path, &authority)?;
    let audit_ref = ArtifactRef {
        producer_id: PRODUCER.to_string(),
        path: audit_artifact.clone(),
    };

    let expected = item.clone();
    let expected_eligible = eligible.clone();
    let task_id = eligible.task_id.clone();
    let stored_superseded = eligible.superseded_task_ids.clone();
    let instruction = if eligible.schedule_corrective_writer {
        corrective_writer_instructions(&resolution, &audit_artifact)
    } else {
        recovery_instructions(&resolution, &audit_artifact)
    };
    let source_input_artifacts = if eligible.schedule_corrective_writer {
        item.attempts
            .iter()
            .find(|attempt| attempt.id == attempt_id)
            .and_then(|attempt| {
                attempt
                    .tasks
                    .iter()
                    .find(|task| task.id == expected_eligible.source_writer_task_id)
            })
            .map(|task| {
                task.input_artifacts
                    .iter()
                    .filter(|artifact| artifact.producer_id != PRODUCER)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    let mut successor_task_id = None;
    store.mutate_work_item_from_external(
        work_item_id,
        format!("authorize Writer-completion recovery for {attempt_id}"),
        Some(audit_artifact.clone()),
        |fresh| {
            if fresh != &expected {
                return Err(WorkModelError::AttemptResumeIneligible {
                    attempt_id: attempt_id.to_string(),
                    reason:
                        "the Writer-completion state changed while recovery was being authorized"
                            .to_string(),
                });
            }
            let current_attempt = fresh
                .attempts
                .iter()
                .find(|attempt| attempt.id == attempt_id)
                .ok_or_else(|| WorkModelError::AttemptNotFound {
                    id: attempt_id.to_string(),
                })?;
            let current = inspect_eligible(project_root, current_attempt).map_err(|error| {
                WorkModelError::AttemptResumeIneligible {
                    attempt_id: attempt_id.to_string(),
                    reason: format!("Writer-completion state changed: {error:#}"),
                }
            })?;
            if current != expected_eligible {
                return Err(WorkModelError::AttemptResumeIneligible {
                    attempt_id: attempt_id.to_string(),
                    reason: "the Writer-completion recovery target changed before authorization"
                        .to_string(),
                });
            }
            if expected_eligible.schedule_corrective_writer {
                if !expected_eligible.committed_source_matrix && !expected_eligible.existing_no_change {
                    let attempt = fresh
                        .attempts
                        .iter_mut()
                        .find(|attempt| attempt.id == attempt_id)
                        .ok_or_else(|| WorkModelError::AttemptNotFound {
                            id: attempt_id.to_string(),
                        })?;
                    let task = attempt
                        .tasks
                        .iter_mut()
                        .find(|task| task.id == task_id)
                        .ok_or_else(|| WorkModelError::TaskNotFound {
                            id: task_id.clone(),
                        })?;
                    if !task.input_artifacts.contains(&audit_ref) {
                        task.input_artifacts.push(audit_ref.clone());
                    }
                }
                let mut inputs = source_input_artifacts.clone();
                if !inputs.contains(&audit_ref) {
                    inputs.push(audit_ref.clone());
                }
                if expected_eligible.source_matrix_sha256.is_some() {
                    let source = &expected_eligible.source_writer_task_id;
                    let matrix = ArtifactRef {
                        producer_id: source.clone(),
                        path: work_artifact_path(
                            work_item_id,
                            attempt_id,
                            &format!("{source}/{}", crate::recovery_contracts::WRITER_COMPLETION_MATRIX_FILE),
                        ),
                    };
                    if !inputs.contains(&matrix) {
                        inputs.push(matrix);
                    }
                }
                let next_task_id = fresh
                    .add_next_write_round(attempt_id, inputs)
                    .map_err(|error| WorkModelError::AttemptResumeIneligible {
                        attempt_id: attempt_id.to_string(),
                        reason: format!(
                            "could not schedule the corrective Writer for unfinished Writer work: {error}"
                        ),
                    })?;
                if next_task_id != task_id {
                    return Err(WorkModelError::AttemptResumeIneligible {
                        attempt_id: attempt_id.to_string(),
                        reason: "corrective Writer identity changed before authorization".to_string(),
                    });
                }
                let successor = fresh
                    .attempts
                    .iter_mut()
                    .find(|attempt| attempt.id == attempt_id)
                    .and_then(|attempt| {
                        attempt.tasks.iter_mut().find(|task| task.id == next_task_id)
                    })
                    .ok_or_else(|| WorkModelError::TaskNotFound {
                        id: next_task_id.clone(),
                    })?;
                successor.instructions = Some(instruction.clone());
                successor_task_id = Some(next_task_id);
            } else {
                let attempt = fresh
                    .attempts
                    .iter_mut()
                    .find(|attempt| attempt.id == attempt_id)
                    .ok_or_else(|| WorkModelError::AttemptNotFound {
                        id: attempt_id.to_string(),
                    })?;
                let task = attempt
                    .tasks
                    .iter_mut()
                    .find(|task| task.id == task_id)
                    .ok_or_else(|| WorkModelError::TaskNotFound {
                        id: task_id.clone(),
                    })?;
                if !task.input_artifacts.contains(&audit_ref) {
                    task.input_artifacts.push(audit_ref.clone());
                }
                if !expected_eligible.existing_no_change {
                    task.status = TaskStatus::Planned;
                    task.started_at = None;
                    task.completed_at = None;
                    task.output = None;
                    task.instructions = Some(instruction.clone());
                }
            }
            let attempt = fresh
                .attempts
                .iter_mut()
                .find(|attempt| attempt.id == attempt_id)
                .ok_or_else(|| WorkModelError::AttemptNotFound {
                    id: attempt_id.to_string(),
                })?;
            for superseded_id in &stored_superseded {
                let successor = attempt
                    .tasks
                    .iter_mut()
                    .find(|task| task.id == *superseded_id)
                    .ok_or_else(|| WorkModelError::TaskNotFound {
                        id: superseded_id.clone(),
                    })?;
                successor.status = TaskStatus::Superseded;
                successor.started_at = None;
                successor.completed_at.get_or_insert_with(now_iso8601);
                successor.output = None;
            }
            if !attempt.artifacts.contains(&audit_ref) {
                attempt.artifacts.push(audit_ref.clone());
            }
            if expected_eligible.schedule_corrective_writer {
                attempt.status = AttemptStatus::Planned;
                attempt.pause_kind = None;
                attempt.pause_recovery = None;
                attempt.review_state = Some(AttemptReviewState::NotReviewed);
            } else {
                attempt.status = AttemptStatus::NeedsUser;
                attempt.pause_kind = Some(PauseKind::WriterCompletion);
                attempt.pause_recovery = None;
                attempt.review_state = Some(AttemptReviewState::NotReviewed);
            }
            // A stale terminal timestamp made queue and direct status disagree
            // in older records. A resumable recovery has no completion time.
            attempt.completed_at = None;
            let next_action = crate::work_status::derive_attempt_recovery_next_action(
                Some(attempt),
                Some(project_root),
            );
            attempt.record_recovery_operation(&operation, next_action)?;
            Ok(())
        },
    )?;

    Ok(WriterCompletionRecovery {
        task_id: successor_task_id.unwrap_or(task_id),
        candidate_commit: eligible.candidate_commit,
        audit_artifact,
        superseded_task_ids: stored_superseded,
        reconciled_task_ids: Vec::new(),
        already_applied: false,
    })
}

/// Reconcile duplicate Writer Tasks left behind by an older retry after the
/// audited recovery Writer had already settled. These Tasks have no candidate
/// output, no review or Tester Task after the source Writer, and therefore
/// cannot represent a legitimate correction. Marking them superseded keeps
/// their failure history while releasing the exact candidate to the ordinary
/// candidate-gate path. The reconciliation is itself audited and idempotent.
fn reconcile_existing_recovery(
    project_root: &Path,
    store: &WorkModelStore,
    work_item_id: &str,
    attempt_id: &str,
    existing: &WriterCompletionRecovery,
    resolution: &str,
) -> Result<Option<WriterCompletionRecovery>> {
    let item = store.read_work_item(work_item_id)?;
    let attempt = item
        .attempts
        .iter()
        .find(|attempt| attempt.id == attempt_id)
        .ok_or_else(|| anyhow::anyhow!("Attempt {attempt_id:?} not found"))?;
    let authority_ref = attempt
        .artifacts
        .iter()
        .rev()
        .find(|artifact| {
            artifact.producer_id == PRODUCER && artifact.path == existing.audit_artifact
        })
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("Writer-completion recovery authority is not attached"))?;
    let reconciliation_path = work_artifact_path(
        work_item_id,
        attempt_id,
        &format!(
            "writer-completion-recoveries/{}/reconciliation.json",
            existing.task_id
        ),
    );
    let reconciliation_ref = ArtifactRef {
        producer_id: "writer-completion-recovery-reconciliation".to_string(),
        path: reconciliation_path.clone(),
    };
    let reconciliation_prefix = work_artifact_path(
        work_item_id,
        attempt_id,
        &format!(
            "writer-completion-recoveries/{}/reconciliation",
            existing.task_id
        ),
    );
    let existing_reconciliation = if let Some(existing_ref) =
        attempt.artifacts.iter().find(|artifact| {
            artifact.producer_id == reconciliation_ref.producer_id
                && artifact.path.starts_with(&reconciliation_prefix)
        }) {
        let bytes = fs::read(project_root.join(&existing_ref.path)).with_context(|| {
            format!(
                "read Writer-completion recovery reconciliation {}",
                existing_ref.path
            )
        })?;
        let record: RecoveryReconciliation = serde_json::from_slice(&bytes)
            .context("parse Writer-completion recovery reconciliation")?;
        if record.schema_version != SCHEMA_VERSION
            || record.work_item_id != work_item_id
            || record.attempt_id != attempt_id
            || record.authority_artifact != authority_ref.path
            || record.source_writer_task_id != existing.task_id
            || record.resolution != resolution
            || record.resolution_sha256 != content_digest(record.resolution.as_bytes())
        {
            bail!(
                "existing Writer-completion recovery reconciliation does not match this authorization"
            );
        }
        Some((existing_ref.clone(), record))
    } else {
        None
    };

    // A recovery for a completed no-change Writer may schedule a legitimate
    // corrective Writer rather than retrying the completed Task in place. Keep
    // that successor resumable and make a repeated command a no-op instead of
    // treating it as a stale duplicate.
    if let Some(successor) = attempt.tasks.iter().find(|task| {
        task.id != existing.task_id
            && task.kind == TaskKind::Write
            && !matches!(task.status, TaskStatus::Superseded | TaskStatus::Failed)
            && task.input_artifacts.contains(&authority_ref)
    }) {
        return Ok(Some(WriterCompletionRecovery {
            task_id: successor.id.clone(),
            candidate_commit: successor
                .output
                .as_ref()
                .map(|output| output.commit.clone())
                .unwrap_or_else(|| existing.candidate_commit.clone()),
            audit_artifact: existing.audit_artifact.clone(),
            superseded_task_ids: existing.superseded_task_ids.clone(),
            reconciled_task_ids: existing_reconciliation
                .as_ref()
                .map(|(_, record)| record.superseded_task_ids.clone())
                .unwrap_or_default(),
            already_applied: true,
        }));
    }

    if item
        .merge_candidates
        .iter()
        .any(|candidate| candidate.attempt_id == attempt_id)
    {
        bail!("Writer-completion reconciliation is not eligible after a Merge Candidate exists");
    }
    if attempt.status != AttemptStatus::NeedsUser
        || !matches!(
            attempt.pause_kind,
            Some(PauseKind::RoundCap) | Some(PauseKind::WriterCompletion)
        )
    {
        if let Some((_, record)) = existing_reconciliation.as_ref() {
            return Ok(Some(WriterCompletionRecovery {
                task_id: existing.task_id.clone(),
                candidate_commit: record.candidate_commit.clone(),
                audit_artifact: existing.audit_artifact.clone(),
                superseded_task_ids: existing.superseded_task_ids.clone(),
                reconciled_task_ids: record.superseded_task_ids.clone(),
                already_applied: true,
            }));
        }
        return Ok(None);
    }
    if attempt
        .tasks
        .iter()
        .any(|task| task.status == TaskStatus::Executing)
    {
        bail!("Writer-completion reconciliation cannot run while a Task is executing");
    }
    let source = attempt
        .tasks
        .iter()
        .find(|task| task.id == existing.task_id)
        .ok_or_else(|| {
            anyhow::anyhow!("recovery Writer {:?} no longer exists", existing.task_id)
        })?;
    if source.kind != TaskKind::Write {
        bail!("Writer-completion recovery source no longer matches its authority");
    }
    // The first invocation intentionally replans the paused source Writer.
    // Until that exact Task settles, there is no later duplicate to reconcile.
    if source.status != TaskStatus::Complete {
        return Ok(None);
    }
    if source
        .output
        .as_ref()
        .is_some_and(|output| output.no_change.is_some())
        && writer_matrix_has_pending_writer_work(project_root, attempt, source)?
    {
        return schedule_pending_finding_writer(
            project_root,
            store,
            work_item_id,
            attempt_id,
            existing,
            resolution,
            &authority_ref,
            &existing_reconciliation,
            attempt,
        );
    }
    // The original recovery source may be followed by valid corrective
    // Writers and reviews. Reconcile only output-less Writer retries that sit
    // after the latest completed candidate; a review or Tester before that
    // latest Writer is historical work, not a reason to reject this cleanup.
    let (candidate_index, candidate_writer) = attempt
        .tasks
        .iter()
        .enumerate()
        .rev()
        .find(|(_, task)| {
            task.kind == TaskKind::Write
                && task.status == TaskStatus::Complete
                && task.output.is_some()
                && task.status != TaskStatus::Superseded
        })
        .ok_or_else(|| anyhow::anyhow!("Attempt has no completed Writer candidate"))?;
    let candidate_output = candidate_writer
        .output
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("latest completed Writer has no candidate output"))?;
    let current_candidate_commit = candidate_output.commit.clone();
    let workspace = resolve_managed_sibling_workspace_path(
        project_root,
        &candidate_output.workspace_path,
        "Writer-completion reconciliation candidate workspace",
    )?;
    let head = git::run_stdout(
        &workspace,
        &["rev-parse", "HEAD"],
        "resolve Writer-completion reconciliation candidate HEAD",
    )?;
    if head != current_candidate_commit {
        bail!(
            "candidate HEAD changed from {} to {head}; ordinary candidate drift remains rejected",
            current_candidate_commit
        );
    }
    let status = git::run_stdout(
        &workspace,
        &["status", "--porcelain", "--untracked-files=all", "--", "."],
        "check Writer-completion reconciliation candidate cleanliness",
    )?;
    if !status.is_empty() {
        bail!("Writer-completion reconciliation requires a clean candidate worktree:\n{status}");
    }

    // A valid next correction would have a review or Tester Task after the
    // source Writer. Without one, later output-less Writer Tasks are only the
    // duplicate retries this operation is authorized to supersede.
    let later = &attempt.tasks[candidate_index + 1..];
    if later.iter().any(|task| {
        matches!(task.kind, TaskKind::Review | TaskKind::Tester)
            && task.status != TaskStatus::Superseded
    }) {
        return Ok(None);
    }
    let stale_ids = later
        .iter()
        .filter(|task| {
            task.kind == TaskKind::Write
                && matches!(
                    task.status,
                    TaskStatus::Failed | TaskStatus::Planned | TaskStatus::NeedsUser
                )
                && task.output.is_none()
                && existing_reconciliation.as_ref().is_none_or(|(_, record)| {
                    !record.superseded_task_ids.iter().any(|id| id == &task.id)
                })
        })
        .map(|task| task.id.clone())
        .collect::<Vec<_>>();
    if stale_ids.is_empty() {
        if let Some((_, record)) = existing_reconciliation.as_ref() {
            if record.candidate_commit != current_candidate_commit {
                bail!("Writer-completion reconciliation candidate no longer matches its audit");
            }
            return Ok(Some(WriterCompletionRecovery {
                task_id: existing.task_id.clone(),
                candidate_commit: current_candidate_commit,
                audit_artifact: existing.audit_artifact.clone(),
                superseded_task_ids: existing.superseded_task_ids.clone(),
                reconciled_task_ids: record.superseded_task_ids.clone(),
                already_applied: true,
            }));
        }
        return Ok(None);
    }

    let (reconciliation_path, reconciliation_ref) =
        if let Some((_, _record)) = existing_reconciliation.as_ref() {
            let identity = format!(
                "{}\n{}\n{}",
                current_candidate_commit,
                stale_ids.join(","),
                resolution
            );
            let suffix = content_digest(identity.as_bytes());
            let path = work_artifact_path(
                work_item_id,
                attempt_id,
                &format!(
                    "writer-completion-recoveries/{}/reconciliation-{suffix}.json",
                    existing.task_id
                ),
            );
            let reference = ArtifactRef {
                producer_id: "writer-completion-recovery-reconciliation".to_string(),
                path: path.clone(),
            };
            // The earlier record remains immutable; its IDs are carried into the
            // result while this record audits only the newly discovered stale Tasks.
            (path, reference)
        } else {
            (reconciliation_path, reconciliation_ref)
        };

    let authority = RecoveryReconciliation {
        schema_version: SCHEMA_VERSION,
        work_item_id: work_item_id.to_string(),
        attempt_id: attempt_id.to_string(),
        authority_artifact: authority_ref.path.clone(),
        source_writer_task_id: existing.task_id.clone(),
        candidate_commit: current_candidate_commit.clone(),
        successor_task_id: None,
        superseded_task_ids: stale_ids.clone(),
        resolution: resolution.to_string(),
        resolution_sha256: content_digest(resolution.as_bytes()),
        created_at: now_iso8601(),
    };
    write_reconciliation_once(&project_root.join(&reconciliation_path), &authority)?;
    let expected = item.clone();
    let authority_ref_for_mutation = authority_ref.clone();
    let reconciliation_ref_for_mutation = reconciliation_ref.clone();
    store.mutate_work_item_from_external(
        work_item_id,
        format!("reconcile duplicate Writer Tasks for {attempt_id}"),
        Some(reconciliation_path.clone()),
        |fresh| {
            if fresh != &expected {
                return Err(WorkModelError::AttemptResumeIneligible {
                    attempt_id: attempt_id.to_string(),
                    reason: "the Writer-completion state changed while reconciliation was being authorized".to_string(),
                });
            }
            let attempt = fresh
                .attempts
                .iter_mut()
                .find(|attempt| attempt.id == attempt_id)
                .ok_or_else(|| WorkModelError::AttemptNotFound {
                    id: attempt_id.to_string(),
                })?;
            if !attempt.artifacts.contains(&authority_ref_for_mutation) {
                return Err(WorkModelError::AttemptResumeIneligible {
                    attempt_id: attempt_id.to_string(),
                    reason: "the Writer-completion authority is no longer attached".to_string(),
                });
            }
            for task_id in &stale_ids {
                let task = attempt
                    .tasks
                    .iter_mut()
                    .find(|task| task.id == *task_id)
                    .ok_or_else(|| WorkModelError::TaskNotFound { id: task_id.clone() })?;
                if !matches!(task.status, TaskStatus::Failed | TaskStatus::Planned | TaskStatus::NeedsUser)
                    || task.output.is_some()
                {
                    return Err(WorkModelError::AttemptResumeIneligible {
                        attempt_id: attempt_id.to_string(),
                        reason: "a duplicate Writer changed before reconciliation".to_string(),
                    });
                }
                task.status = TaskStatus::Superseded;
                task.completed_at.get_or_insert_with(now_iso8601);
                task.started_at = None;
                task.output = None;
            }
            if !attempt.artifacts.contains(&reconciliation_ref_for_mutation) {
                attempt.artifacts.push(reconciliation_ref_for_mutation.clone());
            }
            attempt.status = AttemptStatus::Planned;
            attempt.pause_kind = None;
            attempt.pause_recovery = None;
            attempt.review_state = Some(AttemptReviewState::NotReviewed);
            attempt.completed_at = None;
            Ok(())
        },
    )?;
    Ok(Some(WriterCompletionRecovery {
        task_id: existing.task_id.clone(),
        candidate_commit: current_candidate_commit,
        audit_artifact: existing.audit_artifact.clone(),
        superseded_task_ids: existing.superseded_task_ids.clone(),
        reconciled_task_ids: existing_reconciliation
            .as_ref()
            .map(|(_, record)| {
                record
                    .superseded_task_ids
                    .clone()
                    .into_iter()
                    .chain(stale_ids.clone())
                    .collect()
            })
            .unwrap_or(stale_ids),
        already_applied: false,
    }))
}

/// Turn an already-authorized completed no-change Writer into one normal
/// corrective Writer when its matrix still contains Writer-owned work. This
/// path is needed for authorities created by an older Fluent binary before it
/// learned that shape; the authority remains immutable and the new
/// reconciliation record makes the follow-up idempotent.
fn schedule_pending_finding_writer(
    project_root: &Path,
    store: &WorkModelStore,
    work_item_id: &str,
    attempt_id: &str,
    existing: &WriterCompletionRecovery,
    resolution: &str,
    authority_ref: &ArtifactRef,
    existing_reconciliation: &Option<(ArtifactRef, RecoveryReconciliation)>,
    attempt: &Attempt,
) -> Result<Option<WriterCompletionRecovery>> {
    let source_index = attempt
        .tasks
        .iter()
        .position(|task| task.id == existing.task_id)
        .ok_or_else(|| {
            anyhow::anyhow!("recovery Writer {:?} no longer exists", existing.task_id)
        })?;
    let source = &attempt.tasks[source_index];
    let Some(source_output) = source.output.as_ref() else {
        return Ok(None);
    };
    if source.status != TaskStatus::Complete || source_output.no_change.is_none() {
        return Ok(None);
    }
    if attempt.tasks[source_index + 1..].iter().any(|task| {
        task.kind == TaskKind::Write
            && task.status == TaskStatus::Complete
            && task.status != TaskStatus::Superseded
    }) {
        return Ok(None);
    }
    let later = &attempt.tasks[source_index + 1..];
    if later.iter().any(|task| {
        matches!(task.kind, TaskKind::Review | TaskKind::Tester)
            && task.status != TaskStatus::Superseded
    }) {
        return Ok(None);
    }

    let workspace = resolve_managed_sibling_workspace_path(
        project_root,
        &source_output.workspace_path,
        "Writer-completion corrective candidate workspace",
    )?;
    let head = git::run_stdout(
        &workspace,
        &["rev-parse", "HEAD"],
        "resolve Writer-completion corrective candidate HEAD",
    )?;
    if head != source_output.commit {
        bail!(
            "candidate HEAD changed from {} to {head}; ordinary candidate drift remains rejected",
            source_output.commit
        );
    }
    let status = git::run_stdout(
        &workspace,
        &["status", "--porcelain", "--untracked-files=all", "--", "."],
        "check Writer-completion corrective candidate cleanliness",
    )?;
    if !status.is_empty() {
        bail!("Writer-completion recovery requires a clean candidate worktree:\n{status}");
    }

    let stale_ids = later
        .iter()
        .filter(|task| {
            task.kind == TaskKind::Write
                && matches!(
                    task.status,
                    TaskStatus::Failed | TaskStatus::Planned | TaskStatus::NeedsUser
                )
                && task.output.is_none()
                && existing_reconciliation.as_ref().is_none_or(|(_, record)| {
                    !record.superseded_task_ids.iter().any(|id| id == &task.id)
                })
        })
        .map(|task| task.id.clone())
        .collect::<Vec<_>>();
    let next_task_id = format!(
        "{attempt_id}-write-{}",
        attempt
            .tasks
            .iter()
            .filter(|task| task.kind == TaskKind::Write)
            .count()
            + 1
    );
    if attempt.tasks.iter().any(|task| task.id == next_task_id) {
        bail!("corrective Writer task {next_task_id:?} already exists without recovery binding");
    }
    let (reconciliation_path, reconciliation_ref) = if existing_reconciliation.is_some() {
        let identity = format!(
            "successor\n{}\n{}\n{}",
            source_output.commit, next_task_id, resolution
        );
        let suffix = content_digest(identity.as_bytes());
        let path = work_artifact_path(
            work_item_id,
            attempt_id,
            &format!(
                "writer-completion-recoveries/{}/reconciliation-{suffix}.json",
                existing.task_id
            ),
        );
        let reference = ArtifactRef {
            producer_id: "writer-completion-recovery-reconciliation".to_string(),
            path: path.clone(),
        };
        (path, reference)
    } else {
        let path = work_artifact_path(
            work_item_id,
            attempt_id,
            &format!(
                "writer-completion-recoveries/{}/reconciliation.json",
                existing.task_id
            ),
        );
        let reference = ArtifactRef {
            producer_id: "writer-completion-recovery-reconciliation".to_string(),
            path: path.clone(),
        };
        (path, reference)
    };
    let reconciliation = RecoveryReconciliation {
        schema_version: SCHEMA_VERSION,
        work_item_id: work_item_id.to_string(),
        attempt_id: attempt_id.to_string(),
        authority_artifact: authority_ref.path.clone(),
        source_writer_task_id: existing.task_id.clone(),
        candidate_commit: source_output.commit.clone(),
        successor_task_id: Some(next_task_id.clone()),
        superseded_task_ids: stale_ids.clone(),
        resolution: resolution.to_string(),
        resolution_sha256: content_digest(resolution.as_bytes()),
        created_at: now_iso8601(),
    };
    write_reconciliation_once(&project_root.join(&reconciliation_path), &reconciliation)?;

    let expected = store.read_work_item(work_item_id)?;
    let source_inputs = source.input_artifacts.clone();
    let instruction = corrective_writer_instructions(resolution, &existing.audit_artifact);
    let mut scheduled_task_id = None;
    let authority_ref_for_mutation = authority_ref.clone();
    let reconciliation_ref_for_mutation = reconciliation_ref.clone();
    store.mutate_work_item_from_external(
        work_item_id,
        format!("schedule corrective Writer for {attempt_id} completion finding"),
        Some(reconciliation_path.clone()),
        |fresh| {
            if fresh != &expected {
                return Err(WorkModelError::AttemptResumeIneligible {
                    attempt_id: attempt_id.to_string(),
                    reason: "the Writer-completion state changed while the corrective Writer was being authorized".to_string(),
                });
            }
            let current_attempt = fresh
                .attempts
                .iter()
                .find(|candidate| candidate.id == attempt_id)
                .ok_or_else(|| WorkModelError::AttemptNotFound {
                    id: attempt_id.to_string(),
                })?;
            if current_attempt.status != AttemptStatus::NeedsUser
                || current_attempt.pause_kind != Some(PauseKind::WriterCompletion)
            {
                return Err(WorkModelError::AttemptResumeIneligible {
                    attempt_id: attempt_id.to_string(),
                    reason: "the Attempt is no longer paused for Writer-completion recovery".to_string(),
                });
            }
            if current_attempt.tasks.iter().any(|task| {
                task.id != existing.task_id
                    && task.kind == TaskKind::Write
                    && task.status != TaskStatus::Superseded
                    && task.input_artifacts.contains(&authority_ref_for_mutation)
            }) {
                return Err(WorkModelError::AttemptResumeIneligible {
                    attempt_id: attempt_id.to_string(),
                    reason: "a corrective Writer for this recovery already exists".to_string(),
                });
            }
            let mut inputs = source_inputs.clone();
            if !inputs.contains(&authority_ref_for_mutation) {
                inputs.push(authority_ref_for_mutation.clone());
            }
            let actual_task_id = fresh
                .add_next_write_round(attempt_id, inputs)
                .map_err(|error| WorkModelError::AttemptResumeIneligible {
                    attempt_id: attempt_id.to_string(),
                    reason: format!("could not schedule corrective Writer: {error}"),
                })?;
            if actual_task_id != next_task_id {
                return Err(WorkModelError::AttemptResumeIneligible {
                    attempt_id: attempt_id.to_string(),
                    reason: format!(
                        "corrective Writer id changed from {next_task_id} to {actual_task_id}"
                    ),
                });
            }
            let attempt = fresh
                .attempts
                .iter_mut()
                .find(|candidate| candidate.id == attempt_id)
                .ok_or_else(|| WorkModelError::AttemptNotFound {
                    id: attempt_id.to_string(),
                })?;
            let successor = attempt
                .tasks
                .iter_mut()
                .find(|task| task.id == actual_task_id)
                .ok_or_else(|| WorkModelError::TaskNotFound {
                    id: actual_task_id.clone(),
                })?;
            successor.instructions = Some(instruction.clone());
            for stale_id in &stale_ids {
                let stale = attempt
                    .tasks
                    .iter_mut()
                    .find(|task| task.id == *stale_id)
                    .ok_or_else(|| WorkModelError::TaskNotFound {
                        id: stale_id.clone(),
                    })?;
                stale.status = TaskStatus::Superseded;
                stale.started_at = None;
                stale.completed_at.get_or_insert_with(now_iso8601);
                stale.output = None;
            }
            if !attempt.artifacts.contains(&reconciliation_ref_for_mutation) {
                attempt.artifacts.push(reconciliation_ref_for_mutation.clone());
            }
            attempt.status = AttemptStatus::Planned;
            attempt.pause_kind = None;
            attempt.pause_recovery = None;
            attempt.review_state = Some(AttemptReviewState::NotReviewed);
            attempt.completed_at = None;
            scheduled_task_id = Some(actual_task_id);
            Ok(())
        },
    )?;

    Ok(Some(WriterCompletionRecovery {
        task_id: scheduled_task_id.unwrap_or(next_task_id),
        candidate_commit: source_output.commit.clone(),
        audit_artifact: existing.audit_artifact.clone(),
        superseded_task_ids: existing.superseded_task_ids.clone(),
        reconciled_task_ids: stale_ids,
        already_applied: false,
    }))
}

fn existing_recovery(
    project_root: &Path,
    item: &crate::work_model::WorkItem,
    attempt: &Attempt,
    resolution: &str,
) -> Result<Option<WriterCompletionRecovery>> {
    attempt.ensure_not_canceled()?;
    let Some(reference) = attempt
        .artifacts
        .iter()
        .rev()
        .find(|artifact| {
            artifact.producer_id == PRODUCER && artifact.path.ends_with("/recovery.json")
        })
        .cloned()
    else {
        return Ok(None);
    };
    ensure_settled_ownership(project_root, attempt)?;
    let authority_bytes = fs::read(project_root.join(&reference.path)).with_context(|| {
        format!(
            "read Writer-completion recovery authority {}",
            reference.path
        )
    })?;
    let authority: RecoveryAuthority = serde_json::from_slice(&authority_bytes)
        .context("parse Writer-completion recovery authority")?;
    if authority.schema_version != SCHEMA_VERSION
        || authority.work_item_id != attempt.work_item_id
        || authority.attempt_id != attempt.id
        || authority_bytes != serde_json::to_vec_pretty(&authority)?
        || authority.resolution != normalize_resolution(&authority.resolution)?
        || authority.resolution_sha256 != content_digest(authority.resolution.as_bytes())
        || reference.path
            != work_artifact_path(
                &attempt.work_item_id,
                &attempt.id,
                &format!(
                    "writer-completion-recoveries/{}/recovery.json",
                    authority.task_id
                ),
            )
    {
        bail!("existing Writer-completion recovery does not match this authorization or Attempt");
    }
    // A settled recovery is history once a later completed Writer has its own
    // malformed completion matrix. Validate the old authority before selecting
    // the current incident; its resolution must not authorize the new Writer.
    if let Ok(eligible) = inspect_eligible(project_root, attempt)
        && eligible.task_id != authority.task_id
    {
        validate_prior_recoveries(project_root, attempt, &eligible, Some(resolution))?;
        return Ok(None);
    }
    if authority.resolution != resolution {
        bail!("existing Writer-completion recovery does not match this authorization or Attempt");
    }
    let task = attempt
        .tasks
        .iter()
        .find(|task| task.id == authority.task_id)
        .ok_or_else(|| {
            anyhow::anyhow!("recovery Writer {:?} no longer exists", authority.task_id)
        })?;
    let source = attempt
        .tasks
        .iter()
        .find(|task| task.id == authority.source_writer_task_id)
        .ok_or_else(|| anyhow::anyhow!("recovery source Writer is missing"))?;
    if source.id != task.id
        && source
            .input_artifacts
            .iter()
            .any(|input| input.producer_id == PRODUCER)
    {
        crate::recovery_contracts::writer_completion::validate_task_recovery_source(
            project_root,
            attempt,
            source,
        )?;
    }
    crate::recovery_contracts::writer_completion::validate_committed_source_evidence(
        project_root,
        attempt,
        source,
        &authority,
    )?;
    let source_output = source
        .output
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("recovery source output is missing"))?;
    let workspace = resolve_managed_sibling_workspace_path(
        project_root,
        &source_output.workspace_path,
        "Writer-completion recovery candidate workspace",
    )?;
    let head = git::run_stdout(
        &workspace,
        &["rev-parse", "HEAD"],
        "check recovered candidate HEAD",
    )?;
    let status = git::run_stdout(
        &workspace,
        &["status", "--porcelain", "--untracked-files=all", "--", "."],
        "check recovered candidate cleanliness",
    )?;
    let settled_successor = task.id != source.id
        && task.status == TaskStatus::Complete
        && task.output.as_ref().is_some_and(|output| {
            output.base_commit.as_deref() == Some(authority.candidate_commit.as_str())
                && output.commit == head
                && output.workspace_path == source_output.workspace_path
        })
        && git::run(
            &workspace,
            &[
                "merge-base",
                "--is-ancestor",
                &authority.candidate_commit,
                &head,
            ],
            "check settled Writer-completion successor ancestry",
        )
        .is_ok();
    if (head != authority.candidate_commit && !settled_successor) || !status.is_empty() {
        bail!("Writer-completion recovery candidate changed or is dirty");
    }
    let operation = crate::work_model::RecoveryOperationRequest::new(
        PRODUCER,
        &item.id,
        &attempt.id,
        Some(authority.candidate_commit.clone()),
        Some(if authority.source_matrix_sha256.is_some() {
            authority.source_writer_task_id.clone()
        } else {
            authority.task_id.clone()
        }),
        None,
        Some(source_output.workspace_path.clone()),
        Some(authority.resolution.clone()),
        Some(format!(
            "sha256:{:x}",
            Sha256::digest(serde_json::to_vec(&authority)?)
        )),
    );
    anyhow::ensure!(
        item.validate_recovery_operation(project_root, &operation)?
            .is_some(),
        "Writer-completion recovery operation is no longer current"
    );
    if !task.input_artifacts.contains(&reference) {
        bail!(
            "recovery Writer {:?} no longer carries its authority",
            task.id
        );
    }
    Ok(Some(WriterCompletionRecovery {
        task_id: authority.task_id,
        candidate_commit: authority.candidate_commit,
        audit_artifact: reference.path,
        superseded_task_ids: authority.superseded_task_ids,
        reconciled_task_ids: Vec::new(),
        already_applied: true,
    }))
}

fn ensure_settled_ownership(project_root: &Path, attempt: &Attempt) -> Result<()> {
    if attempt
        .tasks
        .iter()
        .any(|task| task.status == TaskStatus::Executing)
    {
        bail!("Attempt has a running Task");
    }
    for task in &attempt.tasks {
        crate::usage_limit_contracts::unlocked(
            &crate::lease::task_lock_path(project_root, &attempt.work_item_id, &task.id),
            false,
        )?;
        if let Some(area) = &task.artifact_area {
            let path = project_root.join(&area.path);
            crate::usage_limit_contracts::unlocked(&path.join("execution-owner.lock"), false)?;
            if crate::execution::execution_is_live(&path.join("execution.json"))? {
                bail!("Writer-completion recovery requires settled execution ownership");
            }
        }
    }
    crate::usage_limit_contracts::unlocked(
        &crate::lease::learner_lock_path(project_root, &attempt.work_item_id, &attempt.id),
        false,
    )?;
    Ok(())
}

fn inspect_eligible(project_root: &Path, attempt: &Attempt) -> Result<EligibleRecovery> {
    if attempt.status != AttemptStatus::NeedsUser
        || attempt.pause_kind != Some(PauseKind::WriterCompletion)
    {
        bail!("Attempt is not paused for Writer-completion recovery");
    }
    attempt.ensure_resume_ready()?;
    attempt.ensure_not_canceled()?;
    ensure_settled_ownership(project_root, attempt)?;
    let (source_index, source_writer) = attempt
        .tasks
        .iter()
        .enumerate()
        .rev()
        .find(|(_, task)| task.kind == TaskKind::Write && task.status == TaskStatus::Complete)
        .ok_or_else(|| anyhow::anyhow!("Attempt has no completed Writer candidate"))?;
    let source_output = source_writer
        .output
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("completed Writer has no candidate output"))?;
    let pending_writers = attempt
        .tasks
        .iter()
        .enumerate()
        .filter(|(index, task)| {
            *index > source_index
                && task.kind == TaskKind::Write
                && task.status == TaskStatus::NeedsUser
                && task.output.is_none()
        })
        .map(|(_, task)| task)
        .collect::<Vec<_>>();
    if pending_writers.len() > 1 {
        bail!("Attempt has more than one paused Writer-completion Task");
    }
    let (task_id, existing_no_change, committed_source_matrix) =
        if let Some(task) = pending_writers.first() {
            let area = task
                .artifact_area
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("paused Writer has no artifact area"))?;
            let area_path = project_root.join(&area.path);
            if !area_path.is_dir() {
                bail!("paused Writer artifact area is missing");
            }
            if !area_path
                .join(crate::recovery_contracts::WRITER_COMPLETION_MATRIX_FILE)
                .is_file()
            {
                bail!("paused Writer has no completion matrix to review");
            }
            (task.id.clone(), false, false)
        } else if source_output.no_change.is_some() {
            let area = source_writer.artifact_area.as_ref().ok_or_else(|| {
                anyhow::anyhow!("completed no-change Writer has no artifact area")
            })?;
            if !project_root
                .join(&area.path)
                .join(crate::recovery_contracts::WRITER_COMPLETION_MATRIX_FILE)
                .is_file()
            {
                bail!("completed no-change Writer has no completion matrix to review");
            }
            let next_task_id = format!(
                "{}-write-{}",
                attempt.id,
                attempt
                    .tasks
                    .iter()
                    .filter(|task| task.kind == TaskKind::Write)
                    .count()
                    + 1
            );
            anyhow::ensure!(
                !attempt.tasks.iter().any(|task| task.id == next_task_id),
                "corrective Writer identity is already reserved"
            );
            (next_task_id, true, false)
        } else {
            attempt.writer_completion_contract.as_ref().ok_or_else(|| {
                anyhow::anyhow!("committed Writer has no immutable completion contract")
            })?;
            let area = source_writer
                .artifact_area
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("committed Writer has no artifact area"))?;
            let matrix_path = project_root
                .join(&area.path)
                .join(crate::recovery_contracts::WRITER_COMPLETION_MATRIX_FILE);
            anyhow::ensure!(
                fs::metadata(&matrix_path)?.len()
                    <= crate::work_model::WRITER_COMPLETION_MATRIX_MAX_BYTES as u64,
                "committed Writer completion matrix exceeds the size limit"
            );
            let bytes = fs::read(&matrix_path).with_context(|| {
                format!(
                    "read committed Writer completion matrix {}",
                    matrix_path.display()
                )
            })?;
            if let Ok(matrix) =
                serde_json::from_slice::<crate::work_model::WriterCompletionMatrix>(&bytes)
            {
                validate_stalled_committed_writer(project_root, attempt, source_writer, &matrix)?;
            }
            let next_task_id = format!(
                "{}-write-{}",
                attempt.id,
                attempt
                    .tasks
                    .iter()
                    .filter(|task| task.kind == TaskKind::Write)
                    .count()
                    + 1
            );
            anyhow::ensure!(
                !attempt.tasks.iter().any(|task| task.id == next_task_id),
                "corrective Writer identity is already reserved"
            );
            (next_task_id, false, true)
        };
    let schedule_corrective_writer = committed_source_matrix
        || existing_no_change
            && writer_matrix_has_pending_writer_work(project_root, attempt, source_writer)?;
    if existing_no_change && !schedule_corrective_writer {
        bail!("completed no-change Writer has no pending Writer-owned work");
    }
    let workspace = resolve_managed_sibling_workspace_path(
        project_root,
        &source_output.workspace_path,
        "Writer-completion recovery candidate workspace",
    )?;
    let head = git::run_stdout(
        &workspace,
        &["rev-parse", "HEAD"],
        "resolve Writer-completion recovery candidate HEAD",
    )?;
    if head != source_output.commit {
        bail!(
            "candidate HEAD changed from {} to {head}; ordinary candidate drift remains rejected",
            source_output.commit
        );
    }
    let status = git::run_stdout(
        &workspace,
        &["status", "--porcelain", "--untracked-files=all", "--", "."],
        "check Writer-completion recovery candidate cleanliness",
    )?;
    if !status.is_empty() {
        bail!("Writer-completion recovery requires a clean candidate worktree:\n{status}");
    }
    let superseded_task_ids = attempt.tasks[source_index + 1..]
        .iter()
        .filter(|task| {
            task.kind == TaskKind::Write
                && matches!(task.status, TaskStatus::Planned | TaskStatus::NeedsUser)
                && task.output.is_none()
                && task.id != task_id
        })
        .map(|task| task.id.clone())
        .collect::<Vec<_>>();
    let bind_source = committed_source_matrix || existing_no_change && schedule_corrective_writer;
    let source_matrix_sha256 = if bind_source {
        let area = source_writer.artifact_area.as_ref().unwrap();
        let bytes = fs::read(
            project_root
                .join(&area.path)
                .join(crate::recovery_contracts::WRITER_COMPLETION_MATRIX_FILE),
        )?;
        Some(content_digest(&bytes))
    } else {
        None
    };
    let requirement_contract_sha256 = if bind_source {
        Some(content_digest(&serde_json::to_vec(
            attempt.writer_completion_contract.as_ref().unwrap(),
        )?))
    } else {
        None
    };
    let source_assignment_sha256 = if bind_source {
        Some(content_digest(&serde_json::to_vec(&(
            &source_writer.workspace_access,
            &source_writer.writer_correction_assignment,
        ))?))
    } else {
        None
    };
    let source_transcript_sha256 = if bind_source {
        let area = source_writer.artifact_area.as_ref().unwrap();
        Some(content_digest(
            &fs::read(project_root.join(&area.path).join("transcript.jsonl"))
                .context("read committed Writer transcript")?,
        ))
    } else {
        None
    };
    let source_output_sha256 = if bind_source {
        Some(content_digest(&serde_json::to_vec(source_output)?))
    } else {
        None
    };
    Ok(EligibleRecovery {
        task_id,
        source_writer_task_id: source_writer.id.clone(),
        candidate_commit: source_output.commit.clone(),
        workspace_path: source_output.workspace_path.clone(),
        protected_base_commit: source_output.base_commit.clone(),
        superseded_task_ids,
        existing_no_change,
        schedule_corrective_writer,
        committed_source_matrix,
        source_matrix_sha256,
        requirement_contract_sha256,
        source_assignment_sha256,
        source_transcript_sha256,
        source_output_sha256,
    })
}

/// Prove the repeated-progress guard's shape from the saved runs and matrices.
/// A valid incomplete matrix alone cannot authorize another committed Writer.
fn validate_stalled_committed_writer(
    project_root: &Path,
    attempt: &Attempt,
    source: &crate::work_model::Task,
    matrix: &crate::work_model::WriterCompletionMatrix,
) -> Result<()> {
    use crate::work_model::{WriterOutcome, WriterRunKind};

    anyhow::ensure!(
        attempt.review_state == Some(AttemptReviewState::NotReviewed)
            && attempt
                .tasks
                .last()
                .is_some_and(|task| task.id == source.id),
        "stalled Writer must be the latest Task at the completion pause"
    );
    let runs = attempt.writer_runs.iter().rev().take(3).collect::<Vec<_>>();
    anyhow::ensure!(
        runs.len() == 3
            && runs[0].task_id == source.id
            && runs[0].outcome == WriterOutcome::Blocked
            && runs[..2]
                .iter()
                .all(|run| run.kind != WriterRunKind::Initial),
        "Writer-completion pause does not record two stagnant continuations"
    );
    let contract = attempt
        .writer_completion_contract
        .as_ref()
        .context("stalled Writer completion contract is missing")?;
    let findings = crate::recovery_contracts::writer_completion_findings(project_root, source)?;
    matrix
        .status_against(contract, &source.id, &findings)
        .map_err(|reason| anyhow::anyhow!("stalled Writer matrix is invalid: {reason}"))?;
    anyhow::ensure!(
        !matrix.pending_writer_rows().is_empty(),
        "stalled Writer has no pending Writer-owned work"
    );
    let identity = matrix.completion_identity();
    let writers = attempt
        .tasks
        .iter()
        .rev()
        .filter(|task| task.kind == TaskKind::Write)
        .take(3)
        .collect::<Vec<_>>();
    anyhow::ensure!(writers.len() == 3, "stalled Writer history is incomplete");
    for (run, task) in runs.iter().zip(&writers) {
        let output = task
            .output
            .as_ref()
            .context("stalled Writer history has no output")?;
        anyhow::ensure!(
            run.task_id == task.id
                && task.status == TaskStatus::Complete
                && run.candidate_commit == output.commit
                && output.no_change.is_none()
                && output
                    .base_commit
                    .as_ref()
                    .is_some_and(|base| base != &output.commit)
                && run.checked_required == runs[0].checked_required
                && run.completed_matrix == runs[0].completed_matrix,
            "stalled Writer history changed or did not commit against the same obligations"
        );
        let artifact =
            crate::recovery_contracts::writer_completion_matrix_ref(project_root, attempt, task)
                .context("stalled Writer history has no matrix")?;
        let path =
            crate::recovery_contracts::resolve_input_artifact_path(project_root, &artifact.path)?;
        anyhow::ensure!(
            fs::metadata(&path)?.len()
                <= crate::work_model::WRITER_COMPLETION_MATRIX_MAX_BYTES as u64,
            "stalled Writer matrix exceeds the size limit"
        );
        let saved: crate::work_model::WriterCompletionMatrix =
            serde_json::from_slice(&fs::read(path)?)?;
        let findings = crate::recovery_contracts::writer_completion_findings(project_root, task)?;
        let status = saved
            .status_against(contract, &task.id, &findings)
            .map_err(|reason| anyhow::anyhow!("stalled Writer matrix is invalid: {reason}"))?;
        anyhow::ensure!(
            saved.completion_identity() == identity && status.complete == run.completed_matrix,
            "stalled Writer matrix changed or made completion progress"
        );
    }
    Ok(())
}

/// A completed no-change Writer needs a corrective Writer when its validated
/// matrix still contains an unfinished Writer-owned requirement. External rows
/// cannot grant another Writer round.
fn writer_matrix_has_pending_writer_work(
    project_root: &Path,
    attempt: &Attempt,
    task: &crate::work_model::Task,
) -> Result<bool> {
    let area = task
        .artifact_area
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Writer has no artifact area"))?;
    let path = project_root
        .join(&area.path)
        .join(crate::recovery_contracts::WRITER_COMPLETION_MATRIX_FILE);
    let source = fs::read_to_string(&path)
        .with_context(|| format!("read Writer completion matrix {}", path.display()))?;
    let matrix: crate::work_model::WriterCompletionMatrix = serde_json::from_str(&source)
        .with_context(|| format!("parse Writer completion matrix {}", path.display()))?;
    let contract = attempt
        .writer_completion_contract
        .as_ref()
        .context("Writer completion contract is missing")?;
    let findings = crate::recovery_contracts::writer_completion_findings(project_root, task)?;
    matrix
        .status_against(contract, &task.id, &findings)
        .map_err(|reason| anyhow::anyhow!("Writer completion matrix is invalid: {reason}"))?;
    Ok(matrix.rows.iter().any(|row| {
        row.responsibility.is_none_or(|responsibility| {
            responsibility == crate::work_model::CompletionResponsibility::Writer
        }) && row.state == crate::work_model::WriterCompletionState::Pending
    }))
}

fn write_authority_once(path: &Path, authority: &RecoveryAuthority) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(authority)?;
    match fs::read(path) {
        Ok(existing) if existing == bytes => Ok(()),
        Ok(_) => bail!(
            "Writer-completion recovery authority already differs at {}",
            path.display()
        ),
        Err(error) if error.kind() == ErrorKind::NotFound => {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            crate::atomic_write::atomic_write(path, &bytes)?;
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
}

fn write_reconciliation_once(path: &Path, authority: &RecoveryReconciliation) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(authority)?;
    match fs::read(path) {
        Ok(existing) if existing == bytes => Ok(()),
        Ok(_) => bail!(
            "Writer-completion recovery reconciliation already differs at {}",
            path.display()
        ),
        Err(error) if error.kind() == ErrorKind::NotFound => {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            crate::atomic_write::atomic_write(path, &bytes)?;
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git;
    use crate::work_model::{
        Task, TaskArtifactArea, TaskOutput, WorkItem, WorkspaceAccess, WorkspaceRef,
    };
    use std::fs;
    use tempfile::TempDir;

    struct Fixture {
        _tmp: TempDir,
        project: std::path::PathBuf,
        workspace: std::path::PathBuf,
        store: WorkModelStore,
    }

    fn fixture(paused_writer: bool) -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("main");
        let workspace = tmp.path().join("work-work-1-attempt-1-candidate");
        fs::create_dir_all(&project).unwrap();
        git::run(&project, &["init", "-q"], "init fixture").unwrap();
        git::run(&project, &["config", "user.name", "Test"], "config fixture").unwrap();
        git::run(
            &project,
            &["config", "user.email", "test@example.com"],
            "config fixture",
        )
        .unwrap();
        fs::write(project.join("README.md"), "base\n").unwrap();
        git::run(&project, &["add", "."], "stage fixture").unwrap();
        git::run(&project, &["commit", "-q", "-m", "Base"], "commit fixture").unwrap();
        let base = git::run_stdout(&project, &["rev-parse", "HEAD"], "base").unwrap();
        git::run(
            &project,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "candidate",
                workspace.to_str().unwrap(),
            ],
            "worktree fixture",
        )
        .unwrap();
        fs::write(workspace.join("candidate.txt"), "candidate\n").unwrap();
        git::run(&workspace, &["add", "."], "stage candidate").unwrap();
        git::run(
            &workspace,
            &["commit", "-q", "-m", "Candidate"],
            "commit candidate",
        )
        .unwrap();
        let candidate = git::run_stdout(&workspace, &["rev-parse", "HEAD"], "candidate").unwrap();

        let store = WorkModelStore::new(&project);
        let mut item = WorkItem::planned("work-1", "Writer recovery");
        item.add_initial_attempt("attempt-1").unwrap();
        let attempt = &mut item.attempts[0];
        let first = &mut attempt.tasks[0];
        first.status = TaskStatus::Complete;
        first.workspace_access = WorkspaceAccess {
            reads: Vec::new(),
            writes: vec![WorkspaceRef {
                id: "candidate".into(),
                path: "../work-work-1-attempt-1-candidate".into(),
            }],
        };
        first.artifact_area = Some(TaskArtifactArea {
            path: work_artifact_path("work-1", "attempt-1", &first.id),
        });
        first.output = Some(TaskOutput {
            workspace_id: "candidate".into(),
            workspace_path: "../work-work-1-attempt-1-candidate".into(),
            source_branch: "candidate".into(),
            base_commit: Some(base),
            commit: candidate.clone(),
            no_change: None,
            learner_canonicalization: None,
        });
        let paused_id = "attempt-1-write-2".to_string();
        let paused = Task {
            id: paused_id.clone(),
            kind: TaskKind::Write,
            status: if paused_writer {
                TaskStatus::NeedsUser
            } else {
                TaskStatus::Complete
            },
            role: "author".into(),
            instructions: Some("original instructions".into()),
            work_item_id: "work-1".into(),
            attempt_id: Some("attempt-1".into()),
            workspace_access: WorkspaceAccess {
                reads: Vec::new(),
                writes: vec![WorkspaceRef {
                    id: "candidate".into(),
                    path: "../work-work-1-attempt-1-candidate".into(),
                }],
            },
            artifact_area: Some(TaskArtifactArea {
                path: work_artifact_path("work-1", "attempt-1", &paused_id),
            }),
            review_context: None,
            writer_correction_assignment: None,
            verification_context: None,
            evidence_review_context: None,
            input_artifacts: Vec::new(),
            depends_on: None,
            output: if paused_writer {
                None
            } else {
                Some(TaskOutput {
                    workspace_id: "candidate".into(),
                    workspace_path: "../work-work-1-attempt-1-candidate".into(),
                    source_branch: "candidate".into(),
                    base_commit: Some(candidate.clone()),
                    commit: candidate.clone(),
                    no_change: None,
                    learner_canonicalization: None,
                })
            },
            created_at: Some(now_iso8601()),
            started_at: None,
            completed_at: None,
        };
        let area = project.join(paused.artifact_area.as_ref().unwrap().path.clone());
        fs::create_dir_all(&area).unwrap();
        fs::write(
            area.join(crate::recovery_contracts::WRITER_COMPLETION_MATRIX_FILE),
            b"{}\n",
        )
        .unwrap();
        attempt.tasks.push(paused);
        let stale_id = "attempt-1-write-3".to_string();
        attempt.tasks.push(Task {
            id: stale_id,
            kind: TaskKind::Write,
            status: TaskStatus::Planned,
            role: "author".into(),
            instructions: Some("stale".into()),
            work_item_id: "work-1".into(),
            attempt_id: Some("attempt-1".into()),
            workspace_access: WorkspaceAccess {
                reads: Vec::new(),
                writes: vec![WorkspaceRef {
                    id: "candidate".into(),
                    path: "../work-work-1-attempt-1-candidate".into(),
                }],
            },
            artifact_area: Some(TaskArtifactArea {
                path: work_artifact_path("work-1", "attempt-1", "attempt-1-write-3"),
            }),
            review_context: None,
            writer_correction_assignment: None,
            verification_context: None,
            evidence_review_context: None,
            input_artifacts: Vec::new(),
            depends_on: None,
            output: None,
            created_at: Some(now_iso8601()),
            started_at: None,
            completed_at: None,
        });
        attempt.status = AttemptStatus::NeedsUser;
        attempt.pause_kind = Some(PauseKind::WriterCompletion);
        attempt.review_state = Some(AttemptReviewState::NotReviewed);
        attempt.completed_at = Some(now_iso8601());
        store.create_work_item(&item).unwrap();
        Fixture {
            _tmp: tmp,
            project,
            workspace,
            store,
        }
    }

    fn stalled_committed_fixture(corrective: bool) -> Fixture {
        use crate::work_model::{
            WriterCompletionContract, WriterCompletionMatrix, WriterOutcome, WriterRun,
            WriterRunKind,
        };

        let fixture = fixture(false);
        let mut item = fixture.store.read_work_item("work-1").unwrap();
        let attempt = &mut item.attempts[0];
        attempt.writer_completion_contract = WriterCompletionContract::from_planning_context(&crate::work_model::PlanningContext {
            behaviors: Some("# Behaviors\n## Recovery\n### B1\nFinish the implementation.\nTest: src/lib.rs\n### B2\nFinish the tests.\nTest: src/lib.rs\n".into()),
            plan: Some("# Plan\n| # | State reached | Verification | Responsibility | Req? |\n|---|---|---|---|---|\n| 1 | Review | Reviews | review | Yes |\n| 2 | Test | Full suite | tester | Yes |\n| 3 | Learn | Learn | learner | Yes |\n".into()),
            ..Default::default()
        });
        let review = ArtifactRef {
            producer_id: "review-tests".into(),
            path: work_artifact_path("work-1", "attempt-1", "review-tests/review.md"),
        };
        fs::create_dir_all(fixture.project.join(&review.path).parent().unwrap()).unwrap();
        fs::write(
            fixture.project.join(&review.path),
            "Verdict: fail\n- [ ] Complete the pending correction (blocking)\n",
        )
        .unwrap();
        let tester = ArtifactRef {
            producer_id: "tester".into(),
            path: work_artifact_path("work-1", "attempt-1", "tester/tester-results.json"),
        };
        fs::create_dir_all(fixture.project.join(&tester.path).parent().unwrap()).unwrap();
        fs::write(
            fixture.project.join(&tester.path),
            "retained Tester evidence\n",
        )
        .unwrap();
        attempt.artifacts.extend([review.clone(), tester]);
        let initial_output = attempt.tasks[0].output.clone().unwrap();
        for (index, task) in attempt.tasks.iter_mut().enumerate() {
            let mut output = initial_output.clone();
            if index > 0 {
                output.base_commit = Some(
                    git::run_stdout(
                        &fixture.workspace,
                        &["rev-parse", "HEAD"],
                        "prior candidate",
                    )
                    .unwrap(),
                );
                fs::write(
                    fixture.workspace.join("candidate.txt"),
                    format!("continuation {index}\n"),
                )
                .unwrap();
                git::run(
                    &fixture.workspace,
                    &["add", "candidate.txt"],
                    "stage continuation",
                )
                .unwrap();
                git::run(
                    &fixture.workspace,
                    &["commit", "-qm", "Continue implementation"],
                    "commit continuation",
                )
                .unwrap();
                output.commit =
                    git::run_stdout(&fixture.workspace, &["rev-parse", "HEAD"], "candidate")
                        .unwrap();
            }
            task.status = TaskStatus::Complete;
            task.output = Some(output.clone());
            if corrective {
                task.input_artifacts.push(review.clone());
            }
            let area = task.artifact_area.as_ref().unwrap();
            fs::create_dir_all(fixture.project.join(&area.path)).unwrap();
            let matrix_ref = ArtifactRef {
                producer_id: task.id.clone(),
                path: format!("{}/writer-completion.json", area.path),
            };
            fs::write(
                fixture.project.join(&area.path).join("transcript.jsonl"),
                "retained Writer transcript\n",
            )
            .unwrap();
            task.input_artifacts.push(matrix_ref);
            attempt.writer_runs.push(WriterRun {
                task_id: task.id.clone(),
                outcome: if index == 2 {
                    WriterOutcome::Blocked
                } else {
                    WriterOutcome::Continue
                },
                kind: if corrective {
                    WriterRunKind::Corrective
                } else if index == 0 {
                    WriterRunKind::Initial
                } else {
                    WriterRunKind::PreReviewContinuation
                },
                provider: "codex".into(),
                session_id: None,
                continuation: index as u32,
                checked_required: 0,
                completed_matrix: 0,
                candidate_commit: output.commit,
            });
        }
        fixture
            .store
            .mutate_work_item("work-1", |stored| {
                *stored = item.clone();
                Ok(())
            })
            .unwrap();
        for task in &item.attempts[0].tasks {
            let matrix = WriterCompletionMatrix::skeleton(
                item.attempts[0]
                    .writer_completion_contract
                    .as_ref()
                    .unwrap(),
                &task.id,
                &crate::recovery_contracts::writer_completion_findings(&fixture.project, task)
                    .unwrap(),
                None,
            )
            .unwrap();
            fs::write(
                fixture
                    .project
                    .join(&task.artifact_area.as_ref().unwrap().path)
                    .join("writer-completion.json"),
                serde_json::to_vec_pretty(&matrix).unwrap(),
            )
            .unwrap();
        }
        fixture
    }

    #[test]
    fn recovers_stalled_committed_writer_with_valid_matrix() {
        for corrective in [false, true] {
            let fixture = stalled_committed_fixture(corrective);
            let before = fixture.store.read_work_item("work-1").unwrap();
            let attempt = &before.attempts[0];
            let source = attempt.tasks.last().unwrap();
            let area = fixture
                .project
                .join(&source.artifact_area.as_ref().unwrap().path);
            let matrix = fs::read(area.join("writer-completion.json")).unwrap();
            let transcript = fs::read(area.join("transcript.jsonl")).unwrap();
            assert!(
                recovery_action(&fixture.project, attempt).is_some(),
                "{:?}",
                inspect_eligible(&fixture.project, attempt)
            );
            assert_eq!(
                status_recovery_action(&fixture.project, attempt),
                recovery_action(&fixture.project, attempt)
            );

            let recovered = repair(
                &fixture.project,
                &fixture.store,
                "work-1",
                "attempt-1",
                "Authorize another Writer to finish the unchanged obligations.",
            )
            .unwrap();
            let after = fixture.store.read_work_item("work-1").unwrap();
            let next = &after.attempts[0];
            assert_eq!(recovered.task_id, "attempt-1-write-4");
            assert!(!recovered.already_applied);
            assert_eq!(next.tasks.len(), 4);
            assert_eq!(next.tasks[..3], attempt.tasks);
            assert_eq!(next.writer_runs, attempt.writer_runs);
            assert_eq!(next.status, AttemptStatus::Planned);
            assert_eq!(next.pause_kind, None);
            assert_eq!(next.tasks[3].status, TaskStatus::Planned);
            assert_eq!(next.recovery_operations.len(), 1);
            assert!(planned_successor_resume_ready(&fixture.project, &after, next).unwrap());
            let context = validated_writer_context(&fixture.project, &after, &next.tasks[3])
                .unwrap()
                .unwrap();
            assert_eq!(context.source_task_id, source.id);
            assert_eq!(
                context.candidate_commit,
                source.output.as_ref().unwrap().commit
            );
            let authority: RecoveryAuthority = serde_json::from_slice(
                &fs::read(fixture.project.join(&recovered.audit_artifact)).unwrap(),
            )
            .unwrap();
            assert_eq!(
                authority.source_matrix_sha256,
                Some(content_digest(&matrix))
            );
            assert_eq!(
                authority.source_transcript_sha256,
                Some(content_digest(&transcript))
            );
            assert!(authority.requirement_contract_sha256.is_some());
            assert!(authority.source_assignment_sha256.is_some());
            assert!(authority.source_output_sha256.is_some());
            assert_eq!(
                fs::read(area.join("writer-completion.json")).unwrap(),
                matrix
            );
            assert_eq!(fs::read(area.join("transcript.jsonl")).unwrap(), transcript);
            for artifact in &attempt.artifacts {
                assert!(next.artifacts.contains(artifact));
                assert!(
                    !fs::read(fixture.project.join(&artifact.path))
                        .unwrap()
                        .is_empty()
                );
            }
        }
    }

    #[test]
    fn retries_paused_writer_and_supersedes_duplicate_rounds() {
        let fixture = fixture(true);
        let first = repair(
            &fixture.project,
            &fixture.store,
            "work-1",
            "attempt-1",
            "Retry the exact Writer once.",
        )
        .unwrap();
        assert!(!first.already_applied);
        let item = fixture.store.read_work_item("work-1").unwrap();
        let attempt = &item.attempts[0];
        assert_eq!(attempt.status, AttemptStatus::NeedsUser);
        assert_eq!(attempt.pause_kind, Some(PauseKind::WriterCompletion));
        assert!(attempt.completed_at.is_none());
        assert_eq!(attempt.tasks[1].status, TaskStatus::Planned);
        assert_eq!(attempt.tasks[2].status, TaskStatus::Superseded);
        assert!(
            attempt.tasks[1]
                .input_artifacts
                .iter()
                .any(|a| a.producer_id == PRODUCER)
        );
        assert_eq!(attempt.recovery_operations.len(), 1);
        assert_eq!(
            attempt.recovery_operations[0].operation_kind,
            "writer-completion-recovery"
        );
        let second = repair(
            &fixture.project,
            &fixture.store,
            "work-1",
            "attempt-1",
            "Retry the exact Writer once.",
        )
        .unwrap();
        assert!(second.already_applied);
        assert_eq!(second.task_id, first.task_id);
        assert_eq!(
            fixture.store.read_work_item("work-1").unwrap().attempts[0]
                .tasks
                .len(),
            3
        );
        assert_eq!(
            fixture.store.read_work_item("work-1").unwrap().attempts[0]
                .recovery_operations
                .len(),
            1
        );
    }

    #[test]
    fn learner_history_recognizes_superseded_writer_from_completion_recovery() {
        let fixture = fixture(true);
        repair(
            &fixture.project,
            &fixture.store,
            "work-1",
            "attempt-1",
            "Retry the exact Writer",
        )
        .unwrap();
        let item = fixture.store.read_work_item("work-1").unwrap();
        let mut attempt = item.attempts[0].clone();
        assert!(
            attempt
                .tasks
                .iter()
                .any(|task| task.status == TaskStatus::Superseded)
        );
        let candidate = attempt.tasks[0].output.clone();
        for task in &mut attempt.tasks {
            if task.status != TaskStatus::Superseded {
                task.status = TaskStatus::Complete;
                task.output
                    .get_or_insert_with(|| candidate.clone().unwrap());
            }
        }
        attempt.status = AttemptStatus::Complete;
        attempt.pause_kind = None;
        attempt.review_state = Some(AttemptReviewState::Passed);
        crate::lifecycle_recovery::inspect_learner_terminal_history(
            Some(&fixture.project),
            &attempt,
            "work-1",
        )
        .unwrap();
    }

    #[test]
    fn ordinary_candidate_drift_is_rejected_without_audit() {
        let fixture = fixture(true);
        fs::write(fixture.workspace.join("drift.txt"), "drift\n").unwrap();
        let error = repair(
            &fixture.project,
            &fixture.store,
            "work-1",
            "attempt-1",
            "Retry",
        )
        .unwrap_err();
        assert!(error.to_string().contains("clean candidate worktree"));
        assert!(
            !fixture
                .project
                .join(".fluent/work/artifacts/work-1/attempt-1/writer-completion-recoveries")
                .exists()
        );
    }

    #[test]
    fn reconciles_failed_duplicate_after_recovery_writer_settles() {
        let fixture = fixture(true);
        let first = repair(
            &fixture.project,
            &fixture.store,
            "work-1",
            "attempt-1",
            "Retry the exact Writer once.",
        )
        .unwrap();
        let candidate_output = {
            let item = fixture.store.read_work_item("work-1").unwrap();
            item.attempts[0].tasks[0].output.clone().unwrap()
        };
        fixture
            .store
            .mutate_work_item("work-1", |item| {
                let attempt = &mut item.attempts[0];
                let source = attempt
                    .tasks
                    .iter_mut()
                    .find(|task| task.id == first.task_id)
                    .unwrap();
                source.status = TaskStatus::Complete;
                source.output = Some(candidate_output.clone());
                source.completed_at = Some(now_iso8601());

                let mut duplicate = source.clone();
                duplicate.id = "attempt-1-write-4".to_string();
                duplicate.status = TaskStatus::Failed;
                duplicate.output = None;
                duplicate.started_at = Some(now_iso8601());
                duplicate.completed_at = Some(now_iso8601());
                duplicate.artifact_area = Some(TaskArtifactArea {
                    path: work_artifact_path("work-1", "attempt-1", &duplicate.id),
                });
                attempt.tasks.push(duplicate);
                attempt.status = AttemptStatus::NeedsUser;
                attempt.pause_kind = Some(PauseKind::RoundCap);
                attempt.review_state = Some(AttemptReviewState::Failed);
                attempt.completed_at = Some(now_iso8601());
                Ok(())
            })
            .unwrap();

        let reconciled = repair(
            &fixture.project,
            &fixture.store,
            "work-1",
            "attempt-1",
            "Retry the exact Writer once.",
        )
        .unwrap();
        assert!(!reconciled.already_applied);
        assert_eq!(reconciled.reconciled_task_ids, ["attempt-1-write-4"]);
        let item = fixture.store.read_work_item("work-1").unwrap();
        let attempt = &item.attempts[0];
        assert_eq!(attempt.status, AttemptStatus::Planned);
        assert_eq!(attempt.pause_kind, None);
        assert_eq!(attempt.tasks[2].status, TaskStatus::Superseded);
        assert!(attempt.artifacts.iter().any(|artifact| {
            artifact.producer_id == "writer-completion-recovery-reconciliation"
        }));

        let repeated = repair(
            &fixture.project,
            &fixture.store,
            "work-1",
            "attempt-1",
            "Retry the exact Writer once.",
        )
        .unwrap();
        assert!(repeated.already_applied);
        assert_eq!(repeated.reconciled_task_ids, ["attempt-1-write-4"]);
        assert_eq!(
            fixture.store.read_work_item("work-1").unwrap().attempts[0]
                .tasks
                .len(),
            4
        );
    }

    #[test]
    fn completed_no_change_with_pending_finding_schedules_corrective_writer() {
        let fixture = fixture(false);
        let task_id = "attempt-1-write-2";
        let area = work_artifact_path("work-1", "attempt-1", task_id);
        let review_path = format!("{area}/review-documentation/review.md");
        fs::create_dir_all(fixture.project.join(&area).join("review-documentation")).unwrap();
        fs::write(
            fixture.project.join(&review_path),
            "Verdict: fail\n\n- [ ] Fix the documented line-length finding.\n",
        )
        .unwrap();
        fs::write(fixture.project.join(&area).join("transcript.jsonl"), "").unwrap();
        fs::write(
            fixture.project.join(&area).join("no-change.json"),
            serde_json::json!({
                "schema-version": 1,
                "declaration_path": format!("{area}/no-change.json"),
                "reason": "The prior Writer found no safe source change.",
                "verification": [{"command": "cargo test --lib", "result": "pass"}]
            })
            .to_string(),
        )
        .unwrap();
        fixture
            .store
            .mutate_work_item("work-1", |item| {
                item.attempts[0].writer_completion_contract =
                    Some(crate::work_model::WriterCompletionContract {
                        version: crate::work_model::WRITER_COMPLETION_CONTRACT_VERSION,
                        requirements: Vec::new(),
                    });
                item.attempts[0].tasks[1].output.as_mut().unwrap().no_change =
                    Some(crate::work_model::NoChangeOutput {
                        schema_version: 1,
                        declaration_path: format!("{area}/no-change.json"),
                        reason: "The prior Writer found no safe source change.".to_string(),
                        verification: vec![crate::work_model::NoChangeVerification {
                            command: "cargo test --lib".to_string(),
                            result: crate::work_model::NoChangeVerificationResult::Pass,
                        }],
                    });
                item.attempts[0].tasks[1].input_artifacts.push(ArtifactRef {
                    producer_id: "review-documentation".to_string(),
                    path: review_path,
                });
                item.attempts[0].tasks[1].input_artifacts.push(ArtifactRef {
                    producer_id: task_id.to_string(),
                    path: format!(
                        "{area}/{}",
                        crate::recovery_contracts::WRITER_COMPLETION_MATRIX_FILE
                    ),
                });
                Ok(())
            })
            .unwrap();
        let item = fixture.store.read_work_item("work-1").unwrap();
        let attempt = &item.attempts[0];
        let findings = crate::recovery_contracts::writer_completion_findings(
            &fixture.project,
            &attempt.tasks[1],
        )
        .unwrap();
        assert_eq!(findings.len(), 1);
        let matrix = crate::work_model::WriterCompletionMatrix::skeleton(
            attempt.writer_completion_contract.as_ref().unwrap(),
            task_id,
            &findings,
            None,
        )
        .unwrap();
        fs::write(
            fixture
                .project
                .join(&area)
                .join(crate::recovery_contracts::WRITER_COMPLETION_MATRIX_FILE),
            serde_json::to_vec(&matrix).unwrap(),
        )
        .unwrap();

        let resolution = "Authorize a corrective Writer for the pending documentation finding.";
        let first = repair(
            &fixture.project,
            &fixture.store,
            "work-1",
            "attempt-1",
            resolution,
        )
        .unwrap();
        assert_eq!(first.task_id, "attempt-1-write-4");
        assert!(!first.already_applied);
        let item = fixture.store.read_work_item("work-1").unwrap();
        let attempt = &item.attempts[0];
        assert_eq!(attempt.status, AttemptStatus::Planned);
        assert_eq!(attempt.pause_kind, None);
        assert_eq!(attempt.tasks[1].status, TaskStatus::Complete);
        assert!(
            attempt.tasks[1]
                .output
                .as_ref()
                .unwrap()
                .no_change
                .is_some()
        );
        assert_eq!(attempt.tasks[2].status, TaskStatus::Superseded);
        assert_eq!(attempt.tasks[3].status, TaskStatus::Planned);
        assert!(attempt.tasks[3].input_artifacts.iter().any(|artifact| {
            artifact.producer_id == PRODUCER && artifact.path.ends_with("/recovery.json")
        }));
        assert!(attempt.tasks[3].input_artifacts.iter().any(|artifact| {
            artifact
                .path
                .ends_with("/attempt-1-write-2/writer-completion.json")
        }));

        let second = repair(
            &fixture.project,
            &fixture.store,
            "work-1",
            "attempt-1",
            resolution,
        )
        .unwrap();
        assert!(second.already_applied);
        assert_eq!(second.task_id, "attempt-1-write-4");
        let repeated = fixture.store.read_work_item("work-1").unwrap();
        assert_eq!(repeated.attempts[0].tasks.len(), 4);
        assert_eq!(repeated.attempts[0].tasks[3].status, TaskStatus::Planned);
    }
}
