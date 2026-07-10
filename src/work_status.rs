use std::path::Path;

use crate::work_model::{
    Attempt, AttemptReviewState, AttemptStatus, MergeCandidate, MergeCandidateMergeStatus,
    MergeCandidateReviewState, Task, TaskKind, TaskStatus, WorkItem, WorkModelStore,
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
}

pub fn load_work_status(project_root: &Path) -> Result<WorkStatus, anyhow::Error> {
    let store = WorkModelStore::new(project_root);
    let mut rows = Vec::new();
    let mut errors = Vec::new();

    for result in store.list_work_item_results()? {
        match result {
            Ok(item) => rows.push(summarize_work_item(&item)),
            Err(error) => errors.push(error.to_string()),
        }
    }

    Ok(WorkStatus { rows, errors })
}

pub fn summarize_work_item(item: &WorkItem) -> WorkItemStatus {
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
            .map(format_task)
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
        action: action_label(item, attempt, merge_candidate).to_string(),
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
    format!("{} [{}]", attempt.id, attempt_status_label(&attempt.status))
}

fn format_task(task: &Task) -> String {
    format!(
        "{}:{} [{}]",
        task_kind_label(task.kind),
        task.id,
        task.status.as_str()
    )
}

fn format_merge_state(candidate: &MergeCandidate) -> String {
    let status = merge_status_label(&candidate.merge_state.status);
    let review = merge_review_label(&candidate.review_state);
    format!("{status} review:{review}")
}

fn action_label(
    item: &WorkItem,
    attempt: Option<&Attempt>,
    merge_candidate: Option<&MergeCandidate>,
) -> &'static str {
    if item.abandonment.is_some() {
        return "abandoned";
    }

    if let Some(attempt) = attempt {
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
            .any(|task| task.status == TaskStatus::Executing)
        {
            return "executing";
        }
        if attempt
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
            MergeCandidateMergeStatus::Pending => "merge-ready",
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

fn attempt_status_label(status: &AttemptStatus) -> &'static str {
    status.as_str()
}

fn attempt_review_label(review: &AttemptReviewState) -> &'static str {
    review.as_str()
}

fn task_kind_label(kind: TaskKind) -> &'static str {
    kind.as_str()
}

fn merge_review_label(status: &MergeCandidateReviewState) -> &'static str {
    match status {
        MergeCandidateReviewState::Pending => "pending",
        MergeCandidateReviewState::Reviewing => "reviewing",
        MergeCandidateReviewState::Passed => "passed",
        MergeCandidateReviewState::Failed => "failed",
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
        MergeCandidateMergeState, TaskOutput, WorkItem, WorkspaceAccess, WorkspaceRef,
    };

    #[test]
    fn summarize_planned_work_item_shows_ready_task() {
        let mut item = WorkItem {
            id: "work-1".to_string(),
            title: "Build status view".to_string(),
            planning_context: None,
            instructions: None,
            abandonment: None,
            attempts: Vec::new(),
            merge_candidates: Vec::new(),
        };
        item.add_initial_attempt("attempt-1").unwrap();

        let row = summarize_work_item(&item);

        assert_eq!(row.attempt, "attempt-1 [planned]");
        assert_eq!(row.task, "write:attempt-1-write-1 [planned]");
        assert_eq!(row.review, "-");
        assert_eq!(row.action, "task-ready");
    }

    #[test]
    fn summarize_passed_attempt_shows_merge_ready_candidate() {
        let mut item = WorkItem {
            id: "work-1".to_string(),
            title: "Build status view".to_string(),
            planning_context: None,
            instructions: None,
            abandonment: None,
            attempts: Vec::new(),
            merge_candidates: Vec::new(),
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
            commit: "abc123".to_string(),
        });
        attempt.status = AttemptStatus::Complete;
        attempt.review_state = Some(AttemptReviewState::Passed);
        let candidate_id = item.create_or_get_merge_candidate("attempt-1").unwrap();

        let row = summarize_work_item(&item);

        assert_eq!(row.merge_candidate, candidate_id);
        assert_eq!(row.merge, "pending review:pending");
        assert_eq!(row.action, "merge-ready");
    }

    #[test]
    fn summarize_needs_user_task_takes_priority() {
        let mut item = WorkItem {
            id: "work-1".to_string(),
            title: "Build status view".to_string(),
            planning_context: None,
            instructions: None,
            abandonment: None,
            attempts: Vec::new(),
            merge_candidates: Vec::new(),
        };
        item.add_initial_attempt("attempt-1").unwrap();
        item.attempts[0].tasks[0].status = TaskStatus::NeedsUser;

        let row = summarize_work_item(&item);

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
            attempts: Vec::new(),
            merge_candidates: Vec::new(),
        };
        item.add_initial_attempt("attempt-1").unwrap();
        item.attempts[0].status = AttemptStatus::NeedsUser;
        item.attempts[0].tasks[0].status = TaskStatus::NeedsUser;
        item.abandon(Some("replacement landed".to_string()), None)
            .unwrap();

        let row = summarize_work_item(&item);

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
            attempts: Vec::new(),
            merge_candidates: Vec::new(),
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
            commit: "abc123".to_string(),
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
        };

        let row = summarize_work_item(&item);

        assert_eq!(row.action, "merged");
        assert_eq!(row.merge, "merged review:pending");
    }
}
