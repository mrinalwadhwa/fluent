use anyhow::{Result, bail};
use std::fs;
use std::io::ErrorKind;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use crate::coder::{CoderKind, CoderSandbox};
use crate::content::ContentResolver;
use crate::credential;
use crate::os;
use crate::work_model::{
    ArtifactRef, AttemptStatus, TaskKind, TaskOutput, TaskStatus, WorkItem, WorkModelStorageError,
    WorkModelStore, to_json_pretty,
};
use crate::worktree;

pub struct WorkTaskRunConfig<'a> {
    pub project_root: &'a Path,
    pub store: &'a WorkModelStore,
    pub work_item_id: &'a str,
    pub attempt_id: &'a str,
    pub task_id: &'a str,
    pub resolver: &'a ContentResolver,
    pub extra_args: &'a [String],
    pub coder_kind: CoderKind,
    pub no_sandbox: bool,
}

pub struct WorkTaskRunResult {
    pub task_id: String,
    pub output: String,
}

pub fn run_task(config: WorkTaskRunConfig<'_>) -> Result<WorkTaskRunResult> {
    let item = read_work_item_or_not_found(config.store, config.work_item_id)?;
    let (attempt_index, task_index) =
        find_attempt_task_indexes(&item, config.attempt_id, config.task_id)
            .ok_or_else(|| anyhow::anyhow!("Task {:?} not found", config.task_id))?;

    match item.attempts[attempt_index].tasks[task_index].kind {
        TaskKind::Write => run_write_task(config),
        TaskKind::Review => run_review_task(config),
        kind => bail!(
            "Task {:?} is kind {kind}; unsupported by task run",
            config.task_id
        ),
    }
}

fn run_write_task(config: WorkTaskRunConfig<'_>) -> Result<WorkTaskRunResult> {
    let mut item = read_work_item_or_not_found(config.store, config.work_item_id)?;
    let attempt_index = item
        .attempts
        .iter()
        .position(|attempt| attempt.id == config.attempt_id)
        .ok_or_else(|| anyhow::anyhow!("Attempt {:?} not found", config.attempt_id))?;
    let task_index = item.attempts[attempt_index]
        .tasks
        .iter()
        .position(|task| task.id == config.task_id)
        .ok_or_else(|| anyhow::anyhow!("Task {:?} not found", config.task_id))?;

    let task = &item.attempts[attempt_index].tasks[task_index];
    if task.kind != TaskKind::Write {
        bail!(
            "Task {:?} is kind {}; expected write",
            config.task_id,
            task.kind
        );
    }
    if task.status != TaskStatus::Planned {
        bail!(
            "Task {:?} is {}; expected planned",
            config.task_id,
            task.status
        );
    }
    if task.workspace_access.writes.len() != 1 {
        bail!(
            "Task {:?} must declare exactly one writable workspace; found {}",
            config.task_id,
            task.workspace_access.writes.len()
        );
    }

    let workspace = task.workspace_access.writes[0].clone();
    let workspace_path = resolve_managed_workspace_path(config.project_root, &workspace.path)?;
    let input_artifacts = resolve_input_artifact_paths(config.project_root, &task.input_artifacts)?;
    let source_branch = current_branch(config.project_root)?;
    let branch_name = format!(
        "work/{}/{}/{}",
        config.work_item_id, config.attempt_id, config.task_id
    );

    prepare_task_worktree(
        config.project_root,
        &workspace_path,
        &branch_name,
        &source_branch,
    )?;
    worktree::disable_commit_signing(&workspace_path)?;
    let baseline_commit = head_commit(&workspace_path)?;

    item.attempts[attempt_index].status = AttemptStatus::Executing;
    item.attempts[attempt_index].tasks[task_index].status = TaskStatus::Executing;
    item.attempts[attempt_index].tasks[task_index].output = None;
    config.store.write_work_item(&item)?;

    let run_result = run_task_coder(
        &item,
        config.attempt_id,
        config.task_id,
        &workspace_path,
        &input_artifacts,
        config.resolver,
        config.extra_args,
        config.coder_kind,
        config.no_sandbox,
    );

    if let Err(error) = run_result {
        let mut failed_item = read_work_item_or_not_found(config.store, config.work_item_id)?;
        if let Some((attempt_index, task_index)) =
            find_attempt_task_indexes(&failed_item, config.attempt_id, config.task_id)
        {
            failed_item.attempts[attempt_index].status = AttemptStatus::Failed;
            failed_item.attempts[attempt_index].tasks[task_index].status = TaskStatus::Failed;
            config.store.write_work_item(&failed_item)?;
        }
        return Err(error);
    }

    if let Err(error) = ensure_clean_worktree(&workspace_path) {
        mark_task_failed(
            config.store,
            config.work_item_id,
            config.attempt_id,
            config.task_id,
        )?;
        return Err(error);
    }
    let produced_count = match commits_ahead(&workspace_path, &baseline_commit) {
        Ok(count) => count,
        Err(error) => {
            mark_task_failed(
                config.store,
                config.work_item_id,
                config.attempt_id,
                config.task_id,
            )?;
            return Err(error);
        }
    };
    if produced_count == 0 {
        mark_task_failed(
            config.store,
            config.work_item_id,
            config.attempt_id,
            config.task_id,
        )?;
        bail!(
            "Task {:?} has no committed Task output; commit the Task output before completing",
            config.task_id
        );
    }
    let commit = head_commit(&workspace_path)?;

    let output = TaskOutput {
        workspace_id: workspace.id.clone(),
        workspace_path: workspace.path.clone(),
        source_branch,
        commit: commit.clone(),
    };
    let mut completed_item = read_work_item_or_not_found(config.store, config.work_item_id)?;
    let (attempt_index, task_index) =
        find_attempt_task_indexes(&completed_item, config.attempt_id, config.task_id)
            .ok_or_else(|| anyhow::anyhow!("Task {:?} not found", config.task_id))?;
    completed_item.attempts[attempt_index].tasks[task_index].status = TaskStatus::Complete;
    completed_item.attempts[attempt_index].tasks[task_index].output = Some(output);
    completed_item.attempts[attempt_index]
        .artifacts
        .push(ArtifactRef {
            producer_id: config.task_id.to_string(),
            path: commit.clone(),
        });
    completed_item.attempts[attempt_index].status = if completed_item.attempts[attempt_index]
        .tasks
        .iter()
        .all(|task| task.status == TaskStatus::Complete)
    {
        AttemptStatus::Complete
    } else {
        AttemptStatus::Executing
    };
    config.store.write_work_item(&completed_item)?;

    Ok(WorkTaskRunResult {
        task_id: config.task_id.to_string(),
        output: commit,
    })
}

fn run_review_task(config: WorkTaskRunConfig<'_>) -> Result<WorkTaskRunResult> {
    let mut item = read_work_item_or_not_found(config.store, config.work_item_id)?;
    let (attempt_index, task_index) =
        find_attempt_task_indexes(&item, config.attempt_id, config.task_id)
            .ok_or_else(|| anyhow::anyhow!("Task {:?} not found", config.task_id))?;

    let task = &item.attempts[attempt_index].tasks[task_index];
    if task.status != TaskStatus::Planned {
        bail!(
            "Task {:?} is {}; expected planned",
            config.task_id,
            task.status
        );
    }
    if !task.workspace_access.writes.is_empty() {
        bail!("Review Task {:?} cannot write a workspace", config.task_id);
    }
    if task.workspace_access.reads.is_empty() {
        bail!(
            "Review Task {:?} must declare at least one readable candidate workspace",
            config.task_id
        );
    }
    let artifact_area = task
        .artifact_area
        .as_ref()
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Review Task {:?} must declare an artifact area",
                config.task_id
            )
        })?
        .path
        .clone();
    let review_context = task.review_context.as_ref().ok_or_else(|| {
        anyhow::anyhow!(
            "Review Task {:?} must declare review context",
            config.task_id
        )
    })?;
    let artifact_dir = resolve_managed_artifact_area_path(config.project_root, &artifact_area)?;
    let review_path = artifact_dir.join("review.md");

    let readable_workspaces = task
        .workspace_access
        .reads
        .iter()
        .map(|workspace| {
            resolve_managed_readable_workspace_path(config.project_root, &workspace.path)
        })
        .collect::<Result<Vec<_>>>()?;
    if !task.workspace_access.reads.iter().any(|workspace| {
        workspace.id == review_context.candidate_workspace_id
            && workspace.path == review_context.candidate_workspace_path
    }) {
        bail!(
            "Review Task {:?} review context candidate must match a readable workspace",
            config.task_id
        );
    }
    let mut candidate_heads = Vec::new();
    for workspace_path in &readable_workspaces {
        ensure_same_git_repository(config.project_root, workspace_path)?;
        ensure_registered_worktree(config.project_root, workspace_path)?;
        ensure_clean_worktree(workspace_path)?;
        candidate_heads.push((workspace_path.clone(), head_commit(workspace_path)?));
    }
    fs::create_dir_all(&artifact_dir)?;
    if review_path.is_file() {
        fs::remove_file(&review_path)?;
    } else if review_path.exists() {
        bail!(
            "Review Task {:?} artifact path exists but is not a file: {}",
            config.task_id,
            review_path.display()
        );
    }

    item.attempts[attempt_index].status = AttemptStatus::Reviewing;
    item.attempts[attempt_index].tasks[task_index].status = TaskStatus::Executing;
    item.attempts[attempt_index].tasks[task_index].output = None;
    config.store.write_work_item(&item)?;

    let run_result = run_review_coder(
        &item,
        config.attempt_id,
        config.task_id,
        &artifact_dir,
        &review_path,
        &readable_workspaces,
        config.resolver,
        config.extra_args,
        config.coder_kind,
        config.no_sandbox,
    );

    for (workspace_path, baseline_head) in &candidate_heads {
        if let Err(error) = ensure_head_unchanged(workspace_path, baseline_head) {
            mark_task_failed(
                config.store,
                config.work_item_id,
                config.attempt_id,
                config.task_id,
            )?;
            return Err(error);
        }
    }
    for workspace_path in &readable_workspaces {
        if let Err(error) = ensure_clean_worktree(workspace_path) {
            mark_task_failed(
                config.store,
                config.work_item_id,
                config.attempt_id,
                config.task_id,
            )?;
            return Err(error);
        }
    }

    if let Err(error) = run_result {
        mark_task_failed(
            config.store,
            config.work_item_id,
            config.attempt_id,
            config.task_id,
        )?;
        return Err(error);
    }

    if !review_path.is_file() {
        mark_task_failed(
            config.store,
            config.work_item_id,
            config.attempt_id,
            config.task_id,
        )?;
        bail!(
            "Review Task {:?} completed without writing {}",
            config.task_id,
            review_path.display()
        );
    }

    let mut completed_item = read_work_item_or_not_found(config.store, config.work_item_id)?;
    let (attempt_index, task_index) =
        find_attempt_task_indexes(&completed_item, config.attempt_id, config.task_id)
            .ok_or_else(|| anyhow::anyhow!("Task {:?} not found", config.task_id))?;
    completed_item.attempts[attempt_index].tasks[task_index].status = TaskStatus::Complete;
    completed_item.attempts[attempt_index]
        .artifacts
        .push(ArtifactRef {
            producer_id: config.task_id.to_string(),
            path: path_for_model(config.project_root, &review_path),
        });
    completed_item.attempts[attempt_index].status = if completed_item.attempts[attempt_index]
        .tasks
        .iter()
        .all(|task| task.status == TaskStatus::Complete)
    {
        AttemptStatus::Complete
    } else {
        AttemptStatus::Reviewing
    };
    config.store.write_work_item(&completed_item)?;

    Ok(WorkTaskRunResult {
        task_id: config.task_id.to_string(),
        output: path_for_model(config.project_root, &review_path),
    })
}

fn read_work_item_or_not_found(store: &WorkModelStore, id: &str) -> Result<WorkItem> {
    match store.read_work_item(id) {
        Ok(item) => Ok(item),
        Err(WorkModelStorageError::ReadFile { source, .. })
            if source.kind() == ErrorKind::NotFound =>
        {
            bail!("Work Item {id:?} not found")
        }
        Err(error) => Err(error.into()),
    }
}

fn find_attempt_task_indexes(
    item: &WorkItem,
    attempt_id: &str,
    task_id: &str,
) -> Option<(usize, usize)> {
    let attempt_index = item
        .attempts
        .iter()
        .position(|attempt| attempt.id == attempt_id)?;
    let task_index = item.attempts[attempt_index]
        .tasks
        .iter()
        .position(|task| task.id == task_id)?;
    Some((attempt_index, task_index))
}

fn mark_task_failed(
    store: &WorkModelStore,
    work_item_id: &str,
    attempt_id: &str,
    task_id: &str,
) -> Result<()> {
    let mut item = read_work_item_or_not_found(store, work_item_id)?;
    if let Some((attempt_index, task_index)) = find_attempt_task_indexes(&item, attempt_id, task_id)
    {
        item.attempts[attempt_index].status = AttemptStatus::Failed;
        item.attempts[attempt_index].tasks[task_index].status = TaskStatus::Failed;
        store.write_work_item(&item)?;
    }
    Ok(())
}

fn resolve_workspace_path(project_root: &Path, path: &str) -> PathBuf {
    let path = PathBuf::from(path);
    if path.is_absolute() {
        path
    } else {
        project_root.join(path)
    }
}

fn resolve_managed_workspace_path(project_root: &Path, path: &str) -> Result<PathBuf> {
    resolve_managed_task_workspace_path(project_root, path, "writable")
}

fn resolve_managed_readable_workspace_path(project_root: &Path, path: &str) -> Result<PathBuf> {
    resolve_managed_task_workspace_path(project_root, path, "readable")
}

fn resolve_managed_task_workspace_path(
    project_root: &Path,
    path: &str,
    access_kind: &str,
) -> Result<PathBuf> {
    let relative_path = Path::new(path);
    if relative_path.is_absolute() {
        bail!("Task {access_kind} workspace path must be relative: {path}");
    }

    let mut components = Vec::new();
    for component in relative_path.components() {
        match component {
            Component::Normal(part) => components.push(part.to_owned()),
            _ => bail!(
                "Task {access_kind} workspace path must stay under .factory/work/workspaces: {path}"
            ),
        }
    }

    let managed_prefix = [
        std::ffi::OsStr::new(".factory"),
        std::ffi::OsStr::new("work"),
        std::ffi::OsStr::new("workspaces"),
    ];
    if components.len() <= managed_prefix.len()
        || !components
            .iter()
            .zip(managed_prefix.iter())
            .all(|(actual, expected)| actual == expected)
    {
        bail!("Task {access_kind} workspace path must stay under .factory/work/workspaces: {path}");
    }

    Ok(resolve_workspace_path(project_root, path))
}

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
            _ => bail!("Task artifact area path must stay under .factory/work/artifacts: {path}"),
        }
    }

    let managed_prefix = [
        std::ffi::OsStr::new(".factory"),
        std::ffi::OsStr::new("work"),
        std::ffi::OsStr::new("artifacts"),
    ];
    if components.len() <= managed_prefix.len()
        || !components
            .iter()
            .zip(managed_prefix.iter())
            .all(|(actual, expected)| actual == expected)
    {
        bail!("Task artifact area path must stay under .factory/work/artifacts: {path}");
    }

    Ok(resolve_workspace_path(project_root, path))
}

fn resolve_input_artifact_paths(
    project_root: &Path,
    input_artifacts: &[ArtifactRef],
) -> Result<Vec<PathBuf>> {
    let mut resolved = Vec::new();
    for artifact in input_artifacts {
        let path = resolve_managed_artifact_area_path(project_root, &artifact.path)?;
        if !path.is_file() {
            bail!(
                "Input artifact from Task {} does not exist or is not a file: {}",
                artifact.producer_id,
                path.display()
            );
        }
        resolved.push(path);
    }
    Ok(resolved)
}

fn prepare_task_worktree(
    project_root: &Path,
    workspace_path: &Path,
    branch_name: &str,
    source_ref: &str,
) -> Result<()> {
    if workspace_path.exists() {
        if !workspace_path.is_dir() {
            bail!(
                "Workspace path exists but is not a directory: {}",
                workspace_path.display()
            );
        }
        ensure_same_git_repository(project_root, workspace_path)?;
        ensure_registered_worktree(project_root, workspace_path)?;
        return Ok(());
    }

    if let Some(parent) = workspace_path.parent() {
        fs::create_dir_all(parent)?;
    }

    if git_branch_exists(project_root, branch_name)? {
        bail!(
            "Task branch {branch_name:?} already exists but workspace {} is missing; remove or rebind the branch before running the Task",
            workspace_path.display()
        );
    }

    let output = Command::new("git")
        .args(["-C", &project_root.to_string_lossy()])
        .args([
            "worktree",
            "add",
            "-b",
            branch_name,
            &workspace_path.to_string_lossy(),
            source_ref,
        ])
        .output()?;

    if output.status.success() {
        Ok(())
    } else {
        bail!(
            "Failed to create task worktree: {}",
            String::from_utf8_lossy(&output.stderr)
        )
    }
}

fn git_branch_exists(project_root: &Path, branch_name: &str) -> Result<bool> {
    let output = Command::new("git")
        .args(["-C", &project_root.to_string_lossy()])
        .args([
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch_name}"),
        ])
        .output()?;
    match output.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => bail!(
            "Failed to check task branch {branch_name:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    }
}

fn ensure_same_git_repository(project_root: &Path, workspace_path: &Path) -> Result<()> {
    let source_common = fs::canonicalize(worktree::git_common_dir(project_root)?)?;
    let workspace_common = fs::canonicalize(worktree::git_common_dir(workspace_path)?)?;
    if source_common != workspace_common {
        bail!(
            "Workspace {} belongs to a different git repository",
            workspace_path.display()
        );
    }
    Ok(())
}

fn ensure_registered_worktree(project_root: &Path, workspace_path: &Path) -> Result<()> {
    let expected = fs::canonicalize(workspace_path)?;
    let output = Command::new("git")
        .args(["-C", &project_root.to_string_lossy()])
        .args(["worktree", "list", "--porcelain"])
        .output()?;
    if !output.status.success() {
        bail!(
            "Failed to list git worktrees: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let Some(path) = line.strip_prefix("worktree ") else {
            continue;
        };
        if fs::canonicalize(path).is_ok_and(|actual| actual == expected) {
            return Ok(());
        }
    }

    bail!(
        "Workspace {} exists but is not a registered git worktree",
        workspace_path.display()
    )
}

fn run_task_coder(
    item: &WorkItem,
    attempt_id: &str,
    task_id: &str,
    workspace_path: &Path,
    input_artifacts: &[PathBuf],
    resolver: &ContentResolver,
    extra_args: &[String],
    coder_kind: CoderKind,
    no_sandbox: bool,
) -> Result<()> {
    if !no_sandbox {
        os::check_prerequisites_for(coder_kind)?;
        credential::inject_credentials()?;
        credential::setup_git_signing();
    }

    let task = item
        .attempts
        .iter()
        .find(|attempt| attempt.id == attempt_id)
        .and_then(|attempt| attempt.tasks.iter().find(|task| task.id == task_id))
        .ok_or_else(|| anyhow::anyhow!("Task {task_id:?} not found"))?;
    let task_json = to_json_pretty(task)?;
    let input_artifacts_prompt = input_artifacts_instruction(input_artifacts);
    let task_instructions = task_instructions_prompt(task.instructions.as_deref());
    let prompt = format!(
        "Execute this Factory write Task.\n\nWork Item: {} - {}\nAttempt: {}\nTask: {}\nRole: {}\n\n{}Input artifacts:\n{}\n\nCurrent Task model:\n{}\n",
        item.id,
        item.title,
        attempt_id,
        task_id,
        task.role,
        task_instructions,
        input_artifacts_prompt,
        task_json
    );

    let workspace_resolver = ContentResolver::new(Some(workspace_path));
    let system_prompt = workspace_resolver
        .resolve_content("prompts/author.md")
        .unwrap_or_default();
    let (sandbox, _sandbox_profile) = if no_sandbox {
        (CoderSandbox::None, None)
    } else {
        let common_git_dir = worktree::git_common_dir(workspace_path)?;
        let readable_roots = input_artifact_readable_roots(input_artifacts);
        build_coder_sandbox_with_writable_and_read_only_roots(
            coder_kind,
            resolver,
            workspace_path,
            &[common_git_dir],
            &readable_roots,
        )?
    };

    eprintln!("  Factory           work task run");
    eprintln!("  Work Item         {}", item.id);
    eprintln!("  Attempt           {attempt_id}");
    eprintln!("  Task              {task_id}");
    eprintln!("  Worktree          {}", workspace_path.display());

    let coder = coder_kind.boxed(sandbox);
    let exit_code = coder.run(&prompt, &system_prompt, workspace_path, extra_args, None)?;
    if exit_code == 0 {
        Ok(())
    } else {
        bail!("Coder exited with code {exit_code}")
    }
}

fn input_artifacts_instruction(input_artifacts: &[PathBuf]) -> String {
    if input_artifacts.is_empty() {
        return "None.".to_string();
    }

    input_artifacts
        .iter()
        .map(|path| format!("- {}", path.display()))
        .collect::<Vec<_>>()
        .join("\n")
}

fn task_instructions_prompt(instructions: Option<&str>) -> String {
    match instructions.filter(|instructions| !instructions.trim().is_empty()) {
        Some(instructions) => format!("Task instructions:\n{instructions}\n\n"),
        None => String::new(),
    }
}

fn input_artifact_readable_roots(input_artifacts: &[PathBuf]) -> Vec<PathBuf> {
    input_artifacts
        .iter()
        .filter_map(|path| path.parent().map(Path::to_path_buf))
        .collect()
}

fn run_review_coder(
    item: &WorkItem,
    attempt_id: &str,
    task_id: &str,
    artifact_dir: &Path,
    review_path: &Path,
    readable_workspaces: &[PathBuf],
    resolver: &ContentResolver,
    extra_args: &[String],
    coder_kind: CoderKind,
    no_sandbox: bool,
) -> Result<()> {
    if !no_sandbox {
        os::check_prerequisites_for(coder_kind)?;
        credential::inject_credentials()?;
        credential::setup_git_signing();
    }

    let task = item
        .attempts
        .iter()
        .find(|attempt| attempt.id == attempt_id)
        .and_then(|attempt| attempt.tasks.iter().find(|task| task.id == task_id))
        .ok_or_else(|| anyhow::anyhow!("Task {task_id:?} not found"))?;
    let task_json = to_json_pretty(task)?;
    let review_context = task
        .review_context
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Review Task {task_id:?} must declare review context"))?;
    let review_skill_instruction = review_skill_instruction(&task.role, readable_workspaces);
    let decisions_instruction = decisions_instruction(readable_workspaces);
    let read_paths = task
        .workspace_access
        .reads
        .iter()
        .zip(readable_workspaces.iter())
        .map(|(workspace, resolved_path)| {
            format!("- {}: {}", workspace.id, resolved_path.display())
        })
        .collect::<Vec<_>>()
        .join("\n");
    let prompt = format!(
        "Execute this Factory review Task.\n\nWork Item: {} - {}\nAttempt: {}\nTask: {}\nRole: {}\n\nReadable candidate workspaces:\n{}\n\nReview context:\n- Candidate workspace: {} ({})\n- Source branch: {}\n- Candidate commit: {}\n- Review diff: git -C <candidate-workspace> diff {}..{}\n\nWrite the review artifact to exactly this path:\n{}\n\nThe Task completes when that artifact exists. The artifact may contain Verdict: pass, Verdict: fail, or Verdict: uncertain; do not edit candidate workspaces.\n\nCurrent Task model:\n{}\n",
        item.id,
        item.title,
        attempt_id,
        task_id,
        task.role,
        read_paths,
        review_context.candidate_workspace_id,
        review_context.candidate_workspace_path,
        review_context.source_branch,
        review_context.candidate_commit,
        review_context.source_branch,
        review_context.candidate_commit,
        review_path.display(),
        task_json
    );

    let system_prompt = format!(
        "You are a Factory {} reviewer operating as a Work model review Task.\n{}\nRead candidate workspaces only; do not edit or commit in them.\nWrite the review artifact only to {} with a verdict (pass, fail, or uncertain) and findings.\n{} Do not flag findings that contradict a recorded decision.",
        task.role,
        review_skill_instruction,
        review_path.display(),
        decisions_instruction
    );
    let (sandbox, _sandbox_profile) = if no_sandbox {
        (CoderSandbox::None, None)
    } else {
        let readable_roots = review_readable_sandbox_roots(readable_workspaces)?;
        build_coder_sandbox_with_read_only_roots(
            coder_kind,
            resolver,
            artifact_dir,
            &readable_roots,
        )?
    };

    eprintln!("  Factory           work task run");
    eprintln!("  Work Item         {}", item.id);
    eprintln!("  Attempt           {attempt_id}");
    eprintln!("  Task              {task_id}");
    eprintln!("  Artifact area     {}", artifact_dir.display());

    let coder = coder_kind.boxed(sandbox);
    let exit_code = coder.run(&prompt, &system_prompt, artifact_dir, extra_args, None)?;
    if exit_code == 0 {
        Ok(())
    } else {
        bail!("Coder exited with code {exit_code}")
    }
}

fn review_readable_sandbox_roots(readable_workspaces: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut roots = Vec::new();
    for workspace in readable_workspaces {
        roots.push(workspace.clone());
        let common_git_dir = worktree::git_common_dir(workspace)?;
        if !roots.iter().any(|root| root == &common_git_dir) {
            roots.push(common_git_dir);
        }
    }
    Ok(roots)
}

fn review_skill_instruction(role: &str, readable_workspaces: &[PathBuf]) -> String {
    let relative = format!("skills/review-{role}/SKILL.md");
    readable_workspaces
        .iter()
        .map(|workspace| workspace.join(&relative))
        .find(|path| path.is_file())
        .map(|path| format!("Follow the review-{role} skill at {}.", path.display()))
        .unwrap_or_else(|| {
            format!(
                "No review-{role} skill file was found in the readable candidate workspaces; apply the Task role directly."
            )
        })
}

fn decisions_instruction(readable_workspaces: &[PathBuf]) -> String {
    let relative = Path::new(".factory/expertise/decisions.md");
    readable_workspaces
        .iter()
        .map(|workspace| workspace.join(relative))
        .find(|path| path.is_file())
        .map(|path| {
            format!(
                "Read recorded decisions at {} if it exists.",
                path.display()
            )
        })
        .unwrap_or_else(|| {
            "No project decision file was found in the readable candidate workspaces.".to_string()
        })
}

fn build_coder_sandbox_with_writable_and_read_only_roots(
    coder_kind: CoderKind,
    resolver: &ContentResolver,
    working_dir: &Path,
    additional_writable_roots: &[PathBuf],
    readable_roots: &[PathBuf],
) -> Result<(CoderSandbox, Option<os::SandboxProfile>)> {
    let home = std::env::var("HOME").unwrap_or_default();
    let mut roots = vec![working_dir.to_path_buf()];
    roots.extend(additional_writable_roots.iter().cloned());
    let profile = os::render_profile_for_access_for_coder(
        resolver,
        &home,
        &roots,
        readable_roots,
        coder_kind,
    )?;
    let sandbox = CoderSandbox::SeatbeltProfile(profile.path.to_string_lossy().to_string());
    Ok((sandbox, Some(profile)))
}

fn build_coder_sandbox_with_read_only_roots(
    coder_kind: CoderKind,
    resolver: &ContentResolver,
    working_dir: &Path,
    readable_roots: &[PathBuf],
) -> Result<(CoderSandbox, Option<os::SandboxProfile>)> {
    let home = std::env::var("HOME").unwrap_or_default();
    let writable_roots = vec![working_dir.to_path_buf()];
    let profile = os::render_profile_for_access_for_coder(
        resolver,
        &home,
        &writable_roots,
        readable_roots,
        coder_kind,
    )?;
    let sandbox = CoderSandbox::SeatbeltProfile(profile.path.to_string_lossy().to_string());
    Ok((sandbox, Some(profile)))
}

fn ensure_clean_worktree(workspace_path: &Path) -> Result<()> {
    let output = Command::new("git")
        .args(["-C", &workspace_path.to_string_lossy()])
        .args(["status", "--porcelain"])
        .output()?;
    if !output.status.success() {
        bail!(
            "Failed to read task workspace status: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    if output.stdout.is_empty() {
        Ok(())
    } else {
        bail!(
            "Task workspace has uncommitted changes; commit or remove them before completing:\n{}",
            String::from_utf8_lossy(&output.stdout)
        )
    }
}

fn ensure_head_unchanged(workspace_path: &Path, baseline_head: &str) -> Result<()> {
    let current_head = head_commit(workspace_path)?;
    if current_head == baseline_head {
        Ok(())
    } else {
        reset_worktree_head(workspace_path, baseline_head)?;
        bail!(
            "Review Task changed readable candidate workspace HEAD from {baseline_head} to {current_head}: {}",
            workspace_path.display()
        )
    }
}

fn reset_worktree_head(workspace_path: &Path, target: &str) -> Result<()> {
    let output = Command::new("git")
        .args(["-C", &workspace_path.to_string_lossy()])
        .args(["reset", "--hard", target])
        .output()?;
    if output.status.success() {
        Ok(())
    } else {
        bail!(
            "Failed to restore readable candidate workspace HEAD: {}",
            String::from_utf8_lossy(&output.stderr)
        )
    }
}

fn path_for_model(project_root: &Path, path: &Path) -> String {
    path.strip_prefix(project_root)
        .unwrap_or(path)
        .to_string_lossy()
        .to_string()
}

fn commits_ahead(workspace_path: &Path, source_ref: &str) -> Result<u32> {
    let output = Command::new("git")
        .args(["-C", &workspace_path.to_string_lossy()])
        .args(["rev-list", "--count", &format!("{source_ref}..HEAD")])
        .output()?;
    if !output.status.success() {
        bail!(
            "Failed to compare task workspace with {source_ref}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().parse()?)
}

fn head_commit(workspace_path: &Path) -> Result<String> {
    let output = Command::new("git")
        .args(["-C", &workspace_path.to_string_lossy()])
        .args(["rev-parse", "HEAD"])
        .output()?;
    if !output.status.success() {
        bail!(
            "Failed to resolve task workspace HEAD: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn current_branch(project_root: &Path) -> Result<String> {
    let output = Command::new("git")
        .args(["-C", &project_root.to_string_lossy()])
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()?;
    if !output.status.success() {
        bail!(
            "Failed to resolve source branch: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if branch != "HEAD" {
        return Ok(branch);
    }

    let output = Command::new("git")
        .args(["-C", &project_root.to_string_lossy()])
        .args(["rev-parse", "HEAD"])
        .output()?;
    if !output.status.success() {
        bail!(
            "Failed to resolve source commit: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}
