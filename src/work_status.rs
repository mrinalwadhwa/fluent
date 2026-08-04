use std::collections::HashSet;
use std::fs;
use std::path::Path;

use crate::work_model::{
    Attempt, AttemptReviewState, AttemptStatus, MergeCandidate, MergeCandidateMergeStatus,
    MergeReviewState, Task, TaskKind, TaskStatus, WorkItem, WorkModelStore,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkStatus {
    pub rows: Vec<WorkItemStatus>,
    pub errors: Vec<String>,
}

impl WorkStatus {
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty() && self.errors.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkItemStatus {
    pub id: String,
    pub title: String,
    pub attempt: String,
    pub task: String,
    pub review: String,
    pub merge_candidate: String,
    pub merge: String,
    pub action: String,
    /// An exact operator command for exceptional recovery states.
    pub next_action: Option<String>,
    pub metrics: WorkMetrics,
    pub compatibility_warnings: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct WorkMetrics {
    pub review_rounds: usize,
    pub stage_duration_ms: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub repeated_findings: usize,
    pub artifact_bytes: u64,
    pub avoided_cycles: usize,
}

pub fn load_work_status(project_root: &Path) -> Result<WorkStatus, anyhow::Error> {
    let store = WorkModelStore::new(project_root);
    let mut rows = Vec::new();
    let mut errors = Vec::new();

    for result in store.list_work_item_results()? {
        match result {
            Ok(item) => rows.push(summarize_work_item(&item, Some(project_root))),
            Err(error) => errors.push(error.to_string()),
        }
    }

    Ok(WorkStatus { rows, errors })
}

pub fn summarize_work_item(item: &WorkItem, project_root: Option<&Path>) -> WorkItemStatus {
    let attempt = item.attempts.last();
    let merge_candidate = attempt.and_then(|attempt| {
        item.merge_candidates
            .iter()
            .rev()
            .find(|candidate| candidate.attempt_id == attempt.id)
    });

    WorkItemStatus {
        id: item.id.clone(),
        title: item.title.clone(),
        attempt: attempt
            .map(format_attempt)
            .unwrap_or_else(|| "-".to_string()),
        task: attempt
            .and_then(select_task)
            .map(|task| format_task_with_liveness(task, item, project_root))
            .unwrap_or_else(|| "-".to_string()),
        review: attempt
            .and_then(|attempt| attempt.review_state.as_ref())
            .map(attempt_review_label)
            .unwrap_or("-")
            .to_string(),
        merge_candidate: merge_candidate
            .map(|candidate| candidate.id.clone())
            .unwrap_or_else(|| "-".to_string()),
        merge: merge_candidate
            .map(format_merge_state)
            .unwrap_or_else(|| "-".to_string()),
        action: action_label_with_liveness(item, attempt, merge_candidate, project_root)
            .to_string(),
        next_action: evidence_recovery_next_action(attempt),
        metrics: work_metrics(item, project_root),
        compatibility_warnings: item.compatibility_warnings(),
    }
}

pub fn work_metrics(item: &WorkItem, project_root: Option<&Path>) -> WorkMetrics {
    let usage_rows = project_root
        .map(|root| local_usage_rows(item, root))
        .unwrap_or_default();
    compute_work_metrics(item, project_root, &usage_rows)
}

pub fn work_item_show_value(
    item: &WorkItem,
    project_root: &Path,
) -> serde_json::Result<serde_json::Value> {
    let mut output = serde_json::to_value(item)?;
    output["metrics"] = serde_json::to_value(work_metrics(item, Some(project_root)))?;
    let warnings = item.compatibility_warnings();
    if !warnings.is_empty() {
        output["compatibility-warnings"] = serde_json::to_value(warnings)?;
    }
    Ok(output)
}

fn compute_work_metrics(
    item: &WorkItem,
    project_root: Option<&Path>,
    usage_rows: &[crate::usage::UsageRow],
) -> WorkMetrics {
    let mut metrics = WorkMetrics {
        review_rounds: item
            .attempts
            .iter()
            .flat_map(|attempt| &attempt.tasks)
            .filter(|task| task.kind == TaskKind::Tester)
            .count(),
        avoided_cycles: item
            .attempts
            .iter()
            .flat_map(|attempt| &attempt.writer_runs)
            .filter(|run| run.kind == crate::work_model::WriterRunKind::PreReviewContinuation)
            .count(),
        ..Default::default()
    };

    metrics.stage_duration_ms = item
        .attempts
        .iter()
        .flat_map(|attempt| &attempt.tasks)
        .filter_map(task_duration_ms)
        .fold(0_u64, u64::saturating_add);

    for row in usage_rows.iter().filter(|row| row.work_item_id == item.id) {
        metrics.input_tokens = metrics.input_tokens.saturating_add(row.input_tokens);
        metrics.output_tokens = metrics.output_tokens.saturating_add(row.output_tokens);
    }

    let Some(project_root) = project_root else {
        return metrics;
    };
    let artifact_root = project_root
        .join(crate::work_model::WORK_ARTIFACTS_DIR)
        .join(&item.id);
    if artifact_root.is_dir() {
        metrics.artifact_bytes = crate::prep::logical_bytes(&artifact_root).unwrap_or(0);
    }

    let mut findings_seen = HashSet::new();
    for task in item
        .attempts
        .iter()
        .flat_map(|attempt| &attempt.tasks)
        .filter(|task| task.kind == TaskKind::Review)
    {
        let Some(area) = task.artifact_area.as_ref() else {
            continue;
        };
        let path = project_root.join(&area.path).join("review.md");
        let source = fs::read_to_string(path).unwrap_or_default();
        for title in crate::review::open_finding_titles(&source) {
            let identity = crate::review::finding_identity(&task.role, &title);
            if !findings_seen.insert(identity) {
                metrics.repeated_findings += 1;
            }
        }
    }
    metrics
}

fn task_duration_ms(task: &Task) -> Option<u64> {
    let started = chrono::DateTime::parse_from_rfc3339(task.started_at.as_deref()?).ok()?;
    let completed = chrono::DateTime::parse_from_rfc3339(task.completed_at.as_deref()?).ok()?;
    u64::try_from((completed - started).num_milliseconds()).ok()
}

fn local_usage_rows(item: &WorkItem, project_root: &Path) -> Vec<crate::usage::UsageRow> {
    let root = project_root
        .join(crate::work_model::WORK_ARTIFACTS_DIR)
        .join(&item.id);
    let mut rows = Vec::new();
    collect_local_usage_rows(&root, item, &mut rows);
    rows
}

fn collect_local_usage_rows(
    directory: &Path,
    item: &WorkItem,
    rows: &mut Vec<crate::usage::UsageRow>,
) {
    let usage_path = directory.join("usage.json");
    if usage_path.is_file() {
        let local_rows = fs::read_to_string(&usage_path)
            .ok()
            .and_then(|source| serde_json::from_str::<Vec<crate::usage::UsageRow>>(&source).ok())
            .unwrap_or_default();
        rows.extend(
            local_rows
                .into_iter()
                .filter(|row| row.work_item_id == item.id),
        );
    }

    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            collect_local_usage_rows(&entry.path(), item, rows);
        }
    }
}

pub fn format_work_status(status: &WorkStatus) -> String {
    let mut output = String::new();
    output.push_str("Work Items\n");
    if status.is_empty() {
        output.push_str("No Work Items found\n");
        return output;
    }

    if !status.rows.is_empty() {
        output.push_str(&format!(
            "{:<20} {:<24} {:<28} {:<28} {:<14} {:<28} {:<12} {}\n",
            "WORK", "ATTEMPT", "TASK", "MERGE CANDIDATE", "REVIEW", "MERGE", "ACTION", "TITLE"
        ));
        output.push_str(&format!(
            "{:<20} {:<24} {:<28} {:<28} {:<14} {:<28} {:<12} {}\n",
            "----", "-------", "----", "---------------", "------", "-----", "------", "-----"
        ));
        for row in &status.rows {
            output.push_str(&format!(
                "{:<20} {:<24} {:<28} {:<28} {:<14} {:<28} {:<12} {}\n",
                row.id,
                row.attempt,
                row.task,
                row.merge_candidate,
                row.review,
                row.merge,
                row.action,
                row.title
            ));
            for warning in &row.compatibility_warnings {
                output.push_str(&format!("  warning: {warning}\n"));
            }
            output.push_str(&format!(
                "  metrics: rounds:{} duration:{}ms tokens:{}/{} repeated:{} artifacts:{}B avoided:{}\n",
                row.metrics.review_rounds,
                row.metrics.stage_duration_ms,
                row.metrics.input_tokens,
                row.metrics.output_tokens,
                row.metrics.repeated_findings,
                row.metrics.artifact_bytes,
                row.metrics.avoided_cycles
            ));
        }
    }

    if !status.errors.is_empty() {
        if !status.rows.is_empty() {
            output.push('\n');
        }
        output.push_str("Work Item read errors\n");
        for error in &status.errors {
            output.push_str(&format!("- {error}\n"));
        }
    }

    output
}

pub fn format_work_dashboard_lines(status: &WorkStatus) -> Vec<String> {
    if status.is_empty() {
        return vec!["No Work Items found".to_string()];
    }

    let mut lines = Vec::new();
    for row in &status.rows {
        lines.push(format!("{} - {} [{}]", row.id, row.title, row.action));
        lines.push(format!("  Attempt: {}", row.attempt));
        lines.push(format!("  Task: {}", row.task));
        lines.push(format!("  Review: {}", row.review));
        lines.push(format!("  Merge Candidate: {}", row.merge_candidate));
        lines.push(format!("  Merge: {}", row.merge));
        lines.push(format!(
            "  Metrics: rounds {} · duration {}ms · tokens {}/{} · repeated {} · artifacts {}B · avoided {}",
            row.metrics.review_rounds,
            row.metrics.stage_duration_ms,
            row.metrics.input_tokens,
            row.metrics.output_tokens,
            row.metrics.repeated_findings,
            row.metrics.artifact_bytes,
            row.metrics.avoided_cycles
        ));
        for warning in &row.compatibility_warnings {
            lines.push(format!("  Warning: {warning}"));
        }
        lines.push(String::new());
    }
    if !status.errors.is_empty() {
        lines.push("Work Item read errors".to_string());
        for error in &status.errors {
            lines.push(format!("  {error}"));
        }
    }
    lines
}

fn select_task(attempt: &Attempt) -> Option<&Task> {
    attempt
        .tasks
        .iter()
        .find(|task| matches!(task.status, TaskStatus::Executing | TaskStatus::NeedsUser))
        .or_else(|| {
            attempt
                .tasks
                .iter()
                .find(|task| task.status == TaskStatus::Planned)
        })
        .or_else(|| {
            attempt
                .tasks
                .iter()
                .rev()
                .find(|task| task.status == TaskStatus::Failed)
        })
        .or_else(|| attempt.tasks.last())
}

fn format_attempt(attempt: &Attempt) -> String {
    let pause = attempt
        .pause_kind
        .as_ref()
        .map(|kind| format!("; pause:{}", kind.as_str()))
        .unwrap_or_default();
    if attempt.writer_runs.is_empty() {
        return format!(
            "{} [{}{}]",
            attempt.id,
            attempt_status_label(&attempt.status),
            pause
        );
    }
    let initial = attempt
        .writer_runs
        .iter()
        .filter(|run| run.kind == crate::work_model::WriterRunKind::Initial)
        .count();
    let pre_review = attempt
        .writer_runs
        .iter()
        .filter(|run| run.kind == crate::work_model::WriterRunKind::PreReviewContinuation)
        .count();
    let corrective = attempt
        .writer_runs
        .iter()
        .filter(|run| run.kind == crate::work_model::WriterRunKind::Corrective)
        .count();
    let last = attempt
        .writer_runs
        .last()
        .expect("writer runs is not empty");
    format!(
        "{} [{}{}; writers initial:{initial} pre-review:{pre_review} corrective:{corrective}; last:{}]",
        attempt.id,
        attempt_status_label(&attempt.status),
        pause,
        last.outcome.as_str()
    )
}

fn format_task_with_liveness(task: &Task, item: &WorkItem, project_root: Option<&Path>) -> String {
    format!(
        "{}:{} [{}]",
        task_kind_label(task.kind),
        task.id,
        effective_task_status_label(task, item, project_root)
    )
}

fn effective_task_status_label(
    task: &Task,
    item: &WorkItem,
    project_root: Option<&Path>,
) -> &'static str {
    if task.status == TaskStatus::Executing {
        if let Some(root) = project_root {
            let lock_path = crate::lease::task_lock_path(root, &item.id, &task.id);
            if !crate::lease::is_leased(&lock_path) {
                return "interrupted";
            }
        }
    }
    task.status.as_str()
}

fn is_task_live_executing(task: &Task, item: &WorkItem, project_root: Option<&Path>) -> bool {
    task.status == TaskStatus::Executing
        && match project_root {
            Some(root) => {
                let lock_path = crate::lease::task_lock_path(root, &item.id, &task.id);
                crate::lease::is_leased(&lock_path)
            }
            None => true,
        }
}

fn format_merge_state(candidate: &MergeCandidate) -> String {
    let status = merge_status_label(&candidate.merge_state.status);
    let review = merge_review_label(&candidate.merge_review_state);
    format!("{status} review:{review}")
}

fn action_label_with_liveness(
    item: &WorkItem,
    attempt: Option<&Attempt>,
    merge_candidate: Option<&MergeCandidate>,
    project_root: Option<&Path>,
) -> &'static str {
    if item.abandonment.is_some() {
        return "abandoned";
    }

    if let Some(attempt) = attempt {
        if attempt.status == AttemptStatus::NeedsUser
            && attempt.evidence_recoveries.last().is_some_and(|recovery| {
                recovery.state == crate::work_model::EvidenceRecoveryState::NeedsEvidence
            })
        {
            return "evidence-needed";
        }
        if attempt.status == AttemptStatus::NeedsUser
            || attempt
                .tasks
                .iter()
                .any(|task| task.status == TaskStatus::NeedsUser)
        {
            return "needs-user";
        }
        if attempt
            .tasks
            .iter()
            .any(|task| is_task_live_executing(task, item, project_root))
        {
            return "executing";
        }
        let has_reclaimable = attempt.tasks.iter().any(|task| {
            task.status == TaskStatus::Executing
                && !is_task_live_executing(task, item, project_root)
        });
        if has_reclaimable
            || attempt
                .tasks
                .iter()
                .any(|task| task.status == TaskStatus::Planned)
        {
            return "task-ready";
        }
        if attempt.status == AttemptStatus::Failed
            || attempt
                .tasks
                .iter()
                .any(|task| task.status == TaskStatus::Failed)
            || attempt.review_state == Some(AttemptReviewState::Failed)
        {
            return "failed";
        }
    }

    if let Some(candidate) = merge_candidate {
        return match candidate.merge_state.status {
            MergeCandidateMergeStatus::NeedsUser => "needs-user",
            MergeCandidateMergeStatus::Executing => "merging",
            MergeCandidateMergeStatus::Failed => "merge-failed",
            MergeCandidateMergeStatus::Merged => "merged",
            MergeCandidateMergeStatus::Pending
                if item
                    .attempt_learning_advancement(&candidate.attempt_id)
                    .is_ok() =>
            {
                "merge-ready"
            }
            MergeCandidateMergeStatus::Pending
                if attempt
                    .and_then(|attempt| attempt.learning.as_ref())
                    .is_some_and(|learning| !learning.is_relaunchable()) =>
            {
                "learner-blocked"
            }
            MergeCandidateMergeStatus::Pending => "learner-not-ready",
        };
    }

    match attempt.map(|attempt| &attempt.status) {
        Some(AttemptStatus::Complete) => "complete",
        Some(AttemptStatus::Reviewing) => "reviewing",
        Some(AttemptStatus::Executing) => "executing",
        Some(AttemptStatus::Planned) => "planned",
        Some(AttemptStatus::Failed) => "failed",
        Some(AttemptStatus::NeedsUser) => "needs-user",
        None => "not-started",
    }
}

fn evidence_recovery_next_action(attempt: Option<&Attempt>) -> Option<String> {
    let attempt = attempt?;
    let recovery = attempt.evidence_recoveries.last()?;
    if attempt.status != AttemptStatus::NeedsUser
        || recovery.state != crate::work_model::EvidenceRecoveryState::NeedsEvidence
    {
        return None;
    }
    let artifacts = recovery
        .targets
        .iter()
        .map(|target| format!(" --review-artifact {}", target.prior_review_artifact))
        .collect::<String>();
    Some(format!(
        "fluent attempt evidence attach {} {} --candidate {} --evidence-file <path>{artifacts}",
        attempt.work_item_id, attempt.id, recovery.candidate_commit
    ))
}

fn attempt_status_label(status: &AttemptStatus) -> &'static str {
    status.as_str()
}

fn attempt_review_label(review: &AttemptReviewState) -> &'static str {
    review.as_str()
}

fn task_kind_label(kind: TaskKind) -> &'static str {
    kind.as_str()
}

fn merge_review_label(status: &MergeReviewState) -> &'static str {
    match status {
        MergeReviewState::Pending => "pending",
        MergeReviewState::Reviewing => "reviewing",
        MergeReviewState::Passed => "passed",
        MergeReviewState::Failed => "failed",
    }
}

fn merge_status_label(status: &MergeCandidateMergeStatus) -> &'static str {
    match status {
        MergeCandidateMergeStatus::Pending => "pending",
        MergeCandidateMergeStatus::Executing => "executing",
        MergeCandidateMergeStatus::Failed => "failed",
        MergeCandidateMergeStatus::NeedsUser => "needs-user",
        MergeCandidateMergeStatus::Merged => "merged",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::work_model::{
        AttemptLearning, LearningFailureKind, MergeCandidateMergeState, TaskOutput, WorkItem,
        WorkspaceAccess, WorkspaceRef,
    };

    fn passed_attempt_with_candidate(learning: Option<AttemptLearning>) -> (WorkItem, String) {
        let mut item = WorkItem {
            id: "work-1".to_string(),
            title: "Build status view".to_string(),
            planning_context: None,
            instructions: None,
            abandonment: None,
            post_merge_review_fix_depth: None,
            attempts: Vec::new(),
            merge_candidates: Vec::new(),
            ..Default::default()
        };
        item.add_initial_attempt("attempt-1").unwrap();
        let attempt = item.attempts.last_mut().unwrap();
        let task = attempt.tasks.last_mut().unwrap();
        task.status = TaskStatus::Complete;
        task.workspace_access = WorkspaceAccess {
            reads: vec![WorkspaceRef {
                id: "target".to_string(),
                path: ".".to_string(),
            }],
            writes: vec![WorkspaceRef {
                id: "candidate".to_string(),
                path: "../work-6-work-1-attempt-1".to_string(),
            }],
        };
        task.output = Some(TaskOutput {
            workspace_id: "candidate".to_string(),
            workspace_path: "../work-6-work-1-attempt-1".to_string(),
            source_branch: "main".to_string(),
            base_commit: None,
            commit: "abc123".to_string(),
            no_change: None,
            learner_canonicalization: None,
        });
        attempt.status = AttemptStatus::Complete;
        attempt.review_state = Some(AttemptReviewState::Passed);
        attempt.learning = learning;
        let candidate_id = item.create_or_get_merge_candidate("attempt-1").unwrap();
        (item, candidate_id)
    }

    #[test]
    fn summarize_planned_work_item_shows_ready_task() {
        let mut item = WorkItem {
            id: "work-1".to_string(),
            title: "Build status view".to_string(),
            planning_context: None,
            instructions: None,
            abandonment: None,
            post_merge_review_fix_depth: None,
            attempts: Vec::new(),
            merge_candidates: Vec::new(),
            ..Default::default()
        };
        item.add_initial_attempt("attempt-1").unwrap();

        let row = summarize_work_item(&item, None);

        assert_eq!(row.attempt, "attempt-1 [planned]");
        assert_eq!(row.task, "write:attempt-1-write-1 [planned]");
        assert_eq!(row.review, "-");
        assert_eq!(row.action, "task-ready");
    }

    #[test]
    fn unknown_pause_is_visible_with_upgrade_warning() {
        let mut item = WorkItem {
            id: "future-work".to_string(),
            title: "Future state".to_string(),
            ..Default::default()
        };
        item.add_initial_attempt("attempt-1").unwrap();
        item.attempts[0].status = AttemptStatus::NeedsUser;
        item.attempts[0].pause_kind = Some(crate::work_model::PauseKind::Unknown(
            "future-pause".to_string(),
        ));

        let row = summarize_work_item(&item, None);
        let status = WorkStatus {
            rows: vec![row],
            errors: Vec::new(),
        };
        let output = format_work_status(&status);

        assert!(output.contains("pause:future-pause"));
        assert!(output.contains("upgrade Fluent before mutation"));
        let show = work_item_show_value(&item, Path::new(".")).unwrap();
        assert_eq!(show["attempts"][0]["pause_kind"], "future-pause");
        assert!(
            show["compatibility-warnings"][0]
                .as_str()
                .unwrap()
                .contains("upgrade Fluent")
        );
    }

    #[test]
    fn metrics_are_derived_from_local_work_evidence() {
        let tmp = tempfile::tempdir().unwrap();
        let mut item = WorkItem {
            id: "metrics-work".to_string(),
            title: "Measure cycle cost".to_string(),
            ..Default::default()
        };
        item.add_initial_attempt("attempt-1").unwrap();
        let attempt = &mut item.attempts[0];
        attempt.tasks[0].started_at = Some("2026-08-03T10:00:00Z".to_string());
        attempt.tasks[0].completed_at = Some("2026-08-03T10:00:02Z".to_string());
        attempt.writer_runs.push(crate::work_model::WriterRun {
            task_id: attempt.tasks[0].id.clone(),
            outcome: crate::work_model::WriterOutcome::Continue,
            kind: crate::work_model::WriterRunKind::PreReviewContinuation,
            provider: "codex".to_string(),
            session_id: Some("thread-1".to_string()),
            continuation: 1,
            checked_required: 1,
            candidate_commit: "abc123".to_string(),
        });

        let mut tester = attempt.tasks[0].clone();
        tester.id = "attempt-1-test-1".to_string();
        tester.kind = TaskKind::Tester;
        tester.started_at = None;
        tester.completed_at = None;
        let mut first_review = tester.clone();
        first_review.id = "attempt-1-review-tests-1".to_string();
        first_review.kind = TaskKind::Review;
        first_review.role = "tests".to_string();
        first_review.artifact_area = Some(crate::work_model::TaskArtifactArea {
            path: format!(
                "{}/metrics-work/attempt-1/attempt-1-review-tests-1",
                crate::work_model::WORK_ARTIFACTS_DIR
            ),
        });
        let mut second_review = first_review.clone();
        second_review.id = "attempt-1-review-tests-2".to_string();
        second_review.artifact_area = Some(crate::work_model::TaskArtifactArea {
            path: format!(
                "{}/metrics-work/attempt-1/attempt-1-review-tests-2",
                crate::work_model::WORK_ARTIFACTS_DIR
            ),
        });
        attempt.tasks.extend([tester, first_review, second_review]);

        for task_id in ["attempt-1-review-tests-1", "attempt-1-review-tests-2"] {
            let dir = tmp
                .path()
                .join(crate::work_model::WORK_ARTIFACTS_DIR)
                .join("metrics-work/attempt-1")
                .join(task_id);
            fs::create_dir_all(&dir).unwrap();
            fs::write(
                dir.join("review.md"),
                "Verdict: fail\n\n- [ ] Fix stable boundary (blocking)\n",
            )
            .unwrap();
        }
        let usage_dir = tmp
            .path()
            .join(crate::work_model::WORK_ARTIFACTS_DIR)
            .join("metrics-work/attempt-1/attempt-1-write-1");
        fs::create_dir_all(&usage_dir).unwrap();
        fs::write(
            usage_dir.join("usage.json"),
            r#"[{"ts":"2026-08-03T10:00:02Z","coder":"codex","work_item_id":"metrics-work","attempt_id":"attempt-1","task_id":"attempt-1-write-1","model":"gpt-5","input_tokens":120,"output_tokens":30,"cached_input_tokens":0}]"#,
        )
        .unwrap();

        let metrics = work_metrics(&item, Some(tmp.path()));

        assert_eq!(metrics.review_rounds, 1);
        assert_eq!(metrics.stage_duration_ms, 2_000);
        assert_eq!(metrics.input_tokens, 120);
        assert_eq!(metrics.output_tokens, 30);
        assert_eq!(metrics.repeated_findings, 1);
        assert!(metrics.artifact_bytes > 0);
        assert_eq!(metrics.avoided_cycles, 1);
        let show = work_item_show_value(&item, tmp.path()).unwrap();
        assert_eq!(show["metrics"]["stage-duration-ms"], 2_000);
        assert_eq!(show["metrics"]["input-tokens"], 120);
    }

    #[test]
    fn attempt_status_shows_writer_continuations_separately() {
        let mut attempt = Attempt::default();
        attempt.id = "attempt-1".to_string();
        for (kind, continuation, outcome) in [
            (
                crate::work_model::WriterRunKind::Initial,
                0,
                crate::work_model::WriterOutcome::Continue,
            ),
            (
                crate::work_model::WriterRunKind::PreReviewContinuation,
                1,
                crate::work_model::WriterOutcome::Continue,
            ),
            (
                crate::work_model::WriterRunKind::Corrective,
                1,
                crate::work_model::WriterOutcome::Complete,
            ),
        ] {
            attempt.writer_runs.push(crate::work_model::WriterRun {
                task_id: format!("writer-{continuation}"),
                outcome,
                kind,
                provider: "codex".to_string(),
                session_id: Some("thread-1".to_string()),
                continuation,
                checked_required: 1,
                candidate_commit: "abc123".to_string(),
            });
        }

        let rendered = format_attempt(&attempt);

        assert!(rendered.contains("pre-review:1"));
        assert!(rendered.contains("corrective:1"));
        assert!(rendered.contains("last:complete"));
    }

    #[test]
    fn summarize_passed_attempt_shows_merge_ready_candidate() {
        let learning = AttemptLearning::succeeded(
            1,
            crate::follow_up::ArtifactRef {
                path: "handoff.json".to_string(),
                digest: "sha256:x".to_string(),
            },
        );
        let (item, candidate_id) = passed_attempt_with_candidate(Some(learning));

        let row = summarize_work_item(&item, None);

        assert_eq!(row.merge_candidate, candidate_id);
        assert_eq!(row.merge, "pending review:pending");
        assert_eq!(row.action, "merge-ready");
    }

    #[test]
    fn summarize_passed_attempt_without_learning_is_not_merge_ready() {
        let (item, _) = passed_attempt_with_candidate(None);

        let row = summarize_work_item(&item, None);

        assert_eq!(row.action, "learner-not-ready");
    }

    #[test]
    fn summarize_passed_attempt_with_running_learning_is_not_merge_ready() {
        let (item, _) = passed_attempt_with_candidate(Some(AttemptLearning::in_progress(1)));

        let row = summarize_work_item(&item, None);

        assert_eq!(row.action, "learner-not-ready");
    }

    #[test]
    fn summarize_passed_attempt_with_handoff_pending_learning_is_not_merge_ready() {
        let (item, _) = passed_attempt_with_candidate(Some(AttemptLearning::handoff_pending(1)));

        let row = summarize_work_item(&item, None);

        assert_eq!(row.action, "learner-not-ready");
    }

    #[test]
    fn summarize_passed_attempt_with_failed_learning_is_not_merge_ready() {
        let (item, _) =
            passed_attempt_with_candidate(Some(AttemptLearning::failed(1, "retry learner")));

        let row = summarize_work_item(&item, None);

        assert_eq!(row.action, "learner-not-ready");
    }

    #[test]
    fn summarize_passed_attempt_with_non_relaunchable_learning_is_blocked() {
        let learning = AttemptLearning::failed_with_kind(
            1,
            "host evidence needs recovery",
            LearningFailureKind::EvidencePending,
        );
        let (item, _) = passed_attempt_with_candidate(Some(learning));

        let row = summarize_work_item(&item, None);

        assert_eq!(row.action, "learner-blocked");
    }

    #[test]
    fn summarize_needs_user_task_takes_priority() {
        let mut item = WorkItem {
            id: "work-1".to_string(),
            title: "Build status view".to_string(),
            planning_context: None,
            instructions: None,
            abandonment: None,
            post_merge_review_fix_depth: None,
            attempts: Vec::new(),
            merge_candidates: Vec::new(),
            ..Default::default()
        };
        item.add_initial_attempt("attempt-1").unwrap();
        item.attempts[0].tasks[0].status = TaskStatus::NeedsUser;

        let row = summarize_work_item(&item, None);

        assert_eq!(row.task, "write:attempt-1-write-1 [needs-user]");
        assert_eq!(row.action, "needs-user");
    }

    #[test]
    fn summarize_abandoned_work_item_shows_terminal_action() {
        let mut item = WorkItem {
            id: "work-1".to_string(),
            title: "Build status view".to_string(),
            planning_context: None,
            instructions: None,
            abandonment: None,
            post_merge_review_fix_depth: None,
            attempts: Vec::new(),
            merge_candidates: Vec::new(),
            ..Default::default()
        };
        item.add_initial_attempt("attempt-1").unwrap();
        item.attempts[0].status = AttemptStatus::NeedsUser;
        item.attempts[0].tasks[0].status = TaskStatus::NeedsUser;
        item.abandon(Some("replacement landed".to_string()), None)
            .unwrap();

        let row = summarize_work_item(&item, None);

        assert_eq!(row.task, "write:attempt-1-write-1 [needs-user]");
        assert_eq!(row.action, "abandoned");
    }

    #[test]
    fn format_work_status_includes_errors_after_rows() {
        let status = WorkStatus {
            rows: vec![WorkItemStatus {
                id: "work-1".to_string(),
                title: "Build status view".to_string(),
                attempt: "attempt-1 [planned]".to_string(),
                task: "write:attempt-1-write-1 [planned]".to_string(),
                review: "-".to_string(),
                merge_candidate: "-".to_string(),
                merge: "-".to_string(),
                action: "task-ready".to_string(),
                next_action: None,
                metrics: Default::default(),
                compatibility_warnings: Vec::new(),
            }],
            errors: vec!["invalid work model in bad.json".to_string()],
        };

        let output = format_work_status(&status);

        assert!(output.contains("Work Items"));
        assert!(output.contains("work-1"));
        assert!(output.contains("task-ready"));
        assert!(output.contains("Work Item read errors"));
        assert!(output.contains("invalid work model"));
    }

    #[test]
    fn dashboard_lines_are_readable() {
        let status = WorkStatus {
            rows: vec![WorkItemStatus {
                id: "work-1".to_string(),
                title: "Build status view".to_string(),
                attempt: "attempt-1 [planned]".to_string(),
                task: "write:attempt-1-write-1 [planned]".to_string(),
                review: "-".to_string(),
                merge_candidate: "-".to_string(),
                merge: "-".to_string(),
                action: "task-ready".to_string(),
                next_action: None,
                metrics: Default::default(),
                compatibility_warnings: Vec::new(),
            }],
            errors: Vec::new(),
        };

        let lines = format_work_dashboard_lines(&status);

        assert!(lines.iter().any(|line| line.contains("work-1")));
        assert!(lines.iter().any(|line| line.contains("Attempt:")));
        assert!(lines.iter().any(|line| line.contains("Merge Candidate:")));
    }

    #[test]
    fn merge_action_reflects_terminal_merge_state() {
        let mut item = WorkItem {
            id: "work-1".to_string(),
            title: "Build status view".to_string(),
            planning_context: None,
            instructions: None,
            abandonment: None,
            post_merge_review_fix_depth: None,
            attempts: Vec::new(),
            merge_candidates: Vec::new(),
            ..Default::default()
        };
        item.add_initial_attempt("attempt-1").unwrap();
        let attempt = item.attempts.last_mut().unwrap();
        let task = attempt.tasks.last_mut().unwrap();
        task.status = TaskStatus::Complete;
        task.workspace_access = WorkspaceAccess {
            reads: vec![WorkspaceRef {
                id: "target".to_string(),
                path: ".".to_string(),
            }],
            writes: vec![WorkspaceRef {
                id: "candidate".to_string(),
                path: "../work-6-work-1-attempt-1".to_string(),
            }],
        };
        task.output = Some(TaskOutput {
            workspace_id: "candidate".to_string(),
            workspace_path: "../work-6-work-1-attempt-1".to_string(),
            source_branch: "main".to_string(),
            base_commit: None,
            commit: "abc123".to_string(),
            no_change: None,
            learner_canonicalization: None,
        });
        attempt.status = AttemptStatus::Complete;
        attempt.review_state = Some(AttemptReviewState::Passed);
        item.create_or_get_merge_candidate("attempt-1").unwrap();
        item.merge_candidates[0].merge_state = MergeCandidateMergeState {
            status: MergeCandidateMergeStatus::Merged,
            merged_commit: Some("def456".to_string()),
            failure_reason: None,
            check_artifacts: Vec::new(),
            review_artifacts: Vec::new(),
            auto_merge_skipped: None,
            follow_up_failure: None,
        };

        let row = summarize_work_item(&item, None);

        assert_eq!(row.action, "merged");
        assert_eq!(row.merge, "merged review:pending");
    }

    #[test]
    fn evidence_needed_status_names_exact_attachment_command() {
        let attempt = Attempt {
            id: "attempt-1".to_string(),
            work_item_id: "work-1".to_string(),
            status: AttemptStatus::NeedsUser,
            evidence_recoveries: vec![crate::work_model::EvidenceRecovery {
                id: "host-evidence-1".to_string(),
                candidate_commit: "abc123".to_string(),
                attachment: crate::work_model::EvidenceAttachment {
                    snapshot_path: ".fluent/work/artifacts/work-1/attempt-1/host-evidence/a.json"
                        .to_string(),
                    digest: "sha256:abc".to_string(),
                },
                targets: vec![crate::work_model::EvidenceReviewTarget {
                    role: "architecture".to_string(),
                    prior_review_artifact:
                        ".fluent/work/artifacts/work-1/attempt-1/review/review.md".to_string(),
                    review_task_id: None,
                }],
                state: crate::work_model::EvidenceRecoveryState::NeedsEvidence,
                created_at: "2026-08-03T00:00:00Z".to_string(),
            }],
            ..Attempt::default()
        };

        assert_eq!(
            evidence_recovery_next_action(Some(&attempt)).as_deref(),
            Some(
                "fluent attempt evidence attach work-1 attempt-1 --candidate abc123 --evidence-file <path> --review-artifact .fluent/work/artifacts/work-1/attempt-1/review/review.md"
            )
        );
    }
}
