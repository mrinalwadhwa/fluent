//! Publish the bounded retry through the existing completion-recovery command.
use super::*;
use crate::recovery_contracts::context_retry as contracts;
use crate::work_model::{WorkItem, publish_recovery_snapshot};

pub(super) fn repair(
    root: &Path,
    store: &WorkModelStore,
    work: &str,
    attempt_id: &str,
    resolution: &str,
) -> Result<Option<WriterCompletionRecovery>> {
    let observed = match store.read_work_item_recovery_target(work)? {
        Some(target) => target,
        None => store.read_work_item_snapshot(work)?,
    };
    let attempt = observed
        .attempts
        .iter()
        .find(|a| a.id == attempt_id)
        .context("Attempt missing")?;
    if !contracts::recognized(root, &observed, attempt)? {
        return Ok(None);
    }
    if let Some(retry) = contracts::current(root, attempt)? {
        anyhow::ensure!(
            retry.resolution == resolution,
            "completion retry authorization changed"
        );
        if matches!(
            retry.source,
            contracts::RetrySource::InvalidCompletion
                | contracts::RetrySource::MissingClaudeConversation
        ) && (attempt.cancellation_requested()
            || attempt
                .tasks
                .iter()
                .find(|task| task.id == retry.task_id)
                .is_some_and(|task| {
                    matches!(
                        task.status,
                        TaskStatus::Complete | TaskStatus::Failed | TaskStatus::NeedsUser
                    )
                }))
        {
            contracts::validate_retained_files(root, &retry)?;
            return Ok(Some(result(&retry, true)?));
        }
        contracts::validate_pending(root, &observed, &retry, false, false)?;
        store.mutate_work_item_from_external_with_recovery_check(
            work,
            "replay context-repaired completion retry",
            Some(contracts::audit_ref(&retry)?.path),
            |target| check_replay(root, target, &retry),
            |fresh| check_replay(root, fresh, &retry),
        )?;
        return Ok(Some(result(&retry, true)?));
    }
    let retry = contracts::inspect(root, &observed, attempt_id, resolution)?;
    let reference = contracts::audit_ref(&retry)?;
    store.mutate_work_item_from_external(
        work,
        "authorize completion retry after exact context recovery",
        Some(reference.path.clone()),
        |fresh| {
            let current = contracts::inspect(root, fresh, attempt_id, resolution)
                .map_err(|e| invalid(attempt_id, e))?;
            if current != retry {
                return Err(invalid(
                    attempt_id,
                    "recovery state changed before publication",
                ));
            }
            publish_recovery_snapshot(
                root,
                &reference.path,
                &serde_json::to_vec_pretty(&retry).map_err(|e| invalid(attempt_id, e))?,
            )
            .map_err(|e| invalid(attempt_id, e))?;
            contracts::test_interrupt("audit");
            let attempt = fresh
                .attempts
                .iter_mut()
                .find(|a| a.id == attempt_id)
                .unwrap();
            attempt.status = AttemptStatus::NeedsUser;
            attempt.pause_kind = Some(PauseKind::WriterCompletion);
            attempt.pause_recovery = None;
            attempt.completed_at = None;
            attempt.artifacts.push(reference.clone());
            let record = attempt.record_recovery_operation(
                &contracts::request(&retry, contracts::approve_kind(&retry))
                    .map_err(|e| invalid(attempt_id, e))?,
                Some(format!("fluent attempt run {work} {attempt_id}")),
            )?;
            let index = attempt
                .tasks
                .iter()
                .position(|t| t.id == retry.task_id)
                .unwrap();
            attempt.tasks[index] =
                contracts::prepared_task(&retry).map_err(|e| invalid(attempt_id, e))?;
            for task in &mut attempt.tasks[index + 1..] {
                task.status = TaskStatus::Superseded;
                task.completed_at = Some(record.completed_at.clone());
            }
            Ok(())
        },
    )?;
    Ok(Some(result(&retry, false)?))
}

fn invalid(attempt: &str, error: impl std::fmt::Display) -> WorkModelError {
    WorkModelError::AttemptResumeIneligible {
        attempt_id: attempt.into(),
        reason: format!("context-repaired Writer retry: {error}"),
    }
}

fn check_replay(
    root: &Path,
    item: &WorkItem,
    retry: &contracts::Retry,
) -> std::result::Result<(), WorkModelError> {
    let attempt = item
        .attempts
        .iter()
        .find(|a| a.id == retry.before.id)
        .ok_or_else(|| invalid(&retry.before.id, "Attempt missing"))?;
    if contracts::current(root, attempt)
        .map_err(|e| invalid(&attempt.id, e))?
        .as_ref()
        != Some(retry)
    {
        return Err(invalid(&attempt.id, "pending authorization changed"));
    }
    contracts::validate_pending(root, item, retry, false, false)
        .map_err(|e| invalid(&attempt.id, e))
}

fn result(retry: &contracts::Retry, replay: bool) -> Result<WriterCompletionRecovery> {
    let task = contracts::failed_task(retry)?;
    let index = retry
        .before
        .tasks
        .iter()
        .position(|t| t.id == task.id)
        .unwrap();
    Ok(WriterCompletionRecovery {
        task_id: task.id.clone(),
        candidate_commit: contracts::request(retry, contracts::approve_kind(retry))?
            .candidate_commit
            .context("completion retry has no candidate")?,
        audit_artifact: contracts::audit_ref(retry)?.path,
        superseded_task_ids: retry.before.tasks[index + 1..]
            .iter()
            .map(|t| t.id.clone())
            .collect(),
        reconciled_task_ids: vec![],
        already_applied: replay,
    })
}

pub(crate) fn record_start(
    root: &Path,
    item: &mut WorkItem,
    attempt_id: &str,
    task_id: &str,
) -> Result<()> {
    let attempt = item
        .attempts
        .iter_mut()
        .find(|a| a.id == attempt_id)
        .context("Attempt missing")?;
    if let Some(retry) = contracts::current(root, attempt)? {
        if task_id == retry.task_id {
            let operation = contracts::request(&retry, contracts::START)?;
            anyhow::ensure!(
                attempt
                    .recovery_operation(&operation.operation_id())
                    .is_none(),
                "completion retry start already consumed"
            );
            attempt.record_recovery_operation(&operation, None)?;
        }
    }
    Ok(())
}

/// Explicit resume prepares the already authorized Task before generic replanning.
pub(crate) fn resume(
    root: &Path,
    store: &WorkModelStore,
    work: &str,
    attempt_id: &str,
) -> Result<()> {
    let observed = store.read_work_item_snapshot(work)?;
    if !contracts::resume_ready(root, &observed, attempt_id)? {
        return Ok(());
    }
    let attempt = observed
        .attempts
        .iter()
        .find(|attempt| attempt.id == attempt_id)
        .context("Attempt missing")?;
    if contracts::current(root, attempt)?.is_none()
        && crate::recovery_contracts::claude_session::recognized(root, attempt)?
    {
        repair(
            root,
            store,
            work,
            attempt_id,
            crate::recovery_contracts::claude_session::RESOLUTION,
        )?
        .context("missing-session retry was not prepared")?;
    }
    store.mutate_work_item_from_external(work, "resume context-repaired Writer", None, |item| {
        let attempt = item.attempts.iter().find(|a| a.id == attempt_id).unwrap();
        let retry = contracts::current(root, attempt)
            .map_err(|e| invalid(attempt_id, e))?
            .unwrap();
        contracts::validate_pending(root, item, &retry, false, false)
            .map_err(|e| invalid(attempt_id, e))?;
        let operation =
            contracts::request(&retry, contracts::RESUME).map_err(|e| invalid(attempt_id, e))?;
        let attempt = item
            .attempts
            .iter_mut()
            .find(|a| a.id == attempt_id)
            .unwrap();
        attempt.status = AttemptStatus::Planned;
        attempt.pause_kind = None;
        attempt.pause_record = None;
        attempt.record_recovery_operation(&operation, None)?;
        Ok(())
    })?;
    Ok(())
}

/// Recheck the exact candidate under execution's model lock and consume one spawn.
pub(crate) fn before_spawn(
    root: &Path,
    item: &WorkItem,
    attempt_id: &str,
    transcript: &Path,
    handoff_correction: bool,
) -> Result<()> {
    let attempt = item
        .attempts
        .iter()
        .find(|a| a.id == attempt_id)
        .context("Attempt missing")?;
    let Some(retry) = contracts::current(root, attempt)? else {
        return Ok(());
    };
    let task = contracts::prepared_task(&retry)?;
    if transcript
        != root
            .join(&task.artifact_area.as_ref().unwrap().path)
            .join("transcript.jsonl")
    {
        return Ok(());
    }
    contracts::validate_pending(root, item, &retry, true, true)?;
    let marker = root
        .join(contracts::audit_ref(&retry)?.path)
        .with_file_name("launch.json");
    if marker.try_exists()? {
        if retry.source == contracts::RetrySource::MissingClaudeConversation
            && crate::claude_session::FallbackLaunch::active()
        {
            let fallback = marker.with_file_name("claude-fallback-launch.json");
            anyhow::ensure!(
                !fallback.try_exists()?,
                "this completion retry already attempted its fresh Claude launch"
            );
            publish_recovery_snapshot(
                root,
                fallback
                    .strip_prefix(root)?
                    .to_str()
                    .context("invalid Claude fallback marker path")?,
                b"missing-claude-conversation",
            )?;
            return Ok(());
        }
        anyhow::ensure!(
            handoff_correction && retry.source == contracts::RetrySource::RepairedContext,
            "this completion retry already attempted a provider launch"
        );
        let correction = marker.with_file_name("handoff-correction-launch.json");
        anyhow::ensure!(
            !correction.try_exists()?,
            "this completion retry already attempted a handoff correction"
        );
        publish_recovery_snapshot(
            root,
            correction
                .strip_prefix(root)?
                .to_str()
                .context("invalid handoff correction marker path")?,
            b"handoff-correction",
        )?;
        return Ok(());
    }
    let bytes = contracts::request(&retry, contracts::START)?.operation_id();
    // The caller holds the model lock through this one-time claim and spawn.
    publish_recovery_snapshot(
        root,
        marker
            .strip_prefix(root)?
            .to_str()
            .context("invalid launch marker path")?,
        bytes.as_bytes(),
    )?;
    Ok(())
}
