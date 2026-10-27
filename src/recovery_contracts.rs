#[cfg(test)]
use crate::verification_assessment::reconstruct_legacy_manifest;
pub(crate) use crate::verification_assessment::{
    assess_canonical_checks, bound_recovery_artifact, candidate_workspace_path,
    completed_review_check_commands, expired_completed_review_recovery, parse_evidence_document,
    review_artifact_path, validate_check_applicability, validate_check_applicability_at,
    validate_check_applicability_in_workspace_at, validate_managed_file_path,
    validate_required_check_binding,
};
// Shared admission and completion contracts for recovery-bound Writers.
//
// This boundary is intentionally independent from the host-evidence producer
// and task executor. Queue admission, direct execution, and host recovery all
// consume the same immutable checks without introducing a module cycle.

use anyhow::{Context, Result, bail};
use chrono::DateTime;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path, PathBuf};

use crate::candidate_gate;
use crate::git;
use crate::review;
use crate::work_model::{
    self, ArtifactRef, AttemptReviewState, AttemptStatus, EvidenceAttachment, EvidenceRecovery,
    EvidenceRecoveryState, EvidenceReviewTarget, HostEvidenceClassification, PauseKind,
    RoundCapExtension, RoundCapReviewArtifact, Task, TaskKind, TaskStatus, WorkModelError,
    WriterCompletionFinding, WriterCorrectionAssignment, WriterCorrectionAssignmentInput,
    WriterCorrectionAssignmentOrigin,
};

mod carried_finding;
pub(crate) mod claude_session;
mod plan_evidence;
pub(crate) use plan_evidence::apply_completed_plan_host_evidence;
pub(crate) mod context_retry;
pub(crate) mod review_coverage;
pub(crate) mod review_evidence;
pub(crate) mod writer_completion;
pub(crate) use carried_finding::{
    apply_accepted_carried_findings, carried_finding_review_passes,
    inspect_carried_finding_attachment,
};

pub(crate) const WRITER_COMPLETION_MATRIX_FILE: &str = "writer-completion.json";
const CODE_CHANGE_DISPOSITION: &str = "code-change";

/// Validate candidate and evidence facts for a continuation recognized by the
/// lifecycle layer. This boundary never invokes its lifecycle callers.
pub(crate) fn validate_interrupted_plan_host_recovery(
    project_root: &Path,
    attempt: &work_model::Attempt,
    row_id: &str,
    canceled: &Task,
) -> Result<()> {
    let (writer, _, matrix) = owned_host_completion_matrix(project_root, attempt)?
        .context("interrupted recovery requires approved host ownership")?;
    let contract = attempt
        .writer_completion_contract
        .as_ref()
        .expect("owned matrix has a contract");
    let findings = writer_completion_findings(project_root, &writer)?;
    anyhow::ensure!(
        matrix
            .status_against(contract, &writer.id, &findings)
            .map_err(anyhow::Error::msg)?
            .is_complete(),
        "Writer obligations remain incomplete"
    );
    anyhow::ensure!(
        matrix.rows.iter().any(|row| row.id == row_id
            && row.responsibility == Some(work_model::CompletionResponsibility::Host)
            && row.state == work_model::WriterCompletionState::Pending),
        "pending obligation is not approved host work"
    );
    let output = writer.output.as_ref().expect("validated source output");
    anyhow::ensure!(
        candidate_workspace_is_clean_at_managed(project_root, output, &output.commit),
        "interrupted host recovery needs the unchanged managed candidate"
    );
    let selected = writer_completion_matrix_ref(project_root, attempt, &writer)
        .context("source Writer has no matrix")?;
    let preserved = validated_ownership_approval(project_root, attempt)?
        .filter(|audit| audit.matrix_artifact == selected)
        .map(|audit| audit.preview.source_matrix)
        .unwrap_or(selected);
    anyhow::ensure!(
        canceled.input_artifacts.contains(&preserved),
        "canceled Writer does not name the preserved source completion evidence"
    );
    for task in &attempt.tasks {
        anyhow::ensure!(
            task.id == canceled.id
                || matches!(task.status, TaskStatus::Complete | TaskStatus::Superseded),
            "unrelated unresolved Task {}",
            task.id
        );
        anyhow::ensure!(
            !crate::lease::is_leased(&crate::lease::task_lock_path(
                project_root,
                &attempt.work_item_id,
                &task.id
            )),
            "Task {} has a live owner",
            task.id
        );
        if let Some(area) = &task.artifact_area {
            anyhow::ensure!(
                !crate::execution::execution_is_live(&crate::execution::record_path_for_artifact(
                    project_root,
                    &area.path
                ))?,
                "Task {} still owns a live execution",
                task.id
            );
        }
    }
    Ok(())
}

/// Revalidate active Plan evidence before foreground resume, queue admission,
/// scheduler claim, and Reviewer launch. Return whether every targeted Plan
/// review passed and its active receipt can settle without crediting a later Writer.
/// Legacy attachment semantics remain intact.
pub(crate) fn validate_owned_plan_evidence_review(
    project_root: &Path,
    attempt: &work_model::Attempt,
) -> Result<bool> {
    validate_owned_plan_evidence_review_at(
        project_root,
        attempt,
        crate::check_applicability::evaluation_time(),
        false,
    )
}

fn validate_owned_plan_evidence_review_at(
    project_root: &Path,
    attempt: &work_model::Attempt,
    now: chrono::DateTime<chrono::Utc>,
    settle_history: bool,
) -> Result<bool> {
    if let Some(writer) = attempt
        .tasks
        .iter()
        .rev()
        .find(|t| t.kind == TaskKind::Write)
        && attempt.evidence_recoveries.iter().any(|r| {
            r.carried_finding
                .as_ref()
                .is_some_and(|s| s.binding.writer_task_id == writer.id)
                && r.state == EvidenceRecoveryState::Complete
        })
    {
        let mut matrix = authoritative_writer_completion_matrix(project_root, attempt, writer)?
            .context("accepted finding source completion matrix is missing")?;
        apply_accepted_carried_findings(project_root, attempt, writer, &mut matrix)?;
    }
    let Some(recovery) = attempt
        .active_evidence_recovery()
        .filter(|recovery| recovery.state == EvidenceRecoveryState::Reviewing)
    else {
        return Ok(false);
    };
    if recovery.carried_finding.is_some() {
        anyhow::ensure!(
            !settle_history,
            "finding evidence cannot settle against a later candidate"
        );
        return carried_finding::carried_finding_review_passes_at(
            project_root,
            attempt,
            recovery,
            now,
        );
    }
    if let Some(check) = &recovery.required_check {
        validate_required_check_binding(project_root, attempt, check)?;
        let output = attempt
            .tasks
            .iter()
            .rev()
            .find(|task| task.kind == TaskKind::Write && task.status == TaskStatus::Complete)
            .and_then(|task| task.output.as_ref())
            .context("required check candidate is missing")?;
        let clean = if check.source.is_canonical_tester() {
            candidate_workspace_is_clean_at_managed(project_root, output, &check.candidate_commit)
        } else {
            candidate_workspace_is_clean_at(project_root, output, &check.candidate_commit)
        };
        anyhow::ensure!(
            clean,
            "required check candidate is changed, dirty, or no longer managed"
        );
        let (attachment, bytes) =
            bound_recovery_artifact(project_root, &recovery.attachment.snapshot_path)?;
        anyhow::ensure!(
            attachment.digest == recovery.attachment.digest,
            "required check evidence digest changed"
        );
        let document = parse_evidence_document(&bytes)?;
        validate_check_applicability_at(&check.applicability, &document, now)?;
        anyhow::ensure!(
            recovery.candidate_commit == check.candidate_commit
                && document.check == check.command
                && document.working_directory == check.working_directory,
            "required check result no longer matches its candidate or command"
        );
        anyhow::ensure!(
            recovery.targets.len() == 1,
            "required check needs one targeted Tests Review"
        );
        let target = &recovery.targets[0];
        let (prior, bytes) = bound_recovery_artifact(project_root, &target.prior_review_artifact)?;
        match &check.source {
            work_model::RequiredCheckSource::CanonicalTester { .. } => anyhow::ensure!(
                serde_json::from_slice::<work_model::RequiredCheck>(&bytes)? == *check
                    && prior.path
                        == snapshot_path(&attempt.work_item_id, &attempt.id, &prior.digest)?,
                "required check source snapshot digest changed"
            ),
            work_model::RequiredCheckSource::ReviewRequested { request, .. } => {
                anyhow::ensure!(prior == *request, "review request digest changed")
            }
        }
        let reviewer = attempt
            .tasks
            .iter()
            .find(|task| Some(task.id.as_str()) == target.review_task_id.as_deref())
            .context("required check reviewer is missing")?;
        let context = reviewer
            .evidence_review_context
            .as_ref()
            .context("required check review context is missing")?;
        anyhow::ensure!(
            target.role == "tests"
                && reviewer.kind == TaskKind::Review
                && reviewer.role == "tests"
                && context.recovery_id == recovery.id
                && context.candidate_commit == check.candidate_commit
                && context.attachment == recovery.attachment
                && context.prior_review_artifact == target.prior_review_artifact,
            "required check reviewer binding changed"
        );
    }
    validate_plan_evidence_recovery_at(project_root, attempt, recovery, now, settle_history)
}

fn validate_plan_evidence_recovery_at(
    project_root: &Path,
    attempt: &work_model::Attempt,
    recovery: &EvidenceRecovery,
    now: chrono::DateTime<chrono::Utc>,
    settle_history: bool,
) -> Result<bool> {
    let mut completed = !recovery.targets.is_empty();
    for target in &recovery.targets {
        if !attempt.artifacts.iter().any(|artifact| {
            artifact.producer_id == "plan-host-evidence"
                && artifact.path == target.prior_review_artifact
        }) {
            completed = false;
            continue;
        }
        let binding: work_model::PlanHostEvidenceBinding = serde_json::from_slice(&fs::read(
            resolve_input_artifact_path(project_root, &target.prior_review_artifact)?,
        )?)?;
        // Keep current candidate admission strict, but validate each receipt
        // against the Writer it actually reviewed, including completed history.
        let current_writer = attempt
            .tasks
            .iter()
            .rev()
            .find(|task| task.kind == TaskKind::Write && task.status == TaskStatus::Complete)
            .context("host evidence has no current Writer")?;
        let writer = attempt
            .tasks
            .iter()
            .find(|task| {
                task.id == binding.writer_task_id
                    && task.kind == TaskKind::Write
                    && task.status == TaskStatus::Complete
            })
            .context("host evidence source Writer disappeared")?;
        let source = writer_completion_matrix_ref(project_root, attempt, writer)
            .context("host evidence source matrix disappeared")?;
        let matrix = plan_host_source_matrix(
            project_root,
            attempt,
            writer,
            &source,
            &binding.source_matrix,
            &binding.plan_row,
        )?;
        let row = matrix
            .rows
            .iter()
            .find(|row| row.id == binding.plan_row)
            .context("host evidence row disappeared")?;
        if row.responsibility.is_none() {
            completed = false;
            continue;
        }
        anyhow::ensure!(
            row.responsibility == Some(work_model::CompletionResponsibility::Host),
            "evidence row is not host-owned"
        );
        for artifact in [&binding.source_matrix, &binding.evidence] {
            let bytes = fs::read(resolve_input_artifact_path(project_root, &artifact.path)?)?;
            anyhow::ensure!(
                format!("sha256:{:x}", Sha256::digest(&bytes)) == artifact.digest,
                "host evidence digest changed: {}",
                artifact.path
            );
        }
        let output = writer
            .output
            .as_ref()
            .context("host evidence source output disappeared")?;
        let command = row.command.as_ref().context("host command disappeared")?;
        // Explicit fresh attachment may settle a prior passing Review as history.
        // Resume and current acceptance still require the current candidate and row.
        if settle_history {
            anyhow::ensure!(
                current_writer
                    .output
                    .as_ref()
                    .is_some_and(|current| current.commit != output.commit),
                "historical Plan settlement requires a different current candidate"
            );
        } else {
            let Some((validated_writer, _, current_matrix)) =
                owned_host_completion_matrix(project_root, attempt)?
            else {
                completed = false;
                continue;
            };
            anyhow::ensure!(
                validated_writer.id == current_writer.id,
                "current host Writer changed during validation"
            );
            let current_row = current_matrix
                .rows
                .iter()
                .find(|current| current.id == row.id)
                .context("current host requirement disappeared")?;
            anyhow::ensure!(
                current_writer
                    .output
                    .as_ref()
                    .is_some_and(|output| output.commit == binding.candidate_commit)
                    && current_row.command == row.command
                    && current_row.requirement == row.requirement
                    && current_row.verification_refs == row.verification_refs,
                "current host requirement or candidate changed: {}",
                binding.plan_row
            );
        }
        anyhow::ensure!(
            binding.applicability.definition == command.applicability,
            "host check applicability changed"
        );
        anyhow::ensure!(
            binding.schema_version == 1
                && binding.work_item_id == attempt.work_item_id
                && binding.attempt_id == attempt.id
                && binding.writer_task_id == writer.id
                && binding.candidate_commit == output.commit
                && recovery.candidate_commit == output.commit
                && binding.requirement == row.requirement
                && binding.command == command.command
                && binding.working_directory == command.working_directory
                && binding.evidence.path == recovery.attachment.snapshot_path
                && binding.evidence.digest == recovery.attachment.digest,
            "host evidence no longer matches its candidate, row, command, or source"
        );
        let evidence = parse_evidence_document(&fs::read(resolve_input_artifact_path(
            project_root,
            &binding.evidence.path,
        )?)?)?;
        anyhow::ensure!(
            evidence.check == binding.command
                && evidence.result == binding.result
                && evidence.working_directory == binding.working_directory
                && evidence.run_at == binding.run_at,
            "host evidence document changed"
        );
        if settle_history {
            // Writer rounds reuse one managed candidate worktree, so its live
            // HEAD may now name a later candidate even though this receipt is
            // historical. The binding already records the source Worker's
            // resolved comparison commits; validate those immutable facts
            // without consulting the newer worktree's refs.
            crate::verification_assessment::validate_check_applicability_at(
                &binding.applicability,
                &evidence,
                now,
            )?;
        } else {
            validate_check_applicability_in_workspace_at(
                &candidate_workspace_path(project_root, current_writer.output.as_ref().unwrap()),
                &binding.applicability,
                &evidence,
                now,
            )?;
        }
        let reviewer = target
            .review_task_id
            .as_ref()
            .and_then(|id| attempt.tasks.iter().find(|task| task.id == *id))
            .context("targeted reviewer disappeared")?;
        let context = reviewer
            .evidence_review_context
            .as_ref()
            .context("targeted review lost its evidence binding")?;
        anyhow::ensure!(
            context.recovery_id == recovery.id
                && context.candidate_commit == output.commit
                && context.attachment == recovery.attachment
                && context.prior_review_artifact == target.prior_review_artifact,
            "targeted review binding changed"
        );
        completed &= binding.result == work_model::EvidenceResult::Pass
            && reviewer.kind == TaskKind::Review
            && reviewer.role == "tests"
            && reviewer.status == TaskStatus::Complete
            && reviewer.artifact_area.as_ref().is_some_and(|area| {
                fs::read_to_string(project_root.join(&area.path).join("review.md"))
                    .is_ok_and(|text| review::extract_verdict(&text) == review::Verdict::Pass)
            });
    }
    Ok(completed)
}

/// Record the exact passing Review bytes when Plan evidence settles.
/// Only fresh explicit attachment may settle an older candidate's completed
/// Review as history; resume always validates the current candidate and row.
pub(crate) fn complete_owned_plan_evidence_review(
    project_root: &Path,
    attempt: &mut work_model::Attempt,
    settle_history: bool,
) -> Result<bool> {
    if !validate_owned_plan_evidence_review_at(
        project_root,
        attempt,
        crate::check_applicability::evaluation_time(),
        settle_history,
    )? {
        return Ok(false);
    }
    let recovery = attempt
        .active_evidence_recovery()
        .context("validated Plan recovery disappeared")?;
    let recovery_id = recovery.id.clone();
    let digests = recovery
        .targets
        .iter()
        .map(|target| {
            let task = attempt
                .tasks
                .iter()
                .find(|task| Some(task.id.as_str()) == target.review_task_id.as_deref())
                .context("validated Plan reviewer disappeared")?;
            let path = review_artifact_path(task).context("Plan Review artifact is missing")?;
            let (bound, bytes) = bound_recovery_artifact(project_root, &path)?;
            anyhow::ensure!(
                target
                    .review_digest
                    .as_ref()
                    .is_none_or(|digest| *digest == bound.digest),
                "Plan Review digest changed"
            );
            anyhow::ensure!(
                review::extract_verdict(std::str::from_utf8(&bytes)?) == review::Verdict::Pass,
                "Plan Review no longer passes"
            );
            Ok(bound.digest)
        })
        .collect::<Result<Vec<_>>>()?;
    let recovery = attempt
        .evidence_recoveries
        .iter_mut()
        .find(|entry| entry.id == recovery_id)
        .unwrap();
    for (target, digest) in recovery.targets.iter_mut().zip(digests) {
        target.review_digest = Some(digest);
    }
    recovery.state = EvidenceRecoveryState::Complete;
    Ok(true)
}

/// Select the newest admitted result for each Plan obligation, including
/// unresolved failures. A malformed indexed binding cannot revive older proof.
pub(crate) fn latest_plan_host_binding_paths(
    project_root: &Path,
    attempt: &work_model::Attempt,
    candidate: &str,
) -> Option<std::collections::BTreeMap<String, String>> {
    let mut latest = std::collections::BTreeMap::new();
    for recovery in &attempt.evidence_recoveries {
        if recovery.candidate_commit != candidate {
            continue;
        }
        for target in &recovery.targets {
            if !attempt.artifacts.iter().any(|artifact| {
                artifact.producer_id == "plan-host-evidence"
                    && artifact.path == target.prior_review_artifact
                    && artifact.path.ends_with("/binding.json")
            }) {
                continue;
            }
            let (_, bytes) = crate::recovery_contracts::bound_recovery_artifact(
                project_root,
                &target.prior_review_artifact,
            )
            .ok()?;
            let binding: work_model::PlanHostEvidenceBinding =
                serde_json::from_slice(&bytes).ok()?;
            if binding.schema_version != 1
                || binding.work_item_id != attempt.work_item_id
                || binding.attempt_id != attempt.id
                || binding.candidate_commit != candidate
            {
                return None;
            }
            latest.insert(binding.plan_row, target.prior_review_artifact.clone());
        }
    }
    Some(latest)
}

/// Validate completed Plan review authority without changing its history.
pub(crate) fn plan_evidence_review_is_accepted(
    project_root: &Path,
    attempt: &work_model::Attempt,
    recovery: &EvidenceRecovery,
    target: &work_model::EvidenceReviewTarget,
    binding: &work_model::PlanHostEvidenceBinding,
) -> bool {
    let validate = || -> Result<()> {
        let task = attempt
            .tasks
            .iter()
            .find(|task| Some(task.id.as_str()) == target.review_task_id.as_deref())
            .context("Plan reviewer is missing")?;
        let context = task
            .evidence_review_context
            .as_ref()
            .context("Plan review context is missing")?;
        anyhow::ensure!(
            task.kind == TaskKind::Review
                && task.role == "tests"
                && task.status == TaskStatus::Complete
                && context.recovery_id == recovery.id
                && context.candidate_commit == binding.candidate_commit
                && context.attachment == recovery.attachment
                && context.prior_review_artifact == target.prior_review_artifact,
            "Plan review binding changed"
        );
        let (bound, bytes) = bound_recovery_artifact(
            project_root,
            &review_artifact_path(task).context("Plan Review artifact is missing")?,
        )?;
        anyhow::ensure!(
            match &target.review_digest {
                Some(digest) => *digest == bound.digest,
                // Older default-code Plan receipts did not record a review digest.
                // Policy-bearing receipts require complete proof from their reader.
                None => binding.applicability.is_default(),
            },
            "accepted Plan Review proof is missing or changed"
        );
        anyhow::ensure!(
            review::extract_verdict(std::str::from_utf8(&bytes)?) == review::Verdict::Pass,
            "accepted Plan Review no longer passes"
        );
        Ok(())
    };
    validate().is_ok()
}

/// Verify the candidate and stopped owners before an operator adopts a saved
/// responsibility split. Approval itself never retries a Writer.
pub(crate) fn inspect_ownership_adoption(
    project_root: &Path,
    item: &work_model::WorkItem,
    attempt_id: &str,
) -> Result<()> {
    let attempt = item
        .attempts
        .last()
        .filter(|attempt| attempt.id == attempt_id)
        .context("ownership adoption requires the current Attempt")?;
    anyhow::ensure!(
        attempt.status == work_model::AttemptStatus::NeedsUser
            && matches!(
                attempt.pause_kind,
                Some(work_model::PauseKind::WriterCompletion | work_model::PauseKind::Interrupted)
            ),
        "ownership adoption requires a Writer-completion or interrupted pause"
    );
    anyhow::ensure!(
        !item
            .merge_candidates
            .iter()
            .any(|candidate| candidate.attempt_id == attempt_id),
        "ownership adoption is ineligible after a Merge Candidate exists"
    );
    for task in &attempt.tasks {
        anyhow::ensure!(
            task.status != TaskStatus::Executing,
            "Task {} is executing",
            task.id
        );
        anyhow::ensure!(
            !crate::lease::is_leased(&crate::lease::task_lock_path(
                project_root,
                &item.id,
                &task.id
            )),
            "Task {} has a live owner",
            task.id
        );
        if let Some(area) = &task.artifact_area {
            anyhow::ensure!(
                !crate::execution::execution_is_live(&crate::execution::record_path_for_artifact(
                    project_root,
                    &area.path
                ))?,
                "Task {} has a live execution owner",
                task.id
            );
        }
        anyhow::ensure!(
            matches!(task.status, TaskStatus::Complete | TaskStatus::Superseded)
                || (attempt.pause_kind == Some(work_model::PauseKind::Interrupted)
                    && task.kind == TaskKind::Write
                    && task.status == TaskStatus::NeedsUser
                    && task.output.is_none()),
            "Task {} has an incompatible lifecycle state",
            task.id
        );
    }
    let output = attempt
        .tasks
        .iter()
        .rev()
        .find(|task| task.kind == TaskKind::Write && task.status == TaskStatus::Complete)
        .and_then(|task| task.output.as_ref())
        .context("ownership adoption needs a completed source candidate")?;
    anyhow::ensure!(
        candidate_workspace_is_clean_at(project_root, output, &output.commit),
        "ownership adoption requires the unchanged clean candidate {}",
        output.commit
    );
    Ok(())
}

/// Read the approved host obligations against the exact clean Writer candidate.
/// Consumers may overlay accepted evidence, but the Writer cannot supply it.
pub(crate) fn owned_host_completion_matrix(
    project_root: &Path,
    attempt: &work_model::Attempt,
) -> Result<Option<(Task, ArtifactRef, work_model::WriterCompletionMatrix)>> {
    let Some(contract) = attempt
        .writer_completion_contract
        .as_ref()
        .filter(|contract| {
            contract
                .requirements
                .iter()
                .any(|row| row.responsibility == Some(work_model::CompletionResponsibility::Host))
        })
    else {
        return Ok(None);
    };
    let writer = attempt
        .tasks
        .iter()
        .rev()
        .find(|task| task.kind == TaskKind::Write && task.status == TaskStatus::Complete)
        .context("required host verification has no completed Writer")?;
    let output = writer
        .output
        .as_ref()
        .context("required host verification has no candidate")?;
    anyhow::ensure!(
        candidate_workspace_is_clean_at(project_root, output, &output.commit),
        "required host verification needs the unchanged clean candidate {}",
        output.commit
    );
    let artifact = writer_completion_matrix_ref(project_root, attempt, writer)
        .context("required host verification has no source matrix")?;
    let bytes = fs::read(resolve_input_artifact_path(project_root, &artifact.path)?)?;
    anyhow::ensure!(
        bytes.len() <= work_model::WRITER_COMPLETION_MATRIX_MAX_BYTES,
        "completion matrix exceeds size limit"
    );
    let mut matrix: work_model::WriterCompletionMatrix = serde_json::from_slice(&bytes)?;
    if let Some(audit) = validated_ownership_approval(project_root, attempt)?
        && artifact.producer_id == "ownership-recovery"
    {
        anyhow::ensure!(
            audit.matrix_artifact == artifact
                && audit.preview.candidate_commit == output.commit
                && audit.preview.writer_task_id == writer.id,
            "ownership mapping no longer names this Writer candidate"
        );
    }
    // An approved Approach delta leaves the original host proof bound to the
    // historical matrix. Validate those exact bytes before projecting current rows.
    let mut source_contract = contract.clone();
    for audit_ref in attempt.artifacts.iter().rev().filter(|entry| {
        entry.producer_id == "planning-approach-recovery" && entry.path.ends_with("/revision.json")
    }) {
        let revision: work_model::ApproachRevisionAudit = serde_json::from_slice(&fs::read(
            resolve_input_artifact_path(project_root, &audit_ref.path)?,
        )?)?;
        if let Some(source) = revision
            .source_completion
            .filter(|source| source.artifact == artifact)
        {
            anyhow::ensure!(
                format!("{:x}", Sha256::digest(&bytes)) == source.sha256,
                "approved Approach source completion evidence changed"
            );
            source_contract = source.contract;
            break;
        }
    }
    let findings = writer_completion_findings(project_root, writer)?;
    matrix
        .status_against(&source_contract, &writer.id, &findings)
        .map_err(anyhow::Error::msg)?;
    if &source_contract != contract {
        matrix = work_model::WriterCompletionMatrix::skeleton(
            contract,
            &writer.id,
            &findings,
            Some(&matrix),
        )
        .map_err(anyhow::Error::msg)?;
    }
    matrix
        .repair_writer_claims_against(contract, &writer.id, &findings)
        .map_err(anyhow::Error::msg)?;
    Ok(Some((writer.clone(), artifact, matrix)))
}

/// Recheck the immutable approval before any consumer uses its completion.
pub(crate) fn validated_ownership_approval(
    project_root: &Path,
    attempt: &work_model::Attempt,
) -> Result<Option<work_model::OwnershipApproval>> {
    let Some(audit_ref) = attempt.artifacts.iter().rev().find(|artifact| {
        artifact.producer_id == "ownership-recovery" && artifact.path.ends_with("/approval.json")
    }) else {
        validate_planning_revisions(project_root, attempt, None, 0)?;
        return Ok(None);
    };
    let audit: work_model::OwnershipApproval = serde_json::from_slice(&fs::read(
        resolve_input_artifact_path(project_root, &audit_ref.path)?,
    )?)?;
    let preview = &audit.preview;
    anyhow::ensure!(
        preview.work_item_id == attempt.work_item_id && preview.attempt_id == attempt.id,
        "ownership approval belongs to another Attempt"
    );
    let source_bytes = fs::read(resolve_input_artifact_path(
        project_root,
        &preview.source_matrix.path,
    )?)?;
    anyhow::ensure!(
        format!("sha256:{:x}", Sha256::digest(&source_bytes)) == preview.source_matrix_digest,
        "approved source completion evidence changed"
    );
    let source: work_model::WriterCompletionMatrix = serde_json::from_slice(&source_bytes)?;
    validate_preserved_completion_repair(project_root, attempt, &preview.source_matrix)?;
    let (approved_contract, approved_matrix) = preview
        .proposal
        .apply(&preview.original_contract, &source)
        .map_err(anyhow::Error::msg)?;
    let adopted: work_model::WriterCompletionMatrix = serde_json::from_slice(&fs::read(
        resolve_input_artifact_path(project_root, &audit.matrix_artifact.path)?,
    )?)?;
    anyhow::ensure!(
        adopted == approved_matrix,
        "approved completion mapping changed"
    );
    let writer = attempt
        .tasks
        .iter()
        .find(|task| task.id == preview.writer_task_id)
        .context("approved source Writer disappeared")?;
    anyhow::ensure!(
        source.task_id == writer.id
            && adopted.task_id == writer.id
            && writer
                .output
                .as_ref()
                .is_some_and(|output| output.commit == preview.candidate_commit),
        "ownership approval no longer names its source Writer candidate"
    );
    let start = attempt
        .artifacts
        .iter()
        .position(|artifact| artifact == audit_ref)
        .expect("registered ownership approval")
        + 1;
    validate_planning_revisions(project_root, attempt, Some(approved_contract), start)?;
    Ok(Some(audit))
}

/// Check all recorded planning deltas before admitting another revision.
pub(crate) fn validate_planning_completion_history(
    project_root: &Path,
    attempt: &work_model::Attempt,
) -> Result<()> {
    validated_ownership_approval(project_root, attempt).map(|_| ())
}

/// Validate planning history and every published round-cap Approach binding
/// before foreground or queued execution can advance the Attempt.
pub(crate) fn validate_planning_resume(
    project_root: &Path,
    item: &work_model::WorkItem,
    attempt: &work_model::Attempt,
) -> Result<bool> {
    validate_planning_resume_at(project_root, item, attempt, false)
}

/// Validate planning recovery while queue admission preserves a settled
/// cancellation authority for the runner to consume.
pub(crate) fn validate_planning_resume_for_queue(
    project_root: &Path,
    item: &work_model::WorkItem,
    attempt: &work_model::Attempt,
) -> Result<bool> {
    validate_planning_resume_at(project_root, item, attempt, true)
}

fn validate_planning_resume_at(
    project_root: &Path,
    item: &work_model::WorkItem,
    attempt: &work_model::Attempt,
    allow_settled_cancellation: bool,
) -> Result<bool> {
    validate_planning_completion_history(project_root, attempt)?;
    // Completed Plan-host receipts predate this Approach-revision replay
    // contract. Keep ordinary host-evidence recovery on its existing route;
    // replay the stricter historical receipt only when this Attempt records a
    // planning delta that can have changed its completion authority.
    if attempt.artifacts.iter().any(|artifact| {
        matches!(
            artifact.producer_id.as_str(),
            "planning-reference-recovery" | "planning-approach-recovery"
        ) && artifact.path.ends_with("/revision.json")
    }) {
        validate_completed_plan_evidence(project_root, attempt)?;
    }
    if attempt.status == AttemptStatus::Complete
        && attempt.review_state == Some(AttemptReviewState::Passed)
        && attempt
            .tasks
            .iter()
            .any(|task| task.role == "approach-revision-correction")
        && attempt
            .learning
            .as_ref()
            .is_some_and(|learning| learning.is_succeeded())
    {
        validate_completed_downstream_evidence(project_root, attempt)?;
    }
    // Completion-history replay above validates every accepted revision against
    // the contract that preceded it. The strict retry validator, in contrast,
    // applies only to the current non-terminal corrective Writer. Reapplying it
    // to an older audit would compare that historical revision with the latest
    // composed planning contract after a later revision and reject valid history.
    let Some(audit_artifact) = attempt
        .tasks
        .iter()
        .rev()
        .filter(|task| {
            task.kind == TaskKind::Write
                && task.role == "approach-revision-correction"
                && !matches!(task.status, TaskStatus::Complete | TaskStatus::Superseded)
        })
        .flat_map(|task| task.input_artifacts.iter())
        .filter(|artifact| {
            artifact.producer_id == "planning-approach-recovery"
                && artifact.path.ends_with("/revision.json")
        })
        .next()
    else {
        return Ok(false);
    };
    let bytes = registered_approach_revision_audit_bytes(project_root, attempt, audit_artifact)?;
    let revision: work_model::ApproachRevisionAudit = serde_json::from_slice(&bytes)?;
    if revision.revision_kind != work_model::ApproachRevisionKind::FailedReviewRoundCap {
        return Ok(false);
    }

    validate_round_cap_approach_revision_retry(
        project_root,
        item,
        attempt,
        &revision,
        audit_artifact,
        allow_settled_cancellation,
    )?;
    Ok(attempt.status == AttemptStatus::Planned
        && attempt.tasks.iter().any(|task| {
            task.kind == TaskKind::Write
                && task.role == "approach-revision-correction"
                && task.status == TaskStatus::Planned
                && task.started_at.is_none()
                && task.output.is_none()
                && task.input_artifacts.contains(audit_artifact)
        }))
}

/// Revalidate the downstream proof before a completed Approach-recovery Attempt
/// can be exposed as ready again. Completion status and the Learner state are
/// model authority, but their referenced Review, Tester, candidate-gate, and
/// handoff files remain managed inputs that may have changed since the last
/// execution.
pub(crate) fn validate_completed_downstream_evidence(
    project_root: &Path,
    attempt: &work_model::Attempt,
) -> Result<()> {
    let writer_index = attempt
        .tasks
        .iter()
        .rposition(|task| task.kind == TaskKind::Write && task.status == TaskStatus::Complete)
        .context("completed Approach recovery has no Writer")?;
    let writer = &attempt.tasks[writer_index];
    let output = writer
        .output
        .as_ref()
        .context("completed Approach recovery Writer has no candidate")?;

    // Reuse advancement's effective ordinary coverage, including unchanged
    // domains carried across Writers and replacement full-domain verdicts.
    // Earlier failures and targeted Reviews remain immutable history; the
    // canonical assessment below separately revalidates accepted check proof.
    let reviews = review_coverage::ordinary_review_tasks(project_root, attempt);
    anyhow::ensure!(
        review::REVIEWERS
            .iter()
            .all(|role| reviews.iter().any(|(task, _)| task.role == *role)),
        "completed Approach recovery lacks current ordinary Review evidence"
    );
    for (review_task, verdict) in reviews {
        let artifact = review_artifact_path(review_task)
            .with_context(|| format!("completed Review Task {} has no artifact", review_task.id))?;
        anyhow::ensure!(
            verdict == review::Verdict::Pass,
            "completed review {} changed or no longer passes",
            artifact
        );
    }

    let tester = attempt.tasks[writer_index + 1..]
        .iter()
        .rev()
        .find(|task| {
            task.kind == TaskKind::Tester
                && task.status == TaskStatus::Complete
                && !task.is_failed_command_recheck()
        })
        .context("completed Approach recovery has no final Tester")?;
    let tester_area = tester
        .artifact_area
        .as_ref()
        .with_context(|| format!("final Tester Task {} has no artifact area", tester.id))?;
    let tester_results_ref = ArtifactRef {
        producer_id: tester.id.clone(),
        path: format!("{}/tester-results.json", tester_area.path),
    };
    let tester_results_path = resolve_input_artifact_path(project_root, &tester_results_ref.path)?;
    let tester_bytes = fs::read(&tester_results_path).with_context(|| {
        format!(
            "read final Tester evidence {}",
            tester_results_path.display()
        )
    })?;
    let tester_results: crate::tester::TesterResults = serde_json::from_slice(&tester_bytes)
        .with_context(|| {
            format!(
                "parse final Tester evidence {}",
                tester_results_path.display()
            )
        })?;
    anyhow::ensure!(
        tester_results.candidate_commit.as_deref() == Some(output.commit.as_str()),
        "final Tester evidence is not bound to candidate {}",
        output.commit
    );
    anyhow::ensure!(
        tester_results.error.is_none(),
        "final Tester evidence records a harness error"
    );
    let assessment = assess_canonical_checks(project_root, attempt)
        .context("completed final Tester verification is unverifiable")?;
    if !assessment.is_satisfied() {
        let mut reasons = assessment.blockers.clone();
        reasons.extend(assessment.diagnoses.iter().cloned());
        reasons.extend(
            assessment
                .defects
                .iter()
                .map(|defect| defect.diagnostic.clone()),
        );
        reasons.extend(assessment.outstanding.iter().map(|index| {
            let check = &assessment.checks[*index];
            format!(
                "Required check {} remains unresolved: `{}`",
                check.id, check.command
            )
        }));
        if reasons.is_empty() {
            reasons.push("canonical verification has no satisfied assessment".into());
        }
        anyhow::bail!(
            "completed final Tester verification remains unresolved: {}",
            reasons.join("; ")
        );
    }

    let gate_path = format!(
        "{}/{}/{}",
        work_model::work_artifact_path(&attempt.work_item_id, &attempt.id, "candidate-gates"),
        tester.id,
        crate::candidate_gate::CANDIDATE_GATE_FILE
    );
    let gate_ref = attempt
        .artifacts
        .iter()
        .rev()
        .find(|artifact| artifact.path == gate_path)
        .with_context(|| format!("completed final Tester gate {} is not recorded", gate_path))?;
    let gate_path = resolve_input_artifact_path(project_root, &gate_ref.path)?;
    let gate_bytes = fs::read(&gate_path)
        .with_context(|| format!("read final Tester candidate gate {}", gate_path.display()))?;
    let gate: crate::candidate_gate::CandidateGateEvidence = serde_json::from_slice(&gate_bytes)
        .with_context(|| format!("parse final Tester candidate gate {}", gate_path.display()))?;
    anyhow::ensure!(
        gate.schema_version == crate::candidate_gate::CANDIDATE_GATE_SCHEMA_VERSION
            && gate.phase == crate::candidate_gate::CandidateGatePhase::FinalTester
            && gate.disposition == crate::candidate_gate::CandidateGateDisposition::Passed,
        "final Tester candidate gate is not a passing current gate"
    );
    anyhow::ensure!(
        gate.base_commit == output.commit && gate.candidate_commit == output.commit,
        "final Tester candidate gate is not bound to candidate {}",
        output.commit
    );
    anyhow::ensure!(
        gate.tester_results_path.as_deref() == Some(tester_results_ref.path.as_str()),
        "final Tester candidate gate names different Tester evidence"
    );
    anyhow::ensure!(
        gate.checks.iter().all(|check| {
            !matches!(
                check.status,
                crate::candidate_gate::CandidateCheckStatus::Failed
                    | crate::candidate_gate::CandidateCheckStatus::Error
            )
        }),
        "final Tester candidate gate contains a failed or errored check"
    );

    let learning = attempt
        .learning
        .as_ref()
        .context("completed Approach recovery has no Learning state")?;
    anyhow::ensure!(
        learning.is_succeeded(),
        "completed Approach recovery Learning has not succeeded"
    );
    let handoff = learning
        .handoff
        .as_ref()
        .context("succeeded Learning has no handoff evidence")?;
    crate::learner::load_verified_handoff(project_root, handoff)
        .with_context(|| "completed Learner handoff evidence changed".to_string())?;
    Ok(())
}

/// Revalidate every accepted structured Plan-host receipt before any resume
/// route advances the Attempt. Active receipts take the normal settlement path,
/// while completed receipts remain immutable evidence that may either bind the
/// current Writer or document an earlier candidate in the same Attempt.
fn validate_completed_plan_evidence(
    project_root: &Path,
    attempt: &work_model::Attempt,
) -> Result<()> {
    let current_candidate = attempt
        .tasks
        .iter()
        .rev()
        .find(|task| task.kind == TaskKind::Write && task.status == TaskStatus::Complete)
        .and_then(|task| task.output.as_ref())
        .map(|output| output.commit.as_str());

    let recoveries = attempt
        .evidence_recoveries
        .iter()
        .filter(|recovery| {
            recovery.state == EvidenceRecoveryState::Complete
                && recovery.required_check.is_none()
                && (recovery.id.starts_with("plan-host-evidence-")
                    || recovery.targets.iter().any(|target| {
                        attempt.artifacts.iter().any(|artifact| {
                            artifact.producer_id == "plan-host-evidence"
                                && artifact.path == target.prior_review_artifact
                        })
                    }))
        })
        .cloned()
        .collect::<Vec<_>>();

    for recovery in recoveries {
        let historical = current_candidate != Some(recovery.candidate_commit.as_str());
        let mut replay = attempt.clone();
        for entry in &mut replay.evidence_recoveries {
            if entry.id == recovery.id {
                entry.state = EvidenceRecoveryState::Reviewing;
            } else if entry.is_active() {
                // The copied Attempt may still contain a newer active frontier.
                // Isolate this historical receipt so the shared validator reads
                // the recovery selected by this replay pass.
                entry.state = EvidenceRecoveryState::Complete;
            }
        }
        anyhow::ensure!(
            validate_owned_plan_evidence_review_at(
                project_root,
                &replay,
                crate::check_applicability::evaluation_time(),
                historical,
            )?,
            "completed Plan host evidence {} is not accepted",
            recovery.id
        );
    }
    Ok(())
}

/// Revalidate planning approvals for adopted and explicitly owned Attempts.
fn registered_approach_revision_audit_bytes(
    project_root: &Path,
    attempt: &work_model::Attempt,
    audit_artifact: &ArtifactRef,
) -> Result<Vec<u8>> {
    let registration_count = attempt
        .artifacts
        .iter()
        .filter(|registered| *registered == audit_artifact)
        .count();
    anyhow::ensure!(
        registration_count == 1,
        "Approach revision audit is not registered exactly once on the Attempt"
    );
    let path = resolve_input_artifact_path(project_root, &audit_artifact.path)?;
    let bytes = fs::read(&path)
        .with_context(|| format!("read registered Approach revision audit {}", path.display()))?;
    let digest = format!("{:x}", Sha256::digest(&bytes));
    let operation_count = attempt
        .recovery_operations
        .iter()
        .filter(|operation| {
            operation.operation_kind == "planning-approach-revision"
                && operation.work_item_id == attempt.work_item_id
                && operation.attempt_id == attempt.id
                && operation.input_digest.as_deref() == Some(digest.as_str())
        })
        .count();
    anyhow::ensure!(
        operation_count == 1,
        "Approach revision audit is not bound to one recorded recovery operation"
    );
    Ok(bytes)
}

fn validate_approach_revision_operation_binding(
    attempt: &work_model::Attempt,
    revision: &work_model::ApproachRevisionAudit,
    audit_digest: &str,
) -> Result<()> {
    let request = work_model::RecoveryOperationRequest::new(
        "planning-approach-revision",
        &attempt.work_item_id,
        &attempt.id,
        Some(revision.candidate_commit.clone()),
        Some(revision.source_review_task_id.clone()),
        None,
        None,
        Some(revision.resolution.clone()),
        Some(audit_digest.to_string()),
    );
    let matches = attempt
        .recovery_operations
        .iter()
        .filter(|operation| {
            operation.operation_id == request.operation_id()
                && operation.operation_kind == request.operation_kind
                && operation.work_item_id == request.work_item_id
                && operation.attempt_id == request.attempt_id
                && operation.candidate_commit == request.candidate_commit
                && operation.task_id == request.task_id
                && operation.authorization == request.authorization
                && operation.authorization_digest == request.authorization_digest()
                && operation.input_digest == request.input_digest
        })
        .count();
    anyhow::ensure!(
        matches == 1,
        "Approach revision audit is not bound to its exact recovery authorization"
    );
    Ok(())
}

fn validate_planning_revisions(
    project_root: &Path,
    attempt: &work_model::Attempt,
    mut contract: Option<work_model::WriterCompletionContract>,
    start: usize,
) -> Result<()> {
    for artifact in attempt.artifacts.iter().skip(start).filter(|artifact| {
        matches!(
            artifact.producer_id.as_str(),
            "planning-reference-recovery" | "planning-approach-recovery"
        ) && artifact.path.ends_with("/revision.json")
    }) {
        if artifact.producer_id == "planning-approach-recovery" {
            let bytes = registered_approach_revision_audit_bytes(project_root, attempt, artifact)?;
            let revision: work_model::ApproachRevisionAudit = serde_json::from_slice(&bytes)?;
            if let Some(prior) = &revision.prior_contract {
                if let Some(expected) = &contract {
                    anyhow::ensure!(
                        expected == prior,
                        "approved Approach source contract changed"
                    );
                } else {
                    contract = Some(prior.clone());
                }
            }
            let Some(prior) = contract.as_ref() else {
                // Legacy unowned revisions predate captured source contracts.
                continue;
            };
            let digest = format!("{:x}", Sha256::digest(&bytes));
            let expected_schema = if revision.revision_kind
                == work_model::ApproachRevisionKind::FailedReviewRoundCap
            {
                2
            } else {
                1
            };
            anyhow::ensure!(
                revision.schema_version == expected_schema
                    && revision.work_item_id == attempt.work_item_id
                    && revision.attempt_id == attempt.id
                    && !revision.resolution.trim().is_empty()
                    && format!("{:x}", Sha256::digest(revision.resolution.as_bytes()))
                        == revision.resolution_sha256
                    && format!("{:x}", Sha256::digest(revision.prior_approach.as_bytes()))
                        == revision.prior_approach_sha256
                    && format!(
                        "{:x}",
                        Sha256::digest(revision.replacement_approach.as_bytes())
                    ) == revision.replacement_approach_sha256,
                "Approach revision has invalid recorded approval bytes"
            );
            validate_approach_revision_operation_binding(attempt, &revision, &digest)?;
            anyhow::ensure!(
                attempt
                    .tasks
                    .iter()
                    .any(|task| task.id == revision.source_review_task_id
                        && task.kind == TaskKind::Review
                        && task.role == "architecture"
                        && task.review_context.as_ref().is_some_and(|context| context
                            .candidate_commit
                            == revision.candidate_commit)),
                "approved Approach source review candidate changed"
            );
            if revision.revision_kind == work_model::ApproachRevisionKind::FailedReviewRoundCap {
                // Replay every failed-review round-cap audit through the
                // bound-frontier proof. Do not compare this historical
                // extension with the latest planning contract: later
                // revisions may have composed a different current contract.
                attempt
                    .validate_round_cap_extensions_for_approach_revision()
                    .map_err(anyhow::Error::msg)?;
                let extension = revision
                    .round_cap_extension
                    .as_ref()
                    .context("round-cap Approach revision has no extension binding")?;
                extension
                    .validate_human_resolution()
                    .map_err(anyhow::Error::msg)?;
                anyhow::ensure!(
                    extension.candidate_commit == revision.candidate_commit
                        && attempt
                            .round_cap_extensions
                            .iter()
                            .any(|recorded| recorded == extension),
                    "round-cap Approach revision lost its exact extension authorization"
                );
                anyhow::ensure!(
                    !revision.supporting_reviews.is_empty(),
                    "round-cap Approach revision has no supporting review bindings"
                );
                let review_tasks = revision
                    .supporting_reviews
                    .iter()
                    .map(|supporting| {
                        attempt
                            .tasks
                            .iter()
                            .find(|task| task.id == supporting.artifact.producer_id)
                            .with_context(|| {
                                format!(
                                    "Approach supporting review Task {} changed",
                                    supporting.artifact.producer_id
                                )
                            })
                    })
                    .collect::<Result<Vec<_>>>()?;
                let bindings = validate_round_cap_review_bindings(
                    project_root,
                    attempt,
                    &revision.candidate_commit,
                    &review_tasks,
                    Some(&revision.supporting_reviews),
                    Some(&revision.source_review_task_id),
                )?;
                anyhow::ensure!(
                    bindings.failed_reviews == extension.failed_reviews,
                    "round-cap Approach review evidence no longer matches its authorization"
                );
            }
            if let Some(source) = &revision.source_completion {
                anyhow::ensure!(
                    &source.contract == prior,
                    "approved Approach source contract changed"
                );
                let bytes = fs::read(resolve_input_artifact_path(
                    project_root,
                    &source.artifact.path,
                )?)?;
                anyhow::ensure!(
                    format!("{:x}", Sha256::digest(&bytes)) == source.sha256,
                    "approved Approach source completion evidence changed"
                );
                let matrix: work_model::WriterCompletionMatrix = serde_json::from_slice(&bytes)?;
                let writer = attempt
                    .tasks
                    .iter()
                    .find(|task| {
                        task.id == matrix.task_id
                            && task.kind == TaskKind::Write
                            && task
                                .output
                                .as_ref()
                                .is_some_and(|output| output.commit == revision.candidate_commit)
                    })
                    .context("approved Approach source Writer candidate changed")?;
                let findings = writer_completion_findings(project_root, writer)?;
                matrix
                    .status_against(prior, &writer.id, &findings)
                    .map_err(anyhow::Error::msg)?;
            }
            contract = Some(
                prior
                    .with_revised_approach(&revision.prior_approach, &revision.replacement_approach)
                    .map_err(anyhow::Error::msg)?,
            );
            continue;
        }
        let revision: work_model::TestReferenceRevisionAudit = serde_json::from_slice(&fs::read(
            resolve_input_artifact_path(project_root, &artifact.path)?,
        )?)?;
        if let Some(source) = &revision.source_completion {
            if let Some(expected) = &contract {
                anyhow::ensure!(
                    expected == &source.contract,
                    "approved planning source contract changed"
                );
            } else {
                contract = Some(source.contract.clone());
            }
            let bytes = fs::read(resolve_input_artifact_path(
                project_root,
                &source.artifact.path,
            )?)?;
            anyhow::ensure!(
                format!("{:x}", Sha256::digest(&bytes)) == source.sha256,
                "approved planning source completion evidence changed"
            );
            let matrix: work_model::WriterCompletionMatrix = serde_json::from_slice(&bytes)?;
            let writer = attempt
                .tasks
                .iter()
                .find(|task| {
                    task.id == matrix.task_id
                        && task.kind == TaskKind::Write
                        && task
                            .output
                            .as_ref()
                            .is_some_and(|output| output.commit == revision.candidate_commit)
                })
                .context("approved planning source Writer candidate changed")?;
            let findings = writer_completion_findings(project_root, writer)?;
            matrix
                .status_against(&source.contract, &writer.id, &findings)
                .map_err(anyhow::Error::msg)?;
        }
        let Some(approved_contract) = contract.as_mut() else {
            // Older unowned recoveries have no captured source contract.
            continue;
        };
        let revision_digest = format!("{:x}", Sha256::digest(serde_json::to_vec(&revision)?));
        anyhow::ensure!(
            revision.schema_version == 1
                && revision.work_item_id == attempt.work_item_id
                && revision.attempt_id == attempt.id
                && !revision.resolution.trim().is_empty()
                && format!("{:x}", Sha256::digest(revision.resolution.as_bytes()))
                    == revision.resolution_sha256
                && attempt.recovery_operations.iter().any(|operation| {
                    operation.operation_kind == "planning-test-reference-revision"
                        && operation.work_item_id == attempt.work_item_id
                        && operation.attempt_id == attempt.id
                        && operation.candidate_commit.as_ref() == Some(&revision.candidate_commit)
                        && operation.input_digest.as_ref() == Some(&revision_digest)
                }),
            "Test-reference revision has no matching recorded approval"
        );
        let mut changed = std::collections::HashSet::new();
        anyhow::ensure!(
            !revision.changes.is_empty(),
            "Test-reference revision has no changes"
        );
        for change in &revision.changes {
            let row = approved_contract
                .requirements
                .iter_mut()
                .find(|row| row.id == change.requirement_id)
                .context("approved Test-reference requirement disappeared")?;
            anyhow::ensure!(
                changed.insert(&change.requirement_id)
                    && row.kind == work_model::WriterCompletionRequirementKind::Behavior
                    && row.verification_refs == change.old_references
                    && !change.new_references.is_empty(),
                "approved Test-reference delta changed"
            );
            row.verification_refs = change.new_references.clone();
        }
        anyhow::ensure!(
            attempt.artifacts.contains(&ArtifactRef {
                producer_id: "planning-reference-recovery".into(),
                path: revision.completion_matrix_artifact.clone(),
            }),
            "approved Test-reference matrix is not registered"
        );
        let bytes = fs::read(resolve_input_artifact_path(
            project_root,
            &revision.completion_matrix_artifact,
        )?)?;
        anyhow::ensure!(
            revision.completion_matrix_sha256.as_ref()
                == Some(&format!("{:x}", Sha256::digest(&bytes))),
            "approved Test-reference completion evidence changed"
        );
        let matrix: work_model::WriterCompletionMatrix = serde_json::from_slice(&bytes)?;
        let writer = attempt
            .tasks
            .iter()
            .find(|task| {
                task.id == matrix.task_id
                    && task.kind == TaskKind::Write
                    && task
                        .output
                        .as_ref()
                        .is_some_and(|output| output.commit == revision.candidate_commit)
            })
            .context("approved Test-reference source Writer candidate changed")?;
        let findings = writer_completion_findings(project_root, writer)?;
        matrix
            .status_against(approved_contract, &writer.id, &findings)
            .map_err(anyhow::Error::msg)?;
    }
    if let Some(contract) = contract {
        anyhow::ensure!(
            attempt.writer_completion_contract.as_ref() == Some(&contract),
            "approved responsibility contract changed"
        );
    }
    Ok(())
}

/// Validate the saved repair chain without rewriting the original matrices.
fn validate_preserved_completion_repair(
    project_root: &Path,
    attempt: &work_model::Attempt,
    artifact: &ArtifactRef,
) -> Result<()> {
    let mut current = artifact.clone();
    let mut seen = std::collections::HashSet::new();
    while current.producer_id == "writer-completion-repair" {
        anyhow::ensure!(
            seen.insert(current.path.clone()),
            "completion repair source cycle"
        );
        let parent = Path::new(&current.path)
            .parent()
            .context("repair has no directory")?;
        let audit_name = if parent.file_name().is_some_and(|name| name == "format") {
            "format-repair.json"
        } else {
            "repair.json"
        };
        let audit_path = parent.join(audit_name).to_string_lossy().into_owned();
        anyhow::ensure!(
            attempt
                .artifacts
                .iter()
                .any(
                    |reference| reference.producer_id == "writer-completion-repair"
                        && reference.path == audit_path
                ),
            "approved completion source has no recorded repair audit"
        );
        let audit: serde_json::Value = serde_json::from_slice(&fs::read(
            resolve_input_artifact_path(project_root, &audit_path)?,
        )?)?;
        let source = audit["source-artifact"]
            .as_str()
            .context("repair has no source artifact")?;
        let source_bytes = fs::read(resolve_input_artifact_path(project_root, source)?)?;
        let repaired_bytes = fs::read(resolve_input_artifact_path(project_root, &current.path)?)?;
        anyhow::ensure!(
            audit["schema-version"] == 1
                && audit["repaired-artifact"] == current.path
                && audit["source-sha256"] == format!("{:x}", Sha256::digest(&source_bytes))
                && audit["repaired-sha256"] == format!("{:x}", Sha256::digest(&repaired_bytes)),
            "approved completion repair provenance changed"
        );
        let Some(prior) = attempt.artifacts.iter().find(|reference| {
            reference.path == source && reference.producer_id == "writer-completion-repair"
        }) else {
            break;
        };
        current = prior.clone();
    }
    Ok(())
}

pub use crate::work_model::{EvidenceDocument, EvidenceResult};

#[derive(Debug, serde::Serialize)]
pub(crate) struct CodeChangeClassificationAudit<'a> {
    pub(crate) schema_version: u32,
    pub(crate) operation_id: &'a str,
    pub(crate) work_item_id: &'a str,
    pub(crate) attempt_id: &'a str,
    pub(crate) candidate_commit: &'a str,
    pub(crate) recovery_id: &'a str,
    pub(crate) evidence: &'a EvidenceAttachment,
    pub(crate) review_artifact: &'a str,
    pub(crate) review_digest: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) disposition: Option<&'a str>,
    pub(crate) authorization: &'a str,
    pub(crate) authorization_digest: &'a str,
    pub(crate) writer_task_id: &'a str,
}

/// Return the task-owned completion matrix path without opening it.
pub(crate) fn writer_completion_matrix_path(project_root: &Path, task: &Task) -> Option<PathBuf> {
    task.artifact_area.as_ref().map(|area| {
        project_root
            .join(&area.path)
            .join(WRITER_COMPLETION_MATRIX_FILE)
    })
}

/// Resolve the task-owned completion matrix path under the managed artifact
/// root before it is read or written.
pub(crate) fn managed_writer_completion_matrix_path(
    project_root: &Path,
    task: &Task,
) -> Result<Option<PathBuf>> {
    task.artifact_area
        .as_ref()
        .map(|area| {
            resolve_managed_artifact_area_path(project_root, &area.path)
                .map(|dir| dir.join(WRITER_COMPLETION_MATRIX_FILE))
        })
        .transpose()
}

/// Resolve an input artifact from one of Fluent's managed roots.
pub(crate) fn resolve_input_artifact_path(project_root: &Path, path: &str) -> Result<PathBuf> {
    let relative_path = Path::new(path);
    if relative_path.is_absolute() {
        bail!("Input artifact path must be relative: {path}");
    }
    let mut components = Vec::new();
    for component in relative_path.components() {
        match component {
            Component::Normal(part) => components.push(part.to_owned()),
            _ => bail!(
                "Input artifact path must stay under .fluent/work/artifacts/ or .fluent/work/progress/: {path}"
            ),
        }
    }
    let valid = components.len() >= 3
        && components[0] == std::ffi::OsStr::new(".fluent")
        && components[1] == std::ffi::OsStr::new("work")
        && (components[2] == std::ffi::OsStr::new("artifacts")
            || components[2] == std::ffi::OsStr::new("progress"));
    if !valid {
        bail!(
            "Input artifact path must be under .fluent/work/artifacts/ or .fluent/work/progress/: {path}"
        );
    }
    Ok(resolve_workspace_path(project_root, path))
}

/// Resolve a task artifact area below `.fluent/work/artifacts`.
pub(crate) fn resolve_managed_artifact_area_path(
    project_root: &Path,
    path: &str,
) -> Result<PathBuf> {
    let relative_path = Path::new(path);
    if relative_path.is_absolute() {
        bail!("Task artifact area path must be relative: {path}");
    }

    let mut components = Vec::new();
    for component in relative_path.components() {
        match component {
            Component::Normal(part) => components.push(part.to_owned()),
            _ => bail!("Task artifact area path must stay under .fluent/work/artifacts: {path}"),
        }
    }

    let managed_prefix = [
        std::ffi::OsStr::new(".fluent"),
        std::ffi::OsStr::new("work"),
        std::ffi::OsStr::new("artifacts"),
    ];
    if components.len() <= managed_prefix.len()
        || !components
            .iter()
            .zip(managed_prefix.iter())
            .all(|(actual, expected)| actual == expected)
    {
        bail!("Task artifact area path must stay under .fluent/work/artifacts: {path}");
    }

    Ok(resolve_workspace_path(project_root, path))
}

fn resolve_workspace_path(project_root: &Path, path: &str) -> PathBuf {
    let path = PathBuf::from(path);
    if path.is_absolute() {
        path
    } else {
        project_root.join(path)
    }
}

/// Collect immutable findings that a Writer completion matrix must address.
pub(crate) fn writer_completion_findings(
    project_root: &Path,
    task: &Task,
) -> Result<Vec<WriterCompletionFinding>> {
    let snapshot = read_settled_work_item_snapshot(project_root, &task.work_item_id);
    let item = if task.writer_correction_assignment.is_some() {
        Some(snapshot.with_context(|| {
            format!("read the host-owned assignment for Writer Task {}", task.id)
        })?)
    } else {
        snapshot.ok()
    };
    writer_completion_findings_from_item(project_root, task, item.as_ref())
}

/// Let a peer's bounded publication settle without recovering its journal.
pub(crate) fn read_settled_work_item_snapshot(
    project_root: &Path,
    work_item_id: &str,
) -> Result<work_model::WorkItem, work_model::WorkModelStorageError> {
    let store = work_model::WorkModelStore::new(project_root);
    let mut snapshot = store.read_work_item_snapshot(work_item_id);
    for _ in 0..20 {
        if !matches!(
            snapshot,
            Err(work_model::WorkModelStorageError::SnapshotUnavailable { .. })
        ) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
        snapshot = store.read_work_item_snapshot(work_item_id);
    }
    snapshot
}

fn writer_completion_findings_from_item(
    project_root: &Path,
    task: &Task,
    item: Option<&work_model::WorkItem>,
) -> Result<Vec<WriterCompletionFinding>> {
    if let Some(assignment) = task.writer_correction_assignment.as_ref() {
        validate_writer_correction_assignment(
            project_root,
            item.context("host-owned Writer assignment has no Work Item")?,
            task,
            assignment,
        )?;
        return Ok(assignment.findings.clone());
    }
    for artifact in &task.input_artifacts {
        resolve_input_artifact_path(project_root, &artifact.path)?;
    }
    // Initial Writers own only their Task inputs. A legacy correction without
    // a frozen assignment still needs current verification to classify an
    // inherited candidate gate before it can admit source work.
    let assessment = item
        .filter(|_| is_corrective_writer_task(task))
        .and_then(|item| {
            item.attempts
                .iter()
                .find(|attempt| Some(attempt.id.as_str()) == task.attempt_id.as_deref())
        })
        .filter(|attempt| {
            attempt.tasks.iter().any(|task| {
                task.kind == TaskKind::Write
                    && task.status == TaskStatus::Complete
                    && task.output.is_some()
            })
        })
        .map(|attempt| {
            crate::verification_assessment::assess_current_verification(project_root, attempt)
        })
        .transpose()?;
    findings_from_inputs(
        project_root,
        &task.input_artifacts,
        task.role != work_model::TESTER_HARNESS_REPAIR_ROLE,
        assessment.as_ref(),
    )
}

/// Classify a Writer Task using the same correction markers that select the
/// reviewed-correction execution path. A continuation of one Writer session
/// carries its assignment; a new corrective round captures a new one.
pub(crate) fn is_corrective_writer_task(task: &Task) -> bool {
    task.input_artifacts
        .iter()
        .any(|artifact| crate::writer_context::is_correction_artifact(Path::new(&artifact.path)))
}

/// Reconstruct one missing assignment only when a paused, changed candidate
/// still exposes the complete original source chain. This inspector is read
/// only; lifecycle callers publish the returned record during an explicit
/// resume transaction.
pub(crate) fn inspect_legacy_writer_correction_assignment(
    project_root: &Path,
    item: &work_model::WorkItem,
    attempt_id: &str,
) -> Result<Option<WriterCorrectionAssignment>> {
    let Some(attempt) = item
        .attempts
        .iter()
        .find(|attempt| attempt.id == attempt_id)
    else {
        bail!("Attempt {attempt_id:?} not found");
    };
    if item.attempts.last().map(|attempt| attempt.id.as_str()) != Some(attempt_id)
        || attempt.status != work_model::AttemptStatus::NeedsUser
        || attempt.pause_kind != Some(work_model::PauseKind::WriterCompletion)
    {
        return Ok(None);
    }
    attempt.ensure_resume_ready().map_err(anyhow::Error::msg)?;

    let Some(correction_index) = attempt.tasks.len().checked_sub(1) else {
        return Ok(None);
    };
    let correction = &attempt.tasks[correction_index];
    if correction.kind != TaskKind::Write
        || correction.status != TaskStatus::Complete
        || correction.output.is_none()
        || correction.writer_correction_assignment.is_some()
    {
        return Ok(None);
    }
    let output = correction
        .output
        .as_ref()
        .expect("completed Writer output checked above");
    let Some(base_commit) = output.base_commit.as_deref() else {
        return Ok(None);
    };
    if base_commit == output.commit {
        return Ok(None);
    }

    let Some((source_index, source)) = attempt.tasks[..correction_index]
        .iter()
        .enumerate()
        .rev()
        .find(|(_, task)| {
            task.kind == TaskKind::Write
                && task.status == TaskStatus::Complete
                && task.output.is_some()
        })
    else {
        // A first Writer can also stop at WriterCompletion, but it has no
        // older assignment to reconstruct. Leave that ordinary path to the
        // existing completion recovery logic.
        return Ok(None);
    };
    let source_output = source
        .output
        .as_ref()
        .expect("completed source Writer output checked above");
    if source_output.commit != base_commit {
        bail!(
            "legacy Writer assignment source candidate {} does not match correction base {}",
            source_output.commit,
            base_commit
        );
    }
    if attempt.tasks[..correction_index].iter().any(|task| {
        matches!(
            task.status,
            TaskStatus::Planned | TaskStatus::Executing | TaskStatus::NeedsUser
        ) || (task.status == TaskStatus::Failed && task.output.is_some())
    }) {
        bail!("legacy Writer assignment has an unresolved source Task");
    }
    if item
        .merge_candidates
        .iter()
        .any(|candidate| candidate.attempt_id == attempt_id)
    {
        bail!("legacy Writer assignment cannot recover after Merge Candidate publication");
    }
    for task in &attempt.tasks {
        anyhow::ensure!(
            !crate::lease::is_leased(&crate::lease::task_lock_path(
                project_root,
                &item.id,
                &task.id
            )),
            "Task {} still has a live owner",
            task.id
        );
        anyhow::ensure!(
            !legacy_task_execution_is_live(project_root, task)?,
            "Task {} still owns a live execution",
            task.id
        );
    }
    anyhow::ensure!(
        candidate_workspace_is_clean_at_managed(project_root, output, &output.commit),
        "legacy Writer assignment requires the corrected managed candidate to remain clean and unchanged"
    );

    let matrix_ref = writer_completion_matrix_ref(project_root, attempt, correction)
        .context("legacy Writer assignment has no completion matrix")?;
    let matrix_path = resolve_input_artifact_path(project_root, &matrix_ref.path)?;
    let matrix_metadata = fs::symlink_metadata(&matrix_path).with_context(|| {
        format!(
            "inspect legacy Writer completion matrix {}",
            matrix_path.display()
        )
    })?;
    anyhow::ensure!(
        matrix_metadata.is_file() && !matrix_metadata.file_type().is_symlink(),
        "legacy Writer completion matrix is not a regular file"
    );
    anyhow::ensure!(
        matrix_metadata.len() <= work_model::WRITER_COMPLETION_MATRIX_MAX_BYTES as u64,
        "legacy Writer completion matrix exceeds its size bound"
    );
    let matrix: work_model::WriterCompletionMatrix =
        serde_json::from_slice(&fs::read(&matrix_path).with_context(|| {
            format!(
                "read legacy Writer completion matrix {}",
                matrix_path.display()
            )
        })?)
        .context("parse legacy Writer completion matrix")?;
    if !matrix
        .rows
        .iter()
        .any(|row| row.kind == work_model::WriterCompletionRequirementKind::Finding)
    {
        return Ok(None);
    }

    let current_findings =
        writer_completion_findings_from_item(project_root, correction, Some(item))?;
    let contract = attempt
        .writer_completion_contract
        .as_ref()
        .context("legacy Writer assignment has no immutable completion contract")?;
    // A structurally valid matrix for the current candidate is not a legacy
    // mismatch. Any structural disagreement is only a signal to inspect the
    // older source evidence below; it is not itself authority for recovery.
    if matrix
        .status_against(contract, &correction.id, &current_findings)
        .is_ok()
    {
        return Ok(None);
    }

    let mut historical_attempt = attempt.clone();
    historical_attempt.tasks = attempt.tasks[..correction_index].to_vec();
    historical_attempt.status = work_model::AttemptStatus::Reviewing;
    historical_attempt.pause_kind = None;
    historical_attempt.pause_recovery = None;
    historical_attempt.pause_record = None;
    historical_attempt.completed_at = None;

    let source_testers = historical_attempt
        .tasks
        .iter()
        .filter(|task| {
            task.kind == TaskKind::Tester
                && task.status == TaskStatus::Complete
                && !task.is_failed_command_recheck()
                && task
                    .review_context
                    .as_ref()
                    .is_some_and(|context| context.candidate_commit == source_output.commit)
        })
        .collect::<Vec<_>>();
    anyhow::ensure!(
        source_testers.len() <= 1,
        "legacy Writer assignment has multiple complete source Testers"
    );
    let canonical = if source_testers.is_empty() {
        None
    } else {
        Some(assess_canonical_checks(project_root, &historical_attempt)?)
    };
    if let Some(canonical) = canonical.as_ref() {
        anyhow::ensure!(
            canonical.blockers.is_empty() && !canonical.checks.is_empty(),
            "legacy source Tester does not provide complete, unblocked canonical evidence"
        );
    }
    let historical_assessment = if canonical.is_some() {
        Some(crate::verification_assessment::assess_current_verification(
            project_root,
            &historical_attempt,
        )?)
    } else {
        None
    };
    if let Some(assessment) = historical_assessment.as_ref() {
        anyhow::ensure!(
            assessment.candidate_commit == source_output.commit,
            "legacy source verification assessed a different candidate"
        );
    }
    validate_legacy_assignment_inputs(
        project_root,
        item,
        &historical_attempt,
        source,
        correction,
        canonical.as_ref(),
    )?;
    let findings = findings_from_inputs(
        project_root,
        &correction.input_artifacts,
        true,
        historical_assessment.as_ref(),
    )?;
    matrix
        .status_against(contract, &correction.id, &findings)
        .map_err(|error| anyhow::anyhow!("legacy Writer completion matrix is not bound to the verified source assignment: {error}"))?;
    anyhow::ensure!(
        !findings.is_empty(),
        "legacy Writer assignment has no verified findings to reconstruct"
    );

    let source_task_inputs = task_assignment_inputs(project_root, &source.input_artifacts)?;
    let evidence = legacy_assignment_evidence(
        project_root,
        &correction.input_artifacts,
        &findings,
        historical_assessment.as_ref(),
        canonical.as_ref(),
    )?;
    let assignment = WriterCorrectionAssignment {
        schema_version: work_model::WRITER_CORRECTION_ASSIGNMENT_SCHEMA_VERSION,
        work_item_id: item.id.clone(),
        attempt_id: attempt.id.clone(),
        writer_task_id: correction.id.clone(),
        source_task_id: source.id.clone(),
        source_candidate_commit: source_output.commit.clone(),
        source_task_inputs,
        evidence,
        findings,
        origin: WriterCorrectionAssignmentOrigin::LegacyReconstructed,
    };
    assignment
        .validate_for_task(correction)
        .map_err(|error| anyhow::anyhow!("legacy Writer assignment is invalid: {error}"))?;
    // Keep the source index live in this function so future changes cannot
    // accidentally select a source after the correction Task.
    debug_assert!(source_index < correction_index);
    Ok(Some(assignment))
}

fn validate_legacy_assignment_inputs(
    project_root: &Path,
    item: &work_model::WorkItem,
    historical_attempt: &work_model::Attempt,
    source: &Task,
    correction: &Task,
    canonical: Option<&crate::verification_assessment::CanonicalCheckAssessment>,
) -> Result<()> {
    let canonical_results = canonical
        .into_iter()
        .flat_map(|assessment| assessment.checks.iter())
        .filter_map(|check| match &check.source {
            work_model::RequiredCheckSource::CanonicalTester {
                task_id, results, ..
            } => Some((task_id.as_str(), results.path.as_str())),
            work_model::RequiredCheckSource::ReviewRequested { .. } => None,
        })
        .collect::<Vec<_>>();
    let canonical_results = if let Some(first) = canonical_results.first().copied() {
        anyhow::ensure!(
            canonical_results
                .iter()
                .all(|candidate| *candidate == first),
            "legacy source canonical checks use mixed Tester evidence"
        );
        Some(first)
    } else {
        None
    };
    for artifact in &correction.input_artifacts {
        let path = Path::new(&artifact.path);
        let is_review = path.file_name().is_some_and(|name| {
            name == "review.md" || name.to_string_lossy().starts_with("review-")
        });
        if is_review {
            let task = historical_attempt
                .tasks
                .iter()
                .find(|task| task.id == artifact.producer_id)
                .context("legacy review evidence producer Task is missing")?;
            let context = task
                .review_context
                .as_ref()
                .context("legacy review evidence has no candidate context")?;
            anyhow::ensure!(
                task.kind == TaskKind::Review
                    && task.status == TaskStatus::Complete
                    && review_artifact_path(task).as_deref() == Some(artifact.path.as_str())
                    && context.candidate_commit == source.output.as_ref().unwrap().commit,
                "legacy review evidence is not bound to the source candidate"
            );
            continue;
        }
        if path
            .file_name()
            .is_some_and(|name| name == "candidate-gate.json")
        {
            let bytes = fs::read(resolve_input_artifact_path(project_root, &artifact.path)?)?;
            let gate: candidate_gate::CandidateGateEvidence =
                serde_json::from_slice(&bytes).context("parse legacy candidate-gate evidence")?;
            anyhow::ensure!(
                artifact.producer_id == source.id
                    && gate.candidate_commit == source.output.as_ref().unwrap().commit
                    && gate.base_commit
                        == correction
                            .output
                            .as_ref()
                            .and_then(|output| output.base_commit.as_deref())
                            .unwrap_or_default(),
                "legacy candidate-gate evidence is not bound to the source candidate"
            );
            continue;
        }
        if artifact.path.ends_with("/tester-results.json") {
            let Some((tester_id, results_path)) = canonical_results else {
                let bytes = fs::read(resolve_input_artifact_path(project_root, &artifact.path)?)?;
                let results: crate::tester::TesterResults =
                    serde_json::from_slice(&bytes).context("parse legacy Tester results")?;
                anyhow::ensure!(
                    results.error.is_none()
                        && results
                            .commands
                            .iter()
                            .all(|command| command.exit_code == 0)
                        && results.tests.iter().all(|test| test.status != "fail"),
                    "legacy Tester findings lack a verified complete source Tester"
                );
                continue;
            };
            anyhow::ensure!(
                artifact.producer_id == tester_id && artifact.path == results_path,
                "legacy Tester evidence is not bound to the original complete Tester"
            );
            let bytes = fs::read(resolve_input_artifact_path(project_root, &artifact.path)?)?;
            let results: crate::tester::TesterResults =
                serde_json::from_slice(&bytes).context("parse legacy Tester results")?;
            anyhow::ensure!(
                results.candidate_commit.as_deref()
                    == source.output.as_ref().map(|output| output.commit.as_str()),
                "legacy Tester result candidate binding changed"
            );
        }
    }
    anyhow::ensure!(
        item.id == historical_attempt.work_item_id
            && correction.work_item_id == item.id
            && correction.attempt_id.as_deref() == Some(historical_attempt.id.as_str()),
        "legacy Writer assignment Task identity changed"
    );
    Ok(())
}

fn legacy_task_execution_is_live(project_root: &Path, task: &Task) -> Result<bool> {
    let Some(area) = task.artifact_area.as_ref() else {
        return Ok(false);
    };
    let path = crate::execution::record_path_for_artifact(project_root, &area.path);
    if !path.exists() {
        return Ok(false);
    }
    crate::execution::execution_is_live(&path)
}

fn legacy_assignment_evidence(
    project_root: &Path,
    task_inputs: &[ArtifactRef],
    findings: &[WriterCompletionFinding],
    assessment: Option<&crate::verification_assessment::VerificationAssessment>,
    canonical: Option<&crate::verification_assessment::CanonicalCheckAssessment>,
) -> Result<Vec<WriterCorrectionAssignmentInput>> {
    let mut refs =
        assignment_evidence_for_findings(project_root, task_inputs, findings, assessment)?
            .into_iter()
            .map(|input| input.artifact)
            .collect::<Vec<_>>();
    let mut add_if_present = |artifact: ArtifactRef| -> Result<()> {
        if refs.iter().any(|existing| existing.path == artifact.path) {
            return Ok(());
        }
        let resolved = resolve_input_artifact_path(project_root, &artifact.path)?;
        if !resolved.exists() && artifact.path.contains("/evidence-reconstructions/") {
            // The canonical assessor may have produced an in-memory proof for
            // an older run without a manifest. The committed config, extractor,
            // result, and logs remain independently bound below.
            return Ok(());
        }
        assignment_input_digest(project_root, &artifact.path)?;
        refs.push(artifact);
        Ok(())
    };
    if let Some(canonical) = canonical {
        if let Some(baseline) = &canonical.baseline {
            add_if_present(ArtifactRef {
                producer_id: "verification-assessment".into(),
                path: baseline.path.clone(),
            })?;
        }
        for check in &canonical.checks {
            if let work_model::RequiredCheckSource::CanonicalTester {
                reconstruction,
                results,
                manifest,
                logs,
                ..
            } = &check.source
            {
                let sources = reconstruction
                    .iter()
                    .chain(std::iter::once(results))
                    .chain(std::iter::once(manifest))
                    .chain(logs);
                for source in sources {
                    add_if_present(ArtifactRef {
                        producer_id: "verification-assessment".into(),
                        path: source.path.clone(),
                    })?;
                }
            }
        }
        for binding in canonical.review_bindings.values().flatten() {
            add_if_present(ArtifactRef {
                producer_id: "verification-assessment".into(),
                path: binding.path.clone(),
            })?;
        }
    }
    refs.into_iter()
        .map(|artifact| {
            Ok(WriterCorrectionAssignmentInput {
                digest: assignment_input_digest(project_root, &artifact.path)?,
                artifact,
            })
        })
        .collect()
}

/// Revalidate the immutable source bindings before a persisted assignment is
/// used by a completion gate. This catches changed evidence even when a caller
/// supplied a stale in-memory Task value.
pub(crate) fn validate_writer_correction_assignment(
    project_root: &Path,
    item: &work_model::WorkItem,
    task: &Task,
    assignment: &WriterCorrectionAssignment,
) -> Result<()> {
    validate_writer_assignment_with_context_proof(project_root, item, task, assignment, &[])
}

fn validate_writer_assignment_with_context_proof(
    project_root: &Path,
    item: &work_model::WorkItem,
    task: &Task,
    assignment: &WriterCorrectionAssignment,
    proven_context: &[WriterCorrectionAssignmentInput],
) -> Result<()> {
    assignment
        .validate_for_task(task)
        .map_err(anyhow::Error::msg)?;
    let attempt = item
        .attempts
        .iter()
        .find(|attempt| Some(attempt.id.as_str()) == task.attempt_id.as_deref())
        .context("assignment Attempt is missing")?;
    let persisted = attempt
        .tasks
        .iter()
        .find(|candidate| candidate.id == task.id)
        .context("assigned Writer Task is missing from its Attempt")?;
    anyhow::ensure!(
        persisted.writer_correction_assignment.as_ref() == Some(assignment),
        "host-owned Writer correction assignment changed in storage"
    );
    let source_index = attempt
        .tasks
        .iter()
        .position(|candidate| candidate.id == assignment.source_task_id)
        .context("assignment source Task is missing")?;
    let writer_index = attempt
        .tasks
        .iter()
        .position(|candidate| candidate.id == task.id)
        .context("assigned Writer Task is missing")?;
    anyhow::ensure!(
        source_index < writer_index,
        "assignment source Task must precede its Writer Task"
    );
    let source = &attempt.tasks[source_index];
    let output = source
        .output
        .as_ref()
        .context("assignment source Task has no candidate output")?;
    anyhow::ensure!(
        source.kind == TaskKind::Write
            && source.status == TaskStatus::Complete
            && output.commit == assignment.source_candidate_commit,
        "assignment source candidate changed"
    );
    for input in &assignment.source_task_inputs {
        if mutable_assignment_context(task, &input.artifact) && proven_context.contains(input) {
            continue;
        }
        anyhow::ensure!(
            assignment_input_digest(project_root, &input.artifact.path)? == input.digest,
            "Writer assignment input changed: {}",
            input.artifact.path
        );
    }
    for input in &assignment.evidence {
        anyhow::ensure!(
            assignment_input_digest(project_root, &input.artifact.path)? == input.digest,
            "Writer assignment input changed: {}",
            input.artifact.path
        );
    }
    Ok(())
}

/// Follow preserved assignments back to their original Writer. Only a proven
/// frozen context reference may replace a historical mutable input; immutable
/// evidence, findings, and source identities retain exact equality.
pub(crate) fn validate_writer_continuation_lineage(
    root: &Path,
    item: &work_model::WorkItem,
    task: &Task,
) -> Result<()> {
    let attempt = item
        .attempts
        .iter()
        .find(|attempt| Some(attempt.id.as_str()) == task.attempt_id.as_deref())
        .context("assignment Attempt is missing")?;
    let mut receiver = task;
    let mut proven_context = Vec::new();
    loop {
        let carried = receiver
            .writer_correction_assignment
            .as_ref()
            .context("Writer continuation source has no preserved assignment")?;
        validate_writer_assignment_with_context_proof(
            root,
            item,
            receiver,
            carried,
            &proven_context,
        )?;
        validate_recorded_assignment_context_recoveries(root, item, receiver)?;
        let index = attempt
            .tasks
            .iter()
            .position(|other| other.id == receiver.id)
            .context("assigned Writer Task is missing")?;
        let previous = attempt.tasks[..index]
            .iter()
            .rev()
            .find(|other| {
                other.kind == TaskKind::Write
                    && other.status == TaskStatus::Complete
                    && other.output.is_some()
            })
            .context("Writer assignment has no preceding completed Writer")?;
        if carried.source_task_id == previous.id {
            anyhow::ensure!(
                previous.writer_correction_assignment.is_none()
                    || is_corrective_writer_task(receiver),
                "Writer continuation changed its original assignment source"
            );
            return Ok(());
        }
        anyhow::ensure!(
            !is_corrective_writer_task(receiver),
            "an inherited assignment requires a pre-review continuation"
        );
        let mut expected = previous
            .writer_correction_assignment
            .clone()
            .context("Writer continuation source has no preserved assignment")?;
        expected.writer_task_id = receiver.id.clone();
        for (index, input) in expected.source_task_inputs.iter_mut().enumerate() {
            if mutable_assignment_context(receiver, &input.artifact)
                && carried.source_task_inputs.get(index) != Some(input)
            {
                let (snapshot, provenance, provenance_bytes) =
                    assignment_context_snapshot(receiver, input)?;
                validate_assignment_context_snapshot(
                    root,
                    &snapshot,
                    &provenance,
                    &provenance_bytes,
                )?;
                proven_context.push(input.clone());
                *input = snapshot;
                expected.evidence.push(provenance);
                expected.schema_version = work_model::WRITER_CORRECTION_ASSIGNMENT_SCHEMA_VERSION;
            }
        }
        anyhow::ensure!(
            expected == *carried,
            "Writer continuation assignment differs from its preserved source"
        );
        receiver = previous;
    }
}

fn validate_recorded_assignment_context_recoveries(
    root: &Path,
    item: &work_model::WorkItem,
    task: &Task,
) -> Result<()> {
    let attempt = item
        .attempts
        .iter()
        .find(|attempt| Some(attempt.id.as_str()) == task.attempt_id.as_deref())
        .context("assignment Attempt is missing")?;
    let records = attempt
        .recovery_operations
        .iter()
        .filter(|record| {
            record.operation_kind == "assignment-context-recovery"
                && record.task_id.as_deref() == Some(task.id.as_str())
        })
        .collect::<Vec<_>>();
    let mut validated = Vec::new();
    for artifact in attempt
        .artifacts
        .iter()
        .filter(|artifact| artifact.producer_id == "assignment-context-recovery")
    {
        let bytes = assignment_input_bytes(root, &artifact.path)?;
        let audit: work_model::AssignmentContextRecoveryAudit = serde_json::from_slice(&bytes)?;
        if audit.before.writer_task_id != task.id && audit.after.writer_task_id != task.id {
            continue;
        }
        let digest = format!("sha256:{:x}", Sha256::digest(&bytes));
        let record = records
            .iter()
            .find(|record| record.input_digest.as_deref() == Some(digest.as_str()))
            .context("assignment context audit has no matching recovery operation")?;
        validate_assignment_context_audit(root, task, &artifact.path, &bytes, &audit, record)?;
        validate_assignment_context_lineage(root, item, task, &audit.before)?;
        validated.push(record.operation_id.as_str());
    }
    anyhow::ensure!(
        records
            .iter()
            .all(|record| validated.contains(&record.operation_id.as_str())),
        "assignment context recovery lost its bound audit"
    );
    Ok(())
}

/// Publish immutable Writer finding assignments on newly planned corrective
/// Tasks before the Task can launch. A continuation carries the same source
/// assignment; a correction captures the findings and evidence named by its
/// failed review or verification inputs.
pub(crate) fn prepare_writer_correction_assignments(
    project_root: &Path,
    item: &mut work_model::WorkItem,
) -> Result<()> {
    let mut plans = Vec::new();
    for (attempt_index, attempt) in item.attempts.iter().enumerate() {
        for (task_index, task) in attempt.tasks.iter().enumerate() {
            if task.kind != TaskKind::Write
                || task.status != TaskStatus::Planned
                || task.writer_correction_assignment.is_some()
            {
                continue;
            }
            let Some(source) = attempt.tasks[..task_index]
                .iter()
                .rev()
                .find(|source| {
                    source.kind == TaskKind::Write
                        && source.status == TaskStatus::Complete
                        && source.output.is_some()
                })
                .cloned()
            else {
                continue;
            };
            plans.push((attempt_index, task_index, task.clone(), source));
        }
    }

    for (attempt_index, task_index, task, source) in plans {
        let mut assignment = if source.writer_correction_assignment.is_some()
            && !is_corrective_writer_task(&task)
        {
            let previous = source
                .writer_correction_assignment
                .clone()
                .expect("the continuation source has an assignment");
            let mut carried = previous;
            carried.work_item_id = item.id.clone();
            carried.attempt_id = task.attempt_id.clone().unwrap_or_default();
            carried.writer_task_id = task.id.clone();
            carried.validate_for_task(&task).map_err(|reason| {
                anyhow::anyhow!(
                    "carry Writer correction assignment to Task {}: {reason}",
                    task.id
                )
            })?;
            carried
        } else {
            let assessment = crate::verification_assessment::assess_current_verification(
                project_root,
                &item.attempts[attempt_index],
            )?;
            let findings = findings_from_inputs(
                project_root,
                &task.input_artifacts,
                task.role != work_model::TESTER_HARNESS_REPAIR_ROLE,
                Some(&assessment),
            )?;
            let mut evidence = assignment_evidence_for_findings(
                project_root,
                &task.input_artifacts,
                &findings,
                Some(&assessment),
            )?;
            for input in corrective_host_evidence_inputs(
                project_root,
                &item.attempts[attempt_index],
                &task,
                &source,
            )? {
                if !evidence.contains(&input) {
                    evidence.push(input);
                }
            }
            let source_task_inputs = task_assignment_inputs(project_root, &source.input_artifacts)?;
            WriterCorrectionAssignment {
                schema_version: work_model::WRITER_CORRECTION_ASSIGNMENT_SCHEMA_VERSION,
                work_item_id: item.id.clone(),
                attempt_id: task.attempt_id.clone().unwrap_or_default(),
                writer_task_id: task.id.clone(),
                source_task_id: source.id,
                source_candidate_commit: source
                    .output
                    .as_ref()
                    .map(|output| output.commit.clone())
                    .unwrap_or_default(),
                source_task_inputs,
                evidence,
                findings,
                origin: WriterCorrectionAssignmentOrigin::Captured,
            }
        };
        for input in &mut assignment.source_task_inputs {
            if mutable_assignment_context(&task, &input.artifact) {
                let bytes = assignment_input_bytes(project_root, &input.artifact.path)?;
                anyhow::ensure!(
                    format!("sha256:{:x}", Sha256::digest(&bytes)) == input.digest,
                    "mutable context changed during capture"
                );
                let (snapshot, provenance, provenance_bytes) =
                    assignment_context_snapshot(&task, input)?;
                work_model::publish_recovery_snapshot(
                    project_root,
                    &snapshot.artifact.path,
                    &bytes,
                )?;
                work_model::publish_recovery_snapshot(
                    project_root,
                    &provenance.artifact.path,
                    &provenance_bytes,
                )?;
                *input = snapshot;
                assignment.evidence.push(provenance);
                assignment.schema_version = work_model::WRITER_CORRECTION_ASSIGNMENT_SCHEMA_VERSION;
            }
        }
        assignment.validate_for_task(&task).map_err(|reason| {
            anyhow::anyhow!(
                "validate Writer correction assignment for Task {}: {reason}",
                task.id
            )
        })?;
        item.attempts[attempt_index].tasks[task_index].writer_correction_assignment =
            Some(assignment);
    }
    Ok(())
}

/// Freeze the exact evidence behind a targeted correction alongside its review.
/// The assignment's existing digest checks then guard every later launch.
fn corrective_host_evidence_inputs(
    root: &Path,
    attempt: &work_model::Attempt,
    task: &Task,
    source: &Task,
) -> Result<Vec<WriterCorrectionAssignmentInput>> {
    let mut inputs = Vec::new();
    for input in &task.input_artifacts {
        let Some(reviewer) = attempt.tasks.iter().find(|reviewer| {
            reviewer.kind == TaskKind::Review
                && reviewer.id == input.producer_id
                && reviewer
                    .artifact_area
                    .as_ref()
                    .is_some_and(|area| input.path == format!("{}/review.md", area.path))
        }) else {
            continue;
        };
        let Some(context) = reviewer.evidence_review_context.as_ref() else {
            continue;
        };
        let recovery = attempt
            .evidence_recoveries
            .iter()
            .find(|recovery| recovery.id == context.recovery_id)
            .context("corrective review lost its host evidence recovery")?;
        anyhow::ensure!(
            reviewer.status == TaskStatus::Complete
                && recovery.state == EvidenceRecoveryState::CodeChange
                && context.attachment == recovery.attachment
                && context.candidate_commit == recovery.candidate_commit
                && source
                    .output
                    .as_ref()
                    .is_some_and(|output| output.commit == context.candidate_commit)
                && recovery.targets.iter().any(|target| {
                    target.review_task_id.as_deref() == Some(reviewer.id.as_str())
                        && target.role == reviewer.role
                        && target.prior_review_artifact == context.prior_review_artifact
                }),
            "corrective host evidence no longer matches its review or source candidate"
        );
        if let Some(check) = &recovery.required_check {
            validate_required_check_binding(root, attempt, check)?;
        }
        validate_plan_evidence_recovery_at(
            root,
            attempt,
            recovery,
            crate::check_applicability::evaluation_time(),
            false,
        )?;
        let (evidence, bytes) = bound_recovery_artifact(root, &context.attachment.snapshot_path)?;
        anyhow::ensure!(
            evidence.digest == context.attachment.digest
                && evidence.path
                    == snapshot_path(&attempt.work_item_id, &attempt.id, &evidence.digest)?,
            "corrective host evidence digest changed"
        );
        parse_evidence_document(&bytes)?;
        let (binding, _) = bound_recovery_artifact(root, &context.prior_review_artifact)?;
        for artifact in [evidence, binding] {
            let input = WriterCorrectionAssignmentInput {
                digest: artifact.digest,
                artifact: ArtifactRef {
                    producer_id: "corrective-host-evidence".into(),
                    path: artifact.path,
                },
            };
            if !inputs.contains(&input) {
                inputs.push(input);
            }
        }
    }
    Ok(inputs)
}

fn assignment_evidence_for_findings(
    project_root: &Path,
    task_inputs: &[ArtifactRef],
    findings: &[WriterCompletionFinding],
    assessment: Option<&crate::verification_assessment::VerificationAssessment>,
) -> Result<Vec<WriterCorrectionAssignmentInput>> {
    let mut refs = Vec::new();
    for finding in findings {
        let mut candidates = task_inputs
            .iter()
            .filter(|artifact| artifact.path == finding.source)
            .cloned()
            .collect::<Vec<_>>();
        if let Some(assessment) = assessment {
            for row in assessment
                .references
                .iter()
                .chain(&assessment.commands)
                .filter(|row| {
                    row.id
                        == finding
                            .id
                            .strip_prefix("finding:verification:")
                            .unwrap_or("")
                })
            {
                for binding in &row.bindings {
                    if binding.path == finding.source {
                        candidates.push(ArtifactRef {
                            producer_id: "verification-assessment".to_string(),
                            path: binding.path.clone(),
                        });
                    }
                }
            }
        }
        if candidates.is_empty() && finding.role == "verification" {
            candidates.extend(
                task_inputs
                    .iter()
                    .filter(|artifact| {
                        Path::new(&artifact.path)
                            .file_name()
                            .is_some_and(|name| name == candidate_gate::CANDIDATE_GATE_FILE)
                    })
                    .cloned(),
            );
        }
        candidates.sort_by(|left, right| left.producer_id.cmp(&right.producer_id));
        candidates.dedup();
        let Some(artifact) = candidates.into_iter().next() else {
            bail!(
                "Writer finding {} has no task-owned or assessment-bound evidence {}",
                finding.id,
                finding.source
            );
        };
        if !refs.contains(&artifact) {
            refs.push(artifact);
        }
    }
    refs.into_iter()
        .map(|artifact| {
            Ok(WriterCorrectionAssignmentInput {
                digest: assignment_input_digest(project_root, &artifact.path)?,
                artifact,
            })
        })
        .collect()
}

fn task_assignment_inputs(
    project_root: &Path,
    inputs: &[ArtifactRef],
) -> Result<Vec<WriterCorrectionAssignmentInput>> {
    inputs
        .iter()
        .map(|artifact| {
            Ok(WriterCorrectionAssignmentInput {
                digest: assignment_input_digest(project_root, &artifact.path)?,
                artifact: artifact.clone(),
            })
        })
        .collect()
}

/// Only the host's exact Work/Attempt projections may change after assignment.
pub(crate) fn mutable_assignment_context(task: &Task, artifact: &ArtifactRef) -> bool {
    let Some(attempt) = task.attempt_id.as_deref() else {
        return false;
    };
    artifact.producer_id == "required-progress"
        && [
            format!(
                ".fluent/work/progress/{}/{attempt}/progress.md",
                task.work_item_id
            ),
            work_model::work_artifact_path(
                &task.work_item_id,
                attempt,
                "required-progress-needs-user.md",
            ),
            work_model::work_artifact_path(
                &task.work_item_id,
                attempt,
                "writer-completion-needs-user.md",
            ),
        ]
        .contains(&artifact.path)
}

pub(crate) fn assignment_source_inputs_match(
    task: &Task,
    source: &Task,
    assignment: &WriterCorrectionAssignment,
) -> bool {
    source.input_artifacts.len() == assignment.source_task_inputs.len()
        && source.input_artifacts.iter().all(|artifact| {
            assignment.source_task_inputs.iter().any(|input| {
                if &input.artifact == artifact {
                    return true;
                }
                if assignment.schema_version < 2 {
                    return false;
                }
                let original = WriterCorrectionAssignmentInput {
                    artifact: artifact.clone(),
                    digest: input.digest.clone(),
                };
                assignment_context_snapshot(task, &original).is_ok_and(
                    |(snapshot, provenance, _)| {
                        snapshot == *input && assignment.evidence.contains(&provenance)
                    },
                )
            })
        })
}

/// Describe ordinary digest-bound artifacts, preserving the original reference
/// in separately bound provenance. Older readers validate these same bytes.
pub(crate) fn assignment_context_snapshot(
    task: &Task,
    input: &WriterCorrectionAssignmentInput,
) -> Result<(
    WriterCorrectionAssignmentInput,
    WriterCorrectionAssignmentInput,
    Vec<u8>,
)> {
    anyhow::ensure!(
        mutable_assignment_context(task, &input.artifact),
        "input is not recognized mutable assignment context"
    );
    let provenance_bytes = serde_json::to_vec_pretty(input)?;
    let identity = format!("{:x}", Sha256::digest(&provenance_bytes));
    let base = work_model::work_artifact_path(
        &task.work_item_id,
        task.attempt_id
            .as_deref()
            .context("assignment Attempt is missing")?,
        &format!("assignment-context/{identity}"),
    );
    Ok((
        WriterCorrectionAssignmentInput {
            artifact: ArtifactRef {
                producer_id: input.artifact.producer_id.clone(),
                path: format!("{base}/input"),
            },
            digest: input.digest.clone(),
        },
        WriterCorrectionAssignmentInput {
            artifact: ArtifactRef {
                producer_id: "writer-assignment-context".into(),
                path: format!("{base}/source.json"),
            },
            digest: format!("sha256:{:x}", Sha256::digest(&provenance_bytes)),
        },
        provenance_bytes,
    ))
}

/// Describe a stale mutable binding; this is guidance, never recovery authority.
pub(crate) fn assignment_context_recovery_action(
    root: &Path,
    attempt: &work_model::Attempt,
) -> Result<Option<String>> {
    if attempt.status != work_model::AttemptStatus::NeedsUser
        || attempt.pause_kind != Some(work_model::PauseKind::WriterCompletion)
        || attempt.cancellation_requested()
    {
        return Ok(None);
    }
    let Some(task) = attempt
        .tasks
        .iter()
        .rev()
        .find(|t| t.kind == TaskKind::Write)
        .filter(|t| t.status == TaskStatus::Complete)
    else {
        return Ok(None);
    };
    let candidate = &task
        .output
        .as_ref()
        .context("saved Writer has no candidate")?
        .commit;
    stale_assignment_context_action(root, attempt, task, candidate)
}

pub(crate) fn stale_assignment_context_action(
    root: &Path,
    attempt: &work_model::Attempt,
    task: &Task,
    candidate: &str,
) -> Result<Option<String>> {
    let Some(assignment) = task.writer_correction_assignment.as_ref() else {
        return Ok(None);
    };
    validate_assignment_context_recovery_inputs(root, task, assignment)?;
    for input in &assignment.source_task_inputs {
        if mutable_assignment_context(task, &input.artifact)
            && assignment_input_digest(root, &input.artifact.path)? != input.digest
        {
            let quote = |s: &str| format!("'{}'", s.replace('\'', "'\"'\"'"));
            return Ok(Some(format!(
                "fluent attempt recover-assignment-context {} {} --task {} --candidate {} --assignment-digest {} --input {} --original-file '<original-bytes-file>' --resolution '<describe the exact-byte recovery>'",
                quote(&task.work_item_id),
                quote(&attempt.id),
                quote(&task.id),
                quote(candidate),
                quote(&assignment_digest(assignment)?),
                quote(&input.artifact.path)
            )));
        }
    }
    Ok(None)
}

/// Read a bounded original-byte file through canonical, nonsymlink paths.
pub(crate) fn assignment_original_bytes(root: &Path, original_file: &Path) -> Result<Vec<u8>> {
    let original_path = if original_file.is_absolute() {
        original_file.to_path_buf()
    } else {
        root.join(original_file)
    };
    anyhow::ensure!(
        original_path
            .components()
            .collect::<std::path::PathBuf>()
            .as_os_str()
            == original_path.as_os_str(),
        "original file path is not canonical"
    );
    let mut path = std::path::PathBuf::new();
    for component in original_path.components() {
        anyhow::ensure!(
            !matches!(
                component,
                std::path::Component::ParentDir | std::path::Component::CurDir
            ),
            "unsafe original file path"
        );
        path.push(component);
        anyhow::ensure!(
            !fs::symlink_metadata(&path)?.file_type().is_symlink(),
            "original file path traverses a symlink"
        );
    }
    let metadata = fs::symlink_metadata(&original_path)?;
    anyhow::ensure!(
        metadata.is_file() && metadata.len() <= 16 * 1024 * 1024,
        "unsupported recovery file: {}",
        original_path.display()
    );
    let bytes = fs::read(&original_path)?;
    anyhow::ensure!(
        bytes.len() <= 16 * 1024 * 1024,
        "original bytes exceed the supported bound"
    );
    Ok(bytes)
}

pub(crate) fn assignment_digest(assignment: &WriterCorrectionAssignment) -> Result<String> {
    Ok(format!(
        "sha256:{:x}",
        Sha256::digest(serde_json::to_vec(assignment)?)
    ))
}

pub(crate) fn assignment_attempt_digest(attempt: &work_model::Attempt) -> Result<String> {
    Ok(format!(
        "sha256:{:x}",
        Sha256::digest(serde_json::to_vec(attempt)?)
    ))
}

/// Check settled ownership and candidate identity without publishing anything.
pub(crate) fn assignment_context_recovery_target<'a>(
    root: &Path,
    item: &'a work_model::WorkItem,
    attempt_id: &str,
    task_id: &str,
    candidate: &str,
) -> Result<(&'a work_model::Attempt, &'a Task)> {
    item.ensure_mutation_compatible()?;
    item.validate()?;
    let attempt = item
        .attempts
        .iter()
        .find(|a| a.id == attempt_id)
        .context("Attempt is missing")?;
    let task = attempt
        .tasks
        .iter()
        .find(|t| t.id == task_id)
        .context("recovery requires the latest completed Writer")?;
    let output_free =
        task.kind == TaskKind::Write && task.status == TaskStatus::Failed && task.output.is_none();
    let output = if output_free {
        output_free_assignment_source(root, item, attempt, task)?
            .output
            .as_ref()
            .unwrap()
    } else {
        anyhow::ensure!(attempt.tasks.iter().rev().find(|t| t.kind == TaskKind::Write)
            .is_some_and(|latest| latest.id == task_id && latest.status == TaskStatus::Complete),
            "recovery requires the latest completed Writer");
        task.output.as_ref().context("Writer has no candidate")?
    };
    anyhow::ensure!(
        !item
            .merge_candidates
            .iter()
            .any(|c| c.attempt_id == attempt_id),
        "Attempt already has a Merge Candidate"
    );
    anyhow::ensure!(
        output.commit == candidate
            && attempt.current_candidate_commit() == Some(candidate)
            && candidate_workspace_is_clean_at_managed(root, output, candidate),
        "candidate is stale, dirty, or not a managed worktree"
    );
    for task in &attempt.tasks {
        anyhow::ensure!(
            output_free || matches!(task.status, TaskStatus::Complete | TaskStatus::Superseded),
            "Attempt has unresolved Tasks"
        );
        anyhow::ensure!(
            !crate::lease::is_leased(&crate::lease::task_lock_path(root, &item.id, &task.id)),
            "Task has a live owner"
        );
        crate::usage_limit_contracts::unlocked(
            &crate::lease::task_lock_path(root, &item.id, &task.id),
            false,
        )?;
        if let Some(area) = &task.artifact_area {
            crate::usage_limit_contracts::unlocked(
                &root.join(&area.path).join("execution-owner.lock"),
                false,
            )
            .context("Task has a live execution owner or invalid ownership lock")?;
            let record = crate::execution::record_path_for_artifact(root, &area.path);
            anyhow::ensure!(
                !crate::execution::execution_is_live(&record)?
                    && !crate::lease::is_leased(
                        &root.join(&area.path).join("execution-owner.lock")
                    ),
                "Task has a live execution owner"
            );
        }
    }
    anyhow::ensure!(
        !crate::lease::is_leased(&crate::lease::learner_lock_path(root, &item.id, attempt_id)),
        "Learner has a live owner"
    );
    crate::usage_limit_contracts::unlocked(
        &crate::lease::learner_lock_path(root, &item.id, attempt_id),
        false,
    )?;
    Ok((attempt, task))
}

/// Admit only the historical completion retry that stopped before provider launch.
/// Native execution identity and settled ownership supply proof; log prose does not.
pub(crate) fn output_free_assignment_source<'a>(
    root: &Path,
    item: &work_model::WorkItem,
    attempt: &'a work_model::Attempt,
    task: &Task,
) -> Result<&'a Task> {
    let assignment = task
        .writer_correction_assignment
        .as_ref()
        .context("output-free Writer has no assignment")?;
    anyhow::ensure!(
        task.kind == TaskKind::Write && task.status == TaskStatus::Failed && task.output.is_none(),
        "context recovery requires an output-free failed Writer"
    );
    let source = attempt
        .tasks
        .iter()
        .rev()
        .find(|t| t.kind == TaskKind::Write && t.status == TaskStatus::Complete)
        .context("output-free Writer has no completed source")?;
    anyhow::ensure!(
        source.id == assignment.source_task_id
            && source
                .output
                .as_ref()
                .is_some_and(|o| o.commit == assignment.source_candidate_commit)
            && task.workspace_access.writes == source.workspace_access.writes,
        "output-free Writer source candidate or workspace changed"
    );
    let context = writer_completion::validated_writer_context(root, item, task)?
        .context("output-free Writer has no completion-recovery authority")?;
    anyhow::ensure!(
        context.source_task_id == source.id
            && context.candidate_commit == assignment.source_candidate_commit
            && context.artifacts.iter().any(|a| a.path
                == work_model::work_artifact_path(
                    &item.id,
                    &attempt.id,
                    &format!("writer-completion-recoveries/{}/recovery.json", task.id)
                )),
        "output-free Writer completion authority changed"
    );
    let index = attempt
        .tasks
        .iter()
        .position(|t| t.id == task.id)
        .context("output-free Writer is missing")?;
    anyhow::ensure!(
        attempt.tasks[..index]
            .iter()
            .all(|t| matches!(t.status, TaskStatus::Complete | TaskStatus::Superseded)),
        "Attempt has unresolved Tasks"
    );
    validate_unstarted_writer_reservation(root, &attempt.tasks[index + 1..])?;
    let area = task
        .artifact_area
        .as_ref()
        .context("output-free Writer has no artifact identity")?;
    let record: crate::execution::ExecutionRecord = serde_json::from_slice(
        &assignment_input_bytes(root, &format!("{}/execution.json", area.path))?,
    )?;
    anyhow::ensure!(
        matches!(record.schema_version, 1 | 2)
            && (record.state == crate::execution::ExecutionState::Launching
                || (record.state == crate::execution::ExecutionState::Canceled
                    && attempt.cancellation_requested()
                    && attempt
                        .recovery_operations
                        .iter()
                        .any(|r| r.operation_kind == "assignment-context-recovery"
                            && r.task_id.as_deref() == Some(task.id.as_str()))))
            && record.leader_pid == 0
            && record.process_group_id == 0
            && (record.settled_at.is_none() || attempt.cancellation_requested())
            && record.heartbeat_at == record.started_at
            && (record.schema_version == 1
                || (record.launch_generation.is_some()
                    && crate::execution::receipt_absent(
                        &root.join(&area.path).join("execution.json")
                    )?)),
        "setup cannot be proven unlaunched"
    );
    validate_failed_preparation_owner(root, item, attempt, task, &record)?;
    Ok(source)
}

/// Bind the reserved native generation to this failed preparation and dead owner.
fn validate_failed_preparation_owner(
    root: &Path,
    item: &work_model::WorkItem,
    attempt: &work_model::Attempt,
    task: &Task,
    record: &crate::execution::ExecutionRecord,
) -> Result<()> {
    let start = DateTime::parse_from_rfc3339(
        task.started_at
            .as_deref()
            .context("failed Writer has no start")?,
    )?;
    let end = DateTime::parse_from_rfc3339(
        task.completed_at
            .as_deref()
            .context("failed Writer has no completion")?,
    )?;
    let prepared = DateTime::parse_from_rfc3339(&record.started_at)?;
    anyhow::ensure!(
        prepared <= start
            && start <= end
            && start.signed_duration_since(prepared).num_seconds() == 0,
        "execution identity does not bind the failed setup"
    );
    let owner = record
        .attempt_owner
        .as_ref()
        .context("failed setup has no Attempt owner")?;
    anyhow::ensure!(
        owner.project_root == root.canonicalize()?
            && owner.work_item_id == item.id
            && owner.attempt_id == attempt.id,
        "failed setup belongs to another Attempt"
    );
    anyhow::ensure!(
        record.owner_pid > 0 && record.owner_pid <= i32::MAX as u32,
        "invalid setup owner"
    );
    let result = unsafe { libc::kill(record.owner_pid as i32, 0) };
    anyhow::ensure!(
        result == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH),
        "setup owner is live or unverifiable"
    );
    Ok(())
}

/// Share only reservation shape, not the distinct native receipt contracts.
pub(crate) fn validate_unstarted_writer_reservation(root: &Path, later: &[Task]) -> Result<()> {
    anyhow::ensure!(
        later.len() <= 1
            && later.iter().all(|t| t.kind == TaskKind::Write
                && t.status == TaskStatus::Planned
                && t.started_at.is_none()
                && t.completed_at.is_none()
                && t.output.is_none()),
        "unsupported later work after output-free Writer"
    );
    for later in later {
        if let Some(area) = &later.artifact_area {
            match fs::symlink_metadata(root.join(&area.path)) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
                Ok(_) => bail!("later Writer reservation has materialized artifacts"),
            }
        } else {
            bail!("later Writer reservation has no artifact identity");
        }
    }
    Ok(())
}

pub(crate) fn output_free_context_binding(
    root: &Path,
    item: &work_model::WorkItem,
    attempt: &work_model::Attempt,
    task: &Task,
) -> Result<Option<work_model::OutputFreeAssignmentContext>> {
    if task.status == TaskStatus::Complete {
        return Ok(None);
    }
    output_free_assignment_source(root, item, attempt, task)?;
    let mut files = BTreeMap::new();
    crate::usage_limit_contracts::bind_directory(
        root,
        &task.artifact_area.as_ref().unwrap().path,
        &mut files,
    )?;
    let context = writer_completion::validated_writer_context(root, item, task)?
        .context("missing completion authority")?;
    for artifact in context.artifacts {
        crate::usage_limit_contracts::bind_file(root, &artifact.path, &mut files)?;
    }
    let index = attempt.tasks.iter().position(|t| t.id == task.id).unwrap();
    Ok(Some(work_model::OutputFreeAssignmentContext {
        task: task.clone(),
        later_tasks: attempt.tasks[index + 1..].to_vec(),
        files,
    }))
}

pub(crate) fn validate_output_free_context_files(
    root: &Path,
    audit: &work_model::AssignmentContextRecoveryAudit,
) -> Result<()> {
    if let Some(failure) = &audit.output_free {
        for (path, digest) in &failure.files {
            anyhow::ensure!(
                assignment_input_digest(root, path)? == *digest,
                "output-free failure evidence changed: {path}"
            );
        }
    }
    Ok(())
}

/// A partial recovery leaves every other mutable binding unresolved. Immutable
/// source inputs and finding evidence always retain their original digests.
pub(crate) fn validate_assignment_context_recovery_inputs(
    root: &Path,
    task: &Task,
    assignment: &WriterCorrectionAssignment,
) -> Result<()> {
    assignment
        .validate_for_task(task)
        .map_err(anyhow::Error::msg)?;
    for input in &assignment.source_task_inputs {
        if mutable_assignment_context(task, &input.artifact) {
            assignment_input_bytes(root, &input.artifact.path)?;
        } else {
            anyhow::ensure!(
                assignment_input_digest(root, &input.artifact.path)? == input.digest,
                "Writer assignment input changed: {}",
                input.artifact.path
            );
        }
    }
    for input in &assignment.evidence {
        anyhow::ensure!(
            assignment_input_digest(root, &input.artifact.path)? == input.digest,
            "Writer assignment input changed: {}",
            input.artifact.path
        );
    }
    Ok(())
}

/// Validate the complete immutable audit at publication and when a later
/// consumer follows the same authorized assignment substitution.
fn validate_assignment_context_audit(
    root: &Path,
    task: &Task,
    audit_path: &str,
    audit_bytes: &[u8],
    audit: &work_model::AssignmentContextRecoveryAudit,
    record: &work_model::RecoveryOperationRecord,
) -> Result<()> {
    anyhow::ensure!(
        (audit.schema_version == 1 && audit.output_free.is_none())
            || (audit.schema_version == 2 && audit.output_free.is_some()),
        "unsupported context recovery audit"
    );
    audit
        .before
        .validate_for_task(task)
        .map_err(anyhow::Error::msg)?;
    audit
        .after
        .validate_for_task(task)
        .map_err(anyhow::Error::msg)?;
    if let Some(failure) = &audit.output_free {
        anyhow::ensure!(
            failure.task.id == task.id
                && failure.task.status == TaskStatus::Failed
                && failure.task.output.is_none()
                && failure.task.writer_correction_assignment.as_ref() == Some(&audit.before),
            "output-free recovery lost its original failed Task"
        );
    }
    let attempt_id = task
        .attempt_id
        .as_deref()
        .context("assignment Attempt is missing")?;
    let material = serde_json::to_vec(&(
        assignment_digest(&audit.before)?,
        audit.original.artifact.path.as_str(),
        audit.original.digest.as_str(),
    ))?;
    let mut operation = work_model::RecoveryOperationRequest::new(
        "assignment-context-recovery",
        &task.work_item_id,
        attempt_id,
        if audit.output_free.is_some() {
            Some(audit.before.source_candidate_commit.clone())
        } else {
            task.output.as_ref().map(|o| o.commit.clone())
        },
        Some(task.id.clone()),
        None,
        None,
        record.authorization.clone(),
        Some(format!("sha256:{:x}", Sha256::digest(material))),
    );
    anyhow::ensure!(
        operation.operation_id() == audit.request_id
            && audit_path
                == work_model::work_artifact_path(
                    &task.work_item_id,
                    attempt_id,
                    &format!("assignment-context-recoveries/{}.json", audit.request_id)
                ),
        "recovery request changed"
    );
    operation.input_digest = Some(format!("sha256:{:x}", Sha256::digest(&audit_bytes)));
    anyhow::ensure!(
        record.matches_request(&operation),
        "recovery operation changed"
    );
    let (snapshot, provenance, provenance_bytes) =
        assignment_context_snapshot(task, &audit.original)?;
    let mut expected = audit.before.clone();
    expected.schema_version = work_model::WRITER_CORRECTION_ASSIGNMENT_SCHEMA_VERSION;
    let input = expected
        .source_task_inputs
        .iter_mut()
        .find(|input| **input == audit.original)
        .context("recovery source input is not assigned")?;
    *input = snapshot.clone();
    expected.evidence.push(provenance.clone());
    anyhow::ensure!(
        expected == audit.after,
        "recovery changed unrelated assignment authority"
    );
    validate_assignment_context_snapshot(root, &snapshot, &provenance, &provenance_bytes)
}

fn validate_assignment_context_snapshot(
    root: &Path,
    snapshot: &WriterCorrectionAssignmentInput,
    provenance: &WriterCorrectionAssignmentInput,
    provenance_bytes: &[u8],
) -> Result<()> {
    anyhow::ensure!(
        assignment_input_digest(root, &snapshot.artifact.path)? == snapshot.digest
            && assignment_input_bytes(root, &provenance.artifact.path)? == provenance_bytes,
        "recovered snapshot changed"
    );
    Ok(())
}

/// Preserve a historical evidence binding only through recorded exact-byte
/// recoveries. This grants no finding acceptance or lifecycle authority.
fn validate_assignment_context_lineage(
    root: &Path,
    item: &work_model::WorkItem,
    task: &Task,
    original: &WriterCorrectionAssignment,
) -> Result<()> {
    let current = task
        .writer_correction_assignment
        .as_ref()
        .context("Writer has no saved assignment")?;
    if current == original {
        return Ok(());
    }
    let attempt = item
        .attempts
        .iter()
        .find(|a| Some(a.id.as_str()) == task.attempt_id.as_deref())
        .context("assignment Attempt is missing")?;
    let mut retained = original.clone();
    for artifact in attempt
        .artifacts
        .iter()
        .filter(|a| a.producer_id == "assignment-context-recovery")
    {
        let bytes = assignment_input_bytes(root, &artifact.path)?;
        let audit: work_model::AssignmentContextRecoveryAudit = serde_json::from_slice(&bytes)?;
        if audit.before != retained {
            continue;
        }
        let digest = format!("sha256:{:x}", Sha256::digest(&bytes));
        let record = attempt
            .recovery_operations
            .iter()
            .find(|r| {
                r.operation_kind == "assignment-context-recovery"
                    && r.task_id.as_deref() == Some(task.id.as_str())
                    && r.input_digest.as_deref() == Some(digest.as_str())
            })
            .context("assignment context audit has no matching recovery operation")?;
        validate_assignment_context_audit(root, task, &artifact.path, &bytes, &audit, record)?;
        retained = audit.after;
    }
    anyhow::ensure!(
        retained == *current,
        "assignment changed without exact context recovery authority"
    );
    Ok(())
}

/// Allow only the exact context substitution recorded by a public recovery.
/// Generic aggregate writes still cannot replace findings, digests, or history.
pub(crate) fn audited_assignment_context_change(
    root: &Path,
    prior: &work_model::Attempt,
    target: &work_model::Attempt,
    before_task: &Task,
    after_task: &Task,
) -> Result<bool> {
    let Some(before) = before_task.writer_correction_assignment.as_ref() else {
        return Ok(false);
    };
    let Some(after) = after_task.writer_correction_assignment.as_ref() else {
        return Ok(false);
    };
    let Some(record) = target.recovery_operations.last().filter(|r| {
        r.operation_kind == "assignment-context-recovery"
            && r.task_id.as_deref() == Some(before_task.id.as_str())
            && !prior
                .recovery_operations
                .iter()
                .any(|old| old.operation_id == r.operation_id)
    }) else {
        return Ok(false);
    };
    let audit_path = target
        .artifacts
        .last()
        .filter(|a| a.producer_id == "assignment-context-recovery")
        .context("recovery has no audit reference")?
        .path
        .clone();
    let audit_bytes = assignment_input_bytes(root, &audit_path)?;
    let audit: work_model::AssignmentContextRecoveryAudit = serde_json::from_slice(&audit_bytes)?;
    anyhow::ensure!(
        matches!(audit.schema_version, 1 | 2)
            && &audit.before == before
            && &audit.after == after
            && audit.attempt_digest == assignment_attempt_digest(prior)?,
        "assignment recovery audit does not match the saved state"
    );
    validate_assignment_context_audit(
        root,
        before_task,
        &audit_path,
        &audit_bytes,
        &audit,
        record,
    )?;
    validate_output_free_context_files(root, &audit)?;
    let mut restored = target.clone();
    restored.recovery_operations.pop();
    let audit_ref = ArtifactRef {
        producer_id: "assignment-context-recovery".into(),
        path: audit_path,
    };
    anyhow::ensure!(
        restored.artifacts.pop() == Some(audit_ref),
        "recovery lost its audit reference"
    );
    *restored
        .tasks
        .iter_mut()
        .find(|t| t.id == before_task.id)
        .context("recovery Task missing")? = before_task.clone();
    restored.pause_record = prior.pause_record.clone(); // The host refreshes diagnostic guidance on save.
    anyhow::ensure!(
        restored == *prior,
        "recovery changed Attempt history or lifecycle state"
    );
    let mut expected_task = before_task.clone();
    expected_task.writer_correction_assignment = Some(after.clone());
    Ok(expected_task == *after_task)
}

pub(crate) fn assignment_input_digest(project_root: &Path, path: &str) -> Result<String> {
    Ok(format!(
        "sha256:{:x}",
        Sha256::digest(assignment_input_bytes(project_root, path)?)
    ))
}

pub(crate) fn assignment_input_bytes(project_root: &Path, path: &str) -> Result<Vec<u8>> {
    let resolved = resolve_input_artifact_path(project_root, path)?;
    let relative = Path::new(path);
    anyhow::ensure!(
        relative.components().collect::<PathBuf>().as_os_str() == relative.as_os_str(),
        "Writer assignment input path is not canonical: {path}"
    );
    let mut current = project_root.to_path_buf();
    for component in relative.components() {
        current.push(component);
        let metadata = fs::symlink_metadata(&current)?;
        anyhow::ensure!(
            !metadata.file_type().is_symlink()
                && (if current == resolved {
                    metadata.is_file() && metadata.len() <= 16 * 1024 * 1024
                } else {
                    metadata.is_dir()
                }),
            "Writer assignment input has an unsafe path or file type: {path}"
        );
    }
    let bytes = fs::read(&resolved)
        .with_context(|| format!("read Writer assignment input: {}", resolved.display()))?;
    anyhow::ensure!(
        bytes.len() <= 16 * 1024 * 1024,
        "Writer assignment input exceeds the supported bound: {path}"
    );
    Ok(bytes)
}

pub(crate) fn findings_from_inputs(
    project_root: &Path,
    inputs: &[ArtifactRef],
    include_tester: bool,
    assessment: Option<&crate::verification_assessment::VerificationAssessment>,
) -> Result<Vec<WriterCompletionFinding>> {
    let mut findings = BTreeMap::new();
    // A Tester-harness repair is authorized by its explicit operator decision.
    // Its ordinary command failures are not product obligations for the scoped
    // repair and therefore do not enter this matrix.
    if !include_tester {
        return Ok(vec![]);
    }
    for artifact in inputs {
        let relative = Path::new(&artifact.path);
        let is_review = relative.file_name().is_some_and(|name| {
            name == "review.md" || name.to_string_lossy().starts_with("review-")
        });
        if !is_review {
            continue;
        }
        let path = if Path::new(&artifact.path).is_absolute() {
            PathBuf::from(&artifact.path)
        } else {
            resolve_input_artifact_path(project_root, &artifact.path)?
        };
        let source = fs::read_to_string(&path).with_context(|| {
            format!(
                "read review findings for Writer completion matrix: {}",
                path.display()
            )
        })?;
        let role = review::REVIEWERS
            .iter()
            .find(|role| {
                artifact.producer_id.contains(**role)
                    || artifact.path.contains(&format!("review-{role}"))
            })
            .copied()
            .unwrap_or("unknown");
        for title in review::open_finding_titles(&source) {
            let identity = review::finding_identity(role, &title);
            findings
                .entry((role.to_string(), identity.clone()))
                .or_insert(WriterCompletionFinding {
                    id: format!("finding:{role}:{identity}"),
                    role: role.to_string(),
                    source: artifact.path.clone(),
                    requirement: title,
                });
        }
    }
    for artifact in inputs {
        let relative = Path::new(&artifact.path);
        let is_candidate_gate = relative
            .file_name()
            .is_some_and(|name| name == candidate_gate::CANDIDATE_GATE_FILE);
        if !is_candidate_gate {
            continue;
        }
        let path = if Path::new(&artifact.path).is_absolute() {
            PathBuf::from(&artifact.path)
        } else {
            resolve_input_artifact_path(project_root, &artifact.path)?
        };
        let source = fs::read(&path).with_context(|| {
            format!(
                "read candidate gate findings for Writer completion matrix: {}",
                path.display()
            )
        })?;
        let evidence = serde_json::from_slice::<candidate_gate::CandidateGateEvidence>(&source)
            .with_context(|| {
                format!(
                    "parse candidate gate findings for Writer completion matrix: {}",
                    path.display()
                )
            })?;
        if evidence.disposition != candidate_gate::CandidateGateDisposition::Rejected {
            continue;
        }
        for check in evidence
            .checks
            .iter()
            .filter(|check| check.status == candidate_gate::CandidateCheckStatus::Failed)
        {
            if matches!(
                check.name.as_str(),
                "tester-reference-evidence" | "behavior-references"
            ) && assessment.is_some()
            {
                continue;
            }
            let role = "candidate-gate";
            let identity = review::finding_identity(
                role,
                &format!("{}:{}", evidence.candidate_commit, check.name),
            );
            findings
                .entry((role.to_string(), identity.clone()))
                .or_insert(WriterCompletionFinding {
                    id: format!("finding:{role}:{identity}"),
                    role: role.into(),
                    source: artifact.path.clone(),
                    requirement: format!(
                        "{} check remains unresolved: {}",
                        check.name,
                        check.diagnostics.join("; ")
                    ),
                });
        }
    }
    if let Some(assessment) = assessment {
        for row in assessment.candidate_defects() {
            let role = "verification";
            findings
                .entry((role.into(), row.id.clone()))
                .or_insert(WriterCompletionFinding {
                    id: format!("finding:verification:{}", row.id),
                    role: role.into(),
                    source: row.source.clone(),
                    requirement: row.diagnostics.join("; "),
                });
        }
    } else {
        for artifact in inputs
            .iter()
            .filter(|artifact| artifact.path.ends_with("/tester-results.json"))
        {
            // Without bound current facts, keep one unknown source obligation.
            // A historical exit alone cannot identify a source correction.
            let path = if Path::new(&artifact.path).is_absolute() {
                PathBuf::from(&artifact.path)
            } else {
                resolve_input_artifact_path(project_root, &artifact.path)?
            };
            let results: crate::tester::TesterResults = serde_json::from_slice(&fs::read(&path)?)?;
            if results.error.is_some()
                || results
                    .commands
                    .iter()
                    .any(|command| command.exit_code != 0)
                || results.tests.iter().any(|test| test.status == "fail")
            {
                let role = "verification";
                let identity = review::finding_identity(role, &artifact.path);
                findings.entry((role.into(), identity.clone())).or_insert(WriterCompletionFinding {
                    id: format!("finding:{role}:{identity}"), role: role.into(), source: artifact.path.clone(),
                    requirement: "Historical Tester evidence needs diagnosis and candidate binding before source correction.".into(),
                });
            }
        }
    }
    Ok(findings.into_values().collect())
}

/// Return the current source-correction assignment after launch admission has
/// validated the complete recovery. Recheck its exact audit bytes because the
/// prompt is built after the Task reservation.
pub(crate) fn classified_writer_delivery(
    project_root: &Path,
    item: &work_model::WorkItem,
    task_id: &str,
) -> Result<Option<(String, PathBuf)>> {
    let Some((recovery, classification)) = item
        .attempts
        .iter()
        .flat_map(|attempt| &attempt.evidence_recoveries)
        .filter_map(|recovery| {
            recovery
                .code_change_classification
                .as_ref()
                .map(|c| (recovery, c))
        })
        .find(|(_, classification)| classification.writer_task_id == task_id)
    else {
        return Ok(None);
    };
    let actual_digest = format!(
        "sha256:{:x}",
        Sha256::digest(classification.authorization.as_bytes())
    );
    anyhow::ensure!(
        actual_digest == classification.authorization_digest,
        "classification authorization digest changed"
    );
    let attempt_id = item
        .attempts
        .iter()
        .find(|attempt| attempt.tasks.iter().any(|task| task.id == task_id))
        .map(|attempt| attempt.id.as_str())
        .context("classified Writer Attempt is missing")?;
    let expected = classification_audit_bytes(
        &item.id,
        attempt_id,
        recovery,
        classification,
        !classification_uses_legacy_identity(classification, attempt_id, recovery),
    )?;
    let audit = validate_managed_file_path(project_root, &classification.audit_artifact.path)?;
    anyhow::ensure!(
        fs::read(&audit)
            .with_context(|| format!("cannot read classification audit {}", audit.display()))?
            == expected,
        "classification audit bytes changed"
    );
    Ok(Some((classification.authorization.clone(), audit)))
}

/// Revalidate a classified source-correction Writer immediately before it is
/// reserved for execution. Ordinary Writers do not carry a matching
/// classification and pass through unchanged.
pub(crate) fn validate_code_change_writer_start(
    project_root: &Path,
    item: &work_model::WorkItem,
    attempt_id: &str,
    task_id: &str,
) -> std::result::Result<(), WorkModelError> {
    let attempt = item
        .attempts
        .iter()
        .find(|attempt| attempt.id == attempt_id)
        .ok_or_else(|| code_change_writer_rejection(attempt_id, "Attempt is not present"))?;
    let (writer_index, writer) = attempt
        .tasks
        .iter()
        .enumerate()
        .find(|(_, task)| task.id == task_id && task.kind == TaskKind::Write)
        .ok_or_else(|| code_change_writer_rejection(attempt_id, "classified Writer is missing"))?;

    let classified = attempt
        .evidence_recoveries
        .iter()
        .rev()
        .find_map(|recovery| {
            recovery
                .code_change_classification
                .as_ref()
                .filter(|classification| classification.writer_task_id == task_id)
                .map(|classification| (recovery, classification))
        });
    let pending_classification_for_other_writer = attempt
        .evidence_recoveries
        .iter()
        .rev()
        .find_map(|recovery| {
            recovery
                .code_change_classification
                .as_ref()
                .filter(|classification| {
                    classification.writer_task_id != task_id
                        && attempt.tasks.iter().any(|task| {
                            task.id == classification.writer_task_id
                                && task.kind == TaskKind::Write
                                && task.status == TaskStatus::Planned
                                && task.output.is_none()
                        })
                })
        });
    let Some((recovery, classification)) = classified else {
        if pending_classification_for_other_writer.is_some() {
            return Err(code_change_writer_rejection(
                attempt_id,
                "another classified Writer owns the planned continuation",
            ));
        }
        if writer_completion::validated_committed_repair_start(project_root, item, writer)
            .map_err(|error| code_change_writer_rejection(attempt_id, format!("{error:#}")))?
        {
            return Ok(());
        }
        validate_verification_writer_start(project_root, attempt, &attempt.tasks[writer_index])
            .map_err(|error| code_change_writer_rejection(attempt_id, format!("{error:#}")))?;
        return Ok(());
    };

    let rejected = |reason: &str| code_change_writer_rejection(attempt_id, reason);
    validate_classification_recovery_scope(attempt, &recovery.id)
        .map_err(|reason| rejected(&reason))?;
    if item.attempts.last().map(|current| current.id.as_str()) != Some(attempt_id)
        || attempt.status != work_model::AttemptStatus::Planned
        || attempt.review_state != Some(work_model::AttemptReviewState::Failed)
        || attempt.pause_kind.is_some()
        || attempt.pause_recovery.is_some()
        || attempt.completed_at.is_some()
    {
        return Err(rejected(
            "classified source-correction Attempt is not the current planned frontier",
        ));
    }
    if recovery.state != EvidenceRecoveryState::CodeChange
        || writer.status != TaskStatus::Planned
        || writer.output.is_some()
        || writer.role != "author"
        || writer.work_item_id != item.id
        || writer.attempt_id.as_deref() != Some(attempt_id)
        || writer_index + 1 != attempt.tasks.len()
    {
        return Err(rejected(
            "classified Writer is not the final planned continuation",
        ));
    }
    if attempt.tasks.iter().enumerate().any(|(index, task)| {
        index != writer_index
            && (task.status == TaskStatus::Executing
                || task.status == TaskStatus::NeedsUser
                || (task.status == TaskStatus::Failed
                    && (task.kind != TaskKind::Write || task.output.is_some()))
                || task.status == TaskStatus::Planned)
    }) {
        return Err(rejected("Attempt contains another live or unresolved Task"));
    }
    if item.merge_candidates.iter().any(|candidate| {
        candidate.attempt_id == attempt_id
            && candidate.candidate_commit == recovery.candidate_commit
    }) {
        return Err(rejected("a Merge Candidate already owns this candidate"));
    }

    let source_output = attempt
        .tasks
        .iter()
        .take(writer_index)
        .rev()
        .find(|task| task.kind == TaskKind::Write && task.status == TaskStatus::Complete)
        .and_then(|task| task.output.as_ref())
        .ok_or_else(|| rejected("the source candidate Writer is missing"))?;
    if writer.workspace_access.writes.len() != 1
        || writer.workspace_access.writes[0].id != source_output.workspace_id
        || writer.workspace_access.writes[0].path != source_output.workspace_path
    {
        return Err(rejected(
            "the corrective Writer workspace is not candidate-bound",
        ));
    }

    let target = unique_tests_review_target(attempt, recovery, &classification.review_artifact)
        .map_err(|reason| rejected(&reason))?
        .target;
    let review_task_id = target
        .review_task_id
        .as_deref()
        .ok_or_else(|| rejected("classified recovery target has no review Task identity"))?;
    if target
        .review_digest
        .as_deref()
        .is_some_and(|digest| digest != classification.review_digest)
    {
        return Err(rejected("saved review digest changed"));
    }
    let review_task = attempt
        .tasks
        .iter()
        .find(|task| {
            task.id == review_task_id
                && task.kind == TaskKind::Review
                && task.role == "tests"
                && task.status == TaskStatus::Complete
                && task.work_item_id == item.id
                && task.attempt_id.as_deref() == Some(attempt_id)
                && review_artifact_path(task).as_deref()
                    == Some(classification.review_artifact.as_str())
                && task.review_context.as_ref().is_some_and(|context| {
                    context.candidate_commit == recovery.candidate_commit
                        && context.candidate_workspace_id == source_output.workspace_id
                        && context.candidate_workspace_path == source_output.workspace_path
                })
                && task
                    .evidence_review_context
                    .as_ref()
                    .is_some_and(|context| {
                        context.recovery_id == recovery.id
                            && context.candidate_commit == recovery.candidate_commit
                            && context.attachment == recovery.attachment
                            && context.prior_review_artifact == target.prior_review_artifact
                    })
        })
        .ok_or_else(|| rejected("bound Tests Review Task is missing or mismatched"))?;
    validate_code_change_candidate_binding(
        project_root,
        item,
        attempt,
        recovery,
        target,
        review_task,
        source_output,
        &recovery.candidate_commit,
        &classification.review_artifact,
        &recovery.attachment.digest,
    )
    .map_err(|reason| rejected(&reason))?;
    validate_classification_task_frontier(attempt, review_task_id, Some(task_id))
        .map_err(|reason| rejected(&reason))?;
    let evidence_ref = ArtifactRef {
        producer_id: "host-evidence".to_string(),
        path: recovery.attachment.snapshot_path.clone(),
    };
    if review_task
        .input_artifacts
        .iter()
        .filter(|input| *input == &evidence_ref)
        .count()
        != 1
        || review_task
            .input_artifacts
            .iter()
            .any(|input| input.path == evidence_ref.path && input != &evidence_ref)
    {
        return Err(rejected("bound Tests Review Task lost its evidence input"));
    }

    let review_path = validate_managed_file_path(project_root, &classification.review_artifact)
        .map_err(|error| {
            code_change_writer_rejection(
                attempt_id,
                format!("review artifact is not a managed regular file: {error}"),
            )
        })?;
    let review_bytes = fs::read(&review_path).map_err(|error| {
        code_change_writer_rejection(
            attempt_id,
            format!("cannot read bound review artifact: {error}"),
        )
    })?;
    let review_digest = format!("sha256:{:x}", Sha256::digest(&review_bytes));
    if review_digest != classification.review_digest {
        return Err(rejected("bound review bytes changed"));
    }
    let review_text = std::str::from_utf8(&review_bytes)
        .map_err(|_| rejected("bound review artifact is not valid UTF-8"))?;
    let disposition = review::extract_evidence_disposition(review_text);
    let legacy_disposition = review::has_unqualified_evidence_needed_disposition(review_text);
    if review::extract_verdict(review_text) != review::Verdict::Fail
        || (disposition != Some(review::EvidenceDisposition::EvidenceNeeded) && !legacy_disposition)
    {
        return Err(rejected("bound review no longer requests evidence-needed"));
    }

    let authorization_digest = format!(
        "sha256:{:x}",
        Sha256::digest(classification.authorization.as_bytes())
    );
    if authorization_digest != classification.authorization_digest {
        return Err(rejected("classification authorization digest changed"));
    }
    if classification.disposition != CODE_CHANGE_DISPOSITION {
        return Err(rejected("classification uses an unsupported disposition"));
    }
    let expected_operation_id = code_change_operation_id(
        attempt_id,
        &recovery.candidate_commit,
        &recovery.id,
        &classification.review_artifact,
        &classification.review_digest,
        &recovery.attachment.digest,
        &classification.disposition,
        &classification.authorization_digest,
    );
    let legacy_identity = classification_uses_legacy_identity(classification, attempt_id, recovery);
    if classification.operation_id != expected_operation_id && !legacy_identity {
        return Err(rejected("classification operation identity changed"));
    }
    let expected_writer_task_id = format!(
        "{attempt_id}-write-{}",
        attempt
            .tasks
            .iter()
            .filter(|task| task.kind == TaskKind::Write)
            .count()
    );
    if classification.writer_task_id != expected_writer_task_id {
        return Err(rejected("classification Writer identity changed"));
    }
    let expected_audit_artifact = ArtifactRef {
        producer_id: classification.operation_id.clone(),
        path: work_model::work_artifact_path(
            &item.id,
            attempt_id,
            &format!("{}/classification.json", classification.operation_id),
        ),
    };
    if classification.audit_artifact != expected_audit_artifact
        || attempt
            .artifacts
            .iter()
            .filter(|artifact| *artifact == &classification.audit_artifact)
            .count()
            != 1
    {
        return Err(rejected("classification audit binding changed"));
    }
    let audit_path = validate_managed_file_path(project_root, &classification.audit_artifact.path)
        .map_err(|error| {
            code_change_writer_rejection(
                attempt_id,
                format!("classification audit is not a managed regular file: {error}"),
            )
        })?;
    let audit_bytes = fs::read(&audit_path).map_err(|error| {
        code_change_writer_rejection(
            attempt_id,
            format!("cannot read classification audit: {error}"),
        )
    })?;
    let expected_audit = CodeChangeClassificationAudit {
        schema_version: 1,
        operation_id: &classification.operation_id,
        work_item_id: &item.id,
        attempt_id,
        candidate_commit: &recovery.candidate_commit,
        recovery_id: &recovery.id,
        evidence: &recovery.attachment,
        review_artifact: &classification.review_artifact,
        review_digest: &classification.review_digest,
        disposition: (!legacy_identity).then_some(classification.disposition.as_str()),
        authorization: &classification.authorization,
        authorization_digest: &classification.authorization_digest,
        writer_task_id: &classification.writer_task_id,
    };
    let expected_audit_bytes = serde_json::to_vec_pretty(&expected_audit)
        .map_err(|error| code_change_writer_rejection(attempt_id, error.to_string()))?;
    if audit_bytes != expected_audit_bytes {
        return Err(rejected("classification audit bytes changed"));
    }

    let expected_review_input = ArtifactRef {
        producer_id: review_task.id.clone(),
        path: classification.review_artifact.clone(),
    };
    let expected_refs = [
        expected_review_input,
        evidence_ref,
        classification.audit_artifact.clone(),
    ];
    for expected in expected_refs {
        if writer
            .input_artifacts
            .iter()
            .filter(|input| *input == &expected)
            .count()
            != 1
            || writer
                .input_artifacts
                .iter()
                .any(|input| input.path == expected.path && input != &expected)
        {
            return Err(rejected("corrective Writer input binding changed"));
        }
    }
    Ok(())
}

/// Fill the target digest on a legacy classified recovery after validating the
/// exact persisted Review and every replay binding against a cloned aggregate.
///
/// Older classified recoveries may omit `EvidenceReviewTarget.review_digest`,
/// even though the classification already stores the digest used to create the
/// operation and its audit. Derive the missing field from the managed Review
/// bytes, validate the candidate-bound replay with that field present, and only
/// then update the caller's aggregate. This keeps a rejected migration a
/// durable no-op and never changes the classification identity or audit.
/// Queue admission uses this replay-only contract so an interrupted corrective
/// Writer can be migrated before the Attempt loop reopens it. The strict
/// planned-frontier contract remains at the actual Writer launch.
pub(crate) fn migrate_legacy_code_change_review_digest(
    project_root: &Path,
    item: &mut work_model::WorkItem,
    attempt_id: &str,
    writer_task_id: Option<&str>,
) -> std::result::Result<bool, String> {
    let Some(attempt_index) = item
        .attempts
        .iter()
        .position(|attempt| attempt.id == attempt_id)
    else {
        return Ok(false);
    };
    let attempt = &item.attempts[attempt_index];
    let Some((recovery_index, classification)) = attempt
        .evidence_recoveries
        .iter()
        .enumerate()
        .rev()
        .find_map(|(index, recovery)| {
            let classification = recovery.code_change_classification.as_ref()?;
            (recovery.state == EvidenceRecoveryState::CodeChange
                && writer_task_id
                    .is_none_or(|writer_task_id| classification.writer_task_id == writer_task_id))
            .then_some((index, classification))
        })
    else {
        return Ok(false);
    };
    let review_artifact = classification.review_artifact.clone();
    let review_digest = classification.review_digest.clone();
    let recovery = &attempt.evidence_recoveries[recovery_index];
    let selected_target = unique_tests_review_target(attempt, recovery, &review_artifact)?;
    let target_index = selected_target.index;
    let target = selected_target.target;
    if target.review_digest.is_some() {
        return Ok(false);
    }
    let review_task_id = target
        .review_task_id
        .clone()
        .ok_or_else(|| "classified recovery target has no review Task identity".to_string())?;
    let review_path = validate_managed_file_path(project_root, &review_artifact)
        .map_err(|error| format!("review artifact is not a managed regular file: {error}"))?;
    let review_bytes = fs::read(&review_path)
        .map_err(|error| format!("cannot read bound review artifact: {error}"))?;
    let derived_review_digest = format!("sha256:{:x}", Sha256::digest(&review_bytes));
    if derived_review_digest != review_digest {
        return Err("bound review bytes changed".to_string());
    }

    let mut migrated = item.clone();
    migrated.attempts[attempt_index].evidence_recoveries[recovery_index].targets[target_index]
        .review_digest = Some(derived_review_digest.clone());
    let migrated_attempt = &migrated.attempts[attempt_index];
    let migrated_recovery = &migrated_attempt.evidence_recoveries[recovery_index];
    let migrated_classification = migrated_recovery
        .code_change_classification
        .as_ref()
        .expect("legacy classification remains present in cloned aggregate");
    validate_code_change_classification_replay(
        project_root,
        &migrated,
        migrated_attempt,
        migrated_recovery,
        migrated_classification,
        &review_task_id,
    )?;
    item.attempts[attempt_index].evidence_recoveries[recovery_index].targets[target_index]
        .review_digest = Some(derived_review_digest);
    Ok(true)
}

/// Validate an exact replay of an already-persisted source classification.
///
/// Classification mutates the model before a corrective Writer can be claimed,
/// so a retry may observe that Writer in a resumable lifecycle state. A hard
/// failure is not resumable without an explicit validated retry transition.
/// Keep this contract separate from the first-mutation frontier: replay checks
/// the stored operation and every immutable binding, while the frontier
/// validator below remains strict for a new classification.
pub(crate) fn validate_code_change_classification_replay(
    project_root: &Path,
    item: &work_model::WorkItem,
    attempt: &work_model::Attempt,
    recovery: &EvidenceRecovery,
    classification: &HostEvidenceClassification,
    review_task_id: &str,
) -> std::result::Result<(), String> {
    let rejected = |reason: &str| reason.to_string();
    if item.attempts.last().map(|current| current.id.as_str()) != Some(attempt.id.as_str()) {
        return Err(rejected("the classified Attempt is no longer current"));
    }
    validate_classification_recovery_scope(attempt, &recovery.id)?;
    if recovery.state != EvidenceRecoveryState::CodeChange {
        return Err(rejected(
            "stored classification recovery is not code-change",
        ));
    }
    if classification.disposition != CODE_CHANGE_DISPOSITION {
        return Err(rejected(
            "stored classification uses an unsupported disposition",
        ));
    }
    if item.merge_candidates.iter().any(|candidate| {
        candidate.attempt_id == attempt.id
            && candidate.candidate_commit == recovery.candidate_commit
    }) {
        return Err(rejected("a Merge Candidate already owns this candidate"));
    }

    let (writer_index, writer) = attempt
        .tasks
        .iter()
        .enumerate()
        .find(|(_, task)| task.id == classification.writer_task_id)
        .ok_or_else(|| rejected("classified Writer is missing"))?;
    if writer.kind != TaskKind::Write
        || writer.role != "author"
        || writer.work_item_id != item.id
        || writer.attempt_id.as_deref() != Some(attempt.id.as_str())
    {
        return Err(rejected("classified Writer identity changed"));
    }
    let lifecycle_is_replayable = match writer.status {
        TaskStatus::Planned | TaskStatus::Executing | TaskStatus::NeedsUser => {
            writer.output.is_none()
        }
        TaskStatus::Complete => writer.output.is_some(),
        TaskStatus::Failed => false,
        TaskStatus::Superseded => false,
    };
    if !lifecycle_is_replayable {
        return Err(rejected("classified Writer lifecycle is not replayable"));
    }

    let source_output = attempt
        .tasks
        .iter()
        .take(writer_index)
        .rev()
        .find(|task| task.kind == TaskKind::Write && task.status == TaskStatus::Complete)
        .and_then(|task| task.output.as_ref())
        .ok_or_else(|| rejected("the source candidate Writer is missing"))?;
    if writer.workspace_access.writes.len() != 1
        || writer.workspace_access.writes[0].id != source_output.workspace_id
        || writer.workspace_access.writes[0].path != source_output.workspace_path
    {
        return Err(rejected(
            "the corrective Writer workspace is not candidate-bound",
        ));
    }

    validate_classification_replay_task_frontier(attempt, review_task_id, writer_index)?;

    let target =
        unique_tests_review_target(attempt, recovery, &classification.review_artifact)?.target;
    if target.review_task_id.as_deref() != Some(review_task_id) {
        return Err(rejected(
            "classified recovery target does not match the stored Review",
        ));
    }
    if target
        .review_digest
        .as_deref()
        .is_some_and(|digest| digest != classification.review_digest)
    {
        return Err(rejected("saved review digest changed"));
    }
    let review_task = attempt
        .tasks
        .iter()
        .find(|task| {
            task.id == review_task_id
                && task.kind == TaskKind::Review
                && task.role == "tests"
                && task.status == TaskStatus::Complete
                && task.work_item_id == item.id
                && task.attempt_id.as_deref() == Some(attempt.id.as_str())
                && review_artifact_path(task).as_deref()
                    == Some(classification.review_artifact.as_str())
                && task.review_context.as_ref().is_some_and(|context| {
                    context.candidate_commit == recovery.candidate_commit
                        && context.candidate_workspace_id == source_output.workspace_id
                        && context.candidate_workspace_path == source_output.workspace_path
                })
                && task
                    .evidence_review_context
                    .as_ref()
                    .is_some_and(|context| {
                        context.recovery_id == recovery.id
                            && context.candidate_commit == recovery.candidate_commit
                            && context.attachment == recovery.attachment
                            && context.prior_review_artifact == target.prior_review_artifact
                    })
        })
        .ok_or_else(|| rejected("bound Tests Review Task is missing or mismatched"))?;
    match writer.status {
        TaskStatus::Complete => {
            let corrective_output = writer
                .output
                .as_ref()
                .ok_or_else(|| rejected("completed classified Writer has no output"))?;
            validate_code_change_post_correction_binding(
                project_root,
                item,
                attempt,
                recovery,
                target,
                review_task,
                source_output,
                corrective_output,
                &classification.review_artifact,
                &recovery.attachment.digest,
            )?;
        }
        TaskStatus::Planned | TaskStatus::Executing | TaskStatus::NeedsUser => {
            validate_code_change_candidate_binding(
                project_root,
                item,
                attempt,
                recovery,
                target,
                review_task,
                source_output,
                &recovery.candidate_commit,
                &classification.review_artifact,
                &recovery.attachment.digest,
            )?;
        }
        TaskStatus::Failed | TaskStatus::Superseded => {
            return Err(rejected("classified Writer lifecycle is not replayable"));
        }
    }
    let evidence_ref = ArtifactRef {
        producer_id: "host-evidence".to_string(),
        path: recovery.attachment.snapshot_path.clone(),
    };
    if review_task
        .input_artifacts
        .iter()
        .filter(|input| *input == &evidence_ref)
        .count()
        != 1
        || review_task
            .input_artifacts
            .iter()
            .any(|input| input.path == evidence_ref.path && input != &evidence_ref)
    {
        return Err(rejected("bound Tests Review Task lost its evidence input"));
    }

    let review_path = validate_managed_file_path(project_root, &classification.review_artifact)
        .map_err(|error| {
            rejected(&format!(
                "review artifact is not a managed regular file: {error}"
            ))
        })?;
    let review_bytes = fs::read(&review_path)
        .map_err(|error| rejected(&format!("cannot read bound review artifact: {error}")))?;
    if format!("sha256:{:x}", Sha256::digest(&review_bytes)) != classification.review_digest {
        return Err(rejected("bound review bytes changed"));
    }
    let review_text = std::str::from_utf8(&review_bytes)
        .map_err(|_| rejected("bound review artifact is not valid UTF-8"))?;
    let disposition = review::extract_evidence_disposition(review_text);
    let legacy_disposition = review::has_unqualified_evidence_needed_disposition(review_text);
    if review::extract_verdict(review_text) != review::Verdict::Fail
        || (disposition != Some(review::EvidenceDisposition::EvidenceNeeded) && !legacy_disposition)
    {
        return Err(rejected("bound review no longer requests evidence-needed"));
    }

    let authorization_digest = format!(
        "sha256:{:x}",
        Sha256::digest(classification.authorization.as_bytes())
    );
    if authorization_digest != classification.authorization_digest {
        return Err(rejected("classification authorization digest changed"));
    }
    let expected_operation_id = code_change_operation_id(
        &attempt.id,
        &recovery.candidate_commit,
        &recovery.id,
        &classification.review_artifact,
        &classification.review_digest,
        &recovery.attachment.digest,
        &classification.disposition,
        &classification.authorization_digest,
    );
    let legacy_identity =
        classification_uses_legacy_identity(classification, &attempt.id, recovery);
    if classification.operation_id != expected_operation_id && !legacy_identity {
        return Err(rejected("classification operation identity changed"));
    }
    let expected_writer_task_id = format!(
        "{}-write-{}",
        attempt.id,
        attempt.tasks[..=writer_index]
            .iter()
            .filter(|task| task.kind == TaskKind::Write)
            .count()
    );
    if classification.writer_task_id != expected_writer_task_id {
        return Err(rejected("classification Writer identity changed"));
    }
    let expected_audit_artifact = ArtifactRef {
        producer_id: classification.operation_id.clone(),
        path: work_model::work_artifact_path(
            &item.id,
            &attempt.id,
            &format!("{}/classification.json", classification.operation_id),
        ),
    };
    if classification.audit_artifact != expected_audit_artifact
        || attempt
            .artifacts
            .iter()
            .filter(|artifact| *artifact == &classification.audit_artifact)
            .count()
            != 1
    {
        return Err(rejected("classification audit binding changed"));
    }
    let expected_audit = CodeChangeClassificationAudit {
        schema_version: 1,
        operation_id: &classification.operation_id,
        work_item_id: &item.id,
        attempt_id: &attempt.id,
        candidate_commit: &recovery.candidate_commit,
        recovery_id: &recovery.id,
        evidence: &recovery.attachment,
        review_artifact: &classification.review_artifact,
        review_digest: &classification.review_digest,
        disposition: (!legacy_identity).then_some(classification.disposition.as_str()),
        authorization: &classification.authorization,
        authorization_digest: &classification.authorization_digest,
        writer_task_id: &classification.writer_task_id,
    };
    let expected_audit_bytes = serde_json::to_vec_pretty(&expected_audit)
        .map_err(|error| rejected(&format!("serialize classification audit: {error}")))?;
    let audit_file = project_root.join(&classification.audit_artifact.path);
    match fs::symlink_metadata(&audit_file) {
        Ok(_) => {
            let audit_path =
                validate_managed_file_path(project_root, &classification.audit_artifact.path)
                    .map_err(|error| {
                        rejected(&format!(
                            "classification audit is not a managed regular file: {error}"
                        ))
                    })?;
            let audit_bytes = fs::read(&audit_path)
                .map_err(|error| rejected(&format!("cannot read classification audit: {error}")))?;
            if audit_bytes != expected_audit_bytes {
                return Err(rejected("classification audit bytes changed"));
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(rejected(&format!(
                "cannot inspect classification audit: {error}"
            )));
        }
    }

    let expected_review_input = ArtifactRef {
        producer_id: review_task.id.clone(),
        path: classification.review_artifact.clone(),
    };
    for expected in [
        expected_review_input,
        evidence_ref,
        classification.audit_artifact.clone(),
    ] {
        if writer
            .input_artifacts
            .iter()
            .filter(|input| *input == &expected)
            .count()
            != 1
            || writer
                .input_artifacts
                .iter()
                .any(|input| input.path == expected.path && input != &expected)
        {
            return Err(rejected("corrective Writer input binding changed"));
        }
    }
    Ok(())
}

fn code_change_writer_rejection(attempt_id: &str, reason: impl Into<String>) -> WorkModelError {
    WorkModelError::HostEvidenceRecoveryIneligible {
        attempt_id: attempt_id.to_string(),
        reason: reason.into(),
    }
}

/// Validate the task frontier that a host-evidence classification may mutate.
pub(crate) fn validate_classification_task_frontier(
    attempt: &work_model::Attempt,
    review_task_id: &str,
    allowed_planned_writer_id: Option<&str>,
) -> std::result::Result<(), String> {
    let (review_index, review_task) = attempt
        .tasks
        .iter()
        .enumerate()
        .find(|(_, task)| task.id == review_task_id)
        .ok_or_else(|| "the classified Tests Review Task is missing".to_string())?;
    if review_task.kind != TaskKind::Review
        || review_task.role != "tests"
        || review_task.status != TaskStatus::Complete
    {
        return Err("the classified Tests Review Task is not complete".to_string());
    }

    let mut failed_writer_count = 0usize;
    let mut planned_writer_count = 0usize;
    for (index, task) in attempt.tasks.iter().enumerate() {
        let allowed_planned_writer = allowed_planned_writer_id.is_some_and(|id| id == task.id);
        match task.status {
            TaskStatus::Complete | TaskStatus::Superseded => {}
            TaskStatus::Failed
                if task.kind == TaskKind::Write
                    && task.output.is_none()
                    && index < review_index =>
            {
                failed_writer_count += 1;
            }
            TaskStatus::Planned
                if allowed_planned_writer
                    && task.kind == TaskKind::Write
                    && task.output.is_none() =>
            {
                planned_writer_count += 1;
            }
            _ => {
                return Err(format!(
                    "Task {} is an unrelated live or unresolved frontier entry",
                    task.id
                ));
            }
        }
    }
    if failed_writer_count > 1 {
        return Err("the frontier contains multiple failed output-free Writers".to_string());
    }
    if allowed_planned_writer_id.is_some() && planned_writer_count != 1 {
        return Err(
            "the classified corrective Writer is not the only planned continuation".to_string(),
        );
    }
    Ok(())
}

/// Validate the immutable Task frontier while replaying a classification.
///
/// A replay may observe the classified Writer after reservation, but no other
/// live or unresolved Task may take ownership of the recovery. The failed
/// output-free source Writer remains permitted historical frontier state.
fn validate_classification_replay_task_frontier(
    attempt: &work_model::Attempt,
    review_task_id: &str,
    writer_index: usize,
) -> std::result::Result<(), String> {
    let (review_index, review_task) = attempt
        .tasks
        .iter()
        .enumerate()
        .find(|(_, task)| task.id == review_task_id)
        .ok_or_else(|| "the classified Tests Review Task is missing".to_string())?;
    if review_task.kind != TaskKind::Review
        || review_task.role != "tests"
        || review_task.status != TaskStatus::Complete
    {
        return Err("the classified Tests Review Task is not complete".to_string());
    }
    if writer_index <= review_index {
        return Err("the classified Writer is not after the Tests Review".to_string());
    }

    let mut failed_writer_count = 0usize;
    for (index, task) in attempt.tasks.iter().enumerate() {
        if index == writer_index {
            continue;
        }
        match task.status {
            TaskStatus::Complete | TaskStatus::Superseded => {}
            TaskStatus::Failed
                if task.kind == TaskKind::Write
                    && task.output.is_none()
                    && index < review_index =>
            {
                failed_writer_count += 1;
            }
            _ => {
                return Err(format!(
                    "Task {} is an unrelated live or unresolved frontier entry",
                    task.id
                ));
            }
        }
    }
    if failed_writer_count > 1 {
        return Err("the frontier contains multiple failed output-free Writers".to_string());
    }
    Ok(())
}

/// Ensure a classification targets the current unresolved host-evidence
/// frontier. A later active or evidence-waiting recovery supersedes an older
/// paused recovery for new classifications and Writer continuation.
pub(crate) fn validate_classification_recovery_scope(
    attempt: &work_model::Attempt,
    recovery_id: &str,
) -> std::result::Result<(), String> {
    let recovery_index = attempt
        .evidence_recoveries
        .iter()
        .position(|recovery| recovery.id == recovery_id)
        .ok_or_else(|| "the classified recovery is missing".to_string())?;
    if attempt
        .evidence_recoveries
        .iter()
        .skip(recovery_index + 1)
        .any(|recovery| {
            matches!(
                recovery.state,
                EvidenceRecoveryState::Reviewing | EvidenceRecoveryState::NeedsEvidence
            )
        })
    {
        return Err(
            "a later active or NeedsEvidence recovery owns the current frontier".to_string(),
        );
    }
    Ok(())
}

pub(crate) fn code_change_operation_id(
    attempt_id: &str,
    candidate_commit: &str,
    recovery_id: &str,
    review_artifact: &str,
    review_digest: &str,
    evidence_digest: &str,
    disposition: &str,
    authorization_digest: &str,
) -> String {
    let operation_material = format!(
        "{attempt_id}\0{candidate_commit}\0{recovery_id}\0{review_artifact}\0{review_digest}\0{evidence_digest}\0{disposition}\0{authorization_digest}"
    );
    format!(
        "host-evidence-classification-{:x}",
        Sha256::digest(operation_material.as_bytes())
    )
}

fn legacy_code_change_operation_id(
    attempt_id: &str,
    candidate_commit: &str,
    recovery_id: &str,
    review_artifact: &str,
    review_digest: &str,
    evidence_digest: &str,
    authorization_digest: &str,
) -> String {
    let operation_material = format!(
        "{attempt_id}\0{candidate_commit}\0{recovery_id}\0{review_artifact}\0{review_digest}\0{evidence_digest}\0{authorization_digest}"
    );
    format!(
        "host-evidence-classification-{:x}",
        Sha256::digest(operation_material.as_bytes())
    )
}

pub(crate) fn classification_uses_legacy_identity(
    classification: &HostEvidenceClassification,
    attempt_id: &str,
    recovery: &EvidenceRecovery,
) -> bool {
    classification.operation_id
        == legacy_code_change_operation_id(
            attempt_id,
            &recovery.candidate_commit,
            &recovery.id,
            &classification.review_artifact,
            &classification.review_digest,
            &recovery.attachment.digest,
            &classification.authorization_digest,
        )
}

pub(crate) fn classification_audit_bytes(
    work_item_id: &str,
    attempt_id: &str,
    recovery: &EvidenceRecovery,
    classification: &HostEvidenceClassification,
    include_disposition: bool,
) -> Result<Vec<u8>> {
    serde_json::to_vec_pretty(&CodeChangeClassificationAudit {
        schema_version: 1,
        operation_id: &classification.operation_id,
        work_item_id,
        attempt_id,
        candidate_commit: &recovery.candidate_commit,
        recovery_id: &recovery.id,
        evidence: &recovery.attachment,
        review_artifact: &classification.review_artifact,
        review_digest: &classification.review_digest,
        disposition: include_disposition.then_some(classification.disposition.as_str()),
        authorization: &classification.authorization,
        authorization_digest: &classification.authorization_digest,
        writer_task_id: &classification.writer_task_id,
    })
    .context("serialize host-evidence classification audit")
}

/// Check candidate HEAD and cleanliness for ordinary evidence paths.
///
/// Classified recovery callers use `candidate_workspace_is_clean_at_managed`
/// when they must also prove the workspace is a registered worktree in the
/// source repository.
pub(crate) fn candidate_workspace_is_clean_at(
    project_root: &Path,
    output: &work_model::TaskOutput,
    candidate: &str,
) -> bool {
    let workspace = candidate_workspace_path(project_root, output);
    candidate_workspace_is_clean_at_path(&workspace, candidate)
}

/// Check the candidate commit and cleanliness of a classified recovery's
/// registered managed worktree.
pub(crate) fn candidate_workspace_is_clean_at_managed(
    project_root: &Path,
    output: &work_model::TaskOutput,
    candidate: &str,
) -> bool {
    let workspace = candidate_workspace_path(project_root, output);
    if crate::worktree::ensure_candidate_worktree(project_root, &workspace).is_err() {
        return false;
    }
    candidate_workspace_is_clean_at_path(&workspace, candidate)
}

fn candidate_workspace_is_clean_at_path(workspace: &Path, candidate: &str) -> bool {
    git::run_stdout(
        workspace,
        &["rev-parse", "HEAD"],
        "inspect evidence candidate",
    )
    .is_ok_and(|head| head == candidate)
        && git::run_stdout(
            workspace,
            &["status", "--porcelain", "--untracked-files=all"],
            "inspect evidence candidate cleanliness",
        )
        .is_ok_and(|status| status.is_empty())
}

/// The read-only proof required before a failed-review round-cap Approach
/// revision can enter the planning publication transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RoundCapApproachFrontier {
    pub candidate_commit: String,
    pub source_review_task_id: String,
    pub supporting_reviews: Vec<RoundCapReviewArtifact>,
    pub extension: RoundCapExtension,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RoundCapReviewBindings {
    source_review_task_id: String,
    supporting_reviews: Vec<RoundCapReviewArtifact>,
    failed_reviews: Vec<RoundCapReviewArtifact>,
}

/// Revalidate every task owner and local execution record at a recovery
/// boundary. Persisted Task status alone cannot prove that a peer has stopped:
/// a lease or a live execution record must also be absent.
fn validate_round_cap_task_ownership(
    project_root: &Path,
    attempt: &work_model::Attempt,
    allowed_task_id: Option<&str>,
) -> Result<()> {
    for task in &attempt.tasks {
        anyhow::ensure!(
            !crate::lease::is_leased(&crate::lease::task_lock_path(
                project_root,
                &attempt.work_item_id,
                &task.id,
            )),
            "live Task owner holds the lease for {}",
            task.id
        );
        if let Some(area) = &task.artifact_area {
            anyhow::ensure!(
                !crate::execution::execution_is_live(&crate::execution::record_path_for_artifact(
                    project_root,
                    &area.path,
                ))?,
                "live execution record owns Task {}",
                task.id
            );
        }

        if allowed_task_id == Some(task.id.as_str()) {
            continue;
        }
        let status_blocks = matches!(
            task.status,
            TaskStatus::Planned | TaskStatus::Executing | TaskStatus::NeedsUser
        ) || (task.status == TaskStatus::Failed
            && !attempt.is_superseded_tester_harness_followup(&task.id)
            && !attempt.has_tester_harness_repair_for(&task.id)
            && !attempt.is_approach_revision_superseded_task(&task.id));
        anyhow::ensure!(
            !status_blocks,
            "unresolved Task {} owns this Attempt",
            task.id
        );
    }
    Ok(())
}

/// Bind a complete review frontier to its exact Tasks, artifact paths, bytes,
/// and verdicts. Initial admission derives the bindings; replay validates the
/// recorded bindings through this same helper.
fn validate_round_cap_review_bindings(
    project_root: &Path,
    attempt: &work_model::Attempt,
    candidate_commit: &str,
    reviews: &[&Task],
    expected: Option<&[RoundCapReviewArtifact]>,
    expected_source_review_task_id: Option<&str>,
) -> Result<RoundCapReviewBindings> {
    anyhow::ensure!(
        !reviews.is_empty(),
        "failed-review round-cap frontier has no completed review artifacts"
    );
    let mut supporting_reviews = Vec::with_capacity(reviews.len());
    let mut failed_reviews = Vec::new();
    let mut source_review_task_id = None;
    for task in reviews {
        anyhow::ensure!(
            task.work_item_id == attempt.work_item_id
                && task.attempt_id.as_deref() == Some(attempt.id.as_str())
                && task.kind == TaskKind::Review
                && task.status == TaskStatus::Complete,
            "review Task {} is not complete",
            task.id
        );
        let context = task
            .review_context
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("review Task {} has no candidate context", task.id))?;
        anyhow::ensure!(
            context.candidate_commit == candidate_commit,
            "review Task {} is not bound to the approved candidate",
            task.id
        );
        let area = task
            .artifact_area
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("review Task {} has no artifact area", task.id))?;
        let artifact = ArtifactRef {
            producer_id: task.id.clone(),
            path: format!("{}/review.md", area.path),
        };
        let path = resolve_input_artifact_path(project_root, &artifact.path)?;
        let bytes =
            fs::read(&path).with_context(|| format!("read review artifact {}", path.display()))?;
        let bound = RoundCapReviewArtifact {
            artifact,
            digest: format!("sha256:{:x}", Sha256::digest(&bytes)),
        };
        let verdict = review::extract_verdict(&String::from_utf8_lossy(&bytes));
        if verdict == review::Verdict::Fail {
            failed_reviews.push(bound.clone());
        }
        if task.role == "architecture" {
            anyhow::ensure!(
                source_review_task_id.is_none(),
                "failed-review round-cap frontier has duplicate Architecture reviews"
            );
            anyhow::ensure!(
                verdict == review::Verdict::Pass,
                "Architecture review must have passed before revising the Approach"
            );
            source_review_task_id = Some(task.id.clone());
        }
        supporting_reviews.push(bound);
    }
    let source_review_task_id = source_review_task_id.ok_or_else(|| {
        anyhow::anyhow!("failed-review round-cap frontier has no Architecture review")
    })?;
    if let Some(expected_source) = expected_source_review_task_id {
        anyhow::ensure!(
            expected_source == source_review_task_id,
            "round-cap Approach source Architecture review identity changed"
        );
    }
    if let Some(expected_reviews) = expected {
        anyhow::ensure!(
            expected_reviews == supporting_reviews,
            "Approach supporting review identities or bytes changed"
        );
    }
    anyhow::ensure!(
        !failed_reviews.is_empty(),
        "failed-review round-cap frontier has no failed review"
    );
    Ok(RoundCapReviewBindings {
        source_review_task_id,
        supporting_reviews,
        failed_reviews,
    })
}

/// Validate the current failed-review round-cap frontier for Approach
/// revision. This helper intentionally returns an owned proof so the planner
/// can re-run the same checks under the Work-model transaction without making
/// the audit file authoritative by itself.
pub(crate) fn validate_round_cap_approach_frontier(
    project_root: &Path,
    attempt: &work_model::Attempt,
    extension: &RoundCapExtension,
) -> Result<RoundCapApproachFrontier> {
    anyhow::ensure!(
        attempt.status == AttemptStatus::NeedsUser
            && attempt.pause_kind == Some(PauseKind::RoundCap)
            && attempt.review_state == Some(AttemptReviewState::Failed),
        "Attempt is not paused at a failed-review round-cap frontier"
    );
    attempt.ensure_not_canceled().map_err(anyhow::Error::msg)?;
    validate_round_cap_task_ownership(project_root, attempt, None)?;
    anyhow::ensure!(
        !extension.human_resolution.trim().is_empty(),
        "round-cap Approach revision requires the recorded human authorization"
    );
    extension
        .validate_human_resolution()
        .map_err(anyhow::Error::msg)?;
    anyhow::ensure!(
        attempt
            .round_cap_extensions
            .iter()
            .any(|recorded| recorded == extension),
        "the round-cap authorization is not recorded on this Attempt"
    );
    anyhow::ensure!(
        extension.new_cap > attempt.materialized_write_rounds(),
        "the approved Writer-round cap is exhausted"
    );
    attempt
        .validate_round_cap_extensions_for_approach_revision()
        .map_err(anyhow::Error::msg)?;
    anyhow::ensure!(
        extension.pre_extension_write_rounds == Some(attempt.materialized_write_rounds()),
        "the approved extension does not match the current pre-extension Writer frontier"
    );

    let (writer_index, writer) = attempt
        .tasks
        .iter()
        .enumerate()
        .rev()
        .find(|(_, task)| task.kind == TaskKind::Write && task.status == TaskStatus::Complete)
        .ok_or_else(|| anyhow::anyhow!("Attempt has no completed Writer candidate"))?;
    let output = writer
        .output
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("latest Writer has no candidate output"))?;
    anyhow::ensure!(
        output.commit == extension.candidate_commit,
        "the reviewed candidate no longer matches the round-cap authorization"
    );
    anyhow::ensure!(
        candidate_workspace_is_clean_at_managed(project_root, output, &output.commit),
        "the approved candidate workspace is changed, dirty, or no longer managed"
    );

    let reviews = latest_round_cap_review_tasks(attempt, writer_index);
    let bindings = validate_round_cap_review_bindings(
        project_root,
        attempt,
        &output.commit,
        &reviews,
        None,
        None,
    )?;
    anyhow::ensure!(
        bindings.failed_reviews == extension.failed_reviews,
        "failed-review bytes no longer match the round-cap authorization"
    );

    Ok(RoundCapApproachFrontier {
        candidate_commit: output.commit.clone(),
        source_review_task_id: bindings.source_review_task_id,
        supporting_reviews: bindings.supporting_reviews,
        extension: extension.clone(),
    })
}

/// Revalidate an accepted failed-review round-cap revision before returning it
/// from an exact retry. Unlike initial admission, this proof allows the
/// extension to be consumed and the Attempt pause to be cleared, but it still
/// checks the immutable review bindings, cancellation state, and the untouched
/// source candidate while the corrective Writer remains unstarted.
pub(crate) fn validate_round_cap_approach_revision_retry(
    project_root: &Path,
    item: &work_model::WorkItem,
    attempt: &work_model::Attempt,
    revision: &work_model::ApproachRevisionAudit,
    audit_artifact: &ArtifactRef,
    allow_settled_cancellation: bool,
) -> Result<()> {
    let registered_bytes =
        registered_approach_revision_audit_bytes(project_root, attempt, audit_artifact)?;
    let registered_revision: work_model::ApproachRevisionAudit =
        serde_json::from_slice(&registered_bytes)?;
    anyhow::ensure!(
        &registered_revision == revision,
        "retry supplied an Approach revision different from the registered audit bytes"
    );
    validate_approach_revision_operation_binding(
        attempt,
        revision,
        &format!("{:x}", Sha256::digest(&registered_bytes)),
    )?;
    anyhow::ensure!(
        revision.revision_kind == work_model::ApproachRevisionKind::FailedReviewRoundCap
            && revision.schema_version == 2
            && revision.work_item_id == attempt.work_item_id
            && revision.attempt_id == attempt.id,
        "retry is not a failed-review round-cap Approach revision"
    );
    if allow_settled_cancellation {
        if attempt.cancellation_requested() {
            anyhow::ensure!(
                attempt
                    .cancellation
                    .as_ref()
                    .and_then(|stop| stop.settled_at.as_ref())
                    .is_some(),
                "canceled local execution has not settled"
            );
        }
    } else {
        attempt.ensure_not_canceled().map_err(anyhow::Error::msg)?;
    }
    let context = item
        .planning_context
        .as_ref()
        .context("Work Item has no stored planning context")?;
    let prior_contract = revision
        .prior_contract
        .as_ref()
        .context("round-cap Approach revision has no prior completion contract")?;
    let current_contract = work_model::WriterCompletionContract::from_planning_context(context)
        .context("Work Item has no Writer completion contract")?;
    let ownership_approval = validated_ownership_approval(project_root, attempt)?;
    let expected_contract = prior_contract
        .with_revised_approach(&revision.prior_approach, &revision.replacement_approach)
        .map_err(anyhow::Error::msg)?;
    anyhow::ensure!(
        attempt.writer_completion_contract.as_ref() == Some(&expected_contract),
        "planning authority and Attempt completion contract differ"
    );
    // An approved ownership split may make the retained planning markdown's
    // contract differ from the Attempt contract. The shared planning-history
    // validator verifies that separate approval before this retry check.
    if current_contract != expected_contract && ownership_approval.is_none() {
        bail!("planning authority and Attempt completion contract differ");
    }
    anyhow::ensure!(
        context.approach.as_deref() == Some(revision.replacement_approach.as_str()),
        "stored Approach does not match the accepted revision"
    );
    let approved_approach = context
        .approved_revisions
        .get("approach")
        .context("accepted Approach revision has no planning approval")?;
    anyhow::ensure!(
        approved_approach.revision_id
            == format!(
                "approach:{}",
                format!(
                    "{:x}",
                    Sha256::digest(revision.replacement_approach.as_bytes())
                )
            )
            && approved_approach.sha256
                == format!(
                    "{:x}",
                    Sha256::digest(revision.replacement_approach.as_bytes())
                ),
        "accepted Approach planning approval changed"
    );
    let extension = revision
        .round_cap_extension
        .as_ref()
        .context("round-cap Approach revision has no extension binding")?;
    attempt
        .validate_round_cap_extensions_for_approach_revision()
        .map_err(anyhow::Error::msg)?;
    extension
        .validate_human_resolution()
        .map_err(anyhow::Error::msg)?;
    anyhow::ensure!(
        extension.candidate_commit == revision.candidate_commit
            && attempt
                .round_cap_extensions
                .iter()
                .any(|recorded| recorded == extension),
        "round-cap Approach revision lost its exact extension authorization"
    );

    let source = revision
        .source_completion
        .as_ref()
        .context("round-cap Approach revision has no source completion evidence")?;
    let source_bytes = fs::read(resolve_input_artifact_path(
        project_root,
        &source.artifact.path,
    )?)?;
    anyhow::ensure!(
        format!("{:x}", Sha256::digest(&source_bytes)) == source.sha256,
        "approved Approach source completion evidence changed"
    );
    let matrix: work_model::WriterCompletionMatrix = serde_json::from_slice(&source_bytes)?;
    let source_writer = attempt
        .tasks
        .iter()
        .find(|task| {
            task.id == matrix.task_id
                && task.kind == TaskKind::Write
                && task.status == TaskStatus::Complete
                && task
                    .output
                    .as_ref()
                    .is_some_and(|output| output.commit == revision.candidate_commit)
        })
        .context("approved Approach source Writer candidate changed")?;
    let source_output = source_writer
        .output
        .as_ref()
        .context("approved Approach source Writer has no candidate output")?;
    let findings = writer_completion_findings(project_root, source_writer)?;
    matrix
        .status_against(&source.contract, &source_writer.id, &findings)
        .map_err(anyhow::Error::msg)?;

    let review_tasks = revision
        .supporting_reviews
        .iter()
        .map(|supporting| {
            attempt
                .tasks
                .iter()
                .find(|task| task.id == supporting.artifact.producer_id)
                .with_context(|| {
                    format!(
                        "Approach supporting review Task {} changed",
                        supporting.artifact.producer_id
                    )
                })
        })
        .collect::<Result<Vec<_>>>()?;
    let bindings = validate_round_cap_review_bindings(
        project_root,
        attempt,
        &revision.candidate_commit,
        &review_tasks,
        Some(&revision.supporting_reviews),
        Some(&revision.source_review_task_id),
    )?;
    anyhow::ensure!(
        bindings.failed_reviews == extension.failed_reviews,
        "round-cap Approach review evidence no longer matches its authorization"
    );

    let corrective_writers: Vec<_> = attempt
        .tasks
        .iter()
        .filter(|task| {
            task.kind == TaskKind::Write
                && task.role == "approach-revision-correction"
                && task
                    .input_artifacts
                    .iter()
                    .any(|input| input == audit_artifact)
        })
        .collect();
    anyhow::ensure!(
        corrective_writers.len() == 1,
        "round-cap Approach revision must have exactly one corrective Writer"
    );
    let corrective = corrective_writers[0];
    if matches!(
        corrective.status,
        TaskStatus::Planned | TaskStatus::Executing
    ) {
        validate_round_cap_task_ownership(project_root, attempt, Some(&corrective.id))?;
    }
    if corrective.status == TaskStatus::Planned
        && corrective.started_at.is_none()
        && corrective.output.is_none()
    {
        anyhow::ensure!(
            candidate_workspace_is_clean_at_managed(
                project_root,
                source_output,
                &revision.candidate_commit,
            ),
            "the approved candidate workspace is changed, dirty, or no longer managed"
        );
    }
    Ok(())
}

fn latest_round_cap_review_tasks(attempt: &work_model::Attempt, writer_index: usize) -> Vec<&Task> {
    let mut latest_by_role: Vec<(String, &Task)> = Vec::new();
    for task in attempt.tasks[writer_index + 1..]
        .iter()
        .filter(|task| task.kind == TaskKind::Review && task.status == TaskStatus::Complete)
    {
        if let Some((_, latest)) = latest_by_role
            .iter_mut()
            .find(|(role, _)| role == &task.role)
        {
            *latest = task;
        } else {
            latest_by_role.push((task.role.clone(), task));
        }
    }
    latest_by_role.into_iter().map(|(_, task)| task).collect()
}

/// Require host evidence to record a failed check before allowing a source
/// correction classification or replay.
pub(crate) fn validate_failed_evidence_snapshot(bytes: &[u8]) -> Result<()> {
    match parse_evidence_document(bytes)?.result {
        EvidenceResult::Fail => Ok(()),
        EvidenceResult::Pass => {
            bail!("evidence snapshot result must be fail for source correction")
        }
    }
}

/// Select the one persisted Tests Review target bound to a review artifact.
///
/// Every source-correction boundary must establish this cardinality before it
/// reads or mutates the recovery. The candidate workspace and host-evidence
/// bindings are checked by `validate_code_change_candidate_binding` after this
/// target has been selected.
pub(crate) struct UniqueTestsReviewTarget<'a> {
    pub(crate) index: usize,
    pub(crate) target: &'a EvidenceReviewTarget,
}

pub(crate) fn unique_tests_review_target<'a>(
    attempt: &work_model::Attempt,
    recovery: &'a EvidenceRecovery,
    review_artifact: &str,
) -> std::result::Result<UniqueTestsReviewTarget<'a>, String> {
    let matching_targets = recovery
        .targets
        .iter()
        .enumerate()
        .filter(|(_, target)| {
            target.role == "tests"
                && target
                    .review_task_id
                    .as_ref()
                    .is_some_and(|review_task_id| {
                        attempt.tasks.iter().any(|task| {
                            task.id == *review_task_id
                                && task.kind == TaskKind::Review
                                && task.role == "tests"
                                && review_artifact_path(task).as_deref() == Some(review_artifact)
                        })
                    })
        })
        .collect::<Vec<_>>();
    if matching_targets.len() != 1 {
        return Err("classified recovery has an ambiguous Tests Review target".to_string());
    }
    let (index, target) = matching_targets[0];
    Ok(UniqueTestsReviewTarget { index, target })
}

/// Validate the immutable candidate and host-evidence bindings shared by every
/// source-correction classification boundary.
///
/// Callers must run this contract before changing the Work Item. It proves that
/// the failed Review still reads the exact candidate workspace, names the
/// persisted recovery, and carries the exact failed host-evidence snapshot.
/// Queue admission and Writer reservation use this strict source binding, while
/// classification replay uses it for no-output resumable Writers and the
/// post-correction binding below for completed Writers. Keeping both replay
/// contracts here prevents a boundary from accepting an immutable binding that
/// another boundary later rejects.
pub(crate) fn validate_code_change_candidate_binding(
    project_root: &Path,
    item: &work_model::WorkItem,
    attempt: &work_model::Attempt,
    recovery: &EvidenceRecovery,
    target: &EvidenceReviewTarget,
    review_task: &Task,
    source_output: &work_model::TaskOutput,
    candidate_commit: &str,
    review_artifact: &str,
    evidence_digest: &str,
) -> std::result::Result<(), String> {
    let rejected = |reason: &str| reason.to_string();
    if source_output.commit != candidate_commit
        || recovery.candidate_commit != candidate_commit
        || !candidate_workspace_is_clean_at_managed(project_root, source_output, candidate_commit)
    {
        return Err(rejected(
            "the candidate workspace no longer matches the classified commit",
        ));
    }
    validate_code_change_review_and_evidence_binding(
        project_root,
        item,
        attempt,
        recovery,
        target,
        review_task,
        source_output,
        candidate_commit,
        review_artifact,
        evidence_digest,
    )
}

/// Validate a completed classified Writer against the candidate it recorded.
///
/// Classification binds its source Review and failed evidence to the original
/// candidate commit. Once the corrective Writer has completed, that Writer's
/// output is the new candidate boundary, so replay must validate its saved
/// workspace and commit rather than require the source commit to remain HEAD.
pub(crate) fn validate_code_change_post_correction_binding(
    project_root: &Path,
    item: &work_model::WorkItem,
    attempt: &work_model::Attempt,
    recovery: &EvidenceRecovery,
    target: &EvidenceReviewTarget,
    review_task: &Task,
    source_output: &work_model::TaskOutput,
    corrective_output: &work_model::TaskOutput,
    review_artifact: &str,
    evidence_digest: &str,
) -> std::result::Result<(), String> {
    let rejected = |reason: &str| reason.to_string();
    if source_output.commit != recovery.candidate_commit
        || corrective_output.workspace_id != source_output.workspace_id
        || corrective_output.workspace_path != source_output.workspace_path
        || !candidate_workspace_is_clean_at_managed(
            project_root,
            corrective_output,
            &corrective_output.commit,
        )
    {
        return Err(rejected(
            "the completed corrective Writer output is not the current clean candidate",
        ));
    }
    validate_code_change_review_and_evidence_binding(
        project_root,
        item,
        attempt,
        recovery,
        target,
        review_task,
        source_output,
        &recovery.candidate_commit,
        review_artifact,
        evidence_digest,
    )
}

fn validate_code_change_review_and_evidence_binding(
    project_root: &Path,
    item: &work_model::WorkItem,
    attempt: &work_model::Attempt,
    recovery: &EvidenceRecovery,
    target: &EvidenceReviewTarget,
    review_task: &Task,
    source_output: &work_model::TaskOutput,
    candidate_commit: &str,
    review_artifact: &str,
    evidence_digest: &str,
) -> std::result::Result<(), String> {
    let rejected = |reason: &str| reason.to_string();
    let unique_target = unique_tests_review_target(attempt, recovery, review_artifact)?;
    if unique_target.target != target {
        return Err(rejected(
            "bound Tests review target is not uniquely selected",
        ));
    }
    if review_task.kind != TaskKind::Review
        || review_task.role != "tests"
        || review_task.status != TaskStatus::Complete
        || review_task.work_item_id != item.id
        || review_task.attempt_id.as_deref() != Some(attempt.id.as_str())
        || review_artifact_path(review_task).as_deref() != Some(review_artifact)
        || target.role != review_task.role
        || target.review_task_id.as_deref() != Some(review_task.id.as_str())
    {
        return Err(rejected("bound Tests review Task identity changed"));
    }
    let review_context = review_task
        .review_context
        .as_ref()
        .ok_or_else(|| rejected("bound Tests review has no candidate workspace context"))?;
    let matching_candidate_reads = review_task
        .workspace_access
        .reads
        .iter()
        .filter(|workspace| {
            workspace.id == source_output.workspace_id
                && workspace.path == source_output.workspace_path
        })
        .count();
    let conflicting_candidate_reads = review_task.workspace_access.reads.iter().any(|workspace| {
        workspace.id == source_output.workspace_id && workspace.path != source_output.workspace_path
    });
    if review_context.candidate_commit != candidate_commit
        || review_context.candidate_workspace_id != source_output.workspace_id
        || review_context.candidate_workspace_path != source_output.workspace_path
        || matching_candidate_reads != 1
        || conflicting_candidate_reads
    {
        return Err(rejected(
            "bound Tests review is not attached to the candidate workspace",
        ));
    }
    let evidence_context = review_task
        .evidence_review_context
        .as_ref()
        .ok_or_else(|| rejected("bound Tests review has no host-evidence context"))?;
    if evidence_context.recovery_id != recovery.id
        || evidence_context.candidate_commit != candidate_commit
        || evidence_context.attachment != recovery.attachment
        || evidence_context.prior_review_artifact != target.prior_review_artifact
    {
        return Err(rejected("bound Tests review host-evidence context changed"));
    }
    if recovery.attachment.digest != evidence_digest {
        return Err(rejected("evidence snapshot binding changed"));
    }
    let evidence_ref = ArtifactRef {
        producer_id: "host-evidence".to_string(),
        path: recovery.attachment.snapshot_path.clone(),
    };
    if review_task
        .input_artifacts
        .iter()
        .filter(|input| *input == &evidence_ref)
        .count()
        != 1
        || review_task
            .input_artifacts
            .iter()
            .any(|input| input.path == evidence_ref.path && input != &evidence_ref)
    {
        return Err(rejected("bound Tests review lost its evidence input"));
    }
    let expected_snapshot = snapshot_path(&item.id, &attempt.id, &recovery.attachment.digest)
        .map_err(|error| rejected(&error.to_string()))?;
    if recovery.attachment.snapshot_path != expected_snapshot {
        return Err(rejected("evidence snapshot binding changed"));
    }
    let evidence_path =
        validate_managed_file_path(project_root, &recovery.attachment.snapshot_path).map_err(
            |error| {
                rejected(&format!(
                    "evidence snapshot is not a managed regular file: {error}"
                ))
            },
        )?;
    let evidence_bytes = fs::read(&evidence_path)
        .map_err(|error| rejected(&format!("cannot read bound evidence snapshot: {error}")))?;
    if format!("sha256:{:x}", Sha256::digest(&evidence_bytes)) != recovery.attachment.digest {
        return Err(rejected("evidence snapshot bytes changed"));
    }
    validate_failed_evidence_snapshot(&evidence_bytes)
        .map_err(|error| rejected(&format!("invalid evidence snapshot: {error}")))?;
    Ok(())
}

pub(crate) use crate::verification_assessment::snapshot_path;

#[cfg(test)]
mod tests {
    use super::{EvidenceResult, parse_evidence_document, validate_failed_evidence_snapshot};

    #[test]
    fn assignment_context_freezes_carried_legacy_binding() {
        use super::*;
        use crate::work_model::*;
        let tmp = tempfile::tempdir().unwrap();
        let mut item = WorkItem::planned("work-1", "Carry context");
        item.add_initial_attempt("attempt-1").unwrap();
        let input = ArtifactRef {
            producer_id: "required-progress".into(),
            path: ".fluent/work/progress/work-1/attempt-1/progress.md".into(),
        };
        let path = tmp.path().join(&input.path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"original\n").unwrap();
        let source = &mut item.attempts[0].tasks[0];
        source.input_artifacts.push(input.clone());
        source.status = TaskStatus::Complete;
        let workspace = source.workspace_access.writes[0].clone();
        source.output = Some(TaskOutput {
            workspace_id: workspace.id,
            workspace_path: workspace.path,
            source_branch: "main".into(),
            base_commit: None,
            commit: "source".into(),
            no_change: None,
            learner_canonicalization: None,
        });
        let source_output = source.output.clone();
        let writer = item.add_next_write_round("attempt-1", vec![]).unwrap();
        item.attempts[0].tasks[1].writer_correction_assignment = Some(WriterCorrectionAssignment {
            schema_version: 1,
            work_item_id: "work-1".into(),
            attempt_id: "attempt-1".into(),
            writer_task_id: writer,
            source_task_id: "attempt-1-write-1".into(),
            source_candidate_commit: "source".into(),
            source_task_inputs: vec![WriterCorrectionAssignmentInput {
                artifact: input.clone(),
                digest: assignment_input_digest(tmp.path(), &input.path).unwrap(),
            }],
            evidence: vec![],
            findings: vec![],
            origin: WriterCorrectionAssignmentOrigin::Captured,
        });
        item.attempts[0].tasks[1].status = TaskStatus::Complete;
        item.attempts[0].tasks[1].output = source_output;
        item.add_next_write_round("attempt-1", vec![]).unwrap();
        prepare_writer_correction_assignments(tmp.path(), &mut item).unwrap();
        let task = &item.attempts[0].tasks[2];
        let assignment = task.writer_correction_assignment.as_ref().unwrap();
        assert_ne!(assignment.source_task_inputs[0].artifact, input);
        fs::write(path, b"later context\n").unwrap();
        validate_writer_correction_assignment(tmp.path(), &item, task, assignment).unwrap();
    }

    #[test]
    fn assignment_context_recognizes_only_owned_projections() {
        use super::*;
        let mut item = work_model::WorkItem::planned("work-1", "Recognize context");
        item.add_initial_attempt("attempt-1").unwrap();
        let task = &item.attempts[0].tasks[0];
        for (producer, path, expected) in [
            (
                "required-progress",
                ".fluent/work/progress/work-1/attempt-1/progress.md",
                true,
            ),
            (
                "required-progress",
                ".fluent/work/artifacts/work-1/attempt-1/required-progress-needs-user.md",
                true,
            ),
            (
                "operator",
                ".fluent/work/progress/work-1/attempt-1/progress.md",
                false,
            ),
            (
                "required-progress",
                ".fluent/work/progress/work-2/attempt-1/progress.md",
                false,
            ),
            (
                "required-progress",
                ".fluent/work/progress/work-1/attempt-2/progress.md",
                false,
            ),
            (
                "required-progress",
                ".fluent/work/artifacts/work-1/attempt-1/progress.md",
                false,
            ),
            (
                "required-progress",
                ".fluent/work/progress/work-1/attempt-1/./progress.md",
                false,
            ),
        ] {
            assert_eq!(
                mutable_assignment_context(
                    task,
                    &ArtifactRef {
                        producer_id: producer.into(),
                        path: path.into()
                    }
                ),
                expected
            );
        }
    }

    #[test]
    fn recovery_contracts_remain_independent_of_lifecycle_callers() {
        let source = [
            include_str!("recovery_contracts.rs"),
            include_str!("recovery_contracts/carried_finding.rs"),
            include_str!("recovery_contracts/writer_completion.rs"),
            include_str!("recovery_contracts/context_retry.rs"),
        ]
        .join("\n");
        assert!(
            !include_str!("tester.rs").contains("crate::recovery_contracts::"),
            "Tester must resolve applicability below the recovery consumer"
        );
        for consumer in ["tester", "recovery_contracts"] {
            assert!(
                !include_str!("check_applicability.rs").contains(&format!("crate::{consumer}::")),
                "shared applicability must not depend on {consumer}"
            );
        }
        for caller in [
            "lifecycle_recovery",
            "work_task_executor",
            "host_evidence",
            "queue",
        ] {
            assert!(
                !source.contains(&format!("crate::{caller}::")),
                "neutral recovery contracts must not depend on {caller}"
            );
        }
        for caller in [
            "candidate_gate",
            "host_evidence",
            "lifecycle_recovery",
            "queue",
            "work_task_executor",
            "recovery_contracts",
        ] {
            assert!(
                !include_str!("verification_assessment.rs").contains(&format!("crate::{caller}::")),
                "verification assessment must stay below {caller}"
            );
        }
    }

    #[test]
    fn corrective_writer_completion_uses_frozen_assignment() {
        use super::*;
        use crate::work_model::{TaskOutput, TaskStatus};

        let tmp = tempfile::tempdir().unwrap();
        let store = work_model::WorkModelStore::new(tmp.path());
        let mut item = work_model::WorkItem::planned("work-1", "Preserve correction");
        item.add_initial_attempt("attempt-1").unwrap();
        let source = &mut item.attempts[0].tasks[0];
        let workspace = source.workspace_access.writes[0].clone();
        source.status = TaskStatus::Complete;
        source.output = Some(TaskOutput {
            workspace_id: workspace.id.clone(),
            workspace_path: workspace.path.clone(),
            source_branch: "main".into(),
            base_commit: None,
            commit: "commit-initial".into(),
            no_change: None,
            learner_canonicalization: None,
        });
        item.attempts[0].status = work_model::AttemptStatus::Reviewing;

        let evidence_path =
            work_model::work_artifact_path("work-1", "attempt-1", "review-tests/review.md");
        let evidence_file = tmp.path().join(&evidence_path);
        std::fs::create_dir_all(evidence_file.parent().unwrap()).unwrap();
        std::fs::write(&evidence_file, "- [ ] Preserve the assigned finding\n").unwrap();
        let input = ArtifactRef {
            producer_id: "review-tests".into(),
            path: evidence_path.clone(),
        };
        let correction_id = item
            .add_next_write_round("attempt-1", vec![input.clone()])
            .unwrap();
        prepare_writer_correction_assignments(tmp.path(), &mut item).unwrap();
        let assignment = item.attempts[0].tasks[1]
            .writer_correction_assignment
            .as_ref()
            .unwrap();
        assert_eq!(assignment.writer_task_id, correction_id);
        assert_eq!(assignment.source_task_id, "attempt-1-write-1");
        assert_eq!(assignment.findings.len(), 1);
        assert_eq!(assignment.findings[0].role, "tests");
        item.validate().unwrap();
        store.create_work_item(&item).unwrap();

        let first = store.read_work_item("work-1").unwrap();
        let first_task = first.attempts[0].tasks[1].clone();
        let assigned_finding_id = first_task
            .writer_correction_assignment
            .as_ref()
            .unwrap()
            .findings[0]
            .id
            .clone();
        let contract = work_model::WriterCompletionContract {
            version: work_model::WRITER_COMPLETION_CONTRACT_VERSION,
            requirements: Vec::new(),
        };
        let mut matrix = work_model::WriterCompletionMatrix::skeleton(
            &contract,
            &first_task.id,
            &first_task
                .writer_correction_assignment
                .as_ref()
                .unwrap()
                .findings,
            None,
        )
        .unwrap();
        matrix.rows[0].state = work_model::WriterCompletionState::Complete;
        matrix.rows[0].implementation_evidence = vec!["src/lib.rs".into()];
        matrix.rows[0].verification = vec![work_model::WriterVerificationClaim {
            command: "cargo test focused".into(),
            result: work_model::WriterVerificationResult::Pass,
        }];
        matrix.rows[0].applicable_constraints = vec!["none: correction has no planning row".into()];
        assert_eq!(
            writer_completion_findings(tmp.path(), &first_task).unwrap()[0].id,
            assigned_finding_id
        );

        let mut changed = first;
        let correction = &mut changed.attempts[0].tasks[1];
        correction.status = TaskStatus::Complete;
        correction.output = Some(TaskOutput {
            workspace_id: workspace.id,
            workspace_path: workspace.path,
            source_branch: "main".into(),
            base_commit: Some("commit-initial".into()),
            commit: "commit-corrected".into(),
            no_change: None,
            learner_canonicalization: None,
        });
        changed.attempts[0].status = work_model::AttemptStatus::Reviewing;
        changed.attempts[0].review_state = Some(work_model::AttemptReviewState::Failed);
        store.write_work_item(&changed).unwrap();

        let stored = store.read_work_item("work-1").unwrap();
        let corrected = stored.attempts[0].tasks[1].clone();
        let findings = writer_completion_findings(tmp.path(), &corrected).unwrap();
        assert_eq!(findings[0].id, assigned_finding_id);
        assert!(
            matrix
                .status_against(&contract, &corrected.id, &findings)
                .unwrap()
                .is_complete()
        );
    }

    #[test]
    fn later_corrective_writer_captures_a_new_assignment() {
        use super::*;
        use crate::work_model::{TaskOutput, TaskStatus};

        let tmp = tempfile::tempdir().unwrap();
        let mut item = work_model::WorkItem::planned("work-1", "Preserve correction rounds");
        item.add_initial_attempt("attempt-1").unwrap();
        let workspace = item.attempts[0].tasks[0].workspace_access.writes[0].clone();
        item.attempts[0].tasks[0].status = TaskStatus::Complete;
        item.attempts[0].tasks[0].output = Some(TaskOutput {
            workspace_id: workspace.id.clone(),
            workspace_path: workspace.path.clone(),
            source_branch: "main".into(),
            base_commit: None,
            commit: "commit-initial".into(),
            no_change: None,
            learner_canonicalization: None,
        });
        item.attempts[0].status = work_model::AttemptStatus::Reviewing;

        let first_path =
            work_model::work_artifact_path("work-1", "attempt-1", "review-tests/round-1/review.md");
        let first_file = tmp.path().join(&first_path);
        std::fs::create_dir_all(first_file.parent().unwrap()).unwrap();
        std::fs::write(&first_file, "- [ ] Resolve the first review finding\n").unwrap();
        let first_input = ArtifactRef {
            producer_id: "review-tests".into(),
            path: first_path,
        };
        let first_task_id = item
            .add_next_write_round("attempt-1", vec![first_input])
            .unwrap();
        prepare_writer_correction_assignments(tmp.path(), &mut item).unwrap();
        let first_assignment = item.attempts[0].tasks[1]
            .writer_correction_assignment
            .clone()
            .unwrap();

        item.attempts[0].tasks[1].status = TaskStatus::Complete;
        item.attempts[0].tasks[1].output = Some(TaskOutput {
            workspace_id: workspace.id.clone(),
            workspace_path: workspace.path.clone(),
            source_branch: "main".into(),
            base_commit: Some("commit-initial".into()),
            commit: "commit-corrected".into(),
            no_change: None,
            learner_canonicalization: None,
        });

        let second_path =
            work_model::work_artifact_path("work-1", "attempt-1", "review-tests/round-2/review.md");
        let second_file = tmp.path().join(&second_path);
        std::fs::create_dir_all(second_file.parent().unwrap()).unwrap();
        std::fs::write(&second_file, "- [ ] Resolve the second review finding\n").unwrap();
        let second_input = ArtifactRef {
            producer_id: "review-tests".into(),
            path: second_path,
        };
        let second_task_id = item
            .add_next_write_round("attempt-1", vec![second_input])
            .unwrap();
        prepare_writer_correction_assignments(tmp.path(), &mut item).unwrap();

        let second_assignment = item.attempts[0].tasks[2]
            .writer_correction_assignment
            .clone()
            .unwrap();
        assert_eq!(first_assignment.writer_task_id, first_task_id);
        assert_eq!(second_assignment.writer_task_id, second_task_id);
        assert_eq!(second_assignment.source_task_id, first_task_id);
        assert_eq!(
            second_assignment.source_candidate_commit,
            "commit-corrected"
        );
        assert_ne!(second_assignment.findings, first_assignment.findings);
        assert_eq!(second_assignment.findings.len(), 1);
        assert!(
            second_assignment.findings[0]
                .requirement
                .contains("second review finding")
        );
        assert_eq!(
            item.attempts[0].tasks[1]
                .writer_correction_assignment
                .as_ref(),
            Some(&first_assignment)
        );
    }

    #[test]
    fn writer_continuation_carries_assignment_after_review_history() {
        use super::*;
        use crate::work_model::{TaskOutput, TaskStatus};

        let tmp = tempfile::tempdir().unwrap();
        let mut item = work_model::WorkItem::planned("work-1", "Preserve Writer continuation");
        item.add_initial_attempt("attempt-1").unwrap();
        let workspace = item.attempts[0].tasks[0].workspace_access.writes[0].clone();
        item.attempts[0].tasks[0].status = TaskStatus::Complete;
        item.attempts[0].tasks[0].output = Some(TaskOutput {
            workspace_id: workspace.id.clone(),
            workspace_path: workspace.path.clone(),
            source_branch: "main".into(),
            base_commit: None,
            commit: "commit-initial".into(),
            no_change: None,
            learner_canonicalization: None,
        });
        item.attempts[0].status = work_model::AttemptStatus::Reviewing;

        let review_path =
            work_model::work_artifact_path("work-1", "attempt-1", "review-tests/review.md");
        let review_file = tmp.path().join(&review_path);
        std::fs::create_dir_all(review_file.parent().unwrap()).unwrap();
        std::fs::write(&review_file, "- [ ] Resolve the assigned finding\n").unwrap();
        let first_task_id = item
            .add_next_write_round(
                "attempt-1",
                vec![ArtifactRef {
                    producer_id: "review-tests".into(),
                    path: review_path,
                }],
            )
            .unwrap();
        prepare_writer_correction_assignments(tmp.path(), &mut item).unwrap();
        let first_assignment = item.attempts[0].tasks[1]
            .writer_correction_assignment
            .clone()
            .unwrap();
        item.attempts[0].tasks[1].status = TaskStatus::Complete;
        item.attempts[0].tasks[1].output = Some(TaskOutput {
            workspace_id: workspace.id.clone(),
            workspace_path: workspace.path.clone(),
            source_branch: "main".into(),
            base_commit: Some("commit-initial".into()),
            commit: "commit-corrected".into(),
            no_change: None,
            learner_canonicalization: None,
        });

        item.add_review_tasks("attempt-1", &["tests"]).unwrap();
        let continuation_path = work_model::work_artifact_path(
            "work-1",
            "attempt-1",
            "writer-completion/continuation.json",
        );
        let continuation_file = tmp.path().join(&continuation_path);
        std::fs::create_dir_all(continuation_file.parent().unwrap()).unwrap();
        std::fs::write(&continuation_file, "{}\n").unwrap();
        let continuation_task_id = item
            .add_next_write_round(
                "attempt-1",
                vec![ArtifactRef {
                    producer_id: "writer-completion".into(),
                    path: continuation_path,
                }],
            )
            .unwrap();
        let continuation = item.attempts[0]
            .tasks
            .iter()
            .find(|task| task.id == continuation_task_id)
            .unwrap();
        assert!(!is_corrective_writer_task(continuation));
        prepare_writer_correction_assignments(tmp.path(), &mut item).unwrap();

        let carried = item.attempts[0]
            .tasks
            .iter()
            .find(|task| task.id == continuation_task_id)
            .unwrap()
            .writer_correction_assignment
            .as_ref()
            .unwrap();
        assert_eq!(first_assignment.source_task_id, "attempt-1-write-1");
        assert_eq!(first_assignment.writer_task_id, first_task_id);
        assert_eq!(carried.source_task_id, first_assignment.source_task_id);
        assert_eq!(carried.source_candidate_commit, "commit-initial");
        assert_eq!(carried.evidence, first_assignment.evidence);
        assert_eq!(carried.findings, first_assignment.findings);
        assert_eq!(carried.origin, first_assignment.origin);
    }

    #[test]
    fn plan_host_source_requires_bound_bytes_and_recorded_lineage() {
        use super::*;
        let tmp = tempfile::tempdir().unwrap();
        let mut item = work_model::WorkItem::planned("work-1", "Retain host evidence");
        item.planning_context = Some(work_model::PlanningContext {
            plan: Some("| # | State reached | Behaviors | Verification | Responsibility | Req? |\n| 1 | Check host. | | `bash tests/host.sh` | host | Yes |\n".into()),
            ..Default::default()
        });
        item.add_initial_attempt("attempt-1").unwrap();
        let attempt = &item.attempts[0];
        let writer = &attempt.tasks[0];
        let matrix = work_model::WriterCompletionMatrix::skeleton(
            attempt.writer_completion_contract.as_ref().unwrap(),
            &writer.id,
            &[],
            None,
        )
        .unwrap();
        let artifact = writer_completion_matrix_ref(tmp.path(), attempt, writer).unwrap();
        let path = tmp.path().join(&artifact.path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let bytes = serde_json::to_vec_pretty(&matrix).unwrap();
        fs::write(&path, &bytes).unwrap();
        let mut bound = work_model::BoundRecheckArtifact {
            path: artifact.path.clone(),
            digest: format!("sha256:{:x}", Sha256::digest(&bytes)),
        };
        assert_eq!(
            plan_host_source_matrix(
                tmp.path(),
                attempt,
                writer,
                &artifact,
                &bound,
                "plan:step-1"
            )
            .unwrap(),
            matrix
        );
        fs::write(&path, [bytes.as_slice(), b"\n"].concat()).unwrap();
        assert!(
            plan_host_source_matrix(
                tmp.path(),
                attempt,
                writer,
                &artifact,
                &bound,
                "plan:step-1"
            )
            .is_err()
        );
        fs::write(&path, &bytes).unwrap();
        bound.path = format!(
            "{}/unapproved.json",
            writer.artifact_area.as_ref().unwrap().path
        );
        fs::write(tmp.path().join(&bound.path), &bytes).unwrap();
        assert!(
            plan_host_source_matrix(
                tmp.path(),
                attempt,
                writer,
                &artifact,
                &bound,
                "plan:step-1"
            )
            .is_err(),
            "matching bytes cannot authorize an unrecorded source"
        );
    }

    #[test]
    fn pending_evidence_owners_require_settled_cancellation() {
        use crate::work_model::*;
        let project = tempfile::tempdir().unwrap();
        let recovery = EvidenceRecovery {
            carried_finding: None,
            id: "pending".into(),
            candidate_commit: "candidate".into(),
            attachment: EvidenceAttachment {
                snapshot_path: "proof.json".into(),
                digest: "sha256:proof".into(),
            },
            targets: vec![],
            state: EvidenceRecoveryState::Reviewing,
            created_at: now_iso8601(),
            required_check: None,
            code_change_classification: None,
        };
        let mut attempt = Attempt::default();
        attempt.cancellation = Some(AttemptCancellation {
            requested_at: now_iso8601(),
            settled_at: None,
            resumed_at: None,
        });
        assert!(
            super::validate_pending_evidence_owners(project.path(), &attempt, &recovery).is_err()
        );
        attempt.cancellation.as_mut().unwrap().settled_at = Some(now_iso8601());
        super::validate_pending_evidence_owners(project.path(), &attempt, &recovery).unwrap();
        assert!(
            attempt.cancellation_requested(),
            "inspection cannot resume a stop"
        );
    }

    #[test]
    fn required_check_status_ignores_unrelated_attempts() {
        let project = tempfile::tempdir().unwrap();
        let mut item = crate::work_model::WorkItem::planned("work-1", "Initial work");
        item.add_initial_attempt("attempt-1").unwrap();
        assert!(
            super::required_check_recovery_status(project.path(), &item.attempts[0])
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn legacy_manifest_requires_complete_default_commands() {
        let results: crate::tester::TesterResults = serde_json::from_value(serde_json::json!({
            "commands":[{"command":"false","test_harness":"shell-harness","exit_code":1,"duration_ms":1,"stdout_log":"commands/0-stdout.log","stderr_log":"commands/0-stderr.log"}],
            "tests":[],"summary":{"total":0,"pass":0,"fail":0,"skipped":0},"error":null
        })).unwrap();
        let config = b"commands:\n  - command: 'false'\n    test_harness: shell-harness\n";
        let manifest = super::reconstruct_legacy_manifest(config, b"extractor", &results).unwrap();
        assert!(manifest.full_run);
        assert!(manifest.run_at.is_none());
        assert!(manifest.environment.is_empty());
        let declared = b"commands:\n  - command: 'false'\n    test_harness: shell-harness\n    applicability: {comparison_refs: [main]}\n";
        assert!(
            super::reconstruct_legacy_manifest(declared, b"extractor", &results)
                .unwrap_err()
                .to_string()
                .contains("historical applicability")
        );
        let mut missing = results.clone();
        missing.commands.clear();
        assert!(super::reconstruct_legacy_manifest(config, b"extractor", &missing).is_err());
    }

    #[test]
    fn failed_evidence_snapshot_rejects_pass_result() {
        let passing = br#"{"schema_version":1,"producer":"host","check":"cargo test","working_directory":".","result":"pass","run_at":"2026-09-02T00:00:00Z","output":"ok"}"#;

        assert_eq!(
            parse_evidence_document(passing).unwrap().result,
            EvidenceResult::Pass
        );
        let error = validate_failed_evidence_snapshot(passing).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("evidence snapshot result must be fail")
        );
    }
}

/// Read recoveries once through the same policy for source selection and
/// Writer inheritance. An invalid approval never falls back to unsplit evidence.
fn recovered_completion_matrices(
    project_root: &Path,
    attempt: &work_model::Attempt,
) -> Result<Vec<(ArtifactRef, work_model::WriterCompletionMatrix)>> {
    let approval = validated_ownership_approval(project_root, attempt)?;
    let mut recovered = Vec::new();
    for artifact in attempt.artifacts.iter().rev().filter(|artifact| {
        matches!(
            artifact.producer_id.as_str(),
            "writer-completion-repair" | "planning-reference-recovery" | "ownership-recovery"
        ) && artifact.path.ends_with("/writer-completion.json")
    }) {
        if artifact.producer_id == "ownership-recovery" {
            let approval = approval
                .as_ref()
                .context("recovered ownership matrix has no recorded approval")?;
            // The latest approved mapping includes every retained obligation.
            // Older mappings remain history, not competing completion authority.
            if artifact != &approval.matrix_artifact {
                continue;
            }
        }
        let path = resolve_input_artifact_path(project_root, &artifact.path)?;
        let metadata = fs::metadata(&path)
            .with_context(|| format!("inspect recovered completion matrix: {}", path.display()))?;
        anyhow::ensure!(
            metadata.len() <= work_model::WRITER_COMPLETION_MATRIX_MAX_BYTES as u64,
            "recovered Writer completion matrix exceeds size limit: {}",
            path.display()
        );
        let matrix: work_model::WriterCompletionMatrix = serde_json::from_slice(&fs::read(&path)?)
            .with_context(|| format!("parse recovered completion matrix: {}", path.display()))?;
        if artifact.producer_id == "writer-completion-repair"
            && !artifact
                .path
                .contains(&format!("/writer-completion-repairs/{}/", matrix.task_id))
        {
            continue;
        }
        recovered.push((artifact.clone(), matrix));
    }
    Ok(recovered)
}

/// Retain a host check across approved Test-reference revisions only when its
/// original bytes and exact Plan row still match the authoritative candidate.
pub(crate) fn plan_host_source_matrix(
    project_root: &Path,
    attempt: &work_model::Attempt,
    writer: &Task,
    current: &ArtifactRef,
    bound: &work_model::BoundRecheckArtifact,
    row_id: &str,
) -> Result<work_model::WriterCompletionMatrix> {
    anyhow::ensure!(
        writer_completion_matrix_ref(project_root, attempt, writer).as_ref() == Some(current),
        "host evidence no longer names the authoritative completion source"
    );
    let bytes = fs::read(resolve_input_artifact_path(project_root, &bound.path)?)?;
    anyhow::ensure!(
        bytes.len() <= work_model::WRITER_COMPLETION_MATRIX_MAX_BYTES
            && format!("sha256:{:x}", Sha256::digest(&bytes)) == bound.digest,
        "host evidence source completion bytes changed"
    );
    let source: work_model::WriterCompletionMatrix = serde_json::from_slice(&bytes)?;
    anyhow::ensure!(
        source.task_id == writer.id,
        "host evidence source Writer changed"
    );
    if bound.path != current.path {
        let approval = validated_ownership_approval(project_root, attempt)?;
        let mut predecessor = current.clone();
        let mut seen = std::collections::HashSet::new();
        while predecessor.path != bound.path {
            anyhow::ensure!(
                seen.insert(predecessor.path.clone()),
                "planning source cycle"
            );
            let revision = attempt
                .artifacts
                .iter()
                .rev()
                .filter(|artifact| {
                    // Only reference revisions produce replacement matrices.
                    // The shared validator above checks Approach audit history
                    // under its own schema; those audits add no matrix edge.
                    artifact.producer_id == "planning-reference-recovery"
                        && artifact.path.ends_with("/revision.json")
                })
                .map(
                    |artifact| -> Result<work_model::TestReferenceRevisionAudit> {
                        Ok(serde_json::from_slice(&fs::read(
                            resolve_input_artifact_path(project_root, &artifact.path)?,
                        )?)?)
                    },
                )
                .collect::<Result<Vec<_>>>()?
                .into_iter()
                .find(|revision| revision.completion_matrix_artifact == predecessor.path)
                .context("host evidence source is outside approved completion history")?;
            if let Some(source) = revision.source_completion {
                predecessor = source.artifact;
            } else {
                // Preserve the original adoption-matrix route for older audits.
                predecessor = approval
                    .as_ref()
                    .context("planning revision has no recorded source")?
                    .matrix_artifact
                    .clone();
            }
        }
        let current_matrix: work_model::WriterCompletionMatrix = serde_json::from_slice(
            &fs::read(resolve_input_artifact_path(project_root, &current.path)?)?,
        )?;
        let source_row = source
            .rows
            .iter()
            .find(|row| {
                row.id == row_id && row.kind == work_model::WriterCompletionRequirementKind::Plan
            })
            .context("host evidence source Plan row disappeared")?;
        anyhow::ensure!(
            current_matrix.task_id == writer.id
                && current_matrix.rows.iter().find(|row| row.id == row_id) == Some(source_row),
            "host evidence Plan obligation changed"
        );
    }
    Ok(source)
}

/// Carry matching immutable claims through already-materialized repair lineage.
pub(crate) fn authoritative_writer_completion_matrix(
    project_root: &Path,
    attempt: &work_model::Attempt,
    writer: &Task,
) -> Result<Option<work_model::WriterCompletionMatrix>> {
    writer_completion::validate_task_recovery_source(project_root, attempt, writer)?;
    let recovered = recovered_completion_matrices(project_root, attempt)?;
    let mut matrix =
        if let Some((_, matrix)) = recovered
            .iter()
            .find(|(_, matrix)| matrix.task_id == writer.id)
        {
            matrix.clone()
        } else {
            let Some(path) = managed_writer_completion_matrix_path(project_root, writer)? else {
                return Ok(None);
            };
            serde_json::from_slice(&fs::read(&path).with_context(|| {
                format!("read prior Writer completion matrix: {}", path.display())
            })?)
            .with_context(|| format!("parse prior Writer completion matrix: {}", path.display()))?
        };
    for (_, repaired) in &recovered {
        matrix.inherit_completed_planning_claims(repaired);
    }
    Ok(Some(matrix))
}

/// Read pending work through the same matrix and assignment contract used by
/// completion. Progress text and current verification cannot retire these rows.
pub(crate) fn pending_writer_completion_rows(
    project_root: &Path,
    attempt: &work_model::Attempt,
    writer: &Task,
) -> Result<Vec<work_model::WriterCompletionRow>> {
    let Some(contract) = attempt.writer_completion_contract.as_ref() else {
        return Ok(Vec::new());
    };
    let mut matrix = authoritative_writer_completion_matrix(project_root, attempt, writer)?
        .context("Writer completion matrix is missing from current context")?;
    let findings = writer_completion_findings(project_root, writer)?;
    apply_accepted_carried_findings(project_root, attempt, writer, &mut matrix)?;
    matrix
        .status_against(contract, &writer.id, &findings)
        .map_err(anyhow::Error::msg)
        .context("validate current Writer completion context")?;
    Ok(matrix.pending_writer_rows())
}

pub(crate) fn writer_completion_matrix_ref(
    project_root: &Path,
    attempt: &work_model::Attempt,
    writer: &Task,
) -> Option<ArtifactRef> {
    if let Some((artifact, _)) = recovered_completion_matrices(project_root, attempt)
        .ok()?
        .into_iter()
        .find(|(_, matrix)| matrix.task_id == writer.id)
    {
        return Some(artifact);
    }
    writer.artifact_area.as_ref().map(|area| ArtifactRef {
        producer_id: writer.id.clone(),
        path: format!(
            "{}/{}",
            area.path,
            crate::recovery_contracts::WRITER_COMPLETION_MATRIX_FILE
        ),
    })
}

/// Select one existing evidence source without granting execution authority.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum EvidenceSelector {
    RequiredCheck(String),
    ReviewArtifact(String),
    PlanRow(String),
    Finding(String),
}

impl EvidenceSelector {
    fn attachment_argument(&self) -> String {
        match self {
            Self::RequiredCheck(id) => format!("--required-check {id}"),
            Self::ReviewArtifact(path) => format!("--review-artifact {path}"),
            Self::PlanRow(id) => format!("--plan-row {id}"),
            Self::Finding(id) => format!("--finding {id}"),
        }
    }
}

pub(crate) struct ExpiredPendingEvidence {
    pub recovery: EvidenceRecovery,
    pub selector: EvidenceSelector,
    pub applicability: work_model::ResolvedCheckApplicability,
    pub command: String,
    pub working_directory: String,
}

/// Identify intact proof that expired before its targeted Review settled.
/// Validate every other binding at the recorded execution time; expiry alone
/// never permits replacing changed sources, candidates or comparison inputs.
pub(crate) fn inspect_expired_pending_evidence(
    project_root: &Path,
    attempt: &work_model::Attempt,
) -> Result<Option<ExpiredPendingEvidence>> {
    let Some(recovery) = attempt.active_evidence_recovery() else {
        return Ok(None);
    };
    if recovery.targets.len() != 1 {
        return Ok(None);
    }
    let target = &recovery.targets[0];
    let (selector, applicability) = if let Some(saved) = &recovery.carried_finding {
        (
            EvidenceSelector::Finding(saved.binding.finding.id.clone()),
            saved.binding.applicability.clone(),
        )
    } else if let Some(check) = &recovery.required_check {
        let selector = match &check.source {
            work_model::RequiredCheckSource::CanonicalTester { .. } => {
                EvidenceSelector::RequiredCheck(check.id.clone())
            }
            work_model::RequiredCheckSource::ReviewRequested { request, .. } => {
                EvidenceSelector::ReviewArtifact(request.path.clone())
            }
        };
        (selector, check.applicability.clone())
    } else if attempt.artifacts.iter().any(|artifact| {
        artifact.producer_id == "plan-host-evidence"
            && artifact.path == target.prior_review_artifact
    }) {
        let (_, bytes) = bound_recovery_artifact(project_root, &target.prior_review_artifact)?;
        let binding: work_model::PlanHostEvidenceBinding = serde_json::from_slice(&bytes)?;
        (
            EvidenceSelector::PlanRow(binding.plan_row),
            binding.applicability,
        )
    } else {
        return Ok(None);
    };
    let work_model::CheckFreshness::LiveState {
        max_age_seconds: Some(seconds),
    } = applicability.definition.freshness
    else {
        return Ok(None);
    };
    let (bound, bytes) = bound_recovery_artifact(project_root, &recovery.attachment.snapshot_path)?;
    anyhow::ensure!(
        bound.digest == recovery.attachment.digest,
        "pending evidence bytes changed"
    );
    let document = parse_evidence_document(&bytes)?;
    let issued = DateTime::parse_from_rfc3339(&document.run_at)?.with_timezone(&chrono::Utc);
    let expiry = chrono::TimeDelta::try_seconds(i64::try_from(seconds)?)
        .and_then(|duration| issued.checked_add_signed(duration))
        .context("unsupported live-state freshness expiry")?;
    if crate::check_applicability::evaluation_time() < expiry {
        return Ok(None);
    }
    validate_owned_plan_evidence_review_at(project_root, attempt, issued, false)?;
    Ok(Some(ExpiredPendingEvidence {
        recovery: recovery.clone(),
        selector,
        applicability,
        command: document.check,
        working_directory: document.working_directory,
    }))
}

/// Require settled owners and only the reservations this proof already owns.
pub(crate) fn validate_pending_evidence_owners(
    project_root: &Path,
    attempt: &work_model::Attempt,
    recovery: &EvidenceRecovery,
) -> Result<()> {
    anyhow::ensure!(
        !attempt.cancellation_requested()
            || attempt
                .cancellation
                .as_ref()
                .is_some_and(|stop| stop.settled_at.is_some()),
        "local cancellation has not settled"
    );
    for task in &attempt.tasks {
        anyhow::ensure!(
            task.status != TaskStatus::Executing
                && !crate::lease::is_leased(&crate::lease::task_lock_path(
                    project_root,
                    &attempt.work_item_id,
                    &task.id
                )),
            "the Attempt has a live Task owner"
        );
        if let Some(area) = &task.artifact_area {
            anyhow::ensure!(
                !crate::execution::execution_is_live(&crate::execution::record_path_for_artifact(
                    project_root,
                    &area.path
                ))?,
                "the Attempt has a live execution owner"
            );
        }
        anyhow::ensure!(
            task.status != TaskStatus::Planned
                || recovery
                    .targets
                    .iter()
                    .any(|target| target.review_task_id.as_deref() == Some(task.id.as_str())),
            "another reserved Task must settle before replacing expired evidence"
        );
    }
    Ok(())
}

#[cfg(test)]
mod applicability_tests {
    use super::*;

    #[test]
    fn evidence_freshness_uses_exact_expiry_and_requires_a_policy() {
        let mut binding = work_model::ResolvedCheckApplicability::default();
        let document: EvidenceDocument = serde_json::from_value(serde_json::json!({
            "schema_version": 1, "producer": "host", "check": "check", "working_directory": ".",
            "result": "pass", "run_at": "2026-09-09T12:00:00Z", "output": "passed"
        }))
        .unwrap();
        let at = |time: &str| {
            DateTime::parse_from_rfc3339(time)
                .unwrap()
                .with_timezone(&chrono::Utc)
        };
        assert!(
            validate_check_applicability_at(&binding, &document, at("2099-01-01T00:00:00Z"))
                .is_ok()
        );
        binding.definition.freshness = work_model::CheckFreshness::LiveState {
            max_age_seconds: Some(60),
        };
        assert!(
            validate_check_applicability_at(&binding, &document, at("2026-09-09T12:00:59Z"))
                .is_ok()
        );
        assert!(
            validate_check_applicability_at(&binding, &document, at("2026-09-09T12:01:00Z"))
                .unwrap_err()
                .to_string()
                .contains("expired")
        );
        binding.definition.freshness = work_model::CheckFreshness::LiveState {
            max_age_seconds: None,
        };
        assert!(
            validate_check_applicability_at(&binding, &document, at("2026-09-09T12:00:00Z"))
                .unwrap_err()
                .to_string()
                .contains("freshness")
        );
    }
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) struct RequiredCheckRecoveryStatus {
    pub can_resume: bool,
    pub reason: String,
    pub next_action: Option<String>,
}

/// Derive admission and guidance from current evidence without publishing state.
pub(crate) fn required_check_recovery_status(
    project_root: &Path,
    attempt: &work_model::Attempt,
) -> Result<Option<RequiredCheckRecoveryStatus>> {
    if attempt
        .evidence_recoveries
        .iter()
        .any(|r| r.carried_finding.is_some())
        || (attempt.pause_kind == Some(work_model::PauseKind::WriterCompletion)
            && attempt
                .tasks
                .iter()
                .any(|t| t.writer_correction_assignment.is_some()))
    {
        if let Some(status) =
            carried_finding::carried_finding_recovery_status(project_root, attempt)?
        {
            return Ok(Some(status));
        }
    }
    if let Some(expired) = inspect_expired_pending_evidence(project_root, attempt)? {
        let settled =
            validate_pending_evidence_owners(project_root, attempt, &expired.recovery).is_ok();
        return Ok(Some(RequiredCheckRecoveryStatus {
            can_resume: false,
            reason: format!(
                "Required check `{}` has expired live-state evidence; submit fresh proof after execution owners settle.",
                expired.command
            ),
            next_action: settled.then(|| {
                format!(
                    "fluent attempt evidence attach {} {} --candidate {} {} --evidence-file <path>",
                    attempt.work_item_id,
                    attempt.id,
                    expired.recovery.candidate_commit,
                    expired.selector.attachment_argument()
                )
            }),
        }));
    }
    let candidate = attempt
        .tasks
        .iter()
        .rev()
        .find(|task| task.kind == TaskKind::Write && task.status == TaskStatus::Complete)
        .and_then(|task| task.output.as_ref())
        .map(|output| output.commit.as_str());
    let has_bound_check = attempt.evidence_recoveries.iter().any(|recovery| {
        recovery
            .required_check
            .as_ref()
            .is_some_and(|check| Some(check.candidate_commit.as_str()) == candidate)
    });
    // A changed Writer candidate still needs its ordinary reviews and first
    // complete Tester. Historical results cannot satisfy or prevent that run.
    // A later failed-command recheck owns its existing recovery route unless
    // an explicit required-check binding still needs validation.
    let has_canonical = attempt
        .tasks
        .iter()
        .rev()
        .find(|task| {
            task.kind == TaskKind::Tester
                && !task.is_failed_command_recheck()
                && task.status == TaskStatus::Complete
                && task
                    .review_context
                    .as_ref()
                    .is_some_and(|context| Some(context.candidate_commit.as_str()) == candidate)
        })
        .is_some();
    if attempt.tasks.iter().any(|task| {
        task.kind == TaskKind::Write
            && !matches!(task.status, TaskStatus::Complete | TaskStatus::Superseded)
    }) {
        return Ok(None);
    }
    if let Some(active) = attempt.active_evidence_recovery() {
        if active.required_check.is_none() || active.state != EvidenceRecoveryState::Reviewing {
            return Ok(None);
        }
        validate_owned_plan_evidence_review(project_root, attempt)?;
        return Ok(Some(RequiredCheckRecoveryStatus {
            can_resume: true,
            reason: format!(
                "Required check {} has bound evidence awaiting targeted Tests Review or settlement.",
                active.required_check.as_ref().unwrap().id
            ),
            next_action: Some(format!(
                "fluent queue resume {} --attempt {}",
                attempt.work_item_id, attempt.id
            )),
        }));
    }
    if !has_canonical {
        return Ok(None);
    }
    if attempt.tasks.iter().any(|task| {
        matches!(task.kind, TaskKind::Review | TaskKind::Tester)
            && matches!(task.status, TaskStatus::Planned | TaskStatus::Executing)
    }) {
        return Ok(None);
    }
    // A completed recheck retains its existing evidence recovery route. This
    // admits recovery work without crediting any unresolved requirement.
    if !has_bound_check
        && attempt
            .tasks
            .iter()
            .rev()
            .find(|task| task.kind == TaskKind::Tester && task.status == TaskStatus::Complete)
            .is_some_and(|task| task.is_failed_command_recheck())
    {
        return Ok(None);
    }
    // Integrity failures remain authoritative even when native failures also
    // exist. Pending verification owners have already been admitted above.
    let canonical = assess_canonical_checks(project_root, attempt)?;
    // Expired independent reviewer checks retain their refresh action below.
    // Integrity failures unrelated to those checks still fail closed here.
    if canonical.has_integrity_blockers {
        return Ok(Some(RequiredCheckRecoveryStatus {
            can_resume: false,
            reason: canonical.blockers.join("\n\n"),
            next_action: None,
        }));
    }
    // An ordinary round scheduled after evidence review still needs aggregate
    // interpretation once its workers finish. Let that transition record any
    // unresolved canonical checks as a pause; this grants no verification credit.
    // Task completion can already mark the Attempt Complete, but NotReviewed
    // still requires interpretation. Paused or interpreted Attempts keep the
    // strict resume checks below.
    if matches!(
        attempt.status,
        work_model::AttemptStatus::Reviewing | work_model::AttemptStatus::Complete
    ) && attempt.review_state == Some(work_model::AttemptReviewState::NotReviewed)
        && attempt.pause_kind.is_none()
        && attempt.tasks.last().is_some_and(|task| {
            task.kind == TaskKind::Review
                && task.status == TaskStatus::Complete
                && task.evidence_review_context.is_none()
                && crate::review::REVIEWERS.contains(&task.role.as_str())
                && task
                    .review_context
                    .as_ref()
                    .is_some_and(|context| Some(context.candidate_commit.as_str()) == candidate)
        })
    {
        return Ok(None);
    }
    if attempt.writer_completion_contract.is_some()
        && attempt
            .tasks
            .iter()
            .any(|task| task.kind == TaskKind::Tester && task.status == TaskStatus::Complete)
    {
        let assessment =
            crate::verification_assessment::assess_current_verification(project_root, attempt)?;
        if !assessment.candidate_defects().is_empty() {
            return Ok(None);
        }
        let gaps = assessment
            .references
            .iter()
            .filter(|row| {
                !row.satisfied
                    && row.cause
                        != Some(crate::verification_assessment::VerificationCause::CandidateDefect)
            })
            .collect::<Vec<_>>();
        if !gaps.is_empty() {
            return Ok(Some(RequiredCheckRecoveryStatus {
                can_resume: false,
                reason: gaps
                    .iter()
                    .map(|row| {
                        format!(
                            "{}: {}. {}",
                            row.id,
                            row.diagnostics.join("; "),
                            row.next_action.as_deref().unwrap_or("")
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n\n"),
                next_action: None,
            }));
        }
    }
    if !has_bound_check && attempt.pause_kind != Some(work_model::PauseKind::HostEvidence) {
        return Ok(None);
    }
    let assessment = assess_canonical_checks(project_root, attempt)?;
    if assessment.is_satisfied() {
        return Ok(Some(RequiredCheckRecoveryStatus { can_resume: true, reason: "The complete canonical run and reviewed evidence satisfy every required check; resume the remaining lifecycle gates.".into(), next_action: Some(format!("fluent queue resume {} --attempt {}", attempt.work_item_id, attempt.id)) }));
    }
    // A known defect follows the existing Writer budget and correction gates.
    if !assessment.defects.is_empty() {
        return Ok(None);
    }
    let mut reasons = assessment.blockers.clone();
    reasons.extend(assessment.diagnoses.iter().cloned());
    reasons.extend(assessment.outstanding.iter().map(|index| {
        let check = &assessment.checks[*index];
        format!(
            "Required check {} remains unresolved: `{}` (working directory {}).",
            check.id, check.command, check.working_directory
        )
    }));
    let owners_settled = attempt.tasks.iter().all(|task| {
        task.status != TaskStatus::Executing
            && !crate::lease::is_leased(&crate::lease::task_lock_path(
                project_root,
                &attempt.work_item_id,
                &task.id,
            ))
            && task.artifact_area.as_ref().is_none_or(|area| {
                !crate::execution::execution_is_live(&crate::execution::record_path_for_artifact(
                    project_root,
                    &area.path,
                ))
                .unwrap_or(true)
            })
    });
    let no_reserved_tasks = attempt
        .tasks
        .iter()
        .all(|task| task.status != TaskStatus::Planned);
    let output = attempt
        .tasks
        .iter()
        .rev()
        .find(|task| task.kind == TaskKind::Write && task.status == TaskStatus::Complete)
        .and_then(|task| task.output.as_ref())
        .context("required check candidate is missing")?;
    let clean = candidate_workspace_is_clean_at_managed(project_root, output, &output.commit);
    if !owners_settled || !no_reserved_tasks {
        reasons.push(
            "Execution owners and reserved Tasks must settle before another attachment.".into(),
        );
    }
    if !clean {
        reasons.push("Restore the unchanged clean managed candidate before attachment.".into());
    }
    let next_action = if owners_settled && no_reserved_tasks && clean {
        assessment.requested.iter().find(|request| request.can_refresh).map(|request| {
            let work_model::RequiredCheckSource::ReviewRequested { request: source, .. } = &request.check.source else { unreachable!() };
            format!("fluent attempt evidence attach {} {} --candidate {} --review-artifact {} --evidence-file <path>", attempt.work_item_id, attempt.id, request.check.candidate_commit, source.path)
        })
    } else { None }.or_else(|| (owners_settled && no_reserved_tasks && clean).then(|| assessment.outstanding.first()).flatten().map(|index| {
        let check = &assessment.checks[*index];
        format!("fluent attempt evidence attach {} {} --candidate {} --required-check {} --evidence-file <path>", attempt.work_item_id, attempt.id, check.candidate_commit, check.id)
    }));
    Ok(Some(RequiredCheckRecoveryStatus {
        can_resume: false,
        reason: reasons.join("\n\n"),
        next_action,
    }))
}

#[cfg(test)]
pub(crate) mod verification_tests {
    use super::*;
    use crate::host_evidence;
    use crate::verification_assessment::assess_current_verification;
    use crate::work_model::*;
    use std::os::unix::fs::PermissionsExt;

    pub(crate) fn fixture(failed_test: bool) -> (tempfile::TempDir, WorkItem) {
        fixture_with_lifetime(failed_test, None)
    }

    pub(crate) fn fixture_with_lifetime(
        failed_test: bool,
        lifetime: Option<u64>,
    ) -> (tempfile::TempDir, WorkItem) {
        let root = tempfile::tempdir().unwrap();
        for args in [
            &["init", "-q"][..],
            &["config", "user.email", "tests@example.test"],
            &["config", "user.name", "Tests"],
        ] {
            crate::git::run(root.path(), args, "prepare assessment fixture").unwrap();
        }
        fs::create_dir_all(root.path().join(".fluent")).unwrap();
        fs::create_dir_all(root.path().join("tests")).unwrap();
        fs::write(root.path().join("tests/native.sh"), "#!/bin/sh\n").unwrap();
        fs::write(root.path().join(".fluent/tester.yaml"), "commands:\n  - command: 'false'\n    test_harness: shell-harness\n  - command: sh -c false\n    test_harness: shell-harness\n").unwrap();
        if let Some(seconds) = lifetime {
            fs::write(root.path().join(".fluent/tester.yaml"), serde_json::to_vec(&serde_json::json!({"commands":[
                {"command":"false","test_harness":"shell-harness","applicability":{"freshness":{"kind":"live-state","max_age_seconds":seconds}}},
                {"command":"sh -c false","test_harness":"shell-harness"}
            ]})).unwrap()).unwrap();
        }
        let native = if failed_test {
            r#"[{"id":"tests/native.sh::broken","test_harness":"shell-harness","status":"fail","failure_excerpt":"assertion failed"}]"#
        } else {
            "[]"
        };
        let extractor = root.path().join(".fluent/extract-tester-results");
        fs::write(
            &extractor,
            format!("#!/bin/sh\nprintf '%s\\n' '{native}'\n"),
        )
        .unwrap();
        fs::set_permissions(&extractor, fs::Permissions::from_mode(0o755)).unwrap();
        crate::git::run(root.path(), &["add", "."], "stage fixture").unwrap();
        crate::git::run(
            root.path(),
            &["commit", "-qm", "Add fixture"],
            "commit fixture",
        )
        .unwrap();
        let candidate =
            crate::git::run_stdout(root.path(), &["rev-parse", "HEAD"], "read candidate").unwrap();
        let mut item = WorkItem::planned("verification", "Assess current evidence");
        item.add_initial_attempt("attempt-1").unwrap();
        let writer = &mut item.attempts[0].tasks[0];
        writer.status = TaskStatus::Complete;
        writer.output = Some(TaskOutput {
            workspace_id: writer.workspace_access.writes[0].id.clone(),
            workspace_path: root.path().display().to_string(),
            source_branch: "candidate".into(),
            base_commit: Some(candidate.clone()),
            commit: candidate.clone(),
            no_change: None,
            learner_canonicalization: None,
        });
        item.add_next_review_tasks("attempt-1", crate::review::REVIEWERS)
            .unwrap();
        for task in item.attempts[0]
            .tasks
            .iter_mut()
            .filter(|task| task.kind == TaskKind::Review)
        {
            task.status = TaskStatus::Complete;
        }
        let id = item.add_final_tester_task("attempt-1").unwrap();
        let tester = item.attempts[0]
            .tasks
            .iter_mut()
            .find(|task| task.id == id)
            .unwrap();
        tester.status = TaskStatus::Complete;
        let area = root
            .path()
            .join(&tester.artifact_area.as_ref().unwrap().path);
        fs::create_dir_all(&area).unwrap();
        crate::tester::run(
            root.path(),
            &area,
            true,
            &crate::content::ContentResolver::new(Some(root.path())),
        )
        .unwrap();
        crate::tester::bind_candidate_commit(&area, &candidate).unwrap();
        item.attempts[0].writer_completion_contract = Some(WriterCompletionContract {
            version: WRITER_COMPLETION_CONTRACT_VERSION,
            requirements: vec![WriterCompletionRequirement {
                responsibility: None,
                id: "behavior:native:b1".into(),
                kind: WriterCompletionRequirementKind::Behavior,
                source: "behaviors.md#native:b1".into(),
                requirement: "THE SYSTEM SHALL work.".into(),
                command: None,
                verification_refs: vec![
                    r#"{"kind":"named-test","path":"tests/native.sh","name":"missing"}"#.into(),
                ],
            }],
        });
        (root, item)
    }

    #[test]
    fn completed_ordinary_review_can_settle_unresolved_checks() {
        let (root, mut item) = fixture(false);
        accept_first_check(root.path(), &mut item);
        item.attempts[0].writer_completion_contract = None;
        let review_ids = item.add_next_review_tasks("attempt-1", &["tests"]).unwrap();
        for task in item.attempts[0]
            .tasks
            .iter_mut()
            .filter(|task| review_ids.contains(&task.id))
        {
            task.status = TaskStatus::Complete;
            let area = root.path().join(&task.artifact_area.as_ref().unwrap().path);
            fs::create_dir_all(&area).unwrap();
            fs::write(area.join("review.md"), "Verdict: pass\n").unwrap();
        }
        for status in [AttemptStatus::Reviewing, AttemptStatus::Complete] {
            item.attempts[0].status = status;
            let admission = required_check_recovery_status(root.path(), &item.attempts[0]).unwrap();
            assert!(
                admission.as_ref().is_none_or(|status| status.can_resume),
                "completed ordinary review must reach aggregate interpretation: {admission:?}"
            );
        }
        // Interpreting the round must still leave the unrelated failed command
        // blocked. The same evidence cannot authorize another resume afterward.
        item.attempts[0].status = AttemptStatus::NeedsUser;
        item.attempts[0].pause_kind = Some(PauseKind::HostEvidence);
        item.attempts[0].review_state = Some(AttemptReviewState::Failed);
        let blocked = required_check_recovery_status(root.path(), &item.attempts[0])
            .unwrap()
            .unwrap();
        assert!(!blocked.can_resume);
        assert!(blocked.reason.contains("sh -c false"));
    }

    pub(crate) fn unique_harness_fixture() -> (tempfile::TempDir, WorkItem) {
        unique_harness_fixture_with_comparison(false)
    }

    pub(crate) fn unique_harness_fixture_with_comparison(
        comparison: bool,
    ) -> (tempfile::TempDir, WorkItem) {
        let (root, mut item) = fixture(true);
        let applicability = if comparison {
            crate::git::run(
                root.path(),
                &["branch", "baseline"],
                "record comparison ref",
            )
            .unwrap();
            "    applicability: {comparison_refs: [baseline]}\n"
        } else {
            ""
        };
        fs::write(
            root.path().join("tests/native.sh"),
            "#!/bin/sh\necho 'not ok broken'\n",
        )
        .unwrap();
        fs::write(
            root.path().join(".fluent/tester.yaml"),
            format!(
                "commands:\n  - command: 'false'\n    test_harness: shell-harness\n{applicability}"
            ),
        )
        .unwrap();
        crate::git::run(
            root.path(),
            &["add", ".fluent/tester.yaml", "tests/native.sh"],
            "stage unique command",
        )
        .unwrap();
        crate::git::run(
            root.path(),
            &["commit", "-qm", "Use unique harness"],
            "commit unique command",
        )
        .unwrap();
        let candidate =
            crate::git::run_stdout(root.path(), &["rev-parse", "HEAD"], "read unique candidate")
                .unwrap();
        for task in &mut item.attempts[0].tasks {
            if let Some(output) = &mut task.output {
                output.commit = candidate.clone();
            }
            if let Some(context) = &mut task.review_context {
                context.candidate_commit = candidate.clone();
            }
        }
        let tester = item.attempts[0]
            .tasks
            .iter()
            .find(|task| task.kind == TaskKind::Tester)
            .unwrap();
        let area = root
            .path()
            .join(&tester.artifact_area.as_ref().unwrap().path);
        crate::tester::run(
            root.path(),
            &area,
            true,
            &crate::content::ContentResolver::new(Some(root.path())),
        )
        .unwrap();
        crate::tester::bind_candidate_commit(&area, &candidate).unwrap();
        item.attempts[0]
            .writer_completion_contract
            .as_mut()
            .unwrap()
            .requirements[0]
            .verification_refs =
            vec![r#"{"kind":"named-test","path":"tests/native.sh","name":"broken"}"#.into()];
        (root, item)
    }

    #[test]
    fn reviewed_command_satisfies_named_reference_without_rewriting_results() {
        let (root, mut item) = unique_harness_fixture();
        let before = assess_canonical_checks(root.path(), &item.attempts[0]).unwrap();
        let original = fs::read(&before.results_path).unwrap();
        assert!(
            !assess_current_verification(root.path(), &item.attempts[0])
                .unwrap()
                .references[0]
                .satisfied
        );
        accept_first_check(root.path(), &mut item);
        let current = assess_current_verification(root.path(), &item.attempts[0]).unwrap();
        assert!(current.commands[0].satisfied);
        assert!(
            current.references[0].satisfied,
            "{:?}",
            current.references[0]
        );
        let canonical = assess_canonical_checks(root.path(), &item.attempts[0]).unwrap();
        assert!(canonical.defects.is_empty());
        assert!(canonical.blockers.is_empty());
        assert_eq!(fs::read(before.results_path).unwrap(), original);
    }

    pub(crate) fn accept_first_check(root: &Path, item: &mut WorkItem) {
        accept_check(root, item, 0);
    }

    pub(crate) fn accept_check(root: &Path, item: &mut WorkItem, index: usize) {
        let attempt = &mut item.attempts[0];
        let check = assess_canonical_checks(root, attempt).unwrap().checks[index].clone();
        let suffix = if index == 0 {
            String::new()
        } else {
            format!("-{index}")
        };
        let recovery_id = format!("accepted{suffix}");
        let area = work_artifact_path(&item.id, &attempt.id, &format!("accepted-proof{suffix}"));
        fs::create_dir_all(root.join(&area)).unwrap();
        let prior = format!("{area}/check.json");
        fs::write(root.join(&prior), serde_json::to_vec(&check).unwrap()).unwrap();
        let proof_path = format!("{area}/evidence.json");
        let proof = serde_json::to_vec(&serde_json::json!({"schema_version":1,"producer":"authorized host","check":check.command,"working_directory":".","result":"pass","output":"passed","run_at":now_iso8601(),"comparison_commits":check.applicability.comparison_commits})).unwrap();
        fs::write(root.join(&proof_path), &proof).unwrap();
        let attachment = EvidenceAttachment {
            snapshot_path: proof_path,
            digest: crate::verification_policy::digest(&proof),
        };
        let mut review = attempt
            .tasks
            .iter()
            .find(|task| task.kind == TaskKind::Review && task.role == "tests")
            .unwrap()
            .clone();
        review.id = format!("accepted-tests-review{suffix}");
        review.artifact_area.as_mut().unwrap().path = format!("{area}/review");
        fs::create_dir_all(root.join(format!("{area}/review"))).unwrap();
        fs::write(
            root.join(format!("{area}/review/review.md")),
            "Verdict: pass\n",
        )
        .unwrap();
        review.evidence_review_context = Some(EvidenceReviewContext {
            recovery_id: recovery_id.clone(),
            candidate_commit: check.candidate_commit.clone(),
            attachment: attachment.clone(),
            prior_review_artifact: prior.clone(),
        });
        attempt.evidence_recoveries.push(EvidenceRecovery {
            carried_finding: None,
            id: recovery_id,
            candidate_commit: check.candidate_commit.clone(),
            attachment,
            targets: vec![EvidenceReviewTarget {
                role: "tests".into(),
                prior_review_artifact: prior,
                review_task_id: Some(review.id.clone()),
                review_digest: Some(crate::verification_policy::digest(b"Verdict: pass\n")),
            }],
            state: EvidenceRecoveryState::Complete,
            created_at: now_iso8601(),
            required_check: Some(check),
            code_change_classification: None,
        });
        attempt.tasks.push(review);
    }

    #[test]
    fn native_defect_remains_actionable_with_missing_reference() {
        let (root, item) = fixture(true);
        assert!(
            required_check_recovery_status(root.path(), &item.attempts[0])
                .unwrap()
                .is_none()
        );
        host_evidence::ensure_owned_host_resume_ready(root.path(), &item.attempts[0]).unwrap();
    }

    #[test]
    fn changed_candidate_can_run_reviews_and_its_first_tester() {
        let (root, mut item) = fixture(true);
        fs::write(root.path().join("candidate-change"), "repair").unwrap();
        crate::git::run(root.path(), &["add", "candidate-change"], "stage repair").unwrap();
        crate::git::run(
            root.path(),
            &["commit", "-qm", "Repair candidate"],
            "commit repair",
        )
        .unwrap();
        let commit =
            crate::git::run_stdout(root.path(), &["rev-parse", "HEAD"], "read repair").unwrap();
        let mut writer = item.attempts[0].tasks[0].clone();
        writer.id = "attempt-1-write-2".into();
        writer.output.as_mut().unwrap().commit = commit.clone();
        item.attempts[0].tasks.push(writer);
        item.attempts[0].status = AttemptStatus::Executing;
        item.attempts[0].pause_kind = None;
        host_evidence::ensure_owned_host_resume_ready(root.path(), &item.attempts[0]).unwrap();
        let pending = assess_current_verification(root.path(), &item.attempts[0]).unwrap();
        assert_eq!(pending.candidate_commit, commit);
        assert!(pending.commands.is_empty());
        assert!(!pending.references[0].satisfied);
        item.add_next_review_tasks("attempt-1", &["tests"]).unwrap();
        for task in &mut item.attempts[0].tasks {
            if task.kind == TaskKind::Review {
                task.status = TaskStatus::Complete;
            }
        }
        item.add_final_tester_task("attempt-1").unwrap();
        host_evidence::ensure_owned_host_resume_ready(root.path(), &item.attempts[0]).unwrap();
    }

    #[test]
    fn missing_reference_allows_pending_targeted_evidence_review() {
        let (root, mut item) = fixture(false);
        accept_first_check(root.path(), &mut item);
        let workspace = root.path().join(".fluent/work/workspaces/candidate");
        crate::git::run(
            root.path(),
            &[
                "worktree",
                "add",
                "--detach",
                workspace.to_str().unwrap(),
                "HEAD",
            ],
            "prepare managed candidate",
        )
        .unwrap();
        item.attempts[0].tasks[0]
            .output
            .as_mut()
            .unwrap()
            .workspace_path = workspace.display().to_string();
        let attempt = &mut item.attempts[0];
        attempt.status = AttemptStatus::Executing;
        attempt.pause_kind = Some(PauseKind::HostEvidence);
        let recovery = &mut attempt.evidence_recoveries[0];
        recovery.state = EvidenceRecoveryState::Reviewing;
        let target = &mut recovery.targets[0];
        let bytes = fs::read(root.path().join(&target.prior_review_artifact)).unwrap();
        let prior = snapshot_path(
            &item.id,
            &attempt.id,
            &crate::verification_policy::digest(&bytes),
        )
        .unwrap();
        fs::create_dir_all(root.path().join(&prior).parent().unwrap()).unwrap();
        fs::write(root.path().join(&prior), bytes).unwrap();
        target.prior_review_artifact = prior.clone();
        target.review_digest = None;
        let review = attempt
            .tasks
            .iter_mut()
            .find(|task| target.review_task_id.as_deref() == Some(task.id.as_str()))
            .unwrap();
        review.status = TaskStatus::Planned;
        review
            .evidence_review_context
            .as_mut()
            .unwrap()
            .prior_review_artifact = prior;
        let status = required_check_recovery_status(root.path(), attempt)
            .unwrap()
            .unwrap();
        assert!(status.can_resume, "{}", status.reason);
        host_evidence::ensure_owned_host_resume_ready(root.path(), attempt).unwrap();
        assert!(
            !assess_current_verification(root.path(), attempt)
                .unwrap()
                .references[0]
                .satisfied
        );
    }

    #[test]
    fn accepted_evidence_survives_unrelated_failures_and_no_change_rounds() {
        let (root, mut item) = fixture(false);
        accept_first_check(root.path(), &mut item);
        let path = assess_canonical_checks(root.path(), &item.attempts[0])
            .unwrap()
            .results_path;
        let original = fs::read(&path).unwrap();
        let first = assess_current_verification(root.path(), &item.attempts[0]).unwrap();
        assert!(first.commands[0].satisfied);
        assert_eq!(first.commands[0].owner, CompletionResponsibility::Host);
        assert!(!first.commands[1].satisfied && !first.references[0].satisfied);
        assert!(
            !first.commands[0]
                .bindings
                .iter()
                .all(|binding| binding.path.ends_with("tester-results.json")),
            "accepted evidence must expose its supporting proof bindings"
        );
        let mut later = item.attempts[0].tasks[0].clone();
        later.id = "attempt-1-write-2".into();
        let area = work_artifact_path(&item.id, "attempt-1", &later.id);
        later.artifact_area.as_mut().unwrap().path = area.clone();
        let declaration = format!("{area}/no-change.json");
        later.output.as_mut().unwrap().no_change = Some(NoChangeOutput {
            schema_version: 1,
            declaration_path: declaration.clone(),
            reason: "Evidence remains applicable".into(),
            verification: vec![NoChangeVerification {
                command: "true".into(),
                result: NoChangeVerificationResult::Pass,
            }],
        });
        fs::create_dir_all(root.path().join(&area)).unwrap();
        fs::write(
            root.path().join(declaration),
            serde_json::to_vec(&later.output.as_ref().unwrap().no_change).unwrap(),
        )
        .unwrap();
        item.attempts[0].tasks.push(later);
        let unchanged = assess_current_verification(root.path(), &item.attempts[0]).unwrap();
        assert_eq!(first, unchanged);
        assert_eq!(fs::read(&path).unwrap(), original);
        let attachment = &item.attempts[0].evidence_recoveries[0]
            .attachment
            .snapshot_path;
        fs::write(root.path().join(attachment), "changed proof").unwrap();
        assert!(
            !assess_current_verification(root.path(), &item.attempts[0])
                .unwrap()
                .commands[0]
                .satisfied
        );
        assert_eq!(fs::read(path).unwrap(), original);
        let (live_root, mut live) = fixture_with_lifetime(false, Some(60));
        accept_first_check(live_root.path(), &mut live);
        let document = parse_evidence_document(
            &fs::read(
                live_root.path().join(
                    &live.attempts[0].evidence_recoveries[0]
                        .attachment
                        .snapshot_path,
                ),
            )
            .unwrap(),
        )
        .unwrap();
        let issued = DateTime::parse_from_rfc3339(&document.run_at)
            .unwrap()
            .with_timezone(&chrono::Utc);
        assert!(
            crate::verification_assessment::assess_current_verification_at(
                live_root.path(),
                &live.attempts[0],
                issued + chrono::TimeDelta::seconds(30)
            )
            .unwrap()
            .commands[0]
                .satisfied
        );
        assert!(
            !crate::verification_assessment::assess_current_verification_at(
                live_root.path(),
                &live.attempts[0],
                issued + chrono::TimeDelta::seconds(60)
            )
            .unwrap()
            .commands[0]
                .satisfied
        );
    }

    #[test]
    fn diagnostic_text_does_not_change_requirements() {
        let (root, item) = fixture(true);
        let attempt = &item.attempts[0];
        let first = assess_current_verification(root.path(), attempt).unwrap();
        let path = assess_canonical_checks(root.path(), attempt)
            .unwrap()
            .results_path;
        let mut results: crate::tester::TesterResults =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        results.tests[0].failure_excerpt = Some("the same assertion, explained differently".into());
        fs::write(&path, serde_json::to_vec(&results).unwrap()).unwrap();
        let second = assess_current_verification(root.path(), attempt).unwrap();
        let identities = |view: &crate::verification_assessment::VerificationAssessment| {
            view.unresolved()
                .map(|row| (row.id.clone(), row.cause.clone(), row.owner))
                .collect::<Vec<_>>()
        };
        assert_eq!(identities(&first), identities(&second));
        assert_eq!(
            first.references.len(),
            1,
            "identity lists are diagnostics, not extra requirements"
        );
        results.summary.total += 1;
        fs::write(&path, serde_json::to_vec(&results).unwrap()).unwrap();
        let invalid = assess_current_verification(root.path(), attempt).unwrap();
        results.tests[0].failure_excerpt = Some("another explanation of the same facts".into());
        fs::write(&path, serde_json::to_vec(&results).unwrap()).unwrap();
        assert_eq!(
            identities(&invalid),
            identities(&assess_current_verification(root.path(), attempt).unwrap())
        );
    }
}

/// Recheck current correction ownership before a saved or direct Writer starts.
pub(crate) fn validate_verification_writer_start(
    project_root: &Path,
    attempt: &work_model::Attempt,
    task: &Task,
) -> Result<()> {
    if task.role == work_model::TESTER_HARNESS_REPAIR_ROLE {
        return Ok(());
    }
    let mut verification_input = false;
    let candidate = attempt
        .tasks
        .iter()
        .rev()
        .find(|task| task.kind == TaskKind::Write && task.status == TaskStatus::Complete)
        .and_then(|task| task.output.as_ref())
        .map(|output| output.commit.as_str());
    for artifact in &task.input_artifacts {
        if artifact.path.ends_with("/tester-results.json") {
            verification_input = true;
        }
        if artifact.path.ends_with("/candidate-gate.json") {
            let path = resolve_input_artifact_path(project_root, &artifact.path)?;
            let gate = candidate_gate::read_prior_writer_gate(&path).map_err(anyhow::Error::msg)?;
            verification_input = true;
            if gate.phase != candidate_gate::CandidateGatePhase::FinalTester
                && Some(gate.candidate_commit.as_str()) == candidate
                && gate.checks.iter().any(|check| {
                    check.status == candidate_gate::CandidateCheckStatus::Failed
                        && matches!(
                            check.name.as_str(),
                            "whitespace" | "commit-metadata" | "project-check"
                        )
                })
            {
                return Ok(());
            }
        }
        if artifact.path.ends_with("/review.md") {
            let source =
                fs::read_to_string(resolve_input_artifact_path(project_root, &artifact.path)?)?;
            if review::extract_verdict(&source) == review::Verdict::Fail
                && !review::open_finding_titles(&source).is_empty()
            {
                return Ok(());
            }
        }
    }
    if !verification_input {
        return Ok(());
    }
    let assessment =
        crate::verification_assessment::assess_current_verification(project_root, attempt)
            .context(
                "verification recovery needs current bound evidence before a corrective Writer",
            )?;
    anyhow::ensure!(
        !assessment.candidate_defects().is_empty(),
        "verification recovery must establish a candidate defect before a corrective Writer; inspect these requirements and their retained evidence: {}",
        assessment
            .unresolved()
            .map(|row| format!(
                "{}: {}",
                row.id,
                row.next_action.as_deref().unwrap_or("inspect evidence")
            ))
            .collect::<Vec<_>>()
            .join("; ")
    );
    Ok(())
}
