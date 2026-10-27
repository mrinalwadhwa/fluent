//! Inspect saved quota authority without calling lifecycle producers.
//!
//! Public recovery records remain append-only. Immutable typed inputs live in an
//! audit beside (never inside) the retry's writable artifact directory.
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::path::{Component, Path, PathBuf};

use crate::work_model::{
    self, ArtifactRef, Attempt, AttemptStatus, CoderModelPair, PauseKind, RecoveryOperationRequest,
    Task, TaskArtifactArea, TaskKind, TaskStatus, WorkItem, WorkModelError, WorkModelStore,
};

pub(crate) const APPROVE: &str = "usage-limit-writer-approval";
pub(crate) const PREPARE: &str = "usage-limit-writer-retry";
pub(crate) const START: &str = "usage-limit-writer-start";
pub(crate) const LIMIT: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PartialCandidate {
    pub(crate) head: String,
    pub(crate) branch: String,
    pub(crate) git_dir: String,
    pub(crate) index_digest: String,
    pub(crate) index_entries_digest: String,
    pub(crate) files: BTreeMap<String, Option<(u32, String)>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Binding {
    pub(crate) work_item_id: String,
    pub(crate) attempt_id: String,
    pub(crate) failed_task: Task,
    pub(crate) prior_tasks: Vec<Task>,
    pub(crate) source_candidate: String,
    pub(crate) coder: CoderModelPair,
    pub(crate) workspace: String,
    pub(crate) partial: PartialCandidate,
    pub(crate) files: BTreeMap<String, String>,
    pub(crate) thread_id: String,
    pub(crate) cancellation: Option<work_model::AttemptCancellation>,
    pub(crate) round_cap_extensions: Vec<work_model::RoundCapExtension>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Approval {
    pub(crate) schema_version: u32,
    pub(crate) binding: Binding,
    pub(crate) resolution: String,
    pub(crate) limitation: String,
}

#[derive(Debug, Clone)]
pub struct RecoveryApproval {
    pub operation_id: String,
    pub task_id: String,
    pub audit_artifact: String,
    pub partial_commit: String,
}

pub(crate) fn digest(bytes: &[u8]) -> String {
    crate::follow_up::content_digest(bytes)
}

pub(crate) fn git_bytes(root: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let result = crate::git::run_raw(root, args)?;
    ensure!(
        result.status.success(),
        "inspect partial candidate: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    Ok(result.stdout)
}

pub(crate) fn git_text(root: &Path, args: &[&str]) -> Result<String> {
    Ok(String::from_utf8(git_bytes(root, args)?)?
        .trim()
        .to_string())
}

pub(crate) fn canonical_relative(path: &str) -> Result<()> {
    let value = Path::new(path);
    ensure!(
        !path.is_empty()
            && value
                .components()
                .all(|part| matches!(part, Component::Normal(_)))
            && value.components().collect::<PathBuf>().as_os_str() == value.as_os_str(),
        "noncanonical recovery path: {path}"
    );
    Ok(())
}

pub(crate) fn file_bytes(path: &Path) -> Result<Vec<u8>> {
    let metadata = fs::symlink_metadata(path)?;
    ensure!(
        metadata.is_file() && !metadata.file_type().is_symlink() && metadata.len() <= LIMIT,
        "unsupported recovery file: {}",
        path.display()
    );
    Ok(fs::read(path)?)
}

pub(crate) fn generated(path: &str) -> bool {
    // Fixed runtime/cache exclusions only. Tracked files always remain bound.
    [
        "target",
        ".git",
        ".fluent/work",
        ".scratch",
        "node_modules",
        ".cargo-home",
        "tests/output",
    ]
    .iter()
    .any(|root| path == *root || path.starts_with(&format!("{root}/")))
}

pub(crate) fn partial_candidate(workspace: &Path) -> Result<PartialCandidate> {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::PermissionsExt;
    let mut tracked = BTreeSet::new();
    let mut paths = BTreeSet::new();
    for (args, is_tracked) in [
        (&["ls-files", "--cached", "-z"][..], true),
        (&["ls-files", "--others", "-z"][..], false),
    ] {
        for raw in git_bytes(workspace, args)?
            .split(|b| *b == 0)
            .filter(|p| !p.is_empty())
        {
            let path = std::str::from_utf8(raw)?.to_string();
            canonical_relative(&path)?;
            if is_tracked {
                tracked.insert(path.clone());
            }
            if is_tracked || !generated(&path) {
                paths.insert(path);
            }
        }
    }
    ensure!(
        paths.len() <= 100_000,
        "partial candidate has too many files"
    );
    let mut files = BTreeMap::new();
    for path in paths {
        let full = workspace.join(&path);
        // Bind source symlink leaves without traversing symlink ancestors.
        let mut parent = full.parent();
        while let Some(p) = parent.filter(|p| *p != workspace) {
            match fs::symlink_metadata(p) {
                Ok(metadata) => ensure!(
                    !metadata.file_type().is_symlink(),
                    "source path traverses a symlink"
                ),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound && tracked.contains(&path) => {}
                Err(e) => return Err(e.into()),
            }
            parent = p.parent();
        }
        let value = match fs::symlink_metadata(&full) {
            Ok(metadata) => {
                let permissions = metadata.permissions().mode() & 0o777;
                Some(if metadata.file_type().is_symlink() {
                    // Preserve regular-file bindings; tag literal link targets
                    // with the POSIX symlink type to distinguish equal bytes.
                    let target = fs::read_link(&full)?;
                    (
                        0o120000 | permissions,
                        digest(target.as_os_str().as_bytes()),
                    )
                } else {
                    (permissions, digest(&file_bytes(&full)?))
                })
            }
            Err(error)
                if error.kind() == std::io::ErrorKind::NotFound && tracked.contains(&path) =>
            {
                None
            }
            Err(error) => return Err(error.into()),
        };
        files.insert(path, value);
    }
    let git_dir = git_text(workspace, &["rev-parse", "--absolute-git-dir"])?;
    let index = git_text(
        workspace,
        &["rev-parse", "--path-format=absolute", "--git-path", "index"],
    )?;
    let entries = git_bytes(workspace, &["ls-files", "--stage", "-z"])?;
    ensure!(
        entries
            .split(|b| *b == 0)
            .filter(|s| !s.is_empty())
            .all(|entry| {
                entry
                    .split(|b| *b == b'\t')
                    .next()
                    .is_some_and(|header| header.ends_with(b" 0"))
            }),
        "unmerged index cannot be approved"
    );
    Ok(PartialCandidate {
        head: git_text(workspace, &["rev-parse", "HEAD"])?,
        branch: git_text(workspace, &["symbolic-ref", "HEAD"])?,
        git_dir,
        index_digest: digest(&file_bytes(Path::new(&index))?),
        index_entries_digest: digest(&entries),
        files,
    })
}

pub(crate) fn bind_file(
    root: &Path,
    relative: &str,
    files: &mut BTreeMap<String, String>,
) -> Result<Vec<u8>> {
    canonical_relative(relative)?;
    ensure!(
        Path::new(relative).starts_with(".fluent/work"),
        "input is not a managed Work file"
    );
    let path = root.join(relative);
    let mut current = root.to_path_buf();
    for part in Path::new(relative).components() {
        current.push(part);
        ensure!(
            !fs::symlink_metadata(&current)
                .with_context(|| format!("inspect recovery input {}", current.display()))?
                .file_type()
                .is_symlink(),
            "recovery input traverses a symlink"
        );
    }
    let bytes = file_bytes(&path)?;
    files.insert(relative.to_string(), digest(&bytes));
    Ok(bytes)
}

pub(crate) fn bind_directory(
    root: &Path,
    relative: &str,
    files: &mut BTreeMap<String, String>,
) -> Result<()> {
    canonical_relative(relative)?;
    let path = root.join(relative);
    ensure!(
        fs::symlink_metadata(&path)?.is_dir(),
        "invalid retained artifact directory"
    );
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let child = format!(
            "{relative}/{}",
            entry
                .file_name()
                .to_str()
                .context("non-UTF8 artifact name")?
        );
        if entry.file_type()?.is_dir() {
            bind_directory(root, &child, files)?;
        } else {
            bind_file(root, &child, files)?;
        }
    }
    Ok(())
}

/// Probe existing ownership files without creating or repairing them.
pub(crate) fn unlocked(path: &Path, required: bool) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => ensure!(
            metadata.is_file() && !metadata.file_type().is_symlink(),
            "invalid ownership lock"
        ),
        Err(e) if !required && e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e).context("execution ownership is unverifiable"),
    }
    let file = match File::open(path) {
        Ok(file) => file,
        Err(e) if !required && e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e).context("execution ownership is unverifiable"),
    };
    ensure!(
        fs::symlink_metadata(path)?.is_file(),
        "invalid ownership lock"
    );
    rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive)
        .context("execution ownership remains live or unverifiable")?;
    Ok(())
}

pub(crate) fn settled(
    root: &Path,
    attempt: &Attempt,
    failed: &Task,
    allow_task_lease: bool,
) -> Result<()> {
    for task in &attempt.tasks {
        ensure!(
            task.status != TaskStatus::Executing,
            "another Task still executes"
        );
        if !allow_task_lease || task.id != failed.id {
            unlocked(
                &crate::lease::task_lock_path(root, &attempt.work_item_id, &task.id),
                task.id == failed.id,
            )?;
        }
    }
    unlocked(
        &crate::lease::learner_lock_path(root, &attempt.work_item_id, &attempt.id),
        false,
    )?;
    let area = &failed
        .artifact_area
        .as_ref()
        .context("failed Writer has no artifacts")?
        .path;
    let record_path = format!("{area}/execution.json");
    let record: crate::execution::ExecutionRecord =
        serde_json::from_slice(&bind_file(root, &record_path, &mut BTreeMap::new())?)?;
    ensure!(
        matches!(record.schema_version, 1 | 2)
            && (record.schema_version == 1 || record.launch_generation.is_some())
            && record.state == crate::execution::ExecutionState::Settled
            && record.settled_at.is_some(),
        "failed execution has not verifiably settled"
    );
    let owner = record
        .attempt_owner
        .context("failed execution has no Attempt binding")?;
    ensure!(
        owner.project_root == root.canonicalize()?
            && owner.work_item_id == attempt.work_item_id
            && owner.attempt_id == attempt.id,
        "failed execution belongs to a different Attempt"
    );
    unlocked(&root.join(area).join("execution-owner.lock"), true)?;
    for pid in [record.owner_pid, record.leader_pid] {
        ensure!(
            pid > 0 && pid <= i32::MAX as u32,
            "invalid recorded execution process"
        );
        let result = unsafe { libc::kill(pid as i32, 0) };
        ensure!(
            result == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH),
            "recorded execution process is live or unverifiable"
        );
    }
    ensure!(
        record.process_group_id > 0 && record.process_group_id <= i32::MAX as u32,
        "invalid execution group"
    );
    let result = unsafe { libc::kill(-(record.process_group_id as i32), 0) };
    ensure!(
        result == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH),
        "recorded execution group is live or unverifiable"
    );
    match fs::symlink_metadata(root.join(area).join("coder-supervision.json")) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
        Ok(_) => bail!("retained supervision failure requires separate recovery"),
    }
    Ok(())
}

pub(crate) fn inspect(root: &Path, item: &WorkItem, attempt_id: &str) -> Result<Binding> {
    inspect_with_lease(root, item, attempt_id, false)
}

pub(crate) fn inspect_with_lease(
    root: &Path,
    item: &WorkItem,
    attempt_id: &str,
    own_task_lease: bool,
) -> Result<Binding> {
    item.ensure_mutation_compatible()?;
    item.ensure_not_abandoned()?;
    item.ensure_execution_ready()?;
    let attempt = item
        .attempts
        .last()
        .filter(|a| a.id == attempt_id)
        .context("recovery requires the current Attempt")?;
    ensure!(
        !item
            .merge_candidates
            .iter()
            .any(|c| c.attempt_id == attempt_id),
        "Attempt already has a Merge Candidate"
    );
    ensure!(
        attempt.status == AttemptStatus::NeedsUser
            && attempt.pause_kind == Some(PauseKind::RoundCap)
            && !attempt.cancellation_requested(),
        "recovery requires the saved quota pause without a later stop"
    );
    let failed = attempt.tasks.last().context("Attempt has no Writer")?;
    ensure!(
        failed.kind == TaskKind::Write
            && failed.status == TaskStatus::Failed
            && failed.output.is_none()
            && failed.started_at.is_some()
            && failed.completed_at.is_some(),
        "last Task is not a settled failed Writer"
    );
    ensure!(
        attempt.tasks[..attempt.tasks.len() - 1]
            .iter()
            .all(|t| matches!(t.status, TaskStatus::Complete | TaskStatus::Superseded)),
        "another unresolved Task blocks recovery"
    );
    ensure!(
        attempt.coder_mapping.write.coder == crate::coder::CoderKind::Codex,
        "unsupported saved provider"
    );
    let assignment = failed
        .writer_correction_assignment
        .as_ref()
        .context("failed Writer has no preserved correction assignment")?;
    crate::recovery_contracts::validate_writer_correction_assignment(
        root, item, failed, assignment,
    )?;
    settled(root, attempt, failed, own_task_lease)?;
    let source = attempt
        .tasks
        .iter()
        .rev()
        .find(|t| t.kind == TaskKind::Write && t.status == TaskStatus::Complete)
        .and_then(|t| t.output.as_ref())
        .context("no completed source Writer")?;
    ensure!(
        failed.workspace_access.writes.len() == 1
            && failed.workspace_access.writes[0].path == source.workspace_path,
        "failed Writer workspace differs from source Writer"
    );
    let workspace = work_model::resolve_managed_sibling_workspace_path(
        root,
        &source.workspace_path,
        "partial candidate",
    )?;
    let common = git_text(
        &workspace,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    ensure!(
        Path::new(&common).canonicalize()?
            == crate::worktree::git_common_dir(root)?.canonicalize()?,
        "partial candidate belongs to another repository"
    );
    let registered = git_bytes(root, &["worktree", "list", "--porcelain", "-z"])?;
    let expected = format!("worktree {}", workspace.canonicalize()?.display());
    ensure!(
        registered
            .split(|b| *b == 0)
            .any(|line| line == expected.as_bytes()),
        "partial candidate is not a registered worktree"
    );
    let partial = partial_candidate(&workspace)?;
    ensure!(
        crate::git::run_raw(
            &workspace,
            &["merge-base", "--is-ancestor", &source.commit, &partial.head]
        )?
        .status
        .success(),
        "partial HEAD does not descend from the completed source Writer"
    );
    let area = &failed
        .artifact_area
        .as_ref()
        .context("no failed artifact area")?
        .path;
    ensure!(
        area.starts_with(&format!(
            "{}/",
            work_model::work_artifact_path(&item.id, attempt_id, "").trim_end_matches('/')
        )),
        "failed artifacts belong to a different Attempt"
    );
    let original_area = work_model::work_artifact_path(&item.id, attempt_id, &failed.id);
    let bound_retry_area = attempt
        .recovery_operations
        .iter()
        .filter(|r| r.operation_kind == APPROVE)
        .any(|record| {
            let expected = work_model::work_artifact_path(
                &item.id,
                attempt_id,
                &format!("usage-limit-retries/{}", record.operation_id),
            );
            record.task_id.as_deref() == Some(failed.id.as_str())
                && expected == *area
                && attempt.recovery_operations.iter().any(|started| {
                    started.operation_kind == START
                        && started.task_id == record.task_id
                        && started.input_digest == record.input_digest
                })
        });
    let bound_completion_retry_area =
        if let Some(retry) = crate::recovery_contracts::context_retry::current(root, attempt)? {
            if retry.task_id == failed.id {
                let mut expected = crate::recovery_contracts::context_retry::prepared_task(&retry)?;
                expected.status = failed.status.clone();
                expected.started_at = failed.started_at.clone();
                expected.completed_at = failed.completed_at.clone();
                expected.output = failed.output.clone();
                let start = crate::recovery_contracts::context_retry::request(
                    &retry,
                    crate::recovery_contracts::context_retry::kind_for(&retry, "start"),
                )?;
                expected == *failed
                    && attempt
                        .recovery_operations
                        .iter()
                        .any(|operation| operation.matches_request(&start))
            } else {
                false
            }
        } else {
            false
        };
    ensure!(
        area == &original_area || bound_retry_area || bound_completion_retry_area,
        "failed artifact area is not bound to this Task execution"
    );
    let mut files = BTreeMap::new();
    let transcript = bind_file(root, &format!("{area}/transcript.jsonl"), &mut files)?;
    let thread_id = crate::provider_evidence::codex_usage_limit_failure(&transcript)
        .context("unsupported final native Codex usage-limit evidence")?;
    let baseline = bind_file(root, &format!("{area}/write-baseline-commit"), &mut files)?;
    ensure!(
        std::str::from_utf8(&baseline)?.trim() == source.commit,
        "failed Writer baseline differs from source candidate"
    );
    bind_directory(root, area, &mut files)?;
    for task in &attempt.tasks {
        for input in &task.input_artifacts {
            bind_file(root, &input.path, &mut files)?;
        }
        if task.kind == TaskKind::Review {
            bind_directory(
                root,
                &task
                    .artifact_area
                    .as_ref()
                    .context("Review has no artifacts")?
                    .path,
                &mut files,
            )?;
        }
    }
    ensure!(
        partial_candidate(&workspace)? == partial,
        "partial candidate changed during inspection"
    );
    Ok(Binding {
        work_item_id: item.id.clone(),
        attempt_id: attempt_id.into(),
        failed_task: failed.clone(),
        prior_tasks: attempt.tasks[..attempt.tasks.len() - 1].to_vec(),
        source_candidate: source.commit.clone(),
        coder: attempt.coder_mapping.write.clone(),
        workspace: source.workspace_path.clone(),
        partial,
        files,
        thread_id,
        cancellation: attempt.cancellation.clone(),
        round_cap_extensions: attempt.round_cap_extensions.clone(),
    })
}

pub(crate) fn request(approval: &Approval, kind: &str) -> Result<RecoveryOperationRequest> {
    let binding = &approval.binding;
    Ok(RecoveryOperationRequest::new(
        kind,
        &binding.work_item_id,
        &binding.attempt_id,
        Some(binding.source_candidate.clone()),
        Some(binding.failed_task.id.clone()),
        None,
        Some(binding.workspace.clone()),
        Some(approval.resolution.clone()),
        Some(digest(&serde_json::to_vec(approval)?)),
    ))
}

pub(crate) fn audit_path(approval: &Approval) -> Result<String> {
    Ok(work_model::work_artifact_path(
        &approval.binding.work_item_id,
        &approval.binding.attempt_id,
        &format!(
            "usage-limit-recoveries/{}/approval.json",
            request(approval, APPROVE)?.operation_id()
        ),
    ))
}

pub(crate) fn replay_approval(
    root: &Path,
    item: &WorkItem,
    attempt_id: &str,
    resolution: &str,
) -> Result<Option<RecoveryApproval>> {
    let attempt = item
        .attempts
        .iter()
        .find(|a| a.id == attempt_id)
        .context("Attempt disappeared")?;
    let Some(approval) = current_approval(root, attempt)? else {
        return Ok(None);
    };
    let task = attempt
        .tasks
        .iter()
        .find(|t| t.id == approval.binding.failed_task.id)
        .context("approved Writer disappeared")?;
    ensure!(
        attempt.cancellation == approval.binding.cancellation,
        "a later stop invalidates the earlier approval"
    );
    let started = recorded(attempt, &approval, START)?;
    if started && task.status == TaskStatus::Failed {
        // A new terminal quota stream requires a separately inspected approval.
        ensure!(
            task.artifact_area.as_ref().map(|a| &a.path) == Some(&retry_area(&approval)?),
            "failed retry lost its execution binding"
        );
        return Ok(None);
    }
    ensure!(
        resolution == approval.resolution,
        "this interruption already has a different recorded approval"
    );
    if started {
        ensure!(
            matches!(task.status, TaskStatus::Executing | TaskStatus::Complete),
            "a later pause invalidates the earlier approval"
        );
        // The retry can change source and progress. Retained provider and Review
        // artifacts remain exact historical evidence, outside writable roots.
        for (path, expected) in &approval.binding.files {
            if Path::new(path).starts_with(".fluent/work/artifacts") {
                ensure!(
                    digest(&bind_file(root, path, &mut BTreeMap::new())?) == *expected,
                    "retained recovery evidence changed"
                );
            }
        }
    } else {
        validate_pending(root, item, &approval, false)?;
    }
    Ok(Some(RecoveryApproval {
        operation_id: request(&approval, APPROVE)?.operation_id(),
        task_id: approval.binding.failed_task.id.clone(),
        audit_artifact: audit_path(&approval)?,
        partial_commit: approval.binding.partial.head.clone(),
    }))
}

pub(crate) fn invalid(attempt: &str, error: impl std::fmt::Display) -> WorkModelError {
    WorkModelError::AttemptResumeIneligible {
        attempt_id: attempt.into(),
        reason: format!("usage-limit recovery: {error}"),
    }
}

pub(crate) fn current_approval(root: &Path, attempt: &Attempt) -> Result<Option<Approval>> {
    let Some(record) = attempt
        .recovery_operations
        .iter()
        .rev()
        .find(|r| r.operation_kind == APPROVE)
    else {
        return Ok(None);
    };
    Ok(Some(read_approval(root, attempt, record)?))
}

fn read_approval(
    root: &Path,
    attempt: &Attempt,
    record: &work_model::RecoveryOperationRecord,
) -> Result<Approval> {
    let path = work_model::work_artifact_path(
        &attempt.work_item_id,
        &attempt.id,
        &format!(
            "usage-limit-recoveries/{}/approval.json",
            record.operation_id
        ),
    );
    ensure!(
        attempt.artifacts.contains(&ArtifactRef {
            producer_id: APPROVE.into(),
            path: path.clone()
        }),
        "approval audit is not indexed"
    );
    let bytes = bind_file(root, &path, &mut BTreeMap::new())?;
    let approval: Approval = serde_json::from_slice(&bytes)?;
    ensure!(
        approval.schema_version == 1
            && request(&approval, APPROVE)?.operation_id() == record.operation_id
            && request(&approval, APPROVE)?.input_digest == record.input_digest
            && approval.binding.work_item_id == attempt.work_item_id
            && approval.binding.attempt_id == attempt.id,
        "approval does not match the recorded operation"
    );
    Ok(approval)
}

pub(crate) fn recorded(attempt: &Attempt, approval: &Approval, kind: &str) -> Result<bool> {
    Ok(attempt
        .recovery_operation(&request(approval, kind)?.operation_id())
        .is_some())
}

pub(crate) fn retry_area(approval: &Approval) -> Result<String> {
    Ok(work_model::work_artifact_path(
        &approval.binding.work_item_id,
        &approval.binding.attempt_id,
        &format!(
            "usage-limit-retries/{}",
            request(approval, APPROVE)?.operation_id()
        ),
    ))
}

pub(crate) fn prepared_task(approval: &Approval) -> Result<Task> {
    let mut task = approval.binding.failed_task.clone();
    task.status = TaskStatus::Planned;
    task.started_at = None;
    task.completed_at = None;
    task.artifact_area = Some(TaskArtifactArea {
        path: retry_area(approval)?,
    });
    task.input_artifacts.push(ArtifactRef {
        producer_id: APPROVE.into(),
        path: audit_path(approval)?,
    });
    task.instructions = Some(format!(
        "{}\n\nContinue the saved partial work after an approved provider usage-limit interruption. Preserve the original correction assignment. Earlier focused results remain historical; satisfy every remaining gate. Approval: {}. Decision: {}",
        task.instructions.as_deref().unwrap_or(""),
        audit_path(approval)?,
        approval.resolution
    ));
    Ok(task)
}

/// Remove only instruction additions bound to recorded quota preparations.
/// Completion recovery validates the remaining original instructions itself.
pub(crate) fn instructions_before_retries(
    root: &Path,
    attempt: &Attempt,
    task: &Task,
) -> Result<Option<String>> {
    let mut original = task.clone();
    for record in
        attempt.recovery_operations.iter().rev().filter(|r| {
            r.operation_kind == APPROVE && r.task_id.as_deref() == Some(task.id.as_str())
        })
    {
        let approval = read_approval(root, attempt, record)?;
        let prepared = prepared_task(&approval)?;
        if original.artifact_area != prepared.artifact_area {
            continue;
        }
        ensure!(
            recorded(attempt, &approval, PREPARE)?
                && original.id == prepared.id
                && original.instructions == prepared.instructions
                && original.input_artifacts == prepared.input_artifacts,
            "Writer instructions do not match the recorded quota preparation"
        );
        original = approval.binding.failed_task;
    }
    Ok(original.instructions)
}

/// Recognize the host's pre-reservation failure from append-only lifecycle
/// records. A pause label, handoff text, or missing execution file is not proof.
fn validate_preflight_pause(
    root: &Path,
    item: &WorkItem,
    attempt: &Attempt,
    approval: &Approval,
) -> Result<()> {
    use work_model::DurableTransitionOrigin::{External, TaskExecutor};

    ensure!(
        attempt.status == AttemptStatus::NeedsUser
            && attempt.pause_kind == Some(PauseKind::HostSandbox)
            && !recorded(attempt, approval, START)?
            && !has_launched(root, approval)?,
        "prepared retry is not an unlaunched host preflight pause"
    );
    let task = attempt
        .tasks
        .last()
        .context("prepared Writer disappeared")?;
    let mut expected = prepared_task(approval)?;
    expected.status = TaskStatus::NeedsUser;
    expected.completed_at = task.completed_at.clone();
    ensure!(
        task == &expected && task.completed_at.is_some(),
        "prepared Writer changed or stopped"
    );
    let mut history = item
        .transitions
        .iter()
        .rev()
        .filter(|entry| entry.object_kind == "task" && entry.object_id == task.id);
    let paused = history
        .next()
        .context("missing native preflight Task proof")?;
    let prepared = history
        .next()
        .context("missing native retry preparation proof")?;
    let preparation = attempt
        .recovery_operation(&request(approval, PREPARE)?.operation_id())
        .context("missing retry preparation operation")?;
    ensure!(
        paused.origin == TaskExecutor
            && paused.old_state.as_deref() == Some("planned")
            && paused.new_state == "needsuser"
            && prepared.origin == External
            && prepared.reason.as_deref() == Some("prepare approved usage-limit retry")
            && matches!(prepared.old_state.as_deref(), Some("failed" | "needsuser"))
            && prepared.new_state == "planned"
            && chrono::DateTime::parse_from_rfc3339(&prepared.occurred_at)?
                >= chrono::DateTime::parse_from_rfc3339(&preparation.completed_at)?,
        "native preflight Task proof conflicts with retry preparation"
    );
    let pause = item
        .transitions
        .iter()
        .rev()
        .find(|entry| entry.object_kind == "attempt" && entry.object_id == attempt.id)
        .context("missing native preflight Attempt proof")?;
    ensure!(
        pause.origin == TaskExecutor
            && matches!(pause.old_state.as_deref(), Some("planned" | "executing"))
            && pause.new_state == "needs-user"
            && pause.reason.as_deref() == Some("host-sandbox")
            && pause.sequence > prepared.sequence
            && pause.sequence.checked_add(1) == Some(paused.sequence)
            && pause.occurred_at == paused.occurred_at,
        "native preflight Attempt proof conflicts with the host pause"
    );
    let area = root.join(retry_area(approval)?);
    for name in [
        "execution.json",
        "transcript.jsonl",
        "coder-supervision.json",
    ] {
        match fs::symlink_metadata(area.join(name)) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).context("preflight execution ownership is unverifiable");
            }
            Ok(_) => bail!("preflight pause has conflicting execution evidence: {name}"),
        }
    }
    unlocked(&area.join("execution-owner.lock"), false)?;
    Ok(())
}

pub(crate) fn validate_pending(
    root: &Path,
    item: &WorkItem,
    approval: &Approval,
    own_task_lease: bool,
) -> Result<()> {
    let attempt = item
        .attempts
        .iter()
        .find(|a| a.id == approval.binding.attempt_id)
        .context("approved Attempt disappeared")?;
    ensure!(
        attempt.cancellation == approval.binding.cancellation,
        "a later stop invalidates the earlier approval"
    );
    if recorded(attempt, approval, START)? {
        unlaunched(root, attempt, approval, own_task_lease)?;
    }
    let mut original = item.clone();
    let restored = original
        .attempts
        .iter_mut()
        .find(|a| a.id == attempt.id)
        .unwrap();
    if recorded(attempt, approval, PREPARE)? {
        if attempt
            .tasks
            .last()
            .is_some_and(|task| task.status == TaskStatus::NeedsUser)
        {
            validate_preflight_pause(root, item, attempt, approval)?;
        } else {
            ensure!(
                attempt.tasks.last() == Some(&prepared_task(approval)?),
                "prepared Writer changed or stopped"
            );
            ensure!(
                matches!(
                    attempt.status,
                    AttemptStatus::Planned | AttemptStatus::Executing
                ),
                "prepared retry has a later pause or failure"
            );
        }
        if root
            .join(retry_area(approval)?)
            .join("execution.json")
            .try_exists()?
        {
            unlaunched(root, attempt, approval, own_task_lease)?;
        }
        *restored.tasks.last_mut().unwrap() = approval.binding.failed_task.clone();
        restored.status = AttemptStatus::NeedsUser;
        restored.pause_kind = Some(PauseKind::RoundCap);
    }
    // The caller already owns the task lease at reservation. Inspect all other
    // identities, including the retained failed execution, with the same rules.
    let binding = inspect_with_lease(root, &original, &attempt.id, own_task_lease)?;
    ensure!(
        binding == approval.binding,
        "approved evidence, assignment, settings, or partial candidate changed"
    );
    Ok(())
}

/// Recheck a prepared retry while constructing its prompt, possibly inside its
/// live Task lease. Admission and provider spawn enforce launch ownership; this
/// check binds the retained failure and approved partial work without restarting.
pub(crate) fn validated_prelaunch_approval(
    root: &Path,
    item: &WorkItem,
    task: &Task,
) -> Result<Option<Approval>> {
    let attempt = item
        .attempts
        .iter()
        .find(|a| Some(a.id.as_str()) == task.attempt_id.as_deref())
        .context("prepared Writer Attempt disappeared")?;
    let Some(approval) = current_approval(root, attempt)? else {
        return Ok(None);
    };
    if approval.binding.failed_task.id != task.id {
        return Ok(None);
    }
    ensure!(
        recorded(attempt, &approval, PREPARE)? && !has_launched(root, &approval)?,
        "quota retry lacks unused preparation authority"
    );
    let mut prepared = prepared_task(&approval)?;
    if task.status == TaskStatus::Executing {
        ensure!(
            recorded(attempt, &approval, START)? && task.started_at.is_some(),
            "executing quota retry lacks its reserved start authority"
        );
        prepared.status = TaskStatus::Executing;
        prepared.started_at = task.started_at.clone();
    }
    ensure!(
        task == &prepared
            && attempt.tasks.last() == Some(task)
            && matches!(
                attempt.status,
                AttemptStatus::Planned | AttemptStatus::Executing
            ),
        "prepared Writer changed or stopped"
    );
    let mut original = item.clone();
    let restored = original
        .attempts
        .iter_mut()
        .find(|a| a.id == attempt.id)
        .unwrap();
    *restored.tasks.last_mut().unwrap() = approval.binding.failed_task.clone();
    restored.status = AttemptStatus::NeedsUser;
    restored.pause_kind = Some(PauseKind::RoundCap);
    ensure!(
        inspect_with_lease(root, &original, &attempt.id, true)? == approval.binding,
        "approved evidence, assignment, settings, or partial candidate changed"
    );
    Ok(Some(approval))
}

/// Return true only for an unused, freshly validated approval. A used approval
/// permits ordinary downstream continuation after completion, never a restart.
pub(crate) fn resume_ready(root: &Path, item: &WorkItem, attempt_id: &str) -> Result<bool> {
    let attempt = item
        .attempts
        .iter()
        .find(|a| a.id == attempt_id)
        .context("Attempt disappeared")?;
    let Some(approval) = current_approval(root, attempt)? else {
        return Ok(false);
    };
    if recorded(attempt, &approval, START)? {
        let task = attempt
            .tasks
            .iter()
            .find(|t| t.id == approval.binding.failed_task.id)
            .context("recovered Writer disappeared")?;
        if task.status == TaskStatus::Executing && !has_launched(root, &approval)? {
            unlaunched(root, attempt, &approval, false)?;
            let mut prepared = item.clone();
            let restored = prepared
                .attempts
                .iter_mut()
                .find(|a| a.id == attempt_id)
                .unwrap();
            *restored.tasks.last_mut().unwrap() = prepared_task(&approval)?;
            restored.status = AttemptStatus::Planned;
            validate_pending(root, &prepared, &approval, false)?;
            return Ok(true);
        }
        if task.status == TaskStatus::Planned && !has_launched(root, &approval)? {
            validate_pending(root, item, &approval, false)?;
            return Ok(true);
        }
        ensure!(
            matches!(task.status, TaskStatus::Executing | TaskStatus::Complete),
            "earlier usage-limit approval cannot restart a stopped or failed retry"
        );
        if task.status == TaskStatus::Executing {
            ensure!(
                crate::execution::execution_is_live(
                    &root.join(retry_area(&approval)?).join("execution.json")
                )?,
                "retry launch ownership is no longer provable; earlier approval cannot restart it"
            );
        }
        return Ok(false);
    }
    validate_pending(root, item, &approval, false)?;
    Ok(true)
}

/// The direct Task boundary rechecks admission even when no Attempt loop ran.
pub(crate) fn validate_start(
    root: &Path,
    item: &WorkItem,
    attempt_id: &str,
    task_id: &str,
    own_task_lease: bool,
) -> Result<()> {
    let attempt = item
        .attempts
        .iter()
        .find(|a| a.id == attempt_id)
        .context("Attempt disappeared")?;
    let Some(approval) = current_approval(root, attempt)? else {
        return Ok(());
    };
    if approval.binding.failed_task.id != task_id {
        return Ok(());
    }
    ensure!(
        recorded(attempt, &approval, PREPARE)?,
        "explicit Attempt resume must prepare the approved retry first"
    );
    validate_pending(root, item, &approval, own_task_lease)?;
    ensure!(
        attempt
            .tasks
            .last()
            .is_some_and(|task| task.status == TaskStatus::Planned),
        "prepared Writer is paused; expected planned after explicit Attempt resume"
    );
    Ok(())
}

/// Revalidate pending authority before journal recovery publishes split records.
/// Started retries no longer freeze the candidate; their start receipt prevents
/// another launch even if publication acknowledgement was lost.
pub(crate) fn validate_publication(
    root: &Path,
    item: &WorkItem,
    operations: &[String],
) -> Result<()> {
    for attempt in &item.attempts {
        let Some(approval) = current_approval(root, attempt)? else {
            continue;
        };
        if !recorded(attempt, &approval, START)?
            && operations.iter().any(|id| {
                request(&approval, APPROVE).is_ok_and(|r| r.operation_id() == *id)
                    || request(&approval, PREPARE).is_ok_and(|r| r.operation_id() == *id)
            })
        {
            validate_pending(root, item, &approval, false)?;
        }
    }
    Ok(())
}

/// Include reused preparation authority when a paused Task reopens. Its journal
/// must recheck the approval even though the operation was already published.
pub(crate) fn pending_publication_ids(item: &WorkItem, prior: Option<&WorkItem>) -> Vec<String> {
    item.attempts
        .iter()
        .flat_map(|attempt| {
            let previous = prior.and_then(|item| item.attempts.iter().find(|a| a.id == attempt.id));
            let reopens_task = attempt.tasks.last().is_some_and(|task| {
                task.status == TaskStatus::Planned
                    && previous
                        .and_then(|a| a.tasks.last())
                        .is_some_and(|old| old.id == task.id && old.status == TaskStatus::NeedsUser)
            });
            attempt.recovery_operations.iter().filter(move |operation| {
                matches!(operation.operation_kind.as_str(), APPROVE | PREPARE)
                    && (previous
                        .is_none_or(|a| a.recovery_operation(&operation.operation_id).is_none())
                        || (operation.operation_kind == PREPARE && reopens_task))
            })
        })
        .map(|r| r.operation_id.clone())
        .collect()
}

pub(crate) fn test_interrupt(boundary: &str) {
    #[cfg(feature = "test-support")]
    if std::env::var("FLUENT_TEST_USAGE_LIMIT_INTERRUPT").as_deref() == Ok(boundary) {
        std::process::exit(86);
    }
    #[cfg(not(feature = "test-support"))]
    let _ = boundary;
}

pub(crate) fn test_interrupt_journal(item: &WorkItem) {
    if let Some(operation) = item
        .attempts
        .iter()
        .flat_map(|a| &a.recovery_operations)
        .last()
    {
        match operation.operation_kind.as_str() {
            APPROVE => test_interrupt("approval-journal"),
            PREPARE => test_interrupt("prepare-journal"),
            START => test_interrupt("start-journal"),
            _ => {}
        }
    }
}

pub(crate) fn launch_marker(approval: &Approval) -> Result<String> {
    Ok(format!(
        "{}/launch.json",
        Path::new(&audit_path(approval)?)
            .parent()
            .unwrap()
            .display()
    ))
}

pub(crate) fn has_launched(root: &Path, approval: &Approval) -> Result<bool> {
    let path = root.join(launch_marker(approval)?);
    match fs::symlink_metadata(&path) {
        Ok(_) => {
            ensure!(
                file_bytes(&path)? == request(approval, START)?.operation_id().as_bytes(),
                "launch marker changed"
            );
            Ok(true)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}

pub(crate) fn unlaunched(
    root: &Path,
    attempt: &Attempt,
    approval: &Approval,
    own_lease: bool,
) -> Result<()> {
    ensure!(
        !has_launched(root, approval)?,
        "this retry may already have launched"
    );
    ensure!(
        attempt.cancellation == approval.binding.cancellation,
        "a later stop invalidates setup recovery"
    );
    if !own_lease {
        unlocked(
            &crate::lease::task_lock_path(
                root,
                &attempt.work_item_id,
                &approval.binding.failed_task.id,
            ),
            true,
        )?;
    }
    let area = retry_area(approval)?;
    let record: crate::execution::ExecutionRecord = serde_json::from_slice(&bind_file(
        root,
        &format!("{area}/execution.json"),
        &mut BTreeMap::new(),
    )?)?;
    ensure!(
        matches!(record.schema_version, 1 | 2)
            && record.state == crate::execution::ExecutionState::Launching
            && record.leader_pid == 0
            && record.process_group_id == 0
            && record.settled_at.is_none()
            && (record.schema_version == 1
                || (record.launch_generation.is_some()
                    && crate::execution::receipt_absent(
                        &root.join(&area).join("execution.json")
                    )?)),
        "setup cannot be proven unlaunched"
    );
    let owner = record
        .attempt_owner
        .context("unlaunched setup lost its owner")?;
    ensure!(
        owner.project_root == root.canonicalize()?
            && owner.work_item_id == attempt.work_item_id
            && owner.attempt_id == attempt.id,
        "setup belongs to another Attempt"
    );
    ensure!(
        record.owner_pid > 0 && record.owner_pid <= i32::MAX as u32,
        "invalid setup owner"
    );
    let result = unsafe { libc::kill(record.owner_pid as i32, 0) };
    ensure!(
        result == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH),
        "setup owner is live or unverifiable"
    );
    unlocked(&root.join(area).join("execution-owner.lock"), false)?;
    Ok(())
}

pub(crate) fn status_action(root: &Path, attempt: &Attempt) -> Result<Option<Option<String>>> {
    if !attempt
        .recovery_operations
        .iter()
        .any(|r| r.operation_kind == APPROVE)
    {
        return Ok(None);
    }
    let item = WorkModelStore::new(root).read_work_item_snapshot(&attempt.work_item_id)?;
    if resume_ready(root, &item, &attempt.id)? {
        return Ok(Some(Some(format!(
            "fluent attempt run {} {}",
            attempt.work_item_id, attempt.id
        ))));
    }
    let approval = current_approval(root, attempt)?.context("approval disappeared")?;
    let task = attempt
        .tasks
        .iter()
        .find(|t| t.id == approval.binding.failed_task.id)
        .context("Writer disappeared")?;
    if task.status == TaskStatus::Complete {
        Ok(None)
    } else {
        Ok(Some(None))
    }
}

pub(crate) fn inspect_spawn(
    root: &Path,
    item: &WorkItem,
    attempt_id: &str,
    transcript: &Path,
) -> Result<Option<Approval>> {
    let attempt = item
        .attempts
        .iter()
        .find(|a| a.id == attempt_id)
        .context("Attempt disappeared")?;
    let Some(approval) = current_approval(root, attempt)? else {
        return Ok(None);
    };
    if transcript != root.join(retry_area(&approval)?).join("transcript.jsonl") {
        return Ok(None);
    }
    ensure!(
        recorded(attempt, &approval, START)?,
        "quota retry lacks its reserved start authority"
    );
    let task = attempt
        .tasks
        .iter()
        .find(|t| t.id == approval.binding.failed_task.id)
        .context("reserved Writer disappeared")?;
    ensure!(
        task.status == TaskStatus::Executing
            && attempt.cancellation == approval.binding.cancellation,
        "reserved retry changed or stopped"
    );
    if !has_launched(root, &approval)? {
        let mut original = item.clone();
        let restored = original
            .attempts
            .iter_mut()
            .find(|a| a.id == attempt_id)
            .unwrap();
        *restored
            .tasks
            .last_mut()
            .context("reserved Writer disappeared")? = approval.binding.failed_task.clone();
        restored.status = AttemptStatus::NeedsUser;
        restored.pause_kind = Some(PauseKind::RoundCap);
        ensure!(
            inspect_with_lease(root, &original, attempt_id, true)? == approval.binding,
            "approved partial candidate changed before provider launch"
        );
        return Ok(Some(approval));
    }
    Ok(None)
}
