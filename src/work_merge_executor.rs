use anyhow::{Context, Result, bail};
use std::fs;
use std::io::ErrorKind;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Output};

use crate::checks;
use crate::coder::{CoderKind, CoderSandbox};
use crate::config;
use crate::content::{ContentResolver, prompt_section};
use crate::credential;
use crate::land;
use crate::os;
use crate::review::{self, ReviewState};
use crate::work_model::{
    ArtifactRef, MergeCandidateMergeState, MergeCandidateMergeStatus, MergeCandidateReviewState,
    WORK_ARTIFACTS_DIR, WorkItem, WorkModelError, WorkModelStorageError, WorkModelStore,
    to_json_pretty,
};
use crate::worktree;

pub struct WorkMergeConfig<'a> {
    pub project_root: &'a Path,
    pub store: &'a WorkModelStore,
    pub work_item_id: &'a str,
    pub merge_candidate_id: &'a str,
    pub resolver: &'a ContentResolver,
    pub extra_args: &'a [String],
    pub coder_kind: CoderKind,
    pub no_sandbox: bool,
}

pub struct WorkMergeOutcome {
    pub merge_candidate_id: String,
    pub landed_commit: String,
}

pub fn merge_candidate(config: WorkMergeConfig<'_>) -> Result<WorkMergeOutcome> {
    let item = read_work_item_or_not_found(config.store, config.work_item_id)?;
    let candidate = item
        .merge_candidates
        .iter()
        .find(|candidate| candidate.id == config.merge_candidate_id)
        .cloned()
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Merge Candidate {:?} not found in Work Item {:?}",
                config.merge_candidate_id,
                config.work_item_id
            )
        })?;
    if let Err(error) = candidate.validate(&item) {
        if can_record_validation_failure(&error) {
            record_candidate_failure(
                config.store,
                config.work_item_id,
                &candidate.id,
                error.to_string(),
                Vec::new(),
                Vec::new(),
            )?;
        }
        bail!("{error}");
    }

    if candidate.merge_state.status == MergeCandidateMergeStatus::Landed {
        if let Some(landed_commit) = candidate.merge_state.landed_commit.clone() {
            return Ok(WorkMergeOutcome {
                merge_candidate_id: candidate.id,
                landed_commit,
            });
        }
    }

    let source_workspace = resolve_managed_candidate_workspace_path(
        config.project_root,
        &candidate.source_workspace.path,
    )?;
    let target_workspace =
        resolve_workspace_path(config.project_root, &candidate.target_workspace.path);
    let artifact_dir =
        merge_artifact_dir(config.project_root, &candidate.attempt_id, &candidate.id);
    fs::create_dir_all(&artifact_dir)?;

    set_candidate_executing(config.store, config.work_item_id, &candidate.id)?;

    let result = execute_merge(
        &config,
        &item,
        &candidate,
        &source_workspace,
        &target_workspace,
        &artifact_dir,
    );
    match result {
        Ok(outcome) => Ok(outcome),
        Err(error) => {
            if !candidate_has_failure(config.store, config.work_item_id, &candidate.id)? {
                record_candidate_failure(
                    config.store,
                    config.work_item_id,
                    &candidate.id,
                    error.to_string(),
                    Vec::new(),
                    Vec::new(),
                )?;
            }
            Err(error)
        }
    }
}

fn execute_merge(
    config: &WorkMergeConfig<'_>,
    item: &WorkItem,
    candidate: &crate::work_model::MergeCandidate,
    source_workspace: &Path,
    target_workspace: &Path,
    artifact_dir: &Path,
) -> Result<WorkMergeOutcome> {
    ensure_same_git_repository(config.project_root, source_workspace)?;
    ensure_same_git_repository(config.project_root, target_workspace)?;
    ensure_registered_worktree(config.project_root, source_workspace)?;
    ensure_clean_worktree(source_workspace)?;
    ensure_clean_worktree(target_workspace)?;
    let target_head_before = git_stdout(
        target_workspace,
        &["rev-parse", &candidate.target_branch],
        "resolve target branch",
    )?;
    let source_head = head_commit(source_workspace)?;
    if source_head != candidate.candidate_commit {
        bail!(
            "Merge Candidate {:?} expected source commit {} but workspace is at {}",
            candidate.id,
            candidate.candidate_commit,
            source_head
        );
    }

    rebase_candidate(source_workspace, &candidate.target_branch)?;
    ensure_clean_worktree(source_workspace)?;

    let check_artifacts =
        match run_merge_checks(config.project_root, source_workspace, artifact_dir) {
            Ok(artifacts) => artifacts,
            Err(error) => {
                let artifacts = check_artifacts_for_failure(config.project_root, artifact_dir);
                record_candidate_failure(
                    config.store,
                    config.work_item_id,
                    &candidate.id,
                    error.to_string(),
                    artifacts,
                    Vec::new(),
                )?;
                return Err(error);
            }
        };
    let candidate_head_after_rebase = head_commit(source_workspace)?;
    set_candidate_reviewing(config.store, config.work_item_id, &candidate.id)?;
    let review_artifacts = run_merge_reviews(
        config,
        item,
        candidate,
        source_workspace,
        artifact_dir,
        &check_artifacts,
        &target_head_before,
        &candidate_head_after_rebase,
    )?;
    record_candidate_reviews_passed(config.store, config.work_item_id, &candidate.id)?;

    let landed_commit = head_commit(source_workspace)?;
    let target_head_now = git_stdout(
        target_workspace,
        &["rev-parse", &candidate.target_branch],
        "resolve target branch before merge",
    )?;
    if target_head_now != target_head_before {
        bail!(
            "Target branch {} moved from {} to {}; retry merge",
            candidate.target_branch,
            target_head_before,
            target_head_now
        );
    }

    git(
        target_workspace,
        &["checkout", &candidate.target_branch],
        "checkout target branch",
    )?;
    git(
        target_workspace,
        &["merge", "--ff-only", &landed_commit],
        "fast-forward target branch",
    )?;

    record_candidate_landed(
        config.store,
        config.work_item_id,
        &candidate.id,
        &landed_commit,
        check_artifacts,
        review_artifacts,
    )?;
    if let Err(error) = cleanup_managed_workspace(config.project_root, source_workspace) {
        eprintln!(
            "  Warning: Merge Candidate {} landed, but managed workspace cleanup failed: {error}",
            candidate.id
        );
    }

    Ok(WorkMergeOutcome {
        merge_candidate_id: candidate.id.clone(),
        landed_commit,
    })
}

fn run_merge_checks(
    project_root: &Path,
    source_workspace: &Path,
    artifact_dir: &Path,
) -> Result<Vec<ArtifactRef>> {
    let Some(config) = config::load_factory_config(project_root)? else {
        return Ok(Vec::new());
    };
    let checks: Vec<_> = config
        .checks
        .into_iter()
        .filter(|check| check.run_before_land)
        .collect();
    if checks.is_empty() {
        return Ok(Vec::new());
    }

    let checks_dir = artifact_dir.join("checks");
    fs::create_dir_all(&checks_dir)?;
    let outcome = land::run_pre_land_checks_for_worktree(source_workspace, &checks)?;
    write_check_results(&checks_dir.join("check-results.txt"), &outcome.results)?;
    if let Some(rerun_results) = outcome.after_autofix {
        write_check_results(
            &checks_dir.join("check-results-after-autofix.txt"),
            &rerun_results,
        )?;
        ensure_clean_worktree(source_workspace)?;
    }

    Ok(vec![ArtifactRef {
        producer_id: "merge-checks".to_string(),
        path: path_for_model(project_root, &checks_dir),
    }])
}

fn write_check_results(path: &Path, results: &[checks::CheckRunResult]) -> Result<()> {
    let mut content = String::new();
    for result in results {
        content.push_str(&format!(
            "Check: {}\nCommand: {}\nPassed: {}\n\n{}\n",
            result.check.name, result.check.command, result.passed, result.output
        ));
    }
    fs::write(path, content)?;
    Ok(())
}

fn check_artifacts_for_failure(project_root: &Path, artifact_dir: &Path) -> Vec<ArtifactRef> {
    let checks_dir = artifact_dir.join("checks");
    if checks_dir.is_dir() {
        vec![ArtifactRef {
            producer_id: "merge-checks".to_string(),
            path: path_for_model(project_root, &checks_dir),
        }]
    } else {
        Vec::new()
    }
}

fn run_merge_reviews(
    config: &WorkMergeConfig<'_>,
    item: &WorkItem,
    candidate: &crate::work_model::MergeCandidate,
    source_workspace: &Path,
    artifact_dir: &Path,
    check_artifacts: &[ArtifactRef],
    target_head_before: &str,
    candidate_head_after_rebase: &str,
) -> Result<Vec<ArtifactRef>> {
    let reviews_dir = artifact_dir.join("reviews");
    fs::create_dir_all(&reviews_dir)?;
    let mut verdicts = std::collections::BTreeMap::new();
    let mut artifacts = Vec::new();

    for reviewer in review::REVIEWERS {
        let reviewer_dir = reviews_dir.join(reviewer);
        fs::create_dir_all(&reviewer_dir)?;
        let review_path = reviewer_dir.join("review.md");
        if review_path.exists() {
            fs::remove_file(&review_path)?;
        }
        let verdict = run_one_merge_reviewer(
            config,
            item,
            candidate,
            source_workspace,
            &reviewer_dir,
            &review_path,
            reviewer,
            check_artifacts,
            target_head_before,
            candidate_head_after_rebase,
        )?;
        verdicts.insert((*reviewer).to_string(), verdict);
        artifacts.push(ArtifactRef {
            producer_id: format!("merge-review-{reviewer}"),
            path: path_for_model(config.project_root, &review_path),
        });
    }

    let state = ReviewState::from_verdicts(1, verdicts);
    review::write_review_state(artifact_dir, &state)?;
    artifacts.push(ArtifactRef {
        producer_id: "merge-review-state".to_string(),
        path: path_for_model(config.project_root, &artifact_dir.join("review-state.json")),
    });
    if !state.is_accepted() {
        record_candidate_failure(
            config.store,
            config.work_item_id,
            &candidate.id,
            format!("Merge-time reviewers returned {}", state.state.as_str()),
            check_artifacts.to_vec(),
            artifacts,
        )?;
        bail!("Merge-time reviewers did not pass");
    }

    Ok(artifacts)
}

fn run_one_merge_reviewer(
    config: &WorkMergeConfig<'_>,
    item: &WorkItem,
    candidate: &crate::work_model::MergeCandidate,
    source_workspace: &Path,
    reviewer_dir: &Path,
    review_path: &Path,
    reviewer: &str,
    check_artifacts: &[ArtifactRef],
    target_head_before: &str,
    candidate_head_after_rebase: &str,
) -> Result<review::Verdict> {
    let prompt_key = format!("prompts/review-{reviewer}.md");
    let prompt_content = config.resolver.resolve_content(&prompt_key);
    let system = prompt_content
        .as_deref()
        .map(|content| prompt_section(content, "system").replace("{{RUN_ID}}", &candidate.id))
        .unwrap_or_else(|| format!("You are a Factory {reviewer} reviewer."));
    let base_prompt = prompt_content
        .as_deref()
        .map(|content| prompt_section(content, "changes").replace("{{RUN_ID}}", &candidate.id))
        .unwrap_or_default();
    let candidate_json = to_json_pretty(candidate)?;
    let attempt_history = merge_review_attempt_history(item, candidate);
    let check_text = if check_artifacts.is_empty() {
        "None.".to_string()
    } else {
        check_artifacts
            .iter()
            .map(|artifact| format!("- {}: {}", artifact.producer_id, artifact.path))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let prompt = format!(
        "{base_prompt}\n\nExecute a merge-time Work model review.\n\nWork Item: {}\nMerge Candidate: {}\nReviewer: {}\nCandidate workspace: {}\nTarget branch: {}\nReview diff: git -C {} diff {}..HEAD\n\nAttempt history:\n{}\n\nRebase/update state:\n- Rebased candidate workspace onto target branch {} before checks and reviewers.\n- Target branch head before merge checks/reviews: {}\n- Candidate head after rebase/update: {}\n\nCheck artifacts:\n{}\n\nWrite the review artifact to exactly this path:\n{}\n\nMerge Candidate model:\n{}\n",
        config.work_item_id,
        candidate.id,
        reviewer,
        source_workspace.display(),
        candidate.target_branch,
        source_workspace.display(),
        candidate.target_branch,
        attempt_history,
        candidate.target_branch,
        target_head_before,
        candidate_head_after_rebase,
        check_text,
        review_path.display(),
        candidate_json
    );
    let reviewer_system = format!(
        "{system}\nReview only this merge candidate. Write a verdict line with pass, fail, or uncertain to {}.",
        review_path.display()
    );
    if !config.no_sandbox {
        os::check_prerequisites_for(config.coder_kind)?;
        credential::inject_credentials()?;
        credential::setup_git_signing();
    }

    let (sandbox, _sandbox_profile) = if config.no_sandbox {
        (CoderSandbox::None, None)
    } else {
        let readable_roots = merge_review_readable_sandbox_roots(source_workspace)?;
        build_reviewer_sandbox(
            config.coder_kind,
            config.resolver,
            reviewer_dir,
            &readable_roots,
        )?
    };
    let coder = config.coder_kind.boxed(sandbox);
    review::run_reviewer_with_coder(review::ReviewCoderRun {
        reviewer_name: reviewer,
        system_prompt: &reviewer_system,
        review_prompt: &prompt,
        artifact_root: reviewer_dir,
        review_path,
        working_dir: reviewer_dir,
        extra_args: config.extra_args,
        reviewer: &*coder,
        transcript_path: None,
    })
}

fn merge_review_attempt_history(
    item: &WorkItem,
    candidate: &crate::work_model::MergeCandidate,
) -> String {
    let Some(attempt) = item
        .attempts
        .iter()
        .find(|attempt| attempt.id == candidate.attempt_id)
    else {
        return format!("- Attempt {} is missing.", candidate.attempt_id);
    };

    let mut lines = vec![format!(
        "- Attempt {} review_state: {}",
        attempt.id,
        attempt
            .review_state
            .as_ref()
            .map(|state| state.as_str())
            .unwrap_or("not-reviewed")
    )];
    for task in &attempt.tasks {
        let mut line = format!(
            "- Task {}: kind={}, role={}, status={}",
            task.id,
            task.kind.as_str(),
            task.role,
            task.status.as_str()
        );
        if let Some(output) = &task.output {
            line.push_str(&format!(
                ", source_branch={}, commit={}",
                output.source_branch, output.commit
            ));
        }
        if !task.input_artifacts.is_empty() {
            line.push_str(", input_artifacts=");
            line.push_str(
                &task
                    .input_artifacts
                    .iter()
                    .map(|artifact| format!("{}:{}", artifact.producer_id, artifact.path))
                    .collect::<Vec<_>>()
                    .join(","),
            );
        }
        lines.push(line);
    }
    lines.join("\n")
}

fn merge_review_readable_sandbox_roots(source_workspace: &Path) -> Result<Vec<PathBuf>> {
    let mut roots = vec![source_workspace.to_path_buf()];
    let common_git_dir = worktree::git_common_dir(source_workspace)?;
    if !roots.iter().any(|root| root == &common_git_dir) {
        roots.push(common_git_dir);
    }
    Ok(roots)
}

fn build_reviewer_sandbox(
    coder_kind: CoderKind,
    resolver: &ContentResolver,
    artifact_dir: &Path,
    readable_roots: &[PathBuf],
) -> Result<(CoderSandbox, Option<os::SandboxProfile>)> {
    let home = std::env::var("HOME").unwrap_or_default();
    let writable_roots = vec![artifact_dir.to_path_buf()];
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

fn set_candidate_executing(
    store: &WorkModelStore,
    work_item_id: &str,
    candidate_id: &str,
) -> Result<()> {
    update_candidate(store, work_item_id, candidate_id, |candidate| {
        candidate.review_state = MergeCandidateReviewState::Pending;
        candidate.merge_state = MergeCandidateMergeState {
            status: MergeCandidateMergeStatus::Executing,
            landed_commit: None,
            failure_reason: None,
            check_artifacts: Vec::new(),
            review_artifacts: Vec::new(),
        };
    })
}

fn set_candidate_reviewing(
    store: &WorkModelStore,
    work_item_id: &str,
    candidate_id: &str,
) -> Result<()> {
    update_candidate(store, work_item_id, candidate_id, |candidate| {
        candidate.review_state = MergeCandidateReviewState::Reviewing;
    })
}

fn record_candidate_reviews_passed(
    store: &WorkModelStore,
    work_item_id: &str,
    candidate_id: &str,
) -> Result<()> {
    update_candidate(store, work_item_id, candidate_id, |candidate| {
        candidate.review_state = MergeCandidateReviewState::Passed;
    })
}

fn record_candidate_failure(
    store: &WorkModelStore,
    work_item_id: &str,
    candidate_id: &str,
    reason: String,
    check_artifacts: Vec<ArtifactRef>,
    review_artifacts: Vec<ArtifactRef>,
) -> Result<()> {
    update_candidate(store, work_item_id, candidate_id, |candidate| {
        if !review_artifacts.is_empty()
            || candidate.review_state == MergeCandidateReviewState::Reviewing
        {
            candidate.review_state = MergeCandidateReviewState::Failed;
        }
        candidate.merge_state = MergeCandidateMergeState {
            status: MergeCandidateMergeStatus::Failed,
            landed_commit: None,
            failure_reason: Some(reason),
            check_artifacts,
            review_artifacts,
        };
    })
}

fn record_candidate_landed(
    store: &WorkModelStore,
    work_item_id: &str,
    candidate_id: &str,
    landed_commit: &str,
    check_artifacts: Vec<ArtifactRef>,
    review_artifacts: Vec<ArtifactRef>,
) -> Result<()> {
    update_candidate(store, work_item_id, candidate_id, |candidate| {
        candidate.review_state = MergeCandidateReviewState::Passed;
        candidate.merge_state = MergeCandidateMergeState {
            status: MergeCandidateMergeStatus::Landed,
            landed_commit: Some(landed_commit.to_string()),
            failure_reason: None,
            check_artifacts,
            review_artifacts,
        };
    })
}

fn update_candidate(
    store: &WorkModelStore,
    work_item_id: &str,
    candidate_id: &str,
    update: impl FnOnce(&mut crate::work_model::MergeCandidate),
) -> Result<()> {
    let mut item = read_work_item_or_not_found(store, work_item_id)?;
    let candidate = item
        .merge_candidates
        .iter_mut()
        .find(|candidate| candidate.id == candidate_id)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Merge Candidate {:?} not found in Work Item {:?}",
                candidate_id,
                work_item_id
            )
        })?;
    update(candidate);
    store.write_work_item(&item)?;
    Ok(())
}

fn candidate_has_failure(
    store: &WorkModelStore,
    work_item_id: &str,
    candidate_id: &str,
) -> Result<bool> {
    let item = read_work_item_or_not_found(store, work_item_id)?;
    let candidate = item
        .merge_candidates
        .iter()
        .find(|candidate| candidate.id == candidate_id)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Merge Candidate {:?} not found in Work Item {:?}",
                candidate_id,
                work_item_id
            )
        })?;
    Ok(candidate.merge_state.status == MergeCandidateMergeStatus::Failed)
}

fn can_record_validation_failure(error: &WorkModelError) -> bool {
    matches!(
        error,
        WorkModelError::MergeCandidateAttemptReviewsNotPassed { .. }
    )
}

fn read_work_item_or_not_found(store: &WorkModelStore, id: &str) -> Result<WorkItem> {
    match store.read_work_item_for_merge_recovery(id) {
        Ok(item) => Ok(item),
        Err(WorkModelStorageError::ReadFile { source, .. })
            if source.kind() == ErrorKind::NotFound =>
        {
            bail!("Work Item {id:?} not found")
        }
        Err(error) => Err(error.into()),
    }
}

fn merge_artifact_dir(project_root: &Path, attempt_id: &str, candidate_id: &str) -> PathBuf {
    project_root
        .join(WORK_ARTIFACTS_DIR)
        .join(attempt_id)
        .join(candidate_id)
        .join("merge")
}

fn resolve_workspace_path(project_root: &Path, path: &str) -> PathBuf {
    let path = PathBuf::from(path);
    if path.is_absolute() {
        path
    } else {
        project_root.join(path)
    }
}

fn resolve_managed_candidate_workspace_path(project_root: &Path, path: &str) -> Result<PathBuf> {
    let relative_path = Path::new(path);
    if relative_path.is_absolute() {
        bail!("Merge Candidate source workspace path must be relative: {path}");
    }

    let mut components = Vec::new();
    for component in relative_path.components() {
        match component {
            Component::Normal(part) => components.push(part.to_owned()),
            _ => bail!(
                "Merge Candidate source workspace path must stay under .factory/work/workspaces: {path}"
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
        bail!(
            "Merge Candidate source workspace path must stay under .factory/work/workspaces: {path}"
        );
    }

    Ok(resolve_workspace_path(project_root, path))
}

fn rebase_candidate(source_workspace: &Path, target_branch: &str) -> Result<()> {
    let output = Command::new("git")
        .args(["-C", &source_workspace.to_string_lossy()])
        .args(["rebase", target_branch])
        .output()
        .context("Failed to run git rebase")?;
    if output.status.success() {
        return Ok(());
    }
    Command::new("git")
        .args(["-C", &source_workspace.to_string_lossy()])
        .args(["rebase", "--abort"])
        .output()
        .ok();
    bail!(
        "Rebase failed while updating Merge Candidate against {target_branch}:\n{}",
        command_output(&output)
    )
}

fn cleanup_managed_workspace(project_root: &Path, source_workspace: &Path) -> Result<()> {
    let output = Command::new("git")
        .args(["-C", &project_root.to_string_lossy()])
        .args([
            "worktree",
            "remove",
            "--force",
            &source_workspace.to_string_lossy(),
        ])
        .output()
        .context("Failed to remove managed workspace")?;
    if output.status.success() {
        Ok(())
    } else {
        bail!(
            "Failed to remove managed workspace {}:\n{}",
            source_workspace.display(),
            command_output(&output)
        )
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

fn ensure_clean_worktree(workspace_path: &Path) -> Result<()> {
    let output = git_output(
        workspace_path,
        &[
            "status",
            "--porcelain",
            "--untracked-files=normal",
            "--",
            ".",
            ":(exclude).factory",
        ],
        "check worktree status",
    )?;
    if !output.stdout.is_empty() {
        bail!(
            "Workspace {} has uncommitted changes",
            workspace_path.display()
        );
    }
    Ok(())
}

fn head_commit(repo: &Path) -> Result<String> {
    git_stdout(repo, &["rev-parse", "HEAD"], "resolve HEAD")
}

fn git(repo: &Path, args: &[&str], action: &str) -> Result<()> {
    let output = git_output(repo, args, action)?;
    if output.status.success() {
        return Ok(());
    }
    bail!("Failed to {action}:\n{}", command_output(&output))
}

fn git_stdout(repo: &Path, args: &[&str], action: &str) -> Result<String> {
    let output = git_output(repo, args, action)?;
    if !output.status.success() {
        bail!("Failed to {action}:\n{}", command_output(&output));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn git_output(repo: &Path, args: &[&str], action: &str) -> Result<Output> {
    Command::new("git")
        .args(["-C", &repo.to_string_lossy()])
        .args(args)
        .output()
        .with_context(|| format!("Failed to {action}"))
}

fn command_output(output: &Output) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let mut combined = String::new();
    if !stdout.trim().is_empty() {
        combined.push_str("stdout:\n");
        combined.push_str(stdout.trim_end());
        combined.push('\n');
    }
    if !stderr.trim().is_empty() {
        combined.push_str("stderr:\n");
        combined.push_str(stderr.trim_end());
        combined.push('\n');
    }
    if combined.is_empty() {
        combined.push_str("(no output)\n");
    }
    combined
}

fn path_for_model(project_root: &Path, path: &Path) -> String {
    path.strip_prefix(project_root)
        .unwrap_or(path)
        .to_string_lossy()
        .to_string()
}
