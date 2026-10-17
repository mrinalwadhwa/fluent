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
    }))
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
