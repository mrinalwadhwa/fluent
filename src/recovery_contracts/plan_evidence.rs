//! Interpret Plan acceptance for completion gates and ordinary Review alike.
use super::*;
use crate::plan_command::{plan_command_binding_matches, resolve_plan_row_command};
use crate::work_model::{
    BoundRecheckArtifact, PlanHostEvidenceBinding, WriterCompletionMatrix,
    WriterCompletionRequirementKind, WriterCompletionState, WriterVerificationClaim,
    WriterVerificationResult,
};

/// One accepted, candidate-bound host run of a Plan script. The caller still
/// decides whether its exact command matches an approved Test reference.
pub(crate) struct AcceptedPlanHostScriptProof {
    pub(crate) command: String,
    pub(crate) working_directory: String,
    pub(crate) bindings: Vec<BoundRecheckArtifact>,
}

pub(crate) fn accepted_plan_host_script_proofs(
    project_root: &Path,
    attempt: &work_model::Attempt,
    now: chrono::DateTime<chrono::Utc>,
) -> Vec<AcceptedPlanHostScriptProof> {
    let Some(writer) = attempt
        .tasks
        .iter()
        .rev()
        .find(|task| task.kind == TaskKind::Write && task.status == TaskStatus::Complete)
    else {
        return Vec::new();
    };
    let Some(source) = writer_completion_matrix_ref(project_root, attempt, writer) else {
        return Vec::new();
    };
    let Ok((_, bytes)) = bound_recovery_artifact(project_root, &source.path) else {
        return Vec::new();
    };
    let Ok(matrix) = serde_json::from_slice::<WriterCompletionMatrix>(&bytes) else {
        return Vec::new();
    };
    accepted_plan_host_evidence(project_root, attempt, writer, &source, &matrix, now)
        .into_iter()
        .filter_map(|(artifact, binding)| {
            let (binding_artifact, _) =
                bound_recovery_artifact(project_root, &artifact.path).ok()?;
            Some(AcceptedPlanHostScriptProof {
                command: binding.command,
                working_directory: binding.working_directory,
                bindings: vec![binding_artifact, binding.evidence],
            })
        })
        .collect()
}

pub(super) fn accepted_plan_host_evidence(
    project_root: &Path,
    attempt: &work_model::Attempt,
    writer: &Task,
    source_matrix: &ArtifactRef,
    matrix: &WriterCompletionMatrix,
    now: chrono::DateTime<chrono::Utc>,
) -> Vec<(ArtifactRef, PlanHostEvidenceBinding)> {
    let Some(output) = writer.output.as_ref() else {
        return Vec::new();
    };
    if writer_completion_matrix_ref(project_root, attempt, writer).as_ref() != Some(source_matrix) {
        return Vec::new();
    }
    let Some(latest) = latest_plan_host_binding_paths(project_root, attempt, &output.commit) else {
        return Vec::new();
    };
    let mut accepted = Vec::new();
    for artifact in attempt.artifacts.iter().filter(|artifact| {
        artifact.producer_id == "plan-host-evidence" && artifact.path.ends_with("/binding.json")
    }) {
        let Ok(binding_bytes) = fs::read(project_root.join(&artifact.path)) else {
            continue;
        };
        let Ok(binding) = serde_json::from_slice::<PlanHostEvidenceBinding>(&binding_bytes) else {
            continue;
        };
        if latest.get(&binding.plan_row) != Some(&artifact.path)
            || binding.schema_version != 1
            || binding.work_item_id != attempt.work_item_id
            || binding.attempt_id != attempt.id
            || binding.candidate_commit != output.commit
            || binding.result != EvidenceResult::Pass
        {
            continue;
        }
        let Some(original_writer) = attempt.tasks.iter().find(|task| {
            task.id == binding.writer_task_id
                && task.kind == TaskKind::Write
                && task.status == TaskStatus::Complete
                && task
                    .output
                    .as_ref()
                    .is_some_and(|original| original.commit == output.commit)
        }) else {
            continue;
        };
        let Some(original_source) =
            writer_completion_matrix_ref(project_root, attempt, original_writer)
        else {
            continue;
        };
        let source_matrix_value = match crate::recovery_contracts::plan_host_source_matrix(
            project_root,
            attempt,
            original_writer,
            &original_source,
            &binding.source_matrix,
            &binding.plan_row,
        ) {
            Ok(source) => source,
            Err(_) => continue,
        };
        let Some(source_row) = source_matrix_value
            .rows
            .iter()
            .find(|row| row.id == binding.plan_row)
        else {
            continue;
        };
        if source_row.kind != WriterCompletionRequirementKind::Plan
            || source_row.state != WriterCompletionState::Pending
            || source_row.requirement != binding.requirement
            || !plan_command_binding_matches(
                source_row,
                &binding.command,
                binding.command_source.as_deref(),
            )
        {
            continue;
        }
        let evidence_bytes = match fs::read(project_root.join(&binding.evidence.path)) {
            Ok(bytes) => bytes,
            Err(_) => continue,
        };
        if format!("sha256:{:x}", Sha256::digest(&evidence_bytes)) != binding.evidence.digest {
            continue;
        }
        let Ok(evidence) = parse_evidence_document(&evidence_bytes) else {
            continue;
        };
        if evidence.result != EvidenceResult::Pass
            || evidence.check != binding.command
            || evidence.working_directory != binding.working_directory
            || source_row
                .command
                .as_ref()
                .map(|command| &command.applicability)
                .cloned()
                .unwrap_or_default()
                != binding.applicability.definition
            || crate::recovery_contracts::validate_check_applicability_in_workspace_at(
                &project_root.join(&output.workspace_path),
                &binding.applicability,
                &evidence,
                now,
            )
            .is_err()
        {
            continue;
        }
        if !attempt.evidence_recoveries.iter().any(|recovery| {
            recovery.candidate_commit == binding.candidate_commit
                && recovery.attachment.snapshot_path == binding.evidence.path
                && recovery.attachment.digest == binding.evidence.digest
                && recovery.state == EvidenceRecoveryState::Complete
                && recovery.targets.iter().any(|target| {
                    target.role == "tests"
                        && target.prior_review_artifact == artifact.path
                        && crate::recovery_contracts::plan_evidence_review_is_accepted(
                            project_root,
                            attempt,
                            recovery,
                            target,
                            &binding,
                        )
                })
        }) {
            continue;
        }
        let Some(row) = matrix.rows.iter().find(|row| {
            row.id == binding.plan_row && row.kind == WriterCompletionRequirementKind::Plan
        }) else {
            continue;
        };
        if resolve_plan_row_command(row).as_deref() != Some(binding.command.as_str())
            || row.requirement != binding.requirement
            || row.command != source_row.command
            || row.verification_refs != source_row.verification_refs
        {
            continue;
        }
        accepted.push((artifact.clone(), binding));
    }
    accepted
}
/// Overlay accepted Plan proof in memory without rewriting Writer claims.
pub(crate) fn apply_completed_plan_host_evidence(
    project_root: &Path,
    attempt: &work_model::Attempt,
    writer: &Task,
    source_matrix: &ArtifactRef,
    matrix: &mut WriterCompletionMatrix,
) -> bool {
    let accepted = accepted_plan_host_evidence(
        project_root,
        attempt,
        writer,
        source_matrix,
        matrix,
        crate::check_applicability::evaluation_time(),
    );
    let mut changed = false;
    for (artifact, binding) in accepted {
        let row = matrix
            .rows
            .iter_mut()
            .find(|row| row.id == binding.plan_row)
            .expect("validated Plan row");
        if row.state != WriterCompletionState::Complete
            || row.implementation_evidence != vec![artifact.path.clone()]
            || row.verification
                != vec![WriterVerificationClaim {
                    command: binding.command.clone(),
                    result: WriterVerificationResult::Pass,
                }]
        {
            row.state = WriterCompletionState::Complete;
            row.implementation_evidence = vec![artifact.path.clone()];
            row.verification = vec![WriterVerificationClaim {
                command: binding.command.clone(),
                result: WriterVerificationResult::Pass,
            }];
            changed = true;
        }
    }
    changed
}
