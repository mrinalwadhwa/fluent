//! Recover one exact review frontier through an audited Approach revision.

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::ErrorKind;
use std::path::Path;

use crate::work_model::{
    ApproachRevisionAudit as RevisionAudit, ApproachRevisionKind, ApproachRevisionSupersession,
    ApprovedPlanningRevision, ArtifactRef, Attempt, AttemptStatus, DurableTransitionMetadata,
    DurableTransitionOrigin, LegacyApproachReconciliationReceipt as LegacyReconciliationReceipt,
    ManagedWorkItemFile, PauseKind, RoundCapExtension, RoundCapExtensionRequest,
    RoundCapReviewArtifact, Task, TaskKind, TaskStatus, WorkModelError, WorkModelStore,
    WriterCompletionContract, WriterCompletionRequirementKind, work_artifact_path,
};

const PRODUCER: &str = "planning-approach-recovery";
const UNCERTAIN_SCHEMA_VERSION: u32 = 1;
const ROUND_CAP_SCHEMA_VERSION: u32 = 2;
const CORRECTIVE_WRITER_ROLE: &str = "approach-revision-correction";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApproachRevision {
    pub revision_id: String,
    pub audit_artifact: String,
    pub writer_task_id: String,
    pub already_applied: bool,
}

/// Replace the approved Approach only at the narrowly audited review frontier.
pub fn revise(
    project_root: &Path,
    store: &WorkModelStore,
    work_item_id: &str,
    attempt_id: &str,
    replacement_bytes: &[u8],
    resolution: &str,
) -> Result<ApproachRevision> {
    let _lock = crate::land_lock::acquire(&crate::land_lock::lock_path(project_root))
        .context("acquire Approach-revision recovery lock")?;
    let replacement = std::str::from_utf8(replacement_bytes)
        .context("replacement Approach must be UTF-8")?
        .to_owned();
    if replacement.trim().is_empty() {
        bail!("--approach-file must contain a complete replacement Approach");
    }
    let resolution = resolution.trim();
    if resolution.is_empty() {
        bail!("--resolution must record the human authorization");
    }

    let item = store.read_work_item(work_item_id)?;
    item.ensure_mutation_compatible()?;
    let context = item
        .planning_context
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Work Item has no stored planning context"))?;
    let prior_approach = context
        .approach
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("Work Item has no approved Approach"))?;
    let attempt = item
        .attempts
        .iter()
        .find(|attempt| attempt.id == attempt_id)
        .ok_or_else(|| anyhow::anyhow!("Attempt {attempt_id:?} not found"))?;
    crate::recovery_contracts::validate_planning_completion_history(project_root, attempt)?;
    let revision_id = format!(
        "revision-{}",
        &digest(format!("{}\0{}\0{}", prior_approach, replacement, resolution).as_bytes())[..12]
    );
    let current_round_cap_frontier = (attempt.status == AttemptStatus::NeedsUser
        && attempt.pause_kind == Some(PauseKind::RoundCap)
        && attempt.review_state == Some(crate::work_model::AttemptReviewState::Failed))
    .then(|| frontier(project_root, attempt))
    .transpose()?;
    if let Some(existing) = existing_revision(
        project_root,
        &item,
        attempt,
        context,
        &replacement,
        resolution,
        current_round_cap_frontier.as_ref(),
    )? {
        return Ok(existing);
    }
    let audited_frontier = match current_round_cap_frontier {
        Some(frontier) => frontier,
        None => frontier(project_root, attempt)?,
    };
    let legacy_cap_extension = audited_frontier.legacy_reconciliation.is_some();
    let round_cap_revision = audited_frontier.round_cap_extension.is_some();
    if !round_cap_revision
        && attempt.materialized_write_rounds()
            >= attempt.effective_write_round_cap(crate::work_attempt_loop::max_total_write_rounds())
        && !legacy_cap_extension
    {
        bail!("Approach revision has no remaining authorized Writer round");
    }
    let old_planning_contract = WriterCompletionContract::from_planning_context(context)
        .ok_or_else(|| anyhow::anyhow!("Work Item has no Writer completion contract"))?;
    let old_contract = attempt
        .writer_completion_contract
        .as_ref()
        .context("Attempt has no authoritative completion contract")?;
    let ownership_approval =
        crate::recovery_contracts::validated_ownership_approval(project_root, attempt)?;
    // An approved ownership split may intentionally make the Attempt contract
    // richer than the retained planning markdown. Keep the Attempt contract
    // authoritative while validating the replacement against both contracts.
    let mut revised_context = context.clone();
    revised_context.approach = Some(replacement.clone());
    if let Some(combined) = revised_context
        .combined
        .as_mut()
        .filter(|value| !value.trim().is_empty())
    {
        if combined.matches(prior_approach).count() != 1 {
            bail!("combined planning authority cannot be revised safely");
        }
        *combined = combined.replacen(prior_approach, &replacement, 1);
    }
    revised_context.approved_revisions.insert(
        "approach".to_string(),
        ApprovedPlanningRevision {
            revision_id: format!("approach:{}", digest(replacement.as_bytes())),
            sha256: digest(replacement.as_bytes()),
        },
    );
    let new_planning_contract =
        WriterCompletionContract::from_planning_context(&revised_context)
            .ok_or_else(|| anyhow::anyhow!("replacement Approach lost the completion contract"))?;
    validate_contract_delta(&old_planning_contract, &new_planning_contract)?;
    let source_completion = source_writer(
        project_root,
        attempt,
        &audited_frontier.candidate_commit,
        old_contract,
    )
    .map(|(_, completion)| completion);
    if round_cap_revision && source_completion.is_none() {
        bail!("round-cap Approach revision requires source completion evidence");
    }
    let new_contract = old_contract
        .with_revised_approach(prior_approach, &replacement)
        .map_err(anyhow::Error::msg)?;
    validate_contract_delta(old_contract, &new_contract)?;
    if new_contract != new_planning_contract && ownership_approval.is_none() {
        bail!("planning authority and Attempt completion contract differ");
    }
    let (review_refs, supporting_reviews) =
        if let Some(reviews) = audited_frontier.supporting_reviews.as_ref() {
            (
                reviews
                    .iter()
                    .map(|review| review.artifact.clone())
                    .collect(),
                reviews.clone(),
            )
        } else {
            (
                settled_round_review_artifacts(attempt, audited_frontier.source_review)?,
                Vec::new(),
            )
        };
    let round_cap_reviews = legacy_cap_extension
        .then(|| {
            review_refs
                .iter()
                .map(|artifact| {
                    Ok(RoundCapReviewArtifact {
                        artifact: artifact.clone(),
                        digest: format!(
                            "sha256:{}",
                            digest(&fs::read(project_root.join(&artifact.path))?)
                        ),
                    })
                })
                .collect::<Result<Vec<_>>>()
        })
        .transpose()?;

    let mut audit = RevisionAudit {
        schema_version: if round_cap_revision {
            ROUND_CAP_SCHEMA_VERSION
        } else {
            UNCERTAIN_SCHEMA_VERSION
        },
        revision_kind: if round_cap_revision {
            ApproachRevisionKind::FailedReviewRoundCap
        } else {
            ApproachRevisionKind::UncertainArchitecture
        },
        revision_id: revision_id.clone(),
        work_item_id: work_item_id.to_string(),
        attempt_id: attempt_id.to_string(),
        candidate_commit: audited_frontier.candidate_commit.clone(),
        source_review_task_id: audited_frontier.source_review.id.clone(),
        superseded_task_id: audited_frontier
            .failed_launch
            .as_ref()
            .map(|task| task.id.clone()),
        prior_contract: Some(old_contract.clone()),
        source_completion: source_completion.clone(),
        prior_approach_sha256: digest(prior_approach.as_bytes()),
        replacement_approach_sha256: digest(replacement.as_bytes()),
        prior_approach: prior_approach.to_string(),
        replacement_approach: replacement,
        resolution: resolution.to_string(),
        resolution_sha256: digest(resolution.as_bytes()),
        legacy_reconciliation: audited_frontier.legacy_reconciliation.clone(),
        round_cap_extension: audited_frontier.round_cap_extension.clone(),
        supporting_reviews,
    };
    let (revision_id, audit_artifact) = select_revision_identity(
        project_root,
        work_item_id,
        attempt_id,
        &revision_id,
        &audit,
        round_cap_revision,
    )?;
    audit.revision_id = revision_id.clone();
    let audit_bytes = serde_json::to_vec_pretty(&audit)?;
    let managed_files = [ManagedWorkItemFile::new(
        audit_artifact.clone(),
        audit_bytes.clone(),
    )];
    #[cfg(feature = "test-support")]
    if std::env::var("FLUENT_TEST_APPROACH_REVISION_INTERRUPT")
        .ok()
        .as_deref()
        == Some("before-work-model-publication")
    {
        bail!("test interruption before Approach revision Work-model publication");
    }
    let source_id = audited_frontier.source_review.id.clone();
    let candidate_commit = audited_frontier.candidate_commit.clone();
    let operation = crate::work_model::RecoveryOperationRequest::new(
        "planning-approach-revision",
        work_item_id,
        attempt_id,
        Some(candidate_commit.clone()),
        Some(source_id.clone()),
        None,
        None,
        Some(resolution.to_string()),
        Some(digest(&audit_bytes)),
    );
    let _ = item.validate_recovery_operation(project_root, &operation)?;
    let expected_context = context.clone();
    let expected_source_completion = source_completion.clone();
    let expected_supporting_reviews = audited_frontier.supporting_reviews.clone();
    let audit_ref = ArtifactRef {
        producer_id: PRODUCER.to_string(),
        path: audit_artifact.clone(),
    };
    let failed_id = audited_frontier
        .failed_launch
        .as_ref()
        .map(|task| task.id.clone());
    let metadata = DurableTransitionMetadata {
        origin: DurableTransitionOrigin::External,
        reason: Some("revise approved Approach".to_string()),
        evidence_ref: Some(audit_artifact.clone()),
    };
    let writer_task_id = store.mutate_work_item_with_transition_and_files(
        work_item_id,
        &metadata,
        &managed_files,
        |fresh| {
        if fresh.planning_context.as_ref() != Some(&expected_context) {
            return Err(WorkModelError::AttemptResumeIneligible { attempt_id: attempt_id.to_string(), reason: "planning context changed before Approach revision".to_string() });
        }
        let index = fresh.attempts.iter().position(|attempt| attempt.id == attempt_id)
            .ok_or_else(|| WorkModelError::AttemptNotFound { id: attempt_id.to_string() })?;
        let current_is_legacy = {
            let current = frontier(project_root, &fresh.attempts[index]).map_err(|error| WorkModelError::AttemptResumeIneligible { attempt_id: attempt_id.to_string(), reason: format!("Approach-revision frontier changed: {error:#}") })?;
            if current.candidate_commit != candidate_commit
                || current.source_review.id != source_id
                || current.failed_launch.as_ref().map(|task| &task.id) != failed_id.as_ref()
                || current.supporting_reviews != expected_supporting_reviews
            {
                return Err(WorkModelError::AttemptResumeIneligible { attempt_id: attempt_id.to_string(), reason: "candidate or review frontier changed before Approach revision".to_string() });
            }
            if current.round_cap_extension != audited_frontier.round_cap_extension {
                return Err(WorkModelError::AttemptResumeIneligible {
                    attempt_id: attempt_id.to_string(),
                    reason: "round-cap authorization changed before Approach revision".to_string(),
                });
            }
            current.legacy_reconciliation.is_some()
        };
        let fresh_context = fresh
            .planning_context
            .as_ref()
            .ok_or_else(|| WorkModelError::AttemptResumeIneligible {
                attempt_id: attempt_id.to_string(),
                reason: "planning context disappeared before Approach revision".to_string(),
            })?;
        if WriterCompletionContract::from_planning_context(fresh_context).is_none() {
            return Err(WorkModelError::AttemptResumeIneligible {
                attempt_id: attempt_id.to_string(),
                reason: "planning context lost its completion contract".to_string(),
            });
        }
        let fresh_source_completion = source_writer(
            project_root,
            &fresh.attempts[index],
            &candidate_commit,
            old_contract,
        )
        .map(|(_, completion)| completion);
        if fresh_source_completion != expected_source_completion {
            return Err(WorkModelError::AttemptResumeIneligible {
                attempt_id: attempt_id.to_string(),
                reason: "source completion evidence changed before Approach revision".to_string(),
            });
        }
        let attempt = &mut fresh.attempts[index];
        if legacy_cap_extension
            && current_is_legacy
            && attempt.materialized_write_rounds()
                >= attempt.effective_write_round_cap(crate::work_attempt_loop::max_total_write_rounds())
        {
            let old_cap = attempt
                .effective_write_round_cap(crate::work_attempt_loop::max_total_write_rounds());
            attempt.record_round_cap_extension(
                crate::work_attempt_loop::max_total_write_rounds(),
                RoundCapExtensionRequest {
                    old_cap,
                    additional_write_rounds: 1,
                    candidate_commit: candidate_commit.clone(),
                    failed_reviews: round_cap_reviews.clone().unwrap_or_default(),
                    human_resolution: resolution.to_string(),
                },
            )?;
        }
        if fresh.attempts[index].writer_completion_contract.as_ref() != Some(old_contract) {
            return Err(WorkModelError::AttemptResumeIneligible {
                attempt_id: attempt_id.to_string(), reason: "completion contract changed before Approach revision".into(),
            });
        }
        fresh.planning_context = Some(revised_context.clone());
        let mut inputs = vec![audit_ref.clone()];
        for review in &review_refs {
            if !inputs.contains(review) { inputs.push(review.clone()); }
        }
        let task_id = fresh.add_next_write_round(attempt_id, inputs)?;
        let attempt = &mut fresh.attempts[index];
        attempt.writer_completion_contract = Some(new_contract.clone());
        if !attempt.artifacts.contains(&audit_ref) { attempt.artifacts.push(audit_ref.clone()); }
        if let Some(failed_id) = &failed_id
            && !attempt
                .tasks
                .iter()
                .any(|task| task.id == *failed_id && task.status == TaskStatus::Failed)
        {
            return Err(WorkModelError::TaskNotFound {
                id: failed_id.clone(),
            });
        }
        let writer = attempt.tasks.iter_mut().find(|task| task.id == task_id)
            .ok_or_else(|| WorkModelError::TaskNotFound { id: task_id.clone() })?;
        writer.role = CORRECTIVE_WRITER_ROLE.to_string();
        writer.instructions = Some(format!("Implement the approved Approach revision and resolve every actionable finding from the settled review round. Human resolution: {resolution}"));
        if let Some(failed_task_id) = failed_id {
            attempt.approach_revision_superseded_tasks.push(ApproachRevisionSupersession {
                failed_task_id,
                audit_artifact: audit_ref.clone(),
                source_review_task_id: source_id.clone(),
                candidate_commit: candidate_commit.clone(),
                corrective_writer_task_id: task_id.clone(),
            });
        }
        attempt.status = AttemptStatus::Planned;
        attempt.pause_kind = None;
        attempt.pause_recovery = None;
        attempt.completed_at = None;
        let next_action = crate::work_status::derive_attempt_recovery_next_action(
            Some(attempt),
            Some(project_root),
        );
        attempt.record_recovery_operation(&operation, next_action)?;
        Ok(task_id)
        },
    )?;
    Ok(ApproachRevision {
        revision_id,
        audit_artifact,
        writer_task_id,
        already_applied: false,
    })
}

struct Frontier<'a> {
    source_review: &'a Task,
    failed_launch: Option<&'a Task>,
    candidate_commit: String,
    legacy_reconciliation: Option<LegacyReconciliationReceipt>,
    supporting_reviews: Option<Vec<RoundCapReviewArtifact>>,
    round_cap_extension: Option<RoundCapExtension>,
}

/// Select the completed Writer whose output is the reviewed candidate and whose
/// completion matrix validates against the contract the revision was approved
/// against.
///
/// The reviewed candidate, not the latest Writer, is the stable anchor of an
/// Approach revision: a corrective or no-change Writer may later preserve the
/// same candidate while writing a matrix against the revised contract. Prefer
/// the newest Writer whose matrix still validates against the historical
/// contract so a later round cannot replace the recorded source completion;
/// fall back to earlier Writers.
fn source_writer<'a>(
    project_root: &Path,
    attempt: &'a Attempt,
    candidate_commit: &str,
    contract: &WriterCompletionContract,
) -> Option<(&'a Task, crate::work_model::PlanningCompletionSource)> {
    attempt
        .tasks
        .iter()
        .rev()
        .filter(|task| {
            task.kind == TaskKind::Write
                && task.status == TaskStatus::Complete
                && task
                    .output
                    .as_ref()
                    .is_some_and(|output| output.commit == candidate_commit)
        })
        .find_map(|task| {
            capture_source_completion(project_root, attempt, task, contract)
                .ok()
                .flatten()
                .map(|completion| (task, completion))
        })
}

fn frontier<'a>(project_root: &Path, attempt: &'a Attempt) -> Result<Frontier<'a>> {
    if attempt.status == AttemptStatus::NeedsUser
        && attempt.pause_kind == Some(PauseKind::RoundCap)
        && attempt.review_state == Some(crate::work_model::AttemptReviewState::Failed)
    {
        let extension = attempt
            .round_cap_extensions
            .last()
            .ok_or_else(|| anyhow::anyhow!("round-cap pause has no approved extension"))?;
        let checked = crate::recovery_contracts::validate_round_cap_approach_frontier(
            project_root,
            attempt,
            extension,
        )?;
        let source_review = attempt
            .tasks
            .iter()
            .find(|task| task.id == checked.source_review_task_id)
            .ok_or_else(|| anyhow::anyhow!("Architecture review is not in the Attempt"))?;
        return Ok(Frontier {
            source_review,
            failed_launch: None,
            candidate_commit: checked.candidate_commit,
            legacy_reconciliation: None,
            supporting_reviews: Some(checked.supporting_reviews),
            round_cap_extension: Some(checked.extension),
        });
    }

    let terminal_failure = attempt.status == AttemptStatus::Failed;
    if !(attempt.status == AttemptStatus::NeedsUser
        && attempt.pause_kind == Some(PauseKind::Uncertain))
        && !terminal_failure
    {
        bail!("Attempt is not paused at an uncertain Architecture review frontier");
    }
    let last = attempt
        .tasks
        .last()
        .ok_or_else(|| anyhow::anyhow!("Attempt has no Tasks"))?;
    let legacy_reconciliation = if terminal_failure {
        if has_typed_provider_argument_handoff(project_root, attempt, last)? {
            None
        } else {
            Some(legacy_reconciliation_receipt(project_root, attempt, last)?)
        }
    } else {
        None
    };
    let failed_launch = terminal_failure.then_some(last);
    let search = if terminal_failure {
        &attempt.tasks[..attempt.tasks.len() - 1]
    } else {
        &attempt.tasks[..]
    };
    let source_review = search
        .iter()
        .rev()
        .find(|task| {
            task.kind == TaskKind::Review
                && task.role == "architecture"
                && task.status == TaskStatus::Complete
        })
        .ok_or_else(|| anyhow::anyhow!("Attempt has no settled Architecture review"))?;
    let current_candidate = attempt
        .tasks
        .iter()
        .rev()
        .find(|task| task.kind == TaskKind::Write && task.status == TaskStatus::Complete)
        .and_then(|task| task.output.as_ref())
        .map(|output| output.commit.clone())
        .ok_or_else(|| anyhow::anyhow!("Attempt has no completed Writer candidate"))?;
    let context = source_review
        .review_context
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Architecture review has no candidate context"))?;
    if context.candidate_commit != current_candidate {
        bail!(
            "the latest Architecture review {:?} is not bound to the current candidate {current_candidate}; run reviews on the corrected candidate before another Approach revision",
            source_review.id
        );
    }
    let candidate_commit = context.candidate_commit.clone();
    let source_artifact = review_artifact(source_review)?;
    if !fs::read_to_string(project_root.join(source_artifact.path))
        .is_ok_and(|text| text.contains("Verdict: uncertain"))
    {
        bail!("Architecture review did not record an uncertain verdict");
    }
    if let Some(failed_launch) = failed_launch {
        let failed_context = failed_launch
            .review_context
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("failed reviewer launch has no candidate context"))?;
        if failed_context.candidate_commit != context.candidate_commit {
            bail!("failed reviewer launch is not bound to the uncertain review frontier");
        }
        let source_index = attempt
            .tasks
            .iter()
            .position(|task| task.id == source_review.id)
            .ok_or_else(|| anyhow::anyhow!("Architecture review is not in the Attempt"))?;
        let source_round = review_round(source_review)
            .and_then(|round| round.parse::<usize>().ok())
            .ok_or_else(|| anyhow::anyhow!("Architecture review has no numbered review round"))?;
        let failed_round = review_round(failed_launch)
            .and_then(|round| round.parse::<usize>().ok())
            .ok_or_else(|| {
                anyhow::anyhow!("failed reviewer launch has no numbered review round")
            })?;
        if source_round != 38
            || failed_round != 39
            || failed_round != source_round + 1
            || attempt.tasks[source_index + 1..]
                .iter()
                .take_while(|task| task.id != failed_launch.id)
                .any(|task| {
                    task.kind != TaskKind::Review
                        || task.status != TaskStatus::Complete
                        || review_round(task).and_then(|round| round.parse::<usize>().ok())
                            != Some(source_round)
                })
        {
            bail!(
                "failed reviewer launch does not immediately follow the uncertain review frontier"
            );
        }
    }
    Ok(Frontier {
        source_review,
        failed_launch,
        candidate_commit,
        legacy_reconciliation,
        supporting_reviews: None,
        round_cap_extension: None,
    })
}

fn legacy_reconciliation_receipt(
    project_root: &Path,
    attempt: &Attempt,
    failed: &Task,
) -> Result<LegacyReconciliationReceipt> {
    validate_artifact_free_failed_reviewer_launch(project_root, attempt, failed)?;
    if attempt
        .tasks
        .iter()
        .filter(|task| task.status == TaskStatus::Failed)
        .count()
        != 1
    {
        bail!("legacy recovery requires exactly one failed reviewer launch");
    }
    let expected_handoff = work_artifact_path(
        &attempt.work_item_id,
        &attempt.id,
        &format!("needs-user-{}.md", failed.id),
    );
    let handoff = attempt
        .artifacts
        .iter()
        .find(|artifact| artifact.producer_id == failed.id && artifact.path == expected_handoff)
        .ok_or_else(|| anyhow::anyhow!("legacy failed reviewer launch has no generic handoff"))?;
    let handoff_bytes = fs::read(project_root.join(&handoff.path))?;
    let handoff_text = std::str::from_utf8(&handoff_bytes)?;
    if !handoff_text.starts_with("# Attempt needs user input") {
        bail!("legacy reviewer handoff is not the preserved generic form");
    }
    let area = failed
        .artifact_area
        .as_ref()
        .expect("artifact-free validation requires an artifact area");
    let directory =
        crate::work_task_executor::resolve_managed_artifact_area_path(project_root, &area.path)?;
    let stderr_bytes = fs::read(directory.join(crate::coder::PROVIDER_STDERR_FILENAME))?;
    let stderr = std::str::from_utf8(&stderr_bytes)?;
    let excerpt = legacy_codex_parser_excerpt(stderr).ok_or_else(|| {
        anyhow::anyhow!("legacy reviewer stderr is not a Codex CLI parser rejection")
    })?;
    let execution_bytes = fs::read(directory.join("execution.json"))?;
    let execution: crate::execution::ExecutionRecord = serde_json::from_slice(&execution_bytes)?;
    if execution.state != crate::execution::ExecutionState::Settled {
        bail!("legacy reviewer execution record is not settled");
    }
    let candidate_commit = failed
        .review_context
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("legacy failed reviewer launch has no candidate context"))?
        .candidate_commit
        .clone();
    Ok(LegacyReconciliationReceipt {
        failed_review_task_id: failed.id.clone(),
        candidate_commit,
        handoff_sha256: digest(&handoff_bytes),
        stderr_sha256: digest(&stderr_bytes),
        execution_sha256: digest(&execution_bytes),
        diagnostic_excerpt: excerpt,
    })
}

fn has_typed_provider_argument_handoff(
    project_root: &Path,
    attempt: &Attempt,
    failed: &Task,
) -> Result<bool> {
    validate_artifact_free_failed_reviewer_launch(project_root, attempt, failed)?;
    Ok(attempt.artifacts.iter().any(|artifact| {
        artifact.producer_id == failed.id
            && artifact.path.ends_with("/needs-user.md")
            && fs::read_to_string(project_root.join(&artifact.path)).is_ok_and(|text| {
                text.starts_with("# Provider arguments need repair")
                    && text.contains("pass-through provider arguments")
            })
    }))
}

fn validate_artifact_free_failed_reviewer_launch(
    project_root: &Path,
    attempt: &Attempt,
    failed: &Task,
) -> Result<()> {
    if failed.kind != TaskKind::Review
        || failed.status != TaskStatus::Failed
        || failed.output.is_some()
    {
        bail!("terminal Attempt is not an artifact-free failed reviewer launch");
    }
    if attempt
        .artifacts
        .iter()
        .any(|artifact| artifact.producer_id == failed.id && artifact.path.ends_with("/review.md"))
    {
        bail!("legacy failed reviewer launch has a review artifact");
    }
    let area = failed
        .artifact_area
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("legacy failed reviewer launch has no artifact area"))?;
    let directory =
        crate::work_task_executor::resolve_managed_artifact_area_path(project_root, &area.path)?;
    if directory.join("review.md").exists() {
        bail!("legacy failed reviewer launch has review output");
    }
    Ok(())
}

fn legacy_codex_parser_excerpt(stderr: &str) -> Option<String> {
    let normalized = stderr.to_ascii_lowercase();
    if !normalized.contains("usage: codex")
        || !["unknown argument", "unexpected argument", "unknown option"]
            .iter()
            .any(|marker| normalized.contains(marker))
    {
        return None;
    }
    Some(stderr.chars().take(512).collect())
}

fn review_round(task: &Task) -> Option<String> {
    task.id
        .split("-review-")
        .nth(1)
        .and_then(|value| value.split('-').next())
        .filter(|value| value.chars().all(|character| character.is_ascii_digit()))
        .map(str::to_owned)
}

fn settled_round_review_artifacts(attempt: &Attempt, source: &Task) -> Result<Vec<ArtifactRef>> {
    let candidate = source
        .review_context
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Architecture review has no candidate context"))?;
    let round = review_round(source);
    let mut artifacts = Vec::new();
    for task in &attempt.tasks {
        if task.kind != TaskKind::Review || task.status != TaskStatus::Complete {
            continue;
        }
        let task_round = review_round(task);
        let same_round = match (round.as_deref(), task_round.as_deref()) {
            (Some(expected), Some(actual)) => expected == actual,
            (None, None) => true,
            _ => false,
        };
        if same_round
            && task
                .review_context
                .as_ref()
                .is_some_and(|context| context.candidate_commit == candidate.candidate_commit)
        {
            artifacts.push(review_artifact(task)?);
        }
    }
    (!artifacts.is_empty())
        .then_some(artifacts)
        .ok_or_else(|| anyhow::anyhow!("settled review round has no review artifacts"))
}

fn review_artifact(task: &Task) -> Result<ArtifactRef> {
    let area = task
        .artifact_area
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Architecture review has no artifact area"))?;
    Ok(ArtifactRef {
        producer_id: task.id.clone(),
        path: format!("{}/review.md", area.path),
    })
}

fn validate_contract_delta(
    old: &WriterCompletionContract,
    new: &WriterCompletionContract,
) -> Result<()> {
    for before in old
        .requirements
        .iter()
        .filter(|row| row.kind != WriterCompletionRequirementKind::Approach)
    {
        let after = new.requirements.iter().find(|row| row.id == before.id);
        if after != Some(before) {
            bail!(
                "replacement Approach changed non-Approach completion requirement {:?}",
                before.id
            );
        }
    }
    if new.requirements.iter().any(|row| {
        row.kind != WriterCompletionRequirementKind::Approach && !old.requirements.contains(row)
    }) {
        bail!("replacement Approach added a non-Approach completion requirement");
    }
    Ok(())
}

fn existing_revision(
    project_root: &Path,
    item: &crate::work_model::WorkItem,
    attempt: &Attempt,
    context: &crate::work_model::PlanningContext,
    replacement: &str,
    resolution: &str,
    current_round_cap_frontier: Option<&Frontier<'_>>,
) -> Result<Option<ApproachRevision>> {
    for artifact in attempt.artifacts.iter().rev() {
        if artifact.producer_id != PRODUCER || !artifact.path.ends_with("/revision.json") {
            continue;
        }
        let audit: RevisionAudit =
            serde_json::from_slice(&fs::read(project_root.join(&artifact.path))?)?;
        if audit.replacement_approach == replacement && audit.resolution == resolution {
            if let Some(frontier) = current_round_cap_frontier {
                let expected = expected_round_cap_revision_audit(
                    project_root,
                    attempt,
                    context,
                    frontier,
                    replacement,
                    resolution,
                )?;
                if audit.revision_kind != ApproachRevisionKind::FailedReviewRoundCap
                    || !audit_identity_matches(&audit, &expected)
                {
                    continue;
                }
            }
            if audit.revision_kind == ApproachRevisionKind::FailedReviewRoundCap {
                crate::recovery_contracts::validate_round_cap_approach_revision_retry(
                    project_root,
                    item,
                    attempt,
                    &audit,
                    artifact,
                    false,
                )?;
            }
            let writer = attempt
                .tasks
                .iter()
                .find(|task| {
                    task.role == CORRECTIVE_WRITER_ROLE
                        && task.input_artifacts.iter().any(|input| input == artifact)
                })
                .ok_or_else(|| {
                    anyhow::anyhow!("Approach revision audit has no corrective Writer")
                })?;
            return Ok(Some(ApproachRevision {
                revision_id: audit.revision_id,
                audit_artifact: artifact.path.clone(),
                writer_task_id: writer.id.clone(),
                already_applied: true,
            }));
        }
    }
    Ok(None)
}

fn expected_round_cap_revision_audit(
    project_root: &Path,
    attempt: &Attempt,
    context: &crate::work_model::PlanningContext,
    frontier: &Frontier<'_>,
    replacement: &str,
    resolution: &str,
) -> Result<RevisionAudit> {
    let old_contract = attempt
        .writer_completion_contract
        .as_ref()
        .context("Attempt has no authoritative completion contract")?;
    let source_completion = source_writer(
        project_root,
        attempt,
        &frontier.candidate_commit,
        old_contract,
    )
    .map(|(_, completion)| completion)
    .context("round-cap Approach revision requires source completion evidence")?;
    let prior_approach = context
        .approach
        .as_deref()
        .context("Work Item has no approved Approach")?;
    let extension = frontier
        .round_cap_extension
        .clone()
        .context("round-cap frontier has no approved extension")?;

    Ok(RevisionAudit {
        schema_version: ROUND_CAP_SCHEMA_VERSION,
        revision_kind: ApproachRevisionKind::FailedReviewRoundCap,
        revision_id: String::new(),
        work_item_id: attempt.work_item_id.clone(),
        attempt_id: attempt.id.clone(),
        candidate_commit: frontier.candidate_commit.clone(),
        source_review_task_id: frontier.source_review.id.clone(),
        superseded_task_id: frontier.failed_launch.as_ref().map(|task| task.id.clone()),
        prior_contract: Some(old_contract.clone()),
        source_completion: Some(source_completion),
        prior_approach_sha256: digest(prior_approach.as_bytes()),
        replacement_approach_sha256: digest(replacement.as_bytes()),
        prior_approach: prior_approach.to_string(),
        replacement_approach: replacement.to_string(),
        resolution: resolution.to_string(),
        resolution_sha256: digest(resolution.as_bytes()),
        legacy_reconciliation: None,
        round_cap_extension: Some(extension),
        supporting_reviews: frontier.supporting_reviews.clone().unwrap_or_default(),
    })
}

fn audit_identity_matches(left: &RevisionAudit, right: &RevisionAudit) -> bool {
    let mut left = left.clone();
    let mut right = right.clone();
    left.revision_id.clear();
    right.revision_id.clear();
    left == right
}

fn select_revision_identity(
    project_root: &Path,
    work_item_id: &str,
    attempt_id: &str,
    legacy_revision_id: &str,
    audit: &RevisionAudit,
    use_full_frontier_identity: bool,
) -> Result<(String, String)> {
    let legacy_artifact = work_artifact_path(
        work_item_id,
        attempt_id,
        &format!("planning-approach-revisions/{legacy_revision_id}/revision.json"),
    );
    let legacy_path = project_root.join(&legacy_artifact);
    let existing_bytes = match fs::read(&legacy_path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            return if use_full_frontier_identity {
                full_revision_identity(work_item_id, attempt_id, audit)
            } else {
                Ok((legacy_revision_id.to_string(), legacy_artifact))
            };
        }
        Err(error) => return Err(error.into()),
    };
    let Ok(existing) = serde_json::from_slice::<RevisionAudit>(&existing_bytes) else {
        // Leave an unparseable sidecar at the legacy path so the transaction
        // publisher can fail closed instead of replacing conflicting data.
        return Ok((legacy_revision_id.to_string(), legacy_artifact));
    };
    if !use_full_frontier_identity || audit_identity_matches(&existing, audit) {
        return Ok((legacy_revision_id.to_string(), legacy_artifact));
    }
    full_revision_identity(work_item_id, attempt_id, audit)
}

fn full_revision_identity(
    work_item_id: &str,
    attempt_id: &str,
    audit: &RevisionAudit,
) -> Result<(String, String)> {
    let mut identity = audit.clone();
    identity.revision_id.clear();
    let full_revision_id = format!(
        "revision-{}",
        &digest(&serde_json::to_vec(&identity)?)[..12]
    );
    let full_artifact = work_artifact_path(
        work_item_id,
        attempt_id,
        &format!("planning-approach-revisions/{full_revision_id}/revision.json"),
    );
    Ok((full_revision_id, full_artifact))
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn capture_source_completion(
    project_root: &Path,
    attempt: &Attempt,
    writer: &Task,
    contract: &WriterCompletionContract,
) -> Result<Option<crate::work_model::PlanningCompletionSource>> {
    let Some(artifact) =
        crate::recovery_contracts::writer_completion_matrix_ref(project_root, attempt, writer)
    else {
        return Ok(None);
    };
    let path =
        crate::recovery_contracts::resolve_input_artifact_path(project_root, &artifact.path)?;
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let matrix: crate::work_model::WriterCompletionMatrix = serde_json::from_slice(&bytes)?;
    let findings = crate::recovery_contracts::writer_completion_findings(project_root, writer)?;
    matrix
        .status_against(contract, &writer.id, &findings)
        .map_err(anyhow::Error::msg)?;
    Ok(Some(crate::work_model::PlanningCompletionSource {
        artifact,
        sha256: digest(&bytes),
        contract: contract.clone(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::work_model::{PlanningContext, TaskOutput, WorkItem};

    // Keep the approved behavior selectors visible to the candidate gate and
    // tied to this module's regression fixture.
    const BEHAVIOR_TEST_SELECTORS: &[&str] = &[
        "tests::revises_approach_and_records_candidate_bound_audit",
        "tests::revision_binds_the_source_writer_matching_the_uncertain_review",
        "tests::corrective_writer_commit_preserves_revision_resume",
        "tests::stale_uncertain_review_refuses_revision_with_a_named_reason",
        "tests::second_revision_after_no_change_corrective_writer_binds_the_current_contract",
        "tests::recovers_legacy_generic_handoff_with_codex_parser_diagnostics",
        "tests::schedules_documentation_correction_and_targeted_reviews",
        "tests::repeated_revision_is_idempotent",
        "tests::rejects_unrelated_legacy_launch_failures_without_mutation",
    ];

    const APPROACH: &str = "# Approach\n\n## Key decisions\n\n### Keep the boundary\n\nChoice: Keep it narrow.\n\n## Structure\n\nRecover under a lock.\n";
    const REPLACEMENT: &str = "# Approach\n\n## Key decisions\n\n### Recover the boundary\n\nChoice: Keep recovery narrow.\n\n## Structure\n\nRecover under a lock.\n";
    const SECOND_REPLACEMENT: &str = "# Approach\n\n## Key decisions\n\n### Recover the boundary twice\n\nChoice: Keep recovery narrow and ordered.\n\n## Structure\n\nRecover under a lock.\n";

    fn fixture() -> (tempfile::TempDir, WorkModelStore) {
        let tmp = tempfile::tempdir().unwrap();
        let store = WorkModelStore::new(tmp.path());
        let mut item = WorkItem::planned("work-1", "Recover Approach");
        item.planning_context = Some(PlanningContext {
            behaviors: Some("## Recovery\n\n### B1\n\nWHEN revised, THE SYSTEM SHALL recover.\nTest: recovery_test\n".to_string()),
            approach: Some(APPROACH.to_string()),
            plan: Some("| # | State reached | Behaviors | Verification | Req? |\n|---|---|---|---|---|\n| 1 | Recovered | Recovery:B1 | recovery_test | Yes |\n".to_string()),
            ..Default::default()
        });
        item.add_initial_attempt("attempt-1").unwrap();
        let writer = &mut item.attempts[0].tasks[0];
        writer.status = TaskStatus::Complete;
        writer.output = Some(TaskOutput {
            workspace_id: "candidate".to_string(),
            workspace_path: "../candidate".to_string(),
            source_branch: "work-1".to_string(),
            base_commit: Some("base".to_string()),
            commit: "candidate".to_string(),
            no_change: None,
            learner_canonicalization: None,
        });
        item.add_review_tasks("attempt-1", &["architecture", "documentation"])
            .unwrap();
        let attempt = &mut item.attempts[0];
        for task in attempt
            .tasks
            .iter_mut()
            .filter(|task| task.kind == TaskKind::Review)
        {
            task.status = TaskStatus::Complete;
        }
        attempt.status = AttemptStatus::NeedsUser;
        attempt.pause_kind = Some(PauseKind::Uncertain);
        for task in attempt
            .tasks
            .iter()
            .filter(|task| task.kind == TaskKind::Review)
        {
            let area = task.artifact_area.as_ref().unwrap();
            let path = tmp.path().join(&area.path).join("review.md");
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            let body = if task.role == "architecture" {
                "# Architecture review\n\nVerdict: uncertain\n\nArchitecture finding.\n"
            } else {
                "# Documentation review\n\nVerdict: findings\n\nDocumentation finding.\n"
            };
            fs::write(path, body).unwrap();
        }
        store.create_work_item(&item).unwrap();
        (tmp, store)
    }

    fn write_source_matrix(
        root: &Path,
        writer: &Task,
        contract: &WriterCompletionContract,
    ) -> std::path::PathBuf {
        let path = root
            .join(&writer.artifact_area.as_ref().unwrap().path)
            .join("writer-completion.json");
        let findings = crate::recovery_contracts::writer_completion_findings(root, writer).unwrap();
        let matrix = crate::work_model::WriterCompletionMatrix::skeleton(
            contract, &writer.id, &findings, None,
        )
        .unwrap();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, serde_json::to_vec_pretty(&matrix).unwrap()).unwrap();
        path
    }

    fn assert_rejected_without_mutation(tmp: &tempfile::TempDir, store: &WorkModelStore) {
        let before = serde_json::to_vec(&store.read_work_item("work-1").unwrap()).unwrap();
        assert!(
            revise(
                tmp.path(),
                store,
                "work-1",
                "attempt-1",
                REPLACEMENT.as_bytes(),
                "Approve recovery."
            )
            .is_err()
        );
        assert_eq!(
            serde_json::to_vec(&store.read_work_item("work-1").unwrap()).unwrap(),
            before
        );
        assert!(
            !tmp.path()
                .join(work_artifact_path(
                    "work-1",
                    "attempt-1",
                    "planning-approach-revisions"
                ))
                .exists()
        );
    }

    fn assert_rejected_state_unchanged(
        tmp: &tempfile::TempDir,
        store: &WorkModelStore,
        replacement: &[u8],
    ) {
        let before = serde_json::to_vec(&store.read_work_item("work-1").unwrap()).unwrap();
        assert!(
            revise(
                tmp.path(),
                store,
                "work-1",
                "attempt-1",
                replacement,
                "Approve recovery."
            )
            .is_err()
        );
        assert_eq!(
            serde_json::to_vec(&store.read_work_item("work-1").unwrap()).unwrap(),
            before
        );
    }

    fn legacy_terminal_fixture() -> (tempfile::TempDir, WorkModelStore) {
        let (tmp, store) = fixture();
        store.mutate_work_item("work-1", |item| {
            item.attempts[0].tasks[1].id = "attempt-1-review-38-architecture".to_string();
            item.attempts[0].tasks[2].id = "attempt-1-review-38-documentation".to_string();
            let mut failed = item.attempts[0].tasks[1].clone();
            failed.id = "attempt-1-review-39-architecture".to_string();
            failed.artifact_area.as_mut().unwrap().path = work_artifact_path(
                "work-1", "attempt-1", "attempt-1-review-39-architecture",
            );
            failed.status = TaskStatus::Failed;
            failed.output = None;
            item.attempts[0].tasks.push(failed);
            let handoff = work_artifact_path(
                "work-1",
                "attempt-1",
                "needs-user-attempt-1-review-39-architecture.md",
            );
            fs::write(tmp.path().join(&handoff), "# Attempt needs user input\n\nlaunch failed\n").unwrap();
            item.attempts[0].artifacts.push(ArtifactRef {
                producer_id: "attempt-1-review-39-architecture".to_string(), path: handoff,
            });
            item.attempts[0].status = AttemptStatus::Failed;
            let area = item.attempts[0].tasks.last().unwrap().artifact_area.as_ref().unwrap();
            let directory = tmp.path().join(&area.path);
            fs::create_dir_all(&directory).unwrap();
            fs::write(directory.join(crate::coder::PROVIDER_STDERR_FILENAME),
                "error: unexpected argument '--replacement-approach' found\n\nUsage: codex exec [OPTIONS] [PROMPT]\n").unwrap();
            fs::write(directory.join("execution.json"),
                r#"{"schema_version":1,"state":"settled","owner_pid":1,"leader_pid":1,"process_group_id":1,"started_at":"now","heartbeat_at":"now","settled_at":"now"}"#).unwrap();
            Ok(())
        }).unwrap();
        (tmp, store)
    }

    fn typed_terminal_fixture() -> (tempfile::TempDir, WorkModelStore) {
        let (tmp, store) = legacy_terminal_fixture();
        store
            .mutate_work_item("work-1", |item| {
                let handoff = work_artifact_path("work-1", "attempt-1", "needs-user.md");
                fs::write(
                    tmp.path().join(&handoff),
                    "# Provider arguments need repair\n\npass-through provider arguments\n",
                )
                .unwrap();
                let failed_id = item.attempts[0].tasks.last().unwrap().id.clone();
                item.attempts[0]
                    .artifacts
                    .retain(|artifact| artifact.producer_id != failed_id);
                item.attempts[0].artifacts.push(ArtifactRef {
                    producer_id: failed_id,
                    path: handoff,
                });
                Ok(())
            })
            .unwrap();
        (tmp, store)
    }

    #[test]
    fn behavior_selector_manifest_covers_recovery_regressions() {
        assert_eq!(BEHAVIOR_TEST_SELECTORS.len(), 9);
    }

    #[test]
    fn replays_reference_approach_reference_history() {
        use crate::planning_reference_recovery::{self, TestReferenceUpdate};
        use crate::work_model::{AttemptReviewState, WriterCompletionMatrix};

        let (tmp, store) = fixture();
        let root = tmp.path();
        let workspace = root.join("candidate");
        fs::create_dir_all(&workspace).unwrap();
        for name in ["recovery_test", "revised_test", "final_test"] {
            fs::write(workspace.join(name), "#!/bin/sh\nexit 0\n").unwrap();
        }
        for args in [
            vec!["init", "--initial-branch=main"],
            vec!["config", "user.email", "test@example.com"],
            vec!["config", "user.name", "Test"],
            vec!["config", "commit.gpgsign", "false"],
            vec!["add", "."],
            vec!["commit", "-qm", "Add reference sources"],
        ] {
            crate::git::run(&workspace, &args, "prepare candidate sources").unwrap();
        }
        let candidate =
            crate::git::run_stdout(&workspace, &["rev-parse", "HEAD"], "read candidate").unwrap();
        store
            .mutate_work_item("work-1", |item| {
                let attempt = &mut item.attempts[0];
                for task in &mut attempt.tasks {
                    if let Some(output) = &mut task.output {
                        output.workspace_path = "candidate".into();
                        output.commit = candidate.clone();
                        output.base_commit = Some(candidate.clone());
                    }
                    if let Some(context) = &mut task.review_context {
                        context.candidate_workspace_path = "candidate".into();
                        context.candidate_commit = candidate.clone();
                        context.base_commit = Some(candidate.clone());
                    }
                    for workspace in &mut task.workspace_access.reads {
                        if workspace.id == "candidate" {
                            workspace.path = "candidate".into();
                        }
                    }
                }
                let writer = &attempt.tasks[0];
                let matrix = WriterCompletionMatrix::skeleton(
                    attempt.writer_completion_contract.as_ref().unwrap(),
                    &writer.id,
                    &[],
                    None,
                )
                .unwrap();
                let path = root
                    .join(&writer.artifact_area.as_ref().unwrap().path)
                    .join("writer-completion.json");
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(path, serde_json::to_vec_pretty(&matrix).unwrap()).unwrap();
                item.add_next_review_tasks("attempt-1", &["behaviors", "tests"])?;
                for task in item.attempts[0]
                    .tasks
                    .iter_mut()
                    .filter(|task| task.kind == TaskKind::Review)
                {
                    task.status = TaskStatus::Complete;
                    let path = root
                        .join(&task.artifact_area.as_ref().unwrap().path)
                        .join("review.md");
                    if !path.exists() {
                        fs::create_dir_all(path.parent().unwrap()).unwrap();
                        fs::write(path, "Verdict: pass\n").unwrap();
                    }
                }
                item.attempts[0].status = AttemptStatus::NeedsUser;
                item.attempts[0].pause_kind = Some(PauseKind::RoundCap);
                item.attempts[0].review_state = Some(AttemptReviewState::Failed);
                Ok(())
            })
            .unwrap();
        planning_reference_recovery::revise(
            root,
            &store,
            "work-1",
            "attempt-1",
            &[TestReferenceUpdate {
                requirement_id: "behavior:recovery:b1".into(),
                reference: "revised_test".into(),
            }],
            "Correct the reference.",
        )
        .unwrap();
        store
            .mutate_work_item("work-1", |item| {
                for task in item.attempts[0].tasks.iter_mut().filter(|task| {
                    task.kind == TaskKind::Review && task.status != TaskStatus::Complete
                }) {
                    task.status = TaskStatus::Complete;
                    let path = root
                        .join(&task.artifact_area.as_ref().unwrap().path)
                        .join("review.md");
                    fs::create_dir_all(path.parent().unwrap()).unwrap();
                    fs::write(path, "Verdict: pass\n").unwrap();
                }
                item.attempts[0].status = AttemptStatus::NeedsUser;
                item.attempts[0].pause_kind = Some(PauseKind::Uncertain);
                Ok(())
            })
            .unwrap();
        revise(
            root,
            &store,
            "work-1",
            "attempt-1",
            REPLACEMENT.as_bytes(),
            "Approve the Approach.",
        )
        .unwrap();
        store
            .mutate_work_item("work-1", |item| {
                let attempt = &mut item.attempts[0];
                let output = attempt.tasks[0].output.clone();
                let writer = attempt.tasks.last_mut().unwrap();
                assert_eq!(writer.kind, TaskKind::Write);
                writer.status = TaskStatus::Complete;
                writer.output = output;
                let findings =
                    crate::recovery_contracts::writer_completion_findings(root, writer).unwrap();
                let matrix = WriterCompletionMatrix::skeleton(
                    attempt.writer_completion_contract.as_ref().unwrap(),
                    &writer.id,
                    &findings,
                    None,
                )
                .unwrap();
                let path = root
                    .join(&writer.artifact_area.as_ref().unwrap().path)
                    .join("writer-completion.json");
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(path, serde_json::to_vec_pretty(&matrix).unwrap()).unwrap();
                attempt.status = AttemptStatus::NeedsUser;
                attempt.pause_kind = Some(PauseKind::RoundCap);
                attempt.review_state = Some(AttemptReviewState::Failed);
                Ok(())
            })
            .unwrap();
        planning_reference_recovery::revise(
            root,
            &store,
            "work-1",
            "attempt-1",
            &[TestReferenceUpdate {
                requirement_id: "behavior:recovery:b1".into(),
                reference: "final_test".into(),
            }],
            "Correct the final reference.",
        )
        .unwrap();
        let item = store.read_work_item("work-1").unwrap();
        let attempt = &item.attempts[0];
        crate::recovery_contracts::validate_planning_completion_history(root, attempt).unwrap();
        assert_eq!(
            attempt.writer_completion_contract,
            WriterCompletionContract::from_planning_context(
                item.planning_context.as_ref().unwrap()
            )
        );
        let mut missing_middle = attempt.clone();
        missing_middle
            .artifacts
            .retain(|artifact| artifact.producer_id != PRODUCER);
        assert!(
            crate::recovery_contracts::validate_planning_completion_history(root, &missing_middle)
                .is_err()
        );
    }

    #[test]
    fn revises_approach_and_records_candidate_bound_audit() {
        let (tmp, store) = fixture();
        let revision = revise(
            tmp.path(),
            &store,
            "work-1",
            "attempt-1",
            REPLACEMENT.as_bytes(),
            "Adopt the reviewed recovery architecture.",
        )
        .unwrap();
        let stored = store.read_work_item("work-1").unwrap();
        let audit: RevisionAudit =
            serde_json::from_slice(&fs::read(tmp.path().join(&revision.audit_artifact)).unwrap())
                .unwrap();
        let context = stored.planning_context.as_ref().unwrap();
        assert_eq!(context.approach.as_deref(), Some(REPLACEMENT));
        assert_eq!(audit.work_item_id, "work-1");
        assert_eq!(audit.attempt_id, "attempt-1");
        assert_eq!(audit.candidate_commit, "candidate");
        assert_eq!(audit.source_review_task_id, "attempt-1-review-architecture");
        assert_eq!(audit.prior_approach, APPROACH);
        assert_eq!(audit.replacement_approach, REPLACEMENT);
        assert_eq!(audit.prior_approach_sha256, digest(APPROACH.as_bytes()));
        assert_eq!(
            audit.replacement_approach_sha256,
            digest(REPLACEMENT.as_bytes())
        );
        assert_eq!(
            audit.resolution,
            "Adopt the reviewed recovery architecture."
        );
        assert_eq!(
            audit.resolution_sha256,
            digest("Adopt the reviewed recovery architecture.".as_bytes())
        );
        assert_eq!(
            stored.attempts[0].writer_completion_contract,
            WriterCompletionContract::from_planning_context(context)
        );
        assert_eq!(
            stored.attempts[0].tasks.last().unwrap().role,
            CORRECTIVE_WRITER_ROLE
        );
        assert_eq!(stored.attempts[0].recovery_operations.len(), 1);
        assert_eq!(
            stored.attempts[0].recovery_operations[0].operation_kind,
            "planning-approach-revision"
        );
    }

    #[test]
    fn revision_binds_the_source_writer_matching_the_uncertain_review() {
        use crate::work_model::AttemptReviewState;

        let (tmp, store) = fixture();
        let root = tmp.path();
        let source = store.read_work_item("work-1").unwrap().attempts[0]
            .tasks
            .iter()
            .find(|task| task.kind == TaskKind::Write)
            .unwrap()
            .clone();
        let contract = store.read_work_item("work-1").unwrap().attempts[0]
            .writer_completion_contract
            .clone()
            .unwrap();
        let matrix_path = write_source_matrix(root, &source, &contract);
        let matrix_bytes = fs::read(&matrix_path).unwrap();

        // A later no-change Writer preserves the reviewed candidate but has no
        // completion matrix of its own. The revision must still bind source
        // completion to the Writer the settled review examined.
        store
            .mutate_work_item("work-1", |item| {
                let output = item.attempts[0].tasks[0].output.clone().unwrap();
                item.add_next_write_round("attempt-1", Vec::new())?;
                let writer = item.attempts[0].tasks.last_mut().unwrap();
                writer.status = TaskStatus::Complete;
                writer.output = Some(TaskOutput {
                    commit: output.commit.clone(),
                    ..output
                });
                item.attempts[0].status = AttemptStatus::NeedsUser;
                item.attempts[0].pause_kind = Some(PauseKind::Uncertain);
                item.attempts[0].review_state = Some(AttemptReviewState::Uncertain);
                Ok(())
            })
            .unwrap();

        let revision = revise(
            root,
            &store,
            "work-1",
            "attempt-1",
            REPLACEMENT.as_bytes(),
            "Approve recovery.",
        )
        .unwrap();
        let audit: RevisionAudit =
            serde_json::from_slice(&fs::read(root.join(&revision.audit_artifact)).unwrap())
                .unwrap();
        let source_completion = audit
            .source_completion
            .expect("the reviewed candidate has source completion evidence");
        assert_eq!(source_completion.artifact.producer_id, source.id);
        assert_eq!(source_completion.sha256, digest(&matrix_bytes));
    }

    #[test]
    fn corrective_writer_commit_preserves_revision_resume() {
        use crate::work_model::AttemptReviewState;

        let (tmp, store) = fixture();
        let root = tmp.path();
        let source = store.read_work_item("work-1").unwrap().attempts[0]
            .tasks
            .iter()
            .find(|task| task.kind == TaskKind::Write)
            .unwrap()
            .clone();
        let contract = store.read_work_item("work-1").unwrap().attempts[0]
            .writer_completion_contract
            .clone()
            .unwrap();
        write_source_matrix(root, &source, &contract);

        let first = revise(
            root,
            &store,
            "work-1",
            "attempt-1",
            REPLACEMENT.as_bytes(),
            "Approve recovery.",
        )
        .unwrap();

        // The corrective Writer commits a new candidate before the next resume.
        store
            .mutate_work_item("work-1", |item| {
                let attempt = &mut item.attempts[0];
                let mut output = attempt.tasks[0].output.clone().unwrap();
                output.commit = "corrected-candidate".to_string();
                let writer = attempt
                    .tasks
                    .iter_mut()
                    .find(|task| task.role == CORRECTIVE_WRITER_ROLE)
                    .unwrap();
                writer.status = TaskStatus::Complete;
                writer.output = Some(output);
                attempt.status = AttemptStatus::Executing;
                Ok(())
            })
            .unwrap();

        // A new uncertain Architecture review settles on the corrected candidate.
        store
            .mutate_work_item("work-1", |item| {
                let review_ids =
                    item.add_next_review_tasks("attempt-1", &["architecture", "documentation"])?;
                for task in item.attempts[0]
                    .tasks
                    .iter_mut()
                    .filter(|task| review_ids.contains(&task.id))
                {
                    task.status = TaskStatus::Complete;
                    let path = root
                        .join(&task.artifact_area.as_ref().unwrap().path)
                        .join("review.md");
                    fs::create_dir_all(path.parent().unwrap()).unwrap();
                    let body = if task.role == "architecture" {
                        "Verdict: uncertain\n"
                    } else {
                        "Verdict: pass\n"
                    };
                    fs::write(path, body).unwrap();
                }
                item.attempts[0].status = AttemptStatus::NeedsUser;
                item.attempts[0].pause_kind = Some(PauseKind::Uncertain);
                item.attempts[0].review_state = Some(AttemptReviewState::Uncertain);
                Ok(())
            })
            .unwrap();

        let resumed = store.read_work_item("work-1").unwrap();
        crate::recovery_contracts::validate_planning_resume(root, &resumed, &resumed.attempts[0])
            .unwrap();

        let again = revise(
            root,
            &store,
            "work-1",
            "attempt-1",
            REPLACEMENT.as_bytes(),
            "Approve recovery.",
        )
        .unwrap();
        assert!(again.already_applied);
        assert_eq!(again.writer_task_id, first.writer_task_id);
    }

    #[test]
    fn second_revision_after_no_change_corrective_writer_binds_the_current_contract() {
        use crate::work_model::AttemptReviewState;

        let (tmp, store) = fixture();
        let root = tmp.path();
        let source = store.read_work_item("work-1").unwrap().attempts[0]
            .tasks
            .iter()
            .find(|task| task.kind == TaskKind::Write)
            .unwrap()
            .clone();
        let old_contract = store.read_work_item("work-1").unwrap().attempts[0]
            .writer_completion_contract
            .clone()
            .unwrap();
        write_source_matrix(root, &source, &old_contract);

        // The first revision binds the source Writer and moves the Attempt to
        // the revised contract.
        let first = revise(
            root,
            &store,
            "work-1",
            "attempt-1",
            REPLACEMENT.as_bytes(),
            "Approve recovery.",
        )
        .unwrap();

        // A no-change corrective Writer preserves the reviewed candidate and
        // writes the only matrix built against the revised contract.
        let revised_contract = store.read_work_item("work-1").unwrap().attempts[0]
            .writer_completion_contract
            .clone()
            .unwrap();
        let corrective = store.read_work_item("work-1").unwrap().attempts[0]
            .tasks
            .iter()
            .find(|task| task.id == first.writer_task_id)
            .unwrap()
            .clone();
        write_source_matrix(root, &corrective, &revised_contract);
        store
            .mutate_work_item("work-1", |item| {
                let output = item.attempts[0].tasks[0].output.clone().unwrap();
                let writer = item.attempts[0]
                    .tasks
                    .iter_mut()
                    .find(|task| task.id == first.writer_task_id)
                    .unwrap();
                writer.status = TaskStatus::Complete;
                writer.output = Some(TaskOutput {
                    no_change: None,
                    ..output
                });
                item.attempts[0].status = AttemptStatus::NeedsUser;
                item.attempts[0].pause_kind = Some(PauseKind::Uncertain);
                item.attempts[0].review_state = Some(AttemptReviewState::Uncertain);
                Ok(())
            })
            .unwrap();

        // A second, different revision must bind the newest Writer whose matrix
        // validates against the current contract, not the original Writer.
        let second = revise(
            root,
            &store,
            "work-1",
            "attempt-1",
            SECOND_REPLACEMENT.as_bytes(),
            "Approve the second revision.",
        )
        .unwrap();
        let audit: RevisionAudit =
            serde_json::from_slice(&fs::read(root.join(&second.audit_artifact)).unwrap()).unwrap();
        let source_completion = audit
            .source_completion
            .expect("the second revision must bind a source completion");
        assert_eq!(
            source_completion.artifact.producer_id, first.writer_task_id,
            "the newest Writer whose matrix validates against the current contract must be the source"
        );

        // Repeating the same revision reuses the unchanged audit instead of
        // recomputing it against a later Writer.
        let repeated = revise(
            root,
            &store,
            "work-1",
            "attempt-1",
            SECOND_REPLACEMENT.as_bytes(),
            "Approve the second revision.",
        )
        .unwrap();
        assert!(repeated.already_applied);
        assert_eq!(repeated.audit_artifact, second.audit_artifact);
        assert_eq!(repeated.writer_task_id, second.writer_task_id);
    }

    #[test]
    fn stale_uncertain_review_refuses_revision_with_a_named_reason() {
        use crate::work_model::AttemptReviewState;

        let (tmp, store) = fixture();
        let root = tmp.path();
        let source_review_id = store.read_work_item("work-1").unwrap().attempts[0]
            .tasks
            .iter()
            .find(|task| task.kind == TaskKind::Review && task.role == "architecture")
            .unwrap()
            .id
            .clone();

        // The corrective Writer advances the Attempt's candidate without a
        // settled Architecture review of that candidate.
        store
            .mutate_work_item("work-1", |item| {
                let output = item.attempts[0].tasks[0].output.clone().unwrap();
                item.add_next_write_round("attempt-1", Vec::new())?;
                let writer = item.attempts[0].tasks.last_mut().unwrap();
                writer.status = TaskStatus::Complete;
                writer.output = Some(TaskOutput {
                    commit: "corrected-candidate".to_string(),
                    ..output
                });
                item.attempts[0].status = AttemptStatus::NeedsUser;
                item.attempts[0].pause_kind = Some(PauseKind::Uncertain);
                item.attempts[0].review_state = Some(AttemptReviewState::Uncertain);
                Ok(())
            })
            .unwrap();

        let before = serde_json::to_vec(&store.read_work_item("work-1").unwrap()).unwrap();
        let error = revise(
            root,
            &store,
            "work-1",
            "attempt-1",
            REPLACEMENT.as_bytes(),
            "Approve recovery.",
        )
        .unwrap_err();
        let message = format!("{error:#}");
        assert!(message.contains(&source_review_id), "{message}");
        assert!(
            message.contains("not bound to the current candidate"),
            "{message}"
        );
        assert!(
            !message.contains("no completed Writer candidate"),
            "{message}"
        );
        assert_eq!(
            serde_json::to_vec(&store.read_work_item("work-1").unwrap()).unwrap(),
            before
        );
    }

    #[test]
    fn recovers_legacy_generic_handoff_with_codex_parser_diagnostics() {
        let (tmp, store) = fixture();
        store
            .mutate_work_item("work-1", |item| {
                let writer = item.attempts[0].tasks[0].clone();
                for round in 2..=crate::work_attempt_loop::max_total_write_rounds() {
                    let mut historical_writer = writer.clone();
                    historical_writer.id = format!("attempt-1-write-{round}");
                    historical_writer.artifact_area = Some(crate::work_model::TaskArtifactArea {
                        path: work_artifact_path(
                            "work-1",
                            "attempt-1",
                            &historical_writer.id,
                        ),
                    });
                    item.attempts[0].tasks.insert(1, historical_writer);
                }
                let source_index = item.attempts[0]
                    .tasks
                    .iter()
                    .position(|task| task.kind == TaskKind::Review && task.role == "architecture")
                    .unwrap();
                let documentation_index = item.attempts[0]
                    .tasks
                    .iter()
                    .position(|task| task.kind == TaskKind::Review && task.role == "documentation")
                    .unwrap();
                item.attempts[0].tasks[source_index].id =
                    "attempt-1-review-38-architecture".to_string();
                item.attempts[0].tasks[documentation_index].id =
                    "attempt-1-review-38-documentation".to_string();
                let failed = item.attempts[0].tasks[source_index].clone();
                let mut failed = failed;
                failed.id = "attempt-1-review-39-architecture".to_string();
                failed.artifact_area.as_mut().unwrap().path =
                    work_artifact_path("work-1", "attempt-1", "attempt-1-review-39-architecture");
                failed.status = TaskStatus::Failed;
                failed.output = None;
                failed.review_context = item.attempts[0].tasks[source_index].review_context.clone();
                item.attempts[0].tasks.push(failed);
                let handoff = work_artifact_path(
                    "work-1",
                    "attempt-1",
                    "needs-user-attempt-1-review-39-architecture.md",
                );
                fs::write(
                    tmp.path().join(&handoff),
                    "# Attempt needs user input\n\nThe reviewer failed before producing output.\n",
                )
                .unwrap();
                item.attempts[0].artifacts.push(ArtifactRef {
                    producer_id: "attempt-1-review-39-architecture".to_string(),
                    path: handoff,
                });
                item.attempts[0].status = AttemptStatus::Failed;
                let area = item.attempts[0].tasks.last().unwrap().artifact_area.as_ref().unwrap();
                let directory = tmp.path().join(&area.path);
                fs::create_dir_all(&directory).unwrap();
                fs::write(
                    directory.join(crate::coder::PROVIDER_STDERR_FILENAME),
                    "error: unexpected argument '--replacement-approach' found\n\nUsage: codex exec [OPTIONS] [PROMPT]\n",
                )
                .unwrap();
                fs::write(
                    directory.join("execution.json"),
                    r#"{"schema_version":1,"state":"settled","owner_pid":1,"leader_pid":1,"process_group_id":1,"started_at":"now","heartbeat_at":"now","settled_at":"now"}"#,
                )
                .unwrap();
                Ok(())
            })
            .unwrap();
        let before = store.read_work_item("work-1").unwrap();
        let candidate = before.attempts[0].tasks[0].output.clone();
        let prior_reviews: Vec<_> = before.attempts[0]
            .tasks
            .iter()
            .filter(|task| task.kind == TaskKind::Review && task.status == TaskStatus::Complete)
            .map(|task| task.id.clone())
            .collect();
        let prior_artifacts = before.attempts[0].artifacts.clone();
        let revision = revise(
            tmp.path(),
            &store,
            "work-1",
            "attempt-1",
            REPLACEMENT.as_bytes(),
            "Recover the failed launch.",
        )
        .unwrap();
        let audit: RevisionAudit =
            serde_json::from_slice(&fs::read(tmp.path().join(revision.audit_artifact)).unwrap())
                .unwrap();
        let receipt = audit
            .legacy_reconciliation
            .expect("legacy evidence receipt");
        assert_eq!(receipt.candidate_commit, "candidate");
        assert_eq!(
            receipt.failed_review_task_id,
            "attempt-1-review-39-architecture"
        );
        assert!(receipt.diagnostic_excerpt.contains("unexpected argument"));
        let attempt = &store.read_work_item("work-1").unwrap().attempts[0];
        assert_eq!(
            attempt
                .tasks
                .iter()
                .find(|task| task.id == "attempt-1-review-39-architecture")
                .unwrap()
                .status,
            TaskStatus::Failed
        );
        assert_eq!(attempt.status, AttemptStatus::Planned);
        assert_eq!(attempt.round_cap_extensions.len(), 1);
        assert_eq!(attempt.round_cap_extensions[0].additional_write_rounds, 1);
        assert_eq!(
            attempt.round_cap_extensions[0].new_cap,
            crate::work_attempt_loop::max_total_write_rounds() + 1
        );
        assert_eq!(attempt.tasks[0].output, candidate);
        assert!(prior_reviews.iter().all(|id| {
            attempt
                .tasks
                .iter()
                .any(|task| &task.id == id && task.status == TaskStatus::Complete)
        }));
        assert!(
            prior_artifacts
                .iter()
                .all(|artifact| attempt.artifacts.contains(artifact))
        );
        let corrective = attempt.tasks.last().unwrap();
        assert_eq!(corrective.role, CORRECTIVE_WRITER_ROLE);
        assert!(
            corrective
                .input_artifacts
                .iter()
                .any(|artifact| artifact.producer_id == PRODUCER)
        );
        let mut completed = store.read_work_item("work-1").unwrap();
        let recovered = &mut completed.attempts[0];
        let writer = recovered.tasks.last_mut().unwrap();
        writer.status = TaskStatus::Complete;
        writer.output = candidate;
        recovered.status = AttemptStatus::Complete;
        assert!(recovered.validate("work-1").is_ok());
    }

    #[test]
    fn recovers_typed_provider_argument_handoff_alongside_legacy_adapter() {
        let (tmp, store) = typed_terminal_fixture();
        let revision = revise(
            tmp.path(),
            &store,
            "work-1",
            "attempt-1",
            REPLACEMENT.as_bytes(),
            "Recover the typed provider failure.",
        )
        .unwrap();

        let audit: RevisionAudit =
            serde_json::from_slice(&fs::read(tmp.path().join(revision.audit_artifact)).unwrap())
                .unwrap();
        assert!(audit.legacy_reconciliation.is_none());
        assert_eq!(
            store.read_work_item("work-1").unwrap().attempts[0].status,
            AttemptStatus::Planned
        );
    }

    #[test]
    fn schedules_documentation_correction_and_targeted_reviews() {
        let (tmp, store) = fixture();
        revise(
            tmp.path(),
            &store,
            "work-1",
            "attempt-1",
            REPLACEMENT.as_bytes(),
            "Resolve Architecture and Documentation findings.",
        )
        .unwrap();
        let attempt = &store.read_work_item("work-1").unwrap().attempts[0];
        let writer = attempt.tasks.last().unwrap();
        let settled_reviews: Vec<_> = attempt
            .tasks
            .iter()
            .filter(|task| {
                task.kind == TaskKind::Review
                    && task.status == TaskStatus::Complete
                    && matches!(task.role.as_str(), "architecture" | "documentation")
            })
            .map(review_artifact)
            .collect::<Result<_>>()
            .unwrap();
        assert!(
            writer
                .input_artifacts
                .iter()
                .any(|artifact| artifact.producer_id == PRODUCER)
        );
        assert!(
            settled_reviews
                .iter()
                .all(|artifact| writer.input_artifacts.contains(artifact)),
            "expected {settled_reviews:?} in {:?}",
            writer.input_artifacts
        );
        assert!(
            writer
                .instructions
                .as_deref()
                .unwrap()
                .contains("every actionable finding")
        );
        store
            .mutate_work_item("work-1", |item| {
                let output = item.attempts[0].tasks[0].output.clone().unwrap();
                let corrective = item.attempts[0].tasks.last_mut().unwrap();
                corrective.status = TaskStatus::Complete;
                corrective.output = Some(TaskOutput {
                    base_commit: None,
                    ..output
                });
                item.attempts[0].writer_completion_contract = None;
                item.attempts[0].progress_contract = None;
                Ok(())
            })
            .unwrap();
        let outcome = crate::work_attempt_loop::plan_after_completed_writer(
            tmp.path(),
            &store,
            store.read_work_item("work-1").unwrap(),
            "attempt-1",
        )
        .unwrap();
        let crate::work_attempt_loop::WorkAttemptRunOutcome::PlannedReviews { task_ids } = outcome
        else {
            panic!("the completed corrective Writer must re-enter targeted reviews: {outcome:?}");
        };
        let attempt = &store.read_work_item("work-1").unwrap().attempts[0];
        let mut roles = task_ids
            .iter()
            .map(|id| {
                attempt
                    .tasks
                    .iter()
                    .find(|task| &task.id == id)
                    .unwrap()
                    .role
                    .as_str()
            })
            .collect::<Vec<_>>();
        roles.sort_unstable();
        assert_eq!(
            roles,
            vec![
                "architecture",
                "behaviors",
                "documentation",
                "skills",
                "tests"
            ],
            "recovery must also establish the missing ordinary domains"
        );
    }

    #[test]
    fn repeated_revision_is_idempotent() {
        let (tmp, store) = fixture();
        let first = revise(
            tmp.path(),
            &store,
            "work-1",
            "attempt-1",
            REPLACEMENT.as_bytes(),
            "Approve recovery.",
        )
        .unwrap();
        let after_first = store.read_work_item("work-1").unwrap();
        let audit_count = after_first.attempts[0]
            .artifacts
            .iter()
            .filter(|artifact| artifact.producer_id == PRODUCER)
            .count();
        let task_count = after_first.attempts[0].tasks.len();
        let review_count = after_first.attempts[0]
            .tasks
            .iter()
            .filter(|task| task.kind == TaskKind::Review)
            .count();
        let second = revise(
            tmp.path(),
            &store,
            "work-1",
            "attempt-1",
            REPLACEMENT.as_bytes(),
            "Approve recovery.",
        )
        .unwrap();
        assert!(second.already_applied);
        assert_eq!(first.writer_task_id, second.writer_task_id);
        let after_second = store.read_work_item("work-1").unwrap();
        assert_eq!(
            after_second.attempts[0]
                .artifacts
                .iter()
                .filter(|artifact| artifact.producer_id == PRODUCER)
                .count(),
            audit_count
        );
        assert_eq!(after_second.attempts[0].tasks.len(), task_count);
        assert_eq!(
            after_second.attempts[0]
                .tasks
                .iter()
                .filter(|task| task.kind == TaskKind::Review)
                .count(),
            review_count
        );
    }

    #[test]
    fn rejects_unrelated_legacy_launch_failures_without_mutation() {
        let (tmp, store) = fixture();
        store
            .mutate_work_item("work-1", |item| {
                item.attempts[0].pause_kind = Some(PauseKind::RoundCap);
                Ok(())
            })
            .unwrap();
        assert_rejected_without_mutation(&tmp, &store);

        for mutate in [
            |tmp: &tempfile::TempDir, store: &WorkModelStore| {
                let handoff = work_artifact_path(
                    "work-1",
                    "attempt-1",
                    "needs-user-attempt-1-review-39-architecture.md",
                );
                fs::write(
                    tmp.path().join(handoff),
                    "# Provider arguments need repair\n\npass-through provider arguments\n",
                )
                .unwrap();
                assert_rejected_without_mutation(tmp, store);
            },
            |tmp: &tempfile::TempDir, store: &WorkModelStore| {
                let path = tmp.path().join(work_artifact_path(
                    "work-1",
                    "attempt-1",
                    "attempt-1-review-39-architecture/review.md",
                ));
                fs::write(path, "Verdict: findings\n").unwrap();
                assert_rejected_without_mutation(tmp, store);
            },
            |tmp: &tempfile::TempDir, store: &WorkModelStore| {
                let failed = store.read_work_item("work-1").unwrap().attempts[0]
                    .tasks
                    .last()
                    .unwrap()
                    .clone();
                let directory = tmp.path().join(failed.artifact_area.unwrap().path);
                fs::write(
                    directory.join(crate::coder::PROVIDER_STDERR_FILENAME),
                    "ordinary error mentioning argument\n",
                )
                .unwrap();
                assert_rejected_without_mutation(tmp, store);
            },
            |tmp: &tempfile::TempDir, store: &WorkModelStore| {
                let failed = store.read_work_item("work-1").unwrap().attempts[0]
                    .tasks
                    .last()
                    .unwrap()
                    .clone();
                let directory = tmp.path().join(failed.artifact_area.unwrap().path);
                fs::write(directory.join("execution.json"), r#"{"schema_version":1,"state":"running","owner_pid":1,"leader_pid":1,"process_group_id":1,"started_at":"now","heartbeat_at":"now"}"#).unwrap();
                assert_rejected_without_mutation(tmp, store);
            },
            |_: &tempfile::TempDir, store: &WorkModelStore| {
                store
                    .mutate_work_item("work-1", |item| {
                        item.attempts[0]
                            .tasks
                            .last_mut()
                            .unwrap()
                            .review_context
                            .as_mut()
                            .unwrap()
                            .candidate_commit = "other-candidate".to_string();
                        Ok(())
                    })
                    .unwrap();
                // The candidate mismatch must reject before changing durable state.
            },
            |_: &tempfile::TempDir, store: &WorkModelStore| {
                store
                    .mutate_work_item("work-1", |item| {
                        item.attempts[0].tasks.last_mut().unwrap().kind = TaskKind::Tester;
                        Ok(())
                    })
                    .unwrap();
            },
            |_: &tempfile::TempDir, store: &WorkModelStore| {
                store
                    .mutate_work_item("work-1", |item| {
                        item.attempts[0].tasks.last_mut().unwrap().id =
                            "attempt-1-review-40-architecture".to_string();
                        Ok(())
                    })
                    .unwrap();
            },
        ] {
            let (tmp, store) = legacy_terminal_fixture();
            mutate(&tmp, &store);
            assert_rejected_without_mutation(&tmp, &store);
        }

        let (tmp, store) = legacy_terminal_fixture();
        let before = serde_json::to_vec(&store.read_work_item("work-1").unwrap()).unwrap();
        let mut attempt = store.read_work_item("work-1").unwrap().attempts.remove(0);
        let output = attempt.tasks[0].output.clone().unwrap();
        attempt.tasks.last_mut().unwrap().output = Some(output);
        let failed = attempt.tasks.last().unwrap();
        assert!(legacy_reconciliation_receipt(tmp.path(), &attempt, failed).is_err());
        assert_eq!(
            serde_json::to_vec(&store.read_work_item("work-1").unwrap()).unwrap(),
            before
        );
        assert!(
            !tmp.path()
                .join(work_artifact_path(
                    "work-1",
                    "attempt-1",
                    "planning-approach-revisions"
                ))
                .exists()
        );

        let (tmp, store) = fixture();
        store
            .mutate_work_item("work-1", |item| {
                let mut failed = item.attempts[0].tasks[1].clone();
                failed.id = "attempt-1-review-2-documentation".to_string();
                failed.status = TaskStatus::Failed;
                failed.output = None;
                item.attempts[0].tasks.push(failed);
                item.attempts[0].status = AttemptStatus::Failed;
                Ok(())
            })
            .unwrap();
        assert_rejected_without_mutation(&tmp, &store);

        let (tmp, store) = fixture();
        store
            .mutate_work_item("work-1", |item| {
                item.attempts[0].tasks[0].output.as_mut().unwrap().commit = "stale".to_string();
                Ok(())
            })
            .unwrap();
        assert_rejected_without_mutation(&tmp, &store);

        let (tmp, store) = fixture();
        let review = tmp.path().join(work_artifact_path(
            "work-1",
            "attempt-1",
            "attempt-1-review-architecture/review.md",
        ));
        fs::write(review, "Verdict: findings\n").unwrap();
        assert_rejected_without_mutation(&tmp, &store);

        let (tmp, store) = fixture();
        revise(
            tmp.path(),
            &store,
            "work-1",
            "attempt-1",
            REPLACEMENT.as_bytes(),
            "Approve recovery.",
        )
        .unwrap();
        assert_rejected_state_unchanged(&tmp, &store, b"# A different replacement\n");

        let (tmp, store) = fixture();
        store
            .mutate_work_item("work-1", |item| {
                let mut failed = item.attempts[0].tasks[1].clone();
                failed.id = "attempt-1-review-2-architecture".to_string();
                failed.kind = TaskKind::Tester;
                failed.status = TaskStatus::Failed;
                failed.output = None;
                item.attempts[0].tasks.push(failed);
                item.attempts[0].status = AttemptStatus::Failed;
                Ok(())
            })
            .unwrap();
        assert_rejected_without_mutation(&tmp, &store);
    }
}
