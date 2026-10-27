use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::Path;

use crate::work_model::{
    Attempt, AttemptReviewState, AttemptStatus, MergeCandidate, MergeCandidateMergeStatus,
    MergeReviewState, PauseKind, PauseRecovery, Task, TaskKind, TaskStatus, WorkItem,
    WorkModelStore,
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
    pub recovery_blocker: Option<String>,
    pub verification: Option<crate::verification_assessment::VerificationAssessment>,
    pub evidence_history: Option<LandedEvidenceHistory>,
    pub metrics: WorkMetrics,
    pub compatibility_warnings: Vec<String>,
    pub release: Option<ReleaseStatus>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct LandedEvidenceHistory {
    pub merged_commit: String,
    pub tester_results: Option<String>,
    pub diagnostic: Option<String>,
}

fn landed_evidence_history(
    root: Option<&Path>,
    attempt: &Attempt,
    candidate: &MergeCandidate,
) -> LandedEvidenceHistory {
    let mut history = LandedEvidenceHistory {
        merged_commit: candidate
            .merge_state
            .merged_commit
            .clone()
            .unwrap_or_default(),
        tester_results: None,
        diagnostic: None,
    };
    let result = root
        .ok_or_else(|| anyhow::anyhow!("project root is unavailable"))
        .and_then(|root| {
            let source = if candidate
                .merge_state
                .check_artifacts
                .iter()
                .any(|artifact| artifact.producer_id == "merge-tester")
            {
                crate::verification_assessment::LandedTesterSource::MergeCandidate
            } else {
                crate::verification_assessment::LandedTesterSource::Attempt
            };
            crate::verification_assessment::assess_landed_tester_history(
                root,
                attempt,
                candidate,
                &history.merged_commit,
                source,
            )
        });
    match result {
        Ok(path) => history.tester_results = Some(path),
        Err(error) => {
            history.diagnostic = Some(format!("Landed evidence history is unavailable: {error:#}"))
        }
    }
    history
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct ReleaseStatus {
    pub criteria: usize,
    pub blockers: usize,
    pub proposed_follow_ups: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct WorkMetrics {
    pub review_rounds: usize,
    pub stage_duration_ms: u64,
    pub active_duration_ms: u64,
    pub provider_wait_ms: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub repeated_findings: usize,
    pub artifact_bytes: u64,
    pub avoided_cycles: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
struct PersistedFilesystemMetrics {
    schema_version: u32,
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    repeated_findings: usize,
    #[serde(default)]
    artifact_bytes: u64,
    #[serde(default)]
    provider_wait_ms: u64,
    #[serde(default)]
    unattributed_input_tokens: u64,
    #[serde(default)]
    unattributed_output_tokens: u64,
    #[serde(default)]
    unattributed_artifact_bytes: u64,
    #[serde(default)]
    tasks: BTreeMap<String, PersistedTaskMetrics>,
    #[serde(default)]
    updated_at: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
struct PersistedTaskMetrics {
    input_tokens: u64,
    output_tokens: u64,
    artifact_bytes: u64,
    #[serde(default)]
    provider_wait_ms: u64,
    #[serde(default)]
    finding_identities: Vec<String>,
}

fn persisted_metrics_path(project_root: &Path, work_item_id: &str) -> std::path::PathBuf {
    project_root
        .join(crate::work_model::WORK_MODEL_DIR)
        .join("metrics")
        .join(format!("{work_item_id}.json"))
}

pub fn load_work_status(project_root: &Path) -> Result<WorkStatus, anyhow::Error> {
    Ok(summarize_work_snapshot(
        WorkModelStore::new(project_root).snapshot_work_items()?,
        project_root,
    ))
}

pub(crate) fn summarize_work_snapshot(
    snapshot: crate::work_model::WorkModelSnapshot,
    project_root: &Path,
) -> WorkStatus {
    WorkStatus {
        rows: snapshot
            .items
            .iter()
            .map(|item| summarize_work_item(item, Some(project_root)))
            .collect(),
        errors: snapshot.diagnostics,
    }
}

pub fn summarize_work_item(item: &WorkItem, project_root: Option<&Path>) -> WorkItemStatus {
    let attempt = display_attempt(item, project_root);
    let merge_candidate = attempt.and_then(|attempt| {
        item.merge_candidates
            .iter()
            .rev()
            .find(|candidate| candidate.attempt_id == attempt.id)
    });

    let evidence_history = attempt
        .zip(merge_candidate)
        .filter(|(_, candidate)| candidate.merge_state.status == MergeCandidateMergeStatus::Merged)
        .map(|(attempt, candidate)| landed_evidence_history(project_root, attempt, candidate));
    let landing_recovery = landing_binding_guidance(item, merge_candidate, project_root);
    let landing_blocked = landing_recovery.is_some();
    let recovery = if let Some(recovery) = landing_recovery {
        recovery
    } else if evidence_history.is_some() {
        AttemptRecoveryGuidance::default()
    } else {
        attempt_recovery_guidance(attempt, project_root)
    };
    let verification = attempt
        .filter(|_| evidence_history.is_none())
        .and_then(|attempt| {
            project_root.and_then(|root| {
                crate::verification_assessment::assess_current_verification(root, attempt).ok()
            })
        });
    WorkItemStatus {
        verification,
        recovery_blocker: evidence_history
            .as_ref()
            .and_then(|history| history.diagnostic.clone())
            .or(recovery.blocker),
        evidence_history,
        id: item.id.clone(),
        title: item.title.clone(),
        attempt: attempt
            .map(format_attempt)
            .unwrap_or_else(|| "-".to_string()),
        task: attempt
            .and_then(|attempt| {
                if merge_candidate.is_some_and(|candidate| {
                    candidate.merge_state.status == MergeCandidateMergeStatus::Merged
                }) {
                    attempt.tasks.last()
                } else {
                    select_task(attempt)
                }
            })
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
        action: if landing_blocked {
            "landing-blocked"
        } else {
            action_label_with_liveness(item, attempt, merge_candidate, project_root)
        }
        .to_string(),
        next_action: if merge_candidate.is_some_and(|candidate| {
            candidate.merge_state.status == MergeCandidateMergeStatus::Merged
        }) {
            None
        } else {
            recovery.next_action
        },
        metrics: work_metrics(item, project_root),
        compatibility_warnings: item.compatibility_warnings(),
        release: item
            .release_contract
            .as_ref()
            .map(|contract| ReleaseStatus {
                criteria: contract.criteria.len(),
                blockers: contract
                    .findings
                    .iter()
                    .filter(|finding| {
                        matches!(
                            finding.classification,
                            crate::work_model::ReleaseFindingClassification::ReleaseBlocker { .. }
                        )
                    })
                    .count(),
                proposed_follow_ups: contract
                    .findings
                    .iter()
                    .filter(|finding| {
                        matches!(
                            finding.classification,
                            crate::work_model::ReleaseFindingClassification::ProposedFollowUp
                        )
                    })
                    .count(),
            }),
    }
}

pub fn work_metrics(item: &WorkItem, project_root: Option<&Path>) -> WorkMetrics {
    let persisted = project_root
        .and_then(|root| fs::read(persisted_metrics_path(root, &item.id)).ok())
        .and_then(|bytes| serde_json::from_slice::<PersistedFilesystemMetrics>(&bytes).ok())
        .filter(|metrics| matches!(metrics.schema_version, 1 | 2))
        .unwrap_or_default();
    compute_work_metrics(item, &persisted)
}

/// Rebuild the one-file status index at an execution or explicit maintenance
/// boundary. Status and show only read this bounded sidecar.
pub fn refresh_persisted_metrics(project_root: &Path, item: &WorkItem) -> anyhow::Result<()> {
    let _lock =
        crate::lease::acquire_blocking(&persisted_metrics_lock_path(project_root, &item.id))?;
    let usage_rows = local_usage_rows(item, project_root);
    let mut persisted = PersistedFilesystemMetrics {
        schema_version: 2,
        updated_at: crate::work_model::now_iso8601(),
        ..Default::default()
    };
    for row in usage_rows.iter().filter(|row| row.work_item_id == item.id) {
        persisted.input_tokens = persisted.input_tokens.saturating_add(row.input_tokens);
        persisted.output_tokens = persisted.output_tokens.saturating_add(row.output_tokens);
    }
    let artifact_root = project_root
        .join(crate::work_model::WORK_ARTIFACTS_DIR)
        .join(&item.id);
    if artifact_root.is_dir() {
        persisted.artifact_bytes = crate::prep::logical_bytes(&artifact_root).unwrap_or(0);
    }

    for task in item.attempts.iter().flat_map(|attempt| &attempt.tasks) {
        persisted.tasks.insert(
            task.id.clone(),
            collect_task_metrics(project_root, item, task),
        );
    }
    let task_input_tokens = persisted
        .tasks
        .values()
        .map(|metrics| metrics.input_tokens)
        .fold(0_u64, u64::saturating_add);
    let task_output_tokens = persisted
        .tasks
        .values()
        .map(|metrics| metrics.output_tokens)
        .fold(0_u64, u64::saturating_add);
    let task_artifact_bytes = persisted
        .tasks
        .values()
        .map(|metrics| metrics.artifact_bytes)
        .fold(0_u64, u64::saturating_add);
    persisted.unattributed_input_tokens = persisted.input_tokens.saturating_sub(task_input_tokens);
    persisted.unattributed_output_tokens =
        persisted.output_tokens.saturating_sub(task_output_tokens);
    persisted.unattributed_artifact_bytes =
        persisted.artifact_bytes.saturating_sub(task_artifact_bytes);
    aggregate_persisted_metrics(&mut persisted);
    write_persisted_metrics(project_root, item, &persisted)
}

/// Refresh only the artifact area owned by one completed Task. This keeps
/// execution-boundary accounting proportional to the Task that just ran; the
/// explicit rebuild command remains the recovery path for historical or
/// manually edited artifacts outside Task areas.
pub fn refresh_persisted_task_metrics(
    project_root: &Path,
    item: &WorkItem,
    task_id: &str,
) -> anyhow::Result<()> {
    let _lock =
        crate::lease::acquire_blocking(&persisted_metrics_lock_path(project_root, &item.id))?;
    let path = persisted_metrics_path(project_root, &item.id);
    let mut persisted = fs::read(&path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<PersistedFilesystemMetrics>(&bytes).ok())
        .filter(|metrics| metrics.schema_version == 2)
        .unwrap_or_else(|| PersistedFilesystemMetrics {
            schema_version: 2,
            ..Default::default()
        });
    let task = item
        .attempts
        .iter()
        .flat_map(|attempt| &attempt.tasks)
        .find(|task| task.id == task_id)
        .ok_or_else(|| anyhow::anyhow!("Task {task_id:?} not found while persisting metrics"))?;
    persisted.tasks.insert(
        task.id.clone(),
        collect_task_metrics(project_root, item, task),
    );
    persisted.updated_at = crate::work_model::now_iso8601();
    aggregate_persisted_metrics(&mut persisted);
    write_persisted_metrics(project_root, item, &persisted)
}

fn collect_task_metrics(project_root: &Path, item: &WorkItem, task: &Task) -> PersistedTaskMetrics {
    let Some(area) = task.artifact_area.as_ref() else {
        return PersistedTaskMetrics::default();
    };
    let root = project_root.join(&area.path);
    let mut usage_rows = Vec::new();
    collect_local_usage_rows(&root, item, &mut usage_rows);
    let mut metrics = PersistedTaskMetrics {
        input_tokens: usage_rows
            .iter()
            .map(|row| row.input_tokens)
            .fold(0_u64, u64::saturating_add),
        output_tokens: usage_rows
            .iter()
            .map(|row| row.output_tokens)
            .fold(0_u64, u64::saturating_add),
        artifact_bytes: if root.is_dir() {
            crate::prep::logical_bytes(&root).unwrap_or(0)
        } else {
            0
        },
        provider_wait_ms: provider_wait_ms(&root),
        ..Default::default()
    };
    if task.kind == TaskKind::Review {
        let source = fs::read_to_string(root.join("review.md")).unwrap_or_default();
        metrics.finding_identities = crate::review::open_finding_titles(&source)
            .into_iter()
            .map(|title| crate::review::finding_identity(&task.role, &title))
            .collect();
    }
    metrics
}

fn aggregate_persisted_metrics(persisted: &mut PersistedFilesystemMetrics) {
    persisted.input_tokens = persisted
        .tasks
        .values()
        .map(|metrics| metrics.input_tokens)
        .fold(persisted.unattributed_input_tokens, u64::saturating_add);
    persisted.output_tokens = persisted
        .tasks
        .values()
        .map(|metrics| metrics.output_tokens)
        .fold(persisted.unattributed_output_tokens, u64::saturating_add);
    persisted.artifact_bytes = persisted
        .tasks
        .values()
        .map(|metrics| metrics.artifact_bytes)
        .fold(persisted.unattributed_artifact_bytes, u64::saturating_add);
    persisted.provider_wait_ms = persisted
        .tasks
        .values()
        .map(|metrics| metrics.provider_wait_ms)
        .fold(0_u64, u64::saturating_add);
    let mut findings_seen = HashSet::new();
    persisted.repeated_findings = persisted
        .tasks
        .values()
        .flat_map(|metrics| &metrics.finding_identities)
        .filter(|identity| !findings_seen.insert((*identity).clone()))
        .count();
}

fn persisted_metrics_lock_path(project_root: &Path, work_item_id: &str) -> std::path::PathBuf {
    persisted_metrics_path(project_root, work_item_id).with_extension("lock")
}

fn write_persisted_metrics(
    project_root: &Path,
    item: &WorkItem,
    persisted: &PersistedFilesystemMetrics,
) -> anyhow::Result<()> {
    let path = persisted_metrics_path(project_root, &item.id);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    crate::atomic_write::atomic_write(&path, &serde_json::to_vec_pretty(persisted)?)?;
    Ok(())
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
    let attempt = display_attempt(item, Some(project_root));
    let candidate = attempt.and_then(|attempt| {
        item.merge_candidates.iter().rev().find(|candidate| {
            candidate.attempt_id == attempt.id
                && candidate.merge_state.status == MergeCandidateMergeStatus::Merged
        })
    });
    let pending_candidate = attempt.and_then(|attempt| {
        item.merge_candidates
            .iter()
            .rev()
            .find(|candidate| candidate.attempt_id == attempt.id)
    });
    let recovery = if candidate.is_some() {
        AttemptRecoveryGuidance::default()
    } else {
        landing_binding_guidance(item, pending_candidate, Some(project_root))
            .unwrap_or_else(|| attempt_recovery_guidance(attempt, Some(project_root)))
    };
    if let (Some(attempt), Some(candidate)) = (attempt, candidate) {
        output["evidence-history"] = serde_json::to_value(landed_evidence_history(
            Some(project_root),
            attempt,
            candidate,
        ))?;
        return Ok(output);
    }
    if let Some(next_action) = recovery.next_action {
        output["next-action"] = serde_json::to_value(next_action)?;
    }
    if let Some(blocker) = recovery.blocker {
        output["recovery-blocker"] = serde_json::to_value(blocker)?;
    }
    if let Some(attempt) = attempt {
        match crate::recovery_contracts::required_check_recovery_status(project_root, attempt) {
            Ok(Some(status)) => {
                output["evidence-recovery"] = serde_json::to_value(status)?;
            }
            Err(error) => {
                output["evidence-recovery"] = serde_json::json!({"can-resume":false,"reason":format!("Required check proof is missing or changed: {error:#}")});
            }
            Ok(None) => {}
        }
        if attempt.kind.is_review_only_like()
            || attempt.tasks.iter().any(|task| {
                task.kind == TaskKind::Write
                    && task.status == TaskStatus::Complete
                    && task.output.is_some()
            })
        {
            match crate::verification_assessment::assess_current_verification(project_root, attempt)
            {
                Ok(verification) => {
                    output["verification"] = serde_json::to_value(verification)?;
                }
                Err(error) => {
                    output["verification-diagnostic"] = serde_json::to_value(format!(
                        "Current verification is unavailable: {error:#}"
                    ))?;
                }
            }
        }
    }
    Ok(output)
}

/// Select the Attempt owned by the latest dispatch when queue history carries
/// an exact binding. This keeps status aligned with an older Attempt resumed by
/// an audited recovery while preserving later superseded Attempts in history.
pub(crate) fn display_attempt<'a>(
    item: &'a WorkItem,
    project_root: Option<&Path>,
) -> Option<&'a Attempt> {
    let bound_attempt = project_root
        .and_then(|root| crate::queue::read_ledger(root, &item.id).ok().flatten())
        .and_then(|ledger| {
            ledger
                .latest()
                .and_then(|dispatch| dispatch.bound_attempt_id.clone())
        });
    bound_attempt
        .as_deref()
        .and_then(|attempt_id| {
            item.attempts
                .iter()
                .find(|attempt| attempt.id == attempt_id)
        })
        .or_else(|| item.attempts.last())
}

fn compute_work_metrics(item: &WorkItem, persisted: &PersistedFilesystemMetrics) -> WorkMetrics {
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
        input_tokens: persisted.input_tokens,
        output_tokens: persisted.output_tokens,
        repeated_findings: persisted.repeated_findings,
        artifact_bytes: persisted.artifact_bytes,
        ..Default::default()
    };

    metrics.stage_duration_ms = item
        .attempts
        .iter()
        .flat_map(|attempt| &attempt.tasks)
        .filter_map(task_duration_ms)
        .fold(0_u64, u64::saturating_add);
    metrics.provider_wait_ms = persisted.provider_wait_ms;
    metrics.active_duration_ms = metrics
        .stage_duration_ms
        .saturating_sub(metrics.provider_wait_ms);

    metrics
}

fn provider_wait_ms(artifact_dir: &Path) -> u64 {
    let pump_wait = fs::read(artifact_dir.join("transcript-pump.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<crate::transcript_pump::PumpStatus>(&bytes).ok())
        .map(|status| status.provider_timing.wait_ms)
        .unwrap_or(0);
    let outer_wait = crate::coder::read_provider_retry_timing(
        &artifact_dir.join(crate::coder::PROVIDER_TIMING_FILENAME),
    )
    .map(|timing| timing.wait_ms)
    .unwrap_or(0);
    pump_wait.saturating_add(outer_wait)
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
            if let Some(release) = &row.release {
                output.push_str(&format!(
                    "  release: criteria:{} blockers:{} proposed-follow-ups:{}\n",
                    release.criteria, release.blockers, release.proposed_follow_ups
                ));
            }
            if let Some(view) = &row.verification {
                for requirement in view.references.iter().chain(&view.commands) {
                    output.push_str(&format!(
                        "  Verification {}: {} ({:?})\n",
                        requirement.id,
                        if requirement.satisfied {
                            "satisfied"
                        } else {
                            "outstanding"
                        },
                        requirement.owner
                    ));
                }
            }
            if let Some(history) = &row.evidence_history {
                if let Some(path) = &history.tester_results {
                    output.push_str(&format!("  evidence-history: Tester result {path}\n"));
                }
            }
            if let Some(reason) = &row.recovery_blocker {
                let label = if row.evidence_history.is_some() {
                    "evidence-history"
                } else {
                    "evidence"
                };
                output.push_str(&format!("  {label}: {reason}\n"));
            }
            if let Some(next_action) = row.next_action.as_deref() {
                output.push_str(&format!("  next-action: {next_action}\n"));
            }
            output.push_str(&format!(
                "  metrics: rounds:{} duration:{}ms active:{}ms provider-wait:{}ms tokens:{}/{} repeated:{} artifacts:{}B avoided:{}\n",
                row.metrics.review_rounds,
                row.metrics.stage_duration_ms,
                row.metrics.active_duration_ms,
                row.metrics.provider_wait_ms,
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
            "  Metrics: rounds {} · duration {}ms · active {}ms · provider-wait {}ms · tokens {}/{} · repeated {} · artifacts {}B · avoided {}",
            row.metrics.review_rounds,
            row.metrics.stage_duration_ms,
            row.metrics.active_duration_ms,
            row.metrics.provider_wait_ms,
            row.metrics.input_tokens,
            row.metrics.output_tokens,
            row.metrics.repeated_findings,
            row.metrics.artifact_bytes,
            row.metrics.avoided_cycles
        ));
        if let Some(view) = &row.verification {
            for requirement in view.references.iter().chain(&view.commands) {
                lines.push(format!(
                    "  Verification {}: {} ({:?})",
                    requirement.id,
                    if requirement.satisfied {
                        "satisfied"
                    } else {
                        "outstanding"
                    },
                    requirement.owner
                ));
            }
        }
        if let Some(history) = &row.evidence_history {
            if let Some(path) = &history.tester_results {
                lines.push(format!("  Evidence history: Tester result {path}"));
            }
        }
        if let Some(reason) = &row.recovery_blocker {
            let label = if row.evidence_history.is_some() {
                "Evidence history"
            } else {
                "Evidence"
            };
            lines.push(format!("  {label}: {reason}"));
        }
        if let Some(next_action) = row.next_action.as_deref() {
            lines.push(format!("  Next action: {next_action}"));
        }
        for warning in &row.compatibility_warnings {
            lines.push(format!("  Warning: {warning}"));
        }
        if let Some(release) = &row.release {
            lines.push(format!(
                "  Release: criteria:{} blockers:{} proposed-follow-ups:{}",
                release.criteria, release.blockers, release.proposed_follow_ups
            ));
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

pub(crate) fn effective_task_status_label(
    task: &Task,
    item: &WorkItem,
    project_root: Option<&Path>,
) -> &'static str {
    if task.status == TaskStatus::Executing {
        if let Some(root) = project_root {
            if !local_task_is_live(root, item, task) {
                return "interrupted";
            }
        }
        if project_root.is_some_and(|root| task_is_provider_waiting(root, task)) {
            return "provider-wait";
        }
    }
    task.status.as_str()
}

fn task_is_provider_waiting(project_root: &Path, task: &Task) -> bool {
    let Some(area) = task.artifact_area.as_ref() else {
        return false;
    };
    let artifact_dir = project_root.join(&area.path);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0);
    let pump_waiting = fs::read(artifact_dir.join("transcript-pump.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<crate::transcript_pump::PumpStatus>(&bytes).ok())
        .and_then(|status| status.provider_timing.resume_at_ms)
        .is_some_and(|resume| resume > now);
    let outer_waiting = crate::coder::read_provider_retry_timing(
        &artifact_dir.join(crate::coder::PROVIDER_TIMING_FILENAME),
    )
    .and_then(|timing| timing.resume_at_ms)
    .is_some_and(|resume| resume > now);
    pump_waiting || outer_waiting
}

fn is_task_live_executing(task: &Task, item: &WorkItem, project_root: Option<&Path>) -> bool {
    task.status == TaskStatus::Executing
        && match project_root {
            Some(root) => local_task_is_live(root, item, task),
            None => true,
        }
}

fn local_task_is_live(project_root: &Path, item: &WorkItem, task: &Task) -> bool {
    let lock_path = crate::lease::task_lock_path(project_root, &item.id, &task.id);
    if crate::lease::is_leased(&lock_path) {
        return true;
    }
    let Some(area) = task.artifact_area.as_ref() else {
        return false;
    };
    let execution = crate::execution::record_path_for_artifact(project_root, &area.path);
    crate::execution::execution_is_live(&execution).unwrap_or(true)
}

fn format_merge_state(candidate: &MergeCandidate) -> String {
    let status = if candidate.merge_state.status == MergeCandidateMergeStatus::LandingPending {
        "publication-pending"
    } else {
        merge_status_label(&candidate.merge_state.status)
    };
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

    // A merged Merge Candidate is the authoritative terminal result. Historical
    // Attempt or Task pauses must never turn it back into actionable Work.
    if merge_candidate
        .is_some_and(|candidate| candidate.merge_state.status == MergeCandidateMergeStatus::Merged)
    {
        return "merged";
    }

    if let Some(attempt) = attempt {
        if attempt.is_failed_candidate_gate_correction() {
            return "needs-user";
        }
        if attempt.status == AttemptStatus::NeedsUser
            && attempt.evidence_recoveries.last().is_some_and(|recovery| {
                recovery.state == crate::work_model::EvidenceRecoveryState::NeedsEvidence
            })
        {
            return "evidence-needed";
        }
        if project_root.is_some_and(|root| {
            crate::host_evidence::failed_recheck_evidence_recovery_action(root, attempt).is_some()
        }) {
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
            MergeCandidateMergeStatus::LandingPending => "landing-pending",
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

/// Derive the current recovery operation from live Attempt state and artifacts.
/// This is deliberately separate from the persisted pause record: persistence
/// uses it to refresh the record, while display uses the wrapper below to reject
/// stale recorded commands.
pub(crate) fn derive_attempt_recovery_next_action(
    attempt: Option<&Attempt>,
    project_root: Option<&Path>,
) -> Option<String> {
    derive_attempt_recovery_guidance(attempt, project_root).next_action
}

#[derive(Default)]
struct AttemptRecoveryGuidance {
    next_action: Option<String>,
    blocker: Option<String>,
}

fn derive_attempt_recovery_guidance(
    attempt: Option<&Attempt>,
    project_root: Option<&Path>,
) -> AttemptRecoveryGuidance {
    let Some(attempt) = attempt else {
        return AttemptRecoveryGuidance::default();
    };
    if let Some(root) = project_root {
        match crate::usage_limit_contracts::status_action(root, attempt) {
            Ok(Some(next_action)) => {
                return AttemptRecoveryGuidance {
                    blocker: next_action
                        .is_none()
                        .then(|| "No eligible usage-limit recovery action".to_string()),
                    next_action,
                };
            }
            Err(error) => {
                return AttemptRecoveryGuidance {
                    next_action: None,
                    blocker: Some(format!("Usage-limit recovery is blocked: {error:#}")),
                };
            }
            Ok(None) => {}
        }
    }
    if let Some(root) = project_root {
        match crate::recovery_contracts::context_retry::status_action(root, attempt) {
            Ok(Some(next_action)) => {
                return AttemptRecoveryGuidance {
                    blocker: next_action
                        .is_none()
                        .then(|| "No eligible context recovery action".to_string()),
                    next_action,
                };
            }
            Err(error) => {
                // Keep the rejection beside the absent action so display cannot
                // replace it with generic resume guidance.
                return AttemptRecoveryGuidance {
                    next_action: None,
                    blocker: Some(format!("Context recovery is blocked: {error:#}")),
                };
            }
            Ok(None) => {}
        }
    }
    if attempt.status == AttemptStatus::NeedsUser
        && attempt.pause_kind == Some(PauseKind::WriterCompletion)
        && let Some(action) = project_root.and_then(|root| {
            crate::writer_completion_recovery::status_recovery_action(root, attempt)
        })
    {
        return AttemptRecoveryGuidance {
            next_action: Some(action),
            blocker: None,
        };
    }
    let blocker =
        project_root.and_then(
            |root| match crate::recovery_contracts::required_check_recovery_status(root, attempt) {
                Ok(Some(status)) => Some(status.reason),
                Err(error) => Some(format!(
                    "Required check proof is missing or changed: {error:#}"
                )),
                Ok(None) => None,
            },
        );
    AttemptRecoveryGuidance {
        next_action: derive_other_attempt_recovery_next_action(attempt, project_root),
        blocker,
    }
}

fn landing_binding_guidance(
    item: &WorkItem,
    candidate: Option<&MergeCandidate>,
    project_root: Option<&Path>,
) -> Option<AttemptRecoveryGuidance> {
    let candidate = candidate?;
    let root = project_root?;
    if candidate.merge_state.status == MergeCandidateMergeStatus::Merged
        || item
            .attempt_learning_advancement(&candidate.attempt_id)
            .is_err()
    {
        return None;
    }
    let error = crate::work_merge_executor::ensure_landing_bindings(root, item, candidate).err()?;
    Some(AttemptRecoveryGuidance {
        blocker: Some(error.to_string()),
        next_action: Some(format!(
            "fluent merge-candidate show {} {}",
            item.id, candidate.id
        )),
    })
}

fn derive_other_attempt_recovery_next_action(
    attempt: &Attempt,
    project_root: Option<&Path>,
) -> Option<String> {
    if let Some(root) = project_root {
        match crate::recovery_contracts::assignment_context_recovery_action(root, attempt) {
            Ok(Some(action)) => return Some(action),
            Err(_) => return None,
            Ok(None) => {}
        }
    }
    if let Some(root) = project_root {
        match crate::usage_limit_contracts::status_action(root, attempt) {
            Ok(Some(action)) => return action,
            Err(_) => return None,
            Ok(None) => {}
        }
    }
    if let Some(root) = project_root {
        match crate::recovery_contracts::required_check_recovery_status(root, attempt) {
            Ok(Some(status)) => return status.next_action,
            Err(_) => return None,
            Ok(None) => {}
        }
    }

    if let Some(action) = project_root.and_then(|root| {
        crate::host_evidence::failed_recheck_evidence_recovery_action(root, attempt)
    }) {
        return Some(action);
    }
    if let Some(action) = project_root
        .and_then(|root| crate::candidate_ancestry_recovery::recovery_action(root, attempt))
    {
        return Some(action);
    }
    if let Some(action) = project_root
        .and_then(|root| crate::candidate_metadata_recovery::recovery_action(root, attempt))
    {
        return Some(action);
    }
    if attempt.status == AttemptStatus::NeedsUser
        && attempt.pause_kind == Some(PauseKind::TesterHarness)
        && attempt.pause_recovery.is_none()
        && attempt.active_tester_harness_repair_source().is_none()
        && project_root.is_some_and(|root| tester_pause_requires_project_repair(root, attempt))
    {
        return Some(format!(
            "fluent attempt repair-tester {} {} --resolution \"<describe the Tester repair>\"",
            attempt.work_item_id, attempt.id
        ));
    }
    if attempt.status == AttemptStatus::NeedsUser
        && attempt.pause_recovery == Some(PauseRecovery::TesterNoSandbox)
    {
        return Some(format!(
            "fluent queue resume {} --attempt {} --no-sandbox",
            attempt.work_item_id, attempt.id
        ));
    }
    if let Some(recovery) = attempt.evidence_recoveries.last().filter(|recovery| {
        attempt.status == AttemptStatus::NeedsUser
            && recovery.state == crate::work_model::EvidenceRecoveryState::NeedsEvidence
    }) {
        let artifacts = recovery
            .targets
            .iter()
            .map(|target| {
                let review_task_id = target.review_task_id.as_ref()?;
                let review_task = attempt.tasks.iter().find(|task| {
                    task.id == *review_task_id
                        && task.kind == TaskKind::Review
                        && task.status == TaskStatus::Complete
                        && task.role == target.role
                })?;
                let context = review_task.evidence_review_context.as_ref()?;
                if context.recovery_id != recovery.id
                    || context.candidate_commit != recovery.candidate_commit
                {
                    return None;
                }
                let artifact_area = review_task.artifact_area.as_ref()?;
                Some(format!(
                    " --review-artifact {}/review.md",
                    artifact_area.path
                ))
            })
            .collect::<Option<String>>()?;
        return Some(format!(
            "fluent attempt evidence attach {} {} --candidate {} --evidence-file <path>{artifacts}",
            attempt.work_item_id, attempt.id, recovery.candidate_commit
        ));
    }
    if let Some(action) = project_root
        .and_then(|root| crate::host_evidence::pending_plan_host_evidence_action(root, attempt))
    {
        return Some(format!(
            "fluent attempt evidence attach {} {} --candidate {} --evidence-file <path> --plan-row {}",
            attempt.work_item_id, attempt.id, action.candidate_commit, action.plan_row
        ));
    }
    if attempt.status == AttemptStatus::NeedsUser
        && attempt.pause_kind == Some(PauseKind::WriterCompletion)
    {
        return project_root.and_then(|root| {
            crate::writer_completion_recovery::status_recovery_action(root, attempt)
        });
    }
    if attempt.status == AttemptStatus::NeedsUser
        && attempt.review_state == Some(AttemptReviewState::Failed)
        && matches!(
            attempt.pause_kind,
            Some(PauseKind::RoundCap | PauseKind::Uncertain)
        )
    {
        let (writer_index, candidate_commit) = attempt
            .tasks
            .iter()
            .enumerate()
            .rev()
            .find(|(_, task)| task.kind == TaskKind::Write && task.status == TaskStatus::Complete)
            .and_then(|(index, task)| {
                task.output
                    .as_ref()
                    .map(|output| (index, output.commit.as_str()))
            })?;
        let latest_tests_review = attempt
            .tasks
            .iter()
            .enumerate()
            .rev()
            .find(|(index, task)| {
                *index > writer_index
                    && task.kind == TaskKind::Review
                    && task.role == "tests"
                    && task.status == TaskStatus::Complete
            });
        if let Some(review_artifact) = latest_tests_review.and_then(|(_, task)| {
            if task
                .review_context
                .as_ref()
                .map(|context| context.candidate_commit.as_str())
                != Some(candidate_commit)
            {
                return None;
            }
            let path = format!("{}/review.md", task.artifact_area.as_ref()?.path);
            let content = std::fs::read_to_string(project_root?.join(&path)).ok()?;
            (crate::review::extract_verdict(&content) == crate::review::Verdict::Fail
                && crate::review::extract_focused_evidence_request(&content).is_some())
            .then_some(path)
        }) {
            return Some(format!(
                "fluent attempt evidence run {} {} --review-artifact {}",
                attempt.work_item_id, attempt.id, review_artifact
            ));
        }
    }
    if attempt.status == AttemptStatus::Complete
        && attempt.review_state == Some(AttemptReviewState::Passed)
        && attempt
            .learning
            .as_ref()
            .is_some_and(|learning| learning.is_failed() && learning.is_relaunchable())
    {
        crate::lifecycle_recovery::inspect_learner_terminal_history(
            project_root,
            attempt,
            &attempt.work_item_id,
        )
        .ok()?;
        return Some(format!(
            "fluent queue resume {} --attempt {}",
            attempt.work_item_id, attempt.id
        ));
    }
    attempt.ensure_resume_ready().ok().map(|()| {
        format!(
            "fluent queue resume {} --attempt {}",
            attempt.work_item_id, attempt.id
        )
    })
}

/// Return the saved operation only while its identity still matches the current
/// Attempt. If the Attempt changed, derive a fresh operation instead of printing
/// an old command to the operator.
pub fn attempt_recovery_next_action(
    attempt: Option<&Attempt>,
    project_root: Option<&Path>,
) -> Option<String> {
    attempt_recovery_guidance(attempt, project_root).next_action
}

fn attempt_recovery_guidance(
    attempt: Option<&Attempt>,
    project_root: Option<&Path>,
) -> AttemptRecoveryGuidance {
    let mut guidance = derive_attempt_recovery_guidance(attempt, project_root);
    let Some(attempt) = attempt else {
        return guidance;
    };
    if let Some(record) = attempt.pause_record.as_ref() {
        if record.matches_attempt(attempt) {
            match (&record.next_operation, &record.no_safe_operation_reason) {
                (Some(operation), _)
                    if guidance.next_action.as_deref() == Some(operation.command.as_str()) =>
                {
                    guidance.next_action = Some(operation.command.clone());
                }
                _ => {}
            }
        }
    }
    guidance
}

/// Render the common pause section appended to operator handoffs.
pub fn render_pause_record(record: &crate::work_model::PauseRecord) -> String {
    let mut output = String::from("## Saved pause record\n\n");
    output.push_str(&format!("Blocker: {}\n", record.blocker));
    if let Some(candidate) = record.candidate_commit.as_deref() {
        output.push_str(&format!("Candidate: `{candidate}`\n"));
    }
    if let Some(task_id) = record.task_id.as_deref() {
        output.push_str(&format!("Task: `{task_id}`\n"));
    }
    output.push_str(&format!(
        "Required evidence or decision: {}\n",
        record.required_evidence_or_decision
    ));
    match (&record.next_operation, &record.no_safe_operation_reason) {
        (Some(operation), _) => {
            output.push_str(&format!("Next operation: `{}`\n", operation.command));
        }
        (None, Some(reason)) => {
            output.push_str(&format!("No safe operation: {reason}\n"));
        }
        (None, None) => output.push_str("No safe operation recorded.\n"),
    }
    output
}

/// Append the shared pause record to a handoff after the Attempt state has been
/// persisted. Handoff-specific explanation remains intact; this section gives
/// every surface the same blocker and recovery command.
pub fn append_pause_record_to_handoff(
    project_root: &Path,
    work_item_id: &str,
    attempt_id: &str,
    mut body: String,
) -> String {
    let store = WorkModelStore::new(project_root);
    if let Ok(item) = store.read_work_item(work_item_id)
        && let Some(attempt) = item
            .attempts
            .iter()
            .find(|attempt| attempt.id == attempt_id)
        && let Some(record) = attempt.pause_record.as_ref()
        && record.matches_attempt(attempt)
    {
        if !body.ends_with('\n') {
            body.push('\n');
        }
        body.push('\n');
        body.push_str(&render_pause_record(record));
    }
    body
}

fn tester_pause_requires_project_repair(project_root: &Path, attempt: &Attempt) -> bool {
    attempt.tasks.iter().rev().any(|task| {
        task.kind == TaskKind::Tester
            && task.status == TaskStatus::NeedsUser
            && task.artifact_area.as_ref().is_some_and(|area| {
                crate::tester::harness_error_requires_project_repair(&project_root.join(&area.path))
            })
    })
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
        MergeCandidateMergeStatus::LandingPending => "publication-pending",
        MergeCandidateMergeStatus::Failed => "failed",
        MergeCandidateMergeStatus::NeedsUser => "needs-user",
        MergeCandidateMergeStatus::Merged => "merged",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::work_model::{
        ArtifactRef, AttemptLearning, LearningFailureKind, MergeCandidateMergeState, TaskOutput,
        WorkItem, WorkspaceAccess, WorkspaceRef,
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
    fn status_uses_the_attempt_bound_to_the_latest_dispatch() {
        let project = tempfile::tempdir().unwrap();
        let store = WorkModelStore::new(project.path());
        let mut item = WorkItem::planned("work-1", "Resume an older repaired Attempt");
        item.add_initial_attempt("attempt-1").unwrap();
        item.add_initial_attempt("attempt-2").unwrap();
        let later = item.attempts.last_mut().unwrap();
        crate::work_model::set_task_terminal(&mut later.tasks[0], TaskStatus::Failed);
        crate::work_model::set_attempt_terminal(later, AttemptStatus::Failed);
        store.create_work_item(&item).unwrap();
        crate::queue::add(project.path(), "work-1", None).unwrap();
        crate::queue::claim(project.path(), "work-1", "attempt-1")
            .unwrap()
            .unwrap();

        let row = summarize_work_item(&item, Some(project.path()));

        assert_eq!(row.attempt, "attempt-1 [planned]");
        assert_eq!(row.task, "write:attempt-1-write-1 [planned]");
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
            completed_matrix: 0,
            candidate_commit: "abc123".to_string(),
        });

        let mut tester = attempt.tasks[0].clone();
        tester.id = "attempt-1-test-1".to_string();
        tester.kind = TaskKind::Tester;
        tester.artifact_area = Some(crate::work_model::TaskArtifactArea {
            path: format!(
                "{}/metrics-work/attempt-1/attempt-1-test-1",
                crate::work_model::WORK_ARTIFACTS_DIR
            ),
        });
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
        fs::write(
            usage_dir.join("transcript-pump.json"),
            serde_json::to_vec_pretty(&crate::transcript_pump::PumpStatus {
                schema_version: crate::transcript_pump::PUMP_STATUS_SCHEMA_VERSION,
                state: crate::transcript_pump::PumpState::Complete,
                started_at_ms: 1,
                updated_at_ms: 2,
                bytes: 10,
                records: 2,
                dropped_console: 2,
                provider_timing: crate::transcript_pump::ProviderTimingDiagnostics {
                    retry_count: 1,
                    wait_ms: 750,
                    waiting_since_ms: None,
                    resume_at_ms: None,
                },
                provider_liveness: Default::default(),
                error: None,
                periodic_error: None,
                transport: Default::default(),
            })
            .unwrap(),
        )
        .unwrap();

        refresh_persisted_metrics(tmp.path(), &item).unwrap();
        let metrics = work_metrics(&item, Some(tmp.path()));

        assert_eq!(metrics.review_rounds, 1);
        assert_eq!(metrics.stage_duration_ms, 2_000);
        assert_eq!(metrics.provider_wait_ms, 750);
        assert_eq!(metrics.active_duration_ms, 1_250);
        assert_eq!(metrics.input_tokens, 120);
        assert_eq!(metrics.output_tokens, 30);
        assert_eq!(metrics.repeated_findings, 1);
        assert!(metrics.artifact_bytes > 0);
        assert_eq!(metrics.avoided_cycles, 1);
        fs::write(usage_dir.join("late-unindexed-artifact"), vec![0_u8; 4096]).unwrap();
        assert_eq!(
            work_metrics(&item, Some(tmp.path())),
            metrics,
            "status reads the persisted sidecar and never walks artifacts"
        );
        refresh_persisted_task_metrics(tmp.path(), &item, "attempt-1-write-1").unwrap();
        let incrementally_refreshed = work_metrics(&item, Some(tmp.path()));
        assert_eq!(incrementally_refreshed.input_tokens, 120);
        assert_eq!(incrementally_refreshed.output_tokens, 30);
        assert!(incrementally_refreshed.artifact_bytes > metrics.artifact_bytes);
        let show = work_item_show_value(&item, tmp.path()).unwrap();
        assert_eq!(show["metrics"]["stage-duration-ms"], 2_000);
        assert_eq!(show["metrics"]["provider-wait-ms"], 750);
        assert_eq!(show["metrics"]["active-duration-ms"], 1_250);
        assert_eq!(show["metrics"]["input-tokens"], 120);
    }

    #[test]
    fn legacy_metrics_without_provider_wait_remain_readable() {
        let tmp = tempfile::tempdir().unwrap();
        let mut item = WorkItem::planned("legacy-metrics", "Read old metrics");
        item.add_initial_attempt("attempt-1").unwrap();
        item.attempts[0].tasks[0].started_at = Some("2026-08-03T10:00:00Z".to_string());
        item.attempts[0].tasks[0].completed_at = Some("2026-08-03T10:00:02Z".to_string());
        let path = persisted_metrics_path(tmp.path(), &item.id);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            path,
            r#"{
                "schema-version":2,
                "input-tokens":10,
                "output-tokens":5,
                "repeated-findings":0,
                "artifact-bytes":20,
                "tasks":{},
                "updated-at":"2026-08-03T10:00:02Z"
            }"#,
        )
        .unwrap();

        let metrics = work_metrics(&item, Some(tmp.path()));

        assert_eq!(metrics.stage_duration_ms, 2_000);
        assert_eq!(metrics.provider_wait_ms, 0);
        assert_eq!(metrics.active_duration_ms, 2_000);
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
                completed_matrix: 0,
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
    fn merged_candidate_suppresses_stale_attempt_recovery_guidance() {
        let (mut item, _) = passed_attempt_with_candidate(None);
        let attempt = &mut item.attempts[0];
        let mut completed_rebase = attempt.tasks[0].clone();
        attempt.tasks[0].id = "attempt-1-rebase".to_string();
        attempt.tasks[0].kind = TaskKind::Rebase;
        attempt.tasks[0].role = "rebase".to_string();
        attempt.tasks[0].status = TaskStatus::NeedsUser;
        attempt.tasks[0].output = None;
        completed_rebase.id = "attempt-1-rebase-2".to_string();
        completed_rebase.kind = TaskKind::Rebase;
        completed_rebase.role = "rebase".to_string();
        completed_rebase.status = TaskStatus::Complete;
        completed_rebase.output = None;
        attempt.tasks.push(completed_rebase);
        attempt.status = AttemptStatus::NeedsUser;
        attempt.pause_kind = Some(PauseKind::HostSandbox);
        let candidate = &mut item.merge_candidates[0];
        candidate.merge_review_state = MergeReviewState::Passed;
        candidate.merge_state.status = MergeCandidateMergeStatus::Merged;
        candidate.merge_state.merged_commit = Some("abc123".to_string());

        let row = summarize_work_item(&item, None);

        assert_eq!(row.task, "rebase:attempt-1-rebase-2 [complete]");
        assert_eq!(row.action, "merged");
        assert_eq!(row.next_action, None);
    }

    #[test]
    fn executing_task_shows_active_provider_wait() {
        let tmp = tempfile::tempdir().unwrap();
        let mut item = WorkItem::planned("work-1", "Wait for provider");
        item.add_initial_attempt("attempt-1").unwrap();
        item.attempts[0].status = AttemptStatus::Executing;
        item.attempts[0].tasks[0].status = TaskStatus::Executing;
        let task = &item.attempts[0].tasks[0];
        let artifact_dir = tmp.path().join(&task.artifact_area.as_ref().unwrap().path);
        fs::create_dir_all(&artifact_dir).unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        let status = crate::transcript_pump::PumpStatus {
            schema_version: crate::transcript_pump::PUMP_STATUS_SCHEMA_VERSION,
            state: crate::transcript_pump::PumpState::Running,
            started_at_ms: now.saturating_sub(1_000),
            updated_at_ms: now,
            bytes: 100,
            records: 2,
            dropped_console: 2,
            provider_timing: crate::transcript_pump::ProviderTimingDiagnostics {
                retry_count: 1,
                wait_ms: 0,
                waiting_since_ms: Some(now),
                resume_at_ms: Some(now.saturating_add(60_000)),
            },
            provider_liveness: Default::default(),
            error: None,
            periodic_error: None,
            transport: Default::default(),
        };
        fs::write(
            artifact_dir.join("transcript-pump.json"),
            serde_json::to_vec_pretty(&status).unwrap(),
        )
        .unwrap();
        let lock = crate::lease::task_lock_path(tmp.path(), &item.id, &task.id);
        let _lease = crate::lease::acquire(&lock).unwrap();

        let row = summarize_work_item(&item, Some(tmp.path()));

        assert_eq!(row.task, "write:attempt-1-write-1 [provider-wait]");
    }

    #[test]
    fn executing_task_shows_outer_provider_wait() {
        let tmp = tempfile::tempdir().unwrap();
        let mut item = WorkItem::planned("work-1", "Wait before provider relaunch");
        item.add_initial_attempt("attempt-1").unwrap();
        item.attempts[0].status = AttemptStatus::Executing;
        item.attempts[0].tasks[0].status = TaskStatus::Executing;
        let task = &item.attempts[0].tasks[0];
        let artifact_dir = tmp.path().join(&task.artifact_area.as_ref().unwrap().path);
        fs::create_dir_all(&artifact_dir).unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        let timing = crate::coder::ProviderRetryTiming {
            schema_version: 1,
            retry_count: 1,
            wait_ms: 0,
            waiting_since_ms: Some(now),
            resume_at_ms: Some(now.saturating_add(60_000)),
            absorbed_through_phase: None,
        };
        fs::write(
            artifact_dir.join(crate::coder::PROVIDER_TIMING_FILENAME),
            serde_json::to_vec_pretty(&timing).unwrap(),
        )
        .unwrap();
        let lock = crate::lease::task_lock_path(tmp.path(), &item.id, &task.id);
        let _lease = crate::lease::acquire(&lock).unwrap();

        let row = summarize_work_item(&item, Some(tmp.path()));

        assert_eq!(row.task, "write:attempt-1-write-1 [provider-wait]");
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
                recovery_blocker: None,
                verification: None,
                evidence_history: None,
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
                release: None,
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
                recovery_blocker: None,
                verification: None,
                evidence_history: None,
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
                release: None,
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
            landing_intent: None,
        };

        let row = summarize_work_item(&item, None);

        assert_eq!(row.action, "merged");
        assert_eq!(row.merge, "merged review:pending");
    }

    #[test]
    fn evidence_needed_status_names_exact_attachment_command() {
        let recovery_id = "host-evidence-1";
        let review_task_id = "attempt-1-evidence-1-architecture";
        let candidate_commit = "abc123";
        let attachment = crate::work_model::EvidenceAttachment {
            snapshot_path: ".fluent/work/artifacts/work-1/attempt-1/host-evidence/a.json"
                .to_string(),
            digest: "sha256:abc".to_string(),
        };
        let attempt = Attempt {
            id: "attempt-1".to_string(),
            work_item_id: "work-1".to_string(),
            status: AttemptStatus::NeedsUser,
            tasks: vec![Task {
                id: review_task_id.to_string(),
                kind: TaskKind::Review,
                status: TaskStatus::Complete,
                role: "architecture".to_string(),
                instructions: None,
                work_item_id: "work-1".to_string(),
                attempt_id: Some("attempt-1".to_string()),
                workspace_access: WorkspaceAccess::read_only(Vec::new()),
                artifact_area: Some(crate::work_model::TaskArtifactArea {
                    path: ".fluent/work/artifacts/work-1/attempt-1/evidence-review".to_string(),
                }),
                review_context: None,
                writer_correction_assignment: None,
                verification_context: None,
                evidence_review_context: Some(crate::work_model::EvidenceReviewContext {
                    recovery_id: recovery_id.to_string(),
                    candidate_commit: candidate_commit.to_string(),
                    attachment: attachment.clone(),
                    prior_review_artifact:
                        ".fluent/work/artifacts/work-1/attempt-1/review/review.md".to_string(),
                }),
                input_artifacts: Vec::new(),
                depends_on: None,
                output: None,
                created_at: None,
                started_at: None,
                completed_at: None,
            }],
            evidence_recoveries: vec![crate::work_model::EvidenceRecovery {
                carried_finding: None,
                required_check: None,
                id: recovery_id.to_string(),
                candidate_commit: candidate_commit.to_string(),
                attachment,
                targets: vec![crate::work_model::EvidenceReviewTarget {
                    role: "architecture".to_string(),
                    prior_review_artifact:
                        ".fluent/work/artifacts/work-1/attempt-1/review/review.md".to_string(),
                    review_task_id: Some(review_task_id.to_string()),
                    review_digest: None,
                }],
                state: crate::work_model::EvidenceRecoveryState::NeedsEvidence,
                created_at: "2026-08-03T00:00:00Z".to_string(),
                code_change_classification: None,
            }],
            ..Attempt::default()
        };

        assert_eq!(
            attempt_recovery_next_action(Some(&attempt), None).as_deref(),
            Some(
                "fluent attempt evidence attach work-1 attempt-1 --candidate abc123 --evidence-file <path> --review-artifact .fluent/work/artifacts/work-1/attempt-1/evidence-review/review.md"
            )
        );
    }

    #[test]
    fn evidence_needed_status_fails_closed_without_targeted_review() {
        let attempt = Attempt {
            id: "attempt-1".to_string(),
            work_item_id: "work-1".to_string(),
            status: AttemptStatus::NeedsUser,
            evidence_recoveries: vec![crate::work_model::EvidenceRecovery {
                carried_finding: None,
                required_check: None,
                id: "host-evidence-1".to_string(),
                candidate_commit: "abc123".to_string(),
                attachment: crate::work_model::EvidenceAttachment {
                    snapshot_path: ".fluent/work/artifacts/work-1/attempt-1/host-evidence/a.json"
                        .to_string(),
                    digest: "sha256:abc".to_string(),
                },
                targets: vec![crate::work_model::EvidenceReviewTarget {
                    role: "tests".to_string(),
                    prior_review_artifact:
                        ".fluent/work/artifacts/work-1/attempt-1/review/review.md".to_string(),
                    review_task_id: None,
                    review_digest: None,
                }],
                state: crate::work_model::EvidenceRecoveryState::NeedsEvidence,
                created_at: "2026-08-03T00:00:00Z".to_string(),
                code_change_classification: None,
            }],
            ..Attempt::default()
        };

        assert_eq!(attempt_recovery_next_action(Some(&attempt), None), None);
    }

    #[test]
    fn focused_evidence_status_names_exact_run_command() {
        let root = tempfile::TempDir::new().unwrap();
        let artifact = ".fluent/work/artifacts/work-1/attempt-1/review-tests";
        std::fs::create_dir_all(root.path().join(artifact)).unwrap();
        std::fs::write(
            root.path().join(artifact).join("review.md"),
            "Verdict: fail\nEvidence-Command: cargo test exact_case\nEvidence-Working-Directory: .\n",
        )
        .unwrap();
        let mut item = WorkItem::planned("work-1", "Focused evidence");
        item.add_initial_attempt("attempt-1").unwrap();
        let writer = &mut item.attempts[0].tasks[0];
        writer.status = TaskStatus::Complete;
        writer.output = Some(crate::work_model::TaskOutput {
            workspace_id: "candidate".to_string(),
            workspace_path: root.path().display().to_string(),
            source_branch: "main".to_string(),
            base_commit: None,
            commit: "abc123".to_string(),
            no_change: None,
            learner_canonicalization: None,
        });
        item.add_review_tasks("attempt-1", &["tests"]).unwrap();
        let review = item.attempts[0].tasks.last_mut().unwrap();
        review.status = TaskStatus::Complete;
        review.artifact_area = Some(crate::work_model::TaskArtifactArea {
            path: artifact.to_string(),
        });
        item.attempts[0].status = AttemptStatus::NeedsUser;
        item.attempts[0].pause_kind = Some(PauseKind::Uncertain);
        item.attempts[0].review_state = Some(AttemptReviewState::Failed);
        assert_eq!(
            attempt_recovery_next_action(Some(&item.attempts[0]), Some(root.path())).as_deref(),
            Some(
                "fluent attempt evidence run work-1 attempt-1 --review-artifact .fluent/work/artifacts/work-1/attempt-1/review-tests/review.md"
            )
        );

        let newer_artifact = ".fluent/work/artifacts/work-1/attempt-1/review-tests-newer";
        std::fs::create_dir_all(root.path().join(newer_artifact)).unwrap();
        std::fs::write(
            root.path().join(newer_artifact).join("review.md"),
            "Verdict: pass\n",
        )
        .unwrap();
        let mut newer_review = item.attempts[0].tasks.last().unwrap().clone();
        newer_review.id = "review-tests-newer".to_string();
        newer_review.artifact_area = Some(crate::work_model::TaskArtifactArea {
            path: newer_artifact.to_string(),
        });
        item.attempts[0].tasks.push(newer_review);
        assert_eq!(
            attempt_recovery_next_action(Some(&item.attempts[0]), Some(root.path())),
            None
        );
    }

    #[test]
    fn writer_completion_status_waits_for_recovery_evidence() {
        let mut item = WorkItem::planned("work-1", "Await Writer completion evidence");
        item.add_initial_attempt("attempt-1").unwrap();
        let attempt = &mut item.attempts[0];
        attempt.status = AttemptStatus::NeedsUser;
        attempt.pause_kind = Some(crate::work_model::PauseKind::WriterCompletion);
        attempt.tasks[0].status = TaskStatus::NeedsUser;

        let row = summarize_work_item(&item, None);

        assert_eq!(row.action, "needs-user");
        assert_eq!(row.next_action, None);
    }

    #[test]
    fn saved_pause_operation_is_used_only_for_the_same_attempt_state() {
        let mut item = WorkItem::planned("work-pause-status", "Pause status");
        item.add_initial_attempt("attempt-1").unwrap();
        let attempt = &mut item.attempts[0];
        attempt.status = AttemptStatus::NeedsUser;
        attempt.pause_kind = Some(PauseKind::Auth);
        attempt.tasks[0].status = TaskStatus::NeedsUser;
        let command = "fluent queue resume work-pause-status --attempt attempt-1";
        attempt.pause_record = Some(crate::work_model::PauseRecord::generated(
            attempt,
            Some(command.to_string()),
            None,
        ));

        assert_eq!(
            attempt_recovery_next_action(Some(attempt), None).as_deref(),
            Some(command)
        );

        // A changed saved command means the recorded operation no longer agrees
        // with the current derivation. The wrapper must not print stale text.
        let stale_command = "fluent attempt run work-pause-status attempt-1";
        attempt
            .pause_record
            .as_mut()
            .unwrap()
            .next_operation
            .as_mut()
            .unwrap()
            .command = stale_command.to_string();
        let current = attempt_recovery_next_action(Some(attempt), None);
        assert_eq!(current.as_deref(), Some(command));
        assert_ne!(current.as_deref(), Some(stale_command));
    }

    #[test]
    fn saved_pause_record_renders_the_same_operator_fields() {
        let mut item = WorkItem::planned("work-pause-render", "Pause render");
        item.add_initial_attempt("attempt-1").unwrap();
        let attempt = &mut item.attempts[0];
        attempt.status = AttemptStatus::NeedsUser;
        attempt.pause_kind = Some(PauseKind::Auth);
        attempt.tasks[0].status = TaskStatus::NeedsUser;
        attempt.pause_record = Some(crate::work_model::PauseRecord::generated(
            attempt,
            Some("fluent queue resume work-pause-render --attempt attempt-1".to_string()),
            None,
        ));

        let output = render_pause_record(attempt.pause_record.as_ref().unwrap());
        assert!(output.contains("Blocker: Provider authentication is required."));
        assert!(output.contains("Task: `attempt-1-write-1`"));
        assert!(output.contains(
            "Next operation: `fluent queue resume work-pause-render --attempt attempt-1`"
        ));
    }

    #[test]
    fn project_tester_error_status_names_repair_command() {
        let root = tempfile::tempdir().unwrap();
        let artifact = ".fluent/work/artifacts/work-1/attempt-1/attempt-1-tester";
        std::fs::create_dir_all(root.path().join(artifact)).unwrap();
        std::fs::write(
            root.path().join(artifact).join("tester-results.json"),
            serde_json::to_vec_pretty(&crate::tester::TesterResults {
                candidate_commit: Some("abc123".to_string()),
                commands: Vec::new(),
                tests: Vec::new(),
                summary: crate::tester::Summary {
                    total: 0,
                    pass: 0,
                    fail: 0,
                    skipped: 0,
                },
                error: Some(crate::tester::TesterError {
                    kind: "duplicate_test_ids".to_string(),
                    message: "Duplicate test ids".to_string(),
                    details: String::new(),
                }),
            })
            .unwrap(),
        )
        .unwrap();
        let mut item = WorkItem::planned("work-1", "Repair project Tester");
        item.add_initial_attempt("attempt-1").unwrap();
        let attempt = &mut item.attempts[0];
        attempt.status = AttemptStatus::NeedsUser;
        attempt.pause_kind = Some(PauseKind::TesterHarness);
        attempt.tasks[0].kind = TaskKind::Tester;
        attempt.tasks[0].status = TaskStatus::NeedsUser;
        attempt.tasks[0].artifact_area = Some(crate::work_model::TaskArtifactArea {
            path: artifact.to_string(),
        });

        assert_eq!(
            attempt_recovery_next_action(Some(attempt), Some(root.path())).as_deref(),
            Some(
                "fluent attempt repair-tester work-1 attempt-1 --resolution \"<describe the Tester repair>\""
            )
        );
    }

    #[test]
    fn environmental_tester_error_status_names_same_candidate_resume() {
        let mut item = WorkItem::planned("work-1", "Repair Tester environment");
        item.add_initial_attempt("attempt-1").unwrap();
        let attempt = &mut item.attempts[0];
        attempt.status = AttemptStatus::NeedsUser;
        attempt.pause_kind = Some(PauseKind::TesterHarness);
        attempt.tasks[0].kind = TaskKind::Tester;
        attempt.tasks[0].status = TaskStatus::NeedsUser;

        assert_eq!(
            attempt_recovery_next_action(Some(attempt), None).as_deref(),
            Some("fluent queue resume work-1 --attempt attempt-1")
        );
    }

    fn tester_sandbox_recovery_item() -> WorkItem {
        let mut item = WorkItem::planned("work-1", "Resume Tester outside Fluent's sandbox");
        item.add_initial_attempt("attempt-1").unwrap();
        let attempt = &mut item.attempts[0];
        attempt.status = AttemptStatus::NeedsUser;
        attempt.pause_kind = Some(PauseKind::TesterHarness);
        attempt.pause_recovery = Some(PauseRecovery::TesterNoSandbox);
        attempt.tasks[0].kind = TaskKind::Tester;
        attempt.tasks[0].status = TaskStatus::NeedsUser;
        item
    }

    #[test]
    fn tester_sandbox_recovery_status_names_the_exact_approved_command() {
        let item = tester_sandbox_recovery_item();
        let expected = "fluent queue resume work-1 --attempt attempt-1 --no-sandbox";
        let row = summarize_work_item(&item, None);
        assert_eq!(row.next_action.as_deref(), Some(expected));
    }

    #[test]
    fn tester_sandbox_recovery_show_names_the_exact_approved_command() {
        let item = tester_sandbox_recovery_item();
        let expected = "fluent queue resume work-1 --attempt attempt-1 --no-sandbox";
        let shown = work_item_show_value(&item, Path::new(".")).unwrap();
        assert_eq!(shown["next-action"], expected);
        assert_eq!(shown["attempts"][0]["pause_kind"], "tester-harness");
        assert_eq!(shown["attempts"][0]["pause_recovery"], "tester-no-sandbox");
    }

    #[test]
    fn candidate_gate_status_names_exact_queue_resume_command() {
        let mut item = WorkItem::planned("work-1", "Resume candidate correction");
        item.add_initial_attempt("attempt-1").unwrap();
        let attempt = &mut item.attempts[0];
        attempt.status = AttemptStatus::NeedsUser;
        attempt.pause_kind = Some(crate::work_model::PauseKind::CandidateGate);
        attempt.review_state = Some(AttemptReviewState::NotReviewed);

        let row = summarize_work_item(&item, None);

        assert_eq!(row.action, "needs-user");
        assert_eq!(
            row.next_action.as_deref(),
            Some("fluent queue resume work-1 --attempt attempt-1")
        );
    }

    #[test]
    fn failed_candidate_gate_correction_reports_needs_user_recovery() {
        let mut item = WorkItem::planned("work-1", "Retry failed candidate correction");
        item.add_initial_attempt("attempt-1").unwrap();
        let first = &mut item.attempts[0].tasks[0];
        first.status = TaskStatus::Complete;
        first.output = Some(TaskOutput {
            workspace_id: "candidate".to_string(),
            workspace_path: "../candidate".to_string(),
            source_branch: "main".to_string(),
            base_commit: Some("base".to_string()),
            commit: "candidate".to_string(),
            no_change: None,
            learner_canonicalization: None,
        });
        item.add_next_write_round(
            "attempt-1",
            vec![ArtifactRef {
                producer_id: "attempt-1-write-1".to_string(),
                path: ".fluent/work/artifacts/work-1/attempt-1/candidate-gates/attempt-1-write-1/candidate-gate.json".to_string(),
            }],
        )
        .unwrap();
        let gate = item.attempts[0].tasks[1].input_artifacts[0].clone();
        item.attempts[0].artifacts.push(gate);
        let attempt = &mut item.attempts[0];
        attempt.status = AttemptStatus::Failed;
        attempt.pause_kind = None;
        attempt.review_state = Some(AttemptReviewState::Failed);
        attempt.tasks[1].status = TaskStatus::Failed;

        let row = summarize_work_item(&item, None);

        assert_eq!(row.action, "needs-user");
        assert_eq!(
            row.next_action.as_deref(),
            Some("fluent queue resume work-1 --attempt attempt-1")
        );
    }
}
