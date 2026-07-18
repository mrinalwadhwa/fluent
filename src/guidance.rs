use std::env;

use crate::work_attempt_loop::WorkAttemptRunOutcome;
use crate::work_model::{CoderMapping, PauseKind};

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

pub fn after_attempt_run(
    outcome: &WorkAttemptRunOutcome,
    pause_kind: Option<&PauseKind>,
) -> Option<&'static str> {
    match outcome {
        WorkAttemptRunOutcome::MergeCandidateReady { .. } => Some(
            "\n→ Next: fluent merge-candidate show <work-item-id>, then fluent merge-candidate land <work-item-id>",
        ),
        WorkAttemptRunOutcome::PlannedWriteRound { .. } => {
            Some("\n→ Next: fluent attempt run <work-item-id>")
        }
        WorkAttemptRunOutcome::NeedsUser { .. } => match pause_kind {
            Some(PauseKind::Auth) => Some(
                "\n→ Next: re-authenticate (claude /login), then fluent attempt run <work-item-id>",
            ),
            _ => Some("\n→ Next: resolve the issue, then fluent attempt run <work-item-id>"),
        },
        WorkAttemptRunOutcome::ReviewOnlyComplete => {
            Some("\n→ Next: review complete; proceed with the next step in the lifecycle")
        }
        WorkAttemptRunOutcome::ReviewOnlyFailed => {
            Some("\n→ Next: inspect the review artifacts and address the failures")
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

pub fn empty_status_primer() -> &'static str {
    "\n→ Next: capture a brief, then define behaviors, design an approach, and plan execution, then fluent work-item create\n  (fluent skill: capture-brief)"
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
        let hint = after_attempt_run(&outcome, None).unwrap();
        assert!(hint.contains("merge-candidate"));
    }

    #[test]
    fn after_attempt_run_planned_write_round_names_attempt_run() {
        let outcome = WorkAttemptRunOutcome::PlannedWriteRound {
            task_id: "t-1".to_string(),
        };
        let hint = after_attempt_run(&outcome, None).unwrap();
        assert!(hint.contains("attempt run"));
    }

    #[test]
    fn after_attempt_run_needs_user_auth_names_reauth() {
        let outcome = WorkAttemptRunOutcome::NeedsUser {
            handoff_path: "path".to_string(),
        };
        let hint = after_attempt_run(&outcome, Some(&PauseKind::Auth)).unwrap();
        assert!(hint.contains("re-authenticate"));
        assert!(hint.contains("attempt run"));
    }

    #[test]
    fn after_attempt_run_needs_user_non_auth_names_resolve() {
        let outcome = WorkAttemptRunOutcome::NeedsUser {
            handoff_path: "path".to_string(),
        };
        let hint = after_attempt_run(&outcome, Some(&PauseKind::Uncertain)).unwrap();
        assert!(hint.contains("resolve"));
        assert!(hint.contains("attempt run"));
    }

    #[test]
    fn after_attempt_run_ran_task_returns_none() {
        let outcome = WorkAttemptRunOutcome::RanTask {
            task_id: "t-1".to_string(),
            output: "out".to_string(),
        };
        assert!(after_attempt_run(&outcome, None).is_none());
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
