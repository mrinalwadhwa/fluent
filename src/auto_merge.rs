use anyhow::{Result, bail};
use std::io::ErrorKind;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::coder::CoderKind;
use crate::content::ContentResolver;
use crate::work_merge_executor::{self, WorkMergeConfig};
use crate::work_model::{
    AttemptReviewState, AttemptStatus, MergeCandidate, MergeCandidateMergeStatus,
    MergeCandidateReviewState, WorkItem, WorkModelStorageError, WorkModelStore,
};

static SHUTDOWN: AtomicBool = AtomicBool::new(false);

pub enum AutoMergeMode {
    Single(String),
    All,
}

pub(crate) enum MergeOutcome {
    Succeeded { commit: String },
    AuthExpired,
    Failed { reason: String },
}

pub fn run(
    project_root: &Path,
    mode: AutoMergeMode,
    poll_seconds: u64,
) -> Result<()> {
    install_signal_handler();
    let store = WorkModelStore::new(project_root);
    let resolver = ContentResolver::new(Some(project_root));

    loop {
        if shutdown_requested() {
            return Ok(());
        }

        let work_items = match &mode {
            AutoMergeMode::Single(id) => match store.read_work_item(id) {
                Ok(item) => vec![item],
                Err(WorkModelStorageError::ReadFile { source, .. })
                    if source.kind() == ErrorKind::NotFound =>
                {
                    bail!("Work Item {id:?} not found");
                }
                Err(error) => return Err(error.into()),
            },
            AutoMergeMode::All => store.list_work_items()?,
        };

        for wi in &work_items {
            if shutdown_requested() {
                return Ok(());
            }

            if let Some(candidate) = find_ready_candidate(wi) {
                let outcome = attempt_merge(
                    project_root,
                    &store,
                    &resolver,
                    &wi.id,
                    &candidate.id,
                );
                match outcome {
                    MergeOutcome::Succeeded { commit } => {
                        eprintln!("[auto-merge] merged {} at {}", wi.id, commit);
                    }
                    MergeOutcome::Failed { reason } => {
                        mark_auto_merge_skipped(&store, &wi.id, &candidate.id);
                        eprintln!(
                            "[auto-merge] skipping {} (merge failed: {})",
                            wi.id, reason
                        );
                    }
                    MergeOutcome::AuthExpired => {
                        eprintln!(
                            "[auto-merge] authentication expired, pausing {}",
                            wi.id
                        );
                    }
                }
            }
        }

        sleep_with_shutdown_check(Duration::from_secs(poll_seconds));
        if shutdown_requested() {
            return Ok(());
        }
    }
}

pub fn find_ready_candidate(wi: &WorkItem) -> Option<&MergeCandidate> {
    let attempt = wi.attempts.last()?;

    if attempt.status != AttemptStatus::Complete {
        return None;
    }
    if attempt.review_state != Some(AttemptReviewState::Passed) {
        return None;
    }

    let candidate = wi
        .merge_candidates
        .iter()
        .find(|c| c.attempt_id == attempt.id)?;

    if candidate.merge_state.status != MergeCandidateMergeStatus::Pending {
        return None;
    }
    if candidate.review_state != MergeCandidateReviewState::Passed {
        return None;
    }
    if candidate.merge_state.auto_merge_skipped == Some(true) {
        return None;
    }

    Some(candidate)
}

fn attempt_merge(
    project_root: &Path,
    store: &WorkModelStore,
    resolver: &ContentResolver,
    work_item_id: &str,
    merge_candidate_id: &str,
) -> MergeOutcome {
    let result = work_merge_executor::merge_candidate(WorkMergeConfig {
        project_root,
        store,
        work_item_id,
        merge_candidate_id,
        resolver,
        extra_args: &[],
        coder_kind: CoderKind::Claude,
        no_sandbox: false,
    });

    classify_merge_outcome(result)
}

pub(crate) fn classify_merge_outcome(
    result: Result<work_merge_executor::WorkMergeOutcome>,
) -> MergeOutcome {
    match result {
        Ok(outcome) => MergeOutcome::Succeeded {
            commit: outcome.merged_commit,
        },
        Err(err) => {
            let msg = format!("{err}");
            if is_auth_error(&msg) {
                MergeOutcome::AuthExpired
            } else {
                MergeOutcome::Failed { reason: msg }
            }
        }
    }
}

fn is_auth_error(msg: &str) -> bool {
    let lower = msg.to_lowercase();
    lower.contains("401")
        || lower.contains("invalid authentication")
        || lower.contains("authentication_failed")
        || lower.contains("authentication expired")
}

fn mark_auto_merge_skipped(store: &WorkModelStore, work_item_id: &str, candidate_id: &str) {
    let result = (|| -> Result<()> {
        let mut item = store.read_work_item(work_item_id)?;
        if let Some(candidate) = item
            .merge_candidates
            .iter_mut()
            .find(|c| c.id == candidate_id)
        {
            candidate.merge_state.auto_merge_skipped = Some(true);
        }
        store.write_work_item(&item)?;
        Ok(())
    })();
    if let Err(err) = result {
        eprintln!(
            "[auto-merge] warning: failed to mark {work_item_id} as skipped: {err}"
        );
    }
}

fn install_signal_handler() {
    ctrlc::set_handler(|| {
        SHUTDOWN.store(true, Ordering::Release);
    })
    .expect("install signal handler");
}

fn shutdown_requested() -> bool {
    SHUTDOWN.load(Ordering::Acquire)
}

fn sleep_with_shutdown_check(duration: Duration) {
    let one_second = Duration::from_secs(1);
    let mut remaining = duration;
    while remaining > Duration::ZERO {
        if shutdown_requested() {
            return;
        }
        let sleep_for = remaining.min(one_second);
        std::thread::sleep(sleep_for);
        remaining = remaining.saturating_sub(sleep_for);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::work_model::{
        Attempt, AttemptKind, MergeCandidateMergeState, Task, TaskKind, TaskOutput, TaskStatus,
        WorkItem, WorkspaceAccess, WorkspaceRef,
    };

    fn make_work_item_with_candidate(
        review_state: AttemptReviewState,
        attempt_status: AttemptStatus,
        candidate_review_state: MergeCandidateReviewState,
        merge_status: MergeCandidateMergeStatus,
        auto_merge_skipped: Option<bool>,
    ) -> WorkItem {
        let mut item = WorkItem {
            id: "wi-1".to_string(),
            title: "Test".to_string(),
            planning_context: None,
            instructions: None,
            abandonment: None,
            attempts: vec![Attempt {
                id: "attempt-1".to_string(),
                work_item_id: "wi-1".to_string(),
                kind: AttemptKind::Write,
                status: attempt_status,
                tasks: vec![Task {
                    id: "attempt-1-write-1".to_string(),
                    kind: TaskKind::Write,
                    status: TaskStatus::Complete,
                    role: "author".to_string(),
                    instructions: None,
                    work_item_id: "wi-1".to_string(),
                    attempt_id: Some("attempt-1".to_string()),
                    workspace_access: WorkspaceAccess {
                        reads: vec![],
                        writes: vec![WorkspaceRef {
                            id: "candidate".to_string(),
                            path: "work/wi-1/attempt-1".to_string(),
                        }],
                    },
                    artifact_area: None,
                    review_context: None,
                    input_artifacts: vec![],
                    depends_on: None,
                    output: Some(TaskOutput {
                        workspace_id: "candidate".to_string(),
                        workspace_path: "work/wi-1/attempt-1".to_string(),
                        source_branch: "main".to_string(),
                        commit: "abc123".to_string(),
                    }),
                    created_at: None,
                    started_at: None,
                    completed_at: None,
                }],
                review_state: Some(review_state),
                artifacts: vec![],
                created_at: None,
                completed_at: None,
            }],
            merge_candidates: vec![],
        };
        item.merge_candidates.push(MergeCandidate {
            id: "attempt-1-merge-candidate".to_string(),
            attempt_id: "attempt-1".to_string(),
            source_workspace: WorkspaceRef {
                id: "candidate".to_string(),
                path: "work/wi-1/attempt-1".to_string(),
            },
            target_workspace: WorkspaceRef {
                id: "target".to_string(),
                path: ".".to_string(),
            },
            source_branch: "main".to_string(),
            target_branch: "main".to_string(),
            candidate_commit: "abc123".to_string(),
            review_state: candidate_review_state,
            merge_state: MergeCandidateMergeState {
                status: merge_status,
                merged_commit: None,
                failure_reason: None,
                check_artifacts: vec![],
                review_artifacts: vec![],
                auto_merge_skipped,
            },
            created_at: None,
            started_at: None,
            completed_at: None,
        });
        item
    }

    #[test]
    fn find_ready_candidate_returns_some_when_attempt_passed_and_candidate_pending() {
        let wi = make_work_item_with_candidate(
            AttemptReviewState::Passed,
            AttemptStatus::Complete,
            MergeCandidateReviewState::Passed,
            MergeCandidateMergeStatus::Pending,
            None,
        );
        assert!(find_ready_candidate(&wi).is_some());
    }

    #[test]
    fn find_ready_candidate_returns_none_when_candidate_already_merged() {
        let wi = make_work_item_with_candidate(
            AttemptReviewState::Passed,
            AttemptStatus::Complete,
            MergeCandidateReviewState::Passed,
            MergeCandidateMergeStatus::Merged,
            None,
        );
        assert!(find_ready_candidate(&wi).is_none());
    }

    #[test]
    fn find_ready_candidate_returns_none_when_auto_merge_skipped() {
        let wi = make_work_item_with_candidate(
            AttemptReviewState::Passed,
            AttemptStatus::Complete,
            MergeCandidateReviewState::Passed,
            MergeCandidateMergeStatus::Pending,
            Some(true),
        );
        assert!(find_ready_candidate(&wi).is_none());
    }

    #[test]
    fn find_ready_candidate_returns_none_for_needs_user_candidate() {
        let wi = make_work_item_with_candidate(
            AttemptReviewState::Passed,
            AttemptStatus::Complete,
            MergeCandidateReviewState::Pending,
            MergeCandidateMergeStatus::Pending,
            None,
        );
        assert!(find_ready_candidate(&wi).is_none());
    }

    #[test]
    fn find_ready_candidate_uses_latest_attempt_only() {
        let mut wi = make_work_item_with_candidate(
            AttemptReviewState::Passed,
            AttemptStatus::Complete,
            MergeCandidateReviewState::Passed,
            MergeCandidateMergeStatus::Pending,
            None,
        );
        // Add a second attempt that is not complete — the candidate is from attempt-1
        // but the latest attempt is attempt-2 which is not passed.
        wi.attempts.push(Attempt {
            id: "attempt-2".to_string(),
            work_item_id: "wi-1".to_string(),
            kind: AttemptKind::Write,
            status: AttemptStatus::Executing,
            tasks: vec![],
            review_state: None,
            artifacts: vec![],
            created_at: None,
            completed_at: None,
        });
        assert!(find_ready_candidate(&wi).is_none());
    }

    #[test]
    fn find_ready_candidate_returns_none_when_no_attempts() {
        let wi = WorkItem {
            id: "wi-1".to_string(),
            title: "Test".to_string(),
            planning_context: None,
            instructions: None,
            abandonment: None,
            attempts: vec![],
            merge_candidates: vec![],
        };
        assert!(find_ready_candidate(&wi).is_none());
    }

    #[test]
    fn find_ready_candidate_returns_none_when_attempt_not_passed() {
        let wi = make_work_item_with_candidate(
            AttemptReviewState::Failed,
            AttemptStatus::Complete,
            MergeCandidateReviewState::Passed,
            MergeCandidateMergeStatus::Pending,
            None,
        );
        assert!(find_ready_candidate(&wi).is_none());
    }

    #[test]
    fn classify_merge_outcome_recognizes_401_as_auth_expired() {
        let err = anyhow::anyhow!("request failed with status 401 Unauthorized");
        assert!(matches!(classify_merge_outcome(Err(err)), MergeOutcome::AuthExpired));
    }

    #[test]
    fn classify_merge_outcome_recognizes_invalid_authentication_phrase() {
        let err = anyhow::anyhow!("Invalid authentication credentials provided");
        assert!(matches!(classify_merge_outcome(Err(err)), MergeOutcome::AuthExpired));
    }

    #[test]
    fn classify_merge_outcome_recognizes_authentication_failed() {
        let err = anyhow::anyhow!("authentication_failed: token expired");
        assert!(matches!(classify_merge_outcome(Err(err)), MergeOutcome::AuthExpired));
    }

    #[test]
    fn classify_merge_outcome_treats_other_errors_as_failed() {
        let err = anyhow::anyhow!("rebase conflict in src/main.rs");
        match classify_merge_outcome(Err(err)) {
            MergeOutcome::Failed { reason } => {
                assert!(reason.contains("rebase conflict"));
            }
            other => panic!("expected Failed, got {:?}", match other {
                MergeOutcome::Succeeded { .. } => "Succeeded",
                MergeOutcome::AuthExpired => "AuthExpired",
                MergeOutcome::Failed { .. } => "Failed",
            }),
        }
    }

    #[test]
    fn classify_merge_outcome_succeeds_on_ok() {
        let outcome = work_merge_executor::WorkMergeOutcome {
            merge_candidate_id: "mc-1".to_string(),
            merged_commit: "abc123".to_string(),
        };
        match classify_merge_outcome(Ok(outcome)) {
            MergeOutcome::Succeeded { commit } => assert_eq!(commit, "abc123"),
            _ => panic!("expected Succeeded"),
        }
    }
}
