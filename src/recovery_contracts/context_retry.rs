//! Validate one completion retry after audited, output-free context recovery.
use super::*;
use anyhow::ensure;
use serde::{Deserialize, Serialize};
use work_model::{
    Attempt, AttemptStatus, PauseKind, RecoveryOperationRequest, TaskArtifactArea, WorkItem,
};

pub(crate) const APPROVE: &str = "writer-completion-context-retry";
pub(crate) const RESUME: &str = "writer-completion-context-resume";
pub(crate) const START: &str = "writer-completion-context-start";
pub(crate) const COMPLETION_APPROVE: &str = "writer-completion-no-change-retry";
pub(crate) const COMPLETION_RESUME: &str = "writer-completion-no-change-resume";
pub(crate) const COMPLETION_START: &str = "writer-completion-no-change-start";

fn host_write_baseline_path(
    store: &work_model::WorkModelStore,
    work_item_id: &str,
    attempt_id: &str,
    task_id: &str,
) -> Result<std::path::PathBuf> {
    Ok(store
        .work_task_path(work_item_id, attempt_id, task_id)?
        .with_extension("write-baseline"))
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum RetrySource {
    #[default]
    RepairedContext,
    InvalidCompletion,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Retry {
    pub schema_version: u32,
    #[serde(default, skip_serializing_if = "is_context_source")]
    pub source: RetrySource,
    pub before: Attempt,
    pub task_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<ArtifactRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline_sha256: Option<String>,
    pub files: BTreeMap<String, String>,
    pub resolution: String,
}

fn is_context_source(source: &RetrySource) -> bool {
    *source == RetrySource::RepairedContext
}

pub(crate) fn approve_kind(retry: &Retry) -> &'static str {
    match retry.source {
        RetrySource::RepairedContext => APPROVE,
        RetrySource::InvalidCompletion => COMPLETION_APPROVE,
    }
}

fn kind_for(retry: &Retry, kind: &str) -> &'static str {
    match (retry.source, kind) {
        (RetrySource::RepairedContext, "resume") => RESUME,
        (RetrySource::RepairedContext, "start") => START,
        (RetrySource::InvalidCompletion, "resume") => COMPLETION_RESUME,
        (RetrySource::InvalidCompletion, "start") => COMPLETION_START,
        _ => approve_kind(retry),
    }
}

fn has_retry_history(attempt: &Attempt) -> bool {
    attempt
        .recovery_operations
        .iter()
        .any(|r| matches!(r.operation_kind.as_str(), APPROVE | COMPLETION_APPROVE))
        || attempt.tasks.iter().any(|t| {
            t.kind == TaskKind::Write
                && t.status == TaskStatus::Failed
                && t.output.is_none()
                && t.input_artifacts.iter().any(|a| {
                    a.producer_id == "writer-completion-recovery"
                        && a.path
                            == work_model::work_artifact_path(
                                &attempt.work_item_id,
                                &attempt.id,
                                &format!("writer-completion-recoveries/{}/recovery.json", t.id),
                            )
                })
        })
}

fn completion_failure_shape(attempt: &Attempt) -> bool {
    matches!(
        attempt.pause_kind,
        Some(PauseKind::WriterCompletion | PauseKind::RoundCap)
    ) && attempt.status == AttemptStatus::NeedsUser
        && attempt.tasks.iter().any(|task| {
            task.kind == TaskKind::Write
                && task.output.is_none()
                && matches!(task.status, TaskStatus::NeedsUser | TaskStatus::Failed)
        })
}

pub(crate) fn recognized(root: &Path, item: &WorkItem, attempt: &Attempt) -> Result<bool> {
    if !has_retry_history(attempt) && !completion_failure_shape(attempt) {
        return Ok(false);
    }
    if !has_retry_history(attempt) {
        let Some(task) = attempt.tasks.iter().rev().find(|task| {
            task.kind == TaskKind::Write
                && task.status == TaskStatus::NeedsUser
                && task.output.is_none()
        }) else {
            return Ok(false);
        };
        let Some(area) = &task.artifact_area else {
            return Ok(false);
        };
        // A plain Writer pause has no completion matrix to repair.
        let matrix = match fs::read(
            root.join(&area.path)
                .join(crate::recovery_contracts::WRITER_COMPLETION_MATRIX_FILE),
        ) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error.into()),
        };
        let task_index = attempt
            .tasks
            .iter()
            .position(|candidate| candidate.id == task.id)
            .unwrap();
        if attempt.tasks[task_index + 1..]
            .iter()
            .any(|later| later.kind == TaskKind::Write && later.status == TaskStatus::Planned)
        {
            return Ok(false);
        }
        let invalid_matrix =
            serde_json::from_slice::<work_model::WriterCompletionMatrix>(&matrix).is_err();
        let invalid_no_change = fs::read(root.join(&area.path).join("no-change.json"))
            .ok()
            .is_some_and(|bytes| {
                serde_json::from_slice::<work_model::NoChangeOutput>(&bytes).is_err()
            });
        if !invalid_matrix && !invalid_no_change {
            return Ok(false);
        }
    }
    if attempt
        .recovery_operations
        .iter()
        .any(|r| matches!(r.operation_kind.as_str(), APPROVE | COMPLETION_APPROVE))
    {
        // Recorded context authority must still reject stale or consumed retries.
        return Ok(true);
    }
    // The original completion approval can precede a launched quota failure.
    // Let the quota owner proceed only after it validates its separate approval.
    Ok(!crate::usage_limit_contracts::resume_ready(
        root,
        item,
        &attempt.id,
    )?)
}

pub(crate) fn inspect(
    root: &Path,
    item: &WorkItem,
    attempt_id: &str,
    resolution: &str,
) -> Result<Retry> {
    item.ensure_mutation_compatible()?;
    let attempt = item
        .attempts
        .iter()
        .find(|a| a.id == attempt_id)
        .context("Attempt missing")?;
    if completion_failure_shape(attempt) {
        let task = attempt
            .tasks
            .iter()
            .rev()
            .find(|task| {
                task.kind == TaskKind::Write
                    && task.output.is_none()
                    && matches!(task.status, TaskStatus::NeedsUser | TaskStatus::Failed)
            })
            .context("completion failure has no unsettled Writer")?;
        if task.status == TaskStatus::NeedsUser
            || (task
                .input_artifacts
                .iter()
                .any(|artifact| artifact.producer_id == "writer-completion-recovery")
                && !attempt
                    .artifacts
                    .iter()
                    .any(|artifact| artifact.producer_id == "assignment-context-recovery"))
        {
            return inspect_invalid_completion(root, item, attempt, task, resolution);
        }
    }
    ensure!(
        attempt.status == AttemptStatus::NeedsUser
            && matches!(
                attempt.pause_kind,
                Some(PauseKind::RoundCap | PauseKind::WriterCompletion)
            )
            && !attempt.cancellation_requested()
            && !attempt
                .recovery_operations
                .iter()
                .any(|r| r.operation_kind == APPROVE),
        "context retry requires fresh authorization for the unchanged paused failure"
    );
    let task = attempt
        .tasks
        .iter()
        .find(|t| t.kind == TaskKind::Write && t.status == TaskStatus::Failed && t.output.is_none())
        .context("context retry has no output-free failed Writer")?;
    let assignment = task
        .writer_correction_assignment
        .as_ref()
        .context("failed Writer assignment missing")?;
    assignment_context_recovery_target(
        root,
        item,
        attempt_id,
        &task.id,
        &assignment.source_candidate_commit,
    )?;
    validate_writer_correction_assignment(root, item, task, assignment)?;
    let mut recovered = None;
    for reference in attempt
        .artifacts
        .iter()
        .filter(|a| a.producer_id == "assignment-context-recovery")
    {
        let bytes = assignment_input_bytes(root, &reference.path)?;
        let audit: work_model::AssignmentContextRecoveryAudit = serde_json::from_slice(&bytes)?;
        if audit.after == *assignment && audit.output_free.is_some() {
            validate_output_free_context_files(root, &audit)?;
            ensure!(
                recovered.is_none(),
                "ambiguous output-free context recovery"
            );
            recovered = Some(reference.clone());
        }
    }
    let context = recovered
        .context("repair the bound stale assignment context before authorizing its retry")?;
    let mut files = output_free_context_binding(root, item, attempt, task)?
        .unwrap()
        .files;
    crate::usage_limit_contracts::bind_file(root, &context.path, &mut files)?;
    for input in assignment
        .source_task_inputs
        .iter()
        .chain(&assignment.evidence)
    {
        crate::usage_limit_contracts::bind_file(root, &input.artifact.path, &mut files)?;
    }
    Ok(Retry {
        schema_version: 1,
        source: RetrySource::RepairedContext,
        before: attempt.clone(),
        task_id: task.id.clone(),
        context: Some(context),
        baseline_sha256: None,
        files,
        resolution: resolution.into(),
    })
}

fn inspect_invalid_completion(
    root: &Path,
    item: &WorkItem,
    attempt: &Attempt,
    task: &Task,
    resolution: &str,
) -> Result<Retry> {
    ensure!(
        attempt.status == AttemptStatus::NeedsUser
            && matches!(
                attempt.pause_kind,
                Some(PauseKind::WriterCompletion | PauseKind::RoundCap)
            )
            && !attempt.cancellation_requested()
            && !attempt
                .recovery_operations
                .iter()
                .any(|operation| operation.operation_kind == COMPLETION_APPROVE),
        "completion retry requires a settled, newly authorized pause"
    );
    ensure!(
        attempt
            .tasks
            .iter()
            .rev()
            .find(|candidate| candidate.kind == TaskKind::Write)
            .is_some_and(|candidate| candidate.id == task.id),
        "completion retry requires the latest Writer Task"
    );
    let index = attempt
        .tasks
        .iter()
        .position(|candidate| candidate.id == task.id)
        .unwrap();
    let previous = attempt.tasks[..index]
        .iter()
        .rev()
        .find(|candidate| {
            candidate.kind == TaskKind::Write
                && candidate.status == TaskStatus::Complete
                && candidate.output.is_some()
        })
        .context("completion retry has no completed candidate")?;
    let output = previous.output.as_ref().unwrap();
    ensure!(
        attempt.current_candidate_commit() == Some(output.commit.as_str())
            && candidate_workspace_is_clean_at_managed(root, output, &output.commit),
        "completion retry candidate changed or is dirty"
    );
    let baseline_path = host_write_baseline_path(
        &work_model::WorkModelStore::new(root),
        &item.id,
        &attempt.id,
        &task.id,
    )?;
    let host_baseline = fs::read_to_string(&baseline_path)
        .context("completion retry requires the host-owned Writer baseline")?;
    ensure!(
        host_baseline.trim() == output.commit,
        "completion retry host baseline differs from the current candidate"
    );
    let area = task
        .artifact_area
        .as_ref()
        .context("completion retry Task has no artifact area")?;
    let sidecar = fs::read_to_string(root.join(&area.path).join("write-baseline-commit"))
        .context("completion retry requires the original Writer baseline")?;
    ensure!(
        sidecar.trim() == output.commit,
        "completion retry Writer baseline changed"
    );
    let matrix = fs::read(
        root.join(&area.path)
            .join(crate::recovery_contracts::WRITER_COMPLETION_MATRIX_FILE),
    )
    .context("completion retry has no retained matrix")?;
    let bad_matrix = serde_json::from_slice::<work_model::WriterCompletionMatrix>(&matrix).is_err();
    let bad_no_change = fs::read(root.join(&area.path).join("no-change.json"))
        .ok()
        .is_some_and(|bytes| serde_json::from_slice::<work_model::NoChangeOutput>(&bytes).is_err());
    ensure!(
        bad_matrix || bad_no_change,
        "completion retry requires an invalid retained handoff"
    );
    if let Some(assignment) = &task.writer_correction_assignment {
        validate_writer_correction_assignment(root, item, task, assignment)?;
        validate_writer_continuation_lineage(root, item, task)?;
    }
    if task.status == TaskStatus::Failed {
        writer_completion::validated_writer_context(root, item, task)?
            .context("failed completion retry lost its original approval")?;
        let execution_path = root.join(&area.path).join("execution.json");
        let record: crate::execution::ExecutionRecord =
            serde_json::from_slice(&fs::read(&execution_path)?)?;
        ensure!(
            record.schema_version == 2
                && record.launch_generation.is_some()
                && record.state == crate::execution::ExecutionState::Launching
                && record.leader_pid == 0
                && crate::execution::receipt_absent(&execution_path)?,
            "failed completion retry has no proof that this generation stopped before provider launch"
        );
    } else {
        ensure!(
            attempt.pause_kind == Some(PauseKind::WriterCompletion),
            "initial completion retry requires a completion pause"
        );
    }
    for other in &attempt.tasks {
        crate::usage_limit_contracts::unlocked(
            &crate::lease::task_lock_path(root, &item.id, &other.id),
            false,
        )?;
        if let Some(area) = &other.artifact_area {
            crate::usage_limit_contracts::unlocked(
                &root.join(&area.path).join("execution-owner.lock"),
                false,
            )?;
            ensure!(
                !crate::execution::execution_is_live(
                    &root.join(&area.path).join("execution.json")
                )?,
                "completion retry has a live execution owner"
            );
        }
    }
    crate::usage_limit_contracts::unlocked(
        &crate::lease::learner_lock_path(root, &item.id, &attempt.id),
        false,
    )?;
    ensure!(
        !item
            .merge_candidates
            .iter()
            .any(|candidate| candidate.attempt_id == attempt.id),
        "completion retry candidate is already finalized"
    );
    let mut files = BTreeMap::new();
    crate::usage_limit_contracts::bind_directory(root, &area.path, &mut files)?;
    if let Some(previous_area) = &previous.artifact_area {
        crate::usage_limit_contracts::bind_directory(root, &previous_area.path, &mut files)?;
    }
    for input in &task.input_artifacts {
        crate::usage_limit_contracts::bind_file(root, &input.path, &mut files)?;
    }
    Ok(Retry {
        schema_version: 2,
        source: RetrySource::InvalidCompletion,
        before: attempt.clone(),
        task_id: task.id.clone(),
        context: None,
        baseline_sha256: Some(format!(
            "sha256:{:x}",
            Sha256::digest(fs::read(baseline_path)?)
        )),
        files,
        resolution: resolution.into(),
    })
}

pub(crate) fn request(retry: &Retry, kind: &str) -> Result<RecoveryOperationRequest> {
    let task = failed_task(retry)?;
    let candidate = match retry.source {
        RetrySource::RepairedContext => task
            .writer_correction_assignment
            .as_ref()
            .context("retry assignment missing")?
            .source_candidate_commit
            .clone(),
        RetrySource::InvalidCompletion => retry.before.tasks[..retry
            .before
            .tasks
            .iter()
            .position(|candidate| candidate.id == task.id)
            .context("completion retry Task missing")?]
            .iter()
            .rev()
            .find_map(|candidate| {
                candidate
                    .output
                    .as_ref()
                    .map(|output| output.commit.clone())
            })
            .context("completion retry candidate missing")?,
    };
    let kind = if matches!(kind, RESUME | COMPLETION_RESUME) {
        kind_for(retry, "resume")
    } else if matches!(kind, START | COMPLETION_START) {
        kind_for(retry, "start")
    } else {
        approve_kind(retry)
    };
    Ok(RecoveryOperationRequest::new(
        kind,
        &retry.before.work_item_id,
        &retry.before.id,
        Some(candidate),
        Some(task.id.clone()),
        None,
        Some(task.workspace_access.writes[0].path.clone()),
        Some(retry.resolution.clone()),
        Some(format!(
            "sha256:{:x}",
            Sha256::digest(serde_json::to_vec(retry)?)
        )),
    ))
}

pub(crate) fn failed_task(retry: &Retry) -> Result<&Task> {
    retry
        .before
        .tasks
        .iter()
        .find(|t| t.id == retry.task_id)
        .context("retry lost its original failed Task")
}

pub(crate) fn audit_ref(retry: &Retry) -> Result<ArtifactRef> {
    let (producer, directory) = match retry.source {
        RetrySource::RepairedContext => (APPROVE, "writer-completion-context-retries"),
        RetrySource::InvalidCompletion => {
            (COMPLETION_APPROVE, "writer-completion-no-change-retries")
        }
    };
    Ok(ArtifactRef {
        producer_id: producer.into(),
        path: work_model::work_artifact_path(
            &retry.before.work_item_id,
            &retry.before.id,
            &format!("{directory}/{}/recovery.json", retry.task_id),
        ),
    })
}

pub(crate) fn prepared_task(retry: &Retry) -> Result<Task> {
    let mut task = failed_task(retry)?.clone();
    task.status = TaskStatus::Planned;
    task.started_at = None;
    task.completed_at = None;
    task.artifact_area = Some(TaskArtifactArea {
        path: work_model::work_artifact_path(
            &retry.before.work_item_id,
            &retry.before.id,
            &format!(
                "writer-completion-retry-runs/{}",
                request(retry, approve_kind(retry))?.operation_id()
            ),
        ),
    });
    task.input_artifacts.push(audit_ref(retry)?);
    Ok(task)
}

pub(crate) fn current(root: &Path, attempt: &Attempt) -> Result<Option<Retry>> {
    let Some(operation) = attempt
        .recovery_operations
        .iter()
        .find(|r| matches!(r.operation_kind.as_str(), APPROVE | COMPLETION_APPROVE))
    else {
        return Ok(None);
    };
    ensure!(
        attempt
            .recovery_operations
            .iter()
            .filter(|r| matches!(r.operation_kind.as_str(), APPROVE | COMPLETION_APPROVE))
            .count()
            == 1,
        "ambiguous completion retry authority"
    );
    let directory = if operation.operation_kind == COMPLETION_APPROVE {
        "writer-completion-no-change-retries"
    } else {
        "writer-completion-context-retries"
    };
    let reference = ArtifactRef {
        producer_id: operation.operation_kind.clone(),
        path: work_model::work_artifact_path(
            &attempt.work_item_id,
            &attempt.id,
            &format!(
                "{directory}/{}/recovery.json",
                operation
                    .task_id
                    .as_deref()
                    .context("retry operation lost its Task")?
            ),
        ),
    };
    ensure!(
        attempt.artifacts.contains(&reference),
        "completion retry lost its audit reference"
    );
    let bytes = assignment_input_bytes(root, &reference.path)?;
    let retry: Retry = serde_json::from_slice(&bytes)?;
    ensure!(
        ((retry.schema_version == 1 && retry.source == RetrySource::RepairedContext)
            || (retry.schema_version == 2 && retry.source == RetrySource::InvalidCompletion))
            && retry.before.id == attempt.id
            && retry.before.work_item_id == attempt.work_item_id
            && operation.matches_request(&request(&retry, approve_kind(&retry))?)
            && reference == audit_ref(&retry)?
            && bytes == serde_json::to_vec_pretty(&retry)?,
        "completion retry audit or operation changed"
    );
    Ok(Some(retry))
}

pub(crate) fn validate_pending(
    root: &Path,
    item: &WorkItem,
    retry: &Retry,
    own_lease: bool,
    starting: bool,
) -> Result<()> {
    let attempt = item
        .attempts
        .iter()
        .find(|a| a.id == retry.before.id)
        .context("retry Attempt missing")?;
    ensure!(
        attempt.cancellation == retry.before.cancellation && !attempt.cancellation_requested(),
        "a later cancellation invalidates context retry authority"
    );
    ensure!(
        !item
            .merge_candidates
            .iter()
            .any(|c| c.attempt_id == attempt.id),
        "retry candidate is already finalized"
    );
    validate_retained_files(root, retry)?;
    let mut expected = prepared_task(retry)?;
    let task = attempt
        .tasks
        .iter()
        .find(|t| t.id == retry.task_id)
        .context("retry Task missing")?;
    if starting {
        ensure!(
            task.status == TaskStatus::Executing && task.started_at.is_some(),
            "retry start has no reservation"
        );
        expected.status = task.status.clone();
        expected.started_at = task.started_at.clone();
    }
    ensure!(
        *task == expected,
        "completion retry Task changed or its authorization was consumed"
    );
    ensure!(
        attempt.tasks.len() == retry.before.tasks.len(),
        "completion retry history changed"
    );
    let index = retry
        .before
        .tasks
        .iter()
        .position(|t| t.id == retry.task_id)
        .unwrap();
    for (i, (prior, actual)) in retry.before.tasks.iter().zip(&attempt.tasks).enumerate() {
        if i == index {
            continue;
        }
        let mut expected = prior.clone();
        if i > index {
            expected.status = TaskStatus::Superseded;
            expected.completed_at = Some(
                attempt
                    .recovery_operations
                    .iter()
                    .find(|r| r.operation_kind == approve_kind(retry))
                    .context("retry has no operation")?
                    .completed_at
                    .clone(),
            );
        }
        ensure!(
            *actual == expected,
            "completion retry changed a source or reservation"
        );
        ensure!(
            actual.status != TaskStatus::Executing,
            "another Task executes"
        );
        crate::usage_limit_contracts::unlocked(
            &crate::lease::task_lock_path(root, &item.id, &actual.id),
            false,
        )?;
        if let Some(area) = &actual.artifact_area {
            crate::usage_limit_contracts::unlocked(
                &root.join(&area.path).join("execution-owner.lock"),
                false,
            )?;
            ensure!(
                !crate::execution::execution_is_live(
                    &root.join(&area.path).join("execution.json")
                )?,
                "another execution remains live"
            );
        }
    }
    if !own_lease {
        crate::usage_limit_contracts::unlocked(
            &crate::lease::task_lock_path(root, &item.id, &task.id),
            false,
        )?;
    }
    crate::usage_limit_contracts::unlocked(
        &crate::lease::learner_lock_path(root, &item.id, &attempt.id),
        false,
    )?;
    let original = failed_task(retry)?;
    match retry.source {
        RetrySource::RepairedContext => {
            let assignment = original.writer_correction_assignment.as_ref().unwrap();
            let source = attempt
                .tasks
                .iter()
                .find(|t| t.id == assignment.source_task_id)
                .context("retry source missing")?;
            let output = source
                .output
                .as_ref()
                .context("retry source has no candidate")?;
            ensure!(
                attempt.current_candidate_commit()
                    == Some(assignment.source_candidate_commit.as_str())
                    && candidate_workspace_is_clean_at_managed(
                        root,
                        output,
                        &assignment.source_candidate_commit
                    ),
                "completion retry candidate changed or is dirty"
            );
            validate_writer_correction_assignment(root, item, task, assignment)?;
            writer_completion::validated_writer_context(root, item, task)?
                .context("retry completion authority missing")?;
        }
        RetrySource::InvalidCompletion => {
            let index = retry
                .before
                .tasks
                .iter()
                .position(|candidate| candidate.id == retry.task_id)
                .unwrap();
            let source = &attempt.tasks[..index]
                .iter()
                .rev()
                .find(|candidate| {
                    candidate.kind == TaskKind::Write && candidate.status == TaskStatus::Complete
                })
                .context("completion retry source missing")?;
            let output = source
                .output
                .as_ref()
                .context("completion retry source has no candidate")?;
            ensure!(
                attempt.current_candidate_commit() == Some(output.commit.as_str())
                    && candidate_workspace_is_clean_at_managed(root, output, &output.commit),
                "completion retry candidate changed or is dirty"
            );
            let baseline = host_write_baseline_path(
                &work_model::WorkModelStore::new(root),
                &item.id,
                &attempt.id,
                &task.id,
            )?;
            ensure!(
                fs::read_to_string(baseline)?.trim() == output.commit,
                "completion retry host baseline changed"
            );
            let baseline = host_write_baseline_path(
                &work_model::WorkModelStore::new(root),
                &item.id,
                &attempt.id,
                &task.id,
            )?;
            ensure!(
                retry.baseline_sha256.as_deref()
                    == Some(format!("sha256:{:x}", Sha256::digest(fs::read(baseline)?)).as_str()),
                "completion retry host baseline bytes changed"
            );
            if let Some(assignment) = &task.writer_correction_assignment {
                validate_writer_correction_assignment(root, item, task, assignment)?;
                validate_writer_continuation_lineage(root, item, task)?;
            }
        }
    }
    ensure!(
        attempt.evidence_recoveries == retry.before.evidence_recoveries
            && attempt.round_cap_extensions == retry.before.round_cap_extensions
            && attempt.coder_mapping == retry.before.coder_mapping,
        "completion retry authority changed"
    );
    let started = attempt
        .recovery_operations
        .iter()
        .any(|r| r.operation_kind == kind_for(retry, "start"));
    ensure!(
        started == starting,
        "completion retry authorization already consumed"
    );
    Ok(())
}

pub(crate) fn validate_retained_files(root: &Path, retry: &Retry) -> Result<()> {
    for (path, digest) in &retry.files {
        ensure!(
            assignment_input_digest(root, path)? == *digest,
            "retained completion retry evidence changed: {path}"
        );
    }
    if retry.source == RetrySource::InvalidCompletion {
        let baseline = host_write_baseline_path(
            &work_model::WorkModelStore::new(root),
            &retry.before.work_item_id,
            &retry.before.id,
            &retry.task_id,
        )?;
        ensure!(
            retry.baseline_sha256.as_deref()
                == Some(format!("sha256:{:x}", Sha256::digest(fs::read(baseline)?)).as_str()),
            "completion retry host baseline bytes changed"
        );
    }
    Ok(())
}

/// Queue admission and foreground resume share this check before any mutation.
pub(crate) fn resume_ready(root: &Path, item: &WorkItem, attempt_id: &str) -> Result<bool> {
    let attempt = item
        .attempts
        .iter()
        .find(|a| a.id == attempt_id)
        .context("Attempt missing")?;
    if !recognized(root, item, attempt)? {
        return Ok(false);
    }
    let retry =
        current(root, attempt)?.context("authorize repair-writer-completion before resume")?;
    let task = attempt
        .tasks
        .iter()
        .find(|t| t.id == retry.task_id)
        .context("retry Task missing")?;
    if task.status == TaskStatus::Complete && task.output.is_some() {
        return Ok(false);
    }
    validate_pending(root, item, &retry, false, false)?;
    Ok(true)
}

/// Derive guidance through the same contracts used by publication and resume.
pub(crate) fn status_action(root: &Path, attempt: &Attempt) -> Result<Option<Option<String>>> {
    if !has_retry_history(attempt) && !completion_failure_shape(attempt) {
        return Ok(None);
    }
    let mut item =
        work_model::WorkModelStore::new(root).read_work_item_snapshot(&attempt.work_item_id)?;
    // Pause derivation runs before the reducer's target is published. Validate
    // that exact Attempt so replay does not refresh previously stale guidance.
    *item
        .attempts
        .iter_mut()
        .find(|a| a.id == attempt.id)
        .context("Attempt missing")? = attempt.clone();
    if !recognized(root, &item, attempt)? {
        return Ok(None);
    }
    if current(root, attempt)?.is_some() {
        return if resume_ready(root, &item, &attempt.id)? {
            Ok(Some(Some(format!(
                "fluent attempt run {} {}",
                attempt.work_item_id, attempt.id
            ))))
        } else {
            Ok(None)
        };
    }
    if completion_failure_shape(attempt) {
        let task = attempt
            .tasks
            .iter()
            .rev()
            .find(|task| task.kind == TaskKind::Write && task.output.is_none());
        if task.is_some_and(|task| {
            task.status == TaskStatus::NeedsUser
                || (task
                    .input_artifacts
                    .iter()
                    .any(|artifact| artifact.producer_id == "writer-completion-recovery")
                    && !attempt
                        .artifacts
                        .iter()
                        .any(|artifact| artifact.producer_id == "assignment-context-recovery"))
        }) {
            inspect(root, &item, &attempt.id, "<describe the retry decision>")?;
            return Ok(Some(Some(format!(
                "fluent attempt repair-writer-completion {} {} --resolution '<describe the retry decision>'",
                attempt.work_item_id, attempt.id
            ))));
        }
    }
    ensure!(
        attempt.status == AttemptStatus::NeedsUser
            && matches!(
                attempt.pause_kind,
                Some(PauseKind::RoundCap | PauseKind::WriterCompletion)
            )
            && !attempt.cancellation_requested(),
        "no eligible context recovery pause"
    );
    let task = attempt
        .tasks
        .iter()
        .find(|t| t.kind == TaskKind::Write && t.status == TaskStatus::Failed && t.output.is_none())
        .context("failed Writer missing")?;
    let assignment = task
        .writer_correction_assignment
        .as_ref()
        .context("assignment missing")?;
    assignment_context_recovery_target(
        root,
        &item,
        &attempt.id,
        &task.id,
        &assignment.source_candidate_commit,
    )?;
    if let Some(action) =
        stale_assignment_context_action(root, attempt, task, &assignment.source_candidate_commit)?
    {
        return Ok(Some(Some(action)));
    }
    inspect(root, &item, &attempt.id, "<describe the retry decision>")?;
    Ok(Some(Some(format!(
        "fluent attempt repair-writer-completion {} {} --resolution '<describe the retry decision>'",
        attempt.work_item_id, attempt.id
    ))))
}

pub(crate) fn validate_start(
    root: &Path,
    item: &WorkItem,
    attempt_id: &str,
    task_id: &str,
    own_lease: bool,
) -> Result<()> {
    let attempt = item
        .attempts
        .iter()
        .find(|a| a.id == attempt_id)
        .context("Attempt missing")?;
    if !recognized(root, item, attempt)? {
        return Ok(());
    }
    let retry =
        current(root, attempt)?.context("same-Task context retry has not been authorized")?;
    if task_id != retry.task_id {
        ensure!(
            attempt.tasks.iter().any(|t| t.id == retry.task_id
                && t.status == TaskStatus::Complete
                && t.output.is_some()),
            "the original failed Writer must retain its assignment; a later Task cannot replace it"
        );
        return Ok(());
    }
    ensure!(
        attempt
            .recovery_operations
            .iter()
            .any(|r| r.matches_request(
                &request(&retry, kind_for(&retry, "resume")).expect("serialize retry")
            )),
        "explicit Attempt resume must prepare the approved context retry"
    );
    validate_pending(root, item, &retry, own_lease, false)
}

pub(crate) fn pending_publications(item: &WorkItem, prior: Option<&WorkItem>) -> Vec<String> {
    item.attempts
        .iter()
        .flat_map(|a| &a.recovery_operations)
        .filter(|r| {
            matches!(
                r.operation_kind.as_str(),
                APPROVE
                    | RESUME
                    | START
                    | COMPLETION_APPROVE
                    | COMPLETION_RESUME
                    | COMPLETION_START
            ) && prior.is_none_or(|p| {
                !p.attempts
                    .iter()
                    .any(|a| a.recovery_operation(&r.operation_id).is_some())
            })
        })
        .map(|r| r.operation_id.clone())
        .collect()
}

pub(crate) fn validate_publication(
    root: &Path,
    item: &WorkItem,
    operations: &[String],
) -> Result<()> {
    for operation in operations {
        let attempt = item
            .attempts
            .iter()
            .find(|a| a.recovery_operation(operation).is_some())
            .context("pending retry operation missing")?;
        let record = attempt.recovery_operation(operation).unwrap();
        let retry = current(root, attempt)?.context("pending retry audit missing")?;
        let starting = matches!(record.operation_kind.as_str(), START | COMPLETION_START);
        ensure!(
            record.matches_request(&request(&retry, &record.operation_kind)?),
            "pending retry operation changed"
        );
        validate_pending(root, item, &retry, starting, starting)?;
    }
    Ok(())
}

pub(crate) fn test_interrupt(point: &str) {
    #[cfg(feature = "test-support")]
    if std::env::var("FLUENT_TEST_CONTEXT_RETRY_INTERRUPT").as_deref() == Ok(point) {
        std::process::exit(86);
    }
    let _ = point;
}
