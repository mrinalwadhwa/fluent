use anyhow::{Context, Result, bail};
use std::collections::BTreeMap;
use std::fs;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::thread;

use crate::checks;
use crate::coder::{CoderKind, CoderSandbox};
use crate::config;
use crate::content::{ContentResolver, prompt_section};
use crate::credential;
use crate::land;
use crate::os;
use crate::review::{self, ReviewState};
use crate::review_diff_command::render_review_diff_command;
use crate::work_model::{
    ArtifactRef, MergeCandidateMergeState, MergeCandidateMergeStatus, MergeCandidateReviewState,
    WORK_ARTIFACTS_DIR, WorkItem, WorkModelError, WorkModelStorageError, WorkModelStore,
    resolve_expected_candidate_workspace_path, reviewer_workspace_path, to_json_pretty,
    work_behavior_review_input,
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
    item.ensure_not_abandoned()?;
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
        config.work_item_id,
        &candidate.attempt_id,
    )?;
    let target_workspace =
        resolve_workspace_path(config.project_root, &candidate.target_workspace.path);
    let artifact_dir = merge_artifact_dir(
        config.project_root,
        config.work_item_id,
        &candidate.attempt_id,
        &candidate.id,
    );
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
    recover_landed_candidate_result(config.store, config.work_item_id, &candidate.id, result)
}

fn recover_landed_candidate_result(
    store: &WorkModelStore,
    work_item_id: &str,
    candidate_id: &str,
    result: Result<WorkMergeOutcome>,
) -> Result<WorkMergeOutcome> {
    match result {
        Ok(outcome) => Ok(outcome),
        Err(error) => {
            if let Some(landed_commit) = candidate_landed_commit(store, work_item_id, candidate_id)?
            {
                eprintln!(
                    "  Warning: Merge Candidate {candidate_id} landed, but post-landing merge cleanup failed: {error}",
                );
                return Ok(WorkMergeOutcome {
                    merge_candidate_id: candidate_id.to_string(),
                    landed_commit,
                });
            }
            if !candidate_has_failure(store, work_item_id, candidate_id)? {
                record_candidate_failure(
                    store,
                    work_item_id,
                    candidate_id,
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
    artifact_dir: &Path,
    check_artifacts: &[ArtifactRef],
    target_head_before: &str,
    candidate_head_after_rebase: &str,
) -> Result<Vec<ArtifactRef>> {
    let reviews_dir = artifact_dir.join("reviews");
    fs::create_dir_all(&reviews_dir)?;
    let mut verdicts = std::collections::BTreeMap::new();
    let mut artifacts = Vec::new();
    let reviewer_worktrees = MergeReviewerWorktrees::prepare(
        config.project_root,
        config.work_item_id,
        &candidate.attempt_id,
        candidate_head_after_rebase,
    )?;

    if !config.no_sandbox {
        os::check_prerequisites_for(config.coder_kind)?;
        credential::inject_credentials()?;
        credential::setup_git_signing();
    }

    let review_result = (|| {
        let mut jobs = Vec::new();
        for reviewer in review::REVIEWERS {
            let reviewer_dir = reviews_dir.join(reviewer);
            fs::create_dir_all(&reviewer_dir)?;
            let review_path = reviewer_dir.join("review.md");
            if review_path.exists() {
                fs::remove_file(&review_path)?;
            }
            let review_artifact = ArtifactRef {
                producer_id: format!("merge-review-{reviewer}"),
                path: path_for_model(config.project_root, &review_path),
            };
            let review_workspace = reviewer_worktrees.path_for(reviewer)?;
            jobs.push(MergeReviewerJob {
                reviewer,
                reviewer_dir,
                review_path,
                review_artifact,
                review_workspace,
            });
        }

        let mut results = thread::scope(|scope| {
            let mut handles = Vec::new();
            for job in jobs {
                let reviewer = job.reviewer;
                let review_artifact = job.review_artifact.clone();
                handles.push((
                    reviewer,
                    review_artifact,
                    scope.spawn(move || {
                        run_merge_reviewer_job(
                            config,
                            item,
                            candidate,
                            job,
                            check_artifacts,
                            target_head_before,
                            candidate_head_after_rebase,
                        )
                    }),
                ));
            }

            let mut results = Vec::new();
            for (reviewer, review_artifact, handle) in handles {
                results.push(match handle.join() {
                    Ok(result) => result,
                    Err(_) => Err(MergeReviewerFailure {
                        reviewer,
                        error: anyhow::anyhow!("Merge-time reviewer {reviewer} thread panicked"),
                        review_artifact,
                    }),
                });
            }
            results
        });

        results.sort_by_key(|result| match result {
            Ok(result) => reviewer_order(result.reviewer),
            Err(result) => reviewer_order(result.reviewer),
        });

        let mut first_error = None;
        for result in results {
            match result {
                Ok(result) => {
                    verdicts.insert(result.reviewer.to_string(), result.verdict);
                    artifacts.push(result.review_artifact);
                }
                Err(result) => {
                    verdicts.insert(result.reviewer.to_string(), review::Verdict::Fail);
                    artifacts.push(result.review_artifact);
                    if first_error.is_none() {
                        first_error = Some(result.error);
                    }
                }
            }
        }

        let state = ReviewState::from_verdicts(1, verdicts.clone());
        review::write_review_state(artifact_dir, &state)?;
        artifacts.push(ArtifactRef {
            producer_id: "merge-review-state".to_string(),
            path: path_for_model(config.project_root, &artifact_dir.join("review-state.json")),
        });

        if let Some(error) = first_error {
            record_candidate_failure(
                config.store,
                config.work_item_id,
                &candidate.id,
                error.to_string(),
                check_artifacts.to_vec(),
                artifacts.clone(),
            )?;
            return Err(error);
        }
        if !state.is_accepted() {
            record_candidate_failure(
                config.store,
                config.work_item_id,
                &candidate.id,
                format!("Merge-time reviewers returned {}", state.state.as_str()),
                check_artifacts.to_vec(),
                artifacts.clone(),
            )?;
            bail!("Merge-time reviewers did not pass");
        }

        Ok(artifacts)
    })();

    let cleanup_result = reviewer_worktrees.cleanup();
    match (review_result, cleanup_result) {
        (Ok(artifacts), Ok(())) => Ok(artifacts),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(cleanup_error)) => Err(cleanup_error),
        (Err(error), Err(cleanup_error)) => {
            eprintln!("  Warning: merge reviewer worktree cleanup failed: {cleanup_error}");
            Err(error)
        }
    }
}

struct MergeReviewerJob<'a> {
    reviewer: &'a str,
    reviewer_dir: PathBuf,
    review_path: PathBuf,
    review_artifact: ArtifactRef,
    review_workspace: PathBuf,
}

struct MergeReviewerSuccess {
    reviewer: &'static str,
    verdict: review::Verdict,
    review_artifact: ArtifactRef,
}

struct MergeReviewerFailure {
    reviewer: &'static str,
    error: anyhow::Error,
    review_artifact: ArtifactRef,
}

fn run_merge_reviewer_job(
    config: &WorkMergeConfig<'_>,
    item: &WorkItem,
    candidate: &crate::work_model::MergeCandidate,
    job: MergeReviewerJob<'static>,
    check_artifacts: &[ArtifactRef],
    target_head_before: &str,
    candidate_head_after_rebase: &str,
) -> Result<MergeReviewerSuccess, MergeReviewerFailure> {
    let baseline = match review_guard_file_snapshot(&job.review_workspace) {
        Ok(baseline) => baseline,
        Err(error) => {
            return Err(MergeReviewerFailure {
                reviewer: job.reviewer,
                error,
                review_artifact: job.review_artifact,
            });
        }
    };
    let verdict_result = run_one_merge_reviewer(
        config,
        item,
        candidate,
        &job.review_workspace,
        &job.reviewer_dir,
        &job.review_path,
        job.reviewer,
        check_artifacts,
        target_head_before,
        candidate_head_after_rebase,
    );
    if let Err(error) =
        ensure_merge_reviewer_kept_candidate_clean(&job.review_workspace, job.reviewer, &baseline)
    {
        return Err(MergeReviewerFailure {
            reviewer: job.reviewer,
            error,
            review_artifact: job.review_artifact,
        });
    }
    match verdict_result {
        Ok(verdict) => Ok(MergeReviewerSuccess {
            reviewer: job.reviewer,
            verdict,
            review_artifact: job.review_artifact,
        }),
        Err(error) => Err(MergeReviewerFailure {
            reviewer: job.reviewer,
            error,
            review_artifact: job.review_artifact,
        }),
    }
}

fn reviewer_order(reviewer: &str) -> usize {
    review::REVIEWERS
        .iter()
        .position(|candidate| *candidate == reviewer)
        .unwrap_or(review::REVIEWERS.len())
}

struct MergeReviewerWorktrees {
    project_root: PathBuf,
    entries: Vec<MergeReviewerWorktree>,
}

struct MergeReviewerWorktree {
    reviewer: &'static str,
    path: PathBuf,
}

impl MergeReviewerWorktrees {
    fn prepare(
        project_root: &Path,
        work_item_id: &str,
        attempt_id: &str,
        commit: &str,
    ) -> Result<Self> {
        let sibling_root = project_root.parent().unwrap_or(project_root);
        let mut entries = Vec::new();
        for reviewer in review::REVIEWERS {
            let relative = reviewer_workspace_path(work_item_id, attempt_id, reviewer);
            let name = relative.strip_prefix("../").unwrap_or(&relative);
            let path = sibling_root.join(name);
            let entry = match prepare_one_reviewer_worktree(project_root, &path, reviewer, commit) {
                Ok(entry) => entry,
                Err(error) => {
                    if let Err(cleanup_error) =
                        cleanup_reviewer_worktree_paths(project_root, &entries)
                    {
                        eprintln!(
                            "  Warning: partial merge reviewer worktree cleanup failed: {cleanup_error}"
                        );
                    }
                    return Err(error);
                }
            };
            entries.push(entry);
        }
        Ok(Self {
            project_root: project_root.to_path_buf(),
            entries,
        })
    }

    fn path_for(&self, reviewer: &str) -> Result<PathBuf> {
        self.entries
            .iter()
            .find(|entry| entry.reviewer == reviewer)
            .map(|entry| entry.path.clone())
            .ok_or_else(|| anyhow::anyhow!("Reviewer worktree for {reviewer} was not prepared"))
    }

    fn cleanup(&self) -> Result<()> {
        let mut errors = Vec::new();
        for entry in &self.entries {
            if let Err(error) = remove_reviewer_worktree_if_present(&self.project_root, &entry.path)
            {
                errors.push(format!("{}: {error}", entry.path.display()));
            }
            if entry.path.exists() {
                if let Err(error) = fs::remove_dir_all(&entry.path) {
                    errors.push(format!("{}: {error}", entry.path.display()));
                }
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            bail!("Failed to clean reviewer worktrees:\n{}", errors.join("\n"))
        }
    }
}

fn prepare_one_reviewer_worktree(
    project_root: &Path,
    path: &Path,
    reviewer: &'static str,
    commit: &str,
) -> Result<MergeReviewerWorktree> {
    remove_reviewer_worktree_if_present(project_root, path)?;
    if path.exists() {
        fs::remove_dir_all(path)
            .with_context(|| format!("Failed to clear reviewer worktree {}", path.display()))?;
    }
    let output = Command::new("git")
        .args(["-C", &project_root.to_string_lossy()])
        .args([
            "worktree",
            "add",
            "--detach",
            &path.to_string_lossy(),
            commit,
        ])
        .output()
        .context("Failed to create reviewer worktree")?;
    if !output.status.success() {
        bail!(
            "Failed to create reviewer worktree {}:\n{}",
            path.display(),
            command_output(&output)
        );
    }
    worktree::disable_commit_signing(path)?;
    Ok(MergeReviewerWorktree {
        reviewer,
        path: path.to_path_buf(),
    })
}

fn cleanup_reviewer_worktree_paths(
    project_root: &Path,
    entries: &[MergeReviewerWorktree],
) -> Result<()> {
    let mut errors = Vec::new();
    for entry in entries {
        if let Err(error) = remove_reviewer_worktree_if_present(project_root, &entry.path) {
            errors.push(format!("{}: {error}", entry.path.display()));
        }
        if entry.path.exists() {
            if let Err(error) = fs::remove_dir_all(&entry.path) {
                errors.push(format!("{}: {error}", entry.path.display()));
            }
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        bail!("Failed to clean reviewer worktrees:\n{}", errors.join("\n"))
    }
}

fn remove_reviewer_worktree_if_present(project_root: &Path, path: &Path) -> Result<()> {
    if !is_registered_worktree(project_root, path)? {
        return Ok(());
    }
    let output = Command::new("git")
        .args(["-C", &project_root.to_string_lossy()])
        .args(["worktree", "remove", "--force", &path.to_string_lossy()])
        .output()
        .context("Failed to remove reviewer worktree")?;
    if output.status.success() {
        Ok(())
    } else {
        bail!(
            "Failed to remove reviewer worktree {}:\n{}",
            path.display(),
            command_output(&output)
        )
    }
}

fn is_registered_worktree(project_root: &Path, path: &Path) -> Result<bool> {
    let expected = match fs::canonicalize(path) {
        Ok(path) => path,
        Err(error) if error.kind() == ErrorKind::NotFound => path.to_path_buf(),
        Err(error) => return Err(error.into()),
    };
    let output = Command::new("git")
        .args(["-C", &project_root.to_string_lossy()])
        .args(["worktree", "list", "--porcelain"])
        .output()
        .context("Failed to list git worktrees")?;
    if !output.status.success() {
        bail!(
            "Failed to list git worktrees: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let Some(actual_path) = line.strip_prefix("worktree ") else {
            continue;
        };
        if fs::canonicalize(actual_path).is_ok_and(|actual| actual == expected) {
            return Ok(true);
        }
    }
    Ok(false)
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
        .map(|content| work_merge_reviewer_system_prompt(content, reviewer, source_workspace))
        .unwrap_or_else(|| {
            format!(
                "You are a Factory {reviewer} reviewer.\n{}",
                merge_review_skill_instruction(reviewer, source_workspace)
            )
        });
    let candidate_json = to_json_pretty(candidate)?;
    let attempt_history = merge_review_attempt_history(item, candidate);
    let review_artifact_path = path_for_model(config.project_root, review_path);
    let review_absolute_path = review_path.display();
    let reviewer_artifact_dir = path_for_model(config.project_root, reviewer_dir);
    let check_status_text = if check_artifacts.is_empty() {
        "No merge checks ran before reviewers.".to_string()
    } else {
        format!(
            "Merge checks ran before reviewers and produced {} artifact record(s). Reviewers are not required to inspect check artifact paths from this sandbox.",
            check_artifacts.len()
        )
    };
    let behavior_review_input = if reviewer == "behaviors" {
        format!("{}\n", work_behavior_review_input(item))
    } else {
        String::new()
    };
    let writable_outputs_guidance = merge_reviewer_writable_outputs_guidance(reviewer_dir);
    let review_range = format!("{target_head_before}..{candidate_head_after_rebase}");
    let review_diff_command = render_review_diff_command(source_workspace, &review_range);
    let prompt = format!(
        "Execute a merge-time Work model review.\n\nWork Item: {}\nMerge Candidate: {}\nReviewer: {}\nCandidate workspace: {}\nTarget branch: {}\nReview diff: {}\n\nCandidate workspace access:\n- Treat the candidate workspace as read-only for review purposes.\n- Do not modify, stage, unstage, commit, create, or delete files in the candidate workspace.\n\n{}\n\n{}Attempt history:\n{}\n\nRebase/update state:\n- Rebased candidate workspace onto target branch {} before checks and reviewers.\n- Target branch head before merge checks/reviews: {}\n- Candidate head after rebase/update: {}\n\nMerge check status:\n{}\n\nWork merge review artifact path:\n{}\nWrite the review artifact to exactly this filesystem path:\n{}\nYour reviewer artifact directory is:\n{}\n\nMerge Candidate model:\n{}\n",
        config.work_item_id,
        candidate.id,
        reviewer,
        source_workspace.display(),
        candidate.target_branch,
        review_diff_command,
        writable_outputs_guidance,
        behavior_review_input,
        attempt_history,
        candidate.target_branch,
        target_head_before,
        candidate_head_after_rebase,
        check_status_text,
        review_artifact_path,
        review_absolute_path,
        reviewer_artifact_dir,
        candidate_json
    );
    let reviewer_system = format!(
        "{system}\n{}\nReview only this Work Merge Candidate. The candidate workspace is read-only for review purposes. Write only merge review artifacts, with the required verdict line (pass, fail, or uncertain) in {}. This is the Work merge review artifact path {}; do not write legacy run review artifacts.\n{}",
        merge_review_decisions_instruction(source_workspace),
        review_absolute_path,
        review_artifact_path,
        writable_outputs_guidance
    );
    let attempt_artifact_root = config
        .project_root
        .join(WORK_ARTIFACTS_DIR)
        .join(config.work_item_id)
        .join(&candidate.attempt_id);
    let (sandbox, _sandbox_profile) = if config.no_sandbox {
        (CoderSandbox::None, None)
    } else {
        let readable_roots =
            merge_review_readable_sandbox_roots(source_workspace, &attempt_artifact_root)?;
        build_reviewer_sandbox(
            config.coder_kind,
            config.resolver,
            reviewer_dir,
            &readable_roots,
        )?
    };
    let cargo_target_dir = reviewer_dir.join("target");
    let extra_env = vec![(
        "CARGO_TARGET_DIR".to_string(),
        cargo_target_dir.to_string_lossy().to_string(),
    )];
    let coder = config.coder_kind.boxed(sandbox);
    review::run_reviewer_with_coder(review::ReviewCoderRun {
        reviewer_name: reviewer,
        system_prompt: &reviewer_system,
        review_prompt: &prompt,
        artifact_root: reviewer_dir,
        review_path,
        working_dir: reviewer_dir,
        extra_args: config.extra_args,
        extra_env: &extra_env,
        reviewer: &*coder,
        transcript_path: None,
    })
}

fn work_merge_reviewer_system_prompt(
    content: &str,
    reviewer: &str,
    source_workspace: &Path,
) -> String {
    let work_system = prompt_section(content, "work-system");
    let source = if work_system.trim().is_empty() {
        prompt_section(content, "system")
    } else {
        work_system
    };
    let mut lines = source.lines().map(str::to_string).collect::<Vec<_>>();
    lines.push(merge_review_skill_instruction(reviewer, source_workspace));
    lines.join("\n")
}

fn merge_reviewer_writable_outputs_guidance(reviewer_dir: &Path) -> String {
    format!(
        "Writable review outputs:\n- Put build caches, scratch files, suggested patches, proposed documentation edits, and temporary outputs under the reviewer artifact directory instead of applying them to the candidate workspace.\n- Factory sets CARGO_TARGET_DIR={}/target in this reviewer's environment. Do not override it or write build outputs into the candidate workspace.",
        reviewer_dir.display()
    )
}

fn merge_review_skill_instruction(reviewer: &str, source_workspace: &Path) -> String {
    let path = source_workspace.join(format!("skills/review-{reviewer}/SKILL.md"));
    if path.is_file() {
        format!("Follow the review-{reviewer} skill at {}.", path.display())
    } else {
        format!(
            "No review-{reviewer} skill file was found in the candidate workspace; apply the reviewer role directly."
        )
    }
}

fn merge_review_decisions_instruction(source_workspace: &Path) -> String {
    let path = source_workspace.join(".factory/expertise/decisions.md");
    if path.is_file() {
        format!(
            "Read recorded decisions at {} if it exists. Do not flag findings that contradict a recorded decision.",
            path.display()
        )
    } else {
        "No project decision file was found in the candidate workspace.".to_string()
    }
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

fn merge_review_readable_sandbox_roots(
    source_workspace: &Path,
    attempt_artifact_root: &Path,
) -> Result<Vec<PathBuf>> {
    let mut roots = vec![source_workspace.to_path_buf()];
    let common_git_dir = worktree::git_common_dir(source_workspace)?;
    if !roots.iter().any(|root| root == &common_git_dir) {
        roots.push(common_git_dir);
    }
    if !roots.iter().any(|root| root == attempt_artifact_root) {
        roots.push(attempt_artifact_root.to_path_buf());
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
        if candidate.merge_state.status == MergeCandidateMergeStatus::Landed
            && candidate.merge_state.landed_commit.is_some()
        {
            return;
        }
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

fn candidate_landed_commit(
    store: &WorkModelStore,
    work_item_id: &str,
    candidate_id: &str,
) -> Result<Option<String>> {
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
    if candidate.merge_state.status == MergeCandidateMergeStatus::Landed {
        Ok(candidate.merge_state.landed_commit.clone())
    } else {
        Ok(None)
    }
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

fn merge_artifact_dir(
    project_root: &Path,
    work_item_id: &str,
    attempt_id: &str,
    candidate_id: &str,
) -> PathBuf {
    project_root
        .join(WORK_ARTIFACTS_DIR)
        .join(work_item_id)
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

fn resolve_managed_candidate_workspace_path(
    project_root: &Path,
    path: &str,
    work_item_id: &str,
    attempt_id: &str,
) -> Result<PathBuf> {
    Ok(resolve_expected_candidate_workspace_path(
        project_root,
        path,
        work_item_id,
        attempt_id,
        "Merge Candidate source",
    )?)
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
    let status = worktree_status(workspace_path)?;
    if !status.is_empty() {
        bail!(
            "Workspace {} has uncommitted changes",
            workspace_path.display()
        );
    }
    Ok(())
}

fn ensure_merge_reviewer_kept_candidate_clean(
    source_workspace: &Path,
    reviewer: &str,
    review_guard_baseline: &BTreeMap<String, ReviewGuardFileState>,
) -> Result<()> {
    let status = worktree_status(source_workspace)?;
    if !status.is_empty() {
        bail!(
            "Merge-time reviewer {reviewer} dirtied candidate workspace {}; candidate workspaces are read-only during merge review. Dirty status:\n{}",
            source_workspace.display(),
            status.trim_end()
        );
    }
    let review_guard_after = review_guard_file_snapshot(source_workspace)?;
    if &review_guard_after != review_guard_baseline {
        let changes = review_guard_snapshot_changes(review_guard_baseline, &review_guard_after);
        bail!(
            "Merge-time reviewer {reviewer} dirtied candidate workspace {}; candidate workspaces are read-only during merge review. Dirty ignored or Factory files:\n{}",
            source_workspace.display(),
            changes.trim_end()
        );
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ReviewGuardFileState {
    len: u64,
    hash: u64,
}

fn review_guard_file_snapshot(
    workspace_path: &Path,
) -> Result<BTreeMap<String, ReviewGuardFileState>> {
    let output = git_output(
        workspace_path,
        &[
            "ls-files",
            "--others",
            "--ignored",
            "--exclude-standard",
            "-z",
        ],
        "snapshot ignored files",
    )?;
    if !output.status.success() {
        bail!(
            "Failed to snapshot ignored files:\n{}",
            command_output(&output)
        );
    }

    let mut snapshot = BTreeMap::new();
    for raw_path in output.stdout.split(|byte| *byte == 0) {
        if raw_path.is_empty() {
            continue;
        }
        let relative_path = String::from_utf8_lossy(raw_path).to_string();
        let path = workspace_path.join(&relative_path);
        if !path.is_file() {
            continue;
        }
        let bytes = fs::read(&path)
            .with_context(|| format!("Failed to read ignored file {}", path.display()))?;
        snapshot.insert(relative_path, ReviewGuardFileState::from_bytes(&bytes));
    }
    collect_factory_file_snapshot(workspace_path, &mut snapshot)?;
    Ok(snapshot)
}

impl ReviewGuardFileState {
    fn from_bytes(bytes: &[u8]) -> Self {
        let mut hasher = DefaultHasher::new();
        bytes.hash(&mut hasher);
        Self {
            len: bytes.len() as u64,
            hash: hasher.finish(),
        }
    }
}

fn collect_factory_file_snapshot(
    workspace_path: &Path,
    snapshot: &mut BTreeMap<String, ReviewGuardFileState>,
) -> Result<()> {
    let factory_dir = workspace_path.join(".factory");
    if !factory_dir.exists() {
        return Ok(());
    }
    let mut pending = vec![factory_dir];
    while let Some(dir) = pending.pop() {
        for entry in
            fs::read_dir(&dir).with_context(|| format!("Failed to read {}", dir.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            let file_type = entry.file_type()?;
            if file_type.is_dir() {
                pending.push(path);
            } else if file_type.is_file() {
                let relative_path = path_for_model(workspace_path, &path);
                let bytes = fs::read(&path)
                    .with_context(|| format!("Failed to read Factory file {}", path.display()))?;
                snapshot.insert(relative_path, ReviewGuardFileState::from_bytes(&bytes));
            } else if file_type.is_symlink() {
                let relative_path = path_for_model(workspace_path, &path);
                let target = fs::read_link(&path).with_context(|| {
                    format!("Failed to read Factory symlink {}", path.display())
                })?;
                snapshot.insert(
                    relative_path,
                    ReviewGuardFileState::from_bytes(target.to_string_lossy().as_bytes()),
                );
            }
        }
    }
    Ok(())
}

fn review_guard_snapshot_changes(
    before: &BTreeMap<String, ReviewGuardFileState>,
    after: &BTreeMap<String, ReviewGuardFileState>,
) -> String {
    let mut lines = Vec::new();
    for path in before.keys() {
        if !after.contains_key(path) {
            lines.push(format!("- deleted ignored file {path}"));
        }
    }
    for (path, state) in after {
        match before.get(path) {
            None => lines.push(format!("- created ignored file {path}")),
            Some(before_state) if before_state != state => {
                lines.push(format!("- modified ignored file {path}"))
            }
            Some(_) => {}
        }
    }
    lines.join("\n")
}

fn worktree_status(workspace_path: &Path) -> Result<String> {
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
    if !output.status.success() {
        bail!(
            "Failed to check worktree status:\n{}",
            command_output(&output)
        );
    }
    if !output.stdout.is_empty() {
        return Ok(String::from_utf8_lossy(&output.stdout).to_string());
    }
    Ok(String::new())
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content::ContentResolver;
    use crate::work_model::WorkItemAbandonment;
    use crate::work_model::{AttemptReviewState, AttemptStatus, TaskOutput, TaskStatus, WorkItem};

    #[test]
    fn merge_candidate_rejects_abandoned_work_item_without_mutating_state() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = WorkModelStore::new(tmp.path());
        let mut item = WorkItem {
            id: "work-1".to_string(),
            title: "Keep abandoned merge terminal".to_string(),
            planning_context: None,
            instructions: None,
            abandonment: None,
            attempts: Vec::new(),
            merge_candidates: Vec::new(),
        };
        item.add_initial_attempt("attempt-1").unwrap();
        item.abandonment = Some(WorkItemAbandonment {
            reason: Some("replacement landed".to_string()),
        });
        store.create_work_item(&item).unwrap();
        let resolver = ContentResolver::new(None);

        let error = match merge_candidate(WorkMergeConfig {
            project_root: tmp.path(),
            store: &store,
            work_item_id: "work-1",
            merge_candidate_id: "attempt-1-merge-candidate",
            resolver: &resolver,
            extra_args: &[],
            coder_kind: CoderKind::Codex,
            no_sandbox: true,
        }) {
            Ok(_) => panic!("abandoned Work Item should reject merge execution"),
            Err(error) => error,
        };

        assert!(error.to_string().contains("is abandoned"));
        let stored = store.read_work_item("work-1").unwrap();
        assert!(stored.abandonment.is_some());
        assert!(stored.merge_candidates.is_empty());
    }

    fn landed_candidate_store() -> (tempfile::TempDir, WorkModelStore, String, String, String) {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = WorkModelStore::new(tmp.path());
        let mut item = WorkItem {
            id: "work-1".to_string(),
            title: "Preserve landed state".to_string(),
            planning_context: None,
            instructions: None,
            abandonment: None,
            attempts: Vec::new(),
            merge_candidates: Vec::new(),
        };
        item.add_initial_attempt("attempt-1").unwrap();

        let attempt = item.attempts.first_mut().unwrap();
        attempt.status = AttemptStatus::Complete;
        attempt.review_state = Some(AttemptReviewState::Passed);
        let task = attempt.tasks.first_mut().unwrap();
        let workspace = task.workspace_access.writes.first().unwrap().clone();
        task.status = TaskStatus::Complete;
        task.output = Some(TaskOutput {
            workspace_id: workspace.id,
            workspace_path: workspace.path,
            source_branch: "main".to_string(),
            commit: "abc123".to_string(),
        });

        let candidate_id = item.create_or_get_merge_candidate("attempt-1").unwrap();
        store.create_work_item(&item).unwrap();
        record_candidate_landed(
            &store,
            "work-1",
            &candidate_id,
            "abc123",
            vec![ArtifactRef {
                producer_id: "checks".to_string(),
                path: ".factory/work/artifacts/checks.json".to_string(),
            }],
            vec![ArtifactRef {
                producer_id: "reviewer".to_string(),
                path: ".factory/work/artifacts/review.md".to_string(),
            }],
        )
        .unwrap();

        (
            tmp,
            store,
            "work-1".to_string(),
            candidate_id,
            "abc123".to_string(),
        )
    }

    #[test]
    fn post_landing_error_returns_landed_outcome_without_rewriting_state() {
        let (_tmp, store, work_item_id, candidate_id, landed_commit) = landed_candidate_store();

        let outcome = recover_landed_candidate_result(
            &store,
            &work_item_id,
            &candidate_id,
            Err(anyhow::anyhow!("candidate workspace is gone")),
        )
        .unwrap();

        assert_eq!(outcome.merge_candidate_id, candidate_id);
        assert_eq!(outcome.landed_commit, landed_commit);

        let item = store.read_work_item(&work_item_id).unwrap();
        let candidate = item
            .merge_candidates
            .iter()
            .find(|candidate| candidate.id == candidate_id)
            .unwrap();
        assert_eq!(candidate.review_state, MergeCandidateReviewState::Passed);
        assert_eq!(
            candidate.merge_state.status,
            MergeCandidateMergeStatus::Landed
        );
        assert_eq!(
            candidate.merge_state.landed_commit.as_deref(),
            Some(landed_commit.as_str())
        );
        assert!(candidate.merge_state.failure_reason.is_none());
        assert_eq!(candidate.merge_state.check_artifacts.len(), 1);
        assert_eq!(candidate.merge_state.review_artifacts.len(), 1);
    }

    #[test]
    fn record_failure_keeps_landed_candidate_landed() {
        let (_tmp, store, work_item_id, candidate_id, landed_commit) = landed_candidate_store();

        record_candidate_failure(
            &store,
            &work_item_id,
            &candidate_id,
            "late cleanup failed".to_string(),
            Vec::new(),
            Vec::new(),
        )
        .unwrap();

        let item = store.read_work_item(&work_item_id).unwrap();
        let candidate = item
            .merge_candidates
            .iter()
            .find(|candidate| candidate.id == candidate_id)
            .unwrap();
        assert_eq!(candidate.review_state, MergeCandidateReviewState::Passed);
        assert_eq!(
            candidate.merge_state.status,
            MergeCandidateMergeStatus::Landed
        );
        assert_eq!(
            candidate.merge_state.landed_commit.as_deref(),
            Some(landed_commit.as_str())
        );
        assert!(candidate.merge_state.failure_reason.is_none());
    }

    #[test]
    fn merge_reviewer_system_prompt_uses_work_section_without_legacy_filtering() {
        let tmp = tempfile::TempDir::new().unwrap();
        let source_workspace = tmp.path();
        fs::create_dir_all(source_workspace.join("skills/review-tests")).unwrap();
        fs::write(
            source_workspace.join("skills/review-tests/SKILL.md"),
            "# Test review\n",
        )
        .unwrap();
        let content = "\
[system]
Write your review to .factory/runs/{{RUN_ID}}/reviews/review-tests.md.
Follow the review-tests skill at skills/review-tests/SKILL.md.

[work-system]
Write your review only to the Work merge review artifact path.
Keep this Work-native sentence.
";

        let system = work_merge_reviewer_system_prompt(content, "tests", source_workspace);

        assert!(system.contains("Work merge review artifact path"));
        assert!(system.contains("Keep this Work-native sentence."));
        assert!(!system.contains(".factory/runs/"));
        assert!(!system.contains("Follow the review-tests skill at skills/review-tests/SKILL.md"));
        assert!(
            system.contains(
                source_workspace
                    .join("skills/review-tests/SKILL.md")
                    .to_string_lossy()
                    .as_ref()
            ),
            "{system}"
        );
    }

    #[test]
    fn merge_reviewer_writable_outputs_guidance_uses_artifact_directory() {
        let guidance =
            merge_reviewer_writable_outputs_guidance(Path::new("/tmp/factory/merge/reviews/tests"));

        assert!(guidance.contains("CARGO_TARGET_DIR"));
        assert!(guidance.contains("/tmp/factory/merge/reviews/tests/target"));
        assert!(guidance.contains("Factory sets CARGO_TARGET_DIR"));
        assert!(guidance.contains("reviewer artifact directory"));
        assert!(!guidance.contains(".factory/runs/"));
    }
}
