use std::env;

use crate::work_attempt_loop::WorkAttemptRunOutcome;
use crate::work_model::{CoderMapping, PauseKind};
use crate::work_status::WorkStatus;

pub fn guidance_enabled() -> bool {
    match env::var("FLUENT_QUIET") {
        Ok(val) => !matches!(val.as_str(), "1" | "true" | "yes"),
        Err(_) => true,
    }
}

pub fn after_work_item_create() -> &'static str {
    "\n→ Next: fluent attempt create <work-item-id>\n  (fluent skill: plan-execution)"
}

pub fn after_attempt_create() -> &'static str {
    "\n→ Next: fluent attempt run <work-item-id>"
}

/// Format the resolved coder mapping as a multi-line plan for stderr output.
pub fn format_coder_plan(mapping: &CoderMapping) -> String {
    fn role_line(label: &str, coder: &str, model: &str, effort: Option<&str>) -> String {
        let model_part = if model.is_empty() {
            "(default)".to_string()
        } else {
            model.to_string()
        };
        match effort {
            Some(e) => format!("  {label:<20} {coder} / {model_part} / effort={e}"),
            None => format!("  {label:<20} {coder} / {model_part}"),
        }
    }

    let mut lines = vec!["  Coder plan:".to_string()];
    lines.push(role_line(
        "writer",
        mapping.write.coder.as_str(),
        &mapping.write.model,
        mapping.write.effort.as_deref(),
    ));
    lines.push(role_line(
        "reviewer",
        mapping.review.coder.as_str(),
        &mapping.review.model,
        mapping.review.effort.as_deref(),
    ));
    lines.push(role_line(
        "behavior-tests",
        mapping.behavior_tests.coder.as_str(),
        &mapping.behavior_tests.model,
        mapping.behavior_tests.effort.as_deref(),
    ));
    lines.join("\n")
}

/// Runtime context an `attempt run` outcome needs to name a concrete next action:
/// why the Attempt paused, which coder to re-authenticate, and where the review
/// verdicts live.
#[derive(Default)]
pub struct AttemptRunContext<'a> {
    pub pause_kind: Option<&'a PauseKind>,
    pub coder: Option<&'a str>,
    pub review_artifact: Option<&'a str>,
}

/// Name the coder-specific re-authentication step, falling back to a coder-agnostic
/// phrase when the coder can't be determined.
pub fn coder_reauth_step(coder: Option<&str>) -> String {
    match coder {
        Some("claude") => "re-authenticate (claude /login)".to_string(),
        Some("codex") => "re-authenticate (codex login)".to_string(),
        _ => "re-authenticate your coder".to_string(),
    }
}

fn review_only_hint(review_artifact: Option<&str>, passed: bool) -> String {
    match (passed, review_artifact) {
        (true, Some(path)) => format!("\n→ Next: reviews passed; read the verdicts in {path}"),
        (true, None) => "\n→ Next: reviews passed; read the review verdicts".to_string(),
        (false, Some(path)) => {
            format!("\n→ Next: inspect the review verdicts in {path}, then address the failures")
        }
        (false, None) => {
            "\n→ Next: inspect the review verdicts, then address the failures".to_string()
        }
    }
}

pub fn after_attempt_run(
    outcome: &WorkAttemptRunOutcome,
    ctx: &AttemptRunContext,
) -> Option<String> {
    match outcome {
        WorkAttemptRunOutcome::MergeCandidateReady { .. } => Some(
            "\n→ Next: fluent merge-candidate show <work-item-id>, then fluent merge-candidate land <work-item-id>".to_string(),
        ),
        WorkAttemptRunOutcome::FollowUpRecoveryPending { next_action, .. } => {
            Some(format!("\n→ Next: {next_action}"))
        }
        WorkAttemptRunOutcome::LearnerNotReady {
            relaunchable: true,
            ..
        } => Some(
            "\n→ Next: fluent attempt run <work-item-id> to re-run the Learner before landing"
                .to_string(),
        ),
        WorkAttemptRunOutcome::LearnerNotReady {
            relaunchable: false,
            ..
        } => Some(
            "\n→ Next: fluent work-item show <work-item-id> to inspect the non-relaunchable Learner evidence failure; do not re-run the Learner or land the candidate"
                .to_string(),
        ),
        WorkAttemptRunOutcome::PlannedWriteRound { .. } => Some(
            "\n→ Next: a follow-up write round was planned from failed reviewers; fluent attempt run <work-item-id> to keep iterating".to_string(),
        ),
        WorkAttemptRunOutcome::NeedsUser { handoff_path } => match ctx.pause_kind {
            Some(PauseKind::Auth) => Some(format!(
                "\n→ Next: {}, then fluent attempt run <work-item-id>",
                coder_reauth_step(ctx.coder)
            )),
            Some(PauseKind::TranscriptPump) => Some(format!(
                "\n→ Next: fix the transcript/console transport (see {handoff_path}), then fluent attempt run <work-item-id> to retry"
            )),
            _ => Some(format!(
                "\n→ Next: read the handoff {handoff_path}, then fluent attempt run <work-item-id>"
            )),
        },
        WorkAttemptRunOutcome::ReviewOnlyComplete => {
            Some(review_only_hint(ctx.review_artifact, true))
        }
        WorkAttemptRunOutcome::ReviewOnlyFailed => {
            Some(review_only_hint(ctx.review_artifact, false))
        }
        WorkAttemptRunOutcome::RanTask { .. } | WorkAttemptRunOutcome::PlannedReviews { .. } => {
            None
        }
    }
}

pub fn after_merge_candidate_show() -> &'static str {
    "\n→ Next: assess the candidate, then fluent merge-candidate land <work-item-id>"
}

pub fn after_merge_candidate_land() -> &'static str {
    "\n→ Next: fluent cleanup <work-item-id>"
}

pub fn after_work_item_list() -> &'static str {
    "\n→ Next: fluent status to see what needs attention, or fluent work-item show <work-item-id>"
}

pub fn after_cleanup() -> &'static str {
    "\n→ Next: fluent status to see remaining work"
}

pub fn after_observation_create() -> &'static str {
    "\n→ Next: recorded; fluent observation list to review open observations"
}

pub fn empty_status_primer() -> &'static str {
    "\n→ Next: capture a brief, then define behaviors, design an approach, and plan execution, then fluent work-item create\n  (fluent skill: capture-brief)"
}

/// Map a board-state `action` (from `work_status`) to the single most-actionable
/// next command for the Work Item `id`. Returns `None` when the state has no
/// operator command to name (transient or terminal states).
pub fn next_action_for_action(action: &str, id: &str) -> Option<String> {
    match action {
        "needs-user" => Some(format!(
            "\n→ Next: {id} is paused for you; read its handoff, then fluent attempt run {id}"
        )),
        "merge-ready" => Some(format!(
            "\n→ Next: fluent merge-candidate show {id}, then fluent merge-candidate land {id}"
        )),
        "learner-not-ready" => Some(format!(
            "\n→ Next: fluent attempt run {id} to run or retry the Learner before landing"
        )),
        "task-ready" => Some(format!("\n→ Next: fluent attempt run {id}")),
        _ => None,
    }
}

/// Name the next command for the most-actionable Work Item on a populated board.
/// Priority follows `work_status`: needs-user > merge-ready > learner-not-ready >
/// task-ready. A `learner-blocked` row remains visible but is not repeatedly
/// actionable: the originating Attempt already emitted its one-time inspection
/// hint. Returns `None` when nothing on the board is actionable.
pub fn status_next_action(status: &WorkStatus) -> Option<String> {
    const PRIORITY: [&str; 4] = [
        "needs-user",
        "merge-ready",
        "learner-not-ready",
        "task-ready",
    ];
    for action in PRIORITY {
        if let Some(row) = status.rows.iter().find(|row| row.action == action) {
            return next_action_for_action(&row.action, &row.id);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn after_work_item_create_names_attempt_create() {
        let hint = after_work_item_create();
        assert!(hint.contains("attempt create"));
    }

    #[test]
    fn after_work_item_create_includes_drift_pointer() {
        let hint = after_work_item_create();
        assert!(hint.contains("fluent skill"));
    }

    #[test]
    fn after_attempt_create_names_attempt_run() {
        let hint = after_attempt_create();
        assert!(hint.contains("attempt run"));
    }

    #[test]
    fn after_attempt_run_merge_candidate_ready_names_merge_candidate() {
        let outcome = WorkAttemptRunOutcome::MergeCandidateReady {
            candidate_id: "mc-1".to_string(),
        };
        let hint = after_attempt_run(&outcome, &AttemptRunContext::default()).unwrap();
        assert!(hint.contains("merge-candidate"));
    }

    #[test]
    fn after_attempt_run_pending_recovery_names_recorded_action() {
        let outcome = WorkAttemptRunOutcome::FollowUpRecoveryPending {
            candidate_id: "mc-1".to_string(),
            stage: "observation".to_string(),
            next_action: "Re-run `fluent merge-candidate land work-1 mc-1`.".to_string(),
        };
        let hint = after_attempt_run(&outcome, &AttemptRunContext::default()).unwrap();
        assert!(hint.contains("merge-candidate land work-1 mc-1"));
    }

    #[test]
    fn after_attempt_run_relaunchable_learner_failure_names_retry() {
        let outcome = WorkAttemptRunOutcome::LearnerNotReady {
            candidate_id: "mc-1".to_string(),
            reason: "failed".to_string(),
            relaunchable: true,
        };
        let hint = after_attempt_run(&outcome, &AttemptRunContext::default()).unwrap();
        assert!(hint.contains("fluent attempt run"));
        assert!(!hint.contains("work-item show"));
        assert!(!hint.contains("merge-candidate land"));
    }

    #[test]
    fn after_attempt_run_non_relaunchable_learner_failure_names_human_inspection() {
        let outcome = WorkAttemptRunOutcome::LearnerNotReady {
            candidate_id: "mc-1".to_string(),
            reason: "failed and non-relaunchable (evidence pending)".to_string(),
            relaunchable: false,
        };
        let hint = after_attempt_run(&outcome, &AttemptRunContext::default()).unwrap();
        assert!(hint.contains("fluent work-item show"));
        assert!(hint.contains("non-relaunchable"));
        assert!(!hint.contains("fluent attempt run"));
        assert!(!hint.contains("merge-candidate land"));
    }

    #[test]
    fn after_attempt_run_planned_write_round_conveys_iteration() {
        let outcome = WorkAttemptRunOutcome::PlannedWriteRound {
            task_id: "t-1".to_string(),
        };
        let hint = after_attempt_run(&outcome, &AttemptRunContext::default()).unwrap();
        assert!(hint.contains("attempt run"));
        assert!(
            hint.contains("follow-up") || hint.contains("iterat"),
            "write-round hint should read as iterating, not stuck; got: {hint}"
        );
    }

    #[test]
    fn after_attempt_run_needs_user_auth_is_coder_aware() {
        let outcome = WorkAttemptRunOutcome::NeedsUser {
            handoff_path: "path".to_string(),
        };
        let ctx = AttemptRunContext {
            pause_kind: Some(&PauseKind::Auth),
            coder: Some("codex"),
            review_artifact: None,
        };
        let hint = after_attempt_run(&outcome, &ctx).unwrap();
        assert!(hint.contains("re-authenticate"));
        assert!(hint.contains("codex login"));
        assert!(hint.contains("attempt run"));
    }

    #[test]
    fn after_attempt_run_needs_user_generic_names_handoff_file() {
        let outcome = WorkAttemptRunOutcome::NeedsUser {
            handoff_path: ".fluent/work/artifacts/work-1/attempt-1/needs-user.md".to_string(),
        };
        let ctx = AttemptRunContext {
            pause_kind: Some(&PauseKind::Uncertain),
            ..AttemptRunContext::default()
        };
        let hint = after_attempt_run(&outcome, &ctx).unwrap();
        assert!(hint.contains("handoff"));
        assert!(hint.contains("needs-user.md"));
        assert!(hint.contains("attempt run"));
    }

    #[test]
    fn after_attempt_run_review_only_failed_names_artifact() {
        let outcome = WorkAttemptRunOutcome::ReviewOnlyFailed;
        let ctx = AttemptRunContext {
            review_artifact: Some(".fluent/work/.../review.md"),
            ..AttemptRunContext::default()
        };
        let hint = after_attempt_run(&outcome, &ctx).unwrap();
        assert!(
            hint.contains("review.md"),
            "should name the artifact; got: {hint}"
        );
        // Pin the failing semantics so the pass/fail bodies cannot be swapped
        // without a test failing.
        assert!(
            hint.contains("address the failures"),
            "failed hint must direct the operator to address failures; got: {hint}"
        );
        assert!(
            !hint.contains("passed"),
            "failed hint must not claim reviews passed; got: {hint}"
        );
        assert!(
            !hint.contains("proceed with the next step"),
            "must not be the generic phrasing; got: {hint}"
        );
    }

    #[test]
    fn after_attempt_run_review_only_complete_names_verdicts() {
        let outcome = WorkAttemptRunOutcome::ReviewOnlyComplete;
        let hint = after_attempt_run(&outcome, &AttemptRunContext::default()).unwrap();
        assert!(hint.contains("verdict"));
        // Pin the passing semantics — the counterpart to the failed hint.
        assert!(
            hint.contains("passed"),
            "complete hint must state the reviews passed; got: {hint}"
        );
        assert!(
            !hint.contains("address the failures"),
            "complete hint must not direct the operator to failures; got: {hint}"
        );
        assert!(!hint.contains("proceed with the next step"));
    }

    #[test]
    fn after_attempt_run_review_only_complete_names_artifact() {
        // Exercise the passed-with-artifact arm: the hint states the reviews
        // passed and points at the verdict file.
        let outcome = WorkAttemptRunOutcome::ReviewOnlyComplete;
        let ctx = AttemptRunContext {
            review_artifact: Some(".fluent/work/.../review.md"),
            ..AttemptRunContext::default()
        };
        let hint = after_attempt_run(&outcome, &ctx).unwrap();
        assert!(
            hint.contains("passed"),
            "passed-with-artifact hint must state the reviews passed; got: {hint}"
        );
        assert!(
            hint.contains("review.md"),
            "passed-with-artifact hint must name the verdict file; got: {hint}"
        );
        assert!(
            !hint.contains("address the failures"),
            "passed hint must not direct the operator to failures; got: {hint}"
        );
    }

    #[test]
    fn coder_reauth_step_is_coder_specific() {
        assert!(coder_reauth_step(Some("claude")).contains("claude /login"));
        assert!(coder_reauth_step(Some("codex")).contains("codex login"));
        assert!(coder_reauth_step(Some("pi")).contains("re-authenticate your coder"));
        assert!(coder_reauth_step(None).contains("re-authenticate your coder"));
    }

    #[test]
    fn after_attempt_run_ran_task_returns_none() {
        let outcome = WorkAttemptRunOutcome::RanTask {
            task_id: "t-1".to_string(),
            output: "out".to_string(),
        };
        assert!(after_attempt_run(&outcome, &AttemptRunContext::default()).is_none());
    }

    #[test]
    fn side_command_hints_name_next_step() {
        assert!(after_work_item_list().contains("fluent status"));
        assert!(after_cleanup().contains("fluent status"));
        assert!(after_observation_create().contains("observation list"));
    }

    #[test]
    fn after_merge_candidate_show_names_land() {
        let hint = after_merge_candidate_show();
        assert!(hint.contains("merge-candidate land"));
    }

    #[test]
    fn after_merge_candidate_land_names_cleanup() {
        let hint = after_merge_candidate_land();
        assert!(hint.contains("cleanup"));
    }

    fn status_row(id: &str, action: &str) -> crate::work_status::WorkItemStatus {
        crate::work_status::WorkItemStatus {
            id: id.to_string(),
            title: "Title".to_string(),
            attempt: "-".to_string(),
            task: "-".to_string(),
            review: "-".to_string(),
            merge_candidate: "-".to_string(),
            merge: "-".to_string(),
            action: action.to_string(),
        }
    }

    #[test]
    fn next_action_for_task_ready_names_attempt_run() {
        let hint = next_action_for_action("task-ready", "work-7").unwrap();
        assert!(hint.contains("fluent attempt run work-7"));
    }

    #[test]
    fn next_action_for_merge_ready_names_land() {
        let hint = next_action_for_action("merge-ready", "work-7").unwrap();
        assert!(hint.contains("fluent merge-candidate land work-7"));
    }

    #[test]
    fn next_action_for_learner_not_ready_names_attempt_run_without_land() {
        let hint = next_action_for_action("learner-not-ready", "work-7").unwrap();
        assert!(hint.contains("fluent attempt run work-7"));
        assert!(hint.contains("Learner"));
        assert!(!hint.contains("merge-candidate land"));
    }

    #[test]
    fn next_action_for_learner_blocked_is_none_to_avoid_inspection_loop() {
        assert!(next_action_for_action("learner-blocked", "work-7").is_none());
    }

    #[test]
    fn next_action_for_needs_user_names_handoff_and_attempt_run() {
        let hint = next_action_for_action("needs-user", "work-7").unwrap();
        assert!(hint.contains("handoff"));
        assert!(hint.contains("fluent attempt run work-7"));
    }

    #[test]
    fn next_action_for_transient_or_terminal_action_is_none() {
        assert!(next_action_for_action("executing", "work-7").is_none());
        assert!(next_action_for_action("merged", "work-7").is_none());
        assert!(next_action_for_action("abandoned", "work-7").is_none());
    }

    #[test]
    fn status_next_action_prioritizes_needs_user_over_merge_and_task_ready() {
        let status = WorkStatus {
            rows: vec![
                status_row("work-a", "merge-ready"),
                status_row("work-b", "needs-user"),
                status_row("work-c", "task-ready"),
            ],
            errors: Vec::new(),
        };
        let hint = status_next_action(&status).unwrap();
        assert!(
            hint.contains("work-b"),
            "needs-user item should win; got: {hint}"
        );
        assert!(hint.contains("attempt run"));
    }

    #[test]
    fn status_next_action_names_merge_ready_over_task_ready() {
        let status = WorkStatus {
            rows: vec![
                status_row("work-a", "task-ready"),
                status_row("work-b", "merge-ready"),
            ],
            errors: Vec::new(),
        };
        let hint = status_next_action(&status).unwrap();
        assert!(
            hint.contains("fluent merge-candidate land work-b"),
            "got: {hint}"
        );
    }

    #[test]
    fn status_next_action_names_learner_retry_over_task_ready() {
        let status = WorkStatus {
            rows: vec![
                status_row("work-a", "task-ready"),
                status_row("work-b", "learner-not-ready"),
            ],
            errors: Vec::new(),
        };
        let hint = status_next_action(&status).unwrap();
        assert!(hint.contains("fluent attempt run work-b"), "got: {hint}");
        assert!(!hint.contains("merge-candidate land"));
    }

    #[test]
    fn status_next_action_ignores_learner_blocked_when_merge_ready_exists() {
        let status = WorkStatus {
            rows: vec![
                status_row("work-a", "merge-ready"),
                status_row("work-b", "learner-blocked"),
            ],
            errors: Vec::new(),
        };
        let hint = status_next_action(&status).unwrap();
        assert!(
            hint.contains("fluent merge-candidate land work-a"),
            "got: {hint}"
        );
    }

    #[test]
    fn status_next_action_is_none_for_only_learner_blocked_work() {
        let status = WorkStatus {
            rows: vec![status_row("work-a", "learner-blocked")],
            errors: Vec::new(),
        };
        assert!(status_next_action(&status).is_none());
    }

    #[test]
    fn status_next_action_is_none_when_nothing_actionable() {
        let status = WorkStatus {
            rows: vec![
                status_row("work-a", "executing"),
                status_row("work-b", "merged"),
            ],
            errors: Vec::new(),
        };
        assert!(status_next_action(&status).is_none());
    }

    #[test]
    fn empty_status_primer_names_planning_stages() {
        let hint = empty_status_primer();
        assert!(hint.contains("brief"));
        assert!(hint.contains("behaviors"));
        assert!(hint.contains("approach"));
        assert!(hint.contains("work-item create"));
    }

    #[test]
    fn empty_status_primer_includes_drift_pointer() {
        let hint = empty_status_primer();
        assert!(hint.contains("fluent skill"));
        assert!(hint.contains("capture-brief"));
    }

    #[test]
    fn format_coder_plan_includes_all_roles_and_models() {
        use crate::coder::CoderKind;
        use crate::work_model::{CoderMapping, CoderModelPair};

        let mapping = CoderMapping {
            write: CoderModelPair {
                coder: CoderKind::Claude,
                model: "claude-sonnet-4-6".to_string(),
                effort: None,
            },
            review: CoderModelPair {
                coder: CoderKind::Codex,
                model: "o3".to_string(),
                effort: None,
            },
            behavior_tests: CoderModelPair {
                coder: CoderKind::Pi,
                model: "pi-model".to_string(),
                effort: None,
            },
        };
        let plan = format_coder_plan(&mapping);
        assert!(plan.contains("writer"));
        assert!(plan.contains("claude"));
        assert!(plan.contains("claude-sonnet-4-6"));
        assert!(plan.contains("reviewer"));
        assert!(plan.contains("codex"));
        assert!(plan.contains("o3"));
        assert!(plan.contains("behavior-tests"));
        assert!(plan.contains("pi"));
        assert!(!plan.contains("effort="));
    }

    #[test]
    fn format_coder_plan_shows_default_when_model_empty() {
        use crate::coder::CoderKind;
        use crate::work_model::{CoderMapping, CoderModelPair};

        let mapping = CoderMapping {
            write: CoderModelPair {
                coder: CoderKind::Claude,
                model: String::new(),
                effort: None,
            },
            review: CoderModelPair {
                coder: CoderKind::Claude,
                model: String::new(),
                effort: None,
            },
            behavior_tests: CoderModelPair {
                coder: CoderKind::Claude,
                model: String::new(),
                effort: None,
            },
        };
        let plan = format_coder_plan(&mapping);
        assert!(
            plan.contains("(default)"),
            "empty model should show (default); got:\n{plan}"
        );
    }

    #[test]
    fn format_coder_plan_shows_effort_when_set() {
        use crate::coder::CoderKind;
        use crate::work_model::{CoderMapping, CoderModelPair};

        let mapping = CoderMapping {
            write: CoderModelPair {
                coder: CoderKind::Claude,
                model: "model-w".to_string(),
                effort: Some("high".to_string()),
            },
            review: CoderModelPair {
                coder: CoderKind::Claude,
                model: "model-r".to_string(),
                effort: None,
            },
            behavior_tests: CoderModelPair {
                coder: CoderKind::Claude,
                model: "model-bt".to_string(),
                effort: Some("low".to_string()),
            },
        };
        let plan = format_coder_plan(&mapping);
        assert!(
            plan.contains("effort=high"),
            "writer effort should appear; got:\n{plan}"
        );
        assert!(
            plan.contains("effort=low"),
            "behavior-tests effort should appear; got:\n{plan}"
        );
        let reviewer_line = plan.lines().find(|l| l.contains("reviewer")).unwrap();
        assert!(
            !reviewer_line.contains("effort="),
            "reviewer line should not show effort when unset; got: {reviewer_line}"
        );
    }
}
