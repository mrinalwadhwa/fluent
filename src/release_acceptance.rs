//! Verify an exact release candidate using retained production evidence.
use crate::work_model::{AttemptStatus, MergeCandidateMergeStatus, TaskStatus, WorkModelStore};
use crate::{git, recovery_contracts, verification_assessment};
use anyhow::{Context, Result, ensure};
use std::path::Path;

/// Read acceptance without running checks, providers, or lifecycle mutations.
pub fn verify(root: &Path, work: &str, attempt_id: &str, commit: &str) -> Result<()> {
    ensure!(
        commit.len() == 40
            && commit
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()),
        "release candidate must be a full lowercase commit SHA"
    );
    ensure!(
        git::run_stdout(root, &["rev-parse", "HEAD"], "resolve release HEAD")? == commit,
        "release candidate differs from current HEAD"
    );
    ensure!(
        git::run_stdout(
            root,
            &["status", "--porcelain", "--untracked-files=all"],
            "inspect release checkout"
        )?
        .is_empty(),
        "release checkout must be clean"
    );
    // Snapshot reads never migrate records or recover pending transactions.
    let item = WorkModelStore::new(root).read_work_item_snapshot(work)?;
    item.validate()?;
    let attempt = item
        .attempts
        .iter()
        .find(|a| a.id == attempt_id)
        .context("release Attempt is missing")?;
    ensure!(
        attempt.status == AttemptStatus::Complete && attempt.completed_at.is_some(),
        "release Attempt has not completed successfully"
    );
    attempt.ensure_not_canceled()?;
    ensure!(
        attempt.current_candidate_commit() == Some(commit),
        "release commit is not the exact reviewed Writer candidate"
    );
    let candidates = item
        .merge_candidates
        .iter()
        .filter(|c| c.attempt_id == attempt_id && c.candidate_commit == commit)
        .collect::<Vec<_>>();
    ensure!(
        candidates.len() == 1,
        "release requires one exact Merge Candidate"
    );
    let candidate = candidates[0];
    candidate.validate(&item)?;
    ensure!(
        candidate.ready_at.is_some()
            && matches!(
                candidate.merge_state.status,
                MergeCandidateMergeStatus::Pending | MergeCandidateMergeStatus::Merged
            ),
        "release Merge Candidate is neither ready nor landed"
    );
    if candidate.merge_state.status == MergeCandidateMergeStatus::Merged {
        ensure!(
            candidate.merge_state.merged_commit.as_deref() == Some(commit),
            "landed release commit differs from the reviewed candidate"
        );
        // Authenticate retained landing artifacts and their digest as well as
        // the stricter original full-run assessment below.
        verification_assessment::assess_landed_tester_history(
            root,
            attempt,
            candidate,
            commit,
            if candidate
                .merge_state
                .check_artifacts
                .iter()
                .any(|a| a.producer_id == "merge-tester")
            {
                verification_assessment::LandedTesterSource::MergeCandidate
            } else {
                verification_assessment::LandedTesterSource::Attempt
            },
        )?;
    }
    ensure!(
        attempt.tasks.iter().all(|task| !matches!(
            task.status,
            TaskStatus::Planned | TaskStatus::Executing | TaskStatus::NeedsUser
        )),
        "release Attempt has unsettled Tasks"
    );
    recovery_contracts::writer_completion::ensure_settled_ownership(root, attempt, None)?;
    // Task areas locate normal worker records; registered local executions
    // also include Learner runs and retries that have no separate Task.
    let mut records = attempt
        .artifacts
        .iter()
        .filter(|artifact| artifact.producer_id == "local-execution")
        .map(|artifact| artifact.path.clone())
        .collect::<std::collections::BTreeSet<_>>();
    for task in &attempt.tasks {
        if let Some(area) = &task.artifact_area {
            let path = format!("{}/execution.json", area.path);
            match std::fs::symlink_metadata(root.join(&path)) {
                Ok(_) => {
                    records.insert(path);
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
    }
    let owner = crate::execution::AttemptExecutionOwner {
        project_root: root.canonicalize()?,
        work_item_id: work.into(),
        attempt_id: attempt_id.into(),
    };
    let prefix = format!(
        "{}/{work}/{attempt_id}/",
        crate::work_model::WORK_ARTIFACTS_DIR
    );
    for record in records {
        ensure!(
            record.starts_with(&prefix) && record.ends_with("/execution.json"),
            "execution record is outside its owning release Attempt"
        );
        let path = verification_assessment::validate_managed_file_path(root, &record)?;
        crate::usage_limit_contracts::unlocked(
            &path.with_file_name("execution-owner.lock"),
            false,
        )?;
        crate::execution::verify_settled(&path, &owner)?;
    }
    recovery_contracts::review_coverage::ensure_landing_review_bindings(root, &item, candidate)?;
    recovery_contracts::validate_completed_downstream_evidence(root, attempt)?;
    let assessment = verification_assessment::assess_canonical_checks(root, attempt)?;
    ensure!(
        assessment.is_satisfied()
            && assessment.preexisting.is_empty()
            && assessment.reconstruction.is_none(),
        "release requires completed canonical proof for every configured command; baseline or reconstructed results cannot replace it"
    );
    let results: crate::tester::TesterResults =
        serde_json::from_slice(&std::fs::read(&assessment.results_path)?)?;
    let commands = results
        .commands
        .iter()
        .map(|c| (c.command.as_str(), c.test_harness.as_str()))
        .collect::<Vec<_>>();
    ensure!(
        results
            .tests
            .iter()
            .filter(|t| t.status == "fail")
            .all(
                |test| verification_assessment::failed_test_covered_by_accepted_command(
                    &commands,
                    &assessment.accepted,
                    test
                )
            ),
        "release has unresolved structured failures; advisory baselines do not satisfy release checks"
    );
    let handoff_ref = attempt
        .learning
        .as_ref()
        .and_then(|learning| learning.handoff.as_ref())
        .context("release Learning handoff is missing")?;
    verification_assessment::validate_managed_file_path(root, &handoff_ref.path)?;
    let handoff = crate::learner::load_verified_handoff(root, handoff_ref)?;
    ensure!(
        handoff.source_work_item_id == work
            && handoff.source_attempt_id == attempt_id
            && handoff.source_merge_candidate_id.as_deref() == Some(candidate.id.as_str()),
        "release Learning handoff belongs to another candidate"
    );
    // Check the source again after reading the proof, before returning authority.
    ensure!(
        git::run_stdout(root, &["rev-parse", "HEAD"], "resolve release HEAD")? == commit
            && git::run_stdout(
                root,
                &["status", "--porcelain", "--untracked-files=all"],
                "recheck release checkout"
            )?
            .is_empty(),
        "release checkout changed during verification"
    );
    Ok(())
}
