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
    pub commit: String,
}

pub fn run_write_task(config: WorkTaskRunConfig<'_>) -> Result<WorkTaskRunResult> {
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
        commit,
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
    let relative_path = Path::new(path);
    if relative_path.is_absolute() {
        bail!("Task writable workspace path must be relative: {path}");
    }

    let mut components = Vec::new();
    for component in relative_path.components() {
        match component {
            Component::Normal(part) => components.push(part.to_owned()),
            _ => bail!(
                "Task writable workspace path must stay under .factory/work/workspaces: {path}"
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
        bail!("Task writable workspace path must stay under .factory/work/workspaces: {path}");
    }

    Ok(resolve_workspace_path(project_root, path))
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
    let prompt = format!(
        "Execute this Factory write Task.\n\nWork Item: {} - {}\nAttempt: {}\nTask: {}\nRole: {}\n\nCurrent Task model:\n{}\n",
        item.id, item.title, attempt_id, task_id, task.role, task_json
    );

    let workspace_resolver = ContentResolver::new(Some(workspace_path));
    let system_prompt = workspace_resolver
        .resolve_content("prompts/author.md")
        .unwrap_or_default();
    let (sandbox, _sandbox_profile) = if no_sandbox {
        (CoderSandbox::None, None)
    } else {
        let common_git_dir = worktree::git_common_dir(workspace_path)?;
        build_coder_sandbox(coder_kind, resolver, workspace_path, &[common_git_dir])?
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

fn build_coder_sandbox(
    coder_kind: CoderKind,
    resolver: &ContentResolver,
    working_dir: &Path,
    additional_writable_roots: &[PathBuf],
) -> Result<(CoderSandbox, Option<os::SandboxProfile>)> {
    let home = std::env::var("HOME").unwrap_or_default();
    let mut roots = vec![working_dir.to_path_buf()];
    roots.extend(additional_writable_roots.iter().cloned());
    let profile = os::render_profile_for_roots_for_coder(resolver, &home, &roots, coder_kind)?;
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
