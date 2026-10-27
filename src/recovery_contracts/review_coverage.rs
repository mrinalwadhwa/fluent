//! Select effective ordinary Review coverage from immutable Task history.
//!
//! Lifecycle advancement and completed recovery consume the same read-only
//! selection. Evidence acceptance remains a separate recovery contract.

use crate::review::{self, Verdict};
use crate::work_model::{
    Attempt, MergeCandidate, Task, TaskKind, TaskOutput, TaskStatus, WorkItem,
    resolve_managed_sibling_workspace_path,
};
use anyhow::{Context, Result};
use std::collections::HashSet;
use std::fs;
use std::path::Path;

/// Retain ordinary results only while every intervening Writer delta leaves
/// their domain unchanged. Targeted evidence never creates ordinary coverage.
pub(crate) fn ordinary_review_tasks<'a>(
    project_root: &Path,
    attempt: &'a Attempt,
) -> Vec<(&'a Task, Verdict)> {
    ordinary_review_tasks_with_target(project_root, attempt, None)
}

/// Reconstruct landing coverage after rebase rewrites the last Writer output.
/// Compare with the recorded pre-rebase tip so target-only paths do not count
/// as Writer changes, even when the target later matches a Writer's contents.
pub(crate) fn ordinary_review_tasks_for_landing<'a>(
    project_root: &Path,
    attempt: &'a Attempt,
    pre_rebase_tip: Option<&str>,
) -> Vec<(&'a Task, Verdict)> {
    ordinary_review_tasks_with_target(project_root, attempt, pre_rebase_tip)
}

fn ordinary_review_tasks_with_target<'a>(
    project_root: &Path,
    attempt: &'a Attempt,
    pre_rebase_tip: Option<&str>,
) -> Vec<(&'a Task, Verdict)> {
    let mut current: Option<&TaskOutput> = None;
    let mut reviews: Vec<(&Task, Verdict)> = Vec::new();
    let mut ambiguous_roles = HashSet::new();
    let last_writer = attempt
        .tasks
        .iter()
        .rposition(|task| task.kind == TaskKind::Write && task.status == TaskStatus::Complete);
    for (index, task) in attempt.tasks.iter().enumerate() {
        if task.status != TaskStatus::Complete {
            continue;
        }
        if task.kind == TaskKind::Write {
            let invalidated = match (current, task.output.as_ref()) {
                _ if !ambiguous_roles.is_empty() => review::REVIEWERS.to_vec(),
                (Some(prior), Some(output)) if prior.commit == output.commit => Vec::new(),
                (Some(prior), Some(output)) => resolve_managed_sibling_workspace_path(
                    project_root,
                    &output.workspace_path,
                    "ordinary review coverage workspace",
                )
                .map_err(anyhow::Error::from)
                .and_then(|workspace| {
                    let compared_tip = if Some(index) == last_writer {
                        pre_rebase_tip.unwrap_or(&output.commit)
                    } else {
                        &output.commit
                    };
                    crate::review_invalidation::changed_paths_between(
                        &workspace,
                        &prior.commit,
                        compared_tip,
                    )
                    .map(|paths| crate::review_invalidation::affected_review_roles(&paths))
                })
                .unwrap_or_else(|_| review::REVIEWERS.to_vec()),
                _ => review::REVIEWERS.to_vec(),
            };
            reviews.retain(|(review, verdict)| {
                *verdict == Verdict::Pass && !invalidated.contains(&review.role.as_str())
            });
            ambiguous_roles.clear();
            current = task.output.as_ref();
            continue;
        }
        if task.kind != TaskKind::Review {
            continue;
        }
        if !review::REVIEWERS.contains(&task.role.as_str()) {
            reviews.clear();
            ambiguous_roles.insert(task.role.as_str());
            continue;
        }
        let Some(output) = current else { continue };
        let candidate_matches = task
            .review_context
            .as_ref()
            .is_some_and(|context| context.candidate_commit == output.commit);
        let verdict = task
            .artifact_area
            .as_ref()
            .and_then(|area| {
                super::resolve_managed_artifact_area_path(project_root, &area.path).ok()
            })
            .and_then(|area| fs::read_to_string(area.join("review.md")).ok())
            .map(|text| review::extract_verdict(&text))
            .unwrap_or(Verdict::Uncertain);
        if task.evidence_review_context.is_some() {
            // Once host proof passes, rerun a failed ordinary role so its
            // full-domain verdict can account for that proof. Preserve a prior
            // ordinary pass and the targeted Task's original evidence scope.
            if candidate_matches && verdict == Verdict::Pass {
                reviews.retain(|(review, verdict)| {
                    review.role != task.role || *verdict == Verdict::Pass
                });
            }
            continue;
        }
        reviews.retain(|(review, _)| review.role != task.role);
        if candidate_matches {
            ambiguous_roles.remove(task.role.as_str());
            reviews.push((task, verdict));
        } else {
            ambiguous_roles.insert(task.role.as_str());
        }
    }
    reviews
}

/// Authenticate effective ordinary coverage against each selected Task's original
/// Writer. Historical branch labels need not equal the final candidate's target.
pub(crate) fn ensure_landing_review_bindings(
    project_root: &Path,
    item: &WorkItem,
    candidate: &MergeCandidate,
) -> Result<()> {
    let attempt = item
        .attempts
        .iter()
        .find(|attempt| attempt.id == candidate.attempt_id)
        .context("landing Attempt is missing")?;
    let source = crate::work_model::resolve_expected_candidate_workspace_path(
        project_root,
        &candidate.source_workspace.path,
        &item.id,
        &candidate.attempt_id,
        "source",
    )?;
    let last_writer = attempt
        .tasks
        .iter()
        .rposition(|task| task.kind == TaskKind::Write && task.status == TaskStatus::Complete);
    let tested_commit = attempt
        .tasks
        .iter()
        .rev()
        .find(|task| task.kind == TaskKind::Tester && task.status == TaskStatus::Complete)
        .and_then(|task| task.review_context.as_ref())
        .map(|context| context.candidate_commit.as_str());
    let rebased_from = attempt
        .tasks
        .iter()
        .rev()
        .find(|task| task.kind == TaskKind::Rebase && task.status == TaskStatus::Complete)
        .and_then(|task| task.output.as_ref())
        .filter(|output| {
            output.workspace_id == candidate.source_workspace.id
                && output.workspace_path == candidate.source_workspace.path
                && output.source_branch == candidate.source_branch
        })
        .and_then(|output| output.base_commit.as_deref());
    // Rebase and capture Learning may move the final Writer's output pointer.
    // Normalize only proven commit transitions in this temporary selection view;
    // never rewrite saved contexts or turn an unknown commit into coverage.
    let mut coverage_attempt = attempt.clone();
    let mut preceding_writer = None;
    let mut bindings = Vec::new();
    for (index, task) in attempt.tasks.iter().enumerate() {
        if task.kind == TaskKind::Write && task.status == TaskStatus::Complete {
            preceding_writer = task.output.as_ref();
        }
        let authentic_commit = preceding_writer.is_some_and(|output| {
            task.review_context.as_ref().is_some_and(|context| {
                context.candidate_commit == output.commit
                    || (last_writer.is_some_and(|writer_index| index > writer_index)
                        && (tested_commit == Some(context.candidate_commit.as_str())
                            || rebased_from == Some(context.candidate_commit.as_str())))
                    || output
                        .learner_canonicalization
                        .as_ref()
                        .is_some_and(|transition| {
                            context.candidate_commit == transition.from_commit
                        })
            })
        });
        if task.kind == TaskKind::Review {
            if authentic_commit {
                coverage_attempt.tasks[index]
                    .review_context
                    .as_mut()
                    .unwrap()
                    .candidate_commit = preceding_writer.unwrap().commit.clone();
            }
            bindings.push((task, preceding_writer, authentic_commit));
        }
    }
    let pre_rebase_tip = attempt
        .tasks
        .iter()
        .rev()
        .find(|task| task.kind == TaskKind::Rebase && task.status == TaskStatus::Complete)
        .map(|task| {
            task.output
                .as_ref()
                .and_then(|output| output.base_commit.as_deref())
                .unwrap_or("")
        });
    let ordinary =
        ordinary_review_tasks_for_landing(project_root, &coverage_attempt, pre_rebase_tip);
    for role in crate::review::REVIEWERS {
        anyhow::ensure!(
            ordinary.iter().any(|(task, verdict)| {
                task.role == *role && *verdict == crate::review::Verdict::Pass
            }),
            "ordinary {role} review binding changed before landing"
        );
    }
    for (task, writer, authentic_commit) in bindings {
        if !ordinary.iter().any(|(selected, _)| selected.id == task.id) {
            continue;
        }
        let valid = writer.is_some_and(|output| {
            task.review_context.as_ref().is_some_and(|context| {
                authentic_commit
                    && context.candidate_workspace_id == output.workspace_id
                    && context.candidate_workspace_path == output.workspace_path
                    && context.source_branch == output.source_branch
                    && output.workspace_id == candidate.source_workspace.id
                    && output.workspace_path == candidate.source_workspace.path
                    && task
                        .workspace_access
                        .reads
                        .contains(&candidate.source_workspace)
                    && crate::git::run_raw(
                        &source,
                        &[
                            "cat-file",
                            "-e",
                            &format!("{}^{{commit}}", context.candidate_commit),
                        ],
                    )
                    .is_ok_and(|result| result.status.success())
            })
        });
        anyhow::ensure!(
            valid,
            "ordinary {} review binding changed before landing",
            task.role
        );
    }
    Ok(())
}
