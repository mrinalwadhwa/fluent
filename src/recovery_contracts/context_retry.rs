//! Validate one completion retry after audited, output-free context recovery.
use super::claude_session as claude;
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
    MissingClaudeConversation,
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
        RetrySource::MissingClaudeConversation => claude::APPROVE,
    }
}

pub(crate) fn kind_for(retry: &Retry, kind: &str) -> &'static str {
    match (retry.source, kind) {
        (RetrySource::RepairedContext, "resume") => RESUME,
        (RetrySource::RepairedContext, "start") => START,
        (RetrySource::InvalidCompletion, "resume") => COMPLETION_RESUME,
        (RetrySource::InvalidCompletion, "start") => COMPLETION_START,
        (RetrySource::MissingClaudeConversation, "resume") => claude::RESUME,
        (RetrySource::MissingClaudeConversation, "start") => claude::START,
        _ => approve_kind(retry),
    }
}

fn has_retry_history(attempt: &Attempt) -> bool {
    attempt.recovery_operations.iter().any(|r| {
        matches!(
            r.operation_kind.as_str(),
            APPROVE | COMPLETION_APPROVE | claude::APPROVE
        )
    }) || attempt.tasks.iter().any(|t| {
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
    if claude::recognized(root, attempt)? {
        return Ok(true);
    }
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
        let invalid_matrix = invalid_completion_matrix(root, item, attempt, task, &matrix)?;
        let invalid_no_change = fs::read(root.join(&area.path).join("no-change.json"))
            .ok()
            .is_some_and(|bytes| {
                serde_json::from_slice::<work_model::NoChangeOutput>(&bytes).is_err()
            });
        if !invalid_matrix && !invalid_no_change {
            return Ok(false);
        }
    }
    if attempt.recovery_operations.iter().any(|r| {
        matches!(
            r.operation_kind.as_str(),
            APPROVE | COMPLETION_APPROVE | claude::APPROVE
        )
    }) {
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
    if claude::recognized(root, attempt)? {
        return claude::inspect(root, item, attempt, resolution);
    }
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

/// An incomplete, valid matrix is ordinary work, not exceptional retry authority.
fn invalid_completion_matrix(
    root: &Path,
    item: &WorkItem,
    attempt: &Attempt,
    task: &Task,
    bytes: &[u8],
) -> Result<bool> {
    let contract = attempt
        .writer_completion_contract
        .as_ref()
        .context("completion retry requires its immutable completion contract")?;
    contract.validate().map_err(anyhow::Error::msg)?;
    let findings = writer_completion_findings_from_item(root, task, Some(item))?;
    Ok(
        match serde_json::from_slice::<work_model::WriterCompletionMatrix>(bytes) {
            Err(_) => true,
            Ok(matrix) => matrix
                .status_against(contract, &task.id, &findings)
                .is_err(),
        },
    )
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
    let index = attempt
        .tasks
        .iter()
        .position(|candidate| candidate.id == task.id)
        .unwrap();
    if task.status == TaskStatus::Failed {
        validate_unstarted_writer_reservation(root, &attempt.tasks[index + 1..])?;
    } else {
        ensure!(
            index + 1 == attempt.tasks.len(),
            "completion retry requires the latest Writer Task"
        );
    }
    ensure!(
        attempt.tasks[..index]
            .iter()
            .all(|prior| matches!(prior.status, TaskStatus::Complete | TaskStatus::Superseded)),
        "completion retry has unresolved prior Tasks"
    );
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
    let bad_matrix = invalid_completion_matrix(root, item, attempt, task, &matrix)?;
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
                && record.process_group_id == 0
                && record.settled_at.is_none()
                && record.heartbeat_at == record.started_at
                && crate::execution::receipt_absent_or_prior_generation(
                    &execution_path,
                    record.launch_generation.as_deref().unwrap(),
                )?,
            "failed completion retry has no proof that this generation stopped before provider launch"
        );
        validate_failed_preparation_owner(root, item, attempt, task, &record)?;
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
        RetrySource::InvalidCompletion | RetrySource::MissingClaudeConversation => {
            retry.before.tasks[..retry
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
                .context("completion retry candidate missing")?
        }
    };
    let kind = if matches!(kind, RESUME | COMPLETION_RESUME | claude::RESUME) {
        kind_for(retry, "resume")
    } else if matches!(kind, START | COMPLETION_START | claude::START) {
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
        RetrySource::MissingClaudeConversation => (claude::APPROVE, "claude-session-retries"),
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
    let Some(operation) = attempt.recovery_operations.iter().find(|r| {
        matches!(
            r.operation_kind.as_str(),
            APPROVE | COMPLETION_APPROVE | claude::APPROVE
        )
    }) else {
        return Ok(None);
    };
    ensure!(
        attempt
            .recovery_operations
            .iter()
            .filter(|r| matches!(
                r.operation_kind.as_str(),
                APPROVE | COMPLETION_APPROVE | claude::APPROVE
            ))
            .count()
            == 1,
        "ambiguous completion retry authority"
    );
    let directory = if operation.operation_kind == claude::APPROVE {
        "claude-session-retries"
    } else if operation.operation_kind == COMPLETION_APPROVE {
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
            || (retry.schema_version == 2 && retry.source == RetrySource::InvalidCompletion)
            || (retry.schema_version == 3
                && retry.source == RetrySource::MissingClaudeConversation))
            && retry.before.id == attempt.id
            && retry.before.work_item_id == attempt.work_item_id
            && operation.matches_request(&request(&retry, approve_kind(&retry))?)
            && reference == audit_ref(&retry)?
            && bytes == serde_json::to_vec_pretty(&retry)?,
        "completion retry audit or operation changed"
    );
    Ok(Some(retry))
}

fn superseded_by_usage_limit(root: &Path, item: &WorkItem, retry: &Retry) -> Result<bool> {
    let attempt = item
        .attempts
        .iter()
        .find(|attempt| attempt.id == retry.before.id)
        .context("completion retry Attempt missing")?;
    let Some(approval) = crate::usage_limit_contracts::current_approval(root, attempt)? else {
        return Ok(false);
    };
    Ok(approval.binding.failed_task.id == retry.task_id)
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
    ensure!(
        attempt.writer_completion_contract == retry.before.writer_completion_contract,
        "completion retry immutable contract changed"
    );
    ensure!(
        attempt
            .recovery_operations
            .starts_with(&retry.before.recovery_operations)
            && attempt.recovery_operations[retry.before.recovery_operations.len()..]
                .iter()
                .all(|operation| {
                    [
                        approve_kind(retry),
                        kind_for(retry, "resume"),
                        kind_for(retry, "start"),
                    ]
                    .contains(&operation.operation_kind.as_str())
                        && request(retry, &operation.operation_kind)
                            .is_ok_and(|request| operation.matches_request(&request))
                }),
        "completion retry recovery history changed"
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
    if retry.source == RetrySource::MissingClaudeConversation {
        ensure!(
            task.instructions == item.write_task_instructions()
                && attempt.writer_runs == retry.before.writer_runs
                && attempt.terminal_mode == retry.before.terminal_mode,
            "missing-session retry assignment or session identity changed"
        );
    }
    if retry.source == RetrySource::InvalidCompletion {
        let area = original
            .artifact_area
            .as_ref()
            .context("original artifact area missing")?;
        crate::usage_limit_contracts::unlocked(
            &root.join(&area.path).join("execution-owner.lock"),
            false,
        )?;
        if original.status == TaskStatus::Failed {
            validate_unstarted_writer_reservation(root, &retry.before.tasks[index + 1..])?;
            let record = serde_json::from_slice(&assignment_input_bytes(
                root,
                &format!("{}/execution.json", area.path),
            )?)?;
            validate_failed_preparation_owner(root, item, attempt, original, &record)?;
            writer_completion::validated_writer_context(root, item, task)?
                .context("completion retry lost its original approval")?;
        }
    }
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
        RetrySource::InvalidCompletion | RetrySource::MissingClaudeConversation => {
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
    if retry.source == RetrySource::MissingClaudeConversation {
        // Additions can change the proof of settlement just as replacements can.
        // Rebuild only the original areas; the prepared retry owns a new area.
        let mut current = BTreeMap::new();
        for task in &retry.before.tasks {
            if let Some(area) = &task.artifact_area {
                crate::usage_limit_contracts::bind_directory(root, &area.path, &mut current)?;
            }
        }
        let task = retry
            .before
            .tasks
            .iter()
            .find(|task| task.id == retry.task_id)
            .context("missing-session source Task disappeared")?;
        for input in &task.input_artifacts {
            crate::usage_limit_contracts::bind_file(root, &input.path, &mut current)?;
        }
        ensure!(
            current == retry.files,
            "retained missing-session evidence changed"
        );
    }
    for (path, digest) in &retry.files {
        ensure!(
            assignment_input_digest(root, path)? == *digest,
            "retained completion retry evidence changed: {path}"
        );
    }
    if retry.source == RetrySource::InvalidCompletion {
        let area = failed_task(retry)?
            .artifact_area
            .as_ref()
            .context("original artifact area missing")?;
        let mut files = BTreeMap::new();
        crate::usage_limit_contracts::bind_directory(root, &area.path, &mut files)?;
        let prefix = format!("{}/", area.path);
        let expected = retry
            .files
            .iter()
            .filter(|(path, _)| path.starts_with(&prefix))
            .map(|(path, digest)| (path.clone(), digest.clone()))
            .collect::<BTreeMap<_, _>>();
        ensure!(
            files == expected,
            "retained completion retry artifact inventory changed"
        );
    }
    if matches!(
        retry.source,
        RetrySource::InvalidCompletion | RetrySource::MissingClaudeConversation
    ) {
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
    if current(root, attempt)?.is_none() && claude::recognized(root, attempt)? {
        claude::inspect(root, item, attempt, claude::RESOLUTION)?;
        return Ok(true);
    }
    let retry =
        current(root, attempt)?.context("authorize repair-writer-completion before resume")?;
    if superseded_by_usage_limit(root, item, &retry)? {
        return Ok(false);
    }
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
    if claude::recognized(root, attempt)? {
        claude::inspect(root, &item, attempt, claude::RESOLUTION)?;
        return Ok(Some(Some(format!(
            "fluent attempt run {} {}",
            attempt.work_item_id, attempt.id
        ))));
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
        let task = attempt.tasks.iter().rev().find(|task| {
            task.kind == TaskKind::Write
                && task.output.is_none()
                && matches!(task.status, TaskStatus::NeedsUser | TaskStatus::Failed)
        });
        if let Some(task) = task
            && task.status == TaskStatus::Failed
            && let Some(assignment) = &task.writer_correction_assignment
            && let Some(action) = stale_assignment_context_action(
                root,
                attempt,
                task,
                &assignment.source_candidate_commit,
            )?
        {
            // Exact-byte context recovery has its own output-free proof. Do
            // not apply its absent-receipt rule to a current assignment whose
            // completion recovery retains an earlier generation's receipt.
            assignment_context_recovery_target(
                root,
                &item,
                &attempt.id,
                &task.id,
                &assignment.source_candidate_commit,
            )?;
            return Ok(Some(Some(action)));
        }
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
    if superseded_by_usage_limit(root, item, &retry)? {
        return Ok(());
    }
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

/// Retain exact reservation provenance after the approved retry completes.
/// This proves terminal history only; it grants no review or Learning credit.
pub(crate) fn terminal_superseded_reservations(
    root: &Path,
    attempt: &Attempt,
) -> Result<Vec<String>> {
    let Some(retry) = current(root, attempt)? else {
        return Ok(Vec::new());
    };
    if retry.source != RetrySource::InvalidCompletion {
        return Ok(Vec::new());
    }
    let index = retry
        .before
        .tasks
        .iter()
        .position(|task| task.id == retry.task_id)
        .context("retry lost original Task")?;
    let later = &retry.before.tasks[index + 1..];
    if later.is_empty() {
        return Ok(Vec::new());
    }
    validate_unstarted_writer_reservation(root, later)?;
    validate_retained_files(root, &retry)?;
    let task = attempt
        .tasks
        .iter()
        .find(|task| task.id == retry.task_id)
        .context("completed retry Task missing")?;
    ensure!(
        task.status == TaskStatus::Complete
            && task.output.is_some()
            && task.started_at.is_some()
            && task.completed_at.is_some(),
        "reservation supersession requires its completed retry"
    );
    let mut expected = prepared_task(&retry)?;
    expected.status = task.status.clone();
    expected.started_at = task.started_at.clone();
    expected.completed_at = task.completed_at.clone();
    expected.output = task.output.clone();
    ensure!(*task == expected, "completed retry identity changed");
    for kind in [kind_for(&retry, "resume"), kind_for(&retry, "start")] {
        let request = request(&retry, kind)?;
        ensure!(
            attempt
                .recovery_operations
                .iter()
                .any(|operation| operation.matches_request(&request)),
            "completed retry lost its resume or start authority"
        );
    }
    let approved = attempt
        .recovery_operations
        .iter()
        .find(|operation| operation.operation_kind == approve_kind(&retry))
        .context("completed retry approval missing")?;
    let mut ids = Vec::new();
    for reservation in later {
        let actual = attempt
            .tasks
            .iter()
            .find(|task| task.id == reservation.id)
            .context("superseded retry reservation missing")?;
        let mut expected = reservation.clone();
        expected.status = TaskStatus::Superseded;
        expected.completed_at = Some(approved.completed_at.clone());
        ensure!(*actual == expected, "superseded retry reservation changed");
        ids.push(actual.id.clone());
    }
    Ok(ids)
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
                    | claude::APPROVE
                    | claude::RESUME
                    | claude::START
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
        let starting = matches!(
            record.operation_kind.as_str(),
            START | COMPLETION_START | claude::START
        );
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
