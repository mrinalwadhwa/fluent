//! Project-local evidence to inform a proposed Work Item's scope decision.

use crate::work_model::{
    AttemptReviewState, AttemptStatus, LearningStatus, PLANNING_SCOPE_CALIBRATION_VERSION,
    TaskKind, TaskStatus, WorkItem, WriterRunKind,
};

pub const MIN_LOCAL_SCOPE_SAMPLES: usize = 5;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LocalScopeEvidence {
    pub comparable_attempts: usize,
    pub nearby_attempts: usize,
    pub nearby_scope_min: u32,
    pub nearby_scope_max: u32,
    pub one_pass_attempts: usize,
    pub median_writer_rounds: Option<u32>,
    pub timed_attempts: usize,
    pub median_active_task_duration_ms: Option<u64>,
}

impl LocalScopeEvidence {
    pub fn is_calibrated(&self) -> bool {
        self.nearby_attempts >= MIN_LOCAL_SCOPE_SAMPLES
    }
}

pub fn format_local_scope_evidence(evidence: &LocalScopeEvidence) -> String {
    if !evidence.is_calibrated() {
        return format!(
            "Project-local evidence is not calibrated: {} successful Attempts with scope breadth {}-{}; {} required ({} comparable successful Attempts recorded).",
            evidence.nearby_attempts,
            evidence.nearby_scope_min,
            evidence.nearby_scope_max,
            MIN_LOCAL_SCOPE_SAMPLES,
            evidence.comparable_attempts
        );
    }
    let one_pass_percent = evidence
        .one_pass_attempts
        .saturating_mul(100)
        .checked_div(evidence.nearby_attempts)
        .unwrap_or(0);
    let median_active_task_time = evidence
        .median_active_task_duration_ms
        .map(|duration| format!("{duration}ms across {} Attempts", evidence.timed_attempts))
        .unwrap_or_else(|| "unavailable".to_string());
    format!(
        "Project-local evidence from {} successful Attempts with scope breadth {}-{}: {}% completed in one reviewed Writer round; median reviewed Writer rounds {}; median recorded active task time {}. This evidence does not change the configured reference.",
        evidence.nearby_attempts,
        evidence.nearby_scope_min,
        evidence.nearby_scope_max,
        one_pass_percent,
        evidence.median_writer_rounds.unwrap_or_default(),
        median_active_task_time
    )
}

pub fn summarize_local_scope_evidence(
    items: &[WorkItem],
    proposed_scope_units: u32,
) -> LocalScopeEvidence {
    let lower = proposed_scope_units.div_ceil(2);
    let upper = proposed_scope_units.saturating_mul(2);
    let mut evidence = LocalScopeEvidence {
        nearby_scope_min: lower,
        nearby_scope_max: upper,
        ..Default::default()
    };
    let mut writer_rounds = Vec::new();
    let mut active_task_durations = Vec::new();

    for item in items {
        let Some(scope) = item
            .planning_scope
            .as_ref()
            .filter(|scope| scope.calibration_version == PLANNING_SCOPE_CALIBRATION_VERSION)
        else {
            continue;
        };
        for attempt in item.attempts.iter().filter(|attempt| {
            !attempt.kind.is_review_only_like()
                && attempt.status == AttemptStatus::Complete
                && attempt.review_state == Some(AttemptReviewState::Passed)
                && attempt
                    .learning
                    .as_ref()
                    .is_some_and(|learning| learning.status == LearningStatus::Succeeded)
        }) {
            evidence.comparable_attempts = evidence.comparable_attempts.saturating_add(1);
            if proposed_scope_units == 0
                || scope.scope_units() < lower
                || scope.scope_units() > upper
            {
                continue;
            }
            let rounds = reviewed_writer_rounds(attempt);
            if rounds == 0 {
                continue;
            }
            evidence.nearby_attempts = evidence.nearby_attempts.saturating_add(1);
            if rounds == 1 {
                evidence.one_pass_attempts = evidence.one_pass_attempts.saturating_add(1);
            }
            writer_rounds.push(rounds);
            if let Some(duration) = active_task_duration_ms(attempt) {
                active_task_durations.push(duration);
                evidence.timed_attempts = evidence.timed_attempts.saturating_add(1);
            }
        }
    }

    evidence.median_writer_rounds = median(&mut writer_rounds);
    evidence.median_active_task_duration_ms = median(&mut active_task_durations);
    evidence
}

fn reviewed_writer_rounds(attempt: &crate::work_model::Attempt) -> u32 {
    if !attempt.writer_runs.is_empty() {
        let corrective_runs = attempt
            .writer_runs
            .iter()
            .filter(|run| run.kind == WriterRunKind::Corrective)
            .count();
        return 1_u32.saturating_add(u32::try_from(corrective_runs).unwrap_or(u32::MAX));
    }

    u32::try_from(
        attempt
            .tasks
            .iter()
            .filter(|task| task.kind == TaskKind::Write && task.status == TaskStatus::Complete)
            .count(),
    )
    .unwrap_or(u32::MAX)
}

fn active_task_duration_ms(attempt: &crate::work_model::Attempt) -> Option<u64> {
    attempt
        .tasks
        .iter()
        .filter(|task| task.status == TaskStatus::Complete)
        .filter_map(|task| {
            let started = chrono::DateTime::parse_from_rfc3339(task.started_at.as_deref()?).ok()?;
            let completed =
                chrono::DateTime::parse_from_rfc3339(task.completed_at.as_deref()?).ok()?;
            u64::try_from((completed - started).num_milliseconds()).ok()
        })
        .reduce(u64::saturating_add)
}

fn median<T: Ord + Copy>(values: &mut [T]) -> Option<T> {
    values.sort_unstable();
    values.get(values.len() / 2).copied()
}

#[cfg(test)]
mod tests {
    use super::{format_local_scope_evidence, summarize_local_scope_evidence};
    use crate::follow_up::ArtifactRef;
    use crate::work_model::{
        AttemptLearning, AttemptReviewState, AttemptStatus, PlanningContext, TaskKind, TaskStatus,
        WorkItem, WriterOutcome, WriterRun, WriterRunKind,
    };

    fn behaviors(count: u32) -> String {
        let mut source = "## Scope\n".to_string();
        for number in 1..=count {
            source.push_str(&format!("\n### B{number}\n\nBehavior {number}.\n"));
        }
        source
    }

    fn completed_item(id: &str, behavior_count: u32, writer_rounds: usize) -> WorkItem {
        let mut item = WorkItem::planned(id, id);
        item.planning_context = Some(PlanningContext {
            behaviors: Some(behaviors(behavior_count)),
            ..Default::default()
        });
        item.planning_scope = item
            .planning_context
            .as_ref()
            .and_then(|context| context.scope_assessment(12, true));
        item.add_initial_attempt("attempt-1").unwrap();
        let attempt = &mut item.attempts[0];
        attempt.status = AttemptStatus::Complete;
        attempt.review_state = Some(AttemptReviewState::Passed);
        attempt.learning = Some(AttemptLearning::succeeded(
            1,
            ArtifactRef {
                path: ".fluent/work/handoffs/handoff.json".to_string(),
                digest: format!("sha256:{id}"),
            },
        ));
        attempt.tasks[0].status = TaskStatus::Complete;
        attempt.tasks[0].started_at = Some("2026-08-06T10:00:00Z".to_string());
        attempt.tasks[0].completed_at = Some("2026-08-06T10:00:01Z".to_string());
        attempt.writer_runs.push(WriterRun {
            task_id: attempt.tasks[0].id.clone(),
            outcome: WriterOutcome::Complete,
            kind: WriterRunKind::Initial,
            provider: "codex".to_string(),
            session_id: Some(format!("thread-{id}")),
            continuation: 0,
            checked_required: 0,
            completed_matrix: behavior_count as usize,
            candidate_commit: format!("{id}-commit-1"),
        });
        for round in 2..=writer_rounds {
            let mut writer = attempt.tasks[0].clone();
            writer.id = format!("attempt-1-write-{round}");
            writer.kind = TaskKind::Write;
            writer.started_at = None;
            writer.completed_at = None;
            attempt.tasks.push(writer);
            attempt.writer_runs.push(WriterRun {
                task_id: format!("attempt-1-write-{round}"),
                outcome: WriterOutcome::Complete,
                kind: WriterRunKind::Corrective,
                provider: "codex".to_string(),
                session_id: Some(format!("thread-{id}")),
                continuation: u32::try_from(round - 1).unwrap(),
                checked_required: 0,
                completed_matrix: behavior_count as usize,
                candidate_commit: format!("{id}-commit-{round}"),
            });
        }
        item
    }

    #[test]
    fn local_scope_evidence_requires_five_nearby_successful_versioned_attempts() {
        let mut items = vec![
            completed_item("one", 5, 1),
            completed_item("two", 8, 2),
            completed_item("three", 10, 1),
            completed_item("four", 12, 3),
        ];
        let insufficient = summarize_local_scope_evidence(&items, 10);
        assert_eq!(insufficient.nearby_attempts, 4);
        assert!(!insufficient.is_calibrated());

        items.push(completed_item("five", 20, 2));
        let calibrated = summarize_local_scope_evidence(&items, 10);
        assert_eq!(calibrated.nearby_attempts, 5);
        assert_eq!(calibrated.nearby_scope_min, 5);
        assert_eq!(calibrated.nearby_scope_max, 20);
        assert_eq!(calibrated.one_pass_attempts, 2);
        assert_eq!(calibrated.median_writer_rounds, Some(2));
        assert_eq!(calibrated.timed_attempts, 5);
        assert_eq!(calibrated.median_active_task_duration_ms, Some(1_000));
        assert!(calibrated.is_calibrated());
        assert!(format_local_scope_evidence(&calibrated).contains("40% completed"));
        assert!(format_local_scope_evidence(&calibrated).contains("scope breadth 5-20"));
        assert!(format_local_scope_evidence(&calibrated).contains("across 5 Attempts"));
    }

    #[test]
    fn local_scope_evidence_excludes_old_distant_and_unsuccessful_work() {
        let nearby = completed_item("nearby", 10, 1);
        let distant = completed_item("distant", 3, 1);
        let mut legacy = completed_item("legacy", 10, 1);
        legacy.planning_scope.as_mut().unwrap().calibration_version = 0;
        legacy.planning_scope.as_mut().unwrap().behavior_count = 0;
        let mut paused = completed_item("paused", 10, 2);
        paused.attempts[0].status = AttemptStatus::NeedsUser;

        let evidence = summarize_local_scope_evidence(&[nearby, distant, legacy, paused], 10);

        assert_eq!(evidence.comparable_attempts, 2);
        assert_eq!(evidence.nearby_attempts, 1);
        assert_eq!(evidence.one_pass_attempts, 1);
        assert!(format_local_scope_evidence(&evidence).contains("not calibrated"));
    }

    #[test]
    fn legacy_attempt_without_writer_runs_uses_completed_write_tasks() {
        let mut item = completed_item("legacy-writer-runs", 10, 2);
        item.attempts[0].writer_runs.clear();

        let evidence = summarize_local_scope_evidence(&[item], 10);

        assert_eq!(evidence.nearby_attempts, 1);
        assert_eq!(evidence.one_pass_attempts, 0);
        assert_eq!(evidence.median_writer_rounds, Some(2));
    }

    #[test]
    fn calibrated_evidence_reports_unavailable_when_task_durations_are_missing() {
        let mut items = (1..=5)
            .map(|number| completed_item(&format!("item-{number}"), 10, 1))
            .collect::<Vec<_>>();
        for item in &mut items {
            for task in &mut item.attempts[0].tasks {
                task.started_at = None;
                task.completed_at = None;
            }
        }

        let evidence = summarize_local_scope_evidence(&items, 10);

        assert!(evidence.is_calibrated());
        assert_eq!(evidence.timed_attempts, 0);
        assert_eq!(evidence.median_active_task_duration_ms, None);
        assert!(
            format_local_scope_evidence(&evidence)
                .contains("median recorded active task time unavailable")
        );
    }

    #[test]
    fn pre_review_continuations_do_not_count_as_corrective_rounds() {
        let mut items = (1..=5)
            .map(|number| completed_item(&format!("item-{number}"), 10, 1))
            .collect::<Vec<_>>();
        let attempt = &mut items[0].attempts[0];
        attempt.writer_runs[0].outcome = WriterOutcome::Continue;
        let mut continuation = attempt.tasks[0].clone();
        continuation.id = "attempt-1-write-2".to_string();
        attempt.tasks.push(continuation);
        attempt.writer_runs.push(WriterRun {
            task_id: "attempt-1-write-2".to_string(),
            outcome: WriterOutcome::Complete,
            kind: WriterRunKind::PreReviewContinuation,
            provider: "codex".to_string(),
            session_id: Some("thread-1".to_string()),
            continuation: 1,
            checked_required: 0,
            completed_matrix: 10,
            candidate_commit: "commit-2".to_string(),
        });

        let evidence = summarize_local_scope_evidence(&items, 10);

        assert_eq!(evidence.nearby_attempts, 5);
        assert_eq!(evidence.one_pass_attempts, 5);
        assert_eq!(evidence.median_writer_rounds, Some(1));
    }
}
