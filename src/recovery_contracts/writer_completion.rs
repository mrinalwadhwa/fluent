//! Validate recorded Writer-completion authority without lifecycle mutation.
use crate::{
    follow_up::content_digest,
    git,
    work_model::{
        ArtifactRef, Attempt, RecoveryOperationRequest, TaskKind, TaskStatus,
        resolve_managed_sibling_workspace_path, work_artifact_path,
    },
};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, fs, path::Path};

pub(crate) const SCHEMA_VERSION: u32 = 1;
pub(crate) const PRODUCER: &str = "writer-completion-recovery";
const MAX_RESOLUTION_BYTES: usize = 4_096;
const INSTRUCTIONS_MARKER: &str = "Fluent writer-completion recovery";

/// Admit terminal Superseded Tasks only when a completed replacement still
/// carries the exact authority and operation written by completion recovery.
pub(crate) fn validate_terminal_superseded_history(
    project_root: &Path,
    attempt: &Attempt,
    work_item_id: &str,
) -> Result<()> {
    let actual = attempt
        .tasks
        .iter()
        .filter(|task| task.status == TaskStatus::Superseded)
        .map(|task| task.id.clone())
        .collect::<BTreeSet<_>>();
    let mut authorized = BTreeSet::new();
    for reference in attempt
        .artifacts
        .iter()
        .filter(|reference| reference.producer_id == PRODUCER)
    {
        let path =
            crate::recovery_contracts::resolve_input_artifact_path(project_root, &reference.path)?;
        let canonical = fs::canonicalize(&path)?;
        anyhow::ensure!(
            canonical.starts_with(fs::canonicalize(project_root)?) && canonical.is_file(),
            "Writer-completion recovery authority must be a project file"
        );
        anyhow::ensure!(
            fs::metadata(&canonical)?.len() <= 64 * 1024,
            "Writer-completion recovery authority exceeds 65536 bytes"
        );
        let bytes = fs::read(&canonical)?;
        let authority: RecoveryAuthority = serde_json::from_slice(&bytes)
            .context("parse terminal Writer-completion recovery authority")?;
        anyhow::ensure!(
            bytes == serde_json::to_vec_pretty(&authority)?,
            "Writer-completion recovery authority bytes changed"
        );
        anyhow::ensure!(
            authority.schema_version == SCHEMA_VERSION
                && authority.work_item_id == work_item_id
                && authority.attempt_id == attempt.id
                && authority.resolution == normalize_resolution(&authority.resolution)?
                && authority.resolution_sha256 == content_digest(authority.resolution.as_bytes())
                && reference.path
                    == work_artifact_path(
                        work_item_id,
                        &attempt.id,
                        &format!(
                            "writer-completion-recoveries/{}/recovery.json",
                            authority.task_id
                        )
                    ),
            "Writer-completion recovery authority does not match this Attempt"
        );
        let source_index = attempt
            .tasks
            .iter()
            .position(|task| task.id == authority.source_writer_task_id)
            .context("Writer-completion recovery source Writer is missing")?;
        let replacement_index = attempt
            .tasks
            .iter()
            .position(|task| task.id == authority.task_id)
            .context("Writer-completion recovery replacement Writer is missing")?;
        let source = &attempt.tasks[source_index];
        let replacement = &attempt.tasks[replacement_index];
        validate_committed_source_evidence(project_root, attempt, source, &authority)?;
        let output = source
            .output
            .as_ref()
            .context("Writer-completion recovery source has no output")?;
        if source_index == replacement_index
            && output.no_change.is_some()
            && authority.source_matrix_sha256.is_none()
        {
            anyhow::ensure!(
                source.kind == TaskKind::Write
                    && source.status == TaskStatus::Complete
                    && output.commit == authority.candidate_commit
                    && output.base_commit == authority.protected_base_commit
                    && source.input_artifacts.contains(reference)
                    && authority.superseded_task_ids.is_empty(),
                "historical no-change Writer-completion approval changed"
            );
            validate_task_recovery_source(project_root, attempt, source)?;
            continue;
        }
        anyhow::ensure!(
            source_index < replacement_index
                && source.kind == TaskKind::Write
                && source.status == TaskStatus::Complete
                && output.commit == authority.candidate_commit
                && output.base_commit == authority.protected_base_commit
                && replacement.kind == TaskKind::Write
                && replacement.status == TaskStatus::Complete
                && replacement.input_artifacts.contains(reference),
            "Writer-completion recovery source or completed replacement changed"
        );
        let request = RecoveryOperationRequest::new(
            PRODUCER,
            work_item_id,
            &attempt.id,
            Some(authority.candidate_commit.clone()),
            Some(if authority.source_matrix_sha256.is_some() {
                authority.source_writer_task_id.clone()
            } else {
                authority.task_id.clone()
            }),
            None,
            Some(output.workspace_path.clone()),
            Some(authority.resolution.clone()),
            Some(format!(
                "sha256:{:x}",
                Sha256::digest(serde_json::to_vec(&authority)?)
            )),
        );
        anyhow::ensure!(
            attempt
                .recovery_operations
                .iter()
                .any(|record| record.matches_request(&request)),
            "Writer-completion recovery has no matching recorded operation"
        );
        for id in &authority.superseded_task_ids {
            let index = attempt
                .tasks
                .iter()
                .position(|task| task.id == *id)
                .context("Writer-completion recovery Superseded Task is missing")?;
            let task = &attempt.tasks[index];
            anyhow::ensure!(
                index > replacement_index
                    && task.status == TaskStatus::Superseded
                    && authorized.insert(id.clone()),
                "Writer-completion recovery Superseded Task changed or was authorized twice"
            );
        }
    }
    for id in super::context_retry::terminal_superseded_reservations(project_root, attempt)? {
        anyhow::ensure!(
            authorized.insert(id),
            "reservation was superseded by conflicting authorities"
        );
    }
    anyhow::ensure!(
        actual == authorized,
        "Superseded Task history has no matching completed recovery authority"
    );
    Ok(())
}

/// Instructions and exact files admitted by the recovery owner. Callers must
/// not turn a filename or a Task instruction into recovery authority.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WriterRecoveryContext {
    pub instructions: String,
    pub artifacts: Vec<ArtifactRef>,
    pub source_task_id: String,
    pub candidate_commit: String,
    // Only the exact replacement named by a matrix-bound audit can use that
    // repair obligation to start without current verification evidence.
    repairs_committed_matrix: bool,
}

pub(crate) fn validated_writer_context(
    project_root: &Path,
    item: &crate::work_model::WorkItem,
    task: &crate::work_model::Task,
) -> Result<Option<WriterRecoveryContext>> {
    let references = task
        .input_artifacts
        .iter()
        .filter(|reference| {
            reference.producer_id == PRODUCER
                || Path::new(&reference.path)
                    .components()
                    .any(|part| part.as_os_str() == "writer-completion-recoveries")
        })
        .collect::<Vec<_>>();
    let instructions = task.instructions.as_deref().unwrap_or_default();
    if references.is_empty() {
        anyhow::ensure!(
            !instructions.contains(INSTRUCTIONS_MARKER),
            "Writer-completion recovery instructions have no declared authority"
        );
        return Ok(None);
    }
    anyhow::ensure!(
        task.kind == TaskKind::Write,
        "completion recovery requires a Writer Task"
    );
    let attempt = item
        .attempts
        .iter()
        .find(|attempt| Some(attempt.id.as_str()) == task.attempt_id.as_deref())
        .context("Writer-completion recovery Attempt is missing")?;
    let original_instructions =
        crate::usage_limit_contracts::instructions_before_retries(project_root, attempt, task)?;
    let original_instructions = original_instructions.as_deref().unwrap_or_default();
    let mut artifacts = Vec::new();
    let mut instruction_binding = None;
    let mut repairs_committed_matrix = false;
    for reference in references {
        anyhow::ensure!(
            reference.producer_id == PRODUCER && attempt.artifacts.contains(reference),
            "Writer-completion recovery authority has an invalid producer or is not attached to the Attempt"
        );
        let path =
            crate::recovery_contracts::resolve_input_artifact_path(project_root, &reference.path)?;
        let canonical = fs::canonicalize(&path).with_context(|| {
            format!(
                "read Writer-completion recovery authority {}",
                path.display()
            )
        })?;
        anyhow::ensure!(
            canonical.starts_with(fs::canonicalize(project_root)?) && canonical.is_file(),
            "Writer-completion recovery authority must resolve to an exact file in the project"
        );
        anyhow::ensure!(
            fs::metadata(&canonical)?.len() <= 64 * 1024,
            "Writer-completion recovery authority exceeds 65536 bytes"
        );
        let bytes = fs::read(&canonical)?;
        let authority: RecoveryAuthority =
            serde_json::from_slice(&bytes).context("parse Writer-completion recovery authority")?;
        anyhow::ensure!(
            bytes == serde_json::to_vec_pretty(&authority)?,
            "Writer-completion recovery authority bytes changed from the recorded format"
        );
        anyhow::ensure!(
            authority.schema_version == SCHEMA_VERSION
                && authority.work_item_id == item.id
                && authority.attempt_id == attempt.id
                && authority.resolution == normalize_resolution(&authority.resolution)?
                && authority.resolution_sha256 == content_digest(authority.resolution.as_bytes())
                && reference.path
                    == work_artifact_path(
                        &item.id,
                        &attempt.id,
                        &format!(
                            "writer-completion-recoveries/{}/recovery.json",
                            authority.task_id
                        )
                    ),
            "Writer-completion recovery authority has invalid version, identity, path, or resolution"
        );
        let source = attempt
            .tasks
            .iter()
            .find(|source| source.id == authority.source_writer_task_id)
            .context("Writer-completion recovery source Writer is missing")?;
        if source.id != task.id
            && source
                .input_artifacts
                .iter()
                .any(|input| input.producer_id == PRODUCER)
        {
            validate_task_recovery_source(project_root, attempt, source)?;
        }
        validate_committed_source_evidence(project_root, attempt, source, &authority)?;
        let output = source
            .output
            .as_ref()
            .context("Writer-completion recovery source has no output")?;
        if authority.source_matrix_sha256.is_some()
            && task.output.is_none()
            && matches!(task.status, TaskStatus::Planned | TaskStatus::Executing)
        {
            if let Some(approval) = crate::usage_limit_contracts::validated_prelaunch_approval(
                project_root,
                item,
                task,
            )? {
                anyhow::ensure!(
                    approval.binding.source_candidate == authority.candidate_commit
                        && approval.binding.workspace == output.workspace_path,
                    "quota approval does not match the Writer-completion recovery source"
                );
            } else {
                let workspace = resolve_managed_sibling_workspace_path(
                    project_root,
                    &output.workspace_path,
                    "Writer-completion recovery candidate workspace",
                )?;
                let head = git::run_stdout(
                    &workspace,
                    &["rev-parse", "HEAD"],
                    "check Writer-completion recovery candidate HEAD before launch",
                )?;
                let status = git::run_stdout(
                    &workspace,
                    &["status", "--porcelain", "--untracked-files=all", "--", "."],
                    "check Writer-completion recovery candidate cleanliness before launch",
                )?;
                anyhow::ensure!(
                    head == authority.candidate_commit && status.is_empty(),
                    "Writer-completion recovery candidate changed or is dirty before launch"
                );
            }
        }
        let authorized = attempt
            .tasks
            .iter()
            .find(|source| source.id == authority.task_id)
            .context("Writer-completion recovery authorized Task is missing")?;
        anyhow::ensure!(
            source.kind == TaskKind::Write
                && source.status == TaskStatus::Complete
                && output.commit == authority.candidate_commit
                && output.base_commit == authority.protected_base_commit
                && authorized.kind == TaskKind::Write
                && authorized.input_artifacts.contains(reference)
                && task.workspace_access.writes == authorized.workspace_access.writes,
            "Writer-completion recovery source or Task binding changed"
        );
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
            Some(output.workspace_path.clone()),
            Some(authority.resolution.clone()),
            Some(format!(
                "sha256:{:x}",
                Sha256::digest(serde_json::to_vec(&authority)?)
            )),
        );
        anyhow::ensure!(
            item.validate_recovery_operation(project_root, &operation)?
                .is_some(),
            "Writer-completion recovery has no matching recorded operation"
        );
        if original_instructions == recovery_instructions(&authority.resolution, &reference.path)
            || original_instructions
                == corrective_writer_instructions(&authority.resolution, &reference.path)
        {
            anyhow::ensure!(
                instruction_binding.is_none(),
                "Writer-completion recovery instructions have ambiguous authority"
            );
            repairs_committed_matrix =
                authority.source_matrix_sha256.is_some() && authority.task_id == task.id;
            if authority.source_matrix_sha256.is_some() {
                // Deliver only the matrix whose bytes this authority validated.
                // The executor grants these exact recovery inputs read access.
                let area = source
                    .artifact_area
                    .as_ref()
                    .context("recovery source has no artifact area")?;
                artifacts.push(ArtifactRef {
                    producer_id: source.id.clone(),
                    path: format!(
                        "{}/{}",
                        area.path,
                        crate::recovery_contracts::WRITER_COMPLETION_MATRIX_FILE
                    ),
                });
            }
            instruction_binding =
                Some((authority.source_writer_task_id, authority.candidate_commit));
        }
        artifacts.push(reference.clone());
    }
    let (source_task_id, candidate_commit) = instruction_binding.context(
        "Writer-completion recovery Task instructions do not match the recorded authorization",
    )?;
    Ok(Some(WriterRecoveryContext {
        instructions: instructions.to_string(),
        artifacts,
        source_task_id,
        candidate_commit,
        repairs_committed_matrix,
    }))
}

/// Recognize the exact audited repair at the caller's checked model frontier.
/// This grants admission only; completion and verification keep their own gates.
pub(crate) fn validated_committed_repair_start(
    project_root: &Path,
    item: &crate::work_model::WorkItem,
    task: &crate::work_model::Task,
) -> Result<bool> {
    let Some(context) = validated_writer_context(project_root, item, task)? else {
        return Ok(false);
    };
    if !context.repairs_committed_matrix {
        return Ok(false);
    }
    let attempt = item
        .attempts
        .last()
        .filter(|attempt| Some(attempt.id.as_str()) == task.attempt_id.as_deref())
        .context("Writer-completion repair is not on the current Attempt")?;
    attempt.ensure_not_canceled()?;
    anyhow::ensure!(
        attempt.tasks.last() == Some(task)
            && task.status == TaskStatus::Planned
            && task.output.is_none()
            && attempt
                .tasks
                .iter()
                .rev()
                .find(|source| {
                    source.kind == TaskKind::Write && source.status == TaskStatus::Complete
                })
                .is_some_and(|source| source.id == context.source_task_id)
            && !item
                .merge_candidates
                .iter()
                .any(|candidate| candidate.attempt_id == attempt.id),
        "Writer-completion repair is not the current authorized Task"
    );
    // The executor already holds the starting Task's leases; every other owner
    // must be settled. Queue admission also checks the starting Task's leases.
    ensure_settled_ownership(project_root, attempt, Some(&task.id))?;
    Ok(true)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) struct RecoveryAuthority {
    pub(crate) schema_version: u32,
    pub(crate) work_item_id: String,
    pub(crate) attempt_id: String,
    pub(crate) task_id: String,
    pub(crate) source_writer_task_id: String,
    pub(crate) candidate_commit: String,
    pub(crate) protected_base_commit: Option<String>,
    pub(crate) superseded_task_ids: Vec<String>,
    pub(crate) resolution: String,
    pub(crate) resolution_sha256: String,
    pub(crate) created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) source_matrix_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) requirement_contract_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) source_assignment_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) source_transcript_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) source_output_sha256: Option<String>,
}

pub(crate) fn validate_committed_source_evidence(
    project_root: &Path,
    attempt: &Attempt,
    source: &crate::work_model::Task,
    authority: &RecoveryAuthority,
) -> Result<()> {
    if let Some(expected) = &authority.source_matrix_sha256 {
        let area = source
            .artifact_area
            .as_ref()
            .context("recovery source has no artifact area")?;
        let path = project_root
            .join(&area.path)
            .join(crate::recovery_contracts::WRITER_COMPLETION_MATRIX_FILE);
        anyhow::ensure!(
            fs::metadata(&path)?.len()
                <= crate::work_model::WRITER_COMPLETION_MATRIX_MAX_BYTES as u64,
            "recovery source matrix exceeds the size limit"
        );
        let bytes = fs::read(&path)
            .with_context(|| format!("read recovery source matrix {}", path.display()))?;
        anyhow::ensure!(
            content_digest(&bytes) == *expected,
            "recovery source matrix changed"
        );
        let contract = attempt
            .writer_completion_contract
            .as_ref()
            .context("recovery completion contract is missing")?;
        anyhow::ensure!(
            authority.requirement_contract_sha256.as_deref()
                == Some(content_digest(&serde_json::to_vec(contract)?).as_str()),
            "recovery completion contract changed"
        );
        anyhow::ensure!(
            authority.source_assignment_sha256.as_deref()
                == Some(
                    content_digest(&serde_json::to_vec(&(
                        &source.workspace_access,
                        &source.writer_correction_assignment,
                    ))?)
                    .as_str()
                ),
            "recovery source assignment changed"
        );
        let output = source
            .output
            .as_ref()
            .context("recovery source output is missing")?;
        anyhow::ensure!(
            authority.source_output_sha256.as_deref()
                == Some(content_digest(&serde_json::to_vec(output)?).as_str()),
            "recovery source output changed"
        );
        anyhow::ensure!(
            authority.source_transcript_sha256.as_deref()
                == Some(
                    content_digest(&fs::read(
                        project_root.join(&area.path).join("transcript.jsonl")
                    )?)
                    .as_str()
                ),
            "recovery source transcript changed"
        );
    }
    Ok(())
}

pub(crate) fn validate_task_recovery_source(
    project_root: &Path,
    attempt: &Attempt,
    task: &crate::work_model::Task,
) -> Result<()> {
    for reference in task
        .input_artifacts
        .iter()
        .filter(|reference| reference.producer_id == PRODUCER)
    {
        anyhow::ensure!(
            attempt.artifacts.contains(reference),
            "Writer-completion recovery authority is not attached to the Attempt"
        );
        let bytes = fs::read(crate::recovery_contracts::resolve_input_artifact_path(
            project_root,
            &reference.path,
        )?)?;
        let authority: RecoveryAuthority =
            serde_json::from_slice(&bytes).context("parse Writer-completion recovery authority")?;
        anyhow::ensure!(
            bytes == serde_json::to_vec_pretty(&authority)?
                && authority.schema_version == SCHEMA_VERSION
                && authority.work_item_id == attempt.work_item_id
                && authority.attempt_id == attempt.id
                && authority.task_id == task.id
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
            "Writer-completion recovery authority changed"
        );
        let source = attempt
            .tasks
            .iter()
            .find(|source| source.id == authority.source_writer_task_id)
            .context("Writer-completion recovery source Writer is missing")?;
        let output = source
            .output
            .as_ref()
            .context("Writer-completion recovery source output is missing")?;
        let request = RecoveryOperationRequest::new(
            PRODUCER,
            &attempt.work_item_id,
            &attempt.id,
            Some(authority.candidate_commit.clone()),
            Some(if authority.source_matrix_sha256.is_some() {
                authority.source_writer_task_id.clone()
            } else {
                authority.task_id.clone()
            }),
            None,
            Some(output.workspace_path.clone()),
            Some(authority.resolution.clone()),
            Some(format!(
                "sha256:{:x}",
                Sha256::digest(serde_json::to_vec(&authority)?)
            )),
        );
        anyhow::ensure!(
            attempt
                .recovery_operations
                .iter()
                .any(|record| record.matches_request(&request)),
            "Writer-completion recovery operation changed"
        );
        validate_committed_source_evidence(project_root, attempt, source, &authority)?;
    }
    Ok(())
}

pub(crate) fn normalize_resolution(resolution: &str) -> Result<String> {
    let value = resolution.trim();
    if value.is_empty() {
        bail!("provide a non-empty --resolution for Writer-completion recovery");
    }
    if value.len() > MAX_RESOLUTION_BYTES {
        bail!("the Writer-completion recovery resolution is too long");
    }
    Ok(value.to_string())
}

pub(crate) fn recovery_instructions(resolution: &str, audit_artifact: &str) -> String {
    format!(
        "{INSTRUCTIONS_MARKER}. Retry this exact Writer Task after the prior run omitted its required no-change declaration. Preserve the candidate and existing history. If no source change is justified, write the required no-change.json with fresh focused verification. Operator authorization: {resolution}. Authority: {audit_artifact}"
    )
}

pub(crate) fn corrective_writer_instructions(resolution: &str, audit_artifact: &str) -> String {
    format!(
        "{INSTRUCTIONS_MARKER}. The completed source Writer has completion evidence that needs correction. Read the source matrix and immutable requirement contract, preserve the source Task and candidate history, and write a complete fresh matrix for this corrective Writer. If no source change is needed, use the verified no-change result. Operator authorization: {resolution}. Authority: {audit_artifact}"
    )
}

pub(crate) fn ensure_settled_ownership(
    project_root: &Path,
    attempt: &Attempt,
    starting_task: Option<&str>,
) -> Result<()> {
    if attempt
        .tasks
        .iter()
        .any(|task| task.status == crate::work_model::TaskStatus::Executing)
    {
        bail!("Attempt has a running Task");
    }
    for task in &attempt.tasks {
        if Some(task.id.as_str()) == starting_task {
            continue;
        }
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

#[cfg(test)]
mod retry_history_tests {
    use super::*;
    use crate::{recovery_contracts::context_retry, work_model::*};

    #[test]
    fn completed_retry_proves_only_its_superseded_reservation() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let mut item = WorkItem::planned("history", "Preserve retry history");
        item.add_initial_attempt("attempt-1").unwrap();
        let source = &mut item.attempts[0].tasks[0];
        source.status = TaskStatus::Complete;
        source.output = Some(TaskOutput {
            workspace_id: "candidate".into(),
            workspace_path: source.workspace_access.writes[0].path.clone(),
            source_branch: "main".into(),
            base_commit: None,
            commit: "a".repeat(40),
            no_change: None,
            learner_canonicalization: None,
        });
        let id = item.add_next_write_round("attempt-1", vec![]).unwrap();
        item.attempts[0].tasks[1].status = TaskStatus::Failed;
        item.add_next_write_round("attempt-1", vec![]).unwrap();
        let area = &item.attempts[0].tasks[1]
            .artifact_area
            .as_ref()
            .unwrap()
            .path;
        fs::create_dir_all(root.join(area)).unwrap();
        let baseline = WorkModelStore::new(root)
            .work_task_path("history", "attempt-1", &id)
            .unwrap()
            .with_extension("write-baseline");
        fs::create_dir_all(baseline.parent().unwrap()).unwrap();
        fs::write(&baseline, b"baseline").unwrap();
        let retry = context_retry::Retry {
            schema_version: 2,
            source: context_retry::RetrySource::InvalidCompletion,
            before: item.attempts[0].clone(),
            task_id: id,
            context: None,
            baseline_sha256: Some(content_digest(b"baseline")),
            files: Default::default(),
            resolution: "Retry this failed preparation".into(),
        };
        let reference = context_retry::audit_ref(&retry).unwrap();
        fs::create_dir_all(root.join(&reference.path).parent().unwrap()).unwrap();
        fs::write(
            root.join(&reference.path),
            serde_json::to_vec_pretty(&retry).unwrap(),
        )
        .unwrap();
        let attempt = &mut item.attempts[0];
        attempt.artifacts.push(reference);
        let approved = attempt
            .record_recovery_operation(
                &context_retry::request(&retry, context_retry::COMPLETION_APPROVE).unwrap(),
                None,
            )
            .unwrap()
            .completed_at
            .clone();
        for kind in [context_retry::RESUME, context_retry::START] {
            attempt
                .record_recovery_operation(&context_retry::request(&retry, kind).unwrap(), None)
                .unwrap();
        }
        attempt.tasks[1] = context_retry::prepared_task(&retry).unwrap();
        attempt.tasks[1].status = TaskStatus::Complete;
        attempt.tasks[1].output = attempt.tasks[0].output.clone();
        attempt.tasks[1].started_at = Some(approved.clone());
        attempt.tasks[1].completed_at = Some(approved.clone());
        attempt.tasks[2].status = TaskStatus::Superseded;
        attempt.tasks[2].completed_at = Some(approved);
        validate_terminal_superseded_history(root, attempt, "history").unwrap();
        let valid = attempt.clone();
        attempt.tasks[2].instructions = Some("changed reservation".into());
        assert!(validate_terminal_superseded_history(root, attempt, "history").is_err());
        *attempt = valid.clone();
        attempt.tasks[1].status = TaskStatus::Failed;
        assert!(validate_terminal_superseded_history(root, attempt, "history").is_err());
        *attempt = valid;
        fs::create_dir_all(root.join(&attempt.tasks[2].artifact_area.as_ref().unwrap().path))
            .unwrap();
        assert!(validate_terminal_superseded_history(root, attempt, "history").is_err());
    }
}
