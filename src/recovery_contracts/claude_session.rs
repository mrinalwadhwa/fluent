//! Recognize the exact released missing-conversation failure. File and process
//! checks here grant no quota, authentication, or additional-round authority.

use super::context_retry::{Retry, RetrySource};
use super::*;
use anyhow::ensure;
use work_model::{Attempt, WorkItem};

pub(crate) const APPROVE: &str = "claude-session-retry";
pub(crate) const RESUME: &str = "claude-session-resume";
pub(crate) const START: &str = "claude-session-start";
pub(crate) const RESOLUTION: &str =
    "Resume the exact zero-turn missing Claude conversation with one fresh persistent Writer";

pub(crate) fn recognized(root: &Path, attempt: &Attempt) -> Result<bool> {
    let Some(task) = attempt.tasks.last().filter(|task| {
        task.kind == TaskKind::Write && task.status == TaskStatus::Failed && task.output.is_none()
    }) else {
        return Ok(false);
    };
    if attempt.status != AttemptStatus::NeedsUser || attempt.pause_kind != Some(PauseKind::RoundCap)
    {
        return Ok(false);
    }
    let Some(run) = attempt
        .writer_runs
        .last()
        .filter(|run| run.provider == "claude")
    else {
        return Ok(false);
    };
    let Some(id) = &run.session_id else {
        return Ok(false);
    };
    let area = task
        .artifact_area
        .as_ref()
        .context("failed Claude Writer has no artifacts")?;
    let transcript = root.join(&area.path).join("transcript.jsonl");
    let stderr = root.join(&area.path).join("provider-stderr.log");
    let bytes = match fs::read(transcript) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    let stderr = match fs::read(stderr) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    Ok(crate::claude_session::is_missing_conversation(
        &bytes, &stderr, id,
    ))
}

pub(crate) fn inspect(
    root: &Path,
    item: &WorkItem,
    attempt: &Attempt,
    resolution: &str,
) -> Result<Retry> {
    ensure!(
        recognized(root, attempt)?,
        "no exact missing Claude conversation failure"
    );
    item.ensure_execution_ready()?;
    item.ensure_not_abandoned()?;
    attempt.ensure_not_canceled()?;
    ensure!(
        attempt.recovery_operations.is_empty()
            && attempt.round_cap_extensions.is_empty()
            && attempt.evidence_recoveries.is_empty()
            && attempt.terminal_mode == work_model::TerminalMode::Headless,
        "missing-session recovery cannot replace another recovery authority"
    );
    ensure!(
        !item
            .merge_candidates
            .iter()
            .any(|candidate| candidate.attempt_id == attempt.id),
        "missing-session candidate is already finalized"
    );
    let task = attempt.tasks.last().unwrap();
    ensure!(
        attempt.tasks[..attempt.tasks.len() - 1]
            .iter()
            .all(|task| task.status == TaskStatus::Complete),
        "missing-session recovery requires settled preceding obligations"
    );
    let prior = attempt
        .tasks
        .iter()
        .rev()
        .skip(1)
        .find(|task| task.kind == TaskKind::Write)
        .context("missing-session recovery has no committed Writer")?;
    let output = prior
        .output
        .as_ref()
        .context("source Writer has no candidate")?;
    let run = attempt.writer_runs.last().unwrap();
    ensure!(
        run.task_id == prior.id
            && run.candidate_commit == output.commit
            && matches!(
                run.outcome,
                work_model::WriterOutcome::Complete | work_model::WriterOutcome::Continue
            )
            && attempt.current_candidate_commit() == Some(output.commit.as_str())
            && candidate_workspace_is_clean_at_managed(root, output, &output.commit)
            && task.workspace_access.writes.len() == 1
            && task.workspace_access.writes[0].path == output.workspace_path
            && task.workspace_access.writes[0].id == output.workspace_id,
        "missing-session recovery candidate or workspace changed"
    );
    ensure!(
        task.instructions
            .as_deref()
            .is_some_and(|text| !text.trim().is_empty())
            && task.instructions == item.write_task_instructions(),
        "missing-session recovery requires the unchanged saved Task assignment"
    );
    if let Some(assignment) = &task.writer_correction_assignment {
        validate_writer_correction_assignment(root, item, task, assignment)?;
    }
    crate::usage_limit_contracts::settled(root, attempt, task, false)?;
    let area = &task.artifact_area.as_ref().unwrap().path;
    ensure!(
        !root
            .join(area)
            .join(crate::claude_session::FALLBACK_FILE)
            .try_exists()?,
        "this Writer already consumed its fresh-session fallback"
    );
    let baseline = work_model::WorkModelStore::new(root)
        .work_task_path(&item.id, &attempt.id, &task.id)?
        .with_extension("write-baseline");
    let baseline_bytes =
        fs::read(baseline).context("missing-session recovery needs the host-owned baseline")?;
    ensure!(
        std::str::from_utf8(&baseline_bytes)?.trim() == output.commit
            && fs::read_to_string(root.join(area).join("write-baseline-commit"))?.trim()
                == output.commit,
        "missing-session recovery baseline changed"
    );
    let mut files = BTreeMap::new();
    for retained in &attempt.tasks {
        crate::usage_limit_contracts::unlocked(
            &crate::lease::task_lock_path(root, &item.id, &retained.id),
            false,
        )?;
        if let Some(area) = &retained.artifact_area {
            ensure!(
                !crate::execution::execution_is_live(
                    &root.join(&area.path).join("execution.json")
                )?,
                "a retained execution is still owned"
            );
            crate::usage_limit_contracts::bind_directory(root, &area.path, &mut files)?;
        }
    }
    for input in &task.input_artifacts {
        crate::usage_limit_contracts::bind_file(root, &input.path, &mut files)?;
    }
    // A retained earlier phase must also prove the same rejection. Any model
    // progress or another provider failure keeps the historical protections.
    for path in files
        .keys()
        .filter(|path| path.starts_with(&format!("{area}/transcript.")) && path.ends_with(".jsonl"))
    {
        ensure!(
            crate::claude_session::is_missing_conversation(
                &fs::read(root.join(path))?,
                b"",
                run.session_id.as_deref().unwrap()
            ),
            "retained launch is not the exact zero-turn missing conversation"
        );
    }
    Ok(Retry {
        schema_version: 3,
        source: RetrySource::MissingClaudeConversation,
        before: attempt.clone(),
        task_id: task.id.clone(),
        context: None,
        baseline_sha256: Some(format!("sha256:{:x}", Sha256::digest(&baseline_bytes))),
        files,
        resolution: resolution.into(),
    })
}
